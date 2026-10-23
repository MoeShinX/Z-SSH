//! 远端压缩 / 解压（UI-13）：在服务器上跑 `tar` / `zip` / `unzip`，数据不经过本机。
//!
//! 执行方式沿用服务器端复制（[`crate::sftp::server_copy`]）：后台 exec 通道里跑命令、
//! `shell_quote` 转义、取消时关闭通道、远端写锁、先写同目录的临时名再改名。只在登录 shell
//! 是 POSIX 时才开放（探测见 [`probe_caps`]）。
//!
//! 安全与取舍：
//! - **压缩**：先写到点开头的临时名（`.<名字>.<任务标识>.zssh-part`），成功后用「不覆盖」
//!   的改名换成最终名；最终名重名时自动加 ` (1)`、` (2)`，**绝不覆盖已有文件**。
//! - **解压**：先在服务器上列出包里的条目，有绝对路径或含 `..` 段的一律拒绝；同时在目标目录
//!   里逐条核对哪些文件已经存在，交给界面问用户「覆盖 / 跳过已有文件 / 取消」。目标是新建的
//!   同名文件夹时先解到临时文件夹，成功后整个改名到位，失败 / 取消直接删掉临时文件夹。
//! - **取消**：关闭 exec 通道（OpenSSH 会终止服务器上的命令，实测见 UI-13 报告）并清理临时文件。
//!   这类命令不设短超时。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ai::shell_kind::{parse_shell_probe_output, ShellKind, SHELL_PROBE_MARKER};
use crate::sftp::error::SftpError;
use crate::sftp::path as remote_path;
use crate::sftp::server_copy::{lock_target, shell_quote, CopyAttrs, CopyDataOps};
use crate::sftp::transfer::endpoint::PART_SUFFIX;
use crate::ssh::{BoxFuture, Connection};

/// 压缩 / 解压最多等多久：实际上不设限（大目录可能压几个小时），靠用户取消。
pub const ARCHIVE_WAIT: Duration = Duration::from_secs(12 * 60 * 60);

/// 列压缩包条目时 stdout 的上限；超过它说明条目多到没法逐条检查，按不安全处理。
pub const LIST_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 普通命令 stdout / stderr 的上限（只用来取错误信息）。
const SMALL_MAX_BYTES: usize = 64 * 1024;

/// 解压界面里最多列出几个冲突的名字。
pub const CONFLICT_SAMPLE: usize = 5;

/// 列条目的命令把自己的退出码写在 stderr 的这一行里（管道的退出码是最后一条命令的）。
const LIST_EXIT_MARK: &str = "__ZSSH_LIST_EXIT__:";

/// 能力探测输出里「有这个命令」那一行的前缀。
const HAS_CMD_MARK: &str = "__ZSSH_HAS_CMD__:";

/// 错误信息里最多留 stderr 的几行、多少字符。
const STDERR_LINES: usize = 6;
const STDERR_CHARS: usize = 600;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// 用户取消：通道已经关掉，临时文件已经清理。
    Cancelled,
    /// 压缩包里有指向目录外的路径。
    UnsafeEntries,
    /// 不认识的格式 / 不能这样做（给人看的原因）。
    Unsupported(String),
    /// 命令跑了但失败，或者其它出错（给人看的原因）。
    Failed(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("已取消"),
            Self::UnsafeEntries => f.write_str("压缩包里有指向目录外的路径，已拒绝解压"),
            Self::Unsupported(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<SftpError> for ArchiveError {
    fn from(err: SftpError) -> Self {
        match err {
            SftpError::Cancelled => Self::Cancelled,
            other => Self::Failed(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// 格式、名字
// ---------------------------------------------------------------------------

/// 认得的压缩包格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    TarGz,
    Tar,
    TarXz,
    TarBz2,
    Zip,
}

/// 后缀（小写）→ 格式。
const SUFFIXES: &[(&str, ArchiveFormat)] = &[
    (".tar.gz", ArchiveFormat::TarGz),
    (".tgz", ArchiveFormat::TarGz),
    (".tar.xz", ArchiveFormat::TarXz),
    (".txz", ArchiveFormat::TarXz),
    (".tar.bz2", ArchiveFormat::TarBz2),
    (".tbz2", ArchiveFormat::TarBz2),
    (".tar", ArchiveFormat::Tar),
    (".zip", ArchiveFormat::Zip),
];

impl ArchiveFormat {
    /// 按文件名后缀认格式（不分大小写）；名字就是后缀本身（没有主名）不算。
    pub fn detect(name: &str) -> Option<Self> {
        Self::split(name).map(|(format, _)| format)
    }

    /// 文件名 → (格式, 去掉后缀后的主名)。
    fn split(name: &str) -> Option<(Self, &str)> {
        let lower = name.to_ascii_lowercase();
        for (suffix, format) in SUFFIXES {
            if lower.ends_with(suffix) && name.len() > suffix.len() {
                return Some((*format, &name[..name.len() - suffix.len()]));
            }
        }
        None
    }

    pub fn is_zip(self) -> bool {
        self == Self::Zip
    }

    /// tar 的解压缩参数字母（`z` / `J` / `j`），纯 tar 没有。
    fn tar_flag(self) -> &'static str {
        match self {
            Self::TarGz => "z",
            Self::TarXz => "J",
            Self::TarBz2 => "j",
            Self::Tar | Self::Zip => "",
        }
    }
}

/// 「解压到同名文件夹」的文件夹名：`site.tar.gz` → `site`。认不出格式、主名为空或是 `.` / `..` 时没有。
pub fn extract_folder_name(archive_name: &str) -> Option<String> {
    let (_, stem) = ArchiveFormat::split(archive_name)?;
    let stem = stem.trim_end();
    if stem.is_empty() || stem == "." || stem == ".." || stem.contains('/') {
        return None;
    }
    Some(stem.to_owned())
}

/// 压缩出来的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressFormat {
    TarGz,
    Zip,
}

impl CompressFormat {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::TarGz => ".tar.gz",
            Self::Zip => ".zip",
        }
    }
}

/// 压缩包的主名：选一个就用它的名字；选多个用当前目录名，根目录（或认不出）用 `archive`。
pub fn archive_base_name(names: &[String], current_dir: &str) -> String {
    if names.len() == 1 {
        return names[0].clone();
    }
    let dir = remote_path::file_name(current_dir);
    if dir.is_empty() || dir == "/" || dir == "." || dir == ".." {
        "archive".to_owned()
    } else {
        dir
    }
}

/// 第 `index` 个候选名：0 是 `主名.后缀`，之后是 `主名 (1).后缀`、`主名 (2).后缀`……
/// （不能用传输里的 `renamed_path`：它按最后一个点改，会把 `logs.tar.gz` 改成 `logs.tar (2).gz`。）
pub fn numbered_name(base: &str, suffix: &str, index: usize) -> String {
    if index == 0 {
        format!("{base}{suffix}")
    } else {
        format!("{base} ({index}){suffix}")
    }
}

/// 找第一个没被占用的名字（`lstat` 报不存在才算空闲；其它错误原样返回，不猜）。
pub async fn first_free_name(
    fs: &dyn ArchiveFs,
    dir: &str,
    base: &str,
    suffix: &str,
) -> Result<String, ArchiveError> {
    for index in 0..=1000 {
        let name = numbered_name(base, suffix, index);
        match fs.stat_path(&remote_path::join(dir, &name)).await {
            Ok(_) => {}
            Err(SftpError::NotFound { .. }) => return Ok(name),
            Err(err) => return Err(err.into()),
        }
    }
    Err(ArchiveError::Failed(format!(
        "「{base}」的压缩包名字试到 (1000) 还是重名，没有压缩"
    )))
}

/// 临时名：同目录、点开头、`.<任务标识>.zssh-part` 结尾（别的任务和用户自己的文件不会重名）。
pub fn temp_path(dir: &str, final_name: &str, owner: &str) -> String {
    remote_path::join(dir, &format!(".{final_name}.{owner}{PART_SUFFIX}"))
}

// ---------------------------------------------------------------------------
// 命令拼装
// ---------------------------------------------------------------------------

/// 压缩命令（要放进 [`guarded`] 里跑）：`cd '目录' && exec tar -czf '临时名' -- '名1' '名2'`（zip 同理）。
/// 名字用相对路径，包里不会带服务器上的绝对路径；`--` 挡住以 `-` 开头的名字；
/// `exec` 让后台任务的进程就是 tar 本身，取消时才能精确地终止它。
pub fn compress_command(dir: &str, temp: &str, format: CompressFormat, names: &[String]) -> String {
    let quoted_names = names
        .iter()
        .map(|name| shell_quote(name))
        .collect::<Vec<_>>()
        .join(" ");
    let tool = match format {
        CompressFormat::TarGz => "tar -czf",
        // -y：符号链接存成链接，不跟进去（和 tar 一致，也不会把选中范围外的内容打进来）。
        CompressFormat::Zip => "zip -r -q -y",
    };
    format!(
        "cd {} && exec {tool} {} -- {quoted_names}",
        shell_quote(dir),
        shell_quote(temp)
    )
}

/// 列条目的命令（只列名字）。`literal`：GNU tar 的 `--quoting-style=literal`，让中文名等不被转义成
/// 八进制；busybox 等不认这个参数，由调用方在报「不认识的参数」后去掉它重来。
fn list_command(format: ArchiveFormat, archive: &str, literal: bool) -> String {
    if format.is_zip() {
        format!("unzip -Z1 {}", shell_quote(archive))
    } else {
        let quoting = if literal {
            "--quoting-style=literal "
        } else {
            ""
        };
        format!(
            "tar {quoting}-t{}f {}",
            format.tar_flag(),
            shell_quote(archive)
        )
    }
}

/// 把一条长时间运行的命令包起来，让「关闭 exec 通道」真的能终止它。
///
/// 实测（UI-13）：只关通道，服务器上的 `tar` 照样继续跑——它不往 stdout / stderr 写，
/// 不会收到 SIGPIPE，OpenSSH 对没有终端的 exec 会话也不发 SIGHUP。但 sshd 关通道时会关掉
/// 子进程的 stdin，所以：先把原来的 stdin 存到 3 号描述符，命令放在后台（`worker` 里最后一条
/// 要 `exec`，后台进程才是工具本身），再起一个只等 stdin 读到结束的看守，读到就给它发 SIGTERM；
/// 正常结束时先把看守杀掉再用工具的退出码退出。
fn guarded(worker: &str) -> String {
    format!(
        "exec 3<&0; ( {worker} ) </dev/null & pid=$!; \
         ( read -r _ <&3; kill -TERM $pid 2>/dev/null ) & w=$!; \
         wait $pid; rc=$?; kill $w 2>/dev/null; exit $rc"
    )
}

/// 解压前的检查命令：列出条目，同时在目标目录里逐条核对是否已存在。
///
/// 列条目的命令（`tar -t` / `unzip -Z1`）输出先落到服务器上的临时文件，退出码写在 stderr 里
/// （见 [`LIST_EXIT_MARK`]）；然后逐行输出：第一个字符是状态，`d` 目录条目（以 `/` 结尾）、
/// `x` 文件已存在、`n` 文件不存在。目标目录还不存在时全部算不存在。列条目的命令同样由
/// 看守盯着（见 [`guarded`]），取消时被终止。
pub fn plan_command(format: ArchiveFormat, archive: &str, dest: &str, literal: bool) -> String {
    let list = list_command(format, archive, literal);
    let dest = shell_quote(dest);
    format!(
        "T=$(mktemp 2>/dev/null) || T=/tmp/.zssh-list.$$; trap 'rm -f \"$T\"' EXIT; \
         exec 3<&0; ( exec {list} ) >\"$T\" </dev/null & pid=$!; \
         ( read -r _ <&3; kill -TERM $pid 2>/dev/null ) & w=$!; \
         wait $pid; rc=$?; kill $w 2>/dev/null; echo \"{LIST_EXIT_MARK}$rc\" >&2; \
         if cd {dest} 2>/dev/null; then \
         while IFS= read -r p; do case \"$p\" in */) printf 'd%s\\n' \"$p\";; \
         *) if [ -e \"$p\" ] || [ -L \"$p\" ]; then printf 'x%s\\n' \"$p\"; \
         else printf 'n%s\\n' \"$p\"; fi;; esac; done <\"$T\"; \
         else while IFS= read -r p; do case \"$p\" in */) printf 'd%s\\n' \"$p\";; \
         *) printf 'n%s\\n' \"$p\";; esac; done <\"$T\"; fi"
    )
}

/// 同名文件的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// 覆盖已有文件。
    Overwrite,
    /// 跳过已有文件，其余照常解出。
    SkipExisting,
}

/// tar 解压时对已有文件的处理参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TarMode {
    Overwrite,
    /// GNU tar ≥ 1.28 的 `--skip-old-files`：安静地跳过。
    SkipOldFiles,
    /// 老版本 / busybox：`-k`，GNU tar 会对每个已有文件报错并以退出码 2 收尾，要靠 stderr 区分。
    KeepOld,
}

/// 解压命令（要放进 [`guarded`] 里跑）。`new_dir`：目标是还不存在的文件夹，先 `mkdir` 再进去解
/// （zip 用 `-d` 自己建）。stdin 由 `guarded` 接到 /dev/null，加密包要密码时直接失败而不是卡住。
fn extract_command(
    format: ArchiveFormat,
    archive: &str,
    dir: &str,
    new_dir: bool,
    tar_mode: TarMode,
    skip_existing: bool,
) -> String {
    let archive = shell_quote(archive);
    let dir = shell_quote(dir);
    if format.is_zip() {
        // 没有 -o / -n unzip 会在同名文件上等输入，所以必须二选一。
        let policy = if skip_existing { "-n" } else { "-o" };
        return format!("exec unzip -q {policy} {archive} -d {dir}");
    }
    let option = match tar_mode {
        TarMode::Overwrite => "",
        TarMode::SkipOldFiles => "--skip-old-files ",
        TarMode::KeepOld => "-k ",
    };
    let enter = if new_dir {
        format!("mkdir {dir} && cd {dir}")
    } else {
        format!("cd {dir}")
    };
    format!(
        "{enter} && exec tar {option}-x{}f {archive}",
        format.tar_flag()
    )
}

// ---------------------------------------------------------------------------
// 解析与判断
// ---------------------------------------------------------------------------

/// 命令不认识某个参数（GNU `--quoting-style` / `--skip-old-files` 在老版本和 busybox 上）。
pub fn is_unsupported_option(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "unrecognized option",
        "unrecognised option",
        "invalid option",
        "unknown option",
        "illegal option",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// stderr 的最后几行（裁剪长度），放进错误提示里。
pub fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(LIST_EXIT_MARK))
        .collect();
    let start = lines.len().saturating_sub(STDERR_LINES);
    let joined = lines[start..].join("；");
    if joined.chars().count() > STDERR_CHARS {
        let cut: String = joined
            .chars()
            .rev()
            .take(STDERR_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{cut}")
    } else {
        joined
    }
}

/// 解码 GNU tar 默认（没有 `--quoting-style=literal` 时）的转义：`\ooo` 八进制、`\n`、`\\` 等。
pub fn decode_tar_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 >= bytes.len() {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let next = bytes[index + 1];
        if (b'0'..=b'7').contains(&next) {
            let mut value = 0u32;
            let mut used = 0;
            while used < 3
                && index + 1 + used < bytes.len()
                && (b'0'..=b'7').contains(&bytes[index + 1 + used])
            {
                value = value * 8 + u32::from(bytes[index + 1 + used] - b'0');
                used += 1;
            }
            out.push((value & 0xff) as u8);
            index += 1 + used;
            continue;
        }
        let decoded = match next {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'v' => 0x0b,
            b'\\' => b'\\',
            b'"' => b'"',
            other => {
                out.push(b'\\');
                out.push(other);
                index += 2;
                continue;
            }
        };
        out.push(decoded);
        index += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 条目路径是不是指向目录外：绝对路径，或者含 `..` 段。
/// zip 里 Windows 打的包常用反斜杠分隔（`unzip` 会把它当分隔符），所以 zip 额外把 `\` 当 `/`，
/// 并拒绝 `C:` 这样的盘符开头。
pub fn entry_escapes_dir(path: &str, zip: bool) -> bool {
    let normalized;
    let path = if zip {
        normalized = path.replace('\\', "/");
        let mut chars = normalized.chars();
        if let (Some(first), Some(':')) = (chars.next(), chars.next()) {
            if first.is_ascii_alphabetic() {
                return true;
            }
        }
        normalized.as_str()
    } else {
        path
    };
    path.starts_with('/') || path.split('/').any(|part| part == "..")
}

/// 解析检查命令的输出得到的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanScan {
    /// 包里一共多少条（含目录）。
    pub total_entries: u64,
    /// 其中文件（非目录条目）多少个。
    pub total_files: u64,
    /// 目标目录里已经存在的文件个数。
    pub conflict_count: u64,
    /// 前几个已存在的文件名（给界面列）。
    pub conflict_sample: Vec<String>,
}

/// 解析 [`plan_command`] 的 stdout：同时做安全检查，有指向目录外的条目返回 `UnsafeEntries`。
pub fn scan_plan_output(stdout: &str, zip: bool) -> Result<PlanScan, ArchiveError> {
    let mut scan = PlanScan::default();
    for line in stdout.split('\n') {
        let mut chars = line.chars();
        let Some(kind) = chars.next() else {
            continue;
        };
        let path = chars.as_str();
        if !matches!(kind, 'd' | 'x' | 'n') {
            continue;
        }
        // 原样和解码后的两种写法都查：宁可多拒绝，不放过。
        if entry_escapes_dir(path, zip)
            || (!zip && entry_escapes_dir(&decode_tar_escapes(path), false))
        {
            return Err(ArchiveError::UnsafeEntries);
        }
        scan.total_entries += 1;
        if kind != 'd' {
            scan.total_files += 1;
        }
        if kind == 'x' {
            scan.conflict_count += 1;
            if scan.conflict_sample.len() < CONFLICT_SAMPLE {
                scan.conflict_sample.push(path.to_owned());
            }
        }
    }
    Ok(scan)
}

/// 从 stderr 里取出列条目命令的退出码。
fn list_exit_code(stderr: &str) -> Option<i32> {
    stderr
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(LIST_EXIT_MARK))
        .and_then(|code| code.trim().parse().ok())
}

/// 压缩命令的结果：`Ok(None)` 干净成功；`Ok(Some(提示))` 成功但有警告；`Err` 失败。
///
/// GNU tar 在源文件打包期间被改动时退出码是 1（「file changed as we read it」），
/// 包本身是完整的，这种只算警告；其它非 0 都是失败。
pub fn classify_compress(
    format: CompressFormat,
    exit_code: Option<u32>,
    stderr: &str,
) -> Result<Option<String>, String> {
    match exit_code {
        Some(0) => Ok(None),
        Some(1) if format == CompressFormat::TarGz && tar_changed_only(stderr) => Ok(Some(
            "有文件在打包期间被改动过，压缩包里是打包时读到的内容".to_owned(),
        )),
        Some(127) => Err(format!(
            "服务器上找不到 {} 命令",
            if format == CompressFormat::Zip {
                "zip"
            } else {
                "tar"
            }
        )),
        Some(code) => Err(format!(
            "压缩失败（退出码 {code}）：{}",
            stderr_tail(stderr)
        )),
        None => Err("压缩命令没有返回退出码，不知道有没有成功".to_owned()),
    }
}

/// stderr 里的每一行是不是都只是「文件在读取时被改动」之类的警告。
fn tar_changed_only(stderr: &str) -> bool {
    let mut any = false;
    for line in stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.contains("file changed as we read it") {
            any = true;
        } else {
            return false;
        }
    }
    any
}

/// `tar -k` 解压的结果：GNU tar 对每个已有文件报「File exists」并以退出码 2 收尾，
/// 但其余文件照常解出。全部 stderr 行都只是「已存在」或收尾提示才算「只是跳过了」，
/// 返回跳过的个数；出现别的错误就是真失败。
pub fn classify_keep_old(exit_code: Option<u32>, stderr: &str) -> Result<u64, String> {
    if exit_code == Some(0) {
        return Ok(0);
    }
    let mut skipped = 0u64;
    let mut other = Vec::new();
    for line in stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.contains("File exists") {
            skipped += 1;
        } else if line.contains("Exiting with failure status due to previous errors")
            || line.contains("Error exit delayed from previous errors")
        {
            // 收尾提示，不是新错误
        } else {
            other.push(line);
        }
    }
    if exit_code.is_some() && other.is_empty() && skipped > 0 {
        Ok(skipped)
    } else {
        Err(format!(
            "解压失败（退出码 {}）：{}",
            exit_code.map_or_else(|| "无".to_owned(), |code| code.to_string()),
            stderr_tail(stderr)
        ))
    }
}

// ---------------------------------------------------------------------------
// 能力探测
// ---------------------------------------------------------------------------

/// 这台主机上压缩 / 解压能做什么。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ArchiveCaps {
    /// 登录 shell 是 POSIX（不是才完全不开放）。
    pub posix: bool,
    pub tar: bool,
    pub zip: bool,
    pub unzip: bool,
}

/// 探测命令：登录 shell（复用 AI 的探针写法）+ `command -v tar zip unzip`，一次 exec 搞定。
pub fn build_caps_probe_command() -> String {
    format!(
        "exec sh -c 'SH=\"$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7)\"; \
         [ -n \"$SH\" ] || SH=\"${{SHELL:-}}\"; printf \"{SHELL_PROBE_MARKER}%s\\n\" \"$SH\"; \
         for c in tar zip unzip; do command -v \"$c\" >/dev/null 2>&1 && \
         printf \"{HAS_CMD_MARK}%s\\n\" \"$c\"; done; true'"
    )
}

/// 解析探测命令的输出。
pub fn parse_caps_probe(stdout: &str) -> ArchiveCaps {
    let has = |name: &str| {
        stdout
            .replace('\r', "")
            .lines()
            .any(|line| line.trim().strip_prefix(HAS_CMD_MARK) == Some(name))
    };
    ArchiveCaps {
        posix: parse_shell_probe_output(stdout) == Some(ShellKind::Posix),
        tar: has("tar"),
        zip: has("zip"),
        unzip: has("unzip"),
    }
}

/// 探测一次（调用方按会话缓存）。命令根本没跑成（拒绝 exec、超时、连接出错）返回 `None`，
/// 不要把这种临时失败缓存成「不支持」；跑了但认不出（Windows）是 `Some(默认值)`，全部不支持。
pub async fn try_probe_caps(connection: &Connection) -> Option<ArchiveCaps> {
    match connection
        .exec_capture(
            &build_caps_probe_command(),
            Duration::from_secs(10),
            16 * 1024,
        )
        .await
    {
        Ok(result) if !result.timed_out && !result.cancelled => {
            Some(parse_caps_probe(&result.stdout))
        }
        _ => None,
    }
}

/// 同 [`try_probe_caps`]，探测不了一律当不支持。
pub async fn probe_caps(connection: &Connection) -> ArchiveCaps {
    try_probe_caps(connection).await.unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 执行接口
// ---------------------------------------------------------------------------

/// 一条命令跑完的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: Option<u32>,
    pub stdout: String,
    pub stderr: String,
    /// stdout 超过上限被截断。
    pub stdout_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
}

/// 跑命令的方式（生产上是 SSH exec 通道；测试用内存实现，记录命令、注入结果）。
pub trait ExecRunner: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        max_stream_bytes: usize,
        cancel: &'a AtomicBool,
    ) -> BoxFuture<'a, Result<ExecOutput, ArchiveError>>;
}

impl ExecRunner for Connection {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        max_stream_bytes: usize,
        cancel: &'a AtomicBool,
    ) -> BoxFuture<'a, Result<ExecOutput, ArchiveError>> {
        Box::pin(async move {
            let result = self
                .exec_capture_cancellable(command, ARCHIVE_WAIT, max_stream_bytes, cancel)
                .await
                .map_err(|err| {
                    if cancel.load(Ordering::SeqCst) {
                        ArchiveError::Cancelled
                    } else {
                        ArchiveError::Failed(format!("没法在服务器上执行命令：{err}"))
                    }
                })?;
            Ok(ExecOutput {
                exit_code: result.exit_code,
                stdout: result.stdout,
                stderr: result.stderr,
                stdout_truncated: result.stdout_truncated_bytes > 0,
                timed_out: result.timed_out,
                cancelled: result.cancelled,
            })
        })
    }
}

/// 压缩 / 解压要用的 SFTP 操作（真实实现是任何 [`CopyDataOps`]；测试用内存实现）。
/// 方法名特意和 `CopyDataOps` 不同，两个 trait 同时在作用域里也不会二义。
pub trait ArchiveFs: Send + Sync {
    fn stat_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<CopyAttrs, SftpError>>;
    /// 改名；目标已存在就失败，**不覆盖**。
    fn rename_new<'a>(&'a self, from: &'a str, to: &'a str)
        -> BoxFuture<'a, Result<(), SftpError>>;
    /// 尽力删掉一棵树（清理自己的临时文件；失败不报错）。
    fn remove_all<'a>(&'a self, path: &'a str) -> BoxFuture<'a, ()>;
}

impl<T: CopyDataOps> ArchiveFs for T {
    fn stat_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<CopyAttrs, SftpError>> {
        CopyDataOps::lstat(self, path)
    }

    fn rename_new<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        CopyDataOps::rename(self, from, to, false)
    }

    fn remove_all<'a>(&'a self, path: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move { crate::sftp::server_copy::remove_tree(self, path).await })
    }
}

/// 压缩进度（临时文件已经写了多少字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveProgress {
    pub bytes: Option<u64>,
}

/// 进度回调。
pub type ProgressFn<'a> = &'a (dyn Fn(ArchiveProgress) + Send + Sync);

/// 清理自己的临时文件 / 文件夹：先用 `rm -rf`（大目录快得多），不行再走 SFTP 逐项删。
/// 只删名字以 `.zssh-part` 结尾的路径——别的路径无论如何不动。
async fn cleanup_temp(runner: &dyn ExecRunner, fs: &dyn ArchiveFs, path: &str) {
    if !path.ends_with(PART_SUFFIX) {
        return;
    }
    let never = AtomicBool::new(false);
    let command = format!("rm -rf -- {}", shell_quote(path));
    match runner.exec(&command, 4 * 1024, &never).await {
        Ok(output) if output.exit_code == Some(0) && !output.timed_out => {}
        _ => fs.remove_all(path).await,
    }
}

/// 跑命令，同时每秒看一眼临时文件的大小报进度。
async fn run_watching(
    runner: &dyn ExecRunner,
    fs: &dyn ArchiveFs,
    command: &str,
    watch: &str,
    cancel: &AtomicBool,
    progress: Option<ProgressFn<'_>>,
) -> Result<ExecOutput, ArchiveError> {
    let mut exec = runner.exec(command, SMALL_MAX_BYTES, cancel);
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut exec => return result,
            _ = ticker.tick() => {
                if let Some(report) = progress {
                    if let Ok(attrs) = fs.stat_path(watch).await {
                        report(ArchiveProgress { bytes: attrs.size });
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 压缩
// ---------------------------------------------------------------------------

pub struct CompressRequest<'a> {
    /// 当前目录（绝对路径）。
    pub dir: &'a str,
    /// 选中的条目名（当前目录下的名字，不含 `/`）。
    pub names: &'a [String],
    pub format: CompressFormat,
    /// 临时文件名里的任务标识。
    pub owner: &'a str,
    /// 远端写锁用的服务器身份（`SftpChannel::server_key`）。
    pub server: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressOutcome {
    /// 压缩包的完整路径。
    pub path: String,
    pub name: String,
    pub warning: Option<String>,
}

fn check_names(names: &[String]) -> Result<(), ArchiveError> {
    if names.is_empty() {
        return Err(ArchiveError::Unsupported("没有选中要压缩的条目".to_owned()));
    }
    for name in names {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            return Err(ArchiveError::Unsupported(format!(
                "「{name}」不是当前目录下的有效名字，没有压缩"
            )));
        }
    }
    Ok(())
}

/// 压缩选中的条目：写临时名 → 成功后改成最终名。失败或取消都删掉临时文件。
pub async fn compress(
    runner: &dyn ExecRunner,
    fs: &dyn ArchiveFs,
    request: &CompressRequest<'_>,
    cancel: &AtomicBool,
    progress: Option<ProgressFn<'_>>,
) -> Result<CompressOutcome, ArchiveError> {
    check_names(request.names)?;
    if !request.dir.starts_with('/') {
        return Err(ArchiveError::Unsupported(
            "当前目录不是绝对路径，没有压缩".to_owned(),
        ));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(ArchiveError::Cancelled);
    }
    let base = archive_base_name(request.names, request.dir);
    let name = first_free_name(fs, request.dir, &base, request.format.suffix()).await?;
    let final_path = remote_path::join(request.dir, &name);
    // 压缩期间占着最终名：别的任务（上传、粘贴）不能同时写这个名字。
    let _guard = lock_target(request.server, &final_path)?;
    let temp = temp_path(request.dir, &name, request.owner);
    cleanup_temp(runner, fs, &temp).await;

    let command = guarded(&compress_command(
        request.dir,
        &temp,
        request.format,
        request.names,
    ));
    let output = match run_watching(runner, fs, &command, &temp, cancel, progress).await {
        Ok(output) => output,
        Err(err) => {
            cleanup_temp(runner, fs, &temp).await;
            return Err(err);
        }
    };
    if output.cancelled || cancel.load(Ordering::SeqCst) {
        cleanup_temp(runner, fs, &temp).await;
        return Err(ArchiveError::Cancelled);
    }
    if output.timed_out {
        cleanup_temp(runner, fs, &temp).await;
        return Err(ArchiveError::Failed(
            "压缩超时，没有等到结束（服务器上可能还在继续压缩）".to_owned(),
        ));
    }
    let warning = match classify_compress(request.format, output.exit_code, &output.stderr) {
        Ok(warning) => warning,
        Err(message) => {
            cleanup_temp(runner, fs, &temp).await;
            return Err(ArchiveError::Failed(message));
        }
    };
    match fs.stat_path(&temp).await {
        Ok(attrs) if !attrs.is_dir => {}
        Ok(_) => {
            cleanup_temp(runner, fs, &temp).await;
            return Err(ArchiveError::Failed("压缩没有生成压缩包文件".to_owned()));
        }
        Err(err) => {
            cleanup_temp(runner, fs, &temp).await;
            return Err(ArchiveError::Failed(format!(
                "压缩命令结束了，但找不到压缩包：{err}"
            )));
        }
    }
    if let Err(err) = fs.rename_new(&temp, &final_path).await {
        cleanup_temp(runner, fs, &temp).await;
        return Err(ArchiveError::Failed(format!(
            "压缩包没能改成最终名字「{name}」：{err}"
        )));
    }
    Ok(CompressOutcome {
        path: final_path,
        name,
        warning,
    })
}

// ---------------------------------------------------------------------------
// 解压
// ---------------------------------------------------------------------------

/// 压缩包的大小和修改时间：检查之后、解压之前它被换掉就拒绝（检查过的才是要解的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArchiveFingerprint {
    pub size: Option<u64>,
    pub mtime: Option<u32>,
}

impl ArchiveFingerprint {
    fn of(attrs: &CopyAttrs) -> Self {
        Self {
            size: attrs.size,
            mtime: attrs.mtime,
        }
    }
}

/// 解压前的检查结果（给界面决定要不要弹冲突对话框）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExtractPlan {
    pub format: ArchiveFormat,
    pub total_entries: u64,
    pub total_files: u64,
    pub conflict_count: u64,
    pub conflict_sample: Vec<String>,
    /// 目标文件夹现在已经存在。
    pub dest_exists: bool,
    pub fingerprint: ArchiveFingerprint,
}

fn archive_format(archive: &str) -> Result<ArchiveFormat, ArchiveError> {
    ArchiveFormat::detect(&remote_path::file_name(archive)).ok_or_else(|| {
        ArchiveError::Unsupported(
            "不认识这种压缩包（支持 .tar.gz .tgz .tar .tar.xz .txz .tar.bz2 .tbz2 .zip）"
                .to_owned(),
        )
    })
}

/// 目标文件夹的状态：`Some(true)` 存在（是目录）、`Some(false)` 不存在；是文件就报错。
async fn dest_state(fs: &dyn ArchiveFs, dest: &str) -> Result<bool, ArchiveError> {
    match fs.stat_path(dest).await {
        Ok(attrs) if attrs.is_dir => Ok(true),
        Ok(_) => Err(ArchiveError::Failed(format!(
            "解压的目标「{}」已经存在，但它是个文件，不是文件夹",
            remote_path::file_name(dest)
        ))),
        Err(SftpError::NotFound { .. }) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// 检查压缩包：列条目（拒绝指向目录外的路径）并数出目标里已经存在的文件。
pub async fn plan_extract(
    runner: &dyn ExecRunner,
    fs: &dyn ArchiveFs,
    archive: &str,
    dest: &str,
    cancel: &AtomicBool,
) -> Result<ExtractPlan, ArchiveError> {
    let format = archive_format(archive)?;
    if !archive.starts_with('/') || !dest.starts_with('/') {
        return Err(ArchiveError::Unsupported(
            "路径不是绝对路径，没有解压".to_owned(),
        ));
    }
    let archive_attrs = fs
        .stat_path(archive)
        .await
        .map_err(|err| ArchiveError::Failed(format!("读不了压缩包：{err}")))?;
    if archive_attrs.is_dir {
        return Err(ArchiveError::Unsupported(
            "这是个文件夹，不是压缩包".to_owned(),
        ));
    }
    let dest_exists = dest_state(fs, dest).await?;

    let mut literal = !format.is_zip();
    let output = loop {
        let command = plan_command(format, archive, dest, literal);
        let output = runner.exec(&command, LIST_MAX_BYTES, cancel).await?;
        if output.cancelled || cancel.load(Ordering::SeqCst) {
            return Err(ArchiveError::Cancelled);
        }
        // GNU 参数不被认识（busybox / 老版本）：去掉它重来一次，输出里中文名会是转义的，
        // 下面解析时两种写法都查。
        if literal
            && list_exit_code(&output.stderr) != Some(0)
            && is_unsupported_option(&output.stderr)
        {
            literal = false;
            continue;
        }
        break output;
    };
    if output.timed_out {
        return Err(ArchiveError::Failed("检查压缩包超时".to_owned()));
    }
    match list_exit_code(&output.stderr) {
        Some(0) => {}
        Some(127) => {
            return Err(ArchiveError::Failed(format!(
                "服务器上找不到 {} 命令",
                if format.is_zip() { "unzip" } else { "tar" }
            )))
        }
        Some(code) => {
            return Err(ArchiveError::Failed(format!(
                "读不了压缩包（退出码 {code}）：{}",
                stderr_tail(&output.stderr)
            )))
        }
        None => {
            return Err(ArchiveError::Failed(format!(
                "读不了压缩包：{}",
                stderr_tail(&output.stderr)
            )))
        }
    }
    if output.stdout_truncated {
        // 条目多到没法逐条检查：宁可拒绝。
        return Err(ArchiveError::Failed(
            "压缩包里的条目太多，没法逐条检查是否安全，已拒绝解压".to_owned(),
        ));
    }
    let scan = scan_plan_output(&output.stdout, format.is_zip())?;
    Ok(ExtractPlan {
        format,
        total_entries: scan.total_entries,
        total_files: scan.total_files,
        conflict_count: scan.conflict_count,
        conflict_sample: scan.conflict_sample,
        dest_exists,
        fingerprint: ArchiveFingerprint::of(&archive_attrs),
    })
}

pub struct ExtractRequest<'a> {
    pub archive: &'a str,
    /// 解压到这个文件夹（可以还不存在）。
    pub dest: &'a str,
    pub policy: ConflictPolicy,
    /// 检查时的结果：压缩包指纹、目标当时存不存在、文件数、冲突数。
    pub plan_fingerprint: ArchiveFingerprint,
    pub plan_dest_exists: bool,
    pub plan_total_files: u64,
    pub plan_conflict_count: u64,
    pub owner: &'a str,
    pub server: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractOutcome {
    pub extracted: u64,
    pub skipped: u64,
    pub warning: Option<String>,
}

/// 实际执行一次解压命令（含 tar 的 `--skip-old-files` → `-k` 回退）。返回按 stderr 数出来的跳过个数
/// （只有 `-k` 回退时有）和警告。
async fn run_extract(
    runner: &dyn ExecRunner,
    format: ArchiveFormat,
    archive: &str,
    dir: &str,
    new_dir: bool,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
) -> Result<(Option<u64>, Option<String>), ArchiveError> {
    let skip = policy == ConflictPolicy::SkipExisting && !new_dir;
    let mode = if skip && !format.is_zip() {
        TarMode::SkipOldFiles
    } else {
        TarMode::Overwrite
    };
    let command = guarded(&extract_command(format, archive, dir, new_dir, mode, skip));
    let mut output = runner.exec(&command, SMALL_MAX_BYTES, cancel).await?;
    let mut counted_skips = None;
    let mut used_keep_old = false;
    if !output.cancelled
        && mode == TarMode::SkipOldFiles
        && output.exit_code != Some(0)
        && is_unsupported_option(&output.stderr)
    {
        // 老版本 tar 不认识 --skip-old-files：参数解析阶段就失败了，什么都没解出，可以改用 -k 重来。
        let command = guarded(&extract_command(
            format,
            archive,
            dir,
            new_dir,
            TarMode::KeepOld,
            true,
        ));
        output = runner.exec(&command, SMALL_MAX_BYTES, cancel).await?;
        used_keep_old = true;
    }
    if output.cancelled || cancel.load(Ordering::SeqCst) {
        return Err(ArchiveError::Cancelled);
    }
    if output.timed_out {
        return Err(ArchiveError::Failed(
            "解压超时，没有等到结束（服务器上可能还在继续解压）".to_owned(),
        ));
    }
    let mut warning = None;
    if used_keep_old {
        let skipped =
            classify_keep_old(output.exit_code, &output.stderr).map_err(ArchiveError::Failed)?;
        counted_skips = Some(skipped);
    } else {
        match output.exit_code {
            Some(0) => {}
            // unzip 的 1 表示「有警告但解完了」。
            Some(1) if format.is_zip() => {
                let tail = stderr_tail(&output.stderr);
                warning = Some(if tail.is_empty() {
                    "解压时有警告".to_owned()
                } else {
                    format!("解压时有警告：{tail}")
                });
            }
            Some(127) => {
                return Err(ArchiveError::Failed(format!(
                    "服务器上找不到 {} 命令",
                    if format.is_zip() { "unzip" } else { "tar" }
                )))
            }
            Some(code) => {
                return Err(ArchiveError::Failed(format!(
                    "解压失败（退出码 {code}）：{}",
                    stderr_tail(&output.stderr)
                )))
            }
            None => {
                return Err(ArchiveError::Failed(
                    "解压命令没有返回退出码，不知道有没有成功".to_owned(),
                ))
            }
        }
    }
    Ok((counted_skips, warning))
}

/// 解压。目标是新建的文件夹时先解到临时文件夹再改名到位；目标已存在时就地解压，
/// 同名文件按 `policy` 处理（取消 / 失败时已经解出的文件无法区分是新的还是原来就有的，保留不动）。
pub async fn extract(
    runner: &dyn ExecRunner,
    fs: &dyn ArchiveFs,
    request: &ExtractRequest<'_>,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    let format = archive_format(request.archive)?;
    if !request.archive.starts_with('/') || !request.dest.starts_with('/') {
        return Err(ArchiveError::Unsupported(
            "路径不是绝对路径，没有解压".to_owned(),
        ));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(ArchiveError::Cancelled);
    }
    let archive_attrs = fs
        .stat_path(request.archive)
        .await
        .map_err(|err| ArchiveError::Failed(format!("读不了压缩包：{err}")))?;
    if ArchiveFingerprint::of(&archive_attrs) != request.plan_fingerprint {
        return Err(ArchiveError::Failed(
            "压缩包在检查之后被改动过，请重新操作".to_owned(),
        ));
    }
    let dest_exists = dest_state(fs, request.dest).await?;
    if dest_exists != request.plan_dest_exists {
        return Err(ArchiveError::Failed(
            "目标文件夹的状态在检查之后变了，请重新操作".to_owned(),
        ));
    }
    // 解压期间占着目标文件夹：别的任务不能同时往里写。
    let _guard = lock_target(request.server, request.dest)?;

    let new_dir = !dest_exists;
    let (target_dir, temp) = if new_dir {
        let name = remote_path::file_name(request.dest);
        let parent = remote_path::parent(request.dest);
        let temp = temp_path(&parent, &name, request.owner);
        cleanup_temp(runner, fs, &temp).await;
        (temp.clone(), Some(temp))
    } else {
        (request.dest.to_owned(), None)
    };

    let ran = run_extract(
        runner,
        format,
        request.archive,
        &target_dir,
        new_dir,
        request.policy,
        cancel,
    )
    .await;
    let (counted_skips, warning) = match ran {
        Ok(done) => done,
        Err(err) => {
            if let Some(temp) = &temp {
                cleanup_temp(runner, fs, temp).await;
            }
            return Err(err);
        }
    };
    if let Some(temp) = &temp {
        if cancel.load(Ordering::SeqCst) {
            cleanup_temp(runner, fs, temp).await;
            return Err(ArchiveError::Cancelled);
        }
        if let Err(err) = fs.rename_new(temp, request.dest).await {
            cleanup_temp(runner, fs, temp).await;
            return Err(ArchiveError::Failed(format!(
                "解压完了，但没能把临时文件夹改成「{}」：{err}",
                remote_path::file_name(request.dest)
            )));
        }
    }
    let skipped = if request.policy == ConflictPolicy::SkipExisting && !new_dir {
        counted_skips.unwrap_or(request.plan_conflict_count)
    } else {
        0
    };
    Ok(ExtractOutcome {
        extracted: request.plan_total_files.saturating_sub(skipped),
        skipped,
        warning,
    })
}

#[cfg(test)]
mod tests;
