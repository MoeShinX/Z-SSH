import { describe, expect, it } from "vitest";

import { keepSubscription, leasesToRelease, type StatsLease } from "./statsLease";

const lease = (subscriptionId: number, sessionId: string): StatsLease => ({
  subscriptionId,
  sessionId,
});

describe("状态栏采集租约：该退哪些（C13）", () => {
  it("关掉开关时退掉全部租约，包括焦点 pane 的", () => {
    const leases = new Map([
      ["p1", lease(1, "s1")],
      ["p2", lease(2, "s2")],
    ]);
    expect(leasesToRelease(leases, false, "p1", "s1")).toEqual(["p1", "p2"]);
  });

  it("开着时只保留焦点 pane 当前会话的那条", () => {
    const leases = new Map([
      ["p1", lease(1, "s1")],
      ["p2", lease(2, "s2")],
    ]);
    expect(leasesToRelease(leases, true, "p1", "s1")).toEqual(["p2"]);
  });

  it("焦点 pane 重连换了会话：旧会话的租约要退", () => {
    const leases = new Map([["p1", lease(1, "s-old")]]);
    expect(leasesToRelease(leases, true, "p1", "s-new")).toEqual(["p1"]);
  });

  it("没有焦点 pane / 焦点 pane 没有会话：全部退", () => {
    const leases = new Map([["p1", lease(1, "s1")]]);
    expect(leasesToRelease(leases, true, null, null)).toEqual(["p1"]);
    expect(leasesToRelease(leases, true, "p1", null)).toEqual(["p1"]);
  });
});

describe("状态栏采集租约：异步回来的订阅要不要（C13）", () => {
  const request = { generation: 3, pane: "p1", sessionId: "s1" };

  it("情况没变：登记", () => {
    expect(
      keepSubscription(request, { generation: 3, paneSession: "s1", alreadyLeased: false }),
    ).toBe(true);
  });

  it("请求发出后开关被关掉（代次变了）：即使又打开、会话没变也不要", () => {
    expect(
      keepSubscription(request, { generation: 4, paneSession: "s1", alreadyLeased: false }),
    ).toBe(false);
  });

  it("pane 关了或换了会话：不要", () => {
    expect(
      keepSubscription(request, { generation: 3, paneSession: null, alreadyLeased: false }),
    ).toBe(false);
    expect(
      keepSubscription(request, { generation: 3, paneSession: "s2", alreadyLeased: false }),
    ).toBe(false);
  });

  it("同一个 pane 已经有租约了：多出来的这条不要，免得旧订阅 ID 被覆盖后再也退不掉", () => {
    expect(
      keepSubscription(request, { generation: 3, paneSession: "s1", alreadyLeased: true }),
    ).toBe(false);
  });
});
