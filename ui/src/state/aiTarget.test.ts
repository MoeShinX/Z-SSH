//! AI 执行位置 ↔ 标签 / pane（AI-SCOPE-1、AI-SCOPE-2）：查看终端的定位、后台标签的活动提示。

import { describe, expect, it } from "vitest";

import { aiTabBadges, locateTarget, paneForTarget, tabForTarget } from "./aiTarget";
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
  { id: "t4", title: "d", tree: { kind: "pane", id: "p4" }, focus: "p4" },
];
const panes: Record<string, PaneSession> = {
  p1: pane(3, "s1", "connected"),
  p2: pane(9, null, "connecting"), // 自动连接的后台标签，还在连
  p3: pane(9, "s3", "connected"),
  p4: pane(3, "s4", "connected"), // 和 p1 同一台主机的另一个终端
};

const ref = (targetId: string, sessionId: string | null) => ({ targetId, sessionId });

describe("定位执行位置所在的标签和窗格", () => {
  it("有回报的会话就只认那个会话：同一台主机的两个终端不会混", () => {
    expect(locateTarget(tabs, panes, ref("host-3", "s1"))).toEqual({ tabId: "t1", paneId: "p1" });
    expect(locateTarget(tabs, panes, ref("host-3", "s4"))).toEqual({ tabId: "t4", paneId: "p4" });
  });

  it("回报的会话已经关了：不去猜这台主机的别的会话", () => {
    expect(locateTarget(tabs, panes, ref("host-3", "gone"))).toBeNull();
  });

  it("回报时还没有会话：按目标找，正在连的优先于已连上的", () => {
    expect(locateTarget(tabs, panes, ref("host-9", null))).toEqual({ tabId: "t2", paneId: "p2" });
    const done = { ...panes, p2: pane(9, "s2", "connected") };
    // 都已连上：取最新打开的
    expect(locateTarget(tabs, done, ref("host-9", null))).toEqual({ tabId: "t3", paneId: "p3" });
    const closed = { ...panes, p2: pane(9, null, "disconnected"), p3: pane(9, null, "disconnected") };
    expect(locateTarget(tabs, closed, ref("host-9", null))).toBeNull();
  });

  it("没有打开的终端返回 null；快速连接没有会话也找不到", () => {
    expect(locateTarget(tabs, panes, ref("host-77", null))).toBeNull();
    expect(paneForTarget(panes, ref("s-quick", null))).toBeNull();
  });

  it("快速连接的目标按会话找", () => {
    expect(tabForTarget(tabs, panes, ref("s1", "s1"))).toBe("t1");
  });
});

describe("后台标签的 AI 活动提示", () => {
  it("后台标签执行中 running，结束未看 unseen；活动标签（正看着）不提示", () => {
    const badges = aiTabBadges(tabs, panes, [ref("host-3", "s1"), ref("host-9", "s3")], ["t2"], "t1");
    expect(badges).toEqual({ t3: "running", t2: "unseen" });
    // 正看着 t3：它不提示
    expect(aiTabBadges(tabs, panes, [ref("host-9", "s3")], [], "t3")).toEqual({});
  });

  it("同一台主机多个终端：活动点亮在执行的那个终端所在的标签", () => {
    expect(aiTabBadges(tabs, panes, [ref("host-3", "s4")], [], "t2")).toEqual({ t4: "running" });
  });

  it("没有会话（自动连接中）：亮在正在连的那个标签", () => {
    expect(aiTabBadges(tabs, panes, [ref("host-9", null)], [], "t1")).toEqual({ t2: "running" });
  });

  it("同一标签既在跑又有没看过的，显示 running", () => {
    expect(aiTabBadges(tabs, panes, [ref("host-3", "s1")], ["t1"], "t2")).toEqual({ t1: "running" });
  });

  it("已经关掉的标签不提示", () => {
    expect(aiTabBadges(tabs, panes, [], ["t-closed"], "t1")).toEqual({});
  });

  it("没有正看着的标签（主机页 / 设置页）时活动标签也提示", () => {
    expect(aiTabBadges(tabs, panes, [ref("host-3", "s1")], [], null)).toEqual({ t1: "running" });
  });
});
