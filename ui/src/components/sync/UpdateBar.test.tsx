/** @vitest-environment jsdom */
//! 更新提示条：有新版本不弹窗、点开才有说明和三个按钮、立即更新 / 稍后 / 跳过各走各的；
//! 重启前有传输 / AI 命令 / 没上传的编辑要先问。

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { UpdateStatus } from "../../api/updater";
import { useUpdater } from "../../stores/updater";
import UpdateBar from "./UpdateBar";

const api = vi.hoisted(() => ({
  updateStatus: vi.fn(),
  updateCheck: vi.fn(),
  updateDownload: vi.fn(),
  updateBusy: vi.fn(),
  updateInstall: vi.fn(),
  updateSkip: vi.fn(),
  updateSetAuto: vi.fn(),
  updateOpenDownloadPage: vi.fn(),
  onUpdateStatus: vi.fn(),
}));
vi.mock("../../api/updater", () => api);

const AVAILABLE: UpdateStatus = {
  phase: "available",
  version: "0.2.0",
  notes: "修了几个问题\n加了新功能",
  date: null,
};
const PREFS = { auto_check: true, last_check_at: null, skipped_version: null };

function setStatus(status: UpdateStatus | null) {
  useUpdater.setState({
    status,
    dismissedVersion: null,
    dialogOpen: false,
    prefs: PREFS,
    portable: false,
    auto: false,
  });
}

const idle = () => ({ transfers: 0, unsavedEdits: 0 });

beforeEach(() => {
  api.updateBusy.mockResolvedValue({ transfers: 0, ai_commands: 0, unsaved_edits: 0 });
  api.updateInstall.mockResolvedValue(undefined);
  api.updateDownload.mockResolvedValue({
    status: { phase: "ready", version: "0.2.0" },
    prefs: PREFS,
    current_version: "0.1.0",
  });
  api.updateSkip.mockResolvedValue({
    status: { phase: "up_to_date", checked_at: 1 },
    prefs: { ...PREFS, skipped_version: "0.2.0" },
    current_version: "0.1.0",
  });
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("UpdateBar", () => {
  it("有新版本只出现一条提示，不弹对话框", () => {
    setStatus(AVAILABLE);
    render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.getByTestId("update-bar").textContent).toContain("v0.2.0");
    expect(screen.queryByTestId("update-notes")).toBeNull();
  });

  it("点开看到更新说明；立即更新会开始下载", async () => {
    setStatus(AVAILABLE);
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    expect((await screen.findByTestId("update-notes")).textContent).toContain("加了新功能");
    fireEvent.click(screen.getByRole("button", { name: "立即更新" }));
    await waitFor(() => expect(api.updateDownload).toHaveBeenCalledTimes(1));
  });

  it("稍后：收起提示，这一次运行里不再出现，也不调后端", async () => {
    setStatus(AVAILABLE);
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    fireEvent.click(await screen.findByRole("button", { name: "稍后" }));
    await waitFor(() => expect(screen.queryByTestId("update-bar")).toBeNull());
    expect(api.updateSkip).not.toHaveBeenCalled();
  });

  it("跳过这个版本：告诉后端记下这个版本号", async () => {
    setStatus(AVAILABLE);
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    fireEvent.click(await screen.findByRole("button", { name: "跳过这个版本" }));
    await waitFor(() => expect(api.updateSkip).toHaveBeenCalledWith("0.2.0"));
  });

  it("下载中显示进度；下载失败显示原因并能重试", async () => {
    setStatus({ phase: "downloading", version: "0.2.0", downloaded: 1024, total: 4096 });
    const { rerender } = render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.getByTestId("update-bar").textContent).toContain("25%");
    expect(screen.getByTestId("update-bar-progress").style.width).toBe("25%");
    setStatus({ phase: "failed", stage: "download", message: "更新包的签名校验没有通过，已拒绝安装" });
    rerender(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.getByTestId("update-bar").textContent).toContain("签名校验没有通过");
    fireEvent.click(screen.getByTestId("update-bar"));
    fireEvent.click(await screen.findByRole("button", { name: "重试下载" }));
    await waitFor(() => expect(api.updateDownload).toHaveBeenCalled());
  });

  it("不知道总大小时状态栏的进度线整条闪烁（不确定进度），不是停在 0", () => {
    setStatus({ phase: "downloading", version: "0.2.0", downloaded: 1024, total: null });
    render(<UpdateBar getFrontendBusy={idle} />);
    const line = screen.getByTestId("update-bar-progress");
    expect(line.style.width).toBe("100%");
    expect(line.className).toContain("animate-pulse");
  });

  it("下载好之后点提示重启；空闲时直接安装，不问", async () => {
    setStatus({ phase: "ready", version: "0.2.0" });
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    await waitFor(() => expect(api.updateInstall).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("现在重启安装更新？")).toBeNull();
  });

  it("有传输 / AI 命令 / 没上传的编辑时先问，取消就不装，确认才装", async () => {
    api.updateBusy.mockResolvedValue({ transfers: 0, ai_commands: 1, unsaved_edits: 0 });
    setStatus({ phase: "ready", version: "0.2.0" });
    render(<UpdateBar getFrontendBusy={() => ({ transfers: 2, unsavedEdits: 1 })} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    const dialog = await screen.findByText("现在重启安装更新？");
    expect(dialog).toBeTruthy();
    const text = document.body.textContent ?? "";
    expect(text).toContain("2 个文件传输");
    expect(text).toContain("1 条 AI 命令");
    expect(text).toContain("1 个文件在编辑器里改了还没保存");
    expect(api.updateInstall).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(screen.queryByText("现在重启安装更新？")).toBeNull());
    expect(api.updateInstall).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("update-bar"));
    fireEvent.click(await screen.findByRole("button", { name: "仍然重启" }));
    await waitFor(() => expect(api.updateInstall).toHaveBeenCalledTimes(1));
  });

  it("检查失败不在状态栏打扰", () => {
    setStatus({ phase: "failed", stage: "check", message: "无法连接到更新服务器" });
    render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.queryByTestId("update-bar")).toBeNull();
  });
});

describe("免安装版", () => {
  it("有新版本时提示手动下载，对话框里是「打开下载页」而不是「立即更新」", async () => {
    api.updateOpenDownloadPage.mockResolvedValue(undefined);
    setStatus(AVAILABLE);
    useUpdater.setState({ portable: true });
    render(<UpdateBar getFrontendBusy={idle} />);

    const bar = screen.getByTestId("update-bar");
    expect(bar.textContent).toContain("免安装版请手动下载");
    fireEvent.click(bar);

    expect(await screen.findByTestId("update-portable-note")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "立即更新" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "打开下载页" }));

    await waitFor(() => expect(api.updateOpenDownloadPage).toHaveBeenCalledTimes(1));
    expect(api.updateDownload).not.toHaveBeenCalled();
  });

  it("安装版照常是「立即更新」", async () => {
    setStatus(AVAILABLE);
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(screen.getByTestId("update-bar"));
    expect(await screen.findByRole("button", { name: "立即更新" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "打开下载页" })).toBeNull();
    expect(screen.queryByTestId("update-portable-note")).toBeNull();
  });
});

describe("自动流程（后台自动下载，退出时安装）：底部不提示", () => {
  const autoStates: UpdateStatus[] = [
    AVAILABLE,
    { phase: "downloading", version: "0.2.0", downloaded: 1024, total: 4096 },
    { phase: "ready", version: "0.2.0" },
    { phase: "failed", stage: "download", message: "下载更新包失败，网络可能中断了" },
  ];

  it.each(autoStates)("自动流程的 $phase 状态不显示提示条", (status) => {
    setStatus(status);
    useUpdater.setState({ auto: true });
    render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.queryByTestId("update-bar")).toBeNull();
  });

  it("同样的状态来自手动流程时照旧显示", () => {
    setStatus({ phase: "ready", version: "0.2.0" });
    useUpdater.setState({ auto: false });
    render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.getByTestId("update-bar").textContent).toContain("已下载好");
  });

  it("免安装版没法自动装：自动检查到的新版本仍然在底部提示去下载页", () => {
    setStatus(AVAILABLE);
    useUpdater.setState({ auto: true, portable: true });
    render(<UpdateBar getFrontendBusy={idle} />);
    expect(screen.getByTestId("update-bar").textContent).toContain("免安装版请手动下载");
  });
});

describe("已经自动下好（Ready）时手动打开的对话框", () => {
  it("说明退出时会自动安装，并保留「立即重启安装」（走重启前检查）", async () => {
    api.updateBusy.mockResolvedValue({ transfers: 0, ai_commands: 1, unsaved_edits: 0 });
    setStatus({ phase: "ready", version: "0.2.0" });
    useUpdater.setState({ auto: true, dialogOpen: true });
    render(<UpdateBar getFrontendBusy={idle} />);

    expect((await screen.findByTestId("update-ready-note")).textContent).toContain(
      "退出 Z-SSH 时会自动安装",
    );
    fireEvent.click(screen.getByRole("button", { name: "立即重启安装" }));
    // 有 AI 命令在跑：先问，确认了才装
    fireEvent.click(await screen.findByRole("button", { name: "仍然重启" }));
    await waitFor(() => expect(api.updateInstall).toHaveBeenCalledTimes(1));
  });

  it("已下载好的对话框里也有这一版的更新说明", async () => {
    setStatus({
      phase: "ready",
      version: "0.2.0",
      notes: "### 新增\n\n- **复制主机名**：右键菜单新增",
    });
    useUpdater.setState({ auto: true, dialogOpen: true, prefs: PREFS });
    render(<UpdateBar getFrontendBusy={idle} />);
    expect((await screen.findByTestId("update-notes")).textContent).toContain("复制主机名");
  });

  it("「稍后，退出时安装」只关掉对话框，不安装", async () => {
    setStatus({ phase: "ready", version: "0.2.0" });
    useUpdater.setState({ auto: true, dialogOpen: true, prefs: PREFS });
    render(<UpdateBar getFrontendBusy={idle} />);
    fireEvent.click(await screen.findByRole("button", { name: "稍后，退出时安装" }));
    expect(useUpdater.getState().dialogOpen).toBe(false);
    expect(api.updateInstall).not.toHaveBeenCalled();
  });

  it("「启动时自动检查」关着时不能说退出会自动安装", async () => {
    setStatus({ phase: "ready", version: "0.2.0" });
    useUpdater.setState({
      auto: false,
      dialogOpen: true,
      prefs: { ...PREFS, auto_check: false },
    });
    render(<UpdateBar getFrontendBusy={idle} />);
    const note = (await screen.findByTestId("update-ready-note")).textContent ?? "";
    expect(note).toContain("不会自动安装");
    expect(note).not.toContain("退出 Z-SSH 时会自动安装");
  });
});
