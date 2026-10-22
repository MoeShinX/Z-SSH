// 端口转发：规则存在本机（不进配置同步），开 / 停 / 状态走后端的 ForwardManager。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ForwardKind = "local" | "remote";

export interface PortForward {
  id: number;
  host_id: number;
  name: string;
  kind: ForwardKind;
  bind_host: string;
  bind_port: number;
  dest_host: string;
  dest_port: number;
  auto_start: boolean;
}

export interface PortForwardDraft extends Omit<PortForward, "id"> {
  id: number | null;
}

export type ForwardPhase = "starting" | "running" | "failed";

export interface ForwardStatus {
  id: number;
  phase: ForwardPhase;
  error: string | null;
  listening_port: number | null;
  counters: { active: number; total: number; bytes_sent: number; bytes_received: number };
}

export const forwardList = (): Promise<PortForward[]> => invoke("forward_list");
export const forwardSave = (draft: PortForwardDraft): Promise<PortForward> =>
  invoke("forward_save", { draft });
export const forwardDelete = (id: number): Promise<void> => invoke("forward_delete", { id });
export const forwardStart = (id: number): Promise<void> => invoke("forward_start", { id });
export const forwardStop = (id: number): Promise<void> => invoke("forward_stop", { id });
export const forwardStatuses = (): Promise<ForwardStatus[]> => invoke("forward_statuses");

/** 某条规则的状态变了（开起来、连上、出错、停了）。 */
export const onForwardStatus = (handler: () => void): Promise<UnlistenFn> =>
  listen<number>("forward-status", () => handler());

/** 规则的一句话描述，例如 `本机 127.0.0.1:8080 → 服务器那边 localhost:80`。 */
export function describeForward(rule: Pick<PortForward, "kind" | "bind_host" | "bind_port" | "dest_host" | "dest_port">): string {
  const bind = `${rule.bind_host}:${rule.bind_port}`;
  const dest = `${rule.dest_host}:${rule.dest_port}`;
  return rule.kind === "local" ? `本机 ${bind} → ${dest}（经服务器）` : `服务器 ${bind} → 本机这边 ${dest}`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
