// 高亮匹配的后台线程：用户写的正则在这里跑，写得不好（灾难性回溯）也只卡这个线程，
// 主线程等不到结果就把它整个结束掉（复审 F6）。

import { findMatches, type CompiledRule } from "./highlight";

export interface MatchRequest {
  id: number;
  lines: string[];
  rules: { source: string; flags: string; color: string; background: boolean }[];
  limit: number;
}

export interface MatchResponse {
  id: number;
  /** 每行的匹配：[开始, 结束, 第几条规则] */
  matches: [number, number, number][][];
}

let cacheKey = "";
let cached: CompiledRule[] = [];

self.onmessage = (event: MessageEvent<MatchRequest>) => {
  const { id, lines, rules, limit } = event.data;
  const key = JSON.stringify(rules);
  if (key !== cacheKey) {
    cacheKey = key;
    cached = rules.map((rule) => ({
      re: new RegExp(rule.source, rule.flags),
      color: rule.color,
      background: rule.background,
    }));
  }
  let budget = limit;
  const matches = lines.map((line) => {
    if (budget <= 0 || line.trim() === "") return [];
    const found = findMatches(line, cached, budget);
    budget -= found.length;
    return found.map((match) => [match.start, match.end, cached.indexOf(match.rule)] as [number, number, number]);
  });
  (self as unknown as Worker).postMessage({ id, matches } satisfies MatchResponse);
};
