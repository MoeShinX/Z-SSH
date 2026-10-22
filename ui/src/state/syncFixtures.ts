//! 同步页测试共用的固定数据。

import type { PendingConfirmation, SyncStatusView } from "../api/sync";

export function makeStatus(overrides: Partial<SyncStatusView> = {}): SyncStatusView {
  return {
    configured: true,
    config: {
      provider: "webdav",
      url: "https://dav.example.internal/",
      directory: "Z-SSH",
      username: "alice",
      allow_insecure_http: false,
      endpoint: "",
      region: "",
      bucket: "",
      access_key: "",
      prefix: "",
      path_style: true,
      api_base: "",
      gist_id: "",
      account: "",
      client_id: "",
      has_secret: true,
      plain_http: false,
    },
    concurrency: { protected: true, notes: ["服务器返回了 ETag"], checked_at: 1 },
    ready: true,
    password_in_memory: true,
    password_remembered: false,
    running: false,
    last: {
      at: 1_700_000_000,
      status: "completed",
      summary: "同步完成：12 条记录",
      version: "\"abc\"",
      conflicts: 0,
      auto: false,
    },
    auto: { enabled: true, interval_minutes: 15 },
    pending: null,
    baseline_broken: false,
    conflict_count: 0,
    backup_keep: 20,
    auth_paused: null,
    onedrive_builtin_client: true,
    onedrive_logging_in: false,
    ...overrides,
  };
}

export const PENDING: PendingConfirmation = {
  at: 1,
  auto: false,
  summary: "这次同步会删除 12 条记录",
  local_records: 20,
  deleted_records: 12,
  local_hosts: 18,
  deleted_hosts: 11,
  samples: [
    { entity: "host", name: "web-1" },
    { entity: "group", name: "生产" },
  ],
};
