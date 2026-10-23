//! 应用外壳（布局参照 Netcatty：顶部标签栏 + 主机页 + 终端区 + 右侧滑出面板）。
//!
//! 终端、分屏、快捷键这些行为保持 M1-06 的样子，只换了外观和组织方式。

import { useShallow } from "zustand/react/shallow";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import VaultView, { HostTreeAside, type HostActions } from "./components/vault/VaultView";
import HostDetailsPanel from "./components/vault/HostDetailsPanel";
import KeychainPanel from "./components/vault/KeychainPanel";
import ProxyPanel from "./components/vault/ProxyPanel";
import PortForwardPanel from "./components/vault/PortForwardPanel";
import TopTabs from "./components/titlebar/TopTabs";
import AuthDialog from "./components/dialogs/AuthDialog";
import GroupFormDialog from "./components/dialogs/GroupFormDialog";
import HostKeyDialog from "./components/dialogs/HostKeyDialog";
import QuickConnectDialog from "./components/dialogs/QuickConnectDialog";
import {
  currentPrompt,
  enqueuePrompt,
  removePrompt,
  type PendingPrompt,
} from "./components/dialogs/promptQueue";
import PaneView from "./components/PaneView";
import SettingsPage from "./components/settings/SettingsPage";
import SftpHostBar from "./components/sftp/SftpHostBar";
import SftpTabView, { type SftpPaths } from "./components/sftp/SftpTabView";
import SftpDockPanel from "./components/sftp/SftpDockPanel";
// 内置编辑器（CodeMirror）占主脚本约四分之一，只有打开远程文件时才用：按需加载，
// 启动完空闲时再预加载（见 App 里的 effect），第一次打开编辑器时一般已经加载好了。
const loadEditor = () => import("./components/editor/EditorTabView");
const EditorTabView = lazy(loadEditor);

import { UnsavedDialog } from "./components/editor/UnsavedDialog";
import { toEditorError } from "./api/editor";
import {
  applyEditEvent,
  closeDoc,
  markReloadFailed,
  openLocalInEditor,
  openRemoteInEditor,
  saveDoc,
  unsavedDocs,
  useEditors,
} from "./stores/editors";
import {
  fileWindowAdopt,
  fileWindowIsOpen,
  notifySettingsChanged,
  onFileSessions,
  onFileWindowState,
  onOpenEditorRequest,
  onReturnToMain,
  type FileTabHandoff,
} from "./api/fileWindow";
import { describeOpenError } from "./state/editorModel";
import {
  saveFailureText,
  tabTooltip,
  unsavedPrompt,
  type UnsavedFile,
} from "./state/editorModel";
import { useHostTreeWidth, useWindowWidth } from "./components/ui/resize-handle";
import { computeDockLayout } from "./state/dockLayout";
import { canDockOnto, carryPaths, newDockSessionDefaults, OTHER_HOST_NOTICE } from "./state/dock";
import { useTerminalCwd } from "./state/followCwd";
import { sessionCwd, termWrite } from "./api/session";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { handleExternalDragEvent } from "./state/pointerDrag";
import {
  AI_PANEL_DEFAULT,
  SFTP_PANEL_DEFAULT,
  hostTreeRightReserved,
  sanitizeStoredWidth,
  setHostTreeRightReserved,
} from "./state/panelWidths";
import AIChatPanel, { type AiActivityEvent } from "./components/ai/AIChatPanel";
import {
  EMPTY_ACTIVITY,
  markTabSeen,
  reduceActivity,
  unseenTabs,
  type ActivityState,
} from "./state/aiActivity";
import { loadDoubleClickTime } from "./state/doubleClickTime";
import { aiTabBadges, locateTarget, tabForTarget, type TargetRef } from "./state/aiTarget";
import { aiTabFor } from "./state/aiTab";
import DockRow from "./components/terminal/DockRow";
import TerminalLayer from "./components/terminal/TerminalLayer";
import ServerStatsBar from "./components/terminal/ServerStatsBar";
import ServerOverviewPanel from "./components/terminal/ServerOverviewPanel";
import { useServerStats, useSessionNetHistory, useSessionStats } from "./stores/serverStats";
import QuickCommandBar from "./components/terminal/QuickCommandBar";
import {
  overviewBottomOffset,
  useLiveQuickBarHeight,
} from "./components/terminal/quickBarHeight";
import QuickCommandsPanel from "./components/terminal/QuickCommandsPanel";
import {
  commandsInGroup,
  groupsOf,
  isMultiline,
  parseQuickCommands,
  payloadOf,
  serializeQuickCommands,
  type QuickCommand,
} from "./state/quickCommands";
import { ConfirmDialog } from "./components/ui/confirm-dialog";
import SyncStatusBar from "./components/sync/SyncStatusBar";
import UpdateBar from "./components/sync/UpdateBar";
import { startUpdaterEvents, useUpdater } from "./stores/updater";
import {
  countConnections,
  decideClose,
  quitConfirmText,
  quitNotes,
  quitVerdict,
  type QuitState,
} from "./state/closeBehavior";
import { forwardStatuses } from "./api/forwards";
import {
  onTrayOpenUpdate,
  onTrayQuit,
  trayHideMain,
  trayShowHint,
  trayShowMain,
  trayUiReady,
} from "./api/tray";
import { startSyncEvents } from "./stores/sync";
import { cn } from "./lib/utils";
import { toast } from "./components/ui/toast";
import {
  onSessionState,
  sessionClose,
  sessionDuplicate,
  sessionOpen,
  sessionOpenQuick,
  sessionsReset,
  statsSubscribe,
  statsUnsubscribe,
  onServerStats,
  type ServerStatsEvent,
  type SessionStateEvent,
} from "./api/session";
import {
  answerAuth,
  answerHostKey,
  onAuthPrompt,
  onHostKeyPrompt,
  onPromptClosed,
  type HostKeyAnswer,
} from "./api/auth";
import { settingsGet, settingsSet } from "./api/settings";
import {
  onSftpEdit,
  sftpClose,
  sftpEditLeftoverAction,
  sftpEditLeftovers,
  sftpEditResolve,
  sftpOpen,
  type EditLeftover,
  type EditLeftoverResult,
  type SftpEditEvent,
} from "./api/sftp";
import { SftpEditConflictDialog } from "./components/sftp/SftpDialogs";
import { EditRecoveryHost } from "./components/sftp/EditRecoveryHost";
import SftpConflictHost from "./components/sftp/SftpConflictHost";
import { SftpEditOwnerDialog } from "./components/sftp/SftpEditOwnerDialog";
import type { RecoveryAction } from "./components/sftp/EditRecoveryDialog";
import { pickFolder } from "./api/dialog";
import { collectSftpSessions, editToastFor } from "./state/editRecovery";
import { editConfirmMessage } from "./state/editDirty";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listHosts, type Group, type Host } from "./api/vault";
import { aiTargetConnectResult, onTargetConnect } from "./api/agent";
import {
  closePaneWithFocus,
  firstPane,
  paneIds,
  setRatio,
  splitPane,
  type PaneId,
} from "./state/splitTree";
import { emptyPane, type PaneOrigin, type PaneSession, type Tab } from "./state/sessions";
import { keepSubscription, leasesToRelease, type StatsLease } from "./state/statsLease";
import { startHostOs } from "./stores/hostOs";
import { useResolvedMode, useSettings, useTerminalITheme, useTerminalMode } from "./stores/settings";
import { useImeDefaultEnglish } from "./terminal/useImeDefaultEnglish";
import { recordRecent } from "./state/hostTree";
import { hostCopyText } from "./state/hostAddress";
import {
  closeSettings,
  escClosesSettings,
  hasOpenLayer,
  openSettings,
  toggleSettings,
  type MainView,
  type ReturnView,
  type SettingsNav,
} from "./state/settingsView";
import { useVault } from "./stores/vault";

/** 停靠在终端旁的一台主机的 SFTP 会话（M3-3）：每台主机一个，跟着焦点 pane 的主机切换。 */
interface DockSession {
  sftpId: string;
  phase: "connecting" | "ready" | "failed";
  status: string;
  /** 两栏当前的目录：拆回独立标签、切走再切回来都靠它保留。 */
  paths: SftpPaths;
  /** 远端栏是否跟随终端目录（用户手动换目录后关掉）。 */
  follow: boolean;
}

interface Workspace {
  tabs: Tab[];
  activeTabId: string;
  panes: Record<PaneId, PaneSession>;
}

/** 待删除的确认框内容。 */
interface ConfirmState {
  title: string;
  message: string;
  confirmLabel: string;
  destructive: boolean;
  onConfirm: () => void;
}

/**
 * 服务器概览面板的定位框：底部让出快捷命令栏和状态栏。命令栏高度可拖，所以偏移跟着它的
 * 实际高度走；订阅放在这个小组件里，拖动时只有它重渲染，不带着整个 App。
 */
function OverviewAnchor({
  quickBarOpen,
  children,
}: {
  quickBarOpen: boolean;
  children: ReactNode;
}) {
  const quickBarHeight = useLiveQuickBarHeight();
  return (
    <div
      className="absolute right-0 top-0 z-20"
      style={{ bottom: overviewBottomOffset(quickBarOpen, quickBarHeight) }}
    >
      {children}
    </div>
  );
}

/** 焦点会话的状态栏：自己订阅这个会话的数据，推送来了只重画它。 */
function FocusStatsBar({
  sessionId,
  ...rest
}: { sessionId: string | null } & Omit<React.ComponentProps<typeof ServerStatsBar>, "stats">) {
  return <ServerStatsBar stats={useSessionStats(sessionId)} {...rest} />;
}

/** 焦点会话的概览面板：同上。标题优先用主机名（主机库里的名字），没有就用服务器报的 hostname。 */
function FocusOverviewPanel({
  sessionId,
  hostLabel,
  onClose,
}: {
  sessionId: string | null;
  hostLabel: string | null;
  onClose: () => void;
}) {
  const stats = useSessionStats(sessionId);
  const history = useSessionNetHistory(sessionId);
  return (
    <ServerOverviewPanel
      stats={stats}
      history={history}
      title={hostLabel ?? stats?.hostname ?? ""}
      onClose={onClose}
    />
  );
}

/** 打开了一台主机：记进最近打开（主机页卡片按它排在前面）。 */
function touchRecentHost(hostId: number): void {
  const settings = useSettings.getState();
  void settings.update("layout.recent_hosts", recordRecent(settings["layout.recent_hosts"], hostId));
}

function makeId(prefix: string): string {
  counter += 1;
  return `${prefix}-${counter}`;
}
let counter = 0;

/**
 * 界面启动时回收上一页留下的会话，**整个页面只发一次**。
 *
 * 放在模块作用域而不是组件里：`StrictMode` 下 App 会挂载两次，组件里的 effect 会
 * 发两次 `sessions_reset`。虽然后端对重入是安全的（第二次是 0），但第一次挂载之后
 * 用户可能已经开始连主机，第二次 reset 的时机（它自己的 mount）可能撞上新会话的
 * 建立。放在模块级就只有真正加载这个模块的那一次会跑。
 */
let resetStarted = false;
function startResetOnce(): void {
  if (resetStarted) return;
  resetStarted = true;
  void sessionsReset()
    .then((closed) => {
      if (closed > 0) console.info(`已回收上一页留下的 ${closed} 个会话`);
    })
    .catch((error: unknown) => {
      console.warn("sessions_reset 失败", error);
    });
}

function emptyTab(): Tab {
  const paneId = makeId("pane");
  return {
    id: makeId("tab"),
    title: "新标签",
    tree: { kind: "pane", id: paneId },
    focus: paneId,
  };
}

/** 启动时没有终端标签：只显示主机页（空标签是 M1-07 截图里那个多余的白标签）。 */
function initialWorkspace(): Workspace {
  return { tabs: [], activeTabId: "", panes: {} };
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * 一个编辑器标签的内容。自己订阅这份文档的“存在 / 是不是远端 / 哪台主机”，不订阅 version：
 * 输入时文档的 version 每次 +1，但不该带着整个 App 重新渲染（PERF-2）。
 */
function EditorTabSlot(props: {
  editId: number;
  active: boolean;
  sftpIdForHost: (hostId: number) => string | null;
}) {
  const exists = useEditors((state) => state.docs[props.editId] !== undefined);
  const remoteHostId = useEditors((state) => {
    const doc = state.docs[props.editId];
    return doc?.side === "remote" ? doc.hostId : null;
  });
  if (!exists) return null;
  return (
    <Suspense fallback={null}>
      <EditorTabView
        editId={props.editId}
        active={props.active}
        sftpId={remoteHostId !== null ? props.sftpIdForHost(remoteHostId) : null}
      />
    </Suspense>
  );
}

/** 目录跟随查询：包一层，到轮询真正发起时才取 `sessionCwd`。 */
const queryTerminalCwd = (sessionId: string) => sessionCwd(sessionId);

export default function App() {
  // 默认英文输入（AI 对话框除外），设置里可关
  useImeDefaultEnglish();
  const [ws, setWs] = useState<Workspace>(initialWorkspace);
  const wsRef = useRef(ws);
  wsRef.current = ws;

  /** 现在显示主机页、终端还是设置页 */
  const [view, setView] = useState<MainView>("vault");
  const viewRef = useRef(view);
  viewRef.current = view;
  /** 进设置页之前在哪一页：关掉设置回那里（R-11），而不是一律回主机页。 */
  const settingsReturn = useRef<ReturnView>("vault");
  /** 右侧面板：主机详情 / 凭据 / 代理 */
  const [hostPanel, setHostPanel] = useState<{ host: Host | null; groupId: number | null } | null>(null);
  const [sidePanel, setSidePanel] = useState<
    "none" | "identities" | "proxies" | "quickCommands" | "forwards"
  >("none");
  /** 终端界面左边的主机树：展开状态和主机页共用一个设置；选中项只是高亮。 */
  const hostTreeCollapsed = useSettings((state) => state["layout.vault_sidebar_collapsed"]);
  // 主机树宽度（可拖，和主机页、顶栏左侧块共用一个值）：终端区域的左边距跟着它走
  const { width: hostTreeWidth } = useHostTreeWidth();
  const [treeHostId, setTreeHostId] = useState<number | null>(null);
  const [treeGroup, setTreeGroup] = useState<number | null | "all">("all");
  /** 端口转发面板只看哪台主机（null = 全部）。 */
  const [forwardHostId, setForwardHostId] = useState<number | null>(null);
  /** 快捷命令面板打开时要编辑的那一条（"new" = 新建）。 */
  const [quickEditId, setQuickEditId] = useState<string | null>(null);
  // 快捷命令栏：列表跟着同步，开关和当前分组只在本机
  const quickRaw = useSettings((state) => state.quick_commands);
  const quickBarOpen = useSettings((state) => state.quick_commands_bar);
  const quickGroup = useSettings((state) => state.quick_commands_group);
  const confirmMultilinePaste = useSettings((state) => state.terminal_confirm_multiline_paste);
  const updateSettingValue = useSettings((state) => state.update);
  const sftpDockLocalHidden = useSettings((state) => state["layout.sftp_dock_local_hidden"]) === true;
  const quickCommands = useMemo(() => parseQuickCommands(quickRaw), [quickRaw]);
  const quickGroups = useMemo(() => groupsOf(quickCommands), [quickCommands]);
  const shownQuickCommands = useMemo(
    () => commandsInGroup(quickCommands, quickGroup),
    [quickCommands, quickGroup],
  );
  /** AI 面板（AI-2）：终端标签右侧展开。 */
  const [aiOpen, setAiOpen] = useState(false);
  /** SFTP 面板（M3-3）：终端右侧，AI 面板的右边。 */
  const [sftpDockOpen, setSftpDockOpen] = useState(false);
  // 文件窗口（像 Xftp 的独立窗口）开着没有：开着时新开的 SFTP 都进它，不进主窗口。
  const fileWindowOpenRef = useRef(false);
  /** 文件窗口里的 SFTP 会话（主机 id → 会话）：编辑器保存时要用。 */
  const fileSessionsRef = useRef(new Map<number, string>());
  /** 文件窗口的会话表换了就让 App 渲染一次：编辑器标签随之重新取 SFTP 会话
   * （ref 本身不会触发渲染；以前靠每次输入顺带渲染，现在输入不再带动 App）。 */
  const [, setFileSessionsRev] = useState(0);
  /** 第一次用 AI：提示一次"输出会发给模型服务商"（存库，只提示一次）。 */
  const [aiFirstUse, setAiFirstUse] = useState(true);
  const [groupDialog, setGroupDialog] = useState<{
    group: Group | null;
    parentId: number | null;
    adoptUngrouped?: boolean;
  } | null>(null);
  const [confirm, setConfirm] = useState<ConfirmState | null>(null);
  const [quickTarget, setQuickTarget] = useState<string | null>(null);

  /** 后端的提问按 req_id 排队，一次显示一条（C12，见 promptQueue.ts）。 */
  const [promptQueue, setPromptQueue] = useState<readonly PendingPrompt[]>([]);
  const shownPrompt = currentPrompt(promptQueue);
  const hostKeyPrompt = shownPrompt?.kind === "hostkey" ? shownPrompt.prompt : null;
  const authPrompt = shownPrompt?.kind === "auth" ? shownPrompt.prompt : null;

  const terminalTheme = useTerminalITheme();
  // 底部的快捷命令栏、状态栏：界面和终端同为浅色或同为深色时直接透出终端底色，和终端是一整块；
  // 一明一暗时用界面底色，免得界面的灰字压在相反明暗的底上看不清
  const barsOnTerminal = useResolvedMode() === useTerminalMode();

  const removeHost = useVault((state) => state.removeHost);
  const removeGroup = useVault((state) => state.removeGroup);

  /** 后端会话 ID → pane。 */
  const sessionToPane = useRef(new Map<string, PaneId>());
  /** 命令的返回值还没到时先到的状态事件。 */
  const bufferedEvents = useRef(new Map<string, SessionStateEvent[]>());
  /** 每个 pane 往 xterm 写输出的函数。 */
  const writers = useRef(new Map<PaneId, (chunk: ArrayBuffer) => void>());
  /** 每个 pane 最近一次的行列数：重连和复制会话都用它。 */
  const sizes = useRef(new Map<PaneId, { cols: number; rows: number }>());
  /** 分屏出来的新 pane：优先用哪个会话 duplicate。 */
  const duplicateFrom = useRef(new Map<PaneId, string>());
  /** 状态栏（M1-10）：pane → 活跃的采集订阅 ID（跟随焦点 pane）。 */
  const statsLease = useRef(new Map<PaneId, StatsLease>());
  /** 状态栏：已发出、还没回来的订阅请求（pane → 会话），避免 ws 一变就重复订阅。 */
  const statsPending = useRef(new Map<PaneId, string>());
  /** 状态栏：订阅代次。关掉开关或界面卸载时加一，之前发出的订阅回来一律退掉（C13）。 */
  const statsGeneration = useRef(0);
  /** 服务器概览面板（点底部状态栏）开着没有；切标签时保持。 */
  const [overviewOpen, setOverviewOpen] = useState(false);
  /**
   * 已经发出开会话请求、还没拿到结果的 pane。
   *
   * 为什么会话的守卫**必须**是这一份同步状态：`setPane({phase:"connecting"})` 是
   * 异步的，`StrictMode` 双挂载（或者用户快速双击）会让两次 `onReady` 落在同一个
   * task 里，两次读到的 `wsRef.current` 都是"还没开始连接"的旧快照，于是同一个
   * pane 开出两个会话——第二个前端不认领，永远关不掉。这个 ref 在请求发出前就写上，
   * 同步可见。
   */
  const openingPanes = useRef(new Set<PaneId>());
  /**
   * 每个 pane 当前有效的「打开尝试」代次（pane → 代次）。
   *
   * 打开会话是异步的：命令返回会话 ID 之前用户可能已经关掉了标签 / 窗格，而关闭时只能关
   * 当时已经有 \`sessionId\` 的会话。所以每次尝试领一个代次，关 pane 时把它删掉；结果回来时
   * 代次对不上（或 pane 已经没了），说明没人再要这个会话，必须马上关掉返回的 ID。
   * 只在发请求前检查是不够的，关闭恰好落在"请求已发出、ID 还没回来"这个窗口里。
   */
  const openAttempts = useRef(new Map<PaneId, number>());
  const openAttemptSeq = useRef(0);
  /** 迟到之后被我们关掉的会话 ID：它们后续的状态事件直接丢，不再按"还没认领"缓冲。 */
  const discardedSessions = useRef(new Set<string>());

  // 界面启动：叫后端把上一页留下的会话收掉（刷新 / 热更新 / 崩溃）。
  // 见下面的 startResetOnce：整个页面生命周期只发一次。
  useEffect(() => {
    startResetOnce();
  }, []);

  // 启动时读一次外观设置（界面主题在这里应用到 <html>）
  useEffect(() => {
    void useSettings.getState().load();
  }, []);

  // 配置同步：读一次状态并订阅后端推送（状态栏和同步页共用）。
  useEffect(() => {
    let stop: (() => void) | null = null;
    let stopUpdater: (() => void) | null = null;
    let cancelled = false;
    void startUpdaterEvents().then((unlisten) => {
      if (cancelled) unlisten();
      else stopUpdater = unlisten;
    });
    void startSyncEvents().then((unlisten) => {
      if (cancelled) unlisten();
      else stop = unlisten;
    });
    return () => {
      cancelled = true;
      stop?.();
      stopUpdater?.();
    };
  }, []);


  const dialogOpen =
    hostPanel !== null ||
    sidePanel !== "none" ||
    groupDialog !== null ||
    confirm !== null ||
    quickTarget !== null ||
    hostKeyPrompt !== null ||
    authPrompt !== null;

  /** 设置页的进出都走这里：按进来前的那一页回去（见 state/settingsView.ts）。 */
  const navigateSettings = useCallback(
    (step: (nav: SettingsNav, hasActiveTab: boolean) => SettingsNav) => {
      const current = wsRef.current;
      const hasActiveTab = current.tabs.some((item) => item.id === current.activeTabId);
      const next = step({ view: viewRef.current, returnTo: settingsReturn.current }, hasActiveTab);
      settingsReturn.current = next.returnTo;
      setView(next.view);
    },
    [],
  );
  const showSettings = useCallback(() => navigateSettings(openSettings), [navigateSettings]);
  const hideSettings = useCallback(() => navigateSettings(closeSettings), [navigateSettings]);
  const flipSettings = useCallback(() => navigateSettings(toggleSettings), [navigateSettings]);

  const setPane = useCallback((paneId: PaneId, patch: Partial<PaneSession>) => {
    setWs((current) => {
      const pane = current.panes[paneId];
      if (!pane) return current;
      return { ...current, panes: { ...current.panes, [paneId]: { ...pane, ...patch } } };
    });
  }, []);

  const registerWriter = useCallback(
    (paneId: PaneId, writer: ((chunk: ArrayBuffer) => void) | null) => {
      if (writer) writers.current.set(paneId, writer);
      else writers.current.delete(paneId);
    },
    [],
  );

  /**
   * 一个窗格第一次就没连上（连不上、认证失败、密码不对）：打开这台主机的详情，方便直接改。
   * 已经连上过、之后才断开的不弹；用户自己在弹窗里点了取消的也不弹。快速连接没有主机记录，不弹。
   */
  const showHostAfterFailedConnect = useCallback((paneId: PaneId, reason: string) => {
    const origin = wsRef.current.panes[paneId]?.origin;
    if (origin?.kind !== "host" || reason.includes("已取消")) return;
    const host = useVault.getState().hosts.find((item) => item.id === origin.hostId);
    if (!host) return;
    setSidePanel("none");
    setHostPanel({ host, groupId: host.group_id });
  }, []);

  const applyEvent = useCallback(
    (paneId: PaneId, event: SessionStateEvent) => {
      switch (event.state) {
        case "connecting":
          setPane(paneId, {
            phase: "connecting",
            status: event.hop
              ? `连接中：第 ${event.hop.index}/${event.hop.total} 跳 ${event.hop.label}`
              : "连接中…",
          });
          break;
        case "authenticating":
          setPane(paneId, { phase: "authenticating", status: "认证中…" });
          break;
        case "connected":
          setPane(paneId, { phase: "connected", status: "已连接" });
          break;
        case "disconnected": {
          sessionToPane.current.delete(event.session_id);
          const phase = wsRef.current.panes[paneId]?.phase;
          if (phase === "connecting" || phase === "authenticating") {
            showHostAfterFailedConnect(paneId, event.reason ?? "");
          }
          setPane(paneId, {
            phase: "disconnected",
            status: "",
            reason: event.reason ?? "未知原因",
            sessionId: null,
          });
          break;
        }
      }
    },
    [setPane, showHostAfterFailedConnect],
  );

  const claimSession = useCallback(
    (paneId: PaneId, sessionId: string) => {
      sessionToPane.current.set(sessionId, paneId);
      setPane(paneId, { sessionId });
      const pending = bufferedEvents.current.get(sessionId);
      if (!pending) return;
      bufferedEvents.current.delete(sessionId);
      for (const event of pending) applyEvent(paneId, event);
    },
    [applyEvent, setPane],
  );

  /**
   * AI 需要一个 @ 主机自动连接（AI-2c）：在**后台**新开一个标签连它，不抢焦点。
   * 连上之后把会话 ID 交给后端等着的工具调用（`ai_target_connect_result`）。
   * 当前标签的会话变化还由面板的 scope 同步负责。
   */
  const pendingAutoConnect = useRef(
    new Map<number, { targetId: string }>(),
  );
  const autoConnectHost = useCallback(
    (hostId: number, targetId: string) => {
      // 已经有一个连它 / 正在连它的后台标签就不用再开一个。
      const existing = Object.values(wsRef.current.panes).find(
        (pane) => pane.origin.kind === "host" && pane.origin.hostId === hostId,
      );
      if (existing) {
        if (existing.sessionId) {
          void aiTargetConnectResult(hostId, existing.sessionId).catch(() => {});
        } else {
          pendingAutoConnect.current.set(hostId, { targetId });
        }
        return;
      }
      pendingAutoConnect.current.set(hostId, { targetId });
      // 从主机库取主机名做标签；取不到就用 host-<id>。
      void listHosts()
        .then((hosts) => {
          const host = hosts.find((h) => h.id === hostId);
          const title = host?.name ?? targetId;
          const paneId = makeId("pane");
          const tab: Tab = {
            id: makeId("tab"),
            title,
            tree: { kind: "pane", id: paneId },
            focus: paneId,
          };
          // 后台标签：不切 activeTabId、不切 view。
          setWs((current) => ({
            ...current,
            tabs: [...current.tabs, tab],
            panes: {
              ...current.panes,
              [paneId]: emptyPane({ kind: "host", hostId, label: title }),
            },
          }));
        })
        .catch((error: unknown) => {
          console.warn("自动连接取主机失败", error);
          pendingAutoConnect.current.delete(hostId);
        });
    },
    [],
  );

  // 会话连上之后，如果有等着的 AI 自动连接，把会话 ID 交回去。
  useEffect(() => {
    if (pendingAutoConnect.current.size === 0) return;
    for (const hostId of [...pendingAutoConnect.current.keys()]) {
      const found = Object.values(ws.panes).find(
        (pane) =>
          pane.sessionId &&
          pane.origin.kind === "host" &&
          pane.origin.hostId === hostId,
      );
      if (found?.sessionId) {
        pendingAutoConnect.current.delete(hostId);
        void aiTargetConnectResult(hostId, found.sessionId).catch((error: unknown) =>
          console.warn("回报自动连接失败", error),
        );
      }
    }
  }, [ws.panes]);

  // 状态事件：按会话 ID 找 pane；还没登记的先按 ID 存着
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void onSessionState((event) => {
      if (discardedSessions.current.has(event.session_id)) {
        if (event.state === "disconnected") discardedSessions.current.delete(event.session_id);
        return;
      }
      const paneId = sessionToPane.current.get(event.session_id);
      if (paneId) {
        applyEvent(paneId, event);
        return;
      }
      const list = bufferedEvents.current.get(event.session_id) ?? [];
      if (list.length < 64) list.push(event);
      bufferedEvents.current.set(event.session_id, list);
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [applyEvent]);

  // 后端的提问：指纹和认证各监听一路
  useEffect(() => {
    let cancelled = false;
    const stops: (() => void)[] = [];
    void onHostKeyPrompt((prompt) =>
      setPromptQueue((queue) => enqueuePrompt(queue, { kind: "hostkey", prompt })),
    ).then((stop) => {
      if (cancelled) stop();
      else stops.push(stop);
    });
    void onAuthPrompt((prompt) =>
      setPromptQueue((queue) => enqueuePrompt(queue, { kind: "auth", prompt })),
    ).then((stop) => {
      if (cancelled) stop();
      else stops.push(stop);
    });
    // 超时、会话关闭、连接被取消：后端不再等这条了，排在后面的也要摘掉
    void onPromptClosed((reqId) => setPromptQueue((queue) => removePrompt(queue, reqId))).then(
      (stop) => {
        if (cancelled) stop();
        else stops.push(stop);
      },
    );
    // AI 需要一个 @ 主机自动连接（AI-2c）：后台新开标签连它。
    void onTargetConnect((request) => {
      autoConnectHost(request.host_id, request.target_id);
    }).then((stop) => {
      if (cancelled) stop();
      else stops.push(stop);
    });
    return () => {
      cancelled = true;
      for (const stop of stops) stop();
    };
  }, []);

  const handleHostKeyAnswer = useCallback(
    (decision: HostKeyAnswer) => {
      const prompt = hostKeyPrompt;
      if (!prompt) return;
      setPromptQueue((queue) => removePrompt(queue, prompt.req_id));
      void answerHostKey(prompt.req_id, decision).catch((error: unknown) => {
        toast.error(`回答指纹提问失败：${errorText(error)}`);
      });
    },
    [hostKeyPrompt],
  );

  const handleAuthAnswer = useCallback(
    (answers: string[] | null, remember: boolean) => {
      const prompt = authPrompt;
      if (!prompt) return;
      setPromptQueue((queue) => removePrompt(queue, prompt.req_id));
      void answerAuth(prompt.req_id, answers, remember).catch((error: unknown) => {
        toast.error(`回答认证提问失败：${errorText(error)}`);
      });
    },
    [authPrompt],
  );

  /**
   * 某次打开尝试的输出通道。尝试被新的一次取代（重连）或作废（关 pane）之后，
   * 旧通道上还在路上的块直接丢：它们属于已经结束的会话，不能写进新会话的终端、
   * 也不能算进新会话的流控。
   */
  const outputFor = useCallback(
    (paneId: PaneId, isCurrent: () => boolean) => (chunk: ArrayBuffer) => {
      if (isCurrent()) writers.current.get(paneId)?.(chunk);
    },
    [],
  );

  /** 迟到返回、已经没人要的会话：关掉，不登记映射，清掉它的缓冲。 */
  const discardLateSession = useCallback((sessionId: string) => {
    bufferedEvents.current.delete(sessionId);
    discardedSessions.current.add(sessionId);
    void sessionClose(sessionId).catch(() => {});
  }, []);

  /** 给一个 pane 开会话（语义和 M1-06 一样：立即返回 ID，事件驱动）。 */
  const openPane = useCallback(
    async (paneId: PaneId, cols: number, rows: number) => {
      const pane = wsRef.current.panes[paneId];
      if (!pane) return;
      // 先查同步的"已经发出请求"集合。这一层才是防重入的：`pane.phase` 是 React
      // 状态，同一个 task 里的第二次调用读到的还是旧快照（见 openingPanes 的注释）。
      if (openingPanes.current.has(paneId)) return;
      if (
        pane.phase === "connecting" ||
        pane.phase === "authenticating" ||
        (pane.phase === "connected" && pane.sessionId)
      ) {
        return;
      }
      openingPanes.current.add(paneId);

      const duplicateSource = duplicateFrom.current.get(paneId) ?? null;
      duplicateFrom.current.delete(paneId);
      // 领一个新的尝试代次；关 pane 会把它删掉（或被后一次尝试取代），之后回来的结果就不再有效。
      const attempt = ++openAttemptSeq.current;
      openAttempts.current.set(paneId, attempt);
      const isCurrent = () =>
        openAttempts.current.get(paneId) === attempt && wsRef.current.panes[paneId] !== undefined;
      const onOutput = outputFor(paneId, isCurrent);
      const origin = pane.origin;

      const openByOrigin = async (): Promise<string | null> => {
        switch (origin.kind) {
          case "host":
            return sessionOpen(origin.hostId, cols, rows, onOutput);
          case "quick":
            return sessionOpenQuick(origin.target, origin.password, cols, rows, onOutput);
          case "empty":
            return null;
        }
      };

      setPane(paneId, { phase: "connecting", status: "连接中…", reason: "" });
      try {
        let sessionId: string | null = null;
        if (duplicateSource !== null) {
          try {
            sessionId = await sessionDuplicate(duplicateSource, cols, rows, onOutput);
          } catch (error) {
            // 等的时候 pane 已经关了：不要再为一个没人要的 pane 重新开连接
            if (!isCurrent()) return;
            console.warn("session_duplicate 失败，改为重新连接", error);
            sessionId = await openByOrigin();
          }
        } else {
          sessionId = await openByOrigin();
        }
        // 结果回来时 pane 已经被关、或这次尝试已作废：会话没人管了，立刻关掉
        // （不登记映射、不重放缓冲里它的事件）。
        if (!isCurrent()) {
          if (sessionId !== null) discardLateSession(sessionId);
          return;
        }
        if (sessionId === null) {
          setPane(paneId, { phase: "idle", status: "" });
          return;
        }
        claimSession(paneId, sessionId);
      } catch (error) {
        if (!isCurrent()) return; // pane 已经没了，失败也不用再提示
        const reason = errorText(error);
        showHostAfterFailedConnect(paneId, reason);
        setPane(paneId, {
          phase: "disconnected",
          status: "",
          reason,
          sessionId: null,
        });
      } finally {
        // 请求已经落地（拿到 ID 或者失败），把名额放掉。
        // 这是**同步**发生的，`onReady` 的重入不会卡在这里：它要等 `await` 让出。
        openingPanes.current.delete(paneId);
      }
    },
    [claimSession, discardLateSession, outputFor, setPane, showHostAfterFailedConnect],
  );

  const handlePaneReady = useCallback(
    (paneId: PaneId, cols: number, rows: number) => {
      sizes.current.set(paneId, { cols, rows });
      void openPane(paneId, cols, rows);
    },
    [openPane],
  );

  const handlePaneResize = useCallback((paneId: PaneId, cols: number, rows: number) => {
    sizes.current.set(paneId, { cols, rows });
  }, []);

  const reconnect = useCallback(
    (paneId: PaneId) => {
      const pane = wsRef.current.panes[paneId];
      if (!pane || pane.phase !== "disconnected") return;
      const size = sizes.current.get(paneId) ?? { cols: 80, rows: 24 };
      toast.info("正在重连…");
      void openPane(paneId, size.cols, size.rows);
    },
    [openPane],
  );

  // ---------- SFTP 标签（M3-2 第 2、3 节） ----------

  /** 每个 SFTP 标签当前活跃（非终态）的传输数（关闭 / 退出确认要读，
   * 传输队列那一提交接上计数来源）。 */
  const sftpActive = useRef(new Map<string, number>());
  const [, setSftpActiveVersion] = useState(0);

  const setSftpActive = useCallback((sftpId: string, count: number) => {
    if ((sftpActive.current.get(sftpId) ?? 0) === count) return;
    sftpActive.current.set(sftpId, count);
    setSftpActiveVersion((version) => version + 1);
  }, []);

  // ---------- 编辑器事件（保存结果 / 冲突 / 属主询问） ----------
  /** 每个 SFTP 会话里"没落到远端的修改"（冲突、失败、正在上传）：关标签 / 退出前的提醒读它。 */
  const editDirty = useRef(new Map<string, Set<number>>());
  const dirtyEditsOf = useCallback(
    (sftpId: string) => editDirty.current.get(sftpId)?.size ?? 0,
    [],
  );
  /** 自动更新重启前的检查：前端这边数得到的两样（进行中的传输、编辑器里没保存的修改）。 */
  const getUpdateBusy = useCallback(() => {
    let transfers = 0;
    for (const count of sftpActive.current.values()) transfers += count;
    let unsavedEdits = 0;
    for (const set of editDirty.current.values()) unsavedEdits += set.size;
    unsavedEdits += unsavedDocs().length;
    return { transfers, unsavedEdits };
  }, []);
  /** 远端被改过、等用户选的编辑；`laterEdits` 是点了"稍后处理"先收起的。 */
  const [editConflicts, setEditConflicts] = useState<SftpEditEvent[]>([]);
  const [laterEdits, setLaterEdits] = useState<Set<number>>(new Set());
  /** 保存后属主会改变、等用户选的编辑（还没替换服务器上的文件）。 */
  const [ownerPrompts, setOwnerPrompts] = useState<SftpEditEvent[]>([]);
  const [laterOwner, setLaterOwner] = useState<Set<number>>(new Set());
  /** 处理一份上次没上传的修改（列表、入口、重新上传该用哪个会话都由 EditRecoveryHost 管）。 */
  const handleLeftoverAct = useCallback(
    async (
      item: EditLeftover,
      action: RecoveryAction,
      sftpId: string | undefined,
    ): Promise<EditLeftoverResult> => {
      try {
        let result: EditLeftoverResult;
        if (action === "save_local") {
          const dir = await pickFolder(undefined, "把没上传的修改保存到…");
          if (!dir) return { state: "failed", message: "没有选择文件夹" };
          result = await sftpEditLeftoverAction(item.id, action, { destDir: dir });
          if (result.state === "done" && result.message) {
            toast.success(`已保存到 ${result.message}`);
          }
        } else if (action === "reupload" || action === "reupload_overwrite") {
          result = await sftpEditLeftoverAction(item.id, action, { sftpId });
          if (result.state === "done") toast.success("已重新上传");
        } else {
          result = await sftpEditLeftoverAction(item.id, action);
        }
        return result;
      } catch (error) {
        return { state: "failed", message: errorText(error) };
      }
    },
    [],
  );

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    try {
      void onSftpEdit((event) => {
        const set = editDirty.current.get(event.sftp_id) ?? new Set<number>();
        if (event.dirty) set.add(event.edit_id);
        else set.delete(event.edit_id);
        if (set.size > 0) editDirty.current.set(event.sftp_id, set);
        else editDirty.current.delete(event.sftp_id);
        const name = event.remote_path.split("/").pop() ?? event.remote_path;
        switch (event.state) {
          case "conflict":
            setLaterEdits((current) => {
              const next = new Set(current);
              next.delete(event.edit_id);
              return next;
            });
            setEditConflicts((current) => [
              ...current.filter((item) => item.edit_id !== event.edit_id),
              event,
            ]);
            break;
          case "owner_prompt":
            setLaterOwner((current) => {
              const next = new Set(current);
              next.delete(event.edit_id);
              return next;
            });
            setOwnerPrompts((current) => [
              ...current.filter((item) => item.edit_id !== event.edit_id),
              event,
            ]);
            break;
          case "uploaded": {
            setEditConflicts((current) => current.filter((item) => item.edit_id !== event.edit_id));
            setOwnerPrompts((current) => current.filter((item) => item.edit_id !== event.edit_id));
            const shown = editToastFor(event);
            if (shown?.type === "warning") toast.warning(shown.text);
            else toast.success(`已上传「${name}」`);
            // 另存为换了路径 / 处理期间又有输入：把结果同步到编辑器文档。
            void applyEditEvent(event).catch(() => {});
            break;
          }
          case "discarded":
            setEditConflicts((current) => current.filter((item) => item.edit_id !== event.edit_id));
            // 后端已经把内容换回服务器上的版本：编辑器里也要换成那份（内容和编码 / 换行）。
            // 载入失败不能吞掉——编辑器里还是旧文本，后端会拒绝保存它，要让用户知道为什么（R-9 T02）。
            void applyEditEvent(event).then(
              (result) =>
                result === "kept"
                  ? toast.warning(`「${name}」重新载入期间内容有改动，没有覆盖，你输入的内容还在`)
                  : toast.info(`已放弃本地修改，「${name}」换回了服务器上的内容`),
              (error: unknown) => {
                const message = toEditorError(error).message;
                markReloadFailed(event.edit_id, message);
                toast.error(`已放弃本地修改，但「${name}」没能重新载入：${message}`);
              },
            );
            break;
          case "failed":
            toast.error(`「${name}」上传失败：${event.message ?? "未知错误"}`);
            break;
          case "warning":
            toast.warning(editToastFor(event)?.text ?? `「${name}」：出了点问题`);
            break;
          default:
            break;
        }
      })
        .then((stop) => {
          if (cancelled) stop();
          else unlisten = stop;
        })
        .catch(() => {});
    } catch {
      // 没有 Tauri 环境（单测）：不挂。
    }
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // ---------- 内置编辑器（M3-5 第 2 节） ----------
  // App 只要“哪些编辑器有未保存修改”（标签上的圆点）：订阅 editId 数组并浅比较，
  // 不订阅整个 docs——输入时 version 每次 +1，会让 docs 换新对象，带着 App 整个重新渲染。
  const dirtyEditIds = useEditors(
    useShallow((state) =>
      Object.values(state.docs)
        .filter((doc) => doc.dirty)
        .map((doc) => doc.editId),
    ),
  );
  type UnsavedReason = "close-tab" | "close-tabs" | "close-connection" | "quit";
  const [unsavedAsk, setUnsavedAsk] = useState<{
    files: UnsavedFile[];
    reason: UnsavedReason;
    proceed: () => void;
    /** 退出时除未保存文件外还会发生的事（断开连接、取消传输），写在同一个对话框里。 */
    extra?: string;
    busy: boolean;
    error: string | null;
  } | null>(null);
  const askUnsaved = useCallback(
    (files: UnsavedFile[], reason: UnsavedReason, proceed: () => void, extra?: string) => {
      if (files.length === 0) {
        proceed();
        return;
      }
      setUnsavedAsk({ files, reason, proceed, extra, busy: false, error: null });
    },
    [],
  );
  const askUnsavedRef = useRef(askUnsaved);
  askUnsavedRef.current = askUnsaved;

  /** 某台主机现在可用的 SFTP 会话：SFTP 标签优先，其次停靠面板；都没有就是断开了。 */
  const sftpIdForHost = useCallback((hostId: number): string | null => {
    for (const tab of wsRef.current.tabs) {
      if (tab.sftp && tab.sftp.hostId === hostId && tab.sftp.phase === "ready" && tab.sftp.sftpId) {
        return tab.sftp.sftpId;
      }
    }
    const dock = docksRef.current[hostId];
    if (dock?.phase === "ready" && dock.sftpId) return dock.sftpId;
    return fileSessionsRef.current.get(hostId) ?? null;
  }, []);

  /**
   * 只把标签从标签栏拿掉（文档的收尾由调用方做）。
   * `returnToOrigin`：单独关闭当前正在看的编辑器标签时，回到打开它的那个标签（它还在的话）；
   * 批量关闭不传，保持按相邻标签选。
   */
  const dropTabs = useCallback((tabIds: Set<string>, returnToOrigin = false) => {
    setWs((current) => {
      const tabs = current.tabs.filter((item) => !tabIds.has(item.id));
      const panes = { ...current.panes };
      for (const tab of current.tabs) if (tabIds.has(tab.id)) delete panes[tab.focus];
      let activeTabId = current.activeTabId;
      if (tabIds.has(activeTabId)) {
        const index = current.tabs.findIndex((item) => item.id === activeTabId);
        const originId = returnToOrigin
          ? current.tabs.find((item) => item.id === activeTabId)?.editor?.originTabId
          : undefined;
        const origin = originId ? tabs.find((item) => item.id === originId) : undefined;
        const next = origin ?? tabs[Math.min(index, tabs.length) - 1] ?? tabs[0] ?? null;
        activeTabId = next?.id ?? "";
        if (!next) setView("vault");
      }
      return { tabs, activeTabId, panes };
    });
  }, []);

  const editorTabIds = useCallback((editIds: number[]) => {
    const wanted = new Set(editIds);
    return new Set(
      wsRef.current.tabs
        .filter((tab) => tab.editor && wanted.has(tab.editor.editId))
        .map((tab) => tab.id),
    );
  }, []);

  useEffect(() => {
    useEditors.getState().setHooks({
      opened: (doc) => {
        const paneId = makeId("pane");
        const tabId = makeId("tab");
        const tab: Tab = {
          id: tabId,
          title: doc.fileName,
          tooltip: tabTooltip(doc),
          tree: { kind: "pane", id: paneId },
          focus: paneId,
          editor: { editId: doc.editId },
        };
        setWs((current) => ({
          ...current,
          tabs: [
            ...current.tabs,
            // 记下打开那一刻正在看的标签（文件面板、SFTP 标签、文件窗口请求都是这一个），关闭时回到它。
            { ...tab, editor: { editId: doc.editId, originTabId: current.activeTabId || undefined } },
          ],
          activeTabId: tabId,
          panes: { ...current.panes, [paneId]: emptyPane() },
        }));
        setView("terminal");
      },
      focus: (editId) => {
        const tab = wsRef.current.tabs.find((item) => item.editor?.editId === editId);
        if (!tab) return;
        setWs((current) => ({ ...current, activeTabId: tab.id }));
        setView("terminal");
      },
      renamed: (editId, title) => {
        const doc = useEditors.getState().docs[editId];
        const tooltip = doc ? tabTooltip(doc) : title;
        setWs((current) => {
          let changed = false;
          const tabs = current.tabs.map((tab) => {
            if (tab.editor?.editId !== editId) return tab;
            if (tab.title === title && tab.tooltip === tooltip) return tab;
            changed = true;
            return { ...tab, title, tooltip };
          });
          return changed ? { ...current, tabs } : current;
        });
      },
    });
    return () => useEditors.getState().setHooks(null);
  }, []);

  const closeEditorTab = useCallback(
    /** `force`：批量关闭已经统一问过了，这里直接关。 */
    (tabId: string, force = false) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab?.editor) return;
      const editId = tab.editor.editId;
      const doc = useEditors.getState().docs[editId];
      const finish = () => {
        // force 是批量关闭：行为不变，不回原标签。
        dropTabs(new Set([tabId]), !force);
        void closeDoc(editId, false);
      };
      if (doc?.dirty && !force) {
        askUnsaved([{ editId, fileName: doc.fileName, path: doc.path }], "close-tab", finish);
      } else {
        finish();
      }
    },
    [askUnsaved, dropTabs],
  );

  const answerUnsaved = useCallback(
    async (choice: "save" | "discard") => {
      const ask = unsavedAsk;
      if (!ask) return;
      if (choice === "save") {
        setUnsavedAsk({ ...ask, busy: true, error: null });
        for (const file of ask.files) {
          const doc = useEditors.getState().docs[file.editId];
          if (!doc) continue;
          const result = await saveDoc(
            file.editId,
            doc.side === "remote" ? sftpIdForHost(doc.hostId) : null,
          );
          if (result.state !== "uploaded") {
            setUnsavedAsk({
              ...ask,
              busy: false,
              error: `「${file.fileName}」没有保存：${saveFailureText(result)}`,
            });
            return;
          }
        }
        // 批量关标签时，列表里还有"只是所在连接要断开"的文档：保存后它们的编辑器留着，
        // 真正要关的编辑器标签由 proceed 自己关。
        if (ask.reason !== "close-connection" && ask.reason !== "close-tabs") {
          dropTabs(editorTabIds(ask.files.map((file) => file.editId)), ask.reason === "close-tab");
          for (const file of ask.files) await closeDoc(file.editId, false);
        }
      } else {
        dropTabs(editorTabIds(ask.files.map((file) => file.editId)), ask.reason === "close-tab");
        for (const file of ask.files) await closeDoc(file.editId, true);
      }
      setUnsavedAsk(null);
      ask.proceed();
    },
    [unsavedAsk, sftpIdForHost, dropTabs, editorTabIds],
  );

  const shownEditConflict = editConflicts.find((item) => !laterEdits.has(item.edit_id)) ?? null;
  const shownOwnerPrompt = ownerPrompts.find((item) => !laterOwner.has(item.edit_id)) ?? null;

  /**
   * 给一个 SFTP 标签连上某台主机。连接回来时标签已经换了主机（用户连点了两次）就把这条关掉，
   * 不让旧结果盖掉新的。
   */
  const connectSftpTab = useCallback((tabId: string, hostId: number) => {
    void sftpOpen(hostId)
      .then((sftpId) => {
        const tab = wsRef.current.tabs.find((item) => item.id === tabId);
        if (!tab?.sftp || tab.sftp.hostId !== hostId || tab.sftp.sftpId) {
          void sftpClose(sftpId).catch(() => {});
          return;
        }
        setWs((current) => ({
          ...current,
          tabs: current.tabs.map((item) =>
            item.id === tabId && item.sftp && item.sftp.hostId === hostId
              ? { ...item, sftp: { ...item.sftp, sftpId, phase: "ready", status: "" } }
              : item,
          ),
        }));
      })
      .catch((error: unknown) => {
        setWs((current) => ({
          ...current,
          tabs: current.tabs.map((item) =>
            item.id === tabId && item.sftp && item.sftp.hostId === hostId && !item.sftp.sftpId
              ? { ...item, sftp: { ...item.sftp, phase: "failed", status: errorText(error) } }
              : item,
          ),
        }));
      });
  }, []);

  /** 打开一台主机的 SFTP 标签（主机卡片右键"打开 SFTP" / 终端 pane 的 SFTP 按钮）。 */
  const openSftpTab = useCallback(
    (host: { id: number; name: string }) => {
      if (fileWindowOpenRef.current) {
        // 文件窗口开着：在那边开新标签（它自己去连）
        void fileWindowAdopt({
          sftp_id: "",
          host_id: host.id,
          title: `${host.name} (SFTP)`,
          local: "",
          remote: "",
        }).catch((error: unknown) => toast.error(errorText(error)));
        return;
      }
      const paneId = makeId("pane");
      const tabId = makeId("tab");
      const tab: Tab = {
        id: tabId,
        title: `${host.name} (SFTP)`,
        tree: { kind: "pane", id: paneId },
        focus: paneId,
        sftp: { sftpId: "", hostId: host.id, phase: "connecting", status: "连接中…" },
      };
      setWs((current) => ({
        ...current,
        tabs: [...current.tabs, tab],
        activeTabId: tabId,
        panes: { ...current.panes, [paneId]: emptyPane() },
      }));
      setView("terminal");
      connectSftpTab(tabId, host.id);
    },
    [connectSftpTab],
  );

  /**
   * SFTP 标签换一台主机：关掉原来的 SFTP 会话，在同一个标签里连新的。
   * 有传输在跑、编辑没落到远端时不换（先让它们结束），和关标签一样先问没保存的编辑器。
   */
  const switchSftpTabHost = useCallback(
    (tabId: string, host: { id: number; name: string }) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab?.sftp || tab.sftp.hostId === host.id) return;
      const oldId = tab.sftp.sftpId;
      const active = oldId ? sftpActive.current.get(oldId) ?? 0 : 0;
      if (active > 0 || (oldId && dirtyEditsOf(oldId) > 0)) {
        toast.info("这个标签还有传输没完成或编辑没保存到服务器，等它们结束再换主机");
        return;
      }
      const doSwitch = () => {
        if (oldId) {
          sftpActive.current.delete(oldId);
          setSftpActiveVersion((version) => version + 1);
          void sftpClose(oldId).catch(() => {});
        }
        setWs((current) => ({
          ...current,
          tabs: current.tabs.map((item) =>
            item.id === tabId
              ? {
                  ...item,
                  title: `${host.name} (SFTP)`,
                  sftp: { sftpId: "", hostId: host.id, phase: "connecting", status: "连接中…" },
                }
              : item,
          ),
        }));
        connectSftpTab(tabId, host.id);
      };
      if (tab.sftp.hostId) {
        askUnsavedRef.current(unsavedDocs(tab.sftp.hostId), "close-connection", doSwitch);
      } else {
        doSwitch();
      }
    },
    [connectSftpTab, dirtyEditsOf],
  );

  /** 关 SFTP 标签前确认"还有 N 个传输未完成，关闭会取消它们"（M3-2 第 3 节）。 */
  const closeSftpTab = useCallback(
    /** `force`：批量关闭已经统一问过了，这里直接关。 */
    (tabId: string, force = false) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab?.sftp) return;
      const active = tab.sftp.sftpId ? sftpActive.current.get(tab.sftp.sftpId) ?? 0 : 0;
      const dirtyEdits = tab.sftp.sftpId ? dirtyEditsOf(tab.sftp.sftpId) : 0;
      const doCloseNow = () => {
        if (tab.sftp!.sftpId) {
          sftpActive.current.delete(tab.sftp!.sftpId);
          setSftpActiveVersion((version) => version + 1);
          void sftpClose(tab.sftp!.sftpId).catch(() => {});
        }
        setWs((current) => {
          const index = current.tabs.findIndex((item) => item.id === tabId);
          const tabs = current.tabs.filter((item) => item.id !== tabId);
          const panes = { ...current.panes };
          delete panes[tab.focus];
          let activeTabId = current.activeTabId;
          if (activeTabId === tabId) {
            const next = tabs[index - 1] ?? tabs[index + 1] ?? null;
            activeTabId = next?.id ?? "";
            if (!next) setView("vault");
          }
          return { tabs, activeTabId, panes };
        });
      };
      if (force) {
        doCloseNow();
        return;
      }
      const doClose = () =>
        askUnsavedRef.current(unsavedDocs(tab.sftp!.hostId), "close-connection", doCloseNow);
      // 还有未完成的传输：确认"关闭会取消它们"（M3-2 第 3 节；不做后台继续传）。
      if (active > 0 || dirtyEdits > 0) {
        const text = editConfirmMessage(active, dirtyEdits, "关闭这个标签");
        setConfirm({
          title: text.title,
          message: text.message,
          confirmLabel: dirtyEdits > 0 ? "关闭并放弃" : "关闭并取消",
          destructive: true,
          onConfirm: doClose,
        });
        return;
      }
      doClose();
    },
    [dirtyEditsOf],
  );


  /**
   * 标签右键「关闭其他 / 右侧 / 所有」：逐个走普通的关闭（SFTP 有传输、编辑器没保存的照样会问；
   * 一次只能弹一个确认框，要确认的那几个可能留着没关，不会悄悄丢东西）。
   */
  const closeTabs = useCallback(
    (tabId: string, which: "others" | "right" | "all") => {
      const tabs = wsRef.current.tabs;
      const index = tabs.findIndex((item) => item.id === tabId);
      if (index < 0) return;
      const targets = tabs.filter((item, i) =>
        which === "all" ? true : which === "others" ? item.id !== tabId : i > index,
      );
      if (targets.length === 0) return;
      // 要确认的东西先汇总：传输 / 没落到远端的编辑（SFTP 标签）、没保存的文档（编辑器标签和
      // 要断开的 SFTP 连接上的文档）。只问一次，确认后整批直接关（复审 F5：以前逐个关，后一个
      // 确认框把前一个盖掉，"关闭所有"关不完）。
      let transfers = 0;
      let pendingEdits = 0;
      const files = new Map<number, UnsavedFile>();
      for (const tab of targets) {
        if (tab.sftp) {
          if (tab.sftp.sftpId) {
            transfers += sftpActive.current.get(tab.sftp.sftpId) ?? 0;
            pendingEdits += dirtyEditsOf(tab.sftp.sftpId);
          }
          for (const file of unsavedDocs(tab.sftp.hostId)) files.set(file.editId, file);
        } else if (tab.editor) {
          const doc = useEditors.getState().docs[tab.editor.editId];
          if (doc?.dirty) {
            files.set(tab.editor.editId, { editId: tab.editor.editId, fileName: doc.fileName, path: doc.path });
          }
        }
      }
      const closeAll = () => {
        for (const tab of targets) closeTabRef.current(tab.id, true);
      };
      const askFiles = () => askUnsavedRef.current([...files.values()], "close-tabs", closeAll);
      if (transfers > 0 || pendingEdits > 0) {
        const text = editConfirmMessage(transfers, pendingEdits, `关闭这 ${targets.length} 个标签`);
        setConfirm({
          title: text.title,
          message: text.message,
          confirmLabel: pendingEdits > 0 ? "关闭并放弃" : "关闭并取消",
          destructive: true,
          onConfirm: askFiles,
        });
        return;
      }
      askFiles();
    },
    [dirtyEditsOf],
  );
  const closeTabRef = useRef<(tabId: string, force?: boolean) => void>(() => {});

  const closeTab = useCallback((tabId: string, force = false) => {
    const tab = wsRef.current.tabs.find((item) => item.id === tabId);
    if (!tab) return;
    if (tab.sftp) {
      closeSftpTab(tabId, force);
      return;
    }
    if (tab.editor) {
      closeEditorTab(tabId, force);
      return;
    }
    for (const paneId of paneIds(tab.tree)) {
      const session = wsRef.current.panes[paneId];
      if (session?.sessionId) {
        sessionToPane.current.delete(session.sessionId);
        void sessionClose(session.sessionId).catch(() => {});
      }
      writers.current.delete(paneId);
      sizes.current.delete(paneId);
      duplicateFrom.current.delete(paneId);
      openAttempts.current.delete(paneId); // 还在路上的打开结果作废（T02）
    }

    setWs((current) => {
      const index = current.tabs.findIndex((item) => item.id === tabId);
      const tabs = current.tabs.filter((item) => item.id !== tabId);
      const panes = { ...current.panes };
      for (const paneId of paneIds(tab.tree)) delete panes[paneId];
      let activeTabId = current.activeTabId;
      let nextView = view;
      if (activeTabId === tabId) {
        const next = current.tabs[index - 1] ?? current.tabs[index + 1] ?? null;
        activeTabId = next?.id ?? "";
        if (!next) nextView = "vault";
      }
      setView(nextView);
      return { tabs, activeTabId, panes };
    });
  }, [view, closeSftpTab, closeEditorTab]);
  closeTabRef.current = closeTab;

  const closePaneAt = useCallback(
    (tabId: string, paneId: PaneId) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab) return;
      const session = wsRef.current.panes[paneId];
      if (session?.sessionId) {
        sessionToPane.current.delete(session.sessionId);
        void sessionClose(session.sessionId).catch(() => {});
      }
      writers.current.delete(paneId);
      sizes.current.delete(paneId);
      duplicateFrom.current.delete(paneId);
      openAttempts.current.delete(paneId); // 还在路上的打开结果作废（T02）

      const { tree, focus } = closePaneWithFocus(tab.tree, paneId, tab.focus);
      if (tree === null) {
        closeTab(tabId);
        return;
      }
      setWs((current) => {
        const panes = { ...current.panes };
        delete panes[paneId];
        return {
          ...current,
          tabs: current.tabs.map((item) =>
            item.id === tabId ? { ...item, tree, focus: focus ?? firstPane(tree) } : item,
          ),
          panes,
        };
      });
    },
    [closeTab],
  );

  const newTab = useCallback(() => {
    const tab = emptyTab();
    setWs((current) => ({
      tabs: [...current.tabs, tab],
      activeTabId: tab.id,
      panes: { ...current.panes, [tab.focus]: emptyPane() },
    }));
    setView("terminal");
  }, []);

  /** 打开一个新的终端标签（带来源）。 */
  useEffect(() => {
    for (const paneId of [...pendingConnect.current]) {
      const pane = ws.panes[paneId];
      if (!pane) {
        pendingConnect.current.delete(paneId);
        continue;
      }
      if (pane.origin.kind === "empty") continue;
      pendingConnect.current.delete(paneId);
      const size = sizes.current.get(paneId) ?? { cols: 80, rows: 24 };
      void openPane(paneId, size.cols, size.rows);
    }
  }, [ws, openPane]);

  /** 标签右键「重新连接」：把这个终端标签里已经断开的会话重连（连着的不动，菜单里那时是灰的）。 */
  const reconnectTab = useCallback(
    (tabId: string) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab || tab.sftp || tab.editor) return;
      for (const paneId of paneIds(tab.tree)) {
        if (wsRef.current.panes[paneId]?.phase === "disconnected") reconnect(paneId);
      }
    },
    [reconnect],
  );

  const openTabWithOrigin = useCallback((origin: PaneOrigin, title: string) => {
    const paneId = makeId("pane");
    const tab: Tab = {
      id: makeId("tab"),
      title,
      tree: { kind: "pane", id: paneId },
      focus: paneId,
    };
    setWs((current) => ({
      tabs: [...current.tabs, tab],
      activeTabId: tab.id,
      panes: { ...current.panes, [paneId]: emptyPane(origin) },
    }));
    setView("terminal");
  }, []);

  /** 空白窗格选了主机：改成连这台主机，状态更新后再开会话（见下面的 effect）。 */
  const pendingConnect = useRef(new Set<PaneId>());
  const connectPaneTo = useCallback((paneId: PaneId, host: Host) => {
    touchRecentHost(host.id);
    pendingConnect.current.add(paneId);
    setWs((current) => {
      const pane = current.panes[paneId];
      if (!pane || pane.origin.kind !== "empty") return current;
      return {
        ...current,
        // 只有一个窗格的标签跟着改名
        tabs: current.tabs.map((tab) =>
          tab.tree.kind === "pane" && tab.tree.id === paneId ? { ...tab, title: host.name } : tab,
        ),
        panes: {
          ...current.panes,
          [paneId]: { ...pane, origin: { kind: "host", hostId: host.id, label: host.name } },
        },
      };
    });
    setView("terminal");
  }, []);

  const openHost = useCallback(
    (host: Host) => {
      // 主机页双击连接时，第一下单击已经打开了这台主机的详情：连接就不需要它了，关掉。
      // 连不上的话 showHostAfterFailedConnect 会再把它打开。
      setHostPanel((current) => (current?.host?.id === host.id ? null : current));
      // 当前是个空白标签（新标签、还没连）：就在它里面连，不再另开一个
      const current = wsRef.current;
      const tab = current.tabs.find((item) => item.id === current.activeTabId);
      if (view === "terminal" && tab && !tab.sftp && !tab.editor && tab.tree.kind === "pane") {
        const pane = current.panes[tab.tree.id];
        if (pane?.origin.kind === "empty" && pane.phase !== "connected") {
          connectPaneTo(tab.tree.id, host);
          return;
        }
      }
      touchRecentHost(host.id);
      openTabWithOrigin({ kind: "host", hostId: host.id, label: host.name }, host.name);
    },
    [openTabWithOrigin, connectPaneTo, view],
  );

  /** 标签右键「复制标签页」：再开一个连同一台主机的终端标签（新会话）。 */
  const duplicateTab = useCallback(
    (tabId: string) => {
      const tab = wsRef.current.tabs.find((item) => item.id === tabId);
      if (!tab || tab.sftp || tab.editor) return;
      const origin = wsRef.current.panes[tab.focus]?.origin;
      if (!origin || origin.kind === "empty") {
        toast.info("这个标签还没连主机，没有可复制的");
        return;
      }
      openTabWithOrigin(origin, tab.title);
    },
    [openTabWithOrigin],
  );

  const splitAt = useCallback((tabId: string, paneId: PaneId, axis: "x" | "y") => {
    const current = wsRef.current;
    const tab = current.tabs.find((item) => item.id === tabId);
    if (!tab) return;
    const source = current.panes[paneId];
    const newPaneId = makeId("pane");
    const tree = splitPane(tab.tree, paneId, newPaneId, makeId("split"), axis);
    if (source?.sessionId) duplicateFrom.current.set(newPaneId, source.sessionId);
    setWs((now) => ({
      ...now,
      tabs: now.tabs.map((item) => (item.id === tabId ? { ...item, tree, focus: newPaneId } : item)),
      panes: { ...now.panes, [newPaneId]: emptyPane(source?.origin ?? { kind: "empty" }) },
    }));
    setView("terminal");
  }, []);

  const selectTab = useCallback((tabId: string) => {
    setWs((current) => ({ ...current, activeTabId: tabId }));
    setView("terminal");
  }, []);

  const cycleTab = useCallback((step: number) => {
    setWs((current) => {
      if (current.tabs.length < 2) return current;
      const index = current.tabs.findIndex((item) => item.id === current.activeTabId);
      if (index < 0) return current;
      const next = (index + step + current.tabs.length) % current.tabs.length;
      return { ...current, activeTabId: current.tabs[next].id };
    });
    setView("terminal");
  }, []);

  const focusPane = useCallback((paneId: PaneId) => {
    setWs((current) => {
      const tab = current.tabs.find((item) => item.id === current.activeTabId);
      if (!tab || tab.focus === paneId) return current;
      return {
        ...current,
        tabs: current.tabs.map((item) => (item.id === tab.id ? { ...item, focus: paneId } : item)),
      };
    });
  }, []);


  // ---------- 快捷命令 / 输入广播 ----------
  /** 往一个终端标签发内容：`all` = 这个标签的所有分屏，否则只发焦点窗格。返回发出去几个。 */
  const sendToTab = useCallback((tabId: string, data: string, all: boolean): number => {
    const current = wsRef.current;
    const tab = current.tabs.find((item) => item.id === tabId);
    if (!tab || tab.sftp || tab.editor) return 0;
    const targets = all ? paneIds(tab.tree) : [tab.focus];
    let sent = 0;
    for (const paneId of targets) {
      const pane = current.panes[paneId];
      if (!pane?.sessionId || pane.phase !== "connected") continue;
      sent += 1;
      void termWrite(pane.sessionId, data).catch((error: unknown) => {
        console.error("term_write 失败", error);
      });
    }
    return sent;
  }, []);

  /** 键盘输入：这个标签开着广播就发到所有分屏（返回 true，PaneView 不再自己发）。 */
  const handlePaneInput = useCallback(
    (paneId: string, data: string): boolean => {
      const current = wsRef.current;
      const tab = current.tabs.find((item) => !item.sftp && paneIds(item.tree).includes(paneId));
      if (!tab?.broadcast) return false;
      sendToTab(tab.id, data, true);
      return true;
    },
    [sendToTab],
  );

  const toggleBroadcast = useCallback((tabId: string) => {
    setWs((current) => ({
      ...current,
      tabs: current.tabs.map((item) =>
        item.id === tabId ? { ...item, broadcast: !item.broadcast } : item,
      ),
    }));
  }, []);

  /** 发完命令把键盘还给终端（点按钮时焦点跑到按钮上了）。 */
  const focusTerminalOf = useCallback((paneId: string) => {
    window.requestAnimationFrame(() => {
      document
        .querySelector<HTMLTextAreaElement>(`[data-pane-id="${paneId}"] textarea`)
        ?.focus();
    });
  }, []);

  const runQuickCommand = useCallback(
    (command: QuickCommand, allPanes: boolean) => {
      const current = wsRef.current;
      const tab = current.tabs.find((item) => item.id === current.activeTabId);
      if (!tab || tab.sftp || tab.editor) return;
      const all = allPanes || Boolean(tab.broadcast);
      const send = () => {
        const sent = sendToTab(tab.id, payloadOf(command), all);
        if (sent === 0) {
          toast.info(all ? "这个标签里还没有连上的窗格" : "当前窗格还没连上");
        }
        focusTerminalOf(tab.focus);
      };
      if (isMultiline(command) && confirmMultilinePaste) {
        setConfirm({
          title: `发送多行命令「${command.name}」？`,
          message: `${command.command.trim().split(/\r?\n/).length} 行，每一行都会回车执行${
            all ? "，发到这个标签的所有分屏" : ""
          }。`,
          confirmLabel: "发送",
          destructive: false,
          onConfirm: send,
        });
        return;
      }
      send();
    },
    [sendToTab, focusTerminalOf, confirmMultilinePaste],
  );

  const saveQuickCommands = useCallback(
    (list: QuickCommand[]) => {
      void updateSettingValue("quick_commands", serializeQuickCommands(list)).catch(
        (error: unknown) => toast.error(`保存快捷命令失败：${String(error)}`),
      );
    },
    [updateSettingValue],
  );

  const openQuickCommandsPanel = useCallback((editId: string | null) => {
    setQuickEditId(editId);
    setSidePanel("quickCommands");
  }, []);

  const setSplitRatio = useCallback((splitId: string, ratio: number) => {
    setWs((current) => ({
      ...current,
      tabs: current.tabs.map((item) =>
        item.id === current.activeTabId ? { ...item, tree: setRatio(item.tree, splitId, ratio) } : item,
      ),
    }));
  }, []);

  // 服务器状态栏（M1-10）：数据在 stores/serverStats，状态栏和概览面板自己按会话订阅。
  const activeTab = useMemo(
    () => ws.tabs.find((item) => item.id === ws.activeTabId) ?? null,
    [ws],
  );
  // AI 面板只跟着 shell 标签（它是在终端里执行命令的，SFTP 标签里没有可操作的终端）。
  // 切到 SFTP 标签时面板藏起来但不卸载，仍然挂在刚才那个 shell 标签上：切回去对话还在。
  const lastShellTabIdRef = useRef<string | null>(null);
  if (activeTab && !activeTab.sftp) lastShellTabIdRef.current = activeTab.id;
  const aiTab = aiTabFor(ws.tabs, activeTab, lastShellTabIdRef.current);
  const focusPaneOf: PaneSession | null = activeTab
    ? ws.panes[activeTab.focus] ?? null
    : null;
  const focusSessionId = focusPaneOf?.sessionId ?? null;

  // AI 面板（AI-2）：当前标签里所有已连上的会话（scope 成员）。
  const aiSessions = useMemo(() => {
    if (!aiTab) return [];
    return paneIds(aiTab.tree)
      .map((paneId) => ({ paneId, pane: ws.panes[paneId] }))
      .filter((entry): entry is { paneId: string; pane: PaneSession } =>
        Boolean(entry.pane?.sessionId),
      )
      .map(({ paneId, pane }) => ({
        sessionId: pane.sessionId as string,
        // 键盘焦点所在的窗格：AI 的逻辑目标（一台主机一个）绑定到它的会话。
        focused: paneId === aiTab.focus,
        hostId: pane.origin.kind === "host" ? pane.origin.hostId : null,
        hostLabel:
          pane.origin.kind === "host"
            ? pane.origin.label
            : pane.origin.kind === "quick"
              ? pane.origin.label
              : "会话",
        username: "",
      }));
  }, [aiTab, ws.panes]);

  // ---------- SFTP 面板（M3-3） ----------
  // 每台主机一个停靠的 SFTP 会话：面板跟着当前焦点 pane 的主机走，没打开过的新开。
  const [docks, setDocks] = useState<Record<number, DockSession>>({});
  const docksRef = useRef(docks);
  docksRef.current = docks;
  /** 现在在用的 SFTP 会话（独立标签 + 终端旁边的文件侧栏）：恢复没上传的修改时重新上传要用。 */
  const sftpSessions = useMemo(() => collectSftpSessions(ws.tabs, docks), [ws.tabs, docks]);
  const conflictSessionIds = useMemo(
    () => sftpSessions.map((session) => session.sftpId).filter(Boolean),
    [sftpSessions],
  );

  // 当前是远端文件的编辑器标签：文件面板接着显示这个文件所在的主机（双击面板里的文件打开
  // 编辑器后，面板不该变成"没有主机"）。本地文件的编辑器没有主机。
  const activeEditId = activeTab?.editor?.editId;
  const editorHost = useEditors(
    useShallow((state) => {
      const doc = activeEditId !== undefined ? state.docs[activeEditId] : undefined;
      return doc?.side === "remote" ? { hostId: doc.hostId, label: doc.hostLabel } : null;
    }),
  );
  const focusedHostId =
    activeTab && !activeTab.sftp && focusPaneOf?.origin.kind === "host"
      ? focusPaneOf.origin.hostId
      : (editorHost?.hostId ?? null);
  const focusedHostLabel =
    focusPaneOf?.origin.kind === "host" ? focusPaneOf.origin.label : (editorHost?.label ?? "");

  const patchDock = useCallback((hostId: number, patch: Partial<DockSession>) => {
    setDocks((current) =>
      current[hostId] ? { ...current, [hostId]: { ...current[hostId], ...patch } } : current,
    );
  }, []);

  /** 每台主机正在打开的那一次停靠会话：打开结果回来时对不上（条目已被清理、或清理后又重开了）就作废。 */
  const dockOpening = useRef(new Map<number, symbol>());

  const ensureDock = useCallback(
    (hostId: number) => {
      if (docksRef.current[hostId]) return;
      const entry: DockSession = {
        sftpId: "",
        phase: "connecting",
        status: "连接中…",
        ...newDockSessionDefaults(),
      };
      docksRef.current = { ...docksRef.current, [hostId]: entry };
      setDocks(docksRef.current);
      const token = Symbol(hostId);
      dockOpening.current.set(hostId, token);
      const current = () =>
        dockOpening.current.get(hostId) === token && docksRef.current[hostId] !== undefined;
      void sftpOpen(hostId)
        .then((sftpId) => {
          if (!current()) {
            // 连接中这台主机的终端就都关了：会话没人用，直接关掉，不写回（否则成了看不见的连接）
            void sftpClose(sftpId).catch(() => {});
            return;
          }
          dockOpening.current.delete(hostId);
          patchDock(hostId, { sftpId, phase: "ready", status: "" });
        })
        .catch((error: unknown) => {
          if (!current()) return;
          dockOpening.current.delete(hostId);
          patchDock(hostId, { phase: "failed", status: errorText(error) });
        });
    },
    [patchDock],
  );

  useEffect(() => {
    if (sftpDockOpen && focusedHostId !== null) ensureDock(focusedHostId);
  }, [sftpDockOpen, focusedHostId, ensureDock]);

  const focusedDock = focusedHostId !== null ? docks[focusedHostId] : undefined;

  // 跟随终端目录：面板开着、跟随开关开着时，定期问一下焦点终端现在在哪个目录。
  // 走后端的 exec 通道（`session_cwd`），不往终端里敲任何东西；取不到就是 null。
  const focusedSessionId = focusPaneOf?.sessionId ?? null;
  const followActive =
    sftpDockOpen &&
    view === "terminal" &&
    focusedDock?.phase === "ready" &&
    focusedDock.follow &&
    focusedSessionId !== null;
  // 目录没变不更新状态（见 `useTerminalCwd`），轮询周期不变。
  const terminalCwd = useTerminalCwd(followActive ? focusedSessionId : null, queryTerminalCwd);
  const followPath =
    focusedDock?.follow && terminalCwd && terminalCwd.sessionId === focusedSessionId
      ? terminalCwd.cwd
      : null;

  // 从资源管理器拖文件进窗口：Tauri 把系统拖放翻译成事件（带路径和物理像素坐标），
  // 这里转成"落在哪个登记过的落点上"——远端栏 = 上传，终端 = 插入本地路径。
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    try {
      void getCurrentWebview()
        .onDragDropEvent((event) => {
          const payload = event.payload;
          handleExternalDragEvent(
            payload.type === "leave"
              ? { type: "leave" }
              : payload.type === "over"
                ? { type: "over", position: payload.position }
                : { type: payload.type, paths: payload.paths, position: payload.position },
            window.devicePixelRatio,
          );
        })
        .then((stop) => {
          if (cancelled) stop();
          else unlisten = stop;
        })
        .catch(() => {});
    } catch {
      // 没有 Tauri 环境（单测）：不挂。
    }
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // SFTP 标签两栏的目录（停靠时原样带进面板）。按 sftpId 记，不进 ws，免得每次换目录整棵树重画。
  const sftpTabPaths = useRef(new Map<string, SftpPaths>());
  /** 拖动中的 SFTP 标签悬停在哪个终端标签上（停靠提示用）。 */
  const [dockHover, setDockHover] = useState<{ tabId: string } | null>(null);
  const lastRejectedHover = useRef<string | null>(null);

  const dockVerdict = useCallback(
    (sftpTabId: string, targetTabId: string): "ok" | "other-host" | "none" => {
      const current = wsRef.current;
      const source = current.tabs.find((item) => item.id === sftpTabId);
      const target = current.tabs.find((item) => item.id === targetTabId);
      if (!source?.sftp || !target) return "none";
      const verdict = canDockOnto(source.sftp.hostId, target, current.panes);
      if (verdict.ok) return "ok";
      return verdict.reason === "other-host" ? "other-host" : "none";
    },
    [],
  );

  const handleSftpDragHover = useCallback(
    (
      info: {
        sftpTabId: string;
        overTabId: string | null;
        verdict: "ok" | "other-host" | "none";
      } | null,
    ) => {
      if (!info) {
        setDockHover(null);
        lastRejectedHover.current = null;
        return;
      }
      setDockHover(info.verdict === "ok" && info.overTabId ? { tabId: info.overTabId } : null);
      if (info.verdict === "other-host") {
        // 别的主机的终端标签：不显示停靠提示，只提示一次原因。
        if (lastRejectedHover.current !== info.overTabId) {
          lastRejectedHover.current = info.overTabId;
          toast.info(OTHER_HOST_NOTICE);
        }
      } else {
        lastRejectedHover.current = null;
      }
    },
    [],
  );

  /** SFTP 标签 → 停靠到终端标签右边：会话和两栏目录原样带过去，原来的标签消失。 */
  const dockSftpTab = useCallback(
    (sftpTabId: string, targetTabId: string) => {
      const current = wsRef.current;
      const source = current.tabs.find((item) => item.id === sftpTabId);
      const target = current.tabs.find((item) => item.id === targetTabId);
      if (!source?.sftp || !source.sftp.sftpId || !target) return;
      const verdict = canDockOnto(source.sftp.hostId, target, current.panes);
      if (!verdict.ok) return;
      const hostId = source.sftp.hostId;
      const sftpId = source.sftp.sftpId;
      const existing = docksRef.current[hostId];
      if (existing?.sftpId && existing.sftpId !== sftpId) {
        // 这台主机已经停着一个：它还有传输在跑就不动它，提示后取消停靠。
        if ((sftpActive.current.get(existing.sftpId) ?? 0) > 0) {
          toast.info("这台主机的文件面板里还有传输在进行，先等它们完成");
          return;
        }
        sftpActive.current.delete(existing.sftpId);
        void sftpClose(existing.sftpId).catch(() => {});
      }
      const next: DockSession = {
        sftpId,
        phase: "ready",
        status: "",
        paths: carryPaths(sftpTabPaths.current.get(sftpId) ?? source.sftp.paths),
        // 停靠要保留两栏当前的目录（用户在标签里自己选的），所以先不跟随终端；想跟随再勾上。
        follow: false,
      };
      // 原来那个还在连接中：它的打开结果作废（回来时会自己关掉，不覆盖这次停靠的会话）
      dockOpening.current.delete(hostId);
      docksRef.current = { ...docksRef.current, [hostId]: next };
      setDocks(docksRef.current);
      setWs((state) => {
        const tabs = state.tabs
          .filter((item) => item.id !== sftpTabId)
          .map((item) => (item.id === targetTabId ? { ...item, focus: verdict.paneId } : item));
        const panes = { ...state.panes };
        delete panes[source.focus];
        return { tabs, panes, activeTabId: targetTabId };
      });
      setSftpDockOpen(true);
      setView("terminal");
      setDockHover(null);
    },
    [],
  );

  /** 面板的"在新标签中打开"：变回独立的双栏标签，会话和目录原样带过去。 */
  const undockToTab = useCallback((hostId: number, label: string) => {
    const dock = docksRef.current[hostId];
    if (!dock || dock.phase !== "ready" || !dock.sftpId) return;
    if (fileWindowOpenRef.current) {
      // 文件窗口开着：拆出来的标签放进文件窗口，会话照用
      const sftpId = dock.sftpId;
      void fileWindowAdopt({
        sftp_id: sftpId,
        host_id: hostId,
        title: `${label} (SFTP)`,
        local: dock.paths.local,
        remote: dock.paths.remote,
      })
        .then(() => {
          sftpActive.current.delete(sftpId);
          const next = { ...docksRef.current };
          delete next[hostId];
          docksRef.current = next;
          setDocks(next);
          setSftpDockOpen(false);
        })
        .catch((error: unknown) => toast.error(errorText(error)));
      return;
    }
    const paneId = makeId("pane");
    const tabId = makeId("tab");
    const tab: Tab = {
      id: tabId,
      title: `${label} (SFTP)`,
      tree: { kind: "pane", id: paneId },
      focus: paneId,
      sftp: { sftpId: dock.sftpId, hostId, phase: "ready", status: "", paths: dock.paths },
    };
    sftpTabPaths.current.set(dock.sftpId, dock.paths);
    const next = { ...docksRef.current };
    delete next[hostId];
    docksRef.current = next;
    setDocks(next);
    setWs((state) => ({
      ...state,
      tabs: [...state.tabs, tab],
      activeTabId: tabId,
      panes: { ...state.panes, [paneId]: emptyPane() },
    }));
    setSftpDockOpen(false);
  }, []);

  /** 主窗口里加一个从文件窗口移回来的 SFTP 标签（没有会话就新连一个）。 */
  const adoptSftpTabFromFileWindow = useCallback((handoff: FileTabHandoff) => {
    const paneId = makeId("pane");
    const tabId = makeId("tab");
    const paths = { local: handoff.local, remote: handoff.remote };
    const ready = handoff.sftp_id !== "";
    if (ready) sftpTabPaths.current.set(handoff.sftp_id, paths);
    setWs((state) => ({
      ...state,
      tabs: [
        ...state.tabs,
        {
          id: tabId,
          title: handoff.title,
          tree: { kind: "pane", id: paneId },
          focus: paneId,
          sftp: ready
            ? { sftpId: handoff.sftp_id, hostId: handoff.host_id, phase: "ready", status: "", paths }
            : { sftpId: "", hostId: handoff.host_id, phase: "connecting", status: "连接中…" },
        },
      ],
      activeTabId: tabId,
      panes: { ...state.panes, [paneId]: emptyPane() },
    }));
    setView("terminal");
    if (ready) return;
    void sftpOpen(handoff.host_id)
      .then((sftpId) =>
        setWs((state) => ({
          ...state,
          tabs: state.tabs.map((item) =>
            item.id === tabId && item.sftp
              ? { ...item, sftp: { ...item.sftp, sftpId, phase: "ready", status: "" } }
              : item,
          ),
        })),
      )
      .catch((error: unknown) =>
        setWs((state) => ({
          ...state,
          tabs: state.tabs.map((item) =>
            item.id === tabId && item.sftp
              ? { ...item, sftp: { ...item.sftp, phase: "failed", status: errorText(error) } }
              : item,
          ),
        })),
      );
  }, []);

  /** 主窗口的 SFTP 标签移到文件窗口（拖出窗口 / 右键）：会话交过去，这边不断开。 */
  const moveSftpTabToFileWindow = useCallback((tabId: string) => {
    const tab = wsRef.current.tabs.find((item) => item.id === tabId);
    if (!tab?.sftp) return;
    if (tab.sftp.phase !== "ready" || !tab.sftp.sftpId) {
      toast.info("这个 SFTP 还没连上，连上之后再移");
      return;
    }
    const sftpId = tab.sftp.sftpId;
    const paths = carryPaths(sftpTabPaths.current.get(sftpId) ?? tab.sftp.paths);
    void fileWindowAdopt({
      sftp_id: sftpId,
      host_id: tab.sftp.hostId,
      title: tab.title,
      local: paths.local,
      remote: paths.remote,
    })
      .then(() => {
        sftpActive.current.delete(sftpId);
        setSftpActiveVersion((version) => version + 1);
        setWs((current) => {
          const index = current.tabs.findIndex((item) => item.id === tabId);
          const tabs = current.tabs.filter((item) => item.id !== tabId);
          const panes = { ...current.panes };
          delete panes[tab.focus];
          let activeTabId = current.activeTabId;
          if (activeTabId === tabId) {
            const next = tabs[index - 1] ?? tabs[index] ?? null;
            activeTabId = next?.id ?? "";
            if (!next) setView("vault");
          }
          return { tabs, activeTabId, panes };
        });
      })
      .catch((error: unknown) => toast.error(`移到文件窗口失败：${errorText(error)}`));
  }, []);

  // 和文件窗口的往来：开关状态、它的会话清单、请主窗口开编辑器、标签移回来。
  // 浏览器 / 单测里没有 Tauri，这些调用会直接抛错：一律吞掉，主界面照常工作。
  const adoptFromFileWindowRef = useRef(adoptSftpTabFromFileWindow);
  adoptFromFileWindowRef.current = adoptSftpTabFromFileWindow;
  useEffect(() => {
    const stops: (() => void)[] = [];
    let cancelled = false;
    const keep = (start: () => Promise<() => void>) => {
      try {
        void start()
          .then((stop) => {
            if (cancelled) stop();
            else stops.push(stop);
          })
          .catch(() => {});
      } catch {
        // 没有 Tauri（测试）
      }
    };
    const bringToFront = () => {
      try {
        const win = getCurrentWindow();
        // 主窗口缩在托盘里（隐藏）时 setFocus 叫不出来，要先 show：文件窗口把标签交回来时用户得看到它们。
        void win.show().catch(() => {});
        void win.unminimize().catch(() => {});
        void win.setFocus().catch(() => {});
      } catch {
        // 没有 Tauri（测试）
      }
    };
    try {
      void fileWindowIsOpen()
        .then((open) => {
          fileWindowOpenRef.current = open;
        })
        .catch(() => {});
    } catch {
      // 没有 Tauri（测试）
    }
    // 主窗口的设置变了（主题等）：通知文件窗口重读，主题跟着变
    const stopSettings = useSettings.subscribe(() => {
      if (!fileWindowOpenRef.current) return;
      try {
        void notifySettingsChanged().catch(() => {});
      } catch {
        // 没有 Tauri（测试）
      }
    });
    stops.push(stopSettings);
    keep(() =>
      onFileWindowState((open) => {
        fileWindowOpenRef.current = open;
        if (!open) {
          fileSessionsRef.current = new Map();
          setFileSessionsRev((rev) => rev + 1);
        }
      }),
    );
    keep(() =>
      onFileSessions((sessions) => {
        fileSessionsRef.current = new Map(sessions.map((s) => [s.host_id, s.sftp_id]));
        setFileSessionsRev((rev) => rev + 1);
      }),
    );
    keep(() =>
      onOpenEditorRequest((request) => {
        const name = request.path.split(/[\\/]/).pop() ?? request.path;
        const open =
          request.kind === "remote"
            ? openRemoteInEditor(request.sftp_id, request.path, request.host_id ?? undefined)
            : openLocalInEditor(request.path);
        void open
          .then(() => {
            setView("terminal");
            bringToFront();
          })
          .catch((error: unknown) => {
            const failure = describeOpenError(toEditorError(error), name);
            toast.info(`${failure.title}：${failure.message}`);
          });
      }),
    );
    keep(() =>
      onReturnToMain((handoff) => {
        adoptFromFileWindowRef.current(handoff);
        bringToFront();
      }),
    );
    return () => {
      cancelled = true;
      for (const stop of stops) stop();
    };
  }, []);

  // 这台主机的终端 pane 都关了：停靠的会话跟着收掉（还有传输在跑就留着，下次打开接着看）。
  useEffect(() => {
    const alive = new Set<number>();
    for (const pane of Object.values(ws.panes)) {
      if (pane.origin.kind === "host") alive.add(pane.origin.hostId);
    }
    for (const tab of ws.tabs) {
      if (tab.sftp) alive.add(tab.sftp.hostId);
    }
    for (const [key, dock] of Object.entries(docksRef.current)) {
      const hostId = Number(key);
      // 还在连接中的也收掉：打开结果回来时发现条目没了，会自己把会话关掉（见 ensureDock）
      if (alive.has(hostId)) continue;
      if (dock.sftpId && (sftpActive.current.get(dock.sftpId) ?? 0) > 0) continue;
      if (dock.sftpId && (editDirty.current.get(dock.sftpId)?.size ?? 0) > 0) continue;
      // 这台主机还开着内置编辑器：连接留着，保存才有地方写。
      if (
        Object.values(useEditors.getState().docs).some(
          (doc) => doc.side === "remote" && doc.hostId === hostId,
        )
      )
        continue;
      if (dock.sftpId) {
        sftpActive.current.delete(dock.sftpId);
        void sftpClose(dock.sftpId).catch(() => {});
      }
      dockOpening.current.delete(hostId);
      const next = { ...docksRef.current };
      delete next[hostId];
      docksRef.current = next;
      setDocks(next);
    }
  }, [ws.panes, ws.tabs]);

  // 三块同屏的宽度（终端｜AI｜SFTP，顺序固定）。
  const windowWidth = useWindowWidth();
  const aiStoredWidth = useSettings((state) => state["layout.ai_panel_width"]);
  const sftpStoredWidth = useSettings((state) => state["layout.sftp_panel_width"]);
  const aiVisible = view === "terminal" && aiOpen && Boolean(activeTab) && !activeTab?.sftp;
  const sftpVisible =
    view === "terminal" && sftpDockOpen && Boolean(activeTab) && !activeTab?.sftp;
  // 主机树的上限要给右边开着的面板让位（只有终端界面算；主机页没有这些面板）。写进模块级
  // 共享状态，主机页、终端界面、顶栏三处的主机树宽度才一致；用 layout effect 是为了
  // 在绘制之前就生效，不闪一下旧宽度。
  useLayoutEffect(() => {
    setHostTreeRightReserved(
      view === "terminal" ? hostTreeRightReserved(aiVisible, sftpVisible) : 0,
    );
  }, [view, aiVisible, sftpVisible]);
  useLayoutEffect(() => () => setHostTreeRightReserved(0), []);
  const dockLayout = computeDockLayout({
    windowWidth,
    aiOpen: aiVisible,
    sftpOpen: sftpVisible,
    aiDesired: sanitizeStoredWidth(aiStoredWidth, AI_PANEL_DEFAULT),
    sftpDesired: sanitizeStoredWidth(sftpStoredWidth, SFTP_PANEL_DEFAULT),
    // 终端界面左边展开的主机树占掉的宽度：终端保底宽度要在它之外算
    reserved: view === "terminal" && !hostTreeCollapsed ? hostTreeWidth : 0,
  });
  // 窗口太窄两个放不下：收起 AI 面板并提示；窗口变宽后不自动展开。
  useEffect(() => {
    if (dockLayout.collapseAi && aiOpen) {
      setAiOpen(false);
      toast.info("窗口太窄，已收起 AI 面板");
    }
  }, [dockLayout.collapseAi, aiOpen]);

  // @ 标签的状态点（AI-2d 3.2）：按主机汇总所有标签里这台主机的连接状态
  // （连着的优先、其次连接中，都没有就是还没连）。
  const aiHostStates = useMemo(() => {
    const states: Record<number, "connected" | "connecting" | "offline"> = {};
    for (const pane of Object.values(ws.panes)) {
      if (pane.origin.kind !== "host") continue;
      const hostId = pane.origin.hostId;
      const current = states[hostId];
      if (pane.phase === "connected") {
        states[hostId] = "connected";
      } else if (
        (pane.phase === "connecting" || pane.phase === "authenticating") &&
        current !== "connected"
      ) {
        states[hostId] = "connecting";
      } else if (current === undefined) {
        states[hostId] = "offline";
      }
    }
    return states;
  }, [ws.panes]);

  // 所有还开着的会话（AI 切走焦点时保留的会话是不是已经关了，面板据此判断）。
  const openSessionIds = useMemo(() => {
    const ids = new Set<string>();
    for (const pane of Object.values(ws.panes)) if (pane.sessionId) ids.add(pane.sessionId);
    return ids;
  }, [ws.panes]);

  // 后台标签的 AI 活动提示（AI-SCOPE-1 第 4 节）：AI 正在某个没在看的标签里执行命令时
  // 显示活动点；跑完还没看过显示「有新输出」点；切过去就清掉。位置用执行层回报的实际执行会话
  // （AI-SCOPE-2），不自己猜。
  // 记账按调用记（见 state/aiActivity.ts）：执行位置被执行编号纠正时，错的标签上的点会收回来，
  // 挪到正确的标签。
  const [aiActivity, setAiActivity] = useState<ActivityState>(EMPTY_ACTIVITY);
  const handleAiActivity = useCallback((event: AiActivityEvent) => {
    setAiActivity((prev) =>
      reduceActivity(prev, event, (target) => {
        const current = wsRef.current;
        return tabForTarget(current.tabs, current.panes, target);
      }),
    );
  }, []);
  const aiRunning = aiActivity.running;
  const aiUnseen = useMemo(() => unseenTabs(aiActivity), [aiActivity]);
  const watchedTabId = view === "terminal" ? ws.activeTabId : null;
  useEffect(() => {
    if (watchedTabId && aiUnseen.includes(watchedTabId)) {
      setAiActivity((prev) => markTabSeen(prev, watchedTabId));
    }
  }, [watchedTabId, aiUnseen]);
  const aiBadges = useMemo(
    () =>
      aiTabBadges(ws.tabs, ws.panes, Object.values(aiRunning), aiUnseen, watchedTabId),
    [ws.tabs, ws.panes, aiRunning, aiUnseen, watchedTabId],
  );

  // 工具卡片上的「查看终端」：切到执行层回报的那个会话所在的标签和窗格（回报时还没有会话的，
  // 切到正在连的那个标签，能看到连接过程）。
  const viewAiTarget = useCallback((target: TargetRef) => {
    const found = locateTarget(wsRef.current.tabs, wsRef.current.panes, target);
    if (!found) {
      toast.info(
        target.sessionId
          ? "执行的那个终端已经关闭了"
          : "这台主机现在没有打开的终端标签",
      );
      return;
    }
    setWs((current) => ({
      ...current,
      activeTabId: found.tabId,
      tabs: current.tabs.map((item) =>
        item.id === found.tabId ? { ...item, focus: found.paneId } : item,
      ),
    }));
    setView("terminal");
  }, []);

  // 第一次用 AI 的提示只出一次（存库）。
  useEffect(() => {
    void settingsGet("ai_first_use_done")
      .then((raw) => setAiFirstUse(!raw))
      .catch(() => {});
  }, []);


  // 主机列表的系统图标：读一次已经探测过的，之后跟着后端事件更新
  useEffect(() => startHostOs(), []);

  // 系统的双击时间（主机树属性面板的收起延迟要跟着它走）：启动时读一次。
  useEffect(() => {
    void loadDoubleClickTime();
  }, []);

  // 服务器状态（M1-10）：后端按订阅 ID 推，这里把最新值记到 pane 上，并推动
  // 状态栏重画。推送本身不进 React 状态（量大的时候会拖慢输入）。
  const statsSettings = useSettings((state) => state.stats_show);
  const statsRefresh = useSettings((state) => state.stats_refresh_secs);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void onServerStats((event: ServerStatsEvent) => {
      // 订阅 ID → 租约 → 会话 ID（statsLease 在订阅时记）
      // 只有对得上租约的才收（退订后迟到的推送直接丢掉，不触发任何重画）
      for (const lease of statsLease.current.values()) {
        if (lease.subscriptionId !== event.subscription_id) continue;
        useServerStats.getState().push(lease.sessionId, event.stats);
        break;
      }
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // 跟随焦点 pane：给它背后的会话订阅采集，离开 pane/关标签时退订。
  // （后端按连接去重，同一连接上的多个订阅只是多一个订阅 ID，不会多开任务。）
  useEffect(() => {
    const paneId = activeTab?.focus ?? null;
    const pane = paneId ? wsRef.current.panes[paneId] : null;
    const sessionId = pane?.sessionId ?? null;

    if (!statsSettings) {
      // 开关关掉：之前发出、还没回来的订阅全部作废，回来时由代次判断退掉
      statsGeneration.current += 1;
      statsPending.current.clear();
    }
    // 退掉不再需要的租约（开关关掉、pane 换焦点、pane 关掉、会话断了或换了）
    const leases = statsLease.current;
    for (const leasedPane of leasesToRelease(leases, statsSettings, paneId, sessionId)) {
      const lease = leases.get(leasedPane);
      leases.delete(leasedPane);
      if (!lease) continue;
      void statsUnsubscribe(lease.subscriptionId).catch((error: unknown) => {
        console.warn("stats_unsubscribe 失败", error);
      });
    }
    if (!statsSettings || !sessionId || !paneId) return;
    if (leases.has(paneId) || statsPending.current.get(paneId) === sessionId) return;

    const request = { generation: statsGeneration.current, pane: paneId, sessionId };
    statsPending.current.set(paneId, sessionId);
    void statsSubscribe(sessionId)
      .then((subscriptionId) => {
        const sameGeneration = request.generation === statsGeneration.current;
        if (sameGeneration && statsPending.current.get(paneId) === sessionId) {
          statsPending.current.delete(paneId);
        }
        // 回来之前开关可能关了、pane 可能换了会话：按当前情况决定，不看发出时
        // 捕获的旧值（后端自己也会在没订阅者时退，但尽快退更省）
        const keep = keepSubscription(request, {
          generation: statsGeneration.current,
          paneSession: wsRef.current.panes[paneId]?.sessionId ?? null,
          alreadyLeased: leases.has(paneId),
        });
        if (keep) {
          leases.set(paneId, { subscriptionId, sessionId });
        } else {
          void statsUnsubscribe(subscriptionId).catch(() => {});
        }
      })
      .catch((error: unknown) => {
        // 会话刚断开/ 或 snapshot 变化时订阅会失败，正常
        console.debug("stats_subscribe 失败", error);
        const sameGeneration = request.generation === statsGeneration.current;
        if (sameGeneration && statsPending.current.get(paneId) === sessionId) {
          statsPending.current.delete(paneId);
        }
      });
  }, [activeTab?.focus, activeTab?.id, statsSettings, statsRefresh, ws]);

  // 界面卸载：退掉全部租约，还没回来的订阅按代次作废
  useEffect(() => {
    const leases = statsLease.current;
    const pending = statsPending.current;
    return () => {
      statsGeneration.current += 1;
      pending.clear();
      for (const lease of leases.values()) {
        void statsUnsubscribe(lease.subscriptionId).catch(() => {});
      }
      leases.clear();
    };
  }, []);

  // 所有会话都关掉/换标签时把状态栏的旧数据清掉
  useEffect(() => {
    const alive = new Set<string>();
    for (const pane of Object.values(wsRef.current.panes)) {
      if (pane.sessionId) alive.add(pane.sessionId);
    }
    useServerStats.getState().prune(alive);
  }, [ws]);

  // 启动完、浏览器空闲时预加载编辑器代码（不抢首屏；没有 requestIdleCallback 就延后 2 秒）
  useEffect(() => {
    const preload = () => void loadEditor().catch(() => {});
    if (typeof window.requestIdleCallback === "function") {
      const id = window.requestIdleCallback(preload, { timeout: 5000 });
      return () => window.cancelIdleCallback(id);
    }
    const timer = window.setTimeout(preload, 2000);
    return () => window.clearTimeout(timer);
  }, []);

  // 快捷键（和 M1-06 一致）。捕获阶段处理，先于 xterm，也不会发给远端。
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const current = wsRef.current;
      const tab = current.tabs.find((item) => item.id === current.activeTabId) ?? null;
      const target = event.target as HTMLElement | null;

      if ((event.ctrlKey || event.metaKey) && event.shiftKey && !event.altKey) {
        const key = event.key.toLowerCase();
        if (key === "a") {
          // AI 面板（AI-2）：只有终端标签下能开。
          event.preventDefault();
          event.stopPropagation();
          if (dialogOpen || view !== "terminal" || !tab) return;
          setAiOpen((open) => !open);
          return;
        }
        if (key === "f") {
          // 文件（SFTP）面板：只有终端标签下能开。
          event.preventDefault();
          event.stopPropagation();
          if (dialogOpen || view !== "terminal" || !tab || tab.sftp) return;
          setSftpDockOpen((open) => !open);
          return;
        }
        if (key === "b") {
          // 输入广播：只对终端标签
          event.preventDefault();
          event.stopPropagation();
          if (dialogOpen || view !== "terminal" || !tab || tab.sftp || tab.editor) return;
          toggleBroadcast(tab.id);
          return;
        }
        if (key === "t" || key === "w" || key === "d" || key === "e") {
          event.preventDefault();
          event.stopPropagation();
          if (dialogOpen) return;
          if (key === "t") {
            newTab();
          } else if (!tab) {
            return;
          } else if (key === "w") {
            closePaneAt(tab.id, tab.focus);
          } else {
            splitAt(tab.id, tab.focus, key === "d" ? "x" : "y");
          }
          return;
        }
      }

      // Ctrl+, 打开设置
      if ((event.ctrlKey || event.metaKey) && event.key === ",") {
        event.preventDefault();
        event.stopPropagation();
        if (!dialogOpen) showSettings();
        return;
      }

      if (event.ctrlKey && event.key === "Tab") {
        event.preventDefault();
        event.stopPropagation();
        if (!dialogOpen) cycleTab(event.shiftKey ? -1 : 1);
        return;
      }

      // 断线的 pane 上按 R 重连
      if (!event.ctrlKey && !event.metaKey && !event.altKey && (event.key === "r" || event.key === "R")) {
        if (!tab || dialogOpen || view !== "terminal") return;
        const typing =
          target !== null &&
          (target.tagName === "INPUT" || target.tagName === "TEXTAREA") &&
          !target.classList.contains("xterm-helper-textarea");
        if (typing) return;
        const pane = current.panes[tab.focus];
        if (pane?.phase === "disconnected") {
          event.preventDefault();
          reconnect(tab.focus);
        }
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [closePaneAt, cycleTab, dialogOpen, newTab, reconnect, showSettings, splitAt, toggleBroadcast, view]);

  // 设置页里按 Esc 关设置（R-11）。挂在 window 的冒泡阶段：Radix 的下拉框、对话框在
  // document 捕获阶段先处理 Esc 并 preventDefault，那一下只关弹层，轮不到这里。
  // 右侧面板 / 确认框开着时也不关：用户的注意力在那边。
  useEffect(() => {
    if (view !== "settings") return;
    const handler = (event: KeyboardEvent) => {
      if (dialogOpen) return;
      if (!escClosesSettings(event, viewRef.current, hasOpenLayer(document))) return;
      event.preventDefault();
      hideSettings();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [dialogOpen, hideSettings, view]);

  /**
   * 关闭主窗口（UI-4 起先看设置，M3-2 第 3 节起有退出确认）。
   *
   * 拦的是 Tauri 的 close-requested（自绘标题栏的关闭按钮走
   * `getCurrentWindow().close()`，Alt+F4、任务栏右键「关闭窗口」也一样；原生窗口关闭不会触发
   * `beforeunload`，用后者的话这个确认根本不会出现）。判断见 `state/closeBehavior.ts`：
   *
   * - 「关闭主窗口时」= 缩到托盘（默认）：拦下关闭、隐藏主窗口，不弹任何确认（什么都没丢）；
   *   第一次提示一下程序还在右下角。后端没有托盘图标时隐藏会被拒绝，退回成退出流程。
   * - = 退出程序，或者这次关闭本来就是要退出（托盘「退出」、用户已经确认）：有未保存的修改或
   *   传输就拦下窗口关闭，问一次；确认之后先让每个 SFTP 标签走 `sftp_close`（取消传输并清掉
   *   临时文件），再真的关窗口。
   *
   * Rust 那边不处理主窗口的 CloseRequested（tray.rs 开头有说明），决定权只在这里。
   */
  const exitConfirmed = useRef(false);
  /** 下一次关闭请求是「要退出」（托盘「退出」或退出流程里自己再关一次窗口）：用一次就清掉。 */
  const quitRequested = useRef(false);
  useEffect(() => {
    let cancelled = false;
    const stops: Array<() => void> = [];
    /** 再关一次窗口，这次明确是要退出：缩到托盘模式下也不能再隐藏。 */
    const closeToQuit = () => {
      quitRequested.current = true;
      void getCurrentWindow().close();
    };
    try {
      void getCurrentWindow()
        .onCloseRequested(async (event) => {
          if (!useSettings.getState().loaded) {
            // 设置还没读回来（刚启动几秒内）：先读，免得用默认值盖掉用户选的「退出程序」。
            await useSettings.getState().load().catch(() => {});
          }
          const settings = useSettings.getState();
          const quitting = quitRequested.current || exitConfirmed.current;
          quitRequested.current = false;
          const decision = decideClose({
            action: settings.close_action,
            quitting,
            hintShown: settings.tray_hint_shown,
          });
          if (decision.type === "hide") {
            event.preventDefault();
            const hidden = await trayHideMain().catch(() => false);
            if (hidden) {
              if (decision.showHint) {
                void settings.update("tray_hint_shown", true);
                void trayShowHint().catch(() => {});
              }
              return;
            }
            // 没隐藏成（没有托盘图标）：窗口还在，按退出走，不能让用户点了 × 没反应。
            closeToQuit();
            return;
          }

          // 点 × 退出和托盘「退出」都走这里：活动连接（终端、端口转发、SFTP）也算要确认的事。
          // 先问完转发状态再读其余状态：等待期间窗口还能输入，不能拿等待前的快照判断。
          const forwardPhases = exitConfirmed.current
            ? []
            : await forwardStatuses()
                .then((list) => list.map((item) => item.phase))
                .catch(() => []);
          const unsaved = unsavedDocs();
          let total = 0;
          for (const count of sftpActive.current.values()) total += count;
          let dirtyTotal = 0;
          for (const set of editDirty.current.values()) dirtyTotal += set.size;
          const state: QuitState = {
            unsavedDocs: unsaved.length,
            transfers: total,
            dirtyEdits: dirtyTotal,
            connections: countConnections({
              tabs: wsRef.current.tabs,
              panes: wsRef.current.panes,
              fileWindowSessions: fileSessionsRef.current.size,
              dockPhases: Object.values(docksRef.current).map((dock) => dock.phase),
              forwardPhases,
            }),
            confirmed: exitConfirmed.current,
          };
          const verdict = quitVerdict(state);
          if (verdict.type === "go") return;
          event.preventDefault();
          // 有事要问：主窗口可能缩在托盘里，先叫出来让用户看见。没事要问时不会走到这里，不会闪一下。
          void trayShowMain().catch(() => {});
          /** 用户确认退出：取消传输、关掉所有 SFTP，再真的关窗口。 */
          const confirmedQuit = () => {
            exitConfirmed.current = true;
            if (total > 0) toast.info("正在取消传输并退出…");
            const closing = [
              ...wsRef.current.tabs
                .filter((tab) => tab.sftp?.sftpId)
                .map((tab) => tab.sftp!.sftpId),
              ...Object.values(docksRef.current)
                .filter((dock) => dock.sftpId)
                .map((dock) => dock.sftpId),
            ].map((id) => sftpClose(id).catch(() => {}));
            // 取消传输最多等 12 秒（后端自己最多等 10 秒），再关窗口。
            void Promise.race([
              Promise.allSettled(closing),
              new Promise((resolve) => setTimeout(resolve, 12000)),
            ]).then(() => getCurrentWindow().close());
          };
          if (verdict.type === "ask-unsaved") {
            // 其余原因（连接、传输……）写进同一个对话框；选保存或不保存后一并确认退出。
            const notes = quitNotes(state);
            askUnsavedRef.current(
              unsaved,
              "quit",
              notes.length > 0 ? confirmedQuit : closeToQuit,
              notes.length > 0 ? notes.join("\n") : undefined,
            );
            return;
          }
          const text = quitConfirmText(state);
          setConfirm({
            title: text.title,
            message: text.message,
            confirmLabel: text.confirmLabel,
            destructive: true,
            onConfirm: confirmedQuit,
          });
        })
        .then(async (stop) => {
          if (cancelled) {
            stop();
            return;
          }
          stops.push(stop);
          // 托盘菜单的两项：「退出」走和 × 退出同一套确认（上面的 closeToQuit → 关闭请求），
          // 「重启安装」打开已经下载好的更新对话框。两个监听都挂好才告诉后端能回答退出确认。
          const [stopQuit, stopUpdate] = await Promise.all([
            onTrayQuit(closeToQuit),
            onTrayOpenUpdate(() => useUpdater.getState().openDialog(true)),
          ]);
          if (cancelled) {
            stopQuit();
            stopUpdate();
            return;
          }
          stops.push(stopQuit, stopUpdate);
          void trayUiReady(true).catch(() => {});
        })
        .catch(() => {});
    } catch {
      // 没有 Tauri 窗口（单测环境）：不挂这个拦截。
    }
    return () => {
      cancelled = true;
      for (const stop of stops) stop();
      void trayUiReady(false).catch(() => {});
    };
  }, []);

  /** 删除主机前确认。 */
  const askDeleteHost = useCallback(
    (host: Host) => {
      setConfirm({
        title: `删除主机「${host.name}」？`,
        message: "主机的记录会被删掉，保险库里的凭据不受影响。",
        confirmLabel: "删除",
        destructive: true,
        onConfirm: () => {
          void removeHost(host.id)
            .then(() => {
              toast.success(`已删除「${host.name}」`);
              setHostPanel(null);
            })
            .catch((error: unknown) => toast.error(`删除失败：${errorText(error)}`));
        },
      });
    },
    [removeHost],
  );

  const askDeleteGroup = useCallback(
    (group: Group) => {
      setConfirm({
        title: `删除分组「${group.name}」？`,
        message: "分组下的主机会变成未分组，不会被删。",
        confirmLabel: "删除",
        destructive: true,
        onConfirm: () => {
          void removeGroup(group.id)
            .then(() => toast.success(`已删除分组「${group.name}」`))
            .catch((error: unknown) => toast.error(`删除失败：${errorText(error)}`));
        },
      });
    },
    [removeGroup],
  );

  /** 终端界面主机树的操作：单击高亮并展开 / 收起树底部的属性面板，双击连接，右键「编辑」打开主机详情。 */
  const terminalTreeActions: HostActions = useMemo(
    () => ({
      select: (host) => setTreeHostId(host.id),
      connect: (host) => openHost(host),
      openSftp: (host) => openSftpTab(host),
      remove: (host) => askDeleteHost(host),
      copy: (host, kind) => {
        const text = hostCopyText(host, kind);
        void navigator.clipboard
          .writeText(text)
          .then(() => toast.info(`已复制：${text}`))
          .catch(() => toast.error("复制失败"));
      },
      edit: (host) => {
        setSidePanel("none");
        setHostPanel({ host, groupId: host.group_id });
      },
      forwards: (host) => {
        setForwardHostId(host.id);
        setSidePanel("forwards");
      },
    }),
    [openHost, openSftpTab, askDeleteHost],
  );

  return (
    <div className="flex h-full flex-col bg-background text-foreground">
      <TopTabs
        tabs={ws.tabs}
        activeTabId={ws.activeTabId}
        panes={ws.panes}
        view={view}
        onShowVault={() => setView("vault")}
        onOpenSettings={flipSettings}
        onSelectTab={selectTab}
        onCloseTab={closeTab}
        onReconnectTab={reconnectTab}
        onDuplicateTab={duplicateTab}
        onCloseTabs={closeTabs}
        onNewTab={newTab}
        aiBadges={aiBadges}
        aiOpen={aiOpen}
        onToggleAI={() => {
          if (view !== "terminal" || !activeTab) {
            toast.info("AI 面板只在终端标签下可用");
            return;
          }
          setAiOpen((open) => !open);
        }}
        dockVerdict={dockVerdict}
        onDockSftpTab={dockSftpTab}
        onDetachSftpTab={moveSftpTabToFileWindow}
        onSftpDragHover={handleSftpDragHover}
        editorDirty={(editId) => dirtyEditIds.includes(editId)}
        sftpOpen={sftpDockOpen}
        onToggleSftp={() => {
          if (view !== "terminal" || !activeTab || activeTab.sftp) {
            toast.info("文件面板只在终端标签下可用");
            return;
          }
          setSftpDockOpen((open) => !open);
        }}
        // 主机页没有终端、看不到底部的命令栏：这个按钮改成打开 / 关上右侧的快捷命令管理面板
        quickBarOpen={view === "vault" ? sidePanel === "quickCommands" : quickBarOpen}
        onToggleQuickBar={() => {
          if (view === "vault") {
            if (sidePanel === "quickCommands") setSidePanel("none");
            else openQuickCommandsPanel(null);
            return;
          }
          void updateSettingValue("quick_commands_bar", !quickBarOpen);
        }}
      />

      <DockRow
        main={
        <div className="relative min-w-0 flex-1">
          {dockHover && (
            /* 拖着 SFTP 标签悬停在同一台主机的终端标签上：右侧出现停靠提示（M3-3 第 2 节） */
            <div
              data-section="dock-hint"
              className="pointer-events-none absolute inset-y-2 right-2 z-30 flex w-[min(420px,40%)] items-center justify-center rounded-md border-2 border-dashed border-primary bg-primary/10 text-sm text-primary"
            >
              松手停靠到这个终端的右侧
            </div>
          )}
          {view === "vault" && (
            <VaultView
              onOpenHost={openHost}
              onOpenSftp={openSftpTab}
              onQuickConnect={setQuickTarget}
              onSelectHost={(host) => {
                setSidePanel("none");
                setHostPanel({ host, groupId: host.group_id });
              }}
              onNewHost={(groupId) => {
                setSidePanel("none");
                setHostPanel({ host: null, groupId });
              }}
              onNewGroup={(parentId, options) =>
                setGroupDialog({ group: null, parentId, adoptUngrouped: options?.adoptUngrouped })
              }
              onEditGroup={(group) => setGroupDialog({ group, parentId: group.parent_id })}
              onDeleteGroup={askDeleteGroup}
              onDeleteHost={askDeleteHost}
              // 主机详情正在编辑时打开凭据 / 代理面板：不关主机详情，只把它藏到下面
              // （见下面 after 里的 hidden），关掉面板回来时没保存的编辑还在。
              onOpenIdentities={() => setSidePanel("identities")}
              onOpenProxies={() => setSidePanel("proxies")}
              onOpenForwards={(hostId) => {
                setForwardHostId(hostId);
                setSidePanel("forwards");
              }}
              onNotice={(message) => toast.info(message)}
            />
          )}

          {/* 终端区：一直挂载，切到主机页/设置页时只是藏起来（会话和 xterm 都留着）。
              所有标签都挂载（非活动的藏起来）：AI 自动连接一个 @ 主机时会在后台新开标签，
              那个标签也必须能连上。 */}
          {view === "terminal" && !hostTreeCollapsed && (
            // 主机树在终端界面也常驻（展开时）：和主机页是同一个组件、同一个开关
            <div className="absolute inset-y-0 left-0 z-10 flex" data-section="terminal-host-tree">
              <HostTreeAside
                selectedGroup={treeGroup}
                onSelectGroup={setTreeGroup}
                selectedHostId={treeHostId}
                hostActions={terminalTreeActions}
                infoPanel
                onNewHost={(groupId) => {
                  setSidePanel("none");
                  setHostPanel({ host: null, groupId });
                }}
                onNewGroup={(parentId, options) =>
                setGroupDialog({ group: null, parentId, adoptUngrouped: options?.adoptUngrouped })
              }
                onEditGroup={(group) => setGroupDialog({ group, parentId: group.parent_id })}
                onDeleteGroup={askDeleteGroup}
                collapsedStyle="hidden"
              />
            </div>
          )}
          <div
            className={cn("absolute inset-y-0 right-0 flex flex-col", view !== "terminal" && "hidden")}
            style={{ left: view === "terminal" && !hostTreeCollapsed ? hostTreeWidth : 0 }}
          >
            {ws.tabs.length === 0 ? (
              <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
                没有打开的标签。按 Ctrl+Shift+T 新建，或者去主机页双击一台主机。
              </div>
            ) : (
              ws.tabs.map((tab) => {
                const isActive = tab.id === activeTab?.id;
                // 内置编辑器标签（M3-5）。
                if (tab.editor) {
                  return (
                    <div
                      key={tab.id}
                      className={cn(
                        "h-full flex-col",
                        isActive ? "flex" : "invisible absolute inset-0 flex",
                      )}
                    >
                      <EditorTabSlot
                        editId={tab.editor.editId}
                        active={isActive && view === "terminal"}
                        sftpIdForHost={sftpIdForHost}
                      />
                    </div>
                  );
                }
                // SFTP 标签（M3-2 第 2 节）：双栏视图，没有终端层。
                if (tab.sftp) {
                  return (
                    <div
                      key={tab.id}
                      className={cn(
                        "h-full flex-col",
                        isActive ? "flex" : "invisible absolute inset-0 flex",
                      )}
                    >
                      <SftpHostBar
                        hostId={tab.sftp.hostId}
                        busy={tab.sftp.phase === "connecting"}
                        onPick={(host) => switchSftpTabHost(tab.id, host)}
                      />
                      <div className="relative flex min-h-0 flex-1 flex-col">
                      {tab.sftp.phase === "picking" ? (
                        <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
                          在上面选一台主机打开 SFTP
                        </div>
                      ) : tab.sftp.sftpId && tab.sftp.phase === "ready" ? (
                        <SftpTabView
                          key={tab.sftp.sftpId}
                          sftpId={tab.sftp.sftpId}
                          hostId={tab.sftp.hostId}
                          active={view === "terminal" && tab.id === ws.activeTabId}
                          initialPaths={tab.sftp.paths}
                          onPathsChange={(paths) =>
                            sftpTabPaths.current.set(tab.sftp!.sftpId, paths)
                          }
                          onActiveCountChange={(count) =>
                            setSftpActive(tab.sftp!.sftpId, count)
                          }
                        />
                      ) : (
                        <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
                          {tab.sftp.phase === "failed"
                            ? `SFTP 打开失败：${tab.sftp.status}`
                            : "正在打开 SFTP…"}
                        </div>
                      )}
                      </div>
                    </div>
                  );
                }
                return (
                  <div
                    key={tab.id}
                    data-section="terminal-area"
                    // 非活动标签用 `invisible`（visibility: hidden）而不是 `hidden`
                    // （display: none）：前者保留布局盒，终端能量出真实行列数，
                    // AI 自动连接的后台标签才连得上（display:none 量不到尺寸就永远
                    // 不 onReady）。
                    className={cn(
                      "h-full flex-col",
                      isActive ? "relative flex" : "invisible absolute inset-0 flex",
                    )}
                    // 分屏之间的缝也用终端背景色，看起来是一整块
                    style={{ backgroundColor: terminalTheme.background }}
                  >
                    <TerminalLayer
                      tree={tab.tree}
                      panes={ws.panes}
                      onRatio={setSplitRatio}
                      renderPane={(paneId) => {
                        const session = ws.panes[paneId];
                        if (!session) return null;
                        return (
                          <PaneView
                            paneId={paneId}
                            session={session}
                            terminalTheme={terminalTheme}
                            focused={isActive && tab.focus === paneId}
                            onFocus={focusPane}
                            onReady={handlePaneReady}
                            onResize={handlePaneResize}
                            onReconnect={reconnect}
                            registerWriter={registerWriter}
                            onInput={handlePaneInput}
                            broadcasting={Boolean(tab.broadcast)}
                            split={tab.tree.kind !== "pane"}
                            onConnectHost={connectPaneTo}
                            onOpenSftp={openSftpTab}
                          />
                        );
                      }}
                    />
                    {isActive && quickBarOpen && (
                      <div className={cn("shrink-0", !barsOnTerminal && "bg-background")}>
                      <QuickCommandBar
                        commands={shownQuickCommands}
                        groups={quickGroups}
                        group={quickGroup}
                        onGroupChange={(group) =>
                          void updateSettingValue("quick_commands_group", group)
                        }
                        onRun={runQuickCommand}
                        onManage={() => openQuickCommandsPanel(quickCommands.length ? null : "new")}
                        onCreate={() => openQuickCommandsPanel("new")}
                        onEdit={(command) => openQuickCommandsPanel(command.id)}
                        onDelete={(command) =>
                          setConfirm({
                            title: `删除快捷命令「${command.name}」？`,
                            message: command.command.split(/\r?\n/)[0],
                            confirmLabel: "删除",
                            destructive: true,
                            onConfirm: () =>
                              saveQuickCommands(quickCommands.filter((item) => item.id !== command.id)),
                          })
                        }
                        broadcast={Boolean(tab.broadcast)}
                        paneCount={paneIds(tab.tree).length}
                        onToggleBroadcast={() => toggleBroadcast(tab.id)}
                      />
                      </div>
                    )}
                    {isActive && statsSettings && focusPaneOf?.origin.kind !== "empty" && (
                      <div className={cn("shrink-0", !barsOnTerminal && "bg-background")}>
                        <FocusStatsBar
                          sessionId={focusSessionId}
                          connected={focusPaneOf?.phase === "connected"}
                          overviewOpen={overviewOpen}
                          onToggleOverview={() => setOverviewOpen((open) => !open)}
                        />
                      </div>
                    )}
                    {isActive && statsSettings && overviewOpen && (
                      // 盖在终端右边，不挤终端（终端不用重新算行列数）；底部让出快捷命令栏（高度可拖）和状态栏
                      <OverviewAnchor quickBarOpen={quickBarOpen}>
                        <FocusOverviewPanel
                          sessionId={focusSessionId}
                          hostLabel={focusPaneOf?.origin.kind === "host" ? focusPaneOf.origin.label : null}
                          onClose={() => setOverviewOpen(false)}
                        />
                      </OverviewAnchor>
                    )}
                  </div>
                );
              })
            )}
          </div>

          {view === "settings" && (
            <SettingsPage
              onClose={hideSettings}
              onManageProxies={() => setSidePanel("proxies")}
              proxyPanelOpen={sidePanel === "proxies"}
            />
          )}
        </div>
        }
        ai={
        view === "terminal" && aiOpen && aiTab && (
          <div className={aiVisible ? "contents" : "hidden"} data-section="ai-slot">
          <AIChatPanel
            tabId={aiTab.id}
            sessions={aiSessions}
            hostStates={aiHostStates}
            firstUse={aiFirstUse}
            onMarkUsed={() => {
              setAiFirstUse(false);
              void settingsSet("ai_first_use_done", JSON.stringify(true)).catch(() => {});
              toast.info("命令的输出会发送给 opencode 所用的模型服务商");
            }}
            onOpenSettings={showSettings}
            onViewTarget={viewAiTarget}
            openSessionIds={openSessionIds}
            onAiActivity={handleAiActivity}
            onClose={() => setAiOpen(false)}
            reservedWidth={sftpVisible ? dockLayout.sftpWidth : 0}
            widthOverride={sftpVisible ? dockLayout.aiWidth : null}
          />
          </div>
        )
        }
        sftp={
        sftpVisible && (
          <SftpDockPanel
            hostLabel={focusedHostLabel}
            widthOverride={dockLayout.sftpWidth}
            reservedWidth={aiVisible ? dockLayout.aiWidth : 0}
            onClose={() => setSftpDockOpen(false)}
            onShowLocal={
              sftpDockLocalHidden && focusedHostId !== null && focusedDock?.phase === "ready"
                ? () => void updateSettingValue("layout.sftp_dock_local_hidden", false)
                : undefined
            }
            toolbar={
              // 编辑器标签没有终端可跟，不显示这个开关
              focusedHostId !== null && focusedDock?.phase === "ready" && !activeTab?.editor ? (
                <label
                  data-section="follow-toggle"
                  className="flex shrink-0 cursor-pointer items-center gap-2 border-b border-border/60 px-3 py-1.5 text-xs text-muted-foreground"
                  title="终端里 cd 之后，远端栏跟着换目录；在面板里手动换目录会自动关掉"
                >
                  <input
                    type="checkbox"
                    checked={focusedDock.follow}
                    onChange={(event) =>
                      patchDock(focusedHostId, { follow: event.target.checked })
                    }
                    className="accent-primary"
                  />
                  跟随终端目录
                </label>
              ) : undefined
            }
            onOpenInTab={
              focusedHostId !== null && focusedDock?.phase === "ready"
                ? () => undockToTab(focusedHostId, focusedHostLabel)
                : undefined
            }
          >
            {focusedHostId === null ? (
              <div className="p-4 text-sm text-muted-foreground">
                {activeTab?.editor
                  ? "这是本机文件，没有对应的服务器，文件面板不可用。"
                  : "这个终端没有主机记录（快速连接），不能用文件面板。"}
              </div>
            ) : focusedDock?.phase === "ready" && focusedDock.sftpId ? (
              <SftpTabView
                key={focusedDock.sftpId}
                sftpId={focusedDock.sftpId}
                hostId={focusedHostId}
                mode="dock"
                initialPaths={focusedDock.paths}
                onPathsChange={(paths) => patchDock(focusedHostId, { paths })}
                followPath={followPath}
                onManualRemoteNavigate={() => {
                  // 用户在面板里手动换了目录：停止跟随，免得来回跳。
                  if (docksRef.current[focusedHostId]?.follow) {
                    patchDock(focusedHostId, { follow: false });
                  }
                }}
                onActiveCountChange={(count) => setSftpActive(focusedDock.sftpId, count)}
              />
            ) : focusedDock?.phase === "failed" ? (
              <div className="space-y-2 p-4 text-sm text-destructive">
                <p className="break-all">SFTP 打开失败：{focusedDock.status}</p>
                <button
                  className="rounded border border-border px-2 py-1 text-xs text-foreground hover:bg-accent"
                  onClick={() => {
                    const hostId = focusedHostId;
                    docksRef.current = { ...docksRef.current };
                    delete docksRef.current[hostId];
                    setDocks(docksRef.current);
                    ensureDock(hostId);
                  }}
                >
                  重试
                </button>
              </div>
            ) : (
              <div className="p-4 text-sm text-muted-foreground">正在打开 SFTP…</div>
            )}
          </SftpDockPanel>
        )
        }
        after={
          <>
        {hostPanel && (
          // 凭据 / 代理面板盖在上面时只隐藏、不卸载：卸载会丢掉没保存的编辑
          <div className={sidePanel === "none" ? "contents" : "hidden"}>
          <HostDetailsPanel
            // 表单只在挂载时从 host 初始化：换主机或改成新建时要重新挂载，不然沿用上一台的内容
            key={hostPanel.host ? `host-${hostPanel.host.id}` : `new-${hostPanel.groupId ?? "none"}`}
            host={hostPanel.host}
            groupId={hostPanel.groupId}
            onClose={() => setHostPanel(null)}
            onSaved={(saved) => {
              setHostPanel(null);
              toast.success(`已保存主机「${saved.name}」`);
            }}
            onDelete={askDeleteHost}
            onError={(message) => toast.error(message)}
          />
          </div>
        )}
        {sidePanel === "identities" && (
          <KeychainPanel
            onClose={() => setSidePanel("none")}
            onError={(message) => toast.error(message)}
          />
        )}
        {sidePanel === "proxies" && (
          <ProxyPanel
            onClose={() => setSidePanel("none")}
            onError={(message) => toast.error(message)}
          />
        )}
        {sidePanel === "forwards" && (
          <PortForwardPanel
            hostId={forwardHostId}
            onClose={() => setSidePanel("none")}
            onError={(message) => toast.error(message)}
          />
        )}
        {sidePanel === "quickCommands" && (
          <QuickCommandsPanel
            commands={quickCommands}
            onChange={saveQuickCommands}
            initialEditId={quickEditId}
            onClose={() => setSidePanel("none")}
            onError={(message) => toast.error(message)}
          />
        )}
          </>
        }
      />

      {groupDialog && (
        <GroupFormDialog
          group={groupDialog.group}
          parentId={groupDialog.parentId}
          adoptUngrouped={groupDialog.adoptUngrouped}
          onClose={() => setGroupDialog(null)}
          onError={(message) => toast.error(message)}
        />
      )}
      {quickTarget !== null && (
        <QuickConnectDialog
          target={quickTarget}
          onCancel={() => setQuickTarget(null)}
          onConnect={(password) => {
            const target = quickTarget;
            setQuickTarget(null);
            setSidePanel("none");
            setHostPanel(null);
            openTabWithOrigin({ kind: "quick", target, password, label: target }, target);
          }}
        />
      )}
      {shownOwnerPrompt && (
        <SftpEditOwnerDialog
          key={shownOwnerPrompt.edit_id}
          event={shownOwnerPrompt}
          onCancel={() =>
            setLaterOwner((current) => new Set(current).add(shownOwnerPrompt.edit_id))
          }
          onChoose={(choice) => {
            setOwnerPrompts((current) =>
              current.filter((item) => item.edit_id !== shownOwnerPrompt.edit_id),
            );
            void sftpEditResolve(shownOwnerPrompt.edit_id, choice).catch((error: unknown) =>
              toast.error(errorText(error)),
            );
          }}
        />
      )}
      {shownEditConflict && (
        <SftpEditConflictDialog
          key={shownEditConflict.edit_id}
          event={shownEditConflict}
          onLater={() =>
            setLaterEdits((current) => new Set(current).add(shownEditConflict.edit_id))
          }
          onResolve={(action, name) =>
            void sftpEditResolve(shownEditConflict.edit_id, action, name).catch((error: unknown) =>
              toast.error(errorText(error)),
            )
          }
        />
      )}
      {hostKeyPrompt && (
        <HostKeyDialog
          key={hostKeyPrompt.req_id}
          prompt={hostKeyPrompt}
          onAnswer={handleHostKeyAnswer}
        />
      )}
      {/* key：换到下一条提问时重建对话框，上一条输入的密码不能带过去 */}
      {authPrompt && (
        <AuthDialog key={authPrompt.req_id} prompt={authPrompt} onAnswer={handleAuthAnswer} />
      )}
      {unsavedAsk && (
        <UnsavedDialog
          {...unsavedPrompt(unsavedAsk.files, unsavedAsk.reason, unsavedAsk.extra)}
          busy={unsavedAsk.busy}
          error={unsavedAsk.error}
          onSave={() => void answerUnsaved("save")}
          onDiscard={() => void answerUnsaved("discard")}
          onCancel={() => setUnsavedAsk(null)}
        />
      )}
      {confirm && (
        <ConfirmDialog
          open
          title={confirm.title}
          message={confirm.message}
          confirmLabel={confirm.confirmLabel}
          destructive={confirm.destructive}
          onOpenChange={(open) => !open && setConfirm(null)}
          onConfirm={() => {
            confirm.onConfirm();
            setConfirm(null);
          }}
        />
      )}
      {/* 同名文件对话框：不管文件面板在不在屏幕上，本窗口所有 SFTP 会话的提问都在这里排队弹 */}
      <SftpConflictHost sessionIds={conflictSessionIds} />
      <EditRecoveryHost
        sessions={sftpSessions}
        load={sftpEditLeftovers}
        act={handleLeftoverAct}
      />
      <UpdateBar getFrontendBusy={getUpdateBusy} />
      <SyncStatusBar onOpen={showSettings} />
    </div>
  );
}
