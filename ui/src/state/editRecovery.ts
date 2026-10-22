//! 启动时"上次有没上传的修改"提示的纯逻辑（F05）。

import type { EditLeftover, SftpEditEvent } from "../api/sftp";

/** 提示的标题。 */
export function recoveryTitle(count: number): string {
  return `上次有 ${count} 个文件的修改没有上传`;
}

/** 这一项的说明：哪台主机的哪个文件；认不出来源就只说文件名。 */
export function recoveryLine(item: EditLeftover): string {
  if (item.local && item.remote_path) return `${item.remote_path}（本机文件）`;
  if (item.remote_path) {
    const host = item.host_label ? `${item.host_label}：` : "";
    return `${host}${item.remote_path}`;
  }
  return `${item.file_name}（认不出对应的服务器文件）`;
}

/** 一个正在用的 SFTP 会话：独立的 SFTP 标签，或者终端旁边的文件侧栏（停靠会话）。 */
export interface SftpSessionInfo {
  sftpId: string;
  hostId: number;
  /** 已经连好、能用了。 */
  ready: boolean;
  via?: "tab" | "dock";
}

/** 这台主机上任何一个已经连好的 SFTP 会话（标签、文件侧栏都算）。 */
export function sftpSessionFor(
  sessions: ReadonlyArray<SftpSessionInfo>,
  hostId: number | undefined,
): string | null {
  if (hostId === undefined) return null;
  return (
    sessions.find(
      (item) => item.hostId === hostId && item.ready && item.sftpId !== "",
    )?.sftpId ?? null
  );
}

/** 把 SFTP 标签和文件侧栏拼成一份会话清单。 */
export function collectSftpSessions(
  tabs: ReadonlyArray<{
    sftp?: { sftpId: string; hostId: number; phase: string };
  }>,
  docks: Readonly<Record<number, { sftpId: string; phase: string }>>,
): SftpSessionInfo[] {
  const out: SftpSessionInfo[] = [];
  for (const tab of tabs) {
    if (tab.sftp) {
      out.push({
        sftpId: tab.sftp.sftpId,
        hostId: tab.sftp.hostId,
        ready: tab.sftp.phase === "ready",
        via: "tab",
      });
    }
  }
  for (const [hostId, dock] of Object.entries(docks)) {
    out.push({
      sftpId: dock.sftpId,
      hostId: Number(hostId),
      ready: dock.phase === "ready" && dock.sftpId !== "",
      via: "dock",
    });
  }
  return out;
}

/** "重新上传"现在能不能点，不能点时给出原因。 */
export function reuploadState(
  item: EditLeftover,
  sftpId: string | null,
): { enabled: boolean; hint?: string } {
  if (item.local) return { enabled: true };
  if (item.host_id === undefined || !item.remote_path) {
    return {
      enabled: false,
      hint: "认不出对应的服务器文件，只能另存到本地或放弃",
    };
  }
  if (!sftpId) {
    return {
      enabled: false,
      hint: `先打开「${item.host_label ?? "那台主机"}」的 SFTP 标签，才能重新上传`,
    };
  }
  return { enabled: true };
}

/** 「重新上传」按钮上的字：本机文件是写回原文件。 */
export function reuploadLabel(item: EditLeftover): string {
  return item.local ? "写回原文件" : "重新上传";
}

/** 草稿（内置编辑器里没保存的内容）要说明一下，免得和「保存过但没上传」混淆。 */
export function draftNote(item: EditLeftover): string | null {
  return item.from_draft ? "这是内置编辑器里还没保存的内容（程序退出时来不及保存）。" : null;
}

/** 这一项为什么留下：没法确认有没有改过时要说清楚，让用户自己看一眼再决定。 */
export function uncertainNote(item: EditLeftover): string | null {
  return item.uncertain
    ? "没法确认这份文件有没有改过（读不了它，或者恢复记录不完整），所以保险起见留下了。请先看一眼内容再决定。"
    : null;
}

/** 编辑事件要不要弹提示、弹什么（上传成功 / 失败 / 放弃 / 恢复记录没写成）。 */
export function editToastFor(
  event: Pick<SftpEditEvent, "state" | "remote_path" | "message" | "owner">,
): { type: "success" | "info" | "error" | "warning"; text: string } | null {
  const name = event.remote_path.split("/").pop() ?? event.remote_path;
  switch (event.state) {
    case "uploaded":
      // 内容已经上传了，但属主没能保持：不能只显示"已上传"。
      if (event.owner) {
        return {
          type: "warning",
          text: `已上传「${name}」，但文件的属主从 ${event.owner.from} 变成了 ${event.owner.to}（没有权限改回去）。用原属主运行的服务可能读不了它。`,
        };
      }
      return { type: "success", text: `已上传「${name}」` };
    case "discarded":
      return {
        type: "info",
        text: `已放弃本地修改，「${name}」换回了服务器上的内容`,
      };
    case "failed":
      return {
        type: "error",
        text: `「${name}」上传失败：${event.message ?? "未知错误"}`,
      };
    case "warning":
      return {
        type: "warning",
        text: `「${name}」：${event.message ?? "出了点问题"}`,
      };
    default:
      return null;
  }
}

/** "保存后属主会变"那个询问里的说明文字。 */
export function ownerPromptText(event: Pick<SftpEditEvent, "owner">): string {
  const owner = event.owner;
  if (!owner) return "保存后文件的属主会改变。";
  return `保存后属主会从 ${owner.from} 变成 ${owner.to}（${owner.error}，没有权限改回去）。`;
}
