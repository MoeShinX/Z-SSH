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
import type { TerminalLinkHandlers } from "../terminal/links";
import TerminalView, { type TerminalHandle } from "./TerminalView";

// 关键词高亮默认开着，假终端没有它要的 onScroll / buffer 等钩子；这里测的不是高亮，关掉
useSettings.setState({ terminal_highlight_enabled: false });

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
  /** createTerminal 收到的链接回调（测试里手动触发悬停 / 点击）。 */
  links: null as TerminalLinkHandlers | null,
  /** terminal.select 收到的参数。 */
  selected: [] as number[][],
  /** 后端 open_external_url 收到的网址。 */
  opened: [] as string[],
  /** 每次 terminal.focus() 被调用时页面上还有没有弹窗（弹窗还开着时 focus 会被焦点陷阱拉回去）。 */
  focusWhileDialog: [] as boolean[],
  /** terminal.clear() 被调用了几次（右键「清屏」）。 */
  clears: 0,
}));

vi.mock("../api/settings", () => ({
  openExternalUrl: (url: string) => {
    fake.opened.push(url);
    return Promise.resolve();
  },
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
    select(column: number, row: number, length: number) {
      fake.selected.push([column, row, length]);
    },
    clear() {
      fake.clears += 1;
    },
    focus() {
      fake.focusWhileDialog.push(document.querySelector('[role="dialog"]') !== null);
    },
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
    createTerminal: (_theme: unknown, links?: TerminalLinkHandlers) => {
      fake.links = links ?? null;
      return { terminal, fitAddon };
    },
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
  fake.links = null;
  fake.selected = [];
  fake.opened = [];
  fake.focusWhileDialog = [];
  fake.clears = 0;
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

  it.each([
    ["点「粘贴」", (view: ReturnType<typeof render>) => fireEvent.click(view.getByText("粘贴"))],
    ["点「取消」", (view: ReturnType<typeof render>) => fireEvent.click(view.getByText("取消"))],
    [
      "按 Esc",
      () => fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" }),
    ],
  ])("%s 关掉确认框后焦点回到终端（弹窗已经收起才 focus），直接按回车就能执行", (_how, close) => {
    useSettings.setState({ terminal_confirm_multiline_paste: true });
    const { ref, view } = renderWithHandle();
    act(() => ref.current?.paste("a\nb"));
    fake.focusWhileDialog = [];

    close(view);
    act(() => {
      vi.runAllTimers();
    });

    expect(view.queryByRole("dialog")).toBeNull();
    expect(fake.focusWhileDialog.at(-1)).toBe(false);
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

describe("终端里的链接", () => {
  afterEach(cleanup);

  const URI = "https://example.com/a/b?c=1";
  // 第 5～30 列（1 起始、含右端），在缓冲区第 12 行
  const RANGE = { start: { x: 5, y: 12 }, end: { x: 30, y: 12 } };
  const clipboard = { writeText: vi.fn(() => Promise.resolve()), readText: vi.fn(() => Promise.resolve("pasted")) };

  beforeEach(() => {
    clipboard.writeText.mockClear();
    clipboard.readText.mockClear();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
    useSettings.setState({ terminal_confirm_multiline_paste: false, terminal_right_click_paste: true });
    fake.cols = 80;
  });

  function hover(uri: string | null = URI, range = RANGE) {
    act(() => fake.links?.onHover(uri === null ? null : { uri, range }));
  }

  /** 模拟 xterm 在鼠标抬起时调 activate：文本网址不带范围，OSC 8 带范围。 */
  function activate(
    uri: string,
    {
      detail = 1,
      x = 0,
      y = 0,
      range,
    }: { detail?: number; x?: number; y?: number; range?: typeof RANGE } = {},
  ) {
    const event = new MouseEvent("mouseup", { button: 0, detail, clientX: x, clientY: y });
    act(() => fake.links?.onActivate(uri, event, range));
  }

  /** 在终端上按下左键（点击计数 detail）。 */
  function press(detail = 1, x = 0, y = 0) {
    fireEvent.mouseDown(fake.openedEl as HTMLElement, { button: 0, detail, clientX: x, clientY: y });
  }

  it("单击：选中整条链接，不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    activate(URI);

    // 列从 0 起、行从 0 起，长度含两端：5..30 共 26 格
    expect(fake.selected).toEqual([[4, 11, 26]]);
    expect(fake.opened).toEqual([]);
  });

  it("折行的链接：选区按整行宽度累加，跨行选中", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover(URI, { start: { x: 70, y: 5 }, end: { x: 15, y: 6 } });

    activate(URI);

    expect(fake.selected).toEqual([[69, 4, 26]]);
  });

  it("双击（点击计数 2）：才调用打开，第一下选中的链接保留", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    activate(URI, { detail: 1 });
    activate(URI, { detail: 2 });

    expect(fake.opened).toEqual([URI]);
    expect(fake.selected).toEqual([[4, 11, 26]]);
  });

  it("连点三下只打开一次", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    activate(URI, { detail: 1 });
    activate(URI, { detail: 2 });
    activate(URI, { detail: 3 });

    expect(fake.opened).toEqual([URI]);
  });

  it("两下相隔超过 500ms 但点击计数是 2（系统双击时间被调慢）：照样打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    const now = vi.spyOn(Date, "now").mockReturnValueOnce(1000).mockReturnValueOnce(1600);
    activate(URI, { detail: 1 });
    vi.advanceTimersByTime(600);
    activate(URI, { detail: 2 });
    now.mockRestore();

    expect(fake.opened).toEqual([URI]);
  });

  it("点击计数没到 2（系统认定不是双击）：隔多近都不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    activate(URI, { detail: 1 });
    vi.advanceTimersByTime(5);
    activate(URI, { detail: 1 });

    expect(fake.opened).toEqual([]);
  });

  it("同一个网址在屏幕上两处，各单击一下（相隔 200ms）：不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const rangeB = { start: { x: 5, y: 20 }, end: { x: 30, y: 20 } };

    hover(URI, RANGE);
    activate(URI);
    hover(null);
    vi.advanceTimersByTime(200);
    hover(URI, rangeB);
    activate(URI);

    expect(fake.opened).toEqual([]);
    // 各自点的那一下都选中了自己那一处
    expect(fake.selected).toEqual([
      [4, 11, 26],
      [4, 19, 26],
    ]);
  });

  it("两处相同网址：就算第二下的点击计数是 2，范围不同也不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const rangeB = { start: { x: 5, y: 20 }, end: { x: 30, y: 20 } };

    hover(URI, RANGE);
    activate(URI, { detail: 1 });
    // 不经过 leave 直接换到另一处（快速移动时 xterm 的事件顺序）
    hover(URI, rangeB);
    activate(URI, { detail: 2 });

    expect(fake.opened).toEqual([]);
  });

  it("两下之间鼠标离开了链接：不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    activate(URI, { detail: 1 });
    hover(null);
    hover();
    activate(URI, { detail: 2 });

    expect(fake.opened).toEqual([]);
  });

  it("两下之间点了别处：不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    activate(URI, { detail: 1 });
    // 在链接以外按下（此刻没有悬停链接），再回到链接上点第二下
    hover(null);
    press(1, 300, 300);
    hover();
    activate(URI, { detail: 2 });

    expect(fake.opened).toEqual([]);
  });

  it("按下和抬起相距很远（拖选）：不选中链接，也不算双击", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    activate(URI, { detail: 1 });
    expect(fake.selected).toHaveLength(1);

    press(2, 100, 100);
    activate(URI, { detail: 2, x: 160, y: 100 });

    expect(fake.opened).toEqual([]);
    expect(fake.selected).toHaveLength(1);
  });

  it("OSC 8 超链接：选中的是显示文字的范围，双击打开的是目标网址", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const target = "https://target.example/page";
    // 显示文字在第 3～9 列，目标网址和它毫无关系
    const display = { start: { x: 3, y: 7 }, end: { x: 9, y: 7 } };
    hover(target, display);

    activate(target, { detail: 1, range: display });
    activate(target, { detail: 2, range: display });

    expect(fake.selected).toEqual([[2, 6, 7]]);
    expect(fake.opened).toEqual([target]);
  });

  it("OSC 8 非 http / https 的目标：双击不打开", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const target = "file:///C:/x.exe";
    hover(target, RANGE);

    activate(target, { detail: 1, range: RANGE });
    activate(target, { detail: 2, range: RANGE });

    expect(fake.opened).toEqual([]);
  });

  it("链接上的第二下按下不传给 xterm（免得双击选词替换掉整条链接的选区）", () => {
    render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const inner = document.createElement("div");
    (fake.openedEl as HTMLElement).appendChild(inner);
    const seen: number[] = [];
    inner.addEventListener("mousedown", (event) => seen.push(event.detail));

    hover();
    fireEvent.mouseDown(inner, { detail: 1 });
    fireEvent.mouseDown(inner, { detail: 2 });
    expect(seen).toEqual([1]);

    // 不在链接上：双击照常选词
    hover(null);
    fireEvent.mouseDown(inner, { detail: 2 });
    expect(seen).toEqual([1, 2]);
  });

  it("链接上右键：弹出「打开链接 / 复制链接」，不粘贴", async () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    fireEvent.contextMenu(fake.openedEl as HTMLElement);

    expect(view.getByText("打开链接")).not.toBeNull();
    expect(view.getByText("复制链接")).not.toBeNull();
    expect(clipboard.readText).not.toHaveBeenCalled();
    expect(fake.pasted).toEqual([]);

    fireEvent.click(view.getByText("复制链接"));
    expect(clipboard.writeText).toHaveBeenCalledWith(URI);
    expect(fake.opened).toEqual([]);
  });

  it("右键菜单「打开链接」走后端打开命令", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();

    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    fireEvent.click(view.getByText("打开链接"));

    expect(fake.opened).toEqual([URI]);
  });

  it("不在链接上右键：维持原来的右键粘贴，不出链接菜单", async () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    hover(null);

    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    await act(async () => {
      await Promise.resolve();
    });

    expect(view.queryByText("打开链接")).toBeNull();
    expect(clipboard.readText).toHaveBeenCalledTimes(1);
    expect(fake.pasted).toEqual(["pasted"]);
  });

  it("不在链接上右键且没开右键粘贴：弹终端菜单（清屏），不出链接菜单、不粘贴", () => {
    useSettings.setState({ terminal_right_click_paste: false });
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);

    fireEvent.contextMenu(fake.openedEl as HTMLElement);

    expect(view.queryByText("打开链接")).toBeNull();
    expect(clipboard.readText).not.toHaveBeenCalled();
    fireEvent.click(view.getByText("清屏"));
    expect(fake.clears).toBe(1);
    expect(fake.pasted).toEqual([]);
  });

  it("开着右键粘贴：普通右键照旧粘贴、不弹菜单；Shift+右键弹终端菜单，不粘贴", async () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);

    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    await act(async () => {
      await Promise.resolve();
    });
    expect(view.queryByText("清屏")).toBeNull();
    expect(fake.pasted).toEqual(["pasted"]);

    fireEvent.contextMenu(fake.openedEl as HTMLElement, { shiftKey: true });
    await act(async () => {
      await Promise.resolve();
    });
    expect(clipboard.readText).toHaveBeenCalledTimes(1);
    fireEvent.click(view.getByText("清屏"));
    expect(fake.clears).toBe(1);
  });

  it("链接上的右键菜单里没有清屏", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    expect(view.getByText("打开链接")).not.toBeNull();
    expect(view.queryByText("清屏")).toBeNull();
  });

  it("菜单开着时收到离开：菜单不收，点「复制链接」用的仍是打开菜单时的链接", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    expect(view.getByText("复制链接")).not.toBeNull();

    hover(null);
    hover("https://other.example/zzz", { start: { x: 1, y: 3 }, end: { x: 9, y: 3 } });
    fireEvent.click(view.getByText("复制链接"));

    expect(clipboard.writeText).toHaveBeenCalledWith(URI);
  });

  it("菜单开着时收到离开：「打开链接」用的仍是打开菜单时的链接", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    hover(null);
    fireEvent.click(view.getByText("打开链接"));

    expect(fake.opened).toEqual([URI]);
  });

  it("菜单关闭后在空白处右键：走原行为（右键粘贴），不再弹上一个链接的菜单", async () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    hover();
    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    expect(view.getByText("打开链接")).not.toBeNull();
    // 菜单开着，鼠标移走（xterm 的离开事件照常到达）
    hover(null);
    // 点「复制链接」关闭菜单
    fireEvent.click(view.getByText("复制链接"));
    expect(view.queryByText("打开链接")).toBeNull();

    // 空白处右键：右键粘贴
    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    await act(async () => {
      await Promise.resolve();
    });
    expect(view.queryByText("打开链接")).toBeNull();
    expect(clipboard.readText).toHaveBeenCalledTimes(1);
    expect(fake.pasted).toEqual(["pasted"]);
  });

  it("OSC 8 非 http / https 的目标：右键菜单里「打开链接」置灰，复制的是目标", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const target = "file:///C:/x.exe";
    hover(target, RANGE);

    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    const open = view.getByText("打开链接").closest("[role='menuitem']");
    expect(open?.getAttribute("data-disabled")).not.toBeNull();
    fireEvent.click(view.getByText("打开链接"));
    expect(fake.opened).toEqual([]);

    fireEvent.click(view.getByText("复制链接"));
    expect(clipboard.writeText).toHaveBeenCalledWith(target);
  });

  it("OSC 8 链接上右键：复制的是目标网址，不是显示文字", () => {
    const view = render(<TerminalView theme={{}} onData={() => {}} onResize={() => {}} />);
    const target = "https://target.example/page?x=1";
    hover(target, { start: { x: 3, y: 7 }, end: { x: 9, y: 7 } });

    fireEvent.contextMenu(fake.openedEl as HTMLElement);
    fireEvent.click(view.getByText("复制链接"));

    expect(clipboard.writeText).toHaveBeenCalledWith(target);
  });
});
