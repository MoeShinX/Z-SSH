//! 自动更新的命令：只做转接，逻辑在 `crate::updater` 和 `zssh_core::updater`。

use serde::Serialize;
use tauri::{AppHandle, State};
use zssh_core::updater::{self, RestartBusy, UpdatePrefs};

use crate::state::AppState;
use crate::updater::{UpdateController, UpdateStatus};

#[derive(Debug, Serialize)]
pub struct UpdateView {
    pub status: UpdateStatus,
    pub prefs: UpdatePrefs,
    pub current_version: String,
    /// 免安装版：有新版本时界面只提示去发布页手动下载。
    pub portable: bool,
    /// 当前状态是自动流程产生的（自动检查 / 自动下载）：底部不提示。
    pub auto: bool,
}

fn view(app: &AppHandle, state: &AppState, update: &UpdateController) -> UpdateView {
    UpdateView {
        status: update.status(),
        prefs: updater::load_prefs(&state.store).unwrap_or_default(),
        current_version: app.package_info().version.to_string(),
        portable: update.portable(),
        auto: update.is_auto(),
    }
}

#[tauri::command]
pub async fn update_status(
    app: AppHandle,
    state: State<'_, AppState>,
    update: State<'_, UpdateController>,
) -> Result<UpdateView, String> {
    Ok(view(&app, &state, &update))
}

/// 手动检查（关于页的按钮）。
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    state: State<'_, AppState>,
    update: State<'_, UpdateController>,
) -> Result<UpdateView, String> {
    update.check(&app, true).await;
    Ok(view(&app, &state, &update))
}

#[tauri::command]
pub async fn update_download(
    app: AppHandle,
    state: State<'_, AppState>,
    update: State<'_, UpdateController>,
) -> Result<UpdateView, String> {
    update.download(&app).await;
    Ok(view(&app, &state, &update))
}

/// 后端知道的“重启会打断的事”（终端里跑着的 AI 命令）。传输和编辑器里没保存的修改在界面里，
/// 由界面合并后再决定要不要先问用户。
#[tauri::command]
pub async fn update_busy(app: AppHandle) -> Result<RestartBusy, String> {
    Ok(UpdateController::busy_from_backend(&app))
}

/// 安装并重启。界面必须已经做过重启前检查。
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    update: State<'_, UpdateController>,
) -> Result<(), String> {
    update.install(&app).await
}

#[tauri::command]
pub async fn update_skip(
    app: AppHandle,
    state: State<'_, AppState>,
    update: State<'_, UpdateController>,
    version: String,
) -> Result<UpdateView, String> {
    update.skip(&app, &version)?;
    Ok(view(&app, &state, &update))
}

#[tauri::command]
pub async fn update_set_auto(
    app: AppHandle,
    state: State<'_, AppState>,
    update: State<'_, UpdateController>,
    enabled: bool,
) -> Result<UpdateView, String> {
    updater::set_auto_check(&state.store, enabled).map_err(|err| err.to_string())?;
    Ok(view(&app, &state, &update))
}

/// 用默认浏览器打开新版本的发布页（免安装版手动下载用）。地址只由配置的更新地址推出来，
/// 不接受界面传入，免得被拿来打开任意地址。
#[tauri::command]
pub async fn update_open_download_page(
    app: AppHandle,
    update: State<'_, UpdateController>,
) -> Result<(), String> {
    let url = update
        .download_page(&app)
        .ok_or_else(|| "推不出发布页地址，请到 GitHub 的 Releases 页面手动下载".to_owned())?;
    std::process::Command::new("explorer")
        .arg(&url)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("打开浏览器失败：{err}（地址：{url}）"))
}
