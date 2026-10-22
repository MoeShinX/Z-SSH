//! 冲突列表：合并时没被采用的那一份留在本机库里，用户可以改用它、恢复被删的
//! 记录，或者“我知道了”清掉。
//!
//! - 冲突里的两份 JSON 可能带密码 / 私钥，所以入库前用本机的 DPAPI 封装
//!   （`enc1:` + base64），列表时解开；
//! - 保留 90 天（和墓碑一致），过期的在每次写入和读取列表时清掉，不会无限增长；
//! - **删除赢的规则不变**。“恢复被删除的记录”是用户的一次明确操作：恢复出来的是
//!   一条**新记录**（新的 uid，内容照旧），不会被原来的墓碑再次删掉，之后正常
//!   参与同步。

use base64::Engine as _;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;

use super::apply::{apply_snapshot_guarded, LocalGuard};
use super::error::SyncError;
use super::package::{
    GroupRecord, HostRecord, IdentityRecord, ProxyRecord, ENTITY_GROUP, ENTITY_HOST,
    ENTITY_IDENTITY, ENTITY_KNOWN_HOST, ENTITY_PROXY, TOMBSTONE_RETENTION_SECS,
};
use super::sealer::SecretSealer;
use super::snapshot::snapshot_local;

/// 冲突保留多久。和墓碑一样 90 天。
pub const CONFLICT_RETENTION_SECS: i64 = TOMBSTONE_RETENTION_SECS;
const SEALED_PREFIX: &str = "enc1:";

/// 主机指纹冲突不能一键改用另一份。
///
/// 换掉一台主机已信任的公钥等于决定“以后信任另一把钥匙”，这是安全判断，应该在
/// 连接那台主机、看到它实际出示的指纹时做，而不是在冲突列表里凭另一台电脑的记录
/// 做。而且 known_hosts 落盘只追加缺的记录、不改已有行，硬做也不会真的生效，
/// 却会把冲突清掉，看起来像成功了（C15）。
const KNOWN_HOST_REFUSAL: &str = "主机指纹不能在这里改用另一份：替换已信任的公钥关系到连接安全。如果这台主机确实换了密钥，下次连接时核对新指纹再确认即可";
const KNOWN_HOST_DELETED_REFUSAL: &str = "已删除的主机指纹不能恢复，下次连接那台主机时重新确认即可";

pub(crate) fn encode_field(sealer: &dyn SecretSealer, json: &str) -> Result<String, SyncError> {
    if json.is_empty() {
        return Ok(String::new());
    }
    let sealed = sealer.seal(json.as_bytes())?;
    Ok(format!(
        "{SEALED_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(sealed)
    ))
}

fn decode_field(sealer: &dyn SecretSealer, stored: &str) -> Result<String, SyncError> {
    let Some(encoded) = stored.strip_prefix(SEALED_PREFIX) else {
        // S-1 写的旧行是明文 JSON（那时还没有界面）。
        return Ok(stored.to_owned());
    };
    let sealed = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| SyncError::InvalidData("冲突记录的编码损坏".to_owned()))?;
    let plain = sealer.open(&sealed)?;
    String::from_utf8(plain).map_err(|_| SyncError::InvalidData("冲突记录不是 UTF-8".to_owned()))
}

/// 清掉超过保留期的冲突，返回清掉的条数。
pub(crate) fn purge_expired(conn: &Connection, now: i64) -> Result<usize, SyncError> {
    let removed = conn.execute(
        "DELETE FROM sync_conflicts WHERE at < ?1",
        [now - CONFLICT_RETENTION_SECS],
    )?;
    Ok(removed)
}

/// 给界面看的一条冲突。**不含密码和私钥**，只有能让人认出是哪条记录的摘要。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConflictView {
    pub id: i64,
    pub at: i64,
    pub entity: String,
    pub key: String,
    /// 记录名称（主机名、分组名……）。
    pub name: String,
    /// 采用的那份的摘要；删除冲突是“已删除”。
    pub kept_summary: String,
    /// 被采用的那份是哪台电脑的改动（`None` = 不知道）。
    pub kept_device: Option<String>,
    pub kept_is_local: bool,
    /// 没采用的那份的摘要。
    pub other_summary: String,
    pub other_device: Option<String>,
    pub other_is_local: bool,
    /// 这是一条“删除赢了”的冲突（另一份是被删掉记录的最后改动）。
    pub deletion: bool,
    /// 能不能一键改用另一份 / 恢复。
    pub can_use_other: bool,
    pub message: String,
}

struct Row {
    id: i64,
    at: i64,
    entity: String,
    key: String,
    kept: String,
    other: String,
}

struct Summary {
    name: String,
    text: String,
    device: Option<String>,
}

fn summarize(entity: &str, key: &str, json: &str) -> Summary {
    let value: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let text_of = |field: &str| {
        value
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let port_of = |field: &str| {
        value
            .get(field)
            .and_then(Value::as_u64)
            .map(|p| p.to_string())
            .unwrap_or_default()
    };
    let device = value
        .get("device_id")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    match entity {
        ENTITY_HOST => Summary {
            name: text_of("name"),
            text: format!(
                "{}@{}:{}",
                text_of("username"),
                text_of("host"),
                port_of("port")
            ),
            device,
        },
        ENTITY_GROUP => Summary {
            name: text_of("name"),
            text: format!("分组「{}」", text_of("name")),
            device,
        },
        ENTITY_IDENTITY => Summary {
            name: text_of("name"),
            text: format!("凭据「{}」（{}）", text_of("name"), text_of("kind")),
            device,
        },
        ENTITY_PROXY => Summary {
            name: text_of("name"),
            text: format!(
                "{} {}:{}",
                text_of("kind"),
                text_of("host"),
                port_of("port")
            ),
            device,
        },
        ENTITY_KNOWN_HOST => Summary {
            name: format!("{}:{}", text_of("host"), port_of("port")),
            text: format!("{} 公钥", text_of("key_type")),
            device,
        },
        _ => {
            // 设置：值本身就是摘要（设置白名单里没有秘密）。
            let shown = if json.chars().count() > 80 {
                format!("{}…", json.chars().take(80).collect::<String>())
            } else {
                json.to_owned()
            };
            Summary {
                name: key.to_owned(),
                text: shown,
                device: None,
            }
        }
    }
}

fn read_rows(conn: &Connection) -> Result<Vec<Row>, SyncError> {
    let mut statement = conn.prepare(
        "SELECT id, at, entity, key, kept, other FROM sync_conflicts ORDER BY at DESC, id DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Row {
            id: row.get(0)?,
            at: row.get(1)?,
            entity: row.get(2)?,
            key: row.get(3)?,
            kept: row.get(4)?,
            other: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list(
    db: &crate::store::Database,
    sealer: &dyn SecretSealer,
    device_id: &str,
    now: i64,
) -> Result<Vec<ConflictView>, SyncError> {
    let rows = db.with_conn_as(|conn| -> Result<Vec<Row>, SyncError> {
        purge_expired(conn, now)?;
        read_rows(conn)
    })?;
    let mut views = Vec::new();
    for row in rows {
        // 一条解不开的冲突不能让整个列表打不开；它仍可以被“我知道了”清掉。
        let (kept, other) = match (
            decode_field(sealer, &row.kept),
            decode_field(sealer, &row.other),
        ) {
            (Ok(kept), Ok(other)) => (kept, other),
            _ => (String::new(), String::new()),
        };
        let deletion = kept.is_empty();
        let other_summary = summarize(&row.entity, &row.key, &other);
        let kept_summary = if deletion {
            Summary {
                name: other_summary.name.clone(),
                text: "已删除".to_owned(),
                device: None,
            }
        } else {
            summarize(&row.entity, &row.key, &kept)
        };
        let name = if other_summary.name.is_empty() {
            row.key.clone()
        } else {
            other_summary.name.clone()
        };
        // 主机指纹一律不给“改用另一份”，原因见 KNOWN_HOST_REFUSAL。
        let known_host = row.entity == ENTITY_KNOWN_HOST;
        let can_use_other = !(other.is_empty() || known_host);
        let message = match (known_host, deletion) {
            (true, true) => KNOWN_HOST_DELETED_REFUSAL.to_owned(),
            (true, false) => KNOWN_HOST_REFUSAL.to_owned(),
            (false, _) => String::new(),
        };
        views.push(ConflictView {
            id: row.id,
            at: row.at,
            kept_is_local: kept_summary.device.as_deref() == Some(device_id),
            other_is_local: other_summary.device.as_deref() == Some(device_id),
            entity: row.entity,
            key: row.key,
            name,
            kept_summary: kept_summary.text,
            kept_device: kept_summary.device,
            other_summary: other_summary.text,
            other_device: other_summary.device,
            deletion,
            can_use_other,
            message,
        });
    }
    Ok(views)
}

/// “我知道了”：清掉一条冲突。
pub fn dismiss(db: &crate::store::Database, id: i64) -> Result<(), SyncError> {
    db.with_conn_as(|conn| -> Result<(), SyncError> {
        conn.execute("DELETE FROM sync_conflicts WHERE id = ?1", [id])?;
        Ok(())
    })
}

fn load_row(db: &crate::store::Database, id: i64) -> Result<Row, SyncError> {
    db.with_conn_as(|conn| -> Result<Row, SyncError> {
        read_rows(conn)?
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| SyncError::InvalidData("这条冲突已经不存在".to_owned()))
    })
}

fn known_uid(uid: &Option<String>, existing: impl Fn(&str) -> bool) -> Option<String> {
    uid.as_ref().filter(|u| existing(u)).cloned()
}

/// 改用另一份：删除类冲突 = 把被删的记录恢复成一条新记录；其余 = 用另一份覆盖
/// 现在的内容。结果写回本地（和同步用同一套“写前重读比较”），之后正常参与下次
/// 同步。成功后这条冲突被清掉。
pub fn use_other(
    db: &crate::store::Database,
    sealer: &dyn SecretSealer,
    device_id: &str,
    known_hosts_path: Option<&std::path::Path>,
    id: i64,
    now: i64,
) -> Result<(), SyncError> {
    let row = load_row(db, id)?;
    let kept = decode_field(sealer, &row.kept)?;
    let other = decode_field(sealer, &row.other)?;
    if other.is_empty() {
        return Err(SyncError::InvalidData(
            "这条冲突没有可用的另一份".to_owned(),
        ));
    }
    let deletion = kept.is_empty();
    let local = snapshot_local(db, sealer, device_id, known_hosts_path, now)?;
    let mut target = local.clone();
    let parse_err =
        |err: serde_json::Error| SyncError::InvalidData(format!("冲突记录解析失败：{err}"));

    match row.entity.as_str() {
        ENTITY_HOST => {
            let mut record: HostRecord = serde_json::from_str(&other).map_err(parse_err)?;
            stamp_host(&mut record, device_id, now);
            if deletion {
                record.uid = uuid::Uuid::new_v4().to_string();
                let groups: Vec<String> = target.groups.iter().map(|g| g.uid.clone()).collect();
                let identities: Vec<String> =
                    target.identities.iter().map(|i| i.uid.clone()).collect();
                let proxies: Vec<String> = target.proxies.iter().map(|p| p.uid.clone()).collect();
                let hosts: Vec<String> = target.hosts.iter().map(|h| h.uid.clone()).collect();
                record.group_uid = known_uid(&record.group_uid, |u| groups.iter().any(|g| g == u));
                record.identity_uid =
                    known_uid(&record.identity_uid, |u| identities.iter().any(|g| g == u));
                record.proxy_uid = known_uid(&record.proxy_uid, |u| proxies.iter().any(|g| g == u));
                record.jump_host_uid =
                    known_uid(&record.jump_host_uid, |u| hosts.iter().any(|g| g == u));
                target.hosts.push(record);
            } else {
                let slot = target
                    .hosts
                    .iter_mut()
                    .find(|h| h.uid == row.key)
                    .ok_or_else(|| SyncError::InvalidData("这条主机已经不存在了".to_owned()))?;
                // 引用以现在的为准：另一份里引用的记录可能已经不在了。
                record.group_uid = slot.group_uid.clone();
                record.identity_uid = slot.identity_uid.clone();
                record.proxy_uid = slot.proxy_uid.clone();
                record.jump_host_uid = slot.jump_host_uid.clone();
                record.uid = slot.uid.clone();
                *slot = record;
            }
        }
        ENTITY_GROUP => {
            let mut record: GroupRecord = serde_json::from_str(&other).map_err(parse_err)?;
            record.updated_at = now;
            record.device_id = device_id.to_owned();
            if deletion {
                record.uid = uuid::Uuid::new_v4().to_string();
                let groups: Vec<String> = target.groups.iter().map(|g| g.uid.clone()).collect();
                record.parent_uid =
                    known_uid(&record.parent_uid, |u| groups.iter().any(|g| g == u));
                target.groups.push(record);
            } else {
                let slot = target
                    .groups
                    .iter_mut()
                    .find(|g| g.uid == row.key)
                    .ok_or_else(|| SyncError::InvalidData("这个分组已经不存在了".to_owned()))?;
                record.uid = slot.uid.clone();
                record.parent_uid = slot.parent_uid.clone();
                *slot = record;
            }
        }
        ENTITY_IDENTITY => {
            let mut record: IdentityRecord = serde_json::from_str(&other).map_err(parse_err)?;
            record.updated_at = now;
            record.device_id = device_id.to_owned();
            if deletion {
                record.uid = uuid::Uuid::new_v4().to_string();
                target.identities.push(record);
            } else {
                let slot = target
                    .identities
                    .iter_mut()
                    .find(|i| i.uid == row.key)
                    .ok_or_else(|| SyncError::InvalidData("这个凭据已经不存在了".to_owned()))?;
                record.uid = slot.uid.clone();
                *slot = record;
            }
        }
        ENTITY_PROXY => {
            let mut record: ProxyRecord = serde_json::from_str(&other).map_err(parse_err)?;
            record.updated_at = now;
            record.device_id = device_id.to_owned();
            if deletion {
                record.uid = uuid::Uuid::new_v4().to_string();
                target.proxies.push(record);
            } else {
                let slot = target
                    .proxies
                    .iter_mut()
                    .find(|p| p.uid == row.key)
                    .ok_or_else(|| SyncError::InvalidData("这个代理已经不存在了".to_owned()))?;
                record.uid = slot.uid.clone();
                *slot = record;
            }
        }
        // 不写任何东西就返回，冲突原样留着（原因见 KNOWN_HOST_REFUSAL）。
        ENTITY_KNOWN_HOST => {
            let reason = if deletion {
                KNOWN_HOST_DELETED_REFUSAL
            } else {
                KNOWN_HOST_REFUSAL
            };
            return Err(SyncError::InvalidData(reason.to_owned()));
        }
        _ => {
            let value: Value = serde_json::from_str(&other).map_err(parse_err)?;
            target.settings.insert(row.key.clone(), value);
        }
    }
    target.sort_canonical();

    apply_snapshot_guarded(
        db,
        &target,
        sealer,
        known_hosts_path,
        &[],
        now,
        Some(LocalGuard {
            expected: &local,
            device_id,
        }),
    )?;
    dismiss(db, id)
}

fn stamp_host(record: &mut HostRecord, device_id: &str, now: i64) {
    record.updated_at = now;
    record.device_id = device_id.to_owned();
}

pub(crate) fn store_conflict(
    conn: &Connection,
    sealer: &dyn SecretSealer,
    now: i64,
    entity: &str,
    key: &str,
    kept: &str,
    other: &str,
) -> Result<(), SyncError> {
    conn.execute(
        "INSERT INTO sync_conflicts (at, entity, key, kept, other) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            now,
            entity,
            key,
            encode_field(sealer, kept)?,
            encode_field(sealer, other)?
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
