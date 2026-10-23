//! 主机和分组在树里的手动顺序（UI-9）。
//!
//! 用的是 `hosts` / `groups` 两张表本来就有的 `sort` 列，不需要迁移。规则：
//! - 「同一层」= 同一个分组下直接挂的主机（未分组算一层）、同一个父分组下的子分组；
//! - `sort > 0` 的按 `sort` 从小到大排前面，`sort == 0` 的排后面、再按名字排（排序在前端做）；
//! - 调整顺序时前端把**整层**按新顺序一次传进来，这里依次设成 1..n；
//! - 每条改了 `sort` 的记录同时更新 `updated_at`，同步按记录的更新时间合并，不更新就传不到别的设备。
//!   `sort` 本来就等于目标值的记录不动（不白白产生同步改动）。

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension};

use crate::error::ZsshError;
use crate::store::db::Database;

/// 检查没有重复的 id。
fn ensure_unique(ids: &[i64]) -> Result<(), ZsshError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(*id) {
            return Err(ZsshError::InvalidInput(format!("顺序里的 id {id} 重复了")));
        }
    }
    Ok(())
}

impl Database {
    /// 把这些主机依次排成 1..n。它们必须都在同一个分组下（或都未分组）。
    ///
    /// 在一个事务里做：任何一条不满足（重复、不存在、不在同一层）就整个不改。
    pub fn host_reorder(&self, ids: &[i64]) -> Result<(), ZsshError> {
        ensure_unique(ids)?;
        if ids.is_empty() {
            return Ok(());
        }
        self.with_conn_write(|conn| {
            let tx = conn.transaction()?;
            let now = crate::store::hosts::now_unix();
            let mut layer: Option<Option<i64>> = None;
            for (index, id) in ids.iter().enumerate() {
                let row: Option<(Option<i64>, i64)> = tx
                    .query_row(
                        "SELECT group_id, sort FROM hosts WHERE id = ?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((group_id, sort)) = row else {
                    return Err(ZsshError::NotFound {
                        entity: "主机",
                        id: *id,
                    });
                };
                match layer {
                    None => layer = Some(group_id),
                    Some(expected) if expected != group_id => {
                        return Err(ZsshError::InvalidInput(
                            "只能调整同一个分组里的主机顺序".to_owned(),
                        ));
                    }
                    Some(_) => {}
                }
                let wanted = index as i64 + 1;
                if sort != wanted {
                    tx.execute(
                        "UPDATE hosts SET sort = ?1, updated_at = ?2 WHERE id = ?3",
                        params![wanted, now, id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 把这些分组依次排成 1..n。它们必须都在同一个父分组下（或都是根分组）。事务规则同上。
    pub fn group_reorder(&self, ids: &[i64]) -> Result<(), ZsshError> {
        ensure_unique(ids)?;
        if ids.is_empty() {
            return Ok(());
        }
        self.with_conn_write(|conn| {
            let tx = conn.transaction()?;
            let now = crate::store::hosts::now_unix();
            let mut layer: Option<Option<i64>> = None;
            for (index, id) in ids.iter().enumerate() {
                let row: Option<(Option<i64>, i64)> = tx
                    .query_row(
                        "SELECT parent_id, sort FROM groups WHERE id = ?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((parent_id, sort)) = row else {
                    return Err(ZsshError::NotFound {
                        entity: "分组",
                        id: *id,
                    });
                };
                match layer {
                    None => layer = Some(parent_id),
                    Some(expected) if expected != parent_id => {
                        return Err(ZsshError::InvalidInput(
                            "只能调整同一个父分组下的分组顺序".to_owned(),
                        ));
                    }
                    Some(_) => {}
                }
                let wanted = index as i64 + 1;
                if sort != wanted {
                    tx.execute(
                        "UPDATE groups SET sort = ?1, updated_at = ?2 WHERE id = ?3",
                        params![wanted, now, id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 「按名字重新排序」：把这个分组下直接挂的主机、直接的子分组的 `sort` 全置 0。
    /// `group_id` 是 `None` 时只管未分组的主机（根分组那一层不重置）。
    pub fn order_reset(&self, group_id: Option<i64>) -> Result<(), ZsshError> {
        self.with_conn_write(|conn| {
            let tx = conn.transaction()?;
            let now = crate::store::hosts::now_unix();
            if let Some(group_id) = group_id {
                crate::store::groups::ensure_group_exists(&tx, group_id)?;
                tx.execute(
                    "UPDATE groups SET sort = 0, updated_at = ?1 WHERE parent_id = ?2 AND sort != 0",
                    params![now, group_id],
                )?;
            }
            tx.execute(
                "UPDATE hosts SET sort = 0, updated_at = ?1 WHERE group_id IS ?2 AND sort != 0",
                params![now, group_id],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
}
