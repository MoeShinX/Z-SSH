//! 全局设置的命令：设置页读写 settings 表；连接相关的设置写完立刻应用到后端
//! （连接池的空闲时长、keepalive 间隔），另外给"关于"提供数据目录信息。

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, State};
use tracing::info;

use crate::resolve_data_dir;
use crate::state::AppState;

/// 读一个设置项（JSON 文本），没有就返回 `null`。
#[tauri::command]
pub async fn settings_get(
    state: State<'_, AppState>,
    key: String,
) -> Result<Option<String>, String> {
    state.store.setting_get(&key).map_err(|err| err.to_string())
}

/// 写一个设置项。连接相关的项写完立刻生效。
#[tauri::command]
pub async fn settings_set(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    state
        .store
        .setting_set(&key, &value)
        .map_err(|err| err.to_string())?;
    apply_connection_setting(&state, &key, &value);
    Ok(())
}

/// 把连接相关的设置应用到后端。值是 JSON 文本，数字直接解析。
///
/// - `pool_idle_timeout_secs`：空闲多久断开，对新进入空闲的连接生效
/// - `keepalive_interval_secs`：保活间隔，0 表示关闭，对新连接生效
fn apply_connection_setting(state: &AppState, key: &str, value: &str) {
    let Ok(secs) = value.parse::<u64>() else {
        return;
    };
    match key {
        zssh_core::store::settings::POOL_IDLE_TIMEOUT_KEY => {
            let secs = secs.min(24 * 3600);
            state.pool.set_idle_timeout(Duration::from_secs(secs));
            info!(secs, "连接空闲时长已更新");
        }
        zssh_core::store::settings::KEEPALIVE_INTERVAL_KEY => {
            let secs = secs.min(3600);
            zssh_core::ssh::set_keepalive_interval(secs);
            info!(secs, "keepalive 间隔已更新");
        }
        _ => {}
    }
}

/// 启动时把库里存的连接设置读出来应用一次。
pub fn apply_saved_connection_settings(state: &AppState) {
    for key in [
        zssh_core::store::settings::POOL_IDLE_TIMEOUT_KEY,
        zssh_core::store::settings::KEEPALIVE_INTERVAL_KEY,
    ] {
        if let Ok(Some(value)) = state.store.setting_get(key) {
            apply_connection_setting(state, key, &value);
        }
    }
}

/// 数据目录和 known_hosts 的路径（设置页"关于"用）。
#[derive(Debug, Clone, Serialize)]
pub struct DataDirInfo {
    pub data_dir: String,
    pub known_hosts: String,
}

#[tauri::command]
pub async fn data_dir_info(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DataDirInfo, String> {
    let data_dir = resolve_data_dir(&app).map_err(|err| err.to_string())?;
    Ok(DataDirInfo {
        data_dir: data_dir.display().to_string(),
        known_hosts: state.known_hosts.our_file().display().to_string(),
    })
}

/// 用资源管理器打开数据目录。
///
/// 不引 Tauri 的 shell / opener 插件（设计文档第 9 节：capabilities 只开放自己
/// 定义的命令），这里就是 Windows 自带的 explorer。
#[tauri::command]
pub async fn open_data_dir(app: AppHandle) -> Result<(), String> {
    let data_dir = resolve_data_dir(&app).map_err(|err| err.to_string())?;
    std::process::Command::new("explorer")
        .arg(&data_dir)
        .spawn()
        .map_err(|err| format!("打不开资源管理器：{err}"))?;
    Ok(())
}

/// 用系统默认浏览器打开终端里点开的网址。
///
/// 调用链：终端里双击链接 / 右键「打开链接」→ `openExternalUrl` → 这里 →
/// `external_url::open_external_url`（只放行 http/https，拒绝空白和控制字符、超长）→ ShellExecuteW。
#[tauri::command]
pub async fn open_external_url(url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || zssh_core::external_url::open_external_url(&url))
        .await
        .map_err(|err| err.to_string())?
}

/// 字符串设置：库里存的是 JSON（`"..."`），老数据可能是裸字符串，两种都认。
fn string_setting(state: &AppState, key: &str) -> Option<String> {
    let raw = state.store.setting_get(key).ok().flatten()?;
    Some(serde_json::from_str::<String>(&raw).unwrap_or(raw))
}

/// 会话日志开没开（默认关）。
pub fn session_log_enabled(state: &AppState) -> bool {
    state
        .store
        .setting_get("session_log_enabled")
        .ok()
        .flatten()
        .is_some_and(|raw| raw == "true")
}

/// 会话日志实际写到哪个目录：设置里填了就用它，没填用数据目录下的 `session-logs`。
pub fn session_log_dir(app: &AppHandle, state: &AppState) -> Result<std::path::PathBuf, String> {
    match string_setting(state, "session_log_dir") {
        Some(dir) if !dir.trim().is_empty() => Ok(std::path::PathBuf::from(dir.trim())),
        _ => Ok(resolve_data_dir(app)
            .map_err(|err| err.to_string())?
            .join("session-logs")),
    }
}

/// 设置页显示用：会话日志的实际目录。
#[tauri::command]
pub async fn session_log_dir_info(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    Ok(session_log_dir(&app, &state)?.display().to_string())
}

/// 用资源管理器打开会话日志目录（还没有就先建出来）。
#[tauri::command]
pub async fn open_session_log_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let dir = session_log_dir(&app, &state)?;
    std::fs::create_dir_all(&dir).map_err(|err| format!("建不了目录：{err}"))?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|err| format!("打不开资源管理器：{err}"))?;
    Ok(())
}

/// 默认代理的 id（`null` 表示没设）。
#[tauri::command]
pub async fn default_proxy_get(state: State<'_, AppState>) -> Result<Option<i64>, String> {
    state
        .store
        .default_proxy_id()
        .map_err(|err| err.to_string())
}

/// 设置默认代理。
#[tauri::command]
pub async fn default_proxy_set(
    state: State<'_, AppState>,
    proxy_id: Option<i64>,
) -> Result<(), String> {
    // 设置之前先确认这个代理真的在
    if let Some(id) = proxy_id {
        if state
            .store
            .proxy_list()
            .map_err(|err| err.to_string())?
            .iter()
            .all(|proxy| proxy.id != id)
        {
            return Err(format!("没有这个代理：{id}"));
        }
    }
    state
        .store
        .set_default_proxy_id(proxy_id)
        .map_err(|err| err.to_string())
}

/// 读当前的 Windows 系统代理，给设置页"软件网络 → 跟随系统代理"显示用。
#[tauri::command]
pub async fn app_network_system_proxy() -> Result<zssh_core::sync::app_proxy::SystemProxy, String> {
    Ok(zssh_core::sync::app_proxy::read_system_proxy())
}

/// 把一个 SFTP 标签交给文件窗口（没开就建一个）。`sftp_id` 为空时文件窗口自己去连。
#[tauri::command]
pub async fn file_window_adopt(
    app: AppHandle,
    state: tauri::State<'_, crate::file_window::FileWindowState>,
    tab: crate::file_window::FileTab,
) -> Result<(), String> {
    state.push(tab);
    crate::file_window::ensure_open(&app)?;
    // 窗口已经在跑的话它在等这个事件；刚建的窗口启动时自己会来取。
    let _ = tauri::Emitter::emit_to(
        &app,
        crate::file_window::LABEL,
        crate::file_window::PENDING_EVENT,
        (),
    );
    Ok(())
}

/// 文件窗口取走待接管的标签。
#[tauri::command]
pub async fn file_window_take(
    state: tauri::State<'_, crate::file_window::FileWindowState>,
) -> Result<Vec<crate::file_window::FileTab>, String> {
    Ok(state.take())
}

/// 文件窗口报告自己现在有哪些 SFTP 会话（窗口意外销毁时据此兜底关掉）。
#[tauri::command]
pub async fn file_window_owned(
    state: tauri::State<'_, crate::file_window::FileWindowState>,
    sftp_ids: Vec<String>,
) -> Result<(), String> {
    state.set_owned(sftp_ids);
    Ok(())
}

/// 文件窗口开着没有（主窗口据此决定新开的 SFTP 去哪）。
#[tauri::command]
pub async fn file_window_is_open(app: AppHandle) -> Result<bool, String> {
    Ok(crate::file_window::is_open(&app))
}
