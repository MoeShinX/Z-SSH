//! 执行层回报 ↔ 工具卡片的对应（AI-SCOPE-2 第 2 节；AI-SCOPE-3 按执行编号校正）。

import { describe, expect, it } from "vitest";

import type { ExecTargetReport } from "../../api/agent";
import type { ChatItem, ToolCall } from "./chatHistory";
import {
  applyResultExecId,
  assignReport,
  callKey,
  execTargetOf,
  execTargetOfApproval,
  findCallForReport,
  pairApproval,
  reportArrived,
  splitExecMarker,
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
  execId: null,
  resultExecId: null,
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
          exec_id: "ab12cd-1",
          tool: "run_command",
          key: "ls",
          target_id: "host-3",
          session_id: "sess-2",
          host_label: "web-1",
        },
        true,
      ),
    ).toEqual({
      execId: "ab12cd-1",
      targetId: "host-3",
      sessionId: "sess-2",
      label: "web-1",
      multi: true,
    });
  });

  it("审批事件带的执行位置；老后端没带就是 null", () => {
    expect(
      execTargetOfApproval({ target_id: "host-3", session_id: null, host_label: "web-1" }),
    ).toEqual({ execId: "", targetId: "host-3", sessionId: null, label: "web-1", multi: true });
    expect(
      execTargetOfApproval({
        exec_id: "ab12cd-5",
        target_id: "host-3",
        session_id: "s1",
        host_label: "web-1",
      }),
    ).toEqual({ execId: "ab12cd-5", targetId: "host-3", sessionId: "s1", label: "web-1", multi: true });
    expect(
      execTargetOfApproval({
        target_id: undefined as unknown as string,
        session_id: null,
        host_label: "web-1",
      }),
    ).toBeNull();
  });
});

const full = (over: Partial<ExecTargetReport> & { exec_id: string }): ExecTargetReport => ({
  scope: "s",
  tool: "run_command",
  key: "uptime",
  target_id: "host-3",
  session_id: "s-web",
  host_label: "web-1",
  ...over,
});

const ledger = (...list: ExecTargetReport[]) => new Map(list.map((r) => [r.exec_id, r]));

describe("工具结果末尾的执行编号标记", () => {
  it("拆出编号并去掉那一行（含前面的换行）", () => {
    expect(splitExecMarker("exit_code: 0\nok\n[zssh exec ab12cd-7]")).toEqual({
      text: "exit_code: 0\nok",
      execId: "ab12cd-7",
    });
    expect(splitExecMarker("[zssh exec ab12cd-7]")).toEqual({ text: "", execId: "ab12cd-7" });
    expect(splitExecMarker("out\r\n[zssh exec ab12cd-7]\n")).toEqual({
      text: "out",
      execId: "ab12cd-7",
    });
  });

  it("没有标记原样返回；只认最后一行，输出里恰好出现同样的文字不算", () => {
    expect(splitExecMarker("plain")).toEqual({ text: "plain", execId: null });
    const quoted = "log: [zssh exec evil-1] was here\n[zssh exec real-2]";
    expect(splitExecMarker(quoted)).toEqual({
      text: "log: [zssh exec evil-1] was here",
      execId: "real-2",
    });
    const notLast = "[zssh exec evil-1]\nmore text";
    expect(splitExecMarker(notLast)).toEqual({ text: notLast, execId: null });
    // 格式不对（带空格、过长）不认
    expect(splitExecMarker("x\n[zssh exec a b]").execId).toBeNull();
    expect(splitExecMarker("x\n[zssh exec " + "a".repeat(65) + "]").execId).toBeNull();
  });
});

describe("按执行编号校正配对（AI-SCOPE-3）", () => {
  it("省略 target_id 的同一条命令调了两次、回报乱序：结果回来后各自对回自己的回报", () => {
    // 同一条命令两次，中间切换了默认主机：启发式只能按先后配，配反了。
    const r1 = full({ exec_id: "p-1", target_id: "host-3", session_id: "s-web", host_label: "web-1" });
    const r2 = full({ exec_id: "p-2", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    const a = call({ callId: "a", key: "uptime" });
    const b = call({ callId: "b", key: "uptime" });
    const calls = [a, b];
    const reports = ledger(r2, r1);
    // 回报乱序：r2 先到，被临时配给先出现的 a；r1 后到配给 b。
    assignReport(calls, reports, a, r2);
    assignReport(calls, reports, b, r1);
    expect([a.execId, b.execId]).toEqual(["p-2", "p-1"]);
    // a 的结果说它是 p-1：a 改成 r1，原来占着 r1 的 b 拿 a 原来的 r2（对调）。
    const changes = applyResultExecId(calls, reports, "a", "p-1");
    expect(changes).toEqual([
      { callId: "a", report: r1 },
      { callId: "b", report: r2 },
    ]);
    expect([a.execId, b.execId]).toEqual(["p-1", "p-2"]);
    // b 的结果也说它是 p-2：已经对了，不再有变化。
    expect(applyResultExecId(calls, reports, "b", "p-2")).toEqual([]);
  });

  it("结果指明的编号和临时配对一致：没有变化", () => {
    const r1 = full({ exec_id: "p-1" });
    const a = call({ callId: "a" });
    assignReport([a], ledger(r1), a, r1);
    expect(applyResultExecId([a], ledger(r1), "a", "p-1")).toEqual([]);
    expect(a.resultExecId).toBe("p-1");
  });

  it("回报还没到：只记下编号，回报到了按编号配（不再按启发式抢别的卡片）", () => {
    const early = call({ callId: "early", key: "uptime" });
    const late = call({ callId: "late", key: "uptime" });
    const calls = [early, late];
    const reports = new Map<string, ExecTargetReport>();
    // early 的结果先回来，说它是 p-2，但 p-2 的回报还没到。
    expect(applyResultExecId(calls, reports, "early", "p-2")).toEqual([]);
    const r1 = full({ exec_id: "p-1", target_id: "host-3" });
    reports.set("p-1", r1);
    // p-1 到了：early 的结果已经指明别的编号，不能认它；轮到 late。
    const first = reportArrived(calls, reports, r1);
    expect(first?.call.callId).toBe("late");
    const r2 = full({ exec_id: "p-2", target_id: "host-4", session_id: "s-db", host_label: "db-1" });
    reports.set("p-2", r2);
    const second = reportArrived(calls, reports, r2);
    expect(second?.call.callId).toBe("early");
    expect(second?.changes).toEqual([{ callId: "early", report: r2 }]);
  });

  it("没有哪张卡片能认的回报返回 null（调用方先存着）", () => {
    const r1 = full({ exec_id: "p-1", tool: "read_file", key: "/x" });
    expect(reportArrived([call({ callId: "a" })], ledger(r1), r1)).toBeNull();
  });

  it("本卡片原来没有回报、而回报被别人占着：别人先空出来，等它自己的回报", () => {
    const r1 = full({ exec_id: "p-1" });
    const holder = call({ callId: "holder", claimed: true, execId: "p-1" });
    const mine = call({ callId: "mine" });
    const changes = assignReport([holder, mine], ledger(r1), mine, r1);
    expect(changes).toEqual([
      { callId: "mine", report: r1 },
      { callId: "holder", report: null },
    ]);
    expect(holder).toMatchObject({ execId: null, claimed: false });
  });

  it("占着的卡片结果已经指明别的编号：不拿对调来的回报，先空着", () => {
    const r1 = full({ exec_id: "p-1" });
    const r2 = full({ exec_id: "p-2" });
    const holder = call({ callId: "holder", claimed: true, execId: "p-1", resultExecId: "p-9" });
    const mine = call({ callId: "mine", claimed: true, execId: "p-2" });
    const changes = assignReport([holder, mine], ledger(r1, r2), mine, r1);
    expect(changes[1]).toEqual({ callId: "holder", report: null });
    expect(holder.execId).toBeNull();
  });
});

describe("确认请求配工具卡片：只按执行编号（AI-SCOPE-3 追加）", () => {
  const toolItem = (over: Partial<ToolCall> & { callId: string }): ChatItem => ({
    kind: "tool",
    call: {
      tool: "zssh_run_command",
      input: { command: "uptime" },
      output: "",
      status: "running",
      ...over,
    },
  });
  const exec = (execId: string) => ({
    execId,
    targetId: "host-3",
    sessionId: "s-web",
    label: "web-1",
    multi: true,
  });
  const approvalOf = (execId: string, tool = "run_command") => ({ reqId: 1, execId, tool });

  it("卡片认领的回报编号和确认请求的编号相同才配", () => {
    const item = toolItem({ callId: "a", exec: exec("p-1") });
    const hit = approvalOf("p-1");
    expect(pairApproval([approvalOf("p-2"), hit], (item as { call: ToolCall }).call, [item])).toBe(hit);
    expect(pairApproval([approvalOf("p-2")], (item as { call: ToolCall }).call, [item])).toBeUndefined();
  });

  it("编号相同也要工具一致；已结束 / 没有回报 / 老后端没带编号的卡片不配", () => {
    const call = (over: Partial<ToolCall>) =>
      (toolItem({ callId: "a", exec: exec("p-1"), ...over }) as { call: ToolCall }).call;
    expect(pairApproval([approvalOf("p-1", "write_file")], call({}), [])).toBeUndefined();
    expect(pairApproval([approvalOf("p-1")], call({ status: "done" }), [])).toBeUndefined();
    expect(pairApproval([approvalOf("p-1")], call({ exec: undefined }), [])).toBeUndefined();
    expect(pairApproval([approvalOf("")], call({ exec: exec("") }), [])).toBeUndefined();
    // 读文件之类不会有确认
    expect(
      pairApproval([approvalOf("p-1")], call({ tool: "zssh_read_file", input: { path: "/x" } }), []),
    ).toBeUndefined();
  });

  it("同样的命令在几处同时执行：卡片对不唯一，一律不配（确认请求单独显示）", () => {
    const a = toolItem({ callId: "a", exec: exec("p-1") });
    const b = toolItem({ callId: "b", exec: { ...exec("p-2"), targetId: "host-4", label: "db-1" } });
    const approvals = [approvalOf("p-1"), approvalOf("p-2")];
    expect(pairApproval(approvals, (a as { call: ToolCall }).call, [a, b])).toBeUndefined();
    expect(pairApproval(approvals, (b as { call: ToolCall }).call, [a, b])).toBeUndefined();
    // 另一条已经结束了，就不再模糊
    const done = toolItem({ callId: "b", status: "done" });
    expect(pairApproval(approvals, (a as { call: ToolCall }).call, [a, done])).toBe(approvals[0]);
    // 命令不同不算重名
    const other = toolItem({ callId: "c", input: { command: "df" } });
    expect(pairApproval(approvals, (a as { call: ToolCall }).call, [a, other])).toBe(approvals[0]);
  });

  it("写文件按路径原文判重名：同路径两处同时写不配", () => {
    const w = (id: string, execId: string) =>
      toolItem({
        callId: id,
        tool: "zssh_write_file",
        input: { path: "/etc/app.conf" },
        exec: exec(execId),
      });
    const a = w("a", "p-1");
    const b = w("b", "p-2");
    const approvals = [approvalOf("p-1", "write_file"), approvalOf("p-2", "write_file")];
    expect(pairApproval(approvals, (a as { call: ToolCall }).call, [a, b])).toBeUndefined();
    expect(pairApproval(approvals, (a as { call: ToolCall }).call, [a])).toBe(approvals[0]);
  });
});
