//! 把合并结果写回本地库：整次导入在一个 SQLite 事务里完成，中途失败全部回滚。
//!
//! 引用从 `uid` 换回本机自增 id；凭据和代理的明文用**本机**的 DPAPI 重新加密。
//! 删掉的记录从库里移除；被引用的记录删了，引用已经由合并阶段置空，这里不会
//! 因为外键失败。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{params, Connection};

use super::conflicts::{purge_expired, store_conflict};
use super::error::SyncError;
use super::merge::Conflict;
use super::package::{SyncSnapshot, ENTITY_GROUP, ENTITY_HOST, ENTITY_IDENTITY, ENTITY_PROXY};
use super::sealer::SecretSealer;
use super::settings::{KeyClass, SettingsPolicy};
use super::snapshot::{record_tombstones, snapshot_from_conn};
use crate::store::settings::DEFAULT_PROXY_KEY;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ChangeCounts {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ApplyReport {
    pub groups: ChangeCounts,
    pub hosts: ChangeCounts,
    pub identities: ChangeCounts,
    pub proxies: ChangeCounts,
    pub settings_written: usize,
    pub known_hosts_added: usize,
    pub conflicts_stored: usize,
}

fn uid_ids(conn: &Connection, table: &str) -> Result<BTreeMap<String, i64>, SyncError> {
    let mut statement = conn.prepare(&format!("SELECT uid, id FROM {table}"))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (uid, id) = row?;
        map.insert(uid, id);
    }
    Ok(map)
}

fn delete_missing(
    conn: &Connection,
    table: &str,
    wanted: &BTreeSet<&str>,
) -> Result<usize, SyncError> {
    let existing = uid_ids(conn, table)?;
    let mut deleted = 0usize;
    for (uid, id) in existing {
        if !wanted.contains(uid.as_str()) {
            conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn require_uid(entity: &str, uid: &str) -> Result<(), SyncError> {
    if uid.trim().is_empty() {
        return Err(SyncError::InvalidData(format!("{entity}记录缺少 uid")));
    }
    Ok(())
}

/// 合并时用的那份本地状态。写入前在同一个事务里重读本地，不一致就不写。
pub struct LocalGuard<'a> {
    pub expected: &'a SyncSnapshot,
    pub device_id: &'a str,
}

/// 把快照写回本地库（不检查合并期间本地有没有被改；导入测试用）。
pub fn apply_snapshot(
    db: &crate::store::Database,
    snapshot: &SyncSnapshot,
    sealer: &dyn SecretSealer,
    known_hosts_path: Option<&Path>,
    conflicts: &[Conflict],
    now: i64,
) -> Result<ApplyReport, SyncError> {
    apply_snapshot_guarded(db, snapshot, sealer, known_hosts_path, conflicts, now, None)
}

/// 把快照写回本地库。`conflicts` 是这次合并留下的另一份副本。
///
/// `guard` 给了就在**同一个事务里**先重读本地状态，和合并时用的那份比较：用户在
/// 合并和写入之间新建 / 修改了记录的话，直接返回 [`SyncError::LocalChanged`]，
/// 本地一个字节都不改（否则“合并结果里没有”的新记录会被删掉，改动会被旧值覆盖）。
pub fn apply_snapshot_guarded(
    db: &crate::store::Database,
    snapshot: &SyncSnapshot,
    sealer: &dyn SecretSealer,
    known_hosts_path: Option<&Path>,
    conflicts: &[Conflict],
    now: i64,
    guard: Option<LocalGuard<'_>>,
) -> Result<ApplyReport, SyncError> {
    apply_snapshot_guarded_with(
        db,
        snapshot,
        sealer,
        known_hosts_path,
        conflicts,
        now,
        guard,
        &SettingsPolicy::CURRENT,
    )
}

/// 同 [`apply_snapshot_guarded`]，设置按给定的规则写（测试用它模拟别的版本）。
#[allow(clippy::too_many_arguments)]
pub fn apply_snapshot_guarded_with(
    db: &crate::store::Database,
    snapshot: &SyncSnapshot,
    sealer: &dyn SecretSealer,
    known_hosts_path: Option<&Path>,
    conflicts: &[Conflict],
    now: i64,
    guard: Option<LocalGuard<'_>>,
    policy: &SettingsPolicy,
) -> Result<ApplyReport, SyncError> {
    db.with_conn_as(|conn| {
        let tx = conn.transaction()?;
        let mut report = ApplyReport::default();

        if let Some(guard) = &guard {
            let current =
                snapshot_from_conn(&tx, sealer, guard.device_id, known_hosts_path, now, policy)?;
            if !current.same_data(guard.expected) {
                // 事务随 tx 一起丢弃，什么都没写。
                return Err(SyncError::LocalChanged);
            }
        }

        for group in &snapshot.groups {
            require_uid("分组", &group.uid)?;
        }
        for host in &snapshot.hosts {
            require_uid("主机", &host.uid)?;
        }
        for identity in &snapshot.identities {
            require_uid("凭据", &identity.uid)?;
        }
        for proxy in &snapshot.proxies {
            require_uid("代理", &proxy.uid)?;
        }

        // 先把本机主机的引用全部释放，才能删除被引用的凭据 / 代理（外键 RESTRICT）。
        tx.execute(
            "UPDATE hosts SET group_id = NULL, identity_id = NULL, proxy_id = NULL, \
             jump_host_id = NULL",
            [],
        )?;

        let wanted_hosts: BTreeSet<&str> = snapshot.hosts.iter().map(|h| h.uid.as_str()).collect();
        let mut existing_hosts = uid_ids(&tx, "hosts")?;
        for (uid, id) in &existing_hosts {
            if !wanted_hosts.contains(uid.as_str()) {
                tx.execute("DELETE FROM hosts WHERE id = ?1", [id])?;
                report.hosts.deleted += 1;
            }
        }
        existing_hosts.retain(|uid, _| wanted_hosts.contains(uid.as_str()));

        let group_wanted: BTreeSet<&str> =
            snapshot.groups.iter().map(|r| r.uid.as_str()).collect();
        let identity_wanted: BTreeSet<&str> = snapshot
            .identities
            .iter()
            .map(|r| r.uid.as_str())
            .collect();
        let proxy_wanted: BTreeSet<&str> = snapshot.proxies.iter().map(|r| r.uid.as_str()).collect();
        report.groups.deleted = delete_missing(&tx, "groups", &group_wanted)?;
        report.identities.deleted = delete_missing(&tx, "identities", &identity_wanted)?;
        report.proxies.deleted = delete_missing(&tx, "proxies", &proxy_wanted)?;

        // 分组：先写自己，再按 uid 连父子（父分组可能后插入）。
        let mut group_ids = uid_ids(&tx, "groups")?;
        for group in &snapshot.groups {
            match group_ids.get(&group.uid) {
                Some(id) => {
                    tx.execute(
                        "UPDATE groups SET name = ?1, parent_id = NULL, sort = ?2, updated_at = ?3 \
                         WHERE id = ?4",
                        params![group.name, group.sort, group.updated_at, id],
                    )?;
                    report.groups.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO groups (uid, name, parent_id, sort, updated_at) \
                         VALUES (?1, ?2, NULL, ?3, ?4)",
                        params![group.uid, group.name, group.sort, group.updated_at],
                    )?;
                    group_ids.insert(group.uid.clone(), tx.last_insert_rowid());
                    report.groups.inserted += 1;
                }
            }
        }
        for group in &snapshot.groups {
            let parent = group
                .parent_uid
                .as_ref()
                .and_then(|uid| group_ids.get(uid).copied());
            if let Some(id) = group_ids.get(&group.uid) {
                tx.execute("UPDATE groups SET parent_id = ?1 WHERE id = ?2", params![parent, id])?;
            }
        }

        // 凭据：明文用本机 DPAPI 重新加密。
        let mut identity_ids = uid_ids(&tx, "identities")?;
        for identity in &snapshot.identities {
            let secret_blob = match &identity.secret {
                Some(secret) => Some(sealer.seal(secret.as_bytes())?),
                None => None,
            };
            match identity_ids.get(&identity.uid) {
                Some(id) => {
                    tx.execute(
                        "UPDATE identities SET name = ?1, kind = ?2, key_path = ?3, \
                         secret_blob = ?4, updated_at = ?5 WHERE id = ?6",
                        params![
                            identity.name,
                            identity.kind,
                            identity.key_path,
                            secret_blob,
                            identity.updated_at,
                            id
                        ],
                    )?;
                    report.identities.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO identities (uid, name, kind, key_path, secret_blob, \
                         updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            identity.uid,
                            identity.name,
                            identity.kind,
                            identity.key_path,
                            secret_blob,
                            identity.updated_at
                        ],
                    )?;
                    identity_ids.insert(identity.uid.clone(), tx.last_insert_rowid());
                    report.identities.inserted += 1;
                }
            }
        }

        let mut proxy_ids = uid_ids(&tx, "proxies")?;
        for proxy in &snapshot.proxies {
            let secret_blob = match &proxy.secret {
                Some(secret) => Some(sealer.seal(secret.as_bytes())?),
                None => None,
            };
            match proxy_ids.get(&proxy.uid) {
                Some(id) => {
                    tx.execute(
                        "UPDATE proxies SET name = ?1, kind = ?2, host = ?3, port = ?4, \
                         username = ?5, secret_blob = ?6, updated_at = ?7 WHERE id = ?8",
                        params![
                            proxy.name,
                            proxy.kind,
                            proxy.host,
                            proxy.port,
                            proxy.username,
                            secret_blob,
                            proxy.updated_at,
                            id
                        ],
                    )?;
                    report.proxies.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO proxies (uid, name, kind, host, port, username, \
                         secret_blob, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            proxy.uid,
                            proxy.name,
                            proxy.kind,
                            proxy.host,
                            proxy.port,
                            proxy.username,
                            secret_blob,
                            proxy.updated_at
                        ],
                    )?;
                    proxy_ids.insert(proxy.uid.clone(), tx.last_insert_rowid());
                    report.proxies.inserted += 1;
                }
            }
        }

        // 主机：第一遍不带跳板机，第二遍再连（跳板机可能后插入）。
        let mut host_ids = uid_ids(&tx, "hosts")?;
        for host in &snapshot.hosts {
            let group_id = host
                .group_uid
                .as_ref()
                .and_then(|uid| group_ids.get(uid).copied());
            let identity_id = host
                .identity_uid
                .as_ref()
                .and_then(|uid| identity_ids.get(uid).copied());
            let proxy_id = host
                .proxy_uid
                .as_ref()
                .and_then(|uid| proxy_ids.get(uid).copied());
            let tags = serde_json::to_string(&host.tags)
                .map_err(|err| SyncError::InvalidData(format!("标签序列化失败：{err}")))?;
            match host_ids.get(&host.uid) {
                Some(id) => {
                    tx.execute(
                        "UPDATE hosts SET group_id = ?1, name = ?2, host = ?3, port = ?4, \
                         username = ?5, auth_method = ?6, identity_id = ?7, proxy_id = ?8, \
                         jump_host_id = NULL, proxy_mode = ?9, tags = ?10, sort = ?11, \
                         legacy_algorithms = ?12, created_at = ?13, updated_at = ?14 WHERE id = ?15",
                        params![
                            group_id,
                            host.name,
                            host.host,
                            host.port,
                            host.username,
                            host.auth_method,
                            identity_id,
                            proxy_id,
                            host.proxy_mode,
                            tags,
                            host.sort,
                            host.legacy_algorithms,
                            host.created_at,
                            host.updated_at,
                            id
                        ],
                    )?;
                    report.hosts.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO hosts (uid, group_id, name, host, port, username, \
                         auth_method, identity_id, proxy_id, jump_host_id, proxy_mode, tags, \
                         sort, legacy_algorithms, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, ?12, ?13, ?14, ?15)",
                        params![
                            host.uid,
                            group_id,
                            host.name,
                            host.host,
                            host.port,
                            host.username,
                            host.auth_method,
                            identity_id,
                            proxy_id,
                            host.proxy_mode,
                            tags,
                            host.sort,
                            host.legacy_algorithms,
                            host.created_at,
                            host.updated_at
                        ],
                    )?;
                    host_ids.insert(host.uid.clone(), tx.last_insert_rowid());
                    report.hosts.inserted += 1;
                }
            }
        }
        for host in &snapshot.hosts {
            let jump = host
                .jump_host_uid
                .as_ref()
                .and_then(|uid| host_ids.get(uid).copied())
                .filter(|jump| Some(*jump) != host_ids.get(&host.uid).copied());
            if let Some(id) = host_ids.get(&host.uid) {
                tx.execute(
                    "UPDATE hosts SET jump_host_id = ?1 WHERE id = ?2",
                    params![jump, id],
                )?;
            }
        }

        // 设置：只动白名单里的键；同步包里没有的同步键删掉（恢复到包里的状态）。
        let wanted_settings: BTreeSet<&str> =
            snapshot.settings.keys().map(String::as_str).collect();
        let mut statement = tx.prepare("SELECT key FROM settings")?;
        let keys: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        drop(statement);
        for key in keys {
            if policy.is_synced(&key) && !wanted_settings.contains(key.as_str()) {
                tx.execute("DELETE FROM settings WHERE key = ?1", [&key])?;
            }
        }
        for (key, value) in &snapshot.settings {
            // 只写本版本认为合格的同步键：本机键和不认识的键（可能是更新版本的设置）不写；
            // 同步键的值不合格（可能是更新版本的新取值）也不写，本机保留原值（R-7 F07 / R-7b）。
            match policy.class(key) {
                KeyClass::LocalOnly | KeyClass::Unknown => {
                    tracing::debug!(key = %key, "同步包里的设置本版本不写进本机，已跳过");
                    continue;
                }
                KeyClass::Synced => {
                    if let Err(reason) = policy.validate(key, value) {
                        tracing::warn!(key = %key, %reason, "同步包里的设置本版本不接受，保留本机原值");
                        continue;
                    }
                }
            }
            let stored = if key == DEFAULT_PROXY_KEY {
                let proxy_id = value
                    .as_str()
                    .and_then(|uid| proxy_ids.get(uid).copied());
                serde_json::to_string(&proxy_id)
            } else {
                serde_json::to_string(value)
            }
            .map_err(|err| SyncError::InvalidData(format!("设置 {key} 序列化失败：{err}")))?;
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, stored],
            )?;
            report.settings_written += 1;
        }

        record_tombstones(&tx, &snapshot.tombstones)?;

        purge_expired(&tx, now)?;
        for conflict in conflicts {
            store_conflict(
                &tx,
                sealer,
                now,
                &conflict.entity,
                &conflict.key,
                &conflict.kept,
                &conflict.other,
            )?;
            report.conflicts_stored += 1;
        }

        // known_hosts 是文件不是表：只追加缺的行。放在 commit 之前，失败就回滚数据库。
        if let Some(path) = known_hosts_path {
            report.known_hosts_added = super::known_hosts::append_missing(path, &snapshot.known_hosts)?;
        }

        tx.commit()?;
        Ok(report)
    })
}

/// 本机库里每一类记录的条数（防误删阈值用）。
pub fn local_counts(
    db: &crate::store::Database,
) -> Result<BTreeMap<&'static str, usize>, SyncError> {
    db.with_conn(|conn| {
        let mut out = BTreeMap::new();
        for (table, entity) in [
            ("groups", ENTITY_GROUP),
            ("hosts", ENTITY_HOST),
            ("identities", ENTITY_IDENTITY),
            ("proxies", ENTITY_PROXY),
        ] {
            let count: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
            out.insert(entity, count as usize);
        }
        Ok(out)
    })
    .map_err(SyncError::from)
}

#[cfg(test)]
mod tests;
