//! 快捷命令（像 Xshell 的快速命令栏）：数据、分组、发送内容。纯函数，单测直接调。
//!
//! 列表存在设置 `quick_commands` 里（JSON 数组序列化成的字符串，跟着配置同步）；
//! 后端同步时按同样的规则校验（`sync/settings.rs` 的 `check_quick_commands`）。

export interface QuickCommand {
  id: string;
  name: string;
  command: string;
  /** 分组名；空 = 不分组（只在"全部"里）。 */
  group?: string;
  /** 发送后自动回车（执行）；false 只填进去，让用户改完再按回车。 */
  enter?: boolean;
  /** 预设颜色 id；不设 = 默认（无色）。 */
  color?: QuickCommandColor;
}

/** 预设颜色的 id（固定这 8 个；存进同步配置里，别改名）。 */
export const QUICK_COMMAND_COLORS = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink"] as const;
export type QuickCommandColor = (typeof QUICK_COMMAND_COLORS)[number];

export interface QuickCommandColorStyle {
  /** 中文名（色块的 aria-label / 提示）。 */
  label: string;
  /** 命令栏按钮：淡底色 + 细边框（ring 不占布局，不改按钮尺寸）+ 悬停加深。文字保持正常前景色。 */
  button: string;
  /** 命令栏按钮左边的小图标。 */
  icon: string;
  /** 管理面板列表里名字前面的小圆点、编辑表单里的色块。 */
  dot: string;
}

// Tailwind 只认完整写出来的类名，所以每个颜色逐条列全，不拼字符串。
export const QUICK_COMMAND_COLOR_STYLES: Record<QuickCommandColor, QuickCommandColorStyle> = {
  red: {
    label: "红",
    button: "bg-red-500/15 ring-1 ring-inset ring-red-500/40 hover:bg-red-500/25 active:bg-red-500/30",
    icon: "text-red-500",
    dot: "bg-red-500",
  },
  orange: {
    label: "橙",
    button: "bg-orange-500/15 ring-1 ring-inset ring-orange-500/40 hover:bg-orange-500/25 active:bg-orange-500/30",
    icon: "text-orange-500",
    dot: "bg-orange-500",
  },
  yellow: {
    label: "黄",
    button: "bg-yellow-500/15 ring-1 ring-inset ring-yellow-500/40 hover:bg-yellow-500/25 active:bg-yellow-500/30",
    icon: "text-yellow-600 dark:text-yellow-400",
    dot: "bg-yellow-500",
  },
  green: {
    label: "绿",
    button: "bg-green-500/15 ring-1 ring-inset ring-green-500/40 hover:bg-green-500/25 active:bg-green-500/30",
    icon: "text-green-600 dark:text-green-500",
    dot: "bg-green-500",
  },
  teal: {
    label: "青",
    button: "bg-teal-500/15 ring-1 ring-inset ring-teal-500/40 hover:bg-teal-500/25 active:bg-teal-500/30",
    icon: "text-teal-600 dark:text-teal-400",
    dot: "bg-teal-500",
  },
  blue: {
    label: "蓝",
    button: "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 hover:bg-blue-500/25 active:bg-blue-500/30",
    icon: "text-blue-500",
    dot: "bg-blue-500",
  },
  purple: {
    label: "紫",
    button: "bg-purple-500/15 ring-1 ring-inset ring-purple-500/40 hover:bg-purple-500/25 active:bg-purple-500/30",
    icon: "text-purple-500",
    dot: "bg-purple-500",
  },
  pink: {
    label: "粉",
    button: "bg-pink-500/15 ring-1 ring-inset ring-pink-500/40 hover:bg-pink-500/25 active:bg-pink-500/30",
    icon: "text-pink-500",
    dot: "bg-pink-500",
  },
};

/** 未知值（比如以后新版本加的颜色）当作没设。 */
export function normalizeQuickCommandColor(value: unknown): QuickCommandColor | undefined {
  return typeof value === "string" && (QUICK_COMMAND_COLORS as readonly string[]).includes(value)
    ? (value as QuickCommandColor)
    : undefined;
}

export const NAME_MAX = 100;
export const COMMAND_MAX = 8000;
export const GROUP_MAX = 50;
export const COMMANDS_MAX = 500;

/** 读出来的列表：坏的整体当空、坏的单条跳过（不让一条坏数据弄掉整个栏）。 */
export function parseQuickCommands(raw: string): QuickCommand[] {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(value)) return [];
  const out: QuickCommand[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const { id, name, command, group, enter, color } = item as Record<string, unknown>;
    if (typeof id !== "string" || typeof name !== "string" || typeof command !== "string") continue;
    out.push({
      id,
      name,
      command,
      group: typeof group === "string" && group.trim() !== "" ? group.trim() : undefined,
      enter: enter !== false,
      color: normalizeQuickCommandColor(color),
    });
  }
  return out;
}

export function serializeQuickCommands(list: QuickCommand[]): string {
  return JSON.stringify(
    list.map(({ id, name, command, group, enter, color }) => ({
      id,
      name,
      command,
      ...(group ? { group } : {}),
      enter: enter !== false,
      ...(normalizeQuickCommandColor(color) ? { color } : {}),
    })),
  );
}

/** 用到的分组，按第一次出现的顺序。 */
export function groupsOf(list: QuickCommand[]): string[] {
  const seen = new Set<string>();
  for (const item of list) if (item.group) seen.add(item.group);
  return [...seen];
}

/** 栏上显示哪些：分组为空 = 全部；选的分组已经没有命令了也退回全部。 */
export function commandsInGroup(list: QuickCommand[], group: string): QuickCommand[] {
  if (!group || !list.some((item) => item.group === group)) return list;
  return list.filter((item) => item.group === group);
}

/** 换行和 Tab 以外的控制字符（ESC 之类会改终端状态，不让存进命令里）。 */
function hasBadControl(text: string): boolean {
  for (const ch of text) {
    const code = ch.charCodeAt(0);
    if ((code < 0x20 || code === 0x7f) && ch !== "\n" && ch !== "\r" && ch !== "\t") return true;
  }
  return false;
}

/** 编辑框的内容能不能存；能存返回 null，不能存返回原因。 */
export function validateQuickCommand(draft: { name: string; command: string; group: string }): string | null {
  const name = draft.name.trim();
  if (!name) return "名称不能为空";
  if (name.length > NAME_MAX) return `名称不能超过 ${NAME_MAX} 个字`;
  if (hasBadControl(name)) return "名称里不能有控制字符";
  if (!draft.command.trim()) return "命令不能为空";
  if (draft.command.length > COMMAND_MAX) return `命令不能超过 ${COMMAND_MAX} 个字`;
  if (hasBadControl(draft.command)) return "命令里只能有换行和 Tab 这两种控制字符";
  if (draft.group.trim().length > GROUP_MAX) return `分组名不能超过 ${GROUP_MAX} 个字`;
  if (hasBadControl(draft.group)) return "分组名里不能有控制字符";
  return null;
}

/** 发进终端的内容：换行统一成回车（和在终端里按回车一样），要执行的末尾再补一个回车。 */
export function payloadOf(command: QuickCommand): string {
  const body = command.command.replace(/\r\n|\r|\n/g, "\r").replace(/\r+$/, "");
  return command.enter === false ? body : `${body}\r`;
}

/** 多行命令（中间有换行）：发之前要不要先确认。 */
export function isMultiline(command: QuickCommand): boolean {
  return /[\r\n]/.test(command.command.trim());
}

export function newQuickCommandId(): string {
  return `qc-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/** 新增或替换（按 id）。 */
export function upsertQuickCommand(list: QuickCommand[], item: QuickCommand): QuickCommand[] {
  const index = list.findIndex((existing) => existing.id === item.id);
  if (index < 0) return [...list, item];
  const next = list.slice();
  next[index] = item;
  return next;
}

/** 上移 / 下移一位（栏上的顺序就是列表顺序）。 */
export function moveQuickCommand(list: QuickCommand[], id: string, delta: -1 | 1): QuickCommand[] {
  const index = list.findIndex((item) => item.id === id);
  const target = index + delta;
  if (index < 0 || target < 0 || target >= list.length) return list;
  const next = list.slice();
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

/** 分组键：没有分组的用空串。 */
const groupOfCommand = (item: QuickCommand): string => item.group ?? "";

/**
 * 在同一分组内上移 / 下移一位。和同组里相邻的那条交换它们在整个列表里的位置，别的分组的命令
 * 原地不动，所以每个分组里的相对顺序（也就是选了这个分组后命令栏里的顺序）恰好变了这一位。
 * 已经是组内第一 / 最后一条时原样返回。
 */
export function moveQuickCommandInGroup(list: QuickCommand[], id: string, delta: -1 | 1): QuickCommand[] {
  const index = list.findIndex((item) => item.id === id);
  if (index < 0) return list;
  const group = groupOfCommand(list[index]);
  let target = index + delta;
  while (target >= 0 && target < list.length && groupOfCommand(list[target]) !== group) target += delta;
  if (target < 0 || target >= list.length) return list;
  const next = list.slice();
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

/** 这条在自己的分组里还能不能上移 / 下移：[上移, 下移]。 */
export function canMoveInGroup(list: QuickCommand[], id: string): [boolean, boolean] {
  return [
    moveQuickCommandInGroup(list, id, -1) !== list,
    moveQuickCommandInGroup(list, id, 1) !== list,
  ];
}

/** 按名字、命令内容、分组过滤（不分大小写）；搜索词为空返回原列表。 */
export function searchQuickCommands(list: QuickCommand[], query: string): QuickCommand[] {
  const text = query.trim().toLowerCase();
  if (text === "") return list;
  return list.filter(
    (item) =>
      item.name.toLowerCase().includes(text) ||
      item.command.toLowerCase().includes(text) ||
      (item.group ?? "").toLowerCase().includes(text),
  );
}

export interface QuickCommandSection {
  /** 分组名；空串 = 未分组。 */
  group: string;
  items: QuickCommand[];
}

/** 按分组归拢（管理面板的显示）：分组按第一次出现的顺序，未分组放最后；组内保持列表里的相对顺序。 */
export function sectionsOfQuickCommands(list: QuickCommand[]): QuickCommandSection[] {
  const sections: QuickCommandSection[] = groupsOf(list).map((group) => ({ group, items: [] }));
  const byGroup = new Map(sections.map((section) => [section.group, section]));
  const ungrouped: QuickCommandSection = { group: "", items: [] };
  for (const item of list) (item.group ? byGroup.get(item.group) : ungrouped)?.items.push(item);
  return ungrouped.items.length > 0 ? [...sections, ungrouped] : sections;
}
