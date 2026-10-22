//! 挡住 WebView 自带的浏览器行为：页面空白处右键弹出的「后退 / 刷新 / 另存为 / 打印」菜单，
//! 以及 F5、Ctrl+R 这类刷新快捷键。刷新等于把整个界面重新加载，所有连接、终端、传输都会断掉。
//!
//! - 右键：只在文本输入框（含只读的，要能复制）、可编辑区域、终端、选区本身上留着自带菜单；
//!   其他地方不弹。我们自己的右键菜单（主机、文件、命令按钮……）先处理并 `preventDefault`，不受影响。
//!   放行以后菜单里具体留哪些项，由后端在 WebView2 层按名字白名单再过滤一遍（见 src-tauri 的 native_menu）。
//! - 快捷键：只拦默认动作，不拦事件本身，终端照样收到 Ctrl+R（反向搜索历史）、文件面板照样收到 F5（刷新目录）。
//! - 开发构建不拦，方便调试时刷新。

import { NON_TEXT_INPUT_TYPES } from "../terminal/imeFocus";

export interface MenuPoint {
  x: number;
  y: number;
}

interface RectLike {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** 取一个选区范围的矩形；默认用浏览器的 `getClientRects`，测试里换成替身（jsdom 不做排版）。 */
export type RectsOfRange = (range: Range) => ArrayLike<RectLike>;

const clientRectsOf: RectsOfRange = (range) => range.getClientRects();

/**
 * 右键位置是不是落在选区上。选区只看 `isCollapsed`，不 trim：只选中空格也算选区。
 * 位置用 contextmenu 事件的 `clientX/clientY`，和 `getClientRects` 同一个坐标系。
 */
export function pointInSelection(
  selection: Pick<Selection, "isCollapsed" | "rangeCount" | "getRangeAt"> | null,
  point: MenuPoint,
  rectsOf: RectsOfRange = clientRectsOf,
): boolean {
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  for (let i = 0; i < selection.rangeCount; i++) {
    const rects = rectsOf(selection.getRangeAt(i));
    for (let j = 0; j < rects.length; j++) {
      const rect = rects[j];
      if (point.x >= rect.left && point.x <= rect.right && point.y >= rect.top && point.y <= rect.bottom) {
        return true;
      }
    }
  }
  return false;
}

/**
 * 能弹自带菜单的输入元素：文本类 input 和 textarea。只读的也要（能复制）；禁用的不要。
 * 密码框也放行：Chromium 自己会把密码框菜单里的剪切 / 复制置灰，我们不再另做。
 */
function isNativeMenuInput(element: Element): boolean {
  if (element instanceof HTMLTextAreaElement) return !element.disabled;
  if (element instanceof HTMLInputElement) {
    return !element.disabled && !NON_TEXT_INPUT_TYPES.has(element.type);
  }
  return false;
}

/**
 * 这次右键要不要留着 WebView 自带的菜单。
 * `selection` 为空表示没有选区信息（等同没有选区）。
 */
export function allowNativeContextMenu(
  target: EventTarget | null,
  selection: Pick<Selection, "isCollapsed" | "rangeCount" | "getRangeAt"> | null,
  point: MenuPoint,
  rectsOf: RectsOfRange = clientRectsOf,
): boolean {
  if (!(target instanceof Element)) return false;
  // 终端（xterm）：它的选区画在画布上，不是 DOM 选区；右键时 xterm 自己把隐藏的输入框挪到鼠标下并选中
  // 终端选区的文字，自带菜单里的复制 / 粘贴才有对象。target 是画布或行，不是那个输入框，所以按区域放行。
  // 开了「右键粘贴」时终端已经 preventDefault，走不到这里。
  if (target.closest(".xterm")) return true;
  // 编辑器（CodeMirror）这类可编辑区域：点到的是里面的行、字，往上找可编辑的容器
  if (target.closest('[contenteditable]:not([contenteditable="false"])')) return true;
  if (isNativeMenuInput(target.closest("input, textarea") ?? target)) return true;
  return pointInSelection(selection, point, rectsOf);
}

/** 会让 WebView 重新加载页面的按键。 */
export function isReloadShortcut(event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey">): boolean {
  if (event.key === "F5" || event.key === "BrowserRefresh") return true;
  return (event.ctrlKey || event.metaKey) && (event.key === "r" || event.key === "R");
}

export interface NativeGuardOptions {
  /** 测试用：替换取选区矩形的方式。 */
  rectsOf?: RectsOfRange;
}

/** 装到窗口上；返回卸载函数。 */
export function installNativeGuards(win: Window = window, options: NativeGuardOptions = {}): () => void {
  // 冒泡阶段、挂在 window 上：页面里各处自己的右键菜单和快捷键都先处理完了再看
  const onContextMenu = (event: MouseEvent) => {
    if (event.defaultPrevented) return;
    const point = { x: event.clientX, y: event.clientY };
    if (allowNativeContextMenu(event.target, win.getSelection(), point, options.rectsOf)) return;
    event.preventDefault();
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (isReloadShortcut(event)) event.preventDefault();
  };
  win.addEventListener("contextmenu", onContextMenu);
  win.addEventListener("keydown", onKeyDown);
  return () => {
    win.removeEventListener("contextmenu", onContextMenu);
    win.removeEventListener("keydown", onKeyDown);
  };
}
