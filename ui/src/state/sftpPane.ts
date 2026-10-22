// Adapted from Netcatty components/sftp/sftpSorting.ts、sftpNameFilter.test.ts（逻辑重写为 Z-SSH 的数据形状）
//! SFTP 两栏共用的纯逻辑：排序、过滤、多选、虚拟滚动的窗口计算。
//!
//! 全部是纯函数（不碰 DOM），单测直接调（M3-2 第 6.1 节：20000 条、排序过滤、
//! 多选和键盘操作）。

import type { SftpEntry } from "../api/sftp";

// ---------------------------------------------------------------------------
// 排序
// ---------------------------------------------------------------------------

export type SortKey = "name" | "size" | "mtime" | "kind";

export interface SortOrder {
  key: SortKey;
  /** false = 升序（目录恒在文件前面） */
  descending: boolean;
}

// 比较器建一次复用：带语言选项调 localeCompare 时，每次比较都要重新建排序规则，
// 几万个条目排一次序会很慢（MDN 建议大量比较时复用 Intl.Collator）。规则和原来一样：
// 中文按拼音、数字按数值大小（file2 排在 file10 前面）、大小写不分先后时小写在前。
const NAME_COLLATOR = new Intl.Collator("zh-Hans-CN-u-kf-lower", { numeric: true });
const DEFAULT_COLLATOR = new Intl.Collator();

/** 目录排在文件前面（不管按什么排）；同类按 SortOrder。 */
export function sortEntries(entries: SftpEntry[], order: SortOrder): SftpEntry[] {
  const dirFirst = (entry: SftpEntry): number =>
    entry.kind === "dir" || entry.kind === "drive" ? 0 : 1;
  const factor = order.descending ? -1 : 1;
  return [...entries].sort((a, b) => {
    const dirDiff = dirFirst(a) - dirFirst(b);
    if (dirDiff !== 0) return dirDiff;
    let diff = 0;
    switch (order.key) {
      case "name":
        diff = NAME_COLLATOR.compare(a.name, b.name);
        break;
      case "size":
        diff = (a.size ?? 0) - (b.size ?? 0);
        break;
      case "mtime":
        diff = (a.mtime ?? 0) - (b.mtime ?? 0);
        break;
      case "kind":
        diff = kindRank(a) - kindRank(b);
        if (diff === 0) diff = DEFAULT_COLLATOR.compare(a.name, b.name);
        break;
    }
    return diff * factor;
  });
}

function kindRank(entry: SftpEntry): number {
  switch (entry.kind) {
    case "drive":
      return 0;
    case "dir":
      return 1;
    case "symlink":
      return 2;
    case "file":
      return 3;
    default:
      return 4;
  }
}

// ---------------------------------------------------------------------------
// 过滤
// ---------------------------------------------------------------------------

export interface FilterOptions {
  /** 按名字过滤（大小写不敏感、子串匹配）。 */
  text: string;
  /** 显示隐藏文件。 */
  showHidden: boolean;
}

/** 排好序的列表，带上每一项名字的小写（过滤时不用每打一个字都重新转一遍）。 */
export interface SortedEntries {
  entries: SftpEntry[];
  lowerNames: string[];
}

/** 先排序（只在目录内容或排序方式变了时做）。 */
export function sortForDisplay(entries: SftpEntry[], order: SortOrder): SortedEntries {
  const sorted = sortEntries(entries, order);
  return { entries: sorted, lowerNames: sorted.map((entry) => entry.name.toLowerCase()) };
}

/** 再过滤（搜索框每打一个字只走这一步）：过滤不改变顺序，结果和"先过滤再排序"一样。 */
export function filterSorted(sorted: SortedEntries, filter: FilterOptions): SftpEntry[] {
  const needle = filter.text.trim().toLowerCase();
  if (!needle && filter.showHidden) return sorted.entries;
  const out: SftpEntry[] = [];
  for (let i = 0; i < sorted.entries.length; i += 1) {
    const entry = sorted.entries[i];
    if (!filter.showHidden && entry.hidden && entry.kind !== "drive") continue;
    if (needle && !sorted.lowerNames[i].includes(needle)) continue;
    out.push(entry);
  }
  return out;
}

/** 过滤 + 排序一条龙（等价于 sortForDisplay + filterSorted）。 */
export function visibleEntries(
  entries: SftpEntry[],
  filter: FilterOptions,
  order: SortOrder,
): SftpEntry[] {
  return filterSorted(sortForDisplay(entries, order), filter);
}

/**
 * 选择 / 焦点按文件身份（路径）记，不按行号记：排序、过滤、后台刷新之后行号会变，
 * 按行号记的话选中的就成了别的文件。这里把路径集合换算成当前列表里的行号（显示用、
 * Shift 范围选择用）；当前列表里没有的（被过滤掉、刷新后没了）不算进来，也就不会被操作。
 */
export function indicesOf(rows: SftpEntry[], paths: ReadonlySet<string>): Set<number> {
  const out = new Set<number>();
  if (paths.size === 0) return out;
  rows.forEach((entry, index) => {
    if (paths.has(entry.path)) out.add(index);
  });
  return out;
}

/** 行号集合 → 路径集合（存下来的是这个）。 */
export function pathsOf(rows: SftpEntry[], indices: Iterable<number>): Set<string> {
  const out = new Set<string>();
  for (const index of indices) {
    const entry = rows[index];
    if (entry) out.add(entry.path);
  }
  return out;
}

// ---------------------------------------------------------------------------
// 多选（Ctrl / Shift / Ctrl+A）
// ---------------------------------------------------------------------------

/**
 * 点击后的新选择集合。
 *
 * - 裸点：只选它；
 * - Ctrl+点：有它就去掉、没有就加上；
 * - Shift+点：从锚点到它（含两端）整段选上；锚点没设过时当裸点。
 */
export function selectionAfterClick(
  index: number,
  anchor: number | null,
  previous: Set<number>,
  modifiers: { ctrl: boolean; shift: boolean },
  total: number,
): { selection: Set<number>; anchor: number } {
  if (index < 0 || index >= total) {
    return { selection: previous, anchor: anchor ?? 0 };
  }
  if (modifiers.shift && anchor !== null) {
    const from = Math.min(anchor, index);
    const to = Math.max(anchor, index);
    const selection = new Set<number>();
    for (let i = from; i <= to; i += 1) selection.add(i);
    return { selection, anchor };
  }
  if (modifiers.ctrl) {
    const selection = new Set(previous);
    if (selection.has(index)) selection.delete(index);
    else selection.add(index);
    return { selection, anchor: index };
  }
  return { selection: new Set([index]), anchor: index };
}

/** 键盘上下移动焦点（带 Shift 就是扩展选择）。 */
export function selectionAfterArrow(
  index: number,
  anchor: number | null,
  shift: boolean,
  delta: number,
  total: number,
): { selection: Set<number>; anchor: number; focused: number } {
  if (total <= 0) return { selection: new Set(), anchor: anchor ?? 0, focused: 0 };
  const next = Math.min(Math.max(index + delta, 0), Math.max(total - 1, 0));
  if (!shift) {
    return { selection: new Set([next]), anchor: next, focused: next };
  }
  const start = anchor ?? index;
  const from = Math.min(start, next);
  const to = Math.max(start, next);
  const selection = new Set<number>();
  for (let i = from; i <= to; i += 1) selection.add(i);
  return { selection, anchor: start, focused: next };
}

/** Ctrl+A：全部选上（total = 0 时是空集）。 */
export function selectAll(total: number): Set<number> {
  const selection = new Set<number>();
  for (let i = 0; i < total; i += 1) selection.add(i);
  return selection;
}

// ---------------------------------------------------------------------------
// 虚拟滚动（20000 条不卡的关键：只算"该渲染哪一段"）
// ---------------------------------------------------------------------------

export const ROW_HEIGHT = 26;

export interface VirtualWindow {
  /** 第一个要渲染的下标。 */
  start: number;
  /** 渲染到哪个下标为止（不含）。 */
  end: number;
  /** 列表内容撑起来的总高度（撑出滚动条用）。 */
  totalHeight: number;
  /** 渲染段顶部偏多少像素（把可见段推到滚动位置）。 */
  offsetY: number;
}

/**
 * 按滚动位置算要渲染的窗口。
 *
 * @param scrollTop  滚动条位置（px）
 * @param viewport   可视区高度（px）
 * @param count      总条数
 * @param overscan   上下各多渲染几行（快速滚动时别露白）
 */
export function virtualWindow(
  scrollTop: number,
  viewport: number,
  count: number,
  overscan = 8,
): VirtualWindow {
  const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - overscan);
  const visible = Math.ceil(viewport / ROW_HEIGHT) + overscan * 2;
  const end = Math.min(count, start + visible);
  return {
    start,
    end,
    totalHeight: count * ROW_HEIGHT,
    offsetY: start * ROW_HEIGHT,
  };
}

// ---------------------------------------------------------------------------
// 面包屑 / 路径
// ---------------------------------------------------------------------------

/** 远端路径的面包屑段：`/tmp/a/b` → [{tmp},{a},{b}]；根是空数组。 */
export function remoteCrumbs(path: string): Array<{ name: string; path: string }> {
  const parts = path.split("/").filter((part) => part.length > 0);
  const out: Array<{ name: string; path: string }> = [];
  let built = "";
  for (const part of parts) {
    built = `${built}/${part}`;
    out.push({ name: part, path: built });
  }
  return out;
}

/**
 * 本地路径的根和根下面的部分。盘符：`C:\a` → 根 `C:`、其余 `a`；
 * 网络共享：`\\server\share\a` → 根 `\\server\share`、其余 `a`（共享根就是最上一级）。
 * 带 `\\?\` 长路径前缀的也认（后端正常不会给）。
 */
function splitLocalRoot(path: string): { root: string; rest: string } {
  let normalized = path.replace(/\//g, "\\");
  if (/^\\\\\?\\UNC\\/i.test(normalized)) normalized = `\\\\${normalized.slice(8)}`;
  else if (normalized.startsWith("\\\\?\\")) normalized = normalized.slice(4);
  if (normalized.startsWith("\\\\")) {
    const [server = "", share = "", ...rest] = normalized.slice(2).split("\\");
    return { root: share ? `\\\\${server}\\${share}` : `\\\\${server}`, rest: rest.join("\\") };
  }
  return { root: normalized.slice(0, 2), rest: normalized.slice(3) };
}

/**
 * 本地路径的面包屑段：`C:\Users\x` → [{C:},{Users},{x}]；`\\srv\share\x` → [{\\srv\share},{x}]；
 * "此电脑"（空）是空数组。
 */
export function localCrumbs(path: string): Array<{ name: string; path: string }> {
  if (!path) return [];
  const { root, rest } = splitLocalRoot(path);
  const parts = [root, ...rest.split("\\").filter((part) => part.length > 0)];
  const out: Array<{ name: string; path: string }> = [];
  let built = "";
  parts.forEach((part, index) => {
    built = index === 0 ? `${part}\\` : `${built}${part}`;
    out.push({ name: part, path: built });
    if (index > 0) built = `${built}\\`;
  });
  return out;
}

type Crumb = { name: string; path: string };

/** 一段面包屑大约占多宽（px）：13px 字号，汉字约 13、其余约 7，加按钮内边距和分隔箭头。 */
export function crumbWidthEstimate(name: string): number {
  let width = 0;
  for (const ch of name) width += ch.charCodeAt(0) > 0x2e7f ? 13 : 7;
  return width + 12 + 11;
}

/** "…"按钮的宽度。 */
const ELLIPSIS_WIDTH = 28;

/**
 * 面包屑放不下时：最后几级优先保留，前面的折叠成"…"。
 * 至少显示最后一级（它再长也交给 CSS 截断）；放得下就一级不折。
 */
export function collapseCrumbs(
  crumbs: Crumb[],
  availableWidth: number,
): { hidden: Crumb[]; shown: Crumb[] } {
  if (crumbs.length === 0 || availableWidth <= 0) return { hidden: [], shown: crumbs };
  const total = crumbs.reduce((sum, crumb) => sum + crumbWidthEstimate(crumb.name), 0);
  if (total <= availableWidth) return { hidden: [], shown: crumbs };
  let used = ELLIPSIS_WIDTH;
  let keep = 0;
  for (let i = crumbs.length - 1; i >= 0; i -= 1) {
    const width = crumbWidthEstimate(crumbs[i].name);
    if (keep > 0 && used + width > availableWidth) break;
    used += width;
    keep += 1;
  }
  const cut = crumbs.length - keep;
  return { hidden: crumbs.slice(0, cut), shown: crumbs.slice(cut) };
}

/** 本地路径的上一级：`C:\a\b` → `C:\a`；`C:\` → ""（此电脑）。 */
export function localParent(path: string): string {
  const crumbs = localCrumbs(path);
  if (crumbs.length <= 1) return "";
  return crumbs[crumbs.length - 2].path;
}

/** 远端路径的上一级：`/tmp/a` → `/tmp`；`/` 还是 `/`（根没有上级）。 */
export function remoteParent(path: string): string {
  const cut = path.lastIndexOf("/");
  if (cut <= 0) return "/";
  return path.slice(0, cut);
}

// ---------------------------------------------------------------------------
// 名字校验（新建 / 重命名对话框用）
// ---------------------------------------------------------------------------

export type NameCheck = { ok: true } | { ok: false; reason: string };

export function validateName(name: string, isRemote: boolean): NameCheck {
  if (name.trim().length === 0) return { ok: false, reason: "名字不能为空" };
  if (name.includes("/")) return { ok: false, reason: "名字里不能有 /" };
  if (isRemote) return { ok: true };
  if (name.includes("\\")) return { ok: false, reason: "名字里不能有 \\" };
  for (const bad of [":", "*", "?", '"', "<", ">", "|"]) {
    if (name.includes(bad)) return { ok: false, reason: `名字里不能有 ${bad}` };
  }
  return { ok: true };
}

// ---------------------------------------------------------------------------
// 显示辅助
// ---------------------------------------------------------------------------

export function formatSize(size: number | undefined): string {
  if (size === undefined) return "—";
  if (size < 1024) return `${size} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = size;
  let unit = "B";
  for (const next of units) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${unit}`;
}

export function formatSpeed(bytesPerSecond: number): string {
  return `${formatSize(bytesPerSecond)}/s`;
}

export function formatEta(seconds: number | undefined): string {
  if (seconds === undefined) return "—";
  if (seconds < 60) return `${Math.max(seconds, 1)} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${Math.floor(seconds / 3600)} 时 ${Math.floor((seconds % 3600) / 60)} 分`;
}

export function formatTime(unixSeconds: number | undefined): string {
  if (unixSeconds === undefined || unixSeconds === 0) return "—";
  return new Date(unixSeconds * 1000).toLocaleString();
}

/** 权限的八进制显示（644 → "0644"）。 */
export function formatPermissions(mode: number | undefined): string {
  if (mode === undefined) return "—";
  // 服务器给的 mode 带文件类型位（如 0o100644）：只显示权限位。
  return (mode & 0o7777).toString(8).padStart(4, "0");
}

// ---------------------------------------------------------------------------
// 停靠面板（M3-3）：上下分隔线、队列摘要
// ---------------------------------------------------------------------------

/** 分隔线的比例（远端栏占的份额）：鼠标在容器里的位置 → 夹进 [min, 1-min]。 */
export function splitRatioFromPointer(
  pointerY: number,
  containerTop: number,
  containerHeight: number,
  min = 0.15,
): number {
  if (!(containerHeight > 0)) return 0.5;
  const raw = (pointerY - containerTop) / containerHeight;
  return Math.min(1 - min, Math.max(min, raw));
}

/** 读回来的比例体检：不是有限数或者越界就用默认值。 */
export function sanitizeSplitRatio(raw: unknown, fallback = 0.5, min = 0.15): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return fallback;
  if (raw < min || raw > 1 - min) return fallback;
  return raw;
}

/** 队列摘要："2 个任务进行中 45%"；没有进行中的任务返回 null。 */
export function queueSummary(
  transfers: ReadonlyArray<{ state: string; done_bytes: number; total_bytes: number }>,
): { active: number; percent: number; text: string } | null {
  const active = transfers.filter(
    (item) => !["done", "failed", "cancelled", "skipped"].includes(item.state),
  );
  if (active.length === 0) return null;
  const total = active.reduce((sum, item) => sum + item.total_bytes, 0);
  const done = active.reduce((sum, item) => sum + item.done_bytes, 0);
  const percent = total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : 0;
  return { active: active.length, percent, text: `${active.length} 个任务进行中 ${percent}%` };
}

/** 远端路径的父目录（根目录的父目录还是 "/"）。 */
export function remoteParentDir(path: string): string {
  const trimmed = path.length > 1 ? path.replace(/\/+$/, "") : path;
  const cut = trimmed.lastIndexOf("/");
  if (cut <= 0) return "/";
  return trimmed.slice(0, cut);
}

/**
 * 编辑器保存完成（含"另存为"）后，远端栏要不要重新列目录：事件属于这个会话、
 * 状态是 uploaded、文件所在的目录正是远端栏当前显示的目录。
 */
export function editUploadNeedsRefresh(
  event: { sftp_id: string; state: string; remote_path: string },
  sftpId: string,
  currentRemoteDir: string,
): boolean {
  if (event.sftp_id !== sftpId || event.state !== "uploaded") return false;
  if (!currentRemoteDir) return false;
  return remoteParentDir(event.remote_path) === currentRemoteDir.replace(/(.)\/+$/, "$1");
}
