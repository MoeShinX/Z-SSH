//! 执行层回报的实际执行位置（AI-SCOPE-2 第 2 节）：把后端的 `ai_exec_target` 回报对到工具卡片上。
//!
//! 工具卡片、「查看终端」、标签上的活动点都只认这份回报，前端不再自己猜"这次会落在哪台
//! 主机 / 哪个终端"。回报和智能体的 `tool_start` 事件走的是两条通道，到达顺序不保证，
//! 所以两边都要能先到：回报先到就先存着，卡片出现时再认领。纯函数，方便单测。
//!
//! 配对分两步（AI-SCOPE-3）：卡片开始时智能体的事件里没有任何能和回报一一对应的东西，先按
//! 「工具 + 命令 / 路径 + 输入里的 target_id + 先后」临时配，让活动点马上能亮；工具结果回来后，
//! 结果文本末尾带着后端给这次调用的执行编号（`[zssh exec <编号>]`），拿它按编号精确校正。
import type { ApprovalRequest, ExecTargetReport } from "../../api/agent";
import { bareToolName, commandOf, pathOf, targetIdOf, toolHasTarget } from "./aiHelpers";
import type { ChatItem, ExecTarget, ToolCall } from "./chatHistory";

/** 一次带目标的工具调用（对到回报用）。 */
export interface TrackedCall {
  callId: string;
  /** 不带前缀的工具名。 */
  tool: string;
  /** 对回报用的键：命令原文 / 路径原文 / 空串（`read_terminal`）；输入里取不到为 `null`。 */
  key: string | null;
  /**
   * 调用输入里写明的 `target_id`；省略（落在默认目标上）为 `null`。回报里的 `target_id` 必须和
   * 它一致才算同一次调用：两台主机同时跑同一条命令时，靠它把各自的回报分清。
   */
  targetId: string | null;
  /** 工具已经返回了（回报可能晚到）。 */
  ended: boolean;
  /** 已经认领过一份回报。 */
  claimed: boolean;
  /** 认领的那份回报的执行编号（没认领为 `null`）。 */
  execId: string | null;
  /** 工具结果末尾标记里的执行编号：有了它就以它为准，不再靠启发式（没拿到为 `null`）。 */
  resultExecId: string | null;
}

/** 工具结果末尾的执行编号标记（和后端 `protocol::exec_marker` 同一个格式），只认最后一行。 */
const EXEC_MARKER_TAIL = /(?:\r?\n)?\[zssh exec ([0-9A-Za-z-]{1,64})\]\s*$/;

/**
 * 把工具结果末尾的执行编号标记拆出来：返回去掉标记后的文本和编号。只认末尾那一行——后端
 * 总是把标记追加在最后，文件内容、命令输出里恰好出现同样的文字不会被当成标记。
 */
export function splitExecMarker(output: string): { text: string; execId: string | null } {
  const match = EXEC_MARKER_TAIL.exec(output);
  if (!match) return { text: output, execId: null };
  return { text: output.slice(0, match.index), execId: match[1] };
}

/** 这次调用对回报用的键（和后端 `ExecTargetReport.key` 同一套）。 */
export function callKey(tool: string, input: unknown): string | null {
  switch (bareToolName(tool)) {
    case "run_command":
      return commandOf(input);
    case "read_terminal":
      return "";
    case "list_dir":
    case "read_file":
    case "write_file":
    case "stat":
      return pathOf(input);
    default:
      return null;
  }
}

/** 登记一次工具调用；不带目标的工具（`list_targets` 之类）返回 null。 */
export function trackCall(callId: string, tool: string, input: unknown): TrackedCall | null {
  if (!toolHasTarget(tool)) return null;
  return {
    callId,
    tool: bareToolName(tool),
    key: callKey(tool, input),
    targetId: targetIdOf(input),
    ended: false,
    claimed: false,
    execId: null,
    resultExecId: null,
  };
}

/**
 * 回报属于哪一次调用：同一个工具、键相同（输入里取不到键的当作通配）、还没认领过的，并且
 * 输入里写明了 `target_id` 的必须和回报的目标一致（没写的是通配）。候选里写明目标的优先于
 * 没写的，还在跑的优先于已经返回的（晚到的回报不抢新的调用），再按出现先后。
 *
 * 智能体的工具事件里拿不到能和桥侧请求一一对应的 ID，所以只能这样配。剩下的歧义只出现在
 * 「命令相同、都省略 target_id」时：它们落在同一个默认目标上，互换回报不影响显示。
 */
export function findCallForReport(
  calls: readonly TrackedCall[],
  report: Pick<ExecTargetReport, "tool" | "key" | "target_id">,
): TrackedCall | undefined {
  const pool = calls.filter(
    (c) =>
      !c.claimed &&
      // 结果已经指明了自己的编号：它只认那份回报，不再抢别人的。
      c.resultExecId === null &&
      c.tool === report.tool &&
      (c.targetId === null || c.targetId === report.target_id),
  );
  const pick = (list: TrackedCall[]) => {
    const named = list.filter((c) => c.targetId !== null);
    const best = named.length > 0 ? named : list;
    return best.find((c) => !c.ended) ?? best[0];
  };
  const exact = pool.filter((c) => c.key === report.key);
  if (exact.length > 0) return pick(exact);
  return pick(pool.filter((c) => c.key === null));
}

/** 一次配对变化：这张卡片（`callId`）现在对应 `report`；`null` 表示取消原来的对应。 */
export interface ExecChange {
  callId: string;
  report: ExecTargetReport | null;
}

/**
 * 把 `report` 配给 `call`（按编号精确配）。回报原来被另一张卡片临时占着时两张对调：另一张
 * 拿到本卡片原来占的那份（临时配对错开的典型情形是两条同样的命令互换），本卡片原来没有
 * 回报或另一张卡片的结果已经指明了别的编号，就让另一张先空着，等它自己的回报到了再认。
 * 返回要更新的卡片。
 */
export function assignReport(
  calls: readonly TrackedCall[],
  reports: ReadonlyMap<string, ExecTargetReport>,
  call: TrackedCall,
  report: ExecTargetReport,
): ExecChange[] {
  if (call.execId === report.exec_id) return [];
  const previous = call.execId ? (reports.get(call.execId) ?? null) : null;
  const holder = calls.find((c) => c !== call && c.execId === report.exec_id);
  call.execId = report.exec_id;
  call.claimed = true;
  const changes: ExecChange[] = [{ callId: call.callId, report }];
  if (holder) {
    if (previous && holder.resultExecId === null) {
      holder.execId = previous.exec_id;
      changes.push({ callId: holder.callId, report: previous });
    } else {
      holder.execId = null;
      holder.claimed = false;
      changes.push({ callId: holder.callId, report: null });
    }
  }
  return changes;
}

/**
 * 工具结果回来了，带着执行编号：记在这次调用上，并按编号校正配对。回报还没到就只记下，
 * 回报到了（`reportArrived`）再配。
 */
export function applyResultExecId(
  calls: readonly TrackedCall[],
  reports: ReadonlyMap<string, ExecTargetReport>,
  callId: string,
  execId: string,
): ExecChange[] {
  const call = calls.find((c) => c.callId === callId);
  if (!call) return [];
  call.resultExecId = execId;
  const report = reports.get(execId);
  return report ? assignReport(calls, reports, call, report) : [];
}

/**
 * 回报到了：先看有没有哪张卡片的结果已经指明要它（按编号精确配），没有再按启发式临时配。
 * 谁也认领不了返回 `null`（调用方先存着，卡片出现时再认）。
 */
export function reportArrived(
  calls: readonly TrackedCall[],
  reports: ReadonlyMap<string, ExecTargetReport>,
  report: ExecTargetReport,
): { call: TrackedCall; changes: ExecChange[] } | null {
  const exact = calls.find((c) => c.resultExecId === report.exec_id);
  const call = exact ?? findCallForReport(calls, report);
  if (!call) return null;
  return { call, changes: assignReport(calls, reports, call, report) };
}

/** 回报 → 卡片上的执行位置。`multi`：要在卡片上标出主机名（多个目标，或者当时没有"当前"）。 */
export function execTargetOf(report: ExecTargetReport, multi: boolean): ExecTarget {
  return {
    execId: report.exec_id,
    targetId: report.target_id,
    sessionId: report.session_id,
    label: report.host_label,
    multi,
  };
}

/** 审批事件里带的执行位置（和回报同一份解析结果）；老版本后端没带就是 null。 */
export function execTargetOfApproval(
  approval: Pick<ApprovalRequest, "target_id" | "session_id" | "host_label"> & {
    exec_id?: string;
  },
): ExecTarget | null {
  if (!approval.target_id) return null;
  return {
    execId: approval.exec_id ?? "",
    targetId: approval.target_id,
    sessionId: approval.session_id ?? null,
    label: approval.host_label,
    multi: true,
  };
}

/** 能弹确认的两个工具（`run_command`、`write_file`）。 */
const APPROVAL_TOOLS = new Set(["run_command", "write_file"]);

/**
 * 哪条待确认请求属于这张还在执行的工具卡片——按执行编号配，绝不按命令 / 路径文本猜
 * （两台主机等同一条命令时，按文本会把 web 卡片上的「执行」配到 db 的请求上）。
 *
 * 卡片的编号来自它认领的那份回报（`call.exec.execId`）。结果还没回来时这份认领只是按
 * 「工具 + 命令 / 路径」临时配的，只有当前没有别的同工具、同命令（路径）的调用也在执行时，
 * 它才是唯一的；有重名的在跑就不配，宁可让确认请求单独显示（带它自己的主机名和命令，按它
 * 自己的编号回答），也不能把批准发给别的。
 */
export function pairApproval<A extends { execId?: string; tool: string }>(
  approvals: readonly A[],
  call: ToolCall,
  items: readonly ChatItem[],
): A | undefined {
  const tool = bareToolName(call.tool);
  const execId = call.exec?.execId;
  if (!APPROVAL_TOOLS.has(tool) || call.status !== "running" || !execId) return undefined;
  const key = callKey(call.tool, call.input);
  if (key === null) return undefined;
  const ambiguous = items.some(
    (item) =>
      item.kind === "tool" &&
      item.call.callId !== call.callId &&
      item.call.status === "running" &&
      bareToolName(item.call.tool) === tool &&
      callKey(item.call.tool, item.call.input) === key,
  );
  if (ambiguous) return undefined;
  return approvals.find((a) => a.tool === tool && a.execId === execId);
}
