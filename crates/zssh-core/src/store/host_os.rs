//! 主机的系统类型（见 [`crate::ssh::os_probe`]）。

use std::collections::BTreeMap;

use rusqlite::params;

use crate::error::ZsshError;
use crate::store::db::Database;

impl Database {
    /// 所有探测过的主机：主机 ID → 系统 ID。
    pub fn host_os_list(&self) -> Result<BTreeMap<i64, String>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare("SELECT host_id, os FROM host_os")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut map = BTreeMap::new();
            for row in rows {
                let (host_id, os) = row?;
                map.insert(host_id, os);
            }
            Ok(map)
        })
    }

    /// 记下一台主机的系统。返回 true 表示和原来记的不一样（要通知界面）。
    pub fn host_os_set(&self, host_id: i64, os: &str) -> Result<bool, ZsshError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "INSERT INTO host_os (host_id, os, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(host_id) DO UPDATE SET os = excluded.os, updated_at = excluded.updated_at \
                 WHERE host_os.os <> excluded.os",
                params![host_id, os, crate::store::hosts::now_unix()],
            )?;
            Ok(changed > 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Database;

    #[test]
    fn set_list_and_follow_the_host() {
        let db = Database::open_in_memory().unwrap();
        let host_id = db
            .host_save(&crate::store::tests::sample_host(None, None))
            .unwrap()
            .id;
        assert!(db.host_os_list().unwrap().is_empty());
        assert!(db.host_os_set(host_id, "debian").unwrap());
        assert!(!db.host_os_set(host_id, "debian").unwrap());
        assert!(db.host_os_set(host_id, "ubuntu").unwrap());
        assert_eq!(
            db.host_os_list().unwrap().get(&host_id).map(String::as_str),
            Some("ubuntu")
        );
        db.host_delete(host_id).unwrap();
        assert!(db.host_os_list().unwrap().is_empty());
    }
}
