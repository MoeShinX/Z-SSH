//! 快捷命令栏高度：默认和最小是一行，最大 5 行与终端区域 40% 取小。

import { describe, expect, it } from "vitest";

import {
  QUICK_BAR_DEFAULT_HEIGHT,
  clampQuickBarHeight,
  dragQuickBarHeight,
  overviewBottomOffset,
  quickBarMaxHeight,
  sanitizeStoredQuickBarHeight,
} from "./quickBarHeight";

describe("命令栏高度的夹取", () => {
  it("默认高度就是原来的一行（h-7 = 28px）", () => {
    expect(QUICK_BAR_DEFAULT_HEIGHT).toBe(28);
  });

  it("上限取 5 行和区域 40% 里较小的，至少是默认高度", () => {
    // 5 行 = 28 + 4 * 24 = 124
    expect(quickBarMaxHeight(2000)).toBe(124);
    expect(quickBarMaxHeight(500)).toBe(124);
    expect(quickBarMaxHeight(300)).toBe(120);
    expect(quickBarMaxHeight(100)).toBe(40);
    expect(quickBarMaxHeight(40)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(quickBarMaxHeight(0)).toBe(124);
    expect(quickBarMaxHeight(Number.NaN)).toBe(124);
  });

  it("夹进 [默认, 上限]，非法值回默认", () => {
    expect(clampQuickBarHeight(10, 1000)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(clampQuickBarHeight(76.4, 1000)).toBe(76);
    expect(clampQuickBarHeight(999, 1000)).toBe(124);
    expect(clampQuickBarHeight(999, 200)).toBe(80);
    expect(clampQuickBarHeight(Number.NaN, 1000)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
  });

  it("往上拖变高、往下拖变矮，不会比默认更矮", () => {
    expect(dragQuickBarHeight(28, 500, 452, 1000)).toBe(76);
    expect(dragQuickBarHeight(76, 500, 540, 1000)).toBe(36);
    expect(dragQuickBarHeight(28, 500, 600, 1000)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(dragQuickBarHeight(28, 500, -5000, 1000)).toBe(124);
  });

  it("读回来的存量值做体检", () => {
    expect(sanitizeStoredQuickBarHeight(76)).toBe(76);
    expect(sanitizeStoredQuickBarHeight(10)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(sanitizeStoredQuickBarHeight(-3)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(sanitizeStoredQuickBarHeight(1e9)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(sanitizeStoredQuickBarHeight("100")).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    expect(sanitizeStoredQuickBarHeight(null)).toBe(QUICK_BAR_DEFAULT_HEIGHT);
  });
});

describe("概览面板的底部偏移", () => {
  it("命令栏关着只让出状态栏；开着再加上命令栏的实际高度", () => {
    expect(overviewBottomOffset(false, 100)).toBe(24);
    // 默认一行时和原来写死的 52 一致
    expect(overviewBottomOffset(true, QUICK_BAR_DEFAULT_HEIGHT)).toBe(52);
    expect(overviewBottomOffset(true, 100)).toBe(124);
  });
});
