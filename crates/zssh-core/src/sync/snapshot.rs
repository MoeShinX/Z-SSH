//! 从本地数据库读出一份同步快照。
//!
//! 引用关系在这里从本机自增 id 换成 `uid`：主机 → 分组 / 凭据 / 代理 / 跳板机，
//! 分组 → 父分组，默认代理设置 → 代理 uid。凭据和代理密码在这里用 DPAPI 解开，
//! 明文只进加密后的同步包 / 本地基线，不会落到明文的磁盘文件里。

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;

use super::error::SyncError;
use super::known_hosts;
use super::package::{
    GroupRecord, HostRecord, IdentityRecord, ProxyRecord, SyncSnapshot, Tombstone, ENTITY_GROUP,
    ENTITY_HOST, ENTITY_IDENTITY, ENTITY_KNOWN_HOST, ENTITY_PROXY,
};
use super::sealer::SecretSealer;
use super::settings::SettingsPolicy;
use crate::store::db::Database;
use crate::store::settings::DEFAULT_PROXY_KEY;

/// 读本机当前数据。`known_hosts_path` 是自己那份 known_hosts 的路径。
pub fn snapshot_local(
    db: &Database,
    sealer: &dyn SecretSealer,
    device_id: &str,
    known_hosts_path: Option<&Path>,
    now: i64,
) -> Result<SyncSnapshot, SyncError> {
    snapshot_local_with(
        db,
        sealer,
        device_id,
        known_hosts_path,
        now,
        &SettingsPolicy::CURRENT,
    )
}

/// 同 [`snapshot_local`]，设置按给定的规则导出（测试用它模拟别的版本）。
pub fn snapshot_local_with(
    db: &Database,
    sealer: &dyn SecretSealer,
    device_id: &str,
    known_hosts_path: Option<&Path>,
    now: i64,
    policy: &SettingsPolicy,
) -> Result<SyncSnapshot, SyncError> {
    let snapshot = db.with_conn_as(|conn| {
        snapshot_from_conn(conn, sealer, device_id, known_hosts_path, now, policy)
    })?;
    Ok(snapshot)
}

/// 在已有连接（或事务）里读本机当前数据。`apply_snapshot` 在写入前用它在同一个
/// 事务里重读一遍，和合并时用的那份比较。
pub(crate) fn snapshot_from_conn(
    conn: &Connection,
    sealer: &dyn SecretSealer,
    device_id: &str,
    known_hosts_path: Option<&Path>,
    now: i64,
    policy: &SettingsPolicy,
) -> Result<SyncSnapshot, SyncError> {
    let mut snapshot = {
        let mut snapshot = SyncSnapshot::empty(device_id, now);

        let mut group_ids = BTreeMap::new();
        {
            let mut statement = conn.prepare(
                "SELECT id, uid, name, parent_id, sort, updated_at FROM groups ORDER BY id",
            )?;
            let query_rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            let mut rows = Vec::new();
            for row in query_rows {
                rows.push(row?);
            }
            // 父分组可能比子分组后建（id 更大），先把 id → uid 全收齐再解析引用。
            for (id, uid, ..) in &rows {
                group_ids.insert(*id, uid.clone());
            }
            for (_, uid, name, parent_id, sort, updated_at) in rows {
                snapshot.groups.push(GroupRecord {
                    uid,
                    name,
                    parent_uid: parent_id.and_then(|id| group_ids.get(&id).cloned()),
                    sort,
                    updated_at,
                    device_id: device_id.to_owned(),
                });
            }
        }

        let mut identity_ids = BTreeMap::new();
        {
            let mut statement = conn.prepare(
                "SELECT id, uid, name, kind, key_path, secret_blob, updated_at \
                 FROM identities ORDER BY id",
            )?;
            let query_rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?;
            let mut rows = Vec::new();
            for row in query_rows {
                rows.push(row?);
            }
            for (id, uid, name, kind, key_path, secret_blob, updated_at) in rows {
                let secret = decrypt_secret(sealer, secret_blob, "凭据", &name)
                    .map_err(|err| crate::error::ZsshError::Internal(err.to_string()))?;
                identity_ids.insert(id, uid.clone());
                snapshot.identities.push(IdentityRecord {
                    uid,
                    name,
                    kind,
                    key_path,
                    secret,
                    updated_at,
                    device_id: device_id.to_owned(),
                });
            }
        }

        let mut proxy_ids = BTreeMap::new();
        {
            let mut statement = conn.prepare(
                "SELECT id, uid, name, kind, host, port, username, secret_blob, updated_at \
                 FROM proxies ORDER BY id",
            )?;
            let query_rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u16>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })?;
            let mut rows = Vec::new();
            for row in query_rows {
                rows.push(row?);
            }
            for (id, uid, name, kind, host, port, username, secret_blob, updated_at) in rows {
                let secret = decrypt_secret(sealer, secret_blob, "代理", &name)
                    .map_err(|err| crate::error::ZsshError::Internal(err.to_string()))?;
                proxy_ids.insert(id, uid.clone());
                snapshot.proxies.push(ProxyRecord {
                    uid,
                    name,
                    kind,
                    host,
                    port,
                    username,
                    secret,
                    updated_at,
                    device_id: device_id.to_owned(),
                });
            }
        }

        let mut host_ids = BTreeMap::new();
        {
            let mut statement = conn.prepare(
                "SELECT id, uid, group_id, name, host, port, username, auth_method, identity_id, \
                 proxy_id, jump_host_id, proxy_mode, tags, sort, legacy_algorithms, created_at, \
                 updated_at FROM hosts ORDER BY id",
            )?;
            let query_rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u16>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                ))
            })?;
            let mut rows = Vec::new();
            for row in query_rows {
                rows.push(row?);
            }
            for (id, uid, ..) in &rows {
                host_ids.insert(*id, uid.clone());
            }
            for (
                _,
                uid,
                group_id,
                name,
                host,
                port,
                username,
                auth_method,
                identity_id,
                proxy_id,
                jump_host_id,
                proxy_mode,
                tags,
                sort,
                legacy_algorithms,
                created_at,
                updated_at,
            ) in rows
            {
                snapshot.hosts.push(HostRecord {
                    uid,
                    group_uid: group_id.and_then(|id| group_ids.get(&id).cloned()),
                    name,
                    host,
                    port,
                    username,
                    auth_method,
                    identity_uid: identity_id.and_then(|id| identity_ids.get(&id).cloned()),
                    proxy_uid: proxy_id.and_then(|id| proxy_ids.get(&id).cloned()),
                    jump_host_uid: jump_host_id.and_then(|id| host_ids.get(&id).cloned()),
                    proxy_mode,
                    tags: tags
                        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
                        .unwrap_or_default(),
                    sort,
                    legacy_algorithms: legacy_algorithms != 0,
                    created_at,
                    updated_at,
                    device_id: device_id.to_owned(),
                });
            }
        }

        let mut statement = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (key, raw) = row?;
            if !policy.is_synced(&key) {
                continue;
            }
            if key == DEFAULT_PROXY_KEY {
                // 设置里的默认代理是本机 id；同步包里换成 uid。
                let proxy_id = serde_json::from_str::<Option<i64>>(&raw).unwrap_or(None);
                let value = match proxy_id.and_then(|id| proxy_ids.get(&id).cloned()) {
                    Some(uid) => Value::String(uid),
                    None => Value::Null,
                };
                snapshot.settings.insert(key, value);
                continue;
            }
            let value = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
            snapshot.settings.insert(key, value);
        }

        Ok::<_, crate::error::ZsshError>(snapshot)
    }?;

    if let Some(path) = known_hosts_path {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                snapshot.known_hosts = known_hosts::collect(&content, device_id, now);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(SyncError::Io(err));
            }
        }
    }

    snapshot.tombstones = load_tombstones_conn(conn, now)?;
    snapshot.sort_canonical();
    Ok(snapshot)
}

fn decrypt_secret(
    sealer: &dyn SecretSealer,
    blob: Option<Vec<u8>>,
    entity: &str,
    name: &str,
) -> Result<Option<String>, SyncError> {
    match blob {
        Some(blob) if !blob.is_empty() => {
            let plaintext = sealer.open(&blob).map_err(|err| {
                SyncError::InvalidData(format!(
                    "{entity}「{name}」的密文解不开，不能导出（不导出比丢密码好）：{err}"
                ))
            })?;
            let text = String::from_utf8(plaintext).map_err(|_| {
                SyncError::InvalidData(format!("{entity}「{name}」解出来的不是 UTF-8"))
            })?;
            Ok(Some(text))
        }
        _ => Ok(None),
    }
}

/// 读墓碑表，过期的丢掉（不在包里传播）。
pub fn load_tombstones(db: &Database, now: i64) -> Result<Vec<Tombstone>, SyncError> {
    db.with_conn_as(|conn| load_tombstones_conn(conn, now))
}

fn load_tombstones_conn(conn: &Connection, now: i64) -> Result<Vec<Tombstone>, SyncError> {
    {
        let mut statement = conn.prepare(
            "SELECT entity, key, deleted_at, device_id FROM sync_tombstones ORDER BY entity, key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Tombstone {
                entity: row.get(0)?,
                key: row.get(1)?,
                deleted_at: row.get(2)?,
                device_id: row.get(3)?,
            })
        })?;
        let mut tombstones = Vec::new();
        for row in rows {
            let tombstone = row?;
            if !tombstone.is_expired(now) {
                tombstones.push(tombstone);
            }
        }
        Ok(tombstones)
    }
}

/// 删除事件写进墓碑表（同一个 key 只更新到更晚的时间）。
pub(crate) fn record_tombstones(
    conn: &Connection,
    tombstones: &[Tombstone],
) -> Result<(), SyncError> {
    for tombstone in tombstones {
        conn.execute(
            "INSERT INTO sync_tombstones (entity, key, deleted_at, device_id) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(entity, key) DO UPDATE SET \
                 deleted_at = max(deleted_at, excluded.deleted_at), \
                 device_id = excluded.device_id",
            rusqlite::params![
                tombstone.entity,
                tombstone.key,
                tombstone.deleted_at,
                tombstone.device_id
            ],
        )?;
    }
    Ok(())
}

/// 实体名（日志和冲突列表用）。
pub fn entities() -> [&'static str; 5] {
    [
        ENTITY_GROUP,
        ENTITY_HOST,
        ENTITY_IDENTITY,
        ENTITY_PROXY,
        ENTITY_KNOWN_HOST,
    ]
}

#[cfg(test)]
mod tests;
