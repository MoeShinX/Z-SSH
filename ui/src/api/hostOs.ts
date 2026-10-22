//! 主机的系统类型（后端连上之后探测，见 crates/zssh-core/src/ssh/os_probe.rs）。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 主机 ID → 系统 ID（JSON 对象的键是字符串）。 */
export const hostOsList = (): Promise<Record<string, string>> => invoke("host_os_list");

export function onHostOs(handler: (event: { host_id: number; os: string }) => void): Promise<UnlistenFn> {
  return listen<{ host_id: number; os: string }>("host-os", (event) => handler(event.payload));
}
