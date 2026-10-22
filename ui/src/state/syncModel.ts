//! 同步页和状态栏用的纯函数：文案、强度、状态归类。不碰界面和后端，方便单测。

import type {
  ConflictView,
  PendingConfirmation,
  SyncStatusView,
} from "../api/sync";

export type Tone = "idle" | "ok" | "busy" | "warn" | "error";

export interface StatusBarModel {
  visible: boolean;
  tone: Tone;
  text: string;
}

const pad = (value: number) => String(value).padStart(2, "0");

/** 本地时间 `YYYY-MM-DD HH:mm`。 */
export function formatStamp(secs: number): string {
  const date = new Date(secs * 1000);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** 把后端的英文类型名变成界面用语。 */
export function entityLabel(entity: string): string {
  switch (entity) {
    case "host":
      return "主机";
    case "group":
      return "分组";
    case "identity":
      return "凭据";
    case "proxy":
      return "代理";
    case "known_host":
      return "主机指纹";
    case "setting":
      return "设置";
    default:
      return entity;
  }
}

/** 哪台电脑：本机，或者另一台（设备 ID 只显示前 8 位）。 */
export function deviceLabel(isLocal: boolean, device: string | null): string {
  if (isLocal) return "本机";
  if (!device) return "另一台电脑";
  return `另一台电脑（${device.slice(0, 8)}）`;
}

/** 各种存储里“被拒绝的那个凭据”叫什么。 */
function credentialName(status: SyncStatusView): string {
  switch (status.config?.provider) {
    case "github":
      return "令牌";
    case "s3":
      return "访问密钥";
    default:
      return "账号或密码";
  }
}

/**
 * 状态栏显示什么。失败只在这里和同步页显示，不弹窗。
 * 没连接存储时整条隐藏，不占地方。
 */
export function statusBarModel(status: SyncStatusView | null): StatusBarModel {
  if (!status || !status.configured) return { visible: false, tone: "idle", text: "" };
  if (status.running) return { visible: true, tone: "busy", text: "同步中…" };
  if (status.pending) {
    const prefix = status.pending.auto ? "自动同步已暂停：" : "";
    return {
      visible: true,
      tone: "warn",
      text: `${prefix}同步会删除 ${status.pending.deleted_records} 条记录，等待你确认`,
    };
  }
  if (status.baseline_broken) {
    return { visible: true, tone: "error", text: "同步基线损坏，需要重建后才能同步" };
  }
  if (!status.ready) {
    return { visible: true, tone: "warn", text: "同步：还没有设置主密码" };
  }
  if (!status.password_in_memory) {
    return { visible: true, tone: "warn", text: "同步：需要输入主密码" };
  }
  if (status.auth_paused) {
    if (status.config?.provider === "onedrive") {
      return {
        visible: true,
        tone: "error",
        text: "同步已暂停：OneDrive 登录已失效，请重新登录",
      };
    }
    return {
      visible: true,
      tone: "error",
      text: `同步已暂停：${credentialName(status)}已失效，请重新填写`,
    };
  }
  const last = status.last;
  if (!last) return { visible: true, tone: "idle", text: "同步：尚未同步" };
  if (last.status === "error") {
    return { visible: true, tone: "error", text: `同步失败：${last.summary}` };
  }
  if (last.status === "local_changed") {
    return { visible: true, tone: "idle", text: "同步期间有改动，稍后自动重新同步" };
  }
  if (last.status === "cancelled") {
    return { visible: true, tone: "idle", text: "上次同步已取消" };
  }
  const conflicts = status.conflict_count > 0 ? `，${status.conflict_count} 条冲突待处理` : "";
  return {
    visible: true,
    tone: status.conflict_count > 0 ? "warn" : "ok",
    text: `已同步 ${formatStamp(last.at).slice(11)}${conflicts}`,
  };
}

/** 同步页“上次同步”一行。 */
export function lastResultText(status: SyncStatusView): string {
  const last = status.last;
  if (!last) return "还没有同步过";
  const when = formatStamp(last.at);
  const how = last.auto ? "（自动）" : "";
  switch (last.status) {
    case "completed": {
      const version = last.version ? `，上传版本 ${last.version}` : "";
      return `${when}${how} 同步成功${version}`;
    }
    case "needs_confirmation":
      return `${when}${how} 停在了防误删确认上`;
    case "cancelled":
      return `${when} 已取消`;
    case "local_changed":
      return `${when}${how} 同步期间本地有改动，稍后自动重新同步`;
    case "error":
      return `${when}${how} 失败：${last.summary}`;
  }
}

export interface Strength {
  /** 0 空 · 1 太弱 · 2 弱 · 3 一般 · 4 强 */
  score: 0 | 1 | 2 | 3 | 4;
  label: string;
}

/** 主密码强度：只看长度和字符种类，是提醒不是门槛（门槛是后端的最短长度）。 */
export function passwordStrength(password: string): Strength {
  const length = [...password].length;
  if (length === 0) return { score: 0, label: "" };
  let kinds = 0;
  if (/[a-z]/.test(password)) kinds += 1;
  if (/[A-Z]/.test(password)) kinds += 1;
  if (/[0-9]/.test(password)) kinds += 1;
  if (/[^A-Za-z0-9]/.test(password)) kinds += 1;
  if (length < 8) return { score: 1, label: "太弱" };
  if (length >= 16 || (length >= 12 && kinds >= 3)) return { score: 4, label: "强" };
  if (length >= 12 || kinds >= 3) return { score: 3, label: "一般" };
  return { score: 2, label: "弱" };
}

/** 并发保护的提示。没检测过 / 不支持都要明说，不能悄悄当成支持。 */
export function concurrencyNotice(
  status: SyncStatusView,
): { tone: Tone; text: string } | null {
  if (!status.configured) return null;
  if (!status.concurrency) {
    return {
      tone: "warn",
      text: "还没有检测这个存储是否支持并发保护，同步会按“写之前再读一次”处理。点“测试连接”检测。",
    };
  }
  if (!status.concurrency.protected) {
    return {
      tone: "warn",
      text: "这个服务不支持并发保护，多台电脑同时同步时有很小的概率覆盖。",
    };
  }
  return { tone: "ok", text: "支持并发保护（多台电脑同时同步不会互相覆盖）。" };
}

/** 防误删对话框的正文：数量 + 前几条的名字。 */
export function deletionLines(pending: PendingConfirmation): {
  headline: string;
  names: string[];
  more: number;
} {
  const names = pending.samples.map((sample) => `${entityLabel(sample.entity)}：${sample.name}`);
  return {
    headline: `这次同步会删除 ${pending.deleted_records} 条记录（本地共 ${pending.local_records} 条），其中主机 ${pending.deleted_hosts} 台（本地共 ${pending.local_hosts} 台）。`,
    names,
    more: Math.max(0, pending.deleted_records - names.length),
  };
}

/** 冲突行里“保留的是什么”。 */
export function keptText(conflict: ConflictView): string {
  if (conflict.deletion) return "删除（这条记录在另一边已被删除）";
  return `${deviceLabel(conflict.kept_is_local, conflict.kept_device)} 的改动：${conflict.kept_summary}`;
}

/** 冲突行里“另一份是什么”。 */
export function otherText(conflict: ConflictView): string {
  const who = deviceLabel(conflict.other_is_local, conflict.other_device);
  return conflict.deletion
    ? `${who} 最后的内容：${conflict.other_summary}`
    : `${who} 的改动：${conflict.other_summary}`;
}
