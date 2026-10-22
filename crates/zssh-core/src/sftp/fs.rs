//! SFTP 文件操作（M3-1 第 2 节）。
//!
//! 这一层把协议细节收起来，给上层（界面 / AI 工具 / 传输引擎）一组语义明确的接口：
//! 列目录、读属性、建目录、建空文件、重命名、删除（递归带进度和取消）、改权限、
//! 读/写小文本。
//!
//! 兼容性做法（每一处都在测试里有对应用例，报告里分"实测"和"只有单测"两类列清楚）：
//! - 符号链接：列表显示链接本身，并尝试解析目标的类型（断链就是 `None`，不报错）；
//! - 非 UTF-8 文件名：列表里可以有损显示，但**标出来**（`name_is_utf8 = false`），
//!   操作时给 [`SftpError::NonUtf8Name`]——拿有损的名字去操作等于把服务器的名字改坏；
//! - 重命名到已存在的目标：走 [`SftpChannel::rename`]（posix-rename 或先删后改名）；
//! - 递归删除：自己走树（不依赖服务器扩展），一层层删，能报进度、能取消。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use russh_sftp::protocol::{FileAttributes, FileType};

use super::error::SftpError;
use super::path;
use super::session::SftpChannel;

/// 小文本文件的体积上限（AI 读文件 / 快速编辑用；超了明确报错，不截断）。
pub const SMALL_TEXT_LIMIT: u64 = 2 * 1024 * 1024;

/// 一个目录项的类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    File,
    /// 符号链接：`target` 是目标的类型（目标不存在就是 `None`，断链）。
    Symlink {
        target: Option<Box<FileKind>>,
    },
    /// 其它类型（socket / fifo / 设备）：带上原始 mode 位，界面照原样显示。
    Other(String),
}

impl FileKind {
    pub fn is_dir(&self) -> bool {
        match self {
            FileKind::Dir => true,
            FileKind::Symlink { target } => target.as_deref().is_some_and(FileKind::is_dir),
            _ => false,
        }
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self, FileKind::Symlink { .. })
    }

    /// 中文说法（界面用；日志里也照这个）。
    pub fn label(&self) -> &'static str {
        match self {
            FileKind::Dir => "目录",
            FileKind::File => "文件",
            FileKind::Symlink { .. } => "符号链接",
            FileKind::Other(_) => "其它",
        }
    }
}

/// 列表里的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// 名字（**可能是有损解码**：见 `name_is_utf8`）。
    pub name: String,
    /// 完整路径（父目录 + 名字，用 POSIX 规则拼）。
    pub path: String,
    pub kind: FileKind,
    /// 符号链接指向哪里（只有链接有）。
    pub link_target: Option<String>,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
    /// 所有者（服务器报了用户名就用用户名，没报就用 uid；SFTP v3 一般只有数字）。
    pub owner: Option<String>,
    /// 名字是不是合法 UTF-8。`false` 时**不要**拿 `name` 去操作（会改坏服务器上的名字）。
    pub name_is_utf8: bool,
}

/// 从协议层的条目列表构造 [`DirEntry`]（解析符号链接目标 + 标记非 UTF-8 名字）。
pub async fn entries_from_protocol(
    channel: &SftpChannel,
    dir: &str,
    files: Vec<russh_sftp::protocol::File>,
) -> Result<Vec<DirEntry>, SftpError> {
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let name = file.filename.clone();
        // `.` / `..` 不列（POSIX 惯例，界面也不需要）。
        if name == "." || name == ".." {
            continue;
        }
        let entry_path = path::join(dir, &name);
        let attrs = file.attrs;
        let longname_owner = owner_from_longname(&file.longname);
        let mut link_target = None;
        let mut kind = kind_of(&attrs);
        if kind.is_symlink() {
            link_target = channel.read_link(&entry_path).await.ok();
            // 目标的类型：跟随链接 stat 一次；断链就留 None（不报错）。
            let target = match channel.stat(&entry_path).await {
                Ok(attrs) => Some(Box::new(kind_of(&attrs))),
                Err(_) => None,
            };
            kind = FileKind::Symlink { target };
        }
        out.push(DirEntry {
            name_is_utf8: name_is_clean_utf8(channel, dir, &name).await,
            name,
            path: entry_path,
            kind,
            link_target,
            size: attrs.size,
            mtime: attrs.mtime,
            permissions: attrs.permissions,
            owner: attrs
                .user
                .clone()
                .or(longname_owner)
                .or_else(|| attrs.uid.map(|uid| uid.to_string())),
        });
    }
    // 目录在前、同类按名字排（和资源管理器一致）。每个名字只转一次小写（不在比较里反复转），
    // 几万条的目录也排得快；结果和逐对比较完全一样（都是稳定排序）。
    out.sort_by_cached_key(|entry| (!entry.kind.is_dir(), entry.name.to_lowercase()));
    Ok(out)
}

/// 从 SFTP v3 的 `longname`（`ls -l` 的一行）里取所有者名字。
///
/// 格式：`-rw-r--r--    1 owner    group        1234 Oct 17 20:26 name`。
/// 前五段是 权限、链接数、所有者、组、大小；少于五段、所有者段为空或是 `?` 时返回
/// `None`（调用方退回 uid）。名字本身可能含空格，所以只数前五段，不碰后面。
pub fn owner_from_longname(longname: &str) -> Option<String> {
    let mut parts = longname.split_whitespace();
    let mode = parts.next()?;
    // 权限段必须像 `drwxr-xr-x`，否则这行不是 ls -l 格式（有的服务器乱填）。
    let looks_like_mode = mode.len() >= 10
        && mode.chars().next().is_some_and(|c| "-dlcbpsD?".contains(c))
        && mode.chars().skip(1).all(|c| "rwxstST-+.@lL".contains(c));
    if !looks_like_mode {
        return None;
    }
    let _links = parts.next()?;
    let owner = parts.next()?;
    let _group = parts.next()?;
    let _size = parts.next()?;
    if owner == "?" || owner.is_empty() {
        return None;
    }
    Some(owner.to_owned())
}

/// 名字里出现替换字符（U+FFFD）时，它可能来自"原始字节不是 UTF-8"的有损解码，
/// 也可能服务器上真的有这个字符。**只在可疑时**多做一次 `lstat` 来分辨：
/// 用有损名字能 stat 到，就说明服务器上真有这个字符；stat 不到（不存在）就说明
/// 真实名字不是 UTF-8（我们操作不了它）。
async fn name_is_clean_utf8(channel: &SftpChannel, dir: &str, name: &str) -> bool {
    if !name.contains(char::REPLACEMENT_CHARACTER) {
        return true;
    }
    let candidate = path::join(dir, name);
    match channel.lstat(&candidate).await {
        Ok(_) => true,
        Err(SftpError::NotFound { .. }) => false,
        // 别的错误（没权限等）：不敢下结论，按"能操作"处理，让真正的操作去报错。
        Err(_) => true,
    }
}

/// 协议层的属性 → 我们的类型。
pub fn kind_of(attrs: &FileAttributes) -> FileKind {
    if attrs.is_symlink() {
        return FileKind::Symlink { target: None };
    }
    if attrs.is_dir() {
        return FileKind::Dir;
    }
    match attrs.file_type() {
        FileType::File => FileKind::File,
        // socket / fifo / 字符设备 / 块设备：带上类型位，界面照原样显示。
        FileType::Other => match attrs.permissions {
            Some(bits) if bits & 0o170000 != 0 => {
                FileKind::Other(format!("mode {:#o}", bits & 0o170000))
            }
            _ => FileKind::File,
        },
        FileType::Dir => FileKind::Dir,
        FileType::Symlink => FileKind::Symlink { target: None },
    }
}

/// 逐级建目录（`a/b/c`）：**已经存在的目录跳过**，只建缺的；最后一级已存在
/// 就报 [`SftpError::AlreadyExists`]（界面"新建文件夹"到同名目录要告诉用户）。
///
/// 中间一级存在但不是目录（普通文件）也报错，不往下建。
pub async fn create_dir_all(channel: &SftpChannel, path_str: &str) -> Result<(), SftpError> {
    let parts: Vec<&str> = path_str
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut built = String::new();
    for (index, part) in parts.iter().enumerate() {
        built = format!("{built}/{part}");
        let is_last = index + 1 == parts.len();
        match channel.stat(&built).await {
            Ok(attrs) => {
                if is_last {
                    return Err(SftpError::AlreadyExists { path: built });
                }
                if !kind_of(&attrs).is_dir() {
                    return Err(SftpError::Remote(format!("{built} 已存在，但不是目录")));
                }
            }
            Err(SftpError::NotFound { .. }) => channel.create_dir(&built).await?,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// 读出来的一份属性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStat {
    pub path: String,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub atime: Option<u32>,
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
}

impl RemoteStat {
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }
}

/// 读属性（跟随链接；`follow = false` 时读链接本身）。
pub async fn stat(
    channel: &SftpChannel,
    path_str: &str,
    follow: bool,
) -> Result<RemoteStat, SftpError> {
    let attrs = if follow {
        channel.stat(path_str).await?
    } else {
        channel.lstat(path_str).await?
    };
    Ok(RemoteStat {
        path: path_str.to_owned(),
        kind: kind_of(&attrs),
        size: attrs.size,
        mtime: attrs.mtime,
        atime: attrs.atime,
        permissions: attrs.permissions,
        uid: attrs.uid,
        gid: attrs.gid,
        user: attrs.user.clone(),
        group: attrs.group.clone(),
    })
}

/// 读写小文本的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmallText {
    pub path: String,
    /// 内容（不是合法 UTF-8 时用有损解码，`utf8 = false`）。
    pub text: String,
    pub utf8: bool,
    pub size: u64,
}

/// 读一个小文件的全部字节。**超过上限明确报错，不截断**。
pub async fn read_bytes(
    channel: &SftpChannel,
    path_str: &str,
    limit: u64,
) -> Result<Vec<u8>, SftpError> {
    channel.guard_name(path_str).await?;
    let info = stat(channel, path_str, true).await?;
    let size = info.size.unwrap_or(0);
    if size > limit {
        return Err(SftpError::TooLarge {
            path: path_str.to_owned(),
            size,
            limit,
        });
    }
    let handle = channel.open_read(path_str).await?;
    let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let chunk = channel.caps().max_read_len();
    let mut offset = 0u64;
    let result = loop {
        match channel.read_at(&handle, offset, chunk).await {
            Ok(piece) => {
                if piece.is_empty() {
                    break Ok(());
                }
                offset += piece.len() as u64;
                // 远端此刻变大也不怕：超过上限就报错，不截断。
                if offset > limit {
                    break Err(SftpError::TooLarge {
                        path: path_str.to_owned(),
                        size: offset,
                        limit,
                    });
                }
                data.extend_from_slice(&piece);
            }
            Err(err) => break Err(err),
        }
    };
    // 读失败也要关句柄。
    let _ = channel.close_handle(&handle).await;
    result?;
    Ok(data)
}

/// 读一个小文本文件。**超过上限明确报错，不截断**。
pub async fn read_small_text(
    channel: &SftpChannel,
    path_str: &str,
    limit: u64,
) -> Result<SmallText, SftpError> {
    let data = read_bytes(channel, path_str, limit).await?;
    let utf8 = std::str::from_utf8(&data).is_ok();
    Ok(SmallText {
        path: path_str.to_owned(),
        text: String::from_utf8_lossy(&data).into_owned(),
        utf8,
        size: data.len() as u64,
    })
}

/// 写一个小文件的全部字节（`truncate` 之后整段写，写完 fsync + close）。
pub async fn write_bytes(
    channel: &SftpChannel,
    path_str: &str,
    bytes: &[u8],
    limit: u64,
) -> Result<u64, SftpError> {
    channel.guard_name(path_str).await?;
    let size = bytes.len() as u64;
    if size > limit {
        return Err(SftpError::TooLarge {
            path: path_str.to_owned(),
            size,
            limit,
        });
    }
    let handle = channel.open_write_truncate(path_str).await?;
    let chunk = channel.caps().max_write_len() as usize;
    let mut written = 0u64;
    let mut result = Ok(());
    for piece in bytes.chunks(chunk.max(1)) {
        if let Err(err) = channel.write_at(&handle, written, piece.to_vec()).await {
            result = Err(err);
            break;
        }
        written += piece.len() as u64;
    }
    if result.is_ok() {
        // fsync 有就用（服务器声明了才刷）；刷失败按写失败处理——文件可能没落盘。
        if let Err(err) = channel.fsync(&handle).await {
            result = Err(err);
        }
    }
    // 无论成败都关句柄。
    let close = channel.close_handle(&handle).await;
    result?;
    close?;
    Ok(written)
}

/// 写一个小文本文件。
pub async fn write_small_text(
    channel: &SftpChannel,
    path_str: &str,
    text: &str,
    limit: u64,
) -> Result<u64, SftpError> {
    write_bytes(channel, path_str, text.as_bytes(), limit).await
}

/// 递归删除的进度（每删掉一项报一次）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteProgress {
    /// 已经删掉了多少项（文件 + 目录，累计）。
    pub deleted: u64,
    /// 刚删掉的那个（绝对路径）。
    pub current: String,
}

/// 递归删除：自己走树（不依赖服务器扩展），能报进度、能取消。
pub struct RecursiveDelete {
    /// 取消标志：置位后在下一次检查时停下来（已删掉的不回滚）。
    cancel: Arc<AtomicBool>,
}

impl RecursiveDelete {
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 拿一个可以在别处（界面线程 / 另一个任务）触发的取消句柄。
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle {
            flag: Arc::clone(&self.cancel),
        }
    }

    /// 取消（等价于 `cancel_handle().cancel()`）。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 删掉 `target`（文件直接删；目录先删里面再删自己）。
    ///
    /// `on_progress` 每删掉一项调一次（上层自己限流后再发给界面）。
    pub async fn run<F>(
        &self,
        channel: &SftpChannel,
        target: &str,
        mut on_progress: F,
    ) -> Result<u64, SftpError>
    where
        F: FnMut(DeleteProgress) + Send,
    {
        channel.guard_name(target).await?;
        let info = stat(channel, target, false).await?;
        if !info.is_dir() {
            channel.remove_file(target).await?;
            on_progress(DeleteProgress {
                deleted: 1,
                current: target.to_owned(),
            });
            return Ok(1);
        }
        self.remove_tree(channel, target, &mut on_progress, 0).await
    }

    /// 递归删目录：深度优先，先删孩子再删自己。
    fn remove_tree<'a, F>(
        &'a self,
        channel: &'a SftpChannel,
        dir: &'a str,
        on_progress: &'a mut F,
        deleted: u64,
    ) -> crate::ssh::BoxFuture<'a, Result<u64, SftpError>>
    where
        F: FnMut(DeleteProgress) + Send,
    {
        Box::pin(async move {
            let mut count = deleted;
            let entries = channel.read_dir_raw(dir).await?;
            for entry in entries {
                if self.is_cancelled() {
                    return Err(SftpError::Cancelled);
                }
                if entry.filename == "." || entry.filename == ".." {
                    continue;
                }
                let child = path::join(dir, &entry.filename);
                // 符号链接当作文件删（不跟着走，免得删到目标目录里去）。
                if entry.attrs.is_dir() && !entry.attrs.is_symlink() {
                    count = self
                        .remove_tree(channel, &child, on_progress, count)
                        .await?;
                } else {
                    channel.remove_file(&child).await?;
                    count += 1;
                    on_progress(DeleteProgress {
                        deleted: count,
                        current: child,
                    });
                }
            }
            if self.is_cancelled() {
                return Err(SftpError::Cancelled);
            }
            channel.remove_dir(dir).await?;
            count += 1;
            on_progress(DeleteProgress {
                deleted: count,
                current: dir.to_owned(),
            });
            Ok(count)
        })
    }
}

impl Default for RecursiveDelete {
    fn default() -> Self {
        Self::new()
    }
}

/// 取消句柄：递归删除这种长操作可以从别处叫停。
#[derive(Clone)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::owner_from_longname;

    #[test]
    fn owner_is_read_from_ls_style_longname() {
        assert_eq!(
            owner_from_longname("-rw-r--r--    1 alice    staff        1234 Oct 17 20:26 a b.txt"),
            Some("alice".to_owned())
        );
        assert_eq!(
            owner_from_longname("drwxr-xr-x 2 root root 4096 Jan  1  2020 dir"),
            Some("root".to_owned())
        );
        // 数字所有者（服务器没有名字时）也原样给出，由界面当 uid 显示。
        assert_eq!(
            owner_from_longname("-rw-r--r-- 1 1000 1000 5 Jan 1 12:00 x"),
            Some("1000".to_owned())
        );
    }

    #[test]
    fn owner_is_none_for_garbage_longname() {
        assert_eq!(owner_from_longname(""), None);
        assert_eq!(owner_from_longname("just a name"), None);
        assert_eq!(
            owner_from_longname("-rw-r--r-- 1 ? ? 5 Jan 1 12:00 x"),
            None
        );
        assert_eq!(owner_from_longname("-rw-r--r-- 1"), None);
    }

    use super::*;

    #[test]
    fn kinds_come_from_attributes() {
        let mut dir = FileAttributes::default();
        dir.set_dir(true);
        assert_eq!(kind_of(&dir), FileKind::Dir);

        let mut file = FileAttributes::default();
        file.set_regular(true);
        assert_eq!(kind_of(&file), FileKind::File);

        let mut link = FileAttributes::default();
        link.set_symlink(true);
        assert!(matches!(kind_of(&link), FileKind::Symlink { target: None }));

        // 没有任何类型位：当普通文件（不然界面上会全是"其它"）。
        assert_eq!(kind_of(&FileAttributes::default()), FileKind::File);
    }

    #[test]
    fn symlink_to_dir_counts_as_dir() {
        let kind = FileKind::Symlink {
            target: Some(Box::new(FileKind::Dir)),
        };
        assert!(kind.is_dir());
        assert!(kind.is_symlink());
        assert_eq!(kind.label(), "符号链接");
    }

    #[test]
    fn recursive_delete_can_be_cancelled_from_another_handle() {
        let deleter = RecursiveDelete::new();
        let handle = deleter.cancel_handle();
        assert!(!deleter.is_cancelled());
        handle.cancel();
        assert!(deleter.is_cancelled(), "别处拿到句柄叫停也要生效");
    }
}
