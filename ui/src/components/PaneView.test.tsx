/** @vitest-environment jsdom */
//! 连接期间量到的尺寸不能丢：会话连上之后必须补发一次 term_resize。
//!
//! M1-08b 的 bug 后半段：终端量到真实尺寸的时候 `session_open` 还在路上
//! （`sessionId` 是空的），那次 resize 被直接丢掉，于是远端 PTY 一直停在
//! 初始尺寸上。重连和分屏 duplicate 走的是同一条路。

import { act, cleanup, render } from "@testing-library/react";
import type { Ref } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { termAck, termResize } from "../api/session";
import { findTarget, handleExternalDragEvent, resetPointerDragForTests } from "../state/pointerDrag";
import type { PaneSession } from "../state/sessions";
import PaneView from "./PaneView";
import type { TerminalHandle } from "./TerminalView";
import { toast } from "./ui/toast";

vi.mock("../api/session", () => ({
  termAck: vi.fn(async () => {}),
  termResize: vi.fn(async () => {}),
  termWrite: vi.fn(async () => {}),
}));

vi.mock("./ui/toast", () => ({
  toast: { error: vi.fn(), info: vi.fn(), success: vi.fn(), warning: vi.fn() },
}));

/** 假的终端：测试手动触发一次量测（TerminalView 自己有用例）。 */
const fake = vi.hoisted(() => ({
  ready: null as ((cols: number, rows: number) => void) | null,
  resize: null as ((cols: number, rows: number) => void) | null,
  /** xterm 写完回调的队列：测试决定什么时候"渲染完"。 */
  renderDone: [] as (() => void)[],
  /** 拖放往终端里粘贴的文本。 */
  pasted: [] as string[],
  /** 上层传给 TerminalView 的 pasteTarget。 */
  pasteTarget: undefined as string | null | undefined,
}));

vi.mock("./TerminalView", async () => {
  const { useImperativeHandle } = await import("react");
  return {
    default: (props: {
      ref?: Ref<TerminalHandle>;
      onReady?: (cols: number, rows: number) => void;
      onResize: (cols: number, rows: number) => void;
      pasteTarget?: string | null;
    }) => {
      fake.ready = props.onReady ?? null;
      fake.resize = props.onResize;
      fake.pasteTarget = props.pasteTarget;
      useImperativeHandle(props.ref, () => ({
        write: (_bytes: Uint8Array, onRendered?: () => void) => {
          if (onRendered) fake.renderDone.push(onRendered);
        },
        writeLine: () => {},
        size: () => ({ cols: 100, rows: 30 }),
        focus: () => {},
        paste: (text: string) => {
          fake.pasted.push(text);
        },
      }));
      return null;
    },
  };
});

const resizeMock = vi.mocked(termResize);
const ackMock = vi.mocked(termAck);

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
  fake.renderDone.length = 0;
  fake.pasted.length = 0;
  fake.pasteTarget = undefined;
  resizeMock.mockClear();
  ackMock.mockClear();
  vi.mocked(toast.error).mockClear();
});

afterEach(() => {
  cleanup();
  resetPointerDragForTests();
});

type PaneViewProps = Parameters<typeof PaneView>[0];

function renderPane(session: PaneSession, overrides: Partial<PaneViewProps> = {}) {
  const onReady = vi.fn();
  const onResize = vi.fn();
  const props: PaneViewProps = {
    paneId: "pane-1",
    session,
    terminalTheme: {},
    focused: false,
    onFocus: () => {},
    onReady,
    onResize,
    onReconnect: () => {},
    registerWriter: () => {},
    ...overrides,
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

describe("连接尝试期间的尺寸（TERM-1 T03）", () => {
  const phase = (p: PaneSession["phase"], sessionId: string | null = "s1") =>
    sessionOf({ phase: p, sessionId });

  it("连接中改了窗口大小、随后进入认证阶段：连上后仍然补发最新尺寸", () => {
    const { rerender, props } = renderPane(sessionOf({}));
    act(() => fake.ready?.(100, 30)); // 以 100×30 开始连接
    rerender(<PaneView {...props} session={phase("connecting")} />);
    act(() => fake.resize?.(120, 35));
    // 后端还在 Connecting，这时发 term_resize 会被拒，所以根本不发
    expect(resizeMock).not.toHaveBeenCalled();

    rerender(<PaneView {...props} session={phase("authenticating")} />);
    rerender(<PaneView {...props} session={phase("connected")} />);

    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s1", 120, 35);
  });

  it("多跳：connecting 和 authenticating 来回切换，尺寸始终以这次打开用的为准", () => {
    const { rerender, props } = renderPane(sessionOf({}));
    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    rerender(<PaneView {...props} session={phase("authenticating", null)} />);
    act(() => fake.resize?.(110, 31));
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    act(() => fake.resize?.(120, 35));
    rerender(<PaneView {...props} session={phase("authenticating", null)} />);
    rerender(<PaneView {...props} session={phase("connected")} />);

    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s1", 120, 35);
  });

  it("多跳期间尺寸没变：连上后不重复发", () => {
    const { rerender, props } = renderPane(sessionOf({}));
    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    rerender(<PaneView {...props} session={phase("authenticating", null)} />);
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    rerender(<PaneView {...props} session={phase("connected")} />);

    expect(resizeMock).not.toHaveBeenCalled();
  });

  it("断线重连：新一次尝试以重连时的尺寸为准，认证阶段同样不覆盖", () => {
    const { rerender, props } = renderPane(sessionOf({}));
    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    rerender(<PaneView {...props} session={phase("connected", "s1")} />);
    act(() => fake.resize?.(90, 30)); // 连上之后改大小：立即发
    expect(resizeMock).toHaveBeenCalledWith("s1", 90, 30);
    resizeMock.mockClear();

    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    rerender(<PaneView {...props} session={phase("connecting", null)} />); // 以 90×30 重连
    act(() => fake.resize?.(80, 24));
    rerender(<PaneView {...props} session={phase("authenticating", "s2")} />);
    rerender(<PaneView {...props} session={phase("connected", "s2")} />);

    expect(resizeMock).toHaveBeenCalledTimes(1);
    expect(resizeMock).toHaveBeenCalledWith("s2", 80, 24);
  });

  it("连上之后量到新尺寸：照常立刻发", () => {
    const { rerender, props } = renderPane(sessionOf({}));
    act(() => fake.ready?.(100, 30));
    rerender(<PaneView {...props} session={phase("connecting", null)} />);
    rerender(<PaneView {...props} session={phase("connected")} />);
    act(() => fake.resize?.(101, 31));
    expect(resizeMock).toHaveBeenCalledWith("s1", 101, 31);
  });
});

describe("渲染 ACK 绑定产生输出的会话（TERM-1 T06）", () => {
  const BATCH = 64 * 1024;

  function mountWithWriter(session: PaneSession) {
    let writer: ((chunk: ArrayBuffer) => void) | null = null;
    const registerWriter = (_paneId: string, fn: ((chunk: ArrayBuffer) => void) | null) => {
      writer = fn;
    };
    const mounted = renderPane(session, { registerWriter });
    return { ...mounted, write: (bytes: number) => writer?.(new ArrayBuffer(bytes)) };
  }
  const connected = (id: string) => sessionOf({ phase: "connected", sessionId: id });

  it("正常：渲染完才记账，满一批报给当前会话", () => {
    const { write } = mountWithWriter(connected("s1"));
    act(() => write(BATCH));
    expect(ackMock).not.toHaveBeenCalled(); // 还没渲染完，不能报
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).toHaveBeenCalledTimes(1);
    expect(ackMock).toHaveBeenCalledWith("s1", BATCH);
  });

  it("没满一批的先攒着，攒够了再报", () => {
    const { write } = mountWithWriter(connected("s1"));
    act(() => write(BATCH / 2));
    act(() => write(BATCH / 2));
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).not.toHaveBeenCalled();
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).toHaveBeenCalledWith("s1", BATCH);
  });

  it("旧会话的输出还在排队时断线、换了新会话：旧回调完成后不向新会话 ACK", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH));
    expect(fake.renderDone).toHaveLength(1);

    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    rerender(<PaneView {...props} session={connected("new")} />);
    act(() => fake.renderDone.shift()?.()); // 旧输出现在才渲染完

    expect(ackMock).not.toHaveBeenCalled();

    // 新会话自己的输出照常记账
    act(() => write(BATCH));
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).toHaveBeenCalledTimes(1);
    expect(ackMock).toHaveBeenCalledWith("new", BATCH);
  });

  it("旧回调的字节也不会混进新会话已攒的计数里", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH / 2)); // 旧会话排队中
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    rerender(<PaneView {...props} session={connected("new")} />);
    act(() => write(BATCH / 2)); // 新会话
    act(() => fake.renderDone.shift()?.()); // 旧
    act(() => fake.renderDone.shift()?.()); // 新

    expect(ackMock).not.toHaveBeenCalled(); // 新会话只有半批，旧的半批不能凑数
  });

  it("没经过断开阶段、会话 ID 直接换成另一个（批量更新）：同样作废旧回调", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={connected("new")} />);
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).not.toHaveBeenCalled();
  });

  it("会话 ID 返回之前就有输出：渲染完先攒着，ID 回来后归给这个会话", () => {
    const { write, rerender, props } = mountWithWriter(sessionOf({ phase: "connecting" }));
    act(() => write(BATCH)); // 连接中，ID 还没回来
    act(() => fake.renderDone.shift()?.());
    expect(ackMock).not.toHaveBeenCalled(); // 没有会话可报

    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting", sessionId: "s1" })} />);

    expect(ackMock).toHaveBeenCalledTimes(1);
    expect(ackMock).toHaveBeenCalledWith("s1", BATCH);
  });

  it("ID 返回之前的输出，渲染完成在 ID 返回之后：照样归给这个会话", () => {
    const { write, rerender, props } = mountWithWriter(sessionOf({ phase: "connecting" }));
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting", sessionId: "s1" })} />);
    act(() => fake.renderDone.shift()?.());

    expect(ackMock).toHaveBeenCalledWith("s1", BATCH);
  });

  it("连接失败（没有拿到过 ID）后重连：失败那次的输出回调不算进新会话", () => {
    const { write, rerender, props } = mountWithWriter(sessionOf({ phase: "connecting" }));
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "失败" })} />);
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(<PaneView {...props} session={connected("s2")} />);
    act(() => fake.renderDone.shift()?.());

    expect(ackMock).not.toHaveBeenCalled();
  });
});

describe("断线后迟到的旧输出不记到重连后的新会话（TERM-2 T06 遗留）", () => {
  const BATCH = 64 * 1024;

  function mountWithWriter(session: PaneSession) {
    let writer: ((chunk: ArrayBuffer) => void) | null = null;
    const registerWriter = (_paneId: string, fn: ((chunk: ArrayBuffer) => void) | null) => {
      writer = fn;
    };
    const mounted = renderPane(session, { registerWriter });
    return { ...mounted, write: (bytes: number) => writer?.(new ArrayBuffer(bytes)) };
  }
  const connected = (id: string) => sessionOf({ phase: "connected", sessionId: id });
  const flushAllRenders = () =>
    act(() => {
      for (const done of fake.renderDone.splice(0)) done();
    });

  it("断线之后、重连之前迟到的输出：照常写进终端，但不 ACK 给新会话", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH)); // 断线前的一块
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    act(() => write(BATCH)); // 旧通道在断线事件之后才把这块送到
    expect(fake.renderDone).toHaveLength(2); // 两块都真的写进了终端

    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(<PaneView {...props} session={connected("new")} />);
    flushAllRenders();

    expect(ackMock).not.toHaveBeenCalled();
  });

  it("两次事件合并成一次渲染（disconnected 直接变 connected）：同样不 ACK 给新会话", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={connected("new")} />);
    flushAllRenders();

    expect(ackMock).not.toHaveBeenCalled();
  });

  it("对照：新会话建立后才到的输出，照常 ACK 给新会话", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    act(() => write(BATCH));
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    act(() => write(BATCH)); // 迟到的旧输出
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(<PaneView {...props} session={connected("new")} />);
    act(() => write(BATCH)); // 新会话自己的输出
    flushAllRenders();

    expect(ackMock).toHaveBeenCalledTimes(1);
    expect(ackMock).toHaveBeenCalledWith("new", BATCH);
  });

  it("没满一批的迟到输出，不会给新会话已攒的计数凑数", () => {
    const { write, rerender, props } = mountWithWriter(connected("old"));
    rerender(<PaneView {...props} session={sessionOf({ phase: "disconnected", reason: "断了" })} />);
    act(() => write(BATCH / 2)); // 迟到的半批
    rerender(<PaneView {...props} session={sessionOf({ phase: "connecting" })} />);
    rerender(<PaneView {...props} session={connected("new")} />);
    act(() => write(BATCH / 2)); // 新会话的半批
    flushAllRenders();

    expect(ackMock).not.toHaveBeenCalled();
  });
});

describe("拖放插入路径（TERM-1 T04）", () => {
  const connected = () => sessionOf({ phase: "connected", sessionId: "s1" });

  /** 让 elementFromPoint 指向 pane 根元素，模拟指针落在这个终端上。 */
  function pointAt(element: Element) {
    document.elementFromPoint = () => element;
  }
  function dropFromExplorer(container: HTMLElement, paths: string[]) {
    const root = container.querySelector('[data-drop-target="terminal"]') as HTMLElement;
    pointAt(root);
    act(() => {
      handleExternalDragEvent({ type: "drop", paths, position: { x: 5, y: 5 } }, 1);
    });
  }
  function dropFromSftp(container: HTMLElement, remotePaths: string[]) {
    const root = container.querySelector('[data-drop-target="terminal"]') as HTMLElement;
    pointAt(root);
    const payload = {
      kind: "sftp-entries" as const,
      side: "remote" as const,
      entries: remotePaths.map((path) => ({ path }) as never),
    };
    const found = findTarget(5, 5, payload);
    expect(found).not.toBeNull();
    act(() => found?.target.onDrop(payload, { x: 5, y: 5, element: root }));
  }

  it("资源管理器拖入：本地路径按 POSIX 单引号插入，不加回车", () => {
    const { container } = renderPane(connected());
    dropFromExplorer(container, [String.raw`C:\a b\x-$(id).txt`, String.raw`D:\it's.txt`]);
    expect(fake.pasted).toEqual([String.raw`'C:\a b\x-$(id).txt' 'D:\it'\''s.txt'`]);
    expect(fake.pasted[0]).not.toMatch(/[\r\n]/);
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("SFTP 远端栏拖入：远端路径照旧插入", () => {
    const { container } = renderPane(connected());
    dropFromSftp(container, ["/srv/my dir/a.txt", "/srv/b.txt"]);
    expect(fake.pasted).toEqual(["'/srv/my dir/a.txt' /srv/b.txt"]);
  });

  it("文件名里有换行等控制字符：资源管理器拖入不插入并提示", () => {
    const { container } = renderPane(connected());
    for (const bad of ["a\nb", "a\rb", "a\x1bb", "a\x03b", "a\x7fb", "a\x00b"]) {
      dropFromExplorer(container, [String.raw`C:\ok.txt`, `C:\\${bad}.txt`]);
    }
    expect(fake.pasted).toEqual([]);
    expect(toast.error).toHaveBeenCalledTimes(6);
    expect(vi.mocked(toast.error).mock.calls[0][0]).toContain("控制字符");
  });

  it("文件名里有控制字符：SFTP 拖入不插入并提示", () => {
    const { container } = renderPane(connected());
    dropFromSftp(container, ["/srv/ok.txt", "/srv/a\nrm -rf x"]);
    expect(fake.pasted).toEqual([]);
    expect(toast.error).toHaveBeenCalledTimes(1);
  });

  it("没连上：不插入（原有行为）", () => {
    const { container } = renderPane(sessionOf({ phase: "disconnected", reason: "断了" }));
    dropFromExplorer(container, [String.raw`C:\a.txt`]);
    expect(fake.pasted).toEqual([]);
  });

  it("把当前会话 ID 交给终端：等确认期间换了会话，终端那边据此作废", () => {
    renderPane(connected());
    expect(fake.pasteTarget).toBe("s1");
  });
});
