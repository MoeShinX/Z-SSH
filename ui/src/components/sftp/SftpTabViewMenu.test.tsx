/** @vitest-environment jsdom */
//! 右键菜单接在真实的 `SftpTabView` 上（M3-5 第 1 节）：两个面板、点在文件 / 文件夹 / 空白处，
//! 菜单项和禁用状态正确，点了确实走到对应的操作。

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import type { SftpEntry } from "../../api/sftp";

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
  sftpTransferSubmit: vi.fn(async () => [1]),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: mocks.sftpList,
    sftpLocalList: mocks.sftpLocalList,
    sftpLocalDefaultDir: vi.fn(async () => "C:\\work"),
    sftpTransfers: vi.fn(async () => []),
    sftpTransferSubmit: mocks.sftpTransferSubmit,
    onSftpTransfer: vi.fn(async () => () => {}),
    onSftpConflict: vi.fn(async () => () => {}),
    onSftpDelete: vi.fn(async () => () => {}),
    onSftpPasteProgress: vi.fn(async () => () => {}),
    onSftpArchiveProgress: vi.fn(async () => () => {}),
    // 一直在检测：菜单里的「压缩为」保持置灰（检测完的几种情况在 SftpTabViewArchive.test.tsx）
    sftpArchiveCaps: vi.fn(() => new Promise(() => {})),
    sftpPasteCancel: vi.fn(async () => {}),
    onSftpEdit: vi.fn(async () => () => {}),
  };
});

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import SftpTabView from "./SftpTabView";

const REMOTE_ENTRIES = [
  entry("logs", { kind: "dir" }),
  entry("app.conf"),
  entry("big.sql", { size: 9 * 1024 * 1024 }),
];
const LOCAL_ENTRIES = [
  entry("notes.txt", { path: "C:\\work\\notes.txt" }),
  entry("pics", { kind: "dir", path: "C:\\work\\pics" }),
];

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
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: 1200,
  });
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: 800,
  });
});

beforeEach(() => {
  mocks.sftpList
    .mockReset()
    .mockResolvedValue({ path: "/srv", entries: REMOTE_ENTRIES });
  mocks.sftpLocalList.mockReset().mockResolvedValue(LOCAL_ENTRIES);
  mocks.sftpTransferSubmit.mockClear();
});

afterEach(cleanup);

/** 等到两栏的目录加载都停下来（调用次数连续几轮没变），免得后来的重新加载把测试里的选择清掉。 */
async function settleLoads() {
  let stable = 0;
  let last = -1;
  while (stable < 4) {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    const now =
      mocks.sftpList.mock.calls.length + mocks.sftpLocalList.mock.calls.length;
    stable = now === last ? stable + 1 : 0;
    last = now;
  }
}

async function renderWorkspace() {
  const view = render(
    <SftpTabView
      sftpId="s1"
      initialPaths={{ local: "C:\\work", remote: "/srv" }}
    />,
  );
  await waitFor(() =>
    expect(view.container.querySelectorAll("[data-row]").length).toBe(5),
  );
  await settleLoads();
  return view;
}

function pane(container: HTMLElement, side: "local" | "remote") {
  return container.querySelector(`[data-pane-side="${side}"]`) as HTMLElement;
}

function rowByName(
  container: HTMLElement,
  side: "local" | "remote",
  name: string,
) {
  const rows = [...pane(container, side).querySelectorAll("[data-row]")];
  return rows.find((row) => row.textContent?.includes(name)) as HTMLElement;
}

const menuItem = (id: string) =>
  document.querySelector(`[data-menu-id="${id}"]`) as HTMLElement;
const menuIds = () =>
  [...document.querySelectorAll("[data-menu-id]")].map((node) =>
    node.getAttribute("data-menu-id"),
  );
const isDisabled = (id: string) =>
  menuItem(id).getAttribute("aria-disabled") === "true";

describe("远端面板的右键菜单", () => {
  it("点在文件上：菜单是卡片里的那一套；内置编辑器已接上，复制 / 属性这一步还没接上，禁用并写明", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), {
      clientX: 400,
      clientY: 200,
    });
    expect(menuIds()).toEqual([
      "open",
      "edit-builtin",
      "transfer",
      "download-to",
      "compress",
      "copy",
      "paste",
      "copy-path",
      "rename",
      "delete",
      "new",
      "permissions",
      "refresh",
      "properties",
    ]);
    // 第一次右键，服务器上的压缩工具还在检测：「压缩为」先置灰
    expect(isDisabled("compress")).toBe(true);
    expect(menuItem("compress").getAttribute("title")).toBe("正在检测服务器上的压缩工具…");
    expect(menuItem("transfer").textContent).toContain("下载到本地");
    expect(isDisabled("rename")).toBe(false);
    expect(isDisabled("edit-builtin")).toBe(false);
  });

  it("点在文件夹上：不能用编辑器打开，可以下载、重命名、改权限", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "remote", "logs"), {
      clientX: 400,
      clientY: 200,
    });
    expect(isDisabled("transfer")).toBe(false);
    expect(isDisabled("permissions")).toBe(false);
  });

  it("超过大小上限的文件：不能用内置编辑器打开，悬停说明", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "remote", "big.sql"), {
      clientX: 400,
      clientY: 200,
    });
    expect(isDisabled("edit-builtin")).toBe(true);
    expect(menuItem("edit-builtin").getAttribute("title")).toContain("超过 5 MB");
  });

  it("点在空白处：只有 粘贴、新建、刷新、属性", async () => {
    const { container } = await renderWorkspace();
    const list = pane(container, "remote").querySelector(
      "[tabindex]",
    ) as HTMLElement;
    fireEvent.contextMenu(list, { clientX: 600, clientY: 700 });
    expect(menuIds()).toEqual(["paste", "new", "refresh", "properties"]);
  });

  it("多选：只能对单个条目的项禁用", async () => {
    const { container } = await renderWorkspace();
    const list = pane(container, "remote").querySelector(
      "[tabindex]",
    ) as HTMLElement;
    fireEvent.keyDown(list, { key: "a", ctrlKey: true });
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), {
      clientX: 400,
      clientY: 200,
    });
    expect(isDisabled("rename")).toBe(true);
    expect(isDisabled("permissions")).toBe(true);
    expect(isDisabled("open")).toBe(true);
    expect(isDisabled("delete")).toBe(false);
    expect(isDisabled("transfer")).toBe(false);
  });

  it("点「下载到本地」提交一个下载任务，目标是本地栏当前目录", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), {
      clientX: 400,
      clientY: 200,
    });
    fireEvent.click(menuItem("transfer"));
    await waitFor(() =>
      expect(mocks.sftpTransferSubmit).toHaveBeenCalledWith(
        "s1",
        "download",
        ["/srv/app.conf"],
        "C:\\work",
      ),
    );
  });

  it("点「刷新」重新读目录", async () => {
    const { container } = await renderWorkspace();
    const before = mocks.sftpList.mock.calls.length;
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), {
      clientX: 400,
      clientY: 200,
    });
    fireEvent.click(menuItem("refresh"));
    await waitFor(() =>
      expect(mocks.sftpList.mock.calls.length).toBeGreaterThan(before),
    );
  });
});

describe("本地面板的右键菜单", () => {
  it("传输是「上传到远端」，没有「下载到…」和「修改权限」", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "local", "notes.txt"), {
      clientX: 200,
      clientY: 200,
    });
    expect(menuIds()).not.toContain("download-to");
    expect(menuIds()).not.toContain("permissions");
    expect(menuItem("transfer").textContent).toContain("上传到远端");
    fireEvent.click(menuItem("transfer"));
    await waitFor(() =>
      expect(mocks.sftpTransferSubmit).toHaveBeenCalledWith(
        "s1",
        "upload",
        ["C:\\work\\notes.txt"],
        "/srv",
      ),
    );
  });

  it("本地空白处同样只有 粘贴、新建、刷新、属性", async () => {
    const { container } = await renderWorkspace();
    const list = pane(container, "local").querySelector(
      "[tabindex]",
    ) as HTMLElement;
    await act(async () => {
      fireEvent.contextMenu(list, { clientX: 100, clientY: 700 });
    });
    expect(menuIds()).toEqual(["paste", "new", "refresh", "properties"]);
  });
});

describe("子菜单「新建」", () => {
  it("展开后有「文件夹」「文件」", async () => {
    const { container } = await renderWorkspace();
    fireEvent.contextMenu(rowByName(container, "remote", "app.conf"), {
      clientX: 400,
      clientY: 200,
    });
    fireEvent.mouseEnter(menuItem("new"));
    expect(menuItem("mkdir").textContent).toBe("文件夹");
    expect(menuItem("create").textContent).toBe("文件");
    expect(screen.getByTestId("sftp-context-submenu")).toBeTruthy();
  });
});
