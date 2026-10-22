//! AI 面板输入框的高度（纯函数，拖动条和单测共用）。
//!
//! 规则：最小是原来的默认高度，最大是面板高度的一半；双击拖动条恢复默认。
//! 高度和面板宽度一样存进设置表（`layout.ai_input_height`，本机布局、不同步）。

/**
 * 输入框原来的默认高度：两行 12px 字（行高 16px）+ 上下内边距 8px + 边框 1px，
 * 和改之前 `<textarea>` 默认 2 行时渲染出来的一样。
 */
export const AI_INPUT_DEFAULT_HEIGHT = 50;

/** 存量值的硬顶：再大的当脏数据（远大于任何屏幕）。 */
const STORED_HARD_MAX = 20000;

/** 当前面板高度下允许的最大高度（面板高度的一半，至少是默认高度）。 */
export function inputHeightMax(panelHeight: number): number {
  if (!Number.isFinite(panelHeight) || panelHeight <= 0) return AI_INPUT_DEFAULT_HEIGHT;
  return Math.max(AI_INPUT_DEFAULT_HEIGHT, Math.floor(panelHeight / 2));
}

/** 把想要的高度夹进 [默认, 面板一半]。 */
export function clampInputHeight(desired: number, panelHeight: number): number {
  if (!Number.isFinite(desired)) return AI_INPUT_DEFAULT_HEIGHT;
  const max = inputHeightMax(panelHeight);
  return Math.min(max, Math.max(AI_INPUT_DEFAULT_HEIGHT, Math.round(desired)));
}

/**
 * 拖动中的高度：拖动条在输入框上沿，往上拖（clientY 变小）输入框变高。
 */
export function dragInputHeight(
  startHeight: number,
  startY: number,
  currentY: number,
  panelHeight: number,
): number {
  return clampInputHeight(startHeight + (startY - currentY), panelHeight);
}

/** 读回来的存量高度做体检：不是有限数、比默认小、大得离谱的一律回到默认。 */
export function sanitizeStoredInputHeight(raw: unknown): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return AI_INPUT_DEFAULT_HEIGHT;
  if (raw < AI_INPUT_DEFAULT_HEIGHT || raw > STORED_HARD_MAX) return AI_INPUT_DEFAULT_HEIGHT;
  return Math.round(raw);
}
