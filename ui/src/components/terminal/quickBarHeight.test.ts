//! 快捷命令栏高度：默认和最小是一行，最大 5 行与终端区域 40% 取小。

import { describe, expect, it } from "vitest";

import {
  COMPOSE_BAR_AUTO,
  COMPOSE_BAR_CHROME,
  COMPOSE_BAR_MIN_HEIGHT,
  QUICK_BAR_DEFAULT_HEIGHT,
  COMPOSE_INPUT_MIN_HEIGHT,
  clampComposeBarHeight,
  clampQuickBarHeight,
  composeBarMaxHeight,
  composeInputMaxHeight,
  dragComposeBarHeight,
  dragQuickBarHeight,
  overviewBottomOffset,
  quickBarMaxHeight,
  sanitizeStoredComposeBarHeight,
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

describe("概览面板的底部偏移（撰写栏）", () => {
  it("撰写栏开着再让出它的实际高度；不传就是 0", () => {
    expect(overviewBottomOffset(false, 100, 0)).toBe(24);
    expect(overviewBottomOffset(true, 28, 29)).toBe(81);
    expect(overviewBottomOffset(false, 28, 60)).toBe(84);
  });
});

describe("撰写栏输入框高度", () => {
  it("一行 = 18 + 内边距和边框 6；最多 5 行 = 96", () => {
    expect(COMPOSE_INPUT_MIN_HEIGHT).toBe(24);
    expect(composeInputMaxHeight(2000)).toBe(96);
    expect(composeInputMaxHeight(0)).toBe(96);
    expect(composeInputMaxHeight(Number.NaN)).toBe(96);
  });

  it("窗口矮时不超过终端区域的 30%，至少一行", () => {
    expect(composeInputMaxHeight(200)).toBe(60);
    expect(composeInputMaxHeight(100)).toBe(30);
    expect(composeInputMaxHeight(40)).toBe(COMPOSE_INPUT_MIN_HEIGHT);
  });
});

describe("撰写栏自身的高度（UI-19）", () => {
  it("一行 = 输入框 24 + 栏的边框和内边距 5 = 29", () => {
    expect(COMPOSE_BAR_CHROME).toBe(5);
    expect(COMPOSE_BAR_MIN_HEIGHT).toBe(29);
  });

  it("上限是终端区域的 60%（比自动增高的 30% 宽松），至少一行；量不到时用兜底", () => {
    expect(composeBarMaxHeight(1000)).toBe(600);
    expect(composeBarMaxHeight(500)).toBe(300);
    expect(composeBarMaxHeight(333)).toBe(199);
    expect(composeBarMaxHeight(30)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(composeBarMaxHeight(0)).toBe(480);
    expect(composeBarMaxHeight(-10)).toBe(480);
    expect(composeBarMaxHeight(Number.NaN)).toBe(480);
    expect(composeBarMaxHeight(2000)).toBeGreaterThan(composeInputMaxHeight(2000));
  });

  it("夹取：不小于一行、不超过上限，上限随终端高度变", () => {
    expect(clampComposeBarHeight(100, 1000)).toBe(100);
    expect(clampComposeBarHeight(1, 1000)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(clampComposeBarHeight(-50, 1000)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(clampComposeBarHeight(Number.NaN, 1000)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(clampComposeBarHeight(Number.POSITIVE_INFINITY, 1000)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(clampComposeBarHeight(900, 1000)).toBe(600);
    expect(clampComposeBarHeight(900, 500)).toBe(300);
    expect(clampComposeBarHeight(100.4, 1000)).toBe(100);
  });

  it("拖动：往上拖（clientY 变小）变高，往下拖变矮", () => {
    expect(dragComposeBarHeight(100, 500, 450, 1000)).toBe(150);
    expect(dragComposeBarHeight(100, 500, 530, 1000)).toBe(70);
    expect(dragComposeBarHeight(100, 500, 5000, 1000)).toBe(COMPOSE_BAR_MIN_HEIGHT);
    expect(dragComposeBarHeight(100, 500, -5000, 1000)).toBe(600);
  });

  it("体检：不是有限数、负数、0、不到一行都当 0（自动）；合法值取整", () => {
    expect(COMPOSE_BAR_AUTO).toBe(0);
    for (const bad of [0, -1, -100, 1, 28, Number.NaN, Number.POSITIVE_INFINITY, "100", null, undefined, {}]) {
      expect(sanitizeStoredComposeBarHeight(bad)).toBe(0);
    }
    expect(sanitizeStoredComposeBarHeight(29)).toBe(29);
    expect(sanitizeStoredComposeBarHeight(120.6)).toBe(121);
    // 大于上限的不在这里夹（显示时按当前终端高度夹，不改存的值）
    expect(sanitizeStoredComposeBarHeight(99999)).toBe(99999);
  });
});
