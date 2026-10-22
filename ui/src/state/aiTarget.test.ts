//! AI 目标 ↔ 标签 / pane（AI-SCOPE-1）：查看终端的定位、后台标签的活动提示。

import { describe, expect, it } from "vitest";

import { aiTabBadges, locateTarget, paneForTarget, tabForTargetId } from "./aiTarget";
import { emptyPane, type PaneSession, type Tab } from "./sessions";

function pane(hostId: number | null, sessionId: string | null, phase: PaneSession["phase"]): PaneSession {
  return {
    ...emptyPane(hostId === null ? { kind: "empty" } : { kind: "host", hostId, label: `h${hostId}` }),
    sessionId,
    phase,
  };
}

const tabs: Tab[] = [
  { id: "t1", title: "a", tree: { kind: "pane", id: "p1" }, focus: "p1" },
  { id: "t2", title: "b", tree: { kind: "pane", id: "p2" }, focus: "p2" },
  { id: "t3", title: "c", tree: { kind: "pane", id: "p3" }, focus: "p3" },
];
const panes: Record<string, PaneSession> = {
  p1: pane(3, "s1", "connected"),
  p2: pane(9, null, "connecting"), // 自动连接的后台标签，还在连
  p3: pane(9, "s3", "connected"),
};

describe("定位目标所在的标签和窗格", () => {
  it("按会话 ID", () => {
    expect(locateTarget(tabs, panes, { sessionId: "s1", hostId: 3 })).toEqual({ tabId: "t1", paneId: "p1" });
  });

  it("会话 ID 找不到时按主机，已连上的优先", () => {
    expect(locateTarget(tabs, panes, { sessionId: "gone", hostId: 9 })).toEqual({ tabId: "t3", paneId: "p3" });
  });

  it("还没连上、正在连的也能切过去看连接过程", () => {
    const connecting = { ...panes, p3: pane(9, null, "disconnected") };
    expect(locateTarget(tabs, connecting, { sessionId: null, hostId: 9 })).toEqual({ tabId: "t2", paneId: "p2" });
  });

  it("没有打开的终端返回 null", () => {
    expect(locateTarget(tabs, panes, { sessionId: null, hostId: 77 })).toBeNull();
    expect(paneForTarget(panes, { sessionId: null, hostId: null })).toBeNull();
  });

  it("目标 ID 可以是会话 ID 或 host-<id>", () => {
    expect(tabForTargetId(tabs, panes, "s1")).toBe("t1");
    expect(tabForTargetId(tabs, panes, "host-9")).toBe("t3");
    expect(tabForTargetId(tabs, panes, "host-5")).toBeNull();
  });
});

describe("后台标签的 AI 活动提示", () => {
  it("后台标签执行中 running，结束未看 unseen；活动标签（正看着）不提示", () => {
    const badges = aiTabBadges(tabs, panes, ["s1", "host-9"], ["t2"], "t1");
    expect(badges).toEqual({ t3: "running", t2: "unseen" });
    // 正看着 t3：它不提示
    expect(aiTabBadges(tabs, panes, ["host-9"], [], "t3")).toEqual({});
  });

  it("同一标签既在跑又有没看过的，显示 running", () => {
    expect(aiTabBadges(tabs, panes, ["s1"], ["t1"], "t2")).toEqual({ t1: "running" });
  });

  it("已经关掉的标签不提示", () => {
    expect(aiTabBadges(tabs, panes, [], ["t-closed"], "t1")).toEqual({});
  });

  it("没有正看着的标签（主机页 / 设置页）时活动标签也提示", () => {
    expect(aiTabBadges(tabs, panes, ["s1"], [], null)).toEqual({ t1: "running" });
  });
});
