//! 主机页右边卡片 / 列表的显示选项（UI-16）：疏密、排序方式，以及按排序方式给主机排序的纯函数。
//! 两项都是本机布局设置（`layout.*`，不同步）。

import type { Group, Host } from "../api/vault";
import { compareNames, compareOrder, filteredHostOrder, hostMatches, sortByRecent, type TreeFilter } from "./hostTree";

/** 卡片 / 列表的疏密：compact 紧凑（默认，一屏多放几台）/ comfortable 舒适（原来的样子）。 */
export type VaultDensity = "compact" | "comfortable";

/** `layout.vault_density` → 疏密；读不懂的值按默认（紧凑）。 */
export function parseDensity(raw: unknown): VaultDensity {
  return raw === "comfortable" ? "comfortable" : "compact";
}

/** 排序方式：manual 手动（和左边的树同一个顺序，默认）/ recent 最近打开 / name 名称。 */
export type VaultSortMode = "manual" | "recent" | "name";

export const SORT_MODES: VaultSortMode[] = ["manual", "recent", "name"];

export const SORT_LABELS: Record<VaultSortMode, string> = {
  manual: "手动",
  recent: "最近打开",
  name: "名称",
};

/** `layout.vault_sort_mode` → 排序方式；读不懂的值按默认（手动）。 */
export function parseSortMode(raw: unknown): VaultSortMode {
  return raw === "recent" || raw === "name" ? raw : "manual";
}

/** 排序按钮点一下切到下一种：手动 → 最近打开 → 名称 → 手动。 */
export function nextSortMode(mode: VaultSortMode): VaultSortMode {
  return SORT_MODES[(SORT_MODES.indexOf(mode) + 1) % SORT_MODES.length];
}

/** 右边选中的分组：全部主机 / 未分组（null）/ 某个分组。 */
export type PageGroup = number | null | "all";

/** 这个分组连同它所有子孙分组的 id（防环：每个分组只走一次）。 */
export function groupWithDescendants(groups: Group[], groupId: number): Set<number> {
  const ids = new Set<number>();
  const stack = [groupId];
  while (stack.length > 0) {
    const id = stack.pop() as number;
    if (ids.has(id)) continue;
    ids.add(id);
    for (const group of groups) if (group.parent_id === id) stack.push(group.id);
  }
  return ids;
}

/**
 * 主机页右边卡片 / 列表要显示的主机和它们的顺序。
 * - 范围：选了分组就是它和所有子分组里的主机（和左边树里分组上的数字一致），未分组只看顶层的；
 *   搜索框里有字再按名称 / 地址 / 用户名 / 标签过滤；
 * - `manual`：和左边的树完全一致（先分组及其子分组、再其中的主机，不分组的在最后，每层按手动顺序）；
 * - `recent`：最近打开的在前，其余按名字；`name`：按名字。
 */
export function pageHosts(
  groups: Group[],
  hosts: Host[],
  options: { query: string; selectedGroup: PageGroup; mode: VaultSortMode; recent: number[] },
): Host[] {
  const { query, selectedGroup, mode, recent } = options;
  const scope =
    typeof selectedGroup === "number" ? groupWithDescendants(groups, selectedGroup) : null;
  const matched = hosts.filter((host) => {
    if (selectedGroup === null && host.group_id !== null) return false;
    if (scope && (host.group_id === null || !scope.has(host.group_id))) return false;
    return hostMatches(host, query);
  });
  if (mode === "recent") return sortByRecent(matched, recent);
  if (mode === "name") return [...matched].sort((a, b) => compareNames(a.name, b.name));
  const filter: TreeFilter = {
    text: "",
    hostIds: new Set(matched.map((host) => host.id)),
    groupIds: new Set(groups.map((group) => group.id)),
    ungrouped: true,
    counts: new Map(),
    total: matched.length,
  };
  const ordered = filteredHostOrder(groups, hosts, filter);
  // 分组数据不正常（上级分组不存在 / 成环）时，树走不到那些分组：主机照样列在最后，不能凭空消失
  const shown = new Set(ordered.map((host) => host.id));
  const orphans = matched.filter((host) => !shown.has(host.id)).sort(compareOrder);
  return orphans.length > 0 ? [...ordered, ...orphans] : ordered;
}
