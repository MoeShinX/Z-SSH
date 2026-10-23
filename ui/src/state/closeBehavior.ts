//! 关闭主窗口时的判断（UI-4）：缩到托盘，还是走退出流程。
//!
//! 纯函数，App.tsx 的 `onCloseRequested` 和托盘「退出」都用它们，所以点 × 和托盘「退出」
//! 走的是同一套确认代码。分工见 src-tauri/src/tray.rs 开头：关闭由前端决定，Rust 不碰主窗口的
//! CloseRequested。

import { editConfirmMessage } from "./editDirty";
import type { PaneSession, Tab } from "./sessions";
import { paneIds } from "./splitTree";

/** 关闭主窗口时：`tray` 缩到托盘（默认），`quit` 退出程序。 */
export type CloseAction = "tray" | "quit";

export const DEFAULT_CLOSE_ACTION: CloseAction = "tray";

/** 库里读回来的值：只有明确是 `quit` 才退出，缺省或乱七八糟的值一律按默认（缩到托盘）。 */
export function parseCloseAction(raw: unknown): CloseAction {
  return raw === "quit" ? "quit" : DEFAULT_CLOSE_ACTION;
}

export interface CloseRequest {
  /** 设置里选的「关闭主窗口时」。 */
  action: CloseAction;
  /**
   * 这次关闭是「要退出程序」触发的：托盘「退出」，或者退出流程里用户已经确认、
   * 正在真的关窗口。这种情况下不管设置怎么选都不能再缩到托盘。
   */
  quitting: boolean;
  /** 第一次缩到托盘的提示已经出过了。 */
  hintShown: boolean;
}

export type CloseDecision =
  /** 缩到托盘；`showHint` = 这是第一次，要提示一下程序还在右下角。 */
  | { type: "hide"; showHint: boolean }
  /** 走退出流程（未保存修改 / 传输中的确认，没有要确认的就直接退出）。 */
  | { type: "quit" };

export function decideClose(request: CloseRequest): CloseDecision {
  if (request.quitting || request.action === "quit") return { type: "quit" };
  return { type: "hide", showHint: !request.hintShown };
}

/** 退出流程里下一步该做什么。 */
export type QuitVerdict =
  /** 没有要确认的了：放行，窗口关掉、程序退出。 */
  | { type: "go" }
  /** 有没保存的编辑器内容：先问保存还是放弃（其他要确认的事写在同一个对话框里）。 */
  | { type: "ask-unsaved" }
  /** 有活动连接 / 进行中的传输 / 未保存的远程编辑：合成一个对话框问是否退出。 */
  | { type: "ask-busy" };

/** 退出会断开的活动连接，按类别数。 */
export interface ActiveConnections {
  /** 已连接或正在连接的终端会话。 */
  terminals: number;
  /** 正在运行（含启动中）的端口转发。 */
  forwards: number;
  /** SFTP 连接：主窗口的 SFTP 标签 + Z-SFTP 文件窗口里打开着的。 */
  sftp: number;
}

export const NO_CONNECTIONS: ActiveConnections = { terminals: 0, forwards: 0, sftp: 0 };

export function totalConnections(connections: ActiveConnections): number {
  return connections.terminals + connections.forwards + connections.sftp;
}

/** 「3 个终端、1 个端口转发」：只列数量大于 0 的类别。 */
export function describeConnections(connections: ActiveConnections): string {
  const parts: string[] = [];
  if (connections.terminals > 0) parts.push(`${connections.terminals} 个终端`);
  if (connections.forwards > 0) parts.push(`${connections.forwards} 个端口转发`);
  if (connections.sftp > 0) parts.push(`${connections.sftp} 个 SFTP 连接`);
  return parts.join("、");
}

/** 终端会话算不算活动连接：已连上，或者正在连（退出会把它们断开 / 取消）。 */
export function isLivePanePhase(phase: string): boolean {
  return phase === "connecting" || phase === "authenticating" || phase === "connected";
}

/** 转发算不算活动：已经在跑或正在启动（失败的、停了的不算）。 */
export function isLiveForwardPhase(phase: string): boolean {
  return phase === "starting" || phase === "running";
}

/** SFTP 标签算不算活动连接：正在连或已连上（选主机的空标签、连失败的不算）。 */
export function isLiveSftpPhase(phase: string): boolean {
  return phase === "connecting" || phase === "ready";
}

/**
 * 数一数退出会断开的连接。终端只数终端标签里的 pane（SFTP 标签、编辑器标签里的占位 pane 不算）；
 * SFTP 数主窗口的 SFTP 标签加文件窗口里报上来的会话；转发数由调用方从后端状态里挑出活动的。
 */
export function countConnections(input: {
  tabs: readonly Pick<Tab, "tree" | "sftp" | "editor">[];
  panes: Readonly<Record<string, Pick<PaneSession, "phase">>>;
  fileWindowSessions: number;
  forwardPhases: readonly string[];
}): ActiveConnections {
  let terminals = 0;
  let sftp = input.fileWindowSessions;
  for (const tab of input.tabs) {
    if (tab.sftp) {
      if (isLiveSftpPhase(tab.sftp.phase)) sftp += 1;
      continue;
    }
    if (tab.editor) continue;
    for (const id of paneIds(tab.tree)) {
      const pane = input.panes[id];
      if (pane && isLivePanePhase(pane.phase)) terminals += 1;
    }
  }
  return {
    terminals,
    forwards: input.forwardPhases.filter(isLiveForwardPhase).length,
    sftp,
  };
}

export interface QuitState {
  /** 编辑器里没保存的文档数。 */
  unsavedDocs: number;
  /** 进行中的 SFTP 传输数。 */
  transfers: number;
  /** SFTP 编辑里有改动没回传的文件数。 */
  dirtyEdits: number;
  /** 退出会断开的活动连接。 */
  connections: ActiveConnections;
  /** 用户已经在退出确认上点过确认。 */
  confirmed: boolean;
}

/**
 * 退出流程要不要问、问哪个对话框。点 × 退出和托盘「退出」都走这一个函数。
 * 有没保存的文档时用「保存 / 不保存 / 取消」那个对话框，其余原因由 [[quitNotes]] 写进同一个对话框，
 * 不会先后弹两个。
 */
export function quitVerdict(state: QuitState): QuitVerdict {
  if (state.confirmed) return { type: "go" };
  if (state.unsavedDocs > 0) return { type: "ask-unsaved" };
  if (needsBusyConfirm(state)) return { type: "ask-busy" };
  return { type: "go" };
}

function needsBusyConfirm(state: QuitState): boolean {
  return state.transfers > 0 || state.dirtyEdits > 0 || totalConnections(state.connections) > 0;
}

/** 除未保存文档之外，退出还会造成什么：一条一行。没有就是空数组。 */
export function quitNotes(state: QuitState): string[] {
  const notes: string[] = [];
  const total = totalConnections(state.connections);
  if (total > 0) {
    notes.push(`还有 ${total} 个连接（${describeConnections(state.connections)}），退出会全部断开。`);
  }
  if (state.transfers > 0) notes.push(`还有 ${state.transfers} 个传输未完成，退出会取消这些传输。`);
  if (state.dirtyEdits > 0) {
    notes.push(`还有 ${state.dirtyEdits} 个文件的修改还没上传，退出会丢掉这些修改。`);
  }
  return notes;
}

/** 只有传输 / 远程编辑要确认时沿用原来的标题和按钮；有活动连接就统一成「确定退出？」+「退出」。 */
export function quitConfirmText(state: QuitState): {
  title: string;
  message: string;
  confirmLabel: string;
} {
  const notes = quitNotes(state);
  const hasConnections = totalConnections(state.connections) > 0;
  if (!hasConnections) {
    const text = editConfirmMessage(state.transfers, state.dirtyEdits, "退出程序");
    return { ...text, confirmLabel: state.dirtyEdits > 0 ? "退出并放弃" : "退出并取消" };
  }
  return {
    title: "确定退出 Z-SSH？",
    message: `${notes.join("\n")}\n确定退出？`,
    confirmLabel: "退出",
  };
}

/** 关闭 Z-SFTP 文件窗口时：`close` 断开里面的 SFTP 再关窗口（默认），`merge` 把标签原样交回主窗口。 */
export type FileWindowCloseAction = "close" | "merge";

export const DEFAULT_FILE_WINDOW_CLOSE_ACTION: FileWindowCloseAction = "close";

/** 库里读回来的值：只有明确是 `merge` 才合并，缺省或乱七八糟的值一律按默认（断开并关闭）。 */
export function parseFileWindowCloseAction(raw: unknown): FileWindowCloseAction {
  return raw === "merge" ? "merge" : DEFAULT_FILE_WINDOW_CLOSE_ACTION;
}

/**
 * 关文件窗口时要不要先把标签合并回主窗口：设置选了合并、窗口里确实有标签、
 * 并且不是"合并已经做完、窗口正在真的关"那一次。
 */
export function shouldMergeOnClose(
  action: FileWindowCloseAction,
  tabCount: number,
  closing: boolean,
): boolean {
  return action === "merge" && tabCount > 0 && !closing;
}
