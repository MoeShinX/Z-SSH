//! SFTP 的错误类型（M3-1 第 2 节：错误要分清）。
//!
//! 目的：把底层（SSH / SFTP 协议 / 本地文件系统）的各种失败**分成能分别处理的类别**，
//! 上层（队列、界面）才能给出"没权限""不存在""已存在""磁盘满""连接断了"这种
//! 明确说法，而不是一句"失败"。

use std::io;

use russh_sftp::client::error::Error as SftpClientError;
use russh_sftp::protocol::StatusCode;

use crate::error::ZsshError;

/// 覆盖改名失败之后，服务器上是什么状态（决定调用方能不能删临时文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceState {
    /// 原文件没动（改名没发生，或者已经原样恢复）。
    Untouched,
    /// 原文件已经挪到了备用名 `backup`，而且没能改回来；新内容还在原来的临时文件里。
    /// 两份都完整，**谁也不能删**。
    OriginalAside { backup: String },
    /// 不确定：连接在改名的过程中断了，改名可能发生了也可能没发生。
    Uncertain,
}

/// SFTP 层的错误。
#[derive(Debug, thiserror::Error)]
pub enum SftpError {
    /// 覆盖改名（服务器没有 `posix-rename`，走"备用名"三步）失败，`state` 说明现在服务器上的状态。
    #[error("{message}")]
    ReplaceFailed {
        state: ReplaceState,
        message: String,
    },

    /// 服务器没开 SFTP（精简系统、路由器，或者服务端关掉了 `Subsystem sftp`）。
    #[error(
        "这台服务器没有开启 SFTP：子系统请求被拒。请确认服务器装了 sftp 子系统（OpenSSH 的 /usr/lib/openssh/sftp-server，配置里 `Subsystem sftp` 没有被注释掉；通道在握手时断开的 话也可能是网络断了——{0}）"
    )]
    NoSubsystem(String),

    /// 服务器拒绝了新通道：多半是碰到 `MaxSessions` 上限。上层据此改走专用连接。
    #[error("服务器拒绝了新的通道（可能已达 MaxSessions 上限）：{0}")]
    ChannelLimit(String),

    #[error("没有权限：{path}")]
    PermissionDenied { path: String },

    #[error("文件不存在：{path}")]
    NotFound { path: String },

    #[error("已经存在：{path}")]
    AlreadyExists { path: String },

    #[error("远端磁盘空间不足：{path}")]
    RemoteNoSpace { path: String },

    #[error("本地磁盘空间不足：{path}")]
    LocalNoSpace { path: String },

    #[error("连接断了：{0}")]
    Disconnected(String),

    /// 名字里带着不是 UTF-8 的字节：我们拿不到原始字节，**不能**拿有损解出来的名字去操作
    /// （那会把服务器上的名字改坏）。列表里能显示，操作时给这个错误。
    #[error("这个文件名不是 UTF-8（{name}），当前实现不能用原始字节操作它")]
    NonUtf8Name { name: String },

    /// 文件超过小文本操作的体积上限（不截断，明确报错）。
    #[error("{path} 有 {size} 字节，超过 {limit} 字节的上限")]
    TooLarge { path: String, size: u64, limit: u64 },

    #[error("远端返回了不认识的错误码（{code}）：{message}")]
    RemoteStatus { code: i32, message: String },

    #[error("远端错误：{0}")]
    Remote(String),

    #[error("本地文件操作失败（{path}）：{message}")]
    Local { path: String, message: String },

    #[error("另一个传输任务正在写 {path}，等它结束后再试")]
    TargetBusy { path: String },

    #[error("操作被取消")]
    Cancelled,

    #[error("输入不合法：{0}")]
    InvalidInput(String),

    #[error("内部错误：{0}")]
    Internal(String),
}

impl SftpError {
    /// 这是不是"连接断了"这一类：队列据此进入"等待重连"而不是直接失败。
    pub fn is_disconnect(&self) -> bool {
        match self {
            SftpError::Disconnected(_) => true,
            SftpError::Local { message, .. } => is_disconnect_text(message),
            SftpError::Remote(message) => is_disconnect_text(message),
            _ => false,
        }
    }

    /// 这个错误重试有没有意义（连接类、临时性错误可以重试；权限/不存在不行）。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            SftpError::Disconnected(_) | SftpError::ChannelLimit(_)
        )
    }

    /// 权限不足（界面可以据此提示"用 `sudo` 或者换个目录"）。
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, SftpError::PermissionDenied { .. })
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            SftpError::PermissionDenied { path }
            | SftpError::NotFound { path }
            | SftpError::AlreadyExists { path }
            | SftpError::RemoteNoSpace { path }
            | SftpError::LocalNoSpace { path }
            | SftpError::Local { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// 文本里像不像"连接断了"。SSH/SFTP 断线在不同层有不同说法，这里统一认出来。
fn is_disconnect_text(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    [
        "connection lost",
        "connection reset",
        "connection closed",
        "connection aborted",
        // Windows 的 TCP 复位文案（os error 10054）：实测掐断连接时就是它。
        "forcibly closed",
        "broken pipe",
        "not connected",
        "session closed",
        "no connection",
        "channel closed",
        "unexpected eof",
        "transport",
        // 连接死了之后，russh-sftp 里等着应答的那一半会收到这个：2026-10-17 实测，
        // 掐断链路时它就是"连接断了"的唯一信号（认不出来任务会直接失败，不会重连）。
        "sender dropped",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// SFTP 状态码 → 我们的错误类别。
pub fn from_status_code(code: StatusCode, path: &str, message: &str) -> SftpError {
    match code {
        StatusCode::NoSuchFile => SftpError::NotFound {
            path: path.to_owned(),
        },
        StatusCode::PermissionDenied => SftpError::PermissionDenied {
            path: path.to_owned(),
        },
        StatusCode::Failure if looks_like_no_space(message) => SftpError::RemoteNoSpace {
            path: path.to_owned(),
        },
        // 服务器用 Failure 表示"名字已存在"是常见行为（SFTP v3 没有专门的码）。
        StatusCode::Failure if looks_like_exists(message) => SftpError::AlreadyExists {
            path: path.to_owned(),
        },
        StatusCode::NoConnection | StatusCode::ConnectionLost => {
            SftpError::Disconnected(message.to_owned())
        }
        other => SftpError::RemoteStatus {
            code: status_code_number(other),
            message: if message.is_empty() {
                other.to_string()
            } else {
                message.to_owned()
            },
        },
    }
}

/// 错误文本里像不像"磁盘满"（不同服务器的说法不一样）。
pub fn looks_like_no_space(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("no space")
        || lowered.contains("disk full")
        || lowered.contains("quota exceeded")
        || lowered.contains("insufficient space")
}

/// 错误文本里像不像"已经存在"。
pub fn looks_like_exists(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("already exists") || lowered.contains("file exists")
}

fn status_code_number(code: StatusCode) -> i32 {
    code as i32
}

impl From<SftpClientError> for SftpError {
    fn from(err: SftpClientError) -> Self {
        match err {
            SftpClientError::Status(status) => {
                from_status_code(status.status_code, "", &status.error_message)
            }
            SftpClientError::IO(message) => {
                if is_disconnect_text(&message) {
                    SftpError::Disconnected(message)
                } else {
                    SftpError::Remote(message)
                }
            }
            SftpClientError::Timeout => SftpError::Remote("SFTP 请求超时".to_owned()),
            SftpClientError::Limited(message) => {
                SftpError::Remote(format!("超过服务器限制：{message}"))
            }
            SftpClientError::UnexpectedPacket => {
                SftpError::Remote("SFTP 协议出错（收到了意料之外的回包）".to_owned())
            }
            SftpClientError::UnexpectedBehavior(message) => {
                if is_disconnect_text(&message) {
                    SftpError::Disconnected(message)
                } else {
                    SftpError::Remote(message)
                }
            }
        }
    }
}

/// 带上路径再映射（协议层的错误里没有路径信息，调用方知道路径）。
pub fn with_path(err: SftpClientError, path: &str) -> SftpError {
    match err {
        SftpClientError::Status(status) => {
            from_status_code(status.status_code, path, &status.error_message)
        }
        other => SftpError::from(other),
    }
}

impl From<ZsshError> for SftpError {
    fn from(err: ZsshError) -> Self {
        match err {
            ZsshError::Ssh(inner) => SftpError::Disconnected(inner.to_string()),
            // 通道数到上限要单独一类：传输层靠它改走专用连接（M3-2 第 0 节实测
            // 发现漏了这条映射——以前会被包成 Remote，兜底永远走不到）。
            ZsshError::ChannelLimit(message) => SftpError::ChannelLimit(message),
            other => {
                let text = other.to_string();
                if is_disconnect_text(&text) {
                    SftpError::Disconnected(text)
                } else {
                    SftpError::Remote(text)
                }
            }
        }
    }
}

/// 本地文件系统的错误（按路径归类）。
pub fn from_io(err: io::Error, path: &str) -> SftpError {
    match err.kind() {
        io::ErrorKind::NotFound => SftpError::NotFound {
            path: path.to_owned(),
        },
        io::ErrorKind::PermissionDenied => SftpError::PermissionDenied {
            path: path.to_owned(),
        },
        io::ErrorKind::AlreadyExists => SftpError::AlreadyExists {
            path: path.to_owned(),
        },
        _ if is_no_space_io(&err) => SftpError::LocalNoSpace {
            path: path.to_owned(),
        },
        _ => SftpError::Local {
            path: path.to_owned(),
            message: err.to_string(),
        },
    }
}

/// `ERROR_DISK_FULL`（Windows）/ `ENOSPC`（其它）都归到"磁盘满"。
fn is_no_space_io(err: &io::Error) -> bool {
    // Rust 的 `ErrorKind::StorageFull` 还是 unstable，只能看 raw OS error：
    // Windows 的 ERROR_DISK_FULL = 112、ERROR_HANDLE_DISK_FULL = 39；POSIX 的 ENOSPC = 28。
    #[cfg(windows)]
    const NO_SPACE: &[i32] = &[112, 39];
    #[cfg(not(windows))]
    const NO_SPACE: &[i32] = &[28, 122];
    match err.raw_os_error() {
        Some(code) => NO_SPACE.contains(&code),
        None => false,
    }
}

/// 加密/编码探测用：这段字节是不是合法的 UTF-8（文件名能不能安全地用字符串操作）。
pub fn name_is_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_map_to_specific_errors() {
        let denied = from_status_code(StatusCode::PermissionDenied, "/etc/shadow", "");
        assert!(denied.is_permission_denied());
        assert_eq!(denied.path(), Some("/etc/shadow"));
        assert!(denied.to_string().contains("没有权限"));

        let missing = from_status_code(StatusCode::NoSuchFile, "/tmp/nope", "");
        assert!(matches!(missing, SftpError::NotFound { .. }));

        // v3 没有"已存在"，服务器用 Failure + 文本表示。
        let exists = from_status_code(StatusCode::Failure, "/tmp/a", "File already exists");
        assert!(matches!(exists, SftpError::AlreadyExists { .. }));

        let full = from_status_code(StatusCode::Failure, "/tmp/a", "No space left on device");
        assert!(matches!(full, SftpError::RemoteNoSpace { .. }));

        // 认不出来的 Failure 也要带上原文，不能吞掉。
        let other = from_status_code(StatusCode::Failure, "/tmp/a", "something odd");
        assert!(other.to_string().contains("something odd"));
    }

    #[test]
    fn disconnect_and_retry_classification() {
        let lost = SftpError::from(SftpClientError::IO("Connection lost".to_owned()));
        assert!(lost.is_disconnect(), "{lost}");
        assert!(lost.is_retryable(), "断线要能重试");
        assert!(
            !SftpError::NotFound { path: "/x".into() }.is_retryable(),
            "不存在不该重试"
        );
        assert!(!SftpError::PermissionDenied { path: "/x".into() }.is_retryable());
    }

    /// 真实链路上"连接断了"的几种说法都要认出来（2026-10-17 实测补的两条）。
    #[test]
    fn disconnect_signatures_from_real_links() {
        // 掐断链路时 russh-sftp 的等待方拿到的就是这个。
        let dropped = SftpError::from(SftpClientError::UnexpectedBehavior(
            "sender dropped".to_owned(),
        ));
        assert!(dropped.is_disconnect(), "{dropped}");
        assert!(dropped.is_retryable());

        // Windows 的 TCP 复位文案（os error 10054）。
        let reset = SftpError::from(SftpClientError::IO(
            "An existing connection was forcibly closed by the remote host. (os error 10054)"
                .to_owned(),
        ));
        assert!(reset.is_disconnect());
    }

    /// 通道上限的 SSH 错误必须映射成 SFTP 的 `ChannelLimit`：传输层靠这一类改走
    /// 专用连接（M3-2 第 0 节实测时发现这条映射漏过，兜底一直走不到）。
    #[test]
    fn ssh_channel_limit_maps_to_sftp_channel_limit() {
        let err = SftpError::from(ZsshError::ChannelLimit("ConnectFailed".to_owned()));
        assert!(matches!(err, SftpError::ChannelLimit(_)), "{err}");
        assert!(err.is_retryable(), "通道上限要能触发专用连接兜底");
    }

    #[test]
    fn io_errors_map_by_kind_and_os_code() {
        let missing = from_io(io::Error::new(io::ErrorKind::NotFound, "nope"), "C:\\a.txt");
        assert!(matches!(missing, SftpError::NotFound { .. }));

        // 磁盘满：Windows 上是 ERROR_DISK_FULL(112)，Linux 上 ENOSPC(28)。
        #[cfg(windows)]
        let full = from_io(io::Error::from_raw_os_error(112), "C:\\a.txt");
        #[cfg(not(windows))]
        let full = from_io(io::Error::from_raw_os_error(28), "C:\\a.txt");
        assert!(matches!(full, SftpError::LocalNoSpace { .. }));

        let other = from_io(io::Error::other("boom"), "C:\\a.txt");
        assert!(other.to_string().contains("boom"));
    }

    #[test]
    fn non_utf8_names_are_detected() {
        assert!(name_is_utf8(b"report.txt"));
        // GBK 的"报"= 0xB1 0xA8
        assert!(!name_is_utf8(&[0xB1, 0xA8]));
    }
}
