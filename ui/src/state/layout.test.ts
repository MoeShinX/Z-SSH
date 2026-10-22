//! 终端层的布局计算：纯函数，单测直接调。

import { describe, expect, it } from "vitest";

import {
  clampRatio,
  computeLayout,
  DIVIDER_SIZE,
  MIN_PANE_HEIGHT,
  MIN_PANE_WIDTH,
  ratioFromPointer,
} from "./layout";
import { splitPane, type TreeNode } from "./splitTree";

const single = (id: string): TreeNode => ({ kind: "pane", id });

describe("computeLayout", () => {
  it("单个 pane 占满整个区域", () => {
    const layout = computeLayout(single("A"), 800, 600);
    expect(layout.rects.A).toEqual({ x: 0, y: 0, width: 800, height: 600 });
    expect(layout.dividers).toHaveLength(0);
  });

  it("左右分屏：两半各占 (总宽 - 分隔条) 的一半，分隔条在中间", () => {
    const tree = splitPane(single("A"), "A", "B", "s1", "x");
    const layout = computeLayout(tree, 800, 600);

    const half = (800 - DIVIDER_SIZE) / 2;
    expect(layout.rects.A).toEqual({ x: 0, y: 0, width: half, height: 600 });
    expect(layout.rects.B).toEqual({
      x: half + DIVIDER_SIZE,
      y: 0,
      width: half,
      height: 600,
    });
    expect(layout.dividers).toHaveLength(1);
    expect(layout.dividers[0]).toMatchObject({ axis: "x", x: half, width: DIVIDER_SIZE });
  });

  it("上下分屏同理", () => {
    const tree = splitPane(single("A"), "A", "B", "s1", "y");
    const layout = computeLayout(tree, 800, 600);
    const half = (600 - DIVIDER_SIZE) / 2;
    expect(layout.rects.A.height).toBe(half);
    expect(layout.rects.B.y).toBe(half + DIVIDER_SIZE);
    expect(layout.dividers[0]).toMatchObject({ axis: "y", y: half, height: DIVIDER_SIZE });
  });

  it("嵌套：里面的分割点只在自己那块区域里分", () => {
    const two = splitPane(single("A"), "A", "B", "s1", "x");
    const tree = splitPane(two, "B", "C", "s2", "y");
    const layout = computeLayout(tree, 800, 400);

    const halfW = (800 - DIVIDER_SIZE) / 2;
    expect(layout.rects.A).toEqual({ x: 0, y: 0, width: halfW, height: 400 });
    // B / C 挤在右半边里，上下分
    const halfH = (400 - DIVIDER_SIZE) / 2;
    expect(layout.rects.B).toMatchObject({ x: halfW + DIVIDER_SIZE, y: 0, height: halfH });
    expect(layout.rects.C).toMatchObject({
      x: halfW + DIVIDER_SIZE,
      y: halfH + DIVIDER_SIZE,
      height: halfH,
    });
  });

  it("比例变了，两边的矩形跟着变", () => {
    const tree = splitPane(single("A"), "A", "B", "s1", "x");
    if (tree.kind !== "split") throw new Error("应该是分割点");
    const wide = computeLayout({ ...tree, ratio: 0.8 }, 1000, 400);
    expect(wide.rects.A.width).toBeCloseTo((1000 - DIVIDER_SIZE) * 0.8);
    expect(wide.rects.B.width).toBeCloseTo((1000 - DIVIDER_SIZE) * 0.2);
  });

  it("尺寸是 0（还没量出来）也不炸，先给 0 大小的矩形", () => {
    const tree = splitPane(single("A"), "A", "B", "s1", "x");
    const layout = computeLayout(tree, 0, 0);
    expect(layout.rects.A.width).toBe(0);
    expect(layout.rects.B.width).toBe(0);
    expect(layout.dividers[0].usable).toBe(0);
  });
});

describe("ratioFromPointer", () => {
  it("指针在区块正中间时是 0.5", () => {
    const tree = splitPane(single("A"), "A", "B", "s1", "x");
    const layout = computeLayout(tree, 800, 600);
    const divider = layout.dividers[0];
    const pointer = DIVIDER_SIZE / 2 + divider.track.start + divider.usable / 2;
    expect(ratioFromPointer(divider, pointer)).toBeCloseTo(0.5);
  });

  it("轨道长度是 0 时给 0.5，不除零", () => {
    const layout = computeLayout(splitPane(single("A"), "A", "B", "s1", "x"), 0, 0);
    expect(ratioFromPointer(layout.dividers[0], 100)).toBe(0.5);
  });

  it("两边都要留出 pane 的最小尺寸（UI-1 第 3 节）", () => {
    const divider = computeLayout(splitPane(single("A"), "A", "B", "s1", "x"), 1000, 600)
      .dividers[0];
    // 拖到最左边：比例停在 最小宽/轨道 上，不能到 0。
    const left = ratioFromPointer(divider, 0);
    expect(left).toBeCloseTo(MIN_PANE_WIDTH / divider.usable, 5);
    // 拖到最右边：右边也要留最小宽。
    const right = ratioFromPointer(divider, 1000);
    expect(right).toBeCloseTo(1 - MIN_PANE_WIDTH / divider.usable, 5);
    // 上下方向用最小高。
    const vertical = computeLayout(splitPane(single("A"), "A", "B", "s1", "y"), 800, 700)
      .dividers[0];
    expect(ratioFromPointer(vertical, 0)).toBeCloseTo(MIN_PANE_HEIGHT / vertical.usable, 5);
  });
});

describe("clampRatio", () => {
  it("常规轨道夹在两边最小尺寸之间", () => {
    expect(clampRatio(0.01, 1000, "x")).toBeCloseTo(MIN_PANE_WIDTH / 1000, 5);
    expect(clampRatio(0.99, 1000, "x")).toBeCloseTo(1 - MIN_PANE_WIDTH / 1000, 5);
    expect(clampRatio(0.5, 1000, "x")).toBe(0.5);
    expect(clampRatio(-3, 1000, "y")).toBeCloseTo(MIN_PANE_HEIGHT / 1000, 5);
  });

  it("轨道太小（放不下两个最小尺寸）时贴在平分上，区间永远合法", () => {
    expect(clampRatio(0.1, 200, "x")).toBe(0.5);
    expect(clampRatio(0.9, 200, "x")).toBe(0.5);
    expect(clampRatio(0.5, 0, "x")).toBe(0.5);
  });
});
