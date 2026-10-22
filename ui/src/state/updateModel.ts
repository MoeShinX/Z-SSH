//! 自动更新的纯函数：状态栏文案、进度、重启前检查。不碰界面和后端，方便单测。

import type { RestartBusy, UpdateStatus } from "../api/updater";

export type UpdateTone = "info" | "busy" | "ready" | "error";

export interface UpdateBarModel {
  visible: boolean;
  tone: UpdateTone;
  text: string;
}

/** `v0.2.0` 和 `0.2.0` 当作同一个版本。 */
export function sameVersion(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  return a.trim().replace(/^v/, "") === b.trim().replace(/^v/, "");
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 下载进度的百分比；不知道总大小时是 `null`。 */
export function progressPercent(downloaded: number, total: number | null): number | null {
  if (!total || total <= 0) return null;
  return Math.min(100, Math.floor((downloaded / total) * 100));
}

/** 进度的明细："42%（4.4 MB / 10.5 MB）"；不知道总大小时只有"已下载 2.0 MB"。 */
export function downloadDetail(downloaded: number, total: number | null): string {
  const percent = progressPercent(downloaded, total);
  if (percent === null) return `已下载 ${formatBytes(downloaded)}`;
  return `${percent}%（${formatBytes(downloaded)} / ${formatBytes(total ?? 0)}）`;
}

export interface DownloadProgress {
  /** 0–100；`null` = 不知道总大小，进度条用不确定样式。 */
  percent: number | null;
  text: string;
}

/** 关于页进度条的数据；不在下载时是 `null`。 */
export function downloadProgress(status: UpdateStatus | null): DownloadProgress | null {
  if (status?.phase !== "downloading") return null;
  return {
    percent: progressPercent(status.downloaded, status.total),
    text: `正在下载 v${status.version.replace(/^v/, "")}：${downloadDetail(status.downloaded, status.total)}`,
  };
}

/**
 * 状态栏里更新提示的内容。
 *
 * 自动流程（`auto`：自动检查 / 后台自动下载 / 自动下载失败）一律不提示：后台静默下载，
 * 用户退出 Z-SSH 时安装。例外是免安装版——它没法自动装，仍然提示去下载页。
 * 手动检查引出的状态照旧：对话框里点了「稍后」就只留一条在这里（`dismissedVersion` 这一次运行里
 * 不再显示）；下载 / 安装失败要显示（用户是主动点了更新的）；检查失败只在“关于”页显示。
 */
export function updateBarModel(
  status: UpdateStatus | null,
  dismissedVersion: string | null,
  portable = false,
  auto = false,
): UpdateBarModel {
  const hidden: UpdateBarModel = { visible: false, tone: "info", text: "" };
  if (!status) return hidden;
  if (auto && !portable) return hidden;
  switch (status.phase) {
    case "available":
      if (sameVersion(status.version, dismissedVersion)) return hidden;
      return {
        visible: true,
        tone: "info",
        text: portable
          ? `有新版本 v${status.version.replace(/^v/, "")}（免安装版请手动下载），点击查看`
          : `有新版本 v${status.version.replace(/^v/, "")}，点击查看`,
      };
    case "downloading": {
      const detail = downloadDetail(status.downloaded, status.total);
      return { visible: true, tone: "busy", text: `正在下载更新 v${status.version.replace(/^v/, "")}：${detail}` };
    }
    case "ready":
      return {
        visible: true,
        tone: "ready",
        text: `更新 v${status.version.replace(/^v/, "")} 已下载好，点击重启安装`,
      };
    case "failed":
      if (status.stage === "check") return hidden;
      return { visible: true, tone: "error", text: `更新失败：${status.message}` };
    default:
      return hidden;
  }
}

/** 重启前要问用户的事：把前端数到的传输和未上传的编辑，加上后端数到的 AI 命令。 */
export function mergeBusy(
  frontend: { transfers: number; unsavedEdits: number },
  backend: RestartBusy,
): RestartBusy {
  return {
    transfers: frontend.transfers,
    unsaved_edits: frontend.unsavedEdits,
    ai_commands: backend.ai_commands,
  };
}

export function isIdle(busy: RestartBusy): boolean {
  return busy.transfers === 0 && busy.ai_commands === 0 && busy.unsaved_edits === 0;
}

/** 重启确认框的正文；空闲时是 `null`（不用问）。 */
export function restartWarning(busy: RestartBusy): string | null {
  if (isIdle(busy)) return null;
  const lines: string[] = [];
  if (busy.transfers > 0) lines.push(`${busy.transfers} 个文件传输还没完成（重启会取消）`);
  if (busy.ai_commands > 0) lines.push(`${busy.ai_commands} 条 AI 命令正在终端里执行（重启会中断）`);
  if (busy.unsaved_edits > 0)
    lines.push(`${busy.unsaved_edits} 个文件在编辑器里改了还没保存（重启后会丢失）`);
  return `现在重启会打断：\n· ${lines.join("\n· ")}`;
}

/** 关于页里“检查更新”旁边的一句话。 */
export function aboutStatusText(status: UpdateStatus | null, autoInstall = true): string {
  if (!status) return "";
  switch (status.phase) {
    case "checking":
      return "正在检查…";
    case "up_to_date":
      return "已经是最新版本";
    case "available":
      return `发现新版本 v${status.version.replace(/^v/, "")}`;
    case "downloading":
      return downloadProgress(status)?.text ?? "";
    case "ready":
      return autoInstall
        ? `更新 v${status.version.replace(/^v/, "")} 已下载好，退出 Z-SSH 时自动安装`
        : `更新 v${status.version.replace(/^v/, "")} 已下载好（自动更新已关闭，点「检查更新」可立即重启安装）`;
    case "failed":
      return status.message;
    default:
      return "";
  }
}

/**
 * "检查更新"的 IPC 本身抛异常后的状态（审计 updater 低项）。
 *
 * 为什么需要：发起检查时先把状态设成 checking 再等 IPC；IPC 被拒绝（命令不存在、
 * 后端 panic、参数错）时后端不会再推状态事件，界面就一直停在"正在检查"。
 * 只在状态还是我们设的 checking 时改成失败：等待期间后端已经推来新状态
 * （比如它自己报的失败或找到新版本）就以后端为准。
 */
export function statusAfterCheckError(
  current: UpdateStatus | null,
  error: unknown,
): UpdateStatus | null {
  if (current?.phase !== "checking") return current;
  const detail = error instanceof Error ? error.message : String(error);
  return { phase: "failed", stage: "check", message: detail || "检查更新失败" };
}
