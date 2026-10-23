//! 输入框高度：最小是默认高度，最大是面板高度的一半。

import { describe, expect, it } from "vitest";

import {
  AI_INPUT_DEFAULT_HEIGHT,
  clampInputHeight,
  dragInputHeight,
  inputHeightMax,
  sanitizeStoredInputHeight,
} from "./inputHeight";

describe("输入框高度的夹取", () => {
  it("上限是面板高度的一半，至少是默认高度", () => {
    expect(inputHeightMax(800)).toBe(400);
    expect(inputHeightMax(801)).toBe(400);
    expect(inputHeightMax(60)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(inputHeightMax(0)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(inputHeightMax(Number.NaN)).toBe(AI_INPUT_DEFAULT_HEIGHT);
  });

  it("夹进 [默认, 面板一半]，非法值回默认", () => {
    expect(clampInputHeight(10, 800)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(clampInputHeight(200.4, 800)).toBe(200);
    expect(clampInputHeight(999, 800)).toBe(400);
    expect(clampInputHeight(Number.POSITIVE_INFINITY, 800)).toBe(AI_INPUT_DEFAULT_HEIGHT);
  });

  it("往上拖变高、往下拖变矮，超出范围停在边上", () => {
    // 从 100px 开始，鼠标从 y=500 往上拖到 y=420：高 80px。
    expect(dragInputHeight(100, 500, 420, 800)).toBe(180);
    expect(dragInputHeight(120, 500, 540, 800)).toBe(80);
    expect(dragInputHeight(100, 500, 900, 800)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(dragInputHeight(100, 500, -500, 800)).toBe(400);
  });

  it("存量值体检：类型不对、比默认小、大得离谱的回默认", () => {
    expect(sanitizeStoredInputHeight(120)).toBe(120);
    expect(sanitizeStoredInputHeight(120.6)).toBe(121);
    expect(sanitizeStoredInputHeight(10)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    // 0.2.6 及以前的默认值 50 比现在的默认小：升级后按新默认显示
    expect(sanitizeStoredInputHeight(50)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(sanitizeStoredInputHeight("120")).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(sanitizeStoredInputHeight(null)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(sanitizeStoredInputHeight(Number.NaN)).toBe(AI_INPUT_DEFAULT_HEIGHT);
    expect(sanitizeStoredInputHeight(1e9)).toBe(AI_INPUT_DEFAULT_HEIGHT);
  });
});
