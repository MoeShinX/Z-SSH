//! 终端获得 / 失去焦点时切换输入法中英文（只在 Windows 上有效，见 src-tauri 的 commands/ime.rs）。
//! 请不要直接调用：焦点变化会乱序，统一走 terminal/imeFocus.ts 的队列。

import { invoke } from "@tauri-apps/api/core";

/** 返回后端处理完以后是否仍认为处于终端焦点（离开时恢复没成功会是 true）。 */
export const imeTerminalFocus = (focused: boolean): Promise<boolean> =>
  invoke<boolean>("ime_terminal_focus", { focused });
