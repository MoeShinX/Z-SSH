//! 常用规则：默认总开关和整组五条都开；最早两条示例原样没改过时升级成整组，改过的不动。

import { describe, expect, it } from "vitest";

import { SETTING_DEFAULTS } from "../stores/settings";
import { SAMPLE_RULES, upgradeLegacySamples } from "./highlightSamples";

const legacy = [
  { id: "sample-error", pattern: "error|fail(ed|ure)?|fatal|denied", regex: true, caseSensitive: false, color: "#ef4444", background: false },
  { id: "sample-warn", pattern: "warn(ing)?", regex: true, caseSensitive: false, color: "#f59e0b", background: false },
];

describe("常用高亮规则", () => {
  it("新装默认带整组，总开关和五条规则都开着", () => {
    const rules = JSON.parse(SETTING_DEFAULTS.terminal_highlight_rules) as { id: string; enabled: boolean }[];
    expect(rules.map((rule) => rule.id)).toEqual(SAMPLE_RULES.map((rule) => rule.id));
    expect(rules.map((rule) => rule.enabled)).toEqual([true, true, true, true, true]);
    expect(SETTING_DEFAULTS.terminal_highlight_enabled).toBe(true);
  });

  it("最早两条示例原样：升级成整组", () => {
    expect(upgradeLegacySamples(JSON.stringify(legacy))).toBe(JSON.stringify(SAMPLE_RULES));
    // 更早的存法没有 caseSensitive / background / enabled 字段，也算原样
    const bare = legacy.map(({ id, pattern, regex, color }) => ({ id, pattern, regex, color }));
    expect(upgradeLegacySamples(JSON.stringify(bare))).toBe(JSON.stringify(SAMPLE_RULES));
  });

  it("改过、多了、少了、坏了都不动", () => {
    const changed = (patch: Record<string, unknown>) =>
      JSON.stringify([{ ...legacy[0], ...patch }, legacy[1]]);
    expect(upgradeLegacySamples(changed({ color: "#ffffff" }))).toBeNull();
    expect(upgradeLegacySamples(changed({ pattern: "error" }))).toBeNull();
    expect(upgradeLegacySamples(changed({ background: true }))).toBeNull();
    expect(upgradeLegacySamples(changed({ enabled: false }))).toBeNull();
    expect(upgradeLegacySamples(JSON.stringify([legacy[0]]))).toBeNull();
    expect(upgradeLegacySamples(JSON.stringify([...legacy, { ...legacy[0], id: "x" }]))).toBeNull();
    expect(upgradeLegacySamples("[]")).toBeNull();
    expect(upgradeLegacySamples("not json")).toBeNull();
  });
});
