import { useEffect, useImperativeHandle, useRef, useState, type Ref } from "react";

import type { FitAddon } from "@xterm/addon-fit";

import { isPanelDragging, onPanelDragChange } from "../state/panelWidths";
import type { ITheme, Terminal } from "@xterm/xterm";

import { useSettings, useTerminalMode } from "../stores/settings";
import { attachHighlighter, compileRules, parseRules } from "../terminal/highlight";
import { imeFocus } from "../terminal/imeFocus";
import { applyTerminalTheme, createTerminal, loadWebglRenderer } from "../terminal/terminal";
import {
  multilinePaste,
  swallowsWithoutSelection,
  terminalClipboardAction,
} from "../state/terminalKeys";
import { ConfirmDialog } from "./ui/confirm-dialog";
import { toast } from "./ui/toast";

/** 容器大小稳定多久之后才通知后端，避免拖动窗口时刷屏。 */
const RESIZE_DEBOUNCE_MS = 100;

/**
 * fit 落到这个行列数以下，说明量到的不是真实尺寸，不能拿它开会话。
 *
 * `@xterm/addon-fit` 的 `proposeDimensions()` 在容器量不出尺寸时返回自己的下限
 * `max(2, …) 列 × max(1, …) 行`；这两个值就是"没量到"的信号。M1-08b 的 bug 正是
 * 容器 0×0 时把 2×1 当初始尺寸发给了远端 PTY。
 *
 * M1-08c 把它从"20 列 × 5 行"降到插件的下限：6 个 pane 挤在 1200px 窗口里，
 * 每个 pane 只有 15～17 列，那是**真实的窄 pane**，该照常开会话；把它当成没量到
 * 会让 pane 一直空着不开会话。真正需要拦的只有"量不出来"这一种。
 */
const DEGENERATE_COLS = 2;
const DEGENERATE_ROWS = 1;

export interface TerminalHandle {
  /** 写入后端送来的字节，xterm 处理完之后调用 onRendered。 */
  write(bytes: Uint8Array, onRendered?: () => void): void;
  /** 写入一行本地信息，比如断开原因。 */
  writeLine(text: string): void;
  /** 当前行列数。 */
  size(): { cols: number; rows: number };
  focus(): void;
  /**
   * 往输入行插入文本（带括号粘贴的处理交给 xterm），不加回车、不执行。
   * 和键盘、剪贴板、右键粘贴走同一套多行确认：开了「多行粘贴前确认」且带换行就先弹确认。
   */
  paste(text: string): void;
}

interface TerminalViewProps {
  ref?: Ref<TerminalHandle>;
  /** xterm 的配色（跟着终端主题走） */
  theme: ITheme;
  /** xterm 里的按键数据。 */
  onData: (data: string) => void;
  /** fit 之后的实际行列数，已经防抖。 */
  onResize: (cols: number, rows: number) => void;
  /** 挂载并完成第一次量测（这个尺寸就是后面开会话用的初始尺寸）。 */
  onReady?: (cols: number, rows: number) => void;
  /**
   * 粘贴内容要发给谁（一般是会话 ID）。等确认期间它变了（断线重连、换了会话），
   * 这次粘贴作废，不会发给新的会话。不传表示不区分。
   */
  pasteTarget?: string | null;
}

export default function TerminalView({
  ref,
  theme,
  onData,
  onResize,
  onReady,
  pasteTarget = null,
}: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  // 回调放 ref：终端只在挂载时建一次，不该跟着 props 重新建。
  const onDataRef = useRef(onData);
  const onResizeRef = useRef(onResize);
  const onReadyRef = useRef(onReady);
  const themeRef = useRef(theme);
  onDataRef.current = onData;
  onResizeRef.current = onResize;
  onReadyRef.current = onReady;
  themeRef.current = theme;

  // 终端外观设置（改完立刻生效，见下面的 effect）
  const fontFamily = useSettings((state) => state.terminal_font_family);
  const fontSize = useSettings((state) => state.terminal_font_size);
  const lineHeight = useSettings((state) => state.terminal_line_height);
  const scrollback = useSettings((state) => state.terminal_scrollback);
  const cursorStyle = useSettings((state) => state.terminal_cursor_style);
  const cursorBlink = useSettings((state) => state.terminal_cursor_blink);
  const copyOnSelect = useSettings((state) => state.terminal_copy_on_select);
  const rightClickPaste = useSettings((state) => state.terminal_right_click_paste);
  const confirmMultiline = useSettings((state) => state.terminal_confirm_multiline_paste);
  const highlightEnabled = useSettings((state) => state.terminal_highlight_enabled);
  const imeEnglish = useSettings((state) => state.terminal_ime_english);
  const highlightRules = useSettings((state) => state.terminal_highlight_rules);
  const lightTerminal = useTerminalMode() === "light";
  const behaviorRef = useRef({ copyOnSelect, rightClickPaste, confirmMultiline });
  behaviorRef.current = { copyOnSelect, rightClickPaste, confirmMultiline };
  const pasteTargetRef = useRef(pasteTarget);
  pasteTargetRef.current = pasteTarget;
  // 等用户确认的多行粘贴内容，连同请求粘贴那一刻的目标
  const [pending, setPending] = useState<{ text: string; target: string | null } | null>(null);
  const pendingPaste = pending?.text ?? null;
  /**
   * 所有往会话里粘贴文本的入口都走这里（键盘、剪贴板、右键、拖放的 handle.paste）：
   * 带换行且开了确认就先问，否则直接送进终端。
   */
  const requestPaste = (text: string) => {
    if (!text) return;
    if (behaviorRef.current.confirmMultiline && multilinePaste(text)) {
      setPending({ text, target: pasteTargetRef.current });
    } else {
      terminalRef.current?.paste(text);
    }
  };
  const requestPasteRef = useRef(requestPaste);
  requestPasteRef.current = requestPaste;
  // 等确认的时候目标会话变了（断线重连等）：这次粘贴作废，别把内容发给新的会话
  useEffect(() => {
    if (pending === null || pending.target === pasteTarget) return;
    setPending(null);
    toast.info("会话已变化，已取消待确认的粘贴");
  }, [pasteTarget, pending]);
  // mount effect 里存的 refit 函数，设置变了拿它重新量一次行列数
  const refitRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const { terminal, fitAddon } = createTerminal(themeRef.current);
    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;
    terminal.open(container);
    loadWebglRenderer(terminal);

    // 选中即复制：xterm 的选区变了就把文字写进剪贴板
    const selectionSubscription = terminal.onSelectionChange(() => {
      if (!behaviorRef.current.copyOnSelect) return;
      const text = terminal.getSelection();
      if (!text) return;
      void navigator.clipboard?.writeText(text).catch((error: unknown) => {
        console.warn("复制失败（剪贴板不可用）", error);
      });
    });

    // 右键粘贴：把剪贴板内容按终端的粘贴语义送进去
    const onContextMenu = (event: MouseEvent) => {
      if (!behaviorRef.current.rightClickPaste) return;
      event.preventDefault();
      void navigator.clipboard
        ?.readText()
        .then((text) => requestPasteRef.current(text))
        .catch((error: unknown) => {
          console.warn("粘贴失败（剪贴板不可用）", error);
        });
    };
    container.addEventListener("contextmenu", onContextMenu);
    // 其他途径的粘贴（Shift+Insert、输入法菜单等）走浏览器的 paste 事件：截下来同样先确认。
    // 捕获阶段拦，xterm 自己的 paste 处理就收不到了，不会粘两遍。
    const onPaste = (event: ClipboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      requestPasteRef.current(event.clipboardData?.getData("text/plain") ?? "");
    };
    container.addEventListener("paste", onPaste, true);

    // Ctrl+C（选中时）复制、Ctrl+V 粘贴。返回 false 让 xterm 不再处理这个键；
    // 粘贴要先 preventDefault，否则浏览器自己的粘贴事件还会让 xterm 再粘贴一遍。
    terminal.attachCustomKeyEventHandler((event) => {
      const action = terminalClipboardAction(event, terminal.hasSelection());
      if (action === "copy") {
        event.preventDefault();
        const text = terminal.getSelection();
        terminal.clearSelection();
        void navigator.clipboard?.writeText(text).catch((error: unknown) => {
          console.warn("复制失败（剪贴板不可用）", error);
        });
        return false;
      }
      if (action === "paste") {
        event.preventDefault();
        void navigator.clipboard
          ?.readText()
          .then((text) => requestPasteRef.current(text))
          .catch((error: unknown) => {
            console.warn("粘贴失败（剪贴板不可用）", error);
          });
        return false;
      }
      if (swallowsWithoutSelection(event)) {
        event.preventDefault();
        return false;
      }
      return true;
    });

    // 容器是 0×0 时（分屏层刚挂载的那一帧、切到设置页被 hidden 藏起来）不能 fit：
    // 0 尺寸上 fit 插件给出的是它的下限 2 列 × 1 行；display:none 时还会把
    // "100%" 当成 100px 量出一个假尺寸。这两种都会把回滚缓冲重排乱，还会顺着
    // onReady / onResize 传到远端去。
    const hasUsableBox = () => container.clientWidth > 0 && container.clientHeight > 0;

    const fit = () => {
      if (!hasUsableBox()) return;
      try {
        fitAddon.fit();
      } catch (error) {
        // 尺寸刚变化时 fit 可能抛，下一次 ResizeObserver 回调会再试。
        console.warn("终端 fit 失败", error);
      }
    };

    // 只在行列数真的变了才往上报，免得重复发 term_resize。
    let reported = { cols: 0, rows: 0 };
    let announced = false;
    const fitAndReport = () => {
      if (!hasUsableBox()) return;
      fit();
      const size = { cols: terminal.cols, rows: terminal.rows };
      if (size.cols === reported.cols && size.rows === reported.rows) return;
      // 落到 fit 的下限：这次不是真实量测，别拿它开会话，等下一次有效的测量再说。
      if (!announced && (size.cols <= DEGENERATE_COLS || size.rows <= DEGENERATE_ROWS)) {
        return;
      }
      reported = size;
      if (!announced) {
        // 第一次拿到有效尺寸：开这个 pane 的会话就用它
        announced = true;
        onReadyRef.current?.(size.cols, size.rows);
      }
      onResizeRef.current(size.cols, size.rows);
    };

    fitAndReport();
    refitRef.current = fitAndReport;

    const dataSubscription = terminal.onData((data) => onDataRef.current(data));

    // open() 时的量测可能早于 WebGL 渲染器接管，字宽会偏大；等第一帧画完
    // 和字体加载完成各再量一次，否则初始行列数和窗口对不上。
    const firstFrame = window.requestAnimationFrame(fitAndReport);
    let disposed = false;
    void document.fonts.ready.then(() => {
      if (!disposed) fitAndReport();
    });

    let resizeTimer: number | undefined;
    const observer = new ResizeObserver(() => {
      fit();
      window.clearTimeout(resizeTimer);
      // 面板 / 分隔线正在拖的时候只做视觉上的 fit，不向服务器上报
      // （UI-1 第 2、3 节：松手才按新尺寸重排并通知服务器）。
      if (isPanelDragging()) return;
      resizeTimer = window.setTimeout(() => {
        fitAndReport();
      }, RESIZE_DEBOUNCE_MS);
    });
    observer.observe(container);

    // 拖动结束：立刻按最终尺寸重排一次并上报。
    const stopDragWatch = onPanelDragChange((dragging) => {
      if (dragging || disposed) return;
      window.clearTimeout(resizeTimer);
      fitAndReport();
    });

    return () => {
      disposed = true;
      window.cancelAnimationFrame(firstFrame);
      window.clearTimeout(resizeTimer);
      stopDragWatch();
      observer.disconnect();
      container.removeEventListener("contextmenu", onContextMenu);
      container.removeEventListener("paste", onPaste, true);
      selectionSubscription.dispose();
      dataSubscription.dispose();
      refitRef.current = null;
      terminal.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
    };
  }, []);

  // 换终端主题：已经开着的终端也立刻跟着变
  useEffect(() => {
    if (terminalRef.current) applyTerminalTheme(terminalRef.current, theme);
  }, [theme]);

  // 终端外观设置：字体、字号、行高、回滚行数、光标。字体和字号会影响行列数，
  // 所以改完要让 fit 重新量一次，行列数变了会顺着 onResize 报给后端。
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    terminal.options.fontFamily = fontFamily;
    terminal.options.fontSize = fontSize;
    terminal.options.lineHeight = lineHeight;
    terminal.options.scrollback = scrollback;
    terminal.options.cursorStyle = cursorStyle;
    terminal.options.cursorBlink = cursorBlink;
    refitRef.current?.();
  }, [fontFamily, fontSize, lineHeight, scrollback, cursorStyle, cursorBlink]);

  // 焦点进终端：输入法切到英文；离开终端（点到别的输入框、切到别的程序）恢复原来的模式。
  // 请求排队串行发出，分屏之间切换不闪，细节见 terminal/imeFocus.ts
  useEffect(() => {
    const textarea = terminalRef.current?.textarea;
    if (!textarea || !imeEnglish) return;
    return imeFocus.attach(textarea);
  }, [imeEnglish]);

  // 关键词高亮：关着（默认）时完全不挂，不多花一点功夫
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal || !highlightEnabled) return;
    const compiled = compileRules(parseRules(highlightRules), lightTerminal);
    if (compiled.length === 0) return;
    const highlighter = attachHighlighter(terminal, compiled, {
      onTimeout: () => {
        // 规则写法会让匹配卡住（灾难性回溯）：整体关掉高亮，等用户改了规则再开
        void useSettings.getState().update("terminal_highlight_enabled", false);
        toast.error("关键词高亮的规则匹配太慢（多半是正则写法会反复回溯），已关闭高亮。请到设置 → 终端里修改规则后再打开。");
      },
    });
    return () => highlighter.dispose();
  }, [highlightEnabled, highlightRules, lightTerminal]);

  useImperativeHandle(
    ref,
    (): TerminalHandle => ({
      write(bytes, onRendered) {
        terminalRef.current?.write(bytes, onRendered);
      },
      writeLine(text) {
        terminalRef.current?.writeln(text);
      },
      size() {
        const terminal = terminalRef.current;
        return { cols: terminal?.cols ?? 80, rows: terminal?.rows ?? 24 };
      },
      paste(text) {
        requestPasteRef.current(text);
      },
      focus() {
        terminalRef.current?.focus();
      },
    }),
    [],
  );

  const pasteInfo = pendingPaste === null ? null : multilinePaste(pendingPaste);
  return (
    <>
      <div ref={containerRef} className="h-full w-full overflow-hidden" />
      {pendingPaste !== null && (
        <ConfirmDialog
          open
          title={`粘贴 ${pasteInfo?.lines ?? 0} 行内容？`}
          message={
            <div className="space-y-2">
              <p>内容带换行，对方 shell 没开括号粘贴模式时会直接执行。</p>
              <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-2 font-mono text-xs">
                {pasteInfo?.preview}
              </pre>
            </div>
          }
          confirmLabel="粘贴"
          onOpenChange={(open) => {
            if (!open) {
              setPending(null);
              terminalRef.current?.focus();
            }
          }}
          onConfirm={() => {
            // 再核对一次目标：确认按钮按下时它还得是请求粘贴时的那个会话
            if (pending && pending.target === pasteTargetRef.current) {
              terminalRef.current?.paste(pending.text);
            } else {
              toast.info("会话已变化，已取消粘贴");
            }
            setPending(null);
            terminalRef.current?.focus();
          }}
        />
      )}
    </>
  );
}
