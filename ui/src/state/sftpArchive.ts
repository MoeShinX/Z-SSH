//! 远端压缩 / 解压的纯逻辑（UI-13）：认压缩包后缀、同名文件夹名、提示条文案、结束提示。
//! 后缀和 `crates/zssh-core/src/sftp/archive.rs` 里的列表保持一致。

import type {
  ArchiveCaps,
  CompressResult,
  ExtractPlan,
  ExtractResult,
  SftpEntry,
} from "../api/sftp";
import { formatSize } from "./sftpPane";

export type ArchiveFormatId = ExtractPlan["format"];

/** 后缀（小写）→ 格式。顺序无所谓：没有一个后缀是另一个的结尾。 */
const SUFFIXES: Array<[string, ArchiveFormatId]> = [
  [".tar.gz", "tar_gz"],
  [".tgz", "tar_gz"],
  [".tar.xz", "tar_xz"],
  [".txz", "tar_xz"],
  [".tar.bz2", "tar_bz2"],
  [".tbz2", "tar_bz2"],
  [".tar", "tar"],
  [".zip", "zip"],
];

function splitArchiveName(name: string): [ArchiveFormatId, string] | null {
  const lower = name.toLowerCase();
  for (const [suffix, format] of SUFFIXES) {
    if (lower.endsWith(suffix) && name.length > suffix.length) {
      return [format, name.slice(0, name.length - suffix.length)];
    }
  }
  return null;
}

/** 按文件名后缀认压缩包格式（不分大小写）；认不出返回 null。 */
export function archiveFormatOf(name: string): ArchiveFormatId | null {
  return splitArchiveName(name)?.[0] ?? null;
}

/** 「解压到「同名文件夹」」的文件夹名：`site.tar.gz` → `site`。主名为空或是 `.` / `..` 时没有。 */
export function extractFolderName(name: string): string | null {
  const parts = splitArchiveName(name);
  if (!parts) return null;
  const stem = parts[1].trimEnd();
  if (stem === "" || stem === "." || stem === ".." || stem.includes("/")) return null;
  return stem;
}

/** 是不是能解压的条目：文件（或指向文件的链接）、名字合法、后缀认得。 */
export function isExtractable(entry: SftpEntry): boolean {
  if (!entry.name_is_utf8) return false;
  const fileLike = entry.kind === "file" || (entry.kind === "symlink" && entry.target_kind === "file");
  return fileLike && archiveFormatOf(entry.name) !== null;
}

/** 解压这种格式服务器上要有什么命令；缺了返回要告诉用户的原因，够用返回 null。 */
export function extractToolReason(format: ArchiveFormatId, caps: ArchiveCaps): string | null {
  if (format === "zip") return caps.unzip ? null : "服务器上没有 unzip 命令";
  return caps.tar ? null : "服务器上没有 tar 命令";
}

/** 压缩成这种格式服务器上要有什么命令。 */
export function compressToolReason(format: "tar_gz" | "zip", caps: ArchiveCaps): string | null {
  if (format === "zip") return caps.zip ? null : "服务器上没有 zip 命令";
  return caps.tar ? null : "服务器上没有 tar 命令";
}

/** 提示条里正在做什么。 */
export type BusyKind = "paste" | "compress" | "scan" | "extract";

/** 压缩 / 检查 / 解压的提示条文案（复制的那条在 `sftpPaste.ts`）。 */
export function archiveBusyText(
  kind: Exclude<BusyKind, "paste">,
  elapsedSeconds: number,
  bytes?: number | null,
): string {
  const elapsed = `已 ${elapsedSeconds} 秒`;
  switch (kind) {
    case "compress":
      return bytes === undefined || bytes === null
        ? `压缩中…（${elapsed}）`
        : `压缩中…（已写入 ${formatSize(bytes)} · ${elapsed}）`;
    case "scan":
      return `检查压缩包中…（${elapsed}）`;
    case "extract":
      return `解压中…（${elapsed}）`;
  }
}

export interface ArchiveNotice {
  type: "success" | "info";
  text: string;
}

/** 压缩结束的一句话。 */
export function compressNotice(result: CompressResult): ArchiveNotice {
  if (result.state === "cancelled") {
    return { type: "info", text: "已取消压缩，没有留下临时文件" };
  }
  const base = `已压缩为「${result.name ?? ""}」`;
  return {
    type: "success",
    text: result.warning ? `${base}（${result.warning}）` : base,
  };
}

/**
 * 解压结束的一句话：解出多少、跳过多少。
 * `inPlace`：解到了已经存在的文件夹里——取消时已经解出的文件没法区分新旧，保留不动，要说清楚。
 */
export function extractNotice(
  result: ExtractResult,
  destName: string,
  inPlace: boolean,
): ArchiveNotice {
  if (result.state === "cancelled") {
    return {
      type: "info",
      text: inPlace
        ? `已取消解压，已经解出的文件保留在「${destName}」里`
        : "已取消解压，没有留下半成品",
    };
  }
  const parts = [`已解压到「${destName}」：解出 ${result.extracted} 个文件`];
  if (result.skipped > 0) parts.push(`跳过 ${result.skipped} 个已有文件`);
  const text = parts.join("，");
  return {
    type: "success",
    text: result.warning ? `${text}（${result.warning}）` : text,
  };
}

/** 冲突对话框里「…等 N 个」那一行；名字全列出来了就没有。 */
export function conflictMoreText(plan: Pick<ExtractPlan, "conflict_count" | "conflict_sample">): string | null {
  return plan.conflict_count > plan.conflict_sample.length
    ? `等 ${plan.conflict_count} 个`
    : null;
}
