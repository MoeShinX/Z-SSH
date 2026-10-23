//! 主机页左边的树（像 Xshell 的会话管理器）：分组可以展开 / 收起，主机挂在分组下面。
//!
//! 展开状态存进设置 `layout.vault_expanded`（本机布局、不同步）：逗号分隔的键，
//! 分组是 `g<id>`，未分组是 `ungrouped`。

import type { Group, Host } from "../api/vault";

export const UNGROUPED_KEY = "ungrouped";

const collator = new Intl.Collator("zh-Hans-CN", { numeric: true, sensitivity: "base" });

/** 名字开头是哪一类：数字 → 英文字母 → 中文 → 其他符号。 */
function nameClass(name: string): number {
  const first = String.fromCodePoint(name.trimStart().codePointAt(0) ?? 0);
  if (/[0-9]/.test(first)) return 0;
  if (/[a-z]/i.test(first)) return 1;
  if (/\p{Script=Han}/u.test(first)) return 2;
  return 3;
}

/**
 * 主机、分组按名字排：先按开头分成 数字、英文、中文、其他符号 四段，段内用中文排序规则
 * （英文不分大小写，中文按拼音，数字按数值大小：2 在 10 前面）。
 */
export function compareNames(a: string, b: string): number {
  return nameClass(a) - nameClass(b) || collator.compare(a, b) || a.localeCompare(b);
}

/**
 * 同一层（同一个分组下的主机、同一个父分组下的子分组）的显示顺序：`sort > 0` 的排前面、
 * 按 `sort` 从小到大；`sort == 0`（没调整过的）排后面、按名字（{@link compareNames}）；
 * `sort` 相同时也用名字兜底。全是 0 的时候和原来的纯按名字完全一样。
 */
export function compareOrder(a: { sort: number; name: string }, b: { sort: number; name: string }): number {
  const aSet = a.sort > 0;
  const bSet = b.sort > 0;
  if (aSet !== bSet) return aSet ? -1 : 1;
  if (aSet && a.sort !== b.sort) return a.sort - b.sort;
  return compareNames(a.name, b.name);
}

export function groupKey(groupId: number | null): string {
  return groupId === null ? UNGROUPED_KEY : `g${groupId}`;
}

export function parseExpanded(raw: string): Set<string> {
  return new Set(
    raw
      .split(",")
      .map((part) => part.trim())
      .filter((part) => part === UNGROUPED_KEY || /^g\d+$/.test(part)),
  );
}

export function serializeExpanded(expanded: Set<string>): string {
  return [...expanded].sort().join(",");
}

export function toggleExpanded(expanded: Set<string>, key: string): Set<string> {
  const next = new Set(expanded);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  return next;
}

/** 搜索框里的文字匹配这台主机（名字、地址、用户名、标签）。空文字都匹配。 */
export function hostMatches(host: Host, text: string): boolean {
  const query = text.trim().toLowerCase();
  if (query === "") return true;
  return (
    host.name.toLowerCase().includes(query) ||
    host.host.toLowerCase().includes(query) ||
    host.username.toLowerCase().includes(query) ||
    host.tags.some((tag) => tag.toLowerCase().includes(query))
  );
}

/** 最多记几台最近打开的主机（再早的回到按名字排）。 */
export const RECENT_HOSTS_MAX = 20;

/** `layout.recent_hosts` → 主机 id 列表（最近的在前）。 */
export function parseRecent(raw: string): number[] {
  return raw
    .split(",")
    .map((part) => Number(part.trim()))
    .filter((id) => Number.isInteger(id) && id > 0);
}

/** 打开了一台主机：把它挪到最前面，超出上限的丢掉。 */
export function recordRecent(raw: string, hostId: number): string {
  const rest = parseRecent(raw).filter((id) => id !== hostId);
  return [hostId, ...rest].slice(0, RECENT_HOSTS_MAX).join(",");
}

/** 主机页卡片 / 列表的顺序：最近打开的在前（越近越前），其余按名字（{@link compareNames}）。 */
export function sortByRecent(hosts: Host[], recent: number[]): Host[] {
  const rank = new Map(recent.map((id, index) => [id, index]));
  return [...hosts].sort((a, b) => {
    const ra = rank.get(a.id);
    const rb = rank.get(b.id);
    if (ra !== undefined || rb !== undefined) {
      if (ra === undefined) return 1;
      if (rb === undefined) return -1;
      return ra - rb;
    }
    return compareNames(a.name, b.name);
  });
}

/** 主机树搜索时「未分组」那一行的名字（名字本身也能被搜到）。 */
export const UNGROUPED_LABEL = "未分组";

/**
 * 主机树搜索框的过滤结果（UI-7）。没在搜索时是 `null`。
 * - 主机显示：自己匹配（名称 / 地址 / 用户名 / 标签），或所在分组（含上级分组）的名字匹配；
 * - 分组显示：名字匹配（连同整个子树）、或子树里有显示的主机；显示的分组一律自动展开，
 *   不碰用户自己的展开状态，清空搜索就恢复原样。
 */
export interface TreeFilter {
  /** 去掉首尾空白并转小写的搜索词，用来高亮。 */
  text: string;
  hostIds: Set<number>;
  groupIds: Set<number>;
  /** 「未分组」行要不要显示（有显示的未分组主机，或它的名字匹配）。 */
  ungrouped: boolean;
  /** 每个显示的分组（含子分组）里显示的主机数。 */
  counts: Map<number, number>;
  /** 显示的主机总数。 */
  total: number;
}

export function treeFilterFor(groups: Group[], hosts: Host[], rawText: string): TreeFilter | null {
  const text = rawText.trim().toLowerCase();
  if (text === "") return null;
  const parentOf = new Map(groups.map((group) => [group.id, group.parent_id]));
  const nameOf = new Map(groups.map((group) => [group.id, group.name.toLowerCase()]));
  // 从这个分组一路往上（含自己），防环：最多走分组个数那么多步
  const chain = (groupId: number | null): number[] => {
    const ids: number[] = [];
    let current = groupId;
    for (let step = 0; current !== null && step <= groups.length; step += 1) {
      ids.push(current);
      current = parentOf.get(current) ?? null;
    }
    return ids;
  };
  const nameHit = (groupId: number): boolean => chain(groupId).some((id) => nameOf.get(id)?.includes(text));
  const ungroupedNameHit = UNGROUPED_LABEL.includes(text);

  const hostIds = new Set<number>();
  const groupIds = new Set<number>();
  const counts = new Map<number, number>();
  let ungrouped = ungroupedNameHit;
  for (const group of groups) {
    // 名字命中的分组连同它的整条祖先路径一起留下：嵌套在没命中的父分组里的（哪怕是空分组）也要看得到
    if (nameHit(group.id)) for (const id of chain(group.id)) groupIds.add(id);
  }
  for (const host of hosts) {
    const visible =
      hostMatches(host, text) ||
      (host.group_id === null ? ungroupedNameHit : nameHit(host.group_id));
    if (!visible) continue;
    hostIds.add(host.id);
    if (host.group_id === null) {
      ungrouped = true;
      continue;
    }
    for (const id of chain(host.group_id)) {
      groupIds.add(id);
      counts.set(id, (counts.get(id) ?? 0) + 1);
    }
  }
  return { text, hostIds, groupIds, ungrouped, counts, total: hostIds.size };
}

/** 某个分组（null = 未分组）下面直接挂的主机，按 {@link compareOrder} 排；搜索时只留过滤结果里的。 */
export function hostsUnder(hosts: Host[], groupId: number | null, filter: TreeFilter | null): Host[] {
  return hosts
    .filter((host) => host.group_id === groupId && (filter === null || filter.hostIds.has(host.id)))
    .sort(compareOrder);
}

/** 某个父分组（null = 根）下面直接的子分组，按 {@link compareOrder} 排；搜索时只留过滤结果里的。 */
export function groupsUnder(groups: Group[], parentId: number | null, filter: TreeFilter | null): Group[] {
  return groups
    .filter((group) => group.parent_id === parentId && (filter === null || filter.groupIds.has(group.id)))
    .sort(compareOrder);
}

/** 把 `movedId` 从列表里拿出来，插到 `targetId` 前面（`after` 为真则后面）。目标不在列表里返回 null。 */
export function insertInLayer(ids: number[], movedId: number, targetId: number, after: boolean): number[] | null {
  if (movedId === targetId) return null;
  const rest = ids.filter((id) => id !== movedId);
  const at = rest.indexOf(targetId);
  if (at < 0) return null;
  rest.splice(after ? at + 1 : at, 0, movedId);
  return rest;
}

/** 在列表里把 `id` 往前（-1）或往后（+1）挪一格；已经在头 / 尾、或不在列表里返回 null。 */
export function stepInLayer(ids: number[], id: number, delta: -1 | 1): number[] | null {
  const at = ids.indexOf(id);
  const to = at + delta;
  if (at < 0 || to < 0 || to >= ids.length) return null;
  const next = [...ids];
  [next[at], next[to]] = [next[to], next[at]];
  return next;
}

/**
 * 搜索结果里的主机，按树里从上到下的显示顺序排（先分组及其子分组、再其中的主机，
 * 最后是未分组），↑ ↓ 就是在这个顺序里移动。
 */
export function filteredHostOrder(groups: Group[], hosts: Host[], filter: TreeFilter): Host[] {
  const order: Host[] = [];
  const seen = new Set<number>();
  const walk = (parentId: number | null) => {
    for (const group of groupsUnder(groups, parentId, filter)) {
      if (seen.has(group.id)) continue;
      seen.add(group.id);
      walk(group.id);
      order.push(...hostsUnder(hosts, group.id, filter));
    }
  };
  walk(null);
  order.push(...hostsUnder(hosts, null, filter));
  return order;
}

/** 分组（含子分组）里一共有多少台主机。 */
export function totalUnder(groups: Group[], hosts: Host[], groupId: number): number {
  const childrenOf = new Map<number, number[]>();
  for (const group of groups) {
    if (group.parent_id === null) continue;
    childrenOf.set(group.parent_id, [...(childrenOf.get(group.parent_id) ?? []), group.id]);
  }
  const ids = new Set<number>();
  const stack = [groupId];
  while (stack.length > 0) {
    const id = stack.pop() as number;
    if (ids.has(id)) continue;
    ids.add(id);
    stack.push(...(childrenOf.get(id) ?? []));
  }
  return hosts.filter((host) => host.group_id !== null && ids.has(host.group_id)).length;
}
