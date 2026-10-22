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

export default function ThinkingIndicator({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return (
    <div
      data-section="ai-thinking"
      role="status"
      aria-live="polite"
      className="flex items-center gap-2 text-xs text-muted-foreground"
    >
      <span className="zz-thinking-dots flex items-center gap-0.5" aria-hidden>
        <span />
        <span />
        <span />
      </span>
      <span className="tabular-nums">{elapsedText(now - since)}</span>
      <span>·</span>
      <span>思考中</span>
    </div>
  );
}
