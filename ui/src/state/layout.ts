//! 终端层的布局计算：把分屏树算成每个 pane 的矩形和每条分隔条的位置。
//!
//! 纯函数，和 React / xterm 都无关，单测直接调。终端层拿这个结果做绝对定位，
//! 这样分屏、关闭、拖比例都只改样式，不会重建 React 结构（也就不会重建 xterm）。

import type { PaneId, TreeNode } from "./splitTree";

export interface PaneRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface DividerRect {
  splitId: string;
  axis: "x" | "y";
  /** 分隔条自己的矩形（绝对定位用） */
  x: number;
  y: number;
  width: number;
  height: number;
  /** 拖动时算比例用的轨道：整个分割点的起点和长度 */
  track: { start: number; length: number };
  /** 轨道方向可用的长度（扣掉分隔条本身） */
  usable: number;
}

export interface LayerLayout {
  rects: Record<PaneId, PaneRect>;
  dividers: DividerRect[];
}

/** 分隔条厚度（px）：和以前的 h-1 / w-1 一致。 */
export const DIVIDER_SIZE = 4;

/// 每个 pane 的最小尺寸（UI-1 第 3 节）：约 20 列 × 5 行、按默认 14px JetBrains
/// Mono 的单元格估算（字宽约 8.4px、行高 17px）。拖分隔条不能把 pane 拖成 0。
export const MIN_PANE_WIDTH = 170;
export const MIN_PANE_HEIGHT = 85;

/** 比例夹取：轨道两边都要留出最小尺寸。 */
export function clampRatio(ratio: number, usable: number, axis: "x" | "y"): number {
  const minPixels = axis === "x" ? MIN_PANE_WIDTH : MIN_PANE_HEIGHT;
  // 轨道本来就不到两个最小尺寸时（很窄的窗口 / 套了好几层分屏）：贴在平分上，
  // 不让拖到极端（区间恒定合法）。
  const minRatio = Math.min(0.5, minPixels / Math.max(1, usable));
  return Math.min(1 - minRatio, Math.max(minRatio, ratio));
}

/** 算出一棵树在给定尺寸下的摆法。 */
export function computeLayout(tree: TreeNode, width: number, height: number): LayerLayout {
  const rects: Record<PaneId, PaneRect> = {};
  const dividers: DividerRect[] = [];

  const walk = (node: TreeNode, rect: PaneRect) => {
    if (node.kind === "pane") {
      rects[node.id] = rect;
      return;
    }
    const horizontal = node.axis === "x";
    const total = horizontal ? rect.width : rect.height;
    const usable = Math.max(0, total - DIVIDER_SIZE);
    const first = usable * node.ratio;
    const second = Math.max(0, usable - first);

    if (horizontal) {
      walk(node.first, { ...rect, width: first });
      dividers.push({
        splitId: node.id,
        axis: "x",
        x: rect.x + first,
        y: rect.y,
        width: DIVIDER_SIZE,
        height: rect.height,
        track: { start: rect.x, length: total },
        usable,
      });
      walk(node.second, {
        ...rect,
        x: rect.x + first + DIVIDER_SIZE,
        width: second,
      });
    } else {
      walk(node.first, { ...rect, height: first });
      dividers.push({
        splitId: node.id,
        axis: "y",
        x: rect.x,
        y: rect.y + first,
        width: rect.width,
        height: DIVIDER_SIZE,
        track: { start: rect.y, length: total },
        usable,
      });
      walk(node.second, {
        ...rect,
        y: rect.y + first + DIVIDER_SIZE,
        height: second,
      });
    }
  };

  walk(tree, { x: 0, y: 0, width, height });
  return { rects, dividers };
}

/** 拖动分隔条时，指针位置换算成比例（指针在分隔条中间，所以要补半个厚度）。 */
export function ratioFromPointer(divider: DividerRect, pointer: number): number {
  if (divider.usable <= 0) return 0.5;
  const raw = (pointer - divider.track.start - DIVIDER_SIZE / 2) / divider.usable;
  return clampRatio(raw, divider.usable, divider.axis);
}
