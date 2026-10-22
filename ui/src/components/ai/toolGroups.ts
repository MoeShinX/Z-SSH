//! AI 面板里工具调用的分组（AI-UX-1）：执行完的工具调用在同一轮回复里连续出现的，
//! 合并成一行淡色小字，默认收起；等待审批的、正在执行的不折叠。
//! 纯函数，只改展示，不碰后端和审计。

import type { ChatItem, ToolCall } from "./chatHistory";
import { bareToolName, commandOf, exitCodeOf, friendlyToolName, pathOf } from "./aiHelpers";

/** 面板里一行的展示形式。 */
export type ChatRow =
  | { type: "text"; index: number; item: Extract<ChatItem, { kind: "user" | "assistant" }> }
  /** 完整卡片：等待审批的调用，必须让用户看到要批准的内容。 */
  | { type: "card"; call: ToolCall }
  /** 正在执行：只显示一行进行中。 */
  | { type: "progress"; call: ToolCall }
  /** 执行完的一组调用，折叠成一行。 */
  | { type: "group"; calls: ToolCall[] };

/** 这次调用算不算失败：报错、被拒绝、被停止，或者命令退出码不是 0。 */
export function callFailed(call: ToolCall): boolean {
  if (call.status === "error" || call.status === "rejected" || call.status === "stopped") return true;
  if (call.status !== "done") return false;
  const code = exitCodeOf(call.output);
  return code !== null && code !== 0;
}

/**
 * 把消息列表变成展示行。`hasApproval(call)` 说明这条调用是不是正在等用户批准。
 * 同一段连续的工具调用里，执行完的并成一组；遇到等待审批 / 正在执行的就断开
 * （它们前后各自成组，等它们执行完再并进来）。
 */
export function groupChatItems(
  items: ChatItem[],
  hasApproval: (call: ToolCall) => boolean,
): ChatRow[] {
  const rows: ChatRow[] = [];
  let open: ToolCall[] | null = null;
  const flush = () => {
    if (open) rows.push({ type: "group", calls: open });
    open = null;
  };
  items.forEach((item, index) => {
    if (item.kind !== "tool") {
      flush();
      rows.push({ type: "text", index, item });
      return;
    }
    const call = item.call;
    if (hasApproval(call)) {
      flush();
      rows.push({ type: "card", call });
    } else if (call.status === "running") {
      flush();
      rows.push({ type: "progress", call });
    } else {
      open = open ?? [];
      open.push(call);
    }
  });
  flush();
  return rows;
}

/** 一组调用的摘要，如"执行了 2 条命令、读了 1 个文件"。 */
export function summarizeCalls(calls: ToolCall[]): string {
  let commands = 0;
  let reads = 0;
  let writes = 0;
  let looks = 0;
  let others = 0;
  for (const call of calls) {
    switch (bareToolName(call.tool)) {
      case "run_command":
        commands += 1;
        break;
      case "read_file":
        reads += 1;
        break;
      case "write_file":
        writes += 1;
        break;
      case "list_dir":
      case "stat":
        looks += 1;
        break;
      default:
        others += 1;
    }
  }
  const parts: string[] = [];
  if (commands > 0) parts.push(`执行了 ${commands} 条命令`);
  if (reads > 0) parts.push(`读了 ${reads} 个文件`);
  if (writes > 0) parts.push(`写了 ${writes} 个文件`);
  if (looks > 0) parts.push(`查看了 ${looks} 处目录或文件信息`);
  if (others > 0) parts.push(`${others} 次其他查询`);
  return parts.join("、");
}

/** 正在执行的一行文字："正在执行：<命令前 60 个字符>…"。 */
export function progressText(call: ToolCall): string {
  const detail = commandOf(call.input) ?? pathOf(call.input);
  if (!detail) return `正在执行：${friendlyToolName(call.tool)}`;
  const flat = detail.replace(/\s+/g, " ").trim();
  const chars = Array.from(flat);
  return chars.length > 60 ? `正在执行：${chars.slice(0, 60).join("")}…` : `正在执行：${flat}`;
}
