//! 托盘相关的接口封装（后端见 src-tauri/src/tray.rs 和 commands/tray.rs）。

import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/** 缩到托盘（隐藏主窗口）。返回 `false` = 没隐藏（比如托盘图标创建失败），调用方要走退出流程。 */
export const trayHideMain = (): Promise<boolean> => invoke("tray_hide_main");

/** 第一次缩到托盘的系统通知。 */
export const trayShowHint = (): Promise<void> => invoke("tray_show_hint");

/** 把主窗口叫到前面（有退出确认要问时用；托盘「退出」不再事先叫出窗口）。 */
export const trayShowMain = (): Promise<void> => invoke("tray_show_main");

/** 告诉后端：能回答托盘「退出」的确认了 / 不能了。 */
export const trayUiReady = (ready: boolean): Promise<void> => invoke("tray_ui_ready", { ready });

/** 托盘「退出」：界面走和点 × 退出同一套确认；没有要确认的就直接退出，有才叫出主窗口。 */
export function onTrayQuit(handler: () => void): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen("tray://quit", () => handler());
}

/** 托盘「重启安装」：界面打开已经下载好的更新对话框。 */
export function onTrayOpenUpdate(handler: () => void): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen("tray://open-update", () => handler());
}
