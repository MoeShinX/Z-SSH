/** @vitest-environment jsdom */
//! 关闭编辑器标签后回到打开它的那个标签（UI-3）。
//!
//! 后端用 mock 替身；前端走真实 App，TopTabs 换成能选中 / 关闭标签并标出当前标签的替身。

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

// 标签栏替身：列出所有标签，标出当前标签，能选中、能点 ×。
vi.mock("./components/titlebar/TopTabs", () => ({
  default: (props: {
    tabs: Array<{ id: string; title: string }>;
    activeTabId: string;
    onSelectTab: (id: string) => void;
    onCloseTab: (id: string) => void;
    onCloseTabs?: (id: string, which: "others" | "right" | "all") => void;
    onShowVault?: () => void;
  }) => (
    <div data-testid="tabs" data-active={props.activeTabId}>
      <button data-testid="show-vault" onClick={() => props.onShowVault?.()}>
        主机页
      </button>
      {props.tabs.map((tab) => (
        <span key={tab.id} data-testid={`tab-${tab.title}`} data-id={tab.id}>
          <button data-testid={`select-${tab.title}`} onClick={() => props.onSelectTab(tab.id)} />
          <button data-testid={`close-${tab.title}`} onClick={() => props.onCloseTab(tab.id)} />
          <button data-testid={`close-others-${tab.title}`} onClick={() => props.onCloseTabs?.(tab.id, "others")} />
        </span>
      ))}
    </div>
  ),
}));
vi.mock("./components/settings/SettingsPage", () => ({ default: () => null }));
vi.mock("./components/vault/HostDetailsPanel", () => ({ default: () => null }));
vi.mock("./components/vault/KeychainPanel", () => ({ default: () => null }));
vi.mock("./components/vault/ProxyPanel", () => ({ default: () => null }));
vi.mock("./components/vault/VaultView", () => ({
  HostTreeAside: () => null,
  default: (props: { onOpenHost?: (host: { id: number; name: string }) => void }) => (
    <>
      <button data-testid="open-a" onClick={() => props.onOpenHost?.({ id: 7, name: "甲" })} />
      <button data-testid="open-b" onClick={() => props.onOpenHost?.({ id: 8, name: "乙" })} />
    </>
  ),
}));
vi.mock("./components/PaneView", () => ({
  default: () => {
    mocks.paneRenders += 1;
    return null;
  },
}));


type StoreModule = typeof import("./stores/editors");

let nextEditId = 1;

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
  nextEditId = 1;
  mocks.editorOpen.mockImplementation(async (_sftpId: string, path: string) => ({
    edit_id: nextEditId++,
    side: "remote" as const,
    text: "A\n",
    bom: false,
    eol: "LF" as const,
    file_name: path.split("/").pop() ?? path,
    path,
    host_id: 7,
    host_label: "测试主机",
    size: 2,
  }));
});

afterEach(cleanup);

async function mount() {
  vi.resetModules();
  const { default: App } = await import("./App");
  const stores = await import("./stores/editors");
  const view = render(<App />);
  await waitFor(() => expect(stores.useEditors.getState().hooks).not.toBeNull());
  return { view, stores };
}

type View = ReturnType<typeof render>;

/** 在主机页里点一台主机，开出一个终端标签（标题是主机名）。 */
async function openHost(view: View, which: "a" | "b") {
  if (!view.queryByTestId(`open-${which}`)) {
    await act(async () => {
      view.getByTestId("show-vault").click();
    });
  }
  await act(async () => {
    view.getByTestId(`open-${which}`).click();
  });
}

async function openEditor(stores: StoreModule, path: string) {
  await act(async () => {
    await stores.openRemoteInEditor("s1", path, 7);
  });
}

async function click(view: View, id: string) {
  await act(async () => {
    view.getByTestId(id).click();
  });
}

/** 当前标签的标题。 */
function activeTitle(view: View): string {
  const active = view.getByTestId("tabs").dataset.active;
  const el = view.container.querySelector(`[data-id="${active}"]`);
  return el?.getAttribute("data-testid")?.replace("tab-", "") ?? "(无)";
}

/** 先开 甲，再开 乙，回到 甲 上打开编辑器：标签顺序是 甲、乙、1.txt。 */
async function setup() {
  const { view, stores } = await mount();
  await openHost(view, "a");
  await openHost(view, "b");
  await click(view, "select-甲");
  await openEditor(stores, "/srv/1.txt");
  await waitFor(() => expect(view.queryByTestId("tab-1.txt")).not.toBeNull());
  return { view, stores };
}

describe("关闭编辑器标签后回到打开它的标签", () => {
  it("从 甲 打开编辑器，关闭后回到 甲，而不是相邻的 乙", async () => {
    const { view } = await setup();
    expect(activeTitle(view)).toBe("1.txt");
    await click(view, "close-1.txt");
    await waitFor(() => expect(view.queryByTestId("tab-1.txt")).toBeNull());
    expect(activeTitle(view)).toBe("甲");
  });

  it("打开它的标签已经关了：按原规则选相邻标签", async () => {
    const { view } = await setup();
    await click(view, "close-甲");
    await waitFor(() => expect(view.queryByTestId("tab-甲")).toBeNull());
    await click(view, "select-1.txt");
    await click(view, "close-1.txt");
    await waitFor(() => expect(view.queryByTestId("tab-1.txt")).toBeNull());
    // 剩 乙：原规则选前一个相邻标签
    expect(activeTitle(view)).toBe("乙");
  });

  it("在 乙 时从标签栏关掉后台的编辑器标签：仍停在 乙", async () => {
    const { view } = await setup();
    await click(view, "select-乙");
    await click(view, "close-1.txt");
    await waitFor(() => expect(view.queryByTestId("tab-1.txt")).toBeNull());
    expect(activeTitle(view)).toBe("乙");
  });

  it("同一文件再次打开只是切过去，不改原来记的标签", async () => {
    const { view, stores } = await setup();
    await click(view, "select-乙");
    await openEditor(stores, "/srv/1.txt");
    expect(activeTitle(view)).toBe("1.txt");
    expect(mocks.editorOpen).toHaveBeenCalledTimes(1);
    await click(view, "close-1.txt");
    await waitFor(() => expect(view.queryByTestId("tab-1.txt")).toBeNull());
    expect(activeTitle(view)).toBe("甲");
  });

  it("批量关闭行为不变：关闭其他只留被点的那个", async () => {
    const { view } = await setup();
    await click(view, "close-others-1.txt");
    await waitFor(() => expect(view.queryByTestId("tab-甲")).toBeNull());
    expect(view.queryByTestId("tab-乙")).toBeNull();
    expect(activeTitle(view)).toBe("1.txt");
  });

  it("有未保存修改时确认放弃后，同样回到打开它的标签", async () => {
    const { view, stores } = await setup();
    await act(async () => {
      stores.useEditors.getState().patch(1, { dirty: true });
    });
    await click(view, "close-1.txt");
    const discard = await waitFor(() => {
      const button = Array.from(document.querySelectorAll("button")).find((b) =>
        /不保存|放弃/.test(b.textContent ?? ""),
      );
      expect(button).toBeTruthy();
      return button as HTMLButtonElement;
    });
    await act(async () => {
      discard.click();
    });
    await waitFor(() => expect(view.queryByTestId("tab-1.txt")).toBeNull());
    expect(activeTitle(view)).toBe("甲");
  });
});
