/** @vitest-environment jsdom */
//! 焦点进终端：输入法切到英文；离开终端：恢复。设置里关掉就什么都不做。
//! 请求排队、分屏不闪、补恢复等队列细节在 terminal/imeFocus.test.ts，这里验接线。

import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ime = vi.hoisted(() => ({ calls: [] as boolean[], textarea: null as HTMLTextAreaElement | null }));

vi.mock("../api/ime", () => ({
  imeTerminalFocus: vi.fn(async (focused: boolean) => {
    ime.calls.push(focused);
    return focused;
  }),
}));
vi.mock("../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));
vi.mock("../terminal/terminal", () => {
  const terminal = {
    cols: 80,
    rows: 24,
    options: {} as Record<string, unknown>,
    unicode: { activeVersion: "" },
    get textarea() {
      return ime.textarea ?? undefined;
    },
    open(el: HTMLElement) {
      ime.textarea = document.createElement("textarea");
      el.appendChild(ime.textarea);
    },
    loadAddon() {},
    onData: () => ({ dispose() {} }),
    onSelectionChange: () => ({ dispose() {} }),
    attachCustomKeyEventHandler() {},
    write(_bytes: Uint8Array, onRendered?: () => void) {
      onRendered?.();
    },
    writeln() {},
    getSelection: () => "",
    paste() {},
    focus() {},
    dispose() {},
  };
  return {
    createTerminal: () => ({ terminal, fitAddon: { fit() {} } }),
    loadWebglRenderer: () => {},
    applyTerminalTheme: () => {},
  };
});

import { useSettings } from "../stores/settings";
import TerminalView from "./TerminalView";

beforeEach(() => {
  vi.useFakeTimers();
  ime.calls.length = 0;
  ime.textarea = null;
  if (!document.fonts) {
    Object.defineProperty(document, "fonts", { configurable: true, value: { ready: Promise.resolve() } });
  }
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  document.body.innerHTML = "";
});

/** 走完离开终端后的等待和 IPC 队列。 */
const settle = () => act(async () => { await vi.advanceTimersByTimeAsync(300); });

const mount = () =>
  render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} onReady={() => {}} />);

describe("进终端自动切到英文输入", () => {
  it("终端获得焦点切英文，失去焦点恢复", async () => {
    useSettings.setState({ terminal_ime_english: true });
    mount();
    act(() => ime.textarea?.focus());
    act(() => ime.textarea?.blur());
    await settle();
    expect(ime.calls).toEqual([true, false]);
  });

  it("设置里关掉：不碰输入法", async () => {
    useSettings.setState({ terminal_ime_english: false });
    mount();
    act(() => ime.textarea?.focus());
    act(() => ime.textarea?.blur());
    await settle();
    expect(ime.calls).toEqual([]);
  });

  it("焦点在终端时关掉开关：立即恢复原来的模式（不等 blur）", async () => {
    useSettings.setState({ terminal_ime_english: true });
    mount();
    act(() => ime.textarea?.focus());
    act(() => useSettings.setState({ terminal_ime_english: false }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(ime.calls).toEqual([true, false]);
  });

  it("终端 → AI 输入框 → 终端 → AI 输入框快速切换：最后发出的是恢复", async () => {
    useSettings.setState({ terminal_ime_english: true });
    mount();
    const ai = document.createElement("textarea");
    document.body.appendChild(ai);
    const term = ime.textarea as HTMLTextAreaElement;
    act(() => term.focus());
    act(() => ai.focus());
    act(() => term.focus());
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([true, false, true, false]);
  });
});
