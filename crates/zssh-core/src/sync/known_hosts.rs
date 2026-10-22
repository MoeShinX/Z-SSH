//! 自己维护的 known_hosts 的同步：按“主机 + 端口 + 密钥类型”作为记录键。
//!
//! 只同步 Z-SSH 自己那份文件（调用方传路径）；系统的 `~/.ssh/known_hosts`
//! 只读，不碰。解析规则和 [`crate::ssh::known_hosts`] 一致：跳过注释、空行、
//! `@cert-authority`，保留 `@revoked`。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use base64::Engine as _;

use super::error::SyncError;
use super::package::KnownHostRecord;

/// 解析文件内容成同步记录。同一记录键出现多次只保留第一条。
pub fn collect(content: &str, device_id: &str, updated_at: i64) -> Vec<KnownHostRecord> {
    let mut records: BTreeMap<String, KnownHostRecord> = BTreeMap::new();
    for line in content.lines() {
        let Some((marker, patterns, key_type, key)) = parse_line(line) else {
            continue;
        };
        for (host, port) in patterns.iter().filter_map(|pattern| split_pattern(pattern)) {
            let record = KnownHostRecord {
                host,
                port,
                key_type: key_type.to_owned(),
                key: key.to_owned(),
                marker: marker.map(str::to_owned),
                updated_at,
                device_id: device_id.to_owned(),
            };
            records.entry(record.key()).or_insert(record);
        }
    }
    records.into_values().collect()
}

fn parse_line(line: &str) -> Option<(Option<&str>, Vec<&str>, &str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut rest = trimmed;
    let mut marker = None;
    if let Some(after) = rest.strip_prefix('@') {
        let (name, tail) = after.split_once(char::is_whitespace)?;
        marker = match name {
            "revoked" => Some("revoked"),
            // M1 不支持证书，整行跳过（和 ssh::known_hosts 一致）。
            _ => return None,
        };
        rest = tail.trim_start();
    }
    let mut fields = rest.split_whitespace();
    let patterns = fields.next()?;
    let key_type = fields.next()?;
    let key = fields.next()?;
    if patterns.is_empty() || key_type.is_empty() || key.is_empty() {
        return None;
    }
    Some((marker, patterns.split(',').collect(), key_type, key))
}

/// `[host]:port` / `host` / 哈希主机名 → (host, port)。
fn split_pattern(pattern: &str) -> Option<(String, u16)> {
    if let Some(rest) = pattern.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(raw) => raw.parse().ok()?,
            None => 22,
        };
        return Some((host.to_owned(), port));
    }
    // 哈希主机名解不出真实主机，原样保留，端口按默认 22。
    Some((pattern.to_owned(), 22))
}

/// 同步来的一条记录能不能写成 known_hosts 的一行。不能就返回原因（只描述哪个字段
/// 不对，不带公钥内容）。
///
/// 同步包来自别的设备，记录里的字段会被原样拼成一行：任何一个字段带空白或换行，
/// 就能多出字段、多出一整行（例如再塞一条 `@cert-authority *`），把信任扩到别的主机。
/// 所以每个字段都必须是单个合法 token：
/// - 主机：逗号分隔的一个或多个模式，每个只能是主机名 / IP 字符，允许 `*` `?` 通配
///   （本机查表支持，`*.example.com` 这类导入很常见）；不收 `!` 否定——采集时一行
///   被拆成了单个模式，单独一条否定模式已经没有原来的意思。哈希主机名
///   `|1|salt|hash` 两段都必须是 base64。
/// - 密钥类型：算法名字符（字母数字和 `-._@`）。
/// - 公钥：能解开的标准 base64。
/// - 标记：只认 `revoked`；不认识的标记写出去会变成普通信任行，不能放过。
fn validate(record: &KnownHostRecord) -> Result<(), &'static str> {
    if record.port == 0 {
        return Err("端口不能是 0");
    }
    validate_host(&record.host, record.port)?;
    let key_type = &record.key_type;
    if key_type.is_empty()
        || key_type.len() > 128
        || !key_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '@'))
    {
        return Err("密钥类型不是合法的算法名");
    }
    if !is_base64(&record.key) {
        return Err("公钥不是合法的 base64");
    }
    match record.marker.as_deref() {
        None | Some("revoked") => Ok(()),
        Some(_) => Err("不认识的标记"),
    }
}

fn validate_host(host: &str, port: u16) -> Result<(), &'static str> {
    if let Some(hashed) = host.strip_prefix("|1|") {
        let (salt, hash) = hashed.split_once('|').ok_or("哈希主机名格式不对")?;
        if !is_base64(salt) || !is_base64(hash) {
            return Err("哈希主机名格式不对");
        }
        return Ok(());
    }
    let patterns: Vec<&str> = host.split(',').collect();
    // 非 22 端口要写成 `[host]:port`，括号里只能放一个主机。
    if port != 22 && patterns.len() > 1 {
        return Err("非默认端口的记录只能有一个主机");
    }
    for pattern in patterns {
        let ok = !pattern.is_empty()
            && pattern.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%' | '*' | '?')
            });
        if !ok {
            return Err("主机名里有不允许的字符");
        }
    }
    Ok(())
}

fn is_base64(text: &str) -> bool {
    !text.is_empty()
        && base64::engine::general_purpose::STANDARD
            .decode(text)
            .is_ok_and(|bytes| !bytes.is_empty())
}

fn line_for(record: &KnownHostRecord) -> String {
    let host = if record.host.starts_with("|1|") || record.port == 22 {
        // 哈希主机名和默认端口都按原样写；非 22 端口要 `[host]:port`。
        record.host.clone()
    } else {
        format!("[{}]:{}", record.host, record.port)
    };
    let marker = match record.marker.as_deref() {
        Some("revoked") => "@revoked ",
        _ => "",
    };
    format!("{marker}{host} {} {}\n", record.key_type, record.key)
}

/// 把还没有的记录追加到文件里。返回追加了几条。
///
/// 只追加，不改已有行：吊销记录、注释和其他程序写的内容都原样保留。字段不合格的
/// 记录跳过并记一条警告（见 [`validate`]）。
///
/// 现有文件读不了（权限、不是 UTF-8……）就报错、什么都不写：当成空文件的话会把
/// 已有的记录再追加一遍，还可能写进一个本来就不该动的文件。只有“文件不存在”才
/// 等于空。
pub fn append_missing(path: &Path, records: &[KnownHostRecord]) -> Result<usize, SyncError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(SyncError::Io(err)),
    };
    let present: BTreeSet<String> = collect(&existing, "local", 0)
        .into_iter()
        .map(|record| record.key())
        .collect();

    let mut lines = String::new();
    let mut appended = 0usize;
    for record in records {
        if let Err(reason) = validate(record) {
            // 不打印公钥；主机名截短，防止异常长的字段刷爆日志。
            let host: String = record.host.chars().take(80).collect();
            tracing::warn!(host = %host.escape_debug(), port = record.port, reason, "同步来的主机指纹字段不合格，没有写入 known_hosts");
            continue;
        }
        if present.contains(&record.key()) {
            continue;
        }
        lines.push_str(&line_for(record));
        appended += 1;
    }
    if appended == 0 {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(lines.as_bytes())?;
    file.flush()?;
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::package::known_host_key;

    const SAMPLE: &str = "\
# 注释
web-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa
[web-2.example.internal]:2222 ssh-rsa AAAAB3NzaC1yc2Eaaa
@revoked old.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIrevoked
@cert-authority *.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIcert
|1|abc|def ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIhash
坏行
";

    #[test]
    fn parses_hosts_ports_key_types_and_markers() {
        let records = collect(SAMPLE, "dev-a", 100);
        assert_eq!(records.len(), 4, "{records:?}");
        let by_key: BTreeMap<String, KnownHostRecord> = records
            .into_iter()
            .map(|record| (record.key(), record))
            .collect();
        let web1 = &by_key[&known_host_key("web-1.example.internal", 22, "ssh-ed25519")];
        assert_eq!(web1.marker, None);
        let web2 = &by_key[&known_host_key("web-2.example.internal", 2222, "ssh-rsa")];
        assert_eq!(web2.port, 2222);
        let revoked = &by_key[&known_host_key("old.example.internal", 22, "ssh-ed25519")];
        assert_eq!(revoked.marker.as_deref(), Some("revoked"));
        assert!(by_key.keys().any(|key| key.starts_with("|1|abc|def")));
    }

    #[test]
    fn appends_only_missing_records_and_keeps_everything_else() {
        let dir = crate::test_util::TestDir::new("kh-sync");
        let path = dir.join("known_hosts");
        std::fs::write(&path, SAMPLE).expect("写样本");

        let records = vec![
            KnownHostRecord {
                host: "web-1.example.internal".to_owned(),
                port: 22,
                key_type: "ssh-ed25519".to_owned(),
                key: "AAAAC3NzaC1lZDI1NTE5AAAAIaaa".to_owned(),
                marker: None,
                updated_at: 1,
                device_id: "dev-a".to_owned(),
            },
            KnownHostRecord {
                host: "new.example.internal".to_owned(),
                port: 2200,
                key_type: "ssh-ed25519".to_owned(),
                key: "AAAAC3NzaC1lZDI1NTE5AAAAInew".to_owned(),
                marker: None,
                updated_at: 1,
                device_id: "dev-a".to_owned(),
            },
            KnownHostRecord {
                host: "bad.example.internal".to_owned(),
                port: 22,
                key_type: "ssh-ed25519".to_owned(),
                key: "AAAAC3NzaC1lZDI1NTE5AAAAIbad".to_owned(),
                marker: Some("revoked".to_owned()),
                updated_at: 1,
                device_id: "dev-a".to_owned(),
            },
        ];
        assert_eq!(append_missing(&path, &records).expect("追加"), 2);
        let content = std::fs::read_to_string(&path).expect("读回");
        assert!(content.contains("# 注释"), "注释要保留");
        assert!(
            content.contains("@cert-authority"),
            "不认识的标记行原样保留"
        );
        assert!(content.contains("@revoked old.example.internal"));
        assert!(content.contains("[new.example.internal]:2200 ssh-ed25519"));
        assert!(content.contains("@revoked bad.example.internal"));
        assert_eq!(
            content
                .matches("web-1.example.internal ssh-ed25519")
                .count(),
            1,
            "已有的记录不能重复追加"
        );
    }

    #[test]
    fn missing_file_is_created() {
        let tmp = crate::test_util::TestDir::new("kh-sync-dir");
        let dir = tmp.join("nested");
        let path = dir.join("known_hosts");
        let records = vec![KnownHostRecord {
            host: "a.example.internal".to_owned(),
            port: 22,
            key_type: "ssh-ed25519".to_owned(),
            key: "AAAAC3NzaC1lZDI1NTE5AAAAIaaa".to_owned(),
            marker: None,
            updated_at: 1,
            device_id: "dev-a".to_owned(),
        }];
        assert_eq!(append_missing(&path, &records).expect("创建"), 1);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("a.example.internal"));
    }

    fn record(host: &str, port: u16, key_type: &str, key: &str) -> KnownHostRecord {
        KnownHostRecord {
            host: host.to_owned(),
            port,
            key_type: key_type.to_owned(),
            key: key.to_owned(),
            marker: None,
            updated_at: 1,
            device_id: "dev-a".to_owned(),
        }
    }

    const KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAInew";

    #[test]
    fn records_with_injected_fields_are_skipped_and_never_written() {
        let dir = crate::test_util::TestDir::new("kh-sync-inject");
        let path = dir.join("known_hosts");
        std::fs::write(
            &path,
            "web-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa
",
        )
        .expect("写样本");
        let mut bad_marker = record("m.example.internal", 22, "ssh-ed25519", KEY);
        bad_marker.marker = Some("cert-authority".to_owned());
        let records = vec![
            // 主机字段里藏换行，想再塞一条 CA 记录
            record(
                "a.example.internal ssh-ed25519 AAAA
@cert-authority *",
                22,
                "ssh-ed25519",
                KEY,
            ),
            record("b.example.internal\r", 22, "ssh-ed25519", KEY),
            record("c.example.internal", 22, "ssh-ed25519 extra", KEY),
            record(
                "d.example.internal",
                22,
                "ssh-ed25519
",
                KEY,
            ),
            record("e.example.internal", 22, "ssh-ed25519", "AAAA AAAA"),
            record(
                "f.example.internal",
                22,
                "ssh-ed25519",
                "AAAA
@cert-authority * x y",
            ),
            record("g.example.internal", 22, "ssh-ed25519", "not*base64"),
            record("h.example.internal", 22, "", KEY),
            record("", 22, "ssh-ed25519", KEY),
            record("[i.example.internal]:22", 2222, "ssh-ed25519", KEY),
            record("!j.example.internal", 22, "ssh-ed25519", KEY),
            record(
                "k1.example.internal,k2.example.internal",
                2222,
                "ssh-ed25519",
                KEY,
            ),
            record(
                "|1|abc|d
ef",
                22,
                "ssh-ed25519",
                KEY,
            ),
            record("l.example.internal", 0, "ssh-ed25519", KEY),
            bad_marker,
        ];
        assert_eq!(append_missing(&path, &records).expect("追加"), 0);
        let content = std::fs::read_to_string(&path).expect("读回");
        assert_eq!(
            content,
            "web-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa
",
            "不合格的记录一条都不能写进去"
        );
    }

    #[test]
    fn legal_forms_are_still_written() {
        let dir = crate::test_util::TestDir::new("kh-sync-legal");
        let path = dir.join("known_hosts");
        let hashed = "|1|c2FsdHNhbHRzYWx0c2FsdHNhbHQ=|aGFzaGhhc2hoYXNoaGFzaGhhc2g=";
        let mut revoked = record("r.example.internal", 22, "ssh-ed25519", KEY);
        revoked.marker = Some("revoked".to_owned());
        let records = vec![
            record("a.example.internal,10.0.0.1", 22, "ssh-ed25519", KEY),
            record("*.example.internal", 22, "ecdsa-sha2-nistp256", KEY),
            record("fe80::1%eth0", 2222, "sk-ssh-ed25519@openssh.com", KEY),
            record(hashed, 22, "ssh-ed25519", KEY),
            revoked,
        ];
        assert_eq!(append_missing(&path, &records).expect("追加"), 5);
        let content = std::fs::read_to_string(&path).expect("读回");
        assert!(content.contains("a.example.internal,10.0.0.1 ssh-ed25519 "));
        assert!(content.contains("*.example.internal ecdsa-sha2-nistp256 "));
        assert!(content.contains("[fe80::1%eth0]:2222 sk-ssh-ed25519@openssh.com "));
        assert!(content.contains(&format!("{hashed} ssh-ed25519 ")));
        assert!(content.contains("@revoked r.example.internal ssh-ed25519 "));
        assert_eq!(content.lines().count(), 5, "{content}");
    }

    #[test]
    fn an_unreadable_existing_file_is_an_error_not_an_empty_file() {
        let dir = crate::test_util::TestDir::new("kh-sync-unreadable");
        let path = dir.join("known_hosts");
        // 不是 UTF-8：读不了，不能当成空文件再追加一遍
        let original: &[u8] =
            b"web-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa # \xff\xfe\n";
        std::fs::write(&path, original).expect("写样本");
        let records = vec![
            record(
                "web-1.example.internal",
                22,
                "ssh-ed25519",
                "AAAAC3NzaC1lZDI1NTE5AAAAIaaa",
            ),
            record("new.example.internal", 22, "ssh-ed25519", KEY),
        ];
        let err = append_missing(&path, &records).expect_err("读不了要报错");
        assert!(matches!(err, SyncError::Io(_)), "{err:?}");
        assert_eq!(
            std::fs::read(&path).expect("读回"),
            original,
            "文件不能被改动"
        );
    }
}
