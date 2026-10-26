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

/** 快速连接串 `user@host[:port]` / `user@[::1]:2222` 里的地址部分（规则同后端 `parse_quick_target`）。 */
export function quickTargetAddress(target: string): string | null {
  const at = target.trim().indexOf("@");
  if (at <= 0) return null;
  const rest = target.trim().slice(at + 1);
  if (rest.startsWith("[")) {
    const close = rest.indexOf("]");
    const inner = close > 0 ? rest.slice(1, close).trim() : "";
    return inner === "" ? null : inner;
  }
  const host = rest.split(":")[0];
  return host === "" ? null : host;
}

/**
 * 终端标签右键「复制地址」：标签里连的那台主机的地址，和主机树右键「复制地址」复制的一样
 * （主机库里的主机走 [`hostCopyText`]）。没连主机、或者主机已经从库里删掉了，返回 null。
 */
export function originAddress(
  origin:
    | { kind: "empty" }
    | { kind: "host"; hostId: number }
    | { kind: "quick"; target: string },
  hosts: ReadonlyArray<{ id: number; name: string; host: string }>,
): string | null {
  switch (origin.kind) {
    case "empty":
      return null;
    case "host": {
      const host = hosts.find((item) => item.id === origin.hostId);
      return host ? hostCopyText(host, "address") : null;
    }
    case "quick":
      return quickTargetAddress(origin.target);
  }
}
