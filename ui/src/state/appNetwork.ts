//! 设置页"软件网络"：检查更新、配置同步、下载 opencode 走哪个代理（和 SSH 的默认代理分开）。

import type { SystemProxy } from "../api/settings";

/** 设置键，值是 JSON 字符串：`ssh_default` / `system` / `none` / `proxy:<id>`（后端 app_proxy.rs）。 */
export const APP_NETWORK_PROXY_KEY = "app_network_proxy";

/** 设置里存的原始值（JSON 文本）→ 下拉框的值。没设置或读不懂按"使用 SSH 默认代理"。 */
export function appProxyChoice(raw: string | null, proxyIds: number[]): string {
  let value: unknown = null;
  try {
    value = raw === null ? null : JSON.parse(raw);
  } catch {
    value = null;
  }
  if (value === "system" || value === "none") return value;
  if (typeof value === "string" && value.startsWith("proxy:")) {
    const id = Number(value.slice("proxy:".length));
    // 选中的代理被删了：后端读不到会报错，界面先回到默认，免得显示一个空选项
    if (Number.isInteger(id) && proxyIds.includes(id)) return value;
  }
  return "ssh_default";
}

/** 下拉框的值 → 存进设置的 JSON 文本。 */
export function appProxyValue(choice: string): string {
  return JSON.stringify(choice);
}

/** "跟随系统代理"时显示读到的系统代理。 */
export function systemProxyText(proxy: SystemProxy | null): string {
  if (!proxy) return "正在读取系统代理…";
  switch (proxy.kind) {
    case "manual":
      return `系统代理：${proxy.scheme === "socks5" ? "SOCKS5 " : ""}${proxy.host}:${proxy.port}`;
    case "auto_config":
      return "系统用的是自动配置脚本（PAC），暂不支持，按直连处理";
    default:
      return "系统没有设置代理，按直连处理";
  }
}
