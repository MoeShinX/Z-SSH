//! 主机的增删改查，以及连接前的凭据解析。

use rusqlite::{params, Connection, Row};

use crate::error::ZsshError;
use crate::model::{AuthMethod, Host, HostDraft, Identity, IdentityKind, ProxyMode};
use crate::ssh::auth::{AuthConfig, KeySource};
use crate::store::db::Database;

/// 拨号链最多几跳（含目标那一跳）。设计文档 4.2 定的是 5 级跳板机。
pub const MAX_CHAIN_HOPS: usize = 5;

/// 连接一台主机需要的全部信息。
///
/// **不要把这个结构体交给前端**：`auth` 里可能有明文密码或私钥内容。
#[derive(Debug, Clone)]
pub struct HostLogin {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 认证方式和保险库里已经有的材料（缺的到连接时问用户）
    pub auth: AuthConfig,
    pub legacy_algorithms: bool,
}

impl Database {
    pub fn host_list(&self) -> Result<Vec<Host>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, group_id, name, host, port, username, auth_method, identity_id, \
                 proxy_id, jump_host_id, proxy_mode, tags, sort, created_at, updated_at, \
                 legacy_algorithms FROM hosts ORDER BY sort, name COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], row_to_host)?;
            let mut hosts = Vec::new();
            for row in rows {
                hosts.push(row?);
            }
            Ok(hosts)
        })
    }

    pub fn host_get(&self, id: i64) -> Result<Host, ZsshError> {
        self.with_conn(|conn| host_get(conn, id))
    }

    /// 新建或修改一台主机，返回落库之后的结果。
    pub fn host_save(&self, draft: &HostDraft) -> Result<Host, ZsshError> {
        validate_host(draft)?;

        self.with_conn_write(|conn| {
            if let Some(group_id) = draft.group_id {
                crate::store::groups::ensure_group_exists(conn, group_id)?;
            }
            if let Some(identity_id) = draft.identity_id {
                crate::store::identities::ensure_identity_exists(conn, identity_id)?;
                let identity = crate::store::identities::identity_get(conn, identity_id)?;
                check_identity_matches(draft.auth_method, &identity)?;
            }
            if let Some(proxy_id) = draft.proxy_id {
                crate::store::proxies::ensure_proxy_exists(conn, proxy_id)?;
            }
            if let Some(jump_host_id) = draft.jump_host_id {
                if Some(jump_host_id) == draft.id {
                    return Err(ZsshError::InvalidInput("跳板机不能是主机自己".to_owned()));
                }
                host_get(conn, jump_host_id)?;
            }

            let tags = serde_json::to_string(&draft.tags)
                .map_err(|err| ZsshError::InvalidInput(format!("标签序列化失败：{err}")))?;
            let legacy = i64::from(draft.legacy_algorithms);
            let now = now_unix();

            match draft.id {
                Some(id) => {
                    let changed = conn.execute(
                        "UPDATE hosts SET group_id = ?1, name = ?2, host = ?3, port = ?4, \
                         username = ?5, auth_method = ?6, identity_id = ?7, proxy_id = ?8, \
                         jump_host_id = ?9, proxy_mode = ?10, tags = ?11, sort = ?12, \
                         updated_at = ?13, legacy_algorithms = ?14 WHERE id = ?15",
                        params![
                            draft.group_id,
                            draft.name.trim(),
                            draft.host.trim(),
                            draft.port,
                            draft.username.trim(),
                            draft.auth_method.as_str(),
                            draft.identity_id,
                            draft.proxy_id,
                            draft.jump_host_id,
                            draft.proxy_mode.as_str(),
                            tags,
                            draft.sort,
                            now,
                            legacy,
                            id,
                        ],
                    )?;
                    if changed == 0 {
                        return Err(ZsshError::NotFound {
                            entity: "主机", id
                        });
                    }
                    host_get(conn, id)
                }
                None => {
                    conn.execute(
                        "INSERT INTO hosts (uid, group_id, name, host, port, username, auth_method, \
                         identity_id, proxy_id, jump_host_id, proxy_mode, tags, sort, created_at, \
                         updated_at, legacy_algorithms) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)",
                        params![
                            crate::store::new_uid(),
                            draft.group_id,
                            draft.name.trim(),
                            draft.host.trim(),
                            draft.port,
                            draft.username.trim(),
                            draft.auth_method.as_str(),
                            draft.identity_id,
                            draft.proxy_id,
                            draft.jump_host_id,
                            draft.proxy_mode.as_str(),
                            tags,
                            draft.sort,
                            now,
                            legacy,
                        ],
                    )?;
                    host_get(conn, conn.last_insert_rowid())
                }
            }
        })
    }

    pub fn host_delete(&self, id: i64) -> Result<(), ZsshError> {
        self.with_conn_write(|conn| {
            let changed = conn.execute("DELETE FROM hosts WHERE id = ?1", [id])?;
            if changed == 0 {
                return Err(ZsshError::NotFound {
                    entity: "主机", id
                });
            }
            Ok(())
        })
    }

    /// 从目标主机沿 `jump_host_id` 展开成拨号链。
    ///
    /// 返回的顺序是**从我们这边开始连的顺序**：最外层的跳板机在前，目标在最后。
    /// 检测环路（A→B→A）并限制最多 [`MAX_CHAIN_HOPS`] 级（设计文档 4.2）。
    pub fn host_chain(&self, target_id: i64) -> Result<Vec<Host>, ZsshError> {
        self.with_conn(|conn| {
            // 从目标往上收集
            let mut upwards = vec![host_get(conn, target_id)?];
            let mut seen = std::collections::HashSet::new();
            seen.insert(target_id);

            while let Some(jump_id) = upwards.last().and_then(|host| host.jump_host_id) {
                if !seen.insert(jump_id) {
                    return Err(ZsshError::InvalidInput(format!(
                        "跳板机配置成环了：主机 id {jump_id} 重复出现"
                    )));
                }
                upwards.push(host_get(conn, jump_id)?);
                if upwards.len() > MAX_CHAIN_HOPS {
                    return Err(ZsshError::InvalidInput(format!(
                        "跳板机最多 {MAX_CHAIN_HOPS} 级，这台主机配得太深了"
                    )));
                }
            }

            // 反过来：最外层跳板机在前，目标在最后
            upwards.reverse();
            Ok(upwards)
        })
    }

    /// 连接前的凭据解析：把主机引用的材料从保险库里解出来（设计文档 4.3）。
    ///
    /// 只有后端用，返回值不回传前端。缺的密码 / 口令不在这里报错，留给连接时弹窗问。
    pub fn host_login(&self, id: i64) -> Result<HostLogin, ZsshError> {
        self.with_conn(|conn| {
            let host = host_get(conn, id)?;

            let auth = match host.auth_method {
                AuthMethod::Password => match host.identity_id {
                    Some(identity_id) => {
                        // 只有密码类凭据的秘密才能当 SSH 密码发出去：私钥类凭据的秘密是私钥口令
                        // 或整段私钥，发给服务器就等于交出了私钥。旧数据或同步来的数据可能是这种
                        // 组合，连接时也要拦（R-10 H2）。
                        let identity = crate::store::identities::identity_get(conn, identity_id)?;
                        check_identity_matches(AuthMethod::Password, &identity)?;
                        let secret = crate::store::identities::identity_secret(conn, identity_id)?;
                        AuthConfig::password(secret)
                    }
                    // 没配凭据：连的时候问用户
                    None => AuthConfig::password(None),
                },

                AuthMethod::Key => {
                    let identity_id = host.identity_id.ok_or_else(|| {
                        ZsshError::InvalidInput(format!(
                            "主机「{}」选了私钥认证但没选凭据",
                            host.name
                        ))
                    })?;
                    let identity = crate::store::identities::identity_get(conn, identity_id)?;
                    let secret = crate::store::identities::identity_secret(conn, identity_id)?;

                    let source = match identity.kind {
                        IdentityKind::KeyFile => {
                            let path = identity
                                .key_path
                                .clone()
                                .filter(|path| !path.trim().is_empty())
                                .ok_or_else(|| {
                                    ZsshError::InvalidInput(format!(
                                        "凭据「{}」没有填私钥文件路径",
                                        identity.name
                                    ))
                                })?;
                            KeySource::File {
                                path: path.into(),
                                passphrase: secret,
                            }
                        }
                        IdentityKind::KeyInline => {
                            let inline = crate::store::identities::InlineKey::parse(
                                secret.as_deref().unwrap_or_default(),
                            );
                            if inline.key.trim().is_empty() {
                                return Err(ZsshError::InvalidInput(format!(
                                    "凭据「{}」里没有私钥内容",
                                    identity.name
                                )));
                            }
                            KeySource::Inline {
                                pem: inline.key,
                                passphrase: inline.passphrase,
                            }
                        }
                        IdentityKind::Password | IdentityKind::Agent => {
                            return Err(ZsshError::InvalidInput(format!(
                                "凭据「{}」不是私钥凭据，不能用来做私钥认证",
                                identity.name
                            )))
                        }
                    };

                    AuthConfig::key(source)
                }

                AuthMethod::Agent => AuthConfig::agent(),
            };

            Ok(HostLogin {
                host: host.host,
                port: host.port,
                username: host.username,
                auth,
                legacy_algorithms: host.legacy_algorithms,
            })
        })
    }
}

/// 引用了这个凭据或代理的主机名，被引用时用来告诉用户先改哪些主机。
pub(crate) fn hosts_referencing(
    conn: &Connection,
    column: &str,
    id: i64,
) -> Result<Vec<String>, ZsshError> {
    // column 只可能是调用方写死的常量（identity_id / proxy_id），不来自输入
    let sql = format!("SELECT name FROM hosts WHERE {column} = ?1 ORDER BY name COLLATE NOCASE");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row?);
    }
    Ok(names)
}

pub(crate) fn host_get(conn: &Connection, id: i64) -> Result<Host, ZsshError> {
    conn.query_row(
        "SELECT id, group_id, name, host, port, username, auth_method, identity_id, \
         proxy_id, jump_host_id, proxy_mode, tags, sort, created_at, updated_at, \
         legacy_algorithms FROM hosts WHERE id = ?1",
        [id],
        row_to_host,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
            entity: "主机", id
        },
        other => other.into(),
    })
}

/// 认证方式和凭据类型要配套：密码认证只能用密码凭据，私钥认证只能用私钥凭据。
/// agent 认证不读凭据里的秘密，不限制。
fn check_identity_matches(method: AuthMethod, identity: &Identity) -> Result<(), ZsshError> {
    let ok = match method {
        AuthMethod::Password => identity.kind == IdentityKind::Password,
        AuthMethod::Key => matches!(
            identity.kind,
            IdentityKind::KeyFile | IdentityKind::KeyInline
        ),
        AuthMethod::Agent => true,
    };
    if ok {
        return Ok(());
    }
    Err(ZsshError::InvalidInput(match method {
        AuthMethod::Password => format!(
            "凭据「{}」不是密码凭据，不能用来做密码认证（它的内容会被当成密码发给服务器）",
            identity.name
        ),
        _ => format!("凭据「{}」不是私钥凭据，不能用来做私钥认证", identity.name),
    }))
}

fn validate_host(draft: &HostDraft) -> Result<(), ZsshError> {
    if draft.name.trim().is_empty() {
        return Err(ZsshError::InvalidInput("主机名不能为空".to_owned()));
    }
    // 地址会原样写进 known_hosts：`*`、逗号、空格之类会让那一行匹配到别的主机。
    crate::ssh::known_hosts::check_host_address(draft.host.trim())
        .map_err(ZsshError::InvalidInput)?;
    if draft.port == 0 {
        return Err(ZsshError::InvalidInput("端口要在 1-65535 之间".to_owned()));
    }
    if draft.username.trim().is_empty() {
        return Err(ZsshError::InvalidInput("用户名不能为空".to_owned()));
    }
    Ok(())
}

fn row_to_host(row: &Row<'_>) -> rusqlite::Result<Host> {
    let auth_method: String = row.get(6)?;
    let proxy_mode: String = row.get(10)?;
    let tags: Option<String> = row.get(11)?;
    Ok(Host {
        id: row.get(0)?,
        group_id: row.get(1)?,
        name: row.get(2)?,
        host: row.get(3)?,
        port: row.get(4)?,
        username: row.get(5)?,
        auth_method: crate::store::parse_column(&auth_method, AuthMethod::parse)?,
        identity_id: row.get(7)?,
        proxy_id: row.get(8)?,
        jump_host_id: row.get(9)?,
        proxy_mode: crate::store::parse_column(&proxy_mode, ProxyMode::parse)?,
        tags: tags
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default(),
        sort: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        legacy_algorithms: row.get::<_, i64>(15)? != 0,
    })
}

/// Unix 秒。
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}
