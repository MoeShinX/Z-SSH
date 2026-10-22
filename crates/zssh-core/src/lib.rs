//! Z-SSH 的核心逻辑：纯 Rust，不依赖 Tauri，可以单独测试。
//!
//! M1 阶段包含 SSH 连接、shell 通道、本地存储和相关模型。界面层（Tauri 命令、
//! React）只负责转接，不放业务逻辑。

pub mod ai;
pub mod error;
pub mod external_url;
pub mod model;
pub mod pathnorm;
pub mod sftp;
pub mod ssh;
pub mod store;
pub mod sync;
#[cfg(test)]
pub(crate) mod test_util;
pub mod updater;

pub use error::ZsshError;
pub use model::{
    parse_quick_target, AuthMethod, Group, GroupDraft, Host, HostDraft, Identity, IdentityDraft,
    IdentityKind, Proxy, ProxyDraft, ProxyKind, ProxyMode, QuickTarget,
};
pub use store::{Database, HostLogin, ProxyLogin};
