//! 主机右键「复制密码」「复制连接信息」：什么时候显示、点了之后调用哪个命令、提示什么。
//!
//! 密码只在后端：命令在后端取密码并直接写进系统剪贴板，这里拿到的只有成功 / 失败，
//! 前端的状态、日志、事件里都不会出现密码。

import * as api from "../api/vault";
import type { Host, Identity } from "../api/vault";

/**
 * 「复制密码」要不要显示：主机是密码登录、引用了密码类凭据、且凭据里存了密码。
 * 和后端取密码的规则一致（后端还会再核一遍，这里只决定菜单项显不显示）。
 */
export function canCopyPassword(
  host: Pick<Host, "auth_method" | "identity_id">,
  identities: readonly Identity[],
): boolean {
  if (host.auth_method !== "password" || host.identity_id === null) return false;
  const identity = identities.find((item) => item.id === host.identity_id);
  return identity !== undefined && identity.kind === "password" && identity.has_secret;
}

/** 复制的结果：`message` 直接拿去做提示，`ok` 决定是成功还是失败样式。 */
export interface CopyResult {
  ok: boolean;
  message: string;
}

function reasonOf(error: unknown): string {
  if (typeof error === "string") return error;
  return error instanceof Error ? error.message : String(error);
}

export async function copyPasswordToClipboard(hostId: number): Promise<CopyResult> {
  try {
    await api.copyHostPassword(hostId);
    return { ok: true, message: "已复制密码，30 秒后自动清除" };
  } catch (error) {
    return { ok: false, message: `复制密码失败：${reasonOf(error)}` };
  }
}

export async function copyInfoToClipboard(hostId: number): Promise<CopyResult> {
  try {
    const { with_password } = await api.copyHostInfo(hostId);
    return {
      ok: true,
      message: with_password ? "已复制连接信息（含密码），30 秒后自动清除" : "已复制连接信息",
    };
  } catch (error) {
    return { ok: false, message: `复制连接信息失败：${reasonOf(error)}` };
  }
}
