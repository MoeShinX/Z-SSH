//! 终端底部的撰写栏（简化版的 Xshell 撰写栏）：在输入框里把命令写好、改好，再发进终端。
//!
//! - Enter：发到当前窗格；Ctrl+Enter：发到这个标签的所有分屏；Shift+Enter：换行
//! - 右边的小开关选 Enter 和「发送」按钮发到「当前窗格」还是「所有分屏」（只在内存里，不保存）
//! - ↑ / ↓：翻最近发过的 50 条（输入框是空的，或光标在第一行 / 最后一行时才响应；历史只在内存里）
//! - Esc：把键盘还给当前终端；输入法组合输入中的 Enter 不发送
//! - 发送成功后清空并留在输入框里（方便连着写）；没连上时不清空
//!
//! 发送本身不在这里：交给 App 的同一套发送流程（和快捷命令栏共用，见 state/sendCommand.ts）。

import { SendHorizontal } from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MutableRefObject,
} from "react";

import { cn } from "../../lib/utils";
import {
  IDLE_NAV,
  caretOnFirstLine,
  caretOnLastLine,
  pushHistory,
  stepHistory,
  type HistoryNav,
} from "../../state/composeHistory";
import { Button } from "../ui/button";
import { composeInputMaxHeight, setLiveComposeBarHeight } from "./quickBarHeight";

export interface ComposeBarProps {
  /**
   * 发送：`all` = 发到这个标签的所有分屏。发过之后（可能一个窗格都没连上，`sent` 为 0）调 `afterSend`；
   * 用户在多行确认框里取消时不调。
   */
  onSend: (text: string, all: boolean, afterSend: (sent: number) => void) => void;
  /** Esc：把键盘还给当前终端。 */
  onEscape: () => void;
  /** 这个标签有几个分屏（只有一个时「所有分屏」没意义，开关灰掉）。 */
  paneCount: number;
  /** 发送历史（旧 → 新）；App 传一个 ref 让所有终端标签共用，不传就只在这个栏里。只在内存里。 */
  history?: MutableRefObject<string[]>;
}

export default function ComposeBar({ onSend, onEscape, paneCount, history }: ComposeBarProps) {
  const [value, setValue] = useState("");
  /** Enter 和发送按钮发到所有分屏（Ctrl+Enter 不看这个，始终是所有分屏）。 */
  const [allPanes, setAllPanes] = useState(false);
  const ownHistory = useRef<string[]>([]);
  const historyRef = history ?? ownHistory;
  const navRef = useRef<HistoryNav>(IDLE_NAV);
  const barRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  /** 翻历史后下一次渲染把光标放到末尾。 */
  const caretToEnd = useRef(false);

  // 输入框随内容长高：默认一行，最多约 5 行（也不超过终端区域的 30%），再多框内滚动。
  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    input.style.height = "auto";
    const border = input.offsetHeight - input.clientHeight;
    const natural = input.scrollHeight + border;
    if (natural <= border) {
      // 没有布局（jsdom 之类）：不设高度，靠 rows=1
      input.style.height = "";
      return;
    }
    const area = input
      .closest<HTMLElement>('[data-section="terminal-area"]')
      ?.getBoundingClientRect().height;
    const max = composeInputMaxHeight(area ?? 0);
    input.style.height = `${Math.min(natural, max)}px`;
    input.style.overflowY = natural > max ? "auto" : "hidden";
  }, [value]);

  useLayoutEffect(() => {
    if (!caretToEnd.current) return;
    caretToEnd.current = false;
    const input = inputRef.current;
    if (input) input.setSelectionRange(value.length, value.length);
  }, [value]);

  // 把栏的实际高度告诉 App：服务器概览面板的底部偏移要让出它。终端自己会被 flex 布局挤小，
  // 它的 ResizeObserver 会重新 fit，这里不用管。
  useEffect(() => {
    const bar = barRef.current;
    if (!bar) return;
    const report = () => setLiveComposeBarHeight(Math.round(bar.getBoundingClientRect().height));
    report();
    if (typeof ResizeObserver === "undefined") return () => setLiveComposeBarHeight(0);
    const observer = new ResizeObserver(report);
    observer.observe(bar);
    return () => {
      observer.disconnect();
      setLiveComposeBarHeight(0);
    };
  }, []);

  const submit = useCallback(
    (all: boolean) => {
      if (value.trim() === "") return;
      const text = value;
      onSend(text, all, (sent) => {
        // 一个窗格都没发出去：内容留着（已经提示过了）
        if (sent === 0) return;
        historyRef.current = pushHistory(historyRef.current, text);
        navRef.current = IDLE_NAV;
        setValue("");
        inputRef.current?.focus();
      });
    },
    [historyRef, onSend, value],
  );

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // 输入法组合输入中：Enter 是选字，不是发送
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === "Enter") {
      if (event.shiftKey) return; // 换行
      event.preventDefault();
      submit(event.ctrlKey || event.metaKey ? true : allPanes);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      onEscape();
      return;
    }
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    const input = event.currentTarget;
    const up = event.key === "ArrowUp";
    const atEdge = up
      ? caretOnFirstLine(input.value, input.selectionStart, input.selectionEnd)
      : caretOnLastLine(input.value, input.selectionStart, input.selectionEnd);
    if (!atEdge) return;
    const step = stepHistory(historyRef.current, navRef.current, up ? "up" : "down", input.value);
    if (!step) return;
    event.preventDefault();
    navRef.current = step.nav;
    caretToEnd.current = true;
    setValue(step.value);
  };

  const canSend = value.trim() !== "";

  return (
    <div
      ref={barRef}
      data-section="compose-bar"
      className="flex shrink-0 items-end gap-1 border-t border-border/60 px-1.5 py-0.5 text-xs"
    >
      {/* data-ime="native"：输入法跟随你自己的中英文状态（只有终端里切英文，见 terminal/imeFocus.ts） */}
      <textarea
        ref={inputRef}
        data-ime="native"
        data-section="compose-input"
        aria-label="撰写栏"
        rows={1}
        value={value}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        autoComplete="off"
        placeholder="输入命令，Enter 发送到当前窗格，Ctrl+Enter 发到所有分屏，Shift+Enter 换行"
        onChange={(event) => {
          // 自己改了内容：不再算在翻历史，下次 ↑ 从最新一条重新开始
          navRef.current = IDLE_NAV;
          setValue(event.target.value);
        }}
        onKeyDown={onKeyDown}
        className="block min-w-0 flex-1 resize-none rounded border border-input bg-transparent px-2 py-0.5 font-mono text-xs leading-[18px] placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      />
      <button
        type="button"
        data-action="compose-target"
        aria-pressed={allPanes}
        disabled={paneCount < 2 && !allPanes}
        onClick={() => setAllPanes((on) => !on)}
        title={
          paneCount < 2 && !allPanes
            ? "发送目标：先分屏（Ctrl+Shift+D / E）才能选「所有分屏」"
            : allPanes
              ? "Enter 和发送按钮现在发到这个标签的所有分屏（点一下改回当前窗格）"
              : "Enter 和发送按钮现在发到当前窗格（点一下改成所有分屏）"
        }
        className={cn(
          "h-6 shrink-0 rounded px-2 disabled:opacity-40",
          allPanes
            ? "bg-amber-500/20 text-amber-600 hover:bg-amber-500/30"
            : "text-muted-foreground hover:bg-accent hover:text-foreground",
        )}
      >
        {allPanes ? "所有分屏" : "当前窗格"}
      </button>
      <Button
        variant="ghost"
        size="icon"
        className="h-6 w-6 shrink-0"
        data-action="compose-send"
        title={allPanes ? "发送到所有分屏（Enter）" : "发送到当前窗格（Enter）；Ctrl+Enter 发到所有分屏"}
        aria-label="发送"
        disabled={!canSend}
        onClick={() => {
          submit(allPanes);
          inputRef.current?.focus();
        }}
      >
        <SendHorizontal size={13} />
      </Button>
    </div>
  );
}
