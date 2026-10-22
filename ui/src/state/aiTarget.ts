//! AI 目标 ↔ 终端标签 / pane 的对应（AI-SCOPE-1）：「查看终端」跳转和后台标签的活动提示共用。
import { parseHostTargetId } from "../components/ai/aiHelpers";
import type { PaneSession, Tab } from "./sessions";
import { paneIds } from "./splitTree";

export interface TargetRef {
  sessionId: string | null;
  hostId: number | null;
}

/**
 * 目标对应哪个 pane：先按会话 ID；没有（或会话已关）再按主机，已连上的优先，
 * 正在连的也算（跳过去能看连接过程）。找不到返回 null。
 */
export function paneForTarget(
  panes: Record<string, PaneSession>,
  ref: TargetRef,
): string | null {
  if (ref.sessionId) {
    for (const [paneId, pane] of Object.entries(panes)) {
      if (pane.sessionId === ref.sessionId) return paneId;
    }
  }
  if (ref.hostId === null) return null;
  let fallback: string | null = null;
  for (const [paneId, pane] of Object.entries(panes)) {
    if (pane.origin.kind !== "host" || pane.origin.hostId !== ref.hostId) continue;
    if (pane.phase === "connected" && pane.sessionId) return paneId;
    if (pane.phase !== "disconnected" && fallback === null) fallback = paneId;
  }
  return fallback;
}

/** pane 所在的终端标签（SFTP / 编辑器标签没有 pane）。 */
export function tabOfPane(tabs: Tab[], paneId: string): Tab | null {
  return (
    tabs.find((tab) => !tab.sftp && !tab.editor && paneIds(tab.tree).includes(paneId)) ?? null
  );
}

/** 「查看终端」要切到的标签和 pane。 */
export function locateTarget(
  tabs: Tab[],
  panes: Record<string, PaneSession>,
  ref: TargetRef,
): { tabId: string; paneId: string } | null {
  const paneId = paneForTarget(panes, ref);
  if (!paneId) return null;
  const tab = tabOfPane(tabs, paneId);
  return tab ? { tabId: tab.id, paneId } : null;
}

/** 目标 ID（会话 ID 或 `host-<id>`）所在的标签。 */
export function tabForTargetId(
  tabs: Tab[],
  panes: Record<string, PaneSession>,
  targetId: string,
): string | null {
  const hostId = parseHostTargetId(targetId);
  const located = locateTarget(
    tabs,
    panes,
    hostId !== null ? { sessionId: null, hostId } : { sessionId: targetId, hostId: null },
  );
  return located?.tabId ?? null;
}

export type TabAiBadge = "running" | "unseen";

/**
 * 各标签上的 AI 活动提示：正在跑的（`running`）和跑完还没看过的（`unseen`）。
 * 用户正看着的那个标签（终端视图下的活动标签）不提示。
 */
export function aiTabBadges(
  tabs: Tab[],
  panes: Record<string, PaneSession>,
  runningTargets: string[],
  unseenTabs: string[],
  watchedTabId: string | null,
): Record<string, TabAiBadge> {
  const badges: Record<string, TabAiBadge> = {};
  for (const tabId of unseenTabs) {
    if (tabId !== watchedTabId && tabs.some((tab) => tab.id === tabId)) badges[tabId] = "unseen";
  }
  for (const targetId of runningTargets) {
    const tabId = tabForTargetId(tabs, panes, targetId);
    if (tabId && tabId !== watchedTabId) badges[tabId] = "running";
  }
  return badges;
}
