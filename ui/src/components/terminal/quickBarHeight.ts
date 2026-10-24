//! 终端底部快捷命令栏的高度（纯函数 + 一点共享状态，拖动条、命令栏和 App 共用）。
//!
//! 规则：默认和最小都是原来的一行高度；最大约 5 行，且不超过终端区域高度的 40%。
//! 高度和别的布局一样存进设置表（`layout.quick_bar_height`，本机布局、不同步）。

import { useSyncExternalStore } from "react";

/** 命令栏原来的一行高度（`h-7`，含上边框 1px），也是最小值和双击恢复的值。 */
export const QUICK_BAR_DEFAULT_HEIGHT = 28;
/** 多行时每一行占的高度：按钮 20px + 上下各 2px 外边距。 */
export const QUICK_BAR_ROW_HEIGHT = 24;
/** 最多显示的行数（超过的在栏内纵向滚动）。 */
export const QUICK_BAR_MAX_ROWS = 5;
/** 最大高度不超过终端区域高度的这个比例。 */
export const QUICK_BAR_MAX_RATIO = 0.4;

/** 存量值的硬顶：再大的当脏数据（远大于任何屏幕）。 */
const STORED_HARD_MAX = 20000;

/** 当前终端区域高度下允许的最大高度：min(5 行, 区域的 40%)，至少是默认高度。 */
export function quickBarMaxHeight(areaHeight: number): number {
  const rowsMax = QUICK_BAR_DEFAULT_HEIGHT + (QUICK_BAR_MAX_ROWS - 1) * QUICK_BAR_ROW_HEIGHT;
  if (!Number.isFinite(areaHeight) || areaHeight <= 0) return rowsMax;
  return Math.max(
    QUICK_BAR_DEFAULT_HEIGHT,
    Math.min(rowsMax, Math.floor(areaHeight * QUICK_BAR_MAX_RATIO)),
  );
}

/** 把想要的高度夹进 [默认, 上限]。 */
export function clampQuickBarHeight(desired: number, areaHeight: number): number {
  if (!Number.isFinite(desired)) return QUICK_BAR_DEFAULT_HEIGHT;
  return Math.min(
    quickBarMaxHeight(areaHeight),
    Math.max(QUICK_BAR_DEFAULT_HEIGHT, Math.round(desired)),
  );
}

/** 拖动中的高度：拖动条在栏的上沿，往上拖（clientY 变小）栏变高。 */
export function dragQuickBarHeight(
  startHeight: number,
  startY: number,
  currentY: number,
  areaHeight: number,
): number {
  return clampQuickBarHeight(startHeight + (startY - currentY), areaHeight);
}

/** 读回来的存量高度做体检：不是有限数、比默认小、大得离谱的一律回到默认。 */
export function sanitizeStoredQuickBarHeight(raw: unknown): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return QUICK_BAR_DEFAULT_HEIGHT;
  if (raw < QUICK_BAR_DEFAULT_HEIGHT || raw > STORED_HARD_MAX) return QUICK_BAR_DEFAULT_HEIGHT;
  return Math.round(raw);
}

/** 状态栏（`h-6`）的高度。 */
const STATS_BAR_HEIGHT = 24;

/**
 * 服务器概览面板的底部偏移：让出状态栏，命令栏开着时再让出它的实际高度，
 * 撰写栏开着时再让出撰写栏的实际高度（它随输入的行数长高）。
 */
export function overviewBottomOffset(
  quickBarOpen: boolean,
  quickBarHeight: number,
  composeBarHeight = 0,
): number {
  return STATS_BAR_HEIGHT + (quickBarOpen ? quickBarHeight : 0) + composeBarHeight;
}

// ---------------------------------------------------------------------------
// 命令栏当前实际高度（含拖动中的）：命令栏上报，App 读去摆概览面板。
// ---------------------------------------------------------------------------

let liveHeight = QUICK_BAR_DEFAULT_HEIGHT;
const listeners = new Set<() => void>();

/** 命令栏上报自己现在的实际高度；卸载时传默认值。 */
export function setLiveQuickBarHeight(next: number): void {
  if (liveHeight === next) return;
  liveHeight = next;
  for (const listener of listeners) listener();
}

/** 订阅命令栏的实际高度（没有命令栏时是默认高度）。 */
export function useLiveQuickBarHeight(): number {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => liveHeight,
    () => QUICK_BAR_DEFAULT_HEIGHT,
  );
}

// ---------------------------------------------------------------------------
// 撰写栏当前实际高度（随输入的行数变）：撰写栏上报，App 读去摆概览面板。没有撰写栏时是 0。
// ---------------------------------------------------------------------------

let liveComposeHeight = 0;
const composeListeners = new Set<() => void>();

/** 撰写栏上报自己现在的实际高度；卸载时传 0。 */
export function setLiveComposeBarHeight(next: number): void {
  if (liveComposeHeight === next) return;
  liveComposeHeight = next;
  for (const listener of composeListeners) listener();
}

/** 订阅撰写栏的实际高度（没有撰写栏时是 0）。 */
export function useLiveComposeBarHeight(): number {
  return useSyncExternalStore(
    (listener) => {
      composeListeners.add(listener);
      return () => {
        composeListeners.delete(listener);
      };
    },
    () => liveComposeHeight,
    () => 0,
  );
}

/** 撰写栏输入框一行的内容高度（`leading-[18px]`），最多长到这么多行，再多在框内滚动。 */
export const COMPOSE_LINE_HEIGHT = 18;
export const COMPOSE_MAX_ROWS = 5;
/** 输入框上下内边距（`py-0.5`）加上下边框，一共多出来的高度。 */
export const COMPOSE_INPUT_CHROME = 6;
/** 输入框高度不超过终端区域高度的这个比例（窗口很矮时不把终端挤没）。 */
export const COMPOSE_MAX_RATIO = 0.3;

/** 输入框只有一行时的高度，也是最小值。 */
export const COMPOSE_INPUT_MIN_HEIGHT = COMPOSE_LINE_HEIGHT + COMPOSE_INPUT_CHROME;

/** 当前终端区域高度下，撰写栏输入框最多长到多高：min(5 行, 区域的 30%)，至少一行。 */
export function composeInputMaxHeight(areaHeight: number): number {
  const rowsMax = COMPOSE_LINE_HEIGHT * COMPOSE_MAX_ROWS + COMPOSE_INPUT_CHROME;
  if (!Number.isFinite(areaHeight) || areaHeight <= 0) return rowsMax;
  return Math.max(
    COMPOSE_INPUT_MIN_HEIGHT,
    Math.min(rowsMax, Math.floor(areaHeight * COMPOSE_MAX_RATIO)),
  );
}

// ---------------------------------------------------------------------------
// 撰写栏本身的高度（UI-19）：上沿拖动条调，存 `layout.compose_bar_height`。
// 0 = 自动（随内容 1 行到约 5 行，上面的 composeInputMaxHeight）；大于 0 = 用户拖出来的固定高度。
// ---------------------------------------------------------------------------

/** 栏比输入框多出来的高度：上边框 1px + 上下内边距（`py-0.5`）各 2px。 */
export const COMPOSE_BAR_CHROME = 5;
/** 撰写栏只有一行时的高度，也是固定高度的最小值。 */
export const COMPOSE_BAR_MIN_HEIGHT = COMPOSE_INPUT_MIN_HEIGHT + COMPOSE_BAR_CHROME;
/** 固定高度最大不超过终端区域高度的这个比例（比自动增高的 30% 宽松，这是「自由调」）。 */
export const COMPOSE_BAR_MAX_RATIO = 0.6;
/** 设置值 0 = 自动增高（默认）。 */
export const COMPOSE_BAR_AUTO = 0;
/** 量不到终端区域高度时的上限。 */
const COMPOSE_BAR_FALLBACK_MAX = 480;

/** 当前终端区域高度下撰写栏最高能拖到多少：区域的 60%，至少一行。 */
export function composeBarMaxHeight(areaHeight: number): number {
  if (!Number.isFinite(areaHeight) || areaHeight <= 0) return COMPOSE_BAR_FALLBACK_MAX;
  return Math.max(COMPOSE_BAR_MIN_HEIGHT, Math.floor(areaHeight * COMPOSE_BAR_MAX_RATIO));
}

/** 把想要的高度夹进 [一行, 区域的 60%]。 */
export function clampComposeBarHeight(desired: number, areaHeight: number): number {
  if (!Number.isFinite(desired)) return COMPOSE_BAR_MIN_HEIGHT;
  return Math.min(
    composeBarMaxHeight(areaHeight),
    Math.max(COMPOSE_BAR_MIN_HEIGHT, Math.round(desired)),
  );
}

/** 拖动中的高度：拖动条在栏的上沿，往上拖（clientY 变小）栏变高。 */
export function dragComposeBarHeight(
  startHeight: number,
  startY: number,
  currentY: number,
  areaHeight: number,
): number {
  return clampComposeBarHeight(startHeight + (startY - currentY), areaHeight);
}

/** 读回来的存量值做体检：不是有限数、负数、0、小于一行的都当 0（自动）；其余取整，上限在显示时再夹。 */
export function sanitizeStoredComposeBarHeight(raw: unknown): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return COMPOSE_BAR_AUTO;
  if (raw < COMPOSE_BAR_MIN_HEIGHT) return COMPOSE_BAR_AUTO;
  return Math.round(raw);
}
