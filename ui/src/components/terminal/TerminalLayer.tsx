// Adapted from Netcatty components/TerminalLayer.tsx（思路：终端挂在稳定的层上）
//! 终端层：所有 pane 的 xterm 都挂在这一个层里，按分屏树算出来的矩形**绝对定位**。
//!
//! 为什么这么做：以前叶子跟着分屏树的 React 结构走，一分屏整棵子树被 React
//! 重建，xterm 跟着重建 → 回滚内容和 WebGL 渲染器都没了。现在 pane 的组件在
//! 一个平的列表里（key 是 paneId），分屏 / 关闭 / 拖比例只改 style，实例不动。

import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";

import { cn } from "../../lib/utils";
import { beginPanelDrag, endPanelDrag } from "../../state/panelWidths";
import {
  computeLayout,
  DIVIDER_SIZE,
  ratioFromPointer,
  type DividerRect,
} from "../../state/layout";
import { paneIds, type PaneId, type TreeNode } from "../../state/splitTree";
import type { PaneSession } from "../../state/sessions";

interface TerminalLayerProps {
  tree: TreeNode;
  panes: Record<string, PaneSession>;
  /** 每个 pane 的内容：测试里换成假的，生产里就是 PaneView 的那套 props */
  renderPane: (paneId: PaneId) => React.ReactNode;
  onRatio: (splitId: string, ratio: number) => void;
}

/** 分隔线的命中区域宽度（UI-1 0.2：6~8px，可见线仍是 DIVIDER_SIZE）。 */
const HIT_EXTRA = 8;

export default function TerminalLayer({ tree, panes, renderPane, onRatio }: TerminalLayerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const measure = () => {
      const rect = container.getBoundingClientRect();
      setSize((previous) =>
        previous.width === rect.width && previous.height === rect.height
          ? previous
          : { width: rect.width, height: rect.height },
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const layout = useMemo(
    () => computeLayout(tree, size.width, size.height),
    [tree, size.width, size.height],
  );

  // 顺序只在增删 pane 时变；key 是 paneId，所以 React 不会重建已有实例
  const order = useMemo(() => paneIds(tree), [tree]);

  const handleDividerDrag = useCallback(
    (divider: DividerRect) => (event: ReactPointerEvent<HTMLDivElement>) => {
      const handle = event.currentTarget;
      event.preventDefault();
      try {
        handle.setPointerCapture(event.pointerId);
      } catch {
        // jsdom 里没有 pointer capture：不影响逻辑，只是快速划过可能断。
      }
      // 拖动期间 pane 的矩形跟着变（视觉），但终端**只在松手后**重排并上报
      // （UI-1 第 3 节：拖的过程中不每帧发 resize）。
      beginPanelDrag();
      const horizontal = divider.axis === "x";
      const move = (moveEvent: PointerEvent) => {
        const pointer = horizontal ? moveEvent.clientX : moveEvent.clientY;
        // 拖的时候容器没动，用当时算出来的轨道就行
        const rect = containerRef.current?.getBoundingClientRect();
        const base = horizontal ? (rect?.left ?? 0) : (rect?.top ?? 0);
        onRatio(divider.splitId, ratioFromPointer(divider, pointer - base));
      };
      const finish = (upEvent: PointerEvent) => {
        try {
          handle.releasePointerCapture(upEvent.pointerId);
        } catch {
          // jsdom 里没有 pointer capture。
        }
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", finish);
        handle.removeEventListener("pointercancel", finish);
        // 松手：终端收到通知后按最终尺寸重排一次并上报 resize。
        endPanelDrag();
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", finish);
      handle.addEventListener("pointercancel", finish);
    },
    [onRatio],
  );

  return (
    <div ref={containerRef} className="relative h-full w-full overflow-hidden">
      {order.map((paneId) => {
        const rect = layout.rects[paneId];
        if (!rect || !panes[paneId]) return null;
        return (
          <div
            key={paneId}
            className="absolute min-h-0 min-w-0"
            style={{ left: rect.x, top: rect.y, width: rect.width, height: rect.height }}
          >
            {renderPane(paneId)}
          </div>
        );
      })}

      {layout.dividers.map((divider) => {
        // 命中区域比可见的分隔线宽（UI-1 0.2）：每边多 2px，合计 8px，
        // 可见线还是 DIVIDER_SIZE；多出来的部分透明、叠在 pane 边缘上。
        const grow = HIT_EXTRA - DIVIDER_SIZE;
        const horizontal = divider.axis === "x";
        return (
          <div
            key={divider.splitId}
            data-section="split-divider"
            onPointerDown={handleDividerDrag(divider)}
            // 双击恢复平分（UI-1 第 3 节）。
            onDoubleClick={() => onRatio(divider.splitId, 0.5)}
            style={{
              left: horizontal ? divider.x - grow / 2 : divider.x,
              top: horizontal ? divider.y : divider.y - grow / 2,
              width: horizontal ? divider.width + grow : divider.width,
              height: horizontal ? divider.height : divider.height + grow,
            }}
            className={cn(
              "group absolute z-10",
              horizontal ? "cursor-col-resize" : "cursor-row-resize",
            )}
          >
            <span
              aria-hidden
              className={cn(
                "pointer-events-none absolute bg-border/70 transition-colors group-hover:bg-primary/70 group-active:bg-primary",
                horizontal
                  ? "inset-y-0 left-1/2 w-1 -translate-x-1/2"
                  : "inset-x-0 top-1/2 h-1 -translate-y-1/2",
              )}
            />
          </div>
        );
      })}
    </div>
  );
}
