/** @vitest-environment jsdom */
//! 远端压缩 / 解压接在真实的 `SftpTabView` 上（UI-13）：右键菜单（含置灰）、压缩带着正确参数走到
//! 后端、解压先检查再按冲突弹对话框、取消、出错提示。

import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { ArchiveCaps, ExtractPlan, SftpEntry } from "../../api/sftp";

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
  sftpArchiveCaps: vi.fn(),
  sftpCompress: vi.fn(),
  sftpExtractPlan: vi.fn(),
  sftpExtract: vi.fn(),
  sftpPasteCancel: vi.fn(async () => {}),
  sftpPaste: vi.fn(),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    sftpList: mocks.sftpList,
    sftpLocalList: mocks.sftpLocalList,
    sftpLocalDefaultDir: vi.fn(async () => "C:\\work"),
    sftpTransfers: vi.fn(async () => []),
    sftpArchiveCaps: mocks.sftpArchiveCaps,
    sftpCompress: mocks.sftpCompress,
    sftpExtractPlan: mocks.sftpExtractPlan,
    sftpExtract: mocks.sftpExtract,
    sftpPasteCancel: mocks.sftpPasteCancel,
    sftpPaste: mocks.sftpPaste,
    onSftpArchiveProgress: vi.fn(async () => () => {}),
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

const FULL: ArchiveCaps = { posix: true, tar: true, zip: true, unzip: true };

const REMOTE_ENTRIES = [
  entry("logs", { kind: "dir" }),
  entry("app.conf"),
  entry("notes.txt"),
  entry("site.tar.gz"),
  entry("pack.zip"),
];
const LOCAL_ENTRIES = [entry("local.txt", { path: "C:\\work\\local.txt" })];

const PLAN: ExtractPlan = {
  format: "tar_gz",
  total_entries: 12,
  total_files: 10,
  conflict_count: 0,
  conflict_sample: [],
  dest_exists: false,
  fingerprint: { size: 4096, mtime: 1_700_000_000 },
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
  mocks.sftpArchiveCaps.mockReset().mockResolvedValue(FULL);
  mocks.sftpCompress.mockReset().mockResolvedValue({
    state: "ok",
    name: "app.conf.tar.gz",
    path: "/srv/app.conf.tar.gz",
  });
  mocks.sftpExtractPlan.mockReset().mockResolvedValue(PLAN);
  mocks.sftpExtract.mockReset().mockResolvedValue({ state: "ok", extracted: 10, skipped: 0 });
  mocks.sftpPasteCancel.mockClear();
  mocks.sftpPaste.mockReset();
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

async function renderWorkspace(onActiveCountChange?: (count: number) => void) {
  const view = render(
    <SftpTabView
      sftpId="s1"
      initialPaths={{ local: "C:\\work", remote: "/srv" }}
      onActiveCountChange={onActiveCountChange}
    />,
  );
  await waitFor(() =>
    expect(view.container.querySelectorAll("[data-row]").length).toBe(
      REMOTE_ENTRIES.length + LOCAL_ENTRIES.length,
    ),
  );
  await settleLoads();
  return view;
}

const pane = (container: HTMLElement, side: "local" | "remote") =>
  container.querySelector(`[data-pane-side="${side}"]`) as HTMLElement;

function rowByName(container: HTMLElement, name: string) {
  const rows = [...pane(container, "remote").querySelectorAll("[data-row]")];
  return rows.find((row) => row.textContent?.includes(name)) as HTMLElement;
}

const menuItem = (id: string) => document.querySelector(`[data-menu-id="${id}"]`) as HTMLElement | null;
const isDisabled = (id: string) => menuItem(id)?.getAttribute("aria-disabled") === "true";

async function rightClick(container: HTMLElement, name: string) {
  await act(async () => {
    fireEvent.click(rowByName(container, name));
  });
  await act(async () => {
    fireEvent.contextMenu(rowByName(container, name), { clientX: 400, clientY: 200 });
  });
}

async function choose(id: string) {
  await act(async () => {
    fireEvent.click(menuItem(id) as HTMLElement);
  });
}

/** 打开「压缩为」子菜单后点其中一项。 */
async function chooseCompress(id: "compress-tar-gz" | "compress-zip") {
  await choose("compress");
  await choose(id);
}

const busyStrip = () => document.querySelector('[data-testid="paste-busy"]') as HTMLElement | null;
const listCalls = () => mocks.sftpList.mock.calls.length;

describe("右键菜单", () => {
  it("第一次右键探测一次服务器上的工具，之后用缓存", async () => {
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    expect(mocks.sftpArchiveCaps).toHaveBeenCalledTimes(1);
    expect(mocks.sftpArchiveCaps).toHaveBeenCalledWith("s1");
    await act(async () => {
      fireEvent.keyDown(document.querySelector('[data-testid="sftp-context-menu"]') as HTMLElement, {
        key: "Escape",
      });
    });
    await rightClick(container, "notes.txt");
    expect(menuItem("compress")).not.toBeNull();
    expect(mocks.sftpArchiveCaps).toHaveBeenCalledTimes(1);
  });

  it("检测期间「压缩为」先置灰，写明正在检测", async () => {
    let finish: (caps: ArchiveCaps) => void = () => {};
    mocks.sftpArchiveCaps.mockImplementation(
      () => new Promise<ArchiveCaps>((resolve) => { finish = resolve; }),
    );
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    expect(isDisabled("compress")).toBe(true);
    expect(menuItem("compress")?.getAttribute("title")).toBe("正在检测服务器上的压缩工具…");
    await act(async () => finish(FULL));
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
  });

  it("服务器上没有 zip：.zip 置灰，悬停写明；.tar.gz 照常", async () => {
    mocks.sftpArchiveCaps.mockResolvedValue({ ...FULL, zip: false });
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await choose("compress");
    expect(isDisabled("compress-zip")).toBe(true);
    expect(menuItem("compress-zip")?.getAttribute("title")).toBe("服务器上没有 zip 命令");
    expect(isDisabled("compress-tar-gz")).toBe(false);
    await choose("compress-zip");
    expect(mocks.sftpCompress).not.toHaveBeenCalled();
  });

  it("登录 shell 不是 POSIX：没有压缩 / 解压菜单", async () => {
    mocks.sftpArchiveCaps.mockResolvedValue({ posix: false, tar: false, zip: false, unzip: false });
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(mocks.sftpArchiveCaps).toHaveBeenCalled());
    await settleLoads();
    expect(menuItem("compress")).toBeNull();
    expect(menuItem("extract-folder")).toBeNull();
    expect(menuItem("rename")).not.toBeNull();
  });

  it("探测失败：不显示这一组，下次右键再试", async () => {
    mocks.sftpArchiveCaps.mockRejectedValueOnce("没能在服务器上检测压缩工具");
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    await settleLoads();
    expect(menuItem("compress")).toBeNull();
    await act(async () => {
      fireEvent.keyDown(document.querySelector('[data-testid="sftp-context-menu"]') as HTMLElement, {
        key: "Escape",
      });
    });
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    expect(mocks.sftpArchiveCaps).toHaveBeenCalledTimes(2);
  });

  it("本地栏没有这一组", async () => {
    const { container } = await renderWorkspace();
    await act(async () => {
      fireEvent.contextMenu(
        [...pane(container, "local").querySelectorAll("[data-row]")][0] as HTMLElement,
        { clientX: 300, clientY: 200 },
      );
    });
    expect(menuItem("compress")).toBeNull();
    expect(mocks.sftpArchiveCaps).not.toHaveBeenCalled();
  });
});

describe("压缩", () => {
  it("选一个文件压成 .tar.gz：带着当前目录、名字、格式调后端，完成后提示并刷新", async () => {
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    const before = listCalls();
    await chooseCompress("compress-tar-gz");
    await waitFor(() => expect(mocks.sftpCompress).toHaveBeenCalledTimes(1));
    expect(mocks.sftpCompress).toHaveBeenCalledWith("s1", {
      op_id: expect.any(String),
      dir: "/srv",
      names: ["app.conf"],
      format: "tar_gz",
    });
    await waitFor(() => expect(toastMocks.success).toHaveBeenCalledWith("已压缩为「app.conf.tar.gz」"));
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
    expect(busyStrip()).toBeNull();
  });

  it("选多个条目压成 .zip：names 是选中的全部", async () => {
    const { container } = await renderWorkspace();
    await act(async () => {
      fireEvent.click(rowByName(container, "app.conf"));
    });
    await act(async () => {
      fireEvent.click(rowByName(container, "logs"), { ctrlKey: true });
    });
    await act(async () => {
      fireEvent.contextMenu(rowByName(container, "logs"), { clientX: 400, clientY: 200 });
    });
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-zip");
    await waitFor(() => expect(mocks.sftpCompress).toHaveBeenCalledTimes(1));
    const request = mocks.sftpCompress.mock.calls[0][1] as { names: string[]; format: string };
    expect([...request.names].sort()).toEqual(["app.conf", "logs"]);
    expect(request.format).toBe("zip");
  });

  it("压缩中显示「压缩中」和取消按钮，点了按任务号取消；取消后的提示说没有残留", async () => {
    const { container } = await renderWorkspace();
    let finish: (value: unknown) => void = () => {};
    mocks.sftpCompress.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    const strip = await waitFor(() => {
      const element = busyStrip();
      expect(element).not.toBeNull();
      return element as HTMLElement;
    });
    expect(strip.textContent).toContain("压缩中");
    const opId = (mocks.sftpCompress.mock.calls[0][1] as { op_id: string }).op_id;
    fireEvent.click([...strip.querySelectorAll("button")].find((b) => b.textContent === "取消")!);
    await waitFor(() => expect(mocks.sftpPasteCancel).toHaveBeenCalledWith("s1", opId));
    expect(strip.textContent).toContain("取消中");
    await act(async () => finish({ state: "cancelled" }));
    await waitFor(() => expect(busyStrip()).toBeNull());
    expect(toastMocks.info).toHaveBeenCalledWith("已取消压缩，没有留下临时文件");
    expect(toastMocks.success).not.toHaveBeenCalled();
  });

  it("失败：把后端给的原因（磁盘满、没权限）原样提示", async () => {
    mocks.sftpCompress.mockRejectedValue("压缩失败（退出码 2）：tar: No space left on device");
    const { container } = await renderWorkspace();
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    await waitFor(() =>
      expect(toastMocks.error).toHaveBeenCalledWith(
        "压缩失败：压缩失败（退出码 2）：tar: No space left on device",
      ),
    );
    expect(busyStrip()).toBeNull();
  });

  it("已有任务在服务器上进行时不再开第二个", async () => {
    const { container } = await renderWorkspace();
    mocks.sftpCompress.mockImplementation(() => new Promise(() => {}));
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    await waitFor(() => expect(busyStrip()).not.toBeNull());
    await rightClick(container, "notes.txt");
    await chooseCompress("compress-zip");
    expect(mocks.sftpCompress).toHaveBeenCalledTimes(1);
    expect(toastMocks.info).toHaveBeenCalledWith("服务器上已有一个任务在进行，请等它结束再试");
  });

  it("正在压缩时不开始复制粘贴，免得盖掉提示条的取消按钮", async () => {
    const { container } = await renderWorkspace();
    mocks.sftpCompress.mockImplementation(() => new Promise(() => {}));
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    await waitFor(() => expect(busyStrip()).not.toBeNull());
    useSftpClipboard.setState({
      content: { side: "remote", sftpId: "s1", items: [{ path: "/srv/notes.txt", is_dir: false }] },
    });
    await act(async () => {
      fireEvent.contextMenu(pane(container, "remote").querySelector("[tabindex]") as HTMLElement, {
        clientX: 420,
        clientY: 700,
      });
    });
    await choose("paste");
    expect(mocks.sftpPaste).not.toHaveBeenCalled();
    expect(busyStrip()?.textContent).toContain("压缩中");
  });
});

describe("解压", () => {
  it("压缩包上有「解压到「同名文件夹」」和「解压到当前目录」，普通文件没有", async () => {
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    expect(menuItem("extract-folder")?.textContent).toContain("解压到「site」");
    expect(menuItem("extract-here")?.textContent).toContain("解压到当前目录");
    await act(async () => {
      fireEvent.keyDown(document.querySelector('[data-testid="sftp-context-menu"]') as HTMLElement, {
        key: "Escape",
      });
    });
    await rightClick(container, "notes.txt");
    expect(menuItem("extract-folder")).toBeNull();
  });

  it("没有同名文件：先检查，再用「跳过已有文件」解压到 /srv/site，完成后提示解出多少并刷新", async () => {
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    const before = listCalls();
    await choose("extract-folder");
    await waitFor(() => expect(mocks.sftpExtractPlan).toHaveBeenCalledTimes(1));
    expect(mocks.sftpExtractPlan).toHaveBeenCalledWith("s1", {
      op_id: expect.any(String),
      archive: "/srv/site.tar.gz",
      dest: "/srv/site",
    });
    await waitFor(() => expect(mocks.sftpExtract).toHaveBeenCalledTimes(1));
    expect(mocks.sftpExtract).toHaveBeenCalledWith("s1", {
      op_id: expect.any(String),
      archive: "/srv/site.tar.gz",
      dest: "/srv/site",
      // 没冲突时不能用覆盖：检查之后才出现的同名文件不会被悄悄盖掉
      policy: "skip_existing",
      plan: {
        fingerprint: PLAN.fingerprint,
        dest_exists: false,
        total_files: 10,
        conflict_count: 0,
      },
    });
    expect(document.querySelector('[data-testid="extract-conflict"]')).toBeNull();
    await waitFor(() =>
      expect(toastMocks.success).toHaveBeenCalledWith("已解压到「site」：解出 10 个文件"),
    );
    await waitFor(() => expect(listCalls()).toBeGreaterThan(before));
  });

  it("解压到当前目录：目标就是当前目录", async () => {
    const { container } = await renderWorkspace();
    await rightClick(container, "pack.zip");
    await waitFor(() => expect(isDisabled("extract-here")).toBe(false));
    await choose("extract-here");
    await waitFor(() => expect(mocks.sftpExtractPlan).toHaveBeenCalledTimes(1));
    expect(mocks.sftpExtractPlan.mock.calls[0][1]).toMatchObject({
      archive: "/srv/pack.zip",
      dest: "/srv",
    });
  });

  it("有同名文件：弹对话框，选「跳过已有文件」才按 skip_existing 解压，并写清楚跳过了几个", async () => {
    mocks.sftpExtractPlan.mockResolvedValue({
      ...PLAN,
      dest_exists: true,
      conflict_count: 7,
      conflict_sample: ["site/a", "site/b", "site/c", "site/d", "site/e"],
    });
    mocks.sftpExtract.mockResolvedValue({ state: "ok", extracted: 3, skipped: 7 });
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    const dialog = await waitFor(() => {
      const element = document.querySelector('[data-testid="extract-conflict"]');
      expect(element).not.toBeNull();
      return element as HTMLElement;
    });
    expect(dialog.textContent).toContain("有 7 个文件在里面已经存在");
    expect(dialog.textContent).toContain("…等 7 个");
    expect(mocks.sftpExtract).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(document.querySelector('[data-action="extract-skip"]') as HTMLElement);
    });
    await waitFor(() => expect(mocks.sftpExtract).toHaveBeenCalledTimes(1));
    expect(mocks.sftpExtract.mock.calls[0][1]).toMatchObject({
      policy: "skip_existing",
      dest: "/srv/site",
      plan: { dest_exists: true, conflict_count: 7 },
    });
    await waitFor(() =>
      expect(toastMocks.success).toHaveBeenCalledWith(
        "已解压到「site」：解出 3 个文件，跳过 7 个已有文件",
      ),
    );
    expect(document.querySelector('[data-testid="extract-conflict"]')).toBeNull();
  });

  it("有同名文件时选「覆盖」按 overwrite 解压；选「取消」什么都不解压", async () => {
    mocks.sftpExtractPlan.mockResolvedValue({
      ...PLAN,
      dest_exists: true,
      conflict_count: 1,
      conflict_sample: ["site/a"],
    });
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    await waitFor(() => expect(document.querySelector('[data-testid="extract-conflict"]')).not.toBeNull());
    await act(async () => {
      fireEvent.click(document.querySelector('[data-action="extract-cancel"]') as HTMLElement);
    });
    expect(mocks.sftpExtract).not.toHaveBeenCalled();
    expect(toastMocks.info).toHaveBeenCalledWith("已取消解压");
    expect(document.querySelector('[data-testid="extract-conflict"]')).toBeNull();

    await rightClick(container, "site.tar.gz");
    await choose("extract-folder");
    await waitFor(() => expect(document.querySelector('[data-testid="extract-conflict"]')).not.toBeNull());
    await act(async () => {
      fireEvent.click(document.querySelector('[data-action="extract-overwrite"]') as HTMLElement);
    });
    await waitFor(() => expect(mocks.sftpExtract).toHaveBeenCalledTimes(1));
    expect(mocks.sftpExtract.mock.calls[0][1]).toMatchObject({ policy: "overwrite" });
  });

  it("压缩包里有指向目录外的路径：后端拒绝，提示原因，不解压", async () => {
    mocks.sftpExtractPlan.mockRejectedValue("压缩包里有指向目录外的路径，已拒绝解压");
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    await waitFor(() =>
      expect(toastMocks.error).toHaveBeenCalledWith(
        "没有解压：压缩包里有指向目录外的路径，已拒绝解压",
      ),
    );
    expect(mocks.sftpExtract).not.toHaveBeenCalled();
    expect(busyStrip()).toBeNull();
  });

  it("检查压缩包时显示「检查压缩包中」并能取消；取消了不再往下走", async () => {
    let finish: (value: ExtractPlan | null) => void = () => {};
    mocks.sftpExtractPlan.mockImplementation(
      () => new Promise<ExtractPlan | null>((resolve) => { finish = resolve; }),
    );
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    const strip = await waitFor(() => {
      const element = busyStrip();
      expect(element).not.toBeNull();
      return element as HTMLElement;
    });
    expect(strip.textContent).toContain("检查压缩包中");
    fireEvent.click([...strip.querySelectorAll("button")].find((b) => b.textContent === "取消")!);
    const opId = (mocks.sftpExtractPlan.mock.calls[0][1] as { op_id: string }).op_id;
    await waitFor(() => expect(mocks.sftpPasteCancel).toHaveBeenCalledWith("s1", opId));
    await act(async () => finish(null));
    await waitFor(() => expect(busyStrip()).toBeNull());
    expect(mocks.sftpExtract).not.toHaveBeenCalled();
    expect(toastMocks.info).toHaveBeenCalledWith("已取消解压");
  });

  it("解压中显示「解压中」；取消解到新文件夹的解压，提示没有半成品", async () => {
    let finish: (value: unknown) => void = () => {};
    mocks.sftpExtract.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    const strip = await waitFor(() => {
      const element = busyStrip();
      expect(element?.textContent).toContain("解压中");
      return element as HTMLElement;
    });
    fireEvent.click([...strip.querySelectorAll("button")].find((b) => b.textContent === "取消")!);
    await act(async () => finish({ state: "cancelled", extracted: 0, skipped: 0 }));
    await waitFor(() => expect(toastMocks.info).toHaveBeenCalledWith("已取消解压，没有留下半成品"));
    expect(busyStrip()).toBeNull();
  });

  it("解压失败：提示后端给的原因（例如磁盘满）", async () => {
    mocks.sftpExtract.mockRejectedValue("解压失败（退出码 2）：tar: No space left on device");
    const { container } = await renderWorkspace();
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    await waitFor(() =>
      expect(toastMocks.error).toHaveBeenCalledWith(
        "解压失败：解压失败（退出码 2）：tar: No space left on device",
      ),
    );
  });
});

describe("关标签前的确认（进行中的数量）", () => {
  // App 在关标签 / 停靠面板 / 退出程序时读这个数：大于 0 就先弹确认。

  it("压缩进行中上报 1（关标签会弹确认），结束后回到 0", async () => {
    const counts: number[] = [];
    const { container } = await renderWorkspace((count) => counts.push(count));
    expect(counts[counts.length - 1]).toBe(0);
    let finish: (value: unknown) => void = () => {};
    mocks.sftpCompress.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    await waitFor(() => expect(counts[counts.length - 1]).toBe(1));
    await act(async () => finish({ state: "ok", name: "a.tar.gz", path: "/srv/a.tar.gz" }));
    await waitFor(() => expect(counts[counts.length - 1]).toBe(0));
  });

  it("检查压缩包和解压进行中也算：整个过程里都是 1，结束后回到 0", async () => {
    const counts: number[] = [];
    let finishPlan: (value: unknown) => void = () => {};
    let finishExtract: (value: unknown) => void = () => {};
    mocks.sftpExtractPlan.mockImplementation(
      () => new Promise((resolve) => { finishPlan = resolve; }),
    );
    mocks.sftpExtract.mockImplementation(
      () => new Promise((resolve) => { finishExtract = resolve; }),
    );
    const { container } = await renderWorkspace((count) => counts.push(count));
    await rightClick(container, "site.tar.gz");
    await waitFor(() => expect(isDisabled("extract-folder")).toBe(false));
    await choose("extract-folder");
    await waitFor(() => expect(counts[counts.length - 1]).toBe(1));
    await act(async () => finishPlan(PLAN));
    await waitFor(() => expect(mocks.sftpExtract).toHaveBeenCalledTimes(1));
    expect(counts[counts.length - 1]).toBe(1);
    await act(async () => finishExtract({ state: "ok", extracted: 10, skipped: 0 }));
    await waitFor(() => expect(counts[counts.length - 1]).toBe(0));
  });

  it("压缩失败后也回到 0，不会让标签一直关不掉", async () => {
    const counts: number[] = [];
    mocks.sftpCompress.mockRejectedValue("压缩失败");
    const { container } = await renderWorkspace((count) => counts.push(count));
    await rightClick(container, "app.conf");
    await waitFor(() => expect(isDisabled("compress")).toBe(false));
    await chooseCompress("compress-tar-gz");
    await waitFor(() => expect(toastMocks.error).toHaveBeenCalled());
    await waitFor(() => expect(counts[counts.length - 1]).toBe(0));
    expect(counts).toContain(1);
  });
});
