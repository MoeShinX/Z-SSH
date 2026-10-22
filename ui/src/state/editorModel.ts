//! 内置编辑器的纯逻辑（M3-5 第 2 节）：状态栏文字、标签标题、打开失败的提示、关闭时的询问内容。

import type { EditorError, EditorOpenInfo } from "../api/editor";
import { REFUSED_CODES } from "../api/editor";
import { LANGUAGE_NAMES, type LanguageId } from "./editorLanguage";

/** 状态栏上的编码：UTF-8，有 BOM 时写出来。 */
export function encodingLabel(bom: boolean): string {
  return bom ? "UTF-8 (BOM)" : "UTF-8";
}

export interface StatusInput {
  bom: boolean;
  eol: "LF" | "CRLF";
  /** 1 起的行、列。 */
  line: number;
  column: number;
  language: LanguageId | "text";
  wrap: boolean;
  /** 选中了多少个字符（没选 = 0）。 */
  selected: number;
}

/** 状态栏从左到右的几段文字。 */
export function statusSegments(input: StatusInput): string[] {
  const position = `行 ${input.line}，列 ${input.column}`;
  return [
    input.selected > 0 ? `${position}（选中 ${input.selected} 个字符）` : position,
    encodingLabel(input.bom),
    input.eol,
    LANGUAGE_NAMES[input.language],
    input.wrap ? "自动换行" : "不换行",
  ];
}

/** 标签标题：文件名。 */
export function tabTitle(fileName: string): string {
  return fileName;
}

/** 本地文件「另存为」的默认名字：`a.txt` → `a (副本).txt`。 */
export function defaultLocalSaveAsName(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  return dot > 0
    ? `${fileName.slice(0, dot)} (副本)${fileName.slice(dot)}`
    : `${fileName} (副本)`;
}

/** 标签悬停显示：`主机:路径`（本地文件显示本地路径；符号链接显示「链接 → 目标」）。 */
export function tabTooltip(tab: {
  side: "remote" | "local";
  hostLabel: string;
  path: string;
  linkTarget?: string | null;
}): string {
  if (tab.side === "remote") return `${tab.hostLabel}:${tab.path}`;
  return tab.linkTarget ? `${tab.path} → ${tab.linkTarget}` : tab.path;
}

/** 两个打开请求是不是指同一个文件（同一台主机的同一个远端路径，或者同一个本地路径）。 */
export function isSameFile(
  a: { side: "remote" | "local"; hostId: number; path: string },
  b: { side: "remote" | "local"; hostId: number; path: string },
): boolean {
  if (a.side !== b.side) return false;
  if (a.side === "remote") return a.hostId === b.hostId && a.path === b.path;
  // Windows 路径不分大小写，斜杠方向也不算数。
  const norm = (value: string) => value.replace(/\//g, "\\").toLowerCase();
  return norm(a.path) === norm(b.path);
}

export interface OpenFailure {
  title: string;
  message: string;
}

/** 打开失败时给用户看什么。 */
export function describeOpenError(error: EditorError, fileName: string): OpenFailure {
  const refused = (REFUSED_CODES as readonly string[]).includes(error.code);
  return {
    title: refused ? `不能用内置编辑器打开「${fileName}」` : `打不开「${fileName}」`,
    message: error.message,
  };
}

/** 从一次打开的结果得到标签的初始信息。 */
export function tabFromInfo(info: EditorOpenInfo) {
  return {
    editId: info.edit_id,
    side: info.side,
    hostId: info.host_id,
    hostLabel: info.host_label,
    path: info.path,
    fileName: info.file_name,
    bom: info.bom,
    eol: info.eol,
    linkTarget: info.link_target ?? null,
  };
}

export interface UnsavedFile {
  editId: number;
  fileName: string;
  path: string;
}

/** 关标签 / 关连接 / 退出前的询问：标题和说明。 */
export function unsavedPrompt(
  files: UnsavedFile[],
  reason: "close-tab" | "close-tabs" | "close-connection" | "quit",
): { title: string; message: string } {
  const count = files.length;
  const what =
    reason === "close-tab"
      ? "关闭这个标签"
      : reason === "close-tabs"
        ? "关闭这些标签"
        : reason === "close-connection"
        ? "关闭这台主机的连接"
        : "退出程序";
  if (count === 1) {
    return {
      title: `「${files[0].fileName}」的修改还没有保存`,
      message: `${what}之前要先保存吗？选「不保存」会丢掉这次的修改。`,
    };
  }
  const names = files
    .slice(0, 6)
    .map((file) => `「${file.fileName}」`)
    .join("、");
  const more = count > 6 ? `等 ${count} 个文件` : `${count} 个文件`;
  return {
    title: `${more}的修改还没有保存`,
    message: `${names}${count > 6 ? "……" : ""}。${what}之前要先保存吗？选「不保存」会丢掉这些修改。`,
  };
}

/** 保存没成功时状态栏 / 提示上写什么。 */
export function saveFailureText(result: { state: string; message?: string | null }): string {
  switch (result.state) {
    case "conflict":
      return result.message ?? "文件在你打开之后被改过，没有保存";
    case "owner_prompt":
      return result.message ?? "保存后文件的属主会改变，等你选择";
    case "failed":
      return result.message ?? "保存失败";
    default:
      return result.message ?? "没有保存";
  }
}
