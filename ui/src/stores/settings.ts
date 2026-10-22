//! 设置：外观 / 终端 / 连接三组，全部存进后端的 settings 表（值是 JSON）。
//!
//! 改一个值就把新值写回库里，同时立刻生效：界面主题往 <html> 写 CSS 变量，
//! 终端相关的设置在 TerminalView 里跟着 store 变化改 xterm 的 options，
//! 连接池的空闲时长和 keepalive 由后端读同一张表（见 settings_set 命令）。
import { useMemo } from "react";
import { create } from "zustand";

import { settingsGet, settingsSet } from "../api/settings";
import { toast } from "../components/ui/toast";
import {
  DEFAULT_DARK_SKIN,
  DEFAULT_LIGHT_SKIN,
  getSkin,
  LEGACY_UI_THEME_MODES,
  paletteToXterm,
  skinFor,
  skinTokens,
  TERMINAL_FOLLOW_SKIN,
  type Skin,
} from "../state/skins";
import { parseCloseAction, type CloseAction } from "../state/closeBehavior";
import { applyUiTokens, type UiThemeMode } from "../state/uiThemes";
import { SAMPLE_RULES, upgradeLegacySamples } from "../terminal/highlightSamples";
import type { ITheme } from "@xterm/xterm";

/** 每项设置的名字、默认值和类型（类型也用来校验从库里读回来的值）。 */
export const SETTING_DEFAULTS = {
  // 外观：主题（系统 / 浅色 / 深色）+ 浅色、深色各一个皮肤；终端配色默认跟随皮肤
  ui_theme_mode: "system",
  ui_skin_light: DEFAULT_LIGHT_SKIN,
  ui_skin_dark: DEFAULT_DARK_SKIN,
  terminal_theme: TERMINAL_FOLLOW_SKIN,
  // 终端
  terminal_font_family: "JetBrains Mono, Cascadia Mono, Consolas, monospace",
  terminal_font_size: 12,
  terminal_line_height: 1.3,
  terminal_scrollback: 10000,
  terminal_cursor_style: "block",
  terminal_cursor_blink: true,
  terminal_copy_on_select: false,
  // 焦点进终端时把输入法切到英文（离开终端恢复）；只是切模式，按 Shift 照样能打中文
  terminal_ime_english: true,
  terminal_right_click_paste: false,
  terminal_confirm_multiline_paste: true,
  // 关键词 / 正则高亮：默认关；规则是 JSON 数组字符串（terminal/highlight.ts）
  terminal_highlight_enabled: false,
  // 新装默认带常用规则（只有错误、警告两条开着），总开关仍然默认关
  terminal_highlight_rules: JSON.stringify(SAMPLE_RULES),
  // 会话日志：默认关；目录留空 = 数据目录下的 session-logs
  session_log_enabled: false,
  session_log_dir: "",
  // 窗口（UI-4）：关闭主窗口时缩到托盘（tray，默认）还是退出程序（quit）；本机习惯，不同步。
  // tray_hint_shown：第一次缩到托盘的提示出过了（本机状态，不同步）
  close_action: "tray",
  tray_hint_shown: false,
  // 连接
  pool_idle_timeout_secs: 60,
  keepalive_interval_secs: 30,
  // 服务器状态栏（M1-10）：默认开，3 秒一刷（1～30 秒）
  stats_show: true,
  stats_refresh_secs: 3,
  // AI（AI-2）：执行模式默认手动；opencode 路径留空表示自动发现
  ai_exec_mode: "manual",
  ai_opencode_path: "",
  // AI-3：codex 路径（同样留空表示自动发现）与默认智能体（默认 opencode）
  ai_codex_path: "",
  ai_default_agent: "opencode",
  // AI-4：Claude Code 路径（留空表示自动发现）
  ai_claude_path: "",
  // M3-4 第 0 节：write_file 覆盖时每个文件保留几份 .zssh-bak-* 备份
  ai_write_backup_keep: 5,
  // SFTP（M3-2 第 5 节）：并发数和保留属性给传输引擎（打开 SFTP 标签时读），
  // 隐藏文件是两栏的初始开关
  sftp_concurrency: 3,
  sftp_preserve_mtime: false,
  sftp_preserve_permissions: false,
  sftp_show_hidden: false,
  // 布局（UI-1）：可调宽面板的宽度，本机布局、M4 同步不同步（见 M4-sync-design.md）
  "layout.ai_panel_width": 400,
  "layout.aside_panel_width": 380,
  // SFTP 停靠面板（M3-3）：宽度和上下两栏的比例，同样是本机布局、不同步
  "layout.sftp_panel_width": 420,
  "layout.sftp_split_ratio": 0.5,
  // AI 面板输入框的高度（拖输入框上沿调），同样是本机布局、不同步
  "layout.ai_input_height": 50,
  // 终端底部快捷命令栏的高度（拖栏的上边框调，默认一行），同样是本机布局、不同步
  "layout.quick_bar_height": 28,
  // 主机页左边树的展开状态（逗号分隔的分组键），同样是本机布局、不同步
  "layout.vault_expanded": "",
  // 主机页左边树收起没有，同样是本机布局、不同步
  "layout.vault_sidebar_collapsed": false,
  // 最近打开的主机（id，逗号分隔，最近的在前），主机页卡片按它排；本机布局、不同步
  "layout.recent_hosts": "",
  // 服务器概览面板的宽度（拖左边缘调），本机布局、不同步
  "layout.overview_width": 360,
  // 快捷命令（终端底部的命令栏）：列表是 JSON 数组序列化成的字符串，跟着同步；
  // 栏开不开、当前看哪一组只在本机
  quick_commands: "[]",
  quick_commands_bar: false,
  quick_commands_group: "",
} as const;

export type TerminalCursorStyle = "block" | "underline" | "bar";

/** 每项设置的实际类型（比上面的默认值宽，默认值只是字面量）。 */
export interface SettingValues {
  // 外观
  ui_theme_mode: ThemeMode;
  ui_skin_light: string;
  ui_skin_dark: string;
  terminal_theme: string;
  // 终端
  terminal_font_family: string;
  terminal_font_size: number;
  terminal_line_height: number;
  terminal_scrollback: number;
  terminal_cursor_style: TerminalCursorStyle;
  terminal_cursor_blink: boolean;
  terminal_copy_on_select: boolean;
  terminal_ime_english: boolean;
  terminal_right_click_paste: boolean;
  terminal_confirm_multiline_paste: boolean;
  terminal_highlight_enabled: boolean;
  terminal_highlight_rules: string;
  session_log_enabled: boolean;
  session_log_dir: string;
  // 窗口
  close_action: CloseAction;
  tray_hint_shown: boolean;
  // 连接
  pool_idle_timeout_secs: number;
  keepalive_interval_secs: number;
  // 服务器状态栏
  stats_show: boolean;
  stats_refresh_secs: number;
  // AI
  ai_exec_mode: ExecMode;
  ai_opencode_path: string;
  ai_codex_path: string;
  ai_claude_path: string;
  ai_default_agent: AgentId;
  ai_write_backup_keep: number;
  // SFTP（M3-2 第 5 节）
  sftp_concurrency: number;
  sftp_preserve_mtime: boolean;
  sftp_preserve_permissions: boolean;
  sftp_show_hidden: boolean;
  // 布局（本机）
  "layout.ai_panel_width": number;
  "layout.aside_panel_width": number;
  "layout.sftp_panel_width": number;
  "layout.sftp_split_ratio": number;
  "layout.ai_input_height": number;
  "layout.quick_bar_height": number;
  "layout.vault_expanded": string;
  "layout.vault_sidebar_collapsed": boolean;
  "layout.recent_hosts": string;
  "layout.overview_width": number;
  quick_commands: string;
  quick_commands_bar: boolean;
  quick_commands_group: string;
}

/** 主题：跟随系统 / 浅色 / 深色。 */
export type ThemeMode = "system" | "light" | "dark";

/** AI 执行模式：逐条确认 / 只读命令自动 / 全部放行（state/execMode.ts）。 */
export type ExecMode = "manual" | "readonly" | "auto";

/** 智能体 id（AI-3；Claude Code 在 AI-4）。 */
export type AgentId = "opencode" | "codex" | "claude";

export type SettingKey = keyof SettingValues;
export type SettingValue<K extends SettingKey> = SettingValues[K];

const KEYS = Object.keys(SETTING_DEFAULTS) as SettingKey[];

/** 读回来一个值，类型对不上就当没存过（防止库里塞了奇怪的东西）。 */
function coerce<K extends SettingKey>(
  key: K,
  raw: unknown,
): SettingValue<K> | null {
  const fallback = SETTING_DEFAULTS[key];
  if (typeof fallback === "boolean") {
    return typeof raw === "boolean" ? (raw as SettingValue<K>) : null;
  }
  if (typeof fallback === "number") {
    return typeof raw === "number" && Number.isFinite(raw)
      ? (raw as SettingValue<K>)
      : null;
  }
  return typeof raw === "string" ? (raw as SettingValue<K>) : null;
}

/** 旧版本存的界面主题 id（只在升级时读一次）。 */
async function readLegacyUiTheme(): Promise<string | null> {
  try {
    const raw = await settingsGet("ui_theme");
    if (raw === null) return null;
    const value: unknown = JSON.parse(raw);
    return typeof value === "string" ? value : null;
  } catch {
    return null;
  }
}

async function readSetting<K extends SettingKey>(
  key: K,
): Promise<SettingValue<K> | null> {
  try {
    const raw = await settingsGet(key);
    if (raw === null) return null;
    return coerce(key, JSON.parse(raw));
  } catch (error) {
    console.warn(`读设置 ${key} 失败`, error);
    return null;
  }
}

/** 写进后端。失败就抛出去，由调用方回滚界面上的值。 */
async function writeSetting(key: SettingKey, value: unknown): Promise<void> {
  await settingsSet(key, JSON.stringify(value));
}

/**
 * 每项设置**最后一次确认保存成功的值**（加载时读到的值，和之后每次写成功的值）。
 * 写失败时界面回滚到它，而不是回滚到"调用那一刻界面上的值"——那个值不一定真的保存过
 * （连续改两次、两次都失败时，后者会让界面显示一个数据库里根本没有的值）。
 */
let confirmed: Record<string, unknown> = { ...SETTING_DEFAULTS };

/** 同一项设置的写入排成一队，按调用顺序一个一个写（不同的设置互不排队）。 */
const writeChains = new Map<string, Promise<unknown>>();
/** 每项设置现在还有几次写入没结束（在队里或者正在写）。 */
const writesInFlight = new Map<string, number>();

interface SettingsState extends SettingValues {
  loaded: boolean;
  /** 系统现在是不是深色模式（主题选"系统"时用；不存库）。 */
  systemDark: boolean;
  load: () => Promise<void>;
  /**
   * 改一项设置：界面立刻跟着变，写进后端；写失败就把界面**回滚到原值**并提示错误
   * （界面显示的值必须和后端实际用的一致，执行模式这类安全相关的设置尤其不能错）。
   * 返回有没有保存成功。
   */
  update: <K extends SettingKey>(
    key: K,
    value: SettingValue<K>,
  ) => Promise<boolean>;
}

/** 外观相关的设置：改了要重新套一次配色。 */
const APPEARANCE_KEYS = new Set<string>(["ui_theme_mode", "ui_skin_light", "ui_skin_dark"]);

const systemDarkQuery =
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: dark)")
    : null;

export const useSettings = create<SettingsState>((set, get) => ({
  ...SETTING_DEFAULTS,
  loaded: false,
  systemDark: systemDarkQuery?.matches ?? true,

  load: async () => {
    const pairs = await Promise.all(
      KEYS.map(async (key) => [key, await readSetting(key)] as const),
    );
    const values: SettingValues = { ...SETTING_DEFAULTS };
    for (const [key, value] of pairs) {
      if (value !== null) {
        (values as unknown as Record<string, unknown>)[key] = value;
      }
    }
    // 从旧版本升级：以前存的是一个界面主题（带明暗），没有主题模式。按旧主题的明暗定模式，
    // 不让深色用户升级后突然变成跟随系统的浅色。
    if (pairs.find(([key]) => key === "ui_theme_mode")?.[1] === null) {
      const legacy = await readLegacyUiTheme();
      const mode = legacy ? LEGACY_UI_THEME_MODES[legacy] : undefined;
      if (mode) values.ui_theme_mode = mode;
    }
    // 旧版本的终端配色 id：换成现在最接近的
    values.terminal_theme = normalizeTerminalTheme(values.terminal_theme);
    // 「关闭主窗口时」：读不懂的值按默认（缩到托盘）
    values.close_action = parseCloseAction(values.close_action);
    confirmed = { ...values };
    set({ ...values, loaded: true });
    applyAppearance(get());
    // 旧版本自带的两条示例规则一点没改过：换成现在的整组常用规则（写回库里）
    const upgraded = upgradeLegacySamples(values.terminal_highlight_rules);
    if (upgraded !== null) void get().update("terminal_highlight_rules", upgraded);
  },

  update: (key, value) => {
    const inFlight = writesInFlight.get(key) ?? 0;
    // 界面、数据库、没有待写入，三者都已经是这个值：什么都不用做。
    if (inFlight === 0 && get()[key] === value && confirmed[key] === value) {
      return Promise.resolve(true);
    }
    set({ [key]: value } as Partial<SettingsState>);
    if (APPEARANCE_KEYS.has(key)) applyAppearance(get());
    writesInFlight.set(key, inFlight + 1);

    const run = async (): Promise<boolean> => {
      let failure: unknown = null;
      let saved = false;
      try {
        await writeSetting(key, value);
        confirmed[key] = value;
        saved = true;
      } catch (error) {
        failure = error;
      }
      writesInFlight.set(key, (writesInFlight.get(key) ?? 1) - 1);
      if (saved) return true;

      console.warn(`写设置 ${key} 失败`, failure);
      // 后面还有这项设置的写入排着队：由最后一次的结果决定界面最终显示什么，这里不动。
      // 界面上已经不是我们设的值了（别处又改过）：也不覆盖它。
      const newerQueued = (writesInFlight.get(key) ?? 0) > 0;
      if (!newerQueued && get()[key] === value) {
        const back = confirmed[key] as SettingValue<typeof key>;
        set({ [key]: back } as Partial<SettingsState>);
        if (APPEARANCE_KEYS.has(key)) applyAppearance(get());
      }
      toast.error(`保存设置失败，已恢复原来的值：${String(failure)}`);
      return false;
    };

    const next = (writeChains.get(key) ?? Promise.resolve()).then(run);
    writeChains.set(key, next);
    return next;
  },
}));

/** 主题模式 + 系统明暗 → 现在实际是浅色还是深色。 */
export function resolveMode(mode: ThemeMode, systemDark: boolean): UiThemeMode {
  if (mode === "light" || mode === "dark") return mode;
  return systemDark ? "dark" : "light";
}

/** 现在用的皮肤。 */
export function activeSkin(values: {
  ui_theme_mode: ThemeMode;
  ui_skin_light: string;
  ui_skin_dark: string;
  systemDark: boolean;
}): Skin {
  const mode = resolveMode(values.ui_theme_mode, values.systemDark);
  return skinFor(mode, mode === "dark" ? values.ui_skin_dark : values.ui_skin_light);
}

/** 把现在的皮肤套到界面上。 */
function applyAppearance(state: SettingsState): void {
  const skin = activeSkin(state);
  applyUiTokens(skin.mode, skinTokens(skin));
}

// 系统切换明暗（Windows 设置里的"应用模式"）：主题选的是"系统"时跟着变
systemDarkQuery?.addEventListener("change", (event) => {
  useSettings.setState({ systemDark: event.matches });
  applyAppearance(useSettings.getState());
});

// 设置还没从库里读回来之前先按默认值（跟随系统）套一次，启动时不会先闪一下没有颜色的白底
if (typeof document !== "undefined") applyAppearance(useSettings.getState());

/**
 * 终端配色设置的值：跟随皮肤，或某个皮肤的配色。旧版本（0.1.4 及以前）存的配色 id 一律当成
 * 跟随皮肤——旧版本里终端背景本来就跟着界面走，升级后固定成某一套会出现"界面浅色、终端深色"。
 */
export function normalizeTerminalTheme(id: string): string {
  if (id === TERMINAL_FOLLOW_SKIN || getSkin(id)) return id;
  return TERMINAL_FOLLOW_SKIN;
}

/** 终端实际用的 xterm 配色：跟随皮肤就用皮肤配套的，单独选了就用选的那套。 */
export function terminalITheme(values: {
  terminal_theme: string;
  ui_theme_mode: ThemeMode;
  ui_skin_light: string;
  ui_skin_dark: string;
  systemDark: boolean;
}): ITheme {
  const chosen = getSkin(normalizeTerminalTheme(values.terminal_theme));
  return paletteToXterm((chosen ?? activeSkin(values)).terminal);
}

/** 组件里用这个：皮肤、明暗、终端配色任何一个变了都会重算。 */
export function useTerminalITheme(): ITheme {
  const terminal_theme = useSettings((state) => state.terminal_theme);
  const ui_theme_mode = useSettings((state) => state.ui_theme_mode);
  const ui_skin_light = useSettings((state) => state.ui_skin_light);
  const ui_skin_dark = useSettings((state) => state.ui_skin_dark);
  const systemDark = useSettings((state) => state.systemDark);
  return useMemo(
    () => terminalITheme({ terminal_theme, ui_theme_mode, ui_skin_light, ui_skin_dark, systemDark }),
    [terminal_theme, ui_theme_mode, ui_skin_light, ui_skin_dark, systemDark],
  );
}

/** 终端配色是浅色还是深色（终端可以单独选一套，不一定和界面一致）。 */
export function useTerminalMode(): UiThemeMode {
  const terminal_theme = useSettings((state) => state.terminal_theme);
  const ui_theme_mode = useSettings((state) => state.ui_theme_mode);
  const ui_skin_light = useSettings((state) => state.ui_skin_light);
  const ui_skin_dark = useSettings((state) => state.ui_skin_dark);
  const systemDark = useSettings((state) => state.systemDark);
  const chosen = getSkin(normalizeTerminalTheme(terminal_theme));
  return (chosen ?? activeSkin({ ui_theme_mode, ui_skin_light, ui_skin_dark, systemDark })).mode;
}

/** 组件里要知道现在是浅色还是深色（编辑器配色之类）。 */
export function useResolvedMode(): UiThemeMode {
  const mode = useSettings((state) => state.ui_theme_mode);
  const systemDark = useSettings((state) => state.systemDark);
  return resolveMode(mode, systemDark);
}
