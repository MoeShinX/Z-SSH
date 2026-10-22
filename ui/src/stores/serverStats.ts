//! 服务器状态栏的数据（M1-10）：每个会话最近一条推送和最近几分钟的网速。
//!
//! 订阅、租约、退订仍由 App 管（见 state/statsLease.ts）；这里只存数据。状态栏和概览面板
//! 自己按会话 ID 订阅这份数据，一次推送只重画它们，不带着整个主界面重新渲染。

import { create } from "zustand";

import type { ServerStats } from "../api/session";
import { pushNetPoint, type NetPoint } from "../components/terminal/ServerOverviewPanel";

interface ServerStatsState {
  bySession: Record<string, ServerStats>;
  history: Record<string, NetPoint[]>;
  /** 收到某个会话的一条推送：换掉它的最新数据、网速曲线加一个点（其他会话的对象不动）。 */
  push: (sessionId: string, stats: ServerStats) => void;
  /** 只留还活着的会话（会话关了、换了就把旧数据清掉）。 */
  prune: (alive: ReadonlySet<string>) => void;
}

const NO_HISTORY: NetPoint[] = [];

export const useServerStats = create<ServerStatsState>((set) => ({
  bySession: {},
  history: {},
  push: (sessionId, stats) =>
    set((state) => ({
      bySession: { ...state.bySession, [sessionId]: stats },
      history: {
        ...state.history,
        [sessionId]: pushNetPoint(state.history[sessionId] ?? NO_HISTORY, {
          rx: stats.net_rx_speed,
          tx: stats.net_tx_speed,
        }),
      },
    })),
  prune: (alive) =>
    set((state) => {
      const stale = Object.keys(state.bySession)
        .concat(Object.keys(state.history))
        .filter((id) => !alive.has(id));
      if (stale.length === 0) return state;
      const bySession = { ...state.bySession };
      const history = { ...state.history };
      for (const id of stale) {
        delete bySession[id];
        delete history[id];
      }
      return { bySession, history };
    }),
}));

/** 某个会话的最新数据（没有会话或还没推送过是 null）。 */
export function useSessionStats(sessionId: string | null): ServerStats | null {
  return useServerStats((state) => (sessionId ? state.bySession[sessionId] ?? null : null));
}

/** 某个会话的网速曲线。 */
export function useSessionNetHistory(sessionId: string | null): NetPoint[] {
  return useServerStats((state) => (sessionId ? state.history[sessionId] ?? NO_HISTORY : NO_HISTORY));
}
