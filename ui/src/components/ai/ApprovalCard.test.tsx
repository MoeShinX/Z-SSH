/** @vitest-environment jsdom */
//! 执行命令的审批卡片（R-8 第 1 节）：命令里的不可见控制字符显示成可见的转义形式，
//! 用户看到的必须和实际要执行的字符一致（工具桥已经在审批前拒绝这类命令，这里是卡片
//! 的第二道防线）。

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PendingApproval } from "./AIChatPanel";
import { ToolCard } from "./AIChatPanel";
import { visibleCommandText } from "./aiHelpers";

afterEach(cleanup);

function runCall(command: string) {
  return {
    callId: "c1",
    tool: "zssh_run_command",
    input: { command },
    output: "",
    status: "running" as const,
  };
}

function approval(command: string): PendingApproval {
  return {
    reqId: 3,
    hostLabel: "web-1",
    command,
    danger: "dangerous",
    reasons: [],
    tool: "run_command",
  };
}

function renderApproval(command: string) {
  return render(
    <ToolCard
      call={runCall(command)}
      hosts={{}}
      approval={approval(visibleCommandText(command))}
      onAnswer={vi.fn()}
    />,
  );
}

describe("执行命令的审批卡片：不可见字符", () => {
  it("Ctrl+U、DEL、回车、C1、方向控制字符都显示成可见的转义形式", () => {
    const raw = "ls\x15rm -rf ~\x7f\r\u009b\u202e";
    const { container, getByText } = renderApproval(raw);
    const shown = container.querySelector("pre")?.textContent ?? "";
    expect(shown).toBe("ls\\x15rm -rf ~\\x7f\\x0d\\u{009b}\\u{202e}");
    // 卡片上不能留下原始不可见字符。
    expect(shown).not.toContain("\x15");
    expect(shown).not.toContain("\x7f");
    expect(shown).not.toContain("\r");
    expect(shown).not.toContain("\u009b");
    expect(shown).not.toContain("\u202e");
    // 确认按钮还在（卡片内容变了不影响审批入口）。
    expect(getByText("执行")).not.toBeNull();
    expect(getByText("拒绝")).not.toBeNull();
  });

  it("后端已经转义过的命令不会重复转义", () => {
    const { container } = render(
      <ToolCard
        call={runCall("ls\\x15rm -rf ~")}
        hosts={{}}
        approval={approval("ls\\x15rm -rf ~")}
        onAnswer={vi.fn()}
      />,
    );
    expect(container.querySelector("pre")?.textContent).toBe("ls\\x15rm -rf ~");
  });

  it("正常换行和制表保留原样，多行命令照旧显示", () => {
    const { container } = renderApproval("echo one\necho two\tend");
    expect(container.querySelector("pre")?.textContent).toBe("echo one\necho two\tend");
  });
});

describe("visibleCommandText", () => {
  it("普通命令原样返回", () => {
    expect(visibleCommandText("git status --short")).toBe("git status --short");
    expect(visibleCommandText("中文命令 && echo ✓")).toBe("中文命令 && echo ✓");
  });

  it("C0 / DEL 用 \\xNN，C1 和方向控制字符用 \\u{NNNN}", () => {
    expect(visibleCommandText("\x04\x1b\x7f")).toBe("\\x04\\x1b\\x7f");
    expect(visibleCommandText("\u0080\u009f")).toBe("\\u{0080}\\u{009f}");
    expect(visibleCommandText("\u202a\u202e\u2066\u2069")).toBe(
      "\\u{202a}\\u{202e}\\u{2066}\\u{2069}",
    );
  });
});
