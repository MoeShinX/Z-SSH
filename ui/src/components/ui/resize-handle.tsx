//! 面板左边缘的拖拽把手 + 宽度状态（UI-1 第 2 节）。
//!
//! - 悬停变色、光标是左右调整；按住拖动改宽度；
//! - **只在松手时**把宽度写回设置表、并让终端重排上报（拖动中只做视觉变化）；
//! - 双击恢复默认宽度；
//! - 用 `setPointerCapture`，鼠标快速划过终端区域也不会断（xterm 吞不掉）。

import { useCallback, useEffect, useRef, useState } from "react";

import {
  beginPanelDrag,
  clampPanelWidth,
  endPanelDrag,
  panelBounds,
  sanitizeStoredWidth,
  type PanelBounds,
  type PanelKind,
} from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import { cn } from "../../lib/utils";

/** 布局宽度的设置键（本机布局，M4 同步设计里注明不同步）。 */
export type PanelWidthKey =
  | "layout.ai_panel_width"
  | "layout.aside_panel_width"
  | "layout.sftp_panel_width"
  | "layout.overview_width";

/** 窗口宽度；面板的上限跟着它变（窗口缩小时面板收窄）。 */
export function useWindowWidth(): number {
  const [width, setWidth] = useState(() =>
    typeof window === "undefined" ? 1440 : window.innerWidth,
  );
  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}

/** 一个面板的宽度：设置表里读（非法值回退默认）、拖动时本地覆盖、松手写回。 */
export function usePanelWidth(
  kind: PanelKind,
  key: PanelWidthKey,
  fallback: number,
  /** `reserved`：同屏另一个面板占的宽度（上限要让给它）；`override`：三块同屏时
   *  由 `computeDockLayout` 算出来的实际宽度（窗口窄时比记住的宽度小）。 */
  options: { reserved?: number; override?: number | null } = {},
) {
  const stored = useSettings((state) => state[key]);
  const update = useSettings((state) => state.update);
  const windowWidth = useWindowWidth();
  const [dragWidth, setDragWidth] = useState<number | null>(null);

  const bounds = panelBounds(kind, windowWidth, options.reserved ?? 0);
  const width =
    dragWidth ??
    options.override ??
    clampPanelWidth(
      kind,
      sanitizeStoredWidth(stored, fallback),
      windowWidth,
      options.reserved ?? 0,
    );

  const onResize = useCallback((next: number) => setDragWidth(next), []);
  const onCommit = useCallback(
    (next: number) => {
      setDragWidth(null);
      void update(key, next);
    },
    [update, key],
  );
  const onReset = useCallback(() => {
    setDragWidth(null);
    void update(key, fallback);
  }, [update, key, fallback]);

  return { width, bounds, onResize, onCommit, onReset };
}

interface ResizeHandleProps {
  /** 当前的宽度（拖动的起点）。 */
  width: number;
  bounds: PanelBounds;
  /** 拖动中：实时改视觉宽度。 */
  onResize: (width: number) => void;
  /** 松手：保存 + 让终端重排上报。 */
  onCommit: (width: number) => void;
  /** 双击：恢复默认宽度。 */
  onReset: () => void;
  className?: string;
}

/** 左边缘的拖拽把手。 */
export function ResizeHandle({
  width,
  bounds,
  onResize,
  onCommit,
  onReset,
  className,
}: ResizeHandleProps) {
  /** 拖动起点：按下时的鼠标 x 和面板宽度。 */
  const start = useRef<{ x: number; width: number } | null>(null);
  /** 拖动过程中的最新宽度（松手时提交它）。 */
  const latest = useRef(width);
  const widthRef = useRef(width);
  const boundsRef = useRef(bounds);
  widthRef.current = width;
  boundsRef.current = bounds;

  const finish = useCallback(() => {
    if (!start.current) return;
    start.current = null;
    endPanelDrag();
    onCommit(latest.current);
  }, [onCommit]);

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      title="拖动调整宽度（双击恢复默认）"
      data-section="panel-resize-handle"
      className={cn(
        // 命中区域 8px（UI-1 0.2）；里面另有一条 2px 的可见高亮条，
        // 平时只有面板边框那条细线，悬停/按下才亮。
        "group absolute inset-y-0 left-0 z-10 w-2 cursor-col-resize",
        className,
      )}
      onPointerDown={(event) => {
        event.preventDefault();
        start.current = { x: event.clientX, width: widthRef.current };
        latest.current = widthRef.current;
        beginPanelDrag();
        try {
          (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        } catch {
          // jsdom 里没有 pointer capture：不影响逻辑，只是快速划过可能断。
        }
      }}
      onPointerMove={(event) => {
        const origin = start.current;
        if (!origin) return;
        // 把手在左边缘：往左拖（clientX 变小）面板变宽。
        const clamped = Math.min(
          boundsRef.current.max,
          Math.max(
            boundsRef.current.min,
            Math.round(origin.width - (event.clientX - origin.x)),
          ),
        );
        if (clamped !== latest.current) {
          latest.current = clamped;
          onResize(clamped);
        }
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onDoubleClick={() => {
        latest.current = widthRef.current;
        onReset();
      }}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-y-0 left-1/2 w-0.5 -translate-x-1/2 bg-transparent transition-colors group-hover:bg-primary/60 group-active:bg-primary/80"
      />
    </div>
  );
}
