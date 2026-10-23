// Adapted from Netcatty components/sftp/SftpContext.tsx 的数据形状（字段按 Z-SSH 后端重定义）
//! SFTP 命令和事件的薄封装，命令名和后端 `commands::sftp` 一一对应。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 一条目录项（两栏通用）。 */
export interface SftpEntry {
  name: string;
  path: string;
  /** dir / file / symlink / other / drive（盘符只在本地"此电脑"里出现） */
  kind: "dir" | "file" | "symlink" | "other" | "drive";
  link_target?: string;
  size?: number;
  /** 修改时间（Unix 秒） */
  mtime?: number;
  /** 八进制权限（远端） */
  permissions?: number;
  /** 所有者（远端，有就显示） */
  owner?: string;
  /** 名字不是合法 UTF-8：标灰、禁操作（已知限制，见设计文档）。 */
  name_is_utf8: boolean;
  hidden: boolean;
  /** 符号链接指向的类型（dir / file）；双击链接时决定进不进目录。 */
  target_kind?: "dir" | "file";
}

/** sftp_list 的返回：真实路径（家目录进来时界面记当前目录用）+ 条目。 */
export interface SftpListing {
  path: string;
  entries: SftpEntry[];
}

/** 传输方向：上传 / 下载，以及复制粘贴的中转（远端到远端）和本地复制。 */
export type SftpDirection = "upload" | "download" | "remote_copy" | "local_copy";

/** 传输任务（队列里的一行）。 */
export interface SftpTransfer {
  id: number;
  direction: SftpDirection;
  state:
    | "queued"
    | "planning"
    | "running"
    | "paused"
    | "waiting_reconnect"
    /** 目标已存在，在等用户回答怎么处理（冲突对话框还没回答）。 */
    | "waiting_conflict"
    | "done"
    | "failed"
    | "cancelled"
    | "skipped";
  reason?: string;
  source: string;
  target: string;
  total_files: number;
  done_files: number;
  total_bytes: number;
  done_bytes: number;
  speed_bps: number;
  eta_secs?: number;
  current?: string;
  notes: string[];
}

/** 冲突提问（后端发事件，前端弹对话框）。 */
export interface SftpConflictPrompt {
  conflict_id: number;
  job: number;
  direction: SftpDirection;
  source: string;
  target: string;
  source_size?: number;
  target_size?: number;
  source_mtime?: number;
  target_mtime?: number;
  can_resume: boolean;
  suggested_name: string;
  /** 属于哪次提交 / 粘贴（同一批次共享"全部应用"）；没有 = 不属于任何批次。 */
  batch?: number;
}

/** 删除事件（进度 / 结束共用一个事件名，tag 区分）。 */
export type SftpDeleteEvent =
  | { kind: "progress"; delete_id: number; deleted: number; current: string }
  | {
      kind: "done";
      delete_id: number;
      deleted: number;
      error?: string;
      cancelled: boolean;
    };

/** sftp_state 事件：connecting / ready / failed。 */
export interface SftpStateEvent {
  sftp_id: string;
  state: string;
  reason?: string;
}

export function sftpOpen(hostId: number): Promise<string> {
  return invoke<string>("sftp_open", { hostId });
}

export function sftpClose(sftpId: string): Promise<void> {
  return invoke("sftp_close", { sftpId });
}

export function sftpList(sftpId: string, path: string): Promise<SftpListing> {
  return invoke<SftpListing>("sftp_list", { sftpId, path });
}

export function sftpMkdir(sftpId: string, path: string): Promise<void> {
  return invoke("sftp_mkdir", { sftpId, path });
}

export function sftpCreateFile(sftpId: string, path: string): Promise<void> {
  return invoke("sftp_create_file", { sftpId, path });
}

export function sftpRename(
  sftpId: string,
  from: string,
  to: string,
  overwrite: boolean,
): Promise<void> {
  return invoke("sftp_rename", { sftpId, from, to, overwrite });
}

export function sftpSetPermissions(
  sftpId: string,
  path: string,
  mode: number,
  recursive: boolean,
): Promise<void> {
  return invoke("sftp_set_permissions", { sftpId, path, mode, recursive });
}

/** 删除（远端，可以多个，目录递归）。返回 delete_id，进度走事件。 */
export function sftpDelete(sftpId: string, paths: string[]): Promise<number> {
  return invoke<number>("sftp_delete", { sftpId, paths });
}

export function sftpDeleteCancel(
  sftpId: string,
  deleteId: number,
): Promise<void> {
  return invoke("sftp_delete_cancel", { sftpId, deleteId });
}

export function sftpTransferSubmit(
  sftpId: string,
  direction: "upload" | "download",
  sources: string[],
  target: string,
): Promise<number[]> {
  return invoke<number[]>("sftp_transfer_submit", {
    sftpId,
    direction,
    sources,
    target,
  });
}

export function sftpTransferPause(sftpId: string, id: number): Promise<void> {
  return invoke("sftp_transfer_pause", { sftpId, id });
}

export function sftpTransferResume(sftpId: string, id: number): Promise<void> {
  return invoke("sftp_transfer_resume", { sftpId, id });
}

export function sftpTransferCancel(sftpId: string, id: number): Promise<void> {
  return invoke("sftp_transfer_cancel", { sftpId, id });
}

export function sftpTransferRetry(sftpId: string, id: number): Promise<void> {
  return invoke("sftp_transfer_retry", { sftpId, id });
}

export function sftpTransfers(sftpId: string): Promise<SftpTransfer[]> {
  return invoke<SftpTransfer[]>("sftp_transfers", { sftpId });
}

export function sftpTransfersClear(sftpId: string): Promise<number> {
  return invoke<number>("sftp_transfers_clear", { sftpId });
}

/** 冲突的回答。关掉对话框不走这个（按超时=跳过）。 */
export function sftpConflictAnswer(
  conflictId: number,
  action: "overwrite" | "skip" | "rename" | "resume",
  applyToAll: boolean,
  renameTo?: string,
): Promise<void> {
  return invoke("sftp_conflict_answer", {
    conflictId,
    action,
    applyToAll,
    renameTo: renameTo ?? null,
  });
}

// ---------- 本地（左栏） ----------

export function sftpLocalDefaultDir(): Promise<string> {
  return invoke<string>("sftp_local_default_dir");
}

export function sftpLocalList(path: string): Promise<SftpEntry[]> {
  return invoke<SftpEntry[]>("sftp_local_list", { path });
}

export function sftpLocalMkdir(path: string): Promise<void> {
  return invoke("sftp_local_mkdir", { path });
}

export function sftpLocalCreateFile(path: string): Promise<void> {
  return invoke("sftp_local_create_file", { path });
}

export function sftpLocalRename(from: string, to: string): Promise<void> {
  return invoke("sftp_local_rename", { from, to });
}

/** 本地删除，返回每个路径的成败（err 有值就是失败原因）。 */
export function sftpLocalDelete(
  paths: string[],
): Promise<Array<[string, string | null]>> {
  return invoke("sftp_local_delete", { paths });
}

// ---------- 编辑器事件（内置编辑器的保存 / 冲突 / 属主询问） ----------

export interface SftpEditEvent {
  sftp_id: string;
  edit_id: number;
  remote_path: string;
  /** uploading / uploaded / conflict / failed / discarded / warning / owner_prompt */
  state:
    | "uploading"
    | "uploaded"
    | "conflict"
    | "failed"
    | "discarded"
    | "warning"
    | "owner_prompt";
  message?: string;
  remote_size?: number;
  remote_mtime?: number;
  /** 冲突时远端文件已经被删了。 */
  remote_gone: boolean;
  suggested_name: string;
  /** 这个编辑现在有没有"没落到远端的修改"。 */
  dirty: boolean;
  /**
   * 属主的变化：`owner_prompt`（保存后属主会从 from 变成 to，还没替换，等用户选）和
   * `uploaded`（已经保存了，但属主没能保持）两种事件带着它。
   */
  owner?: { from: string; to: string; error: string };
}

/** 远端被改过时的选择：overwrite 覆盖 / discard 放弃 / save_as 另存为（带新名字）。 */
export function sftpEditResolve(
  editId: number,
  action:
    "overwrite" | "discard" | "save_as" | "owner_keep_new" | "owner_in_place",
  name?: string,
): Promise<void> {
  return invoke("sftp_edit_resolve", { editId, action, name });
}

/** 上次运行留下的、有没上传的修改（程序崩溃或被强行关掉时才会有）。 */
export interface EditLeftover {
  id: string;
  file_name: string;
  size: number;
  /** 对应的服务器文件；认不出来源时没有。 */
  remote_path?: string;
  host_id?: number;
  host_label?: string;
  /** 没法确认这份文件有没有改过（读不了文件、或者恢复记录缺失 / 损坏），所以保险起见留下了。 */
  uncertain?: boolean;
  /** 内置编辑器里没保存的内容（草稿）。 */
  from_draft?: boolean;
  /** 编辑的是本机文件：「重新上传」是写回原文件。 */
  local?: boolean;
}

export interface EditLeftoverResult {
  /** done 处理完了 / conflict 服务器上的文件变过、没传 / failed 没成功 */
  state: "done" | "conflict" | "failed";
  message?: string;
}

export function sftpEditLeftovers(): Promise<EditLeftover[]> {
  return invoke("sftp_edit_leftovers");
}

/** discard 放弃 / save_local 另存到文件夹 / reupload 重新上传 / reupload_overwrite 看过冲突后覆盖上传。 */
export function sftpEditLeftoverAction(
  id: string,
  action: "discard" | "save_local" | "reupload" | "reupload_overwrite",
  options: { sftpId?: string; destDir?: string } = {},
): Promise<EditLeftoverResult> {
  return invoke("sftp_edit_leftover_action", {
    id,
    action,
    sftpId: options.sftpId,
    destDir: options.destDir,
  });
}

export function onSftpEdit(
  handler: (event: SftpEditEvent) => void,
): Promise<UnlistenFn> {
  return listen<SftpEditEvent>("sftp_edit_event", (event) =>
    handler(event.payload),
  );
}

// ---------- 事件 ----------

export function onSftpState(
  handler: (event: SftpStateEvent) => void,
): Promise<UnlistenFn> {
  return listen<[string, string, string | null]>("sftp_state", (event) => {
    const [sftp_id, state, reason] = event.payload;
    handler({ sftp_id, state, reason: reason ?? undefined });
  });
}

/** 传输事件载荷是 (sftp_id, 任务)。 */
export function onSftpTransfer(
  handler: (sftpId: string, transfer: SftpTransfer) => void,
): Promise<UnlistenFn> {
  return listen<[string, SftpTransfer]>("sftp_transfer_event", (event) => {
    const [sftpId, transfer] = event.payload;
    handler(sftpId, transfer);
  });
}

// ---------- 复制 / 粘贴（M3-5 第 3 节） ----------

export interface PasteItem {
  path: string;
  is_dir: boolean;
}

export interface PasteRequest {
  /** 这一次粘贴的任务号（取消、进度都按它配对）。 */
  paste_id: string;
  /** 用户点了"改为经本机中转重试"：这次不用服务器端复制。 */
  force_relay?: boolean;
  side: "remote" | "local";
  /** 剪贴板里的文件来自哪个 SFTP 会话（远端）。 */
  source_sftp_id?: string;
  items: PasteItem[];
  target_dir: string;
}

export interface PasteItemResult {
  name: string;
  /** 源路径（"改为经本机中转重试"要用）。 */
  path?: string;
  is_dir?: boolean;
  /** ok 已完成 / queued 已进传输队列 / skipped 跳过 / failed 失败 / cancelled 取消 */
  state: "ok" | "queued" | "skipped" | "failed" | "cancelled";
  message?: string;
  /** cp 失败、可以改经本机中转重试。 */
  can_relay?: boolean;
}

export interface PasteOutcome {
  method: "copy_data" | "server_cp" | "relay" | "local";
  method_label: string;
  items: PasteItemResult[];
  job_ids: number[];
  elapsed_ms: number;
}

export function sftpPaste(sftpId: string, request: PasteRequest): Promise<PasteOutcome> {
  return invoke("sftp_paste", { sftpId, request });
}

/** 取消一次服务器端复制（copy-data 在文件之间停；cp 关闭执行通道）。 */
export function sftpPasteCancel(sftpId: string, pasteId: string): Promise<void> {
  return invoke("sftp_paste_cancel", { sftpId, pasteId });
}

/** 服务器端复制的进度：已经复制了几个文件、当前是哪个。 */
export function onSftpPasteProgress(
  handler: (sftpId: string, pasteId: string, doneFiles: number, current?: string) => void,
): Promise<UnlistenFn> {
  return listen<[string, string, number, string | null]>("sftp_paste_progress", (event) => {
    const [sftpId, pasteId, doneFiles, current] = event.payload;
    handler(sftpId, pasteId, doneFiles, current ?? undefined);
  });
}

// ---------- 远端压缩 / 解压（UI-13） ----------

/** 这台主机上压缩 / 解压能做什么（后端第一次问时探测，按会话缓存）。 */
export interface ArchiveCaps {
  /** 登录 shell 是 POSIX；不是的话压缩 / 解压菜单整个不显示。 */
  posix: boolean;
  tar: boolean;
  zip: boolean;
  unzip: boolean;
}

export function sftpArchiveCaps(sftpId: string): Promise<ArchiveCaps> {
  return invoke("sftp_archive_caps", { sftpId });
}

export type CompressFormat = "tar_gz" | "zip";

export interface CompressRequest {
  /** 这次操作的任务号（取消、进度都按它配对；取消沿用 `sftpPasteCancel`）。 */
  op_id: string;
  /** 当前目录（绝对路径）。 */
  dir: string;
  /** 选中的条目名（当前目录下的名字）。 */
  names: string[];
  format: CompressFormat;
}

export interface CompressResult {
  state: "ok" | "cancelled";
  name?: string | null;
  path?: string | null;
  warning?: string | null;
}

export function sftpCompress(sftpId: string, request: CompressRequest): Promise<CompressResult> {
  return invoke("sftp_compress", { sftpId, request });
}

/** 压缩包的大小和修改时间：检查之后、解压之前它被换掉就拒绝。 */
export interface ArchiveFingerprint {
  size: number | null;
  mtime: number | null;
}

/** 解压前的检查结果。 */
export interface ExtractPlan {
  format: "tar_gz" | "tar" | "tar_xz" | "tar_bz2" | "zip";
  total_entries: number;
  total_files: number;
  /** 目标里已经存在的同名文件个数。 */
  conflict_count: number;
  /** 前几个已存在的文件名。 */
  conflict_sample: string[];
  /** 目标文件夹现在已经存在。 */
  dest_exists: boolean;
  fingerprint: ArchiveFingerprint;
}

export interface ExtractPlanRequest {
  op_id: string;
  archive: string;
  dest: string;
}

/** 检查压缩包（拒绝指向目录外的路径、数同名文件）；用户取消返回 null。 */
export function sftpExtractPlan(
  sftpId: string,
  request: ExtractPlanRequest,
): Promise<ExtractPlan | null> {
  return invoke("sftp_extract_plan", { sftpId, request });
}

export type ConflictPolicy = "overwrite" | "skip_existing";

export interface ExtractRequest {
  op_id: string;
  archive: string;
  dest: string;
  policy: ConflictPolicy;
  /** 检查结果里解压时要核对的部分，原样带回来。 */
  plan: {
    fingerprint: ArchiveFingerprint;
    dest_exists: boolean;
    total_files: number;
    conflict_count: number;
  };
}

export interface ExtractResult {
  state: "ok" | "cancelled";
  extracted: number;
  skipped: number;
  warning?: string | null;
}

export function sftpExtract(sftpId: string, request: ExtractRequest): Promise<ExtractResult> {
  return invoke("sftp_extract", { sftpId, request });
}

/** 压缩进度：临时压缩包已经写了多少字节（读不到时是 null）。 */
export function onSftpArchiveProgress(
  handler: (sftpId: string, opId: string, bytes?: number) => void,
): Promise<UnlistenFn> {
  return listen<[string, string, number | null]>("sftp_archive_progress", (event) => {
    const [sftpId, opId, bytes] = event.payload;
    handler(sftpId, opId, bytes ?? undefined);
  });
}

// ---------- 属性：计算大小（M3-5 第 4 节） ----------

export interface SizeTotals {
  files: number;
  dirs: number;
  bytes: number;
  /** 跳过的符号链接 / 目录联接 */
  links: number;
  /** 读不了的目录数 */
  unreadable: number;
}

export interface SftpSizeEvent extends SizeTotals {
  /** 发起计算时前端生成的关联 ID，后端在每条事件里原样带回。 */
  request_id: string;
  calc_id: number;
  state: "running" | "done" | "cancelled" | "failed";
  error?: string | null;
}

export function sftpSizeStart(
  sftpId: string,
  side: "local" | "remote",
  path: string,
  /** 前端生成的关联 ID：同时算多个文件夹时，事件靠它认出是谁的，不能只按调用顺序。 */
  requestId: string,
): Promise<number> {
  return invoke("sftp_size_start", { sftpId, side, path, requestId });
}

export function sftpSizeCancel(sftpId: string, calcId: number): Promise<void> {
  return invoke("sftp_size_cancel", { sftpId, calcId });
}

export function onSftpSize(
  handler: (sftpId: string, event: SftpSizeEvent) => void,
): Promise<UnlistenFn> {
  return listen<[string, SftpSizeEvent]>("sftp_size_event", (event) => {
    const [sftpId, payload] = event.payload;
    handler(sftpId, payload);
  });
}

export function onSftpConflict(
  handler: (sftpId: string, prompt: SftpConflictPrompt) => void,
): Promise<UnlistenFn> {
  return listen<[string, SftpConflictPrompt]>(
    "sftp_conflict_prompt",
    (event) => {
      const [sftpId, prompt] = event.payload;
      handler(sftpId, prompt);
    },
  );
}

/** 一条冲突提问结束了（被回答 / 超时 / 标签关闭）：各窗口据此撤掉队列里的那条。 */
export function onSftpConflictDone(
  handler: (conflictId: number) => void,
): Promise<UnlistenFn> {
  return listen<number>("sftp_conflict_done", (event) => handler(event.payload));
}

export function onSftpDelete(
  handler: (sftpId: string, event: SftpDeleteEvent) => void,
): Promise<UnlistenFn> {
  return listen<[string, SftpDeleteEvent]>("sftp_delete_event", (event) => {
    const [sftpId, deleteEvent] = event.payload;
    handler(sftpId, deleteEvent);
  });
}
