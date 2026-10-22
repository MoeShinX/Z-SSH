//! 同名文件提问的待办队列（SFTP-6）。
//!
//! 后端传输 / 粘贴遇到目标已存在就发一条 `sftp_conflict_prompt`，同时开始等回答（最长 2 分钟，
//! 超时按跳过）。以前提问由 `SftpTabView` 自己收、自己弹，有两个洞：
//! - **一次只存一条**：几个文件同时撞上同名（一批上传、粘贴、两个标签）时，后到的提问把先到的
//!   盖掉，被盖掉的那条没人回答，2 分钟后被当成「跳过」，队列里那一项就一直停着；
//! - **看不见就收不到**：停靠面板关了、换到另一台主机的终端、切到 SFTP 标签，面板里的
//!   `SftpTabView` 被卸载，监听跟着没了，传输却还在后端等。
//! 现在由窗口里唯一的 `SftpConflictHost` 收所有提问放进这个队列，按先来后到逐个弹，
//! 不管停靠面板在不在屏幕上。

import { create } from "zustand";

import type { SftpConflictPrompt } from "../api/sftp";

/** 后端等回答的时限（`CONFLICT_TIMEOUT`）：过了这么久后端已经按「跳过」处理，再弹没有意义。 */
export const CONFLICT_PROMPT_TTL_MS = 120_000;

export interface PendingConflict {
  sftpId: string;
  prompt: SftpConflictPrompt;
  /** 收到的时刻（毫秒时间戳），用来算还剩多久。 */
  receivedAt: number;
}

interface ConflictQueueState {
  queue: PendingConflict[];
  push: (sftpId: string, prompt: SftpConflictPrompt, now?: number) => void;
  remove: (conflictId: number) => void;
}

export const useSftpConflicts = create<ConflictQueueState>((set) => ({
  queue: [],
  push: (sftpId, prompt, now = Date.now()) =>
    set((state) =>
      state.queue.some((item) => item.prompt.conflict_id === prompt.conflict_id)
        ? state
        : { queue: [...state.queue, { sftpId, prompt, receivedAt: now }] },
    ),
  remove: (conflictId) =>
    set((state) =>
      state.queue.some((item) => item.prompt.conflict_id === conflictId)
        ? { queue: state.queue.filter((item) => item.prompt.conflict_id !== conflictId) }
        : state,
    ),
}));

/**
 * 回答时后端说这条提问已经不在了（别的窗口先答了、超时了、标签关了）。
 * 这是正常的竞态，不是故障：界面静默撤掉那条即可。
 */
export function isStaleConflictError(error: unknown): boolean {
  const text = String(error);
  return text.includes("没有这个冲突提问") || text.includes("这次冲突提问已经结束");
}

/** 现在该弹哪一条：队列里先到的、属于这个窗口还开着的会话的那条。 */
export function nextConflict(
  queue: readonly PendingConflict[],
  owns: (sftpId: string) => boolean,
): PendingConflict | null {
  return queue.find((item) => owns(item.sftpId)) ?? null;
}

/** 这条提问还要等多久才过期（毫秒，最小 0）。 */
export function conflictTimeLeft(item: PendingConflict, now: number): number {
  return Math.max(0, item.receivedAt + CONFLICT_PROMPT_TTL_MS - now);
}

/** 同一批次（同一次提交 / 粘贴）的标识：会话 + 批次号；提问没带批次号就不属于任何批次。 */
export function batchKey(sftpId: string, prompt: Pick<SftpConflictPrompt, "batch">): string | null {
  return prompt.batch === undefined ? null : `${sftpId}#${prompt.batch}`;
}

/** 队列里和 `head` 同批次、还没回答的其余提问（不含 `head` 自己，按先后顺序）。 */
export function batchPeers(
  queue: readonly PendingConflict[],
  head: PendingConflict,
): PendingConflict[] {
  const key = batchKey(head.sftpId, head.prompt);
  if (key === null) return [];
  return queue.filter(
    (item) =>
      item.prompt.conflict_id !== head.prompt.conflict_id &&
      batchKey(item.sftpId, item.prompt) === key,
  );
}
