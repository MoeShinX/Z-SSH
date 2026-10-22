// Adapted from Netcatty components/sftp/SftpPaneFileList.tsx、SftpPaneToolbar.tsx、SftpBreadcrumb.tsx（结构重写）
//! SFTP 双栏里的一栏（M3-2 第 2 节）：面包屑、过滤、排序、隐藏文件、
//! 虚拟滚动的文件列表、多选和键盘、拖动、右键菜单。
//!
//! 本地 / 远端的**数据来源**由调用方注入（`list` / 操作回调），这一层只管展示和交互，
//! 单测可以喂假数据（见 sftpPane.test.ts 的纯逻辑单测）。

import {
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  ChevronDown,
  ChevronRight,
  Eye,
  EyeOff,
  File,
  FileSymlink,
  Folder,
  HardDrive,
  RefreshCw,
  Search,
  X,
} from "lucide-react";
import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";

import { compactColumns } from "../../state/dockLayout";
import {
  beginPointerDrag,
  registerDropTarget,
  registerExternalDropTarget,
  useDragStore,
} from "../../state/pointerDrag";

import type { SftpEntry } from "../../api/sftp";
import {
  collapseCrumbs,
  localCrumbs,
  localParent,
  remoteCrumbs,
  remoteParent,
  ROW_HEIGHT,
  selectAll,
  selectionAfterArrow,
  selectionAfterClick,
  filterSorted,
  indicesOf,
  pathsOf,
  sortForDisplay,
  sortEntries,
  type FilterOptions,
  type SortKey,
  type SortOrder,
} from "../../state/sftpPane";
import { cn } from "../../lib/utils";
import { Input } from "../ui/input";

export interface PaneMenuRequest {
  kind: "context";
  /** `items`：右键点在条目上；`blank`：点在空白处（这时 `entries` 是空的，选择已清掉）。 */
  target: "items" | "blank";
  entries: SftpEntry[];
  indices: number[];
  x: number;
  y: number;
}

export interface SftpFilePaneProps {
  side: "local" | "remote";
  /** 当前目录（本地空字符串 = "此电脑"）。 */
  path: string;
  loading: boolean;
  error: string | null;
  entries: SftpEntry[];
  filter: FilterOptions;
  order: SortOrder;
  focused: boolean;
  onPathChange: (path: string) => void;
  onFilterChange: (patch: Partial<FilterOptions>) => void;
  onOrderChange: (order: SortOrder) => void;
  onRefresh: () => void;
  onFocusPane: () => void;
  /** 双击 / Enter：进目录。 */
  onActivate: (entry: SftpEntry, index: number) => void;
  onContextMenu: (request: PaneMenuRequest) => void;
  /** 两栏之间拖动 = 传输。拖起来的条目先报给父层（父层存引用，实时），
   *  松手时只报"落在了哪个目录上"（null = 当前目录）。 */
  onDropInto: (target: { dirPath: string | null }) => void;
  onDragStart: (entries: SftpEntry[]) => void;
  /** 从资源管理器拖进来的文件落在这一栏（只有远端栏会传这个；本地栏不传 = 什么也不做）。
   *  `dirPath` 是落在的目录行，没指到行是 null（= 当前目录）。 */
  onExternalDrop?: (paths: string[], dirPath: string | null) => void;
  /** 拖动结束（落没落下都会调，父层据此清掉暂存的条目）。 */
  onDragEnd?: () => void;
  /** 删除（Delete 键）。 */
  onDeleteSelection: (entries: SftpEntry[]) => void;
  /** 重命名（F2）。 */
  onRenameSelection: (entry: SftpEntry) => void;
  /** 复制 / 粘贴 / 属性的快捷键（Ctrl+C / Ctrl+V / Alt+Enter）。
   *  只在文件列表有焦点时触发；没传 = 这个功能还没开，按键原样放过。 */
  onCopySelection?: (entries: SftpEntry[]) => void;
  onPasteHere?: () => void;
  onShowProperties?: (entries: SftpEntry[]) => void;
  /** 选择变化（父层要拿"选中了哪些"去拖动 / 右键）。 */
  /** 选中的条目（按当前显示顺序；被过滤掉的不在里面）。 */
  onSelectionChange?: (entries: SftpEntry[]) => void;
  /** 面包屑 / 路径输入框直接改路径。 */
  onBrowse: (path: string) => void;
  /** 停靠面板里的窄版：只显示 名称、大小、修改时间，宽度不够先藏修改时间（M3-3 第 3 节）。 */
  compact?: boolean;
  /** 浏览历史：后退 / 前进（像 Xftp、资源管理器那样）。 */
  canBack?: boolean;
  canForward?: boolean;
  onBack?: () => void;
  onForward?: () => void;
  /** 工具条最右边的 ×（收起这一栏）。只有停靠面板的本地栏会传。 */
  onClosePane?: () => void;
  closePaneTitle?: string;
}

export type ColumnId = "name" | "size" | "mtime" | "kind" | "perms" | "owner";

const COLUMN_LABELS: Record<ColumnId, string> = {
  name: "名称",
  size: "大小",
  mtime: "修改时间",
  kind: "类型",
  perms: "权限",
  owner: "所有者",
};

const COLUMN_SORT: Record<ColumnId, SortKey | null> = {
  name: "name",
  size: "size",
  mtime: "mtime",
  kind: "kind",
  perms: null,
  owner: null,
};

/** 表头和数据行共用：宽度、对齐方式只在这里定义一次。 */
const COLUMN_CLASS: Record<ColumnId, string> = {
  name: "min-w-0 flex-1 text-left",
  size: "w-16 shrink-0 text-right justify-end",
  mtime: "w-32 shrink-0 text-left",
  kind: "w-12 shrink-0 text-left",
  perms: "w-12 shrink-0 text-left",
  owner: "w-20 shrink-0 text-left",
};

/** 完整列：远端多出权限和所有者。 */
export function fullColumns(isRemote: boolean): ColumnId[] {
  return isRemote
    ? ["name", "size", "mtime", "kind", "perms", "owner"]
    : ["name", "size", "mtime", "kind"];
}

function cellText(column: ColumnId, entry: SftpEntry): string {
  switch (column) {
    case "size":
      return entry.kind === "dir" || entry.kind === "drive" ? "" : formatSizeShort(entry.size);
    case "mtime":
      return formatTimeShort(entry.mtime);
    case "kind":
      return entry.kind === "dir"
        ? "目录"
        : entry.kind === "symlink"
          ? "链接"
          : entry.kind === "drive"
            ? "盘符"
            : "文件";
    case "perms":
      return entry.permissions !== undefined
        ? (entry.permissions & 0o7777).toString(8).padStart(4, "0")
        : "";
    case "owner":
      return entry.owner ?? "";
    default:
      return "";
  }
}

export default function SftpFilePane({
  side,
  path,
  loading,
  error,
  entries,
  filter,
  order,
  focused,
  onPathChange,
  onFilterChange,
  onOrderChange,
  onRefresh,
  onFocusPane,
  onActivate,
  onContextMenu,
  onDropInto,
  onDragStart,
  onDragEnd,
  onExternalDrop,
  onDeleteSelection,
  onCopySelection,
  onPasteHere,
  onShowProperties,
  onRenameSelection,
  onSelectionChange,
  onBrowse,
  compact = false,
  canBack = false,
  canForward = false,
  onBack,
  onForward,
  onClosePane,
  closePaneTitle,
}: SftpFilePaneProps) {
  const isRemote = side === "remote";
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [paneWidth, setPaneWidth] = useState(10_000);
  // 排序只在目录内容或排序方式变了时做；搜索框每打一个字只过滤（不重排）。
  // 过滤条件用"延后值"：输入框先响应，列表在空闲时跟上，几万条目录时打字不卡。
  const sorted = useMemo(() => sortForDisplay(entries, order), [entries, order]);
  const deferredFilter = useDeferredValue(filter);
  const rows = useMemo(() => filterSorted(sorted, deferredFilter), [sorted, deferredFilter]);
  // 选择 / Shift 起点 / 键盘焦点按文件路径记（不按行号）：排序、过滤、后台刷新之后
  // 行号对应的可能是别的文件，按行号记就会选错、删错。下面的 selection / anchor /
  // focusedIndex 是按当前列表换算出来的行号，只用于显示和 Shift 范围选择。
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(() => new Set());
  const [anchorPath, setAnchorPath] = useState<string | null>(null);
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const selection = useMemo(() => indicesOf(rows, selectedPaths), [rows, selectedPaths]);
  const rowIndexOf = useCallback(
    (target: string | null): number | null => {
      if (target === null) return null;
      const index = rows.findIndex((entry) => entry.path === target);
      return index >= 0 ? index : null;
    },
    [rows],
  );
  const anchor = useMemo(() => rowIndexOf(anchorPath), [rowIndexOf, anchorPath]);
  const focusedIndex = useMemo(() => rowIndexOf(focusedPath) ?? 0, [rowIndexOf, focusedPath]);
  const setSelection = (indices: Set<number>) => setSelectedPaths(pathsOf(rows, indices));
  const setAnchor = (index: number | null) =>
    setAnchorPath(index === null ? null : rows[index]?.path ?? null);
  const setFocusedIndex = (index: number) => setFocusedPath(rows[index]?.path ?? null);
  // 虚拟窗口只关心"滚到了第几行"：滚动事件很密，但只有跨过一行才需要重新渲染。
  const [scrollTop, setScrollTop] = useState(0);
  const updateScrollTop = (top: number) => {
    setScrollTop((current) =>
      Math.floor(current / ROW_HEIGHT) === Math.floor(top / ROW_HEIGHT) ? current : top,
    );
  };
  /** 每个目录离开时的滚动位置：后退 / 前进 / 再进来时回到原处。 */
  const scrollMemory = useRef(new Map<string, number>());
  const lastPathRef = useRef(path);
  const [dragOverRow, setDragOverRow] = useState<number | null>(null);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const [viewportHeight, setViewportHeight] = useState(400);
  const [pathEditing, setPathEditing] = useState<string | null>(null);

  // 换了目录：选择作废；滚动位置换成这个目录上次离开时的位置（没来过就是顶上）。
  // DOM 的滚动条要一起设——只改状态的话，实际滚动位置和虚拟窗口会对不上。
  useEffect(() => {
    const element = viewportRef.current;
    const previous = lastPathRef.current;
    if (previous !== path && element) scrollMemory.current.set(previous, element.scrollTop);
    lastPathRef.current = path;
    setSelectedPaths(new Set());
    setAnchorPath(null);
    setFocusedPath(null);
    const restored = scrollMemory.current.get(path) ?? 0;
    if (element) element.scrollTop = restored;
    setScrollTop(element ? element.scrollTop : restored);
  }, [path]);

  // 列表变短（刷新、过滤）后滚动条被浏览器收回来了：虚拟窗口跟着校正，不留空白。
  useEffect(() => {
    const element = viewportRef.current;
    if (element && element.scrollTop !== scrollTop) updateScrollTop(element.scrollTop);
    // scrollTop 故意不放进依赖：只在列表长度变化时校正
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows.length]);

  // 窄版要按这一栏的实际宽度决定显示哪些列。
  useEffect(() => {
    const element = rootRef.current;
    if (!compact || !element) return;
    const observer = new ResizeObserver(() => setPaneWidth(element.clientWidth));
    observer.observe(element);
    setPaneWidth(element.clientWidth);
    return () => observer.disconnect();
  }, [compact]);

  // 可视区高度（虚拟窗口要用）。
  useEffect(() => {
    const element = viewportRef.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      setViewportHeight(element.clientHeight);
    });
    observer.observe(element);
    setViewportHeight(element.clientHeight);
    return () => observer.disconnect();
  }, []);

  const window2 = useMemo(
    () => ({
      start: Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 8),
      get end() {
        return Math.min(
          rows.length,
          Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 8) +
            Math.ceil(viewportHeight / ROW_HEIGHT) +
            16,
        );
      },
      totalHeight: rows.length * ROW_HEIGHT,
      offsetY: Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 8) * ROW_HEIGHT,
    }),
    [scrollTop, viewportHeight, rows.length],
  );

  const handleClick = (index: number, event: React.MouseEvent) => {
    onFocusPane();
    const result = selectionAfterClick(
      index,
      anchor,
      selection,
      { ctrl: event.ctrlKey, shift: event.shiftKey },
      rows.length,
    );
    setSelection(result.selection);
    setAnchor(result.anchor);
    setFocusedIndex(index);
  };

  const handleActivate = useCallback(
    (entry: SftpEntry, index: number) => {
      onActivate(entry, index);
    },
    [onActivate],
  );

  const goUp = useCallback(() => {
    onPathChange(isRemote ? remoteParent(path) : localParent(path));
  }, [isRemote, path, onPathChange]);

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (pathEditing !== null) return; // 在路径输入框里打字时不动
    onFocusPane();
    // 复制 / 粘贴 / 属性：只在这个列表有焦点时才会走到这里（键盘事件从列表里冒上来），
    // 终端里的 Ctrl+C 不经过这里。没接上对应功能时原样放过（不拦下按键）。
    const picked = () => [...selection].map((index) => rows[index]).filter(Boolean);
    const command = (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey;
    const lower = event.key.toLowerCase();
    if (event.altKey && !event.ctrlKey && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      if (event.key === "ArrowLeft") onBack?.();
      else onForward?.();
      return;
    }
    if (command && lower === "l") {
      event.preventDefault();
      setPathEditing(path);
      return;
    }
    if (command && lower === "c" && onCopySelection) {
      event.preventDefault();
      const entries = picked();
      if (entries.length > 0) onCopySelection(entries);
      return;
    }
    if (command && lower === "v" && onPasteHere) {
      event.preventDefault();
      onPasteHere();
      return;
    }
    if (event.altKey && event.key === "Enter") {
      // Alt+Enter = 属性，不是「回车打开」。
      event.preventDefault();
      if (onShowProperties) onShowProperties(picked());
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const result = selectionAfterArrow(
        focusedIndex,
        anchor,
        event.shiftKey,
        delta,
        rows.length,
      );
      setSelection(result.selection);
      setAnchor(result.anchor);
      setFocusedIndex(result.focused);
      scrollRowIntoView(result.focused);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const entry = rows[focusedIndex];
      if (entry) handleActivate(entry, focusedIndex);
    } else if (event.key === "Backspace") {
      event.preventDefault();
      goUp();
    } else if (event.key === "Delete") {
      event.preventDefault();
      const picked = [...selection].map((index) => rows[index]).filter(Boolean);
      if (picked.length > 0) onDeleteSelection(picked);
    } else if (event.key === "F2") {
      event.preventDefault();
      const entry = rows[focusedIndex];
      if (entry) onRenameSelection(entry);
    } else if (event.key === "F5") {
      event.preventDefault();
      onRefresh();
    } else if (event.ctrlKey && event.key.toLowerCase() === "a") {
      event.preventDefault();
      const all = selectAll(rows.length);
      setSelection(all);
      if (rows.length > 0) setAnchor(0);
    }
  };

  const scrollRowIntoView = (index: number) => {
    const element = viewportRef.current;
    if (!element) return;
    const top = index * ROW_HEIGHT;
    const bottom = top + ROW_HEIGHT;
    if (top < element.scrollTop) element.scrollTop = top;
    else if (bottom > element.scrollTop + element.clientHeight) {
      element.scrollTop = bottom - element.clientHeight;
    }
  };

  const columns: ColumnId[] = compact ? compactColumns(paneWidth) : fullColumns(isRemote);
  const crumbs = useMemo(() => (isRemote ? remoteCrumbs(path) : localCrumbs(path)), [isRemote, path]);
  const typingPath = pathEditing !== null;
  const crumbBoxRef = useRef<HTMLDivElement>(null);
  const [crumbWidth, setCrumbWidth] = useState(0);
  // 面包屑容器的宽度：放不下时 collapseCrumbs 把前面几级折成"…"（M3-3 遗留 10）。
  // 输入路径时容器不存在，退出输入再量一次。
  useEffect(() => {
    const box = crumbBoxRef.current;
    if (!box) return;
    setCrumbWidth(box.clientWidth);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => setCrumbWidth(box.clientWidth));
    observer.observe(box);
    return () => observer.disconnect();
  }, [typingPath]);
  const collapsed = useMemo(() => collapseCrumbs(crumbs, crumbWidth), [crumbs, crumbWidth]);

  /** 在行上按下：拖过阈值才算拖动（点击、双击、多选都照旧）。 */
  const startRowDrag = (index: number, event: React.PointerEvent) => {
    if (event.button !== 0) return;
    beginPointerDrag(
      event,
      () => {
        // 拖的是没选中的条目：把选择换成它（和资源管理器一致）。
        const dragged = selection.has(index)
          ? [...selection].map((i) => rows[i]).filter(Boolean)
          : rows[index]
            ? [rows[index]]
            : [];
        if (!selection.has(index)) {
          setSelection(new Set([index]));
          setAnchor(index);
        }
        const operable = dragged.filter((entry) => entry.name_is_utf8);
        if (operable.length === 0) return null;
        return { kind: "sftp-entries", side, entries: operable };
      },
      {
        onStart: (payload) => onDragStart(payload.entries),
        onEnd: () => {
          setDragOverRow(null);
          onDragEnd?.();
        },
      },
    );
  };

  // 落点：另一栏拖过来的条目落在这一栏上。落在目录行上 = 那个目录（Xftp 的习惯），
  // 没指到行 = 当前目录。
  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const onDropIntoRef = useRef(onDropInto);
  onDropIntoRef.current = onDropInto;
  useEffect(() => {
    const element = rootRef.current;
    if (!element) return;
    return registerDropTarget(element, {
      accepts: (payload) => payload.side !== side,
      onDrop: (_payload, info) => {
        const rowElement = info.element?.closest("[data-row]");
        const index = rowElement ? Number(rowElement.getAttribute("data-row")) : NaN;
        const row = Number.isNaN(index) ? undefined : rowsRef.current[index];
        const ontoDir = row && (row.kind === "dir" || row.kind === "drive") ? row : undefined;
        setDragOverRow(null);
        onDropIntoRef.current({ dirPath: ontoDir ? ontoDir.path : null });
      },
    });
  }, [side]);

  // 资源管理器拖进来的文件：远端栏 = 上传；本地栏登记一个什么也不做的落点，
  // 免得文件落到后面的终端区域去（M3-3 第 4 节："拖到本地栏不做任何事"）。
  const onExternalDropRef = useRef(onExternalDrop);
  onExternalDropRef.current = onExternalDrop;
  useEffect(() => {
    const element = rootRef.current;
    if (!element) return;
    return registerExternalDropTarget(element, {
      onDrop: (paths, info) => {
        if (!isRemote) return;
        const rowElement = info.element?.closest("[data-row]");
        const index = rowElement ? Number(rowElement.getAttribute("data-row")) : NaN;
        const row = Number.isNaN(index) ? undefined : rowsRef.current[index];
        const ontoDir = row && row.kind === "dir" ? row : undefined;
        onExternalDropRef.current?.(paths, ontoDir ? ontoDir.path : null);
      },
    });
  }, [isRemote]);

  // 拖动悬停在这一栏上：高亮整栏，指到目录行时高亮那一行。
  const dropHover = useDragStore((state) => state.hover !== null && state.hover === rootRef.current);
  useEffect(() => {
    if (!dropHover) {
      setDragOverRow(null);
      return;
    }
    const onMove = (event: PointerEvent) => {
      const rowElement = document.elementFromPoint(event.clientX, event.clientY)?.closest("[data-row]");
      setDragOverRow(rowElement ? Number(rowElement.getAttribute("data-row")) : null);
    };
    window.addEventListener("pointermove", onMove, true);
    return () => window.removeEventListener("pointermove", onMove, true);
  }, [dropHover]);

  const selectedEntries = useMemo(
    () => [...selection].sort((a, b) => a - b).map((index) => rows[index]).filter(Boolean),
    [selection, rows],
  );
  useEffect(() => {
    onSelectionChange?.(selectedEntries);
  }, [selectedEntries, onSelectionChange]);

  const toggleSort = (key: SortKey) => {
    if (order.key === key) onOrderChange({ key, descending: !order.descending });
    else onOrderChange({ key, descending: false });
  };

  return (
    <div
      ref={rootRef}
      data-pane-side={side}
      data-drop-target={isRemote ? "sftp-remote" : "sftp-local"}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-md border",
        focused ? "border-primary/50" : "border-border/50",
        dropHover && "border-primary bg-primary/5",
      )}
      onMouseDown={onFocusPane}
      onMouseUp={(event) => {
        // 鼠标侧键：3 = 后退，4 = 前进
        if (event.button === 3 && canBack) onBack?.();
        else if (event.button === 4 && canForward) onForward?.();
      }}
    >
      {/* 工具栏：上级、面包屑/路径输入、刷新、隐藏文件、过滤 */}
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border/50 px-2">
        <button
          title="后退 (Alt+←)"
          disabled={!canBack}
          onClick={onBack}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground disabled:opacity-30"
        >
          <ArrowLeft size={14} />
        </button>
        <button
          title="前进 (Alt+→)"
          disabled={!canForward}
          onClick={onForward}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground disabled:opacity-30"
        >
          <ArrowRight size={14} />
        </button>
        <button
          title="返回上级 (Backspace)"
          disabled={isRemote ? path === "/" : path === ""}
          onClick={goUp}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground disabled:opacity-30"
        >
          <ArrowUp size={14} />
        </button>

        {pathEditing !== null ? (
          <Input
            value={pathEditing}
            onChange={(event) => setPathEditing(event.target.value)}
            onBlur={() => {
              const target = pathEditing.trim();
              setPathEditing(null);
              if (target && target !== path) onBrowse(target);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                const target = pathEditing.trim();
                setPathEditing(null);
                if (target && target !== path) onBrowse(target);
              } else if (event.key === "Escape") {
                setPathEditing(null);
              }
            }}
            autoFocus
            onFocus={(event) => event.target.select()}
            className="h-6 flex-1 font-mono text-xs"
          />
        ) : (
          <div
            ref={crumbBoxRef}
            data-section="crumbs"
            className="flex min-w-0 flex-1 cursor-text items-center gap-0.5 overflow-hidden rounded border border-transparent text-[13px] hover:border-border/60"
            // 点路径栏的空白处（不是某一级目录）= 手动输入路径；Ctrl+L 也行
            onClick={(event) => {
              if (event.target === event.currentTarget) setPathEditing(path);
            }}
            onDoubleClick={() => setPathEditing(path)}
            title={`${path || "此电脑"}（点空白处或按 Ctrl+L 输入路径）`}
          >
            {crumbs.length === 0 && (
              <button
                onClick={() => onBrowse(isRemote ? "/" : "")}
                className="rounded px-1.5 py-0.5 hover:bg-accent"
              >
                {isRemote ? "/" : "此电脑"}
              </button>
            )}
            {collapsed.hidden.length > 0 && (
              <button
                data-section="crumb-ellipsis"
                title={`${path}（点这里输入路径）`}
                onClick={() => setPathEditing(path)}
                className="shrink-0 rounded px-1.5 py-0.5 text-muted-foreground hover:bg-accent"
              >
                …
              </button>
            )}
            {collapsed.shown.map((crumb, index) => (
              <span
                key={crumb.path}
                className={cn("flex items-center", index === collapsed.shown.length - 1 ? "min-w-0" : "shrink-0")}
              >
                {(index > 0 || collapsed.hidden.length > 0) && (
                  <ChevronRight size={11} className="text-muted-foreground/50" />
                )}
                <button
                  onClick={() => onPathChange(crumb.path)}
                  className="truncate rounded px-1.5 py-0.5 hover:bg-accent"
                >
                  {crumb.name}
                </button>
              </span>
            ))}
          </div>
        )}

        <button
          title="刷新 (F5)"
          onClick={onRefresh}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <RefreshCw size={13} className={cn(loading && "animate-spin")} />
        </button>
        <button
          title={filter.showHidden ? "隐藏隐藏文件" : "显示隐藏文件"}
          onClick={() => onFilterChange({ showHidden: !filter.showHidden })}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          {filter.showHidden ? <Eye size={13} /> : <EyeOff size={13} />}
        </button>
        <div className="relative">
          <Search size={12} className="absolute left-1.5 top-1.5 text-muted-foreground" />
          <input
            value={filter.text}
            onChange={(event) => onFilterChange({ text: event.target.value })}
            placeholder="过滤"
            className={cn(
              "h-6 rounded border border-border/50 bg-transparent pl-6 pr-1 text-xs outline-none focus:border-primary/60",
              compact ? "w-20" : "w-28",
            )}
          />
        </div>
        {onClosePane && (
          <button
            title={closePaneTitle ?? "关闭"}
            data-action="close-pane"
            onClick={onClosePane}
            className="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          >
            <X size={13} />
          </button>
        )}
      </div>

      {/* 列头（排序按钮）：和数据行用同一份列宽，左右边界一一对齐 */}
      <div
        data-role="col-header"
        className="flex h-7 shrink-0 items-center gap-2 border-b border-border/50 px-2 text-[11px] text-muted-foreground"
      >
        <span className="w-4 shrink-0" />
        {columns.map((column) => {
          const sortable = COLUMN_SORT[column];
          const label = COLUMN_LABELS[column];
          const common = cn(COLUMN_CLASS[column], "flex items-center gap-0.5");
          return sortable ? (
            <button
              key={column}
              data-col={column}
              onClick={() => toggleSort(sortable)}
              className={cn(
                common,
                "rounded hover:bg-accent",
                order.key === sortable && "text-foreground",
              )}
            >
              <span className="truncate">{label}</span>
              {order.key === sortable &&
                (order.descending ? <ChevronDown size={10} /> : <ChevronRight size={10} />)}
            </button>
          ) : (
            <span key={column} data-col={column} className={common}>
              <span className="truncate">{label}</span>
            </span>
          );
        })}
      </div>

      {/* 文件列表（虚拟滚动） */}
      <div
        ref={viewportRef}
        tabIndex={0}
        onKeyDown={handleKeyDown}
        onScroll={(event) => updateScrollTop(event.currentTarget.scrollTop)}
        onContextMenu={(event) => {
          event.preventDefault();
          onFocusPane();
          // 右键点在没选中的行上：先把选择换成它。
          const row = (event.target as HTMLElement).closest("[data-row]");
          const index = row ? Number(row.getAttribute("data-row")) : NaN;
          let indices = [...selection];
          if (!Number.isNaN(index) && !selection.has(index)) {
            setSelection(new Set([index]));
            setAnchor(index);
            indices = [index];
          }
          // 点在空白处（没有行）：清掉选择，菜单只给「粘贴 / 新建 / 刷新 / 属性」。
          const blank = !row;
          if (blank) {
            setSelection(new Set());
            indices = [];
          }
          onContextMenu({
            kind: "context",
            target: blank ? "blank" : "items",
            entries: indices.map((i) => rows[i]).filter(Boolean),
            indices,
            x: event.clientX,
            y: event.clientY,
          });
        }}
        className={cn(
          "min-h-0 flex-1 overflow-y-auto outline-none transition-opacity",
          // 正在换目录：旧列表调暗一点，表示还在读
          loading && entries.length > 0 && "opacity-60",
        )}
      >
        {error ? (
          <div className="p-4 text-sm text-destructive break-all">{error}</div>
        ) : loading && entries.length === 0 ? (
          // 只有第一次（手里什么都没有）才显示这一行；换目录时先留着旧列表，到了再换，不闪白
          <div className="p-4 text-sm text-muted-foreground">读取目录中…</div>
        ) : rows.length === 0 ? (
          <div className="p-4 text-sm text-muted-foreground">
            {filter.text ? "没有匹配的条目" : "空目录"}
          </div>
        ) : (
          <div style={{ height: window2.totalHeight, position: "relative" }}>
            <div
              style={{
                position: "absolute",
                top: window2.offsetY,
                left: 0,
                right: 0,
              }}
            >
              {rows.slice(window2.start, window2.end).map((entry, offset) => {
                const index = window2.start + offset;
                const selected = selection.has(index);
                const isDropTarget = dragOverRow === index;
                const operable = entry.name_is_utf8; // 非 UTF-8 名字：标灰、禁操作
                return (
                  <div
                    key={entry.path}
                    data-row={index}
                    data-draggable={operable ? "true" : "false"}
                    onPointerDown={(event) => {
                      if (operable) startRowDrag(index, event);
                    }}
                    onClick={(event) => handleClick(index, event)}
                    onDoubleClick={() => handleActivate(entry, index)}
                    className={cn(
                      "flex h-[26px] cursor-default items-center gap-2 px-2 text-[13px]",
                      selected
                        ? "bg-primary/15 text-foreground"
                        : "hover:bg-accent/50",
                      focusedIndex === index && "ring-1 ring-inset ring-primary/40",
                      isDropTarget && "bg-primary/25",
                      !operable && "text-muted-foreground/50",
                    )}
                    title={
                      operable
                        ? entry.kind === "symlink" && entry.link_target
                          ? `${entry.name} → ${entry.link_target}`
                          : entry.name
                        : "文件名编码不是 UTF-8，暂不支持操作"
                    }
                  >
                    <span className="w-4 shrink-0">
                      {entry.kind === "dir" ? (
                        <Folder size={14} className="text-sky-400" />
                      ) : entry.kind === "drive" ? (
                        <HardDrive size={14} className="text-sky-400" />
                      ) : entry.kind === "symlink" ? (
                        <FileSymlink size={14} className="text-emerald-500" />
                      ) : (
                        <File size={14} className="text-muted-foreground" />
                      )}
                    </span>
                    <span data-col="name" className="min-w-0 flex-1 truncate">
                      {entry.name}
                      {entry.kind === "symlink" && entry.link_target && (
                        <span className="ml-1 text-muted-foreground">
                          → {entry.link_target}
                        </span>
                      )}
                      {!operable && (
                        <span className="ml-2 text-[11px] text-muted-foreground">
                          文件名编码不是 UTF-8，暂不支持操作
                        </span>
                      )}
                    </span>
                    {columns.filter((column) => column !== "name").map((column) => (
                      <span
                        key={column}
                        data-col={column}
                        className={cn(
                          COLUMN_CLASS[column],
                          "truncate text-xs text-muted-foreground tabular-nums",
                        )}
                      >
                        {cellText(column, entry)}
                      </span>
                    ))}
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>

      {/* 状态栏：条目数和选中数（右键 / 拖动要的数据父层已经有了） */}
      <div className="flex h-6 shrink-0 items-center gap-3 border-t border-border/50 px-2 text-[11px] text-muted-foreground">
        <span>{rows.length} 项</span>
        {selectedEntries.length > 0 && <span>选中 {selectedEntries.length} 项</span>}
      </div>
    </div>
  );
}

function formatSizeShort(size: number | undefined): string {
  if (size === undefined) return "";
  if (size < 1024) return `${size} B`;
  const units = ["K", "M", "G", "T"];
  let value = size;
  let unit = "";
  for (const next of units) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(0)}${unit}`;
}

function formatTimeShort(unixSeconds: number | undefined): string {
  if (unixSeconds === undefined || unixSeconds === 0) return "";
  const date = new Date(unixSeconds * 1000);
  const sameYear = date.getFullYear() === new Date().getFullYear();
  const pad = (n: number) => `${n}`.padStart(2, "0");
  return sameYear
    ? `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`
    : `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** 排序后条目的原始顺序（供父层做"按名字排序"以外的对账）。 */
export { sortEntries };
