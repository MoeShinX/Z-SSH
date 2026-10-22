//! 终端实例的创建和渲染器降级，以及换主题。
//!
//! 主题来自 state/terminalThemes.ts（从 Netcatty 的 systemPresets 里挑的几套）。
//!
//! WebGL 在个别显卡和远程桌面下会初始化失败或中途丢上下文，这里保证两种情况
//! 都退回 xterm 自带的 DOM 渲染器，界面不会白屏。

import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal, type ITheme } from "@xterm/xterm";

import "@xterm/xterm/css/xterm.css";

/** 默认字体：JetBrains Mono（打进产物），回退到 Win11 自带的 Cascadia Mono。 */
export const DEFAULT_FONT_FAMILY =
  '"JetBrains Mono", "Cascadia Mono", Consolas, monospace';
export const DEFAULT_FONT_SIZE = 12;
export const DEFAULT_SCROLLBACK = 10000;

export interface TerminalBundle {
  terminal: Terminal;
  fitAddon: FitAddon;
}

export function createTerminal(theme: ITheme): TerminalBundle {
  const terminal = new Terminal({
    fontFamily: DEFAULT_FONT_FAMILY,
    fontSize: DEFAULT_FONT_SIZE,
    scrollback: DEFAULT_SCROLLBACK,
    // unicode11 的宽度表属于 proposed API，中文和 emoji 的列宽要靠它才对。
    allowProposedApi: true,
    cursorBlink: true,
    theme,
  });

  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.loadAddon(new WebLinksAddon());

  const unicode11 = new Unicode11Addon();
  terminal.loadAddon(unicode11);
  terminal.unicode.activeVersion = "11";

  return { terminal, fitAddon };
}

/** 尽量用 WebGL 渲染，失败或者丢上下文就用默认渲染器。 */
export function loadWebglRenderer(terminal: Terminal): void {
  const addon = new WebglAddon();
  addon.onContextLoss(() => {
    // dispose 之后 xterm 会退回默认渲染器，内容仍在，只是重绘方式变了。
    addon.dispose();
  });

  try {
    terminal.loadAddon(addon);
  } catch (error) {
    console.warn("WebGL 渲染器不可用，改用默认渲染器", error);
    addon.dispose();
  }
}

/** 换终端配色（已经开着的终端也立刻生效）。 */
export function applyTerminalTheme(terminal: Terminal, theme: ITheme): void {
  terminal.options.theme = theme;
}
