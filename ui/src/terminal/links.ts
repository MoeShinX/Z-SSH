//! 终端里链接的交互规则：单击选中整条链接，双击用系统浏览器打开。
//!
//! 链接有两种来源，共用同一套规则：文本里的网址（WebLinksAddon），和程序用转义序列输出的
//! 超链接（OSC 8，走 xterm 的 `linkHandler`）。OSC 8 链接的显示文字和目标网址可以不同：
//! 范围（`range`）是显示文字，回调里的 uri 是目标网址；选中的是显示文字，打开和复制用目标网址。
//!
//! xterm 的链接在每次鼠标抬起（mouseup）时都会调一次 activate，它自己没有"双击"的概念。
//! 这里用浏览器的点击计数判断：抬起事件的 `detail` 就是点击计数（UI Events 规范里
//! mousedown / mouseup / click 都带它，Chromium 也是这样），>= 2 才是双击；同时要求
//! 前一下点的是同一个链接实例（网址和缓冲区范围都相同），屏幕上两处相同网址各点一下不算。
//! 判定通过后清掉记录，连点三下不会开两个页面。
//!
//! **不再另设固定的时间窗口**：浏览器给出 `detail >= 2`，本身就代表操作系统按用户设置的
//! 双击时间认定这是一次双击（Windows 的双击时间可以调到 5 秒，写死 500ms 会把调慢了的
//! 用户的双击拒掉）。

/** 按下到抬起之间移动超过这个距离（像素）就是拖选，不算点击链接。 */
export const LINK_CLICK_MOVE_PX = 4;

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
  /**
   * 链接被点击（每次鼠标抬起一次）：具体是单击还是双击由使用方判定。
   * 文本网址不带范围（用最近一次 hover 的范围）；OSC 8 链接带范围。
   */
  onActivate: (uri: string, event: MouseEvent, range?: LinkRange) => void;
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

/** 两个范围是不是同一处。 */
export function sameLinkRange(a: LinkRange, b: LinkRange): boolean {
  return a.start.x === b.start.x && a.start.y === b.start.y && a.end.x === b.end.x && a.end.y === b.end.y;
}

/**
 * 这个目标能不能交给系统浏览器：只认 http / https。OSC 8 的目标可以是任意字符串
 * （xterm 默认已把非 http 的链接过滤掉，这里是第二道防线）。
 */
export function isOpenableUrl(uri: string): boolean {
  try {
    const { protocol } = new URL(uri);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

export interface LinkClick {
  uri: string;
  range: LinkRange;
  /** 浏览器的点击计数（`MouseEvent.detail`）。 */
  detail: number;
}

export interface LinkDoubleClickGate {
  /** 每次链接被点击时调用，是双击的第二下返回 true（该打开了）。 */
  isDoubleClick: (click: LinkClick) => boolean;
  /** 清掉候选：离开链接、拖选、点到别处、输出使链接失效时调用。 */
  reset: () => void;
}

export function createLinkDoubleClickGate(): LinkDoubleClickGate {
  let candidate: { uri: string; range: LinkRange } | null = null;
  return {
    isDoubleClick(click) {
      if (click.detail >= 2) {
        // 第二下及以后：只有前面记着同一个链接实例的第一下才算；不管结果如何都清掉候选
        const first = candidate;
        candidate = null;
        return (
          first !== null && first.uri === click.uri && sameLinkRange(first.range, click.range)
        );
      }
      candidate = { uri: click.uri, range: click.range };
      return false;
    },
    reset() {
      candidate = null;
    },
  };
}
