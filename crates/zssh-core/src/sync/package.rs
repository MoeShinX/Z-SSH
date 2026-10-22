//! 同步包的明文结构。
//!
//! 明文是一个带 `schema_version` 的 JSON，加密后才写出去。引用关系（主机 →
//! 分组 / 凭据 / 代理 / 跳板机）全部用 `uid`；本机自增 id **不出现在这里**。
//!
//! 记录里带 `updated_at` 和 `device_id`：三方合并时用时间较新的那份，时间相同
//! 或相差很小时用设备 ID 做确定性的平局规则（[`crate::sync::merge`]）。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroize;

use super::error::SyncError;

/// 明文格式版本。包头的 `version` 是加密信封的版本，这个是明文数据结构的版本。
pub const SNAPSHOT_VERSION: u32 = 1;

/// 墓碑保留 90 天。
pub const TOMBSTONE_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;

pub const ENTITY_GROUP: &str = "group";
pub const ENTITY_HOST: &str = "host";
pub const ENTITY_IDENTITY: &str = "identity";
pub const ENTITY_PROXY: &str = "proxy";
pub const ENTITY_KNOWN_HOST: &str = "known_host";

/// 一个同步快照（云端的“当前版本”或本机的“上次同步基线”都是它）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub schema_version: u32,
    /// 导出这份快照的设备。
    pub device_id: String,
    /// 导出时间（Unix 秒）。
    pub created_at: i64,
    #[serde(default)]
    pub groups: Vec<GroupRecord>,
    #[serde(default)]
    pub hosts: Vec<HostRecord>,
    #[serde(default)]
    pub identities: Vec<IdentityRecord>,
    #[serde(default)]
    pub proxies: Vec<ProxyRecord>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub known_hosts: Vec<KnownHostRecord>,
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRecord {
    pub uid: String,
    pub name: String,
    #[serde(default)]
    pub parent_uid: Option<String>,
    #[serde(default)]
    pub sort: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostRecord {
    pub uid: String,
    #[serde(default)]
    pub group_uid: Option<String>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    #[serde(default)]
    pub identity_uid: Option<String>,
    #[serde(default)]
    pub proxy_uid: Option<String>,
    #[serde(default)]
    pub jump_host_uid: Option<String>,
    #[serde(default)]
    pub proxy_mode: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sort: i64,
    #[serde(default)]
    pub legacy_algorithms: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub uid: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub key_path: Option<String>,
    /// 明文密码 / 私钥 / 口令。**只在加密后的同步包和 DPAPI 基线里存在**。
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub device_id: String,
}

/// `Debug` 手写：`secret` 是明文秘密，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for IdentityRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityRecord")
            .field("uid", &self.uid)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("key_path", &self.key_path)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .field("updated_at", &self.updated_at)
            .field("device_id", &self.device_id)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyRecord {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub device_id: String,
}

/// `Debug` 手写：`secret` 是明文代理密码，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for ProxyRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRecord")
            .field("uid", &self.uid)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .field("updated_at", &self.updated_at)
            .field("device_id", &self.device_id)
            .finish()
    }
}

/// known_hosts 的一条记录。记录键是“主机 + 端口 + 密钥类型”，没有 uid。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownHostRecord {
    /// 主机名或 IP；哈希主机名原样存（`|1|salt|hash`），这时端口固定按 22 处理。
    pub host: String,
    pub port: u16,
    pub key_type: String,
    /// 公钥 base64（不含 `ssh-ed25519` 前缀）。
    pub key: String,
    /// 只有 `@revoked` 会写，普通记录是 `None`。
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub device_id: String,
}

/// 明文秘密在记录被丢掉时清零（合并、导入过程中会有很多临时副本）。
impl Drop for IdentityRecord {
    fn drop(&mut self) {
        if let Some(secret) = self.secret.as_mut() {
            secret.zeroize();
        }
    }
}

impl Drop for ProxyRecord {
    fn drop(&mut self) {
        if let Some(secret) = self.secret.as_mut() {
            secret.zeroize();
        }
    }
}

impl KnownHostRecord {
    /// 合并用的记录键：主机 + 端口 + 密钥类型。
    pub fn key(&self) -> String {
        known_host_key(&self.host, self.port, &self.key_type)
    }

    pub fn same_content(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.key_type == other.key_type
            && self.key == other.key
            && self.marker == other.marker
    }
}

pub fn known_host_key(host: &str, port: u16, key_type: &str) -> String {
    format!("{host}\u{1f}{port}\u{1f}{key_type}")
}

/// 删除事件。过期后不再参与合并，但也不会让记录复活（见合并说明）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    /// [`ENTITY_GROUP`] / [`ENTITY_HOST`] / [`ENTITY_IDENTITY`] / [`ENTITY_PROXY`] /
    /// [`ENTITY_KNOWN_HOST`]。
    pub entity: String,
    /// uid；known_hosts 是 [`known_host_key`] 的结果。
    pub key: String,
    pub deleted_at: i64,
    pub device_id: String,
}

impl Tombstone {
    pub fn is_expired(&self, now: i64) -> bool {
        now.saturating_sub(self.deleted_at) > TOMBSTONE_RETENTION_SECS
    }
}

impl GroupRecord {
    pub fn same_content(&self, other: &Self) -> bool {
        self.uid == other.uid
            && self.name == other.name
            && self.parent_uid == other.parent_uid
            && self.sort == other.sort
    }
}

impl HostRecord {
    pub fn same_content(&self, other: &Self) -> bool {
        self.uid == other.uid
            && self.group_uid == other.group_uid
            && self.name == other.name
            && self.host == other.host
            && self.port == other.port
            && self.username == other.username
            && self.auth_method == other.auth_method
            && self.identity_uid == other.identity_uid
            && self.proxy_uid == other.proxy_uid
            && self.jump_host_uid == other.jump_host_uid
            && self.proxy_mode == other.proxy_mode
            && self.tags == other.tags
            && self.sort == other.sort
            && self.legacy_algorithms == other.legacy_algorithms
            && self.created_at == other.created_at
    }
}

impl IdentityRecord {
    pub fn same_content(&self, other: &Self) -> bool {
        self.uid == other.uid
            && self.name == other.name
            && self.kind == other.kind
            && self.key_path == other.key_path
            && self.secret == other.secret
    }
}

impl ProxyRecord {
    pub fn same_content(&self, other: &Self) -> bool {
        self.uid == other.uid
            && self.name == other.name
            && self.kind == other.kind
            && self.host == other.host
            && self.port == other.port
            && self.username == other.username
            && self.secret == other.secret
    }
}

impl SyncSnapshot {
    pub fn empty(device_id: impl Into<String>, created_at: i64) -> Self {
        Self {
            schema_version: SNAPSHOT_VERSION,
            device_id: device_id.into(),
            created_at,
            groups: Vec::new(),
            hosts: Vec::new(),
            identities: Vec::new(),
            proxies: Vec::new(),
            settings: BTreeMap::new(),
            known_hosts: Vec::new(),
            tombstones: Vec::new(),
        }
    }

    /// 解析明文并检查数据格式版本。
    pub fn parse(bytes: &[u8]) -> Result<Self, SyncError> {
        let snapshot: Self = serde_json::from_slice(bytes)
            .map_err(|err| SyncError::Format(format!("明文 JSON 解析失败：{err}")))?;
        if snapshot.schema_version > SNAPSHOT_VERSION {
            return Err(SyncError::FormatTooNew {
                found: snapshot.schema_version,
                supported: SNAPSHOT_VERSION,
            });
        }
        if snapshot.schema_version < SNAPSHOT_VERSION {
            return Err(SyncError::FormatTooOld {
                found: snapshot.schema_version,
            });
        }
        Ok(snapshot)
    }

    pub fn to_json(&self) -> Result<Vec<u8>, SyncError> {
        serde_json::to_vec(self)
            .map_err(|err| SyncError::Internal(format!("同步包序列化失败：{err}")))
    }

    /// 排序成规范顺序，保证同样的数据序列化出来一样（平局规则、测试都靠它）。
    pub fn sort_canonical(&mut self) {
        self.groups.sort_by(|a, b| a.uid.cmp(&b.uid));
        self.hosts.sort_by(|a, b| a.uid.cmp(&b.uid));
        self.identities.sort_by(|a, b| a.uid.cmp(&b.uid));
        self.proxies.sort_by(|a, b| a.uid.cmp(&b.uid));
        self.known_hosts.sort_by_key(|record| record.key());
        self.tombstones.sort_by(|a, b| {
            (a.entity.as_str(), a.key.as_str()).cmp(&(b.entity.as_str(), b.key.as_str()))
        });
    }

    /// 只比较业务数据（忽略 `device_id`、`created_at` 这些每台电脑都会变的元数据）。
    pub fn same_data(&self, other: &Self) -> bool {
        self.groups == other.groups
            && self.hosts == other.hosts
            && self.identities == other.identities
            && self.proxies == other.proxies
            && self.settings == other.settings
            && self.known_hosts == other.known_hosts
            && self.tombstones == other.tombstones
    }

    /// 记录总数（防误删的阈值用）。
    pub fn record_count(&self) -> usize {
        self.groups.len() + self.hosts.len() + self.identities.len() + self.proxies.len()
    }

    /// 序列化后用主密码加密成同步包（明文立即清零）。
    pub fn seal(&self, password: &str) -> Result<Vec<u8>, SyncError> {
        let plaintext = zeroize::Zeroizing::new(self.to_json()?);
        super::crypto::seal(&plaintext, password)
    }

    /// 解开同步包并解析明文。版本太新在 [`super::crypto::open`] 就拒绝了。
    pub fn open(bytes: &[u8], password: &str) -> Result<Self, SyncError> {
        let plaintext = super::crypto::open(bytes, password)?;
        Self::parse(&plaintext)
    }

    /// 某一类记录的所有 uid（known_hosts 用记录键）。
    pub fn keys_of(&self, entity: &str) -> BTreeSet<String> {
        match entity {
            ENTITY_GROUP => self.groups.iter().map(|r| r.uid.clone()).collect(),
            ENTITY_HOST => self.hosts.iter().map(|r| r.uid.clone()).collect(),
            ENTITY_IDENTITY => self.identities.iter().map(|r| r.uid.clone()).collect(),
            ENTITY_PROXY => self.proxies.iter().map(|r| r.uid.clone()).collect(),
            ENTITY_KNOWN_HOST => self.known_hosts.iter().map(KnownHostRecord::key).collect(),
            _ => BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod tests;
