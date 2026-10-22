//! 终端里网址链接的交互规则：单击选中整条链接，双击用系统浏览器打开。
//!
//! xterm 的链接在每次鼠标抬起（mouseup）时都会调一次 activate，它自己没有"双击"的概念，
//! 所以这里按"同一个网址在双击间隔内第二次被激活"来判定。判定通过后清掉记录，
//! 连点三下不会开两个页面。

/** 两次点击算双击的最长间隔（毫秒），取 Windows 默认的双击时间。 */
export const LINK_DOUBLE_CLICK_MS = 500;

/**
 * 链接在缓冲区里的范围：xterm 给的是 1 起始、右端包含的缓冲区坐标（y 是整个回滚缓冲里的
 * 行号，不是屏幕行号），折行的长链接 start.y 和 end.y 不同。
 */
export interface LinkRange {
  start: { x: number; y: number };
  end: { x: number; y: number };
}

export interface HoveredLink {
  uri: string;
  range: LinkRange;
}

export interface TerminalLinkHandlers {
  /** 链接被点击（每次鼠标抬起一次）：具体是单击还是双击由使用方判定。 */
  onActivate: (uri: string) => void;
  /** 鼠标移到链接上（传链接）或移出链接（传 null）。 */
  onHover: (link: HoveredLink | null) => void;
}

/** 把链接范围换成 `terminal.select(column, row, length)` 的参数（折行的链接按整行宽度累加）。 */
export function linkSelection(
  range: LinkRange,
  cols: number,
): { column: number; row: number; length: number } {
  return {
    column: range.start.x - 1,
    row: range.start.y - 1,
    length: (range.end.y - range.start.y) * cols + (range.end.x - range.start.x) + 1,
  };
}

/** 返回一个函数：每次链接被点击时调用，第二下返回 true（该打开了）。 */
export function createLinkDoubleClickGate(
  now: () => number = () => Date.now(),
  windowMs: number = LINK_DOUBLE_CLICK_MS,
): (uri: string) => boolean {
  let last: { uri: string; at: number } | null = null;
  return (uri) => {
    const at = now();
    if (last && last.uri === uri && at - last.at <= windowMs) {
      last = null;
      return true;
    }
    last = { uri, at };
    return false;
  };
}
