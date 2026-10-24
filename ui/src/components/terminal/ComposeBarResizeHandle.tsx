//! 撰写栏上沿的拖动条：上下拖改栏高，双击恢复自动增高。
//!
//! 和快捷命令栏、AI 输入框的把手同一套做法（pointer capture；拖动中只改视觉高度、松手才写设置；
//! 拖动期间 `beginPanelDrag` 让终端只做视觉 fit、松手后再上报一次尺寸）。不同的是：
//! - 设置值 0 = 自动增高（默认），大于 0 = 固定高度；
//! - 自动模式下开始拖动时，以栏当前的实际高度为起点；只点一下（没拖动）不会把自动变成固定。
//! 栏的实际高度仍由撰写栏自己通过 `setLiveComposeBarHeight` 上报。

import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

import { beginPanelDrag, endPanelDrag } from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import {
  COMPOSE_BAR_AUTO,
  COMPOSE_BAR_MIN_HEIGHT,
  clampComposeBarHeight,
  dragComposeBarHeight,
  sanitizeStoredComposeBarHeight,
} from "./quickBarHeight";
import { useAreaHeight } from "./QuickBarResizeHandle";

/**
 * 撰写栏的固定高度：设置表里读（非法值当 0 = 自动）、按终端区域夹取（只影响显示，不改存的值）、
 * 拖动时本地覆盖。`fixed` 为 null = 自动增高。
 */
export function useComposeBarHeight(barRef: RefObject<HTMLElement | null>) {
  const stored = useSettings((state) => state["layout.compose_bar_height"]);
  const update = useSettings((state) => state.update);
  const areaHeight = useAreaHeight(barRef);
  const [dragHeight, setDragHeight] = useState<number | null>(null);
  const storedHeight = sanitizeStoredComposeBarHeight(stored);
  const fixed =
    dragHeight ??
    (storedHeight > COMPOSE_BAR_AUTO ? clampComposeBarHeight(storedHeight, areaHeight) : null);

  /** 开始拖动时的起点：固定高度就是它自己，自动模式量栏现在的实际高度。 */
  const fixedRef = useRef(fixed);
  fixedRef.current = fixed;
  const getHeight = useCallback(
    () =>
      fixedRef.current ??
      Math.max(
        COMPOSE_BAR_MIN_HEIGHT,
        Math.round(barRef.current?.getBoundingClientRect().height ?? 0),
      ),
    [barRef],
  );

  const onCommit = useCallback(
    (next: number) => {
      setDragHeight(null);
      void update("layout.compose_bar_height", next);
    },
    [update],
  );
  const onReset = useCallback(() => {
    setDragHeight(null);
    void update("layout.compose_bar_height", COMPOSE_BAR_AUTO);
  }, [update]);
  return { fixed, areaHeight, getHeight, onResize: setDragHeight, onCommit, onReset };
}

export function ComposeBarResizeHandle({
  getHeight,
  areaHeight,
  onResize,
  onCommit,
  onReset,
}: {
  getHeight: () => number;
  areaHeight: number;
  onResize: (height: number) => void;
  onCommit: (height: number) => void;
  onReset: () => void;
}) {
  const start = useRef<{ y: number; height: number; moved: boolean } | null>(null);
  const latest = useRef(0);
  const areaRef = useRef(areaHeight);
  areaRef.current = areaHeight;

  // 拖到一半组件被卸载（关掉撰写栏、切标签）：别把终端留在"拖动中"。
  useEffect(
    () => () => {
      if (start.current) {
        start.current = null;
        endPanelDrag();
      }
    },
    [],
  );

  const finish = useCallback(() => {
    const origin = start.current;
    if (!origin) return;
    start.current = null;
    endPanelDrag();
    // 没拖动就松手（比如双击的第一下）：不写设置，自动模式保持自动
    if (origin.moved) onCommit(latest.current);
  }, [onCommit]);

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      title="上下拖动调整高度，双击恢复"
      data-section="compose-bar-resize-handle"
      className="group absolute inset-x-0 top-0 z-10 h-2 -translate-y-1/2 cursor-ns-resize"
      onPointerDown={(event) => {
        event.preventDefault();
        const height = getHeight();
        start.current = { y: event.clientY, height, moved: false };
        latest.current = height;
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
        const next = dragComposeBarHeight(origin.height, origin.y, event.clientY, areaRef.current);
        if (next !== latest.current) {
          latest.current = next;
          origin.moved = true;
          onResize(next);
        }
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onDoubleClick={onReset}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2 bg-transparent transition-colors group-hover:bg-primary/60 group-active:bg-primary/80"
      />
    </div>
  );
}
