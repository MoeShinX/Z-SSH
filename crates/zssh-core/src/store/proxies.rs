//! 代理的增删改查。

use rusqlite::{params, Connection, Row};

use crate::error::ZsshError;
use crate::model::{Proxy, ProxyDraft, ProxyKind};
use crate::store::db::Database;

/// 连接时用的代理信息，含明文密码，**不要交给前端**。
#[derive(Clone)]
pub struct ProxyLogin {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl std::fmt::Debug for ProxyLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyLogin")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Database {
    /// 取一个代理的连接信息（密码从保险库解出来）。
    pub fn proxy_login(&self, id: i64) -> Result<ProxyLogin, ZsshError> {
        self.with_conn(|conn| {
            let proxy = proxy_get(conn, id)?;
            let blob: Option<Vec<u8>> = conn.query_row(
                "SELECT secret_blob FROM proxies WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?;
            let password = match blob {
                Some(blob) if !blob.is_empty() => {
                    let plaintext = crate::store::secrets::unprotect(&blob)?;
                    Some(String::from_utf8(plaintext).map_err(|err| {
                        ZsshError::Secret(format!("解出来的代理密码不是 UTF-8：{err}"))
                    })?)
                }
                _ => None,
            };

            Ok(ProxyLogin {
                kind: proxy.kind,
                host: proxy.host,
                port: proxy.port,
                username: proxy.username,
                password,
            })
        })
    }

    pub fn proxy_list(&self) -> Result<Vec<Proxy>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, name, kind, host, port, username, \
                 secret_blob IS NOT NULL AND length(secret_blob) > 0 \
                 FROM proxies ORDER BY name COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], row_to_proxy)?;
            let mut proxies = Vec::new();
            for row in rows {
                proxies.push(row?);
            }
            Ok(proxies)
        })
    }

    /// 新建或修改一个代理。`secret` 为 `None` 表示不改动已存的密码。
    pub fn proxy_save(&self, draft: &ProxyDraft) -> Result<Proxy, ZsshError> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(ZsshError::InvalidInput("代理名不能为空".to_owned()));
        }
        if draft.host.trim().is_empty() {
            return Err(ZsshError::InvalidInput("代理地址不能为空".to_owned()));
        }
        if draft.port == 0 {
            return Err(ZsshError::InvalidInput("端口要在 1-65535 之间".to_owned()));
        }

        let secret_blob = match draft.secret.as_deref() {
            None => None,
            Some("") => Some(Vec::new()),
            Some(secret) => Some(crate::store::secrets::protect(secret.as_bytes())?),
        };

        self.with_conn_write(|conn| {
            let now = crate::store::hosts::now_unix();
            let id = match draft.id {
                Some(id) => {
                    let changed = match &secret_blob {
                        Some(blob) => conn.execute(
                            "UPDATE proxies SET name = ?1, kind = ?2, host = ?3, port = ?4, \
                             username = ?5, secret_blob = ?6, updated_at = ?7 WHERE id = ?8",
                            params![
                                name,
                                draft.kind.as_str(),
                                draft.host.trim(),
                                draft.port,
                                draft.username,
                                blob,
                                now,
                                id
                            ],
                        )?,
                        None => conn.execute(
                            "UPDATE proxies SET name = ?1, kind = ?2, host = ?3, port = ?4, \
                             username = ?5, updated_at = ?6 WHERE id = ?7",
                            params![
                                name,
                                draft.kind.as_str(),
                                draft.host.trim(),
                                draft.port,
                                draft.username,
                                now,
                                id
                            ],
                        )?,
                    };
                    if changed == 0 {
                        return Err(ZsshError::NotFound {
                            entity: "代理", id
                        });
                    }
                    id
                }
                None => {
                    conn.execute(
                        "INSERT INTO proxies (uid, name, kind, host, port, username, secret_blob, \
                         updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            crate::store::new_uid(),
                            name,
                            draft.kind.as_str(),
                            draft.host.trim(),
                            draft.port,
                            draft.username,
                            secret_blob,
                            now
                        ],
                    )?;
                    conn.last_insert_rowid()
                }
            };
            proxy_get(conn, id)
        })
    }

    /// 删代理：还被主机引用时拒绝，并列出主机名。
    pub fn proxy_delete(&self, id: i64) -> Result<(), ZsshError> {
        self.with_conn_write(|conn| {
            let proxy = proxy_get(conn, id)?;
            let hosts = crate::store::hosts::hosts_referencing(conn, "proxy_id", id)?;
            if !hosts.is_empty() {
                return Err(ZsshError::ProxyInUse {
                    name: proxy.name,
                    hosts: hosts.join("、"),
                });
            }
            conn.execute("DELETE FROM proxies WHERE id = ?1", [id])?;
            // 指向它的默认代理设置一起改掉：留着一个不存在的 id，跟随默认代理的连接、
            // 软件联网都会报"找不到代理"。SSH 默认代理改成不用；软件网络改回默认（跟 SSH 走）。
            conn.execute(
                "UPDATE settings SET value = 'null' WHERE key = ?1 AND value = ?2",
                params![crate::store::settings::DEFAULT_PROXY_KEY, id.to_string()],
            )?;
            conn.execute(
                "DELETE FROM settings WHERE key = ?1 AND value = ?2",
                params![
                    crate::sync::app_proxy::APP_NETWORK_PROXY_KEY,
                    format!("\"proxy:{id}\"")
                ],
            )?;
            Ok(())
        })
    }
}

pub(crate) fn proxy_get(conn: &Connection, id: i64) -> Result<Proxy, ZsshError> {
    conn.query_row(
        "SELECT id, name, kind, host, port, username, \
         secret_blob IS NOT NULL AND length(secret_blob) > 0 FROM proxies WHERE id = ?1",
        [id],
        row_to_proxy,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
            entity: "代理", id
        },
        other => other.into(),
    })
}

pub(crate) fn ensure_proxy_exists(conn: &Connection, id: i64) -> Result<(), ZsshError> {
    proxy_get(conn, id).map(|_| ())
}

fn row_to_proxy(row: &Row<'_>) -> rusqlite::Result<Proxy> {
    let kind: String = row.get(2)?;
    Ok(Proxy {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: crate::store::parse_column(&kind, ProxyKind::parse)?,
        host: row.get(3)?,
        port: row.get(4)?,
        username: row.get(5)?,
        has_secret: row.get(6)?,
    })
}
