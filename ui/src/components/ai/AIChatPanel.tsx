// Adapted from Netcatty components/AIChatSidePanel.tsx
//! AI 面板（AI-2 / AI-2c）：终端标签右侧的对话面板。
//!
//! 一个标签一个对话。顶部选模型、切换手动/自动执行模式、新对话；消息区把驱动
//! 事件渲染成流式 Markdown 和工具调用卡片；手动模式下危险/待确认命令在卡片上
//! 直接点「执行 / 拒绝」（同一张卡片，不另开一张）。输入框里打 @ 把主机库里的
//! 主机加进本次对话的目标列表。本机能力被禁用的隔离在驱动层做。

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowDown,
  Bot,
  ChevronRight,
  History,
  Play,
  Square,
  Plus,
  Trash2,
  X,
} from "lucide-react";

import { Button } from "../ui/button";
import { Badge } from "../ui/badge";
import { Textarea } from "../ui/textarea";
import { Spinner } from "../ui/spinner";
import { ScrollArea } from "../ui/scroll-area";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../ui/select";
import { cn } from "../../lib/utils";
import { toast } from "../ui/toast";
import ThinkingIndicator, { showThinking, type RetryNotice } from "./ThinkingIndicator";
import {
  aiAgentAbort,
  aiAgentCatalog,
  aiAgentDiscover,
  aiAgentLogin,
  aiAgentModels,
  aiAgentScopeSync,
  aiAgentSend,
  aiAgentStart,
  aiAgentStop,
  aiApprovalAnswer,
  aiHistoryDelete,
  aiHistoryList,
  aiHistoryLoad,
  aiHistorySave,
  aiHostPicker,
  onAgentEvent,
  onApprovalRequest,
  onExecTarget,
  type AgentEventPayload,
  type AgentId,
  type AgentInstall,
  type AgentLoginStatus,
  type AgentModelCatalog,
  type ApprovalRequest,
  type ExecTargetReport,
  type HistoryRecord,
  type HistorySummary,
  type HostPickerEntry,
  type ScopeTargetChange,
} from "../../api/agent";
import { EXEC_MODE_OPTIONS, toExecMode } from "../../state/execMode";
import { useSettings } from "../../stores/settings";
import { ResizeHandle, usePanelWidth } from "../ui/resize-handle";
import { InputResizeHandle, useAiInputHeight } from "./InputResizeHandle";
import { useStickToBottom } from "./useStickToBottom";
import { AssistantMessage } from "./Markdown";
import { callFailed, groupChatItems, progressText, summarizeCalls } from "./toolGroups";
import { AI_PANEL_DEFAULT } from "../../state/panelWidths";
import {
  agentAvailabilityOf,
  hostDotClass,
  hostDotTitle,
  hostStateOf,
} from "./aiHelpers";
import {
  buildScopeTargets,
  pinHost,
  pinIsCurrent,
  pinKey,
  pruneRetained,
  registerTargets,
  retainPrevious,
  type KnownTarget,
  type PinnedHost,
  type ScopeSession,
} from "./scopeTargets";
import type { ActivityEvent } from "../../state/aiActivity";
import type { TargetRef } from "../../state/aiTarget";
import {
  applyResultExecId,
  execTargetOf,
  execTargetOfApproval,
  findCallForReport,
  pairApproval,
  reportArrived,
  splitExecMarker,
  trackCall,
  type ExecChange,
  type TrackedCall,
} from "./execTargets";
import {
  CONTEXT_LOST_NOTICE,
  formatHistoryTime,
  fromHistoryMessages,
  hasUserMessage,
  historyHostLabel,
  newHistoryId,
  toHistoryMessages,
  type ChatItem,
  type ExecTarget,
  type ToolCall,
} from "./chatHistory";
import {
  cachedCatalog,
  catalogCacheKey,
  loadCatalog,
  modelForSend,
  storeCatalog,
} from "./modelOptions";
import {
  buildModelPicker,
  effortChoices,
  effortLabel,
  joinSelection,
  pickEffort,
  selectionAfterCatalog,
  splitSelection,
} from "./modelPicker";
import {
  backupPathOf,
  bareToolName,
  commandOf,
  diffSummary,
  exitCodeOf,
  fileDangerReason,
  foldDiff,
  friendlyToolName,
  hostOf,
  isAborted,
  friendlyAgentError,
  isDangerous,
  isFileTool,
  ownerChangeOf,
  pathOf,
  readableOutput,
  visibleCommandText,
} from "./aiHelpers";

/** 智能体的显示名（AI-3）。 */
const AGENT_LABELS: Record<AgentId, string> = {
  opencode: "opencode",
  codex: "codex",
  claude: "Claude Code",
};


/** 等待确认的命令（手动模式）：来自 `ai_approval_request`。 */
export interface PendingApproval {
  reqId: number;
  /** 这次工具调用的执行编号：和工具卡片认领的回报的编号对上，才把「执行 / 拒绝」放到那张卡片上。 */
  execId: string;
  hostLabel: string;
  command: string;
  danger: string;
  reasons: string[];
  /** `run_command` 或 `write_file`（M3-4）。 */
  tool: string;
  /** `write_file` 要写的服务器路径（解析后的绝对路径）。 */
  filePath?: string;
  /** `write_file` 的差异和细节。 */
  file?: ApprovalRequest["file"];
  /** 执行层解析出的实际执行位置（和工具开始的回报同一份）。 */
  exec?: ExecTarget | null;
}

export type { PinnedHost };

/** AI 在终端里执行命令的开始 / 结束 / 位置纠正（App 据此在后台标签上显示活动提示）。 */
export type AiActivityEvent = ActivityEvent;

/** 一个 @ 主机（或者当前那台）的连接状态（标签上的状态点）。 */
export type HostConnectionState = "connected" | "connecting" | "offline";

/** 思考强度下拉框里"默认强度"的值（不发 `--effort`，用智能体自己的默认）。 */
const DEFAULT_EFFORT_VALUE = "__zssh_default_effort__";

interface AIChatPanelProps {
  /** 这个面板对应的标签 id（scope 归属）。 */
  tabId: string;
  /** 标签里当前的会话（目标列表的"当前"来源）。 */
  sessions: {
    sessionId: string;
    hostId: number | null;
    hostLabel: string;
    username: string;
    /** 键盘焦点所在的 pane（绑定会话优先取它）。 */
    focused?: boolean;
  }[];
  /** @ 标签的状态点：主机 id → 真实连接状态（AI-2d 3.2）。 */
  hostStates: Record<number, HostConnectionState>;
  firstUse: boolean;
  onMarkUsed: () => void;
  onOpenSettings: () => void;
  /** 点工具卡片上的"查看终端"：切到那个目标所在的标签和窗格。 */
  onViewTarget?: (target: TargetRef) => void;
  /** 界面里所有还开着的会话 ID（判断切走时保留的会话是不是已经关了；不给就当都开着）。 */
  openSessionIds?: ReadonlySet<string>;
  /** AI 在终端里执行命令的开始 / 结束。 */
  onAiActivity?: (event: AiActivityEvent) => void;
  onClose: () => void;
  /** 三块同屏（M3-3）：SFTP 面板占的宽度（拖宽上限要让给它）。 */
  reservedWidth?: number;
  /** 三块同屏时由布局算出的实际宽度（窗口窄时比记住的小）。 */
  widthOverride?: number | null;
}

/** 把一段 AI 输出的文字接到最后一条 AI 消息后面（最后一条不是 AI 消息就新起一条）。 */
export function appendAssistantText(items: ChatItem[], text: string): ChatItem[] {
  const last = items[items.length - 1];
  if (last && last.kind === "assistant") {
    const next = items.slice(0, -1);
    next.push({ kind: "assistant", text: last.text + text });
    return next;
  }
  return [...items, { kind: "assistant", text }];
}

export default function AIChatPanel({
  sessions,
  hostStates,
  firstUse,
  onMarkUsed,
  onOpenSettings,
  onViewTarget,
  openSessionIds,
  onAiActivity,
  onClose,
  reservedWidth = 0,
  widthOverride = null,
}: AIChatPanelProps) {
  /** 每个智能体的发现结果（undefined = 检测中，null = 没找到）。 */
  const [installs, setInstalls] = useState<Partial<Record<AgentId, AgentInstall | null>>>({});
  /** 登录状态（只有 codex 需要判断）。 */
  const [logins, setLogins] = useState<Partial<Record<AgentId, AgentLoginStatus>>>({});
  /** 用户在面板里选的智能体；没选过就用设置页里的默认值（AI-3 第 6 节）。 */
  const defaultAgent = useSettings((state) => state.ai_default_agent);
  /** 执行模式（R-2 0.1）：面板和设置页读写**同一份** store 状态，两边随时一致。 */
  const execMode = useSettings((state) => state.ai_exec_mode);
  const updateSettings = useSettings((state) => state.update);
  const [chosenAgent, setChosenAgent] = useState<AgentId | null>(null);
  const [sessionKey, setSessionKey] = useState<string | null>(null);
  /** 模型目录（连同它属于哪个智能体：切换智能体时旧目录不能拿来用）。 */
  const [catalogState, setCatalogState] = useState<{
    agent: AgentId;
    catalog: AgentModelCatalog;
  } | null>(null);
  const [model, setModel] = useState<string>("");
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  /** 这一轮是什么时候发出的（"思考中"后面的秒数从这里算）。 */
  const [turnStartedAt, setTurnStartedAt] = useState(0);
  /** 智能体正在自动重试（额度用完 / 限流 / 过载）：「思考中」旁边的提示；出字、回合结束就清掉。 */
  const [retryNotice, setRetryNotice] = useState<RetryNotice | null>(null);
  const [starting, setStarting] = useState(false);
  /** 上一次"新对话 / 换智能体"失败的原因（UI-1 0.1：要一直显示到重试成功）。 */
  const [startError, setStartError] = useState<string | null>(null);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  /** 用户 @ 进来的主机（本次对话保留到移除或对话结束）。 */
  const [pinned, setPinned] = useState<PinnedHost[]>([]);
  /** @ 选择弹窗：null 表示没打开。 */
  const [picker, setPicker] = useState<HostPickerEntry[] | null>(null);
  const [pickerQuery, setPickerQuery] = useState("");
  /** 正在终端里跑的 AI 命令的数量（角上显示"AI 正在执行"）。 */
  const [aiRunningInTerminal, setAiRunningInTerminal] = useState(false);
  /** 历史列表弹窗：null 表示没打开（R11）。 */
  const [historyList, setHistoryList] = useState<HistorySummary[] | null>(null);
  /** 续接没接上（或者续接失败）：消息流顶部提示"之前的上下文没有带给 AI"。 */
  const [contextLost, setContextLost] = useState(false);
  /** 每轮结束 +1，触发一次保存。 */
  const [saveTick, setSaveTick] = useState(0);
  /** 当前对话对应的历史会话 id（第一次保存时生成；打开历史会话时是那条的 id）。 */
  const historyIdRef = useRef<string | null>(null);
  /** 当前对话用的智能体（保存到哪个智能体的历史里）。 */
  const conversationAgentRef = useRef<AgentId | null>(null);
  /** 当前智能体（面板里选过就用选的，否则用设置页的默认值）。 */
  const activeAgent: AgentId = chosenAgent ?? defaultAgent;
  /** 面板宽度（UI-1 第 2 节）：设置表里读、拖左边缘改、双击把手恢复默认。 */
  const {
    width: panelWidth,
    bounds: panelBounds,
    onResize: onPanelResize,
    onCommit: onPanelCommit,
    onReset: onPanelReset,
  } = usePanelWidth("ai", "layout.ai_panel_width", AI_PANEL_DEFAULT, {
    reserved: reservedWidth,
    override: widthOverride,
  });

  /** 面板根节点（输入框高度的上限 = 面板高度的一半）。 */
  const rootRef = useRef<HTMLDivElement | null>(null);
  const inputSize = useAiInputHeight(rootRef);

  // 切焦点（AI-SCOPE-1 第 3 节）：对话进行中，原来的"当前"自动转成固定目标留在范围里
  // （显示成可以 × 掉的标签），新焦点成为"当前"；对话还没开始就跟着焦点走、不累积。
  // 放在渲染阶段同步完成：目标列表只看到"一步到位"的结果，不会先把旧的移出再加回来
  // （后端把"目标被移出"当作在途请求失效的信号）。
  const currentSession: ScopeSession | null = sessions[0] ?? null;
  // 每台主机最近一次获得焦点的会话：这台主机不是当前、或当前标签里没有焦点在它身上时，
  // 工具调用绑定到它（AI-SCOPE-2 第 1 节）。在渲染阶段更新，保证下面算目标列表时是最新的。
  const recentFocusRef = useRef(new Map<number, string>());
  for (const s of sessions) {
    if (s.focused && s.hostId !== null) recentFocusRef.current.set(s.hostId, s.sessionId);
  }
  const [previousSession, setPreviousSession] = useState<ScopeSession | null>(currentSession);
  const conversationStarted = sessionKey !== null && hasUserMessage(items);
  if ((currentSession?.sessionId ?? null) !== (previousSession?.sessionId ?? null)) {
    setPreviousSession(currentSession);
    if (conversationStarted) {
      setPinned((prev) =>
        retainPrevious(prev, previousSession, openSessionIds, currentSession),
      );
    }
  }
  // 自动保留的会话关了就跟着消失。
  useEffect(() => {
    setPinned((prev) => pruneRetained(prev, openSessionIds, recentFocusRef.current));
  }, [openSessionIds]);
  const pinnedRef = useRef(pinned);
  pinnedRef.current = pinned;
  /** 一台主机一个逻辑目标的目标列表（同步给后端，也是目标标签和卡片的依据）。 */
  const scopeTargets = useMemo(
    () => buildScopeTargets(sessions, pinned, openSessionIds, recentFocusRef.current),
    [sessions, pinned, openSessionIds],
  );
  /** 对话里出现过的目标：卡片据此标主机名、跳到终端（会话关了也留着）。 */
  const knownTargetsRef = useRef(new Map<string, KnownTarget>());
  registerTargets(knownTargetsRef.current, scopeTargets);
  const targetStateRef = useRef({ currentId: null as string | null, count: 0 });
  targetStateRef.current = {
    currentId: scopeTargets.find((t) => t.current)?.target_id ?? null,
    count: scopeTargets.length,
  };
  const activityRef = useRef(onAiActivity);
  activityRef.current = onAiActivity;
  /**
   * 带目标的工具调用（把执行层回报的实际执行位置对到卡片上）和比卡片先到的回报。
   * 回报和智能体的 `tool_start` 走两条通道，谁先到不保证（AI-SCOPE-2 第 2 节）。
   */
  const trackedCallsRef = useRef<TrackedCall[]>([]);
  const pendingReportsRef = useRef<ExecTargetReport[]>([]);
  /** 收到过的回报（按执行编号）：工具结果里的编号靠它找回对应的那份回报，上限 300 份。 */
  const reportsRef = useRef(new Map<string, ExecTargetReport>());
  /** 回报 → 卡片上的执行位置：对话里有不止一个目标、或者当时没有"当前"目标时要标出主机名。 */
  const execOfReport = useCallback((report: ExecTargetReport): ExecTarget => {
    const state = targetStateRef.current;
    return execTargetOf(report, state.count > 1 || state.currentId === null);
  }, []);

  /**
   * 配对变了（按编号校正，或者回报到了）：更新卡片上的执行位置；还在跑的 run_command 把活动点
   * 挪到新对应的那个终端（`start` 事件按 callId 覆盖原来的记录）。
   */
  const applyExecChanges = useCallback(
    (changes: ExecChange[]) => {
      if (changes.length === 0) return;
      const execs = new Map(
        changes.map((change) => [
          change.callId,
          change.report ? execOfReport(change.report) : undefined,
        ]),
      );
      setItems((prev) =>
        prev.map((item) => {
          if (item.kind !== "tool" || !execs.has(item.call.callId)) return item;
          const { exec: _old, ...rest } = item.call;
          const exec = execs.get(item.call.callId);
          return { kind: "tool", call: exec ? { ...rest, exec } : rest };
        }),
      );
      for (const change of changes) {
        const call = trackedCallsRef.current.find((c) => c.callId === change.callId);
        if (!call || call.tool !== "run_command") continue;
        if (call.ended) {
          // 已经返回的调用不再点亮活动点；它结束时若已经在某个标签上标过「有新输出」，
          // 位置被纠正了就把那个点收回来、挪到正确的标签（App 里没记过的调用不会补点）。
          activityRef.current?.({
            kind: "retarget",
            callId: call.callId,
            targetId: change.report?.target_id ?? null,
            sessionId: change.report?.session_id ?? null,
          });
        } else if (change.report) {
          activityRef.current?.({
            kind: "start",
            callId: call.callId,
            targetId: change.report.target_id,
            sessionId: change.report.session_id,
          });
        }
      }
    },
    [execOfReport],
  );

  const keyRef = useRef<string | null>(null);
  const scopeIdRef = useRef<string | null>(null);
  /** 上一次同步给 scope 的目标 ID（判断增减）。 */
  const scopeTargetsRef = useRef<string[]>([]);

  // AI 输出的文字一秒几十段：先攒在这里，每帧最多往消息列表里写一次（每写一次整个面板
  // 重新渲染一遍）。攒的文字记着属于哪个会话，换了会话就丢掉，不串到新对话里。
  // 任何别的事件（工具、回合结束、出错）、用户发新消息、换会话之前都先把攒的写进去，
  // 保证顺序不乱，回合结束存历史时也不漏最后几个字。
  const pendingText = useRef<{ key: string; text: string } | null>(null);
  const textFrame = useRef<number | null>(null);
  const flushText = useCallback(() => {
    if (textFrame.current !== null) {
      cancelAnimationFrame(textFrame.current);
      textFrame.current = null;
    }
    const pending = pendingText.current;
    pendingText.current = null;
    if (!pending || pending.text === "" || pending.key !== keyRef.current) return;
    setItems((prev) => appendAssistantText(prev, pending.text));
  }, []);
  const dropPendingText = useCallback(() => {
    if (textFrame.current !== null) {
      cancelAnimationFrame(textFrame.current);
      textFrame.current = null;
    }
    pendingText.current = null;
  }, []);
  useEffect(() => dropPendingText, [dropPendingText]);

  // 事件和审批都是按 key/scope 归位的，用 ref 读最新值，避免闭包旧值。
  const applyEvent = useCallback((payload: AgentEventPayload) => {
    if (payload.kind === "retrying") {
      setRetryNotice({
        attempt: payload.attempt,
        reason: payload.reason,
        nextInSecs: payload.next_in_secs,
      });
      return;
    }
    // 重试成功（又出字 / 出思考 / 跑工具）或这一轮结束 / 出错：重试提示消失。
    setRetryNotice(null);
    if (payload.kind === "text_delta") {
      const pending = pendingText.current;
      if (pending && pending.key === payload.session_key) pending.text += payload.text;
      else pendingText.current = { key: payload.session_key, text: payload.text };
      textFrame.current ??= requestAnimationFrame(() => {
        textFrame.current = null;
        flushText();
      });
      return;
    }
    // 思考过程现在不显示：也不打断文字的合并
    if (payload.kind === "reasoning_delta") return;
    flushText();
    // 带目标的调用先登记，等执行层回报实际执行位置；回报先到的这里立刻认领。
    let tracked: TrackedCall | null = null;
    let earlyExec: ExecTarget | undefined;
    // 工具结果去掉末尾的执行编号标记后的文本（用户看到、存历史的都是它）。
    let output = "";
    if (payload.kind === "tool_start") {
      tracked = trackCall(payload.call_id, payload.tool, payload.input);
      if (tracked) {
        const calls = trackedCallsRef.current;
        calls.push(tracked);
        if (calls.length > 200) calls.splice(0, calls.length - 200);
        const early = pendingReportsRef.current.findIndex(
          (report) => findCallForReport([tracked as TrackedCall], report) !== undefined,
        );
        if (early >= 0) {
          const [report] = pendingReportsRef.current.splice(early, 1);
          tracked.claimed = true;
          tracked.execId = report.exec_id;
          earlyExec = execOfReport(report);
          if (tracked.tool === "run_command") {
            activityRef.current?.({
              kind: "start",
              callId: payload.call_id,
              targetId: report.target_id,
              sessionId: report.session_id,
            });
          }
        }
      }
    } else if (payload.kind === "tool_end") {
      // 工具结果末尾的执行编号：拿它按编号校正这张卡片对应的回报（卡片开始时只能先按「工具 +
      // 命令」猜）。要在标记 ended 之前做，这样还在跑的活动点能先挪到正确的终端再结束。
      const { text, execId } = splitExecMarker(payload.output);
      output = text;
      if (execId) {
        applyExecChanges(
          applyResultExecId(
            trackedCallsRef.current,
            reportsRef.current,
            payload.call_id,
            execId,
          ),
        );
      }
      const call = trackedCallsRef.current.find((c) => c.callId === payload.call_id);
      if (call) call.ended = true;
      if (isRunCommand(payload.tool)) {
        activityRef.current?.({ kind: "end", callId: payload.call_id });
      }
    }
    setItems((prev) => {
      switch (payload.kind) {
        case "tool_start":
          // 执行命令的卡片出现时点亮"AI 正在终端里执行"（真正在终端里跑时它才有效；
          // 降级到后台时下面 tool_end 会清掉）。
          if (isRunCommand(payload.tool)) setAiRunningInTerminal(true);
          return [
            ...prev,
            {
              kind: "tool",
              call: {
                callId: payload.call_id,
                tool: payload.tool,
                input: payload.input,
                output: "",
                status: "running",
                ...(tracked ? { tracked: true } : {}),
                ...(earlyExec ? { exec: earlyExec } : {}),
              },
            },
          ];
        case "tool_end":
          if (isRunCommand(payload.tool)) setAiRunningInTerminal(false);
          return prev.map((item) =>
            item.kind === "tool" && item.call.callId === payload.call_id
              ? {
                  kind: "tool",
                  call: {
                    ...item.call,
                    output,
                    // 被拒绝的命令显示"已拒绝"，不是"失败"（AI-2c 0.2）；
                    // 用户点停止中止的显示"已停止"（AI-2e 3.1）。
                    status: payload.error
                      ? isAborted(output)
                        ? "stopped"
                        : isRejection(output)
                          ? "rejected"
                          : "error"
                      : "done",
                    location: locationOf(output),
                    fallbackReason: fallbackReasonOf(output),
                  },
                }
              : item,
          );
        default:
          return prev;
      }
    });
    if (payload.kind === "turn_end") {
      setRunning(false);
      setAiRunningInTerminal(false);
      activityRef.current?.({ kind: "reset" });
      setSaveTick((n) => n + 1);
    }
    if (payload.kind === "resume_failed") {
      // 续接失败，后端已经用新会话重发了这条消息（R11 方案 B）。
      setContextLost(true);
    }
    if (payload.kind === "error") {
      setRunning(false);
      setAiRunningInTerminal(false);
      activityRef.current?.({ kind: "reset" });
      setSaveTick((n) => n + 1);
      if (isAborted(payload.message)) {
        // 用户主动停止（MessageAbortedError）：不是错误，不弹提示（AI-2e 3.1）。
      } else {
        toast.error(friendlyAgentError(payload.message));
      }
    }
  }, [flushText, execOfReport, applyExecChanges]);

  // 执行层回报的实际执行位置：对到工具卡片上（卡片的主机名、「查看终端」、标签活动点都用它）。
  // 对不上卡片的先存着，卡片出现时认领。
  const handleExecReport = useCallback(
    (report: ExecTargetReport) => {
      if (report.scope !== scopeIdRef.current) return;
      const known = reportsRef.current;
      known.set(report.exec_id, report);
      if (known.size > 300) {
        const oldest = known.keys().next().value;
        if (oldest !== undefined) known.delete(oldest);
      }
      // 有卡片的结果已经指明要这份回报就按编号配，否则按启发式临时配。
      const arrived = reportArrived(trackedCallsRef.current, known, report);
      if (!arrived) {
        const pending = pendingReportsRef.current;
        pending.push(report);
        if (pending.length > 50) pending.splice(0, pending.length - 50);
        return;
      }
      applyExecChanges(arrived.changes);
    },
    [applyExecChanges],
  );
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onExecTarget(handleExecReport)
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      })
      .catch((error: unknown) => console.warn("订阅执行位置回报失败", error));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [handleExecReport]);

  // 发现两个智能体（一次）；codex 还要查一下登录状态（AI-3 第 5 节）。
  useEffect(() => {
    let alive = true;
    const attempt = (id: AgentId, tries: number) => {
      if (!alive) return;
      void aiAgentDiscover(id)
        .then((found) => {
          if (!alive) return;
          if (found || tries <= 0) {
            setInstalls((prev) => ({ ...prev, [id]: found }));
            if ((id === "codex" || id === "claude") && found && !found.too_old) {
              void aiAgentLogin(id)
                .then((status) => {
                  if (alive) setLogins((prev) => ({ ...prev, [id]: status }));
                })
                .catch(() => {});
            }
          } else {
            setTimeout(() => attempt(id, tries - 1), 800);
          }
        })
        .catch((error: unknown) => {
          console.warn(`发现 ${id} 失败`, error);
          if (!alive) return;
          if (tries > 0) setTimeout(() => attempt(id, tries - 1), 800);
          else setInstalls((prev) => ({ ...prev, [id]: null }));
        });
    };
    attempt("opencode", 4);
    attempt("codex", 4);
    attempt("claude", 4);
    return () => {
      alive = false;
    };
  }, []);

  // 订阅驱动事件：按 session_key 归位。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onAgentEvent((payload) => {
      if (payload.session_key === keyRef.current) applyEvent(payload);
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [applyEvent]);

  // 订阅确认请求：只认本对话 scope 的。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onApprovalRequest((request: ApprovalRequest) => {
      if (request.scope !== scopeIdRef.current) return;
      setApprovals((prev) => [
        ...prev,
        {
          reqId: request.req_id,
          execId: request.exec_id ?? "",
          hostLabel: request.host_label,
          command: request.command,
          danger: request.danger,
          reasons: request.reasons,
          tool: request.tool ?? "run_command",
          filePath: request.file?.path,
          file: request.file,
          exec: execTargetOfApproval(request),
        },
      ]);
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // 目标列表 = 当前标签的会话（"当前"）+ @ 进来的主机，变化时同步给 scope。
  useEffect(() => {
    const key = keyRef.current;
    if (!key) return;
    const wanted: ScopeTargetChange[] = scopeTargets;
    const wantedIds = wanted.map((t) => t.target_id);
    const removed = scopeTargetsRef.current.filter((id) => !wantedIds.includes(id));
    scopeTargetsRef.current = wantedIds;
    void aiAgentScopeSync(key, wanted, removed).catch((error: unknown) =>
      console.warn("同步目标列表失败", error),
    );
  }, [scopeTargets, sessionKey]);

  /**
   * 用户最后一次自己选的思考强度（null = 默认强度）：换模型、换智能体时能保持就保持，
   * 新模型不支持才换（见 pickEffort）。
   */
  const effortPrefRef = useRef<string | null>(null);

  /** 换上一份模型目录：用户选过且新目录里还有的模型保留，否则选"默认"并尽量保持强度。 */
  const applyCatalog = useCallback((agent: AgentId, catalog: AgentModelCatalog) => {
    setCatalogState({ agent, catalog });
    const picker = buildModelPicker(catalog);
    setModel((current) => selectionAfterCatalog(current, picker, effortPrefRef.current));
  }, []);

  /**
   * 开一个对话。`history` 给了就是打开历史会话：先把消息放回来回看，再让后端用保存的
   * 智能体会话标识续接；接不上就作为新会话继续，并提示上下文没带给 AI（不把历史文本
   * 拼进提示词）。目标主机按当前标签重新同步，能力密钥是新的。
   */
  const startConversation = useCallback(async (forAgent?: AgentId, history?: HistoryRecord) => {
    const target = forAgent ?? activeAgent;
    setStarting(true);
    setStartError(null);
    // 新对话从干净状态开始：清掉上一条对话的消息、没写进去的文字和运行标记。
    dropPendingText();
    setItems(history ? fromHistoryMessages(history.messages, toolMetaOf) : []);
    historyIdRef.current = history?.id ?? null;
    conversationAgentRef.current = target;
    // 上一个对话切走时自动保留的会话不带进新对话；用户自己 @ 的照旧保留。
    knownTargetsRef.current.clear();
    trackedCallsRef.current = [];
    pendingReportsRef.current = [];
    reportsRef.current.clear();
    setPinned((prev) => (prev.some((p) => p.retained) ? prev.filter((p) => !p.retained) : prev));
    setContextLost(false);
    setApprovals([]);
    setRunning(false);
    setRetryNotice(null);
    setAiRunningInTerminal(false);
    try {
      const targetInstall = installs[target];
      const info = await aiAgentStart(target, targetInstall?.path ?? undefined, history?.id);
      if (history && !info.resumed) setContextLost(true);
      keyRef.current = info.session_key;
      scopeIdRef.current = info.scope_id;
      setSessionKey(info.session_key);
      scopeTargetsRef.current = [];
      try {
        const catalog = await aiAgentModels(info.session_key);
        storeCatalog(catalogCacheKey(target, targetInstall), catalog);
        applyCatalog(target, catalog);
      } catch (error) {
        console.warn("取模型失败", error);
      }
      toast.success(history ? "已打开历史会话" : "已开始新对话");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      // 启动被拒绝（比如 codex 的本机工具没法确认关掉）时留在面板里，配一个重试。
      setStartError(message);
      toast.error(message);
    } finally {
      setStarting(false);
    }
  }, [activeAgent, installs, applyCatalog]);

  // 每轮结束保存一次当前会话（R11）。没有用户消息就不存；失败只记日志，不打扰用户。
  const itemsRef = useRef(items);
  itemsRef.current = items;
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;
  useEffect(() => {
    if (saveTick === 0) return;
    const agent = conversationAgentRef.current;
    const current = itemsRef.current;
    if (!agent || !hasUserMessage(current)) return;
    const id = historyIdRef.current ?? newHistoryId();
    historyIdRef.current = id;
    void aiHistorySave(agent, keyRef.current, {
      id,
      host_label: historyHostLabel(sessionsRef.current),
      messages: toHistoryMessages(current),
    }).catch((error: unknown) => console.warn("保存 AI 历史失败", error));
  }, [saveTick]);

  /** 打开历史列表（当前智能体的）。 */
  const openHistory = useCallback(() => {
    void aiHistoryList(activeAgent)
      .then((list) => setHistoryList(list))
      .catch((error: unknown) => {
        console.warn("取 AI 历史失败", error);
        toast.error("取 AI 历史失败");
      });
  }, [activeAgent]);

  const removeHistory = useCallback(
    (id: string) => {
      void aiHistoryDelete(activeAgent, id)
        .then(() => {
          setHistoryList((prev) => prev?.filter((h) => h.id !== id) ?? null);
          // 删的是正在用的那条：之后再存就另起一条，不把删掉的写回来。
          if (historyIdRef.current === id) historyIdRef.current = null;
        })
        .catch((error: unknown) => toast.error(`删除失败：${String(error)}`));
    },
    [activeAgent],
  );

  const stopConversation = useCallback(async () => {
    const key = keyRef.current;
    if (!key) return;
    await aiAgentStop(key).catch(() => {});
    keyRef.current = null;
    scopeIdRef.current = null;
    scopeTargetsRef.current = [];
    trackedCallsRef.current = [];
    pendingReportsRef.current = [];
    setSessionKey(null);
    setApprovals([]);
    setAiRunningInTerminal(false);
    activityRef.current?.({ kind: "reset" });
  }, []);

  /** 点历史列表里的一条：结束当前对话，打开那条回看并尝试续接。 */
  const resumeHistory = useCallback(
    (id: string) => {
      const agent = activeAgent;
      setHistoryList(null);
      void aiHistoryLoad(agent, id)
        .then(async (record) => {
          await stopConversation();
          await startConversation(agent, record);
        })
        .catch((error: unknown) => toast.error(`打开历史会话失败：${String(error)}`));
    },
    [activeAgent, startConversation, stopConversation],
  );

  /** 切换智能体：已经开始的对话不能中途换（AI-3 第 6 节），换了就开一个新对话。 */
  const switchAgent = useCallback(
    (next: AgentId) => {
      if (next === activeAgent) return;
      setChosenAgent(next);
      // 旧智能体的模型目录和选中的模型不能带过去（下面的预取会换上新智能体的）。
      setCatalogState(null);
      setModel("");
      if (keyRef.current) {
        toast.info(`已切换到 ${AGENT_LABELS[next]}，开始新对话`);
        void stopConversation().then(() => startConversation(next));
      } else {
        toast.info(`已切换到 ${AGENT_LABELS[next]}`);
      }
    },
    [activeAgent, startConversation, stopConversation],
  );

  const follow = useStickToBottom();
  const { scrollToBottom } = follow;

  // 执行完的工具调用折成一行，等待批准的 / 正在执行的单独显示（AI-UX-1）
  const chatRows = useMemo(
    () =>
      groupChatItems(
        items,
        (call) => pairApproval(approvals, call, items) !== undefined,
      ),
    [items, approvals],
  );

  /** 已经放到某张工具卡片上的确认请求；其余的单独显示（带各自的主机名和命令）。 */
  const pairedApprovals = useMemo(() => {
    const paired = new Set<number>();
    for (const item of items) {
      if (item.kind !== "tool") continue;
      const hit = pairApproval(approvals, item.call, items);
      if (hit) paired.add(hit.reqId);
    }
    return paired;
  }, [items, approvals]);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || running || starting) return;
    let key = keyRef.current;
    if (!key) {
      // 刚打开面板还没有对话：发第一条消息时自动开一个，不用先点"+"。
      await startConversation();
      key = keyRef.current;
      if (!key) return; // 启动失败，startConversation 已经提示并给了重试
    }
    flushText();
    setItems((prev) => [...prev, { kind: "user", text }]);
    // 自己发的消息一定要看到，往上翻着也拉回底部并恢复跟随。
    scrollToBottom();
    setInput("");
    setRunning(true);
    setRetryNotice(null);
    setTurnStartedAt(Date.now());
    if (firstUse) onMarkUsed();
    try {
      await aiAgentSend(key, text, modelForSend(model));
    } catch (error) {
      setRunning(false);
      toast.error(error instanceof Error ? error.message : String(error));
    }
  }, [input, running, starting, model, firstUse, onMarkUsed, startConversation, scrollToBottom]);

  const abort = useCallback(async () => {
    const key = keyRef.current;
    if (!key) return;
    setAiRunningInTerminal(false);
    activityRef.current?.({ kind: "reset" });
    await aiAgentAbort(key).catch((error: unknown) =>
      console.warn("停止回合失败", error),
    );
  }, []);

  // 面板关掉：结束对话（摘掉会话、销毁 scope）。切标签不结束对话——「查看终端」要切过去，
  // 对话和它的作用范围得留着（AI-SCOPE-1）。
  useEffect(() => {
    return () => {
      const key = keyRef.current;
      keyRef.current = null;
      scopeIdRef.current = null;
      activityRef.current?.({ kind: "reset" });
      if (key) void aiAgentStop(key).catch(() => {});
    };
  }, []);

  // @ 选择：打开弹窗时拉主机库。
  const openPicker = useCallback(() => {
    setPickerQuery("");
    void aiHostPicker()
      .then((hosts) => setPicker(hosts))
      .catch((error: unknown) => {
        console.warn("取主机列表失败", error);
        toast.error("取主机列表失败");
      });
  }, []);

  /** 当前焦点终端的会话属于哪台主机（判断 @ 的是不是"当前"那台）。 */
  const currentHostId = sessions[0]?.hostId ?? null;
  const addPinned = useCallback((host: HostPickerEntry) => {
    setPicker(null);
    // 同一台主机只算一个目标（AI-SCOPE-1 第 1 节）：已经在对话里的（当前、本标签其他窗格、
    // 之前 @ 过的）不新增标签；当前标签里有它的会话时记一个固定标记，让切走焦点后它仍留在范围里。
    const result = pinHost(
      pinnedRef.current,
      { hostId: host.host_id, name: host.name, username: host.username },
      sessionsRef.current,
    );
    if (result.pinned !== pinnedRef.current) setPinned(result.pinned);
    if (result.alreadyIn) toast.info(`${host.name} 已经在对话里了`);
  }, []);

  const removePinned = useCallback((key: string) => {
    setPinned((prev) => prev.filter((p) => pinKey(p) !== key));
  }, []);

  const hasSessions = sessions.length > 0;
  const install = installs[activeAgent];
  const login = logins[activeAgent];
  const notFound = install === null;
  const tooOld = install?.too_old === true;
  /** codex / claude 没登录时不可用（AI-3 第 5 节 / AI-4 第 4 节）。 */
  const loginRequired = activeAgent !== "opencode" && login?.logged_in === false;
  /**
   * 登录状态判断不了（用户的配置文件读不出来）：只提示，**不拦着用**（R-2 第 3 节）——
   * 读不出来是我们的读法受限，不代表他没登录（他自己的 CLI 可能早就登录好了）。
   */
  const loginUnknown =
    activeAgent !== "opencode" && login?.logged_in === null && Boolean(login?.hint);
  /** 没装好 / 没登录时禁用模型下拉框、输入框和发送（AI-2c 0.2）。 */
  const disabled = notFound || install === undefined || loginRequired;

  // 面板一打开 / 切换智能体就取模型目录（不用先点"+"）：按智能体 + 安装缓存，
  // 同一个安装只取一次；智能体还不能用（没装、版本太低、没登录）时不取。
  const catalogKey = catalogCacheKey(activeAgent, install);
  const catalogReady = !disabled && !tooOld && catalogKey !== null;
  useEffect(() => {
    if (!catalogReady || !catalogKey) return;
    let alive = true;
    const agent = activeAgent;
    const hit = cachedCatalog(catalogKey);
    if (hit) {
      applyCatalog(agent, hit);
      return;
    }
    void loadCatalog(catalogKey, () => aiAgentCatalog(agent, install?.path ?? undefined))
      .then((catalog) => {
        if (alive) applyCatalog(agent, catalog);
      })
      .catch((error: unknown) => console.warn("预取模型目录失败", error));
    return () => {
      alive = false;
    };
    // install 的变化已经体现在 catalogKey 里（路径 + 版本），不单独列依赖。
  }, [catalogReady, catalogKey, activeAgent, applyCatalog]);

  const catalog = catalogState?.agent === activeAgent ? catalogState.catalog : null;
  const modelGroups = useMemo(() => buildModelPicker(catalog).groups, [catalog]);
  const selection = splitSelection(model, modelGroups);
  const selectedGroup = selection?.group ?? modelGroups[0] ?? null;
  const selectedEffort = selection?.effort ?? null;
  const efforts = selectedGroup ? effortChoices(selectedGroup) : [];
  /** 中间框换模型：强度能保持就保持（用户最后选的那个），不支持就换成这个模型能用的。 */
  const chooseModel = (value: string) => {
    const group = modelGroups.find((g) => g.value === value);
    if (!group) return;
    setModel(joinSelection(group, pickEffort(group, effortPrefRef.current ?? selectedEffort)));
  };
  /** 右边框换强度。"默认强度"在下拉框里用一个占位值表示（Radix 的 Select 不收空串）。 */
  const chooseEffort = (value: string) => {
    if (!selectedGroup) return;
    const effort = value === DEFAULT_EFFORT_VALUE ? null : value;
    effortPrefRef.current = effort;
    setModel(joinSelection(selectedGroup, effort));
  };

  /** 这个智能体现在能不能用（选择器里给不可用的标注原因）。 */
  const agentAvailability = useCallback(
    (id: AgentId) =>
      agentAvailabilityOf(
        installs[id],
        id === "opencode" ? undefined : logins[id],
      ),
    [installs, logins],
  );

  /** 回答一条确认请求（工具卡片上的"执行 / 拒绝"）。稳定引用，卡片才能跳过重画。 */
  const answerApproval = useCallback((approval: PendingApproval, allow: boolean) => {
    setApprovals((prev) => prev.filter((a) => a.reqId !== approval.reqId));
    void aiApprovalAnswer(approval.reqId, allow).catch((error: unknown) =>
      console.warn("回答确认请求失败", error),
    );
  }, []);

  // 目标 id（会话 id / host-<id>）→ 主机显示名（工具卡片上标出跑在哪台服务器）。
  // 对话里出现过的都在，切走后关掉的会话也还认得。
  const hostLabels = useMemo(() => {
    const map: Record<string, string> = {};
    for (const [id, target] of knownTargetsRef.current) map[id] = target.label;
    return map;
    // knownTargetsRef 的内容随 sessions / pinned 登记，这两个变了才需要重算
  }, [sessions, pinned]);
  /** 对话里有不止一个目标：卡片上要标明在哪台主机执行。 */
  const showTarget = scopeTargets.length > 1;

  const filteredPicker = useMemo(() => {
    const q = pickerQuery.trim().toLowerCase();
    const all = picker ?? [];
    return q ? all.filter((h) => h.name.toLowerCase().includes(q)) : all;
  }, [picker, pickerQuery]);

  const content = useMemo(() => {
    if (notFound) {
      return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
          <Bot size={32} className="text-muted-foreground" />
          <p className="text-sm text-muted-foreground">
            {activeAgent === "opencode"
              ? "没有找到 opencode。去设置页指定路径，或者一键下载。"
              : activeAgent === "codex"
                ? "没有找到 codex。安装方法：npm i -g @openai/codex（也可以在设置页指定路径）。"
                : "没有找到 Claude Code。安装方法：npm i -g @anthropic-ai/claude-code（也可以在设置页指定路径）。"}
          </p>
          <Button size="sm" onClick={onOpenSettings}>
            去设置
          </Button>
        </div>
      );
    }
    if (loginRequired) {
      return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
          <Bot size={32} className="text-muted-foreground" />
          <p className="text-sm text-muted-foreground">
            {activeAgent === "codex" ? "codex 还没有登录。" : "Claude Code 还没有登录。"}
          </p>
          <p className="font-mono text-xs text-muted-foreground">
            {activeAgent === "codex"
              ? "请先在终端里运行 `codex login`"
              : "请先在终端里运行 `claude` 完成登录"}
          </p>
          {login?.hint && (
            <p className="text-xs text-amber-600">{login.hint}</p>
          )}
        </div>
      );
    }
    if (items.length === 0) {
      return (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
          <Bot size={28} className="text-muted-foreground" />
          <p className="text-xs text-muted-foreground">
            用自然语言让 AI 在当前标签的服务器上排查问题；输入框里打 @ 可以把别的主机
            也加进来。
          </p>
        </div>
      );
    }
    return null;
  }, [notFound, loginRequired, login?.hint, activeAgent, items.length, onOpenSettings]);

  return (
    <div
      ref={rootRef}
      data-dock-panel="ai"
      className="relative flex h-full shrink-0 flex-col border-l border-border/60 bg-background"
      style={{ width: panelWidth }}
    >
      {/* 左边缘的拖拽把手（UI-1 第 2 节） */}
      <ResizeHandle
        width={panelWidth}
        bounds={panelBounds}
        onResize={onPanelResize}
        onCommit={onPanelCommit}
        onReset={onPanelReset}
      />
      {/* 顶部：智能体 + 模型 + 手动/自动 + 新对话 */}
      <div className="shrink-0 space-y-2 border-b border-border/60 px-3 py-2.5">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Bot size={16} />
            <span className="text-sm font-semibold">AI 助手</span>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              title="历史会话"
              data-action="open-history"
              disabled={starting}
              onClick={openHistory}
            >
              <History size={16} />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              title="新对话"
              disabled={starting || disabled}
              onClick={() => void startConversation()}
            >
              <Plus size={16} />
            </Button>
            {/* 关闭面板（不是删除对话：以前用的垃圾桶图标让人误会） */}
            <Button variant="ghost" size="icon" title="关闭" aria-label="关闭 AI 面板" onClick={onClose}>
              <X size={16} />
            </Button>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {/* 智能体选择（AI-3）：每个对话固定一个，开着的对话换要新开。 */}
          <Select value={activeAgent} onValueChange={(value) => switchAgent(value as AgentId)}>
            <SelectTrigger
              className="h-8 w-[124px] shrink-0 text-xs"
              data-section="agent-select"
            >
              {/* 收起时只显示名字（列表里才带"未安装/未登录/版本过低"的标注）：
                  带标注的"Claude Code版本过低"在 320px 下会被截断。 */}
              <SelectValue placeholder="选择智能体">{AGENT_LABELS[activeAgent]}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {(Object.keys(AGENT_LABELS) as AgentId[]).map((id) => {
                const availability = agentAvailability(id);
                const note =
                  availability === "missing"
                    ? "未安装"
                    : availability === "login"
                      ? "未登录"
                      : availability === "old"
                        ? "版本过低"
                        : null;
                return (
                  <SelectItem key={id} value={id} className="text-xs" disabled={note !== null}>
                    <span className="flex items-center gap-1.5">
                      {AGENT_LABELS[id]}
                      {note && (
                        <span className="text-[10px] text-muted-foreground">{note}</span>
                      )}
                    </span>
                  </SelectItem>
                );
              })}
            </SelectContent>
          </Select>
          <Select
            value={selectedGroup?.value ?? ""}
            onValueChange={chooseModel}
            disabled={disabled}
          >
            <SelectTrigger className="h-8 min-w-0 flex-1 text-xs" data-section="model-select">
              {/* 选中"默认"时显示"默认（实际模型）"；读不到实际模型就是"默认模型"。 */}
              <SelectValue placeholder="默认模型">{selectedGroup?.label ?? "默认模型"}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {modelGroups.map((g) => (
                <SelectItem key={g.value} value={g.value} className="text-xs">
                  <span className="flex items-center gap-1.5">
                    {g.label}
                    {g.unavailable && (
                      <Badge variant="secondary" className="px-1 py-0 text-[10px]">
                        不可用
                      </Badge>
                    )}
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {/* 思考强度：只列当前模型支持的；模型不支持强度（opencode、Haiku 4.5）就不显示 */}
          {efforts.length > 1 && (
            <Select
              value={selectedEffort ?? DEFAULT_EFFORT_VALUE}
              onValueChange={chooseEffort}
              disabled={disabled}
            >
              <SelectTrigger
                className="h-8 w-[96px] shrink-0 text-xs"
                data-section="effort-select"
                title="思考强度"
              >
                <SelectValue>{effortLabel(selectedEffort)}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {efforts.map((effort) => (
                  <SelectItem
                    key={effort ?? DEFAULT_EFFORT_VALUE}
                    value={effort ?? DEFAULT_EFFORT_VALUE}
                    className="text-xs"
                  >
                    {effortLabel(effort)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </div>
        {/* 执行模式（R-2 0.1）：单独一行，320px 宽也不挤（开关原来和两个下拉框挤在一起）。 */}
        <div
          className="flex items-center justify-between gap-2"
          data-section="exec-mode"
        >
          <span className="text-xs text-muted-foreground">执行模式</span>
          {/* 三档排成一排（以前是下拉框）；全部放行那档选中时是红的，不再另起一行红字 */}
          <div
            role="radiogroup"
            aria-label="执行模式"
            data-section="panel-exec-mode"
            className="flex shrink-0 rounded-md border border-border p-0.5"
          >
            {EXEC_MODE_OPTIONS.map((item) => {
              const selected = item.value === toExecMode(execMode);
              return (
                <button
                  key={item.value}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  title={item.hint}
                  // 和设置页共用 store：写进去两边立刻都变（后端也在 update 里落库）。
                  onClick={() => void updateSettings("ai_exec_mode", item.value)}
                  className={cn(
                    "whitespace-nowrap rounded px-2 py-0.5 text-[11px] transition-colors",
                    selected
                      ? item.value === "auto"
                        ? "bg-destructive text-destructive-foreground"
                        : "bg-primary text-primary-foreground"
                      : "text-muted-foreground hover:bg-accent hover:text-foreground",
                  )}
                >
                  {item.label}
                </button>
              );
            })}
          </div>
        </div>
        {tooOld && (
          <div className="flex items-center justify-between gap-2 rounded-md bg-amber-500/10 px-2 py-1 text-xs text-amber-600">
            <span className="flex items-center gap-1.5">
              <AlertTriangle size={12} />
              版本过低（{install?.version}），建议升级
            </span>
            <Button
              size="sm"
              variant="outline"
              className="h-6 px-2 text-[11px]"
              onClick={onOpenSettings}
            >
              {activeAgent === "opencode" ? "下载" : "去设置"}
            </Button>
          </div>
        )}
        {loginUnknown && (
          <div
            data-section="login-unknown"
            className="flex items-start gap-1.5 rounded-md bg-amber-500/10 px-2 py-1 text-xs text-amber-600"
          >
            <AlertTriangle size={12} className="mt-0.5 shrink-0" />
            <span>{login?.hint}</span>
          </div>
        )}
      </div>

      {/* 启动失败（比如本机工具禁用没法确认）：一直显示到重试成功（UI-1 0.1）。 */}
      {startError && (
        <div
          data-section="start-error"
          className="flex shrink-0 items-start justify-between gap-2 border-b border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive"
        >
          <span className="flex items-start gap-1.5">
            <AlertTriangle size={12} className="mt-0.5 shrink-0" />
            <span>{startError}</span>
          </span>
          <Button
            size="sm"
            variant="outline"
            className="h-6 shrink-0 px-2 text-[11px]"
            disabled={starting || disabled}
            onClick={() => void startConversation()}
          >
            重试
          </Button>
        </div>
      )}

      {/* 消息区 */}
      <div className="relative flex min-h-0 flex-1 flex-col">
      <ScrollArea ref={follow.rootRef} className="min-h-0 flex-1">
        <div ref={follow.contentRef} className="space-y-3 p-3">
          {contextLost && (
            <div
              data-section="context-lost"
              className="flex items-start gap-1.5 rounded-md bg-amber-500/10 px-2 py-1 text-xs text-amber-600"
            >
              <AlertTriangle size={12} className="mt-0.5 shrink-0" />
              <span>{CONTEXT_LOST_NOTICE}</span>
            </div>
          )}
          {content}
          {chatRows.map((row) => {
            if (row.type === "text") {
              return <TextMessage key={row.index} kind={row.item.kind} text={row.item.text} />;
            }
            if (row.type === "progress") {
              return <ToolProgressRow key={row.call.callId} call={row.call} />;
            }
            if (row.type === "group") {
              return (
                <ToolGroup
                  key={`group-${row.calls[0].callId}`}
                  calls={row.calls}
                  hosts={hostLabels}
                  showTarget={showTarget}
                  onViewTarget={onViewTarget}
                />
              );
            }
            // 等待批准的调用：完整卡片不折叠，卡片上带"执行 / 拒绝"按钮。
            // 一条命令只显示一张卡片。
            const approval = pairApproval(approvals, row.call, items);
            return (
              <ToolMessage
                key={row.call.callId}
                call={row.call}
                hosts={hostLabels}
                showTarget={showTarget}
                approval={approval}
                onViewTarget={onViewTarget}
                onAnswerApproval={answerApproval}
              />
            );
          })}
          {/* 重试期间一定要有提示：哪怕最后一条已经是 AI 的文字（showThinking 为假）。 */}
          {(showThinking(running, items, approvals.length > 0) || (running && retryNotice)) && (
            <ThinkingIndicator since={turnStartedAt} retry={running ? retryNotice : null} />
          )}
          {/* 没有配到工具卡片的审批（工具卡片还没到，或者同样的命令在几处同时执行、对不唯一）：
              单独显示，别丢掉；它按自己的编号回答，不会批准到别的请求上。 */}
          {approvals
            .filter((a) => !pairedApprovals.has(a.reqId))
            .map((approval) => (
              <ToolCard
                key={approval.reqId}
                call={{
                  callId: `approval-${approval.reqId}`,
                  tool: approval.tool === "write_file" ? "zssh_write_file" : "zssh_run_command",
                  input:
                    approval.tool === "write_file"
                      ? { path: approval.filePath ?? approval.command }
                      : { command: approval.command },
                  output: "",
                  status: "running",
                }}
                hosts={hostLabels}
                showTarget={showTarget}
                approval={approval}
                onViewTarget={onViewTarget}
                onAnswer={(allow) => {
                  setApprovals((prev) => prev.filter((a) => a.reqId !== approval.reqId));
                  void aiApprovalAnswer(approval.reqId, allow).catch((error: unknown) =>
                    console.warn("回答确认请求失败", error),
                  );
                }}
              />
            ))}
        </div>
      </ScrollArea>
      {!follow.sticking && items.length > 0 && (
        <Button
          size="sm"
          variant="secondary"
          data-section="jump-to-latest"
          className="absolute bottom-2 right-3 h-7 gap-1 px-2 text-xs shadow"
          onClick={scrollToBottom}
        >
          <ArrowDown size={12} />
          回到最新
        </Button>
      )}
      </div>

      {/* 输入框（上沿的拖动条调高度） */}
      <div className="relative shrink-0 border-t border-border/60 p-2.5">
        <InputResizeHandle
          height={inputSize.height}
          panelHeight={inputSize.panelHeight}
          onResize={inputSize.onResize}
          onCommit={inputSize.onCommit}
          onReset={inputSize.onReset}
        />
        {/* 目标标签：当前焦点那台（不可移除）+ @ 进来的主机（可移除） */}
        <div className="mb-1.5 flex flex-wrap items-center gap-1">
          {sessions.slice(0, 1).map((s) => (
            <span
              key={s.sessionId}
              className="flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] text-emerald-600"
              title="当前焦点终端"
            >
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
              {s.hostLabel}（当前）
            </span>
          ))}
          {pinned
            .filter((pin) => !pinIsCurrent(pin, currentSession ?? undefined))
            .map((pin) => {
              const state: HostConnectionState =
                pin.hostId !== null ? hostStateOf(hostStates, pin.hostId) : "connected";
              return (
                <span
                  key={pinKey(pin)}
                  data-section="pinned-target"
                  data-retained={pin.retained ? "true" : undefined}
                  className="flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[11px]"
                  title={
                    pin.retained
                      ? "切走前的终端，已保留在本次对话里"
                      : hostDotTitle(state)
                  }
                >
                  <span className={`h-1.5 w-1.5 rounded-full ${hostDotClass(state)}`} />
                  {pin.name}
                  <button
                    onClick={() => removePinned(pinKey(pin))}
                    className="text-muted-foreground hover:text-foreground"
                    title="移除"
                  >
                    <X size={11} />
                  </button>
                </span>
              );
            })}
        </div>
        {!hasSessions && pinned.length === 0 && (
          <p className="mb-1.5 text-xs text-muted-foreground">
            这个标签还没有连上的会话；也可以打 @ 加一台主机。
          </p>
        )}
        {aiRunningInTerminal && (
          <p className="mb-1.5 text-[11px] text-amber-600">AI 正在终端里执行命令…</p>
        )}
        {/* 输入框占满整行，发送 / 停止按钮放在框内右下角；右下留出按钮的位置，文字不被挡住 */}
        <div className="relative">
          {/* data-ime="native"：输入法跟随你自己的中英文状态（只有终端里切英文，见 terminal/imeFocus.ts） */}
          <Textarea
            data-ime="native"
            value={input}
            onChange={(event) => {
              const value = event.target.value;
              setInput(value);
              // 打 @ 弹出主机选择。
              if (value.endsWith("@") && !picker) openPicker();
            }}
            onKeyDown={(event) => {
              // 输入法组合期间（选候选字、翻页）的 Enter 是选字，不是发送（同撰写栏）
              if (event.nativeEvent.isComposing || event.keyCode === 229) return;
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
            disabled={disabled}
            placeholder="让 AI 在服务器上做什么…（Enter 发送，打 @ 加主机）"
            style={{ height: inputSize.height }}
            className="min-h-0 w-full resize-none pr-12 text-[11.5px] leading-relaxed"
          />
          {running ? (
            <Button
              size="icon"
              variant="destructive"
              title="停止"
              className="absolute bottom-1.5 right-1.5 h-8 w-8"
              onClick={() => void abort()}
            >
              <Square size={16} />
            </Button>
          ) : (
            <Button
              size="icon"
              title="发送"
              className="absolute bottom-1.5 right-1.5 h-8 w-8"
              disabled={disabled || starting || (pinned.length === 0 && !hasSessions) || input.trim() === ""}
              onClick={() => void send()}
            >
              <Play size={16} />
            </Button>
          )}
        </div>
        {sessionKey && (
          <button
            onClick={() => void stopConversation()}
            className="mt-1.5 text-xs text-muted-foreground hover:text-foreground"
          >
            结束这个对话
          </button>
        )}
      </div>

      {/* 历史会话列表（R11）：当前智能体的，点一条打开回看，每条可删 */}
      {historyList !== null && (
        <div
          data-section="history-list"
          className="absolute inset-0 z-20 flex items-start justify-center bg-background/70 p-3"
        >
          <div className="mt-8 w-full max-w-[340px] rounded-lg border border-border bg-background shadow-lg">
            <div className="border-b border-border/60 px-3 py-2 text-xs font-medium">
              {AGENT_LABELS[activeAgent]} 的历史会话
            </div>
            <div className="max-h-80 overflow-auto p-1">
              {historyList.length === 0 && (
                <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                  还没有历史会话
                </p>
              )}
              {historyList.map((entry) => (
                <div
                  key={entry.id}
                  data-history-id={entry.id}
                  className="flex items-center gap-1 rounded-md hover:bg-muted"
                >
                  <button
                    onClick={() => resumeHistory(entry.id)}
                    className="min-w-0 flex-1 px-2 py-1.5 text-left text-xs"
                  >
                    <span className="block truncate">{entry.title}</span>
                    <span className="block truncate text-[11px] text-muted-foreground">
                      {formatHistoryTime(entry.updated_at)}
                      {entry.host_label ? ` · ${entry.host_label}` : ""}
                    </span>
                  </button>
                  <button
                    title="删除"
                    data-action="delete-history"
                    onClick={() => removeHistory(entry.id)}
                    className="shrink-0 p-1.5 text-muted-foreground hover:text-destructive"
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              ))}
            </div>
            <div className="border-t border-border/60 p-2 text-right">
              <Button variant="ghost" size="sm" onClick={() => setHistoryList(null)}>
                关闭
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* @ 主机选择弹窗 */}
      {picker !== null && (
        <div className="absolute inset-0 z-20 flex items-start justify-center bg-background/70 p-3">
          <div className="mt-8 w-full max-w-[320px] rounded-lg border border-border bg-background shadow-lg">
            <div className="border-b border-border/60 p-2">
              <input
                autoFocus
                value={pickerQuery}
                onChange={(event) => setPickerQuery(event.target.value)}
                placeholder="搜索主机…"
                className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-xs outline-none"
              />
            </div>
            <div className="max-h-64 overflow-auto p-1">
              {filteredPicker.length === 0 && (
                <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                  没有匹配的主机
                </p>
              )}
              {filteredPicker.map((host) => {
                const isCurrent = host.host_id === currentHostId;
                return (
                  <button
                    key={host.host_id}
                    onClick={() => addPinned(host)}
                    className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-left text-xs hover:bg-muted"
                  >
                    <span>
                      {host.name}
                      {isCurrent && <span className="text-muted-foreground">（当前）</span>}
                    </span>
                    <span className="text-[11px] text-muted-foreground">{host.username}</span>
                  </button>
                );
              })}
            </div>
            <div className="border-t border-border/60 p-2 text-right">
              <Button variant="ghost" size="sm" onClick={() => setPicker(null)}>
                取消
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * 消息列表的一行。AI 输出时最后一条每帧都在变，前面的消息内容没变就不重画
 * （长对话里每帧重画全部历史消息很费）。文字、工具状态、待确认变了照常更新。
 */
const TextMessage = memo(function TextMessage({
  kind,
  text,
}: {
  kind: "user" | "assistant";
  text: string;
}) {
  if (kind === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground">
          {text}
        </div>
      </div>
    );
  }
  return <AssistantMessage text={text} />;
});

const ToolMessage = memo(function ToolMessage({
  approval,
  onAnswerApproval,
  ...rest
}: Omit<React.ComponentProps<typeof ToolCard>, "onAnswer"> & {
  onAnswerApproval: (approval: PendingApproval, allow: boolean) => void;
}) {
  return (
    <ToolCard
      {...rest}
      approval={approval}
      onAnswer={(allow) => {
        if (approval) onAnswerApproval(approval, allow);
      }}
    />
  );
});

/** 正在执行的工具调用：只显示一行进行中，执行完并入折叠行。 */
function ToolProgressRow({ call }: { call: ToolCall }) {
  return (
    <div
      data-section="tool-progress"
      className="flex items-center gap-1.5 text-[11px] text-muted-foreground"
    >
      <Spinner size={11} />
      <span className="min-w-0 break-all">{progressText(call)}</span>
    </div>
  );
}

const noopAnswer = () => {};

/**
 * 执行完的一组工具调用：一行淡色小字，默认收起，点开显示完整卡片。
 * 有失败的这一行用警示色并写明几条失败，仍然默认收起。
 */
const ToolGroup = memo(
  function ToolGroup({
    calls,
    hosts,
    showTarget,
    onViewTarget,
  }: {
    calls: ToolCall[];
    hosts: Record<string, string>;
    showTarget: boolean;
    onViewTarget?: (target: TargetRef) => void;
  }) {
    const [open, setOpen] = useState(false);
    const failed = calls.filter(callFailed).length;
    return (
      <div data-section="tool-group" data-failed={failed > 0 ? "true" : undefined}>
        <button
          type="button"
          data-action="toggle-tool-group"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
          className={cn(
            "flex items-center gap-1 text-left text-[11px]",
            failed > 0
              ? "text-amber-600 hover:text-amber-700 dark:text-amber-400"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <ChevronRight size={12} className={cn("shrink-0 transition-transform", open && "rotate-90")} />
          <span>
            {summarizeCalls(calls)}
            {failed > 0 ? `（其中 ${failed} 条失败）` : ""}
          </span>
        </button>
        {open && (
          <div className="mt-1.5 space-y-2">
            {calls.map((call) => (
              <ToolCard
                key={call.callId}
                call={call}
                hosts={hosts}
                showTarget={showTarget}
                onViewTarget={onViewTarget}
                onAnswer={noopAnswer}
              />
            ))}
          </div>
        )}
      </div>
    );
  },
  // calls 每次渲染都是新数组，但里面的调用对象没变就不重画（流式输出时每帧都会渲染）
  (prev, next) =>
    prev.hosts === next.hosts &&
    prev.showTarget === next.showTarget &&
    prev.onViewTarget === next.onViewTarget &&
    prev.calls.length === next.calls.length &&
    prev.calls.every((call, index) => call === next.calls[index]),
);

/** 工具调用卡片：友好名称、主机名、命令全文、退出码、执行位置、状态、可折叠输出。 */
export function ToolCard({
  call,
  hosts,
  showTarget = false,
  approval,
  onViewTarget,
  onAnswer,
}: {
  call: ToolCall;
  hosts: Record<string, string>;
  /** 对话里有不止一个目标：必须标明在哪台主机执行。 */
  showTarget?: boolean;
  approval?: PendingApproval;
  onViewTarget?: (target: TargetRef) => void;
  onAnswer: (allow: boolean) => void;
}) {
  const rawCommand = commandOf(call.input);
  // 审批卡片第二道防线：命令里的不可见控制字符显示成可见的转义形式（R-8 第 1 节）。
  const command = rawCommand === null ? null : visibleCommandText(rawCommand);
  const filePath = isFileTool(call.tool) ? pathOf(call.input) : null;
  // 写系统路径标红：确认前看确认请求的标记，写完看结果里的标记（自动模式没有确认卡片）。
  const readable = call.output ? readableOutput(call.output) : "";
  const fileDanger = fileDangerReason(call.output);
  const backup = backupPathOf(call.output);
  const ownerChange = ownerChangeOf(call.output);
  const danger =
    isDangerous(call.input) || approval?.danger === "dangerous" || fileDanger !== null;
  const [diffExpanded, setDiffExpanded] = useState(false);
  const host = hostOf(call.input, hosts);
  const exitCode = call.status === "done" ? exitCodeOf(call.output) : null;
  // 执行所在的主机：用执行层回报的实际执行位置（审批事件带的是同一份）。目标多于一个、或者
  // 当时没有"当前"目标时必须显示主机名（只有一个当前目标时省略，避免啰嗦）。
  const exec = call.exec ?? approval?.exec ?? undefined;
  const targetLabel = exec && (showTarget || exec.multi) ? exec.label : null;
  const hostName = targetLabel ?? host;
  // 调用结束了、执行层却一直没回报：说明目标没能解析出来（比如对话里没有可用目标），
  // 不能静默省略，写明"未确定"。
  const targetUnknown = !exec && call.tracked === true && call.status !== "running";
  return (
    <div
      className={cn(
        "rounded-lg border px-2.5 py-2 text-xs",
        danger ? "border-destructive/60 bg-destructive/5" : "border-border/60 bg-muted/30",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5">
          <span className="font-medium">{friendlyToolName(call.tool)}</span>
          {hostName && (
            <span
              data-section="call-target"
              className="font-mono text-[11px] text-muted-foreground"
            >
              {hostName}
            </span>
          )}
          {targetUnknown && (
            <span data-section="call-target-unknown" className="text-[11px] text-amber-600">
              执行目标未确定
            </span>
          )}
          {exec && onViewTarget && (
            <button
              type="button"
              data-action="view-terminal"
              className="text-[11px] text-muted-foreground underline decoration-dotted hover:text-foreground"
              title="切到这台主机的终端标签（还没连上、正在连也可以，能看到连接过程）"
              onClick={() =>
                onViewTarget({ targetId: exec.targetId, sessionId: exec.sessionId })
              }
            >
              查看终端
            </button>
          )}
        </span>
        <span className="flex items-center gap-1">
          {danger && (
            <Badge variant="destructive" className="px-1 py-0 text-[10px]">
              危险
            </Badge>
          )}
          {call.status === "running" ? (
            <Spinner size={12} />
          ) : (
            <Badge
              variant={
                call.status === "error" || call.status === "rejected"
                  ? "destructive"
                  : "secondary"
              }
              className="px-1 py-0 text-[10px]"
            >
              {call.status === "error"
                ? "失败"
                : call.status === "rejected"
                  ? "已拒绝"
                  : call.status === "stopped"
                    ? "已停止"
                    : exitCode !== null
                      ? `退出码 ${exitCode}`
                      : "完成"}
            </Badge>
          )}
        </span>
      </div>
      {command && (
        <pre className="mt-1.5 whitespace-pre-wrap break-all font-mono text-[11px] text-foreground">
          {command}
        </pre>
      )}
      {filePath && (
        <pre
          data-section="file-path"
          className="mt-1.5 whitespace-pre-wrap break-all font-mono text-[11px] text-foreground"
        >
          {approval?.filePath ?? filePath}
        </pre>
      )}
      {bareToolName(call.tool) === "write_file" && fileDanger && (
        <p data-section="file-danger" className="mt-1 text-[11px] text-destructive">
          写入系统路径：{fileDanger}
        </p>
      )}
      {backup && (
        <p className="mt-1 text-[11px] text-muted-foreground">原文件已备份到服务器：{backup}</p>
      )}
      {ownerChange && (
        <p data-section="owner-change" className="mt-1 text-[11px] text-destructive">
          属主从 {ownerChange.from} 变成了 {ownerChange.to}（权限不够，没能改回去）
        </p>
      )}
      {/* 执行位置（AI-2c，AI-2e 3.2：前缀不写死"终端忙"，原因在括号里）。 */}
      {call.location === "background" && (
        <p className="mt-1 text-[11px] text-muted-foreground">
          已在后台执行{call.fallbackReason ? `（${call.fallbackReason}）` : ""}
        </p>
      )}
      {call.location === "terminal" && (
        <p className="mt-1 text-[11px] text-muted-foreground">已在终端里执行</p>
      )}
      {call.output && (
        <details className="mt-1.5">
          <summary className="cursor-pointer text-[11px] text-muted-foreground">输出</summary>
          <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] text-muted-foreground">
            {readable}
          </pre>
        </details>
      )}
      {/* 手动模式：确认按钮就在这张卡片上（不另开一张卡片，AI-2c 0.2）。 */}
      {approval && (
        <div className="mt-2 space-y-1.5">
          {approval.reasons.length > 0 && (
            <p data-section="approval-reasons" className="text-[11px] font-medium text-destructive">
              {approval.reasons.join("；")}
            </p>
          )}
          {approval.file && (
            <div data-section="file-diff" className="rounded border border-border/60 bg-background/60">
              <div className="flex items-center justify-between border-b border-border/60 px-2 py-1 text-[11px] text-muted-foreground">
                <span>{diffSummary(approval.file.diff)}</span>
                <span>{approval.file.is_new ? "新建" : "覆盖前自动备份"}</span>
              </div>
              {(() => {
                const { visible, hidden } = foldDiff(approval.file.diff.lines, diffExpanded);
                return (
                  <>
                    <pre className="max-h-64 overflow-auto px-2 py-1 font-mono text-[11px] leading-snug">
                      {visible.map((line, index) => (
                        <div
                          key={index}
                          data-diff-kind={line.kind}
                          className={cn(
                            "whitespace-pre-wrap break-all",
                            line.kind === "add" && "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
                            line.kind === "del" && "bg-destructive/15 text-destructive",
                            line.kind === "hunk" && "text-muted-foreground",
                          )}
                        >
                          {line.kind === "add" ? "+ " : line.kind === "del" ? "- " : line.kind === "hunk" ? "" : "  "}
                          {line.text}
                        </div>
                      ))}
                    </pre>
                    {hidden > 0 && (
                      <button
                        data-action="expand-diff"
                        className="w-full border-t border-border/60 px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground"
                        onClick={() => setDiffExpanded(true)}
                      >
                        还有 {hidden} 行，点击展开
                      </button>
                    )}
                    {diffExpanded && approval.file.diff.lines.length > 30 && (
                      <button
                        className="w-full border-t border-border/60 px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground"
                        onClick={() => setDiffExpanded(false)}
                      >
                        收起
                      </button>
                    )}
                    {approval.file.diff.omitted_lines > 0 && (
                      <p className="border-t border-border/60 px-2 py-1 text-[11px] text-muted-foreground">
                        差异太长，后面还有 {approval.file.diff.omitted_lines} 行没有显示
                      </p>
                    )}
                  </>
                );
              })()}
            </div>
          )}
          <div className="flex gap-2">
            <Button size="sm" className="h-7 text-xs" onClick={() => onAnswer(true)}>
              执行
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-xs"
              onClick={() => onAnswer(false)}
            >
              拒绝
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

/** 工具结果里被拒绝的文案（AI-1 的拒绝错误文本）。 */
function isRejection(output: string): boolean {
  return /用户拒绝|用户取消连接|审批超时/.test(output);
}

/** 是不是执行命令的工具（用来点亮"AI 正在终端里执行"）。 */
function isRunCommand(tool: string): boolean {
  return /run_command$/.test(tool || "");
}

/** 回看历史时从结果文本里还原执行位置（和实时卡片同一套解析）。 */
function toolMetaOf(output: string): Pick<ToolCall, "location" | "fallbackReason"> {
  return { location: locationOf(output), fallbackReason: fallbackReasonOf(output) };
}

/** 从工具结果文本里取执行位置。 */
function locationOf(output: string): "terminal" | "background" | undefined {
  const match = output.match(/location:\s*(terminal|background)/i);
  return (match?.[1] as "terminal" | "background" | undefined) ?? undefined;
}

/** 从工具结果文本里取降级原因。 */
function fallbackReasonOf(output: string): string | undefined {
  const match = output.match(/ran in the background because:\s*(.+)/i);
  return match?.[1]?.trim() || undefined;
}
