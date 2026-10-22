//! 面板消息和历史会话之间的转换（R11）。

import { describe, expect, it } from "vitest";

import {
  formatHistoryTime,
  fromHistoryMessages,
  hasUserMessage,
  historyHostLabel,
  newHistoryId,
  toHistoryMessages,
  type ChatItem,
} from "./chatHistory";

const noMeta = () => ({});

describe("历史会话的转换", () => {
  it("面板消息存盘再读回来形状不变（工具卡片带名称、参数、结果、状态）", () => {
    const items: ChatItem[] = [
      { kind: "user", text: "看看磁盘" },
      { kind: "assistant", text: "好的" },
      {
        kind: "tool",
        call: {
          callId: "c1",
          tool: "zssh_run_command",
          input: { command: "df -h" },
          output: "exit_code: 0",
          status: "done",
          location: "terminal",
        },
      },
    ];
    const stored = toHistoryMessages(items);
    expect(stored[2]).toEqual({
      kind: "tool",
      tool: "zssh_run_command",
      input: { command: "df -h" },
      output: "exit_code: 0",
      status: "done",
    });
    const back = fromHistoryMessages(stored, (output) =>
      output.includes("exit_code") ? { location: "terminal" } : {},
    );
    expect(back[0]).toEqual(items[0]);
    expect(back[1]).toEqual(items[1]);
    expect(back[2]).toMatchObject({
      kind: "tool",
      call: { tool: "zssh_run_command", status: "done", location: "terminal" },
    });
  });

  it("存盘时还在跑的卡片和不认识的状态回看时显示已停止，不会一直转圈", () => {
    const back = fromHistoryMessages(
      [
        { kind: "tool", tool: "t", input: {}, output: "", status: "running" },
        { kind: "tool", tool: "t", input: {}, output: "", status: "奇怪" },
        { kind: "tool", tool: "t", input: {}, output: "", status: "rejected" },
      ],
      noMeta,
    );
    const statuses = back.map((item) => (item.kind === "tool" ? item.call.status : null));
    expect(statuses).toEqual(["stopped", "stopped", "rejected"]);
    // 每张卡片的 id 不重复。
    const ids = back.map((item) => (item.kind === "tool" ? item.call.callId : ""));
    expect(new Set(ids).size).toBe(3);
  });

  it("没有用户消息就不用存", () => {
    expect(hasUserMessage([{ kind: "assistant", text: "hi" }])).toBe(false);
    expect(hasUserMessage([{ kind: "user", text: "hi" }])).toBe(true);
  });

  it("主机名只取主机库里的主机；快速连接的标签名带地址，不存", () => {
    expect(historyHostLabel([{ hostId: 3, hostLabel: "web-1" }])).toBe("web-1");
    expect(historyHostLabel([{ hostId: null, hostLabel: "root@203.0.113.5:22" }])).toBeNull();
    expect(
      historyHostLabel([
        { hostId: null, hostLabel: "root@203.0.113.5:22" },
        { hostId: 7, hostLabel: "db-1" },
      ]),
    ).toBe("db-1");
    expect(historyHostLabel([])).toBeNull();
  });

  it("新 id 只含后端认的字符", () => {
    const id = newHistoryId();
    expect(id).toMatch(/^[A-Za-z0-9-]{1,64}$/);
    expect(newHistoryId()).not.toBe(id);
  });

  it("列表时间：今天只写时分，今年写月日，更早带年份", () => {
    const now = new Date(2026, 9, 19, 15, 0);
    expect(formatHistoryTime(new Date(2026, 9, 19, 9, 5).getTime(), now)).toBe("09:05");
    expect(formatHistoryTime(new Date(2026, 2, 1, 8, 30).getTime(), now)).toBe("3月1日 08:30");
    expect(formatHistoryTime(new Date(2025, 11, 31, 23, 59).getTime(), now)).toBe(
      "2025年12月31日 23:59",
    );
  });
});
