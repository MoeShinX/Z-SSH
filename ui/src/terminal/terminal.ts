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

import type { TerminalLinkHandlers } from "./links";

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

/**
 * `links` 接管链接的点击和悬停，两种来源共用同一套回调：文本里的网址（WebLinksAddon）
 * 和程序用转义序列输出的超链接（OSC 8，走 `options.linkHandler`）。不传就都不接管。
 * xterm 默认的处理是单击 `window.open`（OSC 8 还先弹 `confirm`），在 WebView2 里既不是
 * 用户的默认浏览器又容易误触，所以一律由调用方决定。
 * `allowNonHttpProtocols` 保持默认（false）：非 http / https 的 OSC 8 目标 xterm 根本不会
 * 当成链接，也就不会走到这里。
 */
export function createTerminal(theme: ITheme, links?: TerminalLinkHandlers): TerminalBundle {
  const terminal = new Terminal({
    fontFamily: DEFAULT_FONT_FAMILY,
    fontSize: DEFAULT_FONT_SIZE,
    scrollback: DEFAULT_SCROLLBACK,
    // unicode11 的宽度表属于 proposed API，中文和 emoji 的列宽要靠它才对。
    allowProposedApi: true,
    cursorBlink: true,
    theme,
    // OSC 8 的 activate 是左键 / 右键抬起都会来，和网址一样只认左键。
    linkHandler: links
      ? {
          activate: (event, uri, range) => {
            if (event.button === 0) links.onActivate(uri, event, range);
          },
          hover: (_event, uri, range) => links.onHover({ uri, range }),
          leave: () => links.onHover(null),
        }
      : null,
  });

  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  if (links) {
    terminal.loadAddon(
      // xterm 在鼠标抬起时激活链接，右键抬起也会来；只认左键。
      new WebLinksAddon(
        (event, uri) => {
          if (event.button === 0) links.onActivate(uri, event);
        },
        {
          hover: (_event, uri, range) => links.onHover({ uri, range }),
          leave: () => links.onHover(null),
        },
      ),
    );
  }

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
