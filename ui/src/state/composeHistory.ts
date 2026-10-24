//! 撰写栏的发送历史（纯函数）：最近发过的 50 条，↑ / ↓ 翻。
//!
//! 只存在内存里（App 的一个 ref）：不写磁盘、不进设置、不同步——里面可能有密码。
//! 列表按「旧 → 新」排；翻历史的状态是 `{ cursor, draft }`：cursor 为 null 表示没在翻，
//! 否则是正在看的那条的下标，draft 是开始翻之前输入框里的内容（翻回最新时还原）。

export const COMPOSE_HISTORY_MAX = 50;

export interface HistoryNav {
  cursor: number | null;
  draft: string;
}

export const IDLE_NAV: HistoryNav = { cursor: null, draft: "" };

/** 记一条发送过的内容：和已有的一模一样就挪到最新，超过 50 条丢最旧的；空白不记。 */
export function pushHistory(list: readonly string[], text: string): string[] {
  if (text.trim() === "") return [...list];
  const next = list.filter((item) => item !== text);
  next.push(text);
  return next.length > COMPOSE_HISTORY_MAX ? next.slice(next.length - COMPOSE_HISTORY_MAX) : next;
}

/** 光标是不是选区为空且在第一行（↑ 才翻历史）；输入框是空的也算。 */
export function caretOnFirstLine(value: string, selStart: number, selEnd: number): boolean {
  if (value === "") return true;
  if (selStart !== selEnd) return false;
  return !value.slice(0, selStart).includes("\n");
}

/** 光标是不是选区为空且在最后一行（↓ 才翻历史）；输入框是空的也算。 */
export function caretOnLastLine(value: string, selStart: number, selEnd: number): boolean {
  if (value === "") return true;
  if (selStart !== selEnd) return false;
  return !value.slice(selEnd).includes("\n");
}

export interface HistoryStep {
  value: string;
  nav: HistoryNav;
}

/**
 * 往旧（up）或往新（down）翻一步。翻不动（没有历史、已经是最旧 / 没在翻就按 ↓）返回 null，
 * 调用方就不拦这次按键，光标照常动。
 */
export function stepHistory(
  list: readonly string[],
  nav: HistoryNav,
  direction: "up" | "down",
  current: string,
): HistoryStep | null {
  if (direction === "up") {
    if (list.length === 0) return null;
    if (nav.cursor === null) {
      const cursor = list.length - 1;
      return { value: list[cursor], nav: { cursor, draft: current } };
    }
    if (nav.cursor <= 0) return null;
    const cursor = nav.cursor - 1;
    return { value: list[cursor], nav: { cursor, draft: nav.draft } };
  }
  if (nav.cursor === null) return null;
  const cursor = nav.cursor + 1;
  if (cursor >= list.length) return { value: nav.draft, nav: IDLE_NAV };
  return { value: list[cursor], nav: { cursor, draft: nav.draft } };
}
