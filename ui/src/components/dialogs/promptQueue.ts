//! 后端提问（主机指纹 / 认证）的排队规则（C12）。
//!
//! 为什么要排队：多个连接可能同时向界面提问，原来一个 state 槽只存最新的一条，
//! 早先那条在后端还等着回答，用户却再也看不到它，只能等 120 秒超时、连接失败。
//! 现在按 req_id 排队，一次显示一条，答完（或后端宣布它结束）再显示下一条。

import type { AuthPrompt, HostKeyPrompt } from "../../api/auth";

export type PendingPrompt =
  | { kind: "hostkey"; prompt: HostKeyPrompt }
  | { kind: "auth"; prompt: AuthPrompt };

/** 加到队尾。同一个 req_id 已经在队里就不重复加（事件重发时不弹两次）。 */
export function enqueuePrompt(
  queue: readonly PendingPrompt[],
  item: PendingPrompt,
): PendingPrompt[] {
  if (queue.some((queued) => queued.prompt.req_id === item.prompt.req_id)) return [...queue];
  return [...queue, item];
}

/**
 * 按 req_id 摘掉一条：用户回答了，或者后端说它已经结束（超时、会话关闭、连接被取消）。
 * 指纹和认证共用后端的同一个 ID 序列，所以只按 req_id 匹配就不会摘错。
 * 没有这条时原样返回同一个数组，React 不会因此多渲染一次。
 */
export function removePrompt(
  queue: readonly PendingPrompt[],
  reqId: number,
): readonly PendingPrompt[] {
  if (!queue.some((queued) => queued.prompt.req_id === reqId)) return queue;
  return queue.filter((queued) => queued.prompt.req_id !== reqId);
}

/** 当前该显示的那条：先来先答。 */
export function currentPrompt(queue: readonly PendingPrompt[]): PendingPrompt | null {
  return queue[0] ?? null;
}
