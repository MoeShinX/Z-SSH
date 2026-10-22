//! 后台标签的 AI 活动提示的记账（AI-SCOPE-1 第 4 节；AI-SCOPE-3 追加：按执行编号纠正位置后跟着挪）。
//!
//! 活动点（正在跑）和「有新输出」点（跑完还没看过）都按调用记：知道每个调用落在哪个目标上，
//! 回报晚到、按结果里的执行编号纠正配对以后，才能把已经点在错的标签上的小点收回来、
//! 挪到正确的标签。纯函数，方便单测；App 只负责把标签表交给 tabOf。

import type { TargetRef } from "./aiTarget";

/** AI 在终端里执行命令的开始 / 结束（面板发给 App）。 */
export type ActivityEvent =
  | { kind: "start"; callId: string; targetId: string; sessionId: string | null }
  | { kind: "end"; callId: string }
  /**
   * 已经结束的调用，执行位置被纠正了（回报晚到，或者按结果里的执行编号改了配对）：
   * targetId 为 null 表示这次调用现在没有对应的位置，只收回错的小点。
   */
  | { kind: "retarget"; callId: string; targetId: string | null; sessionId: string | null }
  | { kind: "reset" };

export interface ActivityState {
  /** 还在执行的调用 → 它落在的目标。 */
  running: Record<string, TargetRef>;
  /** 已经结束的调用 → 结束时记的目标（后续纠正位置要用；null = 现在没有位置）。 */
  ended: Record<string, TargetRef | null>;
  /** 「有新输出」点：调用 → 标出这个点的标签。 */
  unseen: Record<string, string>;
}

export const EMPTY_ACTIVITY: ActivityState = { running: {}, ended: {}, unseen: {} };

/** 处理一个活动事件。tabOf 把目标换成它现在所在的标签（找不到返回 null）。 */
export function reduceActivity(
  state: ActivityState,
  event: ActivityEvent,
  tabOf: (target: TargetRef) => string | null,
): ActivityState {
  switch (event.kind) {
    case "start":
      return {
        ...state,
        running: {
          ...state.running,
          [event.callId]: { targetId: event.targetId, sessionId: event.sessionId },
        },
      };
    case "end": {
      const target = state.running[event.callId];
      if (target === undefined) return state;
      const { [event.callId]: _done, ...running } = state.running;
      const tabId = tabOf(target);
      return {
        running,
        ended: { ...state.ended, [event.callId]: target },
        unseen: tabId ? { ...state.unseen, [event.callId]: tabId } : state.unseen,
      };
    }
    case "retarget": {
      // 结束时没有位置的调用（当时没有回报，没点过灯）不补点：晚到的回报不亮灯
      if (!(event.callId in state.ended)) return state;
      const { [event.callId]: _old, ...unseen } = state.unseen;
      const target =
        event.targetId === null
          ? null
          : { targetId: event.targetId, sessionId: event.sessionId };
      const tabId = target ? tabOf(target) : null;
      return {
        ...state,
        ended: { ...state.ended, [event.callId]: target },
        unseen: tabId ? { ...unseen, [event.callId]: tabId } : unseen,
      };
    }
    case "reset":
      // 回合结束 / 出错：不再有在跑的调用，也不会再有纠正；「有新输出」的点留着，等用户去看
      if (Object.keys(state.running).length === 0 && Object.keys(state.ended).length === 0) {
        return state;
      }
      return { running: {}, ended: {}, unseen: state.unseen };
  }
}

/** 现在有「有新输出」点的标签。 */
export function unseenTabs(state: ActivityState): string[] {
  return [...new Set(Object.values(state.unseen))];
}

/** 用户切到这个标签看了：清掉它上面的「有新输出」点。 */
export function markTabSeen(state: ActivityState, tabId: string): ActivityState {
  const keys = Object.keys(state.unseen).filter((callId) => state.unseen[callId] === tabId);
  if (keys.length === 0) return state;
  const unseen = { ...state.unseen };
  for (const key of keys) delete unseen[key];
  return { ...state, unseen };
}
