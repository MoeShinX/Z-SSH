/** @vitest-environment jsdom */
//! 开会话的同步守卫：同一个 pane 在一个 task 内连续两次 `onReady`，只能开一个会话。
//!
//! M1-08b 报告第 4 节第 1 条（M1-08c 修的）：`StrictMode` 双挂载（或者用户快速双击）
//! 会让 `TerminalView` 连续两次 `onReady`。第二次到来时 `setPane({phase:"connecting"})`
//! 还排在这个 task 的队列里，`wsRef.current` 还是旧快照，两次都放行，于是同一个 pane
//! 开出两个会话——第二个前端不认领，永远关不掉。
//!
//! 这里直接盯住 `session_open` 的调用次数：修之前是 2，修之后是 1。

import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import type { ComponentProps, Ref } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TerminalHandle } from "./components/TerminalView";

const mocks = vi.hoisted(() => ({
  sessionOpen: vi.fn<(hostId: number, cols: number, rows: number, out: unknown) => Promise<string>>(
    async () => "sess-1",
  ),
  sessionOpenQuick: vi.fn(async () => "sess-quick"),
  sessionDuplicate: vi.fn(async () => "sess-dup"),
  sessionClose: vi.fn(async (_sessionId: string) => {}),
  termWrite: vi.fn(async () => {}),
  termResize: vi.fn(async () => {}),
  termAck: vi.fn(async () => {}),
  sessionsReset: vi.fn(async () => 0),
  onSessionState: vi.fn(async () => () => {}),
  onServerStats: vi.fn(async () => () => {}),
  statsSubscribe: vi.fn(async () => 0),
  statsUnsubscribe: vi.fn(async () => {}),
  onHostKeyPrompt: vi.fn(async () => () => {}),
  onAuthPrompt: vi.fn(async () => () => {}),
  onPromptClosed: vi.fn(async () => () => {}),
  // 每个 pane 的 onReady / onResize 回调，测试手动触发（模拟终端量到尺寸）
  ready: new Map<string, (cols: number, rows: number) => void>(),
  reconnect: new Map<string, () => void>(),
  /** 终端层收到的输出块（paneId + 字节数）。 */
  written: [] as { paneId: string; bytes: number }[],
  /** true = 用真的 PaneView（带渲染 ACK 逻辑）+ 假的 TerminalView；默认用假 PaneView。 */
  realPaneView: false,
  /** 真 PaneView 路径下，假 TerminalView 收到的写入字节数。 */
  termWrites: [] as number[],
  /** 真 PaneView 路径下，xterm "写完"回调的队列：测试决定什么时候渲染完。 */
  renderDone: [] as (() => void)[],
  onTargetConnect: vi.fn(async () => () => {}),
  aiTargetConnectResult: vi.fn(async () => {}),
  hostPanelMounts: 0,
  /** PaneView 被渲染了几次（App 重新渲染一次，它就跟着渲染一次）。 */
  paneRenders: 0,
  statsHandlers: [] as ((event: unknown) => void)[],
  stateHandlers: [] as ((event: unknown) => void)[],
}));

// AI 的 @ 主机自动连接（`ai_target_connect`）：这里只验证它被订阅，不驱动真实事件。
// 其余导出保持原样，交给真实模块（不调用 `listen` 的话在 jsdom 下没问题）。
vi.mock("./api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/agent")>();
  return {
    ...actual,
    onTargetConnect: mocks.onTargetConnect,
    aiTargetConnectResult: mocks.aiTargetConnectResult,
  };
});

vi.mock("./api/session", () => ({
  sessionOpen: mocks.sessionOpen,
  sessionOpenQuick: mocks.sessionOpenQuick,
  sessionDuplicate: mocks.sessionDuplicate,
  sessionClose: mocks.sessionClose,
  termWrite: mocks.termWrite,
  termResize: mocks.termResize,
  termAck: mocks.termAck,
  sessionsReset: mocks.sessionsReset,
  onSessionState: mocks.onSessionState,
  onServerStats: mocks.onServerStats,
  statsSubscribe: mocks.statsSubscribe,
  statsUnsubscribe: mocks.statsUnsubscribe,
}));

vi.mock("./api/auth", () => ({
  onHostKeyPrompt: mocks.onHostKeyPrompt,
  onAuthPrompt: mocks.onAuthPrompt,
  onPromptClosed: mocks.onPromptClosed,
  answerHostKey: vi.fn(async () => {}),
  answerAuth: vi.fn(async () => {}),
}));

// 设置表：App 挂载时会读一遍，jsdom 下没有 Tauri，直接给空值
vi.mock("./api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "", known_hosts: "" })),
  openDataDir: vi.fn(async () => {}),
}));

// 主机页：点一下卡片就直接触发 App 的 onOpenHost（不用真的去 dblclick DOM）
vi.mock("./components/vault/VaultView", () => ({
  HostTreeAside: () => null,
  default: (props: {
    onOpenHost: (host: { id: number; name: string }) => void;
    onSelectHost: (host: { id: number; group_id: number | null }) => void;
    onOpenProxies: () => void;
  }) => (
    <>
      <button
        data-testid="open-host"
        onClick={() => props.onOpenHost({ id: 7, name: "web-1" })}
      >
        打开 web-1
      </button>
      <button
        data-testid="select-host"
        onClick={() => props.onSelectHost({ id: 7, group_id: null })}
      >
        编辑 web-1
      </button>
      <button data-testid="open-proxies" onClick={props.onOpenProxies}>
        代理
      </button>
    </>
  ),
}));
// 终端层：把每个 pane 的 onReady 回调交出来，测试手动触发
vi.mock("./components/PaneView", async () => {
  const { useEffect } = await import("react");
  const actual = await vi.importActual<typeof import("./components/PaneView")>(
    "./components/PaneView",
  );
  const RealPaneView = actual.default;
  type FakePaneProps = {
    paneId: string;
    onReady: (paneId: string, cols: number, rows: number) => void;
    onResize: (paneId: string, cols: number, rows: number) => void;
    onReconnect: (paneId: string) => void;
    registerWriter: (paneId: string, writer: ((chunk: ArrayBuffer) => void) | null) => void;
    session: { sessionId: string | null; phase: string };
  };
  const FakePaneView = (props: FakePaneProps) => {
    const { paneId, registerWriter } = props;
    mocks.ready.set(paneId, (cols, rows) => props.onReady(paneId, cols, rows));
    mocks.reconnect.set(paneId, () => props.onReconnect(paneId));
    mocks.paneRenders += 1;
    // 和真的 PaneView 一样把写输出的函数交给 App；写进来的块记在 written 里
    useEffect(() => {
      registerWriter(paneId, (chunk) => mocks.written.push({ paneId, bytes: chunk.byteLength }));
      return () => registerWriter(paneId, null);
    }, [paneId, registerWriter]);
    // 渲染一个带 pane id 的空 div：测试靠它判断窗格还在不在、会话认领没有
    return (
      <div
        data-pane-id={paneId}
        data-phase={props.session.phase}
        data-session={props.session.sessionId ?? ""}
      />
    );
  };
  return {
    default: (props: FakePaneProps) => {
      if (!mocks.realPaneView) return <FakePaneView {...props} />;
      mocks.ready.set(props.paneId, (cols, rows) => props.onReady(props.paneId, cols, rows));
      mocks.reconnect.set(props.paneId, () => props.onReconnect(props.paneId));
      return <RealPaneView {...(props as unknown as ComponentProps<typeof RealPaneView>)} />;
    },
  };
});
// 真 PaneView 路径用的假终端：记录写入，写完回调由测试手动触发。
vi.mock("./components/TerminalView", async () => {
  const { useImperativeHandle } = await import("react");
  return {
    default: (props: { ref?: Ref<TerminalHandle> }) => {
      useImperativeHandle(props.ref, () => ({
        write: (bytes: Uint8Array, onRendered?: () => void) => {
          mocks.termWrites.push(bytes.byteLength);
          if (onRendered) mocks.renderDone.push(onRendered);
        },
        writeLine: () => {},
        size: () => ({ cols: 100, rows: 30 }),
        focus: () => {},
        paste: () => {},
      }));
      return null;
    },
  };
});
// 标题栏 / 设置页 / 面板在 jsdom 下不需要真的渲染
// 齿轮按钮和设置页的 × 只留一个按钮，测设置页的进出（R-11）
vi.mock("./components/titlebar/TopTabs", () => ({
  default: (props: {
    onOpenSettings: () => void;
    onToggleQuickBar: () => void;
    quickBarOpen: boolean;
  }) => (
    <>
      <button data-testid="gear" onClick={props.onOpenSettings}>
        设置
      </button>
      <button
        data-testid="quick"
        data-on={String(props.quickBarOpen)}
        onClick={props.onToggleQuickBar}
      >
        快捷命令
      </button>
    </>
  ),
}));
vi.mock("./components/settings/SettingsPage", () => ({
  default: (props: { onClose: () => void }) => (
    <div data-testid="settings">
      <button data-testid="settings-close" onClick={props.onClose}>
        关闭
      </button>
    </div>
  ),
}));
// 主机详情：记下挂载次数，测"打开代理面板时不卸载（不丢没保存的编辑）"
vi.mock("./components/vault/HostDetailsPanel", async () => {
  const { useEffect } = await import("react");
  return {
    default: () => {
      useEffect(() => {
        mocks.hostPanelMounts += 1;
      }, []);
      return <div data-testid="host-panel" />;
    },
  };
});
vi.mock("./components/vault/KeychainPanel", () => ({ default: () => null }));
vi.mock("./components/vault/ProxyPanel", () => ({
  default: (props: { onClose: () => void }) => (
    <button data-testid="proxy-close" onClick={props.onClose}>
      关闭代理
    </button>
  ),
}));

/**
 * 挂载 App。
 *
 * 每个用例都动态 import：`App.tsx` 里"reset 只发一次"的标记是模块级的，静态
 * import 会让所有用例共用它，只有第一个用例能看到 reset。`vi.resetModules()`
 * 让下一个用例拿到一份全新的模块。
 */
async function mountApp(): Promise<ReturnType<typeof render>> {
  vi.resetModules();
  const { default: App } = await import("./App");
  return render(<App />);
}

/** 挂载 App，点一下主机卡片（触发 App 的 onOpenHost）。 */
async function openHost() {
  const view = await mountApp();
  await act(async () => {
    view.getByTestId("open-host").click();
  });
  await waitFor(() => expect(mocks.ready.size).toBe(1));
  return [...mocks.ready.values()][0];
}

// vitest 没开 globals，RTL 不会自动卸载上一个用例渲染的东西
afterEach(cleanup);

beforeEach(() => {
  mocks.sessionOpen.mockClear();
  mocks.sessionOpen.mockResolvedValue("sess-1");
  mocks.sessionOpenQuick.mockClear();
  mocks.sessionDuplicate.mockClear();
  mocks.sessionClose.mockClear();
  mocks.sessionsReset.mockClear();
  mocks.sessionsReset.mockResolvedValue(0);
  mocks.ready.clear();
  mocks.reconnect.clear();
  mocks.written.length = 0;
  mocks.realPaneView = false;
  mocks.termWrites.length = 0;
  mocks.renderDone.length = 0;
  mocks.termAck.mockClear();
  // 终端层（App 里没被 mock）要它量 pane 矩形
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("App 打开会话的守卫", () => {
  it("同一个 pane 在一个 task 内连续两次 onReady：只开一个会话", async () => {
    const ready = await openHost();

    // StrictMode 双挂载的效果：两次 onReady 落在同一个 task 里（中间没有 await 让出）
    await act(async () => {
      ready(120, 30);
      ready(120, 30);
    });

    await waitFor(() => expect(mocks.sessionOpen).toHaveBeenCalled());
    // 让第一次调用的 promise 全部落地，看有没有第二次跟上来
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(mocks.sessionOpen).toHaveBeenCalledTimes(1);
    expect(mocks.sessionOpen).toHaveBeenCalledWith(7, 120, 30, expect.anything());
    expect(mocks.sessionOpenQuick).not.toHaveBeenCalled();
    expect(mocks.sessionDuplicate).not.toHaveBeenCalled();
  });

  it("两次 onReady 的尺寸不同：还是只开一个会话（开的是第一次那份）", async () => {
    const ready = await openHost();

    await act(async () => {
      ready(120, 30);
      ready(90, 30);
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(mocks.sessionOpen).toHaveBeenCalledTimes(1);
    expect(mocks.sessionOpen).toHaveBeenCalledWith(7, 120, 30, expect.anything());
  });
});

describe("打开会话的结果回来时 pane 已经关了（TERM-1 T02）", () => {
  /** 一个手动决定何时返回的 Promise。 */
  function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    return { promise, resolve, reject };
  }
  const paneIdsInDom = () =>
    [...document.querySelectorAll("[data-pane-id]")].map((el) => el.getAttribute("data-pane-id"));
  const closeFocusedPane = () =>
    act(async () => {
      fireEvent.keyDown(window, { key: "W", ctrlKey: true, shiftKey: true });
    });
  const splitFocusedPane = () =>
    act(async () => {
      fireEvent.keyDown(window, { key: "D", ctrlKey: true, shiftKey: true });
    });
  /** 捕获 App 订阅的会话状态回调，测试手动推事件。 */
  function captureStateEvents() {
    mocks.stateHandlers.length = 0;
    mocks.onSessionState.mockImplementation((async (handler: (event: unknown) => void) => {
      mocks.stateHandlers.push(handler);
      return () => {};
    }) as unknown as () => Promise<() => void>);
    return (event: Record<string, unknown>) => {
      for (const handler of mocks.stateHandlers) handler(event);
    };
  }

  afterEach(() => {
    mocks.onSessionState.mockImplementation(async () => () => {});
    mocks.sessionDuplicate.mockReset();
    mocks.sessionDuplicate.mockResolvedValue("sess-dup");
  });

  it("关窗格时 ID 还没回来，之后才返回：马上关掉这个会话，不登记", async () => {
    const open = deferred<string>();
    mocks.sessionOpen.mockImplementationOnce(() => open.promise);
    const emit = captureStateEvents();
    const ready = await openHost();
    await act(async () => ready(100, 30));
    expect(mocks.sessionOpen).toHaveBeenCalledTimes(1);
    expect(paneIdsInDom()).toHaveLength(1);

    // ID 回来之前，后端的状态事件已经先到了（会被按 ID 缓冲）
    await act(async () => emit({ session_id: "late-session", state: "connected" }));

    await closeFocusedPane();
    expect(paneIdsInDom()).toHaveLength(0);
    expect(mocks.sessionClose).not.toHaveBeenCalled(); // 关的时候还没有 ID 可关

    await act(async () => open.resolve("late-session"));

    expect(mocks.sessionClose).toHaveBeenCalledTimes(1);
    expect(mocks.sessionClose).toHaveBeenCalledWith("late-session");
    // 之后这个会话的事件（比如关闭产生的 disconnected）直接丢，不会再有人认领
    await act(async () => emit({ session_id: "late-session", state: "disconnected", reason: "x" }));
    expect(mocks.sessionClose).toHaveBeenCalledTimes(1);
  });

  it("对照：ID 先回来、之后再关窗格，照旧关掉这个会话", async () => {
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await waitFor(() =>
      expect(document.querySelector("[data-pane-id]")?.getAttribute("data-session")).toBe("sess-1"),
    );

    await closeFocusedPane();

    // 只剩一个窗格时 closePaneAt 会转去 closeTab，原本就会发两次 close（后端幂等），这里不管次数
    expect(mocks.sessionClose).toHaveBeenCalledWith("sess-1");
    expect(new Set(mocks.sessionClose.mock.calls.map((call) => call[0]))).toEqual(
      new Set(["sess-1"]),
    );
  });

  it("对照：窗格还在时 ID 回来：登记到这个窗格，并重放先到的事件", async () => {
    const open = deferred<string>();
    mocks.sessionOpen.mockImplementationOnce(() => open.promise);
    const emit = captureStateEvents();
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await act(async () => emit({ session_id: "sess-1", state: "connected" }));

    await act(async () => open.resolve("sess-1"));

    const pane = document.querySelector("[data-pane-id]");
    expect(pane?.getAttribute("data-session")).toBe("sess-1");
    expect(pane?.getAttribute("data-phase")).toBe("connected");
    expect(mocks.sessionClose).not.toHaveBeenCalled();
  });

  it("等待期间打开失败、窗格已经关了：不关任何会话，也不报错", async () => {
    const open = deferred<string>();
    mocks.sessionOpen.mockImplementationOnce(() => open.promise);
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await closeFocusedPane();

    await act(async () => open.reject(new Error("认证失败")));

    expect(mocks.sessionClose).not.toHaveBeenCalled();
    expect(paneIdsInDom()).toHaveLength(0);
  });

  it("分屏 duplicate 的结果回来时窗格已关：同样关掉返回的 ID", async () => {
    const dup = deferred<string>();
    mocks.sessionDuplicate.mockImplementationOnce(() => dup.promise);
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await waitFor(() =>
      expect(document.querySelector("[data-pane-id]")?.getAttribute("data-session")).toBe("sess-1"),
    );

    await splitFocusedPane();
    await waitFor(() => expect(mocks.ready.size).toBe(2));
    const newPaneId = paneIdsInDom().find((id) => id !== [...mocks.ready.keys()][0]) as string;
    await act(async () => mocks.ready.get(newPaneId)?.(100, 30));
    expect(mocks.sessionDuplicate).toHaveBeenCalledWith("sess-1", 100, 30, expect.anything());

    await closeFocusedPane(); // 焦点在新窗格上
    expect(paneIdsInDom()).toEqual([[...mocks.ready.keys()][0]]);
    expect(mocks.sessionClose).not.toHaveBeenCalled();

    await act(async () => dup.resolve("late-dup"));

    expect(mocks.sessionClose).toHaveBeenCalledTimes(1);
    expect(mocks.sessionClose).toHaveBeenCalledWith("late-dup");
  });

  it("duplicate 失败改走重新连接的回退：窗格已关时不再重新连接", async () => {
    const dup = deferred<string>();
    mocks.sessionDuplicate.mockImplementationOnce(() => dup.promise);
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await waitFor(() =>
      expect(document.querySelector("[data-pane-id]")?.getAttribute("data-session")).toBe("sess-1"),
    );
    await splitFocusedPane();
    await waitFor(() => expect(mocks.ready.size).toBe(2));
    const oldPaneId = [...mocks.ready.keys()][0];
    const newPaneId = paneIdsInDom().find((id) => id !== oldPaneId) as string;
    await act(async () => mocks.ready.get(newPaneId)?.(100, 30));
    await closeFocusedPane();

    await act(async () => dup.reject(new Error("duplicate 失败")));

    expect(mocks.sessionOpen).toHaveBeenCalledTimes(1); // 只有最初那一次
    expect(mocks.sessionClose).not.toHaveBeenCalled();
  });

  it("duplicate 失败改走重新连接，回退的结果回来时窗格已关：关掉返回的 ID", async () => {
    const fallback = deferred<string>();
    mocks.sessionDuplicate.mockRejectedValueOnce(new Error("duplicate 失败"));
    const ready = await openHost();
    await act(async () => ready(100, 30));
    await waitFor(() =>
      expect(document.querySelector("[data-pane-id]")?.getAttribute("data-session")).toBe("sess-1"),
    );
    mocks.sessionOpen.mockImplementationOnce(() => fallback.promise);
    await splitFocusedPane();
    await waitFor(() => expect(mocks.ready.size).toBe(2));
    const oldPaneId = [...mocks.ready.keys()][0];
    const newPaneId = paneIdsInDom().find((id) => id !== oldPaneId) as string;
    await act(async () => mocks.ready.get(newPaneId)?.(100, 30));
    await waitFor(() => expect(mocks.sessionOpen).toHaveBeenCalledTimes(2)); // 回退发出了

    await closeFocusedPane();
    await act(async () => fallback.resolve("late-fallback"));

    expect(mocks.sessionClose).toHaveBeenCalledTimes(1);
    expect(mocks.sessionClose).toHaveBeenCalledWith("late-fallback");
  });
});

describe("重连后旧会话通道上迟到的输出（TERM-1 T06）", () => {
  it("新一次连接开始后，旧通道的块直接丢；新通道的块照常写进终端", async () => {
    mocks.stateHandlers.length = 0;
    mocks.onSessionState.mockImplementation((async (handler: (event: unknown) => void) => {
      mocks.stateHandlers.push(handler);
      return () => {};
    }) as unknown as () => Promise<() => void>);
    try {
      const ready = await openHost();
      await act(async () => ready(100, 30));
      await waitFor(() =>
        expect(document.querySelector("[data-pane-id]")?.getAttribute("data-session")).toBe(
          "sess-1",
        ),
      );
      const oldChannel = mocks.sessionOpen.mock.calls[0][3] as (chunk: ArrayBuffer) => void;

      oldChannel(new ArrayBuffer(10));
      expect(mocks.written).toHaveLength(1);

      // 断线 → 点重连
      await act(async () => {
        for (const handler of mocks.stateHandlers) {
          handler({ session_id: "sess-1", state: "disconnected", reason: "网络中断" });
        }
      });
      mocks.sessionOpen.mockResolvedValueOnce("sess-2");
      const paneId = [...mocks.reconnect.keys()][0];
      await act(async () => mocks.reconnect.get(paneId)?.());
      await waitFor(() => expect(mocks.sessionOpen).toHaveBeenCalledTimes(2));
      const newChannel = mocks.sessionOpen.mock.calls[1][3] as (chunk: ArrayBuffer) => void;

      oldChannel(new ArrayBuffer(20)); // 旧会话的尾巴
      newChannel(new ArrayBuffer(30));

      expect(mocks.written.map((item) => item.bytes)).toEqual([10, 30]);
    } finally {
      mocks.onSessionState.mockImplementation(async () => () => {});
    }
  });
});

describe("断线后、重连前迟到的旧块不记到新会话（TERM-2 T06 遗留）", () => {
  const BATCH = 64 * 1024;

  it("旧通道的块照常写进终端，但重连成功后不会 ACK 给新会话；新会话自己的块照常 ACK", async () => {
    mocks.realPaneView = true;
    mocks.stateHandlers.length = 0;
    mocks.onSessionState.mockImplementation((async (handler: (event: unknown) => void) => {
      mocks.stateHandlers.push(handler);
      return () => {};
    }) as unknown as () => Promise<() => void>);
    const push = (event: Record<string, unknown>) =>
      act(async () => {
        for (const handler of mocks.stateHandlers) handler(event);
      });
    const renderAll = () =>
      act(() => {
        for (const done of mocks.renderDone.splice(0)) done();
      });
    try {
      const ready = await openHost();
      await act(async () => ready(100, 30));
      await waitFor(() => expect(mocks.sessionOpen).toHaveBeenCalledTimes(1));
      await push({ session_id: "sess-1", state: "connected" });
      const oldChannel = mocks.sessionOpen.mock.calls[0][3] as (chunk: ArrayBuffer) => void;

      act(() => oldChannel(new ArrayBuffer(BATCH))); // 断线前的一块
      await push({ session_id: "sess-1", state: "disconnected", reason: "网络中断" });
      act(() => oldChannel(new ArrayBuffer(BATCH))); // 旧通道在断线事件之后才送到的一块
      expect(mocks.termWrites).toEqual([BATCH, BATCH]); // 迟到的块照常显示

      mocks.sessionOpen.mockResolvedValueOnce("sess-2");
      const paneId = [...mocks.reconnect.keys()][0];
      await act(async () => mocks.reconnect.get(paneId)?.());
      await waitFor(() => expect(mocks.sessionOpen).toHaveBeenCalledTimes(2));
      await push({ session_id: "sess-2", state: "connected" });
      await renderAll(); // 旧的两块现在才渲染完

      expect(mocks.termAck).not.toHaveBeenCalled();

      // 对照：新会话自己的块照常 ACK 给新会话
      const newChannel = mocks.sessionOpen.mock.calls[1][3] as (chunk: ArrayBuffer) => void;
      act(() => newChannel(new ArrayBuffer(BATCH)));
      await renderAll();
      expect(mocks.termAck.mock.calls).toEqual([["sess-2", BATCH]]);
    } finally {
      mocks.onSessionState.mockImplementation(async () => () => {});
    }
  });
});

describe("App 启动时回收上一页的会话", () => {
  it("挂载两次（StrictMode）只发一次 sessions_reset", async () => {
    vi.resetModules();
    const { default: App } = await import("./App");
    const first = render(<App />);
    await waitFor(() => expect(mocks.sessionsReset).toHaveBeenCalledTimes(1));

    // StrictMode 的那次重挂载
    first.unmount();
    render(<App />);
    await act(async () => {
      await Promise.resolve();
    });

    expect(mocks.sessionsReset).toHaveBeenCalledTimes(1);
  });
});

describe("主机页的快捷命令按钮", () => {
  it("主机页看不到终端底部的命令栏：点按钮打开 / 关上右侧的快捷命令面板，不改命令栏开关", async () => {
    const view = await mountApp();
    const { useSettings } = await import("./stores/settings");
    const before = useSettings.getState().quick_commands_bar;
    const panel = () => view.queryByText("命令列表");
    expect(panel()).toBeNull();
    await act(async () => view.getByTestId("quick").click());
    expect(panel()).not.toBeNull();
    expect(view.getByTestId("quick").dataset.on).toBe("true");
    await act(async () => view.getByTestId("quick").click());
    expect(panel()).toBeNull();
    expect(useSettings.getState().quick_commands_bar).toBe(before);
  });
});

describe("设置页的进出（R-11）", () => {
  it("在主机页点齿轮进设置，再点齿轮回主机页", async () => {
    const view = await mountApp();
    await act(async () => view.getByTestId("gear").click());
    expect(view.queryByTestId("settings")).not.toBeNull();
    expect(view.queryByTestId("open-host")).toBeNull();

    await act(async () => view.getByTestId("gear").click());
    expect(view.queryByTestId("settings")).toBeNull();
    expect(view.queryByTestId("open-host")).not.toBeNull();
  });

  it("从终端进设置：点 × 回终端，不是回主机页", async () => {
    const view = await mountApp();
    await act(async () => view.getByTestId("open-host").click());
    // 主机页的 VaultView 只在 vault 视图下渲染：它不见了说明现在在终端
    expect(view.queryByTestId("open-host")).toBeNull();

    await act(async () => view.getByTestId("gear").click());
    expect(view.queryByTestId("settings")).not.toBeNull();
    await act(async () => view.getByTestId("settings-close").click());
    expect(view.queryByTestId("settings")).toBeNull();
    expect(view.queryByTestId("open-host")).toBeNull();
  });

  it("按 Esc 关设置，回进来之前那一页", async () => {
    const view = await mountApp();
    await act(async () => view.getByTestId("open-host").click());
    await act(async () => view.getByTestId("gear").click());
    expect(view.queryByTestId("settings")).not.toBeNull();

    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });
    expect(view.queryByTestId("settings")).toBeNull();
    expect(view.queryByTestId("open-host")).toBeNull();
  });

  it("有弹层开着（或弹层已经处理了 Esc）：Esc 只关弹层，设置页留着", async () => {
    const view = await mountApp();
    await act(async () => view.getByTestId("gear").click());

    // 模拟设置页里打开的下拉框
    const layer = document.createElement("div");
    layer.setAttribute("role", "listbox");
    layer.setAttribute("data-state", "open");
    document.body.appendChild(layer);
    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });
    expect(view.queryByTestId("settings")).not.toBeNull();
    layer.remove();

    // 弹层在捕获阶段已经 preventDefault 了这次 Esc
    const swallow = (event: KeyboardEvent) => event.preventDefault();
    window.addEventListener("keydown", swallow, true);
    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });
    window.removeEventListener("keydown", swallow, true);
    expect(view.queryByTestId("settings")).not.toBeNull();
  });
});

describe("主机详情开着时打开代理面板（R-11）", () => {
  it("主机详情只藏起来、不卸载；关掉代理面板回到同一个主机详情", async () => {
    mocks.hostPanelMounts = 0;
    const view = await mountApp();
    await act(async () => view.getByTestId("select-host").click());
    expect(mocks.hostPanelMounts).toBe(1);

    await act(async () => view.getByTestId("open-proxies").click());
    expect(view.queryByTestId("proxy-close")).not.toBeNull();
    // 还在 DOM 里（隐藏），没有被卸载
    expect(view.queryByTestId("host-panel")).not.toBeNull();
    expect(view.getByTestId("host-panel").parentElement?.className).toContain("hidden");

    await act(async () => view.getByTestId("proxy-close").click());
    expect(view.queryByTestId("proxy-close")).toBeNull();
    expect(view.getByTestId("host-panel").parentElement?.className).not.toContain("hidden");
    expect(mocks.hostPanelMounts).toBe(1);
  });
});

describe("服务器状态栏的推送", () => {
  const STATS = {
    cpu_percent: 42,
    per_core_percent: [42],
    cpu_cores: 1,
    mem_total_mib: 4096,
    mem_used_mib: 2048,
    mem_free_mib: 1024,
    mem_buffers_mib: 0,
    mem_cached_mib: 0,
    swap_total_mib: 0,
    swap_used_mib: 0,
    disk_total_mib: 40960,
    disk_used_mib: 10240,
    disk_percent: 25,
    net_rx_speed: 0,
    net_tx_speed: 0,
    net_interfaces: [],
    top_processes: [],
    hostname: "demo-host",
    os_name: "",
    kernel: "",
    uptime_secs: 60,
    load_1m: null,
    load_5m: null,
    load_15m: null,
  };

  it("推送只重画状态栏，不带着整个主界面重新渲染；对不上订阅的推送不收", async () => {
    mocks.statsHandlers.length = 0;
    mocks.stateHandlers.length = 0;
    const capture = (list: ((event: unknown) => void)[]) =>
      (async (handler: (event: unknown) => void) => {
        list.push(handler);
        return () => {};
      }) as unknown as () => Promise<() => void>;
    mocks.onServerStats.mockImplementation(capture(mocks.statsHandlers));
    mocks.onSessionState.mockImplementation(capture(mocks.stateHandlers));
    mocks.statsSubscribe.mockResolvedValue(5);
    const ready = await openHost();
    await act(async () => {
      ready(120, 30);
    });
    await waitFor(() => expect(mocks.statsSubscribe).toHaveBeenCalledWith("sess-1"));
    await act(async () => {
      for (const handler of mocks.stateHandlers) handler({ session_id: "sess-1", state: "connected" });
      await Promise.resolve();
    });

    const before = mocks.paneRenders;
    await act(async () => {
      for (const handler of mocks.statsHandlers) handler({ subscription_id: 5, stats: STATS });
    });
    await waitFor(() =>
      expect(document.querySelector('[data-section="server-stats-bar"]')?.textContent).toContain("42%"),
    );
    expect(mocks.paneRenders).toBe(before);

    // 已经退订 / 不是这个界面的订阅：不收
    await act(async () => {
      for (const handler of mocks.statsHandlers) handler({ subscription_id: 99, stats: { ...STATS, cpu_percent: 77 } });
    });
    expect(document.querySelector('[data-section="server-stats-bar"]')?.textContent).not.toContain("77%");
    expect(mocks.paneRenders).toBe(before);
  });
});

describe("主机详情什么时候出来", () => {
  async function withVaultHost() {
    const { useVault } = await import("./stores/vault");
    useVault.setState({
      hosts: [{ id: 7, name: "web-1", group_id: null, host: "198.51.100.7", port: 22, username: "root", tags: [] }],
    } as never);
  }

  it("主机页双击连接（第一下单击打开了详情）：连接时把这台的详情关掉", async () => {
    const view = await mountApp();
    await act(async () => view.getByTestId("select-host").click());
    expect(view.queryByTestId("host-panel")).not.toBeNull();
    await act(async () => view.getByTestId("open-host").click());
    expect(view.queryByTestId("host-panel")).toBeNull();
  });

  it("第一次就没连上：自动打开这台主机的详情", async () => {
    mocks.sessionOpen.mockRejectedValueOnce(new Error("认证失败：密码不对"));
    const ready = await openHost();
    await withVaultHost();
    await act(async () => {
      ready(120, 30);
    });
    await waitFor(() => expect(document.querySelector('[data-testid="host-panel"]')).not.toBeNull());
  });

  it("用户自己取消（比如主机密钥弹窗点了取消）：不弹详情", async () => {
    mocks.sessionOpen.mockRejectedValueOnce(new Error("已取消"));
    const ready = await openHost();
    await withVaultHost();
    await act(async () => {
      ready(120, 30);
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(document.querySelector('[data-testid="host-panel"]')).toBeNull();
  });
});
