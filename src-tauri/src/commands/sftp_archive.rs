//! 远端压缩 / 解压的命令接线（UI-13）：真正的逻辑在 `zssh_core::sftp::archive`。
//!
//! 取消、"这次操作还在跑"的登记沿用服务器端复制的那一套（`SftpRegistry::register_paste` /
//! `sftp_paste_cancel`）：界面用同一个「服务器上处理中 · 取消」提示条。
//!
//! 解压分两步，中间留给界面弹冲突对话框：
//! 1. `sftp_extract_plan`：列条目、拒绝指向目录外的路径、数出目标里已有的同名文件；
//! 2. `sftp_extract`：按用户选的「覆盖 / 跳过已有文件」解压（界面把第 1 步的结果原样带回来，
//!    后端用其中的压缩包指纹确认它还是检查过的那一个）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use zssh_core::sftp::archive::{
    compress, extract, plan_extract, try_probe_caps, ArchiveCaps, ArchiveError, ArchiveFingerprint,
    ArchiveFormat, ArchiveProgress, CompressFormat, CompressRequest, ConflictPolicy, ExtractPlan,
    ExtractRequest,
};
use zssh_core::sftp::path as remote_path;

use crate::commands::sftp::{tab_of, SftpRegistry, SftpTab};

/// 压缩进度（临时文件已写多少字节）：载荷 `(sftp_id, op_id, bytes)`。
pub const SFTP_ARCHIVE_PROGRESS_EVENT: &str = "sftp_archive_progress";

/// 这台主机的压缩能力：第一次问时探测，之后用缓存。探测命令没跑成（拒绝 exec、超时、连接出错）
/// 报错且不缓存，界面下次右键再试；跑了但认不出（Windows）是确定的「都不支持」，会缓存。
async fn archive_caps_of(tab: &SftpTab) -> Result<ArchiveCaps, String> {
    let mut slot = tab.archive_caps.lock().await;
    if let Some(caps) = *slot {
        return Ok(caps);
    }
    match try_probe_caps(tab.connection.connection()).await {
        Some(caps) => {
            *slot = Some(caps);
            Ok(caps)
        }
        None => Err("没能在服务器上检测压缩工具（命令没有执行成功）".to_owned()),
    }
}

/// 界面右键菜单用：这台主机能不能压缩 / 解压，缺哪个命令。
#[tauri::command]
pub async fn sftp_archive_caps(sftp_id: String, app: AppHandle) -> Result<ArchiveCaps, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    archive_caps_of(&tab).await
}

/// 临时文件名里的任务标识：一次操作一个，不会和别的任务撞。
fn make_owner() -> String {
    format!(
        "a{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    )
}

/// 能不能用 `format` 压缩 / 解压：不是 POSIX shell 或缺命令就给出要告诉用户的原因。
fn require_tools(caps: &ArchiveCaps, tar: bool, zip: bool, unzip: bool) -> Result<(), String> {
    if !caps.posix {
        return Err("这台服务器的登录 shell 不是 POSIX，不支持远端压缩 / 解压".to_owned());
    }
    if tar && !caps.tar {
        return Err("服务器上没有 tar 命令".to_owned());
    }
    if zip && !caps.zip {
        return Err("服务器上没有 zip 命令".to_owned());
    }
    if unzip && !caps.unzip {
        return Err("服务器上没有 unzip 命令".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 压缩
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CompressCommand {
    /// 这次操作的任务号（界面用它取消；沿用粘贴的登记）。
    pub op_id: String,
    /// 当前目录（绝对路径）。
    pub dir: String,
    /// 选中的条目名（当前目录下的名字）。
    pub names: Vec<String>,
    pub format: CompressFormat,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompressResult {
    /// ok / cancelled
    pub state: &'static str,
    pub name: Option<String>,
    pub path: Option<String>,
    pub warning: Option<String>,
}

#[tauri::command]
pub async fn sftp_compress(
    sftp_id: String,
    request: CompressCommand,
    app: AppHandle,
) -> Result<CompressResult, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let caps = archive_caps_of(&tab).await?;
    require_tools(
        &caps,
        request.format == CompressFormat::TarGz,
        request.format == CompressFormat::Zip,
        false,
    )?;
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    let cancel = registry.register_paste(&sftp_id, &request.op_id)?;
    let owner = make_owner();
    let on_progress = |progress: ArchiveProgress| {
        let _ = app.emit(
            SFTP_ARCHIVE_PROGRESS_EVENT,
            (sftp_id.clone(), request.op_id.clone(), progress.bytes),
        );
    };
    let connection = tab.connection.connection();
    let result = compress(
        connection.as_ref(),
        tab.browse.as_ref(),
        &CompressRequest {
            dir: &request.dir,
            names: &request.names,
            format: request.format,
            owner: &owner,
            server: tab.browse.server_key(),
        },
        cancel.atomic(),
        Some(&on_progress),
    )
    .await;
    registry.finish_paste(&sftp_id, &request.op_id);
    match result {
        Ok(outcome) => Ok(CompressResult {
            state: "ok",
            name: Some(outcome.name),
            path: Some(outcome.path),
            warning: outcome.warning,
        }),
        Err(ArchiveError::Cancelled) => Ok(CompressResult {
            state: "cancelled",
            name: None,
            path: None,
            warning: None,
        }),
        Err(err) => Err(err.to_string()),
    }
}

// ---------------------------------------------------------------------------
// 解压
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractPlanCommand {
    pub op_id: String,
    /// 压缩包的完整路径。
    pub archive: String,
    /// 要解压到的文件夹（可以还不存在）。
    pub dest: String,
}

/// 检查压缩包。用户取消返回 `None`。
#[tauri::command]
pub async fn sftp_extract_plan(
    sftp_id: String,
    request: ExtractPlanCommand,
    app: AppHandle,
) -> Result<Option<ExtractPlan>, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let caps = archive_caps_of(&tab).await?;
    let format = ArchiveFormat::detect(&remote_path::file_name(&request.archive));
    require_tools(
        &caps,
        format.is_some_and(|format| !format.is_zip()),
        false,
        format.is_some_and(ArchiveFormat::is_zip),
    )?;
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    let cancel = registry.register_paste(&sftp_id, &request.op_id)?;
    let connection = tab.connection.connection();
    let result = plan_extract(
        connection.as_ref(),
        tab.browse.as_ref(),
        &request.archive,
        &request.dest,
        cancel.atomic(),
    )
    .await;
    registry.finish_paste(&sftp_id, &request.op_id);
    match result {
        Ok(plan) => Ok(Some(plan)),
        Err(ArchiveError::Cancelled) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// 界面把检查结果里解压时要核对的部分原样带回来。
#[derive(Debug, Clone, Deserialize)]
pub struct CheckedPlan {
    pub fingerprint: ArchiveFingerprint,
    pub dest_exists: bool,
    pub total_files: u64,
    pub conflict_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractCommand {
    pub op_id: String,
    pub archive: String,
    pub dest: String,
    pub policy: ConflictPolicy,
    pub plan: CheckedPlan,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractResult {
    /// ok / cancelled
    pub state: &'static str,
    pub extracted: u64,
    pub skipped: u64,
    pub warning: Option<String>,
}

#[tauri::command]
pub async fn sftp_extract(
    sftp_id: String,
    request: ExtractCommand,
    app: AppHandle,
) -> Result<ExtractResult, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let caps = archive_caps_of(&tab).await?;
    let format = ArchiveFormat::detect(&remote_path::file_name(&request.archive));
    require_tools(
        &caps,
        format.is_some_and(|format| !format.is_zip()),
        false,
        format.is_some_and(ArchiveFormat::is_zip),
    )?;
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    let cancel = registry.register_paste(&sftp_id, &request.op_id)?;
    let owner = make_owner();
    let connection = tab.connection.connection();
    let result = extract(
        connection.as_ref(),
        tab.browse.as_ref(),
        &ExtractRequest {
            archive: &request.archive,
            dest: &request.dest,
            policy: request.policy,
            plan_fingerprint: request.plan.fingerprint,
            plan_dest_exists: request.plan.dest_exists,
            plan_total_files: request.plan.total_files,
            plan_conflict_count: request.plan.conflict_count,
            owner: &owner,
            server: tab.browse.server_key(),
        },
        cancel.atomic(),
    )
    .await;
    registry.finish_paste(&sftp_id, &request.op_id);
    match result {
        Ok(outcome) => Ok(ExtractResult {
            state: "ok",
            extracted: outcome.extracted,
            skipped: outcome.skipped,
            warning: outcome.warning,
        }),
        Err(ArchiveError::Cancelled) => Ok(ExtractResult {
            state: "cancelled",
            extracted: 0,
            skipped: 0,
            warning: None,
        }),
        Err(err) => Err(err.to_string()),
    }
}
