//! 终端界面主机树底部的属性面板：现在展开的是哪台主机。
//!
//! 只在本次运行内记住（不写设置、不持久化）。放在模块级而不是组件 state 里，是因为主机树
//! 在切到主机页 / 设置页、收起再展开时会卸载重挂，展开的那台不该因此丢掉。
//!
//! 单击规则：点没展开的主机 = 展开它（已经展开了别的就切换）；再点同一台 = 收起。
//! 双击会先后来两次单击再来一次 dblclick，如果按字面处理，"点开着的那台"的第一下就收起、
//! 第二下又展开，面板来回闪。所以两道防线：
//! 1. 只有点击计数为 1 的单击才安排收起；计数 >= 2 的是双击的第二下，只取消待执行的收起，
//!    自己不安排任何动作；
//! 2. 收起延迟到系统双击间隔之后才执行，dblclick（连接）到来时同样取消。
//! 展开是立即的。

import { useSyncExternalStore } from "react";

/**
 * 收起的延迟，取 Windows 默认的双击间隔（500ms，设置里「鼠标」的双击速度默认档）：
 * 双击的两下间隔不会超过它，所以第二下一定赶在收起之前到来；又不会让人觉得面板没反应。
 */
export const COLLAPSE_DELAY_MS = 500;

let current: number | null = null;
let pending: ReturnType<typeof setTimeout> | undefined;
const listeners = new Set<() => void>();

function set(next: number | null) {
  if (current === next) return;
  current = next;
  for (const listener of listeners) listener();
}

export function cancelPendingCollapse() {
  if (pending !== undefined) {
    clearTimeout(pending);
    pending = undefined;
  }
}

/**
 * 单击一台主机。`clickCount` 是浏览器给的点击计数（`MouseEvent.detail`）：
 * >= 2 说明是双击的第二下，不当作"再点一下收起"。
 */
export function clickHostInfo(hostId: number, clickCount = 1) {
  cancelPendingCollapse();
  if (current === hostId) {
    if (clickCount >= 2) return;
    pending = setTimeout(() => {
      pending = undefined;
      set(null);
    }, COLLAPSE_DELAY_MS);
  } else {
    set(hostId);
  }
}

/** 点面板右上角的 ×。 */
export function closeHostInfo() {
  cancelPendingCollapse();
  set(null);
}

/** 测试用：回到初始状态。 */
export function resetHostInfoForTests() {
  cancelPendingCollapse();
  current = null;
  listeners.clear();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 当前展开属性面板的主机 id；没展开是 null。 */
export function useHostInfoId(): number | null {
  return useSyncExternalStore(subscribe, () => current);
}
