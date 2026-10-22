//! 内置编辑器的命令封装（M3-5 第 2 节），命令名和后端 `commands::sftp_edit::editor` 一一对应。

import { invoke } from "@tauri-apps/api/core";

/** 打开成功的结果（也是开新标签用的全部信息）。 */
export interface EditorOpenInfo {
  edit_id: number;
  side: "remote" | "local";
  text: string;
  bom: boolean;
  eol: "LF" | "CRLF";
  file_name: string;
  /** 远端路径，或本地路径。 */
  path: string;
  /** 主机库里的主机 id；本地文件是 0。 */
  host_id: number;
  host_label: string;
  size: number;
  /** 本地文件是符号链接时，打开时解析出来的真实目标（标签悬停显示「链接 → 目标」）。 */
  link_target?: string | null;
}

/** 放弃本地修改后重新读回的内容。 */
export interface EditorText {
  text: string;
  bom: boolean;
  eol: "LF" | "CRLF";
}

/** 保存的结果。 */
export interface EditorSaveResult {
  /** uploaded 已保存 / conflict 文件被别人改过没写 / owner_prompt 保存后属主会变等用户选 / failed */
  state: "uploaded" | "conflict" | "owner_prompt" | "failed";
  message?: string | null;
  /** 「另存为」实际写到的新路径（本地文件用：界面要把文档路径换过去）。 */
  path?: string | null;
}

/** 命令失败时抛出的结构化错误。 */
export interface EditorError {
  code: string;
  message: string;
}

/** 不适合内置编辑器的文件（界面据此显示拒绝的标题）。 */
export const REFUSED_CODES = ["not_utf8", "binary", "mixed_eol", "too_large"] as const;

export function isEditorError(value: unknown): value is EditorError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as EditorError).code === "string" &&
    typeof (value as EditorError).message === "string"
  );
}

/** 把任何失败变成 `EditorError`（后端以外的错误按 `other` 处理）。 */
export function toEditorError(value: unknown): EditorError {
  if (isEditorError(value)) return value;
  return { code: "other", message: value instanceof Error ? value.message : String(value) };
}

export function editorOpen(sftpId: string, remotePath: string): Promise<EditorOpenInfo> {
  return invoke("editor_open", { sftpId, remotePath });
}

/** `sftpId` 是这台主机**现在**可用的 SFTP 会话（原来的可能已经关了）。
 *  `version` 是这次保存的内容版本：后端保存成功后只删版本不晚于它的草稿。 */
export function editorSave(
  sftpId: string,
  editId: number,
  text: string,
  version: number,
): Promise<EditorSaveResult> {
  return invoke("editor_save", { sftpId, editId, text, version });
}

/** `version` 同 [`editorSave`]：草稿带着自己对应的内容版本落盘。 */
export function editorDraft(editId: number, text: string, version: number): Promise<void> {
  return invoke("editor_draft", { editId, text, version });
}

export function editorReload(editId: number): Promise<EditorText> {
  return invoke("editor_reload", { editId });
}

/** `discard`：用户选了「不保存」（连草稿一起删）。 */
export function editorClose(editId: number, discard: boolean): Promise<void> {
  return invoke("editor_close", { editId, discard });
}

export function localEditorOpen(path: string): Promise<EditorOpenInfo> {
  return invoke("local_editor_open", { path });
}

/** `force`：用户看过「文件被别的程序改过」的提示后选了覆盖。`version` 同 [`editorSave`]。 */
export function localEditorSave(
  editId: number,
  text: string,
  force: boolean,
  version: number,
): Promise<EditorSaveResult> {
  return invoke("local_editor_save", { editId, text, force, version });
}

/** 本地文件保存冲突时「另存为」：写到同目录下的新文件，返回实际路径。 */
export function localEditorSaveAs(
  editId: number,
  text: string,
  name: string,
  version: number,
): Promise<EditorSaveResult> {
  return invoke("local_editor_save_as", { editId, text, name, version });
}
