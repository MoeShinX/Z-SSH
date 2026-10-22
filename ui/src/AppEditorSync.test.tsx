/** @vitest-environment jsdom */
//! 第 2 节（数据完整性）：冲突 / 属主 / 另存为处理完之后，编辑器文档必须和后端状态一致。
//!
//! 都从真实的冲突对话框按钮开始走：App 的 `sftp_edit_event` 订阅 → 对话框 → 按钮 →
//! `sftp_edit_resolve` → 后端事件 → 文档同步（放弃要重新载入、另存为要改路径、覆盖/属主处理
//! 之后按“实际传上去的那份文本”判断还有没有未保存修改）。
//!
//! 后端用 mock 替身，但前端走的是真实 App + 真实编辑器（CodeMirror）+ 真实对话框组件。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  type EditEventHandler = (event: Record<string, unknown>) => void;
  return {
    sessionOpen: vi.fn(async () => "sess-1"),
    sessionOpenQuick: vi.fn(async () => "sess-quick"),
    sessionDuplicate: vi.fn(async () => "sess-dup"),
    sessionClose: vi.fn(async () => {}),
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
    onTargetConnect: vi.fn(async () => () => {}),
    aiTargetConnectResult: vi.fn(async () => {}),
    editHandler: null as EditEventHandler | null,
    onSftpEdit: vi.fn(async (handler: EditEventHandler) => {
      mocks.editHandler = handler;
      return () => {};
    }),
    sftpEditResolve: vi.fn(async () => {}),
    sftpEditLeftovers: vi.fn(async () => []),
    sftpEditLeftoverAction: vi.fn(async () => ({ state: "done" })),
    sftpClose: vi.fn(async () => {}),
    sftpOpen: vi.fn(async () => "sftp-1"),
    editorOpen: vi.fn(),
    editorSave: vi.fn(),
    editorDraft: vi.fn(async () => {}),
    editorReload: vi.fn(),
    editorClose: vi.fn(async () => {}),
    localEditorOpen: vi.fn(),
    localEditorSave: vi.fn(),
  };
});

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

vi.mock("./api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
  dataDirInfo: vi.fn(async () => ({ data_dir: "", known_hosts: "" })),
  openDataDir: vi.fn(async () => {}),
}));

vi.mock("./api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/sftp")>();
  return {
    ...actual,
    onSftpEdit: mocks.onSftpEdit,
    sftpEditResolve: mocks.sftpEditResolve,
    sftpEditLeftovers: mocks.sftpEditLeftovers,
    sftpEditLeftoverAction: mocks.sftpEditLeftoverAction,
    sftpClose: mocks.sftpClose,
    sftpOpen: mocks.sftpOpen,
  };
});

vi.mock("./api/editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/editor")>();
  return {
    ...actual,
    editorOpen: mocks.editorOpen,
    editorSave: mocks.editorSave,
    editorDraft: mocks.editorDraft,
    editorReload: mocks.editorReload,
    editorClose: mocks.editorClose,
    localEditorOpen: mocks.localEditorOpen,
    localEditorSave: mocks.localEditorSave,
  };
});

// 标签栏：把标签标题和悬停提示渲染出来（真实的 TopTabs 在 jsdom 下不需要）。
vi.mock("./components/titlebar/TopTabs", () => ({
  default: (props: {
    tabs: Array<{ id: string; title: string; tooltip?: string }>;
    editorDirty?: (editId: number) => boolean;
    onCloseTabs?: (tabId: string, which: "others" | "right" | "all") => void;
    onToggleSftp?: () => void;
  }) => (
    <div data-testid="tabs">
      {props.tabs.map((tab) => (
        <span key={tab.id} data-tab-title={tab.title} title={tab.tooltip ?? tab.title}>
          {tab.title}
        </span>
      ))}
      <button data-testid="toggle-sftp-dock" onClick={() => props.onToggleSftp?.()}>
        文件面板
      </button>
      <button
        data-testid="close-all-tabs"
        onClick={() => props.tabs[0] && props.onCloseTabs?.(props.tabs[0].id, "all")}
      >
        关闭所有
      </button>
      <button
        data-testid="close-other-tabs"
        onClick={() => {
          const last = props.tabs[props.tabs.length - 1];
          if (last) props.onCloseTabs?.(last.id, "others");
        }}
      >
        关闭其他
      </button>
    </div>
  ),
}));
vi.mock("./components/settings/SettingsPage", () => ({ default: () => null }));
vi.mock("./components/vault/HostDetailsPanel", () => ({ default: () => null }));
vi.mock("./components/vault/KeychainPanel", () => ({ default: () => null }));
vi.mock("./components/vault/ProxyPanel", () => ({ default: () => null }));
vi.mock("./components/vault/VaultView", () => ({
  HostTreeAside: () => null,
  default: (props: {
    onOpenHost?: (host: { id: number; name: string }) => void;
    onOpenSftp?: (host: { id: number; name: string }) => void;
  }) => (
    <div data-testid="vault">
      <button data-testid="open-sftp" onClick={() => props.onOpenSftp?.({ id: 7, name: "测试主机" })}>
        打开 SFTP
      </button>
      <button data-testid="open-host" onClick={() => props.onOpenHost?.({ id: 7, name: "测试主机" })}>
        打开主机
      </button>
    </div>
  ),
}));
vi.mock("./components/PaneView", () => ({ default: () => null }));
// SFTP 标签的内容（双栏文件列表）和本节无关：只保留 App 里"这台主机有可用 SFTP 会话"的状态。
vi.mock("./components/sftp/SftpTabView", () => ({ default: () => null }));

import { EditorView } from "@codemirror/view";

type StoreModule = typeof import("./stores/editors");

const SFTP_ID = "sftp-1";

const INFO = {
  edit_id: 1,
  side: "remote" as const,
  text: "A\n",
  bom: false,
  eol: "LF" as const,
  file_name: "a.conf",
  path: "/srv/a.conf",
  host_id: 7,
  host_label: "测试主机",
  size: 2,
};

beforeAll(() => {
  // CodeMirror 量字符尺寸要用到这两个，jsdom 没有。
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

beforeEach(() => {
  vi.clearAllMocks();
  mocks.editHandler = null;
  mocks.editorOpen.mockResolvedValue(INFO);
  mocks.editorSave.mockResolvedValue({ state: "uploaded" });
  mocks.editorReload.mockResolvedValue({ text: "C\n", bom: false, eol: "LF" });
  mocks.sftpEditResolve.mockResolvedValue(undefined);
});

afterEach(cleanup);

/** 挂载真实的 App，并打开一个远端文件的内置编辑器标签。 */
async function mountWithEditor(): Promise<{
  view: ReturnType<typeof render>;
  stores: StoreModule;
}> {
  vi.resetModules();
  const { default: App } = await import("./App");
  const stores = await import("./stores/editors");
  const view = render(<App />);
  // 先给主机 7 开一个 SFTP 标签：编辑器保存要找“这台主机现在可用的 SFTP 会话”。
  act(() => {
    view.getByTestId("open-sftp").click();
  });
  await waitFor(() => expect(mocks.sftpOpen).toHaveBeenCalledWith(7));
  await waitFor(() => expect(stores.useEditors.getState().hooks).not.toBeNull());
  await act(async () => {
    await stores.openRemoteInEditor("s1", "/srv/a.conf", 7);
  });
  await waitFor(() => expect(view.container.querySelector(".cm-editor")).not.toBeNull());
  await waitFor(() => expect(mocks.onSftpEdit).toHaveBeenCalled());
  return { view, stores };
}

function editorView(): EditorView {
  const view = EditorView.findFromDOM(
    document.querySelector(".cm-editor") as HTMLElement,
  );
  if (!view) throw new Error("编辑器没有挂上");
  return view;
}

function editorText(): string {
  return editorView().state.doc.toString();
}

/** 在编辑器里整篇换成 `text`（jsdom 里没有真实键盘输入）。 */
async function typeText(text: string) {
  await act(async () => {
    const view = editorView();
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
  });
}

async function pushEvent(event: Record<string, unknown>) {
  await act(async () => {
    mocks.editHandler?.({ dirty: true, remote_gone: false, ...event });
  });
}

const conflictEvent = (patch: Record<string, unknown> = {}) => ({
  sftp_id: "s1",
  edit_id: 1,
  remote_path: "/srv/a.conf",
  state: "conflict",
  suggested_name: "a (副本).conf",
  remote_size: 2,
  remote_mtime: 1_700_000_000,
  ...patch,
});

async function clickSave() {
  await act(async () => {
    fireEvent.click(document.querySelector('[data-action="editor-save"]') as HTMLElement);
  });
}

describe("第 2 节：编辑器事件处理后的文档同步", () => {
  it("放弃我的修改后：编辑器里换成服务器上的 C，紧接着 Ctrl+S 传的是 C 不是 B", async () => {
    const { stores } = await mountWithEditor();
    await typeText("B\n");
    // 保存失败（冲突）→ 对话框出现。
    mocks.editorSave.mockResolvedValue({ state: "conflict", message: "服务器上的文件变了" });
    await clickSave();
    await pushEvent(conflictEvent());

    fireEvent.click(await screen.findByText("放弃我的修改"));
    await waitFor(() => expect(mocks.sftpEditResolve).toHaveBeenCalledWith(1, "discard", undefined));

    // 后端处理完：内容换回服务器上的 C。
    await pushEvent({ ...conflictEvent(), state: "discarded" });
    await waitFor(() => expect(editorText()).toBe("C\n"));
    expect(stores.useEditors.getState().docs[1].dirty).toBe(false);

    // 紧接着再保存：传上去的必须是 C，不能是放弃前的 B。
    await clickSave();
    await waitFor(() => expect(mocks.editorSave).toHaveBeenCalledTimes(2));
    expect(mocks.editorSave).toHaveBeenLastCalledWith(SFTP_ID, 1, "C\n", expect.any(Number));
  });

  it("另存为之后：文档路径、标签标题跟着后端的实际新路径变", async () => {
    const { stores } = await mountWithEditor();
    await typeText("B\n");
    mocks.editorSave.mockResolvedValue({ state: "conflict" });
    await clickSave();
    await pushEvent(conflictEvent());

    const input = document.querySelector('[data-section="edit-conflict"] input') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "a (副本).conf" } });
    fireEvent.click(await screen.findByText("另存为"));
    await waitFor(() =>
      expect(mocks.sftpEditResolve).toHaveBeenCalledWith(1, "save_as", "a (副本).conf"),
    );

    await pushEvent({
      ...conflictEvent(),
      state: "uploaded",
      remote_path: "/srv/a (副本).conf",
      suggested_name: "a (副本 2).conf",
    });
    await waitFor(() => {
      const doc = stores.useEditors.getState().docs[1];
      expect(doc.path).toBe("/srv/a (副本).conf");
      expect(doc.fileName).toBe("a (副本).conf");
    });
    // 没有新的输入：未保存标记清掉。
    expect(stores.useEditors.getState().docs[1].dirty).toBe(false);
    // 标签标题也跟着变。
    await waitFor(() =>
      expect(document.querySelector('[data-tab-title="a (副本).conf"]')).not.toBeNull(),
    );
  });

  it("覆盖远端成功：处理期间又打了字，仍然算未保存", async () => {
    const { stores } = await mountWithEditor();
    await typeText("B\n");
    mocks.editorSave.mockResolvedValue({ state: "conflict" });
    await clickSave();
    await pushEvent(conflictEvent());
    fireEvent.click(await screen.findByText("覆盖远端"));
    await waitFor(() => expect(mocks.sftpEditResolve).toHaveBeenCalledWith(1, "overwrite", undefined));

    // 处理请求在路上时又输入了新内容。
    await typeText("B + new typing\n");
    await pushEvent({ ...conflictEvent(), state: "uploaded" });
    await waitFor(() => expect(stores.useEditors.getState().docs[1].dirty).toBe(true));
    expect(stores.useEditors.getState().docs[1].getText?.()).toBe("B + new typing\n");
  });

  it("覆盖远端成功：处理期间没有新输入，未保存标记按实际传上去的版本清掉", async () => {
    const { stores } = await mountWithEditor();
    await typeText("B\n");
    mocks.editorSave.mockResolvedValue({ state: "conflict" });
    await clickSave();
    await pushEvent(conflictEvent());
    fireEvent.click(await screen.findByText("覆盖远端"));
    await waitFor(() => expect(mocks.sftpEditResolve).toHaveBeenCalled());

    await pushEvent({ ...conflictEvent(), state: "uploaded" });
    await waitFor(() => expect(stores.useEditors.getState().docs[1].dirty).toBe(false));
  });

  it("属主询问选「仍然保存」后：处理期间打的字仍然算未保存", async () => {
    const { stores } = await mountWithEditor();
    await typeText("B\n");
    mocks.editorSave.mockResolvedValue({ state: "owner_prompt", message: "属主会变" });
    await clickSave();
    await pushEvent({
      ...conflictEvent(),
      state: "owner_prompt",
      owner: { from: "uid 1001", to: "uid 1000", error: "权限不够" },
      message: "保存后属主会改变",
    });
    fireEvent.click(
      screen.getByRole("button", { name: /仍然保存（属主改变）/ }),
    );
    await waitFor(() =>
      expect(mocks.sftpEditResolve).toHaveBeenCalledWith(1, "owner_keep_new"),
    );

    await typeText("B + new typing\n");
    await pushEvent({ ...conflictEvent(), state: "uploaded" });
    await waitFor(() => expect(stores.useEditors.getState().docs[1].dirty).toBe(true));
  });
});

describe("复审 F5：批量关闭", () => {
  it("关闭所有：没保存的文档只问一次，选「不保存」后 SFTP 标签和编辑器标签都关掉", async () => {
    const { view } = await mountWithEditor();
    await typeText("B");
    expect(view.container.querySelectorAll("[data-tab-title]").length).toBe(2);

    await act(async () => {
      fireEvent.click(view.getByTestId("close-all-tabs"));
    });
    const discard = await waitFor(() => {
      const buttons = document.querySelectorAll('[data-action="unsaved-discard"]');
      expect(buttons.length).toBe(1);
      return buttons[0] as HTMLElement;
    });
    expect(document.body.textContent).toContain("a.conf");
    await act(async () => {
      fireEvent.click(discard);
    });
    await waitFor(() =>
      expect(view.container.querySelectorAll("[data-tab-title]").length).toBe(0),
    );
  });

  it("在编辑器标签上关闭其他：一起问了要断开的连接上的文档，保存后编辑器标签留着", async () => {
    const { view, stores } = await mountWithEditor();
    await typeText("B");
    const titles = () =>
      [...view.container.querySelectorAll("[data-tab-title]")].map((el) => el.getAttribute("data-tab-title"));
    expect(titles().length).toBe(2);

    await act(async () => {
      fireEvent.click(view.getByTestId("close-other-tabs"));
    });
    const save = await waitFor(() => {
      const buttons = document.querySelectorAll('[data-action="unsaved-save"]');
      expect(buttons.length).toBe(1);
      return buttons[0] as HTMLElement;
    });
    expect(document.body.textContent).toContain("a.conf");
    await act(async () => {
      fireEvent.click(save);
    });
    await waitFor(() => expect(titles().length).toBe(1));
    expect(mocks.editorSave).toHaveBeenCalled();
    expect(stores.useEditors.getState().docs[1]).toBeDefined();
    expect(view.container.querySelector(".cm-editor")).not.toBeNull();
  });
});

describe("编辑器标签下的文件面板", () => {
  it("远端文件的编辑器在前面时，文件面板接着显示这台主机，不当成快速连接", async () => {
    const { view } = await mountWithEditor();
    await act(async () => {
      fireEvent.click(view.getByTestId("toggle-sftp-dock"));
    });
    expect(document.body.textContent).not.toContain("快速连接");
    expect(screen.getByTitle("测试主机")).toBeTruthy();
    // 编辑器标签没有终端可跟，不显示"跟随终端目录"
    expect(document.querySelector('[data-section="follow-toggle"]')).toBeNull();
  });
});
