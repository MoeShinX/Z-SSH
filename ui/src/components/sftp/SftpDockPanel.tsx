// Adapted from Netcatty components/SftpSidePanel.tsx（面板的外壳、标题栏和窄版布局的思路）
//! 终端右侧的 SFTP 面板外壳（M3-3 第 1、3 节）：左边缘拖宽、标题栏、内容区。
//!
//! 内容是 `SftpTabView` 的 dock 模式（远端在上、本地在下）。这一层只管宽度和标题栏，
//! 会话（sftpId）由 App 按"当前焦点 pane 的主机"准备好再传进来。

import { ExternalLink, FolderTree, Monitor, X } from "lucide-react";
import type { ReactNode } from "react";

import { cn } from "../../lib/utils";
import { SFTP_PANEL_DEFAULT } from "../../state/panelWidths";
import { ResizeHandle, usePanelWidth } from "../ui/resize-handle";

interface SftpDockPanelProps {
  /** 标题里显示的主机名。 */
  hostLabel: string;
  /** 三块同屏时由布局算出的实际宽度。 */
  widthOverride: number | null;
  /** AI 面板占的宽度（拖宽上限要让给它）。 */
  reservedWidth: number;
  /** "在新标签中打开"：变回独立的双栏标签（保留两栏目录）。没有会话时不给。 */
  onOpenInTab?: () => void;
  onClose: () => void;
  /** 本地栏收起时给：标题栏多一个「本地」按钮，点了把本地栏请回来。 */
  onShowLocal?: () => void;
  /** 标题栏下面的一行额外控件（跟随终端目录的开关）。 */
  toolbar?: ReactNode;
  children: ReactNode;
}

export default function SftpDockPanel({
  hostLabel,
  widthOverride,
  reservedWidth,
  onOpenInTab,
  onClose,
  onShowLocal,
  toolbar,
  children,
}: SftpDockPanelProps) {
  const {
    width,
    bounds,
    onResize,
    onCommit,
    onReset,
  } = usePanelWidth("sftp", "layout.sftp_panel_width", SFTP_PANEL_DEFAULT, {
    reserved: reservedWidth,
    override: widthOverride,
  });
  return (
    <div
      data-dock-panel="sftp"
      className="relative flex h-full shrink-0 flex-col border-l border-border/60 bg-background"
      style={{ width }}
    >
      {/* 左边缘的拖拽把手：拖宽、双击恢复默认，和 AI 面板一样（UI-1） */}
      <ResizeHandle
        width={width}
        bounds={bounds}
        onResize={onResize}
        onCommit={onCommit}
        onReset={onReset}
      />
      <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border/60 px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <FolderTree size={16} className="shrink-0 text-sky-400" />
          <span className="shrink-0 text-sm font-semibold">文件</span>
          <span className="min-w-0 truncate text-xs text-muted-foreground" title={hostLabel}>
            {hostLabel}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {onShowLocal && (
            <button
              onClick={onShowLocal}
              title="显示本地栏"
              data-action="sftp-show-local"
              className="flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            >
              <Monitor size={14} />
              本地
            </button>
          )}
          {onOpenInTab && (
            <button
              onClick={onOpenInTab}
              title="在新标签中打开"
              data-action="sftp-undock"
              className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            >
              <ExternalLink size={15} />
            </button>
          )}
          <button
            onClick={onClose}
            title="关闭文件面板"
            className={cn(
              "rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
            )}
          >
            <X size={16} />
          </button>
        </div>
      </div>
      {toolbar}
      <div className="flex min-h-0 flex-1 flex-col">{children}</div>
    </div>
  );
}
