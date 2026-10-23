//! 主机页（参照 Netcatty components/VaultView.tsx 的信息结构和观感）：
//! 左侧主机树（像 Xshell 的会话管理器：分组可展开 / 收起，主机挂在分组下面）+
//! 顶部的搜索/快速连接 + 主机卡片（网格或列表）。
//!
//! 数据仍然来自我们自己的 stores/vault（后端 SQLite），没有搬 Netcatty 的状态。

import {
  ArrowDown,
  ArrowDownAZ,
  ArrowLeftRight,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderOpen,
  FolderPlus,
  FolderInput,
  FolderTree,
  KeyRound,
  PanelLeftClose,
  PanelLeftOpen,
  LayoutGrid,
  List,
  Network,
  Pencil,
  Play,
  Plus,
  Search,
  Server,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import { cn } from "../../lib/utils";
import OsIcon from "./OsIcon";
import type { Group, Host } from "../../api/vault";
import { useVault } from "../../stores/vault";
import { useSettings } from "../../stores/settings";
import {
  filteredHostOrder,
  groupKey,
  groupsUnder,
  hostsUnder,
  parseExpanded,
  parseRecent,
  serializeExpanded,
  sortByRecent,
  toggleExpanded,
  totalUnder,
  treeFilterFor,
  UNGROUPED_KEY,
  type TreeFilter,
} from "../../state/hostTree";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "../ui/context-menu";
import {
  dragJustEnded,
  HostDragGhost,
  InsertLine,
  moveHostToGroup,
  startGroupDrag,
  startHostDrag,
  useInsertMark,
  useIsDropTarget,
  type DropGroup,
} from "./hostDrag";
import { canStepGroup, canStepHost, resetLayerOrder, stepGroup, stepHost } from "./treeOrder";
import { Input } from "../ui/input";
import { InstallKeyDialog } from "./SshKeyDialogs";
import { ScrollArea } from "../ui/scroll-area";
import { ResizeHandle, useHostTreeWidth } from "../ui/resize-handle";
import { guideAttrs, treeGuideStyle, type TreeGuide } from "./treeGuides";
import { hostCopyText, type HostCopyKind } from "../../state/hostAddress";
import {
  cancelPendingCollapse,
  clickHostInfo,
  closeHostInfo,
  useHostInfoId,
} from "../../state/hostInfoPanel";
import HostInfoPanel from "./HostInfoPanel";

interface VaultViewProps {
  /** 双击主机 / 点"连接"：开一个新的终端标签 */
  onOpenHost: (host: Host) => void;
  /** 打开这台主机的 SFTP 标签（M3-2 第 2 节）。 */
  onOpenSftp: (host: Host) => void;
  /** 搜索框里输入了 user@host:port 并回车 */
  onQuickConnect: (target: string) => void;
  /** 选中一台主机（右侧打开编辑面板） */
  onSelectHost: (host: Host) => void;
  /** 新建主机（可以带上默认分组） */
  onNewHost: (groupId: number | null) => void;
  onNewGroup: (parentId: number | null, options?: { adoptUngrouped?: boolean }) => void;
  onEditGroup: (group: Group) => void;
  onDeleteGroup: (group: Group) => void;
  onDeleteHost: (host: Host) => void;
  /** 打开凭据 / 代理面板 */
  onOpenIdentities: () => void;
  onOpenProxies: () => void;
  /** 打开端口转发面板；带主机 ID 时只看这台 */
  onOpenForwards: (hostId: number | null) => void;
  onNotice: (message: string) => void;
}

type ViewMode = "grid" | "list";

/** 搜索框里的文字像不像"user@host:port"这类快速连接串。 */
export function looksLikeQuickTarget(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed === "" || /\s/.test(trimmed)) return false;
  // 要么有 @，要么是 host / host:port（不认识的词仍然当搜索）
  return trimmed.includes("@") || /^[\w.-]+(:\d+)?$/.test(trimmed);
}

export default function VaultView({
  onOpenHost,
  onOpenSftp,
  onQuickConnect,
  onSelectHost,
  onNewHost,
  onNewGroup,
  onEditGroup,
  onDeleteGroup,
  onDeleteHost,
  onOpenIdentities,
  onOpenProxies,
  onOpenForwards,
  onNotice,
}: VaultViewProps) {
  const groups = useVault((state) => state.groups);
  const hosts = useVault((state) => state.hosts);
  const load = useVault((state) => state.load);
  const [query, setQuery] = useState("");

  // 进这一页时把主机库读进来（原来在侧栏里读）
  useEffect(() => {
    void load().catch((error: unknown) => {
      onNotice(`主机库读取失败：${String(error)}`);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [selectedGroup, setSelectedGroup] = useState<number | null | "all">("all");
  const [selectedHostId, setSelectedHostId] = useState<number | null>(null);
  const [mode, setMode] = useState<ViewMode>("grid");
  const [installHost, setInstallHost] = useState<Host | null>(null);

  const groupName = useMemo(() => {
    const map = new Map<number, string>();
    for (const group of groups) map.set(group.id, group.name);
    return map;
  }, [groups]);


  const recentRaw = useSettings((state) => state["layout.recent_hosts"]);
  const filtered = useMemo(() => {
    const text = query.trim().toLowerCase();
    const matched = hosts.filter((host) => {
        if (selectedGroup !== "all" && host.group_id !== selectedGroup) return false;
        if (text === "") return true;
        return (
          host.name.toLowerCase().includes(text) ||
          host.host.toLowerCase().includes(text) ||
          host.username.toLowerCase().includes(text) ||
          host.tags.some((tag) => tag.toLowerCase().includes(text))
        );
      });
    return sortByRecent(matched, parseRecent(recentRaw));
  }, [hosts, query, selectedGroup, recentRaw]);

  const handleSearchKey = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "Enter") return;
    const text = query.trim();
    if (text === "") return;
    if (looksLikeQuickTarget(text)) {
      onQuickConnect(text);
    }
  };

  /** 右边卡片的操作（单击看详情、双击连接、右键菜单）。 */
  const hostActions: HostActions = {
    select: (host) => {
      setSelectedHostId(host.id);
      onSelectHost(host);
    },
    connect: (host) => onOpenHost(host),
    openSftp: (host) => onOpenSftp(host),
    remove: (host) => onDeleteHost(host),
    copy: (host, kind) => copyHost(host, kind),
    installKey: (host) => setInstallHost(host),
    forwards: (host) => onOpenForwards(host.id),
  };

  /** 左边树里的主机：和终端界面的树一样，单击只高亮并展开底部的属性面板，右键「编辑」才打开详情。 */
  const treeHostActions: HostActions = {
    ...hostActions,
    select: (host) => setSelectedHostId(host.id),
    edit: hostActions.select,
  };

  const copyHost = (host: Host, kind: HostCopyKind) => {
    const text = hostCopyText(host, kind);
    void navigator.clipboard
      .writeText(text)
      .then(() => onNotice(`已复制：${text}`))
      .catch(() => onNotice("复制失败"));
  };

  return (
    <div data-section="vault" className="flex h-full min-h-0">
      {/* 左侧主机树（可收起成一列图标）；终端界面也用同一个组件 */}
      <HostTreeAside
        selectedGroup={selectedGroup}
        onSelectGroup={setSelectedGroup}
        selectedHostId={selectedHostId}
        hostActions={treeHostActions}
        onNewHost={onNewHost}
        onNewGroup={onNewGroup}
        onEditGroup={onEditGroup}
        onDeleteGroup={onDeleteGroup}
        collapsedStyle="icons"
        infoPanel
      />

      {/* 主区 */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex shrink-0 items-center gap-3 border-b border-border/60 px-4 py-3">
          <div className="relative max-w-md flex-1">
            <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={handleSearchKey}
              className="h-9 pl-9 font-normal"
              placeholder="搜索主机；或者输入 user@host:port 回车直接连接"
              spellCheck={false}
            />
          </div>
          <div className="ml-auto flex shrink-0 items-center gap-1">
            <div className="mr-1 flex items-center rounded-md border border-border/60 p-0.5">
              <button
                onClick={() => setMode("grid")}
                title="网格视图"
                className={cn(
                  "cursor-pointer rounded p-1.5 transition-colors",
                  mode === "grid"
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <LayoutGrid size={15} />
              </button>
              <button
                onClick={() => setMode("list")}
                title="列表视图"
                className={cn(
                  "cursor-pointer rounded p-1.5 transition-colors",
                  mode === "list"
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <List size={15} />
              </button>
            </div>
            <Button variant="outline" size="sm" className="h-8 px-2.5 text-[13px]" onClick={onOpenIdentities}>
              <KeyRound size={13} /> 凭据
            </Button>
            <Button variant="outline" size="sm" className="h-8 px-2.5 text-[13px]" onClick={onOpenProxies}>
              <Network size={13} /> 代理
            </Button>
            <Button variant="outline" size="sm" className="h-8 px-2.5 text-[13px]" onClick={() => onOpenForwards(null)}>
              <ArrowLeftRight size={13} /> 转发
            </Button>
          </div>
        </header>

        <div className="min-h-0 flex-1 overflow-auto p-4">
          {filtered.length === 0 ? (
            <EmptyState
              hasHosts={hosts.length > 0}
              query={query}
              onNewHost={() => onNewHost(selectedGroup === "all" ? null : selectedGroup)}
            />
          ) : mode === "grid" ? (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
              {filtered.map((host) => (
                <HostCard
                  key={host.id}
                  host={host}
                  groupName={host.group_id === null ? null : groupName.get(host.group_id) ?? null}
                  onConnect={() => onOpenHost(host)}
                  onOpenSftp={() => onOpenSftp(host)}
                  onSelect={() => onSelectHost(host)}
                  onEdit={() => onSelectHost(host)}
                  onDelete={() => onDeleteHost(host)}
                  onCopy={(kind) => copyHost(host, kind)}
                  onInstallKey={() => setInstallHost(host)}
                  onForwards={() => onOpenForwards(host.id)}
                />
              ))}
            </div>
          ) : (
            <div className="overflow-hidden rounded-lg border border-border/60">
              {filtered.map((host, index) => (
                <HostRow
                  key={host.id}
                  host={host}
                  groupName={host.group_id === null ? null : groupName.get(host.group_id) ?? null}
                  last={index === filtered.length - 1}
                  onConnect={() => onOpenHost(host)}
                  onOpenSftp={() => onOpenSftp(host)}
                  onSelect={() => onSelectHost(host)}
                  onEdit={() => onSelectHost(host)}
                  onDelete={() => onDeleteHost(host)}
                  onCopy={(kind) => copyHost(host, kind)}
                  onInstallKey={() => setInstallHost(host)}
                  onForwards={() => onOpenForwards(host.id)}
                />
              ))}
            </div>
          )}
        </div>
      </div>
      {installHost && (
        <InstallKeyDialog
          host={installHost}
          onClose={() => setInstallHost(null)}
          onDone={(message) => {
            setInstallHost(null);
            onNotice(message);
          }}
          onError={onNotice}
        />
      )}
    </div>
  );
}

/**
 * 左侧主机树（像 Xshell 的会话管理器）。主机页和终端界面共用：展开时两边都显示；收起时
 * 主机页缩成一列图标（`collapsedStyle="icons"`），终端界面直接不占地方（`"hidden"`）。
 */
export function HostTreeAside({
  selectedGroup,
  onSelectGroup,
  selectedHostId,
  hostActions,
  onNewHost,
  onNewGroup,
  onEditGroup,
  onDeleteGroup,
  collapsedStyle,
  infoPanel = false,
}: {
  selectedGroup: number | null | "all";
  onSelectGroup: (group: number | null | "all") => void;
  selectedHostId: number | null;
  hostActions: HostActions;
  onNewHost: (groupId: number | null) => void;
  onNewGroup: (parentId: number | null, options?: { adoptUngrouped?: boolean }) => void;
  onEditGroup: (group: Group) => void;
  onDeleteGroup: (group: Group) => void;
  collapsedStyle: "icons" | "hidden";
  /** 单击主机在树底部展开 / 收起只读属性面板（UI-6）；主机页和终端界面现在都开着。 */
  infoPanel?: boolean;
}) {
  const groups = useVault((state) => state.groups);
  const hosts = useVault((state) => state.hosts);
  const proxies = useVault((state) => state.proxies);
  const infoHostId = useHostInfoId();
  const infoHost = infoPanel && infoHostId !== null ? hosts.find((h) => h.id === infoHostId) : undefined;
  // 属性面板的接线：单击切换面板（再照常走调用方的单击），双击连接时取消待执行的收起，
  // 这样双击不会让面板来回闪（见 state/hostInfoPanel.ts）
  const treeActions: HostActions = infoPanel
    ? {
        ...hostActions,
        select: (host, clickCount) => {
          clickHostInfo(host.id, clickCount);
          hostActions.select(host);
        },
        connect: (host) => {
          cancelPendingCollapse();
          hostActions.connect(host);
        },
      }
    : hostActions;
  // 树自己的搜索框（UI-7）：只在这个树实例里记着（不持久化）。主机页和终端界面各有一个
  // 实例、各管各的词：搜索是「找到并连上」的一次性动作，连完切到终端界面时树应该是完整的，
  // 不该带着上一次的过滤；也避免和主机页右侧那个大搜索框互相影响。
  const [search, setSearch] = useState("");
  const [activeId, setActiveId] = useState<number | null>(null);
  const asideRef = useRef<HTMLElement>(null);
  const filter = useMemo(() => treeFilterFor(groups, hosts, search), [groups, hosts, search]);
  const order = useMemo(
    () => (filter ? filteredHostOrder(groups, hosts, filter) : []),
    [groups, hosts, filter],
  );
  // 高亮的那台必须还在结果里（主机被删、搜索词变了都算没有）
  const activeHostId = order.some((host) => host.id === activeId) ? activeId : null;
  useEffect(() => {
    if (activeHostId === null) return;
    asideRef.current
      ?.querySelector<HTMLElement>('[data-search-active="true"]')
      ?.scrollIntoView?.({ block: "nearest" });
  }, [activeHostId]);
  // 树的展开状态（本机布局，记在设置里）。搜索时显示出来的分组一律自动展开，
  // 用户自己的展开状态原样不动，清空搜索后恢复。
  const expandedRaw = useSettings((state) => state["layout.vault_expanded"]);
  const updateSetting = useSettings((state) => state.update);
  const expanded = useMemo(() => parseExpanded(expandedRaw), [expandedRaw]);
  const isExpanded = (key: string) => {
    if (filter === null) return expanded.has(key);
    return key === UNGROUPED_KEY ? filter.ungrouped : filter.groupIds.has(Number(key.slice(1)));
  };
  const toggle = (key: string) => {
    // 搜索时展开状态是自动的，点箭头不去改用户存着的展开状态
    if (filter !== null) return;
    void updateSetting("layout.vault_expanded", serializeExpanded(toggleExpanded(expanded, key)));
  };
  // 收起（本机布局）：主机页和终端界面共用这一个开关
  const collapsed = useSettings((state) => state["layout.vault_sidebar_collapsed"]);
  const setCollapsed = (value: boolean) => void updateSetting("layout.vault_sidebar_collapsed", value);
  // 宽度可拖（UI-5）：主机页和终端界面共用 layout.host_tree_width，顶栏左侧块也读同一个值
  const { width, bounds, onResize, onCommit, onReset } = useHostTreeWidth();

  return (
    <>
      {collapsed ? (
        collapsedStyle === "hidden" ? null : (
        <aside
          data-section="vault-sidebar-collapsed"
          className="flex w-11 shrink-0 flex-col items-center gap-1 border-r border-border/60 bg-background py-3"
        >
          <Button size="icon-sm" variant="ghost" title="展开侧边栏" onClick={() => setCollapsed(false)}>
            <PanelLeftOpen size={15} />
          </Button>
          <Button size="icon-sm" variant="ghost" title="新建主机" onClick={() => onNewHost(null)}>
            <Plus size={15} />
          </Button>
          <Button size="icon-sm" variant="ghost" title="新分组" onClick={() => onNewGroup(null)}>
            <FolderPlus size={15} />
          </Button>
        </aside>
        )
      ) : (
      <aside
        ref={asideRef}
        data-section="vault-sidebar"
        data-order-locked={filter !== null ? "true" : undefined}
        className="relative flex shrink-0 flex-col border-r border-border/60 bg-background"
        style={{ width }}
      >
        <ResizeHandle
          side="right"
          width={width}
          bounds={bounds}
          onResize={onResize}
          onCommit={onCommit}
          onReset={onReset}
        />
        <div className="flex items-center gap-1 p-3 pb-2">
          <Button
            variant="outline"
            className="h-[30px] flex-1 text-[13px] font-normal"
            onClick={() => onNewHost(null)}
          >
            <Plus size={14} /> 新建主机
          </Button>
          <Button size="icon-sm" variant="ghost" title="收起侧边栏" onClick={() => setCollapsed(true)}>
            <PanelLeftClose size={15} />
          </Button>
        </div>
        <TreeSearchBox
          value={search}
          onChange={(value) => {
            setSearch(value);
            setActiveId(null);
          }}
          onMove={(delta) => {
            if (order.length === 0) return;
            const index = order.findIndex((host) => host.id === activeHostId);
            const next =
              index < 0
                ? delta > 0
                  ? 0
                  : order.length - 1
                : Math.min(order.length - 1, Math.max(0, index + delta));
            setActiveId(order[next].id);
          }}
          onConnect={() => {
            const target = order.find((host) => host.id === activeHostId) ?? order[0];
            if (target) treeActions.connect(target);
          }}
        />
        <ScrollArea className="min-h-0 flex-1">
          <div className="px-2 pb-2">
            {filter !== null && filter.total === 0 && filter.groupIds.size === 0 && !filter.ungrouped ? (
              <div data-section="tree-search-empty" className="px-2 py-6 text-center text-[13px] text-muted-foreground">
                没有匹配的主机
              </div>
            ) : (
              <>
            <GroupRow
              label="全部主机"
              count={filter ? filter.total : hosts.length}
              active={selectedGroup === "all"}
              onClick={() => onSelectGroup("all")}
            />
            {groupsUnder(groups, null, filter)
              .map((group) => (
                <GroupBranch
                  key={group.id}
                  group={group}
                  depth={0}
                  trail={[]}
                  last={false}
                  groups={groups}
                  hosts={hosts}
                  filter={filter}
                  activeHostId={activeHostId}
                  selected={selectedGroup}
                  selectedHostId={selectedHostId}
                  isExpanded={isExpanded}
                  onToggle={toggle}
                  onSelect={onSelectGroup}
                  onNewGroup={onNewGroup}
                  onEditGroup={onEditGroup}
                  onDeleteGroup={onDeleteGroup}
                  hostActions={treeActions}
                />
              ))}
            {(filter === null || filter.ungrouped) && (
            <GroupRow
              label={filter ? <Highlight text="未分组" term={filter.text} /> : "未分组"}
              count={hostsUnder(hosts, null, filter).length}
              active={selectedGroup === null}
              onClick={() => onSelectGroup(null)}
              dropGroup={null}
              onContextMenu={
                <>
                  <ContextMenuItem onSelect={() => onNewGroup(null)}>
                    <FolderPlus className="mr-2 h-3.5 w-3.5" /> 新建分组
                  </ContextMenuItem>
                  <ContextMenuItem onSelect={() => onNewGroup(null, { adoptUngrouped: true })}>
                    <Pencil className="mr-2 h-3.5 w-3.5" /> 重命名（建成分组）…
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                  <ContextMenuItem onSelect={() => void resetLayerOrder(null)}>
                    <ArrowDownAZ className="mr-2 h-3.5 w-3.5" /> 按名字重新排序
                  </ContextMenuItem>
                </>
              }
              expanded={isExpanded(UNGROUPED_KEY)}
              onToggle={() => toggle(UNGROUPED_KEY)}
            />
            )}
            {(filter === null || filter.ungrouped) &&
              isExpanded(UNGROUPED_KEY) &&
              hostsUnder(hosts, null, filter).map((host, index, list) => (
                <HostLeaf
                  key={host.id}
                  host={host}
                  depth={1}
                  guide={{ trail: [], last: index === list.length - 1, kind: "host" }}
                  selected={selectedHostId === host.id}
                  actions={treeActions}
                  term={filter?.text}
                  searchActive={activeHostId === host.id}
                />
              ))}
              </>
            )}
          </div>
        </ScrollArea>
        <div className="flex shrink-0 items-center gap-1 border-t border-border/60 p-2">
          <Button
            variant="ghost"
            className="h-[30px] flex-1 text-[13px] font-normal text-muted-foreground"
            onClick={() => onNewGroup(null)}
          >
            <FolderPlus size={14} /> 新分组
          </Button>
        </div>
        {infoHost && (
          <HostInfoPanel
            host={infoHost}
            hosts={hosts}
            proxies={proxies}
            onCopyAddress={() => hostActions.copy(infoHost, "address")}
            onClose={closeHostInfo}
          />
        )}
      </aside>
      )}
      <HostDragGhost />
    </>
  );
}

/** 分组导航的一行（"全部主机"/"未分组"共用）。 */
function GroupRow({
  label,
  count,
  active,
  onClick,
  depth = 0,
  onContextMenu,
  expanded,
  onToggle,
  dropGroup,
  guide,
  orderGroup,
}: {
  label: React.ReactNode;
  count: number;
  active: boolean;
  onClick: () => void;
  depth?: number;
  /** 在父分组下的连接线信息；顶层行没有。 */
  guide?: TreeGuide;
  onContextMenu?: React.ReactNode;
  /** 能把主机拖进来：分组 id，或者 null = 未分组。"全部主机"没有。 */
  dropGroup?: DropGroup;
  /** 有就是可以展开的分组行（"全部主机"没有）。 */
  expanded?: boolean;
  onToggle?: () => void;
  /** 真实的分组行：可以拖动调整它在同一层的顺序（"全部主机"/"未分组"没有）。 */
  orderGroup?: Group;
}) {
  const dropping = useIsDropTarget(dropGroup === undefined ? -1 : dropGroup);
  const insertMark = useInsertMark("group", orderGroup?.id ?? -1);
  const content = (
    <button
      onClick={() => !dragJustEnded() && onClick()}
      onDoubleClick={onToggle}
      data-drop-group={dropGroup === undefined ? undefined : (dropGroup ?? "none")}
      data-order-group={orderGroup?.id}
      data-order-parent={orderGroup ? String(orderGroup.parent_id ?? "root") : undefined}
      onPointerDown={orderGroup ? (event) => startGroupDrag(orderGroup, event) : undefined}
      {...guideAttrs(guide)}
      style={{ paddingLeft: 4 + depth * 14, ...(guide ? treeGuideStyle(depth, guide) : null) }}
      className={cn(
        "relative flex h-7 w-full cursor-pointer items-center gap-1.5 rounded-md pr-2 text-left text-[13px] transition-colors",
        active ? "bg-accent font-medium text-accent-foreground" : "hover:bg-accent/50",
        dropping && "bg-primary/15 ring-1 ring-inset ring-primary",
      )}
    >
      <InsertLine mark={insertMark} />
      {onToggle ? (
        <span
          role="button"
          aria-label={expanded ? "收起" : "展开"}
          data-section="tree-toggle"
          onClick={(event) => {
            event.stopPropagation();
            onToggle();
          }}
          onDoubleClick={(event) => event.stopPropagation()}
          className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-foreground/10"
        >
          {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        </span>
      ) : (
        <span className="w-4 shrink-0" />
      )}
      {onToggle &&
        (expanded ? (
          <FolderOpen size={14} className="shrink-0 text-muted-foreground" />
        ) : (
          <Folder size={14} className="shrink-0 text-muted-foreground" />
        ))}
      <span className="truncate">{label}</span>
      {/* 选中行是主色填充：数字用 primary-foreground 加透明度，不然在填充色上看不见 */}
      <span
        className={cn(
          "ml-auto shrink-0 text-[11px]",
          "text-muted-foreground",
        )}
      >
        {count}
      </span>
    </button>
  );

  if (!onContextMenu) return content;
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{content}</ContextMenuTrigger>
      <ContextMenuContent className="w-40">{onContextMenu}</ContextMenuContent>
    </ContextMenu>
  );
}

/** 一个分组（含子分组）和它的右键菜单。 */
function GroupBranch({
  group,
  depth,
  trail,
  last,
  groups,
  hosts,
  filter,
  activeHostId,
  selected,
  selectedHostId,
  isExpanded,
  onToggle,
  onSelect,
  onNewGroup,
  onEditGroup,
  onDeleteGroup,
  hostActions,
}: {
  group: Group;
  depth: number;
  /** 连接线：祖先各层的竖线要不要穿过这一行（见 `TreeGuide.trail`）。 */
  trail: boolean[];
  /** 是不是父分组里的最后一个子项（顶层不用）。 */
  last: boolean;
  groups: Group[];
  hosts: Host[];
  /** 搜索框的过滤结果；没在搜索是 null。 */
  filter: TreeFilter | null;
  /** 搜索结果里键盘高亮的那台。 */
  activeHostId: number | null;
  selected: number | null | "all";
  selectedHostId: number | null;
  isExpanded: (key: string) => boolean;
  onToggle: (key: string) => void;
  onSelect: (id: number) => void;
  onNewGroup: (parentId: number | null, options?: { adoptUngrouped?: boolean }) => void;
  onEditGroup: (group: Group) => void;
  onDeleteGroup: (group: Group) => void;
  hostActions: HostActions;
}) {
  const children = groupsUnder(groups, group.id, filter);
  const key = groupKey(group.id);
  const open = isExpanded(key);
  // 子项的显示顺序是先子分组后主机；最后一个子项画成 └
  const shownHosts = open ? hostsUnder(hosts, group.id, filter) : [];
  // 子项们的祖先竖线：本组后面还有兄弟，竖线就得穿过它的子孙行（顶层没有竖线）
  const childTrail = depth === 0 ? [] : [...trail, !last];
  return (
    <>
      <GroupRow
        label={filter ? <Highlight text={group.name} term={filter.text} /> : group.name}
        guide={depth === 0 ? undefined : { trail, last, kind: "group" }}
        count={filter ? (filter.counts.get(group.id) ?? 0) : totalUnder(groups, hosts, group.id)}
        active={selected === group.id}
        onClick={() => onSelect(group.id)}
        dropGroup={group.id}
        orderGroup={group}
        depth={depth}
        expanded={open}
        onToggle={() => onToggle(key)}
        onContextMenu={
          <>
            <ContextMenuItem onSelect={() => onNewGroup(group.id)}>
              <FolderPlus className="mr-2 h-3.5 w-3.5" /> 新建子分组
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => onEditGroup(group)}>
              <Pencil className="mr-2 h-3.5 w-3.5" /> 重命名
            </ContextMenuItem>
            <ContextMenuSeparator />
            <GroupOrderItems group={group} locked={filter !== null} />
            <ContextMenuSeparator />
            <ContextMenuItem
              destructive
              onSelect={() => onDeleteGroup(group)}
            >
              <Trash2 className="mr-2 h-3.5 w-3.5" /> 删除分组
            </ContextMenuItem>
          </>
        }
      />
      {open &&
        children.map((child, index) => (
          <GroupBranch
            key={child.id}
            group={child}
            depth={depth + 1}
            trail={childTrail}
            last={shownHosts.length === 0 && index === children.length - 1}
            groups={groups}
            hosts={hosts}
            filter={filter}
            activeHostId={activeHostId}
            selected={selected}
            selectedHostId={selectedHostId}
            isExpanded={isExpanded}
            onToggle={onToggle}
            onSelect={onSelect}
            onNewGroup={onNewGroup}
            onEditGroup={onEditGroup}
            onDeleteGroup={onDeleteGroup}
            hostActions={hostActions}
          />
        ))}
      {open &&
        shownHosts.map((host, index) => (
          <HostLeaf
            key={host.id}
            host={host}
            depth={depth + 1}
            guide={{ trail: childTrail, last: index === shownHosts.length - 1, kind: "host" }}
            selected={selectedHostId === host.id}
            actions={hostActions}
            term={filter?.text}
            searchActive={activeHostId === host.id}
          />
        ))}
    </>
  );
}

export interface HostActions {
  /** `clickCount` 是点击计数（`MouseEvent.detail`），双击的第二下是 2；没有鼠标事件的调用方不传。 */
  select: (host: Host, clickCount?: number) => void;
  connect: (host: Host) => void;
  openSftp: (host: Host) => void;
  remove: (host: Host) => void;
  copy: (host: Host, kind: HostCopyKind) => void;
  installKey?: (host: Host) => void;
  /** 右键「编辑」；没给就和单击一样（主机页单击就是打开详情）。 */
  edit?: (host: Host) => void;
  forwards?: (host: Host) => void;
}

/** 把文字里匹配搜索词（已转小写）的部分用 <mark> 标出来，不区分大小写。 */
function Highlight({ text, term }: { text: string; term: string }) {
  if (term === "") return <>{text}</>;
  const lower = text.toLowerCase();
  // 个别字符转小写后长度会变，位置对不上就不高亮，宁可不标也别标错
  if (lower.length !== text.length) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  let from = 0;
  for (let at = lower.indexOf(term); at >= 0; at = lower.indexOf(term, from)) {
    if (at > from) parts.push(text.slice(from, at));
    parts.push(
      <mark key={at} className="rounded-sm bg-primary/25 text-inherit">
        {text.slice(at, at + term.length)}
      </mark>,
    );
    from = at + term.length;
  }
  parts.push(text.slice(from));
  return <>{parts}</>;
}

/** 名字没匹配、是靠地址 / 用户名 / 标签匹配到的：返回匹配的那一项，让用户看得出为什么在结果里。 */
function matchHint(host: Host, term: string): string | null {
  if (host.name.toLowerCase().includes(term)) return null;
  if (host.host.toLowerCase().includes(term)) return host.host;
  if (host.username.toLowerCase().includes(term)) return host.username;
  return host.tags.find((tag) => tag.toLowerCase().includes(term)) ?? null;
}

/**
 * 主机树顶部的搜索框（像 Xshell 会话管理器）。输入法：普通输入框，按 IME-3 属于英文区，
 * 不需要特别处理。
 */
function TreeSearchBox({
  value,
  onChange,
  onMove,
  onConnect,
}: {
  value: string;
  onChange: (value: string) => void;
  onMove: (delta: 1 | -1) => void;
  onConnect: () => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <div className="relative shrink-0 px-3 pb-2">
      <Search className="pointer-events-none absolute left-5 top-[calc(50%-4px)] h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
      <Input
        ref={inputRef}
        data-section="tree-search"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return;
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            onMove(event.key === "ArrowDown" ? 1 : -1);
          } else if (event.key === "Enter") {
            event.preventDefault();
            onConnect();
          } else if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (value !== "") onChange("");
            else inputRef.current?.blur();
          }
        }}
        className="h-7 pl-7 pr-7 text-[13px] font-normal"
        placeholder="搜索主机"
        aria-label="搜索主机树"
        spellCheck={false}
        autoComplete="off"
      />
      {value !== "" && (
        <button
          type="button"
          title="清空搜索"
          aria-label="清空搜索"
          onClick={() => {
            onChange("");
            inputRef.current?.focus();
          }}
          className="absolute right-4 top-[calc(50%-4px)] flex h-5 w-5 -translate-y-1/2 cursor-pointer items-center justify-center rounded text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
        >
          <X size={13} />
        </button>
      )}
    </div>
  );
}

/** 树里的一台主机：单击看详情，双击连接，右键菜单和卡片一样。 */
function HostLeaf({
  host,
  depth,
  guide,
  selected,
  actions,
  term,
  searchActive = false,
}: {
  host: Host;
  depth: number;
  /** 在父分组下的连接线信息。 */
  guide: TreeGuide;
  selected: boolean;
  actions: HostActions;
  /** 树搜索框里的词（小写）；用来高亮，没在搜索不传。 */
  term?: string;
  /** 搜索结果里键盘高亮的行。 */
  searchActive?: boolean;
}) {
  const hint = term ? matchHint(host, term) : null;
  const insertMark = useInsertMark("host", host.id);
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <button
          data-section="tree-host"
          data-order-host={host.id}
          data-search-active={searchActive ? "true" : undefined}
          data-drop-group={host.group_id ?? "none"}
          onPointerDown={(event) => startHostDrag(host, event)}
          onClick={(event) => !dragJustEnded() && actions.select(host, event.detail)}
          onDoubleClick={() => actions.connect(host)}
          title={`${host.username}@${host.host}:${host.port}（双击连接）`}
          {...guideAttrs(guide)}
          style={{ paddingLeft: 4 + depth * 14 + 16, ...treeGuideStyle(depth, guide) }}
          className={cn(
            "relative flex h-7 w-full cursor-pointer items-center gap-1.5 rounded-md pr-2 text-left text-[13px] transition-colors",
            selected ? "bg-accent text-accent-foreground" : "hover:bg-accent/50",
            searchActive && "bg-accent/70 ring-1 ring-inset ring-primary/60",
          )}
        >
          <InsertLine mark={insertMark} />
          <OsIcon hostId={host.id} size="tree" />
          <span className="truncate">{term ? <Highlight text={host.name} term={term} /> : host.name}</span>
          {hint && (
            <span data-section="tree-match-hint" className="ml-auto max-w-[45%] shrink-0 truncate text-[11px] text-muted-foreground">
              <Highlight text={hint} term={term ?? ""} />
            </span>
          )}
        </button>
      </ContextMenuTrigger>
      <HostMenu
        host={host}
        onConnect={() => actions.connect(host)}
        onOpenSftp={() => actions.openSftp(host)}
        onEdit={() => (actions.edit ?? actions.select)(host)}
        onDelete={() => actions.remove(host)}
        onCopy={(kind) => actions.copy(host, kind)}
        onInstallKey={actions.installKey ? () => actions.installKey?.(host) : undefined}
        onForwards={actions.forwards ? () => actions.forwards?.(host) : undefined}
        orderLocked={term !== undefined}
      />
    </ContextMenu>
  );
}

/** 主机卡片（网格视图）。双击连接，右键出菜单，悬停显示"连接"。 */
function HostCard({
  host,
  groupName,
  onConnect,
  onOpenSftp,
  onSelect,
  onEdit,
  onDelete,
  onCopy,
  onInstallKey,
  onForwards,
}: {
  host: Host;
  groupName: string | null;
  onConnect: () => void;
  onOpenSftp: () => void;
  onSelect: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onCopy: (kind: HostCopyKind) => void;
  onInstallKey: () => void;
  onForwards: () => void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          onPointerDown={(event) => startHostDrag(host, event)}
          onClick={() => !dragJustEnded() && onSelect()}
          onDoubleClick={onConnect}
          className="group cursor-pointer rounded-lg border border-border/60 bg-card p-4 shadow-sm transition-colors hover:border-ring/60 hover:bg-accent/30"
        >
          <div className="flex items-start gap-3">
            <OsIcon hostId={host.id} size="card" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium">{host.name}</div>
              <div className="truncate font-mono text-xs text-muted-foreground">
                {host.username}@{host.host}:{host.port}
              </div>
            </div>
            <Button
              size="icon-sm"
              variant="secondary"
              title="连接"
              className="opacity-0 transition-opacity group-hover:opacity-100"
              onClick={(event) => {
                event.stopPropagation();
                onConnect();
              }}
            >
              <Play size={13} />
            </Button>
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-1">
            {groupName && <Badge variant="secondary">{groupName}</Badge>}
            {host.tags.map((tag) => (
              <Badge key={tag} variant="outline">
                {tag}
              </Badge>
            ))}
            {host.legacy_algorithms && <Badge variant="outline">旧算法</Badge>}
          </div>
        </div>
      </ContextMenuTrigger>
      <HostMenu
        host={host}
        onConnect={onConnect}
        onOpenSftp={onOpenSftp}
        onEdit={onEdit}
        onDelete={onDelete}
        onCopy={onCopy}
        onInstallKey={onInstallKey}
        onForwards={onForwards}
      />
    </ContextMenu>
  );
}

/** 主机行（列表视图）。 */
function HostRow({
  host,
  groupName,
  last,
  onConnect,
  onOpenSftp,
  onSelect,
  onEdit,
  onDelete,
  onCopy,
  onInstallKey,
  onForwards,
}: {
  host: Host;
  groupName: string | null;
  last: boolean;
  onConnect: () => void;
  onOpenSftp: () => void;
  onSelect: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onCopy: (kind: HostCopyKind) => void;
  onInstallKey: () => void;
  onForwards: () => void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          onPointerDown={(event) => startHostDrag(host, event)}
          onClick={() => !dragJustEnded() && onSelect()}
          onDoubleClick={onConnect}
          className={cn(
            "group grid cursor-pointer grid-cols-[minmax(0,1.4fr)_minmax(0,1.6fr)_minmax(0,0.8fr)_auto] items-center gap-3 px-4 py-2.5 text-sm transition-colors hover:bg-accent/30",
            !last && "border-b border-border/40",
          )}
        >
          <span className="truncate font-medium">{host.name}</span>
          <span className="truncate font-mono text-xs text-muted-foreground">
            {host.username}@{host.host}:{host.port}
          </span>
          <span className="truncate text-xs text-muted-foreground">{groupName ?? "未分组"}</span>
          <Button
            size="sm"
            variant="secondary"
            className="opacity-0 transition-opacity group-hover:opacity-100"
            onClick={(event) => {
              event.stopPropagation();
              onConnect();
            }}
          >
            <Play size={13} /> 连接
          </Button>
        </div>
      </ContextMenuTrigger>
      <HostMenu
        host={host}
        onConnect={onConnect}
        onOpenSftp={onOpenSftp}
        onEdit={onEdit}
        onDelete={onDelete}
        onCopy={onCopy}
        onInstallKey={onInstallKey}
        onForwards={onForwards}
      />
    </ContextMenu>
  );
}

/** 分组按树的顺序摊平（父组在前，子组跟在后面），带层级用来缩进。 */
export function flattenGroups(groups: Group[]): { group: Group; depth: number }[] {
  const out: { group: Group; depth: number }[] = [];
  const walk = (parentId: number | null, depth: number) => {
    for (const group of groupsUnder(groups, parentId, null)) {
      out.push({ group, depth });
      walk(group.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

/** 右键「移动到分组」：列出未分组和全部分组，当前所在的打勾、点不了。 */
function MoveToGroupMenu({ host }: { host: Host }) {
  const groups = useVault((state) => state.groups);
  const item = (groupId: number | null, label: string, depth: number) => {
    const current = host.group_id === groupId;
    return (
      <ContextMenuItem
        key={groupId ?? "none"}
        disabled={current}
        data-group-target={groupId ?? "none"}
        onSelect={() => void moveHostToGroup(host, groupId)}
        style={{ paddingLeft: 8 + depth * 12 }}
      >
        <Check className={cn("mr-2 h-3.5 w-3.5 shrink-0", !current && "opacity-0")} />
        <span className="truncate">{label}</span>
      </ContextMenuItem>
    );
  };
  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger>
        <FolderInput className="mr-2 h-3.5 w-3.5" /> 移动到分组
      </ContextMenuSubTrigger>
      <ContextMenuSubContent className="max-h-80 w-48 overflow-y-auto">
        {item(null, "未分组", 0)}
        {groups.length > 0 && <ContextMenuSeparator />}
        {flattenGroups(groups).map(({ group, depth }) => item(group.id, group.name, depth))}
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}

/** 右键「上移 / 下移」（树里的主机）：第一个的上移、最后一个的下移置灰；搜索时过滤后的列表不连续，全部置灰。 */
function HostOrderItems({ host, locked }: { host: Host; locked: boolean }) {
  const hosts = useVault((state) => state.hosts);
  const [canUp, canDown] = canStepHost(hosts, host);
  return (
    <>
      <ContextMenuItem disabled={locked || !canUp} data-order-action="up" onSelect={() => void stepHost(host, -1)}>
        <ArrowUp className="mr-2 h-3.5 w-3.5" /> 上移
      </ContextMenuItem>
      <ContextMenuItem disabled={locked || !canDown} data-order-action="down" onSelect={() => void stepHost(host, 1)}>
        <ArrowDown className="mr-2 h-3.5 w-3.5" /> 下移
      </ContextMenuItem>
    </>
  );
}

/** 分组右键的「上移 / 下移 / 按名字重新排序」。 */
function GroupOrderItems({ group, locked }: { group: Group; locked: boolean }) {
  const groups = useVault((state) => state.groups);
  const [canUp, canDown] = canStepGroup(groups, group);
  return (
    <>
      <ContextMenuItem disabled={locked || !canUp} data-order-action="up" onSelect={() => void stepGroup(group, -1)}>
        <ArrowUp className="mr-2 h-3.5 w-3.5" /> 上移
      </ContextMenuItem>
      <ContextMenuItem disabled={locked || !canDown} data-order-action="down" onSelect={() => void stepGroup(group, 1)}>
        <ArrowDown className="mr-2 h-3.5 w-3.5" /> 下移
      </ContextMenuItem>
      <ContextMenuItem data-order-action="reset" onSelect={() => void resetLayerOrder(group.id)}>
        <ArrowDownAZ className="mr-2 h-3.5 w-3.5" /> 按名字重新排序
      </ContextMenuItem>
    </>
  );
}

function HostMenu({
  host,
  onConnect,
  onOpenSftp,
  onEdit,
  onDelete,
  onCopy,
  onInstallKey,
  onForwards,
  orderLocked,
}: {
  host: Host;
  onConnect: () => void;
  onOpenSftp: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onCopy: (kind: HostCopyKind) => void;
  onInstallKey?: () => void;
  onForwards?: () => void;
  /** 给了就在菜单里显示「上移 / 下移」（树里的主机行）；值 = 当前是不是在搜索（搜索时置灰）。 */
  orderLocked?: boolean;
}) {
  return (
    <ContextMenuContent className="w-44">
      <ContextMenuItem onSelect={onConnect}>
        <Play className="mr-2 h-3.5 w-3.5" /> 连接
      </ContextMenuItem>
      <ContextMenuItem onSelect={onOpenSftp}>
        <FolderTree className="mr-2 h-3.5 w-3.5" /> 打开 SFTP
      </ContextMenuItem>
      <ContextMenuItem onSelect={onEdit}>
        <Pencil className="mr-2 h-3.5 w-3.5" /> 编辑
      </ContextMenuItem>
      <ContextMenuItem onSelect={() => onCopy("name")}>
        <Check className="mr-2 h-3.5 w-3.5 opacity-0" /> 复制主机名
      </ContextMenuItem>
      <ContextMenuItem onSelect={() => onCopy("address")}>
        <Check className="mr-2 h-3.5 w-3.5 opacity-0" /> 复制地址
      </ContextMenuItem>
      {onInstallKey && (
        <ContextMenuItem onSelect={onInstallKey}>
          <KeyRound className="mr-2 h-3.5 w-3.5" /> 安装公钥…
        </ContextMenuItem>
      )}
      {onForwards && (
        <ContextMenuItem onSelect={onForwards}>
          <ArrowLeftRight className="mr-2 h-3.5 w-3.5" /> 端口转发…
        </ContextMenuItem>
      )}
      <MoveToGroupMenu host={host} />
      {orderLocked !== undefined && <HostOrderItems host={host} locked={orderLocked} />}
      <ContextMenuSeparator />
      <ContextMenuItem destructive onSelect={onDelete}>
        <Trash2 className="mr-2 h-3.5 w-3.5" /> 删除
      </ContextMenuItem>
    </ContextMenuContent>
  );
}

/** 空库 / 搜不到时的引导页（照 Netcatty 空状态的样子）。 */
function EmptyState({
  hasHosts,
  query,
  onNewHost,
}: {
  hasHosts: boolean;
  query: string;
  onNewHost: () => void;
}) {
  if (hasHosts) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 text-muted-foreground">
        <Search size={28} />
        <p className="text-sm">没有匹配「{query}」的主机</p>
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col items-center justify-center">
      <div className="flex max-w-md flex-col items-center gap-4 rounded-xl border border-border/60 bg-card px-8 py-10 text-center shadow-sm">
        <div className="rounded-full bg-secondary p-4 text-muted-foreground">
          <Server size={28} />
        </div>
        <div>
          <h3 className="text-base font-semibold">还没有主机</h3>
          <p className="mt-1 text-sm text-muted-foreground">
            添加一台主机，之后双击就能连接。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={onNewHost}>
            <Plus size={15} /> 新建主机
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          也可以直接在搜索框里输入 <span className="font-mono">user@host:port</span> 回车，
          不保存直接连。
        </p>
      </div>
    </div>
  );
}
