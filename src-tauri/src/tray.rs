//! 托盘图标（UI-4）：主窗口点 × 缩到右下角后，程序靠它继续被找到、被叫回来、被真正退出。
//!
//! ## 谁管什么
//!
//! - **关闭主窗口时是缩到托盘还是退出**：由前端决定（`ui/src/state/closeBehavior.ts`）。
//!   Tauri 的 `onCloseRequested` 一旦在 JS 里注册，Tauri 就会先拦下关闭、把事件发给 JS，
//!   JS 回调没 `preventDefault` 就会 `destroy()` 窗口（`tauri-2.12.3/src/manager/window.rs`
//!   的 `on_window_event`，以及 `@tauri-apps/api` 的 `onCloseRequested`）。所以 Rust 这边
//!   **不碰主窗口的 `CloseRequested`**，免得两边同时处理互相打架；JS 要缩到托盘时调
//!   [`hide_main_window`]（`tray_hide_main` 命令）。
//! - **托盘图标本身、菜单、左键叫出窗口**：这里。
//! - **托盘「退出」**：这里只负责把主窗口叫到前面，再通知前端走和点 × 退出同一套确认
//!   （`tray://quit`）；确认后由前端关主窗口，主窗口销毁 → `app.exit(0)` → 正常的
//!   `RunEvent::Exit`（见 `file_window::on_window_destroyed`），清理一项不少。
//!   前端还没就绪（页面没加载出来）时没人能问确认，直接退出，免得托盘里点了「退出」没反应。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tracing::{info, warn};

use crate::updater::UpdateStatus;

const TRAY_ID: &str = "main";
const TOOLTIP: &str = "Z-SSH";
const ID_SHOW: &str = "tray-show";
const ID_UPDATE: &str = "tray-update";
const ID_QUIT: &str = "tray-quit";

/// 托盘「退出」→ 前端：走退出确认（未保存修改 / 传输中），确认后退出。
pub const QUIT_EVENT: &str = "tray://quit";
/// 托盘「重启安装」→ 前端：打开已经下载好的更新对话框（由用户在对话框里点「立即重启安装」）。
pub const OPEN_UPDATE_EVENT: &str = "tray://open-update";

/// 托盘菜单里的一项。抽成纯数据，菜单随更新状态怎么变可以不起窗口直接测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEntry {
    Show,
    /// 后台已经下好了更新：`version` 不带前缀 `v`。
    RestartInstall {
        version: String,
    },
    Separator,
    Quit,
}

impl TrayEntry {
    fn label(&self) -> Option<String> {
        match self {
            Self::Show => Some("显示 Z-SSH".to_owned()),
            Self::RestartInstall { version } => Some(format!("重启安装 v{version}")),
            Self::Separator => None,
            Self::Quit => Some("退出".to_owned()),
        }
    }

    fn id(&self) -> Option<&'static str> {
        match self {
            Self::Show => Some(ID_SHOW),
            Self::RestartInstall { .. } => Some(ID_UPDATE),
            Self::Separator => None,
            Self::Quit => Some(ID_QUIT),
        }
    }
}

/// 现在的更新状态该显示哪些菜单项：只有 `Ready`（已下载并验签）才多一项「重启安装」。
pub fn menu_entries(status: &UpdateStatus) -> Vec<TrayEntry> {
    let mut entries = vec![TrayEntry::Show];
    if let UpdateStatus::Ready { version, .. } = status {
        entries.push(TrayEntry::RestartInstall {
            version: version.trim_start_matches('v').to_owned(),
        });
    }
    entries.push(TrayEntry::Separator);
    entries.push(TrayEntry::Quit);
    entries
}

/// 托盘「退出」怎么走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitRoute {
    /// 通知前端走确认流程。
    AskUi,
    /// 没有能回答确认的界面，直接退出。
    ExitNow,
}

pub fn quit_route(main_window_exists: bool, ui_ready: bool) -> QuitRoute {
    if main_window_exists && ui_ready {
        QuitRoute::AskUi
    } else {
        QuitRoute::ExitNow
    }
}

/// 托盘的运行状态（`app.manage`）。
#[derive(Default)]
pub struct TrayState {
    /// 菜单上次按哪组项建的：状态没变（下载进度每秒推好几次）就不重建。
    entries: Mutex<Vec<TrayEntry>>,
    /// 前端已经挂好 `tray://quit` 的监听（和关闭拦截），能回答退出确认。
    ui_ready: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn build_menu<R: Runtime>(app: &AppHandle<R>, entries: &[TrayEntry]) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for entry in entries {
        match (entry.id(), entry.label()) {
            (Some(id), Some(label)) => {
                menu.append(&MenuItem::with_id(app, id, label, true, None::<&str>)?)?;
            }
            _ => menu.append(&PredefinedMenuItem::separator(app)?)?,
        }
    }
    Ok(menu)
}

/// 建托盘图标（程序启动时一次；两种关闭模式下都显示）。
pub fn setup<R: Runtime>(app: &AppHandle<R>, status: &UpdateStatus) -> tauri::Result<()> {
    let entries = menu_entries(status);
    let menu = build_menu(app, &entries)?;
    app.manage(TrayState {
        entries: Mutex::new(entries),
        ui_ready: AtomicBool::new(false),
    });

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP)
        .menu(&menu)
        // 左键是「叫出窗口」，菜单只给右键。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            ID_SHOW => crate::focus_main_window(app),
            ID_UPDATE => {
                crate::focus_main_window(app);
                if let Err(err) = app.emit_to(crate::MAIN_LABEL, OPEN_UPDATE_EVENT, ()) {
                    warn!(%err, "通知界面打开更新对话框失败");
                }
            }
            ID_QUIT => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => crate::focus_main_window(tray.app_handle()),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    } else {
        warn!("没有找到应用图标，托盘图标会是空白");
    }
    builder.build(app)?;
    info!("托盘图标已创建");
    Ok(())
}

/// 更新状态变了：菜单项变了才重建菜单（不轮询，由 `updater::set_status` 通知）。
pub fn refresh<R: Runtime>(app: &AppHandle<R>, status: &UpdateStatus) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let entries = menu_entries(status);
    {
        let mut current = lock(&state.entries);
        if *current == entries {
            return;
        }
        *current = entries.clone();
    }
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match build_menu(app, &entries) {
        Ok(menu) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                warn!(%err, "刷新托盘菜单失败");
            }
        }
        Err(err) => warn!(%err, "构建托盘菜单失败"),
    }
}

/// 托盘「退出」：先把主窗口叫出来（确认对话框要让用户看见），再按 [`quit_route`] 走。
fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    crate::focus_main_window(app);
    let ready = app
        .try_state::<TrayState>()
        .is_some_and(|state| state.ui_ready.load(Ordering::SeqCst));
    match quit_route(app.get_webview_window(crate::MAIN_LABEL).is_some(), ready) {
        QuitRoute::AskUi => {
            if let Err(err) = app.emit_to(crate::MAIN_LABEL, QUIT_EVENT, ()) {
                warn!(%err, "通知界面确认退出失败，直接退出");
                app.exit(0);
            }
        }
        QuitRoute::ExitNow => app.exit(0),
    }
}

/// 前端告诉后端：退出确认的监听挂好了 / 卸了。
pub fn set_ui_ready<R: Runtime>(app: &AppHandle<R>, ready: bool) {
    if let Some(state) = app.try_state::<TrayState>() {
        state.ui_ready.store(ready, Ordering::SeqCst);
    }
}

/// 隐藏主窗口（缩到托盘）。程序里的会话、转发、AI 任务、定时更新检查都不受影响。
///
/// 返回有没有真的隐藏：没有托盘图标（创建失败）时**不隐藏**——窗口藏起来就再也叫不回来了，
/// 前端拿到 `false` 就按「退出程序」走。
pub fn hide_main_window<R: Runtime>(app: &AppHandle<R>) -> bool {
    if app.tray_by_id(TRAY_ID).is_none() {
        warn!("没有托盘图标，不能把主窗口缩到托盘");
        return false;
    }
    let Some(window) = app.get_webview_window(crate::MAIN_LABEL) else {
        return false;
    };
    match window.hide() {
        Ok(()) => {
            info!("主窗口已缩到托盘");
            true
        }
        Err(err) => {
            warn!(%err, "隐藏主窗口失败");
            false
        }
    }
}

/// 第一次缩到托盘时的系统通知。失败只记日志：提示不到不影响功能。
pub fn show_first_hide_hint<R: Runtime>(app: &AppHandle<R>) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(err) = app
        .notification()
        .builder()
        .title("Z-SSH 还在运行")
        .body("Z-SSH 还在右下角运行，右键托盘图标可以退出。")
        .show()
    {
        warn!(%err, "显示托盘提示通知失败");
    }
}

/// 有事要用户回答时，主窗口该怎么提醒：`show` = 窗口缩在托盘里（看不见），先叫出来；
/// `flash` = 窗口不在前台，闪任务栏按钮。窗口就在前台时什么都不做。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attention {
    pub show: bool,
    pub flash: bool,
}

pub fn attention_for(visible: bool, focused: bool) -> Attention {
    Attention {
        show: !visible,
        flash: !focused,
    }
}

/// AI 要确认命令、主机指纹要确认、要输入密码等**等着用户回答**的事发生时调用。
///
/// 窗口缩在托盘里时用户根本看不到提问，AI 和连接会一直卡着，所以先把窗口叫出来；
/// 窗口开着但不在前台（用户在用别的程序）只闪任务栏，不抢焦点。
pub fn call_attention<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(crate::MAIN_LABEL) else {
        return;
    };
    let attention = attention_for(
        window.is_visible().unwrap_or(true),
        window.is_focused().unwrap_or(false),
    );
    if attention.show {
        info!("有提问等着回答，把缩在托盘里的主窗口叫出来");
        crate::focus_main_window(app);
    }
    if attention.flash {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_shows_hidden_window_and_flashes_when_not_focused() {
        // 缩在托盘里：叫出来，也闪一下
        assert_eq!(
            attention_for(false, false),
            Attention {
                show: true,
                flash: true
            }
        );
        // 开着但在后面：只闪任务栏，不抢焦点
        assert_eq!(
            attention_for(true, false),
            Attention {
                show: false,
                flash: true
            }
        );
        // 就在前台：什么都不做
        assert_eq!(
            attention_for(true, true),
            Attention {
                show: false,
                flash: false
            }
        );
    }

    fn ready(version: &str) -> UpdateStatus {
        UpdateStatus::Ready {
            version: version.to_owned(),
            notes: None,
        }
    }

    #[test]
    fn menu_is_show_separator_quit_without_a_downloaded_update() {
        for status in [
            UpdateStatus::Idle,
            UpdateStatus::Checking,
            UpdateStatus::UpToDate { checked_at: 1 },
            UpdateStatus::Available {
                version: "9.9.9".to_owned(),
                notes: None,
                date: None,
            },
            UpdateStatus::Downloading {
                version: "9.9.9".to_owned(),
                downloaded: 10,
                total: Some(100),
            },
        ] {
            assert_eq!(
                menu_entries(&status),
                [TrayEntry::Show, TrayEntry::Separator, TrayEntry::Quit],
                "{status:?}"
            );
        }
    }

    #[test]
    fn a_downloaded_update_adds_the_restart_item_before_the_separator() {
        assert_eq!(
            menu_entries(&ready("1.2.3")),
            [
                TrayEntry::Show,
                TrayEntry::RestartInstall {
                    version: "1.2.3".to_owned()
                },
                TrayEntry::Separator,
                TrayEntry::Quit,
            ]
        );
    }

    #[test]
    fn the_restart_item_label_has_exactly_one_v_prefix() {
        let entries = menu_entries(&ready("v1.2.3"));
        assert_eq!(entries[1].label().as_deref(), Some("重启安装 v1.2.3"));
        assert_eq!(entries[0].label().as_deref(), Some("显示 Z-SSH"));
        assert_eq!(entries[3].label().as_deref(), Some("退出"));
        assert_eq!(entries[2].label(), None);
        assert_eq!(entries[2].id(), None);
    }

    #[test]
    fn failed_download_removes_the_restart_item_again() {
        let with = menu_entries(&ready("1.2.3"));
        let after = menu_entries(&UpdateStatus::Failed {
            stage: crate::updater::Stage::Install,
            message: "x".to_owned(),
        });
        assert_eq!(with.len(), 4);
        assert_eq!(after.len(), 3);
    }

    #[test]
    fn quit_asks_the_ui_only_when_it_can_answer() {
        assert_eq!(quit_route(true, true), QuitRoute::AskUi);
        assert_eq!(quit_route(true, false), QuitRoute::ExitNow);
        assert_eq!(quit_route(false, true), QuitRoute::ExitNow);
        assert_eq!(quit_route(false, false), QuitRoute::ExitNow);
    }

    /// 模拟运行时里建托盘（不会在桌面上出现真图标）：状态变成 Ready 时菜单项跟着变，
    /// 变回去再变回来；下载进度这种不影响菜单的状态不重建。
    #[test]
    fn refresh_tracks_the_update_status_on_a_mock_tray() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app");
        let handle = app.handle();
        setup(handle, &UpdateStatus::Idle).expect("建托盘");
        assert!(handle.tray_by_id(TRAY_ID).is_some());
        let current = || lock(&handle.state::<TrayState>().entries).clone();
        assert_eq!(current().len(), 3);

        refresh(handle, &ready("2.0.0"));
        assert_eq!(current().len(), 4);
        assert_eq!(
            current()[1],
            TrayEntry::RestartInstall {
                version: "2.0.0".to_owned()
            }
        );

        refresh(
            handle,
            &UpdateStatus::Failed {
                stage: crate::updater::Stage::Install,
                message: "x".to_owned(),
            },
        );
        assert_eq!(current().len(), 3);
    }

    #[test]
    fn hiding_the_main_window_needs_a_tray_and_a_window() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app");
        let handle = app.handle();
        // 没托盘：不隐藏（窗口藏起来就叫不回来了）
        assert!(!hide_main_window(handle));
        setup(handle, &UpdateStatus::Idle).expect("建托盘");
        // 有托盘但没有主窗口（测试里没建）：也没东西可隐藏
        assert!(!hide_main_window(handle));
        // 有托盘也有主窗口：隐藏调用成功
        tauri::WebviewWindowBuilder::new(
            handle,
            crate::MAIN_LABEL,
            tauri::WebviewUrl::App("index.html".into()),
        )
        .build()
        .expect("建主窗口");
        assert!(hide_main_window(handle));
        // 模拟运行时的 hide 不改可见状态，真窗口的隐藏 / 叫回留给手测
    }

    #[test]
    fn refresh_without_a_tray_is_a_noop() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app");
        // 没建托盘时（单测、托盘创建失败）更新状态变化不能出错。
        refresh(app.handle(), &ready("1.0.0"));
    }
}
