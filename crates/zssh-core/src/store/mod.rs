//! 存储层：SQLite、DPAPI 加解密，以及各实体的数据访问。

pub mod audit;
pub mod db;
pub mod forwards;
pub mod groups;
pub mod host_memory;
pub mod host_os;
pub mod hosts;
pub mod identities;
pub mod proxies;
pub mod secrets;
pub mod settings;

pub use db::Database;
pub use hosts::HostLogin;
pub use proxies::ProxyLogin;

use crate::error::ZsshError;

/// 一条记录的全局标识。同步包里的引用只看它，本机自增 id 不出去。
pub(crate) fn new_uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 把数据库里存的枚举字符串转成枚举。值不对说明库被外部改过或迁移有问题。
pub(crate) fn parse_column<T>(
    raw: &str,
    parse: impl Fn(&str) -> Result<T, ZsshError>,
) -> rusqlite::Result<T> {
    parse(raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

#[cfg(test)]
mod tests;
