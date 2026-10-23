//! 主机库的前端状态。数据都在后端 SQLite 里，这里只缓存一份用来渲染。

import { create } from "zustand";

import * as api from "../api/vault";
import { compareNames } from "../state/hostTree";
import type {
  Group,
  GroupDraft,
  Host,
  HostDraft,
  Identity,
  IdentityDraft,
  Proxy,
  ProxyDraft,
} from "../api/vault";

interface VaultState {
  groups: Group[];
  hosts: Host[];
  identities: Identity[];
  proxies: Proxy[];
  loaded: boolean;
  load: () => Promise<void>;
  saveGroup: (draft: GroupDraft) => Promise<Group>;
  /** 一批主机移到某个分组（null = 未分组），全部存完再重新加载一次。 */
  moveHosts: (hostIds: number[], groupId: number | null) => Promise<void>;
  removeGroup: (id: number) => Promise<void>;
  /** 手动顺序（UI-9）：传整层的新顺序；存完重新加载。 */
  reorderHosts: (ids: number[]) => Promise<void>;
  reorderGroups: (ids: number[]) => Promise<void>;
  /** 「按名字重新排序」。 */
  resetOrder: (groupId: number | null) => Promise<void>;
  saveHost: (draft: HostDraft) => Promise<Host>;
  removeHost: (id: number) => Promise<void>;
  saveIdentity: (draft: IdentityDraft) => Promise<Identity>;
  removeIdentity: (id: number) => Promise<void>;
  saveProxy: (draft: ProxyDraft) => Promise<Proxy>;
  removeProxy: (id: number) => Promise<void>;
}

export const useVault = create<VaultState>((set, get) => ({
  groups: [],
  hosts: [],
  identities: [],
  proxies: [],
  loaded: false,

  async load() {
    const [groups, hosts, identities, proxies] = await Promise.all([
      api.listGroups(),
      api.listHosts(),
      api.listIdentities(),
      api.listProxies(),
    ]);
    // 列表、树、下拉框都用这份顺序：数字 → 英文 → 中文 → 其他符号（见 compareNames）
    groups.sort((a, b) => compareNames(a.name, b.name));
    hosts.sort((a, b) => compareNames(a.name, b.name));
    set({ groups, hosts, identities, proxies, loaded: true });
  },

  async saveGroup(draft) {
    const saved = await api.saveGroup(draft);
    await get().load();
    return saved;
  },

  async moveHosts(hostIds, groupId) {
    const wanted = new Set(hostIds);
    try {
      for (const host of get().hosts) {
        if (wanted.has(host.id) && host.group_id !== groupId) {
          await api.saveHost({ ...api.draftOfHost(host), group_id: groupId });
        }
      }
    } finally {
      await get().load();
    }
  },

  async removeGroup(id) {
    await api.deleteGroup(id);
    await get().load();
  },

  async reorderHosts(ids) {
    try {
      await api.reorderHosts(ids);
    } finally {
      await get().load();
    }
  },

  async reorderGroups(ids) {
    try {
      await api.reorderGroups(ids);
    } finally {
      await get().load();
    }
  },

  async resetOrder(groupId) {
    try {
      await api.resetOrder(groupId);
    } finally {
      await get().load();
    }
  },

  async saveHost(draft) {
    const saved = await api.saveHost(draft);
    await get().load();
    return saved;
  },

  async removeHost(id) {
    await api.deleteHost(id);
    await get().load();
  },

  async saveIdentity(draft) {
    const saved = await api.saveIdentity(draft);
    await get().load();
    return saved;
  },

  async removeIdentity(id) {
    await api.deleteIdentity(id);
    await get().load();
  },

  async saveProxy(draft) {
    const saved = await api.saveProxy(draft);
    await get().load();
    return saved;
  },

  async removeProxy(id) {
    await api.deleteProxy(id);
    await get().load();
  },
}));
