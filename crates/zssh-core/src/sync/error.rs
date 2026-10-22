//! 同步相关的错误。
//!
//! 主密码错误、文件被改过、格式版本太新/太旧要能分开告诉用户：界面按这些
//! 变体给出不同提示（S-2）。错误文本里**不能带密码、密钥或同步包明文**。

use crate::error::ZsshError;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("同步包格式不对：{0}")]
    Format(String),

    /// 别的电脑上是更新的 Z-SSH。**不能尝试解析，更不能覆盖**，提示用户升级。
    #[error("同步包的格式版本太新（v{found}，本软件只支持到 v{supported}），请先升级 Z-SSH")]
    FormatTooNew { found: u32, supported: u32 },

    #[error("同步包的格式版本太旧（v{found}），这个版本不再支持")]
    FormatTooOld { found: u32 },

    #[error("同步包用了不支持的密钥派生算法：{0}（可能是更新的 Z-SSH 写的）")]
    UnsupportedKdf(String),

    #[error("主密码错误（或文件头被改过）")]
    WrongPassword,

    #[error("同步包被改过（认证失败），没有解密")]
    Tampered,

    #[error("同步数据不合法：{0}")]
    InvalidData(String),

    /// 装箱：`ZsshError` 里有好几个大字段，直接放进来会让所有 `Result` 都变大。
    #[error("数据库错误：{0}")]
    Database(Box<ZsshError>),

    #[error("读写失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("存储出错：{0}")]
    Storage(String),

    /// 凭据被存储服务拒绝：重试没有用，要用户改配置（或手动同步）。
    #[error("存储出错：{0}")]
    AuthFailed(String),

    /// 存储服务限流（GitHub 的接口会这样）：要等一段时间，等到之前不要再试。
    #[error("{service} 限流，{}后重试", super::storage::wait_text(*retry_after_secs))]
    RateLimited {
        service: String,
        retry_after_secs: u64,
    },

    /// 合并和写入之间用户改了本地数据。本地没动、基线没存，下次同步会把这些
    /// 改动当成“相对旧基线的改动”正常合进去。
    #[error("同步期间本地有改动，稍后自动重新同步")]
    LocalChanged,

    #[error("还没有连接存储")]
    NotConfigured,

    #[error("内部错误：{0}")]
    Internal(String),
}

impl From<ZsshError> for SyncError {
    fn from(err: ZsshError) -> Self {
        SyncError::Database(Box::new(err))
    }
}

impl From<rusqlite::Error> for SyncError {
    fn from(err: rusqlite::Error) -> Self {
        SyncError::Database(Box::new(ZsshError::from(err)))
    }
}

impl From<super::storage::StorageError> for SyncError {
    fn from(err: super::storage::StorageError) -> Self {
        match err {
            super::storage::StorageError::RateLimited {
                service,
                retry_after_secs,
            } => SyncError::RateLimited {
                service,
                retry_after_secs,
            },
            super::storage::StorageError::AuthFailed(message) => SyncError::AuthFailed(message),
            other => SyncError::Storage(other.to_string()),
        }
    }
}
