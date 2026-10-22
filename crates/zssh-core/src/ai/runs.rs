//! 正在终端里跑着的 AI 命令的登记表（AI-2d 1.4）。
//!
//! 停止按钮要区分两种情况：这个对话有一条 AI 命令正在某个终端里跑（那要发
//! Ctrl+C 把它停下来），还是用户自己正在那个终端里跑东西（绝对不能乱发，
//! 会打断用户的程序）。所以每个对话在往终端里写包装脚本**之前**登记一条记录，
//! 命令结束（拿到结束标记、超时返回之后又结束、会话断开）时清掉。

use std::collections::HashMap;
use std::sync::Mutex;

/// 一条正在终端里执行的 AI 命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRun {
    /// 目标会话（真实会话 ID，不是 `host-<id>`）。
    pub session_id: String,
    /// 这次执行的随机结束标记：用来防止清掉这个对话后来那条命令的记录。
    pub marker: String,
}

/// 对话（scope）→ 正在跑的命令。一次只记一条：同一个终端上不会叠两条 AI 命令
/// （由 [`TerminalState::try_acquire`](crate::ai::pty::TerminalState::try_acquire)
/// 保证）。
#[derive(Debug, Default)]
pub struct TerminalRuns {
    runs: Mutex<HashMap<String, TerminalRun>>,
}

impl TerminalRuns {
    /// 记下"这个对话在这个会话里跑着这条命令"。
    pub fn register(&self, scope_id: &str, session_id: &str, marker: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.insert(
                scope_id.to_string(),
                TerminalRun {
                    session_id: session_id.to_string(),
                    marker: marker.to_string(),
                },
            );
        }
    }

    /// 命令结束时清掉这条记录。
    ///
    /// `marker` 对不上就不动：表里已经换成这个对话后来的命令了，不能把它的
    /// 记录误删（否则停止按钮就找不到正在跑的那条命令）。
    pub fn clear(&self, scope_id: &str, marker: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            if runs.get(scope_id).is_some_and(|run| run.marker == marker) {
                runs.remove(scope_id);
            }
        }
    }

    /// 会话断开时的兜底清理（不知道当时是哪条命令）。
    pub fn clear_session(&self, session_id: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.retain(|_, run| run.session_id != session_id);
        }
    }

    /// 现在一共有几条 AI 命令在终端里跑（自动更新重启前的检查用）。
    pub fn count(&self) -> usize {
        self.runs.lock().map(|runs| runs.len()).unwrap_or_default()
    }

    /// 这个对话现在有没有 AI 命令在终端里跑。
    pub fn running(&self, scope_id: &str) -> Option<TerminalRun> {
        self.runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(scope_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_clear_by_marker() {
        let runs = TerminalRuns::default();
        assert!(runs.running("s1").is_none(), "一开始没有记录");
        runs.register("s1", "sess-1", "__M1__");
        assert_eq!(
            runs.running("s1"),
            Some(TerminalRun {
                session_id: "sess-1".to_string(),
                marker: "__M1__".to_string(),
            })
        );
        // 标记对得上才清。
        runs.clear("s1", "__OTHER__");
        assert!(runs.running("s1").is_some(), "标记不对不能清");
        runs.clear("s1", "__M1__");
        assert!(runs.running("s1").is_none(), "标记对上就清掉");
    }

    #[test]
    fn a_stale_clear_does_not_remove_a_newer_command() {
        // 超时那条命令的迟到清理不能把同一个对话后来的命令删掉。
        let runs = TerminalRuns::default();
        runs.register("s1", "sess-1", "__OLD__");
        runs.register("s1", "sess-1", "__NEW__");
        runs.clear("s1", "__OLD__");
        assert_eq!(
            runs.running("s1").map(|run| run.marker),
            Some("__NEW__".to_string()),
            "新命令的记录要在"
        );
    }

    #[test]
    fn scopes_are_independent() {
        let runs = TerminalRuns::default();
        runs.register("s1", "sess-1", "__M1__");
        runs.register("s2", "sess-2", "__M2__");
        runs.clear("s1", "__M1__");
        assert!(runs.running("s1").is_none());
        assert!(runs.running("s2").is_some(), "别的对话不受影响");
    }

    #[test]
    fn clear_session_drops_only_that_session() {
        let runs = TerminalRuns::default();
        runs.register("s1", "sess-1", "__M1__");
        runs.register("s2", "sess-2", "__M2__");
        runs.clear_session("sess-1");
        assert!(runs.running("s1").is_none(), "断开会话的记录清掉");
        assert!(runs.running("s2").is_some(), "其他会话不受影响");
    }
}
