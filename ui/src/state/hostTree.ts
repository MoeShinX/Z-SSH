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

/** 某个分组（null = 未分组）下面直接挂的主机，按名字排。 */
export function hostsUnder(hosts: Host[], groupId: number | null, text: string): Host[] {
  return hosts
    .filter((host) => host.group_id === groupId && hostMatches(host, text))
    .sort((a, b) => compareNames(a.name, b.name));
}

/**
 * 搜索时要自动展开的分组：里面（含子分组）有匹配的主机。没在搜索就是空集合
 * （按用户自己展开的来）。
 */
export function autoExpandedFor(groups: Group[], hosts: Host[], text: string): Set<string> {
  const result = new Set<string>();
  if (text.trim() === "") return result;
  const parentOf = new Map(groups.map((group) => [group.id, group.parent_id]));
  for (const host of hosts) {
    if (!hostMatches(host, text)) continue;
    if (host.group_id === null) {
      result.add(UNGROUPED_KEY);
      continue;
    }
    // 一路往上把祖先分组都展开（防环：最多走分组个数那么多步）
    let current: number | null = host.group_id;
    for (let step = 0; current !== null && step <= groups.length; step += 1) {
      result.add(groupKey(current));
      current = parentOf.get(current) ?? null;
    }
  }
  return result;
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
