//! 本地备份里额外存的"只在这台电脑上"的数据：主机长期记忆、端口转发规则。
//!
//! 它们不进同步包和离线备份文件（换电脑没意义），但**本地自动备份**要带上：替换导入、
//! 同步合并、恢复备份都可能删掉主机，主机删了这两张表跟着级联删除；恢复导入前的那份
//! 备份时，主机回来了，它们也要回来（复审 F4）。
//!
//! 关联用主机的 uid（稳定），不用数据库的整数 id（恢复时主机是重新插入的，id 会变）。

use std::collections::BTreeMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::error::SyncError;
use crate::error::ZsshError;
use crate::store::forwards::{ForwardKind, PortForwardDraft};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalExtras {
    #[serde(default)]
    pub host_memory: Vec<MemoryRecord>,
    #[serde(default)]
    pub forwards: Vec<ForwardRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub host_uid: String,
    pub content: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardRecord {
    pub host_uid: String,
    pub name: String,
    pub kind: ForwardKind,
    pub bind_host: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    pub auto_start: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExtrasRestored {
    pub host_memory: usize,
    pub forwards: usize,
}

impl LocalExtras {
    pub fn read(db: &crate::store::Database) -> Result<Self, SyncError> {
        db.with_conn_as(|conn| {
            let mut extras = LocalExtras::default();
            let mut statement = conn.prepare(
                "SELECT h.uid, m.content, m.updated_at FROM host_memory m \
                 JOIN hosts h ON h.id = m.host_id ORDER BY h.uid",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(MemoryRecord {
                    host_uid: row.get(0)?,
                    content: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?;
            for row in rows {
                extras.host_memory.push(row?);
            }
            let mut statement = conn.prepare(
                "SELECT h.uid, f.name, f.kind, f.bind_host, f.bind_port, f.dest_host, f.dest_port, \
                 f.auto_start FROM port_forwards f JOIN hosts h ON h.id = f.host_id ORDER BY f.id",
            )?;
            let rows = statement.query_map([], |row| {
                let kind: String = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    kind,
                    row.get::<_, String>(3)?,
                    row.get::<_, u16>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, u16>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            })?;
            for row in rows {
                let (host_uid, name, kind, bind_host, bind_port, dest_host, dest_port, auto_start) =
                    row?;
                let Ok(kind) = ForwardKind::parse(&kind) else {
                    continue;
                };
                extras.forwards.push(ForwardRecord {
                    host_uid,
                    name,
                    kind,
                    bind_host,
                    bind_port,
                    dest_host,
                    dest_port,
                    auto_start,
                });
            }
            Ok(extras)
        })
    }

    /// 写回：主机按 uid 找（`renamed` 是恢复时换过 uid 的主机：旧 uid → 新 uid）；找不到的跳过。
    /// 记忆覆盖成备份里的内容；转发规则只补回本机没有的（同一主机、同方向、同监听端口算同一条）。
    pub fn restore(
        &self,
        db: &crate::store::Database,
        renamed: &BTreeMap<String, String>,
    ) -> Result<ExtrasRestored, SyncError> {
        let host_ids: BTreeMap<String, i64> = db.with_conn_as(|conn| {
            let mut statement = conn.prepare("SELECT uid, id FROM hosts")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut map = BTreeMap::new();
            for row in rows {
                let (uid, id): (String, i64) = row?;
                map.insert(uid, id);
            }
            Ok::<_, SyncError>(map)
        })?;
        let host_of = |uid: &str| {
            let uid = renamed.get(uid).map(String::as_str).unwrap_or(uid);
            host_ids.get(uid).copied()
        };

        let mut restored = ExtrasRestored::default();
        for memory in &self.host_memory {
            let Some(host_id) = host_of(&memory.host_uid) else {
                continue;
            };
            db.with_conn_as(|conn| {
                conn.execute(
                    "INSERT INTO host_memory (host_id, content, updated_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(host_id) DO UPDATE SET content = excluded.content, \
                     updated_at = excluded.updated_at",
                    params![host_id, memory.content, memory.updated_at],
                )?;
                Ok::<_, SyncError>(())
            })?;
            restored.host_memory += 1;
        }

        // 旧版本允许同一台主机上两条远程转发占同一个端口，现在保存会被拒绝。恢复时遇到这种
        // 冲突（或其它校验不过的规则）就跳过那一条，不能让整次恢复中途失败。
        let mut existing = db.forward_list().map_err(SyncError::from)?;
        for forward in &self.forwards {
            let Some(host_id) = host_of(&forward.host_uid) else {
                continue;
            };
            let present = existing.iter().any(|rule| {
                rule.host_id == host_id
                    && rule.kind == forward.kind
                    && rule.bind_port == forward.bind_port
            });
            if present {
                continue;
            }
            let saved = db.forward_save(&PortForwardDraft {
                id: None,
                host_id,
                name: forward.name.clone(),
                kind: forward.kind,
                bind_host: forward.bind_host.clone(),
                bind_port: forward.bind_port,
                dest_host: forward.dest_host.clone(),
                dest_port: forward.dest_port,
                auto_start: forward.auto_start,
            });
            match saved {
                Ok(rule) => {
                    existing.push(rule);
                    restored.forwards += 1;
                }
                Err(ZsshError::InvalidInput(_)) => continue,
                Err(err) => return Err(err.into()),
            }
        }
        Ok(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, HostDraft, ProxyMode};
    use crate::store::db::Database;

    fn host(db: &Database) -> (i64, String) {
        let saved = db
            .host_save(&HostDraft {
                id: None,
                group_id: None,
                name: "测试机".to_owned(),
                host: "example.invalid".to_owned(),
                port: 22,
                username: "tester".to_owned(),
                auth_method: AuthMethod::Password,
                identity_id: None,
                proxy_id: None,
                jump_host_id: None,
                proxy_mode: ProxyMode::Default,
                tags: Vec::new(),
                sort: 0,
                legacy_algorithms: false,
            })
            .unwrap();
        let uid: String = db
            .with_conn_as(|conn| {
                conn.query_row("SELECT uid FROM hosts WHERE id = ?1", [saved.id], |row| {
                    row.get(0)
                })
                .map_err(SyncError::from)
            })
            .unwrap();
        (saved.id, uid)
    }

    fn remote(host_uid: &str, name: &str, dest_port: u16) -> ForwardRecord {
        ForwardRecord {
            host_uid: host_uid.to_owned(),
            name: name.to_owned(),
            kind: ForwardKind::Remote,
            bind_host: String::new(),
            bind_port: 18080,
            dest_host: "localhost".to_owned(),
            dest_port,
            auto_start: false,
        }
    }

    #[test]
    fn legacy_duplicate_remote_rules_are_skipped_not_fatal() {
        let db = Database::open_in_memory().unwrap();
        let (host_id, uid) = host(&db);
        let extras = LocalExtras {
            host_memory: vec![MemoryRecord {
                host_uid: uid.clone(),
                content: "记忆".to_owned(),
                updated_at: 1,
            }],
            forwards: vec![remote(&uid, "甲", 80), remote(&uid, "乙", 81)],
        };
        let restored = extras.restore(&db, &BTreeMap::new()).unwrap();
        assert_eq!(restored.host_memory, 1);
        assert_eq!(restored.forwards, 1);
        let rules = db.forward_list().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].host_id, host_id);
        assert_eq!(rules[0].name, "甲");
    }
}
