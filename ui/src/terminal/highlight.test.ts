//! 高亮：规则解析、匹配优先级、宽字符列号、只扫可见行。

import type { IBufferLine, Terminal } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";

import {
  attachHighlighter,
  compileRules,
  findMatches,
  inlineMatcher,
  lineText,
  MATCH_TIMEOUT_MS,
  parseRules,
  REFRESH_DELAY_MS,
  ruleError,
  type HighlightRule,
} from "./highlight";

const rule = (pattern: string, extra: Partial<HighlightRule> = {}): HighlightRule => ({
  id: pattern,
  pattern,
  regex: false,
  caseSensitive: false,
  color: "#ef4444",
  background: false,
  enabled: true,
  ...extra,
});

/** 假的一行缓冲：中文字符占两格，第二格宽度 0。 */
function fakeLine(text: string): IBufferLine {
  const cells: { chars: string; width: number }[] = [];
  for (const char of text) {
    const wide = /[\u4e00-\u9fff]/.test(char);
    cells.push({ chars: char, width: wide ? 2 : 1 });
    if (wide) cells.push({ chars: "", width: 0 });
  }
  return {
    getCell: (x: number) =>
      cells[x] ? { getChars: () => cells[x].chars, getWidth: () => cells[x].width } : undefined,
    translateToString: () => text,
  } as unknown as IBufferLine;
}

describe("规则", () => {
  it("parseRules 丢掉格式不对的条目，颜色不对用默认", () => {
    const rules = parseRules(
      JSON.stringify([{ id: "a", pattern: "err", color: "red" }, { pattern: "" }, 3, { id: "b", pattern: "ok", color: "#22c55e" }]),
    );
    expect(rules.map((item) => [item.pattern, item.color])).toEqual([
      ["err", "#ef4444"],
      ["ok", "#22c55e"],
    ]);
    expect(parseRules("not json")).toEqual([]);
  });

  it("ruleError 指出写错的正则和能匹配空串的正则", () => {
    expect(ruleError(rule("a(", { regex: true }))).not.toBeNull();
    expect(ruleError(rule("x*", { regex: true }))).toContain("空字符串");
    expect(ruleError(rule("a("))).toBeNull();
    expect(compileRules([rule("a(", { regex: true }), rule("a(")])).toHaveLength(1);
  });

  it("findMatches：字面文字要转义；前面的规则优先，不重叠", () => {
    const rules = compileRules([rule("error"), rule("err.r", { regex: true, color: "#3b82f6" }), rule("1.5")]);
    const matches = findMatches("an ERROR, errxr and 1x5 1.5", rules);
    expect(matches.map((item) => [item.start, item.end, item.rule.color])).toEqual([
      [3, 8, "#ef4444"],
      [10, 15, "#3b82f6"],
      [24, 27, "#ef4444"],
    ]);
  });

  it("区分大小写", () => {
    const rules = compileRules([rule("Error", { caseSensitive: true })]);
    expect(findMatches("error Error", rules).map((item) => item.start)).toEqual([6]);
  });
});

describe("lineText", () => {
  it("中文占两列，列号不错位", () => {
    const { text, columns } = lineText(fakeLine("错误 error"), 20);
    expect(text).toBe("错误 error");
    const start = text.indexOf("error");
    expect(columns[start]).toBe(5);
    expect(columns[0]).toBe(0);
    expect(columns[1]).toBe(2);
  });
});

describe("attachHighlighter", () => {
  function fakeTerminal(lines: string[], viewportY: number, rows: number) {
    const decorations: { line: number; x: number; width: number; color?: string }[] = [];
    const writeHandlers: (() => void)[] = [];
    let cursorLine = lines.length - 1;
    const terminal = {
      rows,
      cols: 40,
      buffer: {
        active: {
          type: "normal",
          baseY: 0,
          get cursorY() {
            return cursorLine;
          },
          viewportY,
          getLine: (y: number) => (lines[y] === undefined ? undefined : fakeLine(lines[y])),
        },
        onBufferChange: () => ({ dispose() {} }),
      },
      registerMarker: (offset: number) => ({ line: cursorLine + offset, dispose: vi.fn() }),
      registerDecoration: (options: { marker: { line: number }; x: number; width: number; foregroundColor?: string }) => {
        const entry = { line: options.marker.line, x: options.x, width: options.width, color: options.foregroundColor };
        decorations.push(entry);
        return { dispose: () => decorations.splice(decorations.indexOf(entry), 1) };
      },
      onWriteParsed: (handler: () => void) => {
        writeHandlers.push(handler);
        return { dispose() {} };
      },
      onScroll: () => ({ dispose() {} }),
      onResize: () => ({ dispose() {} }),
      onRender: () => ({ dispose() {} }),
    };
    return {
      terminal: terminal as unknown as Terminal,
      decorations,
      setCursor: (line: number) => (cursorLine = line),
      write: () => writeHandlers.forEach((handler) => handler()),
    };
  }

  it("只标可见的那几行；关掉以后清干净", async () => {
    vi.useFakeTimers();
    const lines = ["error 0", "error 1", "ok", "error 3", "error 4"];
    const { terminal, decorations } = fakeTerminal(lines, 2, 2);
    const highlighter = attachHighlighter(terminal, compileRules([rule("error")]), {
      matcher: inlineMatcher,
    });
    await vi.advanceTimersByTimeAsync(REFRESH_DELAY_MS);
    expect(decorations.map((item) => [item.line, item.x, item.width])).toEqual([[3, 0, 5]]);

    highlighter.setRules([]);
    expect(decorations).toEqual([]);
    highlighter.dispose();
    vi.useRealTimers();
  });

  it("复审 F6：匹配太慢（后台线程超时）就停掉高亮并通知，不再继续标", async () => {
    vi.useFakeTimers();
    const { terminal, decorations } = fakeTerminal(["aaaaaaaaaaaaaaaaaaaaaaaaaaaa!"], 0, 1);
    const onTimeout = vi.fn();
    const matcher = vi.fn(async () => "timeout" as const);
    const highlighter = attachHighlighter(terminal, compileRules([rule("(a+)+$", { regex: true })]), {
      matcher,
      onTimeout,
    });
    await vi.advanceTimersByTimeAsync(REFRESH_DELAY_MS);
    expect(onTimeout).toHaveBeenCalledTimes(1);
    expect(decorations).toEqual([]);
    // 之后的输出不再去匹配（规则已经停了）
    await vi.advanceTimersByTimeAsync(REFRESH_DELAY_MS * 3);
    expect(matcher).toHaveBeenCalledTimes(1);
    highlighter.dispose();
    vi.useRealTimers();
  });

  it("复审 R3：输出不停时超时照样停掉高亮；同一时间只有一次匹配在跑", async () => {
    vi.useFakeTimers();
    const { terminal, write } = fakeTerminal(["aaaaaaaaaaaaaaaaaaaaaaaaaaaa!"], 0, 1);
    const onTimeout = vi.fn();
    const matcher = vi.fn(
      () => new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), MATCH_TIMEOUT_MS)),
    );
    const highlighter = attachHighlighter(terminal, compileRules([rule("(a+)+$", { regex: true })]), {
      matcher,
      onTimeout,
    });
    for (let elapsed = 0; elapsed < MATCH_TIMEOUT_MS * 3; elapsed += 20) {
      write();
      await vi.advanceTimersByTimeAsync(20);
    }
    expect(onTimeout).toHaveBeenCalledTimes(1);
    expect(matcher).toHaveBeenCalledTimes(1);
    highlighter.dispose();
    vi.useRealTimers();
  });
});

describe("单条规则的开关", () => {
  it("关掉的规则不参与匹配；旧版本存的规则没有 enabled 字段算开着", () => {
    const rules = compileRules([rule("error"), rule("warn", { enabled: false })]);
    expect(findMatches("error warn", rules).map((m) => m.start)).toEqual([0]);
    const parsed = parseRules(
      JSON.stringify([
        { id: "a", pattern: "x", color: "#ef4444" },
        { id: "b", pattern: "y", color: "#ef4444", enabled: false },
      ]),
    );
    expect(parsed.map((r) => r.enabled)).toEqual([true, false]);
  });
});

describe("浅色终端换深一号颜色", () => {
  it("预设色的文字规则换深色；涂背景和自定义颜色不换；深色终端不换", () => {
    const rules = [
      rule("warn", { color: "#f59e0b" }),
      rule("ok", { color: "#22C55E" }),
      rule("bg", { color: "#f59e0b", background: true }),
      rule("custom", { color: "#123456" }),
    ];
    expect(compileRules(rules, true).map((item) => item.color)).toEqual(["#b45309", "#15803d", "#f59e0b", "#123456"]);
    expect(compileRules(rules).map((item) => item.color)).toEqual(["#f59e0b", "#22C55E", "#f59e0b", "#123456"]);
  });
});
