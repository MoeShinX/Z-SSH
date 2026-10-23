//! AI 面板输入框的高度（纯函数，拖动条和单测共用）。
//!
//! 规则：最小是默认高度，最大是面板高度的一半；双击拖动条恢复默认。
//! 高度和面板宽度一样存进设置表（`layout.ai_input_height`，本机布局、不同步）。

/**
 * 输入框的默认（也是最小）高度：0.2.6 之前是 50px（两行字），0.2.7 起调成 1.5 倍，约三行。
 * 存量里比它小的值（旧版存的 50 等）读回来按默认处理，所以升级后也会变高。
 */
export const AI_INPUT_DEFAULT_HEIGHT = 75;

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
