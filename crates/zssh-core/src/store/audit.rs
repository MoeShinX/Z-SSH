//! AI 工具桥的审计（`ai_audit` 表）。
//!
//! 只记元数据：时间、scope、主机显示名、命令、危险等级、审批结果、退出码、耗时。
//! **不记命令输出**，输出可能含敏感信息（卡片 AI-1 第 5 节）。

use rusqlite::params;

use crate::error::ZsshError;
use crate::store::db::Database;

/// 写入一条审计所需的字段（不含自增 id）。
#[derive(Debug, Clone)]
pub struct NewAiAudit<'a> {
    pub at: i64,
    pub scope: &'a str,
    pub host_label: &'a str,
    pub command: &'a str,
    pub danger: &'a str,
    pub decision: &'a str,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 执行位置：`terminal` / `background`（AI-2c）。
    pub location: Option<&'a str>,
    /// 降级到后台的原因（AI-2c）。
    pub fallback_reason: Option<&'a str>,
    /// 工具名（M3-4）：`run_command` / `list_dir` / `read_file` / `write_file` / `stat`。
    pub tool: &'a str,
    /// 文件工具操作的路径；`run_command` 为 `None`。
    pub path: Option<&'a str>,
    /// 读 = 返回的字节数，写 = 写入的字节数。只记数量，不记内容。
    pub bytes: Option<i64>,
    /// `write_file` 覆盖时的备份路径。
    pub backup_path: Option<&'a str>,
}

/// 审计表里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiAuditRow {
    pub id: i64,
    pub at: i64,
    pub scope: String,
    pub host_label: String,
    pub command: String,
    pub danger: String,
    pub decision: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 执行位置：`terminal` / `background`（AI-2c）。
    pub location: Option<String>,
    /// 降级到后台的原因（AI-2c）。
    pub fallback_reason: Option<String>,
    pub tool: String,
    pub path: Option<String>,
    pub bytes: Option<i64>,
    pub backup_path: Option<String>,
}

impl Database {
    /// 写一条审计，返回新行的 id。
    pub fn ai_audit_insert(&self, entry: NewAiAudit<'_>) -> Result<i64, ZsshError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO ai_audit \
                 (at, scope, host_label, command, danger, decision, exit_code, duration_ms, \
                  location, fallback_reason, tool, path, bytes, backup_path) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    entry.at,
                    entry.scope,
                    entry.host_label,
                    entry.command,
                    entry.danger,
                    entry.decision,
                    entry.exit_code,
                    entry.duration_ms,
                    entry.location,
                    entry.fallback_reason,
                    entry.tool,
                    entry.path,
                    entry.bytes,
                    entry.backup_path
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    /// 最近的审计，按 id 倒序（最新在前）。`scope` 给 `Some` 时只看该 scope。
    pub fn ai_audit_list(
        &self,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AiAuditRow>, ZsshError> {
        self.with_conn(|conn| {
            let mut rows = Vec::new();
            if let Some(scope) = scope {
                let mut statement = conn.prepare(
                    "SELECT id, at, scope, host_label, command, danger, decision, exit_code, \
                     duration_ms, location, fallback_reason, tool, path, bytes, backup_path FROM ai_audit \
                     WHERE scope = ?1 ORDER BY id DESC LIMIT ?2",
                )?;
                let iter = statement.query_map(params![scope, limit as i64], row_to_audit)?;
                for row in iter {
                    rows.push(row?);
                }
            } else {
                let mut statement = conn.prepare(
                    "SELECT id, at, scope, host_label, command, danger, decision, exit_code, \
                     duration_ms, location, fallback_reason, tool, path, bytes, backup_path FROM ai_audit \
                     ORDER BY id DESC LIMIT ?1",
                )?;
                let iter = statement.query_map(params![limit as i64], row_to_audit)?;
                for row in iter {
                    rows.push(row?);
                }
            }
            Ok(rows)
        })
    }
}

fn row_to_audit(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiAuditRow> {
    Ok(AiAuditRow {
        id: row.get(0)?,
        at: row.get(1)?,
        scope: row.get(2)?,
        host_label: row.get(3)?,
        command: row.get(4)?,
        danger: row.get(5)?,
        decision: row.get(6)?,
        exit_code: row.get(7)?,
        duration_ms: row.get(8)?,
        location: row.get(9)?,
        fallback_reason: row.get(10)?,
        tool: row.get(11)?,
        path: row.get(12)?,
        bytes: row.get(13)?,
        backup_path: row.get(14)?,
    })
}
