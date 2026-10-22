//! 「属性」对话框的纯逻辑（M3-5 第 4 节）：要显示哪些行、大小怎么写、"计算大小"的状态怎么随事件走。

import type { SftpEntry, SftpSizeEvent, SizeTotals } from "../api/sftp";
import { formatPermissions, formatSize, formatTime } from "./sftpPane";

export const EMPTY_TOTALS: SizeTotals = { files: 0, dirs: 0, bytes: 0, links: 0, unreadable: 0 };

/** 属性对话框的对象：右键 / Alt+Enter 选中的条目；点在空白处就是当前目录本身。 */
export interface PropertiesTarget {
  name: string;
  path: string;
  kind: SftpEntry["kind"];
  size?: number;
  mtime?: number;
  permissions?: number;
  owner?: string;
  link_target?: string;
  /** 符号链接指向的类型。 */
  target_kind?: "dir" | "file";
}

function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed || path;
}

/** 选中的条目（没选 = 当前目录）→ 对话框的对象列表。 */
export function propertiesTargets(entries: SftpEntry[], currentDir: string): PropertiesTarget[] {
  if (entries.length === 0) {
    return currentDir ? [{ name: baseName(currentDir), path: currentDir, kind: "dir" }] : [];
  }
  return entries
    .filter((entry) => entry.kind !== "drive")
    .map((entry) => ({
      name: entry.name,
      path: entry.path,
      kind: entry.kind,
      size: entry.size,
      mtime: entry.mtime,
      permissions: entry.permissions,
      owner: entry.owner,
      link_target: entry.link_target,
      target_kind: entry.target_kind,
    }));
}

/** 这个对象要不要给"计算大小"（文件夹，或者指向文件夹的链接不跟随——只算文件夹）。 */
export function canCalculate(target: PropertiesTarget): boolean {
  return target.kind === "dir";
}

export function kindLabel(target: PropertiesTarget): string {
  switch (target.kind) {
    case "dir":
      return "文件夹";
    case "file":
      return "文件";
    case "symlink":
      return target.link_target ? `符号链接 → ${target.link_target}` : "符号链接";
    case "drive":
      return "磁盘";
    default:
      return "其他";
  }
}

/** 字节数的完整写法：`1.5 MB（1,572,864 字节）`；不到 1 KB 就只写字节。 */
export function bytesText(bytes: number): string {
  if (bytes < 1024) return `${bytes} 字节`;
  return `${formatSize(bytes)}（${bytes.toLocaleString("en-US")} 字节）`;
}

/** `3 个文件，2 个文件夹`（根目录自己不算在里面）。 */
export function countsText(totals: SizeTotals): string {
  const folders = Math.max(0, totals.dirs - 1);
  return `${totals.files} 个文件，${folders} 个文件夹`;
}

/** 多个对象的合计。 */
export function sumTotals(list: SizeTotals[]): SizeTotals {
  return list.reduce(
    (sum, item) => ({
      files: sum.files + item.files,
      dirs: sum.dirs + item.dirs,
      bytes: sum.bytes + item.bytes,
      links: sum.links + item.links,
      unreadable: sum.unreadable + item.unreadable,
    }),
    { ...EMPTY_TOTALS },
  );
}

/** 一个对象的"计算大小"状态。 */
export interface CalcState {
  phase: "idle" | "running" | "done" | "cancelled" | "failed";
  /**
   * 发起计算时前端生成的关联 ID。事件在命令返回任务号之前就可能到，光靠 `calcId`
   * 分不清"还没认领"的状态是谁的：同时算两个文件夹时，先到的事件会被两边都认领。
   */
  requestId?: string;
  calcId?: number;
  totals: SizeTotals;
  error?: string;
}

export const IDLE_CALC: CalcState = { phase: "idle", totals: EMPTY_TOTALS };

/** 事件 → 新状态。只接受关联 ID 对得上的事件。
 * 已经结束（完成 / 取消 / 失败）的不会被后到的进度事件改回"进行中"。 */
export function applySizeEvent(state: CalcState, event: SftpSizeEvent): CalcState {
  if (state.requestId === undefined || state.requestId !== event.request_id) return state;
  if (state.calcId !== undefined && state.calcId !== event.calc_id) return state;
  if (state.phase === "done" || state.phase === "failed") return state;
  if (state.phase === "cancelled" && event.state === "running") return state;
  const totals: SizeTotals = {
    files: event.files,
    dirs: event.dirs,
    bytes: event.bytes,
    links: event.links,
    unreadable: event.unreadable,
  };
  return {
    phase: event.state,
    requestId: state.requestId,
    calcId: event.calc_id,
    totals,
    error: event.error ?? undefined,
  };
}

export interface PropertyRow {
  label: string;
  value: string;
}

/** 单个对象的信息行（不含"大小"里和计算有关的部分，那个由对话框按状态补）。 */
export function infoRows(target: PropertiesTarget, side: "local" | "remote"): PropertyRow[] {
  const rows: PropertyRow[] = [
    { label: "名称", value: target.name },
    { label: "位置", value: target.path },
    { label: "类型", value: kindLabel(target) },
  ];
  if (target.kind === "file" && target.size !== undefined) {
    rows.push({ label: "大小", value: bytesText(target.size) });
  }
  rows.push({ label: "修改时间", value: formatTime(target.mtime) });
  if (side === "remote") {
    rows.push({ label: "权限", value: formatPermissions(target.permissions) });
    if (target.owner) rows.push({ label: "所有者", value: target.owner });
  }
  return rows;
}

/** "计算大小"那一行的文字。 */
export function calcText(state: CalcState): string {
  const base = `${bytesText(state.totals.bytes)}，${countsText(state.totals)}`;
  const extras: string[] = [];
  if (state.totals.links > 0) extras.push(`跳过 ${state.totals.links} 个符号链接`);
  if (state.totals.unreadable > 0) extras.push(`${state.totals.unreadable} 个文件夹读不了`);
  const suffix = extras.length > 0 ? `（${extras.join("，")}）` : "";
  switch (state.phase) {
    case "idle":
      return "未计算";
    case "running":
      return `计算中…已数到 ${base}${suffix}`;
    case "cancelled":
      return `已取消，取消前数到 ${base}${suffix}（不是完整结果）`;
    case "failed":
      return `计算失败：${state.error ?? "未知错误"}`;
    default:
      return `${base}${suffix}`;
  }
}
