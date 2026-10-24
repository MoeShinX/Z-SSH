// AI 回合进行中、但这会儿既没有文字在出、也没有命令在跑的时候，消息底部显示
// 「· · · 12秒 · 思考中」，免得发出问题后下面空着像卡住了。

import { useEffect, useState } from "react";
import type { ChatItem } from "./chatHistory";

/**
 * 什么时候显示：回合在跑，且最后一条是用户的问题，或者最后一张命令卡片已经结束
 * （AI 在想下一步）。文字正在出、命令正在跑、等用户批准时不显示——那些本身就有动静。
 * 并行的几条命令里最后一张先跑完、前面的还在跑：也算命令在跑，不显示（只看这一轮的卡片）。
 */
export function showThinking(running: boolean, items: ChatItem[], approvalsPending: boolean): boolean {
  if (!running || approvalsPending) return false;
  const last = items[items.length - 1];
  if (!last) return true;
  if (last.kind === "user") return true;
  if (last.kind !== "tool") return false;
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.kind === "user") break;
    if (item.kind === "tool" && item.call.status === "running") return false;
  }
  return true;
}

/** 已经等了多久（从这一轮发出问题算起）。 */
export function elapsedText(ms: number): string {
  const secs = Math.max(0, Math.floor(ms / 1000));
  if (secs < 60) return `${secs}秒`;
  return `${Math.floor(secs / 60)}分${secs % 60}秒`;
}

/** 智能体正在自动重试时的提示内容（后端的 `retrying` 事件）。 */
export interface RetryNotice {
  attempt: number;
  reason: string;
  /** 距离下次重试大约几秒；不知道是 null。 */
  nextInSecs: number | null;
}

/** 后端同一回合最多放行的重试次数（超过就停掉这一轮，见后端 ai/agent/retry.rs 的 MAX_RETRIES，三个智能体共用）。 */
export const MAX_RETRIES = 3;

/** 「约 X 秒后重试」；不知道或已经到点就不说。 */
function nextText(secs: number | null): string {
  if (secs === null || secs <= 0) return "";
  if (secs < 60) return `，约 ${secs} 秒后重试`;
  if (secs < 3600) return `，约 ${Math.ceil(secs / 60)} 分钟后重试`;
  return `，约 ${Math.ceil(secs / 3600)} 小时后重试`;
}

/** 「遇到问题，正在重试（第 N 次，最多 3 次）：原因，约 X 秒后重试」。 */
export function retryText(notice: RetryNotice): string {
  const attempt = Math.min(Math.max(1, Math.floor(notice.attempt)), MAX_RETRIES);
  const reason = notice.reason.trim();
  return `遇到问题，正在重试（第 ${attempt} 次，最多 ${MAX_RETRIES} 次）${
    reason ? `：${reason}` : ""
  }${nextText(notice.nextInSecs)}`;
}

export default function ThinkingIndicator({
  since,
  retry = null,
}: {
  since: number;
  retry?: RetryNotice | null;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return (
    <div data-section="ai-thinking" role="status" aria-live="polite" className="flex flex-col gap-1">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="zz-thinking-dots flex items-center gap-0.5" aria-hidden>
          <span />
          <span />
          <span />
        </span>
        <span className="tabular-nums">{elapsedText(now - since)}</span>
        <span>·</span>
        <span>思考中</span>
      </div>
      {retry && (
        <p data-section="ai-retrying" className="text-[11px] text-amber-600">
          {retryText(retry)}
        </p>
      )}
    </div>
  );
}
