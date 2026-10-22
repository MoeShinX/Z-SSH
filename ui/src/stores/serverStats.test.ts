//! 状态栏数据：推送只换掉那个会话的数据（别的会话对象不变），网速曲线有上限，关掉的会话清掉。

import { beforeEach, describe, expect, it } from "vitest";

import type { ServerStats } from "../api/session";
import { NET_HISTORY_MAX } from "../components/terminal/ServerOverviewPanel";
import { useServerStats } from "./serverStats";

const stats = (rx: number) => ({ net_rx_speed: rx, net_tx_speed: rx * 2 }) as unknown as ServerStats;

beforeEach(() => {
  useServerStats.setState({ bySession: {}, history: {} });
});

describe("状态栏数据", () => {
  it("推送只换掉那个会话；别的会话的数据和曲线还是原来的对象", () => {
    const { push } = useServerStats.getState();
    push("a", stats(1));
    push("b", stats(5));
    const before = useServerStats.getState();
    push("a", stats(2));
    const after = useServerStats.getState();
    expect(after.bySession.a).toEqual(stats(2));
    expect(after.bySession.b).toBe(before.bySession.b);
    expect(after.history.b).toBe(before.history.b);
    expect(after.history.a).toEqual([
      { rx: 1, tx: 2 },
      { rx: 2, tx: 4 },
    ]);
  });

  it("网速曲线超过上限丢最旧的", () => {
    const { push } = useServerStats.getState();
    for (let i = 0; i < NET_HISTORY_MAX + 5; i += 1) push("a", stats(i));
    const history = useServerStats.getState().history.a;
    expect(history).toHaveLength(NET_HISTORY_MAX);
    expect(history[0].rx).toBe(5);
  });

  it("只留还活着的会话；没有要清的就不换对象（不触发重画）", () => {
    const { push, prune } = useServerStats.getState();
    push("a", stats(1));
    push("b", stats(2));
    prune(new Set(["b"]));
    expect(Object.keys(useServerStats.getState().bySession)).toEqual(["b"]);
    expect(Object.keys(useServerStats.getState().history)).toEqual(["b"]);
    const before = useServerStats.getState();
    prune(new Set(["b"]));
    expect(useServerStats.getState()).toBe(before);
  });
});
