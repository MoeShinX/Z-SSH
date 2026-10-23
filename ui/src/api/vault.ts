//! 主机库、凭据、代理的接口封装。
//!
//! 字段名跟 Rust 那边保持一致（snake_case），命令参数名按 Tauri 的约定用
//! camelCase。

import { invoke } from "@tauri-apps/api/core";

export type AuthMethod = "password" | "key" | "agent";
/** 主机用不用代理：跟随全局默认 / 不用 / 用 proxy_id 指定的那个。 */
export type ProxyMode = "default" | "none" | "custom";
export type IdentityKind = "password" | "key_file" | "key_inline" | "agent";
export type ProxyKind = "socks5" | "http";

export interface Group {
  id: number;
  name: string;
  parent_id: number | null;
  sort: number;
}

export interface GroupDraft {
  id: number | null;
  name: string;
  parent_id: number | null;
  sort: number;
}

export interface Host {
  id: number;
  group_id: number | null;
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: AuthMethod;
  identity_id: number | null;
  proxy_id: number | null;
  jump_host_id: number | null;
  proxy_mode: ProxyMode;
  tags: string[];
  sort: number;
  /** 旧算法兼容开关，只给老服务器用 */
  legacy_algorithms: boolean;
  created_at: number;
  updated_at: number;
}

export interface HostDraft {
  id: number | null;
  group_id: number | null;
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: AuthMethod;
  identity_id: number | null;
  proxy_id: number | null;
  jump_host_id: number | null;
  proxy_mode: ProxyMode;
  tags: string[];
  sort: number;
  legacy_algorithms: boolean;
}

/** 凭据。**不含密码**，只有密码在不在保险库里。 */
export interface Identity {
  id: number;
  name: string;
  kind: IdentityKind;
  key_path: string | null;
  has_secret: boolean;
}

export interface IdentityDraft {
  id: number | null;
  name: string;
  kind: IdentityKind;
  key_path: string | null;
  /** null = 不修改已存的；"" = 清掉；其它 = 覆盖。 */
  secret: string | null;
  /** 只有 key_inline 用：和私钥内容一起存进口令 */
  passphrase?: string | null;
}

export interface Proxy {
  id: number;
  name: string;
  kind: ProxyKind;
  host: string;
  port: number;
  username: string | null;
  has_secret: boolean;
}

export interface ProxyDraft {
  id: number | null;
  name: string;
  kind: ProxyKind;
  host: string;
  port: number;
  username: string | null;
  secret: string | null;
}

export const listGroups = (): Promise<Group[]> => invoke("group_list");
export const saveGroup = (draft: GroupDraft): Promise<Group> =>
  invoke("group_save", { draft });
export const deleteGroup = (id: number): Promise<void> =>
  invoke("group_delete", { id });

export const listHosts = (): Promise<Host[]> => invoke("host_list");
export const saveHost = (draft: HostDraft): Promise<Host> =>
  invoke("host_save", { draft });
export const deleteHost = (id: number): Promise<void> =>
  invoke("host_delete", { id });

/** 把一层主机（同一个分组下 / 都未分组）按这个顺序排成 1..n；一个事务，不合法就整个不改。 */
export const reorderHosts = (ids: number[]): Promise<void> => invoke("host_reorder", { ids });
/** 把一层分组（同一个父分组下）按这个顺序排成 1..n。 */
export const reorderGroups = (ids: number[]): Promise<void> => invoke("group_reorder", { ids });
/** 「按名字重新排序」：分组下直接的主机和子分组回到按名字；null = 只管未分组主机。 */
export const resetOrder = (groupId: number | null): Promise<void> => invoke("order_reset", { groupId });

export const listIdentities = (): Promise<Identity[]> => invoke("identity_list");
export const saveIdentity = (draft: IdentityDraft): Promise<Identity> =>
  invoke("identity_save", { draft });
export const deleteIdentity = (id: number): Promise<void> =>
  invoke("identity_delete", { id });

export const listProxies = (): Promise<Proxy[]> => invoke("proxy_list");

/** 全局默认代理的 id（null 表示没设）。 */
export const defaultProxyGet = (): Promise<number | null> =>
  invoke("default_proxy_get");

export const defaultProxySet = (proxyId: number | null): Promise<void> =>
  invoke("default_proxy_set", { proxyId });
export const saveProxy = (draft: ProxyDraft): Promise<Proxy> =>
  invoke("proxy_save", { draft });
export const deleteProxy = (id: number): Promise<void> =>
  invoke("proxy_delete", { id });

/** 生成密钥的算法。 */
export type KeyAlgorithm = "ed25519" | "ecdsa_p256" | "rsa4096";

/** 在 Z-SSH 里生成一把新密钥，存成"导入的私钥"凭据。 */
export const identityGenerate = (args: {
  name: string;
  algorithm: KeyAlgorithm;
  comment: string;
  passphrase: string | null;
}): Promise<Identity> => invoke("identity_generate", args);

export interface PublicKeyView {
  public_key: string;
  fingerprint: string;
}

/** 私钥凭据的公钥（OpenSSH 一行格式）和 SHA256 指纹。 */
export const identityPublicKey = (id: number): Promise<PublicKeyView> =>
  invoke("identity_public_key", { id });

/** 把公钥装到主机的 ~/.ssh/authorized_keys；已经有就不重复加。 */
export const hostInstallPublicKey = (
  hostId: number,
  identityId: number,
): Promise<"added" | "already_present"> =>
  invoke("host_install_public_key", { hostId, identityId });

/** Host → 保存用的 HostDraft（改某个字段后存回去用）。 */
export function draftOfHost(host: Host): HostDraft {
  return {
    id: host.id,
    group_id: host.group_id,
    name: host.name,
    host: host.host,
    port: host.port,
    username: host.username,
    auth_method: host.auth_method,
    identity_id: host.identity_id,
    proxy_id: host.proxy_id,
    jump_host_id: host.jump_host_id,
    proxy_mode: host.proxy_mode,
    tags: host.tags,
    sort: host.sort,
    legacy_algorithms: host.legacy_algorithms,
  };
}

/** AI 记下的主机笔记（只存本机）。 */
export interface HostMemory {
  content: string;
  /** Unix 秒 */
  updated_at: number;
}

export const hostMemoryGet = (hostId: number): Promise<HostMemory | null> =>
  invoke("host_memory_get", { hostId });

/** 改 / 清空（空串）笔记；敏感值会被隐藏，太长会报错。 */
export const hostMemorySet = (hostId: number, content: string): Promise<void> =>
  invoke("host_memory_set", { hostId, content });
