//! 主机长期记忆的存取（见 [`crate::ai::host_memory`]）。

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::error::ZsshError;
use crate::store::db::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostMemory {
    pub content: String,
    pub updated_at: i64,
}

impl Database {
    pub fn host_memory_get(&self, host_id: i64) -> Result<Option<HostMemory>, ZsshError> {
        self.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT content, updated_at FROM host_memory WHERE host_id = ?1",
                    [host_id],
                    |row| {
                        Ok(HostMemory {
                            content: row.get(0)?,
                            updated_at: row.get(1)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    /// 存一台主机的记忆。内容要先过 [`crate::ai::host_memory::clean_memory`]；空串 = 清空。
    pub fn host_memory_set(&self, host_id: i64, content: &str) -> Result<(), ZsshError> {
        self.with_conn(|conn| {
            if content.trim().is_empty() {
                conn.execute("DELETE FROM host_memory WHERE host_id = ?1", [host_id])?;
            } else {
                conn.execute(
                    "INSERT INTO host_memory (host_id, content, updated_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(host_id) DO UPDATE SET content = excluded.content, \
                     updated_at = excluded.updated_at",
                    params![host_id, content, crate::store::hosts::now_unix()],
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Database;

    #[test]
    fn set_get_clear_and_follow_the_host() {
        let db = Database::open_in_memory().unwrap();
        let host = crate::store::tests::sample_host(None, None);
        let host_id = db.host_save(&host).unwrap().id;
        assert_eq!(db.host_memory_get(host_id).unwrap(), None);
        db.host_memory_set(host_id, "服务：nginx").unwrap();
        db.host_memory_set(host_id, "服务：nginx、redis").unwrap();
        assert_eq!(
            db.host_memory_get(host_id).unwrap().unwrap().content,
            "服务：nginx、redis"
        );
        db.host_memory_set(host_id, "").unwrap();
        assert_eq!(db.host_memory_get(host_id).unwrap(), None);

        db.host_memory_set(host_id, "x").unwrap();
        db.host_delete(host_id).unwrap();
        assert_eq!(
            db.host_memory_get(host_id).unwrap(),
            None,
            "删主机时记忆跟着删"
        );
    }
}
