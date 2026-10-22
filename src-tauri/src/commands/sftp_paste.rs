//! 复制 / 粘贴（M3-5 第 3 节）。
//!
//! 只做远端 → 远端和本地 → 本地（远端和本地之间用上传 / 下载），不碰系统剪贴板。
//!
//! 远端复制按顺序选办法（见 `zssh_core::sftp::server_copy`）：
//! 1. 同一台主机且服务器声明了 `copy-data`：服务器自己拷；
//! 2. 同一台主机：后台 exec 通道里 `cp -a -- '源' '目标'`；`cp` 跑了但失败**不会**换办法；
//!    只有命令根本没法用（没有 cp / 不让开 exec）才往下走；
//! 3. 经本机中转：进传输队列（`Direction::RemoteCopy`，源可以在另一台主机上）。
//!
//! 冲突在动手之前就决定好（沿用传输的冲突对话框）：服务器端复制一旦开始就不能中途问用户。
//! 经本机中转的复制和本地复制是传输任务，同名文件的冲突由引擎逐个文件问；只有"粘贴到源所在
//! 的目录"（名字必然撞上源自己）才先在这里问一次，建议名是「名字 - 副本」。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use zssh_core::pathnorm::key as pathnorm_key;
use zssh_core::sftp::path as remote_path;
use zssh_core::sftp::server_copy::{
    choose_server_copy, copy_with_copy_data, copy_with_cp, ensure_folder_not_inside,
    is_same_or_inside, shell_kind_label, supports_copy_data, CopyConflictDecision,
    CopyConflictInfo, CopyConflictResolver, CopyMethod, CopyOptions, CopyProgress, CpFailure,
};
use zssh_core::sftp::transfer::{
    copy_path, with_file_name, ConflictAction, ConflictBatch, ConflictDecision, ConflictInfo,
    ConflictResolver, Direction, TransferJob,
};
use zssh_core::sftp::SftpChannel;

use crate::commands::sftp::{remote_shell_kind, tab_of, SftpRegistry, SftpTab, UiConflictResolver};
use crate::state::CancelSignal;

/// 服务器上的 `cp` 最多等多久（大目录可能要几分钟；界面显示"服务器上复制中"和已用时间）。
const CP_WAIT: Duration = Duration::from_secs(60 * 60);

/// 服务器端复制的进度（界面显示"第 N 个文件"）。
pub const SFTP_PASTE_PROGRESS_EVENT: &str = "sftp_paste_progress";

#[derive(Debug, Clone, Deserialize)]
pub struct PasteItem {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PasteRequest {
    /// 这一次粘贴的任务号（界面用它取消；空的话后端自己生成一个，但界面就没法取消）。
    #[serde(default)]
    pub paste_id: String,
    /// 用户点了"改为经本机中转重试"：这次不用服务器端复制。
    #[serde(default)]
    pub force_relay: bool,
    /// "remote" | "local"
    pub side: String,
    /// 剪贴板里的文件来自哪个 SFTP 会话（远端；本地不需要）。
    pub source_sftp_id: Option<String>,
    pub items: Vec<PasteItem>,
    pub target_dir: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PasteItemResult {
    pub name: String,
    /// 源路径（"改为经本机中转重试"要用）。
    pub path: String,
    pub is_dir: bool,
    /// ok 已完成 / queued 已进传输队列 / skipped 按冲突选择跳过 / failed / cancelled
    pub state: &'static str,
    pub message: Option<String>,
    /// 这次失败可以改经本机中转重试（cp 跑了但失败；界面显示按钮，点了才换办法）。
    pub can_relay: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PasteOutcome {
    /// copy_data / server_cp / relay / local
    pub method: &'static str,
    pub method_label: String,
    pub items: Vec<PasteItemResult>,
    /// 进了传输队列的任务号（中转复制、本地复制）。
    pub job_ids: Vec<u64>,
    pub elapsed_ms: u64,
}

fn name_of(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn item_result(
    item: &PasteItem,
    state: &'static str,
    message: Option<String>,
    can_relay: bool,
) -> PasteItemResult {
    PasteItemResult {
        name: name_of(&item.path),
        path: item.path.clone(),
        is_dir: item.is_dir,
        state,
        message,
        can_relay,
    }
}

fn ok_item(item: &PasteItem) -> PasteItemResult {
    item_result(item, "ok", None, false)
}

fn failed_item(item: &PasteItem, message: impl Into<String>) -> PasteItemResult {
    item_result(item, "failed", Some(message.into()), false)
}

/// `cp` 跑了但失败：这个条目可以改经本机中转（界面给按钮，点了才换）。
fn failed_relay_item(item: &PasteItem, message: impl Into<String>) -> PasteItemResult {
    item_result(item, "failed", Some(message.into()), true)
}

fn skipped_item(item: &PasteItem) -> PasteItemResult {
    item_result(item, "skipped", None, false)
}

fn queued_item(item: &PasteItem, message: Option<String>) -> PasteItemResult {
    item_result(item, "queued", message, false)
}

fn cancelled_item(item: &PasteItem) -> PasteItemResult {
    item_result(item, "cancelled", None, false)
}

/// 前端没给粘贴号时自己造一个（仍然唯一；只是界面没法取消这一次）。
fn make_paste_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "paste-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// 粘贴前的检查：文件夹不能粘贴到它自己或它的子文件夹里。返回要给用户看的原因。
pub fn refuse_into_self(remote: bool, item: &PasteItem, target_dir: &str) -> Option<String> {
    if !item.is_dir {
        return None;
    }
    let inside = if remote {
        is_same_or_inside(&item.path, target_dir)
    } else {
        zssh_core::pathnorm::is_within(Path::new(&item.path), Path::new(target_dir))
    };
    inside.then(|| {
        format!(
            "不能把文件夹「{}」粘贴到它自己或它的子文件夹里",
            name_of(&item.path)
        )
    })
}

/// 同一个目录里已经有的名字时，第一个没被占用的「名字 - 副本」「名字 - 副本 2」…
pub fn first_free_copy_path(dest: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    let mut index = 1;
    loop {
        let candidate = copy_path(dest, index);
        if !taken(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

/// 冲突对话框里预填的远端副本名：第一个没被占用的「名字 - 副本」「名字 - 副本 2」…
/// 只是建议——查不了（出错）或者试了 1000 个还被占着，就退回固定的「名字 - 副本」，
/// 不能因为这个卡住提问。
async fn suggest_remote_copy_name(browse: &SftpChannel, target: &str) -> String {
    for index in 1..=1000 {
        let candidate = copy_path(target, index);
        match browse.lstat(&candidate).await {
            Ok(_) => {}
            Err(zssh_core::sftp::SftpError::NotFound { .. }) => return candidate,
            Err(_) => break,
        }
    }
    copy_path(target, 1)
}

enum Dest {
    /// 目标是这个路径，现在不存在（或者是要覆盖的文件）。
    Free(String),
    /// 合并进已经存在的文件夹。
    Merge(String),
    Skip,
}

/// 一次粘贴里"先在这里问"的冲突用的状态：和这次粘贴提交的传输任务共用同一个批次，
/// 所以在哪里勾了"全部应用"，其余同名的（不管是这里问的还是引擎逐个文件问的）都按它处理。
struct ConflictState {
    batch: Arc<ConflictBatch>,
}

/// 粘贴命令。远端：`sftp_id` 是**目标**所在的会话；本地：随便一个能弹冲突对话框的会话。
#[tauri::command]
pub async fn sftp_paste(
    sftp_id: String,
    request: PasteRequest,
    app: AppHandle,
) -> Result<PasteOutcome, String> {
    if request.items.is_empty() {
        return Err("剪贴板是空的".to_owned());
    }
    if request.target_dir.trim().is_empty() {
        return Err("没有目标文件夹".to_owned());
    }
    let started = Instant::now();
    let target_tab = tab_of(&app, &sftp_id).await?;
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    let paste_id = if request.paste_id.trim().is_empty() {
        make_paste_id()
    } else {
        request.paste_id.clone()
    };
    // 服务器端复制可能很久：登记一个取消信号，界面按 (标签, 粘贴号) 取消。
    let cancel = registry.register_paste(&sftp_id, &paste_id)?;
    let resolver = UiConflictResolver::new(app.clone(), Arc::clone(&registry), sftp_id.clone());
    let result = match request.side.as_str() {
        "remote" => {
            paste_remote(
                &app,
                &sftp_id,
                &target_tab,
                &resolver,
                &request,
                &cancel,
                &paste_id,
            )
            .await
        }
        "local" => paste_local(&target_tab, &resolver, &request).await,
        other => Err(format!("不认识的粘贴位置：{other}")),
    };
    registry.finish_paste(&sftp_id, &paste_id);
    let mut outcome = result?;
    outcome.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(outcome)
}

/// 取消一次服务器端复制：copy-data 在文件之间停，cp 关闭执行通道。
#[tauri::command]
pub async fn sftp_paste_cancel(
    sftp_id: String,
    paste_id: String,
    app: AppHandle,
) -> Result<(), String> {
    let registry = app.state::<Arc<SftpRegistry>>();
    if registry.cancel_paste(&sftp_id, &paste_id) {
        Ok(())
    } else {
        Err(format!("这次粘贴已经结束了：{paste_id}"))
    }
}

// ---------------------------------------------------------------------------
// 远端
// ---------------------------------------------------------------------------

/// 合并进已有文件夹时，同名文件逐个问（M3-5b 第 5 项）。
///
/// 顶层的「覆盖」只表示"合并这个文件夹"，里面的同名文件仍然逐个问；用户勾了
/// "对后续都这样处理"之后，后面的同名文件按记住的动作处理（和传输引擎一致）。
/// "记住"放在这次粘贴的批次里：合并多个文件夹时第二个文件夹不会再从头问。
struct MergeConflictBridge<'a> {
    resolver: &'a UiConflictResolver,
    browse: &'a SftpChannel,
    batch: &'a Arc<ConflictBatch>,
}

impl MergeConflictBridge<'_> {
    async fn rename_target(
        &self,
        target: &str,
        rename_to: Option<&str>,
        from_memory: bool,
    ) -> Result<String, String> {
        let name = if from_memory {
            None
        } else {
            rename_to.map(str::trim).filter(|name| !name.is_empty())
        };
        let chosen = match name {
            Some(name) => {
                if name.contains('/') {
                    return Err("新名字不能为空，也不能含有 /".to_owned());
                }
                with_file_name(target, name)
            }
            None => {
                let mut index = 1;
                loop {
                    let candidate = copy_path(target, index);
                    if self.browse.lstat(&candidate).await.is_err() {
                        break candidate;
                    }
                    index += 1;
                }
            }
        };
        if self.browse.lstat(&chosen).await.is_ok() {
            return Err(format!("「{}」也已经存在", name_of(&chosen)));
        }
        Ok(chosen)
    }
}

impl CopyConflictResolver for MergeConflictBridge<'_> {
    fn decide<'a>(
        &'a self,
        info: CopyConflictInfo,
    ) -> zssh_core::ssh::BoxFuture<'a, Result<CopyConflictDecision, zssh_core::sftp::SftpError>>
    {
        Box::pin(async move {
            let remembered = self.batch.action();
            let decision = match remembered {
                Some(action) => ConflictDecision {
                    action,
                    apply_to_all: true,
                    rename_to: None,
                },
                None => {
                    let suggested_target =
                        suggest_remote_copy_name(self.browse, &info.target).await;
                    self.resolver
                        .decide(ConflictInfo {
                            job: 0,
                            direction: Direction::RemoteCopy,
                            source: info.source.clone(),
                            target: info.target.clone(),
                            source_size: info.source_size,
                            target_size: info.target_size,
                            source_mtime: info.source_mtime,
                            target_mtime: info.target_mtime,
                            can_resume: false,
                            suggested_target,
                            batch: Some(self.batch.id()),
                        })
                        .await
                }
            };
            if decision.apply_to_all {
                self.batch.remember(decision.action);
            }
            match decision.action {
                ConflictAction::Skip => Ok(CopyConflictDecision::Skip),
                ConflictAction::Overwrite | ConflictAction::Resume => {
                    Ok(CopyConflictDecision::Overwrite)
                }
                ConflictAction::Rename => {
                    let chosen = self
                        .rename_target(
                            &info.target,
                            decision.rename_to.as_deref(),
                            remembered.is_some(),
                        )
                        .await
                        .map_err(zssh_core::sftp::SftpError::Remote)?;
                    Ok(CopyConflictDecision::Rename(chosen))
                }
            }
        })
    }
}

async fn paste_remote(
    app: &AppHandle,
    sftp_id: &str,
    target_tab: &Arc<SftpTab>,
    resolver: &UiConflictResolver,
    request: &PasteRequest,
    cancel: &Arc<CancelSignal>,
    paste_id: &str,
) -> Result<PasteOutcome, String> {
    let source_id = request
        .source_sftp_id
        .clone()
        .unwrap_or_else(|| sftp_id.to_owned());
    let source_tab = tab_of(app, &source_id)
        .await
        .map_err(|_| "复制的源所在的连接已经关闭，请重新打开那个标签再复制".to_owned())?;
    let same_host = source_tab.browse.server_key() == target_tab.browse.server_key();

    let target_attrs = target_tab
        .browse
        .stat(&request.target_dir)
        .await
        .map_err(|err| format!("目标文件夹不可用：{err}"))?;
    if !target_attrs.is_dir() {
        return Err(format!("{} 不是文件夹", request.target_dir));
    }
    if same_host {
        for item in &request.items {
            if let Some(reason) = refuse_into_self(true, item, &request.target_dir) {
                return Err(reason);
            }
            // 字符串比较看不出符号链接别名（目标文件夹可能指回源树里）：再按服务器上的
            // 真实路径核对一遍；中转、copy-data、cp 三条路都在这之后（C21）。
            if item.is_dir {
                ensure_folder_not_inside(
                    target_tab.browse.as_ref(),
                    &item.path,
                    &request.target_dir,
                )
                .await
                .map_err(|err| match err {
                    zssh_core::sftp::SftpError::InvalidInput(_) => format!(
                        "不能把文件夹「{}」粘贴到它自己或它的子文件夹里（目标经符号链接指回了它）",
                        name_of(&item.path)
                    ),
                    other => other.to_string(),
                })?;
            }
        }
    }

    // 这一次粘贴的冲突批次：先在这里问的和提交给引擎的任务共用，"全部应用"对整次粘贴生效。
    let batch = target_tab.engine.new_batch();
    let mut conflicts = ConflictState {
        batch: Arc::clone(&batch),
    };
    let mut results = Vec::new();
    let mut job_ids = Vec::new();
    // 这一次粘贴实际用的办法（第一个条目决定，后面的沿用）。
    let mut method: Option<CopyMethod> = if request.force_relay {
        Some(CopyMethod::Relay)
    } else {
        None
    };
    // 临时文件名里的任务标识（M3-5b 第 1 项）：一次粘贴一个，不会和别的任务撞。
    let owner = format!(
        "p{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    // 进度：界面显示"第 N 个文件"。
    let on_progress = |progress: CopyProgress| {
        let _ = app.emit(
            SFTP_PASTE_PROGRESS_EVENT,
            (
                sftp_id.to_owned(),
                paste_id.to_owned(),
                progress.files_done,
                progress.current,
            ),
        );
    };
    let copy_options = CopyOptions {
        owner: &owner,
        server: target_tab.browse.server_key(),
        cancel: cancel.atomic(),
        progress: Some(&on_progress),
        conflict: None,
    };

    if source_id != sftp_id && !same_host {
        // 源在另一台主机：把它的通道提供者登记给目标这边的引擎。
        target_tab
            .endpoints
            .register_peer(&source_id, source_tab.provider.clone());
    }

    for item in &request.items {
        let name = name_of(&item.path);
        // 已经取消了：后面的条目都不再开始（已经开始的在下面按自己的错误收尾）。
        if cancel.is_cancelled() {
            results.push(cancelled_item(item));
            continue;
        }
        let dest = remote_path::join(&request.target_dir, &name);
        let same_entry =
            same_host && remote_path::normalize(&dest) == remote_path::normalize(&item.path);

        // 经本机中转的复制，冲突交给引擎逐个文件问；其余情况在这里先决定好。
        let relay_possible = !same_host || method == Some(CopyMethod::Relay) || request.force_relay;
        let needs_precheck = !relay_possible || same_entry;
        let dest = if needs_precheck {
            match resolve_remote_dest(target_tab, resolver, item, dest, same_entry, &mut conflicts)
                .await
            {
                Ok(Dest::Skip) => {
                    results.push(skipped_item(item));
                    continue;
                }
                Ok(Dest::Free(path)) | Ok(Dest::Merge(path)) => {
                    // Merge 的信息在后面按"目标是不是已存在的目录"重新判断。
                    path
                }
                Err(message) => {
                    results.push(failed_item(item, message));
                    continue;
                }
            }
        } else {
            dest
        };

        // 复制。
        if !same_host {
            match submit_relay(target_tab, &batch, &source_id, item, &dest, same_host) {
                Ok(id) => {
                    method = Some(CopyMethod::Relay);
                    job_ids.push(id);
                    results.push(queued_item(item, None));
                }
                Err(message) => results.push(failed_item(item, message)),
            }
            continue;
        }

        // 已经决定用中转（用户点了重试、或者上一个条目已经改成中转）：同一台主机也走
        // 引擎（数据经过本机）。
        if method == Some(CopyMethod::Relay) {
            match submit_relay(target_tab, &batch, &source_id, item, &dest, same_host) {
                Ok(id) => {
                    job_ids.push(id);
                    results.push(queued_item(item, None));
                }
                Err(message) => results.push(failed_item(item, message)),
            }
            continue;
        }

        // 合并进已有文件夹：里面同名的文件要逐个问（M3-5b 第 5 项）。cp 没法逐个问，
        // 有 copy-data 就用它；没有就经本机中转。
        let merging = item.is_dir
            && target_tab
                .browse
                .lstat(&dest)
                .await
                .map(|attrs| attrs.is_dir())
                .unwrap_or(false);
        if merging && !supports_copy_data(target_tab.browse.caps()) {
            method = Some(CopyMethod::Relay);
            match submit_relay(target_tab, &batch, &source_id, item, &dest, same_host) {
                Ok(id) => {
                    job_ids.push(id);
                    results.push(queued_item(
                        item,
                        Some("合并要逐个文件确认，服务器不支持 copy-data，改经本机中转".to_owned()),
                    ));
                }
                Err(message) => results.push(failed_item(item, message)),
            }
            continue;
        }

        // 没有 copy-data 时先确认远端是 POSIX shell 才用 cp；不是就经本机中转
        // （M3-5b 第 4 项）。探测结果缓存在标签上，后面的条目不用再问。
        let chosen = match method {
            Some(method) => method,
            None => {
                let has_copy_data = supports_copy_data(source_tab.browse.caps());
                let shell = if has_copy_data || request.force_relay {
                    None
                } else {
                    Some(remote_shell_kind(target_tab).await)
                };
                let chosen = choose_server_copy(has_copy_data, request.force_relay, shell);
                if chosen == CopyMethod::Relay {
                    let reason = if request.force_relay {
                        "按你的选择".to_owned()
                    } else {
                        format!("远端的登录 shell 不是 POSIX（{}）", shell_kind_label(shell))
                    };
                    method = Some(CopyMethod::Relay);
                    match submit_relay(target_tab, &batch, &source_id, item, &dest, same_host) {
                        Ok(id) => {
                            job_ids.push(id);
                            results
                                .push(queued_item(item, Some(format!("{reason}，改经本机中转"))));
                        }
                        Err(message) => results.push(failed_item(item, message)),
                    }
                    continue;
                }
                chosen
            }
        };
        match chosen {
            CopyMethod::CopyData => {
                let merge_bridge = MergeConflictBridge {
                    resolver,
                    browse: target_tab.browse.as_ref(),
                    batch: &batch,
                };
                let mut item_options = CopyOptions {
                    owner: &owner,
                    server: target_tab.browse.server_key(),
                    cancel: cancel.atomic(),
                    progress: Some(&on_progress),
                    conflict: None,
                };
                if merging {
                    item_options.conflict = Some(&merge_bridge);
                }
                match copy_with_copy_data(
                    target_tab.browse.as_ref(),
                    &item.path,
                    &dest,
                    &item_options,
                )
                .await
                {
                    Ok(stats) => {
                        method = Some(CopyMethod::CopyData);
                        results.push(item_result(
                            item,
                            "ok",
                            (!stats.skipped_links.is_empty()).then(|| {
                                format!("跳过了 {} 个符号链接", stats.skipped_links.len())
                            }),
                            false,
                        ));
                    }
                    Err(zssh_core::sftp::SftpError::Cancelled) => {
                        method = Some(CopyMethod::CopyData);
                        results.push(cancelled_item(item));
                    }
                    Err(err) => {
                        method = Some(CopyMethod::CopyData);
                        results.push(failed_item(item, format!("服务器端复制失败：{err}")));
                    }
                }
            }
            CopyMethod::ServerCp => {
                let connection = source_tab.connection.connection();
                match copy_with_cp(
                    connection.as_ref(),
                    target_tab.browse.as_ref(),
                    &item.path,
                    &dest,
                    CP_WAIT,
                    &copy_options,
                )
                .await
                {
                    Ok(_) => {
                        method = Some(CopyMethod::ServerCp);
                        results.push(ok_item(item));
                    }
                    Err(CpFailure::NotAvailable(reason)) => {
                        // 命令没法用：改走中转（这是"用不了"，不是"做失败了"）。
                        tracing::info!(%reason, "服务器上没法用 cp，改经本机中转");
                        match submit_relay(target_tab, &batch, &source_id, item, &dest, same_host) {
                            Ok(id) => {
                                method = Some(CopyMethod::Relay);
                                job_ids.push(id);
                                results.push(queued_item(
                                    item,
                                    Some(format!("{reason}，改经本机中转")),
                                ));
                            }
                            Err(message) => results.push(failed_item(item, message)),
                        }
                    }
                    Err(CpFailure::Failed(message)) => {
                        method = Some(CopyMethod::ServerCp);
                        results.push(failed_relay_item(item, message));
                    }
                    Err(CpFailure::Cancelled) => {
                        method = Some(CopyMethod::ServerCp);
                        results.push(cancelled_item(item));
                    }
                }
            }
            CopyMethod::Relay => unreachable!("中转在上面单独处理"),
        }
    }

    let (key, label) = match method.unwrap_or(CopyMethod::Relay) {
        CopyMethod::CopyData => ("copy_data", CopyMethod::CopyData.label().to_owned()),
        CopyMethod::ServerCp => ("server_cp", CopyMethod::ServerCp.label().to_owned()),
        CopyMethod::Relay => ("relay", CopyMethod::Relay.label().to_owned()),
    };
    Ok(PasteOutcome {
        method: key,
        method_label: label,
        items: results,
        job_ids,
        elapsed_ms: 0,
    })
}

fn submit_relay(
    target_tab: &Arc<SftpTab>,
    batch: &Arc<ConflictBatch>,
    source_id: &str,
    item: &PasteItem,
    dest: &str,
    same_host: bool,
) -> Result<u64, String> {
    let job = TransferJob {
        direction: Direction::RemoteCopy,
        source: item.path.clone(),
        target: dest.to_owned(),
        recursive: true,
        peer: (!same_host).then(|| source_id.to_owned()),
    };
    let id = target_tab.engine.submit_in_batch(job.clone(), batch);
    target_tab.remember_job(id, job);
    Ok(id)
}

async fn resolve_remote_dest(
    tab: &Arc<SftpTab>,
    resolver: &UiConflictResolver,
    item: &PasteItem,
    dest: String,
    same_entry: bool,
    state: &mut ConflictState,
) -> Result<Dest, String> {
    let existing = match tab.browse.lstat(&dest).await {
        Ok(attrs) => attrs,
        Err(_) => return Ok(Dest::Free(dest)),
    };
    let source_attrs = tab.browse.lstat(&item.path).await.ok();
    let suggested_target = suggest_remote_copy_name(&tab.browse, &dest).await;
    let decision = ask(
        resolver,
        state,
        ConflictInfo {
            job: 0,
            direction: Direction::RemoteCopy,
            source: item.path.clone(),
            target: dest.clone(),
            source_size: source_attrs.as_ref().and_then(|attrs| attrs.size),
            target_size: existing.size,
            source_mtime: source_attrs.as_ref().and_then(|attrs| attrs.mtime),
            target_mtime: existing.mtime,
            can_resume: false,
            suggested_target,
            batch: Some(state.batch.id()),
        },
        same_entry,
    )
    .await;
    match decision.action {
        ConflictAction::Skip => Ok(Dest::Skip),
        ConflictAction::Rename => {
            let chosen = match decision.rename_to {
                Some(name) => {
                    if name.contains('/') || name.trim().is_empty() {
                        return Err("新名字不能为空，也不能含有 /".to_owned());
                    }
                    with_file_name(&dest, &name)
                }
                None => {
                    let mut taken = Vec::new();
                    let mut candidate = copy_path(&dest, 1);
                    let mut index = 1;
                    while tab.browse.lstat(&candidate).await.is_ok() {
                        taken.push(candidate);
                        index += 1;
                        candidate = copy_path(&dest, index);
                    }
                    candidate
                }
            };
            if tab.browse.lstat(&chosen).await.is_ok() {
                return Err(format!("「{}」也已经存在", name_of(&chosen)));
            }
            Ok(Dest::Free(chosen))
        }
        ConflictAction::Overwrite | ConflictAction::Resume => {
            if same_entry {
                return Err("不能用它自己覆盖自己（可以选择改名）".to_owned());
            }
            if existing.is_dir() != item.is_dir {
                return Err(if existing.is_dir() {
                    "目标是个文件夹，不能用文件覆盖它".to_owned()
                } else {
                    "目标是个文件，不能用文件夹覆盖它".to_owned()
                });
            }
            if existing.is_dir() {
                Ok(Dest::Merge(dest))
            } else {
                Ok(Dest::Free(dest))
            }
        }
    }
}

/// 问一次冲突（"对后续都这样处理"记在 `state` 里；覆盖自己这种不能记）。
async fn ask(
    resolver: &dyn ConflictResolver,
    state: &mut ConflictState,
    info: ConflictInfo,
    same_entry: bool,
) -> ConflictDecision {
    if let Some(action) = state.batch.action() {
        if !(same_entry && action == ConflictAction::Overwrite) {
            return ConflictDecision {
                action,
                apply_to_all: true,
                rename_to: None,
            };
        }
    }
    let decision = resolver.decide(info).await;
    // "对后续都这样处理"；但「覆盖」不能因为这一项是自己覆盖自己就记下来。
    if decision.apply_to_all && !(same_entry && decision.action == ConflictAction::Overwrite) {
        state.batch.remember(decision.action);
    }
    decision
}

// ---------------------------------------------------------------------------
// 本地
// ---------------------------------------------------------------------------

async fn paste_local(
    tab: &Arc<SftpTab>,
    resolver: &UiConflictResolver,
    request: &PasteRequest,
) -> Result<PasteOutcome, String> {
    let target_dir = PathBuf::from(&request.target_dir);
    if !target_dir.is_dir() {
        return Err(format!("{} 不是文件夹", request.target_dir));
    }
    for item in &request.items {
        if let Some(reason) = refuse_into_self(false, item, &request.target_dir) {
            return Err(reason);
        }
    }
    let batch = tab.engine.new_batch();
    let mut conflicts = ConflictState {
        batch: Arc::clone(&batch),
    };
    let mut results = Vec::new();
    let mut job_ids = Vec::new();

    for item in &request.items {
        let name = name_of(&item.path);
        let dest = target_dir.join(&name);
        let same_entry = pathnorm_key(&dest) == pathnorm_key(Path::new(&item.path));
        // 本地没有"服务器端"，复制全进传输队列：同名文件的冲突由引擎逐个文件问；
        // "粘贴到自己所在的文件夹"在这里先决定名字。
        let precheck = same_entry;
        let dest = if precheck {
            match resolve_local_dest(resolver, item, dest, same_entry, &mut conflicts).await {
                Ok(Some(path)) => path,
                Ok(None) => {
                    results.push(skipped_item(item));
                    continue;
                }
                Err(message) => {
                    results.push(failed_item(item, message));
                    continue;
                }
            }
        } else {
            dest
        };

        let id = submit_local_copy(tab, &batch, item, &dest);
        job_ids.push(id);
        results.push(queued_item(item, None));
    }

    Ok(PasteOutcome {
        method: "local",
        method_label: "本地复制".to_owned(),
        items: results,
        job_ids,
        elapsed_ms: 0,
    })
}

fn submit_local_copy(
    tab: &Arc<SftpTab>,
    batch: &Arc<ConflictBatch>,
    item: &PasteItem,
    dest: &Path,
) -> u64 {
    let job = TransferJob {
        direction: Direction::LocalCopy,
        source: item.path.clone(),
        target: dest.display().to_string(),
        recursive: true,
        peer: None,
    };
    let id = tab.engine.submit_in_batch(job.clone(), batch);
    tab.remember_job(id, job);
    id
}

async fn resolve_local_dest(
    resolver: &UiConflictResolver,
    item: &PasteItem,
    dest: PathBuf,
    same_entry: bool,
    state: &mut ConflictState,
) -> Result<Option<PathBuf>, String> {
    let Ok(existing) = std::fs::symlink_metadata(&dest) else {
        return Ok(Some(dest));
    };
    let source_meta = std::fs::symlink_metadata(&item.path).ok();
    let mtime = |meta: &std::fs::Metadata| {
        meta.modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok())
    };
    let suggested_target = first_free_copy_path(&dest.display().to_string(), |candidate| {
        std::fs::symlink_metadata(candidate).is_ok()
    });
    let decision = ask(
        resolver,
        state,
        ConflictInfo {
            job: 0,
            direction: Direction::LocalCopy,
            source: item.path.clone(),
            target: dest.display().to_string(),
            source_size: source_meta.as_ref().map(|meta| meta.len()),
            target_size: Some(existing.len()),
            source_mtime: source_meta.as_ref().and_then(mtime),
            target_mtime: mtime(&existing),
            can_resume: false,
            suggested_target,
            batch: Some(state.batch.id()),
        },
        same_entry,
    )
    .await;
    match decision.action {
        ConflictAction::Skip => Ok(None),
        ConflictAction::Rename => {
            let chosen = match decision.rename_to {
                Some(name) => {
                    // 必须原样是合法的 Windows 文件名：`C:evil.txt` 会让 `with_file_name` 丢掉所在
                    // 目录，`a:b` 会写进备用数据流（R-10 M1）。拼好之后再核对一次仍在原目录里。
                    let name = zssh_core::sftp::local::check_child_name(&name)
                        .map_err(|err| err.to_string())?;
                    let chosen = dest.with_file_name(&name);
                    if let Some(parent) = dest.parent() {
                        zssh_core::sftp::local::ensure_inside(parent, &chosen)
                            .map_err(|err| err.to_string())?;
                    }
                    chosen
                }
                None => {
                    let path = first_free_copy_path(&dest.display().to_string(), |candidate| {
                        std::fs::symlink_metadata(candidate).is_ok()
                    });
                    PathBuf::from(path)
                }
            };
            if std::fs::symlink_metadata(&chosen).is_ok() {
                return Err(format!(
                    "「{}」也已经存在",
                    name_of(&chosen.display().to_string())
                ));
            }
            Ok(Some(chosen))
        }
        ConflictAction::Overwrite | ConflictAction::Resume => {
            if same_entry {
                return Err("不能用它自己覆盖自己（可以选择改名）".to_owned());
            }
            if existing.is_dir() != item.is_dir {
                return Err("目标和源一个是文件一个是文件夹，不能覆盖".to_owned());
            }
            Ok(Some(dest))
        }
    }
}

#[cfg(test)]
mod tests;
