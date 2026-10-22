/** @vitest-environment jsdom */
//! 初始尺寸的防线：容器还没有尺寸时，不能拿 fit 的下限（2 列 × 1 行）去开会话。
//!
//! M1-08b 的 bug 就是这样来的：分屏层挂载的第一帧 pane 矩形还是 0×0，
//! 这里立刻量了一次，@xterm/addon-fit 给出 2 列 × 1 行，它被当成 session_open
//! 的初始尺寸发了出去——远端 PTY 只有一两列宽，回显全被折碎。
//!
//! 用假的 xterm / fit 插件：真的 fit 是按容器像素尺寸算行列数的，假的那个
//! 按同样的方式算（含最小 2 列 / 1 行），测试就能直接摆布"容器先是 0 尺寸、
//! 再变成正常尺寸"这个时序。

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useSettings } from "../stores/settings";
import TerminalView, { type TerminalHandle } from "./TerminalView";

/** 单格像素尺寸（测试自定；行为只取决于宽高和它的比例）。 */
const CELL_WIDTH = 8;
const CELL_HEIGHT = 17;
/** 防抖 100ms，留点余量。 */
const DEBOUNCE_MS = 100;

/** 假终端和假容器的状态。 */
const fake = vi.hoisted(() => ({
  box: { width: 0, height: 0 },
  cols: 0,
  rows: 0,
  /** fit 被调用了几次（0 尺寸时一次都不该调）。 */
  fits: 0,
  /** 终端挂在哪个元素上（就是被量尺寸的容器）。 */
  openedEl: null as HTMLElement | null,
  disposed: 0,
  /** xterm 的 paste 被调用时收到的文本（真正送进会话的内容）。 */
  pasted: [] as string[],
}));

vi.mock("../terminal/terminal", () => {
  const terminal = {
    get cols() {
      return fake.cols;
    },
    get rows() {
      return fake.rows;
    },
    options: {} as Record<string, unknown>,
    unicode: { activeVersion: "" },
    open(el: HTMLElement) {
      fake.openedEl = el;
    },
    loadAddon() {},
    onData() {
      return { dispose() {} };
    },
    onSelectionChange() {
      return { dispose() {} };
    },
    attachCustomKeyEventHandler() {},
    write(_bytes: Uint8Array, onRendered?: () => void) {
      onRendered?.();
    },
    writeln() {},
    getSelection() {
      return "";
    },
    paste(text: string) {
      fake.pasted.push(text);
    },
    focus() {},
    dispose() {
      fake.disposed += 1;
    },
  };
  const fitAddon = {
    // 和真的 @xterm/addon-fit 一样：按容器尺寸算，宽高为 0 时落在最小值上
    fit() {
      fake.fits += 1;
      fake.cols = Math.max(2, Math.floor(fake.box.width / CELL_WIDTH));
      fake.rows = Math.max(1, Math.floor(fake.box.height / CELL_HEIGHT));
    },
  };
  return {
    createTerminal: () => ({ terminal, fitAddon }),
    loadWebglRenderer: () => {},
    applyTerminalTheme: () => {},
  };
});

/** 记下来的 ResizeObserver 回调：测试手动触发，模拟分屏层量到真实矩形。 */
const observers = new Set<() => void>();

/** jsdom 没有 document.fonts（真机上字体加载完会再量一次）。 */
function stubFonts() {
  if (!document.fonts) {
    Object.defineProperty(document, "fonts", {
      configurable: true,
      value: { ready: Promise.resolve() },
    });
  }
}

beforeEach(() => {
  stubFonts();
  observers.clear();
  fake.box = { width: 0, height: 0 };
  fake.cols = 0;
  fake.rows = 0;
  fake.fits = 0;
  fake.openedEl = null;
  fake.disposed = 0;
  fake.pasted = [];
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  // jsdom 没有 ResizeObserver
  globalThis.ResizeObserver = class {
    private readonly callback: () => void;
    constructor(callback: () => void) {
      this.callback = callback;
    }
    observe() {
      observers.add(this.callback);
    }
    unobserve() {
      observers.delete(this.callback);
    }
    disconnect() {
      observers.delete(this.callback);
    }
  } as unknown as typeof ResizeObserver;
});

/** 把容器的尺寸接到测试控制的 box 上（jsdom 里量出来永远是 0）。 */
function bindBox(el: HTMLElement) {
  Object.defineProperty(el, "clientWidth", { configurable: true, get: () => fake.box.width });
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => fake.box.height });
}

/** 容器量到新尺寸：改尺寸 + 触发 ResizeObserver + 走完防抖。 */
function resize(width: number, height: number) {
  fake.box = { width, height };
  act(() => {
    for (const callback of observers) callback();
    vi.advanceTimersByTime(DEBOUNCE_MS + 20);
  });
}

function renderView() {
  const onReady = vi.fn();
  const onResize = vi.fn();
  render(<TerminalView theme={{}} onData={() => {}} onResize={onResize} onReady={onReady} />);
  const el = fake.openedEl;
  if (!el) throw new Error("终端没挂到容器上");
  bindBox(el);
  return { onReady, onResize };
}

describe("TerminalView 的初始尺寸", () => {
  it("容器先是 0 尺寸：不开会话，量到有效尺寸后只报一次", () => {
    const { onReady, onResize } = renderView();

    // 挂载时容器 0×0（分屏层第一帧），fit 会给出 2 列 × 1 行，这次量测不算数
    expect(fake.cols).toBe(0); // 根本没 fit
    expect(onReady).not.toHaveBeenCalled();
    expect(onResize).not.toHaveBeenCalled();

    resize(800, 400);

    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onReady).toHaveBeenCalledWith(100, 23);
    expect(onResize).toHaveBeenCalledWith(100, 23);

    // 再量几次（字体加载完、第一帧画完）不会重复 announce
    resize(800, 400);
    expect(onReady).toHaveBeenCalledTimes(1);
  });

  it("量测落到 fit 的下限（2 列 × 1 行）：等真实尺寸再开会话", () => {
    const { onReady, onResize } = renderView();

    // 16px 宽 × 16px 高正好让假 fit 落回它的下限 2 列 × 1 行——"没量到"的信号。
    // （这个小尺寸模拟的是量不到真实尺寸的瞬间，不是真的把窗口拖到这么小。）
    resize(16, 16);
    expect(fake.cols).toBe(2);
    expect(fake.rows).toBe(1);
    expect(onReady).not.toHaveBeenCalled();
    expect(onResize).not.toHaveBeenCalled();

    resize(800, 400);
    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onReady).toHaveBeenCalledWith(100, 23);
    expect(onResize).toHaveBeenCalledWith(100, 23);
  });

  it("真实存在的窄 pane（12 列）：照常开会话", () => {
    // M1-08c 的要求：6 个 pane 挤在一个窗口里，每个 pane 只有十几列，这是真实的
    // 尺寸，必须开远端 PTY，否则 pane 会一直空着（M1-08b 那个 20 列下限就是这个毛病）。
    const { onReady, onResize } = renderView();

    resize(100, 400); // 12 列
    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onReady).toHaveBeenCalledWith(12, 23);
    expect(onResize).toHaveBeenCalledWith(12, 23);
  });

  it("尺寸稳定后变化：只走 onResize", () => {
    const { onReady, onResize } = renderView();

    resize(800, 400);
    resize(400, 400);

    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onReady).toHaveBeenCalledWith(100, 23);
    expect(onResize).toHaveBeenLastCalledWith(50, 23);
  });

  it("容器又被藏起来（0 尺寸，比如切到设置页）：不 fit 也不上报", () => {
    const { onReady, onResize } = renderView();

    resize(800, 400);
    expect(onReady).toHaveBeenCalledTimes(1);
    const fits = fake.fits;

    resize(0, 0);

    // 0 尺寸上 fit 会给出 2 列 × 1 行（还会重排回滚缓冲），这次量测要整个跳过
    expect(fake.fits).toBe(fits);
    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onResize).toHaveBeenCalledTimes(1);

    // 再切回来：照常报新尺寸
    resize(600, 400);
    expect(onResize).toHaveBeenLastCalledWith(75, 23);
  });
});

describe("粘贴入口的多行确认（TERM-1 T04）", () => {
  afterEach(cleanup);

  function renderWithHandle(pasteTarget: string | null = null) {
    const ref = createRef<TerminalHandle>();
    const view = render(
      <TerminalView
        ref={ref}
        theme={{}}
        onData={() => {}}
        onResize={() => {}}
        pasteTarget={pasteTarget}
      />,
    );
    const rerenderWith = (target: string | null) =>
      view.rerender(
        <TerminalView
          ref={ref}
          theme={{}}
          onData={() => {}}
          onResize={() => {}}
          pasteTarget={target}
        />,
      );
    return { ref, view, rerenderWith };
  }

  it("开着确认：通过对外的 handle 粘贴多行文本也会弹确认，确认前不送进终端", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view } = renderWithHandle();

    act(() => ref.current?.paste("a\nb"));

    expect(view.queryByRole("dialog")).not.toBeNull();
    expect(fake.pasted).toEqual([]);

    fireEvent.click(view.getByText("粘贴"));
    expect(fake.pasted).toEqual(["a\nb"]);
    expect(view.queryByRole("dialog")).toBeNull();
  });

  it("点取消：不送进终端", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view } = renderWithHandle();

    act(() => ref.current?.paste("a\nb"));
    fireEvent.click(view.getByText("取消"));

    expect(fake.pasted).toEqual([]);
    expect(view.queryByRole("dialog")).toBeNull();
  });

  it("单行文本（拖入的路径）：不弹确认，直接送进终端、不加回车", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view } = renderWithHandle();

    act(() => ref.current?.paste("'/srv/a b.txt'"));

    expect(view.queryByRole("dialog")).toBeNull();
    expect(fake.pasted).toEqual(["'/srv/a b.txt'"]);
  });

  it("关掉确认开关：多行文本直接送进终端", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: false });
    const { ref, view } = renderWithHandle();

    act(() => ref.current?.paste("a\nb"));

    expect(view.queryByRole("dialog")).toBeNull();
    expect(fake.pasted).toEqual(["a\nb"]);
  });

  it("浏览器 paste 事件（对照）：同样弹确认", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { view } = renderWithHandle();

    fireEvent.paste(fake.openedEl as HTMLElement, {
      clipboardData: { getData: () => "a\nb" },
    });

    expect(view.queryByRole("dialog")).not.toBeNull();
    expect(fake.pasted).toEqual([]);
  });

  it("等确认期间目标会话变了（重连）：这次粘贴作废，弹窗收起，不发给新会话", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view, rerenderWith } = renderWithHandle("old-session");

    act(() => ref.current?.paste("a\nb"));
    expect(view.queryByRole("dialog")).not.toBeNull();

    rerenderWith(null); // 断线
    rerenderWith("new-session"); // 重连成新会话

    expect(view.queryByRole("dialog")).toBeNull();
    expect(fake.pasted).toEqual([]);
  });

  it("目标没变：确认之后照常送出", () => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view, rerenderWith } = renderWithHandle("s1");

    act(() => ref.current?.paste("a\nb"));
    rerenderWith("s1");
    fireEvent.click(view.getByText("粘贴"));

    expect(fake.pasted).toEqual(["a\nb"]);
  });
});
