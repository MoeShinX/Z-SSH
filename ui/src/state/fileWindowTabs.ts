//! 文件窗口里的标签（纯函数，方便单测）。

import type { FileTabHandoff, FileWindowSession } from "../api/fileWindow";

export interface FileWindowTab {
  id: string;
  sftpId: string;
  hostId: number;
  title: string;
  phase: "connecting" | "ready" | "failed";
  status: string;
  paths: { local: string; remote: string };
}

/** 主窗口交过来的一个标签 → 文件窗口的标签。没有 sftpId 的要文件窗口自己去连。 */
export function tabFromHandoff(handoff: FileTabHandoff, id: string): FileWindowTab {
  const ready = handoff.sftp_id !== "";
  return {
    id,
    sftpId: handoff.sftp_id,
    hostId: handoff.host_id,
    title: handoff.title,
    phase: ready ? "ready" : "connecting",
    status: ready ? "" : "连接中…",
    paths: { local: handoff.local, remote: handoff.remote },
  };
}

/** 报给主窗口（编辑器保存时找会话）和后端（窗口意外销毁时兜底关掉）的会话清单。 */
export function sessionsOf(tabs: FileWindowTab[]): FileWindowSession[] {
  return tabs
    .filter((tab) => tab.phase === "ready" && tab.sftpId !== "")
    .map((tab) => ({ sftp_id: tab.sftpId, host_id: tab.hostId }));
}

/** 关掉一个标签后选中哪个：关的不是当前的就不变；是当前的就选左边那个，没有就右边那个。 */
export function activeAfterClose(tabs: FileWindowTab[], closing: string, active: string): string {
  if (closing !== active) return active;
  const index = tabs.findIndex((tab) => tab.id === closing);
  const rest = tabs.filter((tab) => tab.id !== closing);
  return (rest[index - 1] ?? rest[index] ?? rest[0])?.id ?? "";
}

/** 一个标签移回主窗口时交过去的内容（目录用它现在的）。 */
export function handoffOf(
  tab: FileWindowTab,
  paths: { local: string; remote: string } | undefined,
): FileTabHandoff {
  const current = paths ?? tab.paths;
  return {
    sftp_id: tab.phase === "ready" ? tab.sftpId : "",
    host_id: tab.hostId,
    title: tab.title,
    local: current.local,
    remote: current.remote,
  };
}

/** 指针松开的位置在不在窗口外面（拖出去 = 换窗口）。 */
export function isOutsideWindow(x: number, y: number, width: number, height: number): boolean {
  return x < 0 || y < 0 || x >= width || y >= height;
}

/** 标签上只显示主机名：文件窗口里全是 SFTP，"(SFTP)" / "（文件）"后缀是多余的。 */
export function fileTabLabel(title: string): string {
  return title.replace(/\s*(\(SFTP\)|（文件）)$/, "");
}
