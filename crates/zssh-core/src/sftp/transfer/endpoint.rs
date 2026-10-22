//! 传输的数据面（M3-1 第 3 节）：源和目标各是一个 [`FileEndpoint`]。
//!
//! 上传时 `source` 是本地文件、`target` 是远端文件；下载时反过来。这样**单文件传输
//! 逻辑（流水线、续传、尾部校验、收尾改名）只写一遍**，本地和远端对称；单测也能用
//! 内存替身（[`memory::MemoryEndpoint`]）造出断线、部分写这类真实网络里难复现的情况。
//!
//! 两端的差异都收在各自的实现里：
//! - 远端（[`RemoteEndpoint`]）：走 SFTP，用 `.zssh-part` 临时文件，收尾用
//!   `posix-rename` 或"先删后改名"（见 [`SftpChannel::rename`]）；
//! - 本地（[`LocalEndpoint`]）：走 `std::fs`（阻塞 IO 丢给 `spawn_blocking`），
//!   长路径加 `\\?\` 前缀，临时文件同样叫 `.zssh-part`。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::sftp::error::{from_io, SftpError};
use crate::sftp::local;
use crate::sftp::session::SftpChannel;

/// 传输里常用的 future 类型（trait 对象要用）。
pub type EndpointFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

use std::future::Future;

/// 临时文件后缀：`.zssh-part`（设计 2.3）。
pub const PART_SUFFIX: &str = ".zssh-part";

/// 临时文件名：`<目标>.<任务标识>.zssh-part`（标识为空时是旧写法 `<目标>.zssh-part`）。
/// 带上任务标识，别的任务和用户自己的同名文件都不会被当成这个任务的缓存。
pub fn part_name(target: &str, owner: &str) -> String {
    if owner.is_empty() {
        format!("{target}{PART_SUFFIX}")
    } else {
        format!("{target}.{owner}{PART_SUFFIX}")
    }
}

/// 一份文件的修改时间和权限（保留属性用；缺失是 `None`）。
pub type FileAttributesPair = (Option<u32>, Option<u32>);

/// 读端点（一个打开的文件，按偏移读）。
pub trait ReadAt: Send + Sync {
    fn read_at<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Vec<u8>, SftpError>>;
    fn close<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>>;
}

/// 写端点（一个打开的文件，按偏移写）。
pub trait WriteAt: Send + Sync {
    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: Vec<u8>,
    ) -> EndpointFuture<'a, Result<(), SftpError>>;
    /// 刷盘（远端有 `fsync` 才刷）+ 关闭句柄。
    fn finish<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>>;
}

/// 一个文件的端点。
pub trait FileEndpoint: Send + Sync {
    /// 给日志/事件看的描述（本地路径或远端路径）。
    fn describe(&self) -> String;

    /// 源文件大小（拿不到就是 `None`）。
    fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>>;

    /// 打开读（返回 `Arc`：流水线要把同一个句柄分给多个并发读任务）。
    fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>>;

    /// 打开写：`resume_len` 是已经有的字节数（0 = 从头写/截断）。
    fn open_write<'a>(
        &'a self,
        resume_len: u64,
    ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>>;

    /// 收尾：把临时文件改名成最终名字（成功才调）。`overwrite` 决定目标存在时要不要覆盖。
    fn commit<'a>(
        &'a self,
        total: u64,
        overwrite: bool,
    ) -> EndpointFuture<'a, Result<(), SftpError>>;

    /// 目标（最终文件）已存在时的大小。
    fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>>;

    /// 目标已存在时的修改时间（冲突对话框要显示；拿不到就 `None`）。
    fn existing_mtime<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u32>, SftpError>> {
        Box::pin(async move { Ok(None) })
    }

    /// 删掉最终文件（"覆盖"之前清掉；不存在不算错）。
    fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>>;

    /// 删掉临时文件（放弃续传 / 取消时用）。
    fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>>;

    /// 临时文件已经有的长度（续传的起点）。
    fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>>;

    /// 把临时文件截断到 `len`。
    ///
    /// 为什么需要：写是**一批一起发**的，某一批里失败的时候，后面的块可能已经写进去了
    /// ——"临时文件长度"就不再是"连续写好的前缀"。续传是按前缀续的，所以失败收尾时
    /// 要截回已确认的前缀，否则会在文件中间留一个洞。
    fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>>;

    /// 读临时文件的一部分（续传前的对账；没有临时文件就 `None`）。
    fn read_partial<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>>;

    /// 保留属性：修改时间 / 权限（有就设，服务器不支持就算了）。
    fn set_attributes<'a>(
        &'a self,
        mtime: Option<u32>,
        permissions: Option<u32>,
    ) -> EndpointFuture<'a, Result<(), SftpError>>;

    /// 源文件的修改时间 / 权限（上传本地文件、下载远端文件时都要读）。
    fn attributes<'a>(&'a self) -> EndpointFuture<'a, Result<FileAttributesPair, SftpError>>;
}

// ---------------------------------------------------------------------------
// 远端端点
// ---------------------------------------------------------------------------

/// 远端文件的端点（走 SFTP）。
pub struct RemoteEndpoint {
    channel: Arc<SftpChannel>,
    path: String,
    /// 任务标识：写进临时文件名，只认自己名字的临时文件。
    owner: String,
}

impl RemoteEndpoint {
    /// 一个全新的、不属于任何已有任务的端点（标识随机）。
    pub fn new(channel: Arc<SftpChannel>, path: impl Into<String>) -> Self {
        Self::with_owner(
            channel,
            path,
            uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
        )
    }

    /// 属于某个任务的端点：同一个任务重试 / 续传要用同一个标识，才找得到自己的临时文件。
    pub fn with_owner(
        channel: Arc<SftpChannel>,
        path: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            path: path.into(),
            owner: owner.into(),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 临时文件路径：`<目标>.<任务标识>.zssh-part`。
    pub fn part_path(&self) -> String {
        part_name(&self.path, &self.owner)
    }
}

impl FileEndpoint for RemoteEndpoint {
    fn describe(&self) -> String {
        self.path.clone()
    }

    fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            match self.channel.lstat(&self.path).await {
                Ok(attrs) => Ok(attrs.size),
                Err(SftpError::NotFound { .. }) => Ok(None),
                Err(err) => Err(err),
            }
        })
    }

    fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>> {
        Box::pin(async move {
            let handle = self.channel.open_read(&self.path).await?;
            Ok(Arc::new(RemoteReader {
                channel: Arc::clone(&self.channel),
                handle,
            }) as Arc<dyn ReadAt>)
        })
    }

    fn open_write<'a>(
        &'a self,
        resume_len: u64,
    ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            let handle = if resume_len == 0 {
                self.channel.open_write_truncate(&part).await?
            } else {
                self.channel.open_write_resume(&part).await?
            };
            Ok(Arc::new(RemoteWriter {
                channel: Arc::clone(&self.channel),
                handle,
            }) as Arc<dyn WriteAt>)
        })
    }

    fn commit<'a>(
        &'a self,
        _total: u64,
        overwrite: bool,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            // 目标已存在且要覆盖：支持 posix-rename 的服务器一步就够；不支持时
            // `rename` 内部会先删目标（并把"中途失败"写进错误里）。
            self.channel
                .rename(&self.part_path(), &self.path, overwrite)
                .await
                .map(|_outcome| ())
        })
    }

    fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            match self.channel.lstat(&self.path).await {
                Ok(attrs) => Ok(attrs.size),
                Err(SftpError::NotFound { .. }) => Ok(None),
                Err(err) => Err(err),
            }
        })
    }

    fn existing_mtime<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u32>, SftpError>> {
        Box::pin(async move {
            match self.channel.lstat(&self.path).await {
                Ok(attrs) => Ok(attrs.mtime),
                Err(SftpError::NotFound { .. }) => Ok(None),
                Err(err) => Err(err),
            }
        })
    }

    fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            match self.channel.remove_file(&self.path).await {
                Ok(()) | Err(SftpError::NotFound { .. }) => Ok(()),
                Err(err) => Err(err),
            }
        })
    }

    fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            match self.channel.remove_file(&part).await {
                Ok(()) | Err(SftpError::NotFound { .. }) => Ok(()),
                Err(err) => Err(err),
            }
        })
    }

    fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>> {
        Box::pin(async move {
            match self.channel.lstat(&self.part_path()).await {
                Ok(attrs) => Ok(attrs.size.unwrap_or(0)),
                Err(SftpError::NotFound { .. }) => Ok(0),
                Err(err) => Err(err),
            }
        })
    }

    fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.channel.truncate(&self.part_path(), len).await })
    }

    fn read_partial<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            match self.channel.lstat(&part).await {
                Ok(attrs) if attrs.size.unwrap_or(0) >= offset + u64::from(len) => {}
                Ok(_) => return Ok(None),
                Err(SftpError::NotFound { .. }) => return Ok(None),
                Err(err) => return Err(err),
            }
            let handle = self.channel.open_read(&part).await?;
            let data = self.channel.read_at(&handle, offset, len).await;
            let _ = self.channel.close_handle(&handle).await;
            data.map(Some)
        })
    }

    fn set_attributes<'a>(
        &'a self,
        mtime: Option<u32>,
        permissions: Option<u32>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            if let Some(mtime) = mtime {
                self.channel.set_mtime(&self.path, mtime).await?;
            }
            if let Some(permissions) = permissions {
                self.channel
                    .set_permissions(&self.path, permissions & 0o7777)
                    .await?;
            }
            Ok(())
        })
    }

    fn attributes<'a>(&'a self) -> EndpointFuture<'a, Result<FileAttributesPair, SftpError>> {
        Box::pin(async move {
            let attrs = self.channel.lstat(&self.path).await?;
            Ok((attrs.mtime, attrs.permissions.map(|mode| mode & 0o7777)))
        })
    }
}

struct RemoteReader {
    channel: Arc<SftpChannel>,
    handle: String,
}

impl ReadAt for RemoteReader {
    fn read_at<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(async move { self.channel.read_at(&self.handle, offset, len).await })
    }

    fn close<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.channel.close_handle(&self.handle).await })
    }
}

struct RemoteWriter {
    channel: Arc<SftpChannel>,
    handle: String,
}

impl WriteAt for RemoteWriter {
    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: Vec<u8>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.channel.write_at(&self.handle, offset, data).await })
    }

    fn finish<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            // fsync 有就用（服务器声明了才刷）；刷失败按失败处理——可能没落盘。
            // 但刷失败也要关句柄：Rust 对象释放不会发 SSH_FXP_CLOSE，漏关的句柄会一直占在
            // 复用的 SFTP 通道上。返回的是 fsync 的错误（先发生的），关闭失败不覆盖它。
            let flushed = self.channel.fsync(&self.handle).await;
            let closed = self.channel.close_handle(&self.handle).await;
            flushed?;
            closed
        })
    }
}

// ---------------------------------------------------------------------------
// 本地端点
// ---------------------------------------------------------------------------

/// 本地文件的端点（走 `std::fs`，阻塞 IO 在 `spawn_blocking` 里做）。
pub struct LocalEndpoint {
    path: PathBuf,
    /// 这个端点所属的任务标识：写进临时文件名，只认自己名字的临时文件。
    owner: String,
}

impl LocalEndpoint {
    /// 一个全新的、不属于任何已有任务的端点（owner 随机）。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_owner(
            path,
            uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
        )
    }

    /// 属于某个任务的端点：同一个任务重试 / 续传时要用同一个 `owner`，才找得到自己的临时文件。
    pub fn with_owner(path: impl Into<PathBuf>, owner: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            owner: owner.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 临时文件：`<名字>.<任务标识>.zssh-part`。带上任务标识，别的任务和用户自己的同名文件
    /// 都不会被当成这个任务的缓存。
    pub fn part_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}", self.owner));
        name.push(PART_SUFFIX);
        match self.path.parent() {
            Some(parent) => parent.join(name),
            None => PathBuf::from(name),
        }
    }
}

impl FileEndpoint for LocalEndpoint {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            match std::fs::metadata(local::long_path(&self.path)) {
                Ok(meta) => Ok(Some(meta.len())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(from_io(err, &self.path.display().to_string())),
            }
        })
    }

    fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>> {
        Box::pin(async move {
            let path = local::long_path(&self.path);
            let display = self.path.display().to_string();
            let file = tokio::task::spawn_blocking(move || std::fs::File::open(path))
                .await
                .map_err(|err| SftpError::Internal(format!("本地读任务没跑起来：{err}")))?
                .map_err(|err| from_io(err, &display))?;
            Ok(Arc::new(LocalReader {
                file,
                path: self.path.clone(),
            }) as Arc<dyn ReadAt>)
        })
    }

    fn open_write<'a>(
        &'a self,
        resume_len: u64,
    ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>> {
        Box::pin(async move {
            local::ensure_parent(&self.path)?;
            let part = local::long_path(&self.part_path());
            let display = self.part_path().display().to_string();
            let file = tokio::task::spawn_blocking(move || {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create(true);
                if resume_len == 0 {
                    options.truncate(true);
                }
                options.open(part)
            })
            .await
            .map_err(|err| SftpError::Internal(format!("本地写任务没跑起来：{err}")))?
            .map_err(|err| from_io(err, &display))?;
            Ok(Arc::new(LocalWriter {
                file,
                path: self.part_path(),
            }) as Arc<dyn WriteAt>)
        })
    }

    fn commit<'a>(
        &'a self,
        _total: u64,
        overwrite: bool,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let part = local::long_path(&self.part_path());
            let final_path = self.path.clone();
            let target = local::long_path(&final_path);
            let display = final_path.display().to_string();
            tokio::task::spawn_blocking(move || {
                if overwrite {
                    // 原子替换：旧文件要么还在、要么已经是完整的新文件，中间不存在"删了旧的、
                    // 新的还没到位"的时刻。
                    local::replace_file(&part, &target)
                } else {
                    // 不覆盖：系统在目标已存在时报错（不是先检查再改名，没有空窗）。
                    // 注意 Windows 上 `std::fs::rename` 目标存在时会直接替换，不能用。
                    local::move_no_replace(&part, &target)
                }
            })
            .await
            .map_err(|err| SftpError::Internal(format!("本地收尾任务没跑起来：{err}")))?
            .map_err(|err| from_io(err, &display))
        })
    }

    fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            match std::fs::metadata(local::long_path(&self.path)) {
                Ok(meta) => Ok(Some(meta.len())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(from_io(err, &self.path.display().to_string())),
            }
        })
    }

    fn existing_mtime<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u32>, SftpError>> {
        Box::pin(async move {
            match std::fs::metadata(local::long_path(&self.path)) {
                Ok(meta) => Ok(meta
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| u32::try_from(d.as_secs()).ok())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(from_io(err, &self.path.display().to_string())),
            }
        })
    }

    fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            match std::fs::remove_file(local::long_path(&self.path)) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(from_io(err, &self.path.display().to_string())),
            }
        })
    }

    fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            match std::fs::remove_file(local::long_path(&part)) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(from_io(err, &part.display().to_string())),
            }
        })
    }

    fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>> {
        Box::pin(async move {
            match std::fs::metadata(local::long_path(&self.part_path())) {
                Ok(meta) => Ok(meta.len()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
                Err(err) => Err(from_io(err, &self.part_path().display().to_string())),
            }
        })
    }

    fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            match std::fs::OpenOptions::new()
                .write(true)
                .open(local::long_path(&part))
            {
                Ok(file) => file
                    .set_len(len)
                    .map_err(|err| from_io(err, &part.display().to_string())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(from_io(err, &part.display().to_string())),
            }
        })
    }

    fn read_partial<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>> {
        Box::pin(async move {
            let part = self.part_path();
            let meta = match std::fs::metadata(local::long_path(&part)) {
                Ok(meta) => meta,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(from_io(err, &part.display().to_string())),
            };
            if meta.len() < offset + u64::from(len) {
                return Ok(None);
            }
            let long = local::long_path(&part);
            let display = part.display().to_string();
            let data = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                let mut file = std::fs::File::open(long)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0u8; len as usize];
                let read = file.read(&mut buf)?;
                buf.truncate(read);
                Ok(buf)
            })
            .await
            .map_err(|err| SftpError::Internal(format!("本地读任务没跑起来：{err}")))?
            .map_err(|err| from_io(err, &display))?;
            Ok(Some(data))
        })
    }

    fn set_attributes<'a>(
        &'a self,
        mtime: Option<u32>,
        permissions: Option<u32>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            // 权限位在 Windows 上只有"只读"这一位有意义；用 std 的权限接口。
            if let Some(mode) = permissions {
                let readonly = mode & 0o200 == 0;
                let path = local::long_path(&self.path);
                let display = self.path.display().to_string();
                let meta = std::fs::metadata(&path).map_err(|err| from_io(err, &display))?;
                let mut perms = meta.permissions();
                perms.set_readonly(readonly);
                std::fs::set_permissions(&path, perms).map_err(|err| from_io(err, &display))?;
            }
            // 修改时间：Windows 上要用 SetFileTime，这里用 std 的 File::set_modified（Rust 1.75+）。
            if let Some(mtime) = mtime {
                let path = local::long_path(&self.path);
                let display = self.path.display().to_string();
                let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(mtime));
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|err| from_io(err, &display))?;
                file.set_modified(time)
                    .map_err(|err| from_io(err, &display))?;
            }
            Ok(())
        })
    }

    fn attributes<'a>(&'a self) -> EndpointFuture<'a, Result<FileAttributesPair, SftpError>> {
        Box::pin(async move {
            let meta = std::fs::metadata(local::long_path(&self.path))
                .map_err(|err| from_io(err, &self.path.display().to_string()))?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().min(u64::from(u32::MAX)) as u32);
            // 本地 → 远端：只读给 0o444，其余给 0o644（远端服务器会按 umask 处理）。
            let permissions = Some(if meta.permissions().readonly() {
                0o444
            } else {
                0o644
            });
            Ok((mtime, permissions))
        })
    }
}

struct LocalReader {
    file: std::fs::File,
    path: PathBuf,
}

impl ReadAt for LocalReader {
    fn read_at<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(async move { read_file_at(&self.file, &self.path, offset, len).await })
    }

    fn close<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { Ok(()) })
    }
}

struct LocalWriter {
    file: std::fs::File,
    path: PathBuf,
}

impl WriteAt for LocalWriter {
    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: Vec<u8>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let path = self.path.display().to_string();
            let file = self.file.try_clone().map_err(|err| from_io(err, &path))?;
            let len = data.len();
            tokio::task::spawn_blocking(move || -> std::io::Result<usize> {
                write_file_at(&file, offset, &data)
            })
            .await
            .map_err(|err| SftpError::Internal(format!("本地写任务没跑起来：{err}")))?
            .map_err(|err| from_io(err, &path))?;
            let _ = len;
            Ok(())
        })
    }

    fn finish<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let path = self.path.display().to_string();
            let file = self.file.try_clone().map_err(|err| from_io(err, &path))?;
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                let mut file = file;
                file.flush()?;
                // Windows 上 sync_all 会把数据刷到盘（FlushFileBuffers）。
                file.sync_all()
            })
            .await
            .map_err(|err| SftpError::Internal(format!("本地刷盘任务没跑起来：{err}")))?
            .map_err(|err| from_io(err, &path))
        })
    }
}

/// 按偏移读本地文件（Windows 用 `seek_read`，不需要额外的锁）。
async fn read_file_at(
    file: &std::fs::File,
    path: &Path,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, SftpError> {
    let display = path.display().to_string();
    let file = file.try_clone().map_err(|err| from_io(err, &display))?;
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len as usize];
        let read = read_at_impl(&file, offset, &mut buf)?;
        buf.truncate(read);
        Ok(buf)
    })
    .await
    .map_err(|err| SftpError::Internal(format!("本地读任务没跑起来：{err}")))?
    .map_err(|err| from_io(err, &display))
}

#[cfg(windows)]
fn read_at_impl(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

#[cfg(not(windows))]
fn read_at_impl(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    // 其它平台：借助 pread 语义（这里用 seek + read，调用方保证单个句柄同一时刻只读一次）。
    let mut file = file;
    file.seek(SeekFrom::Start(offset))?;
    file.read(buf)
}

#[cfg(windows)]
fn write_file_at(file: &std::fs::File, offset: u64, data: &[u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    let mut written = 0;
    while written < data.len() {
        let n = file.seek_write(&data[written..], offset + written as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "写入 0 字节",
            ));
        }
        written += n;
    }
    Ok(written)
}

#[cfg(not(windows))]
fn write_file_at(file: &std::fs::File, offset: u64, data: &[u8]) -> std::io::Result<usize> {
    let mut file = file;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_endpoint_round_trips_through_a_part_file() {
        let dir = crate::test_util::TestDir::new("endpoint");
        let endpoint = LocalEndpoint::new(dir.join("target.bin"));

        assert_eq!(endpoint.existing_size().await.unwrap(), None);
        assert_eq!(endpoint.partial_len().await.unwrap(), 0);
        assert_eq!(endpoint.read_partial(0, 4).await.unwrap(), None);

        // 分两段写（第二段是"续传"）。
        {
            let writer = endpoint.open_write(0).await.unwrap();
            writer.write_at(0, b"hello ".to_vec()).await.unwrap();
            writer.finish().await.unwrap();
        }
        assert_eq!(endpoint.partial_len().await.unwrap(), 6);
        assert_eq!(
            endpoint.read_partial(0, 6).await.unwrap().as_deref(),
            Some(&b"hello "[..])
        );
        // 请求超出已传长度：拿不到（说明不能续传）。
        assert_eq!(endpoint.read_partial(0, 64).await.unwrap(), None);

        {
            let writer = endpoint.open_write(6).await.unwrap();
            writer.write_at(6, b"world".to_vec()).await.unwrap();
            writer.finish().await.unwrap();
        }
        endpoint.commit(11, true).await.unwrap();
        assert_eq!(endpoint.existing_size().await.unwrap(), Some(11));
        assert_eq!(
            std::fs::read(dir.join("target.bin")).unwrap(),
            b"hello world"
        );
        assert!(!endpoint.part_path().exists(), "临时文件应当被改名掉");
    }

    #[tokio::test]
    async fn local_endpoint_can_read_back_what_it_wrote() {
        let dir = crate::test_util::TestDir::new("endpoint-read");
        let path = dir.join("source.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let endpoint = LocalEndpoint::new(&path);

        assert_eq!(endpoint.size().await.unwrap(), Some(10));
        let reader = endpoint.open_read().await.unwrap();
        assert_eq!(reader.read_at(3, 4).await.unwrap(), b"3456");
        assert_eq!(
            reader.read_at(8, 8).await.unwrap(),
            b"89",
            "读到尾就少给几字节"
        );
        reader.close().await.unwrap();

        let (mtime, permissions) = endpoint.attributes().await.unwrap();
        assert!(mtime.is_some(), "本地文件有修改时间");
        assert_eq!(permissions, Some(0o644));
    }

    // ---- F04：本地覆盖是原子替换、临时文件认主、同目标互斥 ----

    async fn write_all_to(endpoint: &LocalEndpoint, data: &[u8]) {
        let writer = endpoint.open_write(0).await.unwrap();
        writer.write_at(0, data.to_vec()).await.unwrap();
        writer.finish().await.unwrap();
    }

    /// 收尾时改名失败（临时文件被别的程序独占着）：旧文件必须还在、内容没变。
    #[cfg(windows)]
    #[tokio::test]
    async fn a_failed_rename_at_commit_leaves_the_old_file_intact() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = crate::test_util::TestDir::new("commit-fail");
        let target = dir.join("keep.txt");
        std::fs::write(&target, b"OLD CONTENT").unwrap();
        let endpoint = LocalEndpoint::new(&target);
        write_all_to(&endpoint, b"NEW CONTENT!").await;

        // 独占着临时文件（不允许别人改名 / 删除），模拟杀毒软件、同步盘之类的占用。
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(endpoint.part_path())
            .unwrap();
        let result = endpoint.commit(12, true).await;
        assert!(result.is_err(), "临时文件被占着，改名应该失败");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"OLD CONTENT",
            "改名失败之后旧文件不能丢、不能变"
        );

        // 占用解除之后重试能成功，并且是整体替换。
        drop(hold);
        endpoint.commit(12, true).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"NEW CONTENT!");
    }

    /// 用户自己恰好有一个叫 `<目标>.zssh-part` 的文件：不能当缓存截断、覆盖或改名掉。
    #[tokio::test]
    async fn a_users_own_zssh_part_file_is_never_touched() {
        let dir = crate::test_util::TestDir::new("user-part");
        let target = dir.join("report.txt");
        let users = dir.join("report.txt.zssh-part");
        std::fs::write(&users, b"USER DATA THAT MATTERS").unwrap();

        let endpoint = LocalEndpoint::new(&target);
        assert_eq!(
            endpoint.partial_len().await.unwrap(),
            0,
            "别人的文件不是我的临时文件，不能拿来续传"
        );
        write_all_to(&endpoint, b"downloaded").await;
        endpoint.commit(10, true).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"downloaded");
        assert_eq!(
            std::fs::read(&users).unwrap(),
            b"USER DATA THAT MATTERS",
            "用户自己的 .zssh-part 文件被动了"
        );

        // 中止（取消）时清理的也只能是自己的临时文件。
        let endpoint = LocalEndpoint::new(dir.join("other.bin"));
        write_all_to(&endpoint, b"x").await;
        endpoint.remove_partial().await.unwrap();
        assert!(users.exists());
    }

    /// 两个任务写同一个最终目标：各自的临时文件互不干扰。
    #[tokio::test]
    async fn two_tasks_for_one_target_do_not_share_a_part_file() {
        let dir = crate::test_util::TestDir::new("two-tasks");
        let target = dir.join("same.bin");
        let a = LocalEndpoint::new(&target);
        let b = LocalEndpoint::new(&target);
        write_all_to(&a, b"AAAA").await;
        write_all_to(&b, b"BBBBBB").await;
        assert_eq!(
            a.read_partial(0, 4).await.unwrap().as_deref(),
            Some(&b"AAAA"[..])
        );
        assert_eq!(
            b.read_partial(0, 6).await.unwrap().as_deref(),
            Some(&b"BBBBBB"[..])
        );
        assert_eq!(a.partial_len().await.unwrap(), 4);
        assert_eq!(b.partial_len().await.unwrap(), 6);
    }

    /// R-6b：不覆盖模式下收尾时目标位置冒出了一个同名文件：绝不能盖掉它，临时文件留着可续传。
    #[tokio::test]
    async fn commit_without_overwrite_never_replaces_a_file_that_appeared() {
        let dir = crate::test_util::TestDir::new("commit-no-overwrite");
        let target = dir.join("report.txt");
        let endpoint = LocalEndpoint::new(&target);
        write_all_to(&endpoint, b"DOWNLOADED").await;
        // 下载期间，用户（或别的程序）在目标位置建了一个同名文件。
        std::fs::write(&target, b"SOMEONE ELSES FILE").unwrap();

        let result = endpoint.commit(10, false).await;
        assert!(result.is_err(), "目标已存在，不覆盖模式必须失败");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"SOMEONE ELSES FILE",
            "那个文件被盖掉了"
        );
        assert_eq!(
            std::fs::read(endpoint.part_path()).unwrap(),
            b"DOWNLOADED",
            "临时文件要留着（可以续传 / 另存）"
        );
        // 目标不存在时不覆盖模式照常成功。
        std::fs::remove_file(&target).unwrap();
        endpoint.commit(10, false).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"DOWNLOADED");
    }
}
