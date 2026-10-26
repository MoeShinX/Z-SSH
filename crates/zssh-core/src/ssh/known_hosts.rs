//! known_hosts 的读写与比对。
//!
//! 自己维护的文件是 `%APPDATA%\Z-ssh\known_hosts`（路径由调用方传进来），另外
//! **只读**加载 `%USERPROFILE%\.ssh\known_hosts`，这样系统 ssh 已经信任过的主机
//! 不用再确认。支持 OpenSSH 的常见写法：普通主机名 / IP、`[host]:port`、一行多个
//! 主机名、`|1|salt|hash` 哈希主机名、注释和空行、`@revoked`、`@cert-authority`。

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use russh::keys::ssh_key::HashAlg;
use russh::keys::PublicKey;
use sha1::Sha1;

use crate::error::ZsshError;
use crate::ssh::interact::HostKeyStatus;

/// 我们自己维护的 known_hosts 和只读加载的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHosts {
    /// 可写的那个文件
    our_file: PathBuf,
    /// 只读文件（`~/.ssh/known_hosts`），永不写入
    read_only: Vec<PathBuf>,
}

impl KnownHosts {
    /// 只管自己的文件。
    pub fn new(our_file: impl Into<PathBuf>) -> Self {
        Self {
            our_file: our_file.into(),
            read_only: Vec::new(),
        }
    }

    /// 再加一个只读文件。
    pub fn with_read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only.push(path.into());
        self
    }

    pub fn our_file(&self) -> &Path {
        &self.our_file
    }

    /// 查一台主机的某个密钥类型。
    ///
    /// 规则（扫描**所有**适用的记录，不在第一条同类型记录上就停）：
    /// 1. 任何文件里只要有一条 `@revoked` 记录就是当前这把密钥，直接拒绝（`Revoked`）——
    ///    不管它排在普通记录的前面还是后面、在自己的文件里还是系统文件里。吊销的是别的
    ///    密钥（比如泄露了的旧密钥）不影响当前这把。
    /// 2. 否则**自己文件里的记录优先**：自己的文件里有这台主机、这种密钥类型的普通记录，
    ///    就只按自己的文件判断，不再看系统文件——系统那份我们只读，用户改不了它怎么都清
    ///    不掉（M1-04 验收结论）。判断时只要有任何一条记录和当前密钥一致就接受
    ///    （换密钥的过渡期同一台主机登记两把同类型密钥很常见），都不一致才是"密钥变了"。
    /// 3. 自己的文件里没有同类型记录，按顺序看只读文件，每个文件同样规则。
    pub fn lookup(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyLookup {
        let key_type = key.algorithm().to_string();
        let fingerprint = fingerprint_of(key);
        let wanted = host_pattern(host, port);

        let mut files: Vec<(PathBuf, StoredIn)> = vec![(self.our_file.clone(), StoredIn::Our)];
        files.extend(
            self.read_only
                .iter()
                .map(|path| (path.clone(), StoredIn::System(path.clone()))),
        );

        // 每个文件里适用于这台主机的记录。
        let mut other_key_types = false;
        let mut per_file: Vec<(StoredIn, Vec<Entry>)> = Vec::new();
        for (path, stored_in) in files {
            let content = match read_known_hosts(&path) {
                Ok(Some(content)) => content,
                // 文件不存在：这份文件里确实没有记录。
                Ok(None) => continue,
                // 读不了 = 不确定：里面可能正好有这台主机的记录（甚至吊销），不能当成
                // "没见过"去问用户要不要信任（R-9 F10）。
                Err(err) => {
                    return HostKeyLookup::Unreadable {
                        path,
                        error: err.to_string(),
                    }
                }
            };
            let mut applicable = Vec::new();
            for entry in parse(&content) {
                if !entry.matches_host(&wanted) {
                    continue;
                }
                if entry.key_type != key_type {
                    if entry.marker.is_none() {
                        other_key_types = true;
                    }
                    continue;
                }
                applicable.push(entry);
            }
            per_file.push((stored_in, applicable));
        }

        // 1. 吊销：任何文件、任何位置，命中当前密钥就拒绝。
        for (stored_in, entries) in &per_file {
            if let Some(revoked) = entries.iter().find(|entry| {
                entry.marker == Some(Marker::Revoked) && entry.fingerprint() == fingerprint
            }) {
                return HostKeyLookup::Revoked {
                    stored_fingerprint: revoked.fingerprint(),
                    stored_in: stored_in.clone(),
                };
            }
        }

        // 2、3. 自己的文件优先，然后按顺序看只读文件：第一个有同类型普通记录的文件说了算。
        for (stored_in, entries) in &per_file {
            let ordinary: Vec<&Entry> = entries
                .iter()
                .filter(|entry| entry.marker.is_none())
                .collect();
            let Some(first) = ordinary.first() else {
                continue;
            };
            if ordinary
                .iter()
                .any(|entry| entry.fingerprint() == fingerprint)
            {
                return HostKeyLookup::Match;
            }
            return HostKeyLookup::Changed {
                stored_fingerprint: first.fingerprint(),
                stored_in: stored_in.clone(),
            };
        }

        if other_key_types {
            HostKeyLookup::OtherKeyTypes
        } else {
            HostKeyLookup::Unknown
        }
    }

    /// 把一条记录追加到自己的文件里（主机名写明文）。地址先过 [`check_host_address`]：
    /// 带 `*`、逗号、空格的地址写进去会变成通配或多个主机，匹配到别的服务器。
    ///
    /// 追加而不是重写，配合进程内的锁，多个连接同时确认同一台主机也不会写坏。
    pub fn append(&self, host: &str, port: u16, key: &PublicKey) -> Result<(), ZsshError> {
        let line = Self::entry_line(host, port, key)?;
        let _guard = write_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.append_locked(&line)
    }

    /// 指纹变化后用户确认「更新指纹」：删掉自己文件里这台主机的全部旧记录，再写上这把新密钥。
    /// 两步在同一把锁里做，别的连接不会在中间读到"旧的删了、新的还没写"。
    ///
    /// 旧记录在只读的系统文件里时也够用：查找时自己的文件优先，写进来的新记录会盖过它
    /// （系统文件本身照旧不碰）。返回删掉的旧记录条数。
    pub fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<usize, ZsshError> {
        let line = Self::entry_line(host, port, key)?;
        let _guard = write_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed = self.forget_locked(host, port, true)?;
        self.append_locked(&line)?;
        Ok(removed)
    }

    /// 一条要写进文件的记录（含换行）。
    fn entry_line(host: &str, port: u16, key: &PublicKey) -> Result<String, ZsshError> {
        check_host_address(host).map_err(ZsshError::HostKeyStore)?;
        Ok(format!(
            "{} {}\n",
            host_pattern(host, port),
            key.to_openssh()
                .map_err(|err| ZsshError::HostKeyStore(format!("公钥序列化失败：{err}")))?
        ))
    }

    /// 调用方必须已经拿着 `write_lock`。
    fn append_locked(&self, line: &str) -> Result<(), ZsshError> {
        if let Some(parent) = self.our_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| ZsshError::HostKeyStore(format!("建目录失败：{err}")))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.our_file)
            .map_err(|err| ZsshError::HostKeyStore(format!("打开 known_hosts 失败：{err}")))?;
        file.write_all(line.as_bytes())
            .map_err(|err| ZsshError::HostKeyStore(format!("写 known_hosts 失败：{err}")))?;
        file.sync_all()
            .map_err(|err| ZsshError::HostKeyStore(format!("刷新 known_hosts 失败：{err}")))?;
        Ok(())
    }

    /// 删掉自己文件里这台主机的全部记录（`hostkey_forget`）。
    ///
    /// 只动自己的文件，只读的那个永远不碰。
    pub fn forget(&self, host: &str, port: u16) -> Result<usize, ZsshError> {
        let _guard = write_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.forget_locked(host, port, false)
    }

    /// 调用方必须已经拿着 `write_lock`。`keep_revoked`：`@revoked` 行留着不删——更新指纹时
    /// 只换掉旧的普通记录，吊销过的密钥以后出现照样拦。
    fn forget_locked(&self, host: &str, port: u16, keep_revoked: bool) -> Result<usize, ZsshError> {
        let content = match read_known_hosts(&self.our_file) {
            Ok(Some(content)) => content,
            Ok(None) => return Ok(0),
            // 读不了就不能说"没有可删的"：用户以为清掉了，下次连接照样按旧指纹拦。
            Err(err) => {
                return Err(ZsshError::HostKeyStore(format!(
                    "读 known_hosts 失败：{err}"
                )))
            }
        };
        let wanted = host_pattern(host, port);

        let mut removed = 0usize;
        let mut kept = String::with_capacity(content.len());
        for line in content.lines() {
            let entry = parse_line(line);
            if entry.as_ref().is_some_and(|entry| {
                entry.matches_host(&wanted)
                    && !(keep_revoked && entry.marker == Some(Marker::Revoked))
            }) {
                removed += 1;
                continue;
            }
            kept.push_str(line);
            kept.push('\n');
        }

        if removed > 0 {
            std::fs::write(&self.our_file, kept)
                .map_err(|err| ZsshError::HostKeyStore(format!("写 known_hosts 失败：{err}")))?;
        }
        Ok(removed)
    }

    /// 已保存的指纹（界面和日志用）。
    pub fn saved_fingerprint(&self, host: &str, port: u16) -> Option<String> {
        let wanted = host_pattern(host, port);
        for path in self.all_files() {
            // 读不到就跳过这个文件继续找（自己的文件常常还没建出来，不能因此不看系统文件）。
            let Ok(Some(content)) = read_known_hosts(&path) else {
                continue;
            };
            for entry in parse(&content) {
                if entry.matches_host(&wanted) && entry.marker.is_none() {
                    return Some(entry.fingerprint());
                }
            }
        }
        None
    }

    fn all_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.our_file.clone()];
        files.extend(self.read_only.iter().cloned());
        files
    }
}

/// 一条记录存在哪儿。指纹变化时要用它区分"旧记录在自己文件里"还是"在系统文件里"：
/// 后者我们清不掉，只能提示用户自己删（M1-04 验收结论）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredIn {
    /// 我们自己维护、可以写的那个文件
    Our,
    /// 只读加载的系统文件
    System(PathBuf),
}

impl StoredIn {
    /// 给用户看的说明。
    pub fn describe(&self) -> String {
        match self {
            Self::Our => "自己的 known_hosts".to_owned(),
            Self::System(path) => format!("系统的 {}（我们只读它）", path.display()),
        }
    }

    /// 指纹变化时给用户的处理建议：自己的文件我们能清，系统的只能给命令。
    pub fn advice(&self, host: &str, port: u16) -> String {
        match self.forget_hint(host, port) {
            Some(hint) => format!(
                "这条旧记录在只读的系统文件里，我们改不了它。确认服务器换过密钥之后，                 在命令行里执行：
  {hint}"
            ),
            None => "确认服务器换过密钥的话，去主机的\"高级\"里点\"清除已保存的指纹\""
                .to_owned(),
        }
    }

    /// 旧记录在系统文件里时，给一条用户自己在命令行里能执行的删除命令。
    pub fn forget_hint(&self, host: &str, port: u16) -> Option<String> {
        match self {
            Self::Our => None,
            Self::System(_) => {
                let pattern = if port == 22 {
                    format!("\"{host}\"")
                } else {
                    format!("\"[{host}]:{port}\"")
                };
                Some(format!("ssh-keygen -R {pattern}"))
            }
        }
    }
}

/// 查一台主机的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyLookup {
    /// 没见过
    Unknown,
    /// 记过别的密钥类型
    OtherKeyTypes,
    /// 指纹一致
    Match,
    /// 有同类型的记录但指纹不一样
    Changed {
        stored_fingerprint: String,
        stored_in: StoredIn,
    },
    /// 命中了 `@revoked`
    Revoked {
        stored_fingerprint: String,
        stored_in: StoredIn,
    },
    /// 有一份 known_hosts 存在但读不了（权限、I/O 错误）：不知道里面有没有这台主机的记录
    Unreadable { path: PathBuf, error: String },
}

impl HostKeyLookup {
    /// 转成给界面看的状态。
    pub fn status(&self) -> Option<HostKeyStatus> {
        match self {
            Self::Unknown => Some(HostKeyStatus::Unknown),
            Self::OtherKeyTypes => Some(HostKeyStatus::OtherKeyTypesSaved),
            Self::Changed { .. } => Some(HostKeyStatus::Changed),
            Self::Revoked { .. } => Some(HostKeyStatus::Revoked),
            Self::Match | Self::Unreadable { .. } => None,
        }
    }

    /// 必须阻断的情况。
    pub fn must_block(&self) -> bool {
        matches!(
            self,
            Self::Changed { .. } | Self::Revoked { .. } | Self::Unreadable { .. }
        )
    }
}

/// OpenSSH 指纹（`SHA256:…`）。
pub fn fingerprint_of(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 主机在 known_hosts 里的写法：非 22 端口要 `[host]:port`。
pub fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// 行首的标记。`@cert-authority` 整行跳过，所以这里只有吊销。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Revoked,
}

/// 一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    marker: Option<Marker>,
    /// 逗号分隔的主机名列表，可能含哈希主机名
    patterns: Vec<String>,
    key_type: String,
    key_base64: String,
}

impl Entry {
    /// OpenSSH 的规则：命中任何一个 `!` 否定模式，整行都不算这台主机（哪怕前面的正向
    /// 通配命中了）；否则至少要命中一个正向模式（R-9 F09）。
    fn matches_host(&self, host: &str) -> bool {
        let mut matched = false;
        for pattern in &self.patterns {
            if let Some(negated) = pattern.strip_prefix('!') {
                if matches_pattern(host, negated) {
                    return false;
                }
            } else if matches_pattern(host, pattern) {
                matched = true;
            }
        }
        matched
    }

    /// 这条记录对应的密钥指纹。公钥解不出来时返回原文，方便排查。
    fn fingerprint(&self) -> String {
        match PublicKey::from_openssh(&format!("{} {}", self.key_type, self.key_base64)) {
            Ok(key) => fingerprint_of(&key),
            Err(_) => "<解析失败>".to_owned(),
        }
    }
}

/// 读一份 known_hosts：不存在是 `Ok(None)`，其它读取错误原样返回。
///
/// 不要求整份文件是 UTF-8：用户的 `~/.ssh/known_hosts` 里可能有本地编码写的注释，
/// 主机名和公钥都是 ASCII，按有损解码读不影响比对；因为编码就整份当成读不了，等于把系统
/// 文件里的全部约束都丢掉。
fn read_known_hosts(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn parse(content: &str) -> Vec<Entry> {
    content.lines().filter_map(parse_line).collect()
}

/// 解析一行，跳过注释、空行、`@cert-authority`（M1 不支持证书）和坏行。
fn parse_line(line: &str) -> Option<Entry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut rest = trimmed;
    let mut marker = None;
    if let Some(after) = rest.strip_prefix('@') {
        let (name, tail) = after.split_once(char::is_whitespace)?;
        marker = match name {
            "revoked" => Some(Marker::Revoked),
            // M1 不做证书认证，这一行直接跳过
            "cert-authority" => return None,
            _ => return None,
        };
        rest = tail.trim_start();
    }

    let mut fields = rest.split_whitespace();
    let patterns = fields.next()?;
    let key_type = fields.next()?;
    let key_base64 = fields.next()?;
    if patterns.is_empty() || key_type.is_empty() || key_base64.is_empty() {
        return None;
    }

    Some(Entry {
        marker,
        patterns: patterns.split(',').map(str::to_owned).collect(),
        key_type: key_type.to_owned(),
        key_base64: key_base64.to_owned(),
    })
}

/// 一个主机名模式是否匹配（含 `|1|salt|hash` 哈希写法）。
fn matches_pattern(host: &str, pattern: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("|1|") {
        let mut parts = rest.split('|');
        let (Some(salt), Some(hash)) = (parts.next(), parts.next()) else {
            return false;
        };
        let (Ok(salt), Ok(hash)) = (decode_base64(salt), decode_base64(hash)) else {
            return false;
        };
        return match Hmac::<Sha1>::new_from_slice(&salt) {
            Ok(hmac) => hmac.chain_update(host).verify_slice(&hash).is_ok(),
            Err(_) => false,
        };
    }

    // 支持 `*` 和 `?` 通配（OpenSSH 也这么干，`*.example.com` 很常见）
    wildcard_match(host, pattern)
}

fn decode_base64(raw: &str) -> Result<Vec<u8>, ()> {
    use base64::Engine as _;

    // OpenSSH 写的哈希主机名不带 `=` 填充，也接受带填充的写法
    let raw = raw.trim();
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(raw))
        .map_err(|_| ())
}

/// 只支持 `*` 和 `?`，够用且不会写错正则。
fn wildcard_match(text: &str, pattern: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return text == pattern;
    }

    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut t, mut p) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            t += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// 进程内一把写锁，避免两条连接同时确认同一台主机时把文件写坏。
///
/// 自己的 known_hosts 只有本进程在写（系统那份我们只读），所以一把锁就够。
fn write_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

/// 主机名列表里用到的分隔符，测试和排查时用得上。
pub fn host_patterns_of(line: &str) -> BTreeSet<String> {
    parse_line(line)
        .map(|entry| entry.patterns.into_iter().collect())
        .unwrap_or_default()
}

/// 主机地址能不能用：域名、IPv4、IPv6（可带方括号和 %zone）。不收 known_hosts 里有特殊含义的
/// 字符（`*` `?` `!` 通配、`,` 分隔多个主机、`|` 哈希、`#` 注释、`@` 标记）和空白、控制字符。
pub fn check_host_address(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("地址不能为空".to_owned());
    }
    if host.len() > 255 {
        return Err("地址太长".to_owned());
    }
    if let Some(bad) = host.chars().find(|c| {
        !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%' | '[' | ']'))
    }) {
        let shown = if bad.is_whitespace() || bad.is_control() {
            "空白或控制字符".to_owned()
        } else {
            format!("「{bad}」")
        };
        return Err(format!(
            "地址里不能有{shown}：只能是域名、IPv4 或 IPv6 地址"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC7n6m8BWZTkvJZfP3oLdMbFbcHYXDsaV3B/2cDmVdKb";
    const ED25519_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG0mFnPXWHG5gDoPWy7nlbLIz9BQLFbXk5z2tb1WfYqg";
    const RSA_A: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDIlVs/zd6RhKZ/B6yZLZQwvCpFcHfvPjAJfFK4z7DvWgvziYb9WcnDO/3k7Pw7JnvD0yLdQcKLDo3ZJzhGuG5VvJ3mJQ2nRdm1u+2l4PRUwAteKQy9Qh1ZKv7vZWSHH2vJZ7fJ5r1k8QqYYJvLKKhC1r7sB4EOt3Y6TqzMOxIhWn7A1B+P3DPXA1YzJnvDVBRpsNj9rZhWCYQO1MzX+TZQcbfBIoSQ8HjhFQxSLmM2ZWJcqXAK8w5vQr5NQY0wSaPuBbImqhqJgxd9tDLSMLVOdJUsWqRJtv2WHCVjGaEUaP+sVFTZRvsCB0";

    fn key(spec: &str) -> PublicKey {
        PublicKey::from_openssh(spec).expect("测试公钥应该能解析")
    }

    /// 临时目录的一份 known_hosts。
    struct TempFile {
        path: PathBuf,
        /// 放在最后：目录守卫最后 drop。
        _dir: crate::test_util::TestDir,
    }

    impl TempFile {
        fn new() -> Self {
            let dir = crate::test_util::TestDir::new("kh");
            let path = dir.join("known_hosts");
            Self { path, _dir: dir }
        }

        fn write(&self, content: &str) {
            std::fs::write(&self.path, content).expect("写临时 known_hosts");
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.path).unwrap_or_default()
        }
    }

    #[test]
    fn plain_hostname_matches() {
        let file = TempFile::new();
        file.write(&format!("example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match,
            "写进去的应该能读出来"
        );

        // 换个空文件就不认识了
        let other_file = TempFile::new();
        let other = KnownHosts::new(&other_file.path);
        assert_eq!(
            other.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
    }

    #[test]
    fn ipv4_and_port_notation() {
        let file = TempFile::new();
        file.write(&format!("[10.0.0.9]:2222 {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);

        assert_eq!(
            known.lookup("10.0.0.9", 2222, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        // 22 端口不算同一个主机
        assert_eq!(
            known.lookup("10.0.0.9", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
    }

    #[test]
    fn multiple_hostnames_on_one_line() {
        let file = TempFile::new();
        file.write(&format!(
            "a.example.com,b.example.com,10.0.0.1 {ED25519_A}\n"
        ));
        let known = KnownHosts::new(&file.path);

        for host in ["a.example.com", "b.example.com", "10.0.0.1"] {
            assert_eq!(
                known.lookup(host, 22, &key(ED25519_A)),
                HostKeyLookup::Match,
                "{host} 应该匹配"
            );
        }
        assert_eq!(
            known.lookup("c.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
    }

    #[test]
    fn hashed_hostname_matches() {
        // ssh-keygen -H 生成的格式：|1|base64(salt)|base64(hmac-sha1(salt, host))
        let salt: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let mut hmac = Hmac::<Sha1>::new_from_slice(&salt).expect("hmac");
        hmac.update(b"hashed.example.com");
        let hash = hmac.finalize().into_bytes();

        use base64::Engine as _;
        let encoded_salt = base64::engine::general_purpose::STANDARD.encode(salt);
        let encoded_hash = base64::engine::general_purpose::STANDARD.encode(hash);
        let file = TempFile::new();
        file.write(&format!("|1|{encoded_salt}|{encoded_hash} {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);

        assert_eq!(
            known.lookup("hashed.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        assert_eq!(
            known.lookup("other.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
    }

    #[test]
    fn wildcard_hostname_matches() {
        let file = TempFile::new();
        file.write(&format!("*.example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("web1.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let file = TempFile::new();
        file.write(&format!(
            "# 注释\nexample.com {ED25519_A}\n\n   \n# 再来一行注释\nexample.net {ED25519_B}\n"
        ));
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("example.net", 22, &key(ED25519_B)),
            HostKeyLookup::Match
        );
    }

    #[test]
    fn changed_fingerprint_is_detected() {
        let file = TempFile::new();
        file.write(&format!("example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);

        match known.lookup("example.com", 22, &key(ED25519_B)) {
            HostKeyLookup::Changed {
                stored_fingerprint,
                stored_in,
            } => {
                assert_eq!(stored_fingerprint, fingerprint_of(&key(ED25519_A)));
                assert_eq!(stored_in, StoredIn::Our);
            }
            other => panic!("应该报指纹变化，实际 {other:?}"),
        }
        assert!(known
            .lookup("example.com", 22, &key(ED25519_B))
            .must_block());
    }

    #[test]
    fn other_key_types_are_reported_separately() {
        let file = TempFile::new();
        file.write(&format!("example.com {RSA_A}\n"));
        let known = KnownHosts::new(&file.path);

        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::OtherKeyTypes,
            "只记过别的密钥类型时按未知主机处理，但状态要区分"
        );
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)).status(),
            Some(HostKeyStatus::OtherKeyTypesSaved)
        );
    }

    #[test]
    fn revoked_marker_blocks() {
        let file = TempFile::new();
        file.write(&format!("@revoked example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);

        let lookup = known.lookup("example.com", 22, &key(ED25519_A));
        assert!(
            matches!(lookup, HostKeyLookup::Revoked { .. }),
            "命中 @revoked 要阻断，实际 {lookup:?}"
        );
        assert!(lookup.must_block());
    }

    #[test]
    fn cert_authority_lines_are_skipped() {
        let file = TempFile::new();
        file.write(&format!("@cert-authority *.example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("web.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown,
            "M1 不支持证书，这一行应该当没看见"
        );
    }

    #[test]
    fn read_only_file_is_consulted_but_never_written() {
        let user_file = TempFile::new();
        user_file.write(&format!("trusted.example.com {ED25519_A}\n"));
        let our_file = TempFile::new();

        let known = KnownHosts::new(&our_file.path).with_read_only(&user_file.path);
        assert_eq!(
            known.lookup("trusted.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match,
            "系统 ssh 信任过的主机不该再问"
        );

        // 只读文件不能被写
        let before = user_file.read();
        known
            .append("trusted.example.com", 22, &key(ED25519_B))
            .expect("写自己的文件");
        assert_eq!(user_file.read(), before, "只读文件不该被改动");
        assert!(
            our_file.read().contains("trusted.example.com"),
            "应该写进自己的文件"
        );
    }

    #[test]
    fn append_then_lookup_matches() {
        let file = TempFile::new();
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );

        known
            .append("example.com", 22, &key(ED25519_A))
            .expect("写入");
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );

        // 非 22 端口写成 [host]:port
        known
            .append("example.com", 2200, &key(ED25519_B))
            .expect("写入");
        assert_eq!(
            known.lookup("example.com", 2200, &key(ED25519_B)),
            HostKeyLookup::Match
        );
        assert!(file.read().contains("[example.com]:2200"));

        // 连写两次是两行（OpenSSH 也允许同一主机多行）
        known
            .append("example.com", 22, &key(ED25519_A))
            .expect("写入");
        assert_eq!(file.read_count("example.com"), 2);
    }

    #[test]
    fn forget_removes_only_the_target_host() {
        let file = TempFile::new();
        let known = KnownHosts::new(&file.path);
        known
            .append("a.example.com", 22, &key(ED25519_A))
            .expect("写入");
        known
            .append("b.example.com", 22, &key(ED25519_B))
            .expect("写入");
        known
            .append("a.example.com", 2200, &key(ED25519_A))
            .expect("写入");

        let removed = known.forget("a.example.com", 22).expect("删除");
        assert_eq!(removed, 1);

        assert_eq!(
            known.lookup("a.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
        assert_eq!(
            known.lookup("a.example.com", 2200, &key(ED25519_A)),
            HostKeyLookup::Match,
            "同名的其它端口要留着"
        );
        assert_eq!(
            known.lookup("b.example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Match
        );

        // 再删一次就没有了
        assert_eq!(known.forget("a.example.com", 22).expect("删除"), 0);
    }

    #[test]
    fn replace_swaps_the_old_key_for_the_new_one_and_leaves_other_hosts_alone() {
        let file = TempFile::new();
        let known = KnownHosts::new(&file.path);
        known
            .append("example.com", 22, &key(ED25519_A))
            .expect("写入");
        known
            .append("other.example.com", 22, &key(ED25519_A))
            .expect("写入");
        assert!(matches!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Changed { .. }
        ));

        assert_eq!(
            known
                .replace("example.com", 22, &key(ED25519_B))
                .expect("更新"),
            1
        );
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Match
        );
        assert!(
            matches!(
                known.lookup("example.com", 22, &key(ED25519_A)),
                HostKeyLookup::Changed { .. }
            ),
            "旧密钥不能再算数"
        );
        assert_eq!(
            known.lookup("other.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match,
            "别的主机的记录不动"
        );
    }

    #[test]
    fn replace_keeps_revocations() {
        let file = TempFile::new();
        file.write(&format!(
            "example.com {ED25519_A}\n@revoked example.com {ED25519_C}\n"
        ));
        let known = KnownHosts::new(&file.path);
        known
            .replace("example.com", 22, &key(ED25519_B))
            .expect("更新");
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Match
        );
        assert!(
            matches!(
                known.lookup("example.com", 22, &key(ED25519_C)),
                HostKeyLookup::Revoked { .. }
            ),
            "吊销记录要留着"
        );
    }

    #[test]
    fn replace_overrides_an_old_entry_in_the_read_only_system_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        system.write(&format!("example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert!(matches!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Changed {
                stored_in: StoredIn::System(_),
                ..
            }
        ));

        assert_eq!(
            known
                .replace("example.com", 22, &key(ED25519_B))
                .expect("更新"),
            0
        );
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Match,
            "自己的文件优先，新记录盖过系统文件里的旧记录"
        );
        assert_eq!(
            std::fs::read_to_string(&system.path).expect("读系统文件"),
            format!("example.com {ED25519_A}\n"),
            "系统文件不碰"
        );
    }

    #[test]
    fn saved_fingerprint_reads_the_entry() {
        let file = TempFile::new();
        let known = KnownHosts::new(&file.path);
        assert!(known.saved_fingerprint("example.com", 22).is_none());
        known
            .append("example.com", 22, &key(ED25519_A))
            .expect("写入");
        assert_eq!(
            known.saved_fingerprint("example.com", 22).as_deref(),
            Some(fingerprint_of(&key(ED25519_A)).as_str())
        );
    }

    #[test]
    fn a_negated_pattern_excludes_the_host_even_if_a_wildcard_matches() {
        // R-9 F09：`*.example.com,!blocked.example.com` 不能让 blocked 命中通配的信任。
        let file = TempFile::new();
        file.write(&format!("*.example.com,!blocked.example.com {ED25519_A}\n"));
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("web.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        assert_eq!(
            known.lookup("blocked.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown,
            "否定模式命中，整行都不适用"
        );
        // 否定项写在前面也一样；只有否定项的行不匹配任何主机。
        file.write(&format!(
            "!blocked.example.com,*.example.com {ED25519_A}\n!other.example.com {ED25519_B}\n"
        ));
        assert_eq!(
            known.lookup("blocked.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown
        );
        assert_eq!(
            known.lookup("web.example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        // forget 用同一套匹配：被否定排除的主机不会误删这一行。
        assert_eq!(known.forget("blocked.example.com", 22).expect("forget"), 0);
    }

    #[test]
    fn an_unreadable_known_hosts_is_not_treated_as_an_unknown_host() {
        // R-9 F10：文件存在但读不了（这里用"其实是个目录"模拟 I/O 错误），不能退化成"没见过"。
        let dir = crate::test_util::TestDir::new("kh-unreadable");
        let our_file = dir.join("known_hosts");
        std::fs::create_dir_all(&our_file).expect("建目录");
        let known = KnownHosts::new(&our_file);
        let lookup = known.lookup("example.com", 22, &key(ED25519_A));
        assert!(
            matches!(lookup, HostKeyLookup::Unreadable { .. }),
            "{lookup:?}"
        );
        assert!(lookup.must_block());
        assert!(known.forget("example.com", 22).is_err(), "删不了要报错");

        // 系统文件读不了也一样（自己的文件不存在）。
        let missing = dir.join("missing_known_hosts");
        let known = KnownHosts::new(&missing).with_read_only(&our_file);
        assert!(matches!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unreadable { .. }
        ));
    }

    #[test]
    fn non_utf8_comments_do_not_hide_the_entries() {
        // 本地编码写的注释（GBK 之类）不能让整份文件读不出来。
        let file = TempFile::new();
        let mut bytes = b"# \xd6\xd0\xce\xc4\n".to_vec();
        bytes.extend_from_slice(format!("example.com {ED25519_A}\n").as_bytes());
        std::fs::write(&file.path, bytes).expect("写文件");
        let known = KnownHosts::new(&file.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
    }

    #[test]
    fn saved_fingerprint_skips_a_missing_own_file_and_reads_the_system_file() {
        // 第一次连接之前自己的 known_hosts 还不存在：界面上的指纹要能从系统文件显示出来，
        // 不能因为第一个文件读不到就整个放弃（R-8 第 11 节）。
        let dir = crate::test_util::TestDir::new("kh-missing");
        let our_file = dir.join("known_hosts"); // 故意不创建
        let system = TempFile::new();
        system.write(&format!("example.com {ED25519_A}\n"));

        let known = KnownHosts::new(&our_file).with_read_only(&system.path);
        assert_eq!(
            known.saved_fingerprint("example.com", 22).as_deref(),
            Some(fingerprint_of(&key(ED25519_A)).as_str()),
            "自己的文件不存在时要继续看只读的系统文件"
        );
        assert!(!our_file.exists(), "只读查询不能创建自己的文件");
    }

    #[test]
    fn host_pattern_uses_brackets_only_for_non_default_port() {
        assert_eq!(host_pattern("example.com", 22), "example.com");
        assert_eq!(host_pattern("example.com", 2222), "[example.com]:2222");
        assert_eq!(host_pattern("::1", 22), "::1");
        assert_eq!(host_pattern("::1", 2222), "[::1]:2222");
    }

    impl TempFile {
        fn read_count(&self, host: &str) -> usize {
            self.read()
                .lines()
                .filter(|line| line.starts_with(host))
                .count()
        }
    }

    // ---------- 自己文件和系统文件的优先级（M1-04 验收结论）----------

    #[test]
    fn our_file_wins_over_the_system_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        ours.write(&format!("example.com {ED25519_A}\n"));
        // 系统文件里是另一把钥匙：自己的文件优先，直接判匹配
        system.write(&format!("example.com {ED25519_B}\n"));

        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match,
            "自己文件里有同类型的记录时只看自己的"
        );

        // 反过来：自己文件里是旧的，系统文件里的才对 → 仍然按自己的判断（变化）
        let ours2 = TempFile::new();
        let system2 = TempFile::new();
        ours2.write(&format!("example.com {ED25519_B}\n"));
        system2.write(&format!("example.com {ED25519_A}\n"));
        let known2 = KnownHosts::new(&ours2.path).with_read_only(&system2.path);
        match known2.lookup("example.com", 22, &key(ED25519_A)) {
            HostKeyLookup::Changed { stored_in, .. } => assert_eq!(stored_in, StoredIn::Our),
            other => panic!("应该报指纹变化，实际 {other:?}"),
        }
    }

    #[test]
    fn change_from_the_system_file_is_marked_and_gives_a_hint() {
        let ours = TempFile::new();
        let system = TempFile::new();
        // 自己的文件里没有这台主机，只有系统文件里有旧记录
        system.write(&format!("example.com {ED25519_B}\n"));

        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        match known.lookup("example.com", 22, &key(ED25519_A)) {
            HostKeyLookup::Changed {
                stored_fingerprint,
                stored_in,
            } => {
                assert_eq!(stored_fingerprint, fingerprint_of(&key(ED25519_B)));
                assert_eq!(stored_in, StoredIn::System(system.path.clone()));
                let hint = stored_in
                    .forget_hint("example.com", 22)
                    .expect("系统文件要给提示");
                assert!(hint.contains("ssh-keygen -R"), "提示要能直接用：{hint}");
                assert!(
                    hint.contains("\"example.com\""),
                    "22 端口不带方括号：{hint}"
                );
                // 临时文件名带 "zssh-kh-"，真机上是 %USERPROFILE%\.ssh\known_hosts
                assert!(
                    stored_in.describe().contains("zssh-kh-"),
                    "要说清在哪个文件：{}",
                    stored_in.describe()
                );
            }
            other => panic!("应该报指纹变化，实际 {other:?}"),
        }

        // 非 22 端口的写法是 "[host]:port"
        let hint = StoredIn::System(system.path.clone())
            .forget_hint("example.com", 2222)
            .expect("有提示");
        assert!(hint.contains("\"[example.com]:2222\""), "实际：{hint}");
    }

    #[test]
    fn our_file_with_other_key_type_falls_through_to_the_system_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        // 自己文件里只记过 RSA，系统文件里有这次的 ed25519
        ours.write(&format!("example.com {RSA_A}\n"));
        system.write(&format!("example.com {ED25519_A}\n"));

        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match,
            "自己文件里没有同类型的记录时应该继续看系统文件"
        );
    }

    #[test]
    fn system_file_alone_reports_other_key_types() {
        let ours = TempFile::new();
        let system = TempFile::new();
        system.write(&format!("example.com {RSA_A}\n"));

        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::OtherKeyTypes
        );
    }

    #[test]
    fn revoked_in_the_system_file_points_at_the_system_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        system.write(&format!("@revoked example.com {ED25519_A}\n"));

        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        match known.lookup("example.com", 22, &key(ED25519_A)) {
            HostKeyLookup::Revoked { stored_in, .. } => {
                assert_eq!(stored_in, StoredIn::System(system.path.clone()))
            }
            other => panic!("应该报已吊销，实际 {other:?}"),
        }
    }

    // ---- F08：扫描所有适用的记录，@revoked 在任何位置、任何文件都生效 ----

    /// 第三把 ed25519 公钥（和 A、B 都不同）。
    const ED25519_C: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH";

    fn lookup_in(content: &str, current: &str) -> HostKeyLookup {
        let file = TempFile::new();
        file.write(content);
        KnownHosts::new(&file.path).lookup("example.com", 22, &key(current))
    }

    #[test]
    fn a_revoked_line_after_the_matching_line_still_blocks() {
        let lookup = lookup_in(
            &format!(
                "example.com {ED25519_A}
@revoked example.com {ED25519_A}
"
            ),
            ED25519_A,
        );
        assert!(
            matches!(lookup, HostKeyLookup::Revoked { .. }),
            "{lookup:?}"
        );
    }

    #[test]
    fn a_revoked_line_before_the_matching_line_blocks() {
        let lookup = lookup_in(
            &format!(
                "@revoked example.com {ED25519_A}
example.com {ED25519_A}
"
            ),
            ED25519_A,
        );
        assert!(
            matches!(lookup, HostKeyLookup::Revoked { .. }),
            "{lookup:?}"
        );
    }

    #[test]
    fn revoking_an_old_key_does_not_block_the_new_one() {
        // 旧密钥泄露了被吊销，换了新密钥：新密钥必须能用。
        for content in [
            format!(
                "@revoked example.com {ED25519_A}
example.com {ED25519_B}
"
            ),
            format!(
                "example.com {ED25519_B}
@revoked example.com {ED25519_A}
"
            ),
        ] {
            assert_eq!(
                lookup_in(&content, ED25519_B),
                HostKeyLookup::Match,
                "{content}"
            );
        }
    }

    #[test]
    fn the_revoked_key_itself_is_blocked_even_if_a_new_key_is_also_listed() {
        let content = format!(
            "example.com {ED25519_B}
@revoked example.com {ED25519_A}
"
        );
        assert!(matches!(
            lookup_in(&content, ED25519_A),
            HostKeyLookup::Revoked { .. }
        ));
    }

    #[test]
    fn two_keys_of_the_same_type_are_both_accepted() {
        let content = format!(
            "example.com {ED25519_A}
example.com {ED25519_B}
"
        );
        assert_eq!(lookup_in(&content, ED25519_A), HostKeyLookup::Match);
        assert_eq!(
            lookup_in(&content, ED25519_B),
            HostKeyLookup::Match,
            "第二把被误判成密钥变了"
        );
    }

    #[test]
    fn a_third_key_is_a_change_when_two_of_that_type_are_stored() {
        let content = format!(
            "example.com {ED25519_A}
example.com {ED25519_B}
"
        );
        assert!(matches!(
            lookup_in(&content, ED25519_C),
            HostKeyLookup::Changed { .. }
        ));
    }

    #[test]
    fn a_revoked_line_of_another_key_type_is_irrelevant() {
        let content = format!(
            "@revoked example.com {RSA_A}
example.com {ED25519_A}
"
        );
        assert_eq!(lookup_in(&content, ED25519_A), HostKeyLookup::Match);
    }

    #[test]
    fn a_revoked_line_in_the_system_file_beats_a_matching_line_in_our_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        ours.write(&format!(
            "example.com {ED25519_A}
"
        ));
        system.write(&format!(
            "@revoked example.com {ED25519_A}
"
        ));
        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        match known.lookup("example.com", 22, &key(ED25519_A)) {
            HostKeyLookup::Revoked { stored_in, .. } => {
                assert_eq!(stored_in, StoredIn::System(system.path.clone()))
            }
            other => panic!("系统文件里的吊销没有生效：{other:?}"),
        }
    }

    #[test]
    fn a_revoked_line_in_our_file_beats_a_matching_line_in_the_system_file() {
        let ours = TempFile::new();
        let system = TempFile::new();
        ours.write(&format!(
            "@revoked example.com {ED25519_A}
"
        ));
        system.write(&format!(
            "example.com {ED25519_A}
"
        ));
        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert!(matches!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Revoked { .. }
        ));
    }

    #[test]
    fn our_file_still_wins_over_the_system_file_for_ordinary_records() {
        // 自己的文件里有这种类型的记录，只按自己的文件判断（M1-04 验收结论）。
        let ours = TempFile::new();
        let system = TempFile::new();
        ours.write(&format!(
            "example.com {ED25519_B}
"
        ));
        system.write(&format!(
            "example.com {ED25519_A}
"
        ));
        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        match known.lookup("example.com", 22, &key(ED25519_A)) {
            HostKeyLookup::Changed { stored_in, .. } => assert_eq!(stored_in, StoredIn::Our),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_keys_split_across_both_files_each_use_the_files_own_rule() {
        // 自己的文件放 A，系统文件放 B：当前是 A → 自己的文件一致；当前是 B →
        // 自己的文件里有同类型但不一致，按"自己的文件优先"算变了。
        let ours = TempFile::new();
        let system = TempFile::new();
        ours.write(&format!(
            "example.com {ED25519_A}
"
        ));
        system.write(&format!(
            "example.com {ED25519_B}
"
        ));
        let known = KnownHosts::new(&ours.path).with_read_only(&system.path);
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Match
        );
        assert!(matches!(
            known.lookup("example.com", 22, &key(ED25519_B)),
            HostKeyLookup::Changed { .. }
        ));
    }

    #[test]
    fn host_addresses_that_would_widen_known_hosts_are_refused() {
        for ok in [
            "web-1.example.internal",
            "10.0.0.1",
            "::1",
            "[2001:db8::1]",
            "fe80::1%eth0",
            "my_host",
        ] {
            assert!(check_host_address(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "*",
            "*.example.com",
            "a,b",
            "a b",
            "host
other",
            "!bad",
            "a|b",
            "a#b",
            "@cert",
        ] {
            assert!(check_host_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn append_refuses_a_wildcard_address() {
        let file = TempFile::new();
        let known = KnownHosts::new(&file.path);
        assert!(known.append("*", 22, &key(ED25519_A)).is_err());
        assert!(known.append("a,b", 22, &key(ED25519_A)).is_err());
        assert_eq!(
            known.lookup("example.com", 22, &key(ED25519_A)),
            HostKeyLookup::Unknown,
            "不合格的地址一条都不写"
        );
    }
}
