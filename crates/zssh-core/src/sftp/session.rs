//! SFTP 会话（M3-1 第 1 节）：在连接池已有的连接上开 `sftp` 子系统通道。
//!
//! 要点：
//! - **不重新认证**：通道开在已有连接上（`Connection::open_sftp_channel`）。
//! - **通道预算**：一条连接上的会话通道是有限的（OpenSSH 默认 `MaxSessions 10`），
//!   终端、服务器状态栏的 exec、AI 的后台 exec 都在抢。所以我们**只开需要的**：
//!   浏览/属性/小文件操作共用一条（[`SftpChannel`]），传输任务各自开一条。
//! - **被拒绝时不拖累终端**：开通道被服务器拒绝（多半是到上限）时，返回
//!   [`SftpError::ChannelLimit`]，由传输层改走一条**专用新连接**（见
//!   [`crate::sftp::transfer`] 的 `SftpReconnect`），终端和别的功能不受影响。
//! - **服务器没开 SFTP**：子系统请求被拒 → [`SftpError::NoSubsystem`]，不卡住。
//!
//! 设计上直接用 `russh_sftp::client::RawSftpSession`（不是它的高层 `SftpSession`）：
//! 我们需要**自己发 INIT 拿扩展表**（判断 `posix-rename` / `fsync` / `statvfs` /
//! `limits` 有没有），还要**自己控制请求的偏移和并发**（传输引擎的流水线、续传、
//! 进度都按块来）。高层封装的 AsyncRead/AsyncWrite 把这两件事都藏在里面了。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use russh_sftp::client::RawSftpSession;
use russh_sftp::extensions::{self, LimitsExtension};
use russh_sftp::protocol::{FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode};

use super::error::{from_status_code, with_path, SftpError};
use crate::ssh::Connection;

/// 保守的读写块大小（服务器没声明 `limits@openssh.com` 时用）。
///
/// 32KB 是 OpenSSH 的 sftp 服务器长期用的报文上限（`SFTP_MAX_MSG_LENGTH`），
/// 几乎所有实现的缓冲区都吃得下；宁可小一点，也不要因为一块太大被服务器拒。
pub const CONSERVATIVE_CHUNK: u32 = 32 * 1024;

/// 单次请求的上限（就算服务器声明了更大的值也不超这个：再大内存和延迟都不划算）。
pub const MAX_CHUNK: u32 = 256 * 1024;

/// `posix-rename@openssh.com`（重命名时目标存在也能覆盖）。
pub const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// 服务器声明的限制（`limits@openssh.com`）。自己再定义一份是因为上游的
/// `LimitsExtension` 没有 `Clone` / `PartialEq`，我们要往事件和测试断言里带。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SftpLimits {
    pub max_packet_len: u64,
    pub max_read_len: u64,
    pub max_write_len: u64,
    pub max_open_handles: u64,
}

impl From<LimitsExtension> for SftpLimits {
    fn from(limits: LimitsExtension) -> Self {
        Self {
            max_packet_len: limits.max_packet_len,
            max_read_len: limits.max_read_len,
            max_write_len: limits.max_write_len,
            max_open_handles: limits.max_open_handles,
        }
    }
}

/// 会话协商出来的能力（M3-1 第 2 节：扩展有就用、没有就降级）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SftpCaps {
    /// 协议版本（读出来的；3 是绝大多数服务器）。
    pub version: u32,
    /// 服务器支持的扩展名（排序后，便于比较和展示）。
    pub extensions: Vec<String>,
    /// `limits@openssh.com` 报的服务器限制（没有就是 `None`）。
    pub limits: Option<SftpLimits>,
}

impl SftpCaps {
    /// 从 INIT 应答的扩展表里读出能力（`limits` 要再问一次，由调用方补）。
    pub fn from_extensions(version: u32, extensions: &HashMap<String, String>) -> Self {
        let mut names: Vec<String> = extensions.keys().cloned().collect();
        names.sort();
        Self {
            version,
            extensions: names,
            limits: None,
        }
    }

    pub fn has(&self, extension: &str) -> bool {
        self.extensions.iter().any(|name| name == extension)
    }

    /// 一次读请求最多要多少字节。
    pub fn max_read_len(&self) -> u32 {
        match self.limits.as_ref().map(|limits| limits.max_read_len) {
            Some(len) if len > 0 => u32::try_from(len).unwrap_or(MAX_CHUNK).min(MAX_CHUNK),
            _ => CONSERVATIVE_CHUNK,
        }
    }

    /// 一次写请求最多发多少字节。
    pub fn max_write_len(&self) -> u32 {
        match self.limits.as_ref().map(|limits| limits.max_write_len) {
            Some(len) if len > 0 => u32::try_from(len).unwrap_or(MAX_CHUNK).min(MAX_CHUNK),
            _ => CONSERVATIVE_CHUNK,
        }
    }

    /// 服务器允许同时打开多少个文件句柄（没声明就给保守值）。
    ///
    /// 传输引擎用它限制"同时打开的文件数"，避免被服务器拒绝。
    pub fn max_open_handles(&self) -> u64 {
        match self.limits.as_ref().map(|limits| limits.max_open_handles) {
            Some(count) if count > 0 => count,
            _ => 4,
        }
    }

    /// 报文长度上限（做流水线时别把包做太大）。
    pub fn max_packet_len(&self) -> u32 {
        match self.limits.as_ref().map(|limits| limits.max_packet_len) {
            Some(len) if len > 0 => u32::try_from(len)
                .unwrap_or(MAX_CHUNK * 2)
                .min(MAX_CHUNK * 2),
            _ => CONSERVATIVE_CHUNK * 2,
        }
    }

    /// 给报告 / 界面看的一句话。
    pub fn describe(&self) -> String {
        let mut parts = vec![format!("协议 v{}", self.version)];
        if self.has(extensions::LIMITS) {
            parts.push(format!(
                "limits：读 {} / 写 {} / 句柄 {}",
                self.max_read_len(),
                self.max_write_len(),
                self.max_open_handles()
            ));
        } else {
            parts.push(format!(
                "没有 limits 扩展，用保守值 {CONSERVATIVE_CHUNK} 字节"
            ));
        }
        for (name, label) in [
            (POSIX_RENAME, "posix-rename"),
            (extensions::FSYNC, "fsync"),
            (extensions::STATVFS, "statvfs"),
            (extensions::HARDLINK, "hardlink"),
            (extensions::EXPAND_PATH, "expand-path"),
            (COPY_DATA, "copy-data"),
        ] {
            if self.has(name) {
                parts.push(label.to_owned());
            }
        }
        parts.join("；")
    }
}

/// 一条 SFTP 通道（= 一个子系统会话）。
///
/// `RawSftpSession` 把底层通道的读写拆成多个请求复用同一条通道（响应按请求号归位），
/// 所以浏览目录、读属性、小文件操作共用这一条就够了。
pub struct SftpChannel {
    raw: Arc<RawSftpSession>,
    caps: SftpCaps,
    connection_id: usize,
    /// 这条通道连的是哪台服务器（用户名@主机:端口），开通道时从连接上取。远端目标锁的键里要用。
    server_key: String,
}

/// 远端锁用的服务器身份：连接实际连到的 用户名@主机:端口。
///
/// 连接上没有记下身份（主机或用户名是空的，正常使用时不会发生）时退一步，用**这条 SSH 连接**
/// 当键：同一条连接上的写入至少互斥。
fn identity_of(connection: &Arc<Connection>) -> String {
    match connection.identity() {
        Some(identity) => identity.to_owned(),
        None => format!("connection#{}", Arc::as_ptr(connection) as usize),
    }
}

impl SftpChannel {
    /// 在已有连接上开一条 `sftp` 子系统通道，完成 INIT 握手。
    pub async fn open(connection: &Arc<Connection>) -> Result<Self, SftpError> {
        let stream = connection.open_sftp_channel().await?;
        let raw = Arc::new(RawSftpSession::new(stream));
        let version = raw.init().await.map_err(|err| match SftpError::from(err) {
            // INIT 阶段通道就断了：多数是服务端把子系统通道关掉了（没装 sftp-server）。
            // 真断网也会落到这里，所以文案里两种可能都提。
            SftpError::Disconnected(message) => SftpError::NoSubsystem(message),
            other => other,
        })?;
        let mut caps = SftpCaps::from_extensions(version.version, &version.extensions);
        // 服务器声明了 limits 才去问；没声明问了也是白问。
        if caps.has(extensions::LIMITS) {
            if let Ok(limits) = raw.limits().await {
                caps.limits = Some(SftpLimits::from(limits));
            }
        }
        Ok(Self {
            raw,
            caps,
            connection_id: Arc::as_ptr(connection) as usize,
            server_key: identity_of(connection),
        })
    }

    pub fn caps(&self) -> &SftpCaps {
        &self.caps
    }

    /// 这条通道连的服务器身份（用户名@主机:端口）。
    pub fn server_key(&self) -> &str {
        &self.server_key
    }

    /// **只给测试用**：把服务器声明的某个扩展当作没有（例如屏蔽 `posix-rename@openssh.com`，
    /// 在支持它的真实服务器上走"没有扩展"的降级路径）。生产代码不调用。
    pub fn hide_extension_for_tests(&mut self, name: &str) {
        self.caps.extensions.retain(|candidate| candidate != name);
    }

    /// 这条通道开在哪条连接上（连接对象地址；只用于同一性判断，测试断言"没重新连"）。
    pub fn connection_id(&self) -> usize {
        self.connection_id
    }

    /// 原始会话（传输层做带偏移的读写）。
    pub fn raw(&self) -> &Arc<RawSftpSession> {
        &self.raw
    }

    /// 关闭通道（会话收尾时调；不调也会随结构体释放关掉底层通道）。
    pub fn close(&self) {
        let _ = self.raw.close_session();
    }

    // ---------- 属性 / 路径 ----------

    /// `stat`：跟随符号链接。
    pub async fn stat(&self, path: &str) -> Result<FileAttributes, SftpError> {
        self.guard_name(path).await?;
        self.raw
            .stat(path)
            .await
            .map(|attrs| attrs.attrs)
            .map_err(|err| with_path(err, path))
    }

    /// `lstat`：链接本身（不跟随）。
    pub async fn lstat(&self, path: &str) -> Result<FileAttributes, SftpError> {
        self.raw
            .lstat(path)
            .await
            .map(|attrs| attrs.attrs)
            .map_err(|err| with_path(err, path))
    }

    /// 文件句柄的属性（写完之后确认大小用）。
    pub async fn fstat(&self, handle: &str) -> Result<FileAttributes, SftpError> {
        self.raw
            .fstat(handle)
            .await
            .map(|attrs| attrs.attrs)
            .map_err(SftpError::from)
    }

    /// 服务端真实路径（`realpath`）。
    ///
    /// chroot 的 `internal-sftp` 里家目录可能就是 `/`，和登录 shell 看到的家目录不一样；
    /// 这是服务器的事，我们如实返回。
    pub async fn realpath(&self, path: &str) -> Result<String, SftpError> {
        let name = self
            .raw
            .realpath(path)
            .await
            .map_err(|err| with_path(err, path))?;
        first_name(name).ok_or_else(|| SftpError::Remote("realpath 没有返回路径".to_owned()))
    }

    /// 操作前的名字检查（M3-1 第 2 节）：名字里有替换字符（U+FFFD）时先确认服务器上
    /// 到底有没有这个字面名字——没有就说明真实字节不是 UTF-8。**宁可报错也不拿有损的
    /// 名字去操作**（那会把服务器上的名字改坏）。
    ///
    /// 名字正常时这是一次纯检查、不发请求。
    pub async fn guard_name(&self, path: &str) -> Result<(), SftpError> {
        let name = super::path::file_name(path);
        if !name.contains(char::REPLACEMENT_CHARACTER) {
            return Ok(());
        }
        match self.lstat(path).await {
            Ok(_) => Ok(()),
            Err(SftpError::NotFound { .. }) => Err(SftpError::NonUtf8Name { name: name.clone() }),
            Err(_) => Ok(()), // 别的错误交给真正的操作去报
        }
    }

    /// 读符号链接指向哪里（`readlink`）。
    pub async fn read_link(&self, path: &str) -> Result<String, SftpError> {
        let name = self
            .raw
            .readlink(path)
            .await
            .map_err(|err| with_path(err, path))?;
        first_name(name).ok_or_else(|| SftpError::Remote("readlink 没有返回目标".to_owned()))
    }

    // ---------- 目录 ----------

    /// 打开目录、读完整列表（`opendir` + 反复 `readdir` 到 EOF + `close`）。
    pub async fn read_dir_raw(
        &self,
        path: &str,
    ) -> Result<Vec<russh_sftp::protocol::File>, SftpError> {
        let handle: Handle = self
            .raw
            .opendir(path)
            .await
            .map_err(|err| with_path(err, path))?;
        let handle_id = handle.handle;
        let mut out = Vec::new();
        let mut failure = None;
        loop {
            match self.raw.readdir(&handle_id).await {
                Ok(name) => {
                    let empty = name.files.is_empty();
                    out.extend(name.files);
                    if empty {
                        break;
                    }
                }
                Err(err) => {
                    // EOF 是正常的结束信号（有些服务器用它代替空列表）。
                    match err {
                        russh_sftp::client::error::Error::Status(status)
                            if status.status_code == StatusCode::Eof =>
                        {
                            break
                        }
                        other => {
                            failure = Some(with_path(other, path));
                            break;
                        }
                    }
                }
            }
        }
        // 句柄一定要关（服务器有句柄上限）。
        let _ = self.raw.close(&handle_id).await;
        match failure {
            Some(err) => Err(err),
            None => Ok(out),
        }
    }

    /// 列目录：符号链接显示链接本身，同时带上目标的类型。
    pub async fn read_dir(&self, path: &str) -> Result<Vec<super::fs::DirEntry>, SftpError> {
        let entries = self.read_dir_raw(path).await?;
        super::fs::entries_from_protocol(self, path, entries).await
    }

    // ---------- 写操作 ----------

    /// 建目录。
    pub async fn create_dir(&self, path: &str) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let status = self
            .raw
            .mkdir(path, FileAttributes::default())
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 建一个空文件（`create + truncate` 之后立刻关掉）。
    pub async fn create_file(&self, path: &str) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let handle = self.open_write_truncate(path).await?;
        self.close_handle(&handle).await
    }

    /// 删除文件。
    pub async fn remove_file(&self, path: &str) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let status = self
            .raw
            .remove(path)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 删除空目录。
    pub async fn remove_dir(&self, path: &str) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let status = self
            .raw
            .rmdir(path)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 重命名 / 移动。
    ///
    /// 不覆盖：普通 rename，目标已存在就失败（`AlreadyExists`）。覆盖：服务器支持
    /// `posix-rename@openssh.com` 就用它；不支持就走"备用名"三步（见 `sftp::replace`），
    /// 任何一步失败服务器上都至少有一份完整的旧文件或新文件。
    pub async fn rename(
        &self,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<RenameOutcome, SftpError> {
        self.guard_name(from).await?;
        self.guard_name(to).await?;
        // 走哪条路由能力决定（见 `rename_plan` 的单测）。
        if rename_plan(&self.caps, overwrite) == RenamePlan::PosixRename {
            let reply = self
                .raw
                .extended(POSIX_RENAME, posix_rename_payload(from, to))
                .await
                .map_err(SftpError::from)?;
            return match reply {
                Packet::Status(status) => {
                    status_ok(status, to).map(|()| RenameOutcome::PosixRename)
                }
                // 有的实现用扩展应答回一个空包表示成功。
                Packet::ExtendedReply(_) => Ok(RenameOutcome::PosixRename),
                _ => Err(SftpError::Remote(
                    "posix-rename 收到了意料之外的回包".to_owned(),
                )),
            };
        }
        if rename_plan(&self.caps, overwrite) == RenamePlan::RemoveThenRename {
            // 不支持扩展：不能先删目标——改名失败旧文件就没了。三步走：原文件改名让开 →
            // 新文件改名到位（失败就改回来）→ 删备用文件。见 `sftp::replace`。
            let token = uuid::Uuid::new_v4().simple().to_string();
            crate::sftp::replace::replace_with_backup(&ChannelOps(self), from, to, &token[..12])
                .await?;
            return Ok(RenameOutcome::RemoveThenRename);
        }
        let result = self
            .raw
            .rename(from, to)
            .await
            .map_err(|err| with_path(err, from))
            .and_then(|status| status_ok(status, from));
        match result {
            Ok(()) => Ok(RenameOutcome::Plain),
            // 服务器对"目标已存在"只回一个光秃秃的 Failure：看一眼目标，真在就说清楚。
            Err(err) if !overwrite && !matches!(err, SftpError::NotFound { .. }) => {
                match self.raw.lstat(to).await {
                    Ok(_) => Err(SftpError::AlreadyExists {
                        path: to.to_owned(),
                    }),
                    Err(_) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// 改权限（只给权限位，不动别的属性）。
    pub async fn set_permissions(&self, path: &str, mode: u32) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..FileAttributes::default()
        };
        let status = self
            .raw
            .setstat(path, attrs)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 改属主/属组（`setstat` 的 uid/gid）。
    ///
    /// `write_file` 改名之后用它把原文件的属主改回去；服务器通常只允许 root
    /// 或文件属主自己改，权限不够时返回错误，由调用方报告"属主变了"。
    pub async fn set_owner(
        &self,
        path: &str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let attrs = FileAttributes {
            uid,
            gid,
            ..FileAttributes::default()
        };
        let status = self
            .raw
            .setstat(path, attrs)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 把远端文件截断到 `len`（`setstat` 的 size；服务器不支持时报错，调用方降级）。
    pub async fn truncate(&self, path: &str, len: u64) -> Result<(), SftpError> {
        let attrs = FileAttributes {
            size: Some(len),
            ..FileAttributes::default()
        };
        let status = self
            .raw
            .setstat(path, attrs)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    /// 改修改时间（"保留属性"里的保留时间戳）。
    pub async fn set_mtime(&self, path: &str, mtime: u32) -> Result<(), SftpError> {
        self.guard_name(path).await?;
        let attrs = FileAttributes {
            mtime: Some(mtime),
            ..FileAttributes::default()
        };
        let status = self
            .raw
            .setstat(path, attrs)
            .await
            .map_err(|err| with_path(err, path))?;
        status_ok(status, path)
    }

    // ---------- 句柄级读写（传输引擎用） ----------

    /// 打开读句柄。
    pub async fn open_read(&self, path: &str) -> Result<String, SftpError> {
        self.guard_name(path).await?;
        self.raw
            .open(path, OpenFlags::READ, FileAttributes::default())
            .await
            .map(|handle| handle.handle)
            .map_err(|err| with_path(err, path))
    }

    /// 打开写句柄（截断）。
    pub async fn open_write_truncate(&self, path: &str) -> Result<String, SftpError> {
        self.guard_name(path).await?;
        self.raw
            .open(
                path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                FileAttributes::default(),
            )
            .await
            .map(|handle| handle.handle)
            .map_err(|err| with_path(err, path))
    }

    /// 打开写句柄，**不截断**（续传：从已有的长度接着写）。
    pub async fn open_write_resume(&self, path: &str) -> Result<String, SftpError> {
        self.guard_name(path).await?;
        self.raw
            .open(
                path,
                OpenFlags::WRITE | OpenFlags::CREATE,
                FileAttributes::default(),
            )
            .await
            .map(|handle| handle.handle)
            .map_err(|err| with_path(err, path))
    }

    /// 带偏移读一次（读到文件尾返回空 Vec）。
    pub async fn read_at(&self, handle: &str, offset: u64, len: u32) -> Result<Vec<u8>, SftpError> {
        match self.raw.read(handle, offset, len).await {
            Ok(data) => Ok(data.data),
            Err(russh_sftp::client::error::Error::Status(status))
                if status.status_code == StatusCode::Eof =>
            {
                Ok(Vec::new())
            }
            Err(err) => Err(SftpError::from(err)),
        }
    }

    /// 带偏移写一次。
    pub async fn write_at(
        &self,
        handle: &str,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<(), SftpError> {
        let status = self
            .raw
            .write(handle, offset, data)
            .await
            .map_err(SftpError::from)?;
        status_ok(status, "")
    }

    /// 服务器自己把一个句柄的内容拷到另一个句柄（`copy-data`，数据不经过本机）。
    ///
    /// `len == 0` 表示一直拷到文件尾。服务器没声明这个扩展时调用方不该走到这里
    /// （先看 [`SftpCaps::has`] `("copy-data")`）。
    pub async fn copy_data(
        &self,
        read_handle: &str,
        read_offset: u64,
        len: u64,
        write_handle: &str,
        write_offset: u64,
    ) -> Result<(), SftpError> {
        let reply = self
            .raw
            .extended(
                COPY_DATA,
                copy_data_payload(read_handle, read_offset, len, write_handle, write_offset),
            )
            .await
            .map_err(SftpError::from)?;
        match reply {
            Packet::Status(status) => status_ok(status, ""),
            Packet::ExtendedReply(_) => Ok(()),
            _ => Err(SftpError::Remote(
                "copy-data 收到了意料之外的回包".to_owned(),
            )),
        }
    }

    /// 刷盘（`fsync@openssh.com`；没有这个扩展就跳过，返回 false）。
    pub async fn fsync(&self, handle: &str) -> Result<bool, SftpError> {
        if !self.caps.has(extensions::FSYNC) {
            return Ok(false);
        }
        let status = self.raw.fsync(handle).await.map_err(SftpError::from)?;
        status_ok(status, "").map(|()| true)
    }

    /// 关闭句柄。
    pub async fn close_handle(&self, handle: &str) -> Result<(), SftpError> {
        let status = self.raw.close(handle).await.map_err(SftpError::from)?;
        status_ok(status, "")
    }
}

/// `SftpChannel` 上的三步改名操作（普通 rename 目标存在会失败，正是三步需要的语义）。
struct ChannelOps<'a>(&'a SftpChannel);

impl crate::sftp::replace::RenameOps for ChannelOps<'_> {
    fn exists<'a>(&'a self, path: &'a str) -> crate::ssh::BoxFuture<'a, Result<bool, SftpError>> {
        Box::pin(async move {
            match self.0.raw.lstat(path).await {
                Ok(_) => Ok(true),
                Err(err) => match SftpError::from(err) {
                    SftpError::NotFound { .. } => Ok(false),
                    other => Err(other),
                },
            }
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> crate::ssh::BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let status = self
                .0
                .raw
                .rename(from, to)
                .await
                .map_err(|err| with_path(err, from))?;
            status_ok(status, from)
        })
    }

    fn remove<'a>(&'a self, path: &'a str) -> crate::ssh::BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.0.remove_file(path).await })
    }
}

/// 重命名走了哪条路（报告和测试要看"降级有没有真的发生"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    /// 服务器支持 `posix-rename@openssh.com`，直接覆盖。
    PosixRename,
    /// 不支持扩展：原文件让开、新文件到位、删备用文件（三步）。
    RemoveThenRename,
    /// 普通 `rename`（目标不存在）。
    Plain,
}

/// 重命名走哪条路（纯决策，便于单测"服务器没有扩展时怎么降级"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamePlan {
    /// 服务器支持 `posix-rename@openssh.com`（目标存在也能覆盖）。
    PosixRename,
    /// 不支持扩展：先删目标再改名。
    RemoveThenRename,
    /// 普通 `rename`（不覆盖）。
    Plain,
}

/// 按服务器能力决定重命名怎么走。
pub fn rename_plan(caps: &SftpCaps, overwrite: bool) -> RenamePlan {
    if !overwrite {
        // 不覆盖必须走普通 SSH_FXP_RENAME：OpenSSH 的 sftp-server 在目标存在时返回失败。
        // `posix-rename` 是 POSIX rename(2) 语义，目标存在会**直接替换**（2026-10 在测试服务器上实测）。
        return RenamePlan::Plain;
    }
    if caps.has(POSIX_RENAME) {
        RenamePlan::PosixRename
    } else {
        RenamePlan::RemoveThenRename
    }
}

/// `posix-rename@openssh.com` 的载荷：`string oldpath` + `string newpath`（两个 SSH string）。
pub fn posix_rename_payload(from: &str, to: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + from.len() + to.len());
    for part in [from, to] {
        let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// 服务器端复制的扩展名（SFTP 草案 `copy-data`，OpenSSH 9.0 起的 sftp-server 支持）。
pub const COPY_DATA: &str = "copy-data";

/// `copy-data` 的载荷：`string read-from-handle`、`uint64 read-from-offset`、
/// `uint64 read-data-length`、`string write-to-handle`、`uint64 write-to-offset`。
pub fn copy_data_payload(
    read_handle: &str,
    read_offset: u64,
    len: u64,
    write_handle: &str,
    write_offset: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        &u32::try_from(read_handle.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(read_handle.as_bytes());
    out.extend_from_slice(&read_offset.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(write_handle.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(write_handle.as_bytes());
    out.extend_from_slice(&write_offset.to_be_bytes());
    out
}

/// `Status` 是不是 OK。
fn status_ok(status: Status, path: &str) -> Result<(), SftpError> {
    match status.status_code {
        StatusCode::Ok => Ok(()),
        code => Err(from_status_code(code, path, &status.error_message)),
    }
}

fn first_name(name: Name) -> Option<String> {
    name.files.into_iter().next().map(|file| file.filename)
}

/// 通道空闲多久回收（专用通道在传输任务结束后关掉；浏览通道跟连接同寿命）。
pub const IDLE_CHANNEL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with(extensions: &[(&str, &str)], limits: Option<SftpLimits>) -> SftpCaps {
        let map: HashMap<String, String> = extensions
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let mut caps = SftpCaps::from_extensions(3, &map);
        caps.limits = limits;
        caps
    }

    #[test]
    fn caps_read_extensions_and_degrade_without_limits() {
        let caps = caps_with(&[], None);
        assert_eq!(caps.version, 3);
        assert!(!caps.has(extensions::LIMITS));
        // 没有 limits：保守值。
        assert_eq!(caps.max_read_len(), CONSERVATIVE_CHUNK);
        assert_eq!(caps.max_write_len(), CONSERVATIVE_CHUNK);
        assert_eq!(caps.max_open_handles(), 4);
        assert!(caps.describe().contains("保守值"));
    }

    #[test]
    fn declared_limits_are_used_and_capped() {
        let caps = caps_with(
            &[
                (extensions::LIMITS, "1"),
                (extensions::FSYNC, "1"),
                (POSIX_RENAME, "1"),
            ],
            Some(SftpLimits {
                max_packet_len: 262_144,
                max_read_len: 65_536,
                max_write_len: 4 * 1024 * 1024, // 服务器说能发 4MB，我们自己封顶
                max_open_handles: 64,
            }),
        );
        assert_eq!(caps.max_read_len(), 65_536);
        assert_eq!(caps.max_write_len(), MAX_CHUNK);
        assert_eq!(caps.max_open_handles(), 64);
        assert!(caps.has(extensions::FSYNC));
        assert!(caps.has(POSIX_RENAME));
        let text = caps.describe();
        assert!(text.contains("fsync"), "{text}");
        assert!(text.contains("posix-rename"), "{text}");
    }

    #[test]
    fn rename_falls_back_when_the_extension_is_missing() {
        // 服务器支持扩展：覆盖时走 posix-rename；**不覆盖时必须走普通 rename**
        // （posix-rename 是 POSIX rename(2) 语义，目标存在会直接替换）。
        let with_extension = caps_with(&[(POSIX_RENAME, "1")], None);
        assert_eq!(rename_plan(&with_extension, true), RenamePlan::PosixRename);
        assert_eq!(rename_plan(&with_extension, false), RenamePlan::Plain);

        // 没有扩展：不覆盖时用普通 rename，覆盖时先删后改名（这条只能单测：
        // 测试服务器有扩展，实测跑不到降级路径）。
        let without = caps_with(&[], None);
        assert_eq!(rename_plan(&without, false), RenamePlan::Plain);
        assert_eq!(rename_plan(&without, true), RenamePlan::RemoveThenRename);
    }

    #[test]
    fn copy_data_payload_layout_follows_the_draft() {
        let payload = copy_data_payload("RH", 5, 0, "WHANDLE", 9);
        let mut expected = Vec::new();
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(b"RH");
        expected.extend_from_slice(&5u64.to_be_bytes());
        expected.extend_from_slice(&0u64.to_be_bytes());
        expected.extend_from_slice(&7u32.to_be_bytes());
        expected.extend_from_slice(b"WHANDLE");
        expected.extend_from_slice(&9u64.to_be_bytes());
        assert_eq!(payload, expected);
    }

    #[test]
    fn posix_rename_payload_is_two_ssh_strings() {
        let payload = posix_rename_payload("a.txt", "b c.txt");
        // 长度前缀 + 内容：5 + "a.txt" + 7 + "b c.txt"
        assert_eq!(&payload[0..4], &5u32.to_be_bytes());
        assert_eq!(&payload[4..9], b"a.txt");
        assert_eq!(&payload[9..13], &7u32.to_be_bytes());
        assert_eq!(&payload[13..], b"b c.txt");
    }
}
