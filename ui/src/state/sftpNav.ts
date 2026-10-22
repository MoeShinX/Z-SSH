//! SFTP 两栏的浏览历史（后退 / 前进）和目录列表缓存（进目录先显示上次的结果，后台再刷新）。

export interface NavHistory {
  stack: string[];
  index: number;
}

export const EMPTY_HISTORY: NavHistory = { stack: [], index: -1 };

/** 历史最多记这么多步，太久远的丢掉。 */
const HISTORY_LIMIT = 100;

export function current(history: NavHistory): string | null {
  return history.stack[history.index] ?? null;
}

/** 进了一个新目录：丢掉"前进"那一截，接在后面。和当前一样（刷新）不算一步。 */
export function visit(history: NavHistory, path: string): NavHistory {
  if (current(history) === path) return history;
  const stack = [...history.stack.slice(0, history.index + 1), path].slice(-HISTORY_LIMIT);
  return { stack, index: stack.length - 1 };
}

export function canBack(history: NavHistory): boolean {
  return history.index > 0;
}

export function canForward(history: NavHistory): boolean {
  return history.index >= 0 && history.index < history.stack.length - 1;
}

/** 后退 / 前进一步：返回新历史和要去的目录；走不了返回 null。 */
export function step(history: NavHistory, delta: -1 | 1): { history: NavHistory; path: string } | null {
  const index = history.index + delta;
  const path = history.stack[index];
  if (path === undefined || index < 0) return null;
  return { history: { ...history, index }, path };
}

/** 去不了的目录（被删了、没权限）从历史里拿掉，免得后退一直撞墙。 */
export function forget(history: NavHistory, path: string): NavHistory {
  const keepIndex = history.stack.slice(0, history.index + 1).filter((p) => p !== path).length - 1;
  const stack = history.stack.filter((p) => p !== path);
  return { stack, index: Math.min(Math.max(keepIndex, stack.length > 0 ? 0 : -1), stack.length - 1) };
}

/** 最近列过的目录（按路径），新的放最后；超过上限丢最久没用的。 */
export class ListingCache<T> {
  private map = new Map<string, T>();
  constructor(private readonly limit = 64) {}

  get(path: string): T | undefined {
    const value = this.map.get(path);
    if (value !== undefined) {
      this.map.delete(path);
      this.map.set(path, value);
    }
    return value;
  }

  set(path: string, value: T): void {
    this.map.delete(path);
    this.map.set(path, value);
    while (this.map.size > this.limit) {
      const oldest = this.map.keys().next().value;
      if (oldest === undefined) break;
      this.map.delete(oldest);
    }
  }

  delete(path: string): void {
    this.map.delete(path);
  }
}
