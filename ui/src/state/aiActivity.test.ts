//! 后台标签的 AI 活动记账：活动点、「有新输出」点，以及按执行编号纠正位置后的挪动。

import { describe, expect, it } from "vitest";

import {
  EMPTY_ACTIVITY,
  markTabSeen,
  reduceActivity,
  unseenTabs,
  type ActivityEvent,
  type ActivityState,
} from "./aiActivity";
import type { TargetRef } from "./aiTarget";

/** 会话 s-web 在标签 tab-web，s-db 在 tab-db；其他找不到。 */
const tabOf = (target: TargetRef): string | null =>
  ({ "s-web": "tab-web", "s-db": "tab-db" })[target.sessionId ?? ""] ?? null;

const run = (events: ActivityEvent[], from: ActivityState = EMPTY_ACTIVITY) =>
  events.reduce((state, event) => reduceActivity(state, event, tabOf), from);

const start = (callId: string, sessionId: string): ActivityEvent => ({
  kind: "start",
  callId,
  targetId: "host-x",
  sessionId,
});

describe("活动点与「有新输出」点", () => {
  it("开始时亮活动点，结束时收掉并在所在标签标「有新输出」", () => {
    const running = run([start("c1", "s-db")]);
    expect(Object.keys(running.running)).toEqual(["c1"]);
    const ended = run([{ kind: "end", callId: "c1" }], running);
    expect(ended.running).toEqual({});
    expect(unseenTabs(ended)).toEqual(["tab-db"]);
  });

  it("没开始过的调用结束：什么都不变", () => {
    const state = run([{ kind: "end", callId: "nope" }]);
    expect(state).toBe(EMPTY_ACTIVITY);
  });

  it("切到标签看了就清掉那个标签的点；别的调用在同一标签上的点一并清掉，别的标签不动", () => {
    let state = run([
      start("c1", "s-db"),
      start("c2", "s-db"),
      start("c3", "s-web"),
      { kind: "end", callId: "c1" },
      { kind: "end", callId: "c2" },
      { kind: "end", callId: "c3" },
    ]);
    expect(unseenTabs(state).sort()).toEqual(["tab-db", "tab-web"]);
    state = markTabSeen(state, "tab-db");
    expect(unseenTabs(state)).toEqual(["tab-web"]);
    expect(markTabSeen(state, "tab-db")).toBe(state);
  });

  it("回合结束：在跑的收掉，「有新输出」的点留着", () => {
    const state = run([
      start("c1", "s-db"),
      { kind: "end", callId: "c1" },
      start("c2", "s-web"),
      { kind: "reset" },
    ]);
    expect(state.running).toEqual({});
    expect(unseenTabs(state)).toEqual(["tab-db"]);
  });
});

describe("位置被纠正以后点跟着挪（AI-SCOPE-3）", () => {
  it("正在跑的调用：再发一次开始，活动点从错的标签挪到正确的", () => {
    const state = run([start("c1", "s-db"), start("c1", "s-web")]);
    expect(Object.values(state.running)).toEqual([{ targetId: "host-x", sessionId: "s-web" }]);
    const ended = run([{ kind: "end", callId: "c1" }], state);
    expect(unseenTabs(ended)).toEqual(["tab-web"]);
  });

  it("已经结束的调用：「有新输出」点从错的标签收回，标到正确的标签", () => {
    const ended = run([start("c1", "s-db"), { kind: "end", callId: "c1" }]);
    expect(unseenTabs(ended)).toEqual(["tab-db"]);
    const moved = run(
      [{ kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" }],
      ended,
    );
    expect(unseenTabs(moved)).toEqual(["tab-web"]);
    // 再纠正一次（对调回来）也一样
    const back = run(
      [{ kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-db" }],
      moved,
    );
    expect(unseenTabs(back)).toEqual(["tab-db"]);
  });

  it("错的标签上还有别的调用的点：只收回这个调用的，别的留着", () => {
    const ended = run([
      start("c1", "s-db"),
      { kind: "end", callId: "c1" },
      start("c2", "s-db"),
      { kind: "end", callId: "c2" },
    ]);
    const moved = run(
      [{ kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" }],
      ended,
    );
    expect(unseenTabs(moved).sort()).toEqual(["tab-db", "tab-web"]);
  });

  it("用户已经看过错的标签（点被清掉了）：纠正后正确的标签仍然标上", () => {
    let state = run([start("c1", "s-db"), { kind: "end", callId: "c1" }]);
    state = markTabSeen(state, "tab-db");
    expect(unseenTabs(state)).toEqual([]);
    state = run([{ kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" }], state);
    expect(unseenTabs(state)).toEqual(["tab-web"]);
  });

  it("纠正成「没有位置」：只收回错的点", () => {
    const ended = run([start("c1", "s-db"), { kind: "end", callId: "c1" }]);
    const cleared = run(
      [{ kind: "retarget", callId: "c1", targetId: null, sessionId: null }],
      ended,
    );
    expect(unseenTabs(cleared)).toEqual([]);
    // 之后拿到正确的位置还能标上
    const again = run(
      [{ kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" }],
      cleared,
    );
    expect(unseenTabs(again)).toEqual(["tab-web"]);
  });

  it("结束时还没有位置的调用（回报比结束晚到）：拿到位置后在实际执行的标签上补「有新输出」", () => {
    const state = run([
      { kind: "retarget", callId: "c9", targetId: "host-y", sessionId: "s-web" },
    ]);
    expect(unseenTabs(state)).toEqual(["tab-web"]);
  });

  it("回合已经结束、回报才到：卡片位置纠正了，点也跟着挪到正确的标签", () => {
    const state = run([
      start("c1", "s-db"),
      { kind: "end", callId: "c1" },
      { kind: "reset" },
      { kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" },
    ]);
    expect(unseenTabs(state)).toEqual(["tab-web"]);
  });

  it("回合结束后才到的回报、而且没有位置：不标点", () => {
    const state = run([
      { kind: "reset" },
      { kind: "retarget", callId: "c9", targetId: null, sessionId: null },
    ]);
    expect(unseenTabs(state)).toEqual([]);
  });

  it("还在跑的调用收到纠正：不当成结束（由 start 管）", () => {
    const state = run([
      start("c1", "s-db"),
      { kind: "retarget", callId: "c1", targetId: "host-y", sessionId: "s-web" },
    ]);
    expect(unseenTabs(state)).toEqual([]);
    expect(Object.keys(state.running)).toEqual(["c1"]);
  });
});
