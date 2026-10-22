// Adapted from Netcatty components/ui/aside-panel.tsx
//! 右侧滑出面板的外壳：标题栏（可带返回键）+ 滚动内容 + 底部按钮区。
//!
//! Netcatty 原版还带宽度拖拽和宽度持久化，这里只保留它需要的那部分视觉和结构
//! （主机编辑、凭据管理、代理管理都用它）。

import { ArrowLeft, X } from "lucide-react";
import type { ReactNode } from "react";

import { cn } from "../../lib/utils";
import { ASIDE_PANEL_DEFAULT } from "../../state/panelWidths";
import { ResizeHandle, usePanelWidth } from "./resize-handle";
import { ScrollArea } from "./scroll-area";

interface AsidePanelProps {
  title: string;
  subtitle?: string;
  children: ReactNode;
  /** 右上角的额外按钮（比如"更多"） */
  actions?: ReactNode;
  showBackButton?: boolean;
  onBack?: () => void;
  onClose: () => void;
  className?: string;
}

export function AsidePanel({
  title,
  subtitle,
  children,
  actions,
  showBackButton,
  onBack,
  onClose,
  className,
}: AsidePanelProps) {
  // 宽度可调（UI-1 第 2 节）：所有侧栏共用一个宽度，存 `layout.aside_panel_width`。
  const {
    width: panelWidth,
    bounds,
    onResize,
    onCommit,
    onReset,
  } = usePanelWidth("aside", "layout.aside_panel_width", ASIDE_PANEL_DEFAULT);
  return (
    <div
      className={cn(
        "zz-panel-in relative flex h-full shrink-0 flex-col border-l border-border/60 bg-background",
        className,
      )}
      style={{ width: panelWidth }}
    >
      {/* 左边缘的拖拽把手（UI-1 第 2 节） */}
      <ResizeHandle
        width={panelWidth}
        bounds={bounds}
        onResize={onResize}
        onCommit={onCommit}
        onReset={onReset}
      />
      <div className="flex shrink-0 items-center justify-between border-b border-border/60 px-4 py-3">
        <div className="flex min-w-0 items-center gap-2">
          {showBackButton && onBack && (
            <button
              onClick={onBack}
              className="shrink-0 cursor-pointer rounded-md p-1 transition-colors hover:bg-muted"
            >
              <ArrowLeft size={18} />
            </button>
          )}
          <div className="min-w-0">
            <h3 className="truncate text-sm font-semibold">{title}</h3>
            {subtitle && (
              <p className="truncate text-xs text-muted-foreground">{subtitle}</p>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {actions}
          <button
            onClick={onClose}
            title="关闭"
            className="cursor-pointer rounded-md p-1.5 transition-colors hover:bg-muted"
          >
            <X size={18} />
          </button>
        </div>
      </div>
      {children}
    </div>
  );
}

/** 面板内容：自带滚动。 */
export function AsidePanelContent({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <ScrollArea
      className={cn(
        "min-w-0 flex-1 [&>[data-radix-scroll-area-viewport]>div]:!block [&>[data-radix-scroll-area-viewport]>div]:!min-w-0",
        className,
      )}
    >
      <div className="min-w-0 space-y-4 overflow-hidden p-4">{children}</div>
    </ScrollArea>
  );
}

/** 面板底部：保存 / 取消这类按钮。 */
export function AsidePanelFooter({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("shrink-0 border-t border-border/60 px-4 py-3", className)}>
      {children}
    </div>
  );
}

/** 面板里的一组字段：小标题 + 内容。 */
export function AsidePanelSection({
  title,
  children,
  className,
}: {
  title?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("space-y-2", className)}>
      {title && (
        <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          {title}
        </h4>
      )}
      {children}
    </section>
  );
}
