//! AI 执行位置 ↔ 终端标签 / pane 的对应（AI-SCOPE-1、AI-SCOPE-2）：「查看终端」跳转和后台
//! 标签的活动提示共用。
//!
//! 输入是执行层回报的实际执行位置（目标 ID + 执行用的会话），这里**不再猜**"这台主机的哪个
//! 会话"：有会话就只认那个会话；回报时还没有会话（这台主机没有可用会话，后端在自动连接）
//! 才按目标找新开的那个标签。
import { parseHostTargetId } from "../components/ai/aiHelpers";
import type { PaneSession, Tab } from "./sessions";
import { paneIds } from "./splitTree";

/** 执行层回报的执行位置：逻辑目标 ID + 执行用的会话（还没有会话为 null）。 */
export interface TargetRef {
  targetId: string;
  sessionId: string | null;
}

/**
 * 执行位置对应哪个 pane：
 * - 有会话 ID：就是持有这个会话的 pane，会话已经关了返回 null（不去猜这台主机的别的会话）；
 * - 没有（回报时这台主机还没有可用会话，后端自动连接）：这台主机正在连的 pane 优先，其次已连上的，
 *   同级取最新打开的。快速连接这类没有主机记录的目标没有会话就找不到。
 */
export function paneForTarget(
  panes: Record<string, PaneSession>,
  ref: TargetRef,
): string | null {
  if (ref.sessionId) {
    for (const [paneId, pane] of Object.entries(panes)) {
      if (pane.sessionId === ref.sessionId) return paneId;
    }
    return null;
  }
  const hostId = parseHostTargetId(ref.targetId);
  if (hostId === null) return null;
  let connecting: string | null = null;
  let connected: string | null = null;
  for (const [paneId, pane] of Object.entries(panes)) {
    if (pane.origin.kind !== "host" || pane.origin.hostId !== hostId) continue;
    if (pane.phase === "connecting" || pane.phase === "authenticating") connecting = paneId;
    else if (pane.phase === "connected") connected = paneId;
  }
  return connecting ?? connected;
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

/** 执行位置所在的标签。 */
export function tabForTarget(
  tabs: Tab[],
  panes: Record<string, PaneSession>,
  ref: TargetRef,
): string | null {
  return locateTarget(tabs, panes, ref)?.tabId ?? null;
}

export type TabAiBadge = "running" | "unseen";

/**
 * 各标签上的 AI 活动提示：正在跑的（`running`）和跑完还没看过的（`unseen`）。
 * 用户正看着的那个标签（终端视图下的活动标签）不提示。
 */
export function aiTabBadges(
  tabs: Tab[],
  panes: Record<string, PaneSession>,
  runningTargets: TargetRef[],
  unseenTabs: string[],
  watchedTabId: string | null,
): Record<string, TabAiBadge> {
  const badges: Record<string, TabAiBadge> = {};
  for (const tabId of unseenTabs) {
    if (tabId !== watchedTabId && tabs.some((tab) => tab.id === tabId)) badges[tabId] = "unseen";
  }
  for (const ref of runningTargets) {
    const tabId = tabForTarget(tabs, panes, ref);
    if (tabId && tabId !== watchedTabId) badges[tabId] = "running";
  }
  return badges;
}
