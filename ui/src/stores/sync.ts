//! 同步状态的前端缓存：后端每次状态变化都推 `sync://status`，这里存一份最新的。
//!
//! 同步把云端数据写进本地库之后，主机列表要重新读（否则界面还显示旧的）。

import { create } from "zustand";

import {
  onSyncStatus,
  syncStatus,
  type ConnectionProbe,
  type SyncStatusView,
} from "../api/sync";
import { useSettings } from "./settings";
import { useVault } from "./vault";

interface SyncState {
  status: SyncStatusView | null;
  /** 最近一次“测试连接”的详细结果（读 / 写 / 并发保护分别显示）。 */
  probe: ConnectionProbe | null;
  /** 从状态栏点进来时，设置页要直接打开“同步”分组。 */
  wantSettingsSection: boolean;
  load: () => Promise<void>;
  apply: (status: SyncStatusView) => void;
  setProbe: (probe: ConnectionProbe | null) => void;
  requestSettingsSection: (want: boolean) => void;
}

/**
 * 同步、恢复备份、用另一份覆盖冲突之后调：这些都会直接改本地库，界面缓存的主机列表和
 * 设置要重新读，不然界面显示的和后端实际用的会不一样。
 */
export async function refreshLocalData(): Promise<void> {
  await Promise.all([useVault.getState().load(), useSettings.getState().load()]);
}

export const useSync = create<SyncState>((set, get) => ({
  status: null,
  probe: null,
  wantSettingsSection: false,

  async load() {
    get().apply(await syncStatus());
  },

  apply(status) {
    const previous = get().status;
    set({ status });
    // 上次结果变了（同步 / 恢复写过本地库）：重新读主机列表和设置。
    if (previous && previous.last?.at !== status.last?.at && status.last?.status === "completed") {
      void refreshLocalData();
    }
  },

  setProbe(probe) {
    set({ probe });
  },

  requestSettingsSection(want) {
    set({ wantSettingsSection: want });
  },
}));

/** 应用启动时调用一次：读一次状态并订阅后端推送。返回取消订阅的函数。 */
export async function startSyncEvents(): Promise<() => void> {
  try {
    await useSync.getState().load();
  } catch {
    // 后端没起来（单测 / 预览）：状态保持 null，状态栏不显示。
  }
  try {
    return await onSyncStatus((status) => useSync.getState().apply(status));
  } catch {
    return () => {};
  }
}
