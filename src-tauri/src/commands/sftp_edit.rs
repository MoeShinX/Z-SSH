//! 编辑器的命令接线（M3-5 第 2 节）：内置编辑器的登记、共享事件、恢复对话框。
//!
//! 逻辑在 `zssh_core::sftp::edit`（冲突判定、上传、三种处理方式都在那边测过）；这里只做 Tauri
//! 这一层该做的事：
//! - 临时文件放**数据目录**下的 `edit-tmp/<edit_id>/`（遵守 `ZSSH_DATA_DIR`，不放系统临时目录）；
//! - 恢复目录、恢复记录和草稿的扫描 / 清理（`recovery`）；
//! - 各命令发 `sftp_edit_event`，界面据此弹冲突 / 属主 / 失败提示；
//! - 关编辑 / 程序退出时清理临时文件。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tracing::{debug, info};
use zssh_core::ai::files::OwnerChange;
use zssh_core::sftp::edit::recovery::{self, Leftover};
use zssh_core::sftp::edit::{
    default_save_as_name, edit_root, remove_dir_quiet, EditBackend, EditOutcome, EditSession,
    RemoteSnapshot, Resolution,
};
use zssh_core::sftp::session::SftpChannel;
use zssh_core::ssh::BoxFuture;

use crate::commands::sftp::{tab_of, SftpRegistry};
use crate::state::AppState;

pub const SFTP_EDIT_EVENT: &str = "sftp_edit_event";

/// 编辑事件发到哪儿。生产里是前端（Tauri 事件），测试里是一个收集器。
pub trait EditSink: Send + Sync {
    fn emit(&self, event: EditEventDto);
}

/// 生产实现：发 Tauri 事件。
pub struct AppSink(pub AppHandle);

impl EditSink for AppSink {
    fn emit(&self, event: EditEventDto) {
        if let Err(err) = self.0.emit(SFTP_EDIT_EVENT, event) {
            debug!(%err, "发编辑事件失败");
        }
    }
}

/// 发给前端的编辑事件。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditEventDto {
    pub sftp_id: String,
    pub edit_id: u64,
    pub remote_path: String,
    /// uploading / uploaded / conflict / failed / discarded / owner_prompt / warning
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 冲突时远端现在的样子（`remote_gone` = 远端文件被删了）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_mtime: Option<u32>,
    pub remote_gone: bool,
    /// 另存为对话框里的默认名字。
    pub suggested_name: String,
    /// 这个编辑现在有没有"没落到远端的修改"（冲突、失败、正在上传）。
    pub dirty: bool,
    /// 属主的变化：`owner_prompt`（保存后属主会从 X 变成 Y，等用户选）和 `uploaded`
    /// （已经保存了，但属主没能保持）两种事件带着它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerChange>,
}

struct EditEntry {
    sftp_id: String,
    /// 打开编辑器时连到的服务器身份（用户名@主机:端口）。冲突 / 属主处理换用当前会话时，
    /// 必须核对仍然是同一台服务器（R-8 第 6 节）。
    server_identity: String,
    dir: PathBuf,
    session: Mutex<EditSession>,
    /// 冲突 / 失败 / 正在上传：用来数"还有几个没上传的修改"。
    dirty: AtomicBool,
    /// 编辑的是哪台主机的文件（用来判断「同一个文件是不是已经打开了」）。
    host_id: i64,
    /// 内置编辑器的状态：内容在界面里编辑，条目随编辑器标签活着，不随 SFTP 会话关闭而被清掉
    /// （断线后编辑内容要保留，重连后还能保存）。
    builtin: Option<editor::BuiltinState>,
}

/// 应用级的编辑登记。
pub struct EditRegistry {
    root: PathBuf,
    entries: StdMutex<HashMap<u64, Arc<EditEntry>>>,
    next_id: AtomicU64,
    /// 上次运行留下的、有没上传的修改的编辑（启动时扫出来，等用户处理）。
    leftovers: StdMutex<Vec<Leftover>>,
    /// 内置编辑器打开的本地文件。
    locals: StdMutex<HashMap<u64, Arc<editor::LocalEditorEntry>>>,
}

impl EditRegistry {
    /// `data_dir`：应用数据目录。启动时扫描上次留下的临时目录：没有未上传修改的清掉，
    /// 有的**留下**，交给用户处理（`sftp_edit_leftovers`）。新编辑的编号接在留下的后面。
    pub fn new(data_dir: &Path) -> Self {
        let root = edit_root(data_dir);
        let leftovers = recovery::scan(&root);
        let start = recovery::max_edit_number(&root);
        Self {
            root,
            entries: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(start),
            leftovers: StdMutex::new(leftovers),
            locals: StdMutex::new(HashMap::new()),
        }
    }

    fn leftovers(&self) -> std::sync::MutexGuard<'_, Vec<Leftover>> {
        self.leftovers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn get(&self, edit_id: u64) -> Option<Arc<EditEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&edit_id)
            .cloned()
    }

    fn insert(&self, edit_id: u64, entry: Arc<EditEntry>) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(edit_id, entry);
    }

    fn remove(&self, edit_id: u64) -> Option<Arc<EditEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&edit_id)
    }

    /// 程序退出：删掉没有未上传修改的临时目录（有的留给下次启动处理）。
    pub fn shutdown(&self) {
        let all: Vec<Arc<EditEntry>> = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in all {
            remove_unless_unsent(&entry.dir);
        }
        let locals: Vec<Arc<editor::LocalEditorEntry>> = lock_std(&self.locals)
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for local in locals {
            remove_unless_unsent(&local.dir);
        }
    }
}

fn lock_std<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 删一个编辑的临时目录，但**只有**确认没有未上传的修改（`recovery::safe_to_delete` =
/// `Unsent::None`）才删：有修改、读不了文件、恢复记录缺失或损坏（`Unknown`）都留着
/// （下次启动会提示用户，宁可多留一个目录也不能丢用户的修改）。R-8 第 5 节：所有自动清理
/// 入口都收敛到这条规则。
fn remove_unless_unsent(dir: &Path) {
    if recovery::safe_to_delete(dir) {
        remove_dir_quiet(dir);
    }
}

/// 事件里除了"哪个编辑、什么状态"之外的附加信息。
#[derive(Default)]
struct Extra {
    message: Option<String>,
    /// 冲突时远端现在的 (大小, 修改时间)。
    remote: Option<(u64, Option<u32>)>,
    remote_gone: bool,
    owner: Option<OwnerChange>,
}

fn emit_edit(
    sink: &dyn EditSink,
    entry: &EditEntry,
    edit_id: u64,
    remote_path: &str,
    state: &'static str,
    extra: Extra,
) {
    let dto = EditEventDto {
        sftp_id: entry.sftp_id.clone(),
        edit_id,
        remote_path: remote_path.to_owned(),
        state,
        message: extra.message,
        remote_size: extra.remote.map(|(size, _)| size),
        remote_mtime: extra.remote.and_then(|(_, mtime)| mtime),
        remote_gone: extra.remote_gone,
        suggested_name: default_save_as_name(remote_path),
        dirty: matches!(state, "uploading" | "conflict" | "failed" | "owner_prompt"),
        owner: extra.owner,
    };
    sink.emit(dto);
}

/// 把一次处理的结果变成事件，同时维护 dirty 标志。
/// 一次处理之后，编辑会话的状态（要发给界面、写进恢复记录的那几样）。
struct SessionView {
    remote_path: String,
    base: RemoteSnapshot,
    /// 实际上传（或拉回）的那些字节的摘要。
    synced_sha256: Option<String>,
    /// 上传成功但属主没能保持。
    owner_change: Option<OwnerChange>,
}

impl SessionView {
    fn of(session: &EditSession) -> Self {
        Self {
            remote_path: session.remote_path.clone(),
            base: session.base(),
            synced_sha256: session.synced_sha256().map(str::to_owned),
            owner_change: session.owner_change().cloned(),
        }
    }
}

fn report_outcome(
    sink: &dyn EditSink,
    entry: &EditEntry,
    edit_id: u64,
    view: SessionView,
    outcome: EditOutcome,
) {
    let SessionView {
        remote_path,
        base,
        synced_sha256,
        owner_change,
    } = view;
    let remote_path = remote_path.as_str();
    let synced_sha256 = synced_sha256.as_deref();
    // 内容已经和远端一致了：把临时目录里的记录更新成"已确认"，崩溃后不会再当成未上传。
    // 记的是**实际上传的那些字节**的摘要，不是现在本地文件的摘要（上传期间可能又保存了一次）。
    if matches!(
        outcome,
        EditOutcome::Uploaded { .. } | EditOutcome::Discarded
    ) {
        if let Some(sha) = synced_sha256 {
            if let Err(err) = recovery::mark_clean(&entry.dir, remote_path, base, sha) {
                debug!(%err, "更新编辑的恢复记录失败");
            }
        }
    }
    match outcome {
        EditOutcome::Uploaded { size } => {
            entry.dirty.store(false, Ordering::SeqCst);
            emit_edit(
                sink,
                entry,
                edit_id,
                remote_path,
                "uploaded",
                Extra {
                    message: Some(format!("{size} 字节")),
                    // 内容已经上传了，但属主没能保持：不能只显示"已上传"。
                    owner: owner_change,
                    ..Extra::default()
                },
            );
        }
        EditOutcome::Discarded => {
            entry.dirty.store(false, Ordering::SeqCst);
            emit_edit(
                sink,
                entry,
                edit_id,
                remote_path,
                "discarded",
                Extra::default(),
            );
        }
        EditOutcome::Conflict { remote } => {
            entry.dirty.store(true, Ordering::SeqCst);
            emit_edit(
                sink,
                entry,
                edit_id,
                remote_path,
                "conflict",
                Extra {
                    message: None,
                    remote: remote.map(|snapshot| (snapshot.size, snapshot.mtime)),
                    remote_gone: remote.is_none(),
                    owner: None,
                },
            );
        }
        EditOutcome::OwnerWouldChange(change) => {
            // 什么都没替换：这次保存还在等用户选，算没上传的修改。
            entry.dirty.store(true, Ordering::SeqCst);
            emit_edit(
                sink,
                entry,
                edit_id,
                remote_path,
                "owner_prompt",
                Extra {
                    message: Some(format!("保存后属主会从 {} 变成 {}", change.from, change.to)),
                    owner: Some(change),
                    ..Extra::default()
                },
            );
        }
        EditOutcome::Failed(reason) => {
            entry.dirty.store(true, Ordering::SeqCst);
            emit_edit(
                sink,
                entry,
                edit_id,
                remote_path,
                "failed",
                Extra {
                    message: Some(reason),
                    ..Extra::default()
                },
            );
        }
    }
}

pub mod editor;

#[cfg(test)]
mod editor_tests;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod tests;

/// 打开一个编辑要用到的"这台主机"信息。
#[derive(Debug, Clone)]
pub struct EditHost {
    pub host_id: i64,
    pub host_label: String,
    /// 打开时实际连到的服务器身份（`SftpChannel::server_key()`，用户名@主机:端口）。
    pub server_identity: String,
}

/// 处理冲突（`sftp_edit_resolve` 的主体）。
pub async fn resolve_edit<B: EditBackend + ?Sized>(
    registry: &Arc<EditRegistry>,
    backend: &B,
    sink: &dyn EditSink,
    edit_id: u64,
    resolution: Resolution,
) -> Result<(), String> {
    let entry = registry
        .get(edit_id)
        .ok_or_else(|| "这个编辑已经关闭".to_owned())?;
    let mut session = entry.session.lock().await;
    let outcome = session.resolve(backend, resolution).await;
    let view = SessionView::of(&session);
    if let Some(builtin) = entry.builtin.as_ref() {
        // 放弃修改：content/ 和基准都换成了服务器上的版本，编辑器重新载入成功之前不许再保存
        // （还在会话锁里设，保存检查它时也拿着这把锁）（R-9 T02）。
        if matches!(outcome, EditOutcome::Discarded) {
            builtin.needs_reload.store(true, Ordering::SeqCst);
        }
        // 挂起的那次保存处理完了（传上去了，或者被放弃了）：删掉版本不晚于它的草稿，
        // 处理期间又打的字在更晚的草稿里，留着（R-9 T03）。
        if matches!(
            outcome,
            EditOutcome::Uploaded { .. } | EditOutcome::Discarded
        ) {
            if let Some(version) = builtin.take_pending_version() {
                recovery::remove_draft_up_to(&entry.dir, &builtin.file_name, version);
            }
        }
    }
    drop(session);
    report_outcome(sink, &entry, edit_id, view, outcome);
    Ok(())
}

/// 一条当前打开的 SFTP 标签：冲突处理要用的连接，以及核对身份要的信息。
pub(crate) struct OpenedTab {
    pub backend: Arc<dyn EditBackend>,
    pub host_id: i64,
    pub server_identity: String,
}

/// 冲突 / 属主处理怎么找"这台主机当前可用的会话"：生产是 AppHandle 里的标签表，
/// 测试注入内存替身（不构造真实的 `SftpChannel` / `AppHandle`）。
pub(crate) trait EditTabSource: Send + Sync {
    /// 当前打开的所有标签：`(sftp_id, host_id, 服务器身份)`。
    fn candidates(&self) -> Vec<(String, i64, String)>;
    /// 取一条标签（拿连接和它实际连到的服务器身份）。
    fn open<'a>(&'a self, sftp_id: &'a str) -> BoxFuture<'a, Result<OpenedTab, String>>;
}

/// 不能用当前标签处理这个编辑的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickError {
    /// 这台主机现在没有打开的 SFTP 标签。
    NotFound,
    /// 有标签，但连到的服务器和打开编辑器时不是同一台（地址或用户名可能改过）。
    ServerChanged,
}

/// 从当前打开的标签里挑出这次处理要用的那条（R-8 第 6 节）。
///
/// 规则：打开时那条还在就用它；否则按 host_id 找这台主机当前的标签，优先服务器身份
/// 相同的；host 对但身份都不对时不能拿到别的服务器，明确报错。
fn pick_sftp_id_for_edit(
    tabs: &[(String, i64, String)],
    host_id: i64,
    server_identity: &str,
    stale_sftp_id: &str,
) -> Result<String, PickError> {
    // 1. 打开编辑器时那条标签还在、主机和服务器身份都对：优先用它。
    if let Some((id, tab_host, tab_identity)) = tabs.iter().find(|(id, _, _)| id == stale_sftp_id) {
        if *tab_host == host_id && tab_identity == server_identity {
            return Ok(id.clone());
        }
    }
    // 2. 否则找这台主机当前打开的标签：服务器身份相同的优先（主机配置没变）。
    let same_host: Vec<&(String, i64, String)> = tabs
        .iter()
        .filter(|(_, tab_host, _)| *tab_host == host_id)
        .collect();
    if let Some((id, _, _)) = same_host
        .iter()
        .find(|(_, _, identity)| identity == server_identity)
    {
        return Ok((*id).clone());
    }
    if same_host.is_empty() {
        return Err(PickError::NotFound);
    }
    // 3. 这台主机的标签都连到了别的服务器（地址 / 用户名改过？）：不能拿它写。
    Err(PickError::ServerChanged)
}

/// 冲突 / 属主处理的主体：按"这台主机当前的会话"找连接、核对服务器身份，再执行。
///
/// 这是 `sftp_edit_resolve` 去掉 AppHandle 之后的原样主体；命令只剩一行转接，
/// 测试用内存 `EditTabSource` 覆盖"关掉标签再重开"的场景（R-8 第 6 节）。
pub(crate) async fn resolve_with_source<S: EditTabSource + ?Sized>(
    registry: &Arc<EditRegistry>,
    source: &S,
    sink: &dyn EditSink,
    edit_id: u64,
    resolution: Resolution,
    host_label: &str,
) -> Result<(), String> {
    let entry = registry
        .get(edit_id)
        .ok_or_else(|| "这个编辑已经关闭".to_owned())?;
    let sftp_id = pick_sftp_id_for_edit(
        &source.candidates(),
        entry.host_id,
        &entry.server_identity,
        &entry.sftp_id,
    )
    .map_err(|err| match err {
        PickError::NotFound => {
            format!("请先打开「{host_label}」的 SFTP 标签，再处理这个冲突或属主询问")
        }
        PickError::ServerChanged => format!(
            "当前打开的 SFTP 标签连到的服务器和打开「{host_label}」时不一致（服务器地址或用户名改过？），请关掉标签重开后再试"
        ),
    })?;
    let tab = source.open(&sftp_id).await?;
    // 取快照和取标签之间标签可能刚换过：再核对一次，避免写到别的服务器。
    if tab.host_id != entry.host_id || tab.server_identity != entry.server_identity {
        return Err(format!(
            "当前打开的 SFTP 标签连到的服务器和打开「{host_label}」时不一致（服务器地址或用户名改过？），请关掉标签重开后再试"
        ));
    }
    resolve_edit(registry, tab.backend.as_ref(), sink, edit_id, resolution).await
}

/// 生产实现：标签表在 Tauri 管理的 `SftpRegistry` 里。
struct AppTabSource<'a>(&'a AppHandle);

impl EditTabSource for AppTabSource<'_> {
    fn candidates(&self) -> Vec<(String, i64, String)> {
        self.0.state::<Arc<SftpRegistry>>().tab_candidates()
    }

    fn open<'a>(&'a self, sftp_id: &'a str) -> BoxFuture<'a, Result<OpenedTab, String>> {
        Box::pin(async move {
            let tab = tab_of(self.0, sftp_id).await?;
            Ok(OpenedTab {
                backend: Arc::clone(&tab.browse) as Arc<dyn EditBackend>,
                host_id: tab.host_id,
                server_identity: tab.browse.server_key().to_owned(),
            })
        })
    }
}

/// 冲突提示里的选择：overwrite / discard / save_as / 属主的两个选择。
#[tauri::command]
pub async fn sftp_edit_resolve(
    app: AppHandle,
    edit_id: u64,
    action: String,
    name: Option<String>,
) -> Result<(), String> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    let entry = registry
        .get(edit_id)
        .ok_or_else(|| "这个编辑已经关闭".to_owned())?;
    let host_label = app
        .state::<AppState>()
        .store
        .host_get(entry.host_id)
        .map(|host| host.name)
        .unwrap_or_default();
    let resolution = match action.as_str() {
        "overwrite" => Resolution::Overwrite,
        "discard" => Resolution::Discard,
        "save_as" => Resolution::SaveAs(name.unwrap_or_default()),
        // 属主保持不了时用户的选择：仍然保存（属主改变）/ 直接写入原文件（保留属主）。
        "owner_keep_new" => Resolution::KeepNewOwner,
        "owner_in_place" => Resolution::InPlace,
        other => return Err(format!("不认识的处理方式：{other}")),
    };
    resolve_with_source(
        &registry,
        &AppTabSource(&app),
        &AppSink(app.clone()),
        edit_id,
        resolution,
        &host_label,
    )
    .await
}

/// 上次运行留下的、有没上传的修改（界面启动时问一次）。
#[derive(Debug, Clone, Serialize)]
pub struct LeftoverDto {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    /// 对应的服务器文件；认不出来源时没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// 没法确认这份文件有没有改过（读不了文件，或者恢复记录缺失 / 损坏），所以保险起见留下了。
    pub uncertain: bool,
    /// 内置编辑器里**没保存**的内容（草稿），不是编辑器保存过但没传上去的文件。
    pub from_draft: bool,
    /// 编辑的是本机的文件：「重新上传」换成「写回原文件」，不需要 SFTP 会话。
    pub local: bool,
}

fn leftover_dto(leftover: &Leftover) -> LeftoverDto {
    LeftoverDto {
        id: leftover.id.clone(),
        file_name: leftover
            .local_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: std::fs::metadata(&leftover.local_path)
            .map(|meta| meta.len())
            .unwrap_or(0),
        remote_path: leftover.meta.as_ref().map(|meta| meta.remote_path.clone()),
        host_id: leftover.meta.as_ref().map(|meta| meta.host_id),
        host_label: leftover.meta.as_ref().map(|meta| meta.host_label.clone()),
        uncertain: leftover.uncertain,
        from_draft: leftover.from_draft,
        local: leftover.meta.as_ref().is_some_and(|meta| meta.local),
    }
}

#[tauri::command]
pub fn sftp_edit_leftovers(app: AppHandle) -> Vec<LeftoverDto> {
    let registry = app.state::<Arc<EditRegistry>>();
    let list: Vec<LeftoverDto> = registry.leftovers().iter().map(leftover_dto).collect();
    list
}

/// 处理一份留下的修改的结果。
#[derive(Debug, Clone, Serialize)]
pub struct LeftoverResultDto {
    /// done（处理完了，这一项消失）/ conflict（服务器上的文件变过，没传）/ failed
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn done(message: Option<String>) -> LeftoverResultDto {
    LeftoverResultDto {
        state: "done",
        message,
    }
}

/// 处理一份留下的修改：`discard` 放弃 / `save_local` 另存到 `dest_dir` /
/// `reupload` 重新上传（`sftp_id` 是那台主机已经打开的 SFTP 标签）/
/// `reupload_overwrite` 看过冲突之后强制覆盖上传。
#[tauri::command]
pub async fn sftp_edit_leftover_action(
    app: AppHandle,
    id: String,
    action: String,
    sftp_id: Option<String>,
    dest_dir: Option<String>,
) -> Result<LeftoverResultDto, String> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    let leftover = registry
        .leftovers()
        .iter()
        .find(|item| item.id == id)
        .cloned()
        .ok_or_else(|| "这一项已经处理过了".to_owned())?;
    let finish = |registry: &EditRegistry| {
        remove_dir_quiet(&leftover.dir);
        registry.leftovers().retain(|item| item.id != id);
    };
    match action.as_str() {
        "discard" => {
            finish(&registry);
            Ok(done(None))
        }
        "save_local" => {
            let dest = dest_dir
                .filter(|dir| !dir.trim().is_empty())
                .ok_or_else(|| "没有选择保存的文件夹".to_owned())?;
            let saved = recovery::save_copy(&leftover, Path::new(&dest))
                .map_err(|err| format!("保存失败（修改还在，可以换个文件夹再试）：{err}"))?;
            finish(&registry);
            Ok(done(Some(saved.display().to_string())))
        }
        "reupload" | "reupload_overwrite" if leftover.meta.as_ref().is_some_and(|m| m.local) => {
            // 本地文件的编辑：写回原文件（先看原文件在这期间有没有被别的程序改过）。
            let force = action == "reupload_overwrite";
            Ok(match recovery::restore_local(&leftover, force)? {
                recovery::LocalRestore::Written { size } => {
                    finish(&registry);
                    done(Some(format!("{size} 字节")))
                }
                recovery::LocalRestore::Conflict => LeftoverResultDto {
                    state: "conflict",
                    message: Some(
                        "这个文件在你离开之后又被改过（或者已经被删了），没有写回".to_owned(),
                    ),
                },
            })
        }
        "reupload" | "reupload_overwrite" => {
            let meta = leftover
                .meta
                .clone()
                .ok_or_else(|| "这份修改认不出对应的服务器文件，只能另存到本地或放弃".to_owned())?;
            let sftp_id = sftp_id.ok_or_else(|| {
                format!("请先打开「{}」的 SFTP 标签，再重新上传", meta.host_label)
            })?;
            let tab = tab_of(&app, &sftp_id).await?;
            if tab.host_id != meta.host_id {
                return Err(format!(
                    "这份修改来自「{}」，当前的 SFTP 标签是另一台主机",
                    meta.host_label
                ));
            }
            let backend: Arc<SftpChannel> = Arc::clone(&tab.browse);
            reupload_leftover(
                &registry,
                &id,
                &leftover,
                backend.as_ref(),
                action == "reupload_overwrite",
            )
            .await
        }
        other => Err(format!("不认识的处理方式：{other}")),
    }
}

/// 「重新上传」成功之后怎么收尾（命令主体；不依赖 AppHandle，测试可以传内存替身）。
///
/// 只有能确认 `Unsent::None` 才删目录、从列表里移除；上传期间又改了（`Yes`）或者读不到
/// 记录 / 本地内容（`Unknown`）都必须保留（R-8 第 5 节："读不到"不等于"没有修改"）。
async fn reupload_leftover<B: EditBackend>(
    registry: &EditRegistry,
    id: &str,
    leftover: &Leftover,
    backend: &B,
    overwrite: bool,
) -> Result<LeftoverResultDto, String> {
    // 只核对 host_id 不够：主机记录的地址 / 用户名可能已经改过，同一个 host_id 连到了另一台服务器
    // （R-10 M17，和普通保存的 Q01 同一个规则）。记录里没有身份（旧版本留下的）就无法确认，不上传。
    let saved = leftover
        .meta
        .as_ref()
        .and_then(|meta| meta.server_identity.clone());
    match (saved, backend.server_identity()) {
        (Some(saved), Some(now)) if saved == now => {}
        (Some(_), Some(_)) => {
            return Err(
                "这份修改来自另一台服务器（主机的地址或用户名改过？），没有上传。\
                        可以另存到本地后自己处理"
                    .to_owned(),
            )
        }
        (None, _) => {
            return Err(
                "这份修改是旧版本留下的，没有记下当时连的是哪台服务器，无法确认现在连的是\
                        同一台，没有上传。请另存到本地后自己上传"
                    .to_owned(),
            )
        }
        (Some(_), None) => return Err("确认不了当前连接的是哪台服务器，没有上传".to_owned()),
    }
    let outcome = recovery::reupload(leftover, backend, overwrite).await?;
    Ok(match outcome {
        EditOutcome::Uploaded { size } => {
            // 传上去的是哪份内容，记录里就是哪份（`reupload` 已经更新）。收尾必须用三态：
            // 只有确认 Unsent::None 才能删目录；Yes（上传期间又改了）和 Unknown（读不到记录 /
            // 本地内容）都要保留（R-8 第 5 节：读不到不等于没有修改）。
            match recovery::unsent_state(&leftover.dir) {
                recovery::Unsent::None => {
                    remove_dir_quiet(&leftover.dir);
                    registry.leftovers().retain(|item| item.id != id);
                    done(Some(format!("{size} 字节")))
                }
                recovery::Unsent::Yes => {
                    if let Some(item) = registry.leftovers().iter_mut().find(|item| item.id == id) {
                        item.meta = recovery::read_meta(&leftover.dir);
                    }
                    LeftoverResultDto {
                        state: "failed",
                        message: Some(
                            "已经上传了一部分，但上传期间这个文件又被改过，新的修改还保留着，请再处理一次"
                                .to_owned(),
                        ),
                    }
                }
                recovery::Unsent::Unknown => {
                    // 记录 / 本地内容现在读不了：保留目录和列表项，让用户稍后重试。
                    LeftoverResultDto {
                        state: "failed",
                        message: Some(
                            "已经上传，但随后读不到本地内容或恢复记录，无法确认是否还有修改；目录和列表项已经保留，请稍后重试"
                                .to_owned(),
                        ),
                    }
                }
            }
        }
        EditOutcome::Conflict { remote } => LeftoverResultDto {
            state: "conflict",
            message: Some(if remote.is_some() {
                "服务器上的文件在你离开之后又被改过，没有上传".to_owned()
            } else {
                "服务器上的文件已经被删掉了，没有上传".to_owned()
            }),
        },
        EditOutcome::OwnerWouldChange(change) => LeftoverResultDto {
            state: "failed",
            message: Some(format!(
                "重新上传后文件的属主会从 {} 变成 {}，没有上传。请先打开这个文件的 SFTP 标签，用编辑器重新保存一次再选处理方式。",
                change.from, change.to
            )),
        },
        EditOutcome::Failed(reason) => LeftoverResultDto {
            state: "failed",
            message: Some(reason),
        },
        EditOutcome::Discarded => done(None),
    })
}

/// 程序退出时调（`main.rs` 的 `RunEvent::Exit`）。
pub fn shutdown(app: &AppHandle) {
    if let Some(registry) = app.try_state::<Arc<EditRegistry>>() {
        registry.shutdown();
        info!("编辑的临时文件已清理");
    }
}
