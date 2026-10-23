/** @vitest-environment jsdom */
//! 关闭主窗口（UI-4）：缩到托盘 / 退出两种模式，和托盘「退出」走同一套关闭请求。
//!
//! 直接抓 App 注册给 Tauri 的 `onCloseRequested` 回调，模拟 × / Alt+F4 / 托盘「退出」。

import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface CloseEvent {
  preventDefault: () => void;
  prevented: boolean;
}

const mocks = vi.hoisted(() => ({
  /** 设置表：key → JSON 文本。 */
  settings: new Map<string, string>(),
  settingsSet: vi.fn(async (_key: string, _value: string) => {}),
  closeHandler: null as ((event: CloseEvent) => void | Promise<void>) | null,
  close: vi.fn(),
  trayHideMain: vi.fn(async () => true),
  trayShowMain: vi.fn(async () => {}),
  /** 后端的端口转发状态（phase 字符串）；默认没有转发。 */
  forwardPhases: [] as string[],
  /** App 订阅的会话状态回调：测试手动推事件。 */
  stateHandlers: [] as ((event: unknown) => void)[],
  /** 每个 pane 的 onReady：测试手动触发，模拟终端量到尺寸、开会话。 */
  ready: new Map<string, (cols: number, rows: number) => void>(),
  /** Z-SFTP 文件窗口报上来的会话。 */
  fileSessionsHandler: null as ((sessions: { sftp_id: string; host_id: number }[]) => void) | null,
  trayShowHint: vi.fn(async () => {}),
  trayUiReady: vi.fn(async (_ready: boolean) => {}),
  trayQuitHandler: null as (() => void) | null,
  trayUpdateHandler: null as (() => void) | null,
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: async (handler: (event: CloseEvent) => void | Promise<void>) => {
      mocks.closeHandler = handler;
      return () => {};
    },
    close: mocks.close,
    isMaximized: vi.fn(async () => false),
  }),
}));

vi.mock("./api/tray", () => ({
  trayHideMain: mocks.trayHideMain,
  trayShowMain: mocks.trayShowMain,
  trayShowHint: mocks.trayShowHint,
  trayUiReady: mocks.trayUiReady,
  onTrayQuit: async (handler: () => void) => {
    mocks.trayQuitHandler = handler;
    return () => {};
  },
  onTrayOpenUpdate: async (handler: () => void) => {
    mocks.trayUpdateHandler = handler;
    return () => {};
  },
}));

vi.mock("./api/settings", () => ({
  settingsGet: vi.fn(async (key: string) => mocks.settings.get(key) ?? null),
  settingsSet: mocks.settingsSet,
  dataDirInfo: vi.fn(async () => ({ data_dir: "", known_hosts: "" })),
  openDataDir: vi.fn(async () => {}),
}));

// AI 的 @ 主机自动连接：这里不需要真实事件，订阅给个空实现（jsdom 下没有 Tauri 的 listen）。
vi.mock("./api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/agent")>();
  return {
    ...actual,
    onTargetConnect: vi.fn(async () => () => {}),
    aiTargetConnectResult: vi.fn(async () => {}),
  };
});

vi.mock("./api/session", () => ({
  sessionOpen: vi.fn(async () => "s"),
  sessionOpenQuick: vi.fn(async () => "s"),
  sessionDuplicate: vi.fn(async () => "s"),
  sessionClose: vi.fn(async (_sessionId: string) => {}),
  termWrite: vi.fn(async () => {}),
  termResize: vi.fn(async () => {}),
  termAck: vi.fn(async () => {}),
  sessionsReset: vi.fn(async () => 0),
  onSessionState: vi.fn(async (handler: (event: unknown) => void) => {
    mocks.stateHandlers.push(handler);
    return () => {};
  }),
  onServerStats: vi.fn(async () => () => {}),
  statsSubscribe: vi.fn(async () => 0),
  statsUnsubscribe: vi.fn(async () => {}),
}));
vi.mock("./api/auth", () => ({
  onHostKeyPrompt: vi.fn(async () => () => {}),
  onAuthPrompt: vi.fn(async () => () => {}),
  onPromptClosed: vi.fn(async () => () => {}),
  answerHostKey: vi.fn(async () => {}),
  answerAuth: vi.fn(async () => {}),
}));
// 主机页：点一下就让 App 开一个终端标签（再手动触发 onReady 开会话）
vi.mock("./components/vault/VaultView", () => ({
  HostTreeAside: () => null,
  default: (props: { onOpenHost: (host: { id: number; name: string }) => void }) => (
    <button data-testid="open-host" onClick={() => props.onOpenHost({ id: 7, name: "web-1" })}>
      打开 web-1
    </button>
  ),
}));
vi.mock("./components/PaneView", () => ({
  default: (props: { paneId: string; onReady: (paneId: string, cols: number, rows: number) => void }) => {
    mocks.ready.set(props.paneId, (cols, rows) => props.onReady(props.paneId, cols, rows));
    return null;
  },
}));
vi.mock("./api/forwards", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/forwards")>();
  return {
    ...actual,
    forwardStatuses: vi.fn(async () =>
      mocks.forwardPhases.map((phase, index) => ({
        id: index + 1,
        phase,
        error: null,
        listening_port: null,
        counters: { active: 0, total: 0, bytes_sent: 0, bytes_received: 0 },
      })),
    ),
  };
});
vi.mock("./api/fileWindow", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/fileWindow")>();
  return {
    ...actual,
    onFileSessions: async (handler: (sessions: { sftp_id: string; host_id: number }[]) => void) => {
      mocks.fileSessionsHandler = handler;
      return () => {};
    },
  };
});
// 顶栏只留打开停靠文件面板、关掉第一个标签的按钮；停靠 SFTP 打开即就绪，面板本身用替身。
vi.mock("./components/titlebar/TopTabs", () => ({
  default: (props: {
    tabs: { id: string }[];
    onToggleSftp: () => void;
    onCloseTab: (tabId: string) => void;
  }) => (
    <>
      <button data-testid="toggle-sftp" onClick={props.onToggleSftp}>
        SFTP
      </button>
      <button data-testid="close-tab" onClick={() => props.onCloseTab(props.tabs[0].id)}>
        关闭
      </button>
    </>
  ),
}));
vi.mock("./api/sftp", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api/sftp")>()),
  sftpOpen: vi.fn(async () => "dock-sftp"),
  sftpClose: vi.fn(async () => {}),
}));
vi.mock("./components/sftp/SftpTabView", () => ({
  default: () => <div data-testid="active-dock-sftp">SFTP ready</div>,
}));
vi.mock("./components/settings/SettingsPage", () => ({ default: () => null }));

function closeEvent(): CloseEvent {
  const event = {
    prevented: false,
    preventDefault() {
      event.prevented = true;
    },
  };
  return event;
}

/** 挂载 App，等关闭拦截和托盘监听都挂好。 */
async function mountApp() {
  vi.resetModules();
  const { default: App } = await import("./App");
  const view = render(<App />);
  await waitFor(() => expect(mocks.trayUiReady).toHaveBeenCalledWith(true));
  return view;
}

/** 模拟一次关闭请求，返回事件（看有没有被 preventDefault）。 */
async function requestClose(): Promise<CloseEvent> {
  const event = closeEvent();
  await act(async () => {
    await mocks.closeHandler?.(event);
  });
  return event;
}

afterEach(cleanup);

beforeEach(() => {
  mocks.settings.clear();
  mocks.settingsSet.mockClear();
  mocks.close.mockClear();
  mocks.trayHideMain.mockClear();
  mocks.trayHideMain.mockResolvedValue(true);
  mocks.trayShowMain.mockClear();
  mocks.forwardPhases = [];
  mocks.stateHandlers.length = 0;
  mocks.ready.clear();
  mocks.fileSessionsHandler = null;
  mocks.trayShowHint.mockClear();
  mocks.trayUiReady.mockClear();
  mocks.closeHandler = null;
  mocks.trayQuitHandler = null;
  mocks.trayUpdateHandler = null;
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("关闭主窗口：缩到托盘（默认）", () => {
  it("设置里什么都没存：× 拦下关闭、隐藏窗口，不退出", async () => {
    await mountApp();
    const event = await requestClose();

    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
    expect(mocks.close).not.toHaveBeenCalled();
  });

  it("第一次缩到托盘提示一次并记下，之后不再提示", async () => {
    await mountApp();

    await requestClose();
    expect(mocks.trayShowHint).toHaveBeenCalledTimes(1);
    await waitFor(() =>
      expect(mocks.settingsSet).toHaveBeenCalledWith("tray_hint_shown", "true"),
    );

    await requestClose();
    await requestClose();
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(3);
    expect(mocks.trayShowHint).toHaveBeenCalledTimes(1);
  });

  it("以前提示过的（库里记着）：一次提示都没有", async () => {
    mocks.settings.set("tray_hint_shown", "true");
    await mountApp();

    await requestClose();
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
    expect(mocks.trayShowHint).not.toHaveBeenCalled();
  });

  it("后端没隐藏成（没有托盘图标）：退回成退出流程，不会点了 × 没反应", async () => {
    mocks.trayHideMain.mockResolvedValue(false);
    await mountApp();

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    // 退出流程：再关一次窗口，这一次明确是要退出
    expect(mocks.close).toHaveBeenCalledTimes(1);
    expect(mocks.trayShowHint).not.toHaveBeenCalled();
    const second = await requestClose();
    expect(second.prevented).toBe(false);
  });

  it("库里存的值读不懂：按默认缩到托盘", async () => {
    mocks.settings.set("close_action", JSON.stringify("something-else"));
    await mountApp();

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
  });
});

describe("关闭主窗口：退出程序", () => {
  it("没有要确认的东西：放行（和以前一样），不隐藏、不提示", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();

    const event = await requestClose();
    expect(event.prevented).toBe(false);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();
    expect(mocks.trayShowHint).not.toHaveBeenCalled();
  });
});

describe("托盘「退出」", () => {
  it("缩到托盘模式下：走关闭请求，并且这一次不能再隐藏（没有要确认的就放行）", async () => {
    await mountApp();
    expect(mocks.trayQuitHandler).not.toBeNull();

    // 托盘「退出」→ 界面关一次主窗口
    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    expect(mocks.close).toHaveBeenCalledTimes(1);

    // 这次关闭请求：模式虽然是缩到托盘，但因为是退出，不隐藏、放行
    const event = await requestClose();
    expect(event.prevented).toBe(false);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();

    // 只管一次：之后点 × 又是缩到托盘
    const next = await requestClose();
    expect(next.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
  });

  it("托盘「重启安装」：打开更新对话框", async () => {
    await mountApp();
    const { useUpdater } = await import("./stores/updater");
    expect(useUpdater.getState().dialogOpen).toBe(false);

    await act(async () => {
      mocks.trayUpdateHandler?.();
    });
    expect(useUpdater.getState().dialogOpen).toBe(true);
  });

  it("界面卸载时告诉后端不能再回答退出确认", async () => {
    const view = await mountApp();
    view.unmount();
    expect(mocks.trayUiReady).toHaveBeenLastCalledWith(false);
  });
});

describe("有未保存的编辑器内容时的退出确认（和 UI-4 之前一样）", () => {
  const dirtyDoc = {
    editId: 1,
    fileName: "notes.txt",
    path: "/tmp/notes.txt",
    side: "local",
    hostId: 0,
    dirty: true,
  };

  async function withDirtyDoc() {
    const { useEditors } = await import("./stores/editors");
    useEditors.setState({ docs: { 1: dirtyDoc as never } });
  }

  it("退出模式点 ×：拦下并询问，取消后窗口还在", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();
    await withDirtyDoc();

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();
    const cancel = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>('[data-action="unsaved-cancel"]');
      expect(button).not.toBeNull();
      return button!;
    });
    await act(async () => {
      cancel.click();
    });
    expect(mocks.close).not.toHaveBeenCalled();
  });

  it("缩到托盘模式点 ×：不问，直接隐藏（什么都没丢）", async () => {
    await mountApp();
    await withDirtyDoc();

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
    expect(document.querySelector('[data-action="unsaved-cancel"]')).toBeNull();
  });

  it("托盘「退出」：和 × 退出同一个询问；取消后再点 × 又是缩到托盘", async () => {
    await mountApp();
    await withDirtyDoc();

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();
    const cancel = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>('[data-action="unsaved-cancel"]');
      expect(button).not.toBeNull();
      return button!;
    });
    await act(async () => {
      cancel.click();
    });
    expect(mocks.close).toHaveBeenCalledTimes(1); // 只有托盘退出那一次主动 close()

    const after = await requestClose();
    expect(after.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
  });
});

describe("退出时有活动连接要确认（托盘「退出」和 × 退出同一套判断）", () => {
  const emitState = (event: Record<string, unknown>) =>
    act(async () => {
      for (const handler of mocks.stateHandlers) handler(event);
    });

  /** 开一个终端标签并让它开出会话（id 是 s）。 */
  async function openTerminal() {
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="open-host"]')!.click();
    });
    await waitFor(() => expect(mocks.ready.size).toBe(1));
    await act(async () => {
      [...mocks.ready.values()][0](100, 30);
    });
    await act(async () => {
      await Promise.resolve();
    });
  }

  const dialogs = () => document.querySelectorAll('[role="dialog"]');
  const buttonByText = (text: string) =>
    [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.trim() === text,
    );
  const click = (text: string) =>
    act(async () => {
      const button = buttonByText(text);
      expect(button, `按钮「${text}」`).toBeDefined();
      button!.click();
    });

  it("有连着的终端：托盘「退出」叫出窗口并弹确认；取消不退出，确认才退出", async () => {
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    expect(mocks.close).toHaveBeenCalledTimes(1);
    const first = await requestClose();
    expect(first.prevented).toBe(true);
    expect(mocks.trayShowMain).toHaveBeenCalledTimes(1);
    expect(dialogs()).toHaveLength(1);
    expect(document.body.textContent).toContain("还有 1 个连接（1 个终端），退出会全部断开。");
    expect(buttonByText("退出")).toBeDefined();

    // 取消：窗口留着，没有再次关闭
    await click("取消");
    expect(dialogs()).toHaveLength(0);
    expect(mocks.close).toHaveBeenCalledTimes(1);

    // 再点托盘「退出」又问；这次确认
    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const second = await requestClose();
    expect(second.prevented).toBe(true);
    await click("退出");
    await waitFor(() => expect(mocks.close).toHaveBeenCalledTimes(3));
    // 确认之后的那次关闭请求放行
    const last = await requestClose();
    expect(last.prevented).toBe(false);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();
  });

  it("正在连接的终端也算", async () => {
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connecting" });

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(document.body.textContent).toContain("1 个终端");
  });

  it("只有已断开的会话：不弹，直接放行，也不叫出窗口", async () => {
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });
    await emitState({ session_id: "s", state: "disconnected", reason: "test" });

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(false);
    expect(mocks.trayShowMain).not.toHaveBeenCalled();
    expect(dialogs()).toHaveLength(0);
  });

  it("没有任何事项：直接放行，不叫出主窗口", async () => {
    await mountApp();
    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(false);
    expect(mocks.trayShowMain).not.toHaveBeenCalled();
    expect(dialogs()).toHaveLength(0);
  });

  it("端口转发在运行（含启动中）要确认；失败的转发不算", async () => {
    await mountApp();
    mocks.forwardPhases = ["failed"];
    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    expect((await requestClose()).prevented).toBe(false);

    mocks.forwardPhases = ["running", "starting", "failed"];
    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayShowMain).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).toContain("还有 2 个连接（2 个端口转发），退出会全部断开。");
  });

  it("Z-SFTP 文件窗口里打开着的 SFTP 连接也算，几类一起列出", async () => {
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });
    mocks.forwardPhases = ["running"];
    await act(async () => {
      mocks.fileSessionsHandler?.([{ sftp_id: "f1", host_id: 7 }]);
    });

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    await requestClose();
    expect(document.body.textContent).toContain(
      "还有 3 个连接（1 个终端、1 个端口转发、1 个 SFTP 连接），退出会全部断开。",
    );
  });

  it("点 × 退出（close_action = quit）也走同一个确认", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("1 个终端");
  });

  it("缩到托盘模式点 ×：有连接也不问，直接隐藏（连接都还在）", async () => {
    await mountApp();
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });

    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayHideMain).toHaveBeenCalledTimes(1);
    expect(mocks.trayShowMain).not.toHaveBeenCalled();
    expect(dialogs()).toHaveLength(0);
  });

  it("和未保存的编辑同时存在：只弹一个对话框，两类都列在里面", async () => {
    await mountApp();
    const { useEditors } = await import("./stores/editors");
    useEditors.setState({
      docs: {
        1: {
          editId: 1,
          fileName: "notes.txt",
          path: "/tmp/notes.txt",
          side: "local",
          hostId: 0,
          dirty: true,
        } as never,
      },
    });
    await openTerminal();
    await emitState({ session_id: "s", state: "connected" });

    await act(async () => {
      mocks.trayQuitHandler?.();
    });
    const event = await requestClose();
    expect(event.prevented).toBe(true);
    expect(mocks.trayShowMain).toHaveBeenCalledTimes(1);
    expect(dialogs()).toHaveLength(1);
    expect(document.querySelector('[data-action="unsaved-cancel"]')).not.toBeNull();
    expect(document.body.textContent).toContain("notes.txt");
    expect(document.body.textContent).toContain("还有 1 个连接（1 个终端），退出会全部断开。");

    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-action="unsaved-cancel"]')!.click();
    });
    expect(dialogs()).toHaveLength(0);
    expect(mocks.close).toHaveBeenCalledTimes(1); // 只有托盘退出那一次主动 close()
  });
});

describe("退出判断用等转发查询之后的状态；停靠文件面板也算连接", () => {
  it("查询转发期间刚变脏的文档也要拦下来确认", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();
    const { useEditors } = await import("./stores/editors");
    act(() =>
      useEditors.setState({
        docs: {
          99: {
            editId: 99,
            fileName: "late-edit.txt",
            path: "/tmp/late-edit.txt",
            side: "local",
            hostId: 0,
            dirty: false,
            version: 0,
          } as never,
        },
      }),
    );
    const { forwardStatuses } = await import("./api/forwards");
    let finish!: () => void;
    vi.mocked(forwardStatuses).mockImplementationOnce(
      () => new Promise((resolve) => (finish = () => resolve([]))),
    );
    const event = closeEvent();
    let pending!: Promise<void>;
    act(() => {
      pending = Promise.resolve(mocks.closeHandler!(event));
    });
    await waitFor(() => expect(finish).toBeTypeOf("function"));
    // 等转发状态的这段时间里文档变脏了
    act(() => useEditors.getState().patch(99, { dirty: true, version: 1 }));
    await act(async () => {
      finish();
      await pending;
    });
    expect(event.prevented, "不能用查询前的空快照放行").toBe(true);
  });

  it("终端已断开、停靠的文件面板还连着：也要确认退出", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="open-host"]')!.click();
    });
    await waitFor(() => expect(mocks.ready.size).toBe(1));
    await act(async () => {
      [...mocks.ready.values()][0](100, 30);
    });
    await act(async () => {
      for (const handler of mocks.stateHandlers) handler({ session_id: "s", state: "connected" });
    });
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="toggle-sftp"]')!.click();
    });
    await waitFor(() =>
      expect(document.querySelector('[data-testid="active-dock-sftp"]')).not.toBeNull(),
    );
    await act(async () => {
      for (const handler of mocks.stateHandlers) {
        handler({ session_id: "s", state: "disconnected", reason: "shell exited" });
      }
    });
    expect(document.querySelector('[data-testid="active-dock-sftp"]')).not.toBeNull();
    const event = await requestClose();
    expect(event.prevented, "停靠的 SFTP 不能因为终端断了就漏数").toBe(true);
    expect(document.body.textContent).toContain("还有 1 个连接（1 个 SFTP 连接），退出会全部断开。");
  });

  it("停靠面板还在连接中就关掉这台主机的终端：连上后会话随即关掉，不留看不见的连接", async () => {
    mocks.settings.set("close_action", JSON.stringify("quit"));
    await mountApp();
    const { sftpOpen, sftpClose } = await import("./api/sftp");
    let finishOpen!: (sftpId: string) => void;
    vi.mocked(sftpOpen).mockImplementationOnce(
      () => new Promise((resolve) => (finishOpen = resolve)),
    );
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="open-host"]')!.click();
    });
    await waitFor(() => expect(mocks.ready.size).toBe(1));
    await act(async () => {
      [...mocks.ready.values()][0](100, 30);
    });
    await act(async () => {
      for (const handler of mocks.stateHandlers) handler({ session_id: "s", state: "connected" });
    });
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="toggle-sftp"]')!.click();
    });
    await waitFor(() => expect(finishOpen).toBeTypeOf("function"));
    // 面板还在连接中，关掉这台主机唯一的终端标签
    await act(async () => {
      document.querySelector<HTMLButtonElement>('[data-testid="close-tab"]')!.click();
    });
    await act(async () => {
      finishOpen("late-sftp");
    });
    await waitFor(() => expect(vi.mocked(sftpClose)).toHaveBeenCalledWith("late-sftp"));
    const event = await requestClose();
    expect(event.prevented, "关掉的会话不能再算成活动连接").toBe(false);
  });
});
