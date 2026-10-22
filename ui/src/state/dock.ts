//! SFTP 标签停靠到终端标签的判定和目录搬运（M3-3 第 2 节）：纯函数，方便单测。

import type { PaneSession, Tab } from "./sessions";
import { paneIds } from "./splitTree";
import type { SftpPaths } from "../components/sftp/SftpTabView";

/** 一个终端标签里有哪些主机（分屏可以连不同主机）。 */
export function hostsInTab(tab: Tab, panes: Record<string, PaneSession>): number[] {
  if (tab.sftp) return [];
  const hosts: number[] = [];
  for (const paneId of paneIds(tab.tree)) {
    const origin = panes[paneId]?.origin;
    if (origin?.kind === "host" && !hosts.includes(origin.hostId)) hosts.push(origin.hostId);
  }
  return hosts;
}

export type DockVerdict =
  | { ok: true; paneId: string }
  | { ok: false; reason: "not-terminal" | "other-host" };

/**
 * SFTP 标签能不能停靠到这个终端标签：只能停靠到**同一台主机**的终端标签上。
 * 通过时同时给出目标标签里该主机的 pane（停靠后让它拿焦点，面板才会显示这台主机）。
 */
export function canDockOnto(
  sftpHostId: number,
  target: Tab,
  panes: Record<string, PaneSession>,
): DockVerdict {
  if (target.sftp) return { ok: false, reason: "not-terminal" };
  const focused = panes[target.focus]?.origin;
  if (focused?.kind === "host" && focused.hostId === sftpHostId) {
    return { ok: true, paneId: target.focus };
  }
  for (const paneId of paneIds(target.tree)) {
    const origin = panes[paneId]?.origin;
    if (origin?.kind === "host" && origin.hostId === sftpHostId) return { ok: true, paneId };
  }
  return { ok: false, reason: "other-host" };
}

export const OTHER_HOST_NOTICE = "这个 SFTP 连的是另一台主机";

/**
 * 新开一台主机的停靠会话时的初始状态。「跟随终端目录」默认**关**：开着的话终端里一 cd，
 * 远端栏就被拉走，用户在面板里正看着的目录会突然换掉；要跟随由用户自己勾上。
 */
export function newDockSessionDefaults(): { paths: SftpPaths; follow: boolean } {
  return { paths: { local: "", remote: "" }, follow: false };
}

/** 停靠时两栏目录原样带走；没记过的用空串（面板按默认目录开）。 */
export function carryPaths(known: SftpPaths | undefined): SftpPaths {
  return { local: known?.local ?? "", remote: known?.remote ?? "" };
}
