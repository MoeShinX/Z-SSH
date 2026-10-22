/** @vitest-environment jsdom */
//! 复制 / 粘贴接在真实的 `SftpTabView` 上（M3-5 第 3 节）：菜单和快捷键放进剪贴板，
//! 粘贴时带着正确的参数走到后端，远端和本地之间不能粘贴。

import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { PasteOutcome, SftpEntry } from "../../api/sftp";

function entry(name: string, patch: Partial<SftpEntry> = {}): SftpEntry {
  return {
    name,
    path: `/srv/${name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    size: 120,
    mtime: 1_700_000_000,
    ...patch,
  };
}

const mocks = vi.hoisted(() => ({
  sftpList: vi.fn(),
  sftpLocalList: vi.fn(),
  sftpPaste: vi.fn(),
  sftpPasteCancel: vi.fn(async () => {}),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: mocks.sftpList,
    sftpLocalList: mocks.sftpLocalList,
    sftpLocalDefaultDir: vi.fn(async () => "C:\\work"),
    sftpTransfers: vi.fn(async () => []),
    sftpPaste: mocks.sftpPaste,
    sftpPasteCancel: mocks.sftpPasteCancel,
    onSftpPasteProgress: vi.fn(async () => () => {}),
    onSftpTransfer: vi.fn(async () => () => {}),
    onSftpConflict: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpEdit: vi.fn(async () => () => {}),
    onSftpSize: vi.fn(async () => () => {}),
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

const toastMocks = vi.hoisted(() => ({
  error: vi.fn(),
  info: vi.fn(),
  success: vi.fn(),
}));
vi.mock("../../components/ui/toast", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../components/ui/toast")>();
  return { ...actual, toast: toastMocks };
});

import { useSftpClipboard } from "../../stores/sftpClipboard";
import SftpTabView from "./SftpTabView";

const REMOTE_ENTRIES = [
  entry("logs", { kind: "dir" }),
  entry("app.conf"),
  entry("big.sql"),
];
const LOCAL_ENTRIES = [
  entry("notes.txt", { path: "C:\\work\\notes.txt" }),
  entry("pics", { kind: "dir", path: "C:\\work\\pics" }),
];

const OK: PasteOutcome = {
  method: "server_cp",
  method_label: "服务器端复制（cp）",
  items: [{ name: "app.conf", state: "ok" }],
  job_ids: [],
  elapsed_ms: 1200,
};

beforeAll(() => {
  class FakeResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get() {
      return 600;
    },
  });
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
});

beforeEach(() => {
  mocks.sftpList.mockReset().mockResolvedValue({ path: "/srv", entries: REMOTE_ENTRIES });
  mocks.sftpLocalList.mockReset().mockResolvedValue(LOCAL_ENTRIES);
  mocks.sftpPaste.mockReset().mockResolvedValue(OK);
  useSftpClipboard.setState({ content: null });
});

afterEach(() => {
  cleanup();
  toastMocks.error.mockClear();
  toastMocks.info.mockClear();
  toastMocks.success.mockClear();
});

async function settleLoads() {
  let stable = 0;
  let last = -1;
  while (stable < 4) {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    const now = mocks.sftpList.mock.calls.length + mocks.sftpLocalList.mock.calls.length;
    stable = now === last ? stable + 1 : 0;
    last = now;
  }
}

async function renderWorkspace() {
  const view = render(
    <SftpTabView sftpId="s1" initialPaths={{ local: "C:\\work", remote: "/srv" }} />,
  );
  await waitFor(() => expect(view.container.querySelectorAll("[data-row]").length).toBe(5));
  await settleLoads();
  return view;
}

const pane = (container: HTMLElement, side: "local" | "remote") =>
  container.querySelector(`[data-pane-side="${side}"]`) as HTMLElement;

function rowByName(container: HTMLElement, side: "local" | "remote", name: string) {
  const rows = [...pane(container, side).querySelectorAll("[data-row]")];
  return rows.find((row) => row.textContent?.includes(name)) as HTMLElement;
}

/** 文件列表的空白处（列表容器本身）。 */
const blank = (container: HTMLElement, side: "local" | "remote") =>
  pane(container, side).querySelector("[tabindex]") as HTMLElement;

const menuItem = (id: string) => document.querySelector(`[data-menu-id="${id}"]`) as HTMLElement;
const isDisabled = (id: string) => menuItem(id).getAttribute("aria-disabled") === "true";

async function select(container: HTMLElement, side: "local" | "remote", name: string) {
  await act(async () => {
    fireEvent.click(rowByName(container, side, name));
  });
}

async function choose(id: string) {
  await act(async () => {
    fireEvent.click(menuItem(id));
  });
}

describe("复制 / 粘贴", () => {
  it("右键复制 → 空白处右键粘贴：带着来源会话、条目和目标目录调后端", async () => {
    const { container } = await renderWorkspace();
    await select(container, "remote", "app.conf");
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), { clientX: 400, clientY: 200 });
    await choose("copy");
    expect(useSftpClipboard.getState().content).toMatchObject({
      side: "remote",
      sftpId: "s1",
      items: [{ path: "/srv/app.conf", is_dir: false }],
    });

    fireEvent.contextMenu(blank(container, "remote"), { clientX: 420, clientY: 700 });
    await choose("paste");
    await waitFor(() => expect(mocks.sftpPaste).toHaveBeenCalledTimes(1));
    expect(mocks.sftpPaste).toHaveBeenCalledWith(
      "s1",
      expect.objectContaining({
        side: "remote",
        source_sftp_id: "s1",
        items: [{ path: "/srv/app.conf", is_dir: false }],
        target_dir: "/srv",
        paste_id: expect.any(String),
      }),
    );
    expect(useSftpClipboard.getState().content).not.toBeNull();
  });

  it("服务器端复制中显示进度和取消按钮，点了按粘贴号取消", async () => {
    const { container } = await renderWorkspace();
    let finish: (outcome: PasteOutcome) => void = () => {};
    mocks.sftpPaste.mockImplementation(
      () => new Promise<PasteOutcome>((resolve) => { finish = resolve; }),
    );
    useSftpClipboard.setState({
      content: {
        side: "remote",
        sftpId: "s1",
        items: [{ path: "/srv/logs", is_dir: true }],
      },
    });
    fireEvent.contextMenu(blank(container, "remote"), { clientX: 420, clientY: 700 });
    await choose("paste");
    const busy = await waitFor(() => {
      const element = document.querySelector('[data-testid="paste-busy"]') as HTMLElement | null;
      expect(element).not.toBeNull();
      return element as HTMLElement;
    });
    expect(busy.textContent).toContain("服务器上复制中");
    const pasteId = (mocks.sftpPaste.mock.calls[0][1] as { paste_id: string }).paste_id;
    expect(pasteId).toBeTruthy();
    fireEvent.click([...busy.querySelectorAll("button")].find((b) => b.textContent === "取消")!);
    await waitFor(() => expect(mocks.sftpPasteCancel).toHaveBeenCalledWith("s1", pasteId));
    expect(busy.textContent).toContain("取消中");
    await act(async () => {
      finish({
        ...OK,
        items: [{ name: "logs", state: "cancelled" }],
      });
    });
    await waitFor(() => expect(document.querySelector('[data-testid="paste-busy"]')).toBeNull());
  });

  it("cp 失败后结果里有「改为经本机中转重试」，点了带 force_relay 再粘一次", async () => {
    const { container } = await renderWorkspace();
    mocks.sftpPaste
      .mockResolvedValueOnce({
        ...OK,
        items: [
          {
            name: "app.conf",
            state: "failed",
            path: "/srv/app.conf",
            is_dir: false,
            can_relay: true,
            message: "服务器上的 cp 失败（退出码 1）：No space left",
          },
        ],
      })
      .mockResolvedValueOnce({
        ...OK,
        method: "relay",
        method_label: "经本机中转",
        items: [{ name: "app.conf", state: "queued" }],
        job_ids: [7],
      });
    useSftpClipboard.setState({
      content: {
        side: "remote",
        sftpId: "s1",
        items: [{ path: "/srv/app.conf", is_dir: false }],
      },
    });
    fireEvent.contextMenu(blank(container, "remote"), { clientX: 420, clientY: 700 });
    await choose("paste");
    await waitFor(() => expect(toastMocks.error).toHaveBeenCalled());
    const options = toastMocks.error.mock.calls.find(
      (call) => (call[1] as { actionLabel?: string } | undefined)?.actionLabel,
    )?.[1] as { actionLabel: string; onClick: () => void };
    expect(options.actionLabel).toBe("改为经本机中转重试");
    await act(async () => {
      options.onClick();
    });
    await waitFor(() => expect(mocks.sftpPaste).toHaveBeenCalledTimes(2));
    const retry = mocks.sftpPaste.mock.calls[1][1] as Record<string, unknown>;
    expect(retry).toMatchObject({
      force_relay: true,
      side: "remote",
      target_dir: "/srv",
      items: [{ path: "/srv/app.conf", is_dir: false }],
    });
    expect(retry.paste_id).not.toBe(mocks.sftpPaste.mock.calls[0][1].paste_id);
  });

  it("远端复制的东西不能粘贴到本地栏：菜单里禁用并写明去用上传 / 下载", async () => {
    const { container } = await renderWorkspace();
    useSftpClipboard.setState({
      content: {
        side: "remote",
        sftpId: "s1",
        items: [{ path: "/srv/app.conf", is_dir: false }],
      },
    });
    fireEvent.contextMenu(rowByName(container, "local", "pics"), { clientX: 300, clientY: 200 });
    expect(isDisabled("paste")).toBe(true);
    expect(menuItem("paste").getAttribute("title")).toBe("远端和本地之间请用上传 / 下载");
    await choose("paste");
    expect(mocks.sftpPaste).not.toHaveBeenCalled();
  });

  it("Ctrl+C / Ctrl+V 只在文件列表有焦点时生效：列表里按键进剪贴板并能粘贴", async () => {
    const { container } = await renderWorkspace();
    await select(container, "local", "notes.txt");
    const list = pane(container, "local");
    fireEvent.keyDown(rowByName(container, "local", "notes.txt"), { key: "c", ctrlKey: true });
    expect(useSftpClipboard.getState().content).toMatchObject({
      side: "local",
      items: [{ path: "C:\\work\\notes.txt", is_dir: false }],
    });
    fireEvent.keyDown(list.querySelector("[data-row]") as HTMLElement, { key: "v", ctrlKey: true });
    await waitFor(() => expect(mocks.sftpPaste).toHaveBeenCalledTimes(1));
    expect(mocks.sftpPaste.mock.calls[0][1]).toMatchObject({
      side: "local",
      target_dir: "C:\\work",
      source_sftp_id: undefined,
    });
  });

  it("终端里的 Ctrl+C 不经过文件列表：文档上直接按键不会碰剪贴板", async () => {
    await renderWorkspace();
    fireEvent.keyDown(document.body, { key: "c", ctrlKey: true });
    expect(useSftpClipboard.getState().content).toBeNull();
  });

  it("后端报错（例如源所在的连接已关闭）：把原因提示出来，剪贴板不动", async () => {
    const { container } = await renderWorkspace();
    useSftpClipboard.setState({
      content: {
        side: "remote",
        sftpId: "gone",
        items: [{ path: "/srv/app.conf", is_dir: false }],
      },
    });
    mocks.sftpPaste.mockRejectedValue("复制的源所在的连接已经关闭，请重新打开那个标签再复制");
    fireEvent.contextMenu(blank(container, "remote"), { clientX: 420, clientY: 700 });
    await choose("paste");
    await waitFor(() => expect(mocks.sftpPaste).toHaveBeenCalled());
    expect(useSftpClipboard.getState().content?.sftpId).toBe("gone");
  });
});

describe("属性", () => {
  it("右键「属性」和 Alt+Enter 都打开对话框，文件夹有「计算大小」", async () => {
    const { container } = await renderWorkspace();
    await select(container, "remote", "logs");
    fireEvent.contextMenu(rowByName(container, "remote", "logs"), { clientX: 400, clientY: 200 });
    await choose("properties");
    expect(document.querySelector('[data-section="sftp-properties"]')).not.toBeNull();
    expect(document.querySelector('[data-prop="名称"]')?.textContent).toBe("logs");
    expect(document.querySelector('[data-action="size-start"]')).not.toBeNull();
    await act(async () => {
      fireEvent.click(document.querySelector('[data-action="properties-close"]') as HTMLElement);
    });
    expect(document.querySelector('[data-section="sftp-properties"]')).toBeNull();

    await select(container, "remote", "app.conf");
    await act(async () => {
      fireEvent.keyDown(rowByName(container, "remote", "app.conf"), { key: "Enter", altKey: true });
    });
    expect(document.querySelector('[data-prop="名称"]')?.textContent).toBe("app.conf");
    expect(document.querySelector('[data-action="size-start"]')).toBeNull();
  });

  it("空白处的「属性」是当前目录本身", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(blank(container, "remote"), { clientX: 420, clientY: 700 });
    await choose("properties");
    expect(document.querySelector('[data-prop="位置"]')?.textContent).toBe("/srv");
    expect(document.querySelector('[data-prop="类型"]')?.textContent).toBe("文件夹");
  });
});
