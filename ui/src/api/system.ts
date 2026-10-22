//! 读系统层面的用户设置（后端见 src-tauri 的 commands/system.rs）。

import { invoke } from "@tauri-apps/api/core";

/** 系统的双击时间（毫秒）：Windows 的「鼠标」设置里可调（默认 500，最大 5000）；其他平台 500。 */
export const systemDoubleClickTime = (): Promise<number> =>
  invoke<number>("system_double_click_time");
