//! 主机树里「上移 / 下移」和「按名字重新排序」（UI-9）：右键菜单用。
//!
//! 同一层 = 同一个分组下直接挂的主机（未分组算一层）/ 同一个父分组下的子分组（根算一层）。
//! 调整时把**整层**按新的显示顺序传给后端，后端在一个事务里重新编号成 1..n。
//! 显示顺序的规则见 `state/hostTree.ts` 的 `compareOrder`。

import type { Group, Host } from "../../api/vault";
import { groupsUnder, hostsUnder, stepInLayer } from "../../state/hostTree";
import { useVault } from "../../stores/vault";
import { toast } from "../ui/toast";

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 这台主机所在那一层现在的显示顺序（主机 id）。 */
export function hostLayerIds(hosts: Host[], groupId: number | null): number[] {
  return hostsUnder(hosts, groupId, null).map((host) => host.id);
}

/** 这个分组所在那一层现在的显示顺序（分组 id）。 */
export function groupLayerIds(groups: Group[], parentId: number | null): number[] {
  return groupsUnder(groups, parentId, null).map((group) => group.id);
}

/** 上移 / 下移还能不能动：[能上移, 能下移]。 */
export function canStepHost(hosts: Host[], host: Host): [boolean, boolean] {
  const ids = hostLayerIds(hosts, host.group_id);
  return [stepInLayer(ids, host.id, -1) !== null, stepInLayer(ids, host.id, 1) !== null];
}

export function canStepGroup(groups: Group[], group: Group): [boolean, boolean] {
  const ids = groupLayerIds(groups, group.parent_id);
  return [stepInLayer(ids, group.id, -1) !== null, stepInLayer(ids, group.id, 1) !== null];
}

export async function stepHost(host: Host, delta: -1 | 1): Promise<void> {
  const { hosts, reorderHosts } = useVault.getState();
  // 用库里最新的所在分组：菜单打开期间主机可能已经被挪走
  const current = hosts.find((item) => item.id === host.id) ?? host;
  const next = stepInLayer(hostLayerIds(hosts, current.group_id), current.id, delta);
  if (!next) return;
  try {
    await reorderHosts(next);
  } catch (error) {
    toast.error(`调整顺序失败：${errorText(error)}`);
  }
}

export async function stepGroup(group: Group, delta: -1 | 1): Promise<void> {
  const { groups, reorderGroups } = useVault.getState();
  const current = groups.find((item) => item.id === group.id) ?? group;
  const next = stepInLayer(groupLayerIds(groups, current.parent_id), current.id, delta);
  if (!next) return;
  try {
    await reorderGroups(next);
  } catch (error) {
    toast.error(`调整顺序失败：${errorText(error)}`);
  }
}

/** 「按名字重新排序」：这个分组下直接的主机和子分组回到按名字；null = 只管未分组的主机。 */
export async function resetLayerOrder(groupId: number | null): Promise<void> {
  try {
    await useVault.getState().resetOrder(groupId);
    toast.success("已按名字重新排序");
  } catch (error) {
    toast.error(`重新排序失败：${errorText(error)}`);
  }
}
