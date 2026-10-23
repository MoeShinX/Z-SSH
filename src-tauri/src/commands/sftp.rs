//! SFTP 的命令接线（M3-2 第 1 节）：把 `zssh_core::sftp` 的能力包成 Tauri 命令和事件。
//!
//! 结构（设计文档 M3 第 2、3 节）：
//! - **标签 = 一次 SFTP 会话**：`sftp_open` 在主机已认证的连接上开浏览通道，
//!   返回 `sftp_id`；`sftp_close` 关掉通道和这个标签发起的全部传输（按第 3 节的
//!   规则，界面先确认过还有没有没传完的）。
//! - **浏览 / 文件操作**走浏览通道（一条就够：`RawSftpSession` 把请求按号归位）。
//! - **传输**走 `TransferEngine`，通道由 `SftpChannelProvider` 出：这条连接开不出
//!   通道（`MaxSessions` 满了）时改走**一条专用连接**（`SavedCredentialReconnect`：
//!   复用主机库里保存的凭据和已确认的指纹，需要用户输入时走正常弹窗）。
//! - **冲突**：后端把 [`ConflictInfo`] 发事件给前端 → 前端弹对话框 → 用户选择 →
//!   `sftp_conflict_answer` 把决定送回后端。**没选 / 超时 / 关掉对话框都按"跳过"**，
//!   任务不会一直挂着。
//! - **进度 / 删除进度**走事件（后端节流过，这里只做转发）。
//!
//! 本地那一侧（左栏）不连服务器：本地列目录 / 建目录 / 改名 / 删除直接 `std::fs`
//! （长路径过 `long_path`，删除前的确认由界面做）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use zssh_core::sftp::fs::DirEntry;
use zssh_core::sftp::session::SftpChannel;
use zssh_core::sftp::transfer::backend::{
    SftpChannelProvider, SftpEndpoints, SftpReconnect, TransferChannelProvider,
};
use zssh_core::sftp::transfer::{
    ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction, ProgressReport,
    TransferEngine, TransferEvent, TransferJob, TransferOptions, TransferState,
};
use zssh_core::sftp::{FileKind, RecursiveDelete, SftpError};
use zssh_core::ssh::{BoxFuture, Connection};
use zssh_core::Database;
use zssh_core::ZsshError;

use crate::commands::session::{build_chain, pooled_connection};
use crate::interact::UiInteractor;
use crate::state::{AppState, CancelSignal, SessionConnection};

/// 发给前端的四个事件名。
pub const SFTP_STATE_EVENT: &str = "sftp_state";
pub const SFTP_TRANSFER_EVENT: &str = "sftp_transfer_event";
pub const SFTP_CONFLICT_EVENT: &str = "sftp_conflict_prompt";
/// 一条冲突提问结束了（回答 / 超时 / 取消）：载荷是 `conflict_id`，各窗口据此撤掉队列里的那条。
pub const SFTP_CONFLICT_DONE_EVENT: &str = "sftp_conflict_done";
pub const SFTP_DELETE_EVENT: &str = "sftp_delete_event";

/// 等用户处理冲突的最长时间：超了按"跳过"（任务不能一直挂着）。
pub const CONFLICT_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// DTO（发给前端的数据形状）
// ---------------------------------------------------------------------------

/// 一条目录项（两栏通用；远端多 owner / permissions）。
#[derive(Debug, Clone, Serialize)]
pub struct EntryDto {
    pub name: String,
    pub path: String,
    /// dir / file / symlink / other / drive（盘符是本地"此电脑"里的）。
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 修改时间（Unix 秒）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// 名字不是合法 UTF-8（界面上标灰、禁操作；见设计文档"已知限制"）。
    pub name_is_utf8: bool,
    /// Windows 的"隐藏"属性（远端是名字以点开头）。
    pub hidden: bool,
    /// 符号链接指向的类型（dir / file；断链和拿不到是 None）。
    /// 界面靠它决定"双击链接"是进目录还是不动。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<&'static str>,
}

impl EntryDto {
    fn from_remote(entry: DirEntry) -> Self {
        let kind = match &entry.kind {
            FileKind::Dir => "dir",
            FileKind::File => "file",
            FileKind::Symlink { .. } => "symlink",
            FileKind::Other(_) => "other",
        };
        Self {
            name: entry.name,
            path: entry.path,
            kind,
            link_target: entry.link_target,
            size: entry.size,
            mtime: entry.mtime.map(u64::from),
            permissions: entry.permissions,
            owner: entry.owner,
            name_is_utf8: entry.name_is_utf8,
            hidden: false,
            target_kind: match &entry.kind {
                FileKind::Symlink { target } => match target.as_deref() {
                    Some(FileKind::Dir) => Some("dir"),
                    Some(FileKind::File) => Some("file"),
                    _ => None,
                },
                _ => None,
            },
        }
    }

    /// 隐藏文件（远端按"名字以点开头"判断；界面的开关管要不要显示）。
    fn hidden_remote(mut self) -> Self {
        self.hidden = self.name.starts_with('.');
        self
    }
}

/// 一个传输任务在队列里的样子（事件和 `sftp_transfers` 共用）。
#[derive(Debug, Clone, Serialize)]
pub struct TransferDto {
    pub id: u64,
    /// upload / download
    pub direction: &'static str,
    /// queued / planning / running / paused / waiting_reconnect / waiting_conflict / done / failed / cancelled / skipped
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub source: String,
    pub target: String,
    pub total_files: u64,
    pub done_files: u64,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub speed_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub notes: Vec<String>,
}

impl TransferDto {
    fn of(report: &ProgressReport, job: &TransferJob) -> Self {
        let (state, reason) = state_str(&report.state);
        Self {
            id: report.id,
            direction: match report.direction {
                Direction::Upload => "upload",
                Direction::Download => "download",
                Direction::RemoteCopy => "remote_copy",
                Direction::LocalCopy => "local_copy",
            },
            state,
            reason,
            source: display_path(&job.source),
            target: display_path(&job.target),
            total_files: report.total_files,
            done_files: report.done_files,
            total_bytes: report.total_bytes,
            done_bytes: report.done_bytes,
            speed_bps: report.speed_bps,
            eta_secs: report.eta_secs,
            current: report.current.clone(),
            notes: report.notes.clone(),
        }
    }
}

fn state_str(state: &TransferState) -> (&'static str, Option<String>) {
    match state {
        TransferState::Queued => ("queued", None),
        TransferState::Planning => ("planning", None),
        TransferState::Running => ("running", None),
        TransferState::Paused => ("paused", None),
        TransferState::WaitingReconnect => ("waiting_reconnect", None),
        TransferState::WaitingConflict => ("waiting_conflict", None),
        TransferState::Done => ("done", None),
        TransferState::Failed(reason) => ("failed", Some(reason.clone())),
        TransferState::Cancelled => ("cancelled", None),
        TransferState::Skipped => ("skipped", None),
    }
}

/// 冲突提问（前端弹对话框）。
#[derive(Debug, Clone, Serialize)]
pub struct ConflictPromptDto {
    pub conflict_id: u64,
    pub job: u64,
    /// upload / download
    pub direction: &'static str,
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_mtime: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_mtime: Option<u64>,
    /// 目标比源小时才能续传。
    pub can_resume: bool,
    /// 冲突里建议的新名字（`name (2).ext`），用户可以改。
    pub suggested_name: String,
    /// 属于哪个批次（同一次提交 / 粘贴）。界面勾了"全部应用"之后，用它把同批次还在排队的
    /// 提问一并回答掉。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<u64>,
}

/// 删除进度事件。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeleteEventDto {
    /// 进度：又删掉一项。
    Progress {
        delete_id: u64,
        deleted: u64,
        current: String,
    },
    /// 结束：删完了 / 出错了 / 被取消了。
    Done {
        delete_id: u64,
        deleted: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        cancelled: bool,
    },
}

// ---------------------------------------------------------------------------
// 状态：标签表、冲突登记、删除句柄
// ---------------------------------------------------------------------------

/// 一个 SFTP 标签的后端状态。
pub struct SftpTab {
    /// 这台主机的 ID（M3-3 终端侧边栏跟随时还要用；重连兜底读凭据用）。
    #[allow(dead_code)]
    pub host_id: i64,
    /// 池里那条连接的引用：SftpTab 活着，这条连接就不会被池子收走。
    #[allow(dead_code)]
    pub connection: SessionConnection,
    pub browse: Arc<SftpChannel>,
    pub cancel: Arc<CancelSignal>,
    pub engine: Arc<TransferEngine>,
    /// 引擎用的数据面（复制粘贴的源在别的主机时，往里登记那台主机的通道提供者）。
    pub endpoints: Arc<SftpEndpoints>,
    /// 这个标签的传输通道提供者（别的标签粘贴到别处时，拿它读这里的文件）。
    pub provider: Arc<SftpChannelProvider>,
    /// 这个标签里还在跑的"计算大小"（属性对话框）：任务号 → 取消标志。
    pub(crate) sizes: Mutex<HashMap<u64, Arc<std::sync::atomic::AtomicBool>>>,
    /// 任务号 → 任务（事件转发时补 source / target）。
    jobs: Mutex<HashMap<u64, TransferJob>>,
    /// 远端登录 shell 的类型（第一次要决定能不能用 cp 时探测一次，之后缓存）。
    pub(crate) shell_kind: tokio::sync::Mutex<Option<zssh_core::ai::shell_kind::ShellKind>>,
    /// 远端压缩 / 解压能做什么（第一次右键时探测一次，之后缓存；探测没跑成不缓存）。
    pub(crate) archive_caps: tokio::sync::Mutex<Option<zssh_core::sftp::archive::ArchiveCaps>>,
    /// 这个标签发起的、还在跑的删除（sftp_delete_cancel 用）。
    deletes: Mutex<HashMap<u64, Arc<CancelSignal>>>,
}

impl SftpTab {
    fn job_of(&self, id: u64) -> Option<TransferJob> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }
}

/// 应用级的 SFTP 状态：标签表 + 冲突登记。
#[derive(Default)]
pub struct SftpRegistry {
    tabs: Mutex<HashMap<String, Arc<SftpTab>>>,
    next_id: AtomicU64,
    conflicts: Mutex<HashMap<u64, PendingConflict>>,
    next_conflict: AtomicU64,
    next_delete: AtomicU64,
    /// 还在跑的粘贴（服务器端复制可能很久，界面要能取消）。键是 (sftp_id, paste_id)。
    pastes: Mutex<HashMap<(String, String), Arc<CancelSignal>>>,
}

/// 一条挂着的冲突提问。
struct PendingConflict {
    /// 等用户回答的通道：Err（超时 / 被丢掉）都按"跳过"。
    sender: oneshot::Sender<ConflictDecision>,
    /// 它属于哪个标签：标签关掉时取消它的提问。
    sftp_id: String,
}

impl SftpRegistry {
    fn insert(&self, sftp_id: String, tab: SftpTab) {
        self.tabs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(sftp_id, Arc::new(tab));
    }

    fn take(&self, sftp_id: &str) -> Option<Arc<SftpTab>> {
        self.tabs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(sftp_id)
    }

    fn get(&self, sftp_id: &str) -> Option<Arc<SftpTab>> {
        self.tabs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(sftp_id)
            .cloned()
    }

    /// 当前打开的所有标签：`(sftp_id, host_id, 服务器身份)`。
    ///
    /// 编辑器的冲突 / 属主处理要在这份快照里找"这台主机现在可用的会话"（R-8 第 6 节），
    /// 不能再用打开编辑器时记下的旧 sftp_id（标签可能已经关了或者重连过）。
    pub(crate) fn tab_candidates(&self) -> Vec<(String, i64, String)> {
        let tabs = self
            .tabs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut list: Vec<(String, i64, String)> = tabs
            .iter()
            .map(|(sftp_id, tab)| {
                (
                    sftp_id.clone(),
                    tab.host_id,
                    tab.browse.server_key().to_owned(),
                )
            })
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    /// 登记一条冲突提问，返回 (id, 接收端)。
    fn add_conflict(&self, sftp_id: &str) -> (u64, oneshot::Receiver<ConflictDecision>) {
        let (sender, receiver) = oneshot::channel();
        let id = self.next_conflict.fetch_add(1, Ordering::SeqCst) + 1;
        self.conflicts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                id,
                PendingConflict {
                    sender,
                    sftp_id: sftp_id.to_owned(),
                },
            );
        (id, receiver)
    }

    /// 把一条还挂着的提问从登记表里拿掉（不发回答）；已经没有就什么都不做。
    fn forget_conflict(&self, conflict_id: u64) {
        self.conflicts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&conflict_id);
    }

    /// 前端的回答送回后端。
    fn answer_conflict(&self, conflict_id: u64, decision: ConflictDecision) -> Result<(), String> {
        let pending = self
            .conflicts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&conflict_id);
        match pending {
            Some(pending) => pending
                .sender
                .send(decision)
                .map_err(|_| "这次冲突提问已经结束了".to_owned()),
            None => Err(format!("没有这个冲突提问：{conflict_id}")),
        }
    }

    /// 登记一次粘贴，返回它的取消信号。重复的 (标签, 粘贴号) 说明前端发了两次，直接拒绝。
    pub(crate) fn register_paste(
        &self,
        sftp_id: &str,
        paste_id: &str,
    ) -> Result<Arc<CancelSignal>, String> {
        if paste_id.trim().is_empty() {
            return Err("粘贴任务号不能为空".to_owned());
        }
        let key = (sftp_id.to_owned(), paste_id.to_owned());
        let cancel = Arc::new(CancelSignal::default());
        let mut pastes = self
            .pastes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pastes.insert(key, Arc::clone(&cancel)).is_some() {
            return Err(format!("粘贴任务号重复：{paste_id}"));
        }
        Ok(cancel)
    }

    /// 粘贴结束（成功、失败、取消都算），把登记去掉。
    pub(crate) fn finish_paste(&self, sftp_id: &str, paste_id: &str) {
        self.pastes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(sftp_id.to_owned(), paste_id.to_owned()));
    }

    /// 取消一次粘贴；已经结束的返回 false。
    pub(crate) fn cancel_paste(&self, sftp_id: &str, paste_id: &str) -> bool {
        let cancel = self
            .pastes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(sftp_id.to_owned(), paste_id.to_owned()))
            .cloned();
        match cancel {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// 标签关闭时取消它挂着的全部粘贴。
    pub(crate) fn cancel_pastes_of(&self, sftp_id: &str) {
        let cancels: Vec<Arc<CancelSignal>> = self
            .pastes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|((id, _), _)| id == sftp_id)
            .map(|(_, cancel)| Arc::clone(cancel))
            .collect();
        for cancel in cancels {
            cancel.cancel();
        }
    }

    /// 取消一个标签挂着的全部冲突提问（发送端被丢掉，等待方按"跳过"收场）。
    fn cancel_conflicts_of(&self, sftp_id: &str) -> usize {
        let mut conflicts = self
            .conflicts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mine: Vec<u64> = conflicts
            .iter()
            .filter(|(_, pending)| pending.sftp_id == sftp_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &mine {
            conflicts.remove(id);
        }
        mine.len()
    }
}

// ---------------------------------------------------------------------------
// 专用连接的重连（M3-2 第 1 节：SftpReconnect 的正式实现）
// ---------------------------------------------------------------------------

/// 传输专用连接的重连：复用主机库里保存的凭据和已经确认过的主机指纹。
///
/// - 密码 / 口令存过、指纹没变：**不问用户**，直接连上（`KnownHosts` 里已有这台主机
///   的记录，`confirm_host_key` 根本不会被调）；
/// - 需要用户输入（密码没保存、keyboard-interactive、指纹变了）：走正常弹窗
///   （`UiInteractor` + 事件），用户取消任务就失败，错误里说明原因。
struct SavedCredentialReconnect {
    app: AppHandle,
    host_id: i64,
    /// 提问挂在哪个标签名下（关闭标签时取消提问）。
    sftp_id: String,
    /// 重连过程的取消信号：和标签共用一个。
    cancel: Arc<CancelSignal>,
}

impl SftpReconnect for SavedCredentialReconnect {
    fn reconnect<'a>(&'a self) -> BoxFuture<'a, Result<Arc<Connection>, SftpError>> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let chain = build_chain(&state, self.host_id).map_err(|err| {
                SftpError::Remote(format!("传输专用连接没有建起来（读主机配置失败）：{err}"))
            })?;
            let interactor = Arc::new(UiInteractor::new(
                self.app.clone(),
                Arc::clone(&state.prompts),
                self.sftp_id.clone(),
                Arc::clone(&self.cancel),
            ));
            // **新建**一条连接，不走连接池：池对同一台主机会返回同一条连接，
            // 而要兜底的正是"这条连接的通道满了"（MaxSessions），再拿它等于没兜底。
            // 凭据是主机库里保存的，指纹是已经确认过的（known_hosts 里有）；需要
            // 用户输入（密码没保存 / keyboard-interactive / 指纹变了）时 interactor
            // 走正常弹窗。
            let connected = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(SftpError::Cancelled),
                result = Connection::connect_chain(chain.hops, interactor) => result,
            };
            match connected {
                Ok(connection) => Ok(Arc::new(connection)),
                // 用户在弹窗里取消 / 拒绝指纹：任务失败并说明原因，不重试（重试只会
                // 再弹一遍）。
                Err(
                    err @ (ZsshError::Cancelled
                    | ZsshError::HostKeyRejected { .. }
                    | ZsshError::AuthTimeout { .. }),
                ) => Err(SftpError::Remote(format!(
                    "传输专用连接没有建起来（登录或指纹确认被取消）：{err}"
                ))),
                Err(err) => Err(SftpError::Disconnected(format!(
                    "传输专用连接建立失败：{err}"
                ))),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// 冲突处理：事件 → 对话框 → 命令（M3-2 第 1 节）
// ---------------------------------------------------------------------------

/// 把引擎的冲突回调接到界面上。
///
/// 没回答 / 超时 / 前端关掉对话框：都按**跳过**（`apply_to_all = false`），
/// 同一个任务后面的文件还会再问。
pub(crate) struct UiConflictResolver {
    app: AppHandle,
    registry: Arc<SftpRegistry>,
    sftp_id: String,
    timeout: Duration,
}

impl UiConflictResolver {
    pub(crate) fn new(app: AppHandle, registry: Arc<SftpRegistry>, sftp_id: String) -> Self {
        Self {
            app,
            registry,
            sftp_id,
            timeout: CONFLICT_TIMEOUT,
        }
    }
}

impl ConflictResolver for UiConflictResolver {
    fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        Box::pin(async move {
            // 标签已经关了：别弹窗（前端收不到事件就永远没人回答）。
            if self
                .registry
                .get(&self.sftp_id)
                .map(|tab| tab.cancel.is_cancelled())
                .unwrap_or(true)
            {
                return skip_decision();
            }
            ask_user(&self.app, &self.registry, &self.sftp_id, self.timeout, info).await
        })
    }
}

/// 一条提问结束时（被回答、超时、标签关闭取消、等待方被丢掉）告诉所有窗口撤掉它。
///
/// 提问事件是广播给所有窗口的，每个窗口都把它收进自己的队列；只有回答的那个窗口会自己删，
/// 别的窗口里那条要等 2 分钟超时才清，这期间把会话迁过去（拆出停靠面板、合并回主窗口）就会把
/// 已经答过的提问再弹一遍。放在 `Drop` 里是为了不管等待怎么结束都发，包括 future 被直接丢掉。
struct ConflictDone<'a, R: Runtime> {
    app: &'a AppHandle<R>,
    registry: &'a SftpRegistry,
    conflict_id: u64,
}

impl<R: Runtime> Drop for ConflictDone<'_, R> {
    fn drop(&mut self) {
        // 超时 / 被丢掉时登记表里还留着这条，一并清掉（迟到的回答会得到「没有这个冲突提问」）。
        self.registry.forget_conflict(self.conflict_id);
        if let Err(err) = self.app.emit(SFTP_CONFLICT_DONE_EVENT, self.conflict_id) {
            warn!(%err, conflict_id = self.conflict_id, "发冲突提问结束事件失败");
        }
    }
}

/// 发一条冲突提问并等回答（`UiConflictResolver::decide` 的主体；拆出来是为了能用模拟运行时测）。
async fn ask_user<R: Runtime>(
    app: &AppHandle<R>,
    registry: &SftpRegistry,
    sftp_id: &str,
    timeout: Duration,
    info: ConflictInfo,
) -> ConflictDecision {
    let (conflict_id, receiver) = registry.add_conflict(sftp_id);
    let _done = ConflictDone {
        app,
        registry,
        conflict_id,
    };
    // 建议名由引擎给：已经跳过了被占用的名字（上传 / 下载从 `名字 (2)` 起，
    // 复制粘贴从「名字 - 副本」起）。
    let suggested = info.suggested_target.clone();
    let prompt = ConflictPromptDto {
        conflict_id,
        job: info.job,
        direction: match info.direction {
            Direction::Upload => "upload",
            Direction::Download => "download",
            Direction::RemoteCopy => "remote_copy",
            Direction::LocalCopy => "local_copy",
        },
        source: info.source.clone(),
        target: info.target.clone(),
        source_size: info.source_size,
        target_size: info.target_size,
        source_mtime: info.source_mtime.map(u64::from),
        target_mtime: info.target_mtime.map(u64::from),
        can_resume: info.can_resume,
        suggested_name: file_name_of(&suggested).to_owned(),
        batch: info.batch,
    };
    if let Err(err) = app.emit(SFTP_CONFLICT_EVENT, (sftp_id.to_owned(), prompt)) {
        warn!(%err, "发冲突提问事件失败");
        return skip_decision();
    }
    // 超时 / sender 被丢掉（标签关闭）：跳过，任务不能挂着。
    match tokio::time::timeout(timeout, receiver).await {
        // 对话框预填的就是建议名：用户没改它就不算手动名字，让引擎走自动起名（被占就往后找）。
        Ok(Ok(decision)) => decision.without_suggested_name(&suggested),
        Ok(Err(_)) => {
            debug!(conflict_id, "冲突提问被取消（标签关闭），按跳过");
            skip_decision()
        }
        Err(_) => {
            info!(conflict_id, "等用户处理冲突超时，按跳过");
            skip_decision()
        }
    }
}

fn skip_decision() -> ConflictDecision {
    ConflictDecision {
        action: ConflictAction::Skip,
        apply_to_all: false,
        rename_to: None,
    }
}

/// 冲突对话框的动作名 → 引擎的动作。
fn parse_conflict_action(action: &str) -> Option<ConflictAction> {
    match action {
        "overwrite" => Some(ConflictAction::Overwrite),
        "skip" => Some(ConflictAction::Skip),
        "rename" => Some(ConflictAction::Rename),
        "resume" => Some(ConflictAction::Resume),
        _ => None,
    }
}

/// 界面显示用的路径：剥掉 `\\?\\` / `\\?\\UNC\\` 长路径前缀（本地端点内部用
/// 长路径打开文件，但前缀不该漏到界面上）。
fn display_path(path: &str) -> String {
    const LONG_UNC: &str = r"\\?\UNC\";
    const LONG_LOCAL: &str = r"\\?\";
    if let Some(rest) = path.strip_prefix(LONG_UNC) {
        return format!(r"\\{rest}");
    }
    path.strip_prefix(LONG_LOCAL).unwrap_or(path).to_owned()
}

fn file_name_of(path: &str) -> &str {
    // 先按两种斜杠都切开（Windows 路径和远端路径都吃得下）。
    let trimmed = path.trim_end_matches(['/', '\\']);
    let cut = trimmed
        .rfind(['/', '\\'])
        .map(|index| index + 1)
        .unwrap_or(0);
    &trimmed[cut..]
}

// ---------------------------------------------------------------------------
// 打开 / 关闭
// ---------------------------------------------------------------------------

/// 打开一个 SFTP 标签：连主机（池里复用）→ 开浏览通道 → 起传输引擎。
///
/// **立即返回** `sftp_id`；连接在后台进行，状态走 `sftp_state` 事件（和终端会话
/// 同一套节奏）。需要用户输入（密码 / 指纹）时走正常弹窗。
#[tauri::command]
pub async fn sftp_open(
    app: AppHandle,
    state: State<'_, AppState>,
    host_id: i64,
) -> Result<String, String> {
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    let sftp_id = format!(
        "sftp-{}",
        registry.next_id.fetch_add(1, Ordering::SeqCst) + 1
    );
    emit_sftp_state(&app, &sftp_id, "connecting", None);

    let connection = match pooled_connection(&app, &sftp_id, host_id).await {
        Ok((connection, cancel)) => (connection, cancel),
        Err(err) => {
            emit_sftp_state(&app, &sftp_id, "failed", Some(err.clone()));
            return Err(format!("连接失败：{err}"));
        }
    };
    let (connection, cancel) = connection;

    // 浏览通道：开在已有连接上。满了（MaxSessions）就直接走专用连接——
    // 浏览也是"传输"的一部分，不能因为终端占满就全废。
    let raw = Arc::clone(connection.connection());
    let reconnect: Arc<dyn SftpReconnect> = Arc::new(SavedCredentialReconnect {
        app: app.clone(),
        host_id,
        sftp_id: sftp_id.clone(),
        cancel: Arc::clone(&cancel),
    });
    let provider = Arc::new(SftpChannelProvider::new(
        Arc::clone(&raw),
        Some(Arc::clone(&reconnect)),
    ));
    let browse = match provider.channel().await {
        Ok(channel) => channel,
        Err(err) => {
            let err = err.to_string();
            emit_sftp_state(&app, &sftp_id, "failed", Some(err.clone()));
            return Err(format!("开 SFTP 通道失败：{err}"));
        }
    };

    // 传输引擎：设置页的选项在这里生效。
    let options = transfer_options(&state.store);
    let resolver = Arc::new(UiConflictResolver::new(
        app.clone(),
        Arc::clone(&registry),
        sftp_id.clone(),
    ));
    let endpoints = Arc::new(SftpEndpoints::new(provider.clone()));
    let (engine, events) = TransferEngine::new(endpoints.clone(), resolver, options);
    let tab = SftpTab {
        host_id,
        connection,
        browse,
        cancel,
        engine: Arc::clone(&engine),
        endpoints,
        provider,
        sizes: Mutex::new(HashMap::new()),
        jobs: Mutex::new(HashMap::new()),
        shell_kind: tokio::sync::Mutex::new(None),
        archive_caps: tokio::sync::Mutex::new(None),
        deletes: Mutex::new(HashMap::new()),
    };
    registry.insert(sftp_id.clone(), tab);
    emit_sftp_state(&app, &sftp_id, "ready", None);
    // 事件转发任务：标签被拿走后自动结束（接收端 drop 会关掉通道）。
    let forward_app = app.clone();
    let forward_id = sftp_id.clone();
    tauri::async_runtime::spawn(async move {
        forward_events(forward_app, forward_id, events).await;
    });
    Ok(sftp_id)
}

/// 事件转发：引擎的报告 → `sftp_transfer_event`（带上任务的 source / target）。
async fn forward_events(
    app: AppHandle,
    sftp_id: String,
    mut events: tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
) {
    let registry = Arc::clone(&app.state::<Arc<SftpRegistry>>());
    while let Some(event) = events.recv().await {
        let report = match event {
            TransferEvent::Progress(report) => report,
            TransferEvent::Finished(report) => report,
        };
        let Some(tab) = registry.get(&sftp_id) else {
            return; // 标签没了：转发也就停了
        };
        let Some(job) = tab.job_of(report.id) else {
            continue;
        };
        let _ = app.emit(
            SFTP_TRANSFER_EVENT,
            (sftp_id.clone(), TransferDto::of(&report, &job)),
        );
    }
}

/// 关闭一个 SFTP 标签：取消挂着的冲突提问、取消传输（界面先确认过）、关通道。
///
/// 引擎被丢弃后后台任务自然结束（它们拿的是 Weak）。
#[tauri::command]
pub async fn sftp_close(app: AppHandle, sftp_id: String) -> Result<(), String> {
    let registry = app.state::<Arc<SftpRegistry>>();
    let cancelled = registry.cancel_conflicts_of(&sftp_id);
    registry.cancel_pastes_of(&sftp_id);
    // 这个标签的专用连接如果正挂着登录 / 指纹弹窗，一并取消。
    app.state::<AppState>().prompts.cancel_session(&sftp_id);
    let Some(tab) = registry.take(&sftp_id) else {
        return Ok(()); // 已经关了：幂等
    };
    tab.cancel.cancel();
    // 取消这个标签发起的、还在跑的删除。
    let deletes: Vec<Arc<CancelSignal>> = {
        let mut deletes = tab
            .deletes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        deletes.drain().map(|(_, cancel)| cancel).collect()
    };
    for cancel in deletes {
        cancel.cancel();
    }
    // 还在跑的传输全部取消（M3-2 第 3 节：不做"关掉标签后台继续传"），
    // 等它们到终态再收（最多 10 秒，卡住就硬收——引擎随标签一起丢）。
    let unfinished: Vec<u64> = tab
        .engine
        .reports()
        .into_iter()
        .filter(|report| !report.state.is_terminal())
        .map(|report| report.id)
        .collect();
    for id in &unfinished {
        tab.engine.cancel(*id);
    }
    if !unfinished.is_empty() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            let still = tab
                .engine
                .reports()
                .into_iter()
                .filter(|report| !report.state.is_terminal())
                .count();
            if still == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    tab.browse.close();
    debug!(%sftp_id, cancelled, "SFTP 标签已关闭");
    Ok(())
}

fn emit_sftp_state(app: &AppHandle, sftp_id: &str, state: &str, reason: Option<String>) {
    let payload: (String, &str, Option<String>) = (sftp_id.to_owned(), state, reason);
    if let Err(err) = app.emit(SFTP_STATE_EVENT, payload) {
        debug!(%err, "发 SFTP 状态事件失败");
    }
}

/// 从设置页读传输参数（第 5 节：并发数、保留属性）。没设置就用默认。
fn transfer_options(store: &Database) -> TransferOptions {
    let mut options = TransferOptions::default();
    if let Ok(Some(raw)) = store.setting_get("sftp_concurrency") {
        if let Ok(value) = raw.parse::<usize>() {
            options.concurrency = value.clamp(1, 16);
        }
    }
    if let Ok(Some(raw)) = store.setting_get("sftp_preserve_mtime") {
        options.preserve_mtime = raw == "true";
    }
    if let Ok(Some(raw)) = store.setting_get("sftp_preserve_permissions") {
        options.preserve_permissions = raw == "true";
    }
    options
}

// ---------------------------------------------------------------------------
// 远端的浏览和文件操作
// ---------------------------------------------------------------------------

/// 远端登录 shell 的类型：第一次探测（复用 AI 的探针），之后用缓存。
/// 认不出（Windows、乱码、超时）当作 `Other`——不能用 `cp`。
pub(crate) async fn remote_shell_kind(tab: &SftpTab) -> zssh_core::ai::shell_kind::ShellKind {
    use zssh_core::ai::shell_kind::ShellKind;
    let mut slot = tab.shell_kind.lock().await;
    if let Some(kind) = *slot {
        return kind;
    }
    let kind = zssh_core::sftp::server_copy::probe_remote_shell(tab.connection.connection())
        .await
        .unwrap_or(ShellKind::Other);
    *slot = Some(kind);
    kind
}

pub(crate) async fn tab_of(app: &AppHandle, sftp_id: &str) -> Result<Arc<SftpTab>, String> {
    let registry = app.state::<Arc<SftpRegistry>>();
    registry
        .get(sftp_id)
        .ok_or_else(|| format!("SFTP 标签不存在：{sftp_id}"))
}

/// 列远端目录（`""` = 家目录；远端栏第一次进来用这个）。
#[tauri::command]
pub async fn sftp_list(
    sftp_id: String,
    path: String,
    app: AppHandle,
) -> Result<SftpListing, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let target = if path.is_empty() {
        tab.browse
            .realpath(".")
            .await
            .map_err(|err| err.to_string())?
    } else {
        path
    };
    let entries = tab
        .browse
        .read_dir(&target)
        .await
        .map_err(|err| err.to_string())?;
    Ok(SftpListing {
        path: target,
        entries: entries
            .into_iter()
            .map(|entry| EntryDto::from_remote(entry).hidden_remote())
            .collect(),
    })
}

/// `sftp_list` 的返回：真实路径（第一次进家目录时界面要拿它记当前目录）+ 条目。
#[derive(Debug, Clone, Serialize)]
pub struct SftpListing {
    pub path: String,
    pub entries: Vec<EntryDto>,
}

/// 读一个远端条目的属性（`..` 在面包屑里跳上一级用）。
#[tauri::command]
pub async fn sftp_stat(sftp_id: String, path: String, app: AppHandle) -> Result<EntryDto, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let info = zssh_core::sftp::fs::stat(&tab.browse, &path, false)
        .await
        .map_err(|err| err.to_string())?;
    let kind = match &info.kind {
        FileKind::Dir => "dir",
        FileKind::File => "file",
        FileKind::Symlink { .. } => "symlink",
        FileKind::Other(_) => "other",
    };
    Ok(EntryDto {
        name: info
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&info.path)
            .to_owned(),
        path: info.path,
        kind,
        link_target: None,
        size: info.size,
        mtime: info.mtime.map(u64::from),
        permissions: info.permissions,
        owner: info
            .user
            .clone()
            .or_else(|| info.uid.map(|uid| uid.to_string())),
        name_is_utf8: true,
        hidden: false,
        target_kind: None,
    })
}

/// 远端建目录（一次建多层：`a/b/c`）。
#[tauri::command]
pub async fn sftp_mkdir(sftp_id: String, path: String, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if path.trim().is_empty() {
        return Err("目录名不能为空".to_owned());
    }
    zssh_core::sftp::fs::create_dir_all(&tab.browse, &path)
        .await
        .map_err(|err| err.to_string())
}

/// 远端建空文件。
#[tauri::command]
pub async fn sftp_create_file(sftp_id: String, path: String, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    tab.browse
        .create_file(&path)
        .await
        .map_err(|err| err.to_string())
}

/// 远端重命名 / 移动。`overwrite` 是覆盖式改名（rename 对话框里选的）。
#[tauri::command]
pub async fn sftp_rename(
    sftp_id: String,
    from: String,
    to: String,
    overwrite: bool,
    app: AppHandle,
) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    tab.browse
        .rename(&from, &to, overwrite)
        .await
        .map_err(|err| err.to_string())
        .map(|_| ())
}

/// 远端改权限（八进制；文件夹可以递归）。
#[tauri::command]
pub async fn sftp_set_permissions(
    sftp_id: String,
    path: String,
    mode: u32,
    recursive: bool,
    app: AppHandle,
) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let mode = mode & 0o777;
    tab.browse
        .set_permissions(&path, mode)
        .await
        .map_err(|err| err.to_string())?;
    if !recursive {
        return Ok(());
    }
    // 递归：目录自己先改，再一层层往下（符号链接当文件改，不跟进去）。
    let mut queue = vec![path];
    while let Some(current) = queue.pop() {
        let info = zssh_core::sftp::fs::stat(&tab.browse, &current, false)
            .await
            .map_err(|err| err.to_string())?;
        if info.kind.is_dir() {
            for entry in tab
                .browse
                .read_dir(&current)
                .await
                .map_err(|err| err.to_string())?
            {
                if !entry.name_is_utf8 {
                    continue; // 非 UTF-8 名字不操作（已知限制）
                }
                queue.push(entry.path);
            }
        }
        tab.browse
            .set_permissions(&current, mode)
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// 删除（可以一次删多个；目录递归）。**立即返回** delete_id，进度走事件。
///
/// 返回的是这次删除的项数（先 `stat` 出来，界面确认对话框用它）。
#[tauri::command]
pub async fn sftp_delete(
    sftp_id: String,
    paths: Vec<String>,
    app: AppHandle,
) -> Result<u64, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if paths.is_empty() {
        return Err("没有要删除的东西".to_owned());
    }
    for path in &paths {
        if path.trim().is_empty() {
            return Err("路径不能为空".to_owned());
        }
    }
    let registry = app.state::<Arc<SftpRegistry>>();
    let delete_id = registry.next_delete.fetch_add(1, Ordering::SeqCst) + 1;
    let cancel = Arc::new(CancelSignal::default());
    tab.deletes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(delete_id, Arc::clone(&cancel));

    // 删除在后台跑（目录可能很大），进度走 sftp_delete_event；这里立即返回。
    let app = app.clone();
    let sftp_id_for_task = sftp_id.clone();
    tauri::async_runtime::spawn(async move {
        let deleter = RecursiveDelete::new();
        // 界面点"取消删除" / 关标签只会触发 CancelSignal；递归删除器有自己的
        // 取消标志，得有人把前者转给后者，不然取消只在多个顶层路径之间生效，
        // 删一个大目录时点取消没有任何作用（M3-2 端到端实测发现）。
        let watcher = forward_cancel(Arc::clone(&cancel), deleter.cancel_handle());
        let mut total = 0u64;
        let mut last_error: Option<String> = None;
        for path in paths {
            if cancel.is_cancelled() {
                break;
            }
            let result = deleter
                .run(&tab.browse, &path, |event| {
                    total = event.deleted;
                    let _ = app.emit(
                        SFTP_DELETE_EVENT,
                        (
                            sftp_id_for_task.clone(),
                            DeleteEventDto::Progress {
                                delete_id,
                                deleted: event.deleted,
                                current: event.current.clone(),
                            },
                        ),
                    );
                })
                .await;
            if let Err(err) = result {
                if !matches!(err, SftpError::Cancelled) {
                    last_error = Some(err.to_string());
                    break;
                }
            }
        }
        watcher.abort();
        tab.deletes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&delete_id);
        let _ = app.emit(
            SFTP_DELETE_EVENT,
            (
                sftp_id_for_task,
                DeleteEventDto::Done {
                    delete_id,
                    deleted: total,
                    error: last_error,
                    cancelled: cancel.is_cancelled(),
                },
            ),
        );
    });
    Ok(delete_id)
}

/// 把界面的取消信号转给递归删除器自己的取消句柄；返回的任务在删除结束时 abort。
fn forward_cancel(
    cancel: Arc<CancelSignal>,
    handle: zssh_core::sftp::fs::CancelHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        cancel.cancelled().await;
        handle.cancel();
    })
}

/// 取消一次还在跑的删除。
#[tauri::command]
pub async fn sftp_delete_cancel(
    sftp_id: String,
    delete_id: u64,
    app: AppHandle,
) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let found = tab
        .deletes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&delete_id)
        .cloned();
    match found {
        Some(cancel) => {
            cancel.cancel();
            Ok(())
        }
        None => Err(format!("没有这个删除任务：{delete_id}")),
    }
}

// ---------------------------------------------------------------------------
// 传输队列
// ---------------------------------------------------------------------------

/// 提交传输（拖动 / 右键"上传下载"进来）：`sources` 里的每一项一个任务。
///
/// 上传：sources 是**本地**路径，target 是远端目录；下载反过来。
/// 目标存在时（整目录传到已有目录）引擎会展开成 `<target>/<名字>`。
#[tauri::command]
pub async fn sftp_transfer_submit(
    sftp_id: String,
    direction: String,
    sources: Vec<String>,
    target: String,
    app: AppHandle,
) -> Result<Vec<u64>, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    let direction = match direction.as_str() {
        "upload" => Direction::Upload,
        "download" => Direction::Download,
        other => return Err(format!("不认识的传输方向：{other}")),
    };
    if sources.is_empty() {
        return Err("没有要传的东西".to_owned());
    }
    if target.trim().is_empty() {
        return Err("目标目录不能为空".to_owned());
    }
    // 一次提交的所有任务属于同一个冲突批次：任何一个任务的对话框里勾了"全部应用"，
    // 同批次后面撞上同名的就不再问。
    let batch = tab.engine.new_batch();
    let mut ids = Vec::with_capacity(sources.len());
    for source in sources {
        if source.trim().is_empty() {
            return Err("源路径不能为空".to_owned());
        }
        // 目录任务：目标带不带名字由"源是不是目录"决定（对齐 Xftp 的习惯：
        // 拖目录 a 到 /remote，落点是 /remote/a）。
        // 目标 = 目标目录 + 源的名字（对齐 Xftp：拖 a 到 /remote，目录落点是
        // /remote/a，文件落点是 /remote/<文件名>）。调用方给的是"目标目录"
        // （另一栏的当前目录）；单文件之前没拼名字，文件会往目录路径上写。
        let job_target = match direction {
            Direction::Upload => join_remote(&target, file_name_of(&source)),
            Direction::Download => download_target(&target, &source)?,
            Direction::RemoteCopy | Direction::LocalCopy => {
                return Err("复制粘贴请用 sftp_paste".to_owned())
            }
        };
        let job = TransferJob {
            direction,
            source,
            target: job_target,
            recursive: true,
            peer: None,
        };
        let id = tab.engine.submit_in_batch(job.clone(), &batch);
        tab.remember_job(id, job);
        ids.push(id);
    }
    Ok(ids)
}

impl SftpTab {
    pub(crate) fn remember_job(&self, id: u64, job: TransferJob) {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, job);
    }
}

fn join_remote(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

#[cfg(test)]
mod download_target_tests;

/// 顶层下载的本地落点：和递归下载的子项走同一套名字规范化，并检查确实在所选目录里。
fn download_target(dir: &str, source: &str) -> Result<String, String> {
    zssh_core::sftp::local::download_target(Path::new(dir), source)
        .map(|path| path.display().to_string())
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn sftp_transfer_pause(sftp_id: String, id: u64, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if tab.engine.pause(id) {
        Ok(())
    } else {
        Err(format!("暂停失败：没有任务 {id}"))
    }
}

#[tauri::command]
pub async fn sftp_transfer_resume(sftp_id: String, id: u64, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if tab.engine.resume(id) {
        Ok(())
    } else {
        Err(format!("继续失败：没有任务 {id}"))
    }
}

#[tauri::command]
pub async fn sftp_transfer_cancel(sftp_id: String, id: u64, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if tab.engine.cancel(id) {
        Ok(())
    } else {
        Err(format!("取消失败：没有任务 {id}"))
    }
}

/// 重试失败的任务（同一个任务号重跑，续传对账把着关）。
#[tauri::command]
pub async fn sftp_transfer_retry(sftp_id: String, id: u64, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if tab.engine.retry(id) {
        Ok(())
    } else {
        Err(format!(
            "重试失败：任务 {id} 不存在，或者不是失败 / 已取消 / 已跳过"
        ))
    }
}

/// 这个标签现在全部任务的快照（队列初始化用）。
#[tauri::command]
pub async fn sftp_transfers(sftp_id: String, app: AppHandle) -> Result<Vec<TransferDto>, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    Ok(tab
        .engine
        .reports()
        .into_iter()
        .filter_map(|report| {
            tab.job_of(report.id)
                .map(|job| TransferDto::of(&report, &job))
        })
        .collect())
}

/// 清掉已经结束的任务（"清除已完成"）。返回清了几个。
#[tauri::command]
pub async fn sftp_transfers_clear(sftp_id: String, app: AppHandle) -> Result<usize, String> {
    let tab = tab_of(&app, &sftp_id).await?;
    Ok(tab.engine.clear_finished())
}

/// 冲突的回答（对话框里选了之后）。
#[tauri::command]
pub async fn sftp_conflict_answer(
    app: AppHandle,
    conflict_id: u64,
    action: String,
    apply_to_all: bool,
    rename_to: Option<String>,
) -> Result<(), String> {
    let registry = app.state::<Arc<SftpRegistry>>();
    let action =
        parse_conflict_action(&action).ok_or_else(|| format!("不认识的冲突动作：{action}"))?;
    // 选"续传"但条件不满足：引擎会降级成覆盖并注明，这里原样送过去。
    let decision = ConflictDecision {
        action,
        apply_to_all,
        rename_to,
    };
    registry.answer_conflict(conflict_id, decision)
}

// ---------------------------------------------------------------------------
// 本地（左栏）：不连服务器
// ---------------------------------------------------------------------------

/// 本地栏默认目录：家目录（第一次打开时）。
#[tauri::command]
pub async fn sftp_local_default_dir() -> Result<String, String> {
    let home = dirs_home().ok_or_else(|| "找不到用户目录".to_owned())?;
    Ok(home.display().to_string())
}

/// 列本地目录：`""` = "此电脑"（所有盘符）。
///
/// 读目录是同步的文件系统调用：整批放到阻塞线程池里做，不占异步运行时的工作线程
/// （大目录、磁盘忙的时候会拖慢别的后台任务）。
#[tauri::command]
pub async fn sftp_local_list(path: String) -> Result<Vec<EntryDto>, String> {
    tauri::async_runtime::spawn_blocking(move || list_local_dir(&path))
        .await
        .map_err(|err| format!("读目录的线程异常结束：{err}"))?
}

fn list_local_dir(path: &str) -> Result<Vec<EntryDto>, String> {
    if path.is_empty() {
        return Ok(drives());
    }
    // 给界面的路径不能带 `\\?\` 前缀：`read_dir` 收到带前缀的目录时，`DirEntry::path()` 也带着它，
    // 前端的面包屑和"向上"按 `X:\` 的形状切路径，带前缀就全乱了（R-10 P1-4）。前缀只在真正调
    // 系统接口时加；传进来的路径本身带着前缀（旧界面状态）也先剥掉。
    let dir = zssh_core::pathnorm::simplify_verbatim(PathBuf::from(&path));
    let entries = std::fs::read_dir(zssh_core::sftp::local::long_path(&dir))
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name();
        // Windows 上枚举目录时属性已经一起拿回来了，DirEntry::metadata 不再多一次系统调用；
        // 它不跟随链接（和 symlink_metadata 一样），隐藏属性正好从它取。
        out.push(local_entry_of(
            dir.join(&name),
            &name.to_string_lossy(),
            entry.metadata().ok(),
        ));
    }
    Ok(out)
}

/// 一个本地条目转成 DTO（失败/拿不到的属性按缺省处理，不整页失败）。
///
/// `own` 是条目本身的属性（不跟随链接）：普通文件和目录直接用它；符号链接、目录联接
/// 才再去读它指向的目标（大小、类型、时间都按目标显示）。没有就按路径现查。
fn local_entry_of(path: PathBuf, name: &str, own: Option<std::fs::Metadata>) -> EntryDto {
    let own = own.or_else(|| std::fs::symlink_metadata(&path).ok());
    let is_link = own
        .as_ref()
        .is_some_and(|meta| meta.file_type().is_symlink());
    let meta = match (&own, is_link) {
        (Some(meta), false) => Ok(meta.clone()),
        _ => std::fs::metadata(zssh_core::sftp::local::long_path(&path)),
    };
    let (kind, size) = match &meta {
        Ok(meta) if meta.is_dir() => ("dir", None),
        Ok(meta) => ("file", Some(meta.len())),
        Err(_) => ("other", None),
    };
    // Windows 的"隐藏"属性（FILE_ATTRIBUTE_HIDDEN）拿 symlinks 兜底：目录联接和
    // 隐藏文件都算，界面的开关管要不要显示。
    let hidden = own
        .as_ref()
        .ok_or(())
        .map(|meta| {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x2 != 0
            }
            #[cfg(not(windows))]
            {
                let _ = meta;
                name_starts_with_dot(name)
            }
        })
        .unwrap_or_else(|_| name_starts_with_dot(name));
    let mtime = meta
        .as_ref()
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    EntryDto {
        name: name.to_owned(),
        path: path.display().to_string(),
        kind,
        link_target: None,
        size,
        mtime,
        permissions: None,
        owner: None,
        name_is_utf8: true,
        hidden,
        target_kind: None,
    }
}

/// "此电脑"：所有盘符。
fn drives() -> Vec<EntryDto> {
    // 已分配的盘符一次拿全（GetLogicalDrives），不逐个探测：断开的网络盘不会拖慢打开。
    #[cfg(windows)]
    {
        zssh_core::sftp::local::logical_drives()
            .into_iter()
            .map(|letter| EntryDto {
                name: format!("本地磁盘 ({letter}:)"),
                path: format!("{letter}:\\"),
                kind: "drive",
                link_target: None,
                size: None,
                mtime: None,
                permissions: None,
                owner: None,
                name_is_utf8: true,
                hidden: false,
                target_kind: None,
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![EntryDto {
            name: "/".to_owned(),
            path: "/".to_owned(),
            kind: "dir",
            link_target: None,
            size: None,
            mtime: None,
            permissions: None,
            owner: None,
            name_is_utf8: true,
            hidden: false,
            target_kind: None,
        }]
    }
}

#[tauri::command]
pub async fn sftp_local_mkdir(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("目录名不能为空".to_owned());
    }
    std::fs::create_dir_all(zssh_core::sftp::local::long_path(Path::new(&path)))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn sftp_local_create_file(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("文件名不能为空".to_owned());
    }
    // 不先查再建：由系统在创建那一刻判断，已存在的同名文件不会被截断。
    zssh_core::sftp::local::create_new_file(Path::new(&path)).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            "已经有同名的文件".to_owned()
        } else {
            err.to_string()
        }
    })
}

#[tauri::command]
pub async fn sftp_local_rename(from: String, to: String) -> Result<(), String> {
    if from.trim().is_empty() || to.trim().is_empty() {
        return Err("路径不能为空".to_owned());
    }
    if std::fs::metadata(&to).is_ok() {
        return Err("已经有同名的文件".to_owned());
    }
    // 系统在改名那一刻判断目标是否存在（上面的检查只是为了给出好读的提示，不能代替它）。
    zssh_core::sftp::local::move_no_replace(
        &zssh_core::sftp::local::long_path(Path::new(&from)),
        &zssh_core::sftp::local::long_path(Path::new(&to)),
    )
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            "已经有同名的文件".to_owned()
        } else {
            err.to_string()
        }
    })
}

/// 删除本地条目（可以一次多个；目录递归）。**同步**返回每个的成败（本地快）。
#[tauri::command]
pub async fn sftp_local_delete(
    paths: Vec<String>,
) -> Result<Vec<(String, Option<String>)>, String> {
    if paths.is_empty() {
        return Err("没有要删除的东西".to_owned());
    }
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let long = zssh_core::sftp::local::long_path(Path::new(&path));
        let result = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                // 链接只删链接本身（和远端的行为一致）。
                std::fs::remove_file(&long)
            }
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&long),
            Ok(_) => std::fs::remove_file(&long),
            Err(err) => Err(err),
        };
        results.push((path, result.err().map(|err| err.to_string())));
    }
    Ok(results)
}

#[cfg_attr(windows, allow(dead_code))]
fn name_starts_with_dot(path: &str) -> bool {
    file_name_of(path).starts_with('.')
}

/// 家目录（不用额外的 crate：`USERPROFILE` 是各版 Windows 都有的）。
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// 单元测试（M3-2 第 6.2 节：命令层的参数校验；冲突超时按跳过）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_actions_parse_from_dialog_strings() {
        assert_eq!(
            parse_conflict_action("overwrite"),
            Some(ConflictAction::Overwrite)
        );
        assert_eq!(parse_conflict_action("skip"), Some(ConflictAction::Skip));
        assert_eq!(
            parse_conflict_action("rename"),
            Some(ConflictAction::Rename)
        );
        assert_eq!(
            parse_conflict_action("resume"),
            Some(ConflictAction::Resume)
        );
        assert_eq!(parse_conflict_action("nonsense"), None);
    }

    /// 冲突的等待：超时按"跳过"（不挂任务）。
    #[tokio::test]
    async fn conflict_wait_times_out_to_skip() {
        // receiver 永远没人回答 → 超时 → Skip（决策本身是纯函数 + 等待语义，
        // 真实等待在 `UiConflictResolver::decide`，超时分支返回 skip_decision()）。
        let (sender, receiver) = oneshot::channel::<ConflictDecision>();
        sender
            .send(ConflictDecision {
                action: ConflictAction::Overwrite,
                apply_to_all: true,
                rename_to: None,
            })
            .ok(); // sender 发完就丢：等待方拿到决定（覆盖 Timeout 的语义这里测"答了用答案"）
        let decision = tokio::time::timeout(CONFLICT_TIMEOUT, async {
            match receiver.await {
                Ok(decision) => decision,
                Err(_) => skip_decision(),
            }
        })
        .await
        .expect("已回答的决定不该超时");
        assert_eq!(decision.action, ConflictAction::Overwrite);

        // 没人回答 + 超时：按"跳过"，且只对这一个文件生效（等 10ms 就够验证语义）。
        let (_sender, receiver) = oneshot::channel::<ConflictDecision>();
        let decision = tokio::time::timeout(Duration::from_millis(10), async {
            match receiver.await {
                Ok(decision) => decision,
                Err(_) => skip_decision(),
            }
        })
        .await;
        assert!(decision.is_err(), "没人回答要超时");
        let decision = skip_decision();
        assert_eq!(decision.action, ConflictAction::Skip);
        assert!(!decision.apply_to_all, "超时的跳过只对这一个文件生效");
    }

    #[tokio::test]
    async fn conflict_sender_dropped_means_skip() {
        // 标签关闭把 sender 丢掉：等待方收到 Err → 跳过。
        let (sender, receiver) = oneshot::channel::<ConflictDecision>();
        drop(sender);
        let decision = match receiver.await {
            Ok(decision) => decision,
            Err(_) => skip_decision(),
        };
        assert_eq!(decision.action, ConflictAction::Skip);
    }

    #[test]
    fn file_name_of_handles_both_kinds_of_slashes() {
        assert_eq!(file_name_of("/tmp/a/b.txt"), "b.txt");
        assert_eq!(file_name_of("C:\\Users\\x\\报告.pdf"), "报告.pdf");
        assert_eq!(file_name_of("plain.txt"), "plain.txt");
        assert_eq!(file_name_of("/tmp/"), "tmp");
    }

    #[test]
    fn remote_join_and_local_join() {
        assert_eq!(join_remote("/tmp/a/", "b"), "/tmp/a/b");
        assert_eq!(join_remote("/tmp", "b"), "/tmp/b");
    }

    /// 本地列目录 + 隐藏属性（不连服务器，直接调命令函数）。
    #[tokio::test]
    async fn local_list_marks_hidden_entries_and_this_pc_lists_drives() {
        let root_guard = tempfile::Builder::new()
            .prefix("zssh-local-list-")
            .tempdir()
            .unwrap();
        let root = root_guard.path().to_path_buf();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        // Windows 的隐藏属性：attrib +h。
        let hidden = root.join("hidden.txt");
        std::fs::write(&hidden, b"h").unwrap();
        #[cfg(windows)]
        {
            let path_arg = hidden.display().to_string().replace('/', "\\");
            let _ = std::process::Command::new("attrib")
                .arg("+h")
                .arg(&path_arg)
                .status();
        }

        let entries = sftp_local_list(root.display().to_string())
            .await
            .expect("列目录");
        let by_name = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("{name} 要在列表里：{entries:?}"))
        };
        assert_eq!(by_name("a.txt").kind, "file");
        #[cfg(windows)]
        assert!(by_name("hidden.txt").hidden, "隐藏属性要标出来");
        // 返回的路径不能带 `\\?\` 前缀（R-10 P1-4），就是"目录 + 名字"。
        assert_eq!(
            by_name("a.txt").path,
            root.join("a.txt").display().to_string()
        );
        for entry in &entries {
            assert!(!entry.path.starts_with(r"\\?\"), "{}", entry.path);
        }
        // 传进来的目录带着前缀也一样。
        #[cfg(windows)]
        {
            let prefixed = format!(r"\\?\{}", root.display());
            let again = sftp_local_list(prefixed).await.expect("带前缀的目录也能列");
            for entry in &again {
                assert!(!entry.path.starts_with(r"\\?\"), "{}", entry.path);
            }
        }

        // "此电脑"（空路径）：至少有当前工作所在的盘。
        let this_pc = sftp_local_list(String::new()).await.expect("盘符列表");
        assert!(
            this_pc.iter().any(|entry| entry.kind == "drive"),
            "盘符列表不能是空的"
        );
    }

    /// 本地文件操作命令的参数校验。
    #[tokio::test]
    async fn local_file_ops_validate_their_arguments() {
        assert!(sftp_local_mkdir("  ".to_owned()).await.is_err());
        assert!(sftp_local_create_file("".to_owned()).await.is_err());
        assert!(sftp_local_rename("a".to_owned(), "".to_owned())
            .await
            .is_err());
        assert!(sftp_local_delete(vec![]).await.is_err());
        assert!(sftp_transfer_submit_validate("sideways".to_owned()).is_err());
    }

    /// 新建文件撞上已有的同名文件：报"已经有同名的文件"，原内容不能被截断（C06）。
    #[tokio::test]
    async fn local_create_file_keeps_an_existing_file_intact() {
        let root_guard = tempfile::Builder::new()
            .prefix("zssh-local-create-")
            .tempdir()
            .unwrap();
        let path = root_guard.path().join("a.txt");
        let path_str = path.to_string_lossy().into_owned();
        sftp_local_create_file(path_str.clone()).await.unwrap();
        std::fs::write(&path, b"keep me").unwrap();
        let err = sftp_local_create_file(path_str).await.unwrap_err();
        assert_eq!(err, "已经有同名的文件");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");

        // 其他错误（父目录不存在）照实报出来，不能当成"不存在"去建、也不能说成"同名"。
        let missing = root_guard.path().join("no-such-dir").join("b.txt");
        let err = sftp_local_create_file(missing.to_string_lossy().into_owned())
            .await
            .unwrap_err();
        assert_ne!(err, "已经有同名的文件");
        assert!(!missing.exists());
    }

    /// 传输方向的校验（不真起引擎，只测解析那段）。
    fn sftp_transfer_submit_validate(direction: String) -> Result<(), String> {
        match direction.as_str() {
            "upload" | "download" => Ok(()),
            other => Err(format!("不认识的传输方向：{other}")),
        }
    }

    /// DTO 的状态映射：失败要带原因。
    #[test]
    fn transfer_dto_carries_state_and_reason() {
        let job = TransferJob {
            direction: Direction::Upload,
            source: "C:\\a.bin".to_owned(),
            target: "/tmp/a.bin".to_owned(),
            recursive: false,
            peer: None,
        };
        let report = ProgressReport {
            id: 7,
            direction: Direction::Upload,
            state: TransferState::Failed("连接断了".to_owned()),
            total_files: 3,
            done_files: 1,
            total_bytes: 30,
            done_bytes: 10,
            speed_bps: 1024,
            eta_secs: Some(20),
            current: Some("/tmp/a.bin".to_owned()),
            notes: vec!["改过名字".to_owned()],
        };
        let dto = TransferDto::of(&report, &job);
        assert_eq!(dto.state, "failed");
        assert_eq!(dto.reason.as_deref(), Some("连接断了"));
        assert_eq!(dto.source, "C:\\a.bin");
        assert_eq!(dto.notes, vec!["改过名字".to_owned()]);

        let report = ProgressReport {
            state: TransferState::WaitingReconnect,
            ..report
        };
        let dto = TransferDto::of(&report, &job);
        assert_eq!(dto.state, "waiting_reconnect");
        assert!(dto.reason.is_none());
    }
}

#[cfg(test)]
mod display_tests {
    use super::display_path;

    /// 长路径前缀不该漏到界面上（队列里的来源/目标）。
    #[test]
    fn display_path_strips_the_long_path_prefix() {
        assert_eq!(display_path(r"\\?\C:\zssh\a.txt"), r"C:\zssh\a.txt");
        assert_eq!(display_path(r"\\?\UNC\server\share\a"), r"\\server\share\a");
        assert_eq!(display_path(r"C:\zssh\a.txt"), r"C:\zssh\a.txt");
        assert_eq!(display_path("/tmp/a"), "/tmp/a");
    }
}

#[cfg(test)]
mod delete_cancel_tests {
    use super::*;

    /// 界面的取消信号要真的传到递归删除器（以前只在多个顶层路径之间检查，
    /// 删一个大目录时点取消没有任何作用）。
    #[tokio::test]
    async fn cancel_signal_reaches_the_recursive_deleter() {
        let deleter = RecursiveDelete::new();
        let cancel = Arc::new(CancelSignal::default());
        let watcher = forward_cancel(Arc::clone(&cancel), deleter.cancel_handle());

        assert!(!deleter.is_cancelled(), "没取消时不该是取消状态");
        cancel.cancel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !deleter.is_cancelled() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "取消信号没传到删除器"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        watcher.abort();
    }

    /// 删除正常结束（没人取消）：abort 观察任务之后，删除器不会被误取消。
    #[tokio::test]
    async fn aborting_the_watcher_does_not_cancel_the_deleter() {
        let deleter = RecursiveDelete::new();
        let cancel = Arc::new(CancelSignal::default());
        let watcher = forward_cancel(Arc::clone(&cancel), deleter.cancel_handle());
        watcher.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!deleter.is_cancelled());
    }
}

#[cfg(test)]
mod options_tests {
    use super::*;

    /// 设置页的三项（并发数、保留修改时间、保留权限）真的进到传输引擎的参数里
    /// （设置页 → settings_set 写库 → sftp_open 时 transfer_options 读库）。
    #[test]
    fn transfer_options_follow_the_settings_table() {
        let store = Database::open_in_memory().expect("内存库");

        // 什么都没设：默认值（并发 3，不保留属性）。
        let defaults = transfer_options(&store);
        assert_eq!(defaults.concurrency, 3);
        assert!(!defaults.preserve_mtime);
        assert!(!defaults.preserve_permissions);

        // 设置页写进来的就是 JSON 文本。
        store.setting_set("sftp_concurrency", "5").expect("写并发");
        store
            .setting_set("sftp_preserve_mtime", "true")
            .expect("写保留时间");
        store
            .setting_set("sftp_preserve_permissions", "true")
            .expect("写保留权限");
        let options = transfer_options(&store);
        assert_eq!(options.concurrency, 5);
        assert!(options.preserve_mtime);
        assert!(options.preserve_permissions);

        // 越界的并发数夹到 1～16；写成 false 就是不保留；不是数字的忽略（用默认）。
        store.setting_set("sftp_concurrency", "0").unwrap();
        assert_eq!(transfer_options(&store).concurrency, 1);
        store.setting_set("sftp_concurrency", "99").unwrap();
        assert_eq!(transfer_options(&store).concurrency, 16);
        store.setting_set("sftp_concurrency", "\"abc\"").unwrap();
        assert_eq!(transfer_options(&store).concurrency, 3);
        store.setting_set("sftp_preserve_mtime", "false").unwrap();
        assert!(!transfer_options(&store).preserve_mtime);
    }

    // ---- 冲突提问的结束事件（SFTP-9）：用模拟运行时，不开窗口 ----

    use std::sync::mpsc;
    use tauri::test::MockRuntime;
    use tauri::Listener;

    fn mock_app() -> tauri::App<MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app")
    }

    fn sample_info() -> ConflictInfo {
        ConflictInfo {
            job: 1,
            direction: Direction::Upload,
            source: "C:/work/a.txt".to_owned(),
            target: "/srv/a.txt".to_owned(),
            source_size: Some(10),
            target_size: Some(5),
            source_mtime: None,
            target_mtime: None,
            can_resume: false,
            suggested_target: "/srv/a (2).txt".to_owned(),
            batch: None,
        }
    }

    /// 监听提问事件和结束事件，返回两个接收端（载荷里取出 `conflict_id`）。
    fn watch_prompts(
        app: &tauri::AppHandle<MockRuntime>,
    ) -> (mpsc::Receiver<u64>, mpsc::Receiver<u64>) {
        let (prompt_tx, prompt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        app.listen(SFTP_CONFLICT_EVENT, move |event| {
            let (_, prompt): (String, serde_json::Value) =
                serde_json::from_str(event.payload()).expect("提问载荷");
            let _ = prompt_tx.send(prompt["conflict_id"].as_u64().expect("conflict_id"));
        });
        app.listen(SFTP_CONFLICT_DONE_EVENT, move |event| {
            let _ = done_tx.send(event.payload().parse::<u64>().expect("结束事件载荷是编号"));
        });
        (prompt_rx, done_rx)
    }

    async fn next_id(receiver: &mpsc::Receiver<u64>) -> u64 {
        for _ in 0..500 {
            if let Ok(id) = receiver.try_recv() {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("没等到事件");
    }

    #[tokio::test]
    async fn answering_a_conflict_broadcasts_that_it_is_done() {
        let app = mock_app();
        let (prompts, done) = watch_prompts(app.handle());
        let registry = Arc::new(SftpRegistry::default());
        let asking = ask_user(
            app.handle(),
            &registry,
            "tab-1",
            Duration::from_secs(30),
            sample_info(),
        );
        let answering = async {
            let id = next_id(&prompts).await;
            assert!(done.try_recv().is_err(), "还没回答就不该发结束事件");
            registry
                .answer_conflict(
                    id,
                    ConflictDecision {
                        action: ConflictAction::Overwrite,
                        apply_to_all: false,
                        rename_to: None,
                    },
                )
                .expect("回答");
            id
        };
        let (decision, id) = tokio::join!(asking, answering);
        assert_eq!(decision.action, ConflictAction::Overwrite);
        assert_eq!(next_id(&done).await, id);
        assert!(done.try_recv().is_err(), "只发一次");
    }

    #[tokio::test]
    async fn a_timed_out_conflict_broadcasts_done_and_forgets_the_prompt() {
        let app = mock_app();
        let (prompts, done) = watch_prompts(app.handle());
        let registry = Arc::new(SftpRegistry::default());
        let decision = ask_user(
            app.handle(),
            &registry,
            "tab-1",
            Duration::from_millis(50),
            sample_info(),
        )
        .await;
        assert_eq!(decision.action, ConflictAction::Skip);
        let id = next_id(&prompts).await;
        assert_eq!(next_id(&done).await, id);
        // 登记表里也没了：迟到的回答得到「没有这个冲突提问」，前端据此静默处理。
        let late = registry.answer_conflict(id, skip_decision());
        assert!(late.expect_err("已超时").contains("没有这个冲突提问"));
    }

    #[tokio::test]
    async fn closing_the_tab_cancels_the_prompt_and_broadcasts_done() {
        let app = mock_app();
        let (prompts, done) = watch_prompts(app.handle());
        let registry = Arc::new(SftpRegistry::default());
        let asking = ask_user(
            app.handle(),
            &registry,
            "tab-1",
            Duration::from_secs(30),
            sample_info(),
        );
        let closing = async {
            let id = next_id(&prompts).await;
            assert_eq!(registry.cancel_conflicts_of("tab-1"), 1);
            id
        };
        let (decision, id) = tokio::join!(asking, closing);
        assert_eq!(decision.action, ConflictAction::Skip);
        assert_eq!(next_id(&done).await, id);
    }

    #[tokio::test]
    async fn an_untouched_suggested_name_is_not_a_hand_typed_one() {
        // 用户没改对话框预填的建议名：回到引擎时 rename_to 是 None（走自动起名）；改过的原样保留。
        for (typed, expected) in [
            ("a (2).txt", None),
            ("  a (2).txt ", None),
            ("mine.txt", Some("mine.txt")),
        ] {
            let app = mock_app();
            let (prompts, _done) = watch_prompts(app.handle());
            let registry = Arc::new(SftpRegistry::default());
            let asking = ask_user(
                app.handle(),
                &registry,
                "tab-1",
                Duration::from_secs(30),
                sample_info(),
            );
            let answering = async {
                let id = next_id(&prompts).await;
                registry
                    .answer_conflict(
                        id,
                        ConflictDecision {
                            action: ConflictAction::Rename,
                            apply_to_all: false,
                            rename_to: Some(typed.to_owned()),
                        },
                    )
                    .expect("回答");
            };
            let (decision, ()) = tokio::join!(asking, answering);
            assert_eq!(decision.action, ConflictAction::Rename);
            assert_eq!(decision.rename_to.as_deref(), expected, "输入 {typed:?}");
        }
    }
}
