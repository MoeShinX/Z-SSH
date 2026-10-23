//! 皮肤：一套界面配色 + 一套配套的终端配色。浅色、深色各有一组，分别记住选了哪个，
//! 按当前明暗（系统 / 浅色 / 深色）用其中一个。
//!
//! 色值取自各配色的原版色板（都是 MIT 许可）：纸白 / 石墨取自 VS Code 的 Light Modern /
//! Dark Modern，其余见每一项的注释。界面用到的几种颜色（卡片、悬停、选中底色等）由我们
//! 从这些色板推出来，不是照搬哪个应用调好的值；个别浅色皮肤的正文和强调色压暗了一点，
//! 让文字和按钮上的字达到对比度要求（skins.test.ts 会检查）。
//!
//! 同步校验（`crates/zssh-core/src/sync/settings.rs` 的 SKIN_IDS）有测试核对这里每行 `id: "…"`。

import type { ITheme } from "@xterm/xterm";

import type { UiThemeMode, UiThemeTokens } from "./uiThemes";

/** 终端的 16 色 + 前景 / 背景 / 光标 / 选区。 */
export interface TerminalPalette {
  background: string;
  foreground: string;
  cursor: string;
  selection: string;
  /** black red green yellow blue magenta cyan white，然后 8 个亮色 */
  ansi: [
    string, string, string, string, string, string, string, string,
    string, string, string, string, string, string, string, string,
  ];
}

export interface Skin {
  id: string;
  name: string;
  mode: UiThemeMode;
  ui: {
    /** 主区背景 */
    background: string;
    /** 卡片、弹出层、侧栏 */
    surface: string;
    /** 悬停、次要按钮、输入框底色 */
    subtle: string;
    border: string;
    text: string;
    textMuted: string;
    /** 强调色（按钮、选中、焦点框） */
    accent: string;
    danger: string;
  };
  terminal: TerminalPalette;
}

const ansi = (...colors: string[]) => colors as TerminalPalette["ansi"];

export const SKINS: Skin[] = [
  // ---------------- 浅色 ----------------
  {
    // VS Code Light Modern
    id: "paper",
    name: "纸白",
    mode: "light",
    ui: {
      background: "#FFFFFF",
      surface: "#F8F8F8",
      subtle: "#F0F0F0",
      border: "#E5E5E5",
      text: "#3B3B3B",
      textMuted: "#6E6E6E",
      accent: "#005FB8",
      danger: "#C72E2E",
    },
    terminal: {
      background: "#FFFFFF",
      foreground: "#3B3B3B",
      cursor: "#005FB8",
      selection: "#ADD6FF",
      ansi: ansi(
        "#000000", "#CD3131", "#107C10", "#949800", "#0451A5", "#BC05BC", "#0598BC", "#555555",
        "#666666", "#CD3131", "#14CE14", "#B5BA00", "#0451A5", "#BC05BC", "#0598BC", "#A5A5A5",
      ),
    },
  },
  {
    // Solarized Light（Ethan Schoonover）
    id: "solarized-light",
    name: "Solarized",
    mode: "light",
    ui: {
      background: "#FDF6E3",
      surface: "#EEE8D5",
      subtle: "#F3EDDA",
      border: "#E1DAC4",
      text: "#4E636A",
      textMuted: "#7B8B8E",
      accent: "#268BD2",
      danger: "#DC322F",
    },
    terminal: {
      background: "#FDF6E3",
      foreground: "#657B83",
      cursor: "#586E75",
      selection: "#EEE8D5",
      ansi: ansi(
        "#073642", "#DC322F", "#859900", "#B58900", "#268BD2", "#D33682", "#2AA198", "#EEE8D5",
        "#002B36", "#CB4B16", "#586E75", "#657B83", "#839496", "#6C71C4", "#93A1A1", "#FDF6E3",
      ),
    },
  },
  {
    // Ayu Light
    id: "ayu-light",
    name: "Ayu",
    mode: "light",
    ui: {
      background: "#FCFCFC",
      surface: "#F3F4F5",
      subtle: "#ECEEF0",
      border: "#E2E5E8",
      text: "#5C6166",
      textMuted: "#7D848B",
      accent: "#B86A00",
      danger: "#E65050",
    },
    terminal: {
      background: "#FCFCFC",
      foreground: "#5C6166",
      cursor: "#FFAA33",
      selection: "#D3E1F5",
      ansi: ansi(
        "#000000", "#EA6C6D", "#6CBF43", "#ECA944", "#3199E1", "#9E75C7", "#46BA94", "#BABABA",
        "#686868", "#F07171", "#86B300", "#F2AE49", "#399EE6", "#A37ACC", "#4CBF99", "#D1D1D1",
      ),
    },
  },
  {
    // GitHub Light（Primer）
    id: "github-light",
    name: "GitHub",
    mode: "light",
    ui: {
      background: "#FFFFFF",
      surface: "#F6F8FA",
      subtle: "#EFF2F5",
      border: "#D8DEE4",
      text: "#1F2328",
      textMuted: "#59636E",
      accent: "#0969DA",
      danger: "#CF222E",
    },
    terminal: {
      background: "#FFFFFF",
      foreground: "#1F2328",
      cursor: "#0969DA",
      selection: "#B6E3FF",
      ansi: ansi(
        "#24292F", "#CF222E", "#116329", "#4D2D00", "#0969DA", "#8250DF", "#1B7C83", "#6E7781",
        "#57606A", "#A40E26", "#1A7F37", "#633C01", "#218BFF", "#A475F9", "#3192AA", "#8C959F",
      ),
    },
  },
  {
    // Catppuccin Latte
    id: "catppuccin-latte",
    name: "Catppuccin Latte",
    mode: "light",
    ui: {
      background: "#EFF1F5",
      surface: "#E6E9EF",
      subtle: "#DCE0E8",
      border: "#CCD0DA",
      text: "#4C4F69",
      textMuted: "#6C6F85",
      accent: "#8839EF",
      danger: "#D20F39",
    },
    terminal: {
      background: "#EFF1F5",
      foreground: "#4C4F69",
      cursor: "#DC8A78",
      selection: "#CCD0DA",
      ansi: ansi(
        "#5C5F77", "#D20F39", "#40A02B", "#DF8E1D", "#1E66F5", "#EA76CB", "#179299", "#ACB0BE",
        "#6C6F85", "#D20F39", "#40A02B", "#DF8E1D", "#1E66F5", "#EA76CB", "#179299", "#BCC0CC",
      ),
    },
  },
  {
    // Rosé Pine Dawn
    id: "rose-pine-dawn",
    name: "Rosé Pine Dawn",
    mode: "light",
    ui: {
      background: "#FAF4ED",
      surface: "#FFFAF3",
      subtle: "#F2E9E1",
      border: "#DFDAD9",
      text: "#575279",
      textMuted: "#797593",
      accent: "#907AA9",
      danger: "#B4637A",
    },
    terminal: {
      background: "#FAF4ED",
      foreground: "#575279",
      cursor: "#575279",
      selection: "#DFDAD9",
      ansi: ansi(
        "#F2E9E1", "#B4637A", "#286983", "#EA9D34", "#56949F", "#907AA9", "#D7827E", "#575279",
        "#9893A5", "#B4637A", "#286983", "#EA9D34", "#56949F", "#907AA9", "#D7827E", "#575279",
      ),
    },
  },
  {
    // Everforest Light（medium）
    id: "everforest-light",
    name: "Everforest",
    mode: "light",
    ui: {
      background: "#FDF6E3",
      surface: "#F4F0D9",
      subtle: "#EFEBD4",
      border: "#E6E2CC",
      text: "#5C6A72",
      textMuted: "#7A8478",
      accent: "#748A00",
      danger: "#F85552",
    },
    terminal: {
      background: "#FDF6E3",
      foreground: "#5C6A72",
      cursor: "#5C6A72",
      selection: "#E6E2CC",
      ansi: ansi(
        "#5C6A72", "#F85552", "#8DA101", "#DFA000", "#3A94C5", "#DF69BA", "#35A77C", "#DFDDC8",
        "#939F91", "#F85552", "#8DA101", "#DFA000", "#3A94C5", "#DF69BA", "#35A77C", "#F3EAD3",
      ),
    },
  },

  // ---------------- 深色 ----------------
  {
    // VS Code Dark Modern
    id: "graphite",
    name: "石墨",
    mode: "dark",
    ui: {
      background: "#1F1F1F",
      surface: "#181818",
      subtle: "#2A2D2E",
      border: "#2B2B2B",
      text: "#CCCCCC",
      textMuted: "#9D9D9D",
      accent: "#0078D4",
      danger: "#F14C4C",
    },
    terminal: {
      background: "#1F1F1F",
      foreground: "#CCCCCC",
      cursor: "#AEAFAD",
      selection: "#264F78",
      ansi: ansi(
        "#000000", "#CD3131", "#0DBC79", "#E5E510", "#2472C8", "#BC3FBC", "#11A8CD", "#E5E5E5",
        "#666666", "#F14C4C", "#23D18B", "#F5F543", "#3B8EEA", "#D670D6", "#29B8DB", "#E5E5E5",
      ),
    },
  },
  {
    // Tokyo Night
    id: "tokyo-night",
    name: "Tokyo Night",
    mode: "dark",
    ui: {
      background: "#1A1B26",
      surface: "#16161E",
      subtle: "#292E42",
      border: "#2A2D3E",
      text: "#C0CAF5",
      textMuted: "#8A92BC",
      accent: "#7AA2F7",
      danger: "#F7768E",
    },
    terminal: {
      background: "#1A1B26",
      foreground: "#C0CAF5",
      cursor: "#C0CAF5",
      selection: "#283457",
      ansi: ansi(
        "#15161E", "#F7768E", "#9ECE6A", "#E0AF68", "#7AA2F7", "#BB9AF7", "#7DCFFF", "#A9B1D6",
        "#414868", "#F7768E", "#9ECE6A", "#E0AF68", "#7AA2F7", "#BB9AF7", "#7DCFFF", "#C0CAF5",
      ),
    },
  },
  {
    // Dracula
    id: "dracula",
    name: "Dracula",
    mode: "dark",
    ui: {
      background: "#282A36",
      surface: "#21222C",
      subtle: "#343746",
      border: "#3A3D4E",
      text: "#F8F8F2",
      textMuted: "#A9ABC2",
      accent: "#BD93F9",
      danger: "#FF5555",
    },
    terminal: {
      background: "#282A36",
      foreground: "#F8F8F2",
      cursor: "#F8F8F2",
      selection: "#44475A",
      ansi: ansi(
        "#21222C", "#FF5555", "#50FA7B", "#F1FA8C", "#BD93F9", "#FF79C6", "#8BE9FD", "#F8F8F2",
        "#6272A4", "#FF6E6E", "#69FF94", "#FFFFA5", "#D6ACFF", "#FF92DF", "#A4FFFF", "#FFFFFF",
      ),
    },
  },
  {
    // Catppuccin Mocha
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    mode: "dark",
    ui: {
      background: "#1E1E2E",
      surface: "#181825",
      subtle: "#313244",
      border: "#313244",
      text: "#CDD6F4",
      textMuted: "#A6ADC8",
      accent: "#CBA6F7",
      danger: "#F38BA8",
    },
    terminal: {
      background: "#1E1E2E",
      foreground: "#CDD6F4",
      cursor: "#F5E0DC",
      selection: "#45475A",
      ansi: ansi(
        "#45475A", "#F38BA8", "#A6E3A1", "#F9E2AF", "#89B4FA", "#F5C2E7", "#94E2D5", "#BAC2DE",
        "#585B70", "#F38BA8", "#A6E3A1", "#F9E2AF", "#89B4FA", "#F5C2E7", "#94E2D5", "#A6ADC8",
      ),
    },
  },
  {
    // Nord（Arctic Ice Studio）
    id: "nord",
    name: "Nord",
    mode: "dark",
    ui: {
      background: "#2E3440",
      surface: "#292E39",
      subtle: "#3B4252",
      border: "#3B4252",
      text: "#E5E9F0",
      textMuted: "#A3ABBA",
      accent: "#88C0D0",
      danger: "#BF616A",
    },
    terminal: {
      background: "#2E3440",
      foreground: "#D8DEE9",
      cursor: "#D8DEE9",
      selection: "#434C5E",
      ansi: ansi(
        "#3B4252", "#BF616A", "#A3BE8C", "#EBCB8B", "#81A1C1", "#B48EAD", "#88C0D0", "#E5E9F0",
        "#4C566A", "#BF616A", "#A3BE8C", "#EBCB8B", "#81A1C1", "#B48EAD", "#8FBCBB", "#ECEFF4",
      ),
    },
  },
  {
    // Gruvbox Dark（morhetz）
    id: "gruvbox-dark",
    name: "Gruvbox",
    mode: "dark",
    ui: {
      background: "#282828",
      surface: "#1D2021",
      subtle: "#3C3836",
      border: "#3C3836",
      text: "#EBDBB2",
      textMuted: "#A89984",
      accent: "#FABD2F",
      danger: "#FB4934",
    },
    terminal: {
      background: "#282828",
      foreground: "#EBDBB2",
      cursor: "#EBDBB2",
      selection: "#504945",
      ansi: ansi(
        "#282828", "#CC241D", "#98971A", "#D79921", "#458588", "#B16286", "#689D6A", "#A89984",
        "#928374", "#FB4934", "#B8BB26", "#FABD2F", "#83A598", "#D3869B", "#8EC07C", "#EBDBB2",
      ),
    },
  },
  {
    // Rosé Pine
    id: "rose-pine",
    name: "Rosé Pine",
    mode: "dark",
    ui: {
      background: "#191724",
      surface: "#1F1D2E",
      subtle: "#26233A",
      border: "#2A2739",
      text: "#E0DEF4",
      textMuted: "#908CAA",
      accent: "#C4A7E7",
      danger: "#EB6F92",
    },
    terminal: {
      background: "#191724",
      foreground: "#E0DEF4",
      cursor: "#E0DEF4",
      selection: "#403D52",
      ansi: ansi(
        "#26233A", "#EB6F92", "#31748F", "#F6C177", "#9CCFD8", "#C4A7E7", "#EBBCBA", "#E0DEF4",
        "#6E6A86", "#EB6F92", "#31748F", "#F6C177", "#9CCFD8", "#C4A7E7", "#EBBCBA", "#E0DEF4",
      ),
    },
  },
  {
    // Kanagawa Wave（rebelot）
    id: "kanagawa",
    name: "Kanagawa",
    mode: "dark",
    ui: {
      background: "#1F1F28",
      surface: "#16161D",
      subtle: "#2A2A37",
      border: "#2A2A37",
      text: "#DCD7BA",
      textMuted: "#A09E8E",
      accent: "#7E9CD8",
      danger: "#E46876",
    },
    terminal: {
      background: "#1F1F28",
      foreground: "#DCD7BA",
      cursor: "#C8C093",
      selection: "#2D4F67",
      ansi: ansi(
        "#090618", "#C34043", "#76946A", "#C0A36E", "#7E9CD8", "#957FB8", "#6A9589", "#C8C093",
        "#727169", "#E82424", "#98BB6C", "#E6C384", "#7FB4CA", "#938AA9", "#7AA89F", "#DCD7BA",
      ),
    },
  },
  {
    // Everforest Dark（medium）
    id: "everforest-dark",
    name: "Everforest",
    mode: "dark",
    ui: {
      background: "#2D353B",
      surface: "#232A2E",
      subtle: "#343F44",
      border: "#3D484D",
      text: "#D3C6AA",
      textMuted: "#9DA9A0",
      accent: "#A7C080",
      danger: "#E67E80",
    },
    terminal: {
      background: "#2D353B",
      foreground: "#D3C6AA",
      cursor: "#D3C6AA",
      selection: "#475258",
      ansi: ansi(
        "#414B50", "#E67E80", "#A7C080", "#DBBC7F", "#7FBBB3", "#D699B6", "#83C092", "#D3C6AA",
        "#859289", "#E67E80", "#A7C080", "#DBBC7F", "#7FBBB3", "#D699B6", "#83C092", "#D3C6AA",
      ),
    },
  },
];

export const LIGHT_SKINS = SKINS.filter((skin) => skin.mode === "light");
export const DARK_SKINS = SKINS.filter((skin) => skin.mode === "dark");
export const DEFAULT_LIGHT_SKIN = "paper";
export const DEFAULT_DARK_SKIN = "dracula";

/** 某个明暗下的皮肤；id 不认识（或明暗对不上）就用这个明暗的默认皮肤。 */
export function skinFor(mode: UiThemeMode, id: string): Skin {
  const list = mode === "dark" ? DARK_SKINS : LIGHT_SKINS;
  return list.find((skin) => skin.id === id) ?? list[0];
}

export function getSkin(id: string): Skin | undefined {
  return SKINS.find((skin) => skin.id === id);
}

// ---------------- 颜色换算 ----------------

function parseHex(hex: string): [number, number, number] {
  const value = hex.replace("#", "");
  return [0, 2, 4].map((i) => parseInt(value.slice(i, i + 2), 16)) as [number, number, number];
}

function toHex([r, g, b]: [number, number, number]): string {
  return `#${[r, g, b].map((c) => Math.round(c).toString(16).padStart(2, "0")).join("")}`.toUpperCase();
}

/** `#RRGGBB` → Tailwind 用的 HSL 三元组字符串（"220 28% 10%"）。 */
export function hexToHslTriplet(hex: string): string {
  const [r, g, b] = parseHex(hex).map((c) => c / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
  }
  return `${Math.round(h)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%`;
}

/** 两种颜色按比例混合：`amount` 是 `top` 占的比例。 */
export function mixHex(top: string, bottom: string, amount: number): string {
  const a = parseHex(top);
  const b = parseHex(bottom);
  return toHex(a.map((c, i) => c * amount + b[i] * (1 - amount)) as [number, number, number]);
}

function luminance(hex: string): number {
  const [r, g, b] = parseHex(hex).map((c) => {
    const v = c / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** 填在强调色上的文字：白字和皮肤里的深色哪个看得清用哪个。 */
export function onAccent(skin: Skin): string {
  const ink = skin.mode === "dark" ? skin.ui.surface : skin.ui.text;
  return contrast("#FFFFFF", skin.ui.accent) >= contrast(ink, skin.ui.accent) ? "#FFFFFF" : ink;
}

/** 皮肤 → 界面的 CSS 变量取值。 */
export function skinTokens(skin: Skin): UiThemeTokens {
  const { ui } = skin;
  const hsl = hexToHslTriplet;
  // 选中项 / 悬停的浅底色：强调色淡淡地叠在背景上
  const accentSoft = mixHex(ui.accent, ui.background, skin.mode === "dark" ? 0.22 : 0.12);
  return {
    background: hsl(ui.background),
    foreground: hsl(ui.text),
    card: hsl(ui.surface),
    cardForeground: hsl(ui.text),
    popover: hsl(ui.surface),
    popoverForeground: hsl(ui.text),
    primary: hsl(ui.accent),
    primaryForeground: hsl(onAccent(skin)),
    secondary: hsl(ui.subtle),
    secondaryForeground: hsl(ui.text),
    muted: hsl(ui.subtle),
    mutedForeground: hsl(ui.textMuted),
    accent: hsl(accentSoft),
    accentForeground: hsl(ui.text),
    destructive: hsl(ui.danger),
    destructiveForeground: hsl("#FFFFFF"),
    border: hsl(ui.border),
    input: hsl(ui.border),
    ring: hsl(ui.accent),
  };
}

/** 皮肤配套的终端配色 → xterm 的主题。 */
export function paletteToXterm(palette: TerminalPalette): ITheme {
  const [black, red, green, yellow, blue, magenta, cyan, white, ...bright] = palette.ansi;
  const [brightBlack, brightRed, brightGreen, brightYellow, brightBlue, brightMagenta, brightCyan, brightWhite] =
    bright;
  return {
    background: palette.background,
    foreground: palette.foreground,
    cursor: palette.cursor,
    cursorAccent: palette.background,
    selectionBackground: palette.selection,
    black,
    red,
    green,
    yellow,
    blue,
    magenta,
    cyan,
    white,
    brightBlack,
    brightRed,
    brightGreen,
    brightYellow,
    brightBlue,
    brightMagenta,
    brightCyan,
    brightWhite,
  };
}

/** 旧版本的界面主题 id → 明暗（升级时决定主题模式用）。 */
export const LEGACY_UI_THEME_MODES: Record<string, UiThemeMode> = {
  snow: "light",
  mist: "light",
  "pure-black": "dark",
  midnight: "dark",
  "deep-blue": "dark",
  graphite: "dark",
  obsidian: "dark",
};

/** 终端配色设置的特殊值：跟随当前皮肤。 */
export const TERMINAL_FOLLOW_SKIN = "skin";

/** 终端配色的默认值：Tokyo Night（深色）。设置页下拉里叫「Tokyo Night（深色）」的那一项。 */
export const DEFAULT_TERMINAL_THEME = "tokyo-night";
