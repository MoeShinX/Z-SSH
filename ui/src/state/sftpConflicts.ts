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
