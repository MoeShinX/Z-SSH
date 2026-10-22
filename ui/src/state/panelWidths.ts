//! 可调宽面板的宽度计算（UI-1 第 2 节）：纯函数 + 一点拖动状态，方便单测。
//!
//! 规则（任务卡给的）：
//! - AI 面板最小 320px、最大窗口宽度的 60%；主机编辑侧栏最小 320px、最大 50%；
//! - 终端区域至少保留 360px；窗口缩小时面板跟着收窄，不能把终端挤没；
//! - 双击把手恢复默认宽度（AI 面板 400px、侧栏 380px）；
//! - 宽度存进 settings 表（`layout.*`，本机布局，M4 不同步）。

/** AI 面板的默认宽度（双击把手恢复到这个值）。 */
export const AI_PANEL_DEFAULT = 400;
/** 主机编辑侧栏的默认宽度。 */
export const ASIDE_PANEL_DEFAULT = 380;
/** 两个面板的最小宽度。 */
export const PANEL_MIN = 320;
/** 终端区域至少保留的宽度。 */
export const TERMINAL_MIN = 360;
/** 面板最大宽度占窗口宽度的比例。 */
export const AI_PANEL_MAX_RATIO = 0.6;
export const ASIDE_PANEL_MAX_RATIO = 0.5;

/** SFTP 停靠面板（M3-3）：默认 420px，最小仍是 320px。 */
export const SFTP_PANEL_DEFAULT = 420;
export const SFTP_PANEL_MAX_RATIO = 0.6;
/** SFTP 面板内上下两栏的默认比例（远端占的份额）。 */
export const SFTP_SPLIT_DEFAULT = 0.5;
/** 上下分隔线能拖到的范围：每一栏至少留这么多份额。 */
export const SFTP_SPLIT_MIN = 0.15;

/**
 * 主机树侧栏（UI-5）：默认 224px（原来写死的 w-56）。
 * 下限 180：顶栏左侧那一块（logo、侧栏开关、「主机」标签）要放得下，主机名也不至于太挤；
 * 上限 min(480, 窗口宽的 40%)：再宽就是浪费终端的地方，同样要给终端留 TERMINAL_MIN。
 * 终端界面里右边开着 AI / SFTP 面板时，上限还要再扣掉它们的最小宽度（见 `hostTreeRightReserved`），
 * 但下限仍保底 180：宁可让 `computeDockLayout` 去收起 AI，也不把主机树挤没。
 */
export const HOST_TREE_DEFAULT = 224;
export const HOST_TREE_MIN = 180;
export const HOST_TREE_MAX = 480;
export const HOST_TREE_MAX_RATIO = 0.4;

/** 四种可调宽的面板。 */
export type PanelKind = "ai" | "aside" | "sftp" | "hostTree";

/** 某种面板的宽度上下限（跟随窗口宽度）。 */
export interface PanelBounds {
  min: number;
  max: number;
}

/**
 * 宽度上下限。
 *
 * 上限 = min(比例上限, 窗口宽 - 终端最少保留)；窗口很小时先保证终端，面板可以
 * 低于 320（两个下限取更小的那个，保证 min <= max，不会算出非法区间）。
 */
export function panelBounds(
  kind: PanelKind,
  windowWidth: number,
  /** 同屏另一个面板占的宽度（AI 和 SFTP 同时开时互相让位）。 */
  reserved = 0,
): PanelBounds {
  const ratio =
    kind === "ai"
      ? AI_PANEL_MAX_RATIO
      : kind === "sftp"
        ? SFTP_PANEL_MAX_RATIO
        : kind === "hostTree"
          ? HOST_TREE_MAX_RATIO
          : ASIDE_PANEL_MAX_RATIO;
  const available = Math.max(0, Math.floor(windowWidth - TERMINAL_MIN - Math.max(0, reserved)));
  // 主机树的下限只看窗口本身（不扣右边的面板）：右边面板占得再多，它也不能比 180 更窄。
  const min =
    kind === "hostTree"
      ? Math.min(HOST_TREE_MIN, Math.max(0, Math.floor(windowWidth - TERMINAL_MIN)))
      : Math.min(PANEL_MIN, available);
  const hardMax = kind === "hostTree" ? HOST_TREE_MAX : Number.POSITIVE_INFINITY;
  const max = Math.max(min, Math.floor(Math.min(windowWidth * ratio, available, hardMax)));
  return { min, max };
}

/** 把想要的宽度夹进合法区间。 */
export function clampPanelWidth(
  kind: PanelKind,
  desired: number,
  windowWidth: number,
  reserved = 0,
): number {
  const { min, max } = panelBounds(kind, windowWidth, reserved);
  if (!Number.isFinite(desired)) return min;
  return Math.min(max, Math.max(min, Math.round(desired)));
}

/**
 * 读回来的存量宽度做体检：不是有限数、负数、超大值一律回退到默认值（UI-1 4.1）。
 *
 * 上限给一个远大于任何屏幕的硬顶（20000），再大的当脏数据。
 */
export function sanitizeStoredWidth(raw: unknown, fallback: number): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return fallback;
  if (raw <= 0 || raw > 20000) return fallback;
  return Math.round(raw);
}

// ---------------------------------------------------------------------------
// 主机树右边开着的面板：主机树的上限要给它们让位。App 知道哪些面板开着，主机页、终端界面、
// 顶栏三处各自调 `useHostTreeWidth`，所以这个数放在模块级共享，三处读到同一个宽度。
// ---------------------------------------------------------------------------

/** 右边开着的面板（AI、SFTP，各占最小宽度）给主机树留出的宽度。 */
export function hostTreeRightReserved(aiOpen: boolean, sftpOpen: boolean): number {
  return (aiOpen ? PANEL_MIN : 0) + (sftpOpen ? PANEL_MIN : 0);
}

let rightReserved = 0;
const rightReservedListeners = new Set<() => void>();

export function getHostTreeRightReserved(): number {
  return rightReserved;
}

/** 设置右边面板占用的宽度（只有终端界面算；主机页传 0）。 */
export function setHostTreeRightReserved(width: number): void {
  const next = Math.max(0, Math.round(width));
  if (rightReserved === next) return;
  rightReserved = next;
  for (const listener of rightReservedListeners) listener();
}

export function subscribeHostTreeRightReserved(listener: () => void): () => void {
  rightReservedListeners.add(listener);
  return () => {
    rightReservedListeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------
// 拖动中的宽度：同一个面板可能有几处在读（主机树的宽度同时决定侧栏、顶栏左侧块和
// 终端区域的左边距），所以拖动期间的临时宽度放在模块级，大家订阅同一份。
// ---------------------------------------------------------------------------

const dragWidths = new Map<string, number>();
const dragWidthListeners = new Set<() => void>();

/** 某个设置键现在被拖到多宽了；没在拖是 null。 */
export function getPanelDragWidth(key: string): number | null {
  return dragWidths.get(key) ?? null;
}

/** 设置 / 清除（null）拖动中的宽度，并通知订阅者。 */
export function setPanelDragWidth(key: string, width: number | null): void {
  if (width === null) {
    if (!dragWidths.delete(key)) return;
  } else {
    if (dragWidths.get(key) === width) return;
    dragWidths.set(key, width);
  }
  for (const listener of dragWidthListeners) listener();
}

/** 订阅拖动中宽度的变化（给 useSyncExternalStore 用）。 */
export function subscribePanelDragWidth(listener: () => void): () => void {
  dragWidthListeners.add(listener);
  return () => {
    dragWidthListeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------
// 拖动状态：拖动期间终端不向服务器发 resize，松手才重排上报（UI-1 第 2 节）。
// ---------------------------------------------------------------------------

let dragging = false;
const listeners = new Set<(dragging: boolean) => void>();

/** 现在有没有面板 / 分隔线在拖。 */
export function isPanelDragging(): boolean {
  return dragging;
}

/** 改拖动状态；只在真的变化时通知（松手的那次会让终端重排一次并上报）。 */
export function setPanelDragging(next: boolean): void {
  if (dragging === next) return;
  dragging = next;
  for (const listener of listeners) listener(next);
}

/** 订阅拖动状态变化。 */
export function onPanelDragChange(listener: (dragging: boolean) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 测试用：复位拖动状态。 */
export function resetPanelDraggingForTests(): void {
  dragging = false;
  listeners.clear();
  dragWidths.clear();
  rightReserved = 0;
}

/** 拖动期间加在 `<body>` 上的类：禁掉文字选中（UI-1 第 2 节）。 */
export const DRAGGING_CLASS = "zz-panel-resizing";

/** 开始拖动：置状态 + 禁选中。 */
export function beginPanelDrag(): void {
  setPanelDragging(true);
  if (typeof document !== "undefined") {
    document.body.classList.add(DRAGGING_CLASS);
  }
}

/** 结束拖动：清状态 + 恢复选中。 */
export function endPanelDrag(): void {
  if (typeof document !== "undefined") {
    document.body.classList.remove(DRAGGING_CLASS);
  }
  setPanelDragging(false);
}
