//! 应用内的指针拖动（M3-3）：两栏之间、远端栏 → 终端。
//!
//! 为什么不用 HTML5 拖放：Tauri 的 `dragDropEnabled` 默认开着（为了拿到从资源管理器拖进来
//! 的文件路径），Tauri 文档写明在 Windows 上要在前端用 HTML5 拖放就必须把它关掉。
//! 关掉就拿不到文件路径，所以改用指针事件自己做：按下 → 拖过阈值 → 指针下面是哪个
//! 已登记的"落点" → 松手触发它。
//!
//! 落点是 DOM 元素 + 回调的登记表，不靠 data 属性，测试里可以直接桩 `elementFromPoint`。

import { create } from "zustand";

import type { SftpEntry } from "../api/sftp";

export interface DragPayload {
  kind: "sftp-entries";
  /** 从哪一栏拖起来的。 */
  side: "local" | "remote";
  entries: SftpEntry[];
}

export interface DropInfo {
  x: number;
  y: number;
  /** 指针下面最里层的元素（落点想知道落在了哪一行，自己 closest）。 */
  element: Element | null;
}

export interface DropTarget {
  accepts: (payload: DragPayload) => boolean;
  onDrop: (payload: DragPayload, info: DropInfo) => void;
}

/** 拖动的起步阈值（像素）：小于它按点击算。 */
export const DRAG_START_DISTANCE = 6;

const targets = new Map<Element, DropTarget>();

export function registerDropTarget(element: Element, target: DropTarget): () => void {
  targets.set(element, target);
  return () => {
    if (targets.get(element) === target) targets.delete(element);
  };
}

interface DragStore {
  payload: DragPayload | null;
  /** 当前悬停的落点元素（拿它判断"是不是我"来高亮）。 */
  hover: Element | null;
}

export const useDragStore = create<DragStore>(() => ({ payload: null, hover: null }));

/** 从指针位置往上找第一个愿意接这个 payload 的落点。 */
export function findTarget(
  x: number,
  y: number,
  payload: DragPayload,
): { element: Element; target: DropTarget; under: Element | null } | null {
  const under = document.elementFromPoint(x, y);
  for (let node: Element | null = under; node; node = node.parentElement) {
    const target = targets.get(node);
    if (target && target.accepts(payload)) return { element: node, target, under };
  }
  return null;
}

/**
 * 在 `pointerdown` 里调用。`makePayload` 在真的开始拖（过了阈值）时才算一次，
 * 点击不会触发它。返回后窗口级监听负责后面的事；拖动期间屏蔽文字选中。
 */
export function beginPointerDrag(
  event: { clientX: number; clientY: number; button: number },
  makePayload: () => DragPayload | null,
  callbacks: { onStart?: (payload: DragPayload) => void; onEnd?: (dropped: boolean) => void } = {},
): void {
  if (event.button !== 0) return;
  const origin = { x: event.clientX, y: event.clientY };
  let payload: DragPayload | null = null;
  let ghost: HTMLElement | null = null;

  const showGhost = (x: number, y: number) => {
    if (!payload) return;
    if (!ghost) {
      ghost = document.createElement("div");
      ghost.setAttribute("data-section", "drag-ghost");
      ghost.className =
        "pointer-events-none fixed z-[70] rounded-md border border-primary/60 bg-background/95 px-2 py-1 text-xs shadow-lg";
      const names = payload.entries.map((entry) => entry.name);
      ghost.textContent =
        names.length === 1 ? names[0] : `${names[0]} 等 ${names.length} 项`;
      document.body.appendChild(ghost);
    }
    ghost.style.left = `${x + 12}px`;
    ghost.style.top = `${y + 8}px`;
  };

  const cleanup = () => {
    window.removeEventListener("pointermove", move, true);
    window.removeEventListener("pointerup", up, true);
    window.removeEventListener("pointercancel", cancel, true);
    window.removeEventListener("keydown", key, true);
    document.body.classList.remove("zz-panel-resizing");
    ghost?.remove();
    ghost = null;
    useDragStore.setState({ payload: null, hover: null });
  };

  const move = (e: PointerEvent) => {
    if (!payload) {
      if (Math.hypot(e.clientX - origin.x, e.clientY - origin.y) < DRAG_START_DISTANCE) return;
      payload = makePayload();
      if (!payload) {
        cleanup();
        return;
      }
      document.body.classList.add("zz-panel-resizing");
      callbacks.onStart?.(payload);
      useDragStore.setState({ payload });
    }
    showGhost(e.clientX, e.clientY);
    const found = findTarget(e.clientX, e.clientY, payload);
    const next = found?.element ?? null;
    if (useDragStore.getState().hover !== next) useDragStore.setState({ hover: next });
  };

  const finish = (e: PointerEvent | null) => {
    const dragged = payload;
    const wasDragging = dragged !== null;
    let dropped = false;
    if (dragged && e) {
      const found = findTarget(e.clientX, e.clientY, dragged);
      if (found) {
        dropped = true;
        found.target.onDrop(dragged, { x: e.clientX, y: e.clientY, element: found.under });
      }
    }
    cleanup();
    if (wasDragging) {
      // 拖动之后浏览器还会补一个 click：吞掉，免得选中状态被改。
      const swallow = (ev: MouseEvent) => {
        ev.stopPropagation();
        ev.preventDefault();
      };
      window.addEventListener("click", swallow, { capture: true, once: true });
      window.setTimeout(() => window.removeEventListener("click", swallow, true), 0);
      callbacks.onEnd?.(dropped);
    }
  };
  const up = (e: PointerEvent) => finish(e);
  const cancel = () => finish(null);
  const key = (e: KeyboardEvent) => {
    if (e.key === "Escape") finish(null);
  };

  window.addEventListener("pointermove", move, true);
  window.addEventListener("pointerup", up, true);
  window.addEventListener("pointercancel", cancel, true);
  window.addEventListener("keydown", key, true);
}

// ---------------------------------------------------------------------------
// 从资源管理器拖进来的文件（M3-3 第 4 节）
//
// 这条路和上面的应用内指针拖动不是一回事：文件是操作系统拖进窗口的，只有 Tauri 的
// `onDragDropEvent` 能拿到路径（物理像素坐标）。这里把它翻译成"落在哪个已登记的落点上"。
// ---------------------------------------------------------------------------

export interface ExternalDropTarget {
  onDrop: (paths: string[], info: DropInfo) => void;
}

const externalTargets = new Map<Element, ExternalDropTarget>();

export function registerExternalDropTarget(
  element: Element,
  target: ExternalDropTarget,
): () => void {
  externalTargets.set(element, target);
  return () => {
    if (externalTargets.get(element) === target) externalTargets.delete(element);
  };
}

function findExternalTarget(
  x: number,
  y: number,
): { element: Element; target: ExternalDropTarget; under: Element | null } | null {
  const under = document.elementFromPoint(x, y);
  for (let node: Element | null = under; node; node = node.parentElement) {
    const target = externalTargets.get(node);
    if (target) return { element: node, target, under };
  }
  return null;
}

export type ExternalDragEvent =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

/**
 * 处理一次系统拖入事件。`position` 是物理像素，换成 CSS 像素要除以 `devicePixelRatio`。
 * 悬停时高亮落点，松手时把路径交给落点；没有落点的地方松手什么也不做。
 */
export function handleExternalDragEvent(event: ExternalDragEvent, pixelRatio: number): void {
  if (event.type === "leave") {
    useDragStore.setState({ hover: null });
    return;
  }
  const ratio = pixelRatio > 0 ? pixelRatio : 1;
  const x = event.position.x / ratio;
  const y = event.position.y / ratio;
  const found = findExternalTarget(x, y);
  if (event.type === "drop") {
    useDragStore.setState({ hover: null });
    if (found && event.paths.length > 0) {
      found.target.onDrop(event.paths, { x, y, element: found.under });
    }
    return;
  }
  const next = found?.element ?? null;
  if (useDragStore.getState().hover !== next) useDragStore.setState({ hover: next });
}

/** 测试用：清空登记表和拖动状态。 */
export function resetPointerDragForTests(): void {
  targets.clear();
  externalTargets.clear();
  useDragStore.setState({ payload: null, hover: null });
}
