//! 快捷命令栏上边框的拖动条：上下拖改栏高，双击恢复默认一行。
//!
//! 和 AI 输入框、面板宽度的把手同一套做法：拖动中只改视觉高度，松手才写回设置表；
//! 拖动期间用 `beginPanelDrag` 告诉终端"只做视觉 fit、先别向服务器上报尺寸"，
//! 松手后终端按最终尺寸重排并上报一次（不会每个像素都改后端 PTY 尺寸）。

import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

import { beginPanelDrag, endPanelDrag } from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import {
  QUICK_BAR_DEFAULT_HEIGHT,
  clampQuickBarHeight,
  dragQuickBarHeight,
  sanitizeStoredQuickBarHeight,
  setLiveQuickBarHeight,
} from "./quickBarHeight";

/** 终端区域（命令栏所在的标签页容器）的高度；量不到时退到窗口高度。 */
function useAreaHeight(barRef: RefObject<HTMLElement | null>): number {
  const [height, setHeight] = useState(() =>
    typeof window === "undefined" ? 800 : window.innerHeight,
  );
  useEffect(() => {
    const bar = barRef.current;
    const area = bar?.closest<HTMLElement>('[data-section="terminal-area"]');
    if (!area) return;
    const measure = () => {
      const h = area.getBoundingClientRect().height;
      setHeight(h > 0 ? h : window.innerHeight);
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(area);
    return () => observer.disconnect();
  }, [barRef]);
  return height;
}

/** 命令栏高度：设置表里读（非法值回默认）、按终端区域夹取、拖动时本地覆盖。 */
export function useQuickBarHeight(barRef: RefObject<HTMLElement | null>) {
  const stored = useSettings((state) => state["layout.quick_bar_height"]);
  const update = useSettings((state) => state.update);
  const areaHeight = useAreaHeight(barRef);
  const [dragHeight, setDragHeight] = useState<number | null>(null);
  const height =
    dragHeight ?? clampQuickBarHeight(sanitizeStoredQuickBarHeight(stored), areaHeight);

  // 把实际高度（含拖动中的）告诉 App，服务器概览面板的底部偏移跟着走。
  useEffect(() => {
    setLiveQuickBarHeight(height);
  }, [height]);
  useEffect(() => () => setLiveQuickBarHeight(QUICK_BAR_DEFAULT_HEIGHT), []);

  const onCommit = useCallback(
    (next: number) => {
      setDragHeight(null);
      void update("layout.quick_bar_height", next);
    },
    [update],
  );
  const onReset = useCallback(() => {
    setDragHeight(null);
    void update("layout.quick_bar_height", QUICK_BAR_DEFAULT_HEIGHT);
  }, [update]);
  return { height, areaHeight, onResize: setDragHeight, onCommit, onReset };
}

export function QuickBarResizeHandle({
  height,
  areaHeight,
  onResize,
  onCommit,
  onReset,
}: {
  height: number;
  areaHeight: number;
  onResize: (height: number) => void;
  onCommit: (height: number) => void;
  onReset: () => void;
}) {
  const start = useRef<{ y: number; height: number } | null>(null);
  const latest = useRef(height);
  const heightRef = useRef(height);
  const areaRef = useRef(areaHeight);
  heightRef.current = height;
  areaRef.current = areaHeight;

  const finish = useCallback(() => {
    if (!start.current) return;
    start.current = null;
    endPanelDrag();
    onCommit(latest.current);
  }, [onCommit]);

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      title="拖动调整命令栏高度（双击恢复默认）"
      data-section="quick-bar-resize-handle"
      className="group absolute inset-x-0 top-0 z-10 h-2 -translate-y-1/2 cursor-ns-resize"
      onPointerDown={(event) => {
        event.preventDefault();
        start.current = { y: event.clientY, height: heightRef.current };
        latest.current = heightRef.current;
        beginPanelDrag();
        try {
          (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        } catch {
          // jsdom 里没有 pointer capture：不影响逻辑。
        }
      }}
      onPointerMove={(event) => {
        const origin = start.current;
        if (!origin) return;
        const next = dragQuickBarHeight(origin.height, origin.y, event.clientY, areaRef.current);
        if (next !== latest.current) {
          latest.current = next;
          onResize(next);
        }
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onDoubleClick={() => {
        latest.current = QUICK_BAR_DEFAULT_HEIGHT;
        onReset();
      }}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2 bg-transparent transition-colors group-hover:bg-primary/60 group-active:bg-primary/80"
      />
    </div>
  );
}
