//! AI 的文件工具（M3-4）：`list_dir`、`read_file`、`write_file`、`stat`。
//!
//! 这里是不依赖 Tauri 的全部逻辑：
//!
//! - 纯函数：路径危险规则（[`assess_path`]）、行差异（[`make_diff`]）、读文件分段
//!   和二进制判断（[`slice_text_chunk`]）、备份文件名（[`backup_name`]）；
//! - [`RemoteFs`]：对服务器文件系统的最小抽象。生产实现是 [`SftpRemoteFs`]（包一条 SFTP
//!   通道），测试用内存实现，可以注入失败（备份失败 / 写临时文件失败 / 改名失败）；
//! - 四个工具的执行体：`tool_list_dir` / `tool_read_file` / `tool_stat`，以及写文件分成
//!   [`prepare_write`]（算出差异和危险标记，供确认卡片用）和 [`execute_write`]
//!   （备份 → 临时文件 → 改名）。
//!
//! 边界：只操作**服务器上**的文件（`RemoteFs` 就是服务器）。上传、下载会碰用户的 Windows
//! 电脑，本卡不做成 AI 工具。

use std::borrow::Borrow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::sftp::edit::MAX_EDIT_BYTES;
use crate::sftp::error::ReplaceState;
use crate::sftp::{fs as sftp_fs, FileKind, SftpChannel, SftpError};
use crate::ssh::BoxFuture;

/// `list_dir` 最多返回多少个条目，超出的注明被截断。
pub const LIST_DIR_LIMIT: usize = 1000;
/// `read_file` 单次最多返回的字节数（和 `run_command` 的输出上限一致）。
pub const READ_MAX_BYTES: usize = 64 * 1024;
/// `write_file` 的内容上限（和内置编辑器一致）。
pub const WRITE_MAX_BYTES: u64 = MAX_EDIT_BYTES;
/// 差异最多保留多少行（再长的在确认卡片里没有意义，注明被截断）。
pub const DIFF_MAX_LINES: usize = 4000;
/// 差异每处改动前后各带几行上下文。
pub const DIFF_CONTEXT: usize = 3;

// ---------------------------------------------------------------------------
// 路径危险规则（纯函数）
// ---------------------------------------------------------------------------

/// 一个路径的危险判断：只用来**标记**（确认卡片标红、审计记 dangerous），不拦截。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathVerdict {
    pub dangerous: bool,
    pub reasons: Vec<String>,
}

/// 把路径规范成绝对路径：`~` / `~/x` 换成家目录，相对路径接在家目录后面，
/// 去掉 `.`、`..`、重复的斜杠。没有家目录又需要它时，返回 `None`。
///
/// 注意：`..` 只在文本层面折叠，不碰符号链接（链接要靠 `realpath` 另查）。
pub fn resolve_path(raw: &str, home: Option<&str>) -> Option<String> {
    let raw = raw.trim();
    let absolute = if raw == "~" {
        home?.to_owned()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{}/{}", home?.trim_end_matches('/'), rest)
    } else if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("{}/{}", home?.trim_end_matches('/'), raw)
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in absolute.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// 路径本身合不合法（拒绝空路径和 NUL）。
pub fn validate_path(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("路径不能为空".to_owned());
    }
    if raw.contains('\0') {
        return Err("路径里不能有 NUL 字符".to_owned());
    }
    Ok(())
}

/// 系统目录：写进这些目录会影响系统本身。`(前缀, 说明)`，前缀按路径段匹配。
const SYSTEM_DIRS: &[(&str, &str)] = &[
    ("/etc", "系统配置目录 /etc"),
    ("/boot", "引导目录 /boot"),
    ("/usr", "系统程序目录 /usr"),
    ("/bin", "系统程序目录 /bin"),
    ("/sbin", "系统程序目录 /sbin"),
    ("/dev", "设备目录 /dev"),
    ("/proc", "内核接口目录 /proc"),
    ("/sys", "内核接口目录 /sys"),
    ("/var/spool/cron", "定时任务目录 /var/spool/cron"),
    ("/var/lib/dpkg", "软件包数据库 /var/lib/dpkg"),
    ("/var/lib/rpm", "软件包数据库 /var/lib/rpm"),
];

/// 判断一个**已经规范成绝对路径**的位置危险不危险。
///
/// 规则：`/etc/`、`/boot/`、`/usr/`、`/bin/`、`/sbin/`、`/lib*`、`/dev`、`/proc`、`/sys`、
/// 定时任务目录；任何 `.ssh/authorized_keys`；名字里带 `sudoers` 的；名字里带 `crontab` 的。
pub fn assess_absolute_path(path: &str) -> PathVerdict {
    let mut reasons = Vec::new();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    for (prefix, label) in SYSTEM_DIRS {
        let prefix_segments: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() >= prefix_segments.len()
            && segments[..prefix_segments.len()] == prefix_segments[..]
        {
            reasons.push(format!("写入系统路径：{label}"));
            break;
        }
    }
    // /lib、/lib32、/lib64、/libx32：第一级以 lib 开头。
    if let Some(first) = segments.first() {
        if first.starts_with("lib") && !reasons.iter().any(|r| r.contains("/lib")) {
            reasons.push(format!("写入系统路径：系统库目录 /{first}"));
        }
    }
    // 远程登录凭据：任何用户的 authorized_keys。
    if segments.len() >= 2 {
        let name = segments[segments.len() - 1];
        let dir = segments[segments.len() - 2];
        if dir == ".ssh" && (name == "authorized_keys" || name == "authorized_keys2") {
            reasons.push(
                "写入 SSH 登录授权文件 authorized_keys（会改变谁能登录这台服务器）".to_owned(),
            );
        }
    }
    if let Some(name) = segments.last() {
        if name.contains("sudoers") {
            reasons.push("写入 sudoers 提权配置".to_owned());
        }
        if name.contains("crontab") {
            reasons.push("写入 crontab 定时任务".to_owned());
        }
    }
    PathVerdict {
        dangerous: !reasons.is_empty(),
        reasons,
    }
}

/// 给用户或模型传来的原始路径判断危险：先规范成绝对路径再查规则；
/// 没法规范（相对路径又没有家目录）时按相对路径里的 `..` 保守处理。
pub fn assess_path(raw: &str, home: Option<&str>) -> PathVerdict {
    match resolve_path(raw, home) {
        Some(absolute) => assess_absolute_path(&absolute),
        None => {
            let escapes = raw.split('/').any(|s| s == "..");
            if escapes {
                PathVerdict {
                    dangerous: true,
                    reasons: vec!["路径里有 ..，又无法确定起点，可能落在系统目录".to_owned()],
                }
            } else {
                PathVerdict {
                    dangerous: false,
                    reasons: Vec::new(),
                }
            }
        }
    }
}

/// 合并两份判断（用户给的路径 + 符号链接解析后的真实路径）。
pub fn merge_verdicts(a: PathVerdict, b: PathVerdict) -> PathVerdict {
    let mut reasons = a.reasons;
    for reason in b.reasons {
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    PathVerdict {
        dangerous: a.dangerous || b.dangerous,
        reasons,
    }
}

// ---------------------------------------------------------------------------
// 差异（纯函数）
// ---------------------------------------------------------------------------

/// 差异里的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// `ctx` 上下文 / `add` 新增 / `del` 删除 / `hunk` 一处改动的开头（`@@ -a,b +c,d @@`）。
    pub kind: String,
    pub text: String,
}

/// 新旧内容的差异（给确认卡片用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    /// 超过 [`DIFF_MAX_LINES`] 被截掉的行数（0 = 没截）。
    pub omitted_lines: usize,
    /// 新建文件（旧内容为空）：差异就是全文。
    pub is_new: bool,
}

/// 生成新旧内容的行差异。`old = None` 表示新建文件，显示全文。
/// 每处改动带 [`DIFF_CONTEXT`] 行上下文；总行数超过 [`DIFF_MAX_LINES`] 时截断并如实注明。
pub fn make_diff(old: Option<&str>, new: &str) -> FileDiff {
    let is_new = old.is_none();
    let old_text = old.unwrap_or("");
    let diff = TextDiff::from_lines(old_text, new);
    let mut lines: Vec<DiffLine> = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut omitted = 0usize;
    let push = |line: DiffLine, lines: &mut Vec<DiffLine>, omitted: &mut usize| {
        if lines.len() < DIFF_MAX_LINES {
            lines.push(line);
        } else {
            *omitted += 1;
        }
    };

    if is_new {
        for change in diff.iter_all_changes() {
            if change.tag() == ChangeTag::Insert {
                added += 1;
                push(
                    DiffLine {
                        kind: "add".to_owned(),
                        text: trim_eol(change.value()).to_owned(),
                    },
                    &mut lines,
                    &mut omitted,
                );
            }
        }
        return FileDiff {
            lines,
            added,
            removed,
            omitted_lines: omitted,
            is_new,
        };
    }

    for group in diff.grouped_ops(DIFF_CONTEXT) {
        let first = group.first();
        let last = group.last();
        if let (Some(first), Some(last)) = (first, last) {
            let old_start = first.old_range().start + 1;
            let old_len = last.old_range().end - first.old_range().start;
            let new_start = first.new_range().start + 1;
            let new_len = last.new_range().end - first.new_range().start;
            push(
                DiffLine {
                    kind: "hunk".to_owned(),
                    text: format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@"),
                },
                &mut lines,
                &mut omitted,
            );
        }
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => "ctx",
                    ChangeTag::Insert => {
                        added += 1;
                        "add"
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        "del"
                    }
                };
                let mut text = trim_eol(change.value()).to_owned();
                // 文件末尾没有换行符的差异要看得出来。
                if change.missing_newline() {
                    text.push_str("  ⏎(无换行符)");
                }
                push(
                    DiffLine {
                        kind: kind.to_owned(),
                        text,
                    },
                    &mut lines,
                    &mut omitted,
                );
            }
        }
    }
    FileDiff {
        lines,
        added,
        removed,
        omitted_lines: omitted,
        is_new,
    }
}

fn trim_eol(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .unwrap_or(line)
}

// ---------------------------------------------------------------------------
// 读文件分段与二进制判断（纯函数）
// ---------------------------------------------------------------------------

/// 一次读出来的文本段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextChunk {
    /// 是文本：`content` 是这一段，`start` 是它在文件里真正的起点（落在多字节字符中间时会
    /// 往后挪到下一个字符边界），`next_offset` 是下一段从哪读（读到文件尾是 `None`）。
    Text {
        content: String,
        start: u64,
        next_offset: Option<u64>,
    },
    /// 二进制：不返回内容。
    Binary,
}

/// 把从 `offset` 起读到的一段原始字节切成文本段。
///
/// - 含 NUL → 二进制；
/// - 不是合法 UTF-8（排除"段尾正好把一个字符切成两半"）→ 二进制；
/// - 起点落在字符中间：往后跳到下一个字符边界；
/// - 终点切在字符中间：把不完整的尾巴留给下一段（`next_offset` 指向它）。
///
/// `at_eof`：这一段读到了文件末尾（尾巴不完整就是真的坏了，算二进制）。
pub fn slice_text_chunk(data: &[u8], offset: u64, at_eof: bool) -> TextChunk {
    // 起点修正：最多跳过 3 个 UTF-8 续字节（10xxxxxx）。
    let mut skip = 0usize;
    while skip < 3 && skip < data.len() && data[skip] & 0b1100_0000 == 0b1000_0000 {
        skip += 1;
    }
    // offset == 0 时开头就不该是续字节：不跳，让后面的校验判它是二进制。
    if offset == 0 {
        skip = 0;
    }
    let body = &data[skip..];
    if body.contains(&0) {
        return TextChunk::Binary;
    }
    match std::str::from_utf8(body) {
        Ok(text) => {
            let end = offset + (skip + body.len()) as u64;
            TextChunk::Text {
                content: text.to_owned(),
                start: offset + skip as u64,
                next_offset: if at_eof { None } else { Some(end) },
            }
        }
        Err(err) => {
            // error_len 为 None：只是尾部不完整，不是坏字节。
            if err.error_len().is_none() && !at_eof {
                let good = err.valid_up_to();
                let text = std::str::from_utf8(&body[..good]).unwrap_or("");
                TextChunk::Text {
                    content: text.to_owned(),
                    start: offset + skip as u64,
                    next_offset: Some(offset + (skip + good) as u64),
                }
            } else {
                TextChunk::Binary
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 备份文件名与时间格式（纯函数）
// ---------------------------------------------------------------------------

/// Unix 秒 → `YYYYMMDD-HHMMSS`（UTC）。
pub fn compact_timestamp(unix_secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil_from_unix(unix_secs);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Unix 秒 → `YYYY-MM-DD HH:MM:SS`（UTC），列目录时给模型看。
pub fn display_timestamp(unix_secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil_from_unix(unix_secs);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// 公历日期换算（Howard Hinnant 的 civil_from_days）。
fn civil_from_unix(unix_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (secs_of_day / 3600) as u32,
        ((secs_of_day % 3600) / 60) as u32,
        (secs_of_day % 60) as u32,
    )
}

/// 目录部分和文件名部分（`/a/b/c.txt` → (`/a/b`, `c.txt`)；`/c.txt` → (`/`, `c.txt`)）。
pub fn split_dir_name(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(0) => ("/".to_owned(), path[1..].to_owned()),
        Some(cut) => (path[..cut].to_owned(), path[cut + 1..].to_owned()),
        None => (".".to_owned(), path.to_owned()),
    }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", dir.trim_end_matches('/'), name)
    }
}

/// 备份路径：同目录下的 `<文件名>.zssh-bak-<时间>`；`attempt > 0` 时加序号（同一秒备份两次）。
pub fn backup_name(path: &str, unix_secs: i64, attempt: u32) -> String {
    let (dir, name) = split_dir_name(path);
    let stamp = compact_timestamp(unix_secs);
    let file = if attempt == 0 {
        format!("{name}.zssh-bak-{stamp}")
    } else {
        format!("{name}.zssh-bak-{stamp}-{}", attempt + 1)
    };
    join_path(&dir, &file)
}

/// 解析同一个文件的备份名，返回可排序的 `(时间, 序号)`。
///
/// 只认 [`backup_name`] 生成的那种形状：`<文件名>.zssh-bak-YYYYMMDD-HHMMSS`
/// 后面最多再跟一个 `-数字`。别的文件、别的时间格式一律返回 `None`，清理时不动。
pub fn backup_sort_key(target_name: &str, candidate: &str) -> Option<(String, u64)> {
    let rest = candidate
        .strip_prefix(target_name)?
        .strip_prefix(".zssh-bak-")?;
    let (stamp, suffix) = match rest.split_once('-') {
        Some((date, after_date)) => {
            let (time, extra) = match after_date.split_once('-') {
                Some((time, extra)) => (time, Some(extra)),
                None => (after_date, None),
            };
            (format!("{date}-{time}"), extra)
        }
        None => return None,
    };
    let (date, time) = stamp.split_once('-')?;
    if date.len() != 8 || time.len() != 6 || !date.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !time.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // 时间部分要像真实时间（月份、日期、时分秒的粗检）；防的是"名字凑巧像备份"的文件。
    let month: u32 = date[4..6].parse().ok()?;
    let day: u32 = date[6..8].parse().ok()?;
    let hour: u32 = time[0..2].parse().ok()?;
    let minute: u32 = time[2..4].parse().ok()?;
    let second: u32 = time[4..6].parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let sequence = match suffix {
        None => 1,
        Some(raw) if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) => {
            raw.parse::<u64>().ok()?
        }
        Some(_) => return None,
    };
    Some((stamp, sequence))
}

/// 清理同一个文件多出来的备份，只留最近的 `keep` 份。
///
/// 返回（删掉的路径，失败说明）。失败**不影响**这次写入，调用方只把说明写进结果。
/// 只删满足 [`backup_sort_key`] 的普通文件，目录和名字相似的其他文件不碰。
pub async fn prune_backups(
    fs: &dyn RemoteFs,
    target: &str,
    keep: usize,
) -> (Vec<String>, Option<String>) {
    let keep = keep.max(1);
    let (dir, name) = split_dir_name(target);
    let entries = match fs.list(&dir).await {
        Ok(entries) => entries,
        Err(err) => {
            return (
                Vec::new(),
                Some(format!("旧备份清理失败（不影响这次写入）：{err}")),
            )
        }
    };
    let mut backups: Vec<(String, u64, String)> = entries
        .into_iter()
        .filter(|entry| entry.kind == FsKind::File)
        .filter_map(|entry| {
            let key = backup_sort_key(&name, &entry.name)?;
            Some((key.0, key.1, join_path(&dir, &entry.name)))
        })
        .collect();
    backups.sort();
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    let excess = backups.len().saturating_sub(keep);
    for (_, _, path) in backups.into_iter().take(excess) {
        match fs.remove_file(&path).await {
            Ok(()) => removed.push(path),
            Err(err) => errors.push(format!("{path}：{err}")),
        }
    }
    let note = if errors.is_empty() {
        None
    } else {
        Some(format!(
            "旧备份清理失败（不影响这次写入）：{}",
            errors.join("；")
        ))
    };
    (removed, note)
}

/// 临时文件路径：同目录（保证 rename 不跨文件系统），隐藏文件，带随机后缀。
pub fn temp_name(path: &str, random_hex: &str) -> String {
    let (dir, name) = split_dir_name(path);
    join_path(&dir, &format!(".{name}.zssh-tmp-{random_hex}"))
}

// ---------------------------------------------------------------------------
// 服务器文件系统的抽象
// ---------------------------------------------------------------------------

/// 文件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl FsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FsKind::File => "file",
            FsKind::Dir => "dir",
            FsKind::Symlink => "symlink",
            FsKind::Other => "other",
        }
    }
}

/// `stat`（跟随符号链接）的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    pub kind: FsKind,
    pub size: u64,
    pub mtime: Option<u32>,
    pub mode: Option<u32>,
    /// 服务器报出来的数字属主（SFTP v3 通常只有数字，没有名字）。
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// 列目录的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub kind: FsKind,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub mode: Option<u32>,
    pub link_target: Option<String>,
}

/// 覆盖改名失败：原因 + 服务器上现在的状态（决定能不能删临时文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameFailure {
    pub state: ReplaceState,
    pub message: String,
}

impl RenameFailure {
    /// 改名没发生、原文件没动。
    pub fn untouched(message: impl Into<String>) -> Self {
        Self {
            state: ReplaceState::Untouched,
            message: message.into(),
        }
    }
}

/// 服务器文件系统的最小抽象。错误都是给模型 / 用户看的中文文本。
pub trait RemoteFs: Send + Sync {
    /// 这台服务器的身份（用户名@主机:端口）。写文件前按它 + 路径加进程级的远端目标锁，
    /// 和传输队列、编辑器保存共用同一把；`None`（单测里的假文件系统）不加锁。
    fn server_key(&self) -> Option<String> {
        None
    }

    /// 登录目录的绝对路径（相对路径和 `~` 都从它算）。
    fn home<'a>(&'a self) -> BoxFuture<'a, Result<String, String>>;
    /// 读属性，跟随符号链接；不存在是 `Ok(None)`。
    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<FsStat>, String>>;
    /// 符号链接解析后的真实路径（路径必须存在）。
    fn realpath<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, String>>;
    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FsEntry>, String>>;
    /// 从 `offset` 起读最多 `len` 字节（到文件尾就少读）。
    fn read_range<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>>;
    /// 读整个文件，超过 `limit` 报错（不截断）。
    fn read_all<'a>(&'a self, path: &'a str, limit: u64) -> BoxFuture<'a, Result<Vec<u8>, String>>;
    /// 创建（或截断）并写入全部内容，刷盘；给了 `mode` 就设权限。
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// 把 `from` 改名成 `to`，`to` 已存在就覆盖。失败时 [`RenameFailure::state`] 说明服务器上现在的状态。
    fn rename_over<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> BoxFuture<'a, Result<(), RenameFailure>>;
    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// 改属主（`setstat` 的 uid/gid）。服务器不允许时返回错误，调用方要把
    /// "属主变了"写进结果，不能悄悄算了。
    fn set_owner<'a>(
        &'a self,
        path: &'a str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// 生产实现：包一条 SFTP 通道（AI 工具自己开一条，用完就关；编辑器保存借用现成的那条，不关）。
pub struct SftpRemoteFs<C: Borrow<SftpChannel> = Arc<SftpChannel>> {
    channel: C,
    close_on_drop: bool,
}

impl SftpRemoteFs<Arc<SftpChannel>> {
    pub fn new(channel: Arc<SftpChannel>) -> Self {
        Self {
            channel,
            close_on_drop: true,
        }
    }
}

impl<'a> SftpRemoteFs<&'a SftpChannel> {
    /// 借用别人的通道：用完不关。
    pub fn borrowed(channel: &'a SftpChannel) -> Self {
        Self {
            channel,
            close_on_drop: false,
        }
    }
}

impl<C: Borrow<SftpChannel>> SftpRemoteFs<C> {
    fn ch(&self) -> &SftpChannel {
        self.channel.borrow()
    }
}

impl<C: Borrow<SftpChannel>> Drop for SftpRemoteFs<C> {
    /// 每次工具调用开一条自己的通道，用完就关，不在连接上留空通道。
    fn drop(&mut self) {
        if self.close_on_drop {
            self.channel.borrow().close();
        }
    }
}

fn sftp_err(err: SftpError) -> String {
    err.to_string()
}

fn kind_from_sftp(kind: &FileKind) -> FsKind {
    match kind {
        FileKind::File => FsKind::File,
        FileKind::Dir => FsKind::Dir,
        FileKind::Symlink { .. } => FsKind::Symlink,
        FileKind::Other(_) => FsKind::Other,
    }
}

impl<C: Borrow<SftpChannel> + Send + Sync> RemoteFs for SftpRemoteFs<C> {
    fn server_key(&self) -> Option<String> {
        Some(self.ch().server_key().to_owned())
    }

    fn home<'a>(&'a self) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { self.ch().realpath(".").await.map_err(sftp_err) })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<FsStat>, String>> {
        Box::pin(async move {
            match sftp_fs::stat(self.ch(), path, true).await {
                Ok(info) => Ok(Some(FsStat {
                    kind: kind_from_sftp(&info.kind),
                    size: info.size.unwrap_or(0),
                    mtime: info.mtime,
                    mode: info.permissions,
                    uid: info.uid,
                    gid: info.gid,
                })),
                Err(SftpError::NotFound { .. }) => Ok(None),
                Err(err) => Err(sftp_err(err)),
            }
        })
    }

    fn realpath<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { self.ch().realpath(path).await.map_err(sftp_err) })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FsEntry>, String>> {
        Box::pin(async move {
            let entries = self.ch().read_dir(path).await.map_err(sftp_err)?;
            Ok(entries
                .into_iter()
                .filter(|entry| entry.name != "." && entry.name != "..")
                .map(|entry| FsEntry {
                    kind: kind_from_sftp(&entry.kind),
                    name: entry.name,
                    size: entry.size,
                    mtime: entry.mtime,
                    mode: entry.permissions,
                    link_target: entry.link_target,
                })
                .collect())
        })
    }

    fn read_range<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let handle = self.ch().open_read(path).await.map_err(sftp_err)?;
            let chunk = self.ch().caps().max_read_len().max(1) as usize;
            let mut out: Vec<u8> = Vec::with_capacity(len.min(READ_MAX_BYTES + 8));
            let mut pos = offset;
            let result: Result<(), SftpError> = loop {
                if out.len() >= len {
                    break Ok(());
                }
                let want = (len - out.len()).min(chunk) as u32;
                match self.ch().read_at(&handle, pos, want).await {
                    Ok(piece) if piece.is_empty() => break Ok(()),
                    Ok(piece) => {
                        pos += piece.len() as u64;
                        out.extend_from_slice(&piece);
                    }
                    Err(err) => break Err(err),
                }
            };
            let _ = self.ch().close_handle(&handle).await;
            result.map_err(sftp_err)?;
            out.truncate(len);
            Ok(out)
        })
    }

    fn read_all<'a>(&'a self, path: &'a str, limit: u64) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            sftp_fs::read_bytes(self.ch(), path, limit)
                .await
                .map_err(sftp_err)
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            sftp_fs::write_bytes(self.ch(), path, data, u64::MAX)
                .await
                .map_err(sftp_err)?;
            if let Some(mode) = mode {
                self.ch()
                    .set_permissions(path, mode & 0o7777)
                    .await
                    .map_err(sftp_err)?;
            }
            Ok(())
        })
    }

    fn rename_over<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> BoxFuture<'a, Result<(), RenameFailure>> {
        Box::pin(async move {
            self.ch()
                .rename(from, to, true)
                .await
                .map(|_| ())
                .map_err(|err| match err {
                    SftpError::ReplaceFailed { state, message } => RenameFailure { state, message },
                    // 连接在改名的过程中断了：改名可能已经发生，不能当成"没动"。
                    other if other.is_disconnect() => RenameFailure {
                        state: ReplaceState::Uncertain,
                        message: other.to_string(),
                    },
                    // 服务器明确拒绝：改名没有发生。
                    other => RenameFailure::untouched(other.to_string()),
                })
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.ch().remove_file(path).await.map_err(sftp_err) })
    }

    fn set_owner<'a>(
        &'a self,
        path: &'a str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.ch().set_owner(path, uid, gid).await.map_err(sftp_err) })
    }
}

// ---------------------------------------------------------------------------
// list_dir / stat / read_file
// ---------------------------------------------------------------------------

/// 八进制权限文本（`0644`）。
pub fn mode_text(mode: Option<u32>) -> String {
    match mode {
        Some(bits) => format!("{:04o}", bits & 0o7777),
        None => "?".to_owned(),
    }
}

/// `list_dir` 的结果（序列化后原样走桥）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirResult {
    pub path: String,
    pub entries: Vec<ListedEntry>,
    /// 目录里一共多少项（超过 [`LIST_DIR_LIMIT`] 时大于 `entries.len()`）。
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedEntry {
    pub name: String,
    /// `file` / `dir` / `symlink` / `other`
    #[serde(rename = "type")]
    pub kind: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

pub async fn tool_list_dir(
    fs: &dyn RemoteFs,
    raw_path: &str,
) -> Result<(ListDirResult, String), String> {
    validate_path(raw_path)?;
    let home = fs.home().await.ok();
    let path = resolve_path(raw_path, home.as_deref())
        .ok_or_else(|| "无法确定登录目录，请用绝对路径".to_owned())?;
    match fs.stat(&path).await? {
        None => return Err(format!("目录不存在：{path}")),
        Some(stat) if stat.kind != FsKind::Dir => {
            return Err(format!("不是目录：{path}（想看文件内容请用 read_file）"))
        }
        Some(_) => {}
    }
    let mut entries = fs.list(&path).await?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let total = entries.len();
    let truncated = total > LIST_DIR_LIMIT;
    entries.truncate(LIST_DIR_LIMIT);
    let listed = entries
        .into_iter()
        .map(|entry| ListedEntry {
            name: entry.name,
            kind: entry.kind.as_str().to_owned(),
            size: entry.size,
            modified: entry.mtime.map(|t| display_timestamp(i64::from(t))),
            mode: mode_text(entry.mode),
            link_target: entry.link_target,
        })
        .collect();
    Ok((
        ListDirResult {
            path: path.clone(),
            entries: listed,
            total,
            truncated,
        },
        path,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatResult {
    pub path: String,
    pub exists: bool,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub mode: Option<String>,
}

pub async fn tool_stat(fs: &dyn RemoteFs, raw_path: &str) -> Result<(StatResult, String), String> {
    validate_path(raw_path)?;
    let home = fs.home().await.ok();
    let path = resolve_path(raw_path, home.as_deref())
        .ok_or_else(|| "无法确定登录目录，请用绝对路径".to_owned())?;
    let result = match fs.stat(&path).await? {
        None => StatResult {
            path: path.clone(),
            exists: false,
            kind: None,
            size: None,
            modified: None,
            mode: None,
        },
        Some(stat) => StatResult {
            path: path.clone(),
            exists: true,
            kind: Some(stat.kind.as_str().to_owned()),
            size: Some(stat.size),
            modified: stat.mtime.map(|t| display_timestamp(i64::from(t))),
            mode: Some(mode_text(stat.mode)),
        },
    };
    Ok((result, path))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub path: String,
    /// 二进制文件：不返回内容。
    pub binary: bool,
    pub content: String,
    /// 这一段在文件里的起点（字节）。
    pub offset: u64,
    /// 这一段的字节数。
    pub returned_bytes: usize,
    /// 文件总大小（字节）。
    pub size: u64,
    /// 还有后续时，下一次 `offset` 填这个值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
}

pub async fn tool_read_file(
    fs: &dyn RemoteFs,
    raw_path: &str,
    offset: Option<u64>,
    max_bytes: Option<usize>,
) -> Result<ReadFileResult, String> {
    validate_path(raw_path)?;
    let home = fs.home().await.ok();
    let path = resolve_path(raw_path, home.as_deref())
        .ok_or_else(|| "无法确定登录目录，请用绝对路径".to_owned())?;
    let stat = match fs.stat(&path).await? {
        None => return Err(format!("文件不存在：{path}")),
        Some(stat) => stat,
    };
    match stat.kind {
        FsKind::File => {}
        FsKind::Dir => return Err(format!("{path} 是目录（想列出内容请用 list_dir）")),
        _ => return Err(format!("{path} 不是普通文件，不能读取")),
    }
    let offset = offset.unwrap_or(0);
    // 下限 4 字节：保证至少能放下一个 UTF-8 字符，分段读才有进展。
    let len = max_bytes.unwrap_or(READ_MAX_BYTES).clamp(4, READ_MAX_BYTES);
    if offset >= stat.size {
        return Ok(ReadFileResult {
            path,
            binary: false,
            content: String::new(),
            offset: stat.size.min(offset),
            returned_bytes: 0,
            size: stat.size,
            next_offset: None,
        });
    }
    let data = fs.read_range(&path, offset, len).await?;
    let at_eof = offset + data.len() as u64 >= stat.size;
    match slice_text_chunk(&data, offset, at_eof) {
        TextChunk::Binary => Ok(ReadFileResult {
            path,
            binary: true,
            content: String::new(),
            offset,
            returned_bytes: 0,
            size: stat.size,
            next_offset: None,
        }),
        TextChunk::Text {
            content,
            start,
            next_offset,
        } => Ok(ReadFileResult {
            path,
            binary: false,
            returned_bytes: content.len(),
            content,
            offset: start,
            size: stat.size,
            next_offset,
        }),
    }
}

// ---------------------------------------------------------------------------
// write_file：准备（差异 + 危险标记）与执行（备份 → 临时文件 → 改名）
// ---------------------------------------------------------------------------

/// 写之前算好的东西：确认卡片显示它，执行时按它办。
#[derive(Debug, Clone)]
pub struct WritePlan {
    /// 用户 / 模型给的原始路径。
    pub requested_path: String,
    /// 最终要写的文件（相对路径、`~` 和符号链接都解析过）。
    pub target: String,
    /// 现有文件的内容；新建为 `None`。
    pub old: Option<Vec<u8>>,
    /// 现有文件的权限位（覆盖时新文件沿用）。
    pub old_mode: Option<u32>,
    /// 现有文件的属主/属组（改名会把它换成登录用户，写完要改回去）。
    pub old_uid: Option<u32>,
    pub old_gid: Option<u32>,
    pub new_content: String,
    pub diff: FileDiff,
    pub danger: PathVerdict,
}

impl WritePlan {
    pub fn is_new(&self) -> bool {
        self.old.is_none()
    }
}

/// 写文件的准备：检查路径、读现有内容、生成差异和危险标记。**不改服务器上的任何东西。**
pub async fn prepare_write(
    fs: &dyn RemoteFs,
    raw_path: &str,
    content: &str,
    create_only: bool,
) -> Result<WritePlan, String> {
    validate_path(raw_path)?;
    if content.len() as u64 > WRITE_MAX_BYTES {
        return Err(format!(
            "内容有 {} 字节，超过 write_file 的上限 {} 字节",
            content.len(),
            WRITE_MAX_BYTES
        ));
    }
    let home = fs.home().await.ok();
    let requested = resolve_path(raw_path, home.as_deref())
        .ok_or_else(|| "无法确定登录目录，请用绝对路径".to_owned())?;
    if requested.ends_with('/') || requested == "/" {
        return Err("路径要指向一个文件".to_owned());
    }

    let mut danger = assess_absolute_path(&requested);
    let mut target = requested.clone();
    let mut old = None;
    let mut old_mode = None;
    let mut old_uid = None;
    let mut old_gid = None;

    match fs.stat(&requested).await? {
        Some(stat) => {
            match stat.kind {
                FsKind::File => {}
                FsKind::Dir => return Err(format!("{requested} 是目录，不能当文件写")),
                _ => return Err(format!("{requested} 不是普通文件，不覆盖")),
            }
            if create_only {
                return Err(format!(
                    "{requested} 已经存在（create_only 只允许新建文件）"
                ));
            }
            if stat.size > WRITE_MAX_BYTES {
                return Err(format!(
                    "现有文件有 {} 字节，超过上限 {}，不覆盖",
                    stat.size, WRITE_MAX_BYTES
                ));
            }
            // 符号链接：写链接指向的真实文件，不把链接换成普通文件。
            if let Ok(real) = fs.realpath(&requested).await {
                if real != requested {
                    danger = merge_verdicts(danger, assess_absolute_path(&real));
                    target = real;
                }
            }
            let bytes = fs.read_all(&target, WRITE_MAX_BYTES).await?;
            if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                return Err(format!(
                    "{target} 是二进制文件或不是 UTF-8 文本，write_file 只覆盖文本文件"
                ));
            }
            old_mode = stat.mode;
            old_uid = stat.uid;
            old_gid = stat.gid;
            old = Some(bytes);
        }
        None => {
            let (dir, _) = split_dir_name(&requested);
            match fs.stat(&dir).await? {
                Some(parent) if parent.kind == FsKind::Dir => {}
                Some(_) => return Err(format!("{dir} 不是目录")),
                None => {
                    return Err(format!(
                        "上级目录不存在：{dir}（write_file 不会自动建目录）"
                    ))
                }
            }
        }
    }

    let old_text = old
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    let diff = make_diff(old_text.as_deref(), content);
    Ok(WritePlan {
        requested_path: raw_path.to_owned(),
        target,
        old,
        old_mode,
        old_uid,
        old_gid,
        new_content: content.to_owned(),
        diff,
        danger,
    })
}

/// 改名之后属主没改回去：明确告诉用户"属主从 X 变成了 Y"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerChange {
    pub from: String,
    pub to: String,
    pub error: String,
}

fn owner_label(uid: Option<u32>, gid: Option<u32>) -> String {
    match (uid, gid) {
        (Some(uid), Some(gid)) => format!("uid {uid} / gid {gid}"),
        (Some(uid), None) => format!("uid {uid}"),
        (None, Some(gid)) => format!("gid {gid}"),
        (None, None) => "未知".to_owned(),
    }
}

/// 改名之前先让临时文件的属主和原文件一样。`None` = 一致了（本来就一样，或者改成功并核对过）；
/// `Some` = 保持不了（没权限、或者设置成功但没生效），里面是"从 X 变成 Y"。
async fn ensure_temp_owner(
    fs: &dyn RemoteFs,
    temp: &str,
    base: &ReplaceBase,
) -> Option<OwnerChange> {
    let from = owner_label(base.uid, base.gid);
    let matches = |stat: &FsStat| {
        (base.uid.is_none() || stat.uid == base.uid) && (base.gid.is_none() || stat.gid == base.gid)
    };
    let current = match fs.stat(temp).await {
        Ok(Some(stat)) => stat,
        Ok(None) => {
            return Some(OwnerChange {
                from,
                to: "未知".to_owned(),
                error: "临时文件不见了".to_owned(),
            })
        }
        Err(err) => {
            return Some(OwnerChange {
                from,
                to: "未知".to_owned(),
                error: format!("读不到临时文件的属主：{err}"),
            })
        }
    };
    if matches(&current) {
        return None;
    }
    let to = owner_label(current.uid, current.gid);
    if let Err(err) = fs.set_owner(temp, base.uid, base.gid).await {
        return Some(OwnerChange {
            from,
            to,
            error: err,
        });
    }
    // 设成功还要读回来确认（setstat 返回成功但服务器不认的情况也有）。
    match fs.stat(temp).await {
        Ok(Some(after)) if matches(&after) => None,
        Ok(Some(after)) => Some(OwnerChange {
            from,
            to: owner_label(after.uid, after.gid),
            error: "setstat 之后属主仍然不一致".to_owned(),
        }),
        _ => Some(OwnerChange {
            from,
            to,
            error: "无法确认属主是否设对".to_owned(),
        }),
    }
}

/// `write_file` 成功的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub path: String,
    pub bytes_written: usize,
    /// 新建（`true`）还是覆盖。
    pub created: bool,
    /// 覆盖时在服务器上留的备份文件；新建没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    /// 本次清理掉的旧备份数量。
    #[serde(default)]
    pub pruned_backups: usize,
    /// 旧备份清理失败时的说明（写入本身成功了）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_note: Option<String>,
    /// 属主没能改回去时的详情；改回去了或本来就没变就是 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_change: Option<OwnerChange>,
    pub dangerous: bool,
    pub danger_reasons: Vec<String>,
}

/// 被替换的原文件的样子（新文件的 `existed` 是 `false`）。
#[derive(Debug, Clone, Default)]
pub struct ReplaceBase {
    pub existed: bool,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// 把 `bytes` 写成 `target`：先写同目录的临时文件、核对大小、再改名覆盖；任何一步失败都
/// 清理临时文件、原文件保持原样。改名之后把原文件的属主改回去（权限不够就把"从 X 变成 Y"
/// 作为返回值带出来）。AI 的 `write_file` 和编辑器保存的上传共用这一段。
pub async fn replace_via_temp(
    fs: &dyn RemoteFs,
    target: &str,
    bytes: &[u8],
    base: &ReplaceBase,
    random_hex: &str,
) -> Result<Option<OwnerChange>, String> {
    match replace_via_temp_with(fs, target, bytes, base, random_hex, OwnerPolicy::Allow).await {
        Ok(change) => Ok(change),
        Err(ReplaceError::Failed(message)) => Err(message),
        // `Allow` 不会走到这里；万一走到就当失败说清楚。
        Err(ReplaceError::OwnerWouldChange(change)) => Err(format!(
            "保存后属主会从 {} 变成 {}：{}",
            change.from, change.to, change.error
        )),
    }
}

/// 属主怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerPolicy {
    /// 改名**之前**先把临时文件的属主设成原文件的；设不了（或者设了没生效）就**不替换**，
    /// 返回 [`ReplaceError::OwnerWouldChange`] 让调用方问用户。
    Require,
    /// 设不了也照样替换，之后如实报告属主变了（AI 写文件走这条：用户已经在确认卡片上批过）。
    Allow,
}

/// `replace_via_temp_with` 的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceError {
    Failed(String),
    /// 属主保持不了，**什么都没替换**（临时文件已清掉）。
    OwnerWouldChange(OwnerChange),
}

impl From<String> for ReplaceError {
    fn from(message: String) -> Self {
        ReplaceError::Failed(message)
    }
}

/// 把 `bytes` 写成 `target`（见 [`replace_via_temp`]），属主按 `policy` 处理。
pub async fn replace_via_temp_with(
    fs: &dyn RemoteFs,
    target: &str,
    bytes: &[u8],
    base: &ReplaceBase,
    random_hex: &str,
    policy: OwnerPolicy,
) -> Result<Option<OwnerChange>, ReplaceError> {
    replace_via_temp_checked(fs, target, bytes, base, random_hex, policy, &|| Ok(())).await
}

/// 检查点：长操作在**还没改动目标**的几个位置调它，返回 `Err(原因)` 就停在那里（清掉自己建的
/// 临时文件 / 备份），原文件保持原样。AI 写文件用它响应停止按钮和目标移出（R-9 F12）。
pub type Checkpoint<'a> = &'a (dyn Fn() -> Result<(), String> + Send + Sync);

/// 同 [`replace_via_temp_with`]，改名覆盖之前先过 `checkpoint`。
pub async fn replace_via_temp_checked(
    fs: &dyn RemoteFs,
    target: &str,
    bytes: &[u8],
    base: &ReplaceBase,
    random_hex: &str,
    policy: OwnerPolicy,
    checkpoint: Checkpoint<'_>,
) -> Result<Option<OwnerChange>, ReplaceError> {
    // 同一台服务器上的同一个路径同一时间只允许一个写：传输队列（任何标签 / 侧栏的引擎）、
    // 编辑器保存、AI 写文件共用同一把进程级的锁。拿不到就明确报错，什么都不写。
    let _write_lock = match fs.server_key() {
        Some(server) => Some(
            crate::sftp::transfer::lock::TargetLocks::remote()
                .try_acquire_remote(&server, target)
                .map_err(|err| err.to_string())?,
        ),
        None => None,
    };
    let temp = temp_name(target, random_hex);
    if let Err(err) = fs.write_file(&temp, bytes, base.mode).await {
        let _ = fs.remove_file(&temp).await;
        return Err(format!("写临时文件失败，原文件没有改动：{err}").into());
    }
    match fs.stat(&temp).await {
        Ok(Some(stat)) if stat.size == bytes.len() as u64 => {}
        Ok(_) => {
            let _ = fs.remove_file(&temp).await;
            return Err("临时文件的大小和要写的内容不一致，原文件没有改动"
                .to_owned()
                .into());
        }
        Err(err) => {
            let _ = fs.remove_file(&temp).await;
            return Err(format!("核对临时文件失败，原文件没有改动：{err}").into());
        }
    }
    // 创建模式：改名前再确认一次目标仍不存在（改名是覆盖语义）。
    if !base.existed {
        match fs.stat(target).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                let _ = fs.remove_file(&temp).await;
                return Err(format!("{target} 刚刚被创建了，已取消写入").into());
            }
            Err(err) => {
                let _ = fs.remove_file(&temp).await;
                return Err(err.into());
            }
        }
    }
    // 改名之前先把临时文件的属主设成原文件的：设不了就不替换，让调用方先问用户。
    if base.existed && (base.uid.is_some() || base.gid.is_some()) {
        if let Some(change) = ensure_temp_owner(fs, &temp, base).await {
            if policy == OwnerPolicy::Require {
                let _ = fs.remove_file(&temp).await;
                return Err(ReplaceError::OwnerWouldChange(change));
            }
        }
    }
    // 最后一个不改动原文件的位置：之后就是替换。
    if let Err(reason) = checkpoint() {
        let _ = fs.remove_file(&temp).await;
        return Err(format!("{reason}，原文件没有改动").into());
    }
    if let Err(failure) = fs.rename_over(&temp, target).await {
        return Err(ReplaceError::Failed(match failure.state {
            ReplaceState::Untouched => {
                // 原文件没动：临时文件没用了，清掉。
                let _ = fs.remove_file(&temp).await;
                format!("提交前失败，原文件没有改动：{}", failure.message)
            }
            // 原文件已经挪开又没能恢复：两份内容都完整地在服务器上，谁也不能删。
            ReplaceState::OriginalAside { backup } => format!(
                "原文件已经挪开但没能恢复：原来的内容现在在 {backup}，新内容在 {temp}（两份都完整，都没有删除）。请手动把其中一个改名成 {target}：{}",
                failure.message
            ),
            // 连接断了，不知道改名发生没有：临时文件可能就是现在的内容，不能删。
            ReplaceState::Uncertain => format!(
                "结果不确定：改名时出了问题，{target} 可能已经是新内容，也可能还是原来的内容；新内容的临时文件保留在 {temp}，请先检查再决定：{}",
                failure.message
            ),
        }));
    }

    // 临时文件是登录用户建的，改名之后原文件的属主被换掉了：改回原值。
    let mut owner_change = None;
    if base.existed && (base.uid.is_some() || base.gid.is_some()) {
        match fs.stat(target).await {
            Ok(Some(now)) if now.uid != base.uid || now.gid != base.gid => {
                let from = owner_label(base.uid, base.gid);
                let attempted = owner_label(now.uid, now.gid);
                if let Err(err) = fs.set_owner(target, base.uid, base.gid).await {
                    owner_change = Some(OwnerChange {
                        from,
                        to: attempted,
                        error: err,
                    });
                } else {
                    // 改成功还要读回来确认（setstat 成功但服务器不认的情况也有）。
                    match fs.stat(target).await {
                        Ok(Some(after)) if after.uid == base.uid && after.gid == base.gid => {}
                        Ok(Some(after)) => {
                            owner_change = Some(OwnerChange {
                                from,
                                to: owner_label(after.uid, after.gid),
                                error: "setstat 之后属主仍然不一致".to_owned(),
                            });
                        }
                        Ok(None) => {
                            owner_change = Some(OwnerChange {
                                from,
                                to: "文件已不存在".to_owned(),
                                error: "改属主后文件不见了".to_owned(),
                            });
                        }
                        Err(err) => {
                            owner_change = Some(OwnerChange {
                                from,
                                to: attempted,
                                error: format!("无法确认属主是否改回：{err}"),
                            });
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                owner_change = Some(OwnerChange {
                    from: owner_label(base.uid, base.gid),
                    to: "未知".to_owned(),
                    error: format!("读不到改名后的属主：{err}"),
                });
            }
        }
    }
    Ok(owner_change)
}

/// 执行写入。**顺序**：校验现有文件没变 → 备份（失败就不写）→ 写临时文件 → 改名覆盖。
/// 任何一步失败都清理临时文件，原文件保持原样。
///
/// `now_unix`：备份文件名里的时间；`random_hex`：临时文件名的随机后缀；
/// `backup_keep`：每个文件保留几份备份（设置项 `ai_write_backup_keep`）。
pub async fn execute_write(
    fs: &dyn RemoteFs,
    plan: &WritePlan,
    create_only: bool,
    now_unix: i64,
    random_hex: &str,
    backup_keep: usize,
) -> Result<WriteResult, String> {
    execute_write_checked(
        fs,
        plan,
        create_only,
        now_unix,
        random_hex,
        backup_keep,
        &|| Ok(()),
    )
    .await
}

/// 同 [`execute_write`]，在还没改动目标的位置过 `checkpoint`：开始之前、备份做完之后
/// （作废就删掉刚做的备份）、改名替换之前（作废就删掉临时文件）。替换一旦开始就不再打断——
/// 半路停下比做完更糟（R-9 F12）。
pub async fn execute_write_checked(
    fs: &dyn RemoteFs,
    plan: &WritePlan,
    create_only: bool,
    now_unix: i64,
    random_hex: &str,
    backup_keep: usize,
    checkpoint: Checkpoint<'_>,
) -> Result<WriteResult, String> {
    let target = plan.target.as_str();
    checkpoint().map_err(|reason| format!("{reason}，文件没有改动"))?;
    // 确认期间文件被别人动过：用户批的差异已经不是现在的差异了，不写。
    match (&plan.old, fs.stat(target).await?) {
        (Some(expected), Some(_)) => {
            let current = fs.read_all(target, WRITE_MAX_BYTES).await?;
            if &current != expected {
                return Err(format!(
                    "{target} 在你确认期间被改动过，已取消写入（请重新读取后再试）"
                ));
            }
        }
        (Some(_), None) => {
            return Err(format!("{target} 在你确认期间被删除了，已取消写入"));
        }
        (None, Some(_)) => {
            return Err(format!(
                "{target} 在确认期间被创建了，已取消写入（不覆盖别人新建的文件）"
            ));
        }
        (None, None) => {}
    }
    if create_only && plan.old.is_some() {
        return Err(format!("{target} 已经存在"));
    }

    // 1. 备份：覆盖时先把原文件在服务器上复制一份；失败就不写。
    let mut backup_path = None;
    if let Some(old) = &plan.old {
        let mut chosen = None;
        for attempt in 0..20 {
            let candidate = backup_name(target, now_unix, attempt);
            match fs.stat(&candidate).await {
                Ok(None) => {
                    chosen = Some(candidate);
                    break;
                }
                Ok(Some(_)) => continue,
                Err(err) => return Err(format!("备份失败，没有写入：{err}")),
            }
        }
        let Some(candidate) = chosen else {
            return Err("备份失败，没有写入：同一秒内已经有太多备份文件".to_owned());
        };
        if let Err(err) = fs.write_file(&candidate, old, plan.old_mode).await {
            // 半截备份也清掉。
            let _ = fs.remove_file(&candidate).await;
            return Err(format!("备份失败，没有写入：{err}"));
        }
        // 读回来核对大小：备份必须真的是完整的。
        match fs.stat(&candidate).await {
            Ok(Some(stat)) if stat.size == old.len() as u64 => {}
            Ok(_) => {
                let _ = fs.remove_file(&candidate).await;
                return Err("备份失败，没有写入：备份文件和原文件大小不一致".to_owned());
            }
            Err(err) => {
                let _ = fs.remove_file(&candidate).await;
                return Err(format!("备份失败，没有写入：{err}"));
            }
        }
        backup_path = Some(candidate);
    }
    // 备份可能要等一阵（大文件、慢链路）：这期间作废了就不再写，刚做的备份也没用了。
    if let Err(reason) = checkpoint() {
        if let Some(backup) = &backup_path {
            let _ = fs.remove_file(backup).await;
        }
        return Err(format!("{reason}，文件没有改动"));
    }

    // 2. 写临时文件（同目录），3. 改名覆盖，4. 属主改回去。
    let bytes = plan.new_content.as_bytes();
    // 记下"是不是检查点拦下的"：只有那种情况能确定原文件没动、备份可以删。改名本身失败时
    // （结果可能不确定）备份必须留着，哪怕用户这时恰好点了停止。
    let refused = std::sync::atomic::AtomicBool::new(false);
    let tracked = || {
        let verdict = checkpoint();
        if verdict.is_err() {
            refused.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        verdict
    };
    let replaced = replace_via_temp_checked(
        fs,
        target,
        bytes,
        &ReplaceBase {
            existed: plan.old.is_some(),
            mode: plan.old_mode,
            uid: plan.old_uid,
            gid: plan.old_gid,
        },
        random_hex,
        OwnerPolicy::Allow,
        &tracked,
    )
    .await;
    let owner_change = match replaced {
        Ok(change) => change,
        Err(err) => {
            // 替换前被作废：原文件没动，刚做的备份不再有用，清掉。其它失败照旧保留备份。
            if refused.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(backup) = &backup_path {
                    let _ = fs.remove_file(backup).await;
                }
            }
            return Err(match err {
                ReplaceError::Failed(message) => message,
                // `Allow` 不会走到这里；万一走到就当失败说清楚。
                ReplaceError::OwnerWouldChange(change) => format!(
                    "保存后属主会从 {} 变成 {}：{}",
                    change.from, change.to, change.error
                ),
            });
        }
    };

    // 备份保留份数：每个文件只留最近 N 份。清理失败不影响这次写入。
    let (removed, retention_note) = if plan.old.is_some() {
        prune_backups(fs, target, backup_keep).await
    } else {
        (Vec::new(), None)
    };

    Ok(WriteResult {
        path: target.to_owned(),
        bytes_written: bytes.len(),
        created: plan.old.is_none(),
        backup_path,
        pruned_backups: removed.len(),
        retention_note,
        owner_change,
        dangerous: plan.danger.dangerous,
        danger_reasons: plan.danger.reasons.clone(),
    })
}

#[cfg(test)]
mod replace_tests;
#[cfg(test)]
pub(crate) mod testfs;
#[cfg(test)]
mod tests;
