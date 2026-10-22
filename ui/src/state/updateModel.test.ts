import { describe, expect, it } from "vitest";

import type { UpdateStatus } from "../api/updater";
import {
  aboutStatusText,
  downloadDetail,
  downloadProgress,
  formatBytes,
  isIdle,
  mergeBusy,
  progressPercent,
  restartWarning,
  sameVersion,
  statusAfterCheckError,
  updateBarModel,
} from "./updateModel";

describe("检查更新的 IPC 抛异常（updater 低项）", () => {
  it("还停在 checking：改成检查失败并带上错误信息", () => {
    expect(statusAfterCheckError({ phase: "checking" }, new Error("命令不存在"))).toEqual({
      phase: "failed",
      stage: "check",
      message: "命令不存在",
    });
    expect(statusAfterCheckError({ phase: "checking" }, "后端没响应")).toMatchObject({
      phase: "failed",
      message: "后端没响应",
    });
  });

  it("等待期间后端已经推来新状态：以后端为准，不覆盖", () => {
    expect(statusAfterCheckError(available, new Error("x"))).toBe(available);
    expect(statusAfterCheckError(null, new Error("x"))).toBeNull();
  });
});

const available: UpdateStatus = { phase: "available", version: "0.2.0", notes: "修了几个问题", date: null };

describe("updateBarModel：自动流程不提示", () => {
  const states: UpdateStatus[] = [
    available,
    { phase: "downloading", version: "0.2.0", downloaded: 1, total: 10 },
    { phase: "ready", version: "0.2.0" },
    { phase: "failed", stage: "download", message: "下载更新包失败，网络可能中断了" },
  ];

  it("自动流程的 available / downloading / ready / 下载失败都隐藏", () => {
    for (const status of states) {
      expect(updateBarModel(status, null, false, true).visible, status.phase).toBe(false);
      expect(updateBarModel(status, null, false, false).visible, status.phase).toBe(true);
    }
  });

  it("免安装版没法自动装，自动检查到的新版本仍然提示", () => {
    expect(updateBarModel(available, null, true, true).visible).toBe(true);
  });

  it("关于页：已下载好写明退出时自动安装；自动更新关了就不这么说", () => {
    expect(aboutStatusText({ phase: "ready", version: "0.2.0" })).toContain("退出 Z-SSH 时自动安装");
    expect(aboutStatusText({ phase: "ready", version: "0.2.0" }, false)).not.toContain("自动安装");
  });
});

describe("updateBarModel", () => {
  it("有新版本时只在状态栏提示，不弹窗", () => {
    const model = updateBarModel(available, null);
    expect(model.visible).toBe(true);
    expect(model.text).toContain("v0.2.0");
  });

  it("点了“稍后”的版本这一次运行里不再提示，更新的版本照常提示", () => {
    expect(updateBarModel(available, "0.2.0").visible).toBe(false);
    expect(updateBarModel(available, "v0.2.0").visible).toBe(false);
    const newer: UpdateStatus = { ...available, version: "0.2.1" } as UpdateStatus;
    expect(updateBarModel(newer, "0.2.0").visible).toBe(true);
  });

  it("下载时显示百分比和大小；不知道总大小时只显示已下载的", () => {
    const withTotal = updateBarModel(
      { phase: "downloading", version: "0.2.0", downloaded: 5 * 1024 * 1024, total: 10 * 1024 * 1024 },
      null,
    );
    expect(withTotal.text).toContain("50%");
    expect(withTotal.tone).toBe("busy");
    const noTotal = updateBarModel(
      { phase: "downloading", version: "0.2.0", downloaded: 2048, total: null },
      null,
    );
    expect(noTotal.text).toContain("2 KB");
    expect(noTotal.text).not.toContain("%");
  });

  it("下载完提示重启；下载 / 安装失败要显示，检查失败只在关于页", () => {
    expect(updateBarModel({ phase: "ready", version: "0.2.0" }, null).text).toContain("重启");
    const download = updateBarModel({ phase: "failed", stage: "download", message: "网络断了" }, null);
    expect(download).toMatchObject({ visible: true, tone: "error" });
    expect(download.text).toContain("网络断了");
    expect(updateBarModel({ phase: "failed", stage: "install", message: "x" }, null).visible).toBe(true);
    expect(updateBarModel({ phase: "failed", stage: "check", message: "x" }, null).visible).toBe(false);
  });

  it("没有更新 / 空闲 / 检查中 / 没状态都不显示", () => {
    expect(updateBarModel(null, null).visible).toBe(false);
    expect(updateBarModel({ phase: "idle" }, null).visible).toBe(false);
    expect(updateBarModel({ phase: "checking" }, null).visible).toBe(false);
    expect(updateBarModel({ phase: "up_to_date", checked_at: 1 }, null).visible).toBe(false);
  });
});

describe("重启前检查", () => {
  it("空闲时不用问", () => {
    expect(restartWarning({ transfers: 0, ai_commands: 0, unsaved_edits: 0 })).toBeNull();
  });

  it("传输、AI 命令、编辑器里没保存的修改各自会被点名", () => {
    for (const [busy, word] of [
      [{ transfers: 2, ai_commands: 0, unsaved_edits: 0 }, "文件传输"],
      [{ transfers: 0, ai_commands: 1, unsaved_edits: 0 }, "AI 命令"],
      [{ transfers: 0, ai_commands: 0, unsaved_edits: 3 }, "还没保存"],
    ] as const) {
      const text = restartWarning(busy);
      expect(text).toContain(word);
      expect(isIdle(busy)).toBe(false);
    }
    const all = restartWarning({ transfers: 1, ai_commands: 1, unsaved_edits: 1 });
    expect(all).toContain("文件传输");
    expect(all).toContain("AI 命令");
    expect(all).toContain("还没保存");
  });

  it("前端数的和后端数的合到一起", () => {
    expect(
      mergeBusy({ transfers: 2, unsavedEdits: 1 }, { transfers: 0, ai_commands: 3, unsaved_edits: 0 }),
    ).toEqual({ transfers: 2, ai_commands: 3, unsaved_edits: 1 });
  });
});

describe("小工具", () => {
  it("版本号比较忽略 v", () => {
    expect(sameVersion("v0.2.0", "0.2.0")).toBe(true);
    expect(sameVersion("0.2.0", "0.2.1")).toBe(false);
    expect(sameVersion(null, "0.2.0")).toBe(false);
  });

  it("大小和百分比", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("2 KB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MB");
    expect(progressPercent(50, 200)).toBe(25);
    expect(progressPercent(300, 200)).toBe(100);
    expect(progressPercent(10, null)).toBeNull();
    expect(progressPercent(10, 0)).toBeNull();
  });

  it("关于页的一句话", () => {
    expect(aboutStatusText({ phase: "up_to_date", checked_at: 1 })).toBe("已经是最新版本");
    expect(aboutStatusText({ phase: "failed", stage: "check", message: "无法连接到更新服务器" })).toBe(
      "无法连接到更新服务器",
    );
    expect(aboutStatusText(null)).toBe("");
  });
});

describe("免安装版的状态栏文案", () => {
  it("有新版本时写明要手动下载，安装版不变", () => {
    const available: UpdateStatus = { phase: "available", version: "v0.3.0", notes: null, date: null };
    expect(updateBarModel(available, null, true).text).toBe("有新版本 v0.3.0（免安装版请手动下载），点击查看");
    expect(updateBarModel(available, null).text).toBe("有新版本 v0.3.0，点击查看");
  });
});

describe("关于页的下载进度", () => {
  const MB = 1024 * 1024;
  const downloading = (downloaded: number, total: number | null): UpdateStatus => ({
    phase: "downloading",
    version: "v0.1.3",
    downloaded,
    total,
  });

  it("知道总大小：百分比 + 已下载 / 总大小", () => {
    const progress = downloadProgress(downloading(4.4 * MB, 10.5 * MB));
    expect(progress).toEqual({ percent: 41, text: "正在下载 v0.1.3：41%（4.4 MB / 10.5 MB）" });
    expect(aboutStatusText(downloading(4.4 * MB, 10.5 * MB))).toBe(progress?.text);
  });

  it("不知道总大小：只有已下载字节数，进度是 null（不确定样式）", () => {
    const progress = downloadProgress(downloading(2 * MB, null));
    expect(progress).toEqual({ percent: null, text: "正在下载 v0.1.3：已下载 2.0 MB" });
    expect(downloadProgress(downloading(10, 0))?.percent).toBeNull();
  });

  it("开头是 0%，下载满了是 100%，超过总大小也封顶 100%", () => {
    expect(downloadProgress(downloading(0, 10 * MB))?.percent).toBe(0);
    expect(downloadProgress(downloading(10 * MB, 10 * MB))?.text).toBe(
      "正在下载 v0.1.3：100%（10.0 MB / 10.0 MB）",
    );
    expect(downloadProgress(downloading(11 * MB, 10 * MB))?.percent).toBe(100);
  });

  it("不在下载时没有进度条", () => {
    expect(downloadProgress(null)).toBeNull();
    expect(downloadProgress({ phase: "ready", version: "0.1.3" })).toBeNull();
  });

  it("明细文案状态栏和关于页共用", () => {
    expect(downloadDetail(5 * MB, 10 * MB)).toBe("50%（5.0 MB / 10.0 MB）");
    expect(downloadDetail(2048, null)).toBe("已下载 2 KB");
  });
});
