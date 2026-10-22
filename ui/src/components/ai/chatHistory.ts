//! AI 面板的消息类型，以及和历史会话（R11）之间的转换（纯函数，方便单测）。
//!
//! 截断、脱敏、上限都在后端做（`ai::history`），这里只负责形状转换：面板的消息
//! ⇄ 存盘的消息。

import type { HistoryMessage } from "../../api/agent";

/** 一条工具调用卡片的数据。 */
export interface ToolCall {
  callId: string;
  tool: string;
  input: unknown;
  output: string;
  status: "running" | "done" | "error" | "rejected" | "stopped";
  /** 执行位置（终端 / 后台），来自工具结果。 */
  location?: "terminal" | "background";
  /** 降级到后台的原因。 */
  fallbackReason?: string;
  /**
   * 执行层回报的实际执行位置（AI-SCOPE-2：以后端的解析结果为准，前端不猜）。只在界面里用，
   * 不存历史；回看历史的卡片没有。
   */
  exec?: ExecTarget;
  /** 实时的、带目标的调用（回报没到、或者一直没有回报时界面要说明"执行目标未确定"）。 */
  tracked?: boolean;
}

/** 一次工具调用实际执行在哪个目标上（执行层解析并回报的）。 */
export interface ExecTarget {
  /** 逻辑目标 ID：`host-<id>`，快速连接是会话 ID。 */
  targetId: string;
  /** 执行用的会话；这台主机当时没有可用会话（自动连接）时为 null。 */
  sessionId: string | null;
  /** 主机显示名。 */
  label: string;
  /** 卡片上要标明主机名：对话里有不止一个目标，或者当时没有"当前"目标。 */
  multi: boolean;
}

/** 消息区的一条：用户消息、助手正文、工具调用。 */
export type ChatItem =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; call: ToolCall };

const TOOL_STATUSES: ToolCall["status"][] = ["running", "done", "error", "rejected", "stopped"];

/** 面板消息 → 存盘消息。 */
export function toHistoryMessages(items: ChatItem[]): HistoryMessage[] {
  return items.map((item) => {
    if (item.kind === "tool") {
      return {
        kind: "tool",
        tool: item.call.tool,
        input: item.call.input,
        output: item.call.output,
        status: item.call.status,
      };
    }
    return { kind: item.kind, text: item.text };
  });
}

/**
 * 存盘消息 → 面板消息（回看）。
 *
 * 存盘时还在跑的卡片（保存发生在回合中间、或者程序被关掉）显示成"已停止"，不能一直转圈；
 * 执行位置照样从结果文本里解析（`parseToolMeta` 由面板传入，和实时卡片同一套解析）。
 */
export function fromHistoryMessages(
  messages: HistoryMessage[],
  parseToolMeta: (output: string) => Pick<ToolCall, "location" | "fallbackReason">,
): ChatItem[] {
  return messages.map((message, index): ChatItem => {
    if (message.kind === "tool") {
      const known = TOOL_STATUSES.find((status) => status === message.status);
      const status: ToolCall["status"] = !known || known === "running" ? "stopped" : known;
      return {
        kind: "tool",
        call: {
          callId: `history-${index}`,
          tool: message.tool,
          input: message.input,
          output: message.output,
          status,
          ...parseToolMeta(message.output),
        },
      };
    }
    return { kind: message.kind, text: message.text };
  });
}

/** 有没有用户消息（没有就不用存）。 */
export function hasUserMessage(items: ChatItem[]): boolean {
  return items.some((item) => item.kind === "user");
}

/**
 * 存进历史的目标主机名：只取主机库里的主机（显示名）。快速连接的标签名就是
 * `用户@地址:端口`，不能存，宁可不写。
 */
export function historyHostLabel(
  sessions: { hostId: number | null; hostLabel: string }[],
): string | null {
  const host = sessions.find((s) => s.hostId !== null);
  return host ? host.hostLabel : null;
}

/** 新的历史会话 id（后端只认字母、数字、`-`）。 */
export function newHistoryId(): string {
  return crypto.randomUUID();
}

/** 历史列表里的时间：今天只写时分，今年写月日时分，更早带年份。 */
export function formatHistoryTime(ms: number, now: Date = new Date()): string {
  const date = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  if (sameDay) return hm;
  const md = `${date.getMonth() + 1}月${date.getDate()}日`;
  if (date.getFullYear() === now.getFullYear()) return `${md} ${hm}`;
  return `${date.getFullYear()}年${md} ${hm}`;
}

/** 续接没接上时消息流顶部的提示。 */
export const CONTEXT_LOST_NOTICE = "之前的上下文没有带给 AI，只供回看";
