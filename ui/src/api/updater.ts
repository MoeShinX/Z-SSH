//! 自动更新的接口封装（后端见 src-tauri/src/commands/update.rs 和 updater.rs）。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type UpdateStage = "check" | "download" | "install";

export type UpdateStatus =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "up_to_date"; checked_at: number }
  | { phase: "available"; version: string; notes: string | null; date: string | null }
  | { phase: "downloading"; version: string; downloaded: number; total: number | null }
  | { phase: "ready"; version: string; notes?: string | null }
  | { phase: "failed"; stage: UpdateStage; message: string };

export interface UpdatePrefs {
  auto_check: boolean;
  last_check_at: number | null;
  skipped_version: string | null;
}

export interface UpdateView {
  status: UpdateStatus;
  prefs: UpdatePrefs;
  current_version: string;
  /** 免安装版：有新版本时只提示去发布页手动下载，不自动下载安装。 */
  portable: boolean;
  /** 当前状态是自动流程（自动检查 / 自动下载）产生的：底部不提示。手动检查引出的状态是 false。 */
  auto: boolean;
}

/** 重启会打断的事（后端能数的那部分：终端里跑着的 AI 命令）。 */
export interface RestartBusy {
  transfers: number;
  ai_commands: number;
  unsaved_edits: number;
}

export const updateStatus = () => invoke<UpdateView>("update_status");
export const updateCheck = () => invoke<UpdateView>("update_check");
export const updateDownload = () => invoke<UpdateView>("update_download");
export const updateBusy = () => invoke<RestartBusy>("update_busy");
export const updateInstall = () => invoke<void>("update_install");
export const updateSkip = (version: string) => invoke<UpdateView>("update_skip", { version });
/** 用浏览器打开新版本的发布页（地址由后端从更新地址推出来）。 */
export const updateOpenDownloadPage = () => invoke<void>("update_open_download_page");
export const updateSetAuto = (enabled: boolean) =>
  invoke<UpdateView>("update_set_auto", { enabled });

/** 状态事件：状态本身，加上"这是自动流程产生的"标记 `auto`。 */
export function onUpdateStatus(
  handler: (status: UpdateStatus, auto: boolean) => void,
): Promise<UnlistenFn> {
  return listen<UpdateStatus & { auto?: boolean }>("update://status", (event) => {
    const { auto, ...status } = event.payload;
    handler(status as UpdateStatus, auto === true);
  });
}
