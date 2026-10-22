//! 和 Tauri 命令一一对应的薄封装，命令名和后端 `commands::session` 保持一致。

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type SessionState =
  | "connecting"
  | "authenticating"
  | "connected"
  | "disconnected";

/** 拨号链进度：第几跳、连的是谁。 */
export interface HopInfo {
  index: number;
  total: number;
  label: string;
}

export interface SessionStateEvent {
  /** 后端用的是 snake_case，这里按后端的字段名读。 */
  session_id: string;
  state: SessionState;
  reason?: string;
  /** 只有 connecting 阶段有 */
  hop?: HopInfo;
}

/**
 * 打开一个会话，shell 输出通过 Channel 以二进制送回来。
 *
 * 后端发的是 `InvokeResponseBody::Raw`，所以回调拿到的是 ArrayBuffer，不是
 * JSON 数字数组。
 */
export async function sessionOpenQuick(
  target: string,
  password: string,
  cols: number,
  rows: number,
  onOutput: (chunk: ArrayBuffer) => void,
): Promise<string> {
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = onOutput;

  return invoke<string>("session_open_quick", {
    target,
    password,
    cols,
    rows,
    onOutput: channel,
  });
}

/**
 * 在同一条已认证的连接上再开一个 shell（分屏用），不重新握手和认证。
 *
 * 和 `session_open` 一样立即返回会话 ID。
 */
export function sessionDuplicate(
  sessionId: string,
  cols: number,
  rows: number,
  onOutput: (chunk: ArrayBuffer) => void,
): Promise<string> {
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = onOutput;
  return invoke<string>("session_duplicate", {
    sessionId,
    cols,
    rows,
    onOutput: channel,
  });
}

export function termWrite(sessionId: string, data: string): Promise<void> {
  return invoke("term_write", { sessionId, data });
}

/** 从主机库连一台主机：地址、用户名和密码都在后端取，密码不过前端。 */
export function sessionOpen(
  hostId: number,
  cols: number,
  rows: number,
  onOutput: (chunk: ArrayBuffer) => void,
): Promise<string> {
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = onOutput;
  return invoke<string>("session_open", { hostId, cols, rows, onOutput: channel });
}

export function termResize(
  sessionId: string,
  cols: number,
  rows: number,
): Promise<void> {
  return invoke("term_resize", { sessionId, cols, rows });
}

/** 告诉后端这些字节已经在终端里渲染完了，后端据此放行流控。 */
export function termAck(sessionId: string, bytes: number): Promise<void> {
  return invoke("term_ack", { sessionId, bytes });
}

/** 终端当前所在的目录；取不到是 null（调用方退回家目录，不报错）。 */
export function sessionCwd(sessionId: string): Promise<string | null> {
  return invoke<string | null>("session_cwd", { sessionId });
}

export function sessionClose(sessionId: string): Promise<void> {
  return invoke("session_close", { sessionId });
}

/**
 * 前端换了一页：让后端把上一页留下的会话全部收掉，返回关了几个。
 *
 * 界面启动时调一次。刷新 / 热更新 / 崩溃之后，旧会话的 Channel 已经没人认领，
 * 前端也不会再关闭它们，只能这样回收。
 */
export function sessionsReset(): Promise<number> {
  return invoke<number>("sessions_reset");
}

/**
 * 订阅一台会话的服务器状态采集（CPU/内存/磁盘/网速）。
 *
 * 同一条连接上的多个会话共享同一份数据（后端按连接去重）。返回订阅 ID，
 * 用来取消订阅（`statsUnsubscribe`）。会话断开 / 标签全关了要退订。
 */
export function statsSubscribe(sessionId: string): Promise<number> {
  return invoke<number>("stats_subscribe", { sessionId });
}

/** 取消状态采集订阅（订阅 ID 来自 `statsSubscribe`）。 */
export function statsUnsubscribe(subscriptionId: number): Promise<void> {
  return invoke("stats_unsubscribe", { subscriptionId });
}

export function onSessionState(
  handler: (event: SessionStateEvent) => void,
): Promise<UnlistenFn> {
  return listen<SessionStateEvent>("session_state", (event) =>
    handler(event.payload),
  );
}

/** 服务器状态事件（`server_stats`）：每个订阅者条独立，`subscription_id` 认领。 */
export interface ServerStatsEvent {
  subscription_id: number;
  stats: ServerStats;
}

/** 采集脚本解析出的服务器状态（字段缺失时是 null，见 stats.rs）。 */
export interface ServerStats {
  cpu_percent: number | null;
  per_core_percent: (number | null)[];
  cpu_cores: number | null;
  mem_total_mib: number | null;
  mem_used_mib: number | null;
  mem_free_mib: number | null;
  mem_buffers_mib: number | null;
  mem_cached_mib: number | null;
  swap_total_mib: number | null;
  swap_used_mib: number | null;
  disk_total_mib: number | null;
  disk_used_mib: number | null;
  disk_percent: number | null;
  net_rx_speed: number;
  net_tx_speed: number;
  /** 字节：所有网卡开机以来收 / 发的总量。旧后端没有这两个字段。 */
  net_rx_total?: number;
  net_tx_total?: number;
  net_interfaces: {
    name: string;
    rx_speed: number;
    tx_speed: number;
    /** 字节：这块网卡启用以来（一般就是开机以来）的总量。旧后端没有。 */
    rx_total?: number;
    tx_total?: number;
  }[];
  /** 全部真实分区（概览面板）；旧后端没有这个字段。 */
  mounts?: {
    mount: string;
    fs_type: string;
    source: string;
    used_mib: number;
    total_mib: number;
    percent: number;
  }[];
  top_processes: { pid: string; mem_percent: number; command: string }[];
  hostname: string;
  os_name: string;
  kernel: string;
  uptime_secs: number | null;
  load_1m: number | null;
  load_5m: number | null;
  load_15m: number | null;
  /** CPU 型号；每个连接只探测一次。旧后端没有这个字段，拿不到时是 null。 */
  cpu_model?: string | null;
}

export function onServerStats(
  handler: (event: ServerStatsEvent) => void,
): Promise<UnlistenFn> {
  return listen<ServerStatsEvent>("server_stats", (event) =>
    handler(event.payload),
  );
}
