//! 界面层的会话模型：标签、pane 和它们的会话状态。

import type { PaneId, TreeNode } from "./splitTree";

/** 一个 pane 从哪来。重连要用同一套参数（分屏出来的 pane 也带自己的来源）。 */
export type PaneOrigin =
  | { kind: "empty" }
  | { kind: "host"; hostId: number; label: string }
  | { kind: "quick"; target: string; password: string; label: string };

export type PanePhase =
  | "idle" // 空 pane，或者在等第一个尺寸
  | "connecting"
  | "authenticating"
  | "connected"
  | "disconnected";

export interface PaneSession {
  phase: PanePhase;
  /** 状态栏文字（"连接中：第 2/3 跳 …"） */
  status: string;
  /** 断开原因；只有 disconnected 时有意义 */
  reason: string;
  /** 后端会话 ID；还没连上或者已经断开时是 null */
  sessionId: string | null;
  origin: PaneOrigin;
}

export function emptyPane(origin: PaneOrigin = { kind: "empty" }): PaneSession {
  return { phase: "idle", status: "", reason: "", sessionId: null, origin };
}

export interface Tab {
  id: string;
  title: string;
  /** 悬停提示（编辑器标签显示文件位置 / 符号链接指向）。没有就用 title。 */
  tooltip?: string;
  tree: TreeNode;
  /** 键盘输入进哪个 pane */
  focus: PaneId;
  /** SFTP 标签（M3-2）：不是终端标签，没有 tree 内容；终端标签是 null。 */
  sftp?: SftpTabInfo;
  /** 内置编辑器标签（M3-5）：对应后端的编辑会话。 */
  editor?: {
    editId: number;
    /** 打开它时正在看的标签；关闭这个编辑器时优先回到那里（它还在的话）。 */
    originTabId?: string;
  };
  /** 输入广播：键盘输入同时发到这个标签的所有分屏。 */
  broadcast?: boolean;
}

/** SFTP 标签挂的信息（M3-2 第 2 节）。 */
export interface SftpTabInfo {
  /** 后端的 sftp_id */
  sftpId: string;
  hostId: number;
  /** 标签状态：connecting / ready / failed（事件驱动）。 */
  /** picking：「新建 SFTP」开出来、还没选主机（hostId 是 0）。 */
  phase: "picking" | "connecting" | "ready" | "failed";
  status: string;
  /** 从停靠面板拆回来时带过来的两栏目录（M3-3）。 */
  paths?: { local: string; remote: string };
}

/** 标签上的状态点：正在连（黄）> 已连上（绿）> 全断开（红）。 */
export function tabDot(phases: PanePhase[]): { className: string; title: string } {
  if (phases.some((p) => p === "connecting" || p === "authenticating")) {
    return { className: "bg-amber-400", title: "连接中" };
  }
  if (phases.some((p) => p === "connected")) {
    return { className: "bg-[#2fa16b]", title: "已连接" };
  }
  if (phases.some((p) => p === "disconnected")) {
    return { className: "bg-rose-500", title: "已断开" };
  }
  return { className: "bg-slate-600", title: "空标签" };
}
