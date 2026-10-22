//! 认证和主机指纹弹窗用的接口。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 指纹弹窗的状态：未知 / 指纹变化 / 只记过别的密钥类型。 */
export type HostKeyStatus = "unknown" | "changed" | "other_key_types_saved";

export interface HostKeyPrompt {
  req_id: number;
  host: string;
  port: number;
  key_type: string;
  fingerprint: string;
  status: HostKeyStatus;
}

export type PromptKind = "password" | "passphrase" | "keyboard_interactive";

export interface AuthPromptItem {
  text: string;
  echo: boolean;
}

export interface AuthPrompt {
  req_id: number;
  kind: PromptKind;
  /** keyboard-interactive 的标题和说明原文 */
  name: string;
  instructions: string;
  prompts: AuthPromptItem[];
  can_remember: boolean;
}

export type HostKeyAnswer = "trust_and_save" | "trust_once" | "reject";

export function onHostKeyPrompt(
  handler: (prompt: HostKeyPrompt) => void,
): Promise<UnlistenFn> {
  return listen<HostKeyPrompt>("hostkey_prompt", (event) =>
    handler(event.payload),
  );
}

export function onAuthPrompt(
  handler: (prompt: AuthPrompt) => void,
): Promise<UnlistenFn> {
  return listen<AuthPrompt>("auth_prompt", (event) => handler(event.payload));
}

/** 后端说某条提问已经结束（回答、超时、会话关闭、连接取消），前端把它从队列里摘掉。 */
export function onPromptClosed(handler: (reqId: number) => void): Promise<UnlistenFn> {
  return listen<{ req_id: number }>("prompt_closed", (event) => handler(event.payload.req_id));
}

export function answerHostKey(
  reqId: number,
  decision: HostKeyAnswer,
): Promise<void> {
  return invoke("hostkey_answer", { reqId, decision });
}

/** answers 为 null 表示用户取消这次连接。 */
export function answerAuth(
  reqId: number,
  answers: string[] | null,
  remember: boolean,
): Promise<void> {
  return invoke("auth_answer", { reqId, answers, remember });
}

/** 清掉自己 known_hosts 里这台主机的记录，返回删掉的行数。 */
export function hostkeyForget(host: string, port: number): Promise<number> {
  return invoke("hostkey_forget", { host, port });
}

/** 已保存的指纹，没有就返回 null。 */
export function hostkeySaved(
  host: string,
  port: number,
): Promise<string | null> {
  return invoke("hostkey_saved", { host, port });
}
