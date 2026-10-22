// Adapted from Netcatty components/TopTabs.tsx（视觉和结构）
//! 自绘标题栏：最左固定"主机"标签，后面是终端标签，右侧是主题入口和
//! 窗口按钮（最小化 / 最大化 / 关闭）。空白处可以拖动窗口、双击最大化——
//! 这些由 Tauri 的 data-tauri-drag-region 完成。
//!
//! 条上用的是 `data-tauri-drag-region="deep"`：光写一个裸属性（或者 `"true"`）
//! 只认"鼠标正好点在这个元素本身"，点在子元素上会判定失败。标签滚动容器、按钮
//! 之间的空隙都是子元素，所以必须用 `deep` 让整棵子树都是拖动区。可点的东西
//! （标签、按钮、窗口控制）各自带 `data-tauri-drag-region={false}` 挡掉，
//! 点击行为不受影响。

import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Bot,
  Command,
  Copy,
  ExternalLink,
  FileText,
  FolderOpen,
  FolderTree,
  LayoutGrid,
  Minus,
  PanelLeftClose,
  PanelLeftOpen,
  PanelRightClose,
  PanelTop,
  Palette,
  Plus,
  RotateCw,
  Settings,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import type { PaneSession, Tab } from "../../state/sessions";
import { paneIds } from "../../state/splitTree";
import { tabDot } from "../../state/sessions";
import type { TabAiBadge } from "../../state/aiTarget";
import { useResolvedMode, useSettings, useTerminalITheme, useTerminalMode } from "../../stores/settings";
import { SkinGrid, THEME_MODES } from "../settings/AppearancePickers";
import { cn } from "../../lib/utils";
import { Dropdown, DropdownContent, DropdownTrigger } from "../ui/dropdown";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../ui/tooltip";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "../ui/context-menu";
import { isOutsideWindow } from "../../state/fileWindowTabs";
import { useHostTreeWidth } from "../ui/resize-handle";
import logoUrl from "../../assets/logo.png";

interface TopTabsProps {
  tabs: Tab[];
  activeTabId: string;
  panes: Record<string, PaneSession>;
  /** 现在显示的是主机页、终端还是设置页 */
  view: "vault" | "terminal" | "settings";
  onShowVault: () => void;
  onOpenSettings: () => void;
  onSelectTab: (tabId: string) => void;
  onCloseTab: (tabId: string) => void;
  /** 标签右键：重新连接这个终端标签里已经断开的会话。 */
  onReconnectTab?: (tabId: string) => void;
  /** 标签右键：再开一个连同一台主机的标签。 */
  onDuplicateTab?: (tabId: string) => void;
  /** 标签右键：关闭其他 / 右侧 / 所有标签。 */
  onCloseTabs?: (tabId: string, which: "others" | "right" | "all") => void;
  onNewTab: () => void;
  /** 标签上的 AI 活动提示：running = AI 正在这个标签里执行命令，unseen = 跑完了还没看过。 */
  aiBadges?: Record<string, TabAiBadge>;
  /** AI 面板是否展开；只有终端标签下有意义。 */
  aiOpen: boolean;
  onToggleAI: () => void;
  /** SFTP 面板（"文件"按钮）是否展开（M3-3）。 */
  sftpOpen: boolean;
  onToggleSftp: () => void;
  /** 终端底部的快捷命令栏开着没有。 */
  quickBarOpen?: boolean;
  onToggleQuickBar?: () => void;
  /** SFTP 标签能不能停靠到某个终端标签（M3-3 第 2 节）：ok / other-host（别的主机）/ none。 */
  dockVerdict?: (sftpTabId: string, targetTabId: string) => "ok" | "other-host" | "none";
  /** 松手：把 SFTP 标签停靠到终端标签上。 */
  onDockSftpTab?: (sftpTabId: string, targetTabId: string) => void;
  /** SFTP 标签拖出主窗口松手、或者右键「移到文件窗口」：交给文件窗口（像 Xftp 那样）。 */
  onDetachSftpTab?: (sftpTabId: string) => void;
  /** 拖动中悬停到哪个标签了（App 据此显示停靠提示、弹"另一台主机"的提示）；拖动结束传 null。 */
  /** 内置编辑器标签有没有未保存的修改（标题前加 ●）。 */
  editorDirty?: (editId: number) => boolean;
  onSftpDragHover?: (
    info: { sftpTabId: string; overTabId: string | null; verdict: "ok" | "other-host" | "none" } | null,
  ) => void;
}

/** 拖动的起步阈值（像素）：小于它按"点击"算。 */
const DRAG_THRESHOLD = 6;

export default function TopTabs({
  tabs,
  activeTabId,
  panes,
  view,
  onShowVault,
  onOpenSettings,
  onSelectTab,
  onCloseTab,
  onReconnectTab,
  onDuplicateTab,
  onCloseTabs,
  onNewTab,
  aiBadges,
  aiOpen,
  onToggleAI,
  sftpOpen,
  onToggleSftp,
  quickBarOpen = false,
  onToggleQuickBar,
  dockVerdict,
  onDockSftpTab,
  onDetachSftpTab,
  onSftpDragHover,
  editorDirty,
}: TopTabsProps) {
  // 主机树展开时（主机页、终端界面都有），左侧块（logo、开关、「主机」标签）的宽度等于
  // 主机树的宽度，终端标签从主机树右边缘开始排，和下面终端区域的左边缘对齐。
  // 拖主机树宽度时 width 实时跟着变（拖动值是共享的）；收起、设置页保持原来紧挨着排。
  const hostTreeCollapsed = useSettings((state) => state["layout.vault_sidebar_collapsed"]);
  const { width: hostTreeWidth } = useHostTreeWidth();
  const alignToHostTree = !hostTreeCollapsed && (view === "vault" || view === "terminal");

  // SFTP 标签的拖动：用指针事件自己做（Tauri 默认开着 dragDropEnabled，Windows 上
  // WebView 里的 HTML5 拖放不可用），松手时看指针下面是哪个标签。
  const [drag, setDrag] = useState<{
    tabId: string;
    label: string;
    x: number;
    y: number;
    over: string | null;
    verdict: "ok" | "other-host" | "none";
    /** 指针已经在窗口外面：松手就移到文件窗口。 */
    outside: boolean;
  } | null>(null);
  const suppressClick = useRef(false);

  const startSftpDrag = useCallback(
    (tab: Tab, event: React.PointerEvent) => {
      if (event.button !== 0 || (!onDockSftpTab && !onDetachSftpTab)) return;
      if ((event.target as HTMLElement).closest("button")) return;
      const start = { x: event.clientX, y: event.clientY };
      // 拖到窗口外面也要继续收到指针事件（判断是不是拖出去了）
      try {
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
      } catch {
        // 有的环境（测试）没有指针捕获
      }
      let outside = false;
      let active = false;
      let latest: { over: string | null; verdict: "ok" | "other-host" | "none" } = {
        over: null,
        verdict: "none",
      };
      const probe = (x: number, y: number) => {
        const element = document.elementFromPoint(x, y)?.closest("[data-tab-id]");
        const over = element?.getAttribute("data-tab-id") ?? null;
        const verdict =
          over && over !== tab.id && dockVerdict ? dockVerdict(tab.id, over) : "none";
        return { over: verdict === "none" ? null : over, verdict };
      };
      const move = (e: PointerEvent) => {
        if (!active) {
          if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < DRAG_THRESHOLD) return;
          active = true;
          document.body.classList.add("zz-panel-resizing");
        }
        outside =
          Boolean(onDetachSftpTab) &&
          isOutsideWindow(e.clientX, e.clientY, window.innerWidth, window.innerHeight);
        latest = outside ? { over: null, verdict: "none" } : probe(e.clientX, e.clientY);
        setDrag({ tabId: tab.id, label: tab.title, x: e.clientX, y: e.clientY, outside, ...latest });
        onSftpDragHover?.({ sftpTabId: tab.id, overTabId: latest.over, verdict: latest.verdict });
      };
      const finish = (commit: boolean) => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        window.removeEventListener("pointercancel", cancel);
        window.removeEventListener("keydown", key, true);
        document.body.classList.remove("zz-panel-resizing");
        if (!active) return;
        suppressClick.current = true;
        window.setTimeout(() => (suppressClick.current = false), 0);
        setDrag(null);
        onSftpDragHover?.(null);
        if (commit && outside) {
          onDetachSftpTab?.(tab.id);
        } else if (commit && latest.over && latest.verdict === "ok") {
          onDockSftpTab?.(tab.id, latest.over);
        }
      };
      const up = () => finish(true);
      const cancel = () => finish(false);
      const key = (e: KeyboardEvent) => {
        if (e.key === "Escape") finish(false);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      window.addEventListener("pointercancel", cancel);
      window.addEventListener("keydown", key, true);
    },
    [dockVerdict, onDockSftpTab, onDetachSftpTab, onSftpDragHover],
  );

  return (
    <TooltipProvider delayDuration={400}>
      <div
        data-section="top-tabs"
        data-tauri-drag-region="deep"
        className="relative z-20 flex h-9 w-full shrink-0 items-end gap-1 bg-background select-none"
        style={{ color: "hsl(var(--foreground))" }}
      >
        {/* 左侧块：logo、主机树开关、「主机」标签。主机树展开时宽度 = 主机树宽度
            （-mr-1 抵掉条上 gap-1 的 4px，让终端标签正好从主机树右边缘起排）。 */}
        <div
          data-section="top-tabs-left"
          data-aligned={alignToHostTree ? "true" : "false"}
          className={cn("flex h-9 shrink-0 items-end gap-1 pl-3", alignToHostTree && "-mr-1 min-w-0")}
          style={alignToHostTree ? { width: hostTreeWidth } : undefined}
        >
          <img
            src={logoUrl}
            alt="Z-SSH"
            draggable={false}
            data-tauri-drag-region
            className="mb-2 h-[18px] w-[18px] shrink-0"
          />
          {/* 主机树侧栏开关（主机页和终端界面共用一个开关） */}
          <SidebarToggle />
          {/* 固定最左：主机页 */}
          <TabShape
            active={view === "vault"}
            onClick={onShowVault}
            className="rounded-md"
            label="主机"
            icon={<LayoutGrid size={14} />}
          />
        </div>

        {/* 终端标签 */}
        <div className="no-scrollbar flex h-9 min-w-0 flex-1 items-end gap-0.5 overflow-x-auto">
          {tabs.map((tab, tabIndex) => {
            // SFTP 标签：区别于终端的图标（M3-2 第 2 节），不显示状态点。
            if (tab.sftp) {
              return (
                <ContextMenu key={tab.id}>
                  <ContextMenuTrigger asChild>
                    <div className="contents">
                    <TabShape
                      tabId={tab.id}
                      tabKind="sftp"
                      active={view === "terminal" && tab.id === activeTabId}
                      onClick={() => {
                        if (!suppressClick.current) onSelectTab(tab.id);
                      }}
                      onClose={() => onCloseTab(tab.id)}
                      label={tab.title}
                      icon={<FolderTree size={14} className="text-sky-400" />}
                      onPointerDown={(event) => startSftpDrag(tab, event)}
                      dragging={drag?.tabId === tab.id}
                    />
                    </div>
                  </ContextMenuTrigger>
                  <ContextMenuContent className="w-48">
                    {onDetachSftpTab && (
                      <>
                        <ContextMenuItem onSelect={() => onDetachSftpTab(tab.id)}>
                          <ExternalLink className="mr-2 h-3.5 w-3.5" /> 移到文件窗口
                        </ContextMenuItem>
                        <ContextMenuSeparator />
                      </>
                    )}
                    <CloseItems
                      tabId={tab.id}
                      index={tabIndex}
                      count={tabs.length}
                      onCloseTab={onCloseTab}
                      onCloseTabs={onCloseTabs}
                    />
                  </ContextMenuContent>
                </ContextMenu>
              );
            }
            if (tab.editor) {
              return (
                <ContextMenu key={tab.id}>
                  <ContextMenuTrigger asChild>
                    <div className="contents">
                      <TabShape
                        tabId={tab.id}
                        tabKind="editor"
                        active={view === "terminal" && tab.id === activeTabId}
                        onClick={() => onSelectTab(tab.id)}
                        onClose={() => onCloseTab(tab.id)}
                        label={editorDirty?.(tab.editor.editId) ? `● ${tab.title}` : tab.title}
                        tooltip={tab.tooltip}
                        icon={<FileText size={14} className="text-emerald-400" />}
                      />
                    </div>
                  </ContextMenuTrigger>
                  <ContextMenuContent className="w-48">
                    <CloseItems
                      tabId={tab.id}
                      index={tabIndex}
                      count={tabs.length}
                      onCloseTab={onCloseTab}
                      onCloseTabs={onCloseTabs}
                    />
                  </ContextMenuContent>
                </ContextMenu>
              );
            }
            const phases = paneIds(tab.tree).map((paneId) => panes[paneId]?.phase ?? "idle");
            const dot = tabDot(phases);
            // 双击标签和右键「复制标签页」是同一个函数
            const duplicate = onDuplicateTab ? () => onDuplicateTab(tab.id) : undefined;
            return (
              <ContextMenu key={tab.id}>
                <ContextMenuTrigger asChild>
                  <div className="contents">
                    <TabShape
                      tabId={tab.id}
                      tabKind="terminal"
                      active={view === "terminal" && tab.id === activeTabId}
                      onClick={() => onSelectTab(tab.id)}
                      onDoubleClick={duplicate}
                      onClose={() => onCloseTab(tab.id)}
                      label={tab.title}
                      dotClass={dot.className}
                      dotTitle={dot.title}
                      aiBadge={aiBadges?.[tab.id]}
                      dockTarget={drag?.over === tab.id && drag.verdict === "ok" ? "ok" : undefined}
                    />
                  </div>
                </ContextMenuTrigger>
                <ContextMenuContent className="w-48">
                  {onReconnectTab && (
                    <ContextMenuItem
                      // 只有断开了才能重连；连着的时候是灰的
                      disabled={!phases.includes("disconnected")}
                      onSelect={() => onReconnectTab(tab.id)}
                    >
                      <RotateCw className="mr-2 h-3.5 w-3.5" /> 重新连接
                    </ContextMenuItem>
                  )}
                  {onDuplicateTab && (
                    <ContextMenuItem onSelect={duplicate}>
                      <Copy className="mr-2 h-3.5 w-3.5" /> 复制标签页
                    </ContextMenuItem>
                  )}
                  {(onReconnectTab || onDuplicateTab) && <ContextMenuSeparator />}
                  <CloseItems
                    tabId={tab.id}
                    index={tabIndex}
                    count={tabs.length}
                    onCloseTab={onCloseTab}
                    onCloseTabs={onCloseTabs}
                  />
                </ContextMenuContent>
              </ContextMenu>
            );
          })}
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                data-tauri-drag-region={false}
                onClick={onNewTab}
                className="mb-1 ml-0.5 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
              >
                <Plus size={15} />
              </button>
            </TooltipTrigger>
            <TooltipContent>新标签 (Ctrl+Shift+T)</TooltipContent>
          </Tooltip>
        </div>

        {/* 右侧：AI、主题入口、设置、窗口按钮 */}
        <div className="flex h-9 shrink-0 items-end">
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                data-tauri-drag-region={false}
                title="AI 助手 (Ctrl+Shift+A)"
                onClick={onToggleAI}
                className={cn(
                  "top-tab-utility-btn mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md",
                  aiOpen && "bg-foreground/10 text-foreground",
                )}
              >
                <Bot size={16} />
              </button>
            </TooltipTrigger>
            <TooltipContent>AI 助手 (Ctrl+Shift+A)</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                data-tauri-drag-region={false}
                data-action="toggle-sftp-panel"
                title="文件 (Ctrl+Shift+F)"
                onClick={onToggleSftp}
                className={cn(
                  "top-tab-utility-btn mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md",
                  sftpOpen && "bg-foreground/10 text-foreground",
                )}
              >
                <FolderOpen size={16} />
              </button>
            </TooltipTrigger>
            <TooltipContent>文件 (Ctrl+Shift+F)</TooltipContent>
          </Tooltip>
          {onToggleQuickBar && (
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  data-tauri-drag-region={false}
                  data-action="toggle-quick-commands"
                  title="快捷命令栏"
                  onClick={onToggleQuickBar}
                  className={cn(
                    "top-tab-utility-btn mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md",
                    quickBarOpen && "bg-foreground/10 text-foreground",
                  )}
                >
                  <Command size={16} />
                </button>
              </TooltipTrigger>
              <TooltipContent>快捷命令栏</TooltipContent>
            </Tooltip>
          )}
          <ThemeSwitcher />
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                data-tauri-drag-region={false}
                title="设置 (Ctrl+,)"
                onClick={onOpenSettings}
                className={cn(
                  "top-tab-utility-btn mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md",
                  view === "settings" && "bg-foreground/10 text-foreground",
                )}
              >
                <Settings size={16} />
              </button>
            </TooltipTrigger>
            <TooltipContent>设置</TooltipContent>
          </Tooltip>
          <WindowControls />
        </div>
        {/* 标签栏和内容区同一个底色，靠底部这条细线分开；当前标签盖住它下面那段，和内容连成一块 */}
        <div aria-hidden className="pointer-events-none absolute inset-x-0 bottom-0 h-px bg-border" />
        {drag && (
          <div
            data-section="tab-drag-ghost"
            className="pointer-events-none fixed z-50 flex items-center gap-1.5 rounded-md border border-primary/60 bg-background/95 px-2 py-1 text-xs shadow-lg"
            style={{ left: drag.x + 12, top: drag.y + 8 }}
          >
            <FolderTree size={12} className="text-sky-400" />
            {drag.outside ? `${drag.label} → 松开移到文件窗口` : drag.label}
          </div>
        )}
      </div>
    </TooltipProvider>
  );
}

interface TabShapeProps {
  label: string;
  active: boolean;
  onClick: () => void;
  /** 双击标签（只有终端标签传：复制标签页）。 */
  onDoubleClick?: () => void;
  onClose?: () => void;
  icon?: React.ReactNode;
  dotClass?: string;
  dotTitle?: string;
  /** AI 活动提示（和连接状态点分开，紫色）。 */
  aiBadge?: TabAiBadge;
  /** 悬停提示；没有就用 label。 */
  tooltip?: string;
  className?: string;
  tabId?: string;
  tabKind?: "terminal" | "sftp" | "editor";
  onPointerDown?: (event: React.PointerEvent) => void;
  /** 这个标签正在被拖（半透明）。 */
  dragging?: boolean;
  /** 拖动的 SFTP 标签悬停在这个终端标签上、可以停靠。 */
  dockTarget?: "ok";
}

/** 一个标签的形状：底边贴住内容区，选中时是内容区底色。 */
function TabShape({
  label,
  active,
  onClick,
  onDoubleClick,
  onClose,
  icon,
  dotClass,
  dotTitle,
  aiBadge,
  tooltip,
  className,
  tabId,
  tabKind,
  onPointerDown,
  dragging,
  dockTarget,
}: TabShapeProps) {
  // 当前终端标签借终端的底色，和下面的终端连成一块；主机名始终是界面的文字色（换终端配色不变色）。
  // 界面和终端一明一暗时不借：标签整块用界面配色，和终端之间就是自然的分界。
  const terminalTheme = useTerminalITheme();
  const uiMode = useResolvedMode();
  const terminalMode = useTerminalMode();
  const blend = active && tabKind === "terminal" && !dockTarget && uiMode === terminalMode;
  return (
    <div
      data-tauri-drag-region={false}
      data-blend={blend ? "terminal" : undefined}
      style={blend ? { backgroundColor: terminalTheme.background } : undefined}
      data-tab-id={tabId}
      data-tab-kind={tabKind}
      data-dock-target={dockTarget}
      onClick={onClick}
      onDoubleClick={onDoubleClick}
      onPointerDown={onPointerDown}
      title={tooltip ?? label}
      className={cn(
        "group relative mb-0 flex h-8 shrink-0 cursor-pointer items-center gap-2 rounded-t-md border-x border-t px-3 text-[13px] transition-colors duration-100",
        // 会话标签：名字短也不至于挤成一小块，太长就省略（悬停看全名）；关闭按钮不会被挤掉
        onClose ? "min-w-[112px] max-w-[200px] pr-1.5" : "max-w-56",
        dragging && "opacity-50",
        dockTarget && "border-transparent bg-primary/20 ring-2 ring-primary",
        active
          ? "z-10 border-border bg-background text-foreground"
          : "border-transparent text-muted-foreground hover:bg-foreground/5",
        className,
      )}
    >
      {icon}
      {dotClass && (
        <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", dotClass)} title={dotTitle} />
      )}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {aiBadge && (
        <span
          data-ai-badge={aiBadge}
          title={aiBadge === "running" ? "AI 正在这个标签里执行命令" : "AI 在这个标签里有新输出"}
          className={cn(
            "h-1.5 w-1.5 shrink-0 rounded-full bg-violet-500",
            aiBadge === "running" && "animate-pulse",
          )}
        />
      )}
      {onClose && (
        <button
          data-tauri-drag-region={false}
          onClick={(event) => {
            event.stopPropagation();
            onClose();
          }}
          // 连点关闭按钮不能冒泡成标签的双击
          onDoubleClick={(event) => event.stopPropagation()}
          title="关闭标签"
          aria-label={`关闭 ${label}`}
          className={cn(
            "flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded transition-[background-color,opacity] duration-100 hover:bg-foreground/15 hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            active ? "opacity-60" : "opacity-0 group-hover:opacity-60",
          )}
        >
          <X size={12} />
        </button>
      )}
    </div>
  );
}

/** 标题栏最左的主机树开关：展开时主机页和终端界面左边都有主机树。 */
function SidebarToggle() {
  const collapsed = useSettings((state) => state["layout.vault_sidebar_collapsed"]);
  const update = useSettings((state) => state.update);
  return (
    <button
      type="button"
      data-tauri-drag-region={false}
      data-action="toggle-host-sidebar"
      title={collapsed ? "展开侧边栏" : "收起侧边栏"}
      onClick={() => void update("layout.vault_sidebar_collapsed", !collapsed)}
      className="mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
    >
      {collapsed ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}
    </button>
  );
}

/** 标签右键菜单里的关闭几项（所有标签通用）。 */
function CloseItems({
  tabId,
  index,
  count,
  onCloseTab,
  onCloseTabs,
}: {
  tabId: string;
  index: number;
  count: number;
  onCloseTab: (tabId: string) => void;
  onCloseTabs?: (tabId: string, which: "others" | "right" | "all") => void;
}) {
  return (
    <>
      <ContextMenuItem onSelect={() => onCloseTab(tabId)}>
        <X className="mr-2 h-3.5 w-3.5" /> 关闭
      </ContextMenuItem>
      {onCloseTabs && (
        <>
          <ContextMenuItem disabled={count < 2} onSelect={() => onCloseTabs(tabId, "others")}>
            <PanelTop className="mr-2 h-3.5 w-3.5" /> 关闭其他标签页
          </ContextMenuItem>
          <ContextMenuItem disabled={index >= count - 1} onSelect={() => onCloseTabs(tabId, "right")}>
            <PanelRightClose className="mr-2 h-3.5 w-3.5" /> 关闭右侧标签页
          </ContextMenuItem>
          <ContextMenuItem
            destructive
            onSelect={() => onCloseTabs(tabId, "all")}
          >
            <Trash2 className="mr-2 h-3.5 w-3.5" /> 关闭所有标签页
          </ContextMenuItem>
        </>
      )}
    </>
  );
}

/** 主题入口：主题模式 + 当前明暗下的皮肤，选完立刻生效并存进 settings 表。 */
function ThemeSwitcher() {
  const mode = useSettings((state) => state.ui_theme_mode);
  const update = useSettings((state) => state.update);
  const resolved = useResolvedMode();
  const [open, setOpen] = useState(false);

  return (
    <Dropdown open={open} onOpenChange={setOpen}>
      {/* 触发按钮直接是 dropdown 的孩子：套一层 Tooltip 的话 onClick 会被克隆到
          Tooltip 上，按钮点不动（用原生 title 就不一样了） */}
      <DropdownTrigger asChild>
        <button
          data-tauri-drag-region={false}
          title="主题"
          className="top-tab-utility-btn mb-1 flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md"
        >
          <Palette size={16} />
        </button>
      </DropdownTrigger>
      <DropdownContent align="end" side="bottom" className="max-h-[70vh] w-72 overflow-y-auto p-2">
        <p className="px-1 pb-1 text-xs font-semibold text-muted-foreground">主题</p>
        <div className="flex flex-col gap-0.5">
          {THEME_MODES.map((item) => {
            const Icon = item.icon;
            const selected = item.value === mode;
            return (
              <button
                key={item.value}
                onClick={() => void update("ui_theme_mode", item.value)}
                className={cn(
                  "flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] transition-colors",
                  selected ? "bg-accent text-accent-foreground" : "hover:bg-accent hover:text-accent-foreground",
                )}
              >
                <Icon size={14} />
                <span className="flex-1">{item.label}</span>
                {selected && <span className="text-[11px] text-primary">✓</span>}
              </button>
            );
          })}
        </div>
        <p className="px-1 pb-1.5 pt-3 text-xs font-semibold text-muted-foreground">
          {resolved === "dark" ? "深色皮肤" : "浅色皮肤"}
        </p>
        <SkinGrid mode={resolved} compact />
      </DropdownContent>
    </Dropdown>
  );
}

/** 右上角的最小化 / 最大化 / 关闭（无边框窗口的自绘按钮）。 */
export function WindowControls() {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const window = getCurrentWindow();
    void window.isMaximized().then(setMaximized);
  }, []);

  return (
    <div className="flex h-9 items-end">
      <button
        data-tauri-drag-region={false}
        title="最小化"
        className="window-control-btn"
        onClick={() => void getCurrentWindow().minimize()}
      >
        <Minus size={15} />
      </button>
      <button
        data-tauri-drag-region={false}
        title={maximized ? "还原" : "最大化"}
        className="window-control-btn"
        onClick={() => {
          void getCurrentWindow()
            .toggleMaximize()
            .then(() => getCurrentWindow().isMaximized())
            .then(setMaximized);
        }}
      >
        {maximized ? <Copy size={13} /> : <Square size={12} />}
      </button>
      <button
        data-tauri-drag-region={false}
        title="关闭窗口"
        className="window-control-btn window-control-btn--close"
        onClick={() => void getCurrentWindow().close()}
      >
        <X size={15} />
      </button>
    </div>
  );
}
