//! 更新状态的前端缓存：后端每次状态变化都推 `update://status`，这里存一份最新的。

import { create } from "zustand";

import {
  onUpdateStatus,
  updateCheck,
  updateDownload,
  updateOpenDownloadPage,
  updateSetAuto,
  updateSkip,
  updateStatus,
  type UpdatePrefs,
  type UpdateStatus,
} from "../api/updater";
import { statusAfterCheckError } from "../state/updateModel";

interface UpdaterState {
  status: UpdateStatus | null;
  prefs: UpdatePrefs | null;
  currentVersion: string;
  /** 当前状态是自动流程产生的（自动检查 / 自动下载）：底部不提示。 */
  auto: boolean;
  /** 免安装版：更新只能去发布页手动下载。 */
  portable: boolean;
  /** 点了“稍后”的版本：这一次运行里状态栏不再提示它。 */
  dismissedVersion: string | null;
  /** 新版本详情对话框开着没有。 */
  dialogOpen: boolean;
  /** 后台自动下好后已经弹过对话框的版本：每个版本这一次运行里只弹一次。 */
  promptedReadyVersion: string | null;
  load: () => Promise<void>;
  applyStatus: (status: UpdateStatus, auto?: boolean) => void;
  check: () => Promise<void>;
  /** 用户手动点「检查更新」：查到新版本就直接打开更新对话框，不用再点「查看更新」。 */
  checkManually: () => Promise<void>;
  download: () => Promise<void>;
  openDownloadPage: () => Promise<void>;
  skip: (version: string) => Promise<void>;
  setAuto: (enabled: boolean) => Promise<void>;
  dismiss: (version: string) => void;
  openDialog: (open: boolean) => void;
}

export const useUpdater = create<UpdaterState>((set, get) => ({
  status: null,
  prefs: null,
  currentVersion: "",
  auto: false,
  portable: false,
  dismissedVersion: null,
  dialogOpen: false,
  promptedReadyVersion: null,

  async load() {
    const view = await updateStatus();
    set({
      status: view.status,
      prefs: view.prefs,
      currentVersion: view.current_version,
      portable: view.portable === true,
      auto: view.auto === true,
    });
  },

  applyStatus(status, auto) {
    // 事件没带标记（旧后端 / 测试）时保持原值
    set(auto === undefined ? { status } : { status, auto });
    // 后台自动下好了：弹一次对话框告诉用户，让他选现在重启安装还是退出时再装。
    const state = get();
    if (
      status.phase === "ready" &&
      state.auto &&
      !state.portable &&
      state.promptedReadyVersion !== status.version
    ) {
      set({ promptedReadyVersion: status.version });
      state.openDialog(true);
    }
  },

  async check() {
    set({ status: { phase: "checking" } });
    try {
      const view = await updateCheck();
      set({
        status: view.status,
        prefs: view.prefs,
        currentVersion: view.current_version,
        auto: view.auto === true,
      });
    } catch (error: unknown) {
      // IPC 被拒绝时后端不会再推状态，不恢复就一直卡在"正在检查"
      set((state) => ({ status: statusAfterCheckError(state.status, error) }));
      throw error;
    }
  },

  async checkManually() {
    await get().check();
    // 手动点的才弹对话框：查到新版本，或者后台已经自动下好了（对话框里说明退出时自动安装）。
    // 自动流程本身不弹任何东西。
    const phase = get().status?.phase;
    if (phase === "available" || phase === "ready") get().openDialog(true);
  },

  async download() {
    const view = await updateDownload();
    set({ status: view.status, prefs: view.prefs, auto: view.auto === true });
  },

  async openDownloadPage() {
    await updateOpenDownloadPage();
  },

  async skip(version) {
    const view = await updateSkip(version);
    set({ status: view.status, prefs: view.prefs, auto: view.auto === true, dialogOpen: false });
  },

  async setAuto(enabled) {
    const view = await updateSetAuto(enabled);
    set({ prefs: view.prefs });
  },

  dismiss(version) {
    set({ dismissedVersion: version, dialogOpen: false });
  },

  openDialog(open) {
    set({ dialogOpen: open });
    // 重新点开已经“稍后”过的版本，等于撤销稍后。
    if (open) set({ dismissedVersion: null });
  },
}));

/** 应用启动时调用一次：读一次状态并订阅后端推送。返回取消订阅的函数。 */
export async function startUpdaterEvents(): Promise<() => void> {
  try {
    await useUpdater.getState().load();
  } catch {
    // 后端没起来（单测 / 预览）：状态保持 null，状态栏不显示。
  }
  try {
    return await onUpdateStatus((status, auto) => useUpdater.getState().applyStatus(status, auto));
  } catch {
    return () => {};
  }
}
