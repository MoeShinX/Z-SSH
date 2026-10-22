//! 主机的系统类型：启动时读一次，之后跟着后端的 `host-os` 事件更新。

import { create } from "zustand";
import { hostOsList, onHostOs } from "../api/hostOs";

interface HostOsState {
  byHost: Record<number, string>;
}

export const useHostOs = create<HostOsState>(() => ({ byHost: {} }));

let started = false;

/** App 挂载时调一次。拿不到（后端旧、测试环境）就什么都不显示，用通用图标。 */
export function startHostOs(): void {
  if (started) return;
  started = true;
  try {
    void onHostOs(({ host_id, os }) => {
      useHostOs.setState((state) => ({ byHost: { ...state.byHost, [host_id]: os } }));
    }).catch(() => {});
    void hostOsList()
      .then((map) => {
        const byHost: Record<number, string> = {};
        for (const [id, os] of Object.entries(map ?? {})) byHost[Number(id)] = os;
        useHostOs.setState((state) => ({ byHost: { ...byHost, ...state.byHost } }));
      })
      .catch(() => {});
  } catch {
    // 没有 Tauri 运行时
  }
}

export function useHostOsId(hostId: number): string | undefined {
  return useHostOs((state) => state.byHost[hostId]);
}
