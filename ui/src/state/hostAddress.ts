//! 主机地址的小工具：IPv6 字面量的方括号、右键「复制主机名 / 复制地址」的文字。

/** 右键菜单里复制的是什么：主机名（列表里显示的名字）还是地址（IP 或域名）。 */
export type HostCopyKind = "name" | "address";

/** 只认地址部分有冒号的写法：普通主机名和 IPv4 没有冒号，有冒号的只能是 IPv6（可能带 zone id）。 */
function isIpv6Literal(address: string): boolean {
  return address.includes(":");
}

/**
 * 保存主机前整理用户填的地址：去掉首尾空白；IPv6 写成 `[2001:db8::1]` 的去掉方括号。
 * 存进库的是裸地址：连接时按地址解析、known_hosts 按 `[地址]:端口` 拼，带着方括号都会出错。
 */
export function normalizeHostInput(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    const inner = trimmed.slice(1, -1).trim();
    if (isIpv6Literal(inner)) return inner;
  }
  return trimmed;
}

/** 右键复制的文字：主机名，或者只有地址本身（不带用户名和端口；老数据里带方括号的 IPv6 去掉方括号）。 */
export function hostCopyText(host: { name: string; host: string }, kind: HostCopyKind): string {
  return kind === "name" ? host.name : normalizeHostInput(host.host);
}
