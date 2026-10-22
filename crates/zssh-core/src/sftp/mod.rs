//! SFTP（M3-1）：会话、文件操作、传输引擎。**只做后端**，界面在 M3-2 / M3-3。
//!
//! - [`session`]：在连接池已有的连接上开 `sftp` 子系统通道（不重新认证），
//!   协商出来的能力（扩展、`limits@openssh.com`）都在 [`session::SftpCaps`] 里。
//! - [`fs`]：文件操作（列目录、属性、建目录、重命名、删除、权限、小文本读写）。
//! - [`path`]：远端 POSIX 路径的拼接与规范化。
//! - [`local`]：Windows 本地这一侧的兼容（非法名字、长路径、大小写冲突、联接）。
//! - [`transfer`]：传输引擎（队列、并发、流水线、进度、暂停续传、冲突、重试）。
//!
//! 错误类型见 [`error::SftpError`]：没权限 / 不存在 / 已存在 / 磁盘满 / 连接断了各有各的
//! 说法，不笼统地报"失败"。

pub mod edit;
pub mod error;
pub mod fs;
pub mod local;
pub mod path;
pub mod replace;
pub mod server_copy;
pub mod session;
pub mod size;
pub mod transfer;

pub use error::SftpError;
pub use fs::SMALL_TEXT_LIMIT;
pub use fs::{DeleteProgress, DirEntry, FileKind, RecursiveDelete, RemoteStat, SmallText};
pub use session::{RenameOutcome, SftpCaps, SftpChannel};
