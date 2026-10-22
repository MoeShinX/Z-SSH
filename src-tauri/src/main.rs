// Windows 上发布版不带控制台窗口，调试版保留，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod file_window;
mod interact;
mod native_menu;
mod state;
mod sync_controller;
mod tray;
mod updater;
mod window_geometry;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use state::AppState;
use tauri::Manager;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use zssh_core::ai::bridge::Bridge;
use zssh_core::ssh::{KnownHosts, PoolConfig};
use zssh_core::sync::engine::SyncEngine;
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::Database;

/// 数据目录名。默认数据在 `%APPDATA%\Z-ssh`（设计文档 6.1）。
///
/// 产品名已改为 `Z-SSH`，但这个目录名**故意不动**：Windows 路径不区分大小写，
/// 改名只是外观区别，反而可能让已有用户的数据库找不到（R-1 卡第 1.2 节）。
const APP_DIR: &str = "Z-ssh";
const DB_FILE: &str = "data.db";
/// 覆盖默认数据目录的环境变量：自动化脚本用它把数据和用户的真实数据隔开。
const DATA_DIR_ENV: &str = "ZSSH_DATA_DIR";
/// 窗口状态文件名（落在数据目录里，UI-1 第 1 节）。
const WINDOW_STATE_FILE: &str = ".window-state.json";

/// 窗口状态文件的位置：数据目录里（`ZSSH_DATA_DIR` 优先；没设就是默认数据目录
/// `%APPDATA%\Z-ssh`）。故意在 Tauri 起来之前算——插件的文件名要在 builder 阶段定。
fn window_state_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join(WINDOW_STATE_FILE));
    }
    // 默认数据目录 = `app.path().data_dir()`（Windows 上是 `%APPDATA%`）+ `Z-ssh`，
    // 和 [`resolve_data_dir`] 同一规则；这里没有 AppHandle，直接按规则拼。
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(|dir| PathBuf::from(dir).join(APP_DIR).join(WINDOW_STATE_FILE))
}

/// 窗口状态记哪些：大小 / 位置 / 最大化。插件和下面的主动保存共用。
const WINDOW_STATE_FLAGS: tauri_plugin_window_state::StateFlags =
    tauri_plugin_window_state::StateFlags::SIZE
        .union(tauri_plugin_window_state::StateFlags::POSITION)
        .union(tauri_plugin_window_state::StateFlags::MAXIMIZED);
/// 拖动 / 调整大小停下来多久后写盘。
const WINDOW_STATE_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);

/// 官方窗口状态插件（UI-1 第 1 节）：记大小 / 位置 / 最大化。
fn window_state_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_window_state::Builder;
    let mut builder = Builder::default().with_state_flags(WINDOW_STATE_FLAGS);
    if let Some(path) = window_state_path() {
        // 插件把文件名 `join` 到 `app_config_dir`；给绝对路径时 `join` 整体替换，
        // 文件就落在数据目录里了（ZSSH_DATA_DIR 因此天然生效）。
        builder = builder.with_filename(path.to_string_lossy().into_owned());
    }
    builder.build()
}

/// 把所有窗口当前的大小 / 位置写进状态文件。
///
/// 插件只在 `RunEvent::Exit` 时写盘；不经过正常退出的结束（点「立即重启安装」后安装程序
/// 直接结束进程、关机注销、崩溃、被任务管理器结束）都会丢掉这次运行里调过的大小。
pub(crate) fn save_window_state(app: &tauri::AppHandle) {
    use tauri_plugin_window_state::AppHandleExt;
    if let Err(err) = app.save_window_state(WINDOW_STATE_FLAGS) {
        warn!(%err, "保存窗口大小和位置失败");
    }
}

/// 拖动 / 调整大小停下来 [`WINDOW_STATE_SAVE_DELAY`] 后写一次盘（拖动过程中事件很密，不每次都写）。
fn schedule_window_state_save(app: &tauri::AppHandle) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(WINDOW_STATE_SAVE_DELAY).await;
        if GENERATION.load(Ordering::SeqCst) == generation {
            save_window_state(&app);
        }
    });
}

/// 从状态文件里读主窗口保存的位置和尺寸（物理像素；`maximized` 的不算）。
fn saved_window_rect(path: &std::path::Path) -> Option<window_geometry::Rect> {
    let bytes = fs::read(path).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    let main = value.get("main")?;
    if main.get("maximized").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    Some(window_geometry::Rect {
        x: main.get("x")?.as_i64()? as i32,
        y: main.get("y")?.as_i64()? as i32,
        width: main.get("width")?.as_u64()? as u32,
        height: main.get("height")?.as_u64()? as u32,
    })
}

/// 状态文件里有没有主窗口的记录（判断"以前保存过"）。
fn saved_state_has_main(path: &std::path::Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    value.get("main").is_some()
}

/// 窗口几何的收尾（UI-1 第 1 节）。插件已经在窗口就绪时恢复了保存的状态，
/// 这里补两件插件不管的事：
///
/// 1. 没有保存过状态（第一次启动 / 状态文件被删）→ 默认尺寸：1610×946 适配工作区
///    90%、居中；最小尺寸 900×600（工作区更小就按 90% 放宽）；
/// 2. 保存的位置不在任何屏幕上（副屏拔了）→ 回主屏幕居中；保存的尺寸比当前工作区
///    还大 → 先缩进工作区。
fn apply_initial_window_geometry(app: &tauri::App) {
    use tauri::{LogicalPosition, LogicalSize};
    use window_geometry::Rect;

    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    let Ok(Some(primary)) = window.primary_monitor() else {
        return;
    };
    let scale = primary.scale_factor().max(0.1);
    let to_logical = |x: i32, y: i32, width: u32, height: u32| Rect {
        x: (x as f64 / scale).round() as i32,
        y: (y as f64 / scale).round() as i32,
        width: (width as f64 / scale).round() as u32,
        height: (height as f64 / scale).round() as u32,
    };
    let work = primary.work_area();
    let work_logical = to_logical(
        work.position.x,
        work.position.y,
        work.size.width,
        work.size.height,
    );

    // 最小尺寸永远按当前工作区放宽（配置文件里写死的 900×600 在小屏上会拖不动）。
    let (min_width, min_height) =
        window_geometry::fit_min_size(work_logical.width, work_logical.height);
    let _ = window.set_min_size(Some(LogicalSize::new(min_width, min_height)));

    // 最大化 / 全屏：位置尺寸交给窗口管理器，别去动。
    if window.is_maximized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false) {
        return;
    }

    let saved = window_state_path();
    if !saved.as_deref().is_some_and(saved_state_has_main) {
        let geometry = window_geometry::default_geometry(work_logical);
        let _ = window.set_size(LogicalSize::new(geometry.width, geometry.height));
        let _ = window.set_position(LogicalPosition::new(geometry.x, geometry.y));
        info!(
            width = geometry.width,
            height = geometry.height,
            x = geometry.x,
            y = geometry.y,
            "窗口没有保存过状态，用默认尺寸（适配工作区并居中）"
        );
        return;
    }

    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let logical_width = (size.width as f64 / scale).round() as u32;
    let logical_height = (size.height as f64 / scale).round() as u32;
    let (fit_width, fit_height) =
        window_geometry::clamp_size_to_work_area(logical_width, logical_height, work_logical);
    if (fit_width, fit_height) != (logical_width, logical_height) {
        let _ = window.set_size(LogicalSize::new(fit_width, fit_height));
        warn!(
            width = fit_width,
            height = fit_height,
            "保存的窗口尺寸比当前工作区还大，已缩进工作区"
        );
    }

    // 离屏判定看**文件里保存的**位置：插件对"保存的位置没有任何屏幕"会跳过设置、
    // 交给系统摆（摆放结果可能仍在屏幕上，但位置是随机的层叠位置）——那正是要补的。
    let _ = position;
    let monitors: Vec<Rect> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| Rect {
            x: monitor.position().x,
            y: monitor.position().y,
            width: monitor.size().width,
            height: monitor.size().height,
        })
        .collect();
    let saved_rect = saved.as_deref().and_then(saved_window_rect);
    let off_screen = match (saved_rect, monitors.is_empty()) {
        (Some(saved_rect), false) => !window_geometry::intersects_any(saved_rect, &monitors),
        _ => false,
    };
    if off_screen {
        let (x, y) = window_geometry::center_in(work_logical, fit_width, fit_height);
        let _ = window.set_position(LogicalPosition::new(x, y));
        warn!(
            x,
            y, "保存的窗口位置不在任何屏幕上（副屏可能拔了），回到主屏幕居中"
        );
    }
}

/// 要不要只允许开一个 Z-SSH。安装版和免安装版用同一个应用标识，也用同一个数据目录，
/// 同时开两个会两边同时写同一个库；第二次打开改为把已经开着的窗口叫到前面。
///
/// 设了 `ZSSH_DATA_DIR` 的（截图脚本、测试，用的是独立数据目录）不参与，免得被用户正在用的
/// Z-SSH 挡住，也不挡用户。插件按应用标识加锁，没法按数据目录区分，所以只能整体不参与。
/// 调试构建里 `ZSSH_FORCE_SINGLE_INSTANCE=1` 强制参与，用来在独立数据目录里验证这个行为。
fn single_instance_enabled(data_dir_env: Option<&std::ffi::OsStr>, forced: bool) -> bool {
    forced || data_dir_env.is_none_or(|value| value.is_empty())
}

fn single_instance_forced() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("ZSSH_FORCE_SINGLE_INSTANCE").is_some_and(|value| value == "1")
}

/// 主窗口的标签（`tauri.conf.json` 里定的）。
pub(crate) const MAIN_LABEL: &str = "main";

/// 把主窗口还原、显示并放到前面（第二次打开 Z-SSH、OneDrive 浏览器授权完成时、托盘叫出窗口都用；
/// 窗口缩在托盘里（隐藏状态）时同样能叫出来）。
pub(crate) fn focus_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    info!("已把主窗口切到前台");
}

fn main() {
    init_tracing();

    let builder = tauri::Builder::default();
    // 单实例插件要最先注册：第二个实例在它这里就退出，不会走到后面打开数据库。
    let builder = if single_instance_enabled(
        std::env::var_os(DATA_DIR_ENV).as_deref(),
        single_instance_forced(),
    ) {
        builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app)
        }))
    } else {
        builder
    };

    builder
        // 窗口状态（UI-1 第 1 节）：官方插件记大小 / 位置 / 最大化。状态文件放进
        // Z-SSH 的数据目录（遵守 ZSSH_DATA_DIR），而不是插件的默认位置
        // （`app_config_dir`）——否则截图脚本和测试用的独立数据目录会互相读到
        // 用户真实的窗口状态。
        .plugin(window_state_plugin())
        // 系统的文件对话框（SFTP "下载到…"、离线备份的导出 / 导入）。
        // capability 里只开了 dialog:allow-open 和 dialog:allow-save。
        .plugin(tauri_plugin_dialog::init())
        // 自动更新（R-4）：插件负责下载和验签；检查、代理、状态都走 crate::updater，
        // capability 里不开 updater:*，界面绕不开这些检查。process 用于其他平台装完后重启。
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // 第一次缩到托盘时的系统通知（UI-4）；只在 Rust 里调用，capability 不开放给界面。
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            apply_initial_window_geometry(app);
            // WebView2 自带右键菜单只留编辑项（NATIVE-1）；挂不上只记日志
            if let Some(main) = app.get_webview_window(MAIN_LABEL) {
                native_menu::install(&main);
            }
            let data_dir = resolve_data_dir(app)?;
            let store = open_store(&data_dir)?;
            info!(dir = %data_dir.display(), "数据目录");
            // 连接池：最后一个通道关闭后空闲 60 秒断开（设计文档 4.1）；
            // 库里存了"连接空闲断开"和 keepalive 的话，启动时先应用一次
            let state = AppState::new(
                store,
                known_hosts_paths(app, &data_dir)?,
                PoolConfig::default(),
            );
            commands::settings::apply_saved_connection_settings(&state);
            // 配置同步（S-2）：数据变了通知调度器（自动同步的“变化后 30 秒”）；
            // 启动时把“记住”的主密码读进内存并起调度任务。
            // OneDrive 登录用的 client ID：构建时从环境变量 ZSSH_ONEDRIVE_CLIENT_ID 编进来（发布版由
            // 工作流从仓库 Variables 取）。自己编译的版本没有，同步设置“高级”里可以填自己的。
            zssh_core::sync::onedrive::config::set_builtin_client_id(option_env!(
                "ZSSH_ONEDRIVE_CLIENT_ID"
            ));
            let sync = sync_controller::SyncController::new(data_dir.clone());
            {
                let notifier = sync.clone();
                state
                    .store
                    .set_change_listener(move || notifier.notify_change());
            }
            app.manage(state);
            app.manage(sync.clone());
            sync.start(app.handle());
            let update = updater::UpdateController::new();
            app.manage(update.clone());
            update.start(app.handle());
            // 托盘图标（UI-4）。创建失败（比如系统没有通知区域）只记日志、程序照常用；
            // 这时 tray_hide_main 会拒绝隐藏窗口（见 tray::hide_main_window），× 退回成退出流程。
            if let Err(err) = tray::setup(app.handle(), &update.status()) {
                warn!(%err, "创建托盘图标失败");
            }
            // S-1：应用版本升级后第一次启动自动备份一次本地数据（DPAPI 加密）。
            // 失败只记日志，不能挡住启动。
            backup_after_upgrade(app, &data_dir);
            // K06：上次崩溃留下的含工具桥令牌的 MCP 配置文件，启动时清掉
            zssh_core::ai::agent::claude::remove_stale_mcp_configs(
                &data_dir.join("agent-workspace"),
            );
            app.manage(commands::stats::StatsRegistry::default());
            app.manage(commands::forward::ForwardManager::default());
            app.manage(commands::agent::AgentRegistry::default());
            app.manage(std::sync::Arc::new(commands::sftp::SftpRegistry::default()));
            app.manage(std::sync::Arc::new(commands::sftp_edit::EditRegistry::new(
                &data_dir,
            )));
            start_tool_bridge(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::session::session_open_quick,
            commands::session::session_open,
            commands::session::session_duplicate,
            commands::session::term_write,
            commands::session::term_resize,
            commands::session::term_ack,
            commands::session::session_close,
            commands::session::session_cwd,
            commands::session::sessions_reset,
            commands::sftp::sftp_open,
            commands::sftp::sftp_close,
            commands::sftp::sftp_list,
            commands::sftp::sftp_stat,
            commands::sftp::sftp_mkdir,
            commands::sftp::sftp_create_file,
            commands::sftp::sftp_rename,
            commands::sftp::sftp_set_permissions,
            commands::sftp::sftp_delete,
            commands::sftp::sftp_delete_cancel,
            commands::sftp::sftp_transfer_submit,
            commands::sftp_paste::sftp_paste,
            commands::sftp_paste::sftp_paste_cancel,
            commands::sftp_size::sftp_size_start,
            commands::sftp_size::sftp_size_cancel,
            commands::sftp::sftp_transfer_pause,
            commands::sftp::sftp_transfer_resume,
            commands::sftp::sftp_transfer_cancel,
            commands::sftp::sftp_transfer_retry,
            commands::sftp::sftp_transfers,
            commands::sftp::sftp_transfers_clear,
            commands::sftp::sftp_conflict_answer,
            commands::sftp::sftp_local_default_dir,
            commands::sftp::sftp_local_list,
            commands::sftp::sftp_local_mkdir,
            commands::sftp::sftp_local_create_file,
            commands::sftp::sftp_local_rename,
            commands::sftp::sftp_local_delete,
            commands::sftp_edit::sftp_edit_resolve,
            commands::sftp_edit::sftp_edit_leftovers,
            commands::sftp_edit::editor::editor_open,
            commands::sftp_edit::editor::editor_save,
            commands::sftp_edit::editor::editor_draft,
            commands::sftp_edit::editor::editor_reload,
            commands::sftp_edit::editor::editor_close,
            commands::sftp_edit::editor::local_editor_open,
            commands::sftp_edit::editor::local_editor_save,
            commands::sftp_edit::editor::local_editor_save_as,
            commands::sftp_edit::sftp_edit_leftover_action,
            commands::ai::ai_approval_answer,
            commands::ai::ai_target_connect_result,
            commands::ai::ai_target_connect_cancel,
            commands::agent::ai_agent_discover,
            commands::agent::ai_agent_login,
            commands::agent::ai_agent_guard,
            commands::agent::ai_agent_probe_path,
            commands::agent::ai_host_picker,
            commands::agent::ai_agent_start,
            commands::agent::ai_agent_stop,
            commands::agent::ai_agent_send,
            commands::agent::ai_agent_abort,
            commands::agent::ai_agent_models,
            commands::agent::ai_agent_catalog,
            commands::agent::ai_history_list,
            commands::agent::ai_history_load,
            commands::agent::ai_history_save,
            commands::agent::ai_history_delete,
            commands::agent::ai_history_clear,
            commands::agent::ai_agent_scope_sync,
            commands::agent::ai_agent_release,
            commands::agent::ai_agent_download_info,
            commands::agent::ai_agent_download,
            commands::agent::ai_agent_download_cancel,
            commands::stats::stats_subscribe,
            commands::stats::stats_unsubscribe,
            commands::auth::hostkey_answer,
            commands::auth::auth_answer,
            commands::auth::hostkey_forget,
            commands::auth::hostkey_saved,
            commands::ime::ime_terminal_focus,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::default_proxy_get,
            commands::settings::default_proxy_set,
            commands::settings::data_dir_info,
            commands::settings::open_data_dir,
            commands::settings::app_network_system_proxy,
            commands::settings::file_window_adopt,
            commands::settings::file_window_take,
            commands::settings::file_window_owned,
            commands::settings::file_window_is_open,
            commands::vault::group_list,
            commands::vault::group_save,
            commands::vault::group_delete,
            commands::vault::host_list,
            commands::vault::host_save,
            commands::vault::host_delete,
            commands::vault::identity_list,
            commands::vault::identity_save,
            commands::vault::identity_delete,
            commands::vault::host_memory_get,
            commands::vault::host_os_list,
            commands::vault::host_memory_set,
            commands::settings::session_log_dir_info,
            commands::settings::open_session_log_dir,
            commands::forward::forward_list,
            commands::forward::forward_save,
            commands::forward::forward_delete,
            commands::forward::forward_start,
            commands::forward::forward_stop,
            commands::forward::forward_statuses,
            commands::vault::identity_generate,
            commands::vault::identity_public_key,
            commands::vault::host_install_public_key,
            commands::vault::proxy_list,
            commands::vault::proxy_save,
            commands::vault::proxy_delete,
            commands::tray::tray_hide_main,
            commands::tray::tray_show_hint,
            commands::tray::tray_ui_ready,
            commands::update::update_status,
            commands::update::update_check,
            commands::update::update_download,
            commands::update::update_busy,
            commands::update::update_install,
            commands::update::update_skip,
            commands::update::update_set_auto,
            commands::update::update_open_download_page,
            commands::sync::sync_status,
            commands::sync::sync_config_save,
            commands::sync::sync_test_connection,
            commands::sync::sync_github_create_gist,
            commands::sync::sync_onedrive_login,
            commands::sync::sync_onedrive_login_cancel,
            commands::sync::sync_onedrive_disconnect,
            commands::sync::sync_onedrive_open_revoke_page,
            commands::sync::sync_set_password,
            commands::sync::sync_remember_password,
            commands::sync::sync_change_password,
            commands::sync::sync_now,
            commands::sync::sync_auto_set,
            commands::sync::sync_conflicts_list,
            commands::sync::sync_conflict_use_other,
            commands::sync::sync_conflict_dismiss,
            commands::sync::sync_backups_list,
            commands::sync::sync_backup_restore,
            commands::sync::sync_offline_export,
            commands::sync::sync_offline_import,
            commands::sync::sync_backup_keep_set,
            commands::sync::sync_reset_baseline,
        ])
        .manage(file_window::FileWindowState::default())
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Destroyed => {
                file_window::on_window_destroyed(window.app_handle(), window.label());
            }
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                schedule_window_state_save(window.app_handle());
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("Z-SSH 启动失败")
        .run(|app, event| {
            // 退出前把还活着的 opencode 对话收掉，不能留孤儿进程。
            if let tauri::RunEvent::Exit = event {
                tauri::async_runtime::block_on(commands::agent::shutdown_all(app));
                commands::sftp_edit::shutdown(app);
                // 退出时安装已经自动下好的更新（UPD-1）。必须排在所有清理之后：
                // 安装成功会直接结束进程，后面的东西就走不到了。
                if let Some(update) = app.try_state::<updater::UpdateController>() {
                    update.install_on_exit(app);
                }
            }
        });
}

/// 数据目录：默认 `%APPDATA%\Z-ssh`，`ZSSH_DATA_DIR` 设置了就用它。
///
/// 自动化脚本（截图、CDP 驱动）靠这个把种子数据和用户的真实库隔开——
/// **不能**让脚本往用户的真实数据目录里写。
pub fn resolve_data_dir(
    app: &impl tauri::Manager<tauri::Wry>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().data_dir()?.join(APP_DIR))
}

/// 版本升级后的第一次启动：在 `backups/` 下留一份本机快照。
///
/// 放到单独的线程里做（DPAPI 解密 + 写文件，库大的时候不短），不挡启动和首屏。
fn backup_after_upgrade(app: &tauri::App, data_dir: &std::path::Path) {
    let handle = app.handle().clone();
    let data_dir = data_dir.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("zssh-upgrade-backup".to_owned())
        .spawn(move || {
            let state = handle.state::<AppState>();
            let sealer = DpapiSealer;
            let device_id = match state.store.sync_device_id() {
                Ok(id) => id,
                Err(err) => {
                    warn!(%err, "同步设备 ID 生成失败，跳过升级备份");
                    return;
                }
            };
            let engine = SyncEngine::new(
                &state.store,
                &sealer,
                device_id,
                data_dir,
                Some(state.known_hosts.our_file().to_path_buf()),
                Some(env!("CARGO_PKG_VERSION").to_owned()),
            );
            match engine.backup_after_upgrade(env!("CARGO_PKG_VERSION")) {
                Ok(Some(info)) => info!(path = %info.path, "版本升级后已自动备份"),
                Ok(None) => {}
                Err(err) => warn!(%err, "版本升级后的自动备份失败"),
            }
        });
    if let Err(err) = spawned {
        warn!(%err, "启动升级备份线程失败");
    }
}

/// 启动 AI 工具桥（卡片 AI-1）：监听 `127.0.0.1` 的随机端口。
///
/// 只接受带作用范围令牌的连接；`zssh-mcp.exe`（AI-2 启动它时注入端口和令牌）
/// 连过来后，工具调用在会话所在的连接上执行、审批走 `ai_approval_request` 事件、
/// 审计写 `ai_audit` 表。端口和作用范围令牌由调用方（AI-2）使用。
fn start_tool_bridge(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let approvals = Arc::new(commands::ai::AiApprovals::default());
    let connects = Arc::new(commands::ai::TargetConnects::default());
    let handle = app.handle().clone();
    let bridge = Arc::new(Bridge::new(
        Arc::new(commands::ai::AppBridgeHost::new(
            handle.clone(),
            Arc::clone(&connects),
        )),
        Arc::new(commands::ai::StoreAudit::new(handle.clone())),
        Arc::new(commands::ai::TauriApprover::new(
            handle.clone(),
            Arc::clone(&approvals),
        )),
        Arc::new(commands::ai::StoreExecMode::new(handle)),
    ));

    // 先同步绑定拿到端口，再把监听交给异步运行时。
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    info!(port, "AI 工具桥开始监听");
    let serve_bridge = Arc::clone(&bridge);
    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => serve_bridge.serve(listener).await,
            Err(err) => warn!(%err, "工具桥监听失败"),
        }
    });

    app.manage(approvals);
    app.manage(connects);
    // AI-2d 1.4：正在终端里跑着的 AI 命令（停止按钮用它决定要不要发 Ctrl+C）。
    app.manage(zssh_core::ai::runs::TerminalRuns::default());
    // AI 面板启动驱动时要拿桥和端口：scope 令牌、zssh-mcp 的连接信息都从这里来。
    app.manage(commands::agent::ToolBridge::new(bridge, port));
    Ok(())
}

/// 打开（必要时新建）数据目录里的数据库，并把迁移跑完。
fn open_store(data_dir: &std::path::Path) -> Result<Database, Box<dyn std::error::Error>> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(DB_FILE);
    let store = Database::open(&path)?;
    info!(db = %path.display(), version = store.schema_version()?, "主机库已就绪");
    Ok(store)
}

/// known_hosts 的两份文件：自己的在数据目录里（可写），系统那份只读。
fn known_hosts_paths(
    app: &tauri::App,
    data_dir: &std::path::Path,
) -> Result<KnownHosts, Box<dyn std::error::Error>> {
    let ours = data_dir.join("known_hosts");
    let mut known_hosts = KnownHosts::new(ours);
    if let Ok(home) = app.path().home_dir() {
        let user_file = home.join(".ssh").join("known_hosts");
        if user_file.exists() {
            known_hosts = known_hosts.with_read_only(user_file);
        }
    }
    Ok(known_hosts)
}

/// 日志级别用 `RUST_LOG` 覆盖，默认 `info`。
///
/// 密码不会进日志：`ConnectParams` 自己实现了 `Debug`，把密码字段隐去了。
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if let Err(err) = tracing_subscriber::fmt().with_env_filter(filter).try_init() {
        warn!(%err, "日志初始化失败");
    }
}

#[cfg(test)]
mod single_instance_tests {
    use super::single_instance_enabled;
    use std::ffi::OsStr;

    #[test]
    fn only_the_default_data_dir_is_single_instance() {
        assert!(single_instance_enabled(None, false), "正常启动只开一个");
        assert!(
            single_instance_enabled(Some(OsStr::new("")), false),
            "空值等于没设"
        );
        assert!(
            !single_instance_enabled(Some(OsStr::new(r"C:\zssh-shots\a")), false),
            "独立数据目录（脚本、测试）不参与"
        );
        assert!(single_instance_enabled(
            Some(OsStr::new(r"C:\zssh-shots\a")),
            true
        ));
    }
}
