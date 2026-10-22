//! 认证与主机指纹相关的命令：回答弹窗提问，以及 known_hosts 的维护。

use serde::Deserialize;
use tauri::State;
use tracing::info;
use zssh_core::ssh::HostKeyDecision;

use crate::state::AppState;

/// 前端对指纹弹窗的回答。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyAnswer {
    TrustAndSave,
    TrustOnce,
    Reject,
}

impl From<HostKeyAnswer> for HostKeyDecision {
    fn from(answer: HostKeyAnswer) -> Self {
        match answer {
            HostKeyAnswer::TrustAndSave => HostKeyDecision::TrustAndSave,
            HostKeyAnswer::TrustOnce => HostKeyDecision::TrustOnce,
            HostKeyAnswer::Reject => HostKeyDecision::Reject,
        }
    }
}

/// 回答指纹弹窗。
#[tauri::command]
pub async fn hostkey_answer(
    state: State<'_, AppState>,
    req_id: u64,
    decision: HostKeyAnswer,
) -> Result<(), String> {
    state.prompts.answer_host_key(req_id, decision.into())
}

/// 回答认证弹窗。`answers` 为 `null` 表示取消这次连接。
#[tauri::command]
pub async fn auth_answer(
    state: State<'_, AppState>,
    req_id: u64,
    answers: Option<Vec<String>>,
    remember: Option<bool>,
) -> Result<(), String> {
    state
        .prompts
        .answer_auth(req_id, answers, remember.unwrap_or(false))
}

/// 清掉自己 known_hosts 里这台主机的记录（设计文档 4.4 的"替换指纹"）。
#[tauri::command]
pub async fn hostkey_forget(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> Result<usize, String> {
    let removed = state
        .known_hosts
        .forget(&host, port)
        .map_err(|err| err.to_string())?;
    info!(%host, port, removed, "清除已保存的主机指纹");
    Ok(removed)
}

/// 已保存的指纹（主机设置里显示用）。
#[tauri::command]
pub async fn hostkey_saved(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> Result<Option<String>, String> {
    Ok(state.known_hosts.saved_fingerprint(&host, port))
}
