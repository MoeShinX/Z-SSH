//! AI 面板的纯函数（方便单测，不拉 Tauri 运行时）。
import type { AgentInstall, AgentLoginStatus } from "../../api/agent";

/** 从工具输入里取命令全文（run_command 的 input 里有 command 字段）。 */
export function commandOf(input: unknown): string | null {
  if (input && typeof input === "object" && "command" in input) {
    const command = (input as { command?: unknown }).command;
    if (typeof command === "string") return command;
  }
  return null;
}

/**
 * 把命令里的不可见控制字符显示成可见的转义形式（R-8 第 1 节，审批卡片第二道防线）。
 *
 * 命令是当键盘输入打进终端的：控制字符会在终端里产生按键效果（Ctrl+U 删掉已输入的
 * 前缀），C1 / Unicode 方向控制字符还能让卡片显示的内容和实际执行的不一样。工具桥
 * 已经在审批前拒绝这类命令，这里兜底保证卡片上不出现看不见的字符。正常换行和制表
 * 保留原样（多行命令照旧显示），其余 C0、DEL 显示成 `\x15`，C1 / 方向控制字符显示成
 * `\u{009b}`。
 */
export function visibleCommandText(command: string): string {
  let out = "";
  for (const ch of command) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      ((code < 0x20 || code === 0x7f) && ch !== "\n" && ch !== "\t") ||
      (code >= 0x80 && code <= 0x9f) ||
      (code >= 0x202a && code <= 0x202e) ||
      (code >= 0x2066 && code <= 0x2069);
    if (!forbidden) out += ch;
    else if (code <= 0x7f) out += `\\x${code.toString(16).padStart(2, "0")}`;
    else out += `\\u{${code.toString(16).padStart(4, "0")}}`;
  }
  return out;
}

/** 工具输入里标的会话 id（`session_id`）。 */
export function sessionIdOf(input: unknown): string | null {
  if (input && typeof input === "object" && "session_id" in input) {
    const id = (input as { session_id?: unknown }).session_id;
    if (typeof id === "string") return id;
  }
  return null;
}

/** 工具输入里标了危险吗（危险等级来自 AI-1 的 assess 结果）。 */
export function isDangerous(input: unknown): boolean {
  return (
    input !== null &&
    typeof input === "object" &&
    (input as { danger?: unknown }).danger === "dangerous"
  );
}

/**
 * 未连接主机的目标 ID（`host-<host_id>`，和后端 `host_target_id` 一致）。
 */
export function hostTargetId(hostId: number): string {
  return `host-${hostId}`;
}

/**
 * 工具调用的友好名称（卡片 §4：界面显示"列出服务器/执行命令/读取终端"，
 * 而不是 `zssh_run_command` 这种内部 id）。
 */
export function friendlyToolName(tool: string): string {
  const bare = (tool || "").replace(/^zssh[_-]?/i, "").toLowerCase();
  switch (bare) {
    case "list_sessions":
    case "list_targets":
      return "列出服务器";
    case "run_command":
      return "执行命令";
    case "read_terminal":
      return "读取终端";
    case "list_dir":
      return "列出目录";
    case "read_file":
      return "读取文件";
    case "write_file":
      return "写入文件";
    case "stat":
      return "查看文件信息";
    default:
      return tool || "工具调用";
  }
}

/** 工具结果里跑的哪台主机（卡片 §4：工具卡片要显示主机名和退出码）。 */
export function hostOf(
  input: unknown,
  hosts: Record<string, string>,
): string | null {
  const id = sessionIdOf(input);
  if (!id) return null;
  return hosts[id] ?? null;
}

/** 从工具输出里抠出退出码（zssh-mcp 结果文本里是 `exit_code: N`）。 */
export function exitCodeOf(output: string): number | null {
  const match = output.match(/exit_code:\s*(-?\d+)/i);
  if (!match) return null;
  const code = Number.parseInt(match[1], 10);
  return Number.isNaN(code) ? null : code;
}

/**
 * 是不是"用户主动停止"造成的错误（AI-2e 3.1）。
 *
 * 点停止会 abort 当前回合：会话级错误是 `MessageAbortedError: Aborted`，正在跑的
 * 工具被中止时结果是 `Tool execution aborted`。这是用户自己的动作，不是失败：
 * 不弹错误提示，卡片显示"已停止"。
 */
/** 智能体在选择器里的可用状态（AI-3 第 6 节；不可用时给原因）。 */
export type AgentAvailability = "ok" | "missing" | "login" | "old";

/**
 * 算一个智能体的可用状态。
 *
 * `install` 是发现结果（undefined = 还在检测），`login` 只有 codex 传（opencode
 * 没有登录概念）。没安装 / 版本过低 / 没登录都算不可用，各自给不同的原因。
 */
export function agentAvailabilityOf(
  install: AgentInstall | null | undefined,
  login?: AgentLoginStatus,
): AgentAvailability {
  if (install === null) return "missing";
  if (install === undefined) return "ok";
  if (install.too_old) return "old";
  if (login?.logged_in === false) return "login";
  return "ok";
}

/** 来源的中文说法（设置页 / 提示文案共用）。 */
export function installSourceLabel(source: AgentInstall["source"]): string {
  switch (source) {
    case "setting":
      return "手动指定";
    case "tools_dir":
      return "数据目录";
    case "known_location":
      return "常见安装位置";
    default:
      return "PATH";
  }
}

/**
 * "还有一个旧版本被跳过了"的提示（M3-1 第 0 节）；没有可跳过的不提示。
 *
 * 例：`PATH 上还有一个旧版本 1.17.11（低于最低要求，没有用它）`。
 */
export function skippedOlderNote(install: AgentInstall): string | null {
  const older = install.skipped_older;
  if (!older) return null;
  return `${installSourceLabel(older.source)} 上还有一个旧版本 ${older.version.trim()}（低于最低要求，没有用它）`;
}

export function isAborted(text: string): boolean {
  return /MessageAbortedError|Tool execution aborted/i.test(text);
}

/** opencode 免费服务的原始报错，换成用户看得懂、知道该怎么办的话；其他错误返回原文。 */
export function friendlyAgentError(message: string): string {
  if (/1.18.0 or newer is required/i.test(message)) {
    return "opencode 版本太旧，免费模型需要 1.18 以上，请升级 opencode（npm i -g opencode-ai@latest）后重启 Z-SSH";
  }
  if (/free tier can only be used from within OpenCode/i.test(message)) {
    return "OpenCode 的免费服务拒绝了这次请求，可以先换一个非免费模型；如果一直这样，请反馈";
  }
  return message;
}

/** 一个 @ 主机（或当前那台）在标签状态点上的状态（AI-2d 3.2）。 */
export type HostDotState = "connected" | "connecting" | "offline";

/**
 * 还能不能把这台主机 @ 进对话（AI-2d 3.1）。
 *
 * 已经是"当前焦点终端"那台就不要再加一个标签：目标列表里它已经以
 * "（当前）"的形式在了，再加一条就是重复。
 */
export function canPinHost(hostId: number, currentHostId: number | null): boolean {
  return currentHostId === null || hostId !== currentHostId;
}

/** @ 标签状态点的颜色类（AI-2d 3.2：反映真实状态，不是固定琥珀色）。 */
export function hostDotClass(state: HostDotState): string {
  switch (state) {
    case "connected":
      return "bg-emerald-500";
    case "connecting":
      return "bg-amber-400 animate-pulse";
    default:
      return "bg-muted-foreground/50";
  }
}

/** @ 标签状态点的提示文字。 */
export function hostDotTitle(state: HostDotState): string {
  switch (state) {
    case "connected":
      return "已 @ 进本次对话（已连接）";
    case "connecting":
      return "已 @ 进本次对话（连接中）";
    default:
      return "已 @ 进本次对话（还没连上）";
  }
}

/** 一台主机在当前界面里的连接状态（App 汇总所有标签得到）。 */
export function hostStateOf(
  states: Record<number, HostDotState>,
  hostId: number,
): HostDotState {
  return states[hostId] ?? "offline";
}

// ---------------------------------------------------------------------------
// 文件工具（M3-4）
// ---------------------------------------------------------------------------

/** 去掉智能体加的前缀，得到 `zssh-mcp` 里的工具名（`zssh_write_file` → `write_file`）。 */
export function bareToolName(tool: string): string {
  return (tool || "").replace(/^(mcp__)?zssh[_-]{1,2}/i, "").toLowerCase();
}

/** 是不是文件工具。 */
export function isFileTool(tool: string): boolean {
  return ["list_dir", "read_file", "write_file", "stat"].includes(bareToolName(tool));
}

/** 文件工具输入里的路径。 */
export function pathOf(input: unknown): string | null {
  if (input && typeof input === "object" && "path" in input) {
    const path = (input as { path?: unknown }).path;
    if (typeof path === "string") return path;
  }
  return null;
}

/** 确认卡片里 write_file 的差异行（和后端 `FileDiff` 一一对应）。 */
export interface DiffLineView {
  kind: "ctx" | "add" | "del" | "hunk";
  text: string;
}

export interface FileDiffView {
  lines: DiffLineView[];
  added: number;
  removed: number;
  omitted_lines: number;
  is_new: boolean;
}

/** 差异默认最多显示多少行，更长的折叠起来，点"展开"看全部。 */
export const DIFF_FOLD_LIMIT = 30;

/**
 * 差异太长时折叠：返回要显示的行和被折叠的行数。`expanded` 为真时全部显示。
 * 后端自己还会在 4000 行处截断（`omitted_lines`），两者分开算。
 */
export function foldDiff(
  lines: DiffLineView[],
  expanded: boolean,
  limit: number = DIFF_FOLD_LIMIT,
): { visible: DiffLineView[]; hidden: number } {
  if (expanded || lines.length <= limit) return { visible: lines, hidden: 0 };
  return { visible: lines.slice(0, limit), hidden: lines.length - limit };
}

/** 差异的一句话摘要：`新建文件，共 3 行` / `+2 −1`。 */
export function diffSummary(diff: Pick<FileDiffView, "added" | "removed" | "is_new">): string {
  if (diff.is_new) return `新建文件，共 ${diff.added} 行`;
  if (diff.added === 0 && diff.removed === 0) return "内容没有变化";
  return `+${diff.added} −${diff.removed}`;
}

/** 待确认请求里和文件有关的部分（`PendingApproval` 的子集，方便单测）。 */
export interface FileApprovalLike {
  reqId: number;
  tool: string;
  command: string;
  filePath?: string;
}

/**
 * 哪条待确认请求属于这张工具卡片（`status` 是卡片当前状态，只有 `running` 的卡片才会走退路）。
 *
 * - `run_command`：命令全文相同；
 * - `write_file`：请求里的路径是服务器上解析后的绝对路径，工具输入里可能是相对路径或
 *   `~/…`，所以按"绝对路径等于它，或以它（去掉 `~/` `./`）结尾"来对；
 *   工具输入的路径对不上时（比如带了 `..`），退回"只有一条写文件请求在等"。
 */
export function matchApproval<T extends FileApprovalLike>(
  approvals: T[],
  tool: string,
  input: unknown,
  status: string = "running",
): T | undefined {
  const bare = bareToolName(tool);
  if (bare === "run_command") {
    const command = commandOf(input);
    return approvals.find((a) => a.tool === "run_command" && a.command === command);
  }
  if (bare !== "write_file") return undefined;
  const candidates = approvals.filter((a) => a.tool === "write_file");
  const raw = pathOf(input);
  if (raw) {
    const tail = raw.replace(/^~\//, "").replace(/^(\.\/)+/, "");
    const hit = candidates.find(
      (a) =>
        a.filePath === raw ||
        (tail !== "" && !tail.startsWith("/") && !tail.includes("..") && a.filePath?.endsWith(`/${tail}`)),
    );
    if (hit) return hit;
  }
  // 路径对不上时的退路只给**还在等结果的**卡片：已经写完的卡片不能把新一条确认抢走
  // （M3-4 截图里出现过：新的系统路径确认被挂到了前一张已完成的卡片上）。
  return status === "running" && candidates.length === 1 ? candidates[0] : undefined;
}

/**
 * 把智能体包的 JSON 拆成可读文本。
 *
 * 有的智能体把 MCP 结果原样放进 `[{"type":"text","text":"…"}]`，卡片上就显示成一串
 * 带 `\n` 的 JSON（M3-4 截图 03）。这里按层拆开文本块；拆不出来就原样返回，
 * 不能把本来就不是 JSON 的输出吞掉。
 */
export function readableOutput(output: string): string {
  let text = output;
  for (let depth = 0; depth < 3; depth += 1) {
    const trimmed = text.trim();
    if (
      !trimmed.startsWith("[") &&
      !trimmed.startsWith("{") &&
      !trimmed.startsWith('"')
    ) {
      break;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      break;
    }
    if (typeof parsed === "string") {
      text = parsed;
      continue;
    }
    if (Array.isArray(parsed)) {
      const pieces: string[] = [];
      const known = parsed.every((item) => {
        if (typeof item === "string") {
          pieces.push(item);
          return true;
        }
        if (item && typeof item === "object" && typeof (item as { text?: unknown }).text === "string") {
          pieces.push((item as { text: string }).text);
          return true;
        }
        return false;
      });
      if (!known) break;
      text = pieces.join("");
      continue;
    }
    if (parsed && typeof parsed === "object" && typeof (parsed as { text?: unknown }).text === "string") {
      text = (parsed as { text: string }).text;
      continue;
    }
    break;
  }
  return text;
}

/** 从 write_file 的结果文本里取"被标红"的原因（`danger: flagged as a system path (…)`）。 */
export function fileDangerReason(output: string): string | null {
  const match = readableOutput(output).match(/danger:\s*flagged as a system path \(([^)"\\]*)\)/i);
  return match ? match[1].trim() : null;
}

/** 从 write_file 的结果文本里取备份路径。 */
export function backupPathOf(output: string): string | null {
  // 有的智能体把结果包成 JSON 字符串（换行是字面的反斜杠加 n，后面跟着引号），路径到这些字符为止。
  const match = readableOutput(output).match(/saved on the server at\s+([^\s"\\]+)/i);
  return match ? match[1] : null;
}

/** 从 write_file 的结果文本里取"属主没能改回去"的详情。 */
export function ownerChangeOf(output: string): { from: string; to: string } | null {
  const match = readableOutput(output).match(
    /owner: could not restore the file's owner: it changed from (.*?) to ([^\n(]+)/i,
  );
  return match ? { from: match[1].trim(), to: match[2].trim() } : null;
}
