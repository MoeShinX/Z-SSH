//! 焦点进入 / 离开「英文区」时切换输入法中英文（只在 Windows 上有效，见 src-tauri 的 commands/ime.rs）。
//! 请不要直接调用：焦点变化会乱序，统一走 terminal/imeFocus.ts 的队列。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 返回后端处理完以后是否仍认为这个窗口处于英文区（离开时恢复没成功会是 true）。 */
export const imeTerminalFocus = (focused: boolean): Promise<boolean> =>
  invoke<boolean>("ime_terminal_focus", { focused });

/**
 * 后端广播（`ime_restore_pending`）：某个窗口被关掉时输入法没恢复成功，而剩下的窗口此前
 * 没进过英文区、自己不知道还欠着一次恢复。收到后下一次焦点进跟随区时补发「离开」。
 */
export const onImeRestorePending = (handler: () => void): Promise<UnlistenFn> =>
  listen("ime_restore_pending", () => handler());
