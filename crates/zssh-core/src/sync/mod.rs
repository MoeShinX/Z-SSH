//! 配置同步（S-1）：同步包、加密、三方合并、防误删、本地备份和存储接口。
//!
//! 这一层不碰任何云存储，也不做界面：导出/导入的都是一个加密文件
//! （[`crypto::seal`] / [`crypto::open`]），存储由调用方通过
//! [`storage::SyncStorage`] 提供。数据全部来自本地 [`Database`](crate::Database)
//! 和一个 known_hosts 文件。

pub mod app_proxy;
pub mod apply;
pub mod backup;
pub mod conflicts;
pub mod crypto;
pub mod engine;
pub mod error;
pub mod github;
pub mod guard;
pub mod known_hosts;
pub mod local_extras;
pub mod merge;
pub mod net;
pub mod offline;
pub mod onedrive;
pub mod package;
pub mod remote;
pub mod s3;
pub mod sealer;
pub mod service;
pub mod settings;
pub mod sigv4;
pub mod snapshot;
pub mod storage;
pub mod webdav;

pub use error::SyncError;
pub use package::{
    GroupRecord, HostRecord, IdentityRecord, KnownHostRecord, ProxyRecord, SyncSnapshot, Tombstone,
};
