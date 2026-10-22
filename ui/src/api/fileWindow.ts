//! 文件窗口（像 Xftp 那样的独立窗口，最多一个，里面用标签放多个 SFTP）和主窗口之间的通信。
//!
//! 后端负责建窗口和"待接管队列"（窗口刚建好时前端还没开始监听，不能只靠事件）；
//! 两个窗口之间的其他消息（打开编辑器、SFTP 会话清单、移回主窗口）直接用 emitTo。
//! 监听都挂在当前窗口上（getCurrentWebviewWindow().listen），只收发给自己的事件。

import { invoke } from "@tauri-apps/api/core";
import { emitTo, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

export const MAIN_LABEL = "main";

/** 交给文件窗口（或者移回主窗口）的一个 SFTP 标签。`sftp_id` 为空 = 还没连，接手的一方去连。 */
export interface FileTabHandoff {
  sftp_id: string;
  host_id: number;
  title: string;
  local: string;
  remote: string;
}

/** 文件窗口里的一个 SFTP 会话（主窗口的编辑器保存时据此找会话）。 */
export interface FileWindowSession {
  sftp_id: string;
  host_id: number;
}

/** 文件窗口请主窗口用内置编辑器打开一个文件。 */
export interface OpenEditorRequest {
  kind: "remote" | "local";
  sftp_id: string;
  host_id: number | null;
  path: string;
}

/** 这个页面是不是文件窗口（后端建窗口时注入的标记）。 */
export function isFileWindow(): boolean {
  return (window as { __ZSSH_WINDOW__?: string }).__ZSSH_WINDOW__ === "files";
}

export const fileWindowAdopt = (tab: FileTabHandoff) => invoke<void>("file_window_adopt", { tab });
export const fileWindowTake = () => invoke<FileTabHandoff[]>("file_window_take");
export const fileWindowOwned = (sftpIds: string[]) =>
  invoke<void>("file_window_owned", { sftpIds });
export const fileWindowIsOpen = () => invoke<boolean>("file_window_is_open");

function listenHere<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen<T>(event, (e) => handler(e.payload));
}

/** 文件窗口：待接管队列里有新东西了。 */
export const onFileWindowPending = (handler: () => void) =>
  listenHere<unknown>("files://pending", () => handler());
/** 主窗口：文件窗口开了 / 关了。 */
export const onFileWindowState = (handler: (open: boolean) => void) =>
  listenHere<boolean>("files://state", handler);

export const requestOpenEditor = (request: OpenEditorRequest) =>
  emitTo(MAIN_LABEL, "files://open-editor", request);
export const onOpenEditorRequest = (handler: (request: OpenEditorRequest) => void) =>
  listenHere<OpenEditorRequest>("files://open-editor", handler);

export const reportFileSessions = (sessions: FileWindowSession[]) =>
  emitTo(MAIN_LABEL, "files://sessions", sessions);
export const onFileSessions = (handler: (sessions: FileWindowSession[]) => void) =>
  listenHere<FileWindowSession[]>("files://sessions", handler);

/** 主窗口的设置变了（主题等）：文件窗口重新读一遍。 */
export const notifySettingsChanged = () => emitTo("files", "files://settings", null);
export const onSettingsChanged = (handler: () => void) =>
  listenHere<unknown>("files://settings", () => handler());

export const returnToMain = (tab: FileTabHandoff) => emitTo(MAIN_LABEL, "files://return", tab);
export const onReturnToMain = (handler: (tab: FileTabHandoff) => void) =>
  listenHere<FileTabHandoff>("files://return", handler);
