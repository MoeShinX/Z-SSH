//! 皮肤：每一套都看得清（对比度），id 不重复；旧版本的设置能接上。

import { describe, expect, it } from "vitest";

import {
  activeSkin,
  normalizeTerminalTheme,
  resolveMode,
  terminalITheme,
} from "../stores/settings";
import {
  contrast,
  DARK_SKINS,
  hexToHslTriplet,
  LIGHT_SKINS,
  mixHex,
  onAccent,
  SKINS,
  skinTokens,
} from "./skins";

describe("皮肤", () => {
  it("id 不重复，浅色深色各自归类", () => {
    expect(new Set(SKINS.map((skin) => skin.id)).size).toBe(SKINS.length);
    expect(LIGHT_SKINS.every((skin) => skin.mode === "light")).toBe(true);
    expect(DARK_SKINS.every((skin) => skin.mode === "dark")).toBe(true);
    expect(LIGHT_SKINS[0].id).toBe("paper");
    expect(DARK_SKINS[0].id).toBe("graphite");
  });

  it.each(SKINS.map((skin) => [skin.id, skin] as const))("%s 的文字都看得清", (_id, skin) => {
    const { ui, terminal } = skin;
    expect(contrast(ui.text, ui.background), "正文").toBeGreaterThanOrEqual(4.5);
    expect(contrast(ui.text, ui.surface), "卡片上的正文").toBeGreaterThanOrEqual(4.5);
    expect(contrast(ui.textMuted, ui.background), "次要文字").toBeGreaterThanOrEqual(3);
    expect(contrast(onAccent(skin), ui.accent), "按钮上的字").toBeGreaterThanOrEqual(3);
    expect(contrast(terminal.foreground, terminal.background), "终端文字").toBeGreaterThanOrEqual(4);
    // 明暗要名副其实：深色皮肤背景比文字暗
    const darkBg = contrast(ui.background, "#000000") < contrast(ui.background, "#FFFFFF");
    expect(darkBg).toBe(skin.mode === "dark");
  });

  it("颜色换算", () => {
    expect(hexToHslTriplet("#FFFFFF")).toBe("0 0% 100%");
    expect(hexToHslTriplet("#005FB8")).toBe("209 100% 36%");
    expect(mixHex("#FFFFFF", "#000000", 0.5)).toBe("#808080");
    expect(skinTokens(LIGHT_SKINS[0]).background).toBe("0 0% 100%");
  });
});

describe("主题模式和终端配色", () => {
  const base = {
    ui_theme_mode: "system" as const,
    ui_skin_light: "solarized-light",
    ui_skin_dark: "nord",
    systemDark: true,
    terminal_theme: "skin",
  };

  it("跟随系统按系统明暗选皮肤；明暗对不上的皮肤 id 退回默认", () => {
    expect(resolveMode("system", true)).toBe("dark");
    expect(resolveMode("system", false)).toBe("light");
    expect(resolveMode("light", true)).toBe("light");
    expect(activeSkin(base).id).toBe("nord");
    expect(activeSkin({ ...base, systemDark: false }).id).toBe("solarized-light");
    expect(activeSkin({ ...base, ui_skin_dark: "paper" }).id).toBe("graphite");
  });

  it("终端配色默认跟随皮肤，单独选了就用选的", () => {
    expect(terminalITheme(base).background).toBe("#2E3440");
    expect(terminalITheme({ ...base, systemDark: false }).background).toBe("#FDF6E3");
    expect(terminalITheme({ ...base, terminal_theme: "dracula" }).background).toBe("#282A36");
  });

  it("旧版本的终端配色 id 一律跟随皮肤（不然升级后界面浅色、终端还是深色）", () => {
    for (const legacy of ["system-monokai-pro-spectrum-dark", "system-catppuccin-dark", "system-github-light"]) {
      expect(normalizeTerminalTheme(legacy)).toBe("skin");
    }
    expect(normalizeTerminalTheme("nonsense")).toBe("skin");
    expect(normalizeTerminalTheme("gruvbox-dark")).toBe("gruvbox-dark");
    expect(
      terminalITheme({ ...base, systemDark: false, terminal_theme: "system-monokai-pro-spectrum-dark" }).background,
    ).toBe("#FDF6E3");
  });
});
