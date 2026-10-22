//! 端口转发规则的增删改查。只存本机，不进配置同步（端口在每台电脑上可能被别的程序占着）。

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

use crate::error::ZsshError;
use crate::store::db::Database;

/// 转发方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// 本机监听，经 SSH 服务器连到目标（`ssh -L`）。
    Local,
    /// 服务器监听，连回本机这一侧的目标（`ssh -R`）。
    Remote,
}

impl ForwardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }

    pub fn parse(text: &str) -> Result<Self, ZsshError> {
        match text {
            "local" => Ok(Self::Local),
            "remote" => Ok(Self::Remote),
            other => Err(ZsshError::InvalidInput(format!("未知的转发类型：{other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForward {
    pub id: i64,
    pub host_id: i64,
    pub name: String,
    pub kind: ForwardKind,
    pub bind_host: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    pub auto_start: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortForwardDraft {
    pub id: Option<i64>,
    pub host_id: i64,
    #[serde(default)]
    pub name: String,
    pub kind: ForwardKind,
    #[serde(default)]
    pub bind_host: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    #[serde(default)]
    pub auto_start: bool,
}

/// 监听地址留空时用回环：只有本机能连，不会一不小心把端口暴露到局域网。
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";

/// 主机名 / 地址只允许常见字符：拼进 SSH 请求里，不能带空白和控制字符。
fn valid_address(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 255
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']' | '*')
        })
}

fn validate(draft: &PortForwardDraft) -> Result<(String, String, String), ZsshError> {
    let name = draft.name.trim().to_owned();
    if name.chars().count() > 64 {
        return Err(ZsshError::InvalidInput("名称最多 64 个字".to_owned()));
    }
    let bind_host = match draft.bind_host.trim() {
        "" => DEFAULT_BIND_HOST.to_owned(),
        other => other.to_owned(),
    };
    if !valid_address(&bind_host) {
        return Err(ZsshError::InvalidInput(format!(
            "监听地址不对：{bind_host}"
        )));
    }
    let dest_host = draft.dest_host.trim().to_owned();
    if !valid_address(&dest_host) {
        return Err(ZsshError::InvalidInput(
            "目标地址不能为空，也不能带空格".to_owned(),
        ));
    }
    if draft.bind_port == 0 || draft.dest_port == 0 {
        return Err(ZsshError::InvalidInput("端口要在 1-65535 之间".to_owned()));
    }
    Ok((name, bind_host, dest_host))
}

fn row_to_forward(row: &Row<'_>) -> rusqlite::Result<PortForward> {
    let kind: String = row.get(3)?;
    Ok(PortForward {
        id: row.get(0)?,
        host_id: row.get(1)?,
        name: row.get(2)?,
        kind: crate::store::parse_column(&kind, ForwardKind::parse)?,
        bind_host: row.get(4)?,
        bind_port: row.get(5)?,
        dest_host: row.get(6)?,
        dest_port: row.get(7)?,
        auto_start: row.get(8)?,
    })
}

const COLUMNS: &str =
    "id, host_id, name, kind, bind_host, bind_port, dest_host, dest_port, auto_start";

impl Database {
    /// 所有转发规则，按主机、再按创建先后排。
    pub fn forward_list(&self) -> Result<Vec<PortForward>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM port_forwards ORDER BY host_id, id"
            ))?;
            let rows = statement.query_map([], row_to_forward)?;
            let mut forwards = Vec::new();
            for row in rows {
                forwards.push(row?);
            }
            Ok(forwards)
        })
    }

    pub fn forward_get(&self, id: i64) -> Result<PortForward, ZsshError> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM port_forwards WHERE id = ?1"),
                [id],
                row_to_forward,
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
                    entity: "端口转发",
                    id,
                },
                other => other.into(),
            })
        })
    }

    /// 新建或修改一条规则。
    pub fn forward_save(&self, draft: &PortForwardDraft) -> Result<PortForward, ZsshError> {
        let (name, bind_host, dest_host) = validate(draft)?;
        // 同一台主机上两条远程转发监听同一个端口：服务器只会接受一条，另一条开启时一定失败，
        // 保存时就拦下（复审 F3）。
        if draft.kind == ForwardKind::Remote {
            let clash = self.forward_list()?.into_iter().any(|rule| {
                rule.host_id == draft.host_id
                    && rule.kind == ForwardKind::Remote
                    && rule.bind_port == draft.bind_port
                    && Some(rule.id) != draft.id
            });
            if clash {
                return Err(ZsshError::InvalidInput(format!(
                    "这台主机已经有一条远程转发监听端口 {}",
                    draft.bind_port
                )));
            }
        }
        let id = self.with_conn(|conn| {
            let now = crate::store::hosts::now_unix();
            match draft.id {
                Some(id) => {
                    let changed = conn.execute(
                        "UPDATE port_forwards SET host_id = ?1, name = ?2, kind = ?3, \
                         bind_host = ?4, bind_port = ?5, dest_host = ?6, dest_port = ?7, \
                         auto_start = ?8, updated_at = ?9 WHERE id = ?10",
                        params![
                            draft.host_id,
                            name,
                            draft.kind.as_str(),
                            bind_host,
                            draft.bind_port,
                            dest_host,
                            draft.dest_port,
                            draft.auto_start,
                            now,
                            id
                        ],
                    )?;
                    if changed == 0 {
                        return Err(ZsshError::NotFound {
                            entity: "端口转发",
                            id,
                        });
                    }
                    Ok(id)
                }
                None => {
                    conn.execute(
                        "INSERT INTO port_forwards (host_id, name, kind, bind_host, bind_port, \
                         dest_host, dest_port, auto_start, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                        params![
                            draft.host_id,
                            name,
                            draft.kind.as_str(),
                            bind_host,
                            draft.bind_port,
                            dest_host,
                            draft.dest_port,
                            draft.auto_start,
                            now
                        ],
                    )?;
                    Ok(conn.last_insert_rowid())
                }
            }
        })?;
        self.forward_get(id)
    }

    pub fn forward_delete(&self, id: i64) -> Result<(), ZsshError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM port_forwards WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, HostDraft, ProxyMode};

    fn host(db: &Database) -> i64 {
        db.host_save(&HostDraft {
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
        .unwrap()
        .id
    }

    fn draft(host_id: i64) -> PortForwardDraft {
        PortForwardDraft {
            id: None,
            host_id,
            name: " 数据库 ".to_owned(),
            kind: ForwardKind::Local,
            bind_host: String::new(),
            bind_port: 15432,
            dest_host: "localhost".to_owned(),
            dest_port: 5432,
            auto_start: true,
        }
    }

    #[test]
    fn save_list_update_delete() {
        let db = Database::open_in_memory().unwrap();
        let host_id = host(&db);
        let saved = db.forward_save(&draft(host_id)).unwrap();
        assert_eq!(saved.name, "数据库");
        assert_eq!(saved.bind_host, DEFAULT_BIND_HOST, "监听地址留空时用回环");
        assert!(saved.auto_start);

        let mut edit = draft(host_id);
        edit.id = Some(saved.id);
        edit.kind = ForwardKind::Remote;
        edit.bind_port = 8080;
        let updated = db.forward_save(&edit).unwrap();
        assert_eq!(updated.kind, ForwardKind::Remote);
        assert_eq!(db.forward_list().unwrap(), vec![updated]);

        db.forward_delete(saved.id).unwrap();
        assert!(db.forward_list().unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        let db = Database::open_in_memory().unwrap();
        let host_id = host(&db);
        let mut bad = draft(host_id);
        bad.dest_host = "a b".to_owned();
        assert!(db.forward_save(&bad).is_err());
        let mut bad = draft(host_id);
        bad.bind_port = 0;
        assert!(db.forward_save(&bad).is_err());
    }

    #[test]
    fn duplicate_remote_ports_on_one_host_are_refused() {
        let db = Database::open_in_memory().unwrap();
        let host_id = host(&db);
        let mut remote = draft(host_id);
        remote.kind = ForwardKind::Remote;
        let saved = db.forward_save(&remote).unwrap();
        let err = db.forward_save(&remote).unwrap_err();
        assert!(err.to_string().contains("15432"), "{err}");
        // 改自己那条（同一个 id）不算重复
        remote.id = Some(saved.id);
        remote.name = "改名".to_owned();
        db.forward_save(&remote).unwrap();
    }

    #[test]
    fn deleting_the_host_deletes_its_forwards() {
        let db = Database::open_in_memory().unwrap();
        let host_id = host(&db);
        db.forward_save(&draft(host_id)).unwrap();
        db.host_delete(host_id).unwrap();
        assert!(db.forward_list().unwrap().is_empty());
    }
}
