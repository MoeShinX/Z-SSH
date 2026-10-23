//! 常用高亮规则（设置页和设置 store 都要用，放在不带界面的模块里）。

import type { HighlightRule } from "./highlight";

/**
 * 常用规则：新装默认就有这一整组，总开关默认开，五条规则（错误、警告、成功、IP、网址）也都默认开着，
 * 用户自己按需关。点「添加常用规则」只补还没有的（按 id 认）。
 * 前两条沿用最早的示例规则的 id，早先加过示例的用户不会多出重复的两条。
 * 顺序就是优先级：前面的规则标过的地方，后面的不再重叠着标。
 */
export const SAMPLE_RULES: HighlightRule[] = [
  {
    id: "sample-error",
    pattern: String.raw`\b(errors?|fail(ed|ure|s)?|fatal|denied|refused|panic|critical)\b`,
    regex: true,
    caseSensitive: false,
    color: "#ef4444",
    background: false,
    enabled: true,
  },
  {
    id: "sample-warn",
    pattern: String.raw`\b(warn(ings?)?|deprecated|timeout|timed out)\b`,
    regex: true,
    caseSensitive: false,
    color: "#f59e0b",
    background: false,
    enabled: true,
  },
  {
    id: "sample-ok",
    pattern: String.raw`\b(ok|success(ful|fully)?|succeeded|passed|active|running)\b`,
    regex: true,
    caseSensitive: false,
    color: "#22c55e",
    background: false,
    enabled: true,
  },
  {
    id: "sample-ipv4",
    pattern: String.raw`\b(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,2})?\b`,
    regex: true,
    caseSensitive: false,
    color: "#a855f7",
    background: false,
    enabled: true,
  },
  {
    id: "sample-url",
    pattern: String.raw`https?://[^\s'"<>]+`,
    regex: true,
    caseSensitive: false,
    color: "#3b82f6",
    background: false,
    enabled: true,
  },
];

/** 还没加进来的常用规则。 */
export function missingSamples(rules: HighlightRule[]): HighlightRule[] {
  const have = new Set(rules.map((rule) => rule.id));
  return SAMPLE_RULES.filter((rule) => !have.has(rule.id));
}

/** 最早版本自带的两条示例规则（原样没改过时，升级会换成上面整组）。 */
const LEGACY_SAMPLES = [
  { id: "sample-error", pattern: "error|fail(ed|ure)?|fatal|denied", color: "#ef4444" },
  { id: "sample-warn", pattern: "warn(ing)?", color: "#f59e0b" },
];

/**
 * 存着的规则是不是最早那两条示例、一点没改过：是就返回新的整组规则（JSON），否则 null。
 * 改过任何一处（模式、颜色、大小写、背景、开关、多了少了）都不动，保留用户自己的。
 */
export function upgradeLegacySamples(raw: string): string | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed) || parsed.length !== LEGACY_SAMPLES.length) return null;
  const untouched = LEGACY_SAMPLES.every((legacy, index) => {
    const rule = parsed[index] as Record<string, unknown> | null;
    return (
      typeof rule === "object" &&
      rule !== null &&
      rule.id === legacy.id &&
      rule.pattern === legacy.pattern &&
      typeof rule.color === "string" &&
      rule.color.toLowerCase() === legacy.color &&
      rule.regex === true &&
      !rule.caseSensitive &&
      !rule.background &&
      rule.enabled !== false
    );
  });
  return untouched ? JSON.stringify(SAMPLE_RULES) : null;
}
