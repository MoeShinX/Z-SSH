//! 可调宽面板的纯计算测试（UI-1 4.1）：宽度夹取、窗口缩小、非法值回退。

import { describe, expect, it } from "vitest";

import {
  AI_PANEL_DEFAULT,
  ASIDE_PANEL_DEFAULT,
  PANEL_MIN,
  TERMINAL_MIN,
  clampPanelWidth,
  panelBounds,
  sanitizeStoredWidth,
} from "./panelWidths";

describe("面板宽度：上下限", () => {
  it("常规窗口：AI 面板 320~60%，侧栏 320~50%", () => {
    expect(panelBounds("ai", 1400)).toEqual({ min: PANEL_MIN, max: 840 });
    expect(panelBounds("aside", 1400)).toEqual({ min: PANEL_MIN, max: 700 });
  });

  it("终端至少保留 360px", () => {
    // 窗口 800：60% 是 480，但 800-360=440 更小，取 440。
    expect(panelBounds("ai", 800).max).toBe(800 - TERMINAL_MIN);
    // 窗口 700：侧栏 50% 是 350，但 700-360=340 更小 → 340（先保证终端）。
    expect(panelBounds("aside", 700).max).toBe(700 - TERMINAL_MIN);
  });

  it("窗口特别小：先保证终端，面板可以低于 320；区间永远合法", () => {
    const bounds = panelBounds("ai", 600);
    expect(bounds.min).toBe(600 - TERMINAL_MIN); // 240
    expect(bounds.max).toBe(240);
    expect(bounds.min).toBeLessThanOrEqual(bounds.max);
  });

  it("夹取：最小、最大、窗口缩小时跟着收窄", () => {
    expect(clampPanelWidth("ai", 100, 1400)).toBe(PANEL_MIN);
    expect(clampPanelWidth("ai", 5000, 1400)).toBe(840);
    expect(clampPanelWidth("ai", 600, 1400)).toBe(600);
    // 窗口缩到 700：存着的 600 放不下 → 收到 340。
    expect(clampPanelWidth("ai", 600, 700)).toBe(700 - TERMINAL_MIN);
    // NaN / Infinity 回退到最小宽度。
    expect(clampPanelWidth("ai", Number.NaN, 1400)).toBe(PANEL_MIN);
    expect(clampPanelWidth("aside", Number.POSITIVE_INFINITY, 1400)).toBe(PANEL_MIN);
  });
});

describe("面板宽度：存量值体检（非法值回退默认）", () => {
  it("正常数字原样返回", () => {
    expect(sanitizeStoredWidth(560, AI_PANEL_DEFAULT)).toBe(560);
    expect(sanitizeStoredWidth(380.4, ASIDE_PANEL_DEFAULT)).toBe(380);
  });

  it("负数 / 零 / 字符串 / 超大值都回退默认", () => {
    expect(sanitizeStoredWidth(-100, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth(0, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth("700", AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth(null, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth(undefined, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth(999999, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
    expect(sanitizeStoredWidth(Number.NaN, AI_PANEL_DEFAULT)).toBe(AI_PANEL_DEFAULT);
  });
});
