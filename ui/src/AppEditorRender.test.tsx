/** @vitest-environment jsdom */
//! 编辑器输入不带动 App（PERF-2 第 2 项）：文档的 version 每次编辑都 +1，
//! 但 App 只订阅它真正用到的元数据（存在性、dirty、side、主机），输入时无关子树（终端窗格）不再渲染。
//!
//! 后端用 mock 替身；前端走真实 App + 真实 CodeMirror。TopTabs 换成能显示“哪个编辑器标签是脏的”的替身。

import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
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
  sftpOpen: vi.fn(async () => "sftp-1"),
  sftpClose: vi.fn(async () => {}),
  sftpEditLeftovers: vi.fn(async () => []),
  editorOpen: vi.fn(),
  editorDraft: vi.fn(async () => {}),
  editorClose: vi.fn(async () => {}),
  /** PaneView 被渲染了几次（App 重新渲染一次，它就跟着渲染一次）。 */
  paneRenders: 0,
}));

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
    sftpOpen: mocks.sftpOpen,
    sftpClose: mocks.sftpClose,
    sftpEditLeftovers: mocks.sftpEditLeftovers,
  };
});
vi.mock("./components/sftp/SftpTabView", () => ({ default: () => null }));
vi.mock("./api/editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api/editor")>();
  return {
    ...actual,
    editorOpen: mocks.editorOpen,
    editorDraft: mocks.editorDraft,
    editorClose: mocks.editorClose,
  };
});

// 标签栏替身：每个编辑器标签画出它的 editorDirty 结果。
vi.mock("./components/titlebar/TopTabs", () => ({
  default: (props: {
    tabs: Array<{ id: string; title: string; editor?: { editId: number } }>;
    editorDirty?: (editId: number) => boolean;
    onShowVault?: () => void;
  }) => (
    <div data-testid="tabs">
      <button data-testid="show-vault" onClick={() => props.onShowVault?.()}>
        主机页
      </button>
      {props.tabs.map((tab) =>
        tab.editor ? (
          <span
            key={tab.id}
            data-testid={`editor-tab-${tab.editor.editId}`}
            data-dirty={String(props.editorDirty?.(tab.editor.editId) ?? false)}
          >
            {tab.title}
          </span>
        ) : null,
      )}
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
    <>
      <button data-testid="open-host" onClick={() => props.onOpenHost?.({ id: 7, name: "测试主机" })}>
        打开主机
      </button>
      <button data-testid="open-sftp" onClick={() => props.onOpenSftp?.({ id: 7, name: "测试主机" })}>
        打开 SFTP
      </button>
    </>
  ),
}));
vi.mock("./components/PaneView", () => ({
  default: () => {
    mocks.paneRenders += 1;
    return null;
  },
}));

import { EditorView } from "@codemirror/view";

type StoreModule = typeof import("./stores/editors");

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
  mocks.paneRenders = 0;
  mocks.editorOpen.mockResolvedValue(INFO);
});

afterEach(cleanup);

/** 挂载真实的 App：一个终端标签 + 一个远端文件的编辑器标签。 */
async function mountWithEditor(): Promise<{
  view: ReturnType<typeof render>;
  stores: StoreModule;
}> {
  vi.resetModules();
  const { default: App } = await import("./App");
  const stores = await import("./stores/editors");
  const view = render(<App />);
  await act(async () => {
    view.getByTestId("open-host").click();
  });
  await waitFor(() => expect(mocks.paneRenders).toBeGreaterThan(0));
  await waitFor(() => expect(stores.useEditors.getState().hooks).not.toBeNull());
  await act(async () => {
    await stores.openRemoteInEditor("s1", "/srv/a.conf", 7);
  });
  await waitFor(() => expect(view.container.querySelector(".cm-editor")).not.toBeNull());
  return { view, stores };
}

function editorView(): EditorView {
  const view = EditorView.findFromDOM(document.querySelector(".cm-editor") as HTMLElement);
  if (!view) throw new Error("编辑器没有挂上");
  return view;
}

/** 在文档末尾追加一段文字（jsdom 里没有真实键盘输入）。 */
async function type(text: string) {
  await act(async () => {
    const view = editorView();
    view.dispatch({ changes: { from: view.state.doc.length, insert: text } });
  });
}

describe("编辑器输入不带动 App", () => {
  it("首次变脏时标签更新；之后连续输入终端窗格不再渲染，文本和版本照常变化", async () => {
    const { view, stores } = await mountWithEditor();
    expect(view.getByTestId("editor-tab-1").dataset.dirty).toBe("false");

    await type("x");
    await waitFor(() => expect(view.getByTestId("editor-tab-1").dataset.dirty).toBe("true"));
    expect(stores.useEditors.getState().docs[1].version).toBe(1);

    const before = mocks.paneRenders;
    for (const ch of ["b", "c", "d", "e", "f"]) await type(ch);
    expect(editorView().state.doc.toString()).toBe("A\nxbcdef");
    expect(stores.useEditors.getState().docs[1].version).toBe(6);
    expect(stores.useEditors.getState().docs[1].dirty).toBe(true);
    expect(mocks.paneRenders).toBe(before);
  });

  it("元数据变化仍然传到 App：放弃修改变回干净，关掉文档标签消失", async () => {
    const { view, stores } = await mountWithEditor();
    await type("x");
    await waitFor(() => expect(view.getByTestId("editor-tab-1").dataset.dirty).toBe("true"));

    await act(async () => {
      stores.useEditors.getState().patch(1, { dirty: false });
    });
    expect(view.getByTestId("editor-tab-1").dataset.dirty).toBe("false");

    await act(async () => {
      stores.useEditors.getState().remove(1);
    });
    expect(view.container.querySelector(".cm-editor")).toBeNull();
  });
});

describe("主机的 SFTP 会话变化仍然传到编辑器标签", () => {
  it("编辑器先开、SFTP 后连上：断线提示消失（不依赖输入触发重渲染）", async () => {
    const { view } = await mountWithEditor();
    expect(view.getByTestId("editor-error").textContent).toContain("连接已经断开");

    // 回到主机页再打开这台主机的 SFTP 标签
    await act(async () => {
      view.getByTestId("show-vault").click();
    });
    await act(async () => {
      view.getByTestId("open-sftp").click();
    });
    await waitFor(() => expect(mocks.sftpOpen).toHaveBeenCalledWith(7));
    await waitFor(() => expect(view.queryByTestId("editor-error")).toBeNull());
  });
});
