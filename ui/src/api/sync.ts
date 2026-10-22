//! 配置同步的接口封装（后端见 src-tauri/src/commands/sync.rs）。
//!
//! 字段名和 Rust 那边一致（snake_case）；命令的参数名按 Tauri 的约定用 camelCase。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Provider = "webdav" | "s3" | "github" | "onedrive";

/** 界面提交的连接信息。密码 / Secret 留空 = 沿用已保存的。 */
export interface ConfigInput {
  provider: Provider;
  url?: string;
  directory?: string;
  username?: string;
  password?: string;
  allow_insecure_http?: boolean;
  endpoint?: string;
  region?: string;
  bucket?: string;
  access_key?: string;
  secret_key?: string;
  prefix?: string;
  path_style?: boolean;
  /** GitHub：接口地址（空 = api.github.com）、令牌（留空 = 沿用已保存的）、Gist ID。 */
  api_base?: string;
  token?: string;
  gist_id?: string;
}

/** 已保存的连接信息（没有密码和 Secret）。 */
export interface ConfigView {
  provider: Provider;
  url: string;
  directory: string;
  username: string;
  allow_insecure_http: boolean;
  endpoint: string;
  region: string;
  bucket: string;
  access_key: string;
  prefix: string;
  path_style: boolean;
  api_base: string;
  gist_id: string;
  /** OneDrive：登录的账号显示名；用户在“高级”里填的 client ID（空 = 用编进程序的）。 */
  account: string;
  client_id: string;
  /** 密码 / Secret / 令牌已经保存过（界面永远拿不到它们的内容）。 */
  has_secret: boolean;
  plain_http: boolean;
}

export interface ConcurrencyView {
  protected: boolean;
  notes: string[];
  checked_at: number;
}

export type LastStatus =
  | "completed"
  | "needs_confirmation"
  | "cancelled"
  | "local_changed"
  | "error";

export interface LastResult {
  at: number;
  status: LastStatus;
  summary: string;
  version: string | null;
  conflicts: number;
  auto: boolean;
}

export interface AutoSettings {
  enabled: boolean;
  interval_minutes: number;
}

export interface DeletedSample {
  entity: "host" | "group" | "identity" | "proxy";
  name: string;
}

/** 防误删：这次同步会删掉很多，等用户选。 */
export interface PendingConfirmation {
  at: number;
  auto: boolean;
  summary: string;
  local_records: number;
  deleted_records: number;
  local_hosts: number;
  deleted_hosts: number;
  samples: DeletedSample[];
}

export interface SyncStatusView {
  configured: boolean;
  config: ConfigView | null;
  /** `null` = 还没测试过连接（不能当成支持并发保护）。 */
  concurrency: ConcurrencyView | null;
  ready: boolean;
  password_in_memory: boolean;
  password_remembered: boolean;
  running: boolean;
  last: LastResult | null;
  auto: AutoSettings;
  pending: PendingConfirmation | null;
  baseline_broken: boolean;
  conflict_count: number;
  /** 本地自动备份最多保留几份（默认 20）。 */
  backup_keep: number;
  /** 凭据被存储服务拒绝，自动同步已暂停；改了存储配置或手动同步后恢复。 */
  auth_paused: string | null;
  /** 这个版本编进了 OneDrive 的 client ID（没有的话登录要用户在“高级”里填自己的）。 */
  onedrive_builtin_client: boolean;
  /** 正在浏览器里登录 OneDrive。 */
  onedrive_logging_in: boolean;
}

export interface Check {
  ok: boolean;
  message: string;
}

export interface ConnectionProbe {
  read: Check;
  write: Check;
  concurrency: { mode: "conditional" | "read_before_write"; notes: string[] } | null;
  has_existing_data: boolean;
}

export interface TestResult {
  probe: ConnectionProbe;
  status: SyncStatusView;
}

export interface RunView {
  status: LastStatus;
  summary: string;
  conflicts: number;
  nulled_references: string[];
  pending: PendingConfirmation | null;
  status_view: SyncStatusView;
}

export type Decision = "continue" | "use_cloud" | "cancel";

export interface ConflictView {
  id: number;
  at: number;
  entity: string;
  key: string;
  name: string;
  kept_summary: string;
  kept_device: string | null;
  kept_is_local: boolean;
  other_summary: string;
  other_device: string | null;
  other_is_local: boolean;
  deletion: boolean;
  can_use_other: boolean;
  message: string;
}

export interface BackupDetail {
  path: string;
  name: string;
  size: number;
  stamp: string;
  sequence: number;
  readable: boolean;
  created_at: number | null;
  app_version: string | null;
  record_count: number | null;
  host_count: number | null;
}

export interface RestoreView {
  summary: string;
  safety_backup: string;
  status: SyncStatusView;
}

export const syncStatus = (): Promise<SyncStatusView> => invoke("sync_status");

export const syncConfigSave = (input: ConfigInput): Promise<SyncStatusView> =>
  invoke("sync_config_save", { input });

/** 带 `input` 就先保存再测；不带就测已保存的。 */
export const syncTestConnection = (input?: ConfigInput): Promise<TestResult> =>
  invoke("sync_test_connection", { input: input ?? null });

/** 设置（给 `confirm`）或输入主密码并同步一次。 */
export const syncSetPassword = (
  password: string,
  remember: boolean,
  confirm?: string,
): Promise<RunView> =>
  invoke("sync_set_password", { password, remember, confirm: confirm ?? null });

export const syncRememberPassword = (remember: boolean): Promise<SyncStatusView> =>
  invoke("sync_remember_password", { remember });

export const syncChangePassword = (
  oldPassword: string,
  newPassword: string,
): Promise<SyncStatusView> => invoke("sync_change_password", { oldPassword, newPassword });

export const syncNow = (decision?: Decision): Promise<RunView> =>
  invoke("sync_now", { decision: decision ?? null });

export const syncAutoSet = (enabled: boolean, intervalMinutes: number): Promise<SyncStatusView> =>
  invoke("sync_auto_set", { enabled, intervalMinutes });

export const syncConflictsList = (): Promise<ConflictView[]> => invoke("sync_conflicts_list");

export const syncConflictUseOther = (id: number): Promise<SyncStatusView> =>
  invoke("sync_conflict_use_other", { id });

export const syncConflictDismiss = (id: number): Promise<SyncStatusView> =>
  invoke("sync_conflict_dismiss", { id });

export const syncBackupsList = (): Promise<BackupDetail[]> => invoke("sync_backups_list");

export const syncBackupKeepSet = (keep: number): Promise<SyncStatusView> =>
  invoke("sync_backup_keep_set", { keep });

export const syncBackupRestore = (path: string): Promise<RestoreView> =>
  invoke("sync_backup_restore", { path });

export interface CreatedGist {
  gist_id: string;
  status: SyncStatusView;
}

/** 新建一个 secret Gist，把令牌和新 Gist 的 ID 一起保存。 */
export const syncGithubCreateGist = (input: ConfigInput): Promise<CreatedGist> =>
  invoke("sync_github_create_gist", { input });

/**
 * OneDrive 登录：用系统默认浏览器打开微软登录页，等用户授权。会一直等到登录完成 / 被拒绝 /
 * 超时（5 分钟）/ 被取消。`clientId` 是“高级”里填的（空 = 用编进程序的）。
 */
export const syncOnedriveLogin = (clientId?: string): Promise<SyncStatusView> =>
  invoke("sync_onedrive_login", { clientId: clientId ?? null });

/** 登录中点“取消”：关闭回环监听，上面的登录以“已取消登录”结束。 */
export const syncOnedriveLoginCancel = (): Promise<void> => invoke("sync_onedrive_login_cancel");

/** 断开 OneDrive：删掉本机的令牌和连接设置（云端的同步包不动）。 */
export const syncOnedriveDisconnect = (): Promise<SyncStatusView> =>
  invoke("sync_onedrive_disconnect");

/** 用系统浏览器打开微软账户里管理“应用和服务”授权的页面（撤销授权用）。 */
export const syncOnedriveOpenRevokePage = (): Promise<void> =>
  invoke("sync_onedrive_open_revoke_page");

export const syncResetBaseline = (): Promise<SyncStatusView> => invoke("sync_reset_baseline");

export function onSyncStatus(handler: (status: SyncStatusView) => void): Promise<UnlistenFn> {
  return listen<SyncStatusView>("sync://status", (event) => handler(event.payload));
}

/** 导出离线备份（密码加密的文件，含凭据明文的密文）。返回给用户看的一句话。 */
export const syncOfflineExport = (path: string, password: string): Promise<string> =>
  invoke("sync_offline_export", { path, password });

/** 导入离线备份。merge = 两边都留；replace = 本机变成和文件一样。导入前自动备份。 */
export const syncOfflineImport = (
  path: string,
  password: string,
  mode: "merge" | "replace",
): Promise<RestoreView> => invoke("sync_offline_import", { path, password, mode });
