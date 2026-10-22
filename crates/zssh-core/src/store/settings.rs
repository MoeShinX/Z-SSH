//! 全局设置（`settings` 表，值是 JSON 文本）。

use rusqlite::{params, OptionalExtension};

use crate::error::ZsshError;
use crate::store::db::Database;

/// 连接池空闲多久断开（秒）。
pub const POOL_IDLE_TIMEOUT_KEY: &str = "pool_idle_timeout_secs";

/// keepalive 间隔（秒，0 = 关闭）。
pub const KEEPALIVE_INTERVAL_KEY: &str = "keepalive_interval_secs";

/// 全局默认代理的 key。值是一个 JSON 数字（代理 id）或者 `null`。
pub const DEFAULT_PROXY_KEY: &str = "default_proxy_id";

/// AI 工具桥执行模式的 key。值是 JSON 字符串：`"manual"` 或 `"auto"`。
pub const AI_EXEC_MODE_KEY: &str = "ai_exec_mode";

/// 手动指定的 opencode 路径的 key。值是 JSON 字符串（空串表示自动发现）。
pub const AI_OPENCODE_PATH_KEY: &str = "ai_opencode_path";

/// 手动指定的 codex 路径的 key（AI-3）。值是 JSON 字符串（空串表示自动发现）。
pub const AI_CODEX_PATH_KEY: &str = "ai_codex_path";

/// Claude Code 手动指定的路径的 key（AI-4）。值是 JSON 字符串（空串表示自动发现）。
pub const AI_CLAUDE_PATH_KEY: &str = "ai_claude_path";

/// 默认智能体的 key（AI-3）。值是 JSON 字符串：`"opencode"` / `"codex"` / `"claude"`。
pub const AI_DEFAULT_AGENT_KEY: &str = "ai_default_agent";

/// `write_file` 覆盖时每个文件保留几份 `.zssh-bak-*` 备份（M3-4 第 0 节）。
pub const AI_WRITE_BACKUP_KEEP_KEY: &str = "ai_write_backup_keep";

/// 备份份数的默认值（用户 2026-10-18 决定）。
pub const AI_WRITE_BACKUP_KEEP_DEFAULT: usize = 5;
/// 备份份数的上下限：0 等于不留备份，和"备份失败就不写"的约定冲突，所以最小 1。
pub const AI_WRITE_BACKUP_KEEP_MIN: usize = 1;
pub const AI_WRITE_BACKUP_KEEP_MAX: usize = 100;

/// 本机的设备 ID（同步平局规则用）。值是一个 UUID 字符串，**不同步**。
pub const SYNC_DEVICE_ID_KEY: &str = "sync_device_id";

/// 本地自动备份最多保留几份。
pub const SYNC_BACKUP_KEEP_KEY: &str = "sync_backup_keep";

/// 上次自动备份时的应用版本（升级后第一次启动要备份）。
pub const SYNC_LAST_BACKUP_VERSION_KEY: &str = "sync_last_backup_version";

pub const SYNC_BACKUP_KEEP_DEFAULT: usize = 20;
pub const SYNC_BACKUP_KEEP_MAX: usize = 200;

impl Database {
    /// 读一个设置项，没有就是 `None`。
    pub fn setting_get(&self, key: &str) -> Result<Option<String>, ZsshError> {
        self.with_conn(|conn| {
            let value: Option<String> = conn
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(value)
        })
    }

    /// 写一个设置项。
    pub fn setting_set(&self, key: &str, value: &str) -> Result<(), ZsshError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })?;
        // 只有会进同步包的设置才算“数据变了”（面板宽度之类的拖一下不该触发同步）。
        if crate::sync::settings::is_synced(key) {
            self.notify_changed();
        }
        Ok(())
    }

    /// 默认代理的 id（没设置或者存的是 null 就是 `None`）。
    pub fn default_proxy_id(&self) -> Result<Option<i64>, ZsshError> {
        match self.setting_get(DEFAULT_PROXY_KEY)? {
            Some(raw) => Ok(serde_json::from_str::<Option<i64>>(&raw).unwrap_or(None)),
            None => Ok(None),
        }
    }

    /// 设置默认代理（`None` 表示不用默认代理）。
    pub fn set_default_proxy_id(&self, id: Option<i64>) -> Result<(), ZsshError> {
        let value = serde_json::to_string(&id)
            .map_err(|err| ZsshError::InvalidInput(format!("默认代理序列化失败：{err}")))?;
        self.setting_set(DEFAULT_PROXY_KEY, &value)
    }

    /// AI 执行模式（`"manual"` 或 `"auto"`）；没设置或值不认识按 `"manual"`。
    pub fn ai_exec_mode(&self) -> Result<String, ZsshError> {
        match self.setting_get(AI_EXEC_MODE_KEY)? {
            Some(raw) => {
                Ok(serde_json::from_str::<String>(&raw).unwrap_or_else(|_| "manual".into()))
            }
            None => Ok("manual".into()),
        }
    }

    /// 设置 AI 执行模式。
    pub fn set_ai_exec_mode(&self, mode: &str) -> Result<(), ZsshError> {
        let value = serde_json::to_string(mode)
            .map_err(|err| ZsshError::InvalidInput(format!("AI 执行模式序列化失败：{err}")))?;
        self.setting_set(AI_EXEC_MODE_KEY, &value)
    }

    /// 手动指定的 opencode 路径（JSON 字符串）；空串或没设置表示自动发现。
    pub fn ai_opencode_path(&self) -> Result<Option<String>, ZsshError> {
        match self.setting_get(AI_OPENCODE_PATH_KEY)? {
            Some(raw) => {
                let path = serde_json::from_str::<String>(&raw).unwrap_or_default();
                if path.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(path))
                }
            }
            None => Ok(None),
        }
    }

    /// 手动指定的 codex 路径（JSON 字符串）；空串或没设置表示自动发现。
    pub fn ai_codex_path(&self) -> Result<Option<String>, ZsshError> {
        match self.setting_get(AI_CODEX_PATH_KEY)? {
            Some(raw) => {
                let path = serde_json::from_str::<String>(&raw).unwrap_or_default();
                if path.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(path))
                }
            }
            None => Ok(None),
        }
    }

    /// 手动指定的 Claude Code 路径（JSON 字符串）；空串或没设置表示自动发现。
    pub fn ai_claude_path(&self) -> Result<Option<String>, ZsshError> {
        match self.setting_get(AI_CLAUDE_PATH_KEY)? {
            Some(raw) => {
                let path = serde_json::from_str::<String>(&raw).unwrap_or_default();
                if path.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(path))
                }
            }
            None => Ok(None),
        }
    }

    /// 默认智能体（`"opencode"` / `"codex"` / `"claude"`）；没设置或值不认识按 `"opencode"`。
    pub fn ai_default_agent(&self) -> Result<String, ZsshError> {
        match self.setting_get(AI_DEFAULT_AGENT_KEY)? {
            Some(raw) => match serde_json::from_str::<String>(&raw) {
                Ok(agent) if matches!(agent.as_str(), "codex" | "claude") => Ok(agent),
                _ => Ok("opencode".into()),
            },
            None => Ok("opencode".into()),
        }
    }

    /// 设置默认智能体（AI-3 界面用）。
    pub fn set_ai_default_agent(&self, agent: &str) -> Result<(), ZsshError> {
        let value = serde_json::to_string(agent)
            .map_err(|err| ZsshError::InvalidInput(format!("默认智能体序列化失败：{err}")))?;
        self.setting_set(AI_DEFAULT_AGENT_KEY, &value)
    }

    /// `write_file` 每个文件保留几份备份；没设置或值不合法用默认值。
    pub fn ai_write_backup_keep(&self) -> Result<usize, ZsshError> {
        let keep = match self.setting_get(AI_WRITE_BACKUP_KEEP_KEY)? {
            Some(raw) => serde_json::from_str::<u64>(&raw).ok(),
            None => None,
        };
        Ok(keep
            .map(|value| (value as usize).clamp(AI_WRITE_BACKUP_KEEP_MIN, AI_WRITE_BACKUP_KEEP_MAX))
            .unwrap_or(AI_WRITE_BACKUP_KEEP_DEFAULT))
    }

    /// 本机的同步设备 ID；第一次调用时生成并保存。
    ///
    /// 存在 `settings` 表里，但**不在同步白名单**里：每台电脑一个，跟着包走会让
    /// 平局规则失效。
    pub fn sync_device_id(&self) -> Result<String, ZsshError> {
        self.with_conn(|conn| {
            let existing: Option<String> = conn
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [SYNC_DEVICE_ID_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(raw) = existing {
                if let Ok(Some(id)) = serde_json::from_str::<Option<String>>(&raw) {
                    if !id.trim().is_empty() {
                        return Ok(id);
                    }
                }
            }
            let id = crate::store::new_uid();
            let value = serde_json::to_string(&id)
                .map_err(|err| ZsshError::InvalidInput(format!("设备 ID 序列化失败：{err}")))?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![SYNC_DEVICE_ID_KEY, value],
            )?;
            Ok(id)
        })
    }

    /// 本地自动备份最多保留几份；没设置或值不合法用默认 20。
    pub fn sync_backup_keep(&self) -> Result<usize, ZsshError> {
        let keep = match self.setting_get(SYNC_BACKUP_KEEP_KEY)? {
            Some(raw) => serde_json::from_str::<u64>(&raw).ok(),
            None => None,
        };
        Ok(keep
            .map(|value| (value as usize).clamp(1, SYNC_BACKUP_KEEP_MAX))
            .unwrap_or(SYNC_BACKUP_KEEP_DEFAULT))
    }
}
