//! 执行层回报 ↔ 工具卡片的对应（AI-SCOPE-2 第 2 节）。

import { describe, expect, it } from "vitest";

import {
  callKey,
  execTargetOf,
  execTargetOfApproval,
  findCallForReport,
  trackCall,
  type TrackedCall,
} from "./execTargets";

const report = (over: Partial<{ tool: string; key: string; target_id: string }> = {}) => ({
  tool: "run_command",
  key: "ls",
  target_id: "host-3",
  ...over,
});

const call = (over: Partial<TrackedCall> & { callId: string }): TrackedCall => ({
  tool: "run_command",
  key: "ls",
  targetId: null,
  ended: false,
  claimed: false,
  ...over,
});

describe("登记带目标的调用", () => {
  it("键：命令原文 / 路径原文 / 读终端为空串", () => {
    expect(callKey("zssh_run_command", { command: "ls -la" })).toBe("ls -la");
    expect(callKey("mcp__zssh__read_file", { path: "/etc/hosts" })).toBe("/etc/hosts");
    expect(callKey("zssh_read_terminal", {})).toBe("");
    expect(callKey("zssh_stat", {})).toBeNull();
  });

  it("不带目标的工具不登记", () => {
    expect(trackCall("c1", "zssh_list_targets", {})).toBeNull();
    expect(trackCall("c1", "zssh_run_command", { command: "ls" })).toMatchObject({
      callId: "c1",
      tool: "run_command",
      key: "ls",
      targetId: null,
      ended: false,
      claimed: false,
    });
    expect(trackCall("c2", "zssh_run_command", { command: "ls", target_id: "host-4" })?.targetId).toBe(
      "host-4",
    );
  });
});

describe("回报属于哪一次调用", () => {
  it("同工具、同键", () => {
    const calls = [call({ callId: "a", key: "pwd" }), call({ callId: "b", key: "ls" })];
    expect(findCallForReport(calls, report())?.callId).toBe("b");
    expect(findCallForReport(calls, report({ key: "pwd" }))?.callId).toBe("a");
    expect(findCallForReport(calls, report({ key: "uptime" }))).toBeUndefined();
    expect(findCallForReport(calls, report({ tool: "read_file" }))).toBeUndefined();
  });

  it("已经认领过的不再认领；同一条命令连发两次按先后各认一个", () => {
    const calls = [call({ callId: "a" }), call({ callId: "b" })];
    const first = findCallForReport(calls, report());
    expect(first?.callId).toBe("a");
    (first as TrackedCall).claimed = true;
    expect(findCallForReport(calls, report())?.callId).toBe("b");
  });

  it("还在跑的优先于已经返回的（晚到的回报不抢新的调用）", () => {
    const calls = [call({ callId: "old", ended: true }), call({ callId: "new" })];
    expect(findCallForReport(calls, report())?.callId).toBe("new");
    // 只有已返回的：回报晚到也认
    expect(findCallForReport([call({ callId: "old", ended: true })], report())?.callId).toBe("old");
  });

  it("输入里取不到键的调用当通配，但精确匹配优先", () => {
    const calls = [call({ callId: "blind", key: null }), call({ callId: "exact" })];
    expect(findCallForReport(calls, report())?.callId).toBe("exact");
    expect(findCallForReport([calls[0]], report())?.callId).toBe("blind");
  });
});

describe("同一条命令发给不同主机", () => {
  it("回报乱序到达：按输入里的 target_id 各认各的", () => {
    const calls = [
      call({ callId: "a", key: "uptime", targetId: "host-3" }),
      call({ callId: "b", key: "uptime", targetId: "host-4" }),
    ];
    // host-4 的回报先到
    const first = findCallForReport(calls, report({ key: "uptime", target_id: "host-4" }));
    expect(first?.callId).toBe("b");
    (first as TrackedCall).claimed = true;
    expect(findCallForReport(calls, report({ key: "uptime", target_id: "host-3" }))?.callId).toBe("a");
  });

  it("回报的目标和输入里写明的不一致：不认领", () => {
    const calls = [call({ callId: "a", key: "uptime", targetId: "host-3" })];
    expect(findCallForReport(calls, report({ key: "uptime", target_id: "host-4" }))).toBeUndefined();
  });

  it("写明目标的优先于省略目标的；省略目标的可以认任何目标的回报", () => {
    const calls = [
      call({ callId: "blank", key: "uptime", targetId: null }),
      call({ callId: "named", key: "uptime", targetId: "host-4" }),
    ];
    expect(findCallForReport(calls, report({ key: "uptime", target_id: "host-4" }))?.callId).toBe("named");
    expect(findCallForReport(calls, report({ key: "uptime", target_id: "host-3" }))?.callId).toBe("blank");
  });
});

describe("回报 → 卡片上的执行位置", () => {
  it("字段对应", () => {
    expect(
      execTargetOf(
        {
          scope: "s",
          tool: "run_command",
          key: "ls",
          target_id: "host-3",
          session_id: "sess-2",
          host_label: "web-1",
        },
        true,
      ),
    ).toEqual({ targetId: "host-3", sessionId: "sess-2", label: "web-1", multi: true });
  });

  it("审批事件带的执行位置；老后端没带就是 null", () => {
    expect(
      execTargetOfApproval({ target_id: "host-3", session_id: null, host_label: "web-1" }),
    ).toEqual({ targetId: "host-3", sessionId: null, label: "web-1", multi: true });
    expect(
      execTargetOfApproval({
        target_id: undefined as unknown as string,
        session_id: null,
        host_label: "web-1",
      }),
    ).toBeNull();
  });
});
