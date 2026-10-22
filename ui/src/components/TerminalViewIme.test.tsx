/** @vitest-environment jsdom */
//! 默认英文输入的接线：设置打开时，终端和别的输入框都按英文区处理，AI 输入框（带标记）跟随用户；
//! 设置里关掉就什么都不做。请求排队、延迟、补恢复等细节在 terminal/imeFocus.test.ts。

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
import { useImeDefaultEnglish } from "../terminal/useImeDefaultEnglish";
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

/** 走完延迟和 IPC 队列。 */
const settle = () => act(async () => { await vi.advanceTimersByTimeAsync(300); });

function Probe() {
  useImeDefaultEnglish();
  return null;
}

const mount = () =>
  render(
    <>
      <Probe />
      <TerminalView theme={{}} onData={() => {}} onResize={() => {}} onReady={() => {}} />
    </>,
  );

const makeAi = () => {
  const ai = document.createElement("textarea");
  ai.setAttribute("data-ime", "native");
  document.body.appendChild(ai);
  return ai;
};

describe("默认使用英文输入", () => {
  it("终端获得焦点切英文，焦点进了 AI 输入框才恢复", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: true });
    mount();
    const ai = makeAi();
    act(() => ime.textarea?.focus());
    act(() => ime.textarea?.blur());
    await settle();
    // 只是失去焦点（焦点不在任何输入框里）：待恢复，不发
    expect(ime.calls).toEqual([true]);
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([true, false]);
  });

  it("终端 → 按钮 → AI 输入框：到了输入框才恢复", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: true });
    mount();
    const button = document.createElement("button");
    const ai = makeAi();
    document.body.append(button);
    act(() => ime.textarea?.focus());
    act(() => button.focus());
    await settle();
    expect(ime.calls).toEqual([true]);
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([true, false]);
  });

  it("终端以外的输入框（搜索框、表单）同样是英文区；从 AI 输入框回到它们再进入", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: true });
    mount();
    const search = document.createElement("input");
    document.body.appendChild(search);
    const ai = makeAi();
    act(() => search.focus());
    await settle();
    expect(ime.calls).toEqual([true]);
    act(() => ime.textarea?.focus());
    await settle();
    expect(ime.calls).toEqual([true]); // 已在英文区
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([true, false]);
    act(() => search.focus());
    await settle();
    expect(ime.calls).toEqual([true, false, true]);
  });

  it("设置里关掉：不碰输入法", async () => {
    useSettings.setState({ terminal_ime_english: false, loaded: true });
    mount();
    const ai = makeAi();
    act(() => ime.textarea?.focus());
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([]);
  });

  it("设置还没读出来：不碰输入法", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: false });
    mount();
    act(() => ime.textarea?.focus());
    await settle();
    expect(ime.calls).toEqual([]);
  });

  it("焦点在终端时关掉开关：立即恢复原来的模式（不等 blur）", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: true });
    mount();
    act(() => ime.textarea?.focus());
    act(() => useSettings.setState({ terminal_ime_english: false }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(ime.calls).toEqual([true, false]);
  });

  it("终端 → AI → 终端 → AI 快速切换：中途回终端取消待发的恢复，最后停在恢复", async () => {
    useSettings.setState({ terminal_ime_english: true, loaded: true });
    mount();
    const ai = makeAi();
    const term = ime.textarea as HTMLTextAreaElement;
    act(() => term.focus());
    act(() => ai.focus());
    act(() => term.focus());
    act(() => ai.focus());
    await settle();
    expect(ime.calls).toEqual([true, false]);
  });
});
