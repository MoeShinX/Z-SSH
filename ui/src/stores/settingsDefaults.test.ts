/** @vitest-environment jsdom */
//! 0.2.7 起的新默认值：深色 / Dracula / 终端 Tokyo Night，关键词高亮默认开（整组五条规则也开）。
//! 只影响没存过这些设置的人：库里存过的值照旧，旧版本升级上来的明暗判断也不受影响。

import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  /** 假的设置表：key → JSON 文本。 */
  stored: new Map<string, string>(),
  settingsSet: vi.fn(async (_key: string, _value: string): Promise<void> => {}),
}));

vi.mock("../api/settings", () => ({
  settingsGet: vi.fn(async (key: string) => mocks.stored.get(key) ?? null),
  settingsSet: mocks.settingsSet,
}));

import { DEFAULT_DARK_SKIN, DEFAULT_TERMINAL_THEME, getSkin } from "../state/skins";
import { SAMPLE_RULES } from "../terminal/highlightSamples";
import { SETTING_DEFAULTS, activeSkin, normalizeTerminalTheme, terminalITheme, useSettings } from "./settings";

const store = () => useSettings.getState();

/** 回到「什么都没读过」的状态，再按假设置表读一遍。 */
async function loadFresh() {
  useSettings.setState({ ...SETTING_DEFAULTS, loaded: false, systemDark: false });
  await store().load();
}

beforeEach(() => {
  mocks.stored.clear();
  mocks.settingsSet.mockClear();
});

describe("新默认值", () => {
  it("默认值本身：深色 / Dracula / Tokyo Night / 高亮开", () => {
    expect(SETTING_DEFAULTS.ui_theme_mode).toBe("dark");
    expect(SETTING_DEFAULTS.ui_skin_dark).toBe("dracula");
    expect(DEFAULT_DARK_SKIN).toBe("dracula");
    expect(SETTING_DEFAULTS.terminal_theme).toBe("tokyo-night");
    expect(SETTING_DEFAULTS.terminal_highlight_enabled).toBe(true);
    // 浅色皮肤的默认不变
    expect(SETTING_DEFAULTS.ui_skin_light).toBe("paper");
  });

  it("终端默认主题就是设置页里叫「Tokyo Night」的那套深色皮肤配色", () => {
    expect(DEFAULT_TERMINAL_THEME).toBe("tokyo-night");
    const skin = getSkin(DEFAULT_TERMINAL_THEME);
    expect(skin?.name).toBe("Tokyo Night");
    expect(skin?.mode).toBe("dark");
    // 是能直接用的值，不会被当成旧版本的 id 换成「跟随皮肤」
    expect(normalizeTerminalTheme(DEFAULT_TERMINAL_THEME)).toBe("tokyo-night");
  });

  it("全新数据目录（库里什么都没有）：读出来是深色 / Dracula / Tokyo Night，即使系统是浅色", async () => {
    await loadFresh();
    expect(store().ui_theme_mode).toBe("dark");
    expect(store().ui_skin_dark).toBe("dracula");
    expect(store().terminal_theme).toBe("tokyo-night");
    expect(store().loaded).toBe(true);
    // 实际生效的界面皮肤和终端配色
    const state = store();
    expect(activeSkin(state).id).toBe("dracula");
    expect(terminalITheme(state).background).toBe(getSkin("tokyo-night")?.terminal.background);
    // 读默认值不会往库里写任何设置
    expect(mocks.settingsSet.mock.calls.filter(([key]) => key === "ui_theme_mode")).toEqual([]);
  });

  it("库里存过别的值的：原样保留", async () => {
    mocks.stored.set("ui_theme_mode", JSON.stringify("light"));
    mocks.stored.set("ui_skin_dark", JSON.stringify("graphite"));
    mocks.stored.set("ui_skin_light", JSON.stringify("solarized-light"));
    mocks.stored.set("terminal_theme", JSON.stringify("skin"));
    await loadFresh();
    expect(store().ui_theme_mode).toBe("light");
    expect(store().ui_skin_dark).toBe("graphite");
    expect(store().ui_skin_light).toBe("solarized-light");
    expect(store().terminal_theme).toBe("skin");
  });

  it("存过「跟随系统」的人仍然跟随系统", async () => {
    mocks.stored.set("ui_theme_mode", JSON.stringify("system"));
    await loadFresh();
    expect(store().ui_theme_mode).toBe("system");
  });

  it("只改过其中一项：其余仍是新默认", async () => {
    mocks.stored.set("ui_skin_dark", JSON.stringify("nord"));
    await loadFresh();
    expect(store().ui_skin_dark).toBe("nord");
    expect(store().ui_theme_mode).toBe("dark");
    expect(store().terminal_theme).toBe("tokyo-night");
  });

  it("老版本留下的界面主题：仍按它的明暗定模式，不被新默认盖掉", async () => {
    mocks.stored.set("ui_theme", JSON.stringify("snow"));
    await loadFresh();
    expect(store().ui_theme_mode).toBe("light");
  });

  it("同步 / 备份恢复带来的老设置：读出来就是它们自己的值", async () => {
    mocks.stored.set("ui_theme_mode", JSON.stringify("system"));
    mocks.stored.set("ui_skin_dark", JSON.stringify("graphite"));
    mocks.stored.set("terminal_theme", JSON.stringify("system-nord-midnight-dark"));
    mocks.stored.set("terminal_highlight_enabled", JSON.stringify(false));
    await loadFresh();
    expect(store().ui_theme_mode).toBe("system");
    expect(store().ui_skin_dark).toBe("graphite");
    // 旧版本的终端配色 id 一律换成「跟随皮肤」（既有规则），不是新的默认
    expect(store().terminal_theme).toBe("skin");
    expect(store().terminal_highlight_enabled).toBe(false);
  });
});

describe("关键词高亮的默认值", () => {
  it("全新数据目录：总开关开，整组五条规则都开", async () => {
    await loadFresh();
    expect(store().terminal_highlight_enabled).toBe(true);
    const rules = JSON.parse(store().terminal_highlight_rules) as { id: string; enabled: boolean }[];
    expect(rules.map((rule) => rule.id)).toEqual([
      "sample-error",
      "sample-warn",
      "sample-ok",
      "sample-ipv4",
      "sample-url",
    ]);
    expect(rules.every((rule) => rule.enabled)).toBe(true);
    expect(SAMPLE_RULES.every((rule) => rule.enabled)).toBe(true);
  });

  it("自己关过总开关的人保持关闭", async () => {
    mocks.stored.set("terminal_highlight_enabled", JSON.stringify(false));
    await loadFresh();
    expect(store().terminal_highlight_enabled).toBe(false);
  });

  it("存过自己的规则（含关掉的几条）的人原样保留", async () => {
    const mine = SAMPLE_RULES.map((rule, index) => ({ ...rule, enabled: index < 2 }));
    // 规则是「JSON 数组序列化成的字符串」，库里存的是再包一层 JSON
    mocks.stored.set("terminal_highlight_rules", JSON.stringify(JSON.stringify(mine)));
    await loadFresh();
    const rules = JSON.parse(store().terminal_highlight_rules) as { enabled: boolean }[];
    expect(rules.map((rule) => rule.enabled)).toEqual([true, true, false, false, false]);
  });
});
