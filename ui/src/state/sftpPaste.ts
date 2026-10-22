//! 复制 / 粘贴的纯逻辑（M3-5 第 3 节）：剪贴板内容、结果提示。

import type { PasteItem, PasteOutcome, PasteRequest, SftpEntry } from "../api/sftp";

export type ClipSide = "local" | "remote";

/** 剪贴板里记的东西：来源（哪一边、哪个会话）和条目。 */
export interface ClipboardContent {
  side: ClipSide;
  /** 来源的 SFTP 会话（粘贴时后端据此找到源所在的连接）。 */
  sftpId: string;
  items: PasteItem[];
}

/** 选中的条目 → 剪贴板内容。非 UTF-8 名字的条目（不可操作）不放进去。 */
export function clipboardFrom(
  side: ClipSide,
  sftpId: string,
  entries: SftpEntry[],
): ClipboardContent | null {
  const items = entries
    .filter((entry) => entry.name_is_utf8 && entry.kind !== "drive")
    .map((entry) => ({
      path: entry.path,
      is_dir:
        entry.kind === "dir" || (entry.kind === "symlink" && entry.target_kind === "dir"),
    }));
  return items.length === 0 ? null : { side, sftpId, items };
}

/** 服务器上复制中……（第 N 个文件 · 已 N 秒）。 */
export function copyingText(elapsedSeconds: number, doneFiles?: number | null): string {
  const progress =
    doneFiles === undefined || doneFiles === null
      ? ""
      : `第 ${doneFiles + 1} 个文件 · `;
  return `服务器上复制中…（${progress}已 ${elapsedSeconds} 秒）`;
}

export interface PasteNotice {
  type: "success" | "info" | "error";
  text: string;
}

/** 粘贴结束后给用户看的一句话；失败的条目逐个写出原因。 */
export function pasteNotice(outcome: PasteOutcome): PasteNotice {
  const count = (state: string) => outcome.items.filter((item) => item.state === state).length;
  const ok = count("ok");
  const queued = count("queued");
  const skipped = count("skipped");
  const cancelledCount = count("cancelled");
  const failed = outcome.items.filter((item) => item.state === "failed");
  const seconds = Math.max(1, Math.round(outcome.elapsed_ms / 1000));

  const parts: string[] = [];
  if (ok > 0) {
    parts.push(
      `已复制 ${ok} 项（${outcome.method_label}，用时 ${outcome.elapsed_ms < 1000 ? "不到 1" : seconds} 秒）`,
    );
  }
  if (queued > 0) parts.push(`${queued} 项已加入传输队列（${outcome.method_label}）`);
  if (skipped > 0) parts.push(`跳过 ${skipped} 项`);
  if (cancelledCount > 0) parts.push(`已取消 ${cancelledCount} 项`);
  if (failed.length > 0) {
    parts.push(
      `${failed.length} 项失败：${failed
        .slice(0, 3)
        .map((item) => `「${item.name}」${item.message ?? ""}`)
        .join("；")}${failed.length > 3 ? "……" : ""}`,
    );
  }
  if (parts.length === 0) parts.push("没有可粘贴的内容");
  return {
    type: failed.length > 0 ? "error" : ok + queued > 0 ? "success" : "info",
    text: parts.join("；"),
  };
}

/**
 * `cp` 失败之后点了"改为经本机中转重试"：只把可以中转的失败条目挑出来，同一个目标目录、
 * 同一个来源，换成中转再粘一次。
 */
export function relayRetryRequest(
  request: PasteRequest,
  outcome: PasteOutcome,
  pasteId: string,
): PasteRequest | null {
  const items = outcome.items
    .filter((item) => item.state === "failed" && item.can_relay && item.path)
    .map((item) => ({ path: item.path as string, is_dir: item.is_dir === true }));
  if (items.length === 0) return null;
  return { ...request, paste_id: pasteId, force_relay: true, items };
}
