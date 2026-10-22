//! 终端关键词 / 正则高亮。
//!
//! 只标**屏幕上看得见的那几十行**，用 xterm 的 decoration 叠一层颜色，不改输出流：
//! 以前在输出流里插 ANSI 颜色的做法要逐字节改写，大量输出时把界面卡死了，已经撤掉。
//! 每次输出、滚动、改尺寸后合并成一次（最多每 {@link REFRESH_DELAY_MS} 一次）重新扫描。
//! 备用屏（vim、less、top）不标：xterm 的 marker 只能挂在普通屏上。
//!
//! 正则在后台线程里跑（highlightWorker.ts）：用户写的正则可能灾难性回溯（`(a+)+$` 这种），
//! 在主线程跑会把整个界面卡死，同步的 `RegExp.exec` 也没法从外面打断。后台线程
//! {@link MATCH_TIMEOUT_MS} 内没答复就结束掉它，高亮停下并告诉用户（复审 F6）。

import type { IBufferLine, IDecoration, IDisposable, IMarker, Terminal } from "@xterm/xterm";

export interface HighlightRule {
  id: string;
  pattern: string;
  /** true = 按正则；false = 按字面文字 */
  regex: boolean;
  caseSensitive: boolean;
  /** `#RRGGBB` */
  color: string;
  /** true = 涂背景（文字变黑）；false = 只改文字颜色 */
  background: boolean;
  /** 这条规则开着没有（总开关之外，每条也能单独关）。旧版本存的规则没有这个字段，算开着。 */
  enabled: boolean;
}

export interface CompiledRule {
  re: RegExp;
  color: string;
  background: boolean;
}

/** 一次匹配（一屏）最多等后台线程这么久；超过就当这条正则卡住了。 */
export const MATCH_TIMEOUT_MS = 250;

/** 一屏文字 → 每行的匹配（[开始, 结束, 第几条规则]）。`"timeout"` 表示匹配太慢被停掉了。 */
export type Matcher = (
  lines: string[],
  rules: CompiledRule[],
  limit: number,
) => Promise<[number, number, number][][] | "timeout">;

/** 就在当前线程匹配（没有 Worker 的环境——测试——用它；没有超时保护）。 */
export const inlineMatcher: Matcher = async (lines, rules, limit) => {
  let budget = limit;
  return lines.map((line) => {
    if (budget <= 0 || line.trim() === "") return [];
    const found = findMatches(line, rules, budget);
    budget -= found.length;
    return found.map((match) => [match.start, match.end, rules.indexOf(match.rule)] as [number, number, number]);
  });
};

/** 后台线程匹配：一个共用的 Worker，超时就结束它，下次用时再起一个新的。 */
function createWorkerMatcher(): Matcher {
  let worker: Worker | null = null;
  let nextId = 0;
  const start = () =>
    new Worker(new URL("./highlightWorker.ts", import.meta.url), { type: "module" });
  return (lines, rules, limit) =>
    new Promise((resolve) => {
      worker ??= start();
      const current = worker;
      const id = (nextId += 1);
      const timer = setTimeout(() => {
        current.terminate();
        if (worker === current) worker = null;
        resolve("timeout");
      }, MATCH_TIMEOUT_MS);
      const onMessage = (event: MessageEvent<{ id: number; matches: [number, number, number][][] }>) => {
        if (event.data.id !== id) return;
        clearTimeout(timer);
        current.removeEventListener("message", onMessage);
        resolve(event.data.matches);
      };
      current.addEventListener("message", onMessage);
      current.postMessage({
        id,
        lines,
        rules: rules.map((rule) => ({
          source: rule.re.source,
          flags: rule.re.flags,
          color: rule.color,
          background: rule.background,
        })),
        limit,
      });
    });
}

let sharedMatcher: Matcher | null = null;

/** 默认的匹配方式：有 Worker 用后台线程，没有（测试环境）就在当前线程。 */
export function defaultMatcher(): Matcher {
  if (typeof Worker === "undefined") return inlineMatcher;
  sharedMatcher ??= createWorkerMatcher();
  return sharedMatcher;
}

export interface Match {
  start: number;
  end: number;
  rule: CompiledRule;
}

/** 合并刷新的间隔：大量输出时一秒最多扫十来次。 */
export const REFRESH_DELAY_MS = 80;
/** 一屏最多标多少处：防止 `.` 这种规则把一屏几千个格子都变成 decoration。 */
export const MAX_MATCHES_PER_PASS = 300;
export const MAX_RULES = 50;
const COLOR_RE = /^#[0-9a-fA-F]{6}$/;

/** 预设的几种颜色（终端深色、浅色背景下都看得清）。 */
export const HIGHLIGHT_COLORS = [
  { value: "#ef4444", label: "红" },
  { value: "#f59e0b", label: "橙" },
  { value: "#eab308", label: "黄" },
  { value: "#22c55e", label: "绿" },
  { value: "#06b6d4", label: "青" },
  { value: "#3b82f6", label: "蓝" },
  { value: "#a855f7", label: "紫" },
];

/**
 * 浅色终端上用的深一号颜色：预设色是按深色底挑的，黄、绿、橙放在白底上对比度只有 2 左右，看不清。
 * 只换预设色、只换"改文字颜色"的规则；涂背景的规则字是黑的，在哪种底上都清楚，不换。
 */
export const LIGHT_TERMINAL_COLORS: Record<string, string> = {
  "#ef4444": "#b91c1c",
  "#f59e0b": "#b45309",
  "#eab308": "#a16207",
  "#22c55e": "#15803d",
  "#06b6d4": "#0e7490",
  "#3b82f6": "#1d4ed8",
  "#a855f7": "#7e22ce",
};

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 设置里存的 JSON → 规则列表。格式不对的条目丢掉，不报错（设置可能是旧版本写的）。 */
export function parseRules(raw: string): HighlightRule[] {
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(data)) return [];
  const rules: HighlightRule[] = [];
  for (const item of data.slice(0, MAX_RULES)) {
    if (typeof item !== "object" || item === null) continue;
    const record = item as Record<string, unknown>;
    if (typeof record.pattern !== "string" || record.pattern === "") continue;
    const color = typeof record.color === "string" && COLOR_RE.test(record.color) ? record.color : "#ef4444";
    rules.push({
      id: typeof record.id === "string" ? record.id : `rule-${rules.length}`,
      pattern: record.pattern,
      regex: record.regex === true,
      caseSensitive: record.caseSensitive === true,
      color,
      background: record.background === true,
      enabled: record.enabled !== false,
    });
  }
  return rules;
}

/** 正则写错时返回错误说明，写对返回 null（设置页实时提示用）。 */
export function ruleError(rule: Pick<HighlightRule, "pattern" | "regex">): string | null {
  if (rule.pattern === "") return "不能为空";
  if (!rule.regex) return null;
  try {
    const re = new RegExp(rule.pattern, "g");
    if (re.test("")) return "这个正则能匹配空字符串，会标满整屏";
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

/** 编译规则；写错的正则、能匹配空串的正则跳过。 */
export function compileRules(rules: HighlightRule[], lightTerminal = false): CompiledRule[] {
  const compiled: CompiledRule[] = [];
  for (const rule of rules) {
    if (rule.enabled === false || ruleError(rule) !== null) continue;
    const source = rule.regex ? rule.pattern : escapeRegExp(rule.pattern);
    compiled.push({
      re: new RegExp(source, rule.caseSensitive ? "g" : "gi"),
      color: lightTerminal && !rule.background
        ? LIGHT_TERMINAL_COLORS[rule.color.toLowerCase()] ?? rule.color
        : rule.color,
      background: rule.background,
    });
  }
  return compiled;
}

/** 一行里的匹配。规则按顺序优先：前面的规则标过的地方，后面的规则不再重叠着标。 */
export function findMatches(text: string, rules: CompiledRule[], limit = MAX_MATCHES_PER_PASS): Match[] {
  const taken: Match[] = [];
  for (const rule of rules) {
    rule.re.lastIndex = 0;
    let found: RegExpExecArray | null;
    while ((found = rule.re.exec(text)) !== null) {
      const start = found.index;
      const end = start + found[0].length;
      if (end === start) {
        rule.re.lastIndex += 1;
        continue;
      }
      if (!taken.some((other) => start < other.end && other.start < end)) {
        taken.push({ start, end, rule });
        if (taken.length >= limit) return taken;
      }
    }
  }
  return taken.sort((a, b) => a.start - b.start);
}

/**
 * 一行缓冲 → 文字 + 每个字符（UTF-16 下标）对应的列。
 * 中文等宽字符占两列，第二列是宽度 0 的占位格，要跳过，否则列号会错位。
 */
export function lineText(line: IBufferLine, cols: number): { text: string; columns: number[] } {
  let text = "";
  const columns: number[] = [];
  for (let x = 0; x < cols; x += 1) {
    const cell = line.getCell(x);
    if (!cell) break;
    if (cell.getWidth() === 0) continue;
    const chars = cell.getChars() || " ";
    for (let i = 0; i < chars.length; i += 1) columns.push(x);
    text += chars;
  }
  columns.push(cols);
  return { text, columns };
}

export interface Highlighter extends IDisposable {
  /** 规则变了：换规则并马上重扫。传空数组等于关掉。 */
  setRules(rules: CompiledRule[]): void;
}

export function attachHighlighter(
  terminal: Terminal,
  initial: CompiledRule[],
  options: {
    matcher?: Matcher;
    /** 匹配太慢被停掉了（高亮已经停下）：界面提示用户改规则。 */
    onTimeout?: () => void;
  } = {},
): Highlighter {
  const matcher = options.matcher ?? defaultMatcher();
  let rules = initial;
  let marks: { marker: IMarker; decoration: IDecoration | undefined }[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;
  let scannedViewport = -1;
  /** 每次扫描加一：结果回来时已经有更新的扫描了就丢掉。 */
  let generation = 0;
  let disposed = false;
  /** 同一时间只让一次匹配在跑；跑的时候又要扫，就记一笔，跑完再扫最新的一屏。 */
  let inFlight = false;
  let rescan = false;

  const clear = () => {
    for (const mark of marks) {
      mark.decoration?.dispose();
      mark.marker.dispose();
    }
    marks = [];
  };

  const scan = () => {
    timer = null;
    if (rules.length === 0) {
      generation += 1;
      clear();
      return;
    }
    if (inFlight) {
      rescan = true;
      return;
    }
    const ticket = (generation += 1);
    const buffer = terminal.buffer.active;
    scannedViewport = buffer.viewportY;
    if (buffer.type !== "normal") {
      clear();
      return;
    }
    const rows: { y: number; text: string; columns: number[] }[] = [];
    for (let row = 0; row < terminal.rows; row += 1) {
      const y = buffer.viewportY + row;
      const line = buffer.getLine(y);
      if (!line) continue;
      const { text, columns } = lineText(line, terminal.cols);
      rows.push({ y, text, columns });
    }
    const current = rules;
    inFlight = true;
    void matcher(
      rows.map((row) => row.text),
      current,
      MAX_MATCHES_PER_PASS,
    )
      .catch(() => [] as [number, number, number][][])
      .then((result) => {
        inFlight = false;
        if (disposed) return;
        if (result === "timeout") {
          // 这组规则会卡住：停掉高亮，别每次输出都再卡 250ms。不看 ticket——输出不停时
          // 总有更新的扫描，看 ticket 的话超时永远被吞掉。规则已经换过就不怪新规则。
          if (current === rules) {
            rules = [];
            rescan = false;
            clear();
            options.onTimeout?.();
            return;
          }
        } else if (ticket === generation && current === rules) {
          apply(result, rows, current);
        }
        if (rescan) {
          rescan = false;
          schedule();
        }
      });
  };

  const apply = (
    result: [number, number, number][][],
    rows: { y: number; text: string; columns: number[] }[],
    current: CompiledRule[],
  ) => {
    clear();
    const cursorLine = terminal.buffer.active.baseY + terminal.buffer.active.cursorY;
    result.forEach((matches, index) => {
      const { y, columns } = rows[index];
      for (const [start, end, ruleIndex] of matches) {
        const rule = current[ruleIndex];
        if (!rule) continue;
        const marker = terminal.registerMarker(y - cursorLine);
        if (!marker) continue;
        const x = columns[start] ?? 0;
        const endColumn = columns[end] ?? terminal.cols;
        const decoration = terminal.registerDecoration({
          marker,
          x,
          width: Math.max(1, endColumn - x),
          layer: "bottom",
          ...(rule.background
            ? { backgroundColor: rule.color, foregroundColor: "#000000" }
            : { foregroundColor: rule.color }),
        });
        marks.push({ marker, decoration });
      }
    });
  };

  const schedule = () => {
    if (rules.length === 0 && marks.length === 0) return;
    if (timer === null) timer = setTimeout(scan, REFRESH_DELAY_MS);
  };

  const subscriptions = [
    terminal.onWriteParsed(schedule),
    terminal.onScroll(schedule),
    terminal.onResize(schedule),
    terminal.buffer.onBufferChange(schedule),
  ];
  // 滚轮翻回滚缓冲时 onScroll 不一定触发，用渲染事件兜底；但只认"视口真的动了"，
  // 否则 decoration 自己引起的重绘、光标闪烁都会触发重扫，空闲时也在白转
  subscriptions.push(
    terminal.onRender(() => {
      if (terminal.buffer.active.viewportY !== scannedViewport) schedule();
    }),
  );
  schedule();

  return {
    setRules(next) {
      rules = next;
      if (timer !== null) clearTimeout(timer);
      scan();
    },
    dispose() {
      disposed = true;
      if (timer !== null) clearTimeout(timer);
      timer = null;
      for (const subscription of subscriptions) subscription.dispose();
      clear();
    },
  };
}
