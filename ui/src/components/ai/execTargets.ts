//! 执行层回报的实际执行位置（AI-SCOPE-2 第 2 节）：把后端的 `ai_exec_target` 回报对到工具卡片上。
//!
//! 工具卡片、「查看终端」、标签上的活动点都只认这份回报，前端不再自己猜"这次会落在哪台
//! 主机 / 哪个终端"。回报和智能体的 `tool_start` 事件走的是两条通道，到达顺序不保证，
//! 所以两边都要能先到：回报先到就先存着，卡片出现时再认领。纯函数，方便单测。
import type { ApprovalRequest, ExecTargetReport } from "../../api/agent";
import { bareToolName, commandOf, pathOf, targetIdOf, toolHasTarget } from "./aiHelpers";
import type { ExecTarget } from "./chatHistory";

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

/** 回报 → 卡片上的执行位置。`multi`：要在卡片上标出主机名（多个目标，或者当时没有"当前"）。 */
export function execTargetOf(report: ExecTargetReport, multi: boolean): ExecTarget {
  return {
    targetId: report.target_id,
    sessionId: report.session_id,
    label: report.host_label,
    multi,
  };
}

/** 审批事件里带的执行位置（和回报同一份解析结果）；老版本后端没带就是 null。 */
export function execTargetOfApproval(
  approval: Pick<ApprovalRequest, "target_id" | "session_id" | "host_label">,
): ExecTarget | null {
  if (!approval.target_id) return null;
  return {
    targetId: approval.target_id,
    sessionId: approval.session_id ?? null,
    label: approval.host_label,
    multi: true,
  };
}
