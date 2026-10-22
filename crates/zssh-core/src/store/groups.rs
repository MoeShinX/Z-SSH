//! 分组的增删改查。

use rusqlite::{params, Row};

use crate::error::ZsshError;
use crate::model::{Group, GroupDraft};
use crate::store::db::Database;

impl Database {
    pub fn group_list(&self) -> Result<Vec<Group>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, name, parent_id, sort FROM groups ORDER BY sort, name COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], row_to_group)?;
            let mut groups = Vec::new();
            for row in rows {
                groups.push(row?);
            }
            Ok(groups)
        })
    }

    /// 新建或修改一个分组，返回落库之后的结果。
    pub fn group_save(&self, draft: &GroupDraft) -> Result<Group, ZsshError> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(ZsshError::InvalidInput("分组名不能为空".to_owned()));
        }
        if draft.id.is_none() && draft.parent_id.is_some() {
            // 新建时父分组必须存在
        }

        self.with_conn_write(|conn| {
            if let Some(parent_id) = draft.parent_id {
                ensure_group_exists(conn, parent_id)?;
                if let Some(id) = draft.id {
                    if id == parent_id {
                        return Err(ZsshError::InvalidInput("分组不能把自己当父分组".to_owned()));
                    }
                    // 不能在子孙下面建父分组，否则树会成环
                    let mut ancestor = Some(parent_id);
                    while let Some(current) = ancestor {
                        if current == id {
                            return Err(ZsshError::InvalidInput(
                                "不能把分组移到它自己的子分组下面".to_owned(),
                            ));
                        }
                        ancestor = conn
                            .query_row(
                                "SELECT parent_id FROM groups WHERE id = ?1",
                                [current],
                                |row| row.get::<_, Option<i64>>(0),
                            )
                            .ok()
                            .flatten();
                    }
                }
            }

            let now = crate::store::hosts::now_unix();
            match draft.id {
                Some(id) => {
                    let changed = conn.execute(
                        "UPDATE groups SET name = ?1, parent_id = ?2, sort = ?3, updated_at = ?4 \
                         WHERE id = ?5",
                        params![name, draft.parent_id, draft.sort, now, id],
                    )?;
                    if changed == 0 {
                        return Err(ZsshError::NotFound {
                            entity: "分组", id
                        });
                    }
                    group_get(conn, id)
                }
                None => {
                    conn.execute(
                        "INSERT INTO groups (uid, name, parent_id, sort, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            crate::store::new_uid(),
                            name,
                            draft.parent_id,
                            draft.sort,
                            now
                        ],
                    )?;
                    group_get(conn, conn.last_insert_rowid())
                }
            }
        })
    }

    /// 删分组：里面的主机和子分组都挂回根目录，不删主机（设计文档 6.1 的规则）。
    pub fn group_delete(&self, id: i64) -> Result<(), ZsshError> {
        self.with_conn_write(|conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute("DELETE FROM groups WHERE id = ?1", [id])?;
            if changed == 0 {
                return Err(ZsshError::NotFound {
                    entity: "分组", id
                });
            }
            // groups.parent_id 上还有 ON DELETE SET NULL 兜底，这两句是显式的意图
            tx.execute(
                "UPDATE groups SET parent_id = NULL WHERE parent_id = ?1",
                [id],
            )?;
            tx.execute("UPDATE hosts SET group_id = NULL WHERE group_id = ?1", [id])?;
            tx.commit()?;
            Ok(())
        })
    }
}

pub(crate) fn group_get(conn: &rusqlite::Connection, id: i64) -> Result<Group, ZsshError> {
    conn.query_row(
        "SELECT id, name, parent_id, sort FROM groups WHERE id = ?1",
        [id],
        row_to_group,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
            entity: "分组", id
        },
        other => other.into(),
    })
}

pub(crate) fn ensure_group_exists(conn: &rusqlite::Connection, id: i64) -> Result<(), ZsshError> {
    let exists: i64 = conn.query_row("SELECT count(*) FROM groups WHERE id = ?1", [id], |row| {
        row.get(0)
    })?;
    if exists == 0 {
        return Err(ZsshError::NotFound {
            entity: "分组", id
        });
    }
    Ok(())
}

fn row_to_group(row: &Row<'_>) -> rusqlite::Result<Group> {
    Ok(Group {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        sort: row.get(3)?,
    })
}
