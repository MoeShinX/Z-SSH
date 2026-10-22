//! 主机树的虚线连接线（像 Xshell 的会话树）：从父分组往下一条竖线，再连一小段横线到每个子项；
//! 最后一个子项只画到它这一行中间（└ 形）。
//!
//! 树是一排扁平的行，不为连接线多挂 DOM：每一行用自己的 `background-image`（几层 1px 的
//! 虚线渐变）画出属于它的那几段——祖先里还有后续兄弟的各层竖线、自己这一层的竖线（├ 或 └）
//! 和横线。行高、缩进是固定的（`h-7`、每层 14px），所以位置都能算出来；颜色用现有的
//! `--muted-foreground` 主题令牌加透明度，浅色、深色皮肤下都是淡淡的但看得见。

import type { CSSProperties } from "react";

/** 树每层缩进（和 `paddingLeft: 4 + depth * 14` 一致）。 */
export const TREE_INDENT = 14;
/** 一行的高度（`h-7`）。 */
export const TREE_ROW_HEIGHT = 28;
/** 某一层分组的展开箭头中心离行左边的距离：行内边距 4 + 箭头框（16px）的一半。 */
const TOGGLE_CENTER = 12;
/** 横线的终点：连到子项图标前（子分组到它的展开箭头，主机到它的图标）。 */
const STUB_END_GROUP = 4;
const STUB_END_HOST = 18;

/** 连接线颜色：低对比度的现有主题令牌。 */
export const TREE_LINE_COLOR = "hsl(var(--muted-foreground) / 0.4)";

/** 一行在树里的位置信息。顶层节点（深度 0）没有连接线，不需要这个。 */
export interface TreeGuide {
  /**
   * 祖先各层的竖线要不要在这一行继续画（下标 = 层深，0 到 depth-2）：
   * 那一层的祖先后面还有兄弟，竖线就得穿过这一行。
   */
  trail: boolean[];
  /** 自己是不是父分组里的最后一个子项（└，竖线只画到这一行中间）。 */
  last: boolean;
  /** 子项是主机还是子分组（横线终点不一样）。 */
  kind: "group" | "host";
}

/** 第 `level` 层分组竖线离行左边的距离。 */
export function guideX(level: number): number {
  return TOGGLE_CENTER + level * TREE_INDENT;
}

const DASH_DOWN = `repeating-linear-gradient(to bottom, ${TREE_LINE_COLOR} 0 2px, transparent 2px 4px)`;
const DASH_RIGHT = `repeating-linear-gradient(to right, ${TREE_LINE_COLOR} 0 2px, transparent 2px 4px)`;

/**
 * 某一行的连接线样式。`depth` 是这一行自己的深度（≥ 1）。
 * 返回的是几层背景（竖线们 + 自己的竖线 + 横线）；背景色（悬停 / 选中）不受影响。
 */
export function treeGuideStyle(depth: number, guide: TreeGuide): CSSProperties {
  const images: string[] = [];
  const sizes: string[] = [];
  const positions: string[] = [];

  // 祖先各层还要继续往下的竖线：整行高
  guide.trail.forEach((continues, level) => {
    if (!continues) return;
    images.push(DASH_DOWN);
    sizes.push(`1px 100%`);
    positions.push(`${guideX(level)}px 0`);
  });

  // 自己这一层的竖线：├ 整行高，└ 只到中间
  const x = guideX(depth - 1);
  const mid = TREE_ROW_HEIGHT / 2;
  images.push(DASH_DOWN);
  sizes.push(guide.last ? `1px ${mid}px` : `1px 100%`);
  positions.push(`${x}px 0`);

  // 横线：从竖线连到子项前面
  const end = depth * TREE_INDENT + (guide.kind === "host" ? STUB_END_HOST : STUB_END_GROUP);
  images.push(DASH_RIGHT);
  sizes.push(`${Math.max(0, end - x)}px 1px`);
  positions.push(`${x}px ${mid - 1}px`);

  return {
    backgroundImage: images.join(", "),
    backgroundSize: sizes.join(", "),
    backgroundPosition: positions.join(", "),
    backgroundRepeat: "no-repeat",
  };
}

/** 给测试和调试看的标记：这一行的连接线形状（├ 或 └）和继续穿过的祖先层。 */
export function guideAttrs(guide: TreeGuide | undefined): {
  "data-tree-line"?: "branch" | "last";
  "data-tree-trail"?: string;
} {
  if (!guide) return {};
  return {
    "data-tree-line": guide.last ? "last" : "branch",
    "data-tree-trail": guide.trail.map((c) => (c ? "1" : "0")).join(""),
  };
}
