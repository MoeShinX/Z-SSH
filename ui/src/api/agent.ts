//! AI 面板（AI-2，AI-3 起多智能体）的命令和事件封装，命令名和后端 `commands::agent` 一致。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 智能体 id（AI-3）。 */
export type AgentId = "opencode" | "codex" | "claude";

/** 找到的智能体安装。 */
export interface AgentInstall {
  path: string;
  version: string;
  source: "setting" | "tools_dir" | "path" | "known_location";
  /** `.cmd` / `.bat` 包装脚本，启动要走 cmd /c（后端内部处理）。 */
  needs_shell: boolean;
  /** 自动发现的结果（手动指定时为 false）。 */
  auto: boolean;
  /** 版本低于最低要求（低于 1.19 时有 SQLite 外键 bug）。 */
  too_old: boolean;
  /**
   * 前面找到过、但版本低于最低要求所以被跳过的安装（M3-1 第 0 节）：PATH 上还有个
   * 旧版本时这里会有值，界面据此提示"PATH 上还有一个旧版本 x.y.z"。
   */
  skipped_older: { version: string; source: AgentInstall["source"] } | null;
}

/** 最新 Release 的下载信息（前端弹确认框用）。 */
export interface ReleaseInfo {
  version: string;
  asset_name: string;
  size: number;
  /** Release 公布的 SHA-256（小写十六进制）；后端下载完核对一致才安装。 */
  sha256: string;
  download_url: string;
  page_url: string;
}

/** 启动一个对话后的信息。 */
export interface AgentStartInfo {
  session_key: string;
  /** 工具桥 scope id：审批事件里带的是它。 */
  scope_id: string;
  agent_session_id: string;
  /** 本对话用的智能体（每个对话固定）。 */
  agent: AgentId;
  /** 要求续接历史会话时是否接上了（false = 新会话，之前的上下文没带给 AI）。 */
  resumed: boolean;
}

/** 登录状态（AI-3）。`logged_in` 为 null 表示这个智能体没有登录概念。 */
export interface AgentLoginStatus {
  logged_in: boolean | null;
  hint: string | null;
}

/** 可用模型。 */
export interface AgentModel {
  id: string;
  name: string;
  /** 已知不可用（例如 opencode 的免费用量模型只能从它自己的界面用）。 */
  unavailable?: boolean;
}

export interface AgentModelCatalog {
  models: AgentModel[];
  default: string | null;
  /** "默认"实际对应的模型（显示用）；读不到为 null（界面只写"默认模型"）。 */
  default_label?: string | null;
}

/** 驱动的统一流式事件（`kind` 区分类型）。 */
export type AgentEvent =
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string }
  | { kind: "tool_start"; call_id: string; tool: string; input: unknown }
  | { kind: "tool_end"; call_id: string; tool: string; output: string; error: boolean }
  | { kind: "turn_end" }
  | { kind: "error"; message: string }
  /** 智能体遇到额度用完 / 限流 / 过载，正在自动重试（opencode）；`reason` 已经是人话。 */
  | { kind: "retrying"; attempt: number; reason: string; next_in_secs: number | null }
  /** 续接历史会话失败，已作为新会话继续。 */
  | { kind: "resume_failed" };

/** 事件带对话键。 */
export type AgentEventPayload = AgentEvent & { session_key: string };

/** 目标进出 scope 的同步项（AI-2c）。 */
export interface ScopeTargetChange {
  target_id: string;
  host_label: string;
  username: string;
  status: string;
  current?: boolean;
  pinned?: boolean;
  /** 这个目标绑定的会话（这台主机焦点所在 / 最近获得焦点的终端）；没有为 null。 */
  session_id?: string | null;
}

/** @ 主机选择弹窗里的一项（主机库里的一台主机，不含地址）。 */
export interface HostPickerEntry {
  host_id: number;
  name: string;
  username: string;
}

/** 事件：AI 需要一个还没打开的 @ 主机自动连接。 */
export interface TargetConnectRequest {
  host_id: number;
  target_id: string;
}

/** 下载进度。 */
export interface DownloadProgress {
  received: number;
  total: number;
  done: boolean;
}

/** 找可用的智能体；设置里的路径不可用时抛错。 */
export function aiAgentDiscover(agent: AgentId): Promise<AgentInstall | null> {
  return invoke<AgentInstall | null>("ai_agent_discover", { agent });
}

/** 探测一个指定路径（设置页「检测」）。 */
export function aiAgentProbePath(agent: AgentId, path: string): Promise<AgentInstall> {
  return invoke<AgentInstall>("ai_agent_probe_path", { agent, path });
}

/** 启动一个对话；`installPath` 指定用哪个安装（空则自动发现）。 */
export function aiAgentStart(
  agent: AgentId,
  installPath?: string,
  /** 续接哪个历史会话（只给历史 id，智能体自己的会话标识由后端从历史里取）。 */
  resumeHistoryId?: string,
): Promise<AgentStartInfo> {
  return invoke<AgentStartInfo>("ai_agent_start", {
    agent,
    installPath: installPath && installPath.trim() ? installPath : null,
    resumeHistoryId: resumeHistoryId ?? null,
  });
}

/** 历史会话里的一条消息（和后端 `ai::history::HistoryMessage` 一致）。 */
export type HistoryMessage =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; tool: string; input: unknown; output: string; status: string };

/** 历史列表里的一项。 */
export interface HistorySummary {
  id: string;
  agent: AgentId;
  title: string;
  /** 毫秒时间戳。 */
  created_at: number;
  updated_at: number;
  host_label: string | null;
  message_count: number;
  /** 有智能体自己的会话标识，可以尝试续接。 */
  resumable: boolean;
}

/** 一个历史会话的完整内容。 */
export interface HistoryRecord {
  version: number;
  id: string;
  agent: AgentId;
  title: string;
  created_at: number;
  updated_at: number;
  host_label: string | null;
  agent_session_id: string | null;
  messages: HistoryMessage[];
}

/** 保存时交给后端的内容（标题、时间、会话标识由后端定）。 */
export interface HistoryDraft {
  id: string;
  host_label: string | null;
  messages: HistoryMessage[];
}

export function aiHistoryList(agent: AgentId): Promise<HistorySummary[]> {
  return invoke<HistorySummary[]>("ai_history_list", { agent });
}

export function aiHistoryLoad(agent: AgentId, id: string): Promise<HistoryRecord> {
  return invoke<HistoryRecord>("ai_history_load", { agent, id });
}

/** 保存当前会话；`sessionKey` 是还活着的对话（后端从它取续接标识）。 */
export function aiHistorySave(
  agent: AgentId,
  sessionKey: string | null,
  draft: HistoryDraft,
): Promise<HistorySummary | null> {
  return invoke<HistorySummary | null>("ai_history_save", { agent, sessionKey, draft });
}

export function aiHistoryDelete(agent: AgentId, id: string): Promise<void> {
  return invoke("ai_history_delete", { agent, id });
}

/** 清空所有智能体的 AI 历史，返回删了几个会话。 */
export function aiHistoryClear(): Promise<number> {
  return invoke<number>("ai_history_clear");
}

/** 登录状态（codex 需要；opencode 返回 null）。只判断有没有登录，不读密钥。 */
export function aiAgentLogin(agent: AgentId): Promise<AgentLoginStatus> {
  return invoke<AgentLoginStatus>("ai_agent_login", { agent });
}

/** 核对 codex 的本机工具能不能确认关掉（UI-1 0.1）。
 *  返回 null = 通过；返回字符串 = 拒绝启动的完整原因（界面直接显示）。 */
export function aiAgentGuard(
  agent: AgentId,
  installPath?: string,
): Promise<string | null> {
  return invoke<string | null>("ai_agent_guard", {
    agent,
    installPath: installPath && installPath.trim() ? installPath : null,
  });
}

export function aiAgentStop(sessionKey: string): Promise<void> {
  return invoke("ai_agent_stop", { sessionKey });
}

export function aiAgentSend(
  sessionKey: string,
  text: string,
  model?: string | null,
): Promise<void> {
  return invoke("ai_agent_send", { sessionKey, text, model: model ?? null });
}

export function aiAgentAbort(sessionKey: string): Promise<void> {
  return invoke("ai_agent_abort", { sessionKey });
}

export function aiAgentModels(sessionKey: string): Promise<AgentModelCatalog> {
  return invoke<AgentModelCatalog>("ai_agent_models", { sessionKey });
}

/** 不开对话直接取模型目录（面板一打开就显示默认模型）。 */
export function aiAgentCatalog(agent: AgentId, installPath?: string): Promise<AgentModelCatalog> {
  return invoke<AgentModelCatalog>("ai_agent_catalog", {
    agent,
    installPath: installPath && installPath.trim() ? installPath : null,
  });
}

export function aiAgentScopeSync(
  sessionKey: string,
  added: ScopeTargetChange[],
  removed: string[],
): Promise<void> {
  return invoke("ai_agent_scope_sync", { sessionKey, added, removed });
}

/** 列主机库里的主机（@ 选择弹窗用；只有名字和用户名）。 */
export function aiHostPicker(): Promise<HostPickerEntry[]> {
  return invoke<HostPickerEntry[]>("ai_host_picker");
}

/** 界面为 AI 自动连接一个 @ 主机成功了（新标签连上后调）。 */
export function aiTargetConnectResult(
  hostId: number,
  sessionId: string,
): Promise<void> {
  return invoke("ai_target_connect_result", { hostId, sessionId });
}

/** 界面拒绝了自动连接（用户取消）。 */
export function aiTargetConnectCancel(hostId: number): Promise<void> {
  return invoke("ai_target_connect_cancel", { hostId });
}

/** 订阅：AI 需要一个还没打开的 @ 主机自动连接。 */
export function onTargetConnect(
  handler: (request: TargetConnectRequest) => void,
): Promise<UnlistenFn> {
  return listen<TargetConnectRequest>("ai_target_connect", (event) =>
    handler(event.payload),
  );
}

export function aiAgentRelease(sessionKey: string): Promise<void> {
  return invoke("ai_agent_release", { sessionKey });
}

/** 查最新 Release 的下载信息（来源、版本、大小），供确认框展示。 */
export function aiAgentDownloadInfo(): Promise<ReleaseInfo> {
  return invoke<ReleaseInfo>("ai_agent_download_info");
}

/** 一键下载 opencode（下载前应先向用户确认来源和大小）。 */
export function aiAgentDownload(): Promise<AgentInstall> {
  return invoke<AgentInstall>("ai_agent_download");
}

export function aiAgentDownloadCancel(): Promise<void> {
  return invoke("ai_agent_download_cancel");
}

export function onAgentEvent(
  handler: (event: AgentEventPayload) => void,
): Promise<UnlistenFn> {
  return listen<AgentEventPayload>("ai_agent_event", (event) =>
    handler(event.payload),
  );
}

export function onDownloadProgress(
  handler: (progress: DownloadProgress) => void,
): Promise<UnlistenFn> {
  return listen<DownloadProgress>("ai_download_progress", (event) =>
    handler(event.payload),
  );
}

/** 手动模式下的确认请求（`ai_approval_request`，来自工具桥）。 */
export interface ApprovalRequest {
  req_id: number;
  scope: string;
  /** 这次工具调用的执行编号（和 `ai_exec_target` 回报、工具结果末尾的标记是同一个）。 */
  exec_id: string;
  host_label: string;
  command: string;
  danger: string;
  reasons: string[];
  /** 实际执行的目标和会话（和 `ai_exec_target` 同一份解析结果；会话为 null = 还没连上）。 */
  target_id: string;
  session_id: string | null;
  /** `run_command` 或 `write_file`（M3-4）。 */
  tool: string;
  /** `write_file` 的细节：写哪个文件、和现有文件的差异。 */
  file?: {
    path: string;
    is_new: boolean;
    new_bytes: number;
    diff: {
      lines: Array<{ kind: "ctx" | "add" | "del" | "hunk"; text: string }>;
      added: number;
      removed: number;
      omitted_lines: number;
      is_new: boolean;
    };
  } | null;
}

/**
 * 执行层解析出的一次工具调用的实际执行位置（`ai_exec_target`，AI-SCOPE-2）：每次调用解析完
 * 目标、真正开始之前发一次。卡片上的主机名、「查看终端」、标签上的活动点都用它。
 */
export interface ExecTargetReport {
  scope: string;
  /**
   * 这次工具调用的执行编号（AI-SCOPE-3）：后端每接到一个工具请求生成一个，工具结果文本末尾
   * 带 `[zssh exec <编号>]` 一行。界面拿结果里的编号校正卡片和回报的配对。只是关联用，不是凭据。
   */
  exec_id: string;
  /** 不带前缀的工具名：`run_command`、`read_terminal`、`list_dir`、`read_file`、`write_file`、`stat`。 */
  tool: string;
  /** 对到卡片上用的键：命令原文 / 调用时给的路径原文 / 空（`read_terminal`）。 */
  key: string;
  /** 逻辑目标 ID（`host-<id>`，快速连接是会话 ID）。 */
  target_id: string;
  /** 实际执行用的会话；这台主机现在没有可用会话（会自动连接）时为 null。 */
  session_id: string | null;
  host_label: string;
}

export function onExecTarget(
  handler: (report: ExecTargetReport) => void,
): Promise<UnlistenFn> {
  return listen<ExecTargetReport>("ai_exec_target", (event) => handler(event.payload));
}

/** 回答一条确认请求（"执行 / 拒绝"按钮调它）。 */
export function aiApprovalAnswer(reqId: number, allow: boolean): Promise<void> {
  return invoke("ai_approval_answer", { reqId, allow });
}

export function onApprovalRequest(
  handler: (request: ApprovalRequest) => void,
): Promise<UnlistenFn> {
  return listen<ApprovalRequest>("ai_approval_request", (event) =>
    handler(event.payload),
  );
}
