//! Windows 本地这一侧的兼容（M3-1 第 3 节）。
//!
//! 远端（POSIX）能有的名字，Windows 上不一定能落盘：`\ / : * ? " < > |`、以点或空格
//! 结尾、`CON` / `NUL` / `COM1` 这类保留名、大小写只差一个字母的两个文件、超过 260 个
//! 字符的路径。下载时要把这些处理好：**能改名的改名并在任务里注明**，不能含糊地丢一个
//! `os error 123` 出去。
//!
//! 这一层只做**纯计算**（名字规范化、长路径前缀、大小写冲突分配）和**本地树的遍历**，
//! 不碰网络，所以单测可以直接造临时目录验证。

use std::path::{Path, PathBuf};

use super::error::{from_io, SftpError};

/// Windows 文件名里不允许出现的字符。
const ILLEGAL_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// 设备保留名（不区分大小写；带扩展名也算，例如 `CON.txt`）。
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 名字替换用的字符（和很多命令行工具一样用下划线）。
const REPLACEMENT: char = '_';

/// 一个远端名字落到 Windows 上要变成什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalName {
    /// 最终用的名字（已经规范化过；没有变化时等于原名）。
    pub name: String,
    /// 改过名的原因（`None` = 原名就能用）。界面把它们列出来。
    pub note: Option<String>,
}

/// 这个名字能不能直接用（不能用的原因见 [`LocalName::note`]）。
pub fn local_name(remote_name: &str) -> LocalName {
    let mut notes: Vec<String> = Vec::new();
    let mut name = String::with_capacity(remote_name.len());

    for ch in remote_name.chars() {
        if ILLEGAL_CHARS.contains(&ch) || (ch as u32) < 0x20 {
            name.push(REPLACEMENT);
            let label = if (ch as u32) < 0x20 {
                format!("\\x{:02x}", ch as u32)
            } else {
                ch.to_string()
            };
            if !notes.iter().any(|note| note.contains(&label)) {
                notes.push(format!("非法字符 {label} 换成 {REPLACEMENT}"));
            }
        } else {
            name.push(ch);
        }
    }

    // 结尾的点和空格（Windows 会悄悄去掉，导致名字和预期不一致）。
    let trimmed = name.trim_end_matches(['.', ' ']);
    if trimmed.len() != name.len() {
        notes.push("结尾的点或空格去掉了".to_owned());
        name = trimmed.to_owned();
    }

    // 保留名：加个前缀（`CON` → `_CON`）。
    if is_reserved_name(&name) {
        notes.push(format!("{name} 是 Windows 保留名，加了前缀"));
        name = format!("_{name}");
    }

    if name.is_empty() {
        notes.push("名字变成空的，用 _ 代替".to_owned());
        name = "_".to_owned();
    }

    LocalName {
        name,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("；"))
        },
    }
}

/// 用户在对话框里输入的"同一目录下的新名字"（另存为、粘贴时重命名）：必须原样就是一个合法的
/// Windows 文件名，不改写、不猜。
///
/// 只挡 `/`、`\`、`.`、`..` 不够：`C:evil.txt` 带盘符，`join` 之后会把父目录整个换掉，写到进程
/// 当前目录去；`a:b` 会写进 `a` 的 NTFS 备用数据流（R-10 M1）。调用方拼出路径之后还应该再用
/// [`ensure_inside`] 核对一次。
pub fn check_child_name(name: &str) -> Result<String, SftpError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SftpError::InvalidInput("名字不能为空".to_owned()));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(SftpError::InvalidInput("名字不能是 . 或 ..".to_owned()));
    }
    if let Some(note) = local_name(trimmed).note {
        return Err(SftpError::InvalidInput(format!(
            "「{trimmed}」不能用作文件名：{note}"
        )));
    }
    Ok(trimmed.to_owned())
}

/// 是不是 Windows 保留名（按主名判断，`CON.txt` 也算）。
pub fn is_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.trim().to_ascii_uppercase();
    RESERVED_NAMES.contains(&upper.as_str())
}

/// 给本地路径加长路径前缀（超过 260 字符时 Windows 才认）。
///
/// - `C:\a\b` → `\\?\C:\a\b`
/// - `\\server\share\a` → `\\?\UNC\server\share\a`
/// - 已经是 `\\?\` 开头的原样返回；相对路径不加（`\\?\` 只认绝对路径）。
pub fn long_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\?\\") {
        return path.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix("\\\\") {
        return PathBuf::from(format!("\\\\?\\UNC\\{rest}"));
    }
    // 盘符绝对路径（`C:\`）。
    let looks_absolute = text.len() >= 3
        && text.as_bytes()[1] == b':'
        && (text.as_bytes()[2] == b'\\' || text.as_bytes()[2] == b'/');
    if looks_absolute {
        return PathBuf::from(format!("\\\\?\\{text}"));
    }
    path.to_path_buf()
}

/// 本地路径是不是该用长路径前缀（240 个字符就开始加，留点余量）。
pub fn needs_long_path(path: &Path) -> bool {
    path.to_string_lossy().chars().count() >= 240
}

/// 下载时的一份"本地名字分配"：同一个目录里落地的一批文件，处理改名和大小写冲突。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNamePlan {
    /// 远端名字 → 本地名字。
    pub assignments: Vec<(String, LocalName)>,
}

impl LocalNamePlan {
    /// 给一批远端名字分本地名字（都按文件看待）。见 [`LocalNamePlan::plan_with_kinds`]。
    pub fn plan(remote_names: &[String], existing: &[String]) -> Self {
        let remote: Vec<(String, bool)> = remote_names
            .iter()
            .map(|name| (name.clone(), false))
            .collect();
        let existing: Vec<(String, bool)> =
            existing.iter().map(|name| (name.clone(), false)).collect();
        Self::plan_with_kinds(&remote, &existing)
    }

    /// 给一批远端条目分本地名字。`remote` 和 `existing` 都是 `(名字, 是不是目录)`。
    ///
    /// 规则：先做 [`local_name`] 规范化；同一批里规范化之后（或本来）**只差大小写**的两个名字
    /// 会撞在一起（Windows 不区分大小写），后面的加 ` (2)`、` (3)` 序号。
    ///
    /// 和本地**已有的同类条目**同名不改名：文件同名是"目标已存在"，交给冲突对话框（覆盖 /
    /// 跳过 / 续传 / 重命名）；目录同名就下载进去、里面的文件各自走冲突处理。不能悄悄换成带
    /// 序号的新名字——重新下载同一个文件夹时用户就选不了覆盖或续传了（R-9 F13）。
    /// 已有条目类型不同（远端是目录、本地是同名文件，或者反过来）写不进去，照旧加序号。
    /// 加序号时挑的新名字要避开所有已有条目，免得撞上另一份无关的文件。
    pub fn plan_with_kinds(remote: &[(String, bool)], existing: &[(String, bool)]) -> Self {
        let existing: Vec<(String, bool)> = existing
            .iter()
            .map(|(name, is_dir)| (name.to_lowercase(), *is_dir))
            .collect();
        // 这一批里已经分出去的名字。
        let mut taken: Vec<String> = Vec::new();
        let mut assignments = Vec::with_capacity(remote.len());
        for (remote_name, is_dir) in remote {
            let base = local_name(remote_name);
            let mut candidate = base.name.clone();
            let mut note = base.note.clone();
            let lower = candidate.to_lowercase();
            let batch_clash = taken.contains(&lower);
            let kind_clash = existing
                .iter()
                .any(|(name, existing_dir)| *name == lower && existing_dir != is_dir);
            if batch_clash || kind_clash {
                let mut dedup = 1;
                loop {
                    dedup += 1;
                    candidate = add_suffix(&base.name, dedup);
                    let lower = candidate.to_lowercase();
                    if !taken.contains(&lower) && !existing.iter().any(|(name, _)| *name == lower) {
                        break;
                    }
                }
                let why = if batch_clash {
                    "和同一批的另一个条目同名（只差大小写）"
                } else if *is_dir {
                    "本地已有同名文件"
                } else {
                    "本地已有同名文件夹"
                };
                note = Some(match note {
                    Some(note) => format!("{note}；{why}，改成 {candidate}"),
                    None => format!("{why}，改成 {candidate}"),
                });
            }
            taken.push(candidate.to_lowercase());
            assignments.push((
                remote_name.clone(),
                LocalName {
                    name: candidate,
                    note,
                },
            ));
        }
        Self { assignments }
    }
}

/// `photo.jpg` + 2 → `photo (2).jpg`。
fn add_suffix(name: &str, index: usize) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({index}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({index})"),
    }
}

/// 本地树里的一个文件（上传时用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// 本地绝对路径。
    pub path: PathBuf,
    /// 相对根的路径（用 `/` 拼，直接拼到远端目标目录后面）。
    pub relative: String,
    pub size: u64,
}

/// 本地树的遍历结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTree {
    pub files: Vec<LocalFile>,
    /// 跳过的符号链接 / 目录联接（默认不跟着走，列出来给用户看）。
    pub skipped_links: Vec<String>,
    /// 空目录（远端的目录结构要照搬，空目录也算）。
    pub empty_dirs: Vec<String>,
}

/// 遍历本地目录（上传用）：**不跟着符号链接 / 目录联接走**，空目录也记下来。
pub fn walk_local_tree(root: &Path) -> Result<LocalTree, SftpError> {
    let mut tree = LocalTree {
        files: Vec::new(),
        skipped_links: Vec::new(),
        empty_dirs: Vec::new(),
    };
    if !root.is_dir() {
        return Err(SftpError::NotFound {
            path: root.display().to_string(),
        });
    }
    walk_dir(root, root, &mut tree, 0)?;
    Ok(tree)
}

fn walk_dir(root: &Path, dir: &Path, tree: &mut LocalTree, depth: usize) -> Result<(), SftpError> {
    // 深度上限：正常人不会传 100 层，防的是病态目录结构（也可能藏着链接环）。
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        tree.skipped_links.push(format!(
            "{}（目录太深，超过 {MAX_DEPTH} 层）",
            dir.display()
        ));
        return Ok(());
    }
    // 注意：只在真正调系统接口时才套长路径前缀，列表里的路径保持"原样"，
    // 否则 `\?\` 会混进相对路径和给用户的提示里。
    let listing = std::fs::read_dir(long_path(dir))
        .map_err(|err| from_io(err, &dir.display().to_string()))?;
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|err| from_io(err, &dir.display().to_string()))?;
        names.push(entry.file_name());
    }
    names.sort();
    if names.is_empty() && dir != root {
        tree.empty_dirs.push(relative_slash(root, dir));
    }
    for name in names {
        let child = dir.join(&name);
        let meta = match std::fs::symlink_metadata(long_path(&child)) {
            Ok(meta) => meta,
            Err(err) => return Err(from_io(err, &child.display().to_string())),
        };
        // 符号链接 / 目录联接（reparse point）：默认跳过，不跟着递归。
        if is_reparse_point(&meta) || meta.file_type().is_symlink() {
            tree.skipped_links.push(child.display().to_string());
            continue;
        }
        if meta.is_dir() {
            walk_dir(root, &child, tree, depth + 1)?;
        } else if meta.is_file() {
            tree.files.push(LocalFile {
                relative: relative_slash(root, &child),
                path: child,
                size: meta.len(),
            });
        }
        // 其它类型（设备文件等）忽略：Windows 上少见，也不该传。
    }
    Ok(())
}

/// 是不是 reparse point（符号链接或目录联接）。
#[cfg(windows)]
pub fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// 相对路径，用 `/` 拼（远端路径规则）。
fn relative_slash(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rest) => rest
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// 顶层下载的本地落点：`local_dir` + 远端路径最后一段（按 POSIX 规则取名，再按 Windows 规则
/// 规范化）。顶层文件和递归下载的子项走同一套名字处理；最后还会检查结果确实在 `local_dir`
/// 直接下面，不在就报错、什么都不写。
pub fn download_target(local_dir: &Path, remote_source: &str) -> Result<PathBuf, SftpError> {
    let trimmed = remote_source.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or_default();
    if name.is_empty() {
        return Err(SftpError::InvalidInput(format!(
            "{remote_source:?} 没有可用的文件名，没法下载到本地"
        )));
    }
    let target = local_dir.join(local_name(name).name);
    ensure_inside(local_dir, &target)?;
    Ok(target)
}

/// 检查 `target` 在 `root` 的里面（不含 `root` 自己）。比较前先做词法规范化（处理 `.`、`..`，
/// Windows 上不分大小写）；带盘符、`..` 逃出去、名字里有 `:` 的都拒绝。
pub fn ensure_inside(root: &Path, target: &Path) -> Result<(), SftpError> {
    let outside = || {
        SftpError::InvalidInput(format!(
            "目标路径 {} 不在所选目录 {} 里，已拒绝",
            target.display(),
            root.display()
        ))
    };
    // 先规范化（8.3 短路径 / 长路径、大小写、符号链接）：目标还不存在时只解析它已存在的父目录。
    let root_parts = lexical_parts(&crate::pathnorm::normalize(root)).ok_or_else(outside)?;
    let target_parts = lexical_parts(&crate::pathnorm::normalize(target)).ok_or_else(outside)?;
    if target_parts.len() <= root_parts.len() || !target_parts.starts_with(&root_parts) {
        return Err(outside());
    }
    if target_parts[root_parts.len()..]
        .iter()
        .any(|part| part.contains([':', '\\', '/']))
    {
        return Err(outside());
    }
    Ok(())
}

/// 比较用的键：规范化（`.`、`..`、大小写）之后的路径。
pub fn lexical_key(path: &Path) -> String {
    match lexical_parts(&crate::pathnorm::normalize(path)) {
        Some(parts) => parts.join("/"),
        None => path.to_string_lossy().to_lowercase(),
    }
}

/// 路径拆成小写的段（`.` 去掉、`..` 抵消上一段；`..` 越过开头返回 `None`）。
fn lexical_parts(path: &Path) -> Option<Vec<String>> {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_lowercase())
            }
            Component::RootDir => parts.push("\\".to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().to_lowercase()),
        }
    }
    Some(parts)
}

/// 用 `from` 原子地替换 `to`（`to` 已存在也行）：任何失败下旧文件或新文件至少有一个完整存在。
#[cfg(windows)]
pub fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file(from, to, true)
}

/// 把 `from` 改名成 `to`，**`to` 已存在就失败**（`AlreadyExists`），绝不替换。
/// 由系统在改名那一刻判断，不是先检查再改名。
#[cfg(windows)]
pub fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file(from, to, false)
}

#[cfg(windows)]
fn move_file(from: &Path, to: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(from), wide(to));
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }
    // SAFETY: 两个缓冲区都以 0 结尾，调用期间一直活着。
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }
        .map_err(|err| std::io::Error::from_raw_os_error(err.code().0 & 0xFFFF))
}

#[cfg(not(windows))]
pub fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    // POSIX 的 rename 本身就是原子替换。
    std::fs::rename(from, to)
}

#[cfg(not(windows))]
pub fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    // 硬链接在目标存在时失败，成功后再删掉原名：同样没有"先检查再改名"的空窗。
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

/// 新建一个空文件，**同名的东西已存在就失败**（`AlreadyExists`），绝不截断它。
/// 由系统在创建那一刻判断：先查 metadata 再 `File::create` 中间有空窗，
/// 空窗里别的程序建的同名文件会被清空；查询本身失败也不能当成"不存在"。
pub fn create_new_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(long_path(path))
        .map(|_| ())
}

/// 把 `data` 写成 `target`：先写同目录的临时文件（刷盘），再原子替换原文件。
/// 任何一步失败，原文件保持原样、临时文件被清掉。`target` 不存在也行（等于新建）。
pub fn write_atomic(target: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let tmp = target.with_file_name(format!(
        ".{name}.zssh-tmp-{}-{}",
        std::process::id(),
        // 同一个进程里连着写两次也不会撞名。
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(long_path(&tmp))?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write {
        let _ = std::fs::remove_file(long_path(&tmp));
        return Err(err);
    }
    // 原文件的只读属性 / 权限不用管：替换的是目录项，和编辑器保存的行为一致。
    if let Err(err) = replace_file(&long_path(&tmp), &long_path(target)) {
        let _ = std::fs::remove_file(long_path(&tmp));
        return Err(err);
    }
    Ok(())
}

/// 已分配的盘符（"此电脑"列出来用），一次系统调用拿全。
///
/// 只说明盘符存在，不保证此刻能访问（光驱没盘、网络盘断开）：不在这里逐个探测——
/// 以前对每个盘符做一次 metadata，断开的网络盘会让打开"此电脑"等好几秒。点进去
/// 访问不了时再报错。
#[cfg(windows)]
pub fn logical_drives() -> Vec<char> {
    // SAFETY：没有参数，只返回位掩码。
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    drives_from_mask(mask)
}

#[cfg(not(windows))]
pub fn logical_drives() -> Vec<char> {
    Vec::new()
}

/// GetLogicalDrives 的位掩码 → 盘符（第 0 位是 A:）。
pub fn drives_from_mask(mask: u32) -> Vec<char> {
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (b'A' + bit) as char)
        .collect()
}

/// 本地路径拼一段远端相对路径（`C:\a` + `x/y` → `C:\a\x\y`）。
pub fn join_local(root: &Path, relative: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        out.push(part);
    }
    out
}

/// 需要建出来的本地父目录（下载多层目录时用）。
pub fn ensure_parent(path: &Path) -> Result<(), SftpError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(long_path(parent))
            .map_err(|err| from_io(err, &parent.display().to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_target_normalizes_and_stays_inside() {
        let dir = Path::new("C:\\Users\\x\\Downloads");
        for (source, expected) in [
            ("/srv/C:evil.txt", Some("C_evil.txt")),
            ("/srv/CON", Some("_CON")),
            ("/srv/a:b", Some("a_b")),
            ("/srv/..", Some("_")),
            ("/srv/a\\b.txt", Some("a_b.txt")),
            ("/srv/dir/", Some("dir")),
            ("/", None),
            ("", None),
        ] {
            match (download_target(dir, source), expected) {
                (Ok(path), Some(name)) => assert_eq!(path, dir.join(name), "{source}"),
                (Err(_), None) => {}
                (got, want) => panic!("{source:?}: {got:?} vs {want:?}"),
            }
        }
    }

    #[test]
    fn ensure_inside_rejects_escapes() {
        let root = Path::new("C:\\d\\out");
        assert!(ensure_inside(root, Path::new("C:\\d\\out\\a.txt")).is_ok());
        assert!(ensure_inside(root, Path::new("C:\\D\\OUT\\sub\\a.txt")).is_ok());
        for bad in [
            "C:\\d\\out",
            "C:\\d\\out\\..\\x",
            "C:\\d\\other\\a.txt",
            "D:\\d\\out\\a.txt",
            "C:evil.txt",
            "C:\\d\\outside.txt",
        ] {
            assert!(ensure_inside(root, Path::new(bad)).is_err(), "{bad}");
        }
    }

    /// 同一个目录的 8.3 短路径和长路径写法：判断结果必须一致（R-6b）。
    #[cfg(windows)]
    #[test]
    fn short_and_long_spellings_of_a_directory_agree() {
        let dir = crate::test_util::TestDir::new("local-a-rather-long-directory-name");
        let long_root = dir.join("Download Target Directory");
        std::fs::create_dir_all(&long_root).unwrap();
        let long_root = crate::pathnorm::normalize(&long_root);
        let Some(short_root) = crate::pathnorm::short_path(&long_root) else {
            eprintln!("跳过：这个卷上没有 8.3 短名字，造不出短路径写法");
            return;
        };
        assert_ne!(short_root, long_root);

        // 目标还不存在，用另一种写法的根。
        assert!(ensure_inside(&short_root, &long_root.join("a.txt")).is_ok());
        assert!(ensure_inside(&long_root, &short_root.join("a.txt")).is_ok());
        assert!(ensure_inside(&short_root, &long_root.join("sub").join("b.txt")).is_ok());
        // 根自己、根外面的都仍然拒绝。
        assert!(ensure_inside(&short_root, &long_root).is_err());
        assert!(ensure_inside(&short_root, &long_root.parent().unwrap().join("x.txt")).is_err());
        // 同一个目标两种写法 = 同一个键（互斥锁靠它）。
        assert_eq!(
            lexical_key(&short_root.join("same.bin")),
            lexical_key(&long_root.join("same.bin"))
        );
        // 顶层下载落点在两种写法的根下都成功。
        assert!(download_target(&short_root, "/srv/f.txt").is_ok());
    }

    #[test]
    fn illegal_characters_are_replaced_with_a_note() {
        let planned = local_name("a:b*c?d\"e<f>g|h\\i/j.txt");
        assert_eq!(planned.name, "a_b_c_d_e_f_g_h_i_j.txt");
        let note = planned.note.expect("要有说明");
        assert!(note.contains("非法字符"), "{note}");
    }

    #[test]
    fn trailing_dots_and_spaces_are_trimmed() {
        let planned = local_name("报告. ");
        assert_eq!(planned.name, "报告");
        let _ = planned.note.expect("要有说明");
    }

    #[test]
    fn reserved_names_get_a_prefix() {
        let planned = local_name("CON");
        assert_eq!(planned.name, "_CON");
        assert!(planned.note.unwrap().contains("保留名"));
        // 带扩展名也算。
        assert_eq!(local_name("nul.txt").name, "_nul.txt");
        assert_eq!(local_name("Com1.log").name, "_Com1.log");
        // 不是保留名的不动。
        assert_eq!(local_name("console.txt").name, "console.txt");
        assert!(local_name("console.txt").note.is_none());
    }

    #[test]
    fn control_characters_are_replaced() {
        let planned = local_name("bad\u{7}name.txt");
        assert_eq!(planned.name, "bad_name.txt");
        assert!(planned.note.unwrap().contains("\\x07"));
    }

    #[test]
    fn case_conflicts_get_numbered_names() {
        let plan = LocalNamePlan::plan(
            &["A.txt".to_owned(), "a.txt".to_owned(), "b.txt".to_owned()],
            &[],
        );
        assert_eq!(plan.assignments[0].1.name, "A.txt");
        assert_eq!(plan.assignments[1].1.name, "a (2).txt", "只差大小写要避让");
        assert!(plan.assignments[1].1.note.is_some());
        assert_eq!(plan.assignments[2].1.name, "b.txt");
        assert!(plan.assignments[2].1.note.is_none());
    }

    #[test]
    fn a_child_name_must_be_a_plain_windows_file_name() {
        // R-10 M1：带盘符、冒号、分隔符、保留名、结尾点的名字都不能用（不改写，直接拒绝）。
        for bad in [
            "C:evil.txt",
            "D:x",
            "foo:stream",
            "a/b",
            r"a\b",
            ".",
            "..",
            "",
            "   ",
            "CON",
            "nul.txt",
            "trailing.",
            "q?.txt",
        ] {
            assert!(check_child_name(bad).is_err(), "{bad:?} 应该被拒绝");
        }
        assert_eq!(
            check_child_name(" 报告 (副本).txt ").unwrap(),
            "报告 (副本).txt"
        );
        assert_eq!(check_child_name(".bashrc").unwrap(), ".bashrc");
    }

    #[test]
    fn an_existing_local_file_keeps_its_name_for_the_conflict_dialog() {
        // R-9 F13：本地已经有同名文件是"目标已存在"，要走冲突对话框，不能悄悄改名。
        let plan = LocalNamePlan::plan(&["readme.md".to_owned()], &["readme.md".to_owned()]);
        assert_eq!(plan.assignments[0].1.name, "readme.md");
        assert!(plan.assignments[0].1.note.is_none());
        // 只差大小写在 Windows 上也是同一个文件：同样交给冲突处理。
        let plan = LocalNamePlan::plan(&["readme.md".to_owned()], &["README.MD".to_owned()]);
        assert_eq!(plan.assignments[0].1.name, "readme.md");
    }

    #[test]
    fn an_existing_entry_of_the_other_kind_still_gets_a_new_name() {
        // 远端是文件夹、本地是同名文件（或反过来）：写不进去，只能换名字。
        let plan = LocalNamePlan::plan_with_kinds(
            &[("logs".to_owned(), true), ("data".to_owned(), false)],
            &[("logs".to_owned(), false), ("data".to_owned(), true)],
        );
        assert_eq!(plan.assignments[0].1.name, "logs (2)");
        assert_eq!(plan.assignments[1].1.name, "data (2)");
        assert!(plan.assignments[0].1.note.is_some());
        // 同类的已有文件夹：直接下载进去（里面的文件各自走冲突处理）。
        let plan = LocalNamePlan::plan_with_kinds(
            &[("logs".to_owned(), true)],
            &[("logs".to_owned(), true)],
        );
        assert_eq!(plan.assignments[0].1.name, "logs");
    }

    #[test]
    fn numbered_names_skip_existing_files() {
        // 同一批里撞名要加序号，但序号挑出来的名字不能正好是另一份已有文件。
        let plan = LocalNamePlan::plan(
            &["A.txt".to_owned(), "a.txt".to_owned()],
            &["a (2).txt".to_owned()],
        );
        assert_eq!(plan.assignments[0].1.name, "A.txt");
        assert_eq!(plan.assignments[1].1.name, "a (3).txt");
    }

    #[test]
    fn long_paths_get_the_prefix() {
        assert_eq!(
            long_path(Path::new("C:\\a\\b")).to_string_lossy(),
            "\\\\?\\C:\\a\\b"
        );
        assert_eq!(
            long_path(Path::new("\\\\server\\share\\a")).to_string_lossy(),
            "\\\\?\\UNC\\server\\share\\a"
        );
        // 已经有前缀的原样返回；相对路径不加。
        assert_eq!(
            long_path(Path::new("\\\\?\\C:\\a")).to_string_lossy(),
            "\\\\?\\C:\\a"
        );
        assert_eq!(long_path(Path::new("a\\b")).to_string_lossy(), "a\\b");
        assert!(needs_long_path(Path::new(&"x".repeat(250))));
        assert!(!needs_long_path(Path::new("C:\\short")));
    }

    #[test]
    fn create_new_file_never_truncates_an_existing_file() {
        let root = crate::test_util::TestDir::new("create-new");
        let path = root.join("a.txt");
        create_new_file(&path).expect("新建空文件");
        assert_eq!(std::fs::read(&path).unwrap(), b"");

        // 模拟"检查之后、创建之前"别的程序写进来的同名文件：必须报已存在，内容不能被清空。
        std::fs::write(&path, b"someone else").unwrap();
        let err = create_new_file(&path).expect_err("同名文件已存在");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"someone else");

        // 同名目录也一样不能"建成功"。
        std::fs::create_dir(root.join("d")).unwrap();
        assert!(create_new_file(&root.join("d")).is_err());

        // 父目录不存在是 NotFound，不能被当成"已存在"或吞掉。
        let err = create_new_file(&root.join("missing").join("b.txt")).expect_err("父目录不存在");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

        // 超过 260 个字符的路径也能建（和其他本地操作一样过长路径前缀）。
        let deep = root.join("x".repeat(120)).join("y".repeat(120));
        std::fs::create_dir_all(long_path(&deep)).unwrap();
        let long_file = deep.join("z".repeat(40));
        create_new_file(&long_file).expect("长路径新建");
        assert!(std::fs::metadata(long_path(&long_file)).unwrap().is_file());
        assert_eq!(
            create_new_file(&long_file).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn local_tree_walk_collects_files_and_empty_dirs() {
        let root = crate::test_util::TestDir::new("local-tree");
        std::fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"world!").unwrap();
        std::fs::write(root.join("sub").join("deep").join("c.bin"), vec![0u8; 32]).unwrap();

        let tree = walk_local_tree(&root).expect("遍历本地目录");
        let mut relatives: Vec<&str> = tree.files.iter().map(|f| f.relative.as_str()).collect();
        relatives.sort();
        assert_eq!(relatives, vec!["a.txt", "sub/b.txt", "sub/deep/c.bin"]);
        assert!(tree.empty_dirs.contains(&"empty".to_owned()));
        assert!(tree.skipped_links.is_empty());

        // 相对路径拼回本地路径。
        assert_eq!(
            join_local(&root, "sub/b.txt"),
            root.join("sub").join("b.txt")
        );
    }

    #[test]
    fn local_tree_walk_skips_symlinks_or_junctions() {
        let root = crate::test_util::TestDir::new("local-link");
        let outside = crate::test_util::TestDir::new("local-out");
        std::fs::write(root.join("inside.txt"), b"in").unwrap();
        std::fs::write(outside.join("outside.txt"), b"out").unwrap();

        // 目录联接（junction）：Windows 上不需要管理员权限就能建。
        let junction = root.join("link-to-outside");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output();
        let created = made.map(|out| out.status.success()).unwrap_or(false);
        if !created {
            println!("跳过：这台机器建不了 junction");
            return;
        }

        let tree = walk_local_tree(&root).expect("遍历本地目录");
        let relatives: Vec<&str> = tree.files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(relatives, vec!["inside.txt"], "不跟着联接走");
        assert_eq!(tree.skipped_links.len(), 1, "{:?}", tree.skipped_links);
        assert!(
            tree.skipped_links[0].contains("link-to-outside"),
            "{:?}",
            tree.skipped_links
        );
    }

    #[test]
    fn drive_letters_come_from_the_bitmask() {
        assert_eq!(drives_from_mask(0), Vec::<char>::new());
        assert_eq!(drives_from_mask(0b1), vec!['A']);
        assert_eq!(drives_from_mask(0b1100), vec!['C', 'D']);
        assert_eq!(drives_from_mask(1 << 25), vec!['Z']);
        assert_eq!(drives_from_mask(u32::MAX).len(), 26, "高位多出来的位不算");
    }
}
