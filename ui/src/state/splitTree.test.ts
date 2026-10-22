//! 分屏树 reducer 的单测（`npm test` 跑的就是这些）。

import { describe, expect, it } from "vitest";

import {
  clampRatio,
  closePane,
  closePaneWithFocus,
  firstPane,
  hasPane,
  MAX_RATIO,
  MIN_RATIO,
  paneIds,
  setRatio,
  siblingFirstPane,
  splitPane,
  type TreeNode,
} from "./splitTree";

/** 建一棵树：split(A, split(B, C))，x 轴。 */
function threePanes(): TreeNode {
  const two = splitPane({ kind: "pane", id: "A" }, "A", "B", "s1", "x");
  return splitPane(two, "B", "C", "s2", "y");
}

describe("splitPane", () => {
  it("把一个 pane 分成两个：原来的在前，新 pane 在后", () => {
    const tree = splitPane({ kind: "pane", id: "A" }, "A", "B", "s1", "x");
    expect(tree.kind).toBe("split");
    if (tree.kind !== "split") return;
    expect(tree.axis).toBe("x");
    expect(tree.ratio).toBe(0.5);
    expect(tree.first).toEqual({ kind: "pane", id: "A" });
    expect(tree.second).toEqual({ kind: "pane", id: "B" });
    expect(paneIds(tree)).toEqual(["A", "B"]);
  });

  it("分割已经嵌套的 pane，其他分支不动", () => {
    const tree = threePanes();
    const after = splitPane(tree, "B", "D", "s3", "x");
    expect(paneIds(after)).toEqual(["A", "B", "D", "C"]);
    // A 还是老样子
    if (after.kind !== "split") throw new Error("根还是分割点");
    expect(after.first).toEqual({ kind: "pane", id: "A" });
  });

  it("分割不存在的 pane 不改树", () => {
    const tree = threePanes();
    expect(splitPane(tree, "X", "D", "s3", "x")).toBe(tree);
  });
});

describe("closePane", () => {
  it("关掉最后一个 pane 返回 null（标签要关）", () => {
    expect(closePane({ kind: "pane", id: "A" }, "A")).toBeNull();
  });

  it("关掉中间的 pane：兄弟顶上来", () => {
    const tree = threePanes(); // A | (B / C)
    const after = closePane(tree, "B");
    expect(after).not.toBeNull();
    expect(paneIds(after as TreeNode)).toEqual(["A", "C"]);
  });

  it("关掉右边内层的 pane", () => {
    const tree = threePanes();
    const after = closePane(tree, "C");
    expect(paneIds(after as TreeNode)).toEqual(["A", "B"]);
  });

  it("关掉外层左边的 pane，右边整棵子树留下", () => {
    const tree = threePanes();
    const after = closePane(tree, "A");
    expect(paneIds(after as TreeNode)).toEqual(["B", "C"]);
    expect(after?.kind).toBe("split");
  });

  it("关掉不存在的 pane 不改树", () => {
    const tree = threePanes();
    expect(closePane(tree, "X")).toBe(tree);
  });

  it("一层层关完", () => {
    let tree: TreeNode | null = threePanes();
    tree = closePane(tree as TreeNode, "A");
    tree = closePane(tree as TreeNode, "B");
    tree = closePane(tree as TreeNode, "C");
    expect(tree).toBeNull();
  });
});

describe("closePaneWithFocus", () => {
  it("关的不是焦点 pane：焦点不动", () => {
    const tree = threePanes();
    const { tree: after, focus } = closePaneWithFocus(tree, "A", "C");
    expect(focus).toBe("C");
    expect(paneIds(after as TreeNode)).toEqual(["B", "C"]);
  });

  it("关的是焦点 pane：焦点落到相邻（最近的另一半）的第一个叶子", () => {
    const tree = threePanes(); // A | (B / C)
    // B 和 C 相邻，关 B 应该聚焦 C
    const { focus, tree: after } = closePaneWithFocus(tree, "B", "B");
    expect(focus).toBe("C");
    expect(paneIds(after as TreeNode)).toEqual(["A", "C"]);
  });

  it("关掉焦点 pane 之后没有相邻的：取整棵树的第一个叶子", () => {
    const tree = splitPane({ kind: "pane", id: "A" }, "A", "B", "s1", "x");
    const { focus } = closePaneWithFocus(tree, "B", "B");
    expect(focus).toBe("A");
  });

  it("关掉最后一个 pane：树和焦点都没了", () => {
    const { tree, focus } = closePaneWithFocus({ kind: "pane", id: "A" }, "A", "A");
    expect(tree).toBeNull();
    expect(focus).toBeNull();
  });
});

describe("siblingFirstPane", () => {
  it("相邻的是最近的另一半", () => {
    const tree = threePanes(); // A | (B / C)
    expect(siblingFirstPane(tree, "B")).toBe("C");
    expect(siblingFirstPane(tree, "C")).toBe("B");
    // A 的另一半是整棵右子树，取它的第一个叶子
    expect(siblingFirstPane(tree, "A")).toBe("B");
  });

  it("只剩一个 pane 时没有相邻的", () => {
    expect(siblingFirstPane({ kind: "pane", id: "A" }, "A")).toBeNull();
  });
});

describe("setRatio / clampRatio", () => {
  it("比例夹在上下限之间", () => {
    expect(clampRatio(0.5)).toBe(0.5);
    expect(clampRatio(0)).toBe(MIN_RATIO);
    expect(clampRatio(1)).toBe(MAX_RATIO);
    expect(clampRatio(-3)).toBe(MIN_RATIO);
    expect(clampRatio(Number.NaN)).toBe(0.5);
  });

  it("设置外层分割点的比例", () => {
    const tree = threePanes();
    const after = setRatio(tree, "s1", 0.7);
    if (after.kind !== "split") throw new Error("根还是分割点");
    expect(after.ratio).toBeCloseTo(0.7);
    // 内层不动
    if (after.second.kind !== "split") throw new Error("内层还在");
    expect(after.second.ratio).toBe(0.5);
  });

  it("设置内层分割点的比例并夹住越界值", () => {
    const tree = threePanes();
    const after = setRatio(tree, "s2", 9);
    if (after.kind !== "split") throw new Error("根还是分割点");
    expect(after.ratio).toBe(0.5);
    if (after.second.kind !== "split") throw new Error("内层还在");
    expect(after.second.ratio).toBe(MAX_RATIO);
  });

  it("不存在的分割点不改树", () => {
    const tree = threePanes();
    expect(setRatio(tree, "nope", 0.3)).toBe(tree);
  });
});

describe("基础工具", () => {
  it("paneIds 按视觉顺序，firstPane / hasPane 正确", () => {
    const tree = threePanes();
    expect(paneIds(tree)).toEqual(["A", "B", "C"]);
    expect(firstPane(tree)).toBe("A");
    expect(hasPane(tree, "C")).toBe(true);
    expect(hasPane(tree, "X")).toBe(false);
  });
});
