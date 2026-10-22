import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  updateCheck: vi.fn(),
}));

vi.mock("../api/updater", () => ({
  onUpdateStatus: vi.fn(async () => () => {}),
  updateCheck: mocks.updateCheck,
  updateDownload: vi.fn(),
  updateSetAuto: vi.fn(),
  updateSkip: vi.fn(),
  updateStatus: vi.fn(),
}));

import { useUpdater } from "./updater";

describe("检查更新：IPC 抛异常不能卡在 checking（updater 低项）", () => {
  beforeEach(() => {
    useUpdater.setState({ status: { phase: "idle" } });
    mocks.updateCheck.mockReset();
  });

  it("IPC 被拒绝：状态恢复为检查失败，错误继续抛给调用方提示", async () => {
    mocks.updateCheck.mockRejectedValue(new Error("command update_check not found"));
    await expect(useUpdater.getState().check()).rejects.toThrow("not found");
    expect(useUpdater.getState().status).toEqual({
      phase: "failed",
      stage: "check",
      message: "command update_check not found",
    });
  });

  it("正常返回：用后端给的状态", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "up_to_date", checked_at: 1 },
      prefs: { auto_check: true, last_check_at: 1, skipped_version: null },
      current_version: "0.1.0",
    });
    await useUpdater.getState().check();
    expect(useUpdater.getState().status).toEqual({ phase: "up_to_date", checked_at: 1 });
  });
});

describe("手动点「检查更新」：有新版本直接弹出更新对话框", () => {
  const prefs = { auto_check: true, last_check_at: 1, skipped_version: null };
  beforeEach(() => {
    useUpdater.setState({ status: { phase: "idle" }, dialogOpen: false, dismissedVersion: "0.1.9" });
    mocks.updateCheck.mockReset();
  });

  it("查到新版本：对话框打开（之前点过「稍后」的也重新弹）", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "available", version: "0.1.9", notes: null, date: null },
      prefs,
      current_version: "0.1.8",
    });
    await useUpdater.getState().checkManually();
    expect(useUpdater.getState().dialogOpen).toBe(true);
    expect(useUpdater.getState().dismissedVersion).toBeNull();
  });

  it("已经是最新：不弹", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "up_to_date", checked_at: 1 },
      prefs,
      current_version: "0.1.9",
    });
    await useUpdater.getState().checkManually();
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });

  it("自动检查（check）查到新版本也不弹，只在底部提示", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "available", version: "0.1.9", notes: null, date: null },
      prefs,
      current_version: "0.1.8",
    });
    await useUpdater.getState().check();
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });
});

describe("自动 / 手动来源标记（底部提示条据此隐藏自动流程）", () => {
  const prefs = { auto_check: true, last_check_at: 1, skipped_version: null };
  beforeEach(() => {
    useUpdater.setState({ status: { phase: "idle" }, auto: false, dialogOpen: false, promptedReadyVersion: null, portable: false });
    mocks.updateCheck.mockReset();
  });

  it("后端事件带的 auto 标记存进 store；没带就保持原值", () => {
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.9" }, true);
    expect(useUpdater.getState().auto).toBe(true);
    useUpdater.getState().applyStatus({ phase: "idle" });
    expect(useUpdater.getState().auto).toBe(true);
    useUpdater.getState().applyStatus({ phase: "idle" }, false);
    expect(useUpdater.getState().auto).toBe(false);
  });

  it("手动检查的返回里用后端给的 auto", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "available", version: "0.1.9", notes: null, date: null },
      prefs,
      current_version: "0.1.8",
      auto: false,
    });
    useUpdater.setState({ auto: true });
    await useUpdater.getState().checkManually();
    expect(useUpdater.getState().auto).toBe(false);
  });

  it("后台已经自动下好：手动点「检查更新」弹对话框（里面说明退出时自动安装）", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "ready", version: "0.1.9" },
      prefs,
      current_version: "0.1.8",
      auto: true,
    });
    await useUpdater.getState().checkManually();
    expect(useUpdater.getState().dialogOpen).toBe(true);
    expect(useUpdater.getState().auto).toBe(true);
  });

  it("自动检查（check）遇到 ready 不弹", async () => {
    mocks.updateCheck.mockResolvedValue({
      status: { phase: "ready", version: "0.1.9" },
      prefs,
      current_version: "0.1.8",
      auto: true,
    });
    await useUpdater.getState().check();
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });
});

describe("后台自动下好时弹一次对话框", () => {
  beforeEach(() => {
    useUpdater.setState({
      status: { phase: "downloading", version: "0.1.9", downloaded: 0, total: null },
      auto: true,
      portable: false,
      dialogOpen: false,
      promptedReadyVersion: null,
    });
  });

  it("自动下载完成：弹出对话框，同一版本这一次运行里只弹一次", () => {
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.9" }, true);
    expect(useUpdater.getState().dialogOpen).toBe(true);
    useUpdater.getState().openDialog(false);
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.9" }, true);
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });

  it("更新的版本再下好：再弹一次", () => {
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.9" }, true);
    useUpdater.getState().openDialog(false);
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.10" }, true);
    expect(useUpdater.getState().dialogOpen).toBe(true);
  });

  it("手动点了下载的（auto=false）不自动弹，照旧在底部提示", () => {
    useUpdater.getState().applyStatus({ phase: "ready", version: "0.1.9" }, false);
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });

  it("下载中不弹", () => {
    useUpdater
      .getState()
      .applyStatus({ phase: "downloading", version: "0.1.9", downloaded: 1, total: 2 }, true);
    expect(useUpdater.getState().dialogOpen).toBe(false);
  });
});
