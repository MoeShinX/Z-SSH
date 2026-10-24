//! 发一条命令进终端标签的公共流程：回车执行、多行确认、没连上提示。

import { describe, expect, it, vi } from "vitest";

import type { QuickCommand } from "./quickCommands";
import { sendCommandToTab, type SendCommandDeps, type SendConfirmRequest } from "./sendCommand";

const command = (text: string, extra: Partial<QuickCommand> = {}): QuickCommand => ({
  id: "c",
  name: "撰写栏",
  command: text,
  enter: true,
  ...extra,
});

function deps(patch: Partial<SendCommandDeps> = {}) {
  const asked: SendConfirmRequest[] = [];
  const value: SendCommandDeps = {
    send: vi.fn(() => 1),
    confirmMultiline: true,
    askConfirm: (request) => asked.push(request),
    notify: vi.fn(),
    ...patch,
  };
  return { value, asked };
}

describe("sendCommandToTab", () => {
  it("单行：补回车发出去，发完调 afterSend", () => {
    const { value } = deps();
    const after = vi.fn();
    sendCommandToTab(value, command("ls -la"), false, after);
    expect(value.send).toHaveBeenCalledWith("ls -la\r", false);
    expect(value.notify).not.toHaveBeenCalled();
    expect(after).toHaveBeenCalledWith(1);
  });

  it("发到所有分屏：all 原样传给 send", () => {
    const { value } = deps();
    sendCommandToTab(value, command("uptime"), true);
    expect(value.send).toHaveBeenCalledWith("uptime\r", true);
  });

  it("enter 为 false 只填入不回车", () => {
    const { value } = deps();
    sendCommandToTab(value, command("rm -rf ", { enter: false }), false);
    expect(value.send).toHaveBeenCalledWith("rm -rf ", false);
  });

  it("一个窗格都没发出去：提示，afterSend 收到 0", () => {
    const { value } = deps({ send: vi.fn(() => 0) });
    const after = vi.fn();
    sendCommandToTab(value, command("ls"), false, after);
    expect(value.notify).toHaveBeenLastCalledWith("当前窗格还没连上");
    expect(after).toHaveBeenCalledWith(0);
    sendCommandToTab(value, command("ls"), true, after);
    expect(value.notify).toHaveBeenLastCalledWith("这个标签里还没有连上的窗格");
  });

  it("多行且要确认：先弹确认，点了发送才发，发完调 afterSend", () => {
    const { value, asked } = deps();
    const after = vi.fn();
    sendCommandToTab(value, command("cd /tmp\nls\npwd"), true, after);
    expect(value.send).not.toHaveBeenCalled();
    expect(asked).toHaveLength(1);
    expect(asked[0].title).toBe("发送多行命令「撰写栏」？");
    expect(asked[0].message).toBe("3 行，每一行都会回车执行，发到这个标签的所有分屏。");
    expect(after).not.toHaveBeenCalled();
    asked[0].onConfirm();
    expect(value.send).toHaveBeenCalledWith("cd /tmp\rls\rpwd\r", true);
    expect(after).toHaveBeenCalledWith(1);
  });

  it("多行但没开确认：直接发", () => {
    const { value, asked } = deps({ confirmMultiline: false });
    sendCommandToTab(value, command("a\nb"), false);
    expect(asked).toHaveLength(0);
    expect(value.send).toHaveBeenCalledWith("a\rb\r", false);
  });

  it("末尾多出来的换行不算多行", () => {
    const { value, asked } = deps();
    sendCommandToTab(value, command("ls\n\n"), false);
    expect(asked).toHaveLength(0);
    expect(value.send).toHaveBeenCalledWith("ls\r", false);
  });
});
