//! 托盘相关命令（UI-4）：只做转接，逻辑在 `crate::tray`。

use tauri::AppHandle;

/// 缩到托盘：前端在「关闭主窗口时缩到托盘」模式下拦住关闭后调用。返回 `false` = 没隐藏（没有托盘图标）。
#[tauri::command]
pub fn tray_hide_main(app: AppHandle) -> bool {
    crate::tray::hide_main_window(&app)
}

/// 第一次缩到托盘的系统通知（是否第一次由前端记在设置里判断）。
#[tauri::command]
pub fn tray_show_hint(app: AppHandle) {
    crate::tray::show_first_hide_hint(&app);
}

/// 把主窗口叫到前面：托盘「退出」不再事先叫出窗口，前端发现有东西要确认时自己来叫。
#[tauri::command]
pub fn tray_show_main(app: AppHandle) {
    crate::focus_main_window(&app);
}

/// 前端告诉后端：能回答托盘「退出」的确认了（`true`），或者不能了（`false`）。
#[tauri::command]
pub fn tray_ui_ready(app: AppHandle, ready: bool) {
    crate::tray::set_ui_ready(&app, ready);
}
