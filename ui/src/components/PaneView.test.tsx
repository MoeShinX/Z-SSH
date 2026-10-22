/** @vitest-environment jsdom */
//! 连接期间量到的尺寸不能丢：会话连上之后必须补发一次 term_resize。
//!
//! M1-08b 的 bug 后半段：终端量到真实尺寸的时候 `session_open` 还在路上
//! （`sessionId` 是空的），那次 resize 被直接丢掉，于是远端 PTY 一直停在
//! 初始尺寸上。重连和分屏 duplicate 走的是同一条路。

import { act, render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { termResize } from "../api/session";
import type { PaneSession } from "../state/sessions";
import PaneView from "./PaneView";

vi.mock("../api/session", () => ({
  termAck: vi.fn(async () => {}),
  termResize: vi.fn(async () => {}),
  termWrite: vi.fn(async () => {}),
}));

/** 假的终端：测试手动触发一次量测（TerminalView 自己有用例）。 */
const fake = vi.hoisted(() => ({
  ready: null as ((cols: number, rows: number) => void) | null,
  resize: null as ((cols: number, rows: number) => void) | null,
}));

vi.mock("./TerminalView", () => ({
  default: (props: {
    onReady?: (cols: number, rows: number) => void;
    onResize: (cols: number, rows: number) => void;
  }) => {
    fake.ready = props.onReady ?? null;
    fake.resize = props.onResize;
    return null;
  },
}));

const resizeMock = vi.mocked(termResize);

function sessionOf(patch: Partial<PaneSession>): PaneSession {
  return {
    phase: "idle",
    status: "",
    reason: "",
    sessionId: null,
    origin: { kind: "host", hostId: 1, label: "web-1" },
    ...patch,
  };
}

beforeEach(() => {
  fake.ready = null;
  fake.resize = null;
  resizeMock.mockClear();
});

function renderPane(session: PaneSession) {
  const onReady = vi.fn();
  const onResize = vi.fn();
  const props = {
    paneId: "pane-1",
    session,
    terminalTheme: {},
    focused: false,
    onFocus: () => {},
    onReady,
    onResize,
    onReconnect: () => {},
    onClose: () => {},
    registerWriter: () => {},
  };
  const view = render(<PaneView {...props} />);
  return { ...view, props, onReady, onResize };
}

describe("PaneView 的尺寸补发", () => {
  it("连接中量到的新尺寸：连上之后补发一次", () => {
    const { rerender, props, onReady } = renderPane(sessionOf({}));

    // 终端量到第一份尺寸，上层就拿它去开会话
    act(() => fake.ready?.(100, 30));
    expect(onReady).toHaveBeenCalledWith("pane-1", 100, 30);

    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);

    // 连接期间容器又量了一次（字体加载完、第一帧画完都会发生）
    act(() => fake.resize?.(120, 35));
    expect(resizeMock).not.toHaveBeenCalled(); // 还没有 sessionId，发不出去

    // 连上：补发最新的那次
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "connected", sessionId: "s1" })} />,
    );
    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s1", 120, 35);
  });

  it("连接期间尺寸没变：连上之后不重复发", () => {
    const { rerender, props } = renderPane(sessionOf({}));

    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "connected", sessionId: "s1" })} />,
    );

    expect(resizeMock).not.toHaveBeenCalled();
  });

  it("连上之后量到新尺寸：立刻发出去", () => {
    const { rerender, props } = renderPane(sessionOf({}));

    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "connected", sessionId: "s1" })} />,
    );

    act(() => fake.resize?.(90, 30));

    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s1", 90, 30);
  });

  it("重连：连接期间的新尺寸同样在连上后补发", () => {
    const { rerender, props } = renderPane(sessionOf({}));

    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "connected", sessionId: "s1" })} />,
    );

    // 断开，重连时用当时的尺寸开会话
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "网络中断" })} />,
    );
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    act(() => fake.resize?.(80, 30));
    rerender(
      <PaneView {...props} session={sessionOf({ phase: "connected", sessionId: "s2" })} />,
    );

    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s2", 80, 30);
  });
});

describe("pane 右上角的悬停按钮", () => {
  it("连着的终端上不放任何按钮（关闭窗格用 Ctrl+Shift+W 或标签菜单，SFTP 入口在标题栏）", () => {
    const { container } = renderPane(sessionOf({ phase: "connected", sessionId: "s1" }));
    expect(container.querySelectorAll("button")).toHaveLength(0);
    expect(container.textContent).not.toContain("×");
    expect(container.textContent).not.toContain("SFTP");
  });
});

describe("PaneView 的外观", () => {
  it("没有外框；底色是终端背景；只有分屏时焦点窗格顶上有一道细线", () => {
    const { container, rerender, props } = renderPane(sessionOf({ phase: "connected" }));
    const root = container.querySelector(".workspace-pane") as HTMLElement;
    rerender(<PaneView {...props} terminalTheme={{ background: "#1F1F1F" }} focused />);
    expect(root.className).not.toMatch(/\bring-1\b/);
    expect(root.style.backgroundColor).toBe("rgb(31, 31, 31)");
    expect(container.querySelector('[data-section="pane-focus-line"]')).toBeNull();
    rerender(<PaneView {...props} terminalTheme={{ background: "#1F1F1F" }} focused split />);
    expect(container.querySelector('[data-section="pane-focus-line"]')).not.toBeNull();
    rerender(<PaneView {...props} terminalTheme={{ background: "#1F1F1F" }} focused={false} split />);
    expect(container.querySelector('[data-section="pane-focus-line"]')).toBeNull();
  });
});
