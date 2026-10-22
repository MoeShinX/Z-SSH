// Adapted from Netcatty components/sftp/SftpPaneView.tsx、SftpTransferQueue.tsx、SftpTransferItem.tsx（结构重写）
//! 一个 SFTP 标签：本地 / 远端双栏 + 传输队列（M3-2 第 2、3、4 节）。
//!
//! 父层（App）只给 `sftpId` 和 `hostId`，这一层自己管两栏的目录、选择、
//! 队列状态和对话框；标签关闭时的"还有 N 个传输未完成"确认由父层做
//! （要拦住关闭动作本身）。

import { ConfirmDialog } from "../ui/confirm-dialog";
import { renameRemoteSafely } from "../../state/sftpRename";
import {
  Copy,
  FolderTree,
  ListRestart,
  Pause,
  Play,
  RotateCcw,
  Info,
  Upload,
  Download,
  X,
} from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";


import {
  onSftpConflict,
  onSftpDelete,
  onSftpEdit,
  onSftpPasteProgress,
  onSftpTransfer,
  sftpConflictAnswer,
  sftpCreateFile,
  sftpDelete,
  sftpDeleteCancel,
  sftpList,
  sftpLocalCreateFile,
  sftpLocalDefaultDir,
  sftpLocalDelete,
  sftpLocalList,
  sftpLocalMkdir,
  sftpLocalRename,
  sftpMkdir,
  sftpRename,
  sftpSetPermissions,
  sftpTransferCancel,
  sftpTransferPause,
  sftpTransferResume,
  sftpTransferRetry,
  sftpTransferSubmit,
  sftpTransfers,
  sftpTransfersClear,
  type PasteRequest,
  type SftpConflictPrompt,
  type SftpDeleteEvent,
  type SftpEntry,
  type SftpTransfer,
} from "../../api/sftp";
import {
  editUploadNeedsRefresh,
  formatEta,
  formatSize,
  formatSpeed,
  queueSummary,
  sanitizeSplitRatio,
  splitRatioFromPointer,
  type FilterOptions,
  type SortOrder,
} from "../../state/sftpPane";
import { beginPanelDrag, endPanelDrag } from "../../state/panelWidths";
import { followTarget } from "../../state/followCwd";
import { toast } from "../ui/toast";
import { cn } from "../../lib/utils";
import { useSettings } from "../../stores/settings";
import { toEditorError } from "../../api/editor";
import { describeOpenError } from "../../state/editorModel";
import { openLocalInEditor, openRemoteInEditor } from "../../stores/editors";
import { useSftpClipboard } from "../../stores/sftpClipboard";
import { SftpPropertiesDialog } from "./SftpPropertiesDialog";
import { propertiesTargets, type PropertiesTarget } from "../../state/sftpProperties";
import {
  clipboardFrom,
  copyingText,
  pasteNotice,
  relayRetryRequest,
} from "../../state/sftpPaste";
import { sftpPaste, sftpPasteCancel } from "../../api/sftp";
import { Button } from "../ui/button";
import {
  SftpConflictDialog,
  SftpDeleteConfirm,
  SftpDeleteProgressDialog,
  SftpDownloadToDialog,
  SftpNameDialog,
  SftpPermissionsDialog,
  type ConflictAnswer,
} from "./SftpDialogs";
import SftpContextMenu from "./SftpContextMenu";
import SftpFilePane, { type PaneMenuRequest } from "./SftpFilePane";
import { buildMenu, pasteReason, type MenuActionId, type MenuFeatures } from "../../state/sftpMenu";
import {
  canBack,
  canForward,
  EMPTY_HISTORY,
  forget,
  ListingCache,
  step,
  visit,
  type NavHistory,
} from "../../state/sftpNav";

/** 列目录的原因：用户换目录（记历史）/ 后退前进（不改历史）/ 刷新（不用缓存）。 */
type NavMode = "visit" | "history" | "refresh";

const DEFAULT_ORDER: SortOrder = { key: "name", descending: false };

/** 粘贴任务号：进度事件和取消命令都按它配对（同一标签里可能连着粘好几次）。 */
let pasteSerial = 0;
function nextPasteId(): string {
  pasteSerial += 1;
  return `paste-${Date.now().toString(36)}-${pasteSerial}`;
}

const DIRECTION_LABELS: Record<SftpTransfer["direction"], string> = {
  upload: "上传",
  download: "下载",
  remote_copy: "复制（经本机中转）",
  local_copy: "本地复制",
};

/** 菜单里哪些功能已经接上（分步实施：没做完的先禁用，写明原因）。 */
const MENU_FEATURES: MenuFeatures = {
  builtinEditor: true,
  clipboard: true,
  properties: true,
};

interface DeleteRequest {
  side: "local" | "remote";
  entries: SftpEntry[];
}

interface NameRequest {
  kind: "mkdir" | "create" | "rename";
  side: "local" | "remote";
  dir: string;
  initial: string;
  existingNames: string[];
  /** rename 专用。 */
  from?: string;
}

interface RenameState {
  side: "local" | "remote";
  entry: SftpEntry;
}

/** 队列里非终态都算"未完成"（关标签 / 退出要确认的就是这些）。 */
export function transferIsActive(transfer: SftpTransfer): boolean {
  return !["done", "failed", "cancelled", "skipped"].includes(transfer.state);
}

export interface SftpPaths {
  local: string;
  remote: string;
}

export interface SftpWorkspaceProps {
  sftpId: string;
  /** 这台主机在主机库里的 id（内置编辑器据此判断"同一个文件"）。 */
  hostId?: number;
  /** `tab`：独立标签的左右双栏；`dock`：终端旁的面板，远端在上、本地在下（M3-3）。 */
  mode?: "tab" | "dock";
  /** 停靠 / 拆回时带过来的两栏目录；空字符串 = 用默认（本地家目录、远端家目录）。 */
  initialPaths?: SftpPaths;
  /** 两栏目录变了（父层记下来，停靠 / 拆回时原样带过去）。 */
  onPathsChange?: (paths: SftpPaths) => void;
  /** 活跃（非终态）传输数变了：App 在标题上显示进行中的数量。 */
  onActiveCountChange?: (count: number) => void;
  /** 跟随终端目录：远端栏要去的目录；和当前不同就跳过去（不算用户手动换目录）。 */
  followPath?: string | null;
  /** 用户在远端栏手动换了目录（面板据此关掉"跟随终端目录"）。 */
  onManualRemoteNavigate?: () => void;
  /**
   * 用内置编辑器打开文件的另一条路：文件窗口里没有编辑器标签，交给主窗口去开。
   * 不给就在本窗口打开（主窗口的 SFTP 标签、停靠面板）。
   */
  openInEditor?: { remote: (path: string) => void; local: (path: string) => void };
  /**
   * 这个视图现在看得见没有（标签在前台）。看不见时传输进度只记下来、不刷新界面，
   * 切回来一次显示最新状态；进行中的个数照常实时上报（关闭前的提醒要用）。
   */
  active?: boolean;
}

export default function SftpTabView({
  sftpId,
  hostId,
  mode = "tab",
  initialPaths,
  onPathsChange,
  onActiveCountChange,
  followPath = null,
  onManualRemoteNavigate,
  openInEditor,
  active = true,
}: SftpWorkspaceProps) {
  const isDock = mode === "dock";
  // ---------- 两栏的目录状态 ----------
  const [localPath, setLocalPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [localEntries, setLocalEntries] = useState<SftpEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = useState<SftpEntry[]>([]);
  const [localLoading, setLocalLoading] = useState(false);
  const [remoteLoading, setRemoteLoading] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  // "默认显示隐藏文件"（设置页）是两栏的初始开关（M3-2 第 5 节）。
  const initialShowHidden = useSettings((state) => state.sftp_show_hidden);
  const [localFilter, setLocalFilter] = useState<FilterOptions>({
    text: "",
    showHidden: initialShowHidden,
  });
  const [remoteFilter, setRemoteFilter] = useState<FilterOptions>({
    text: "",
    showHidden: initialShowHidden,
  });
  const [localOrder, setLocalOrder] = useState<SortOrder>(DEFAULT_ORDER);
  const [remoteOrder, setRemoteOrder] = useState<SortOrder>(DEFAULT_ORDER);
  const [focusedSide, setFocusedSide] = useState<"local" | "remote">("remote");

  // 两栏各自记住选中（拖动 / 右键 / 键盘都要用）。
  // 两栏选中的条目（文件栏按文件身份记，报上来的就是条目本身，不是行号）。
  // 两栏的选中只在操作时读（拖放、快捷键、菜单），界面不依赖它：只记在 ref 里。
  // 放进 state 的话每点选一次，整个 SFTP 页面、两栏和传输队列都要白白重新渲染一遍。
  const localSelectionRef = useRef<SftpEntry[]>([]);
  const remoteSelectionRef = useRef<SftpEntry[]>([]);
  const setLocalSelection = useCallback((entries: SftpEntry[]) => {
    localSelectionRef.current = entries;
  }, []);
  const setRemoteSelection = useCallback((entries: SftpEntry[]) => {
    remoteSelectionRef.current = entries;
  }, []);
  /** 拖起来的条目（引用存：合成的拖动事件里 React 状态还没来得及更新）。 */
  const draggedRef = useRef<SftpEntry[]>([]);

  // ---------- 队列（M3-2 第 3 节） ----------
  // 任务列表和进度事件都在 TransferQueue 里：进度一秒好几次，放在这里会让左右两栏
  // 跟着重新计算。这里只要"进行中的个数"（传输结束时刷新两栏、关闭前提醒）。
  const [activeCount, setActiveCount] = useState(0);
  const [queueOpen, setQueueOpen] = useState(true);
  const toggleQueue = useCallback(() => setQueueOpen((open) => !open), []);

  // ---------- 对话框 ----------
  const [conflict, setConflict] = useState<SftpConflictPrompt | null>(null);
  const [deleteRequest, setDeleteRequest] = useState<DeleteRequest | null>(null);
  const [deleteProgress, setDeleteProgress] = useState<{
    id: number;
    deleted: number;
    current: string;
  } | null>(null);
  const [nameRequest, setNameRequest] = useState<NameRequest | null>(null);
  const [renameRequest, setRenameRequest] = useState<RenameState | null>(null);
  /** 远端重命名遇到同名文件时等用户回答"覆盖服务器上的同名文件？"。 */
  const [overwriteAsk, setOverwriteAsk] = useState<{
    name: string;
    resolve: (overwrite: boolean) => void;
  } | null>(null);
  const [permissionRequest, setPermissionRequest] = useState<SftpEntry | null>(null);
  /** "下载到…"：要下载的远端条目。 */
  const [downloadTo, setDownloadTo] = useState<SftpEntry[] | null>(null);
  const [menuRequest, setMenuRequest] = useState<
    (PaneMenuRequest & { side: "local" | "remote" }) | null
  >(null);

  /** 最新的"刷新两栏"：事件订阅的 effect 只在挂载时建一次，里面直接用
   *  refreshBoth 会捕获挂载时的旧路径（空），刷新时把远端栏重新列成家目录。 */
  const refreshBothRef = useRef<() => void>(() => {});
  const refreshRemoteRef = useRef<() => void>(() => {});

  // ---------- 事件订阅 ----------
  useEffect(() => {
    const stops: Array<Promise<void>> = [];
    let cancelled = false;
    const unlistens: Array<() => void> = [];
    const track = (promise: Promise<unknown>) => {
      void promise.then((stop) => {
        if (cancelled) (stop as () => void)();
        else unlistens.push(stop as () => void);
      });
    };

    track(onSftpConflict((id, prompt) => {
      if (id !== sftpId) return;
      // 一次只显示一个（后端是按文件串行问的）。
      setConflict(prompt);
    }));
    track(onSftpPasteProgress((id, pasteId, doneFiles) => {
      if (id !== sftpId) return;
      if (pasteBusyRef.current?.pasteId !== pasteId) return;
      setPasteProgress({ doneFiles });
    }));
    track(onSftpDelete((id, event: SftpDeleteEvent) => {
      if (id !== sftpId) return;
      if (event.kind === "progress") {
        setDeleteProgress({ id: event.delete_id, deleted: event.deleted, current: event.current });
      } else {
        setDeleteProgress(null);
        if (event.cancelled) {
          toast.info(`已删除 ${event.deleted} 项，剩下的保留`);
        } else if (event.error) {
          toast.error(`删除失败：${event.error}`);
        } else {
          toast.success(`已删除 ${event.deleted} 项`);
        }
        refreshBothRef.current();
      }
    }));
    // 编辑器保存完（含另存为）：文件所在目录正是远端栏当前目录就重新列一遍。
    // 停靠面板和独立 SFTP 标签用的是同一个组件，两处都走这里。
    track(onSftpEdit((event) => {
      if (editUploadNeedsRefresh(event, sftpId, remotePathRef.current)) {
        refreshRemoteRef.current();
      }
    }));
    stops.push(Promise.resolve());
    return () => {
      cancelled = true;
      for (const stop of unlistens) stop();
    };
  }, [sftpId]);


  // 活跃（非终态）计数：标题上的进行中数量、关闭确认都读它。
  useEffect(() => {
    onActiveCountChange?.(activeCount);
  }, [activeCount, onActiveCountChange]);

  // ---------- 列目录 ----------
  // 换目录要"丝滑"：去过的目录先显示上次的列表，后台再读一遍悄悄换上；没去过的先留着
  // 旧列表（调暗），读到再换。连点几下时只认最后一次请求（序号），旧结果晚到不能盖掉新目录。
  // mode：visit = 用户换目录（记进历史）；history = 后退 / 前进（不改历史）；refresh = 刷新。
  const remoteCache = useRef(new ListingCache<SftpEntry[]>());
  const localCache = useRef(new ListingCache<SftpEntry[]>());
  const remoteSeq = useRef(0);
  const localSeq = useRef(0);
  const [remoteHistory, setRemoteHistory] = useState<NavHistory>(EMPTY_HISTORY);
  const [localHistory, setLocalHistory] = useState<NavHistory>(EMPTY_HISTORY);

  const loadRemote = useCallback(
    async (path: string, usePath: boolean, mode: NavMode = "visit") => {
      const seq = ++remoteSeq.current;
      setRemoteError(null);
      const cached = usePath && mode !== "refresh" ? remoteCache.current.get(path) : undefined;
      if (cached) {
        setRemoteEntries(cached);
        setRemotePath(path);
      }
      setRemoteLoading(true);
      try {
        // 空路径 = 家目录；后端把真实路径（realpath 的结果）一起带回来。
        const listing = await sftpList(sftpId, usePath ? path : "");
        if (seq !== remoteSeq.current) return;
        remoteCache.current.set(listing.path, listing.entries);
        setRemoteEntries(listing.entries);
        setRemotePath(listing.path);
        if (mode === "visit") setRemoteHistory((history) => visit(history, listing.path));
      } catch (error) {
        if (seq !== remoteSeq.current) return;
        remoteCache.current.delete(path);
        if (mode === "history") setRemoteHistory((history) => forget(history, path));
        setRemoteError(String(error));
      } finally {
        if (seq === remoteSeq.current) setRemoteLoading(false);
      }
    },
    [sftpId],
  );

  const loadLocal = useCallback(async (path: string, mode: NavMode = "visit") => {
    const seq = ++localSeq.current;
    setLocalError(null);
    const cached = mode !== "refresh" ? localCache.current.get(path) : undefined;
    if (cached) {
      setLocalEntries(cached);
      setLocalPath(path);
    }
    setLocalLoading(true);
    try {
      const entries = await sftpLocalList(path);
      if (seq !== localSeq.current) return;
      localCache.current.set(path, entries);
      setLocalEntries(entries);
      setLocalPath(path);
      if (mode === "visit") setLocalHistory((history) => visit(history, path));
    } catch (error) {
      if (seq !== localSeq.current) return;
      localCache.current.delete(path);
      if (mode === "history") setLocalHistory((history) => forget(history, path));
      setLocalError(String(error));
    } finally {
      if (seq === localSeq.current) setLocalLoading(false);
    }
  }, []);

  const refreshBoth = useCallback(() => {
    void loadRemote(remotePath || "", remotePath !== "", "refresh");
    void loadLocal(localPath, "refresh");
  }, [loadRemote, loadLocal, remotePath, localPath]);

  refreshBothRef.current = refreshBoth;
  refreshRemoteRef.current = () =>
    void loadRemote(remotePath || "", remotePath !== "", "refresh");

  // 初次加载：带过来的目录优先（停靠 / 拆回时两栏的目录都保留）；没有就是
  // 本地家目录（或此电脑）、远端家目录。
  useEffect(() => {
    if (initialPaths?.local) {
      void loadLocal(initialPaths.local);
    } else {
      void sftpLocalDefaultDir()
        .then((home) => loadLocal(home))
        .catch(() => loadLocal(""));
    }
    const remoteStart = followPath || initialPaths?.remote || "";
    const shown = (listing: { path: string; entries: SftpEntry[] }) => {
      remoteCache.current.set(listing.path, listing.entries);
      setRemoteEntries(listing.entries);
      setRemotePath(listing.path);
      setRemoteHistory((history) => visit(history, listing.path));
    };
    void sftpList(sftpId, remoteStart)
      .then(shown)
      .catch(() => {
        // 带过来的目录没了（被删、没权限）：退回家目录，不报错。
        if (!remoteStart) return;
        void sftpList(sftpId, "")
          .then(shown)
          .catch((error) => setRemoteError(String(error)));
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sftpId]);

  // 目录变了就告诉父层（停靠 / 拆回要带走）。
  const onPathsChangeRef = useRef(onPathsChange);
  onPathsChangeRef.current = onPathsChange;
  useEffect(() => {
    if (localPath === "" && remotePath === "") return;
    onPathsChangeRef.current?.({ local: localPath, remote: remotePath });
  }, [localPath, remotePath]);

  // 跟随终端目录：终端的目录变了，远端栏跟过去（用户手动换目录后父层把 followPath 置空）。
  const remotePathRef = useRef(remotePath);
  remotePathRef.current = remotePath;
  useEffect(() => {
    const target = followTarget(true, followPath, remotePathRef.current);
    if (target) void loadRemote(target, true);
  }, [followPath, loadRemote]);

  // 用户手动换远端目录：报给父层（面板据此停止跟随）。
  const browseRemote = useCallback(
    (path: string) => {
      onManualRemoteNavigate?.();
      void loadRemote(path, true);
    },
    [loadRemote, onManualRemoteNavigate],
  );

  /** 后退 / 前进一步。走不了就什么都不做。 */
  const stepRemote = useCallback(
    (delta: -1 | 1) => {
      const next = step(remoteHistory, delta);
      if (!next) return;
      onManualRemoteNavigate?.();
      setRemoteHistory(next.history);
      void loadRemote(next.path, true, "history");
    },
    [remoteHistory, loadRemote, onManualRemoteNavigate],
  );
  const stepLocal = useCallback(
    (delta: -1 | 1) => {
      const next = step(localHistory, delta);
      if (!next) return;
      setLocalHistory(next.history);
      void loadLocal(next.path, "history");
    },
    [localHistory, loadLocal],
  );

  // dock 的上下比例（远端占的份额）：拖动中本地覆盖，松手写回设置表。
  const storedRatio = useSettings((state) => state["layout.sftp_split_ratio"]);
  const updateSetting = useSettings((state) => state.update);
  const [dragRatio, setDragRatio] = useState<number | null>(null);
  const ratio = dragRatio ?? sanitizeSplitRatio(storedRatio);
  const splitRef = useRef<HTMLDivElement | null>(null);

  /** 栏开始拖动：把条目存进引用（drop 的时候用）。 */
  const handlePaneDragStart = useCallback((entries: SftpEntry[]) => {
    draggedRef.current = entries;
  }, []);
  /** 拖动结束：落点的回调（handleDropInto）在这之前已经跑过，这里只清暂存。 */
  const handlePaneDragEnd = useCallback(() => {
    draggedRef.current = [];
  }, []);

  // ---------- 传输 ----------
  const submitTransfer = useCallback(
    (direction: "upload" | "download", sources: string[], target: string) => {
      if (sources.length === 0) return;
      void sftpTransferSubmit(sftpId, direction, sources, target)
        .then(() => setQueueOpen(true))
        .catch((error) => toast.error(`提交传输失败：${String(error)}`));
    },
    [sftpId],
  );

  /** 两栏之间拖动 = 传输（M3-2 第 2 节）。
   *  条目来自 dragstart 时存的引用（合成的拖动事件里 React 状态还没更新），
   *  目标 = 落点的目录行，没指到就是这一栏的当前目录。 */
  const handleDropInto = useCallback(
    (side: "local" | "remote", target: { dirPath: string | null }) => {
      const picked = draggedRef.current;
      const fallbackDir = side === "remote" ? remotePath : localPath;
      const dir = target.dirPath ?? fallbackDir;
      if (picked.length === 0 || !dir) return;
      submitTransfer(
        side === "remote" ? "upload" : "download",
        picked.map((entry) => entry.path),
        dir,
      );
      draggedRef.current = [];
    },
    [remotePath, localPath, submitTransfer],
  );

  // 传输完成时刷新对应那一栏（看得到结果）。
  const prevActive = useRef(0);
  useEffect(() => {
    // 从"有活跃"变到"没有活跃"：可能有任务完成了，刷新两栏。
    if (prevActive.current > 0 && activeCount === 0) {
      refreshBoth();
    }
    prevActive.current = activeCount;
  }, [activeCount, refreshBoth]);

  // ---------- 文件操作 ----------
  const operateRemote = useCallback(
    async (action: () => Promise<void>, done: string) => {
      try {
        await action();
        toast.success(done);
        void loadRemote(remotePath, true, "refresh");
      } catch (error) {
        toast.error(String(error));
      }
    },
    [loadRemote, remotePath],
  );

  const operateLocal = useCallback(
    async (action: () => Promise<void>, done: string) => {
      try {
        await action();
        toast.success(done);
        void loadLocal(localPath, "refresh");
      } catch (error) {
        toast.error(String(error));
      }
    },
    [loadLocal, localPath],
  );

  const startDelete = useCallback(
    (side: "local" | "remote", entries: SftpEntry[]) => {
      const operable = entries.filter((entry) => entry.name_is_utf8);
      if (operable.length === 0) {
        toast.info("选中的条目名字不是 UTF-8，暂不支持操作");
        return;
      }
      if (operable.length < entries.length) {
        toast.info("名字不是 UTF-8 的条目已跳过");
      }
      setDeleteRequest({ side, entries: operable });
    },
    [],
  );

  const confirmDelete = useCallback(() => {
    const request = deleteRequest;
    setDeleteRequest(null);
    if (!request) return;
    const paths = request.entries.map((entry) => entry.path);
    if (request.side === "remote") {
      void sftpDelete(sftpId, paths)
        .then((id) => setDeleteProgress({ id, deleted: 0, current: "" }))
        .catch((error) => toast.error(String(error)));
    } else {
      void sftpLocalDelete(paths)
        .then((results) => {
          const failed = results.filter(([, err]) => err !== null);
          for (const [path, err] of failed) {
            toast.error(`删除 ${path} 失败：${err}`);
          }
          if (failed.length === 0) toast.success(`已删除 ${results.length} 项`);
          void loadLocal(localPath, "refresh");
        })
        .catch((error) => toast.error(String(error)));
    }
  }, [deleteRequest, sftpId, loadLocal, localPath]);

  // ---------- 冲突的回答 ----------
  const answerConflict = useCallback(
    (answer: ConflictAnswer) => {
      const prompt = conflict;
      setConflict(null);
      if (!prompt) return;
      void sftpConflictAnswer(
        prompt.conflict_id,
        answer.action,
        answer.applyToAll,
        answer.renameTo,
      ).catch((error) => toast.error(String(error)));
    },
    [conflict],
  );

  // ---------- 内置编辑器（M3-5） ----------
  /** 被内置编辑器拒绝（不是 UTF-8、二进制、太大、换行混用）时提示原因。 */
  const failBuiltin = useCallback(
    (error: unknown, path: string) => {
      const failure = describeOpenError(toEditorError(error), path.split(/[\\/]/).pop() ?? path);
      toast.info(`${failure.title}：${failure.message}`);
    },
    [],
  );
  const openBuiltinRemote = useCallback(
    (path: string) => {
      if (openInEditor) {
        openInEditor.remote(path);
        return;
      }
      void openRemoteInEditor(sftpId, path, hostId).catch((error) => failBuiltin(error, path));
    },
    [sftpId, hostId, failBuiltin, openInEditor],
  );
  const openBuiltinLocal = useCallback(
    (path: string) => {
      if (openInEditor) {
        openInEditor.local(path);
        return;
      }
      void openLocalInEditor(path).catch((error) => failBuiltin(error, path));
    },
    [failBuiltin, openInEditor],
  );

  // ---------- 双击 / Enter ----------
  const activateRemote = useCallback(
    (entry: SftpEntry) => {
      if (!entry.name_is_utf8) {
        toast.info("文件名编码不是 UTF-8，暂不支持操作");
        return;
      }
      // 目录直接进；符号链接看它指向的类型（断链不动）。
      const isEnterable =
        entry.kind === "dir" || (entry.kind === "symlink" && entry.target_kind === "dir");
      if (isEnterable) {
        browseRemote(entry.path);
        return;
      }
      // 双击文本文件 = 内置编辑器（M3-3 第 5 节 / M3-5 第 2 节）：下载到数据目录的临时目录、
      // 在编辑器标签里打开。二进制和超大文件后端拒绝，原因直接提示。
      const isFile =
        entry.kind === "file" || (entry.kind === "symlink" && entry.target_kind === "file");
      if (isFile) {
        openBuiltinRemote(entry.path);
      }
    },
    [browseRemote, openBuiltinRemote],
  );

  const activateLocal = useCallback(
    (entry: SftpEntry) => {
      if (entry.kind === "dir" || entry.kind === "drive") {
        void loadLocal(entry.path);
        return;
      }
      if (entry.kind === "file" || (entry.kind === "symlink" && entry.target_kind === "file")) {
        openBuiltinLocal(entry.path);
      }
    },
    [loadLocal, openBuiltinLocal],
  );

  // ---------- 复制 / 粘贴（M3-5 第 3 节） ----------
  const clip = useSftpClipboard((state) => state.content);
  const setClip = useSftpClipboard((state) => state.set);
  const menuClipboard = clip
    ? { side: clip.side, count: clip.items.length }
    : null;
  const [pasteBusy, setPasteBusy] = useState<{
    startedAt: number;
    pasteId: string;
    side: "local" | "remote";
  } | null>(null);
  const [pasteElapsed, setPasteElapsed] = useState(0);
  /** 服务器端复制的进度（第 N 个文件）。 */
  const [pasteProgress, setPasteProgress] = useState<{ doneFiles: number } | null>(null);
  /** 已经点了取消，等后端收尾（cp 关通道、删临时文件）。 */
  const [pasteCancelling, setPasteCancelling] = useState(false);
  const pasteBusyRef = useRef(pasteBusy);
  pasteBusyRef.current = pasteBusy;
  useEffect(() => {
    if (!pasteBusy) return;
    setPasteElapsed(0);
    const timer = window.setInterval(
      () => setPasteElapsed(Math.floor((Date.now() - pasteBusy.startedAt) / 1000)),
      1000,
    );
    return () => window.clearInterval(timer);
  }, [pasteBusy]);

  const copyToClip = useCallback(
    (side: "local" | "remote", entries: SftpEntry[]) => {
      const content = clipboardFrom(side, sftpId, entries);
      if (!content) {
        toast.info("选中的项目现在不能复制（文件名编码不是 UTF-8）");
        return;
      }
      setClip(content);
      toast.info(`已复制 ${content.items.length} 项`);
    },
    [sftpId, setClip],
  );

  /** 真正发起一次粘贴（首次、或者 cp 失败后点"改为经本机中转重试"都走这里）。 */
  const runPasteRef = useRef<(request: PasteRequest) => void>(() => {});
  const runPaste = useCallback(
    (request: PasteRequest) => {
      setPasteBusy({ startedAt: Date.now(), pasteId: request.paste_id, side: request.side });
      setPasteProgress(null);
      setPasteCancelling(false);
      void sftpPaste(sftpId, request)
        .then((outcome) => {
          const notice = pasteNotice(outcome);
          const retry = relayRetryRequest(request, outcome, nextPasteId());
          if (retry) {
            // cp 跑了但失败：结果里给按钮，点了才换成中转（M3-5b 第 4 项）。
            toast.error(notice.text, {
              duration: 0,
              actionLabel: "改为经本机中转重试",
              onClick: () => runPasteRef.current(retry),
            });
          } else if (notice.type === "error") {
            toast.error(notice.text);
          } else if (notice.type === "success") {
            toast.success(notice.text);
          } else {
            toast.info(notice.text);
          }
          if (outcome.job_ids.length > 0) setQueueOpen(true);
          refreshBothRef.current();
        })
        .catch((error) => toast.error(`粘贴失败：${String(error)}`))
        .finally(() => {
          setPasteBusy(null);
          setPasteProgress(null);
          setPasteCancelling(false);
        });
    },
    [sftpId],
  );
  runPasteRef.current = runPaste;

  const pasteInto = useCallback(
    (side: "local" | "remote", targetDir: string) => {
      const content = useSftpClipboard.getState().content;
      const reason = pasteReason({
        side,
        currentDir: targetDir,
        clipboard: content
          ? { side: content.side, count: content.items.length }
          : null,
        features: MENU_FEATURES,
      });
      if (reason || !content) {
        toast.info(reason ?? "剪贴板是空的");
        return;
      }
      runPaste({
        paste_id: nextPasteId(),
        side,
        source_sftp_id: side === "remote" ? content.sftpId : undefined,
        items: content.items,
        target_dir: targetDir,
      });
    },
    [runPaste],
  );
  const pasteHere = useCallback(
    (side: "local" | "remote") => pasteInto(side, side === "remote" ? remotePath : localPath),
    [pasteInto, remotePath, localPath],
  );

  // ---------- 属性（M3-5 第 4 节） ----------
  const [propertiesRequest, setPropertiesRequest] = useState<{
    side: "local" | "remote";
    targets: PropertiesTarget[];
  } | null>(null);
  const showProperties = useCallback(
    (side: "local" | "remote", entries: SftpEntry[]) => {
      const targets = propertiesTargets(entries, side === "remote" ? remotePath : localPath);
      if (targets.length === 0) {
        toast.info("先进入一个文件夹");
        return;
      }
      setPropertiesRequest({ side, targets });
    },
    [remotePath, localPath],
  );

  // ---------- 右键菜单 ----------
  // 菜单的内容、禁用规则和禁用原因由 `buildMenu` 按上下文算出来（`state/sftpMenu.ts`，有单元测试）。
  const closeMenu = () => setMenuRequest(null);

  const menuNodes = menuRequest
    ? buildMenu({
        side: menuRequest.side,
        target: menuRequest.target,
        entries: menuRequest.entries,
        currentDir: menuRequest.side === "remote" ? remotePath : localPath,
        clipboard: menuClipboard,
        features: MENU_FEATURES,
      })
    : [];

  const runMenuAction = (id: MenuActionId) => {
    const request = menuRequest;
    closeMenu();
    if (!request) return;
    const entries = request.entries;
    const side = request.side;
    switch (id) {
      case "open":
        if (entries.length === 1) {
          if (side === "remote") activateRemote(entries[0]);
          else activateLocal(entries[0]);
        }
        break;
      case "edit-builtin":
        if (entries.length === 1) {
          if (side === "remote") openBuiltinRemote(entries[0].path);
          else openBuiltinLocal(entries[0].path);
        }
        break;
      case "transfer": {
        // 上传 / 下载到另一栏当前目录。
        const picked =
          entries.length > 0
            ? entries
            : side === "local"
              ? localSelectionRef.current
              : [];
        if (picked.length === 0) break;
        if (side === "local") {
          submitTransfer("upload", picked.map((e) => e.path), remotePath);
        } else {
          submitTransfer("download", picked.map((e) => e.path), localPath);
        }
        break;
      }
      case "download-to":
        if (entries.length > 0 && side === "remote") setDownloadTo(entries);
        break;
      case "rename":
        if (entries.length === 1 && entries[0].name_is_utf8) {
          setRenameRequest({ side, entry: entries[0] });
        }
        break;
      case "delete":
        startDelete(side, entries);
        break;
      case "mkdir":
      case "create":
        setNameRequest({
          kind: id,
          side,
          dir: side === "remote" ? remotePath : localPath,
          initial: "",
          existingNames: [],
        });
        break;
      case "permissions":
        if (entries.length === 1 && side === "remote") {
          setPermissionRequest(entries[0]);
        }
        break;
      case "copy-path": {
        const text = entries.map((entry) => entry.path).join("\n");
        void navigator.clipboard
          ?.writeText(text)
          .then(() => toast.success("路径已复制"))
          .catch(() => {});
        break;
      }
      case "refresh":
        if (side === "remote") void loadRemote(remotePath, true, "refresh");
        else void loadLocal(localPath, "refresh");
        break;
      case "copy":
        copyToClip(side, entries);
        break;
      case "paste":
        // 右键在文件夹上粘贴 = 粘贴进那个文件夹（和资源管理器一致）；其余都是当前目录。
        if (request.target === "items" && entries.length === 1 && entries[0].kind === "dir") {
          pasteInto(side, entries[0].path);
        } else {
          pasteHere(side);
        }
        break;
      case "properties":
        showProperties(side, entries);
        break;
    }
  };

  const localSide = (
    <SftpFilePane
      side="local"
      path={localPath}
      loading={localLoading}
      error={localError}
      entries={localEntries}
      filter={localFilter}
      order={localOrder}
      focused={focusedSide === "local"}
      onPathChange={(path) => void loadLocal(path)}
      onFilterChange={(patch) => setLocalFilter((current) => ({ ...current, ...patch }))}
      onOrderChange={setLocalOrder}
      onRefresh={() => void loadLocal(localPath, "refresh")}
      onFocusPane={() => setFocusedSide("local")}
      onActivate={activateLocal}
      onContextMenu={(request) => setMenuRequest({ ...request, side: "local" })}
      onDropInto={(target) => handleDropInto("local", target)}
      onDragStart={handlePaneDragStart}
      onDragEnd={handlePaneDragEnd}
      onDeleteSelection={(entries) => startDelete("local", entries)}
      onShowProperties={(entries) => showProperties("local", entries)}
      onCopySelection={(entries) => copyToClip("local", entries)}
      onPasteHere={() => pasteHere("local")}
      onRenameSelection={(entry) => {
        if (entry.name_is_utf8) setRenameRequest({ side: "local", entry });
      }}
      onSelectionChange={setLocalSelection}
      onBrowse={(path) => void loadLocal(path)}
      compact={isDock}
      canBack={canBack(localHistory)}
      canForward={canForward(localHistory)}
      onBack={() => stepLocal(-1)}
      onForward={() => stepLocal(1)}
    />
  );

  const remoteSide = (
    <SftpFilePane
      side="remote"
      path={remotePath}
      loading={remoteLoading}
      error={remoteError}
      entries={remoteEntries}
      filter={remoteFilter}
      order={remoteOrder}
      focused={focusedSide === "remote"}
      onPathChange={browseRemote}
      onFilterChange={(patch) => setRemoteFilter((current) => ({ ...current, ...patch }))}
      onOrderChange={setRemoteOrder}
      onRefresh={() => void loadRemote(remotePath, true, "refresh")}
      onFocusPane={() => setFocusedSide("remote")}
      onActivate={activateRemote}
      onContextMenu={(request) => setMenuRequest({ ...request, side: "remote" })}
      onDropInto={(target) => handleDropInto("remote", target)}
      onDragStart={handlePaneDragStart}
      onDragEnd={handlePaneDragEnd}
      onExternalDrop={(paths, dirPath) => {
        // 资源管理器拖进来 = 上传到当前目录（落在目录行上就是那个目录），走同一个传输队列和冲突处理。
        const dir = dirPath ?? remotePath;
        if (!dir) return;
        submitTransfer("upload", paths, dir);
      }}
      onDeleteSelection={(entries) => startDelete("remote", entries)}
      onShowProperties={(entries) => showProperties("remote", entries)}
      onCopySelection={(entries) => copyToClip("remote", entries)}
      onPasteHere={() => pasteHere("remote")}
      onRenameSelection={(entry) => {
        if (entry.name_is_utf8) setRenameRequest({ side: "remote", entry });
      }}
      onSelectionChange={setRemoteSelection}
      onBrowse={browseRemote}
      compact={isDock}
      canBack={canBack(remoteHistory)}
      canForward={canForward(remoteHistory)}
      onBack={() => stepRemote(-1)}
      onForward={() => stepRemote(1)}
    />
  );


  const finishSplitDrag = (next: number) => {
    setDragRatio(null);
    endPanelDrag();
    void updateSetting("layout.sftp_split_ratio", Number(next.toFixed(4)));
  };

  return (
    <div
      data-sftp-workspace={mode}
      className={cn("flex min-h-0 flex-1 flex-col p-2", isDock ? "gap-1.5 p-1.5" : "gap-2")}
    >
      {isDock ? (
        /* 面板：远端在上、本地在下，中间的分隔线可以拖（双击恢复平分，比例记住） */
        <div ref={splitRef} className="flex min-h-0 flex-1 flex-col">
          <div className="flex min-h-0 min-w-0" style={{ flex: `${ratio} 1 0%` }}>
            {remoteSide}
          </div>
          <div
            role="separator"
            aria-orientation="horizontal"
            data-section="sftp-split-handle"
            title="拖动调整上下比例（双击恢复平分）"
            className="group relative h-2 shrink-0 cursor-row-resize"
            onPointerDown={(event) => {
              event.preventDefault();
              beginPanelDrag();
              try {
                event.currentTarget.setPointerCapture(event.pointerId);
              } catch {
                // jsdom 没有 pointer capture
              }
              setDragRatio(ratio);
            }}
            onPointerMove={(event) => {
              if (dragRatio === null) return;
              const box = splitRef.current?.getBoundingClientRect();
              if (!box) return;
              setDragRatio(splitRatioFromPointer(event.clientY, box.top, box.height));
            }}
            onPointerUp={() => {
              if (dragRatio !== null) finishSplitDrag(dragRatio);
            }}
            onPointerCancel={() => {
              if (dragRatio !== null) finishSplitDrag(dragRatio);
            }}
            onDoubleClick={() => {
              setDragRatio(null);
              void updateSetting("layout.sftp_split_ratio", 0.5);
            }}
          >
            <span className="pointer-events-none absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2 bg-transparent transition-colors group-hover:bg-primary/60" />
          </div>
          <div className="flex min-h-0 min-w-0" style={{ flex: `${1 - ratio} 1 0%` }}>
            {localSide}
          </div>
        </div>
      ) : (
        /* 双栏 */
        <div className="flex min-h-0 flex-1 gap-2">
          {localSide}
          {remoteSide}
        </div>
      )}

      {/* 传输队列（M3-2 第 3 节）：可展开 / 收起；标题显示进行中的数量 */}
      <div className="shrink-0">
        <TransferQueue
          sftpId={sftpId}
          visible={active}
          compact={isDock}
          open={queueOpen}
          onToggle={toggleQueue}
          onActiveCountChange={setActiveCount}
        />
      </div>

      {/* 右键菜单（自绘；位置按菜单实际尺寸摆，不超出屏幕） */}
      {menuRequest && (
        <SftpContextMenu
          nodes={menuNodes}
          x={menuRequest.x}
          y={menuRequest.y}
          onSelect={runMenuAction}
          onClose={closeMenu}
        />
      )}

      {/* 对话框们 */}
      {conflict && (
        <SftpConflictDialog
          prompt={conflict}
          onAnswer={answerConflict}
          onDismissAsSkip={() =>
            answerConflict({ action: "skip", applyToAll: false })
          }
        />
      )}
      {deleteRequest && (
        <SftpDeleteConfirm
          count={deleteRequest.entries.length}
          names={deleteRequest.entries.map((entry) => entry.name)}
          hasFolder={deleteRequest.entries.some(
            (entry) => entry.kind === "dir" || entry.kind === "drive",
          )}
          isRemote={deleteRequest.side === "remote"}
          onConfirm={confirmDelete}
          onCancel={() => setDeleteRequest(null)}
        />
      )}
      {propertiesRequest && (
        <SftpPropertiesDialog
          sftpId={sftpId}
          side={propertiesRequest.side}
          targets={propertiesRequest.targets}
          onClose={() => setPropertiesRequest(null)}
        />
      )}
      {pasteBusy && !conflict && (
        <div
          role="status"
          data-testid="paste-busy"
          className="absolute bottom-2 left-1/2 z-40 flex -translate-x-1/2 items-center gap-2 rounded-md border border-border bg-card px-3 py-1.5 text-xs text-foreground shadow"
        >
          <span>{copyingText(pasteElapsed, pasteProgress?.doneFiles)}</span>
          {pasteBusy.side === "remote" && (
            <Button
              variant="outline"
              size="sm"
              className="h-6 px-2 text-xs"
              disabled={pasteCancelling}
              onClick={() => {
                setPasteCancelling(true);
                void sftpPasteCancel(sftpId, pasteBusy.pasteId).catch(() => {
                  // 已经结束了：结果会从 sftpPaste 那边回来，这里不用再提示。
                  setPasteCancelling(false);
                });
              }}
            >
              {pasteCancelling ? "取消中…" : "取消"}
            </Button>
          )}
        </div>
      )}
      {deleteProgress && (
        <SftpDeleteProgressDialog
          deleted={deleteProgress.deleted}
          current={deleteProgress.current}
          onCancel={() => void sftpDeleteCancel(sftpId, deleteProgress.id)}
        />
      )}
      {nameRequest && (
        <SftpNameDialog
          title={nameRequest.kind === "mkdir" ? "新建文件夹" : "新建文件"}
          description={nameRequest.dir || "当前目录"}
          initial={nameRequest.initial}
          isRemote={nameRequest.side === "remote"}
          existingNames={nameRequest.existingNames}
          onConfirm={(name) => {
            const request = nameRequest;
            setNameRequest(null);
            if (!request) return;
            const target = joinPath(request.side, request.dir, name);
            if (request.kind === "mkdir") {
              if (request.side === "remote") {
                void operateRemote(() => sftpMkdir(sftpId, target), "文件夹已创建");
              } else {
                void operateLocal(() => sftpLocalMkdir(target), "文件夹已创建");
              }
            } else if (request.side === "remote") {
              void operateRemote(() => sftpCreateFile(sftpId, target), "文件已创建");
            } else {
              void operateLocal(() => sftpLocalCreateFile(target), "文件已创建");
            }
          }}
          onCancel={() => setNameRequest(null)}
        />
      )}
      {overwriteAsk && (
        <ConfirmDialog
          open
          title="覆盖服务器上的同名文件？"
          message={`服务器上已经有一个叫 ${overwriteAsk.name} 的文件，覆盖后原来的内容就没有了。`}
          confirmLabel="覆盖"
          destructive
          onOpenChange={(open) => {
            if (!open) {
              overwriteAsk.resolve(false);
              setOverwriteAsk(null);
            }
          }}
          onConfirm={() => {
            overwriteAsk.resolve(true);
            setOverwriteAsk(null);
          }}
        />
      )}
      {renameRequest && (
        <SftpNameDialog
          title="重命名"
          description={renameRequest.entry.path}
          initial={renameRequest.entry.name}
          isRemote={renameRequest.side === "remote"}
          existingNames={
            (renameRequest.side === "remote" ? remoteEntries : localEntries)
              .map((entry) => entry.name)
              .filter((name) => name !== renameRequest.entry.name)
          }
          onConfirm={(name) => {
            const request = renameRequest;
            setRenameRequest(null);
            if (!request) return;
            const target = siblingPath(request.entry.path, name);
            if (request.side === "remote") {
              // 先不覆盖地试：界面缓存的文件列表可能过期，重名要以服务器的回答为准。
              void (async () => {
                try {
                  const result = await renameRemoteSafely({
                    rename: (overwrite) =>
                      sftpRename(sftpId, request.entry.path, target, overwrite),
                    confirmOverwrite: () =>
                      new Promise<boolean>((resolve) => setOverwriteAsk({ name, resolve })),
                  });
                  if (result === "renamed") {
                    toast.success("已重命名");
                    void loadRemote(remotePath, true, "refresh");
                  }
                } catch (error) {
                  toast.error(String(error));
                }
              })();
            } else {
              void operateLocal(
                () => sftpLocalRename(request.entry.path, target),
                "已重命名",
              );
            }
          }}
          onCancel={() => setRenameRequest(null)}
        />
      )}
      {downloadTo && (
        <SftpDownloadToDialog
          count={downloadTo.length}
          initialDir={localPath}
          onCancel={() => setDownloadTo(null)}
          onConfirm={async (dir) => {
            // 目录要存在：先列一次，列不出来就把原因留在对话框里。
            try {
              await sftpLocalList(dir);
            } catch (error) {
              return `这个目录用不了：${String(error)}`;
            }
            submitTransfer(
              "download",
              downloadTo.map((entry) => entry.path),
              dir,
            );
            setDownloadTo(null);
            return null;
          }}
        />
      )}
      {permissionRequest && (
        <SftpPermissionsDialog
          name={permissionRequest.name}
          initialMode={permissionRequest.permissions}
          isDir={permissionRequest.kind === "dir"}
          onApply={(mode, recursive) => {
            const entry = permissionRequest;
            setPermissionRequest(null);
            if (!entry) return;
            void operateRemote(
              () => sftpSetPermissions(sftpId, entry.path, mode, recursive),
              "权限已修改",
            );
          }}
          onCancel={() => setPermissionRequest(null)}
        />
      )}
    </div>
  );
}

function joinPath(side: "local" | "remote", dir: string, name: string): string {
  if (side === "remote") {
    const base = dir === "~" ? "" : dir.replace(/\/$/, "");
    return `${base}/${name}`;
  }
  const base = dir.replace(/[\\/]$/, "");
  return `${base}\\${name}`;
}

function siblingPath(path: string, name: string): string {
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (slash < 0) return name;
  return `${path.slice(0, slash + 1)}${name}`;
}

/**
 * 传输队列：任务列表和进度事件只在这里更新，不带着左右两栏重新渲染。
 * 收到一个任务的更新只换掉那一条（其余任务对象不变，行组件可以跳过重新渲染）；
 * 顺序按任务 id，只在新增任务时插入。
 * 整个组件也 memo：父组件因为点选、过滤、焦点之类重新渲染时，队列不跟着把每一行再过一遍
 * （props 都是稳定的：开关回调用 useCallback，活动数回调是 setState）。
 */
const TransferQueue = memo(function TransferQueue({
  sftpId,
  visible,
  compact,
  open,
  onToggle,
  onActiveCountChange,
}: {
  sftpId: string;
  /** 看不见（后台标签）：只记最新状态，不刷新界面。 */
  visible: boolean;
  compact: boolean;
  open: boolean;
  onToggle: () => void;
  onActiveCountChange: (count: number) => void;
}) {
  const [transfers, setTransfers] = useState<SftpTransfer[]>([]);
  // 最新的任务列表（看不见时也一直更新），和已经上报的进行中个数
  const latest = useRef<SftpTransfer[]>([]);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const reportCount = useRef(onActiveCountChange);
  reportCount.current = onActiveCountChange;
  const reported = useRef(-1);
  // 正在等快照的请求（挂载时一次、"清除已完成"一次）：等的期间收到事件的任务记在各自的集合里
  const windows = useRef(new Set<Set<number>>());
  const apply = useCallback((next: SftpTransfer[]) => {
    latest.current = next;
    const count = next.filter(transferIsActive).length;
    if (count !== reported.current) {
      reported.current = count;
      reportCount.current(count);
    }
    if (visibleRef.current) setTransfers(next);
  }, []);
  // 切回前台：一次显示最新状态（不回放中间的进度）
  useEffect(() => {
    if (visible) setTransfers(latest.current);
  }, [visible]);

  useEffect(() => {
    let cancelled = false;
    let stop: (() => void) | null = null;
    void onSftpTransfer((id, transfer) => {
      if (id !== sftpId) return;
      for (const touched of windows.current) touched.add(transfer.id);
      apply(upsertTransfer(latest.current, transfer));
    }).then((unlisten) => {
      if (cancelled) unlisten();
      else stop = unlisten;
    });
    // 拿一次快照（标签挂载时可能已经有任务在跑）；等快照期间收到事件的任务以事件为准
    const touched = new Set<number>();
    windows.current.add(touched);
    void sftpTransfers(sftpId)
      .then((items) => {
        if (!cancelled) apply(reconcileSnapshot(latest.current, items, touched, 0));
      })
      .catch(() => {})
      .finally(() => windows.current.delete(touched));
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [sftpId, apply]);

  const activeCount = useMemo(() => transfers.filter(transferIsActive).length, [transfers]);

  const summary = queueSummary(transfers);

  return (
    <>
        <button
          onClick={onToggle}
          className="flex w-full items-center gap-2 rounded-md border border-border/50 px-3 py-1.5 text-left text-[13px] hover:bg-accent/50"
        >
          <FolderTree size={14} />
          <span>传输队列</span>
          {compact && summary ? (
            <span
              data-section="queue-summary"
              className="rounded-full bg-primary px-1.5 text-[11px] text-primary-foreground"
            >
              {summary.text}
            </span>
          ) : (
            activeCount > 0 && (
              <span className="rounded-full bg-primary px-1.5 text-[11px] text-primary-foreground">
                {activeCount} 个进行中
              </span>
            )
          )}
          <span className="ml-auto text-xs text-muted-foreground">
            {open ? "收起" : `共 ${transfers.length} 个任务`}
          </span>
        </button>
        {open && (
          <div className="mt-1 max-h-56 overflow-y-auto rounded-md border border-border/50">
            {transfers.length === 0 ? (
              <div className="px-3 py-3 text-sm text-muted-foreground">
                还没有传输任务。在两栏之间拖动，或者右键选"上传 / 下载"。
              </div>
            ) : (
              <>
                {transfers.map((transfer) => (
                  <TransferRow
                    key={transfer.id}
                    sftpId={sftpId}
                    transfer={transfer}
                    compact={compact}
                  />
                ))}
                <div className="flex justify-end border-t border-border/50 px-2 py-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      // 清除后拿快照替换列表；等的期间有事件的任务以事件为准（不然刚完成的会被
                      // 快照里的"传输中"盖掉，之后再没有事件，活动数卡住不归零）
                      const touched = new Set<number>();
                      const newestBefore = latest.current.reduce((max, item) => Math.max(max, item.id), 0);
                      windows.current.add(touched);
                      void sftpTransfersClear(sftpId)
                        .then(() => sftpTransfers(sftpId))
                        .then((items) => apply(reconcileSnapshot(latest.current, items, touched, newestBefore)))
                        .catch(() => {})
                        .finally(() => windows.current.delete(touched));
                    }}
                  >
                    <ListRestart size={13} className="mr-1" />
                    清除已完成
                  </Button>
                </div>
              </>
            )}
          </div>
        )}
    </>
  );
});

/**
 * 快照和事件合并。前端开始监听以后，每个任务的状态变化都会发事件（完成、失败、重试这类必发，
 * 只有进度是节流的），所以**等快照期间收到过事件的任务（`touched`）以事件为准**：
 * 快照可能是在那条事件之前生成的。唯一例外：同一状态下都还在跑、快照的已传字节更多——
 * 那只是节流没发出来的进度，用快照的。没收到事件的任务以快照为准（快照里没有 = 已被清除）。
 * 只在事件里有、快照里没有的：还在进行的保留；已结束的只保留 id 大于 `keepNewAbove` 的
 * （请求之后才建的新任务），否则就是刚被清除的旧任务，不让它复活。
 *
 * 不靠"终态优先"或"字节多的优先"判断新旧：重试会把同一个任务改回排队、字节归零。
 */
export function reconcileSnapshot(
  current: SftpTransfer[],
  snapshot: SftpTransfer[],
  touched: ReadonlySet<number>,
  keepNewAbove: number,
): SftpTransfer[] {
  const byId = new Map(current.map((transfer) => [transfer.id, transfer]));
  const inSnapshot = new Set<number>();
  const result: SftpTransfer[] = [];
  for (const fromSnapshot of snapshot) {
    inSnapshot.add(fromSnapshot.id);
    const fromEvent = touched.has(fromSnapshot.id) ? byId.get(fromSnapshot.id) : undefined;
    const snapshotAhead =
      fromEvent !== undefined &&
      fromEvent.state === fromSnapshot.state &&
      transferIsActive(fromEvent) &&
      fromSnapshot.done_bytes > fromEvent.done_bytes;
    result.push(fromEvent && !snapshotAhead ? fromEvent : fromSnapshot);
  }
  for (const id of touched) {
    if (inSnapshot.has(id)) continue;
    const fromEvent = byId.get(id);
    if (fromEvent && (transferIsActive(fromEvent) || id > keepNewAbove)) result.push(fromEvent);
  }
  return result.sort((a, b) => a.id - b.id);
}

/** 收到一个任务的新状态：已有的原地替换（其余对象不动），新任务按 id 插到合适位置。 */
export function upsertTransfer(current: SftpTransfer[], transfer: SftpTransfer): SftpTransfer[] {
  const index = current.findIndex((item) => item.id === transfer.id);
  if (index >= 0) {
    if (current[index] === transfer) return current;
    const next = current.slice();
    next[index] = transfer;
    return next;
  }
  const at = current.findIndex((item) => item.id > transfer.id);
  if (at < 0) return [...current, transfer];
  return [...current.slice(0, at), transfer, ...current.slice(at)];
}

/** 队列里的一行（M3-2 第 3 节：方向、名称、来源→目标、进度、速度、剩余、状态、操作）。
 *  memo：别的任务更新时这一行的 transfer 对象没变，不用重新渲染。 */
const TransferRow = memo(function TransferRow({
  sftpId,
  transfer,
  compact = false,
}: {
  sftpId: string;
  transfer: SftpTransfer;
  /** 停靠面板里窄：来源→目标、剩余时间这些列不显示。 */
  compact?: boolean;
}) {
  const isDir = transfer.total_files > 1;
  const name = lastSegment(isDir ? transfer.source : transfer.current ?? transfer.source);
  const percent =
    transfer.total_bytes > 0
      ? Math.min(100, (transfer.done_bytes / transfer.total_bytes) * 100)
      : transfer.state === "done"
        ? 100
        : 0;
  const active = transferIsActive(transfer);
  const [notesOpen, setNotesOpen] = useState(false);

  const stateLabel: Record<SftpTransfer["state"], string> = {
    queued: "排队中",
    planning: "准备中",
    running: "传输中",
    paused: "已暂停",
    waiting_reconnect: "等待重连",
    done: "已完成",
    failed: "失败",
    cancelled: "已取消",
    skipped: "已跳过",
  };

  return (
    <div className="border-b border-border/40 px-2 py-1.5 last:border-b-0">
      <div className="flex items-center gap-2 text-[13px]">
        <span title={DIRECTION_LABELS[transfer.direction]}>
          {transfer.direction === "upload" ? (
            <Upload size={13} className="text-sky-400" />
          ) : transfer.direction === "download" ? (
            <Download size={13} className="text-emerald-500" />
          ) : (
            <Copy size={13} className="text-amber-400" />
          )}
        </span>
        <span
          className={cn("truncate", compact ? "min-w-0 flex-1" : "w-44 shrink-0")}
          title={transfer.source}
        >
          {name}
          {isDir && (
            <span className="ml-1 text-[11px] text-muted-foreground">
              （{transfer.done_files}/{transfer.total_files} 个文件）
            </span>
          )}
        </span>
        {!compact && (
          <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
            {transfer.source} → {transfer.target}
          </span>
        )}
        <span
          className={cn(
            "shrink-0 text-right text-xs tabular-nums",
            compact ? "ml-auto" : "w-32",
          )}
        >
          {formatSize(transfer.done_bytes)} / {formatSize(transfer.total_bytes)}
        </span>
        {!compact && (
          <>
            <span className="w-20 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
              {active ? formatSpeed(transfer.speed_bps) : ""}
            </span>
            <span className="w-20 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
              {transfer.state === "running" ? formatEta(transfer.eta_secs) : ""}
            </span>
          </>
        )}
        <span
          className={cn(
            "w-20 shrink-0 text-right text-xs",
            transfer.state === "failed" && "text-destructive",
            transfer.state === "done" && "text-emerald-500",
          )}
        >
          {stateLabel[transfer.state]}
        </span>
        <span className="flex w-28 shrink-0 items-center justify-end gap-1">
          {(transfer.state === "running" ||
            transfer.state === "queued" ||
            transfer.state === "planning") && (
            <IconBtn title="暂停" onClick={() => void sftpTransferPause(sftpId, transfer.id)}>
              <Pause size={13} />
            </IconBtn>
          )}
          {transfer.state === "paused" && (
            <IconBtn title="继续" onClick={() => void sftpTransferResume(sftpId, transfer.id)}>
              <Play size={13} />
            </IconBtn>
          )}
          {(transfer.state === "failed" || transfer.state === "cancelled") && (
            <IconBtn title="重试" onClick={() => void sftpTransferRetry(sftpId, transfer.id)}>
              <RotateCcw size={13} />
            </IconBtn>
          )}
          {active && (
            <IconBtn title="取消" onClick={() => void sftpTransferCancel(sftpId, transfer.id)}>
              <X size={13} />
            </IconBtn>
          )}
          {transfer.notes.length > 0 && (
            <IconBtn title="附加说明" onClick={() => setNotesOpen((open) => !open)}>
              <Info size={13} className={notesOpen ? "text-primary" : ""} />
            </IconBtn>
          )}
        </span>
      </div>

      {/* 进度条 */}
      <div className="mt-1 flex items-center gap-2">
        <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-secondary">
          <div
            className={cn(
              "h-full rounded-full transition-[width]",
              transfer.state === "failed" ? "bg-destructive" : "bg-primary",
            )}
            style={{ width: `${percent}%` }}
          />
        </div>
        <span className="w-10 shrink-0 text-right text-[11px] text-muted-foreground tabular-nums">
          {percent.toFixed(0)}%
        </span>
      </div>

      {/* 附加说明（notes：改过名、跳过的链接、续传失败从头传…） */}
      {(notesOpen || transfer.state === "failed") && transfer.notes.length > 0 && (
        <ul className="mt-1 list-disc pl-5 text-xs text-muted-foreground">
          {transfer.notes.map((note, index) => (
            <li key={index} className="break-all">
              {note}
            </li>
          ))}
        </ul>
      )}
      {transfer.state === "failed" && transfer.reason && (
        <p className="mt-1 break-all text-xs text-destructive">{transfer.reason}</p>
      )}
    </div>
  );
});

function IconBtn({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
    >
      {children}
    </button>
  );
}

function lastSegment(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut < 0 ? path : path.slice(cut + 1);
}
