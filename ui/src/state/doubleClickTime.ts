//! 系统的双击时间（毫秒）：启动时向后端读一次并缓存，读不到用 500（Windows 的默认档）。
//!
//! Windows 的双击时间用户可以在「鼠标」设置里调（最大 5000ms）。界面里凡是依赖"两下点击
//! 算不算双击"的延迟，都要用它而不是写死的 500ms，否则调慢了双击的用户会看到面板闪烁。
//! 只在启动时读一次：中途改系统设置要重启 Z-SSH 才跟上，换来的是同步可读（定时器里直接用）。

import { systemDoubleClickTime } from "../api/system";

/** 读不到系统值时用的双击时间。 */
export const DEFAULT_DOUBLE_CLICK_MS = 500;
/** Windows 允许的双击时间上限，超出的值一律当作读到了脏数据。 */
const MAX_DOUBLE_CLICK_MS = 5000;

let cached = DEFAULT_DOUBLE_CLICK_MS;
let loading: Promise<number> | undefined;

/** 现在缓存的系统双击时间（还没读到时是 500）。 */
export function doubleClickMs(): number {
  return cached;
}

function valid(ms: unknown): ms is number {
  return typeof ms === "number" && Number.isFinite(ms) && ms >= 1 && ms <= MAX_DOUBLE_CLICK_MS;
}

/** 启动时调一次：读系统值并缓存；失败或值不合法就保持 500。重复调用只读一次。 */
export function loadDoubleClickTime(): Promise<number> {
  loading ??= Promise.resolve()
    .then(systemDoubleClickTime)
    .then((ms) => {
      if (valid(ms)) cached = Math.round(ms);
      return cached;
    })
    .catch(() => cached);
  return loading;
}

/** 测试用：直接指定双击时间（传 null 回到默认并允许重新读）。 */
export function setDoubleClickMsForTests(ms: number | null) {
  cached = ms ?? DEFAULT_DOUBLE_CLICK_MS;
  loading = undefined;
}
