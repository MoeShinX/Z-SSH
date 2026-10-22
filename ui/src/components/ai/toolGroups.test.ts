//! 工具调用分组（AI-UX-1）的纯函数：失败判定、摘要文字、进行中文字、分组断开规则。

import { describe, expect, it } from "vitest";

import type { ChatItem, ToolCall } from "./chatHistory";
import { callFailed, groupChatItems, progressText, summarizeCalls } from "./toolGroups";

const call = (id: string, tool: string, over: Partial<ToolCall> = {}): ToolCall => ({
  callId: id,
  tool,
  input: tool.endsWith("run_command") ? { command: "ls" } : { path: "/etc/hosts" },
  output: "exit_code: 0",
  status: "done",
  ...over,
});
const tool = (c: ToolCall): ChatItem => ({ kind: "tool", call: c });

describe("callFailed", () => {
  it("退出码非 0、报错、被拒绝、被停止算失败；成功和进行中不算", () => {
    expect(callFailed(call("a", "zssh_run_command"))).toBe(false);
    expect(callFailed(call("a", "zssh_run_command", { output: "exit_code: 2" }))).toBe(true);
    expect(callFailed(call("a", "zssh_run_command", { status: "error" }))).toBe(true);
    expect(callFailed(call("a", "zssh_run_command", { status: "rejected" }))).toBe(true);
    expect(callFailed(call("a", "zssh_run_command", { status: "stopped" }))).toBe(true);
    expect(callFailed(call("a", "zssh_run_command", { status: "running", output: "" }))).toBe(false);
    // 没有退出码的工具（读文件等）成功就是成功
    expect(callFailed(call("a", "zssh_read_file", { output: "内容" }))).toBe(false);
  });
});

describe("summarizeCalls", () => {
  it("命令和文件工具分开数", () => {
    expect(summarizeCalls([call("1", "zssh_run_command"), call("2", "zssh_run_command")])).toBe(
      "执行了 2 条命令",
    );
    expect(
      summarizeCalls([
        call("1", "zssh_run_command"),
        call("2", "zssh_read_file"),
        call("3", "zssh_write_file"),
        call("4", "zssh_list_dir"),
        call("5", "zssh_stat"),
        call("6", "zssh_read_terminal"),
      ]),
    ).toBe("执行了 1 条命令、读了 1 个文件、写了 1 个文件、查看了 2 处目录或文件信息、1 次其他查询");
  });
});

describe("progressText", () => {
  it("命令超过 60 个字符截断加省略号，不超过就原样", () => {
    expect(progressText(call("1", "zssh_run_command", { input: { command: "uptime" } }))).toBe(
      "正在执行：uptime",
    );
    const text = progressText(call("1", "zssh_run_command", { input: { command: "x".repeat(80) } }));
    expect(text).toBe(`正在执行：${"x".repeat(60)}…`);
  });

  it("文件工具显示路径，没有可显示的内容就显示工具名", () => {
    expect(progressText(call("1", "zssh_read_file", { input: { path: "/etc/hosts" } }))).toBe(
      "正在执行：/etc/hosts",
    );
    expect(progressText(call("1", "zssh_list_targets", { input: {} }))).toBe("正在执行：列出服务器");
  });
});

describe("groupChatItems", () => {
  it("连续的已完成调用并成一组；文字、等待批准、进行中把它们隔开", () => {
    const rows = groupChatItems(
      [
        { kind: "user", text: "q" },
        tool(call("1", "zssh_run_command")),
        tool(call("2", "zssh_run_command")),
        tool(call("3", "zssh_run_command", { status: "running", output: "" })),
        tool(call("4", "zssh_run_command")),
        { kind: "assistant", text: "a" },
        tool(call("5", "zssh_run_command")),
      ],
      () => false,
    );
    expect(rows.map((row) => row.type)).toEqual(["text", "group", "progress", "group", "text", "group"]);
    const first = rows[1];
    expect(first.type === "group" && first.calls.map((c) => c.callId)).toEqual(["1", "2"]);
  });

  it("等待批准的调用单独成完整卡片", () => {
    const rows = groupChatItems(
      [tool(call("1", "zssh_run_command")), tool(call("2", "zssh_run_command", { status: "running", output: "" }))],
      (c) => c.callId === "2",
    );
    expect(rows.map((row) => row.type)).toEqual(["group", "card"]);
  });
});
