//! 分屏树：标签里每个终端是一个叶子，分割点是二叉节点（设计文档第 8 节）。
//!
//! 纯函数，不依赖 React 和 xterm，单测直接调。

export type PaneId = string;

export interface PaneNode {
  kind: "pane";
  id: PaneId;
}

export interface SplitNode {
  kind: "split";
  id: string;
  /** x = 左右排（两个子节点一左一右），y = 上下排 */
  axis: "x" | "y";
  /** 第一个子节点占的比例，夹在 [MIN_RATIO, MAX_RATIO] */
  ratio: number;
  first: TreeNode;
  second: TreeNode;
}

export type TreeNode = PaneNode | SplitNode;

/** 拖动分隔条的比例下限 / 上限：不允许把某一侧拖没。 */
export const MIN_RATIO = 0.1;
export const MAX_RATIO = 0.9;

export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
}

/** 树上所有叶子，按视觉顺序（左到右、上到下）。 */
export function paneIds(node: TreeNode): PaneId[] {
  if (node.kind === "pane") return [node.id];
  return [...paneIds(node.first), ...paneIds(node.second)];
}

/** 第一个叶子（默认焦点）。 */
export function firstPane(node: TreeNode): PaneId {
  return node.kind === "pane" ? node.id : firstPane(node.first);
}

export function hasPane(node: TreeNode, paneId: PaneId): boolean {
  if (node.kind === "pane") return node.id === paneId;
  return hasPane(node.first, paneId) || hasPane(node.second, paneId);
}

/** 把 paneId 一分为二：原来的留在前半，新 pane 在后半（右 / 下）。 */
export function splitPane(
  node: TreeNode,
  paneId: PaneId,
  newPaneId: PaneId,
  splitId: string,
  axis: "x" | "y",
): TreeNode {
  if (node.kind === "pane") {
    if (node.id !== paneId) return node;
    return {
      kind: "split",
      id: splitId,
      axis,
      ratio: 0.5,
      first: node,
      second: { kind: "pane", id: newPaneId },
    };
  }
  const first = splitPane(node.first, paneId, newPaneId, splitId, axis);
  const second = splitPane(node.second, paneId, newPaneId, splitId, axis);
  if (first === node.first && second === node.second) return node;
  return { ...node, first, second };
}

/**
 * 关掉一个 pane：它所在的分割点被兄弟节点顶掉。
 *
 * 返回 `null` 表示这是最后一个 pane，整个标签该关了。
 */
export function closePane(node: TreeNode, paneId: PaneId): TreeNode | null {
  if (node.kind === "pane") {
    return node.id === paneId ? null : node;
  }
  const first = closePane(node.first, paneId);
  const second = closePane(node.second, paneId);
  if (first === node.first && second === node.second) return node; // 不在这支里
  if (first === null) return second;
  if (second === null) return first;
  return { ...node, first, second };
}

/** 和 paneId 相邻（沿树往上最近的另一半）的第一个叶子。 */
export function siblingFirstPane(node: TreeNode, paneId: PaneId): PaneId | null {
  if (node.kind === "pane") return null;
  const sibling = siblingOf(node, paneId);
  return sibling === null ? null : firstPane(sibling);
}

function siblingOf(node: SplitNode, paneId: PaneId): TreeNode | null {
  if (hasPane(node.first, paneId)) {
    if (node.first.kind === "pane") return node.second;
    return siblingOf(node.first, paneId) ?? node.second;
  }
  if (hasPane(node.second, paneId)) {
    if (node.second.kind === "pane") return node.first;
    return siblingOf(node.second, paneId) ?? node.first;
  }
  return null;
}

/**
 * 关掉一个 pane，并给出新的焦点：
 *
 * - 关的不是焦点 pane：焦点不动
 * - 关的是焦点 pane：落到相邻 pane（最近的另一半）的第一个叶子
 * - 关的是最后一个 pane：树变成 `null`，焦点也没了
 */
export function closePaneWithFocus(
  node: TreeNode,
  paneId: PaneId,
  focusedId: PaneId,
): { tree: TreeNode | null; focus: PaneId | null } {
  const sibling = siblingFirstPane(node, paneId);
  const tree = closePane(node, paneId);
  if (tree === null) return { tree: null, focus: null };
  if (paneId !== focusedId) return { tree, focus: focusedId };
  if (sibling !== null && hasPane(tree, sibling)) return { tree, focus: sibling };
  return { tree, focus: firstPane(tree) };
}

/** 调整某个分割点的比例（自动夹到 [MIN_RATIO, MAX_RATIO]）。 */
export function setRatio(node: TreeNode, splitId: string, ratio: number): TreeNode {
  if (node.kind === "pane") return node;
  if (node.id === splitId) return { ...node, ratio: clampRatio(ratio) };
  const first = setRatio(node.first, splitId, ratio);
  const second = setRatio(node.second, splitId, ratio);
  if (first === node.first && second === node.second) return node;
  return { ...node, first, second };
}
