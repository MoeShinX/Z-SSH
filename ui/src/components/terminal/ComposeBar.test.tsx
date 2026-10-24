/** @vitest-environment jsdom */
//! 撰写栏：Enter 发当前窗格、Ctrl+Enter 发所有分屏、Shift+Enter 换行、输入法组合中不发送、
//! 没连上不清空、↑ ↓ 翻历史（多行里不抢光标）、Esc 还焦点、发送目标开关。

import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const settingsSet = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet,
}));

import { isPanelDragging, resetPanelDraggingForTests } from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import ComposeBar, { type ComposeBarProps } from "./ComposeBar";
import {
  COMPOSE_BAR_MIN_HEIGHT,
  composeBarMaxHeight,
  useLiveComposeBarHeight,
} from "./quickBarHeight";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

type SendFn = ComposeBarProps["onSend"];

function setup(patch: Partial<ComposeBarProps> = {}) {
  const onSend = vi.fn<SendFn>((_text, _all, afterSend) => afterSend(1));
  const onEscape = vi.fn();
  const props: ComposeBarProps = { onSend, onEscape, paneCount: 2, ...patch };
  const view = render(<ComposeBar {...props} />);
  const input = screen.getByLabelText("撰写栏") as HTMLTextAreaElement;
  const type = (text: string) => fireEvent.change(input, { target: { value: text } });
  const press = (key: string, init: KeyboardEventInit = {}) =>
    fireEvent.keyDown(input, { key, ...init });
  return { view, input, onSend, onEscape, type, press };
}

describe("发送", () => {
  it("Enter 发到当前窗格，发完清空，焦点留在输入框里", () => {
    const { input, onSend, type, press } = setup();
    input.focus();
    type("ls -la");
    press("Enter");
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend.mock.calls[0].slice(0, 2)).toEqual(["ls -la", false]);
    expect(input.value).toBe("");
    expect(document.activeElement).toBe(input);
  });

  it("Enter 的默认动作（换行）被拦下", () => {
    const { input, type } = setup();
    type("ls");
    expect(fireEvent.keyDown(input, { key: "Enter" })).toBe(false);
  });

  it("Shift+Enter 换行：不发送，也不拦默认动作", () => {
    const { input, onSend, type } = setup();
    type("ls");
    expect(fireEvent.keyDown(input, { key: "Enter", shiftKey: true })).toBe(true);
    expect(onSend).not.toHaveBeenCalled();
    expect(input.value).toBe("ls");
  });

  it("Ctrl+Enter 发到这个标签的所有分屏", () => {
    const { onSend, type, press } = setup();
    type("uptime");
    press("Enter", { ctrlKey: true });
    expect(onSend.mock.calls[0].slice(0, 2)).toEqual(["uptime", true]);
  });

  it("输入法组合输入中 Enter 不发送（选字）", () => {
    const { input, onSend, type, press } = setup();
    type("ni hao");
    press("Enter", { isComposing: true });
    press("Enter", { keyCode: 229 });
    expect(onSend).not.toHaveBeenCalled();
    expect(input.value).toBe("ni hao");
  });

  it("只有空白不发送，发送按钮也是灰的", () => {
    const { onSend, type, press } = setup();
    const button = screen.getByRole("button", { name: "发送" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    type("   \n  ");
    press("Enter");
    expect(button.disabled).toBe(true);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("点发送按钮 = 发到当前窗格", () => {
    const { onSend, type } = setup();
    type("pwd");
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(onSend.mock.calls[0].slice(0, 2)).toEqual(["pwd", false]);
  });

  it("没连上（sent = 0）：内容留着，不记进历史", () => {
    const onSend = vi.fn<SendFn>((_text, _all, afterSend) => afterSend(0));
    const history = { current: [] as string[] };
    const { input, type, press } = setup({ onSend, history });
    type("ls");
    press("Enter");
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(input.value).toBe("ls");
    expect(history.current).toEqual([]);
  });

  it("多行确认还没点之前不清空；点了发送（afterSend）才清", () => {
    let pending: ((sent: number) => void) | null = null;
    const onSend = vi.fn<SendFn>((_text, _all, afterSend) => {
      pending = afterSend;
    });
    const { input, type, press } = setup({ onSend });
    type("cd /tmp\nls");
    press("Enter");
    expect(input.value).toBe("cd /tmp\nls");
    // 用户在确认框里取消时 afterSend 不会被调，内容还在；点了发送才会调
    act(() => pending?.(1));
    expect(input.value).toBe("");
  });

  it("文字原样交给发送（多行不在这里改）", () => {
    const { onSend, type, press } = setup();
    type("a\nb\n");
    press("Enter");
    expect(onSend.mock.calls[0][0]).toBe("a\nb\n");
  });
});

describe("发送目标", () => {
  it("选了「所有分屏」之后，Enter 和发送按钮都发全部", () => {
    const { onSend, type, press } = setup();
    fireEvent.click(screen.getByRole("button", { name: "当前窗格" }));
    expect(screen.getByRole("button", { name: "所有分屏" }).getAttribute("aria-pressed")).toBe(
      "true",
    );
    type("a");
    press("Enter");
    type("b");
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(onSend.mock.calls.map((call) => call[1])).toEqual([true, true]);
  });

  it("再点一下改回当前窗格；Ctrl+Enter 不看这个开关", () => {
    const { onSend, type, press } = setup();
    fireEvent.click(screen.getByRole("button", { name: "当前窗格" }));
    fireEvent.click(screen.getByRole("button", { name: "所有分屏" }));
    type("a");
    press("Enter");
    type("b");
    press("Enter", { ctrlKey: true });
    expect(onSend.mock.calls.map((call) => call[1])).toEqual([false, true]);
  });

  it("只有一个分屏时开关灰掉", () => {
    setup({ paneCount: 1 });
    expect((screen.getByRole("button", { name: "当前窗格" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });
});

describe("输入法与外观", () => {
  it("输入框跟随用户的输入法（data-ime=native），关掉拼写检查和自动大写", () => {
    const { input } = setup();
    expect(input.getAttribute("data-ime")).toBe("native");
    expect(input.getAttribute("spellcheck")).toBe("false");
    expect(input.getAttribute("autocapitalize")).toBe("off");
    expect(input.getAttribute("autocorrect")).toBe("off");
    expect(input.rows).toBe(1);
  });

  it("占位文字说明三种按键", () => {
    const { input } = setup();
    expect(input.placeholder).toBe(
      "输入命令，Enter 发送到当前窗格，Ctrl+Enter 发到所有分屏，Shift+Enter 换行",
    );
  });
});

describe("Esc", () => {
  it("把焦点交回终端", () => {
    const { onEscape, press } = setup();
    press("Escape");
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("输入法组合输入中的 Esc（取消组合）不抢焦点", () => {
    const { onEscape, press } = setup();
    press("Escape", { isComposing: true });
    expect(onEscape).not.toHaveBeenCalled();
  });
});

describe("历史", () => {
  function sendThree(ctx: ReturnType<typeof setup>) {
    for (const text of ["ls", "pwd", "df -h"]) {
      ctx.type(text);
      ctx.press("Enter");
    }
  }

  it("输入框是空的时 ↑ 翻到最近一条，一路往旧；↓ 往新，回到最新后是空的", () => {
    const ctx = setup();
    sendThree(ctx);
    const { input, press } = ctx;
    press("ArrowUp");
    expect(input.value).toBe("df -h");
    press("ArrowUp");
    expect(input.value).toBe("pwd");
    press("ArrowUp");
    expect(input.value).toBe("ls");
    press("ArrowUp"); // 到头了
    expect(input.value).toBe("ls");
    press("ArrowDown");
    expect(input.value).toBe("pwd");
    press("ArrowDown");
    press("ArrowDown");
    expect(input.value).toBe("");
  });

  it("翻之前写了一半的内容，翻回来时还在", () => {
    const ctx = setup();
    sendThree(ctx);
    ctx.type("半截");
    // 单行内容、光标在第一行：↑ 响应
    ctx.input.setSelectionRange(2, 2);
    ctx.press("ArrowUp");
    expect(ctx.input.value).toBe("df -h");
    ctx.press("ArrowDown");
    expect(ctx.input.value).toBe("半截");
  });

  it("翻出来的内容光标在末尾；改了内容就不再算在翻", () => {
    const ctx = setup();
    sendThree(ctx);
    ctx.press("ArrowUp");
    expect(ctx.input.selectionStart).toBe("df -h".length);
    ctx.type("df -h /");
    ctx.press("ArrowUp"); // 重新从最新一条开始
    expect(ctx.input.value).toBe("df -h");
  });

  it("多行内容里光标不在第一行 / 最后一行时，↑ ↓ 照常移动光标，不翻历史", () => {
    const ctx = setup();
    sendThree(ctx);
    ctx.type("one\ntwo\nthree");
    ctx.input.setSelectionRange(5, 5); // 第二行
    expect(fireEvent.keyDown(ctx.input, { key: "ArrowUp" })).toBe(true);
    expect(fireEvent.keyDown(ctx.input, { key: "ArrowDown" })).toBe(true);
    expect(ctx.input.value).toBe("one\ntwo\nthree");
    // 第一行 ↑ 才翻
    ctx.input.setSelectionRange(1, 1);
    expect(fireEvent.keyDown(ctx.input, { key: "ArrowUp" })).toBe(false);
    expect(ctx.input.value).toBe("df -h");
  });

  it("有选中的文字、或按着修饰键时不翻", () => {
    const ctx = setup();
    sendThree(ctx);
    ctx.type("abc");
    ctx.input.setSelectionRange(0, 3);
    ctx.press("ArrowUp");
    expect(ctx.input.value).toBe("abc");
    ctx.input.setSelectionRange(0, 0);
    ctx.press("ArrowUp", { shiftKey: true });
    ctx.press("ArrowUp", { ctrlKey: true });
    ctx.press("ArrowUp", { altKey: true });
    expect(ctx.input.value).toBe("abc");
  });

  it("没有历史时 ↑ 不拦默认动作", () => {
    const { input } = setup();
    expect(fireEvent.keyDown(input, { key: "ArrowUp" })).toBe(true);
  });

  it("最多记 50 条，重复的挪到最新", () => {
    const history = { current: [] as string[] };
    const ctx = setup({ history });
    for (let i = 0; i < 55; i += 1) {
      ctx.type(`cmd ${i}`);
      ctx.press("Enter");
    }
    expect(history.current).toHaveLength(50);
    expect(history.current[0]).toBe("cmd 5");
    ctx.type("cmd 10");
    ctx.press("Enter");
    expect(history.current).toHaveLength(50);
    expect(history.current.at(-1)).toBe("cmd 10");
    expect(history.current.filter((item) => item === "cmd 10")).toHaveLength(1);
  });

  it("历史是调用方给的 ref：换一个栏（换标签）接着翻", () => {
    const history = { current: [] as string[] };
    const first = setup({ history });
    first.type("kubectl get pods");
    first.press("Enter");
    cleanup();
    const second = setup({ history });
    second.press("ArrowUp");
    expect(second.input.value).toBe("kubectl get pods");
  });

  it("不传 ref 时历史只在这个栏里", () => {
    const ctx = setup();
    ctx.type("ls");
    ctx.press("Enter");
    ctx.press("ArrowUp");
    expect(ctx.input.value).toBe("ls");
  });
});

describe("上报高度", () => {
  it("挂载时上报栏的实际高度，卸载后归零", () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      height: 30,
      width: 100,
      top: 0,
      left: 0,
      right: 100,
      bottom: 30,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    const live = renderHook(() => useLiveComposeBarHeight());
    expect(live.result.current).toBe(0);
    const { view } = setup();
    expect(live.result.current).toBe(30);
    view.unmount();
    expect(live.result.current).toBe(0);
  });
});


describe("撰写栏高度拖动条", () => {
  beforeEach(() => {
    settingsSet.mockClear();
    useSettings.setState({ "layout.compose_bar_height": 0 });
    resetPanelDraggingForTests();
    Element.prototype.setPointerCapture = () => {};
    Element.prototype.hasPointerCapture = () => false;
    Element.prototype.releasePointerCapture = () => {};
  });

  const handle = () =>
    document.querySelector<HTMLElement>('[data-section="compose-bar-resize-handle"]')!;
  const bar = () => document.querySelector<HTMLElement>('[data-section="compose-bar"]')!;
  const saved = () => settingsSet.mock.calls as unknown as [string, string][];

  it("上沿有拖动条：上下调整的光标和提示；默认是自动增高（栏没有固定高度）", () => {
    const { input } = setup();
    expect(handle().className).toContain("cursor-ns-resize");
    expect(handle().getAttribute("title")).toBe("上下拖动调整高度，双击恢复");
    expect(bar().style.height).toBe("");
    expect(input.className).not.toContain("self-stretch");
  });

  it("往上拖栏变高、输入框撑满栏；拖动中不写设置，松手才写；拖动期间终端只做视觉适配", async () => {
    const { input } = setup();
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    expect(isPanelDragging()).toBe(true);
    fireEvent.pointerMove(handle(), { clientY: 440, pointerId: 1 });
    // jsdom 量不到栏的高度，起点按一行高度算
    expect(bar().style.height).toBe(`${COMPOSE_BAR_MIN_HEIGHT + 60}px`);
    expect(input.className).toContain("self-stretch");
    expect(input.className).toContain("overflow-y-auto");
    expect(input.style.height).toBe("");
    expect(settingsSet).not.toHaveBeenCalled();
    fireEvent.pointerUp(handle(), { clientY: 440, pointerId: 1 });
    expect(isPanelDragging()).toBe(false);
    await waitFor(() =>
      expect(saved()).toEqual([["layout.compose_bar_height", String(COMPOSE_BAR_MIN_HEIGHT + 60)]]),
    );
    expect(bar().style.height).toBe(`${COMPOSE_BAR_MIN_HEIGHT + 60}px`);
  });

  it("往下拖变矮，不会比一行更矮；往上不会超过终端区域的 60%", () => {
    useSettings.setState({ "layout.compose_bar_height": 120 });
    setup();
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: 530, pointerId: 1 });
    expect(bar().style.height).toBe("90px");
    fireEvent.pointerMove(handle(), { clientY: 5000, pointerId: 1 });
    expect(bar().style.height).toBe(`${COMPOSE_BAR_MIN_HEIGHT}px`);
    fireEvent.pointerMove(handle(), { clientY: -5000, pointerId: 1 });
    // jsdom 量不到终端区域，退到窗口高度
    expect(bar().style.height).toBe(`${composeBarMaxHeight(window.innerHeight)}px`);
    expect(composeBarMaxHeight(window.innerHeight)).toBe(Math.floor(window.innerHeight * 0.6));
    fireEvent.pointerUp(handle(), { clientY: -5000, pointerId: 1 });
  });

  it("只点一下（没拖动）不会把自动变成固定", () => {
    setup();
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerUp(handle(), { clientY: 500, pointerId: 1 });
    expect(settingsSet).not.toHaveBeenCalled();
    expect(bar().style.height).toBe("");
    expect(isPanelDragging()).toBe(false);
  });

  it("双击拖动条恢复自动增高（写 0）", async () => {
    useSettings.setState({ "layout.compose_bar_height": 120 });
    const { input } = setup();
    expect(bar().style.height).toBe("120px");
    fireEvent.doubleClick(handle());
    await waitFor(() => expect(saved()).toEqual([["layout.compose_bar_height", "0"]]));
    expect(bar().style.height).toBe("");
    expect(input.className).not.toContain("self-stretch");
  });

  it("存的值不合法（负数、不是数字、不到一行）当自动；超过上限显示按上限夹取、不改存的值", () => {
    for (const bad of [-5, 0, 10, "abc", null, Number.NaN]) {
      useSettings.setState({ "layout.compose_bar_height": bad as unknown as number });
      const { view } = setup();
      expect(bar().style.height).toBe("");
      view.unmount();
    }
    useSettings.setState({ "layout.compose_bar_height": 99999 });
    setup();
    expect(bar().style.height).toBe(`${composeBarMaxHeight(window.innerHeight)}px`);
    expect(useSettings.getState()["layout.compose_bar_height"]).toBe(99999);
    expect(settingsSet).not.toHaveBeenCalled();
  });

  it("固定高度：输入框撑满栏（不再自动增高），内容再多也在框内滚动", () => {
    useSettings.setState({ "layout.compose_bar_height": 150 });
    const { input, type } = setup();
    type(Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n"));
    expect(bar().style.height).toBe("150px");
    expect(input.style.height).toBe("");
    expect(input.className).toContain("self-stretch");
    expect(input.className).toContain("overflow-y-auto");
    expect(input.style.overflowY).toBe("");
  });

  it("自动模式下输入框还是自己长高：不设栏高度，不撑满", () => {
    const { input, type } = setup();
    type("a\nb\nc");
    expect(bar().style.height).toBe("");
    expect(input.className).not.toContain("self-stretch");
    expect(input.className).not.toContain("overflow-y-auto");
  });

  it("固定高度下发送成功清空后高度保持，Enter 和历史照常", () => {
    useSettings.setState({ "layout.compose_bar_height": 120 });
    const { input, onSend, type, press } = setup();
    type("ls -la");
    press("Enter");
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(input.value).toBe("");
    expect(bar().style.height).toBe("120px");
    expect(document.activeElement).toBe(input);
    press("ArrowUp");
    expect(input.value).toBe("ls -la");
    expect(bar().style.height).toBe("120px");
  });

  it("拖动过程中把栏的实际高度实时上报，卸载后归零", () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const height = Number.parseFloat(this.style.height) || 0;
      return { height, width: 100, top: 0, left: 0, right: 100, bottom: height, x: 0, y: 0, toJSON: () => ({}) };
    });
    const live = renderHook(() => useLiveComposeBarHeight());
    const { view } = setup();
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: 400, pointerId: 1 });
    expect(live.result.current).toBe(COMPOSE_BAR_MIN_HEIGHT + 100);
    fireEvent.pointerMove(handle(), { clientY: 350, pointerId: 1 });
    expect(live.result.current).toBe(COMPOSE_BAR_MIN_HEIGHT + 150);
    fireEvent.pointerUp(handle(), { clientY: 350, pointerId: 1 });
    expect(live.result.current).toBe(COMPOSE_BAR_MIN_HEIGHT + 150);
    view.unmount();
    expect(live.result.current).toBe(0);
  });

  it("拖到一半卸载：不把终端留在拖动状态", () => {
    const { view } = setup();
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    expect(isPanelDragging()).toBe(true);
    view.unmount();
    expect(isPanelDragging()).toBe(false);
  });
});

describe("标签开着广播时", () => {
  it("开关显示实际目标（所有分屏）并说明原因；Enter 按所有分屏发送", () => {
    const { input, onSend, type, press } = setup({ broadcast: true });
    const toggle = document.querySelector('[data-action="compose-target"]') as HTMLElement;
    expect(toggle.textContent).toBe("所有分屏");
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    expect(toggle.getAttribute("title")).toContain("正在广播输入");

    type("ls");
    press("Enter");
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend.mock.calls[0].slice(0, 2)).toEqual(["ls", true]);
    expect(input.value).toBe("");
  });

  it("广播关掉后回到开关自己的选择", () => {
    const onSend = vi.fn<SendFn>((_text, _all, afterSend) => afterSend(1));
    const props: ComposeBarProps = { onSend, onEscape: vi.fn(), paneCount: 2 };
    const view = render(<ComposeBar {...props} />);
    const toggle = () => document.querySelector('[data-action="compose-target"]') as HTMLElement;
    expect(toggle().textContent).toBe("当前窗格");
    view.rerender(<ComposeBar {...props} broadcast />);
    expect(toggle().textContent).toBe("所有分屏");
    view.rerender(<ComposeBar {...props} />);
    expect(toggle().textContent).toBe("当前窗格");
  });
});
