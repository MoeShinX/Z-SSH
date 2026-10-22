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
  sessionClose: vi.fn(async () => {}),
  termWrite: vi.fn(async () => {}),
  termResize: vi.fn(async () => {}),
  termAck: vi.fn(async () => {}),
  sessionsReset: vi.fn(async () => 0),
  onSessionState: vi.fn(async () => () => {}),
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
vi.mock("./components/vault/VaultView", () => ({
  HostTreeAside: () => null,
  default: () => null,
}));
vi.mock("./components/PaneView", () => ({ default: () => null }));
vi.mock("./components/titlebar/TopTabs", () => ({ default: () => null }));
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
