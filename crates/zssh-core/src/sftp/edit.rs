//! 编辑会话（M3-3 第 5 节、M3-5 第 2 节）：一个远端文件 ↔ 一个本地临时文件。
//!
//! 内置编辑器用它：打开时下载内容并记下远端快照（大小 + 修改时间），保存时把文本写回临时文件、
//! 再上传（临时文件 + 改名、属主处理），上传前对一次远端、变了就让用户选 覆盖 / 放弃 / 另存为。
//!
//! 这一层只有**逻辑**（和界面、系统调用无关），远端读写通过 [`EditBackend`] 注入：
//! 生产实现是 [`SftpChannel`]，测试用内存里的假远端。Tauri 层（`commands/sftp_edit.rs`）负责
//! 恢复目录、事件和关闭时的清理；内置编辑器（`commands/sftp_edit/editor.rs`）负责文本编码和草稿。
//!
//! 冲突判定沿用设计文档说的办法：打开时记下远端的大小和修改时间，上传前再看一次，
//! 不一样（或者文件没了）就**不上传**，交给用户选 覆盖 / 放弃 / 另存为。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::error::SftpError;
use super::fs::{self, FileKind};
use super::session::SftpChannel;
use crate::ssh::BoxFuture;

/// 内置编辑器能打开的文件大小上限（超过就提示，不打开）。
pub const MAX_EDIT_BYTES: u64 = 5 * 1024 * 1024;

/// 远端文件此刻的样子：大小 + 修改时间（秒）。打开时记一份，上传前对一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub size: u64,
    pub mtime: Option<u32>,
}

/// 远端读写的注入口。
pub trait EditBackend: Send + Sync {
    /// 读属性（跟随链接）；不存在是 `Err(NotFound)`；第二项是"是不是目录"。
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>>;
    fn read_all<'a>(
        &'a self,
        path: &'a str,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>>;
    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>>;

    /// 覆盖一个**已经存在**的远端文件（编辑器保存）。`mode` 说明属主怎么处理；
    /// 默认实现不关心属主（单测里的简单替身用）。
    fn overwrite<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        _mode: OwnerMode,
    ) -> BoxFuture<'a, Result<OverwriteReport, OverwriteError>> {
        Box::pin(async move {
            self.write_all(path, data)
                .await
                .map(|()| OverwriteReport::default())
                .map_err(OverwriteError::Failed)
        })
    }

    /// 这台服务器的身份（用户名@主机:端口）。远端写的进程级锁按它 + 路径做键；
    /// 单测替身可以不实现（不加锁，和以前一样）。
    fn server_identity(&self) -> Option<String> {
        None
    }

    /// 符号链接解析后的真实路径（锁和传输队列一样按真实路径加）。
    /// 解析失败返回 `None`，调用方退回原路径。
    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Option<String>> {
        let _ = path;
        Box::pin(async { None })
    }

    /// 直接写原文件（截断后整段重写，不换文件）：`OwnerMode::InPlace` 用。
    /// 默认按普通写入处理；生产实现覆盖成不经过改名的直接写。
    fn write_in_place<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        self.write_all(path, data)
    }
}

/// 覆盖已有远端文件时，属主怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerMode {
    /// 先在临时文件上把属主设成原文件的；设不了就**不替换**，返回 [`OverwriteError::OwnerWouldChange`]。
    Require,
    /// 用户同意属主改变：设不了也照样替换，之后如实报告。
    Allow,
    /// 用户选了"直接写入原文件"：不换文件，截断后原地重写——属主不变，但中途断线可能留下半截文件。
    InPlace,
}

/// 覆盖成功的附带信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverwriteReport {
    /// 替换成功了，但属主没能保持（`Allow` 之下才会有）：从 X 变成 Y。
    pub owner_change: Option<crate::ai::files::OwnerChange>,
}

/// 覆盖失败。
#[derive(Debug)]
pub enum OverwriteError {
    Failed(SftpError),
    /// 属主保持不了，**什么都没替换**：问用户怎么办。
    OwnerWouldChange(crate::ai::files::OwnerChange),
}

impl EditBackend for SftpChannel {
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        Box::pin(async move {
            let info = fs::stat(self, path, true).await?;
            let is_dir = matches!(info.kind, FileKind::Dir);
            Ok((
                RemoteSnapshot {
                    size: info.size.unwrap_or(0),
                    mtime: info.mtime,
                },
                is_dir,
            ))
        })
    }

    fn read_all<'a>(
        &'a self,
        path: &'a str,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(fs::read_bytes(self, path, limit))
    }

    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let fs = crate::ai::files::SftpRemoteFs::borrowed(self);
            upload_via_fs(
                &fs,
                path,
                data,
                &uuid::Uuid::new_v4().simple().to_string()[..12],
                OwnerMode::Allow,
            )
            .await
            .map(|_report| ())
            .map_err(|err| match err {
                OverwriteError::Failed(err) => err,
                OverwriteError::OwnerWouldChange(change) => {
                    SftpError::Remote(format!("属主会从 {} 变成 {}", change.from, change.to))
                }
            })
        })
    }

    fn overwrite<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: OwnerMode,
    ) -> BoxFuture<'a, Result<OverwriteReport, OverwriteError>> {
        Box::pin(async move {
            if mode == OwnerMode::InPlace {
                // 直接写原文件：不换文件，属主不变。锁在 overwrite_in_place 里统一拿。
                return overwrite_in_place(self, path, data).await;
            }
            let remote = crate::ai::files::SftpRemoteFs::borrowed(self);
            upload_via_fs(
                &remote,
                path,
                data,
                &uuid::Uuid::new_v4().simple().to_string()[..12],
                mode,
            )
            .await
        })
    }

    fn server_identity(&self) -> Option<String> {
        Some(self.server_key().to_owned())
    }

    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move { self.realpath(path).await.ok() })
    }

    fn write_in_place<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            fs::write_bytes(self, path, data, u64::MAX)
                .await
                .map(|_| ())
        })
    }
}

/// 把编辑后的内容传到远端：和 AI 的 `write_file` 一样，先写同目录的临时文件、核对大小、
/// 再改名覆盖（服务器支持 `posix-rename` 就一步到位），原文件的权限和属主尽量保持不变。
/// 任何一步失败，远端原文件都是完整的旧内容，临时文件也会清掉。
///
/// 属主：`Require` 先把临时文件的属主设成原文件的，设不了就**不替换**、返回
/// [`OverwriteError::OwnerWouldChange`]；`Allow` 设不了也替换，在返回值里如实报告。
///
/// 目标是符号链接时写到链接指向的真实文件，不把链接本身换掉。
pub async fn upload_via_fs(
    fs: &dyn crate::ai::files::RemoteFs,
    path: &str,
    data: &[u8],
    random_hex: &str,
    mode: OwnerMode,
) -> Result<OverwriteReport, OverwriteError> {
    let target = match fs.realpath(path).await {
        Ok(real) => real,
        Err(_) => path.to_owned(),
    };
    let base = match fs
        .stat(&target)
        .await
        .map_err(|err| OverwriteError::Failed(SftpError::Remote(err)))?
    {
        Some(stat) => crate::ai::files::ReplaceBase {
            existed: true,
            mode: stat.mode.map(|mode| mode & 0o7777),
            uid: stat.uid,
            gid: stat.gid,
        },
        None => crate::ai::files::ReplaceBase::default(),
    };
    let policy = match mode {
        OwnerMode::Require => crate::ai::files::OwnerPolicy::Require,
        OwnerMode::Allow | OwnerMode::InPlace => crate::ai::files::OwnerPolicy::Allow,
    };
    match crate::ai::files::replace_via_temp_with(fs, &target, data, &base, random_hex, policy)
        .await
    {
        Ok(owner_change) => Ok(OverwriteReport { owner_change }),
        Err(crate::ai::files::ReplaceError::Failed(message)) => {
            Err(OverwriteError::Failed(SftpError::Remote(message)))
        }
        Err(crate::ai::files::ReplaceError::OwnerWouldChange(change)) => {
            Err(OverwriteError::OwnerWouldChange(change))
        }
    }
}

/// 直接写原文件（`OwnerMode::InPlace`，R-8 第 4 节）。
///
/// 生产实现和测试替身都走这里，锁的逻辑只写一份：先解析真实路径，再按真实路径拿传输
/// 队列同一把进程级远端目标锁，锁覆盖打开→写入→关闭全过程；拿不到锁什么都不写。
pub async fn overwrite_in_place<B: EditBackend + ?Sized>(
    backend: &B,
    path: &str,
    data: &[u8],
) -> Result<OverwriteReport, OverwriteError> {
    let target = backend
        .real_path(path)
        .await
        .unwrap_or_else(|| path.to_owned());
    // 和传输队列、AI 写文件、编辑器"临时文件+改名"共用同一把进程级远端目标锁：
    // 键 = 服务器身份 + 真实路径。守卫活到 write_in_place 的 future 结束，覆盖
    // "打开 → 写入 → fsync → 关闭"整个过程；拿不到锁时什么都不写。
    let _write_lock = match backend.server_identity() {
        Some(server) => Some(
            crate::sftp::transfer::lock::TargetLocks::remote()
                .try_acquire_remote(&server, &target)
                .map_err(OverwriteError::Failed)?,
        ),
        None => None,
    };
    backend
        .write_in_place(&target, data)
        .await
        .map(|()| OverwriteReport::default())
        .map_err(OverwriteError::Failed)
}

impl<T: EditBackend + ?Sized> EditBackend for Arc<T> {
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        (**self).stat(path)
    }
    fn read_all<'a>(
        &'a self,
        path: &'a str,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>> {
        (**self).read_all(path, limit)
    }
    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        (**self).write_all(path, data)
    }
    fn overwrite<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: OwnerMode,
    ) -> BoxFuture<'a, Result<OverwriteReport, OverwriteError>> {
        (**self).overwrite(path, data, mode)
    }
    fn server_identity(&self) -> Option<String> {
        (**self).server_identity()
    }
    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Option<String>> {
        (**self).real_path(path)
    }
    fn write_in_place<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        (**self).write_in_place(path, data)
    }
}

/// 打不开的原因（界面分别提示）。
#[derive(Debug)]
pub enum OpenError {
    /// 超过上限：不打开。
    TooLarge {
        size: u64,
        limit: u64,
    },
    /// 是目录、设备之类的。
    NotAFile,
    Remote(SftpError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::TooLarge { size, limit } => write!(
                f,
                "文件有 {size} 字节，超过内置编辑器的上限（{limit} 字节），请下载后再看"
            ),
            OpenError::NotAFile => f.write_str("这不是普通文件，不能编辑"),
            OpenError::Remote(err) => write!(f, "{err}"),
        }
    }
}

/// 打开前下载：返回内容和打开时的远端快照。二进制 / 编码问题由内置编辑器解码时判断
/// （它要区分 UTF-16 和二进制，这里不嗅探）。
pub async fn download<B: EditBackend>(
    backend: &B,
    remote_path: &str,
    limit: u64,
) -> Result<(Vec<u8>, RemoteSnapshot), OpenError> {
    let (snapshot, is_dir) = backend.stat(remote_path).await.map_err(OpenError::Remote)?;
    if is_dir {
        return Err(OpenError::NotAFile);
    }
    if snapshot.size > limit {
        return Err(OpenError::TooLarge {
            size: snapshot.size,
            limit,
        });
    }
    let data = backend
        .read_all(remote_path, limit)
        .await
        .map_err(|err| match err {
            SftpError::TooLarge { size, limit, .. } => OpenError::TooLarge { size, limit },
            other => OpenError::Remote(other),
        })?;
    // 下载过程中远端可能又变了：以下载完之后的属性为准（和内容最接近）。
    let after = backend
        .stat(remote_path)
        .await
        .map(|(snapshot, _)| snapshot)
        .unwrap_or(snapshot);
    Ok((data, after))
}

/// 对用户的三个选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// 用本地内容盖掉远端。
    Overwrite,
    /// 放弃本地修改：把远端现在的内容重新下载回临时文件。
    Discard,
    /// 另存为：在同一目录下用这个新名字存一份，之后这个编辑会话改跟踪新文件。
    SaveAs(String),
    /// 属主保持不了时用户选了"仍然保存（属主改变）"。
    KeepNewOwner,
    /// 属主保持不了时用户选了"直接写入原文件（保留属主，但中途断线可能留下半截文件）"。
    InPlace,
}

/// 一次处理本地保存的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum EditOutcome {
    Uploaded {
        size: u64,
    },
    /// 放弃本地修改后，临时文件已经换回远端的内容。
    Discarded,
    /// 远端变了（`Some` 是它现在的样子）或者被删了（`None`）：没有上传，等用户选。
    Conflict {
        remote: Option<RemoteSnapshot>,
    },
    /// 保存后属主会改变（没权限改回去），**什么都没有替换**：等用户选 仍然保存 / 直接写入原文件 / 取消。
    OwnerWouldChange(crate::ai::files::OwnerChange),
    Failed(String),
}

/// 一个编辑会话（一个远端文件 ↔ 一个本地临时文件）。
#[derive(Debug)]
pub struct EditSession {
    pub remote_path: String,
    pub local_path: PathBuf,
    base: RemoteSnapshot,
    /// 远端变了、等用户决定。
    conflicted: bool,
    /// 上一次上传失败了、等重试。
    failed: bool,
    /// 最近一次和远端对齐的内容的 SHA-256：**实际上传（或从远端拉回来）的那些字节**的摘要，
    /// 不是之后又去读本地文件——上传期间用户可能又保存了一次。
    synced_sha256: Option<String>,
    /// 最近一次上传成功了，但属主没能保持（从 X 变成 Y）：要明确告诉用户。
    owner_change: Option<crate::ai::files::OwnerChange>,
}

impl EditSession {
    pub fn new(remote_path: String, local_path: PathBuf, base: RemoteSnapshot) -> Self {
        Self {
            remote_path,
            local_path,
            base,
            conflicted: false,
            failed: false,
            synced_sha256: None,
            owner_change: None,
        }
    }

    /// 最近一次上传成功后属主变了没有（变了就是 `Some`）。
    pub fn owner_change(&self) -> Option<&crate::ai::files::OwnerChange> {
        self.owner_change.as_ref()
    }

    /// 最近一次成功对齐远端的那份内容的摘要（上传成功 / 放弃本地修改 / 另存为之后才有）。
    pub fn synced_sha256(&self) -> Option<&str> {
        self.synced_sha256.as_deref()
    }

    pub fn base(&self) -> RemoteSnapshot {
        self.base
    }

    /// 有没有没落到远端的修改（冲突挂着、上传失败过）。
    pub fn has_pending_problem(&self) -> bool {
        self.conflicted || self.failed
    }

    /// 本地保存了：先对一遍远端，没变就上传。
    pub async fn on_local_saved<B: EditBackend + ?Sized>(&mut self, backend: &B) -> EditOutcome {
        let data = match tokio::fs::read(&self.local_path).await {
            Ok(data) => data,
            Err(err) => {
                self.failed = true;
                return EditOutcome::Failed(format!("读不了本地临时文件：{err}"));
            }
        };
        match backend.stat(&self.remote_path).await {
            Ok((now, _)) if now == self.base => {}
            Ok((now, _)) => {
                self.conflicted = true;
                return EditOutcome::Conflict { remote: Some(now) };
            }
            Err(SftpError::NotFound { .. }) => {
                self.conflicted = true;
                return EditOutcome::Conflict { remote: None };
            }
            Err(err) => {
                self.failed = true;
                return EditOutcome::Failed(err.to_string());
            }
        }
        self.upload(backend, &data, OwnerMode::Require).await
    }

    async fn upload<B: EditBackend + ?Sized>(
        &mut self,
        backend: &B,
        data: &[u8],
        mode: OwnerMode,
    ) -> EditOutcome {
        self.owner_change = None;
        let report = match backend.overwrite(&self.remote_path, data, mode).await {
            Ok(report) => report,
            Err(OverwriteError::OwnerWouldChange(change)) => {
                // 什么都没替换：这次保存还在等用户决定，算"没上传的修改"。
                self.failed = true;
                return EditOutcome::OwnerWouldChange(change);
            }
            Err(OverwriteError::Failed(err)) => {
                self.failed = true;
                return EditOutcome::Failed(err.to_string());
            }
        };
        self.owner_change = report.owner_change;
        match backend.stat(&self.remote_path).await {
            Ok((after, _)) => self.base = after,
            Err(err) => {
                // 上传成功但读不回属性：下次保存会被当成"远端变了"，保守地报失败。
                self.failed = true;
                return EditOutcome::Failed(format!("已上传，但读不回远端属性：{err}"));
            }
        }
        self.conflicted = false;
        self.failed = false;
        self.synced_sha256 = Some(recovery::sha256_hex(data));
        EditOutcome::Uploaded {
            size: data.len() as u64,
        }
    }

    /// 用户在冲突提示里做了选择。
    pub async fn resolve<B: EditBackend + ?Sized>(
        &mut self,
        backend: &B,
        resolution: Resolution,
    ) -> EditOutcome {
        match resolution {
            Resolution::Overwrite => {
                let data = match tokio::fs::read(&self.local_path).await {
                    Ok(data) => data,
                    Err(err) => return EditOutcome::Failed(format!("读不了本地临时文件：{err}")),
                };
                self.upload(backend, &data, OwnerMode::Require).await
            }
            Resolution::KeepNewOwner | Resolution::InPlace => {
                // 用户看到"属主会改变"以后做的选择。这期间远端可能又被改了：先对一遍。
                match backend.stat(&self.remote_path).await {
                    Ok((now, _)) if now == self.base => {}
                    Ok((now, _)) => {
                        self.conflicted = true;
                        return EditOutcome::Conflict { remote: Some(now) };
                    }
                    Err(SftpError::NotFound { .. }) => {
                        self.conflicted = true;
                        return EditOutcome::Conflict { remote: None };
                    }
                    Err(err) => return EditOutcome::Failed(err.to_string()),
                }
                let data = match tokio::fs::read(&self.local_path).await {
                    Ok(data) => data,
                    Err(err) => return EditOutcome::Failed(format!("读不了本地临时文件：{err}")),
                };
                let mode = if resolution == Resolution::InPlace {
                    OwnerMode::InPlace
                } else {
                    OwnerMode::Allow
                };
                self.upload(backend, &data, mode).await
            }
            Resolution::Discard => {
                // 沿用打开时的上限：编辑期间远端可能变得很大（或者服务器报的大小不可信），
                // 不设上限会把整个文件读进内存。超限时本地临时文件一个字节都不动，
                // 会话仍处于冲突状态，用户可以改选覆盖 / 另存为。
                let data = match backend.read_all(&self.remote_path, MAX_EDIT_BYTES).await {
                    Ok(data) => data,
                    Err(SftpError::TooLarge { size, limit, .. }) => {
                        return EditOutcome::Failed(format!(
                            "远端文件现在有 {size} 字节，超过内置编辑器的上限（{limit} 字节），                             没有放弃本地修改；请下载后再看，或改选覆盖 / 另存为"
                        ))
                    }
                    Err(err) => return EditOutcome::Failed(err.to_string()),
                };
                if let Err(err) = tokio::fs::write(&self.local_path, &data).await {
                    return EditOutcome::Failed(format!("写不了本地临时文件：{err}"));
                }
                match backend.stat(&self.remote_path).await {
                    Ok((now, _)) => self.base = now,
                    Err(err) => return EditOutcome::Failed(err.to_string()),
                }
                self.conflicted = false;
                self.failed = false;
                self.synced_sha256 = Some(recovery::sha256_hex(&data));
                EditOutcome::Discarded
            }
            Resolution::SaveAs(name) => {
                if let Err(reason) = validate_remote_name(&name) {
                    return EditOutcome::Failed(reason);
                }
                let target = sibling_remote_path(&self.remote_path, name.trim());
                if backend.stat(&target).await.is_ok() {
                    return EditOutcome::Failed(format!("{target} 已经存在，换个名字"));
                }
                let data = match tokio::fs::read(&self.local_path).await {
                    Ok(data) => data,
                    Err(err) => return EditOutcome::Failed(format!("读不了本地临时文件：{err}")),
                };
                if let Err(err) = backend.write_all(&target, &data).await {
                    return EditOutcome::Failed(err.to_string());
                }
                match backend.stat(&target).await {
                    Ok((after, _)) => {
                        self.remote_path = target;
                        self.base = after;
                        self.conflicted = false;
                        self.failed = false;
                        self.synced_sha256 = Some(recovery::sha256_hex(&data));
                        EditOutcome::Uploaded {
                            size: data.len() as u64,
                        }
                    }
                    Err(err) => EditOutcome::Failed(err.to_string()),
                }
            }
        }
    }
}

/// 同目录下换个名字（远端是 POSIX 路径）。
pub fn sibling_remote_path(path: &str, name: &str) -> String {
    match path.rfind('/') {
        Some(0) => format!("/{name}"),
        Some(cut) => format!("{}/{}", &path[..cut], name),
        None => name.to_owned(),
    }
}

/// 另存为的新名字：不能为空、不能带路径分隔符。
pub fn validate_remote_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("名字不能为空".to_owned());
    }
    if trimmed.contains('/') || trimmed == "." || trimmed == ".." || trimmed.contains('\0') {
        return Err("名字里不能有 /，也不能是 . 或 ..".to_owned());
    }
    Ok(())
}

/// 给"另存为"一个默认名字：`a.txt` → `a (副本).txt`。
pub fn default_save_as_name(remote_path: &str) -> String {
    let name = remote_path.rsplit('/').next().unwrap_or(remote_path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} (副本){}", &name[..dot], &name[dot..]),
        _ => format!("{name} (副本)"),
    }
}

/// 临时文件的名字：保留原名（编辑器标题栏、扩展名决定语法高亮和默认程序），
/// 但去掉 Windows 不允许的字符和保留名。
pub fn sanitize_local_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_owned();
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if cleaned.is_empty() || reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// 编辑的临时目录：放在数据目录下（遵守 `ZSSH_DATA_DIR`），不放系统临时目录。
pub fn edit_root(data_dir: &Path) -> PathBuf {
    data_dir.join("edit-tmp")
}

/// 删目录（不存在、删不掉都不报错：清理是尽力而为）。
pub fn remove_dir_quiet(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

pub mod recovery;
pub mod text;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod upload_tests;
