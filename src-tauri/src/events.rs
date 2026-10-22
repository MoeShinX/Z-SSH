//! 发给前端的事件（`session_state` 以及带在它里面的跳数进度）。

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::debug;
use zssh_core::ssh::HopProgress;

pub const SESSION_STATE_EVENT: &str = "session_state";

/// 服务器状态（M1-10）：每个订阅者一条，`subscription_id` 认领。
pub const APP_EVENT_STATS: &str = "server_stats";

/// 拨号链的进度，前端拿它显示"连接中：第 2/3 跳 jump1"。
#[derive(Debug, Clone, Serialize)]
pub struct HopInfo {
    pub index: usize,
    pub total: usize,
    pub label: String,
}

/// 会话状态事件。
#[derive(Debug, Clone, Serialize)]
pub struct SessionState {
    pub session_id: String,
    /// connecting / connected / disconnected
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 只有 connecting 阶段有
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hop: Option<HopInfo>,
}

pub fn emit_state(app: &AppHandle, session_id: &str, state: &'static str, reason: Option<String>) {
    emit(app, session_id, state, reason, None);
}

pub fn emit_hop(app: &AppHandle, session_id: &str, progress: &HopProgress) {
    emit(
        app,
        session_id,
        "connecting",
        None,
        Some(HopInfo {
            index: progress.index,
            total: progress.total,
            label: progress.label.clone(),
        }),
    );
}

fn emit(
    app: &AppHandle,
    session_id: &str,
    state: &'static str,
    reason: Option<String>,
    hop: Option<HopInfo>,
) {
    let payload = SessionState {
        session_id: session_id.to_owned(),
        state,
        reason,
        hop,
    };
    if let Err(err) = app.emit(SESSION_STATE_EVENT, payload) {
        debug!(%session_id, %err, "发送会话状态事件失败");
    }
}
