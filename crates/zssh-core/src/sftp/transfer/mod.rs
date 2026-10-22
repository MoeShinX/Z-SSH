//! 传输引擎（M3-1 第 3 节）：队列、并发、进度、冲突、暂停续传、失败重试。
//!
//! 边界情况的清单参考 Netcatty `electron/bridges/transferBridge.cjs` 与 `sftpBridge.cjs`
//! （暂存文件、续传对账、暂停/续传的竞争、冲突处理、本地与远端的名字差异），实现是
//! 独立写的 Rust，不是代码移植。
//!
//! 结构：一个 [`TransferEngine`] 管着一批任务，每个任务展开成文件清单后**逐文件**
//! 传（同时进行的文件数受 `concurrency` 限制，默认 3）。单文件内部按批发多个读请求
//! （流水线，见 [`file::copy_file`]）。
//!
//! 通道预算（M3-1 第 1 节）：传输**不开在浏览用的那条 SFTP 通道上**，而是问
//! [`TransferChannelProvider`] 要一条；服务器拒绝开通道（`MaxSessions` 到顶）时，
//! provider 会改走**一条专用连接**（[`SftpReconnect`]），终端和别的功能都不受影响。
//!
//! 进度与状态通过 [`TransferEvent`] 往外发（有节流，不是每块都发）；暂停/取消是对
//! 任务的 [`ControlFlag`] 置位，传输循环在批之间检查。

pub mod backend;
pub mod endpoint;
pub mod file;
pub mod lock;
pub mod memory;
pub mod plan;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify, Semaphore};

use crate::sftp::error::SftpError;
use crate::ssh::BoxFuture;

use backend::TransferEndpoints;
use endpoint::FileEndpoint;
use file::{copy_file, tails_match, ControlFlag, CopyOutcome, CopyTuning};

/// 任务编号。
pub type TransferId = u64;

/// 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
    /// 远端 → 远端（复制粘贴的中转路径：数据经过本机，两端都是 SFTP；源可以在另一台主机上）。
    RemoteCopy,
    /// 本地 → 本地（复制粘贴）。
    LocalCopy,
}

/// 一个传输任务（单文件或文件夹）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferJob {
    pub direction: Direction,
    /// 上传：本地源路径；下载：远端源路径。
    pub source: String,
    /// 上传：远端目标路径；下载：本地目标路径。
    pub target: String,
    /// 目录任务要不要递归展开。
    pub recursive: bool,
    /// 只有 `RemoteCopy` 用：源在另一台主机时，那台主机的数据面在
    /// [`backend::SftpEndpoints::register_peer`] 里登记的名字；`None` = 源和目标在同一台主机。
    pub peer: Option<String>,
}

/// 任务状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Planning,
    Running,
    /// 用户按了暂停（临时文件留着，可以续传）。
    Paused,
    /// 断线了，等重连之后自动接着传。
    WaitingReconnect,
    /// 目标已存在，正在等用户回答怎么处理（界面上显示「等待处理同名文件」，而不是一直
    /// 「准备中」让人以为卡住了）。回答之后回到 `Running`；超时 / 关掉按跳过。
    WaitingConflict,
    /// 传完了。
    Done,
    /// 失败（带原因）。
    Failed(String),
    /// 用户取消。
    Cancelled,
    /// 按冲突策略跳过（目标已存在且用户选了"跳过"）。
    Skipped,
}

impl TransferState {
    /// 终态（不会再变）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransferState::Done
                | TransferState::Failed(_)
                | TransferState::Cancelled
                | TransferState::Skipped
        )
    }
}

/// 进度报告（`TransferEvent` 里的载荷）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressReport {
    pub id: TransferId,
    pub direction: Direction,
    pub state: TransferState,
    /// 队列里的文件总数（展开之后才知道；展开前是 0）。
    pub total_files: u64,
    pub done_files: u64,
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// 瞬时速度（字节/秒）。
    pub speed_bps: u64,
    /// 剩余秒数（速度未知时是 `None`）。
    pub eta_secs: Option<u64>,
    /// 正在传的文件。
    pub current: Option<String>,
    /// 要不要告诉用户的说明（改过的名字、跳过的链接、重连次数…）。
    pub notes: Vec<String>,
}

/// 引擎往外发的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// 进度更新（节流后有变化才发）。
    Progress(ProgressReport),
    /// 任务结束（终态；`state` 里带原因）。
    Finished(ProgressReport),
}

/// 目标已存在时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// 覆盖。
    Overwrite,
    /// 跳过这个文件。
    Skip,
    /// 换个名字（`name (2).ext`）。
    Rename,
    /// 续传（只在目标比源小的时候可选）。
    Resume,
}

/// 冲突信息（交给上层决定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictInfo {
    pub job: TransferId,
    pub direction: Direction,
    pub source: String,
    pub target: String,
    pub source_size: Option<u64>,
    pub target_size: Option<u64>,
    /// 源 / 目标的修改时间（冲突对话框要显示；拿不到是 None）。
    pub source_mtime: Option<u32>,
    pub target_mtime: Option<u32>,
    /// 能不能续传（目标比源小）。
    pub can_resume: bool,
    /// 对话框里预填的新名字（完整路径）：第一个没被占用的自动名字；检查不了或者试到上限
    /// 就退回固定的 `名字 (2)` / `名字 - 副本`，不能因为检查失败卡住提问。
    pub suggested_target: String,
    /// 这次提问属于哪个批次（同一次提交 / 粘贴）。界面用它把同批次还在排队的提问一起回答；
    /// `None` = 不属于任何批次（单独提交的任务、重试过的任务）。
    pub batch: Option<u64>,
}

/// 冲突决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDecision {
    pub action: ConflictAction,
    /// 后面的文件都按这个来（界面上的"对后续文件都这样处理"）。
    pub apply_to_all: bool,
    /// 选"重命名"时用户改过的目标名（None = 用自动生成的 `名字 (2)`）。
    /// 只对这一次生效：勾了"对后续都这样"时，后面的文件仍用自动名字。
    pub rename_to: Option<String>,
}

/// 同一次提交（一次 `sftp_transfer_submit`、一次粘贴）里所有任务共享的"全部应用"选择。
///
/// 引擎按任务各记一份 `conflict_all`，一次传 10 个同名文件就是 10 个任务，第一个对话框里勾了
/// "都这样处理"只管它自己。批次把这个选择放到任务之上：同批次里任何一个任务勾了，后面撞上
/// 同名的任务直接按它处理，不再问。
#[derive(Debug)]
pub struct ConflictBatch {
    id: u64,
    action: Mutex<Option<ConflictAction>>,
}

impl ConflictBatch {
    fn new(id: u64) -> Self {
        Self {
            id,
            action: Mutex::new(None),
        }
    }

    /// 批次号（在同一个引擎里唯一；提问事件带着它）。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 这个批次已经记住的"全部应用"动作。
    pub fn action(&self) -> Option<ConflictAction> {
        *self
            .action
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 记下"全部应用"的动作（后写的覆盖先写的）。
    pub fn remember(&self, action: ConflictAction) {
        *self
            .action
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(action);
    }
}

/// 冲突回调：界面弹窗实现它；测试里用固定策略。
pub trait ConflictResolver: Send + Sync {
    fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision>;
}

/// 默认策略：目标已存在就覆盖（没有界面时用；测试里也用它）。
pub struct OverwriteAll;

impl ConflictResolver for OverwriteAll {
    fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        Box::pin(async {
            ConflictDecision {
                action: ConflictAction::Overwrite,
                apply_to_all: true,
                rename_to: None,
            }
        })
    }
}

/// 全跳过（测试用）。
pub struct SkipAll;

impl ConflictResolver for SkipAll {
    fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        Box::pin(async {
            ConflictDecision {
                action: ConflictAction::Skip,
                apply_to_all: true,
                rename_to: None,
            }
        })
    }
}

/// 传输参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOptions {
    /// 同时进行的文件数（默认 3）。
    pub concurrency: usize,
    /// 是否开流水线（单个大文件同时发多个读请求）。
    pub pipeline: bool,
    /// 流水线的在途请求数。
    pub in_flight: usize,
    /// 每块字节数（`None` = 按服务器 limits）。
    pub chunk_len: Option<u32>,
    /// 保留修改时间。
    pub preserve_mtime: bool,
    /// 保留权限。
    pub preserve_permissions: bool,
    /// 断线重试次数上限。
    pub max_retries: u32,
    /// 进度的最小发送间隔（节流）。
    pub progress_interval: Duration,
    /// 续传对账的窗口大小。
    pub tail_window: u64,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            concurrency: 3,
            pipeline: true,
            in_flight: 8,
            chunk_len: None,
            preserve_mtime: false,
            preserve_permissions: false,
            max_retries: 3,
            progress_interval: Duration::from_millis(200),
            tail_window: file::TAIL_WINDOW,
        }
    }
}

/// 任务在引擎里的状态（进度 + 控制）。
struct JobState {
    id: TransferId,
    job: TransferJob,
    control: ControlFlag,
    /// 所属批次；重试时摘掉（重试要重新问，不受批次里旧的选择影响）。
    batch: Mutex<Option<Arc<ConflictBatch>>>,
    /// 用户暂停之后唤醒的开关。
    resume_signal: Notify,
    progress: Mutex<JobProgress>,
    state: Mutex<TransferState>,
}

struct JobProgress {
    total_files: u64,
    done_files: u64,
    total_bytes: u64,
    done_bytes: u64,
    current: Option<String>,
    notes: Vec<String>,
    last_emit: Option<Instant>,
    /// 速度采样（最近一次：时刻 + 当时已传字节）。
    sample: Option<(Instant, u64)>,
    speed_bps: u64,
    /// "对后续文件都这样"记住的冲突策略。
    conflict_all: Option<ConflictAction>,
    /// 已经问过冲突的目标（按计划里的目标路径记）和用户当时的选择。**暂停再续传不重新问**：
    /// 暂停之前用户已经选过（覆盖 / 续传 / 改名），续传接着断点传就行；重新问一遍
    /// 的话，没人盯着就会按超时跳过，传了一半的文件被丢掉（M3-2 实测踩到）。
    /// 记的是选择本身而不只是"问过"：只有用户真的同意覆盖，提交时才允许替换目标（C05）。
    conflicted_targets: std::collections::HashMap<String, RememberedConflict>,
}

/// 一个目标上用户做过的冲突选择（暂停后重进同一个文件时沿用）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RememberedConflict {
    /// 同意覆盖（选了覆盖或续传）。
    Overwrite,
    /// 改成了这个名字。
    Renamed(String),
}

impl JobProgress {
    fn new() -> Self {
        Self {
            total_files: 0,
            done_files: 0,
            total_bytes: 0,
            done_bytes: 0,
            current: None,
            notes: Vec::new(),
            last_emit: None,
            sample: None,
            speed_bps: 0,
            conflict_all: None,
            conflicted_targets: Default::default(),
        }
    }
}

/// 传输引擎。
pub struct TransferEngine {
    /// 数据面（生产是本地 ↔ 远端；单测是内存替身）。
    endpoints: Arc<dyn TransferEndpoints>,
    resolver: Arc<dyn ConflictResolver>,
    options: TransferOptions,
    events: mpsc::UnboundedSender<TransferEvent>,
    jobs: Mutex<HashMap<TransferId, Arc<JobState>>>,
    next_id: AtomicU64,
    next_batch: AtomicU64,
    /// 这个引擎的标识：和任务号一起组成临时文件名里的任务标识（不同引擎、不同次启动不会撞）。
    nonce: String,
    /// 同时进行的文件数上限。
    permits: Arc<Semaphore>,
    /// 自己（弱引用）：后台任务拿着它，引擎被丢掉时任务自然退出。
    this: std::sync::OnceLock<std::sync::Weak<TransferEngine>>,
}

impl TransferEngine {
    /// 建一个引擎；返回的接收端拿事件（进度、结束）。
    pub fn new(
        endpoints: Arc<dyn TransferEndpoints>,
        resolver: Arc<dyn ConflictResolver>,
        options: TransferOptions,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<TransferEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let this = std::sync::OnceLock::new();
        let engine = Arc::new_cyclic(|weak| {
            let _ = this.set(weak.clone());
            Self {
                endpoints,
                resolver,
                permits: Arc::new(Semaphore::new(options.concurrency.max(1))),
                options,
                events,
                jobs: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                next_batch: AtomicU64::new(1),
                nonce: uuid::Uuid::new_v4().simple().to_string()[..8].to_owned(),
                this,
            }
        });
        (engine, receiver)
    }

    pub fn options(&self) -> &TransferOptions {
        &self.options
    }

    /// 开一个冲突批次：同一次提交 / 粘贴的所有任务用 [`Self::submit_in_batch`] 排进同一个批次，
    /// 共享"全部应用"的选择。
    pub fn new_batch(&self) -> Arc<ConflictBatch> {
        Arc::new(ConflictBatch::new(
            self.next_batch.fetch_add(1, Ordering::SeqCst),
        ))
    }

    /// 把一个任务排进队列（立即返回任务号；真正干活在后台）。不属于任何批次：
    /// "全部应用"只管这个任务自己。
    pub fn submit(&self, job: TransferJob) -> TransferId {
        self.submit_with(job, None)
    }

    /// 同 [`Self::submit`]，但任务属于一个批次。
    pub fn submit_in_batch(&self, job: TransferJob, batch: &Arc<ConflictBatch>) -> TransferId {
        self.submit_with(job, Some(Arc::clone(batch)))
    }

    fn submit_with(&self, job: TransferJob, batch: Option<Arc<ConflictBatch>>) -> TransferId {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let state = Arc::new(JobState {
            id,
            job: job.clone(),
            control: ControlFlag::new(),
            batch: Mutex::new(batch),
            resume_signal: Notify::new(),
            progress: Mutex::new(JobProgress::new()),
            state: Mutex::new(TransferState::Queued),
        });
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, Arc::clone(&state));
        self.emit(&state, true);

        // 后台任务拿 Weak：引擎被丢掉时任务自然结束（不会拽着引擎不放）。
        let weak = self
            .this
            .get()
            .cloned()
            .unwrap_or_else(std::sync::Weak::<TransferEngine>::new);
        tokio::spawn(async move {
            if let Some(engine) = weak.upgrade() {
                engine.run_job(state, false).await;
            }
        });
        id
    }

    /// 重试一个失败的任务（M3-2 第 1 节的"重试"命令）。
    ///
    /// 同一个任务号重跑：进度归零、重新展开；上次失败留下的 `.zssh-part` 先做尾部
    /// 对账，对得上的接着传，对不上的从头传。失败 / 取消 / 跳过的任务能重试；
    /// 跳过的任务重试时会重新问一次同名文件怎么处理（上次记住的选择已清掉）。
    pub fn retry(&self, id: TransferId) -> bool {
        let Some(state) = self.job(id) else {
            return false;
        };
        if !matches!(
            self.state_of(&state),
            TransferState::Failed(_) | TransferState::Cancelled | TransferState::Skipped
        ) {
            return false;
        }
        {
            let mut progress = self.lock_progress(&state);
            progress.total_files = 0;
            progress.done_files = 0;
            progress.total_bytes = 0;
            progress.done_bytes = 0;
            progress.current = None;
            progress.sample = None;
            progress.speed_bps = 0;
            progress.last_emit = None;
            // "对后续都这样"和已问过的目标只对一次任务生效；重试要重新问。
            progress.conflict_all = None;
            progress.conflicted_targets.clear();
        }
        // 同理，重试的任务脱离原来的批次：批次里旧的"全部应用"不再替它回答，它重新问，
        // 重试后勾的"全部应用"也只管它自己。
        *state
            .batch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        state.control.reset();
        {
            // set_state 对终态只读，重试要显式改回去。
            let mut current = state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *current = TransferState::Queued;
        }
        self.emit(&state, true);
        let weak = self
            .this
            .get()
            .cloned()
            .unwrap_or_else(std::sync::Weak::<TransferEngine>::new);
        tokio::spawn(async move {
            if let Some(engine) = weak.upgrade() {
                engine.run_job(state, true).await;
            }
        });
        true
    }

    /// 清掉已经结束的任务（界面上的"清除已完成"）。返回清了几个。
    pub fn clear_finished(&self) -> usize {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = jobs.len();
        jobs.retain(|_, state| {
            !state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_terminal()
        });
        before - jobs.len()
    }

    /// 所有任务的最新报告（界面初始化 / 诊断用），按任务号排序。
    pub fn reports(&self) -> Vec<ProgressReport> {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut ids: Vec<&TransferId> = jobs.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| jobs.get(id).map(|state| self.report_of(state)))
            .collect()
    }

    /// 由 JobState 拼一份最新报告。
    fn report_of(&self, state: &Arc<JobState>) -> ProgressReport {
        let progress = self.lock_progress(state);
        ProgressReport {
            id: state.id,
            direction: state.job.direction,
            state: self.state_of(state),
            total_files: progress.total_files,
            done_files: progress.done_files,
            total_bytes: progress.total_bytes,
            done_bytes: progress.done_bytes,
            speed_bps: progress.speed_bps,
            eta_secs: None,
            current: progress.current.clone(),
            notes: progress.notes.clone(),
        }
    }

    async fn run_job(self: Arc<Self>, state: Arc<JobState>, prefer_resume: bool) {
        self.set_state(&state, TransferState::Planning);
        self.emit(&state, true);

        // 展开任务（下载要读远端目录，走数据面）。
        let plan = match self.build_plan(&state.job).await {
            Ok(plan) => plan,
            Err(err) => {
                self.finish_with_error(&state, err);
                return;
            }
        };
        {
            let mut progress = self.lock_progress(&state);
            progress.total_files = plan.files.len() as u64;
            progress.total_bytes = plan.total_bytes();
            for note in &plan.notes {
                progress.notes.push(note.clone());
            }
        }
        self.emit(&state, true);

        // 先建目录（目标端）。
        if let Err(err) = self.create_dirs(&state.job, &plan).await {
            self.finish_with_error(&state, err);
            return;
        }

        self.set_state(&state, TransferState::Running);
        // 状态变成传输中要马上告诉界面：不然在第一条进度到来之前（含等同名文件的回答）
        // 界面一直停在「准备中」。
        self.emit(&state, true);
        let mut failed: Option<SftpError> = None;
        let mut completed_files = 0u64;
        let mut skipped_files = 0u64;
        // 用下标循环：**暂停之后要重跑同一个文件**（不是跳过去）。
        let mut index = 0usize;
        while index < plan.files.len() {
            let planned = plan.files[index].clone();
            // 暂停：等 resume；取消：收尾。
            loop {
                match state.control.state() {
                    file::ControlState::Running => break,
                    file::ControlState::Paused => {
                        self.set_state(&state, TransferState::Paused);
                        self.emit(&state, true);
                        state.resume_signal.notified().await;
                        // 唤醒之后再看一次状态（可能是 resume，也可能是 cancel）。
                    }
                    file::ControlState::Cancelled => {
                        // 暂停中被取消：临时文件还留在目标端，和"传输中取消"一样要清掉
                        // （那条路径在 run_one_file 里清；这里是另一条路径，没人清）。
                        self.discard_partial(&state, &planned).await;
                        self.set_state(&state, TransferState::Cancelled);
                        self.emit(&state, true);
                        return;
                    }
                }
            }

            // 文件级并发许可。
            let _permit = match Arc::clone(&self.permits).acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    self.set_state(&state, TransferState::Cancelled);
                    self.emit(&state, true);
                    return;
                }
            };

            self.lock_progress(&state).current = Some(planned.target.clone());
            match self.run_one_file(&state, &planned, prefer_resume).await {
                Ok(FileOutcome::Done) => {
                    completed_files += 1;
                    self.lock_progress(&state).done_files += 1;
                }
                Ok(FileOutcome::Skipped) => {
                    skipped_files += 1;
                    self.lock_progress(&state).done_files += 1;
                }
                Ok(FileOutcome::Paused) => {
                    // 暂停：已传的字节保留在临时文件里，**同一个文件下次接着传**。
                    self.set_state(&state, TransferState::Paused);
                    self.emit(&state, true);
                    continue;
                }
                Err(SftpError::Cancelled) => {
                    self.set_state(&state, TransferState::Cancelled);
                    self.emit(&state, true);
                    return;
                }
                Err(err) => {
                    failed = Some(err);
                    break;
                }
            }
            index += 1;
            self.emit(&state, false);
        }

        let final_state = match failed {
            Some(err) => TransferState::Failed(err.to_string()),
            // 全都按"跳过"处理了：这个任务算"跳过"（不是失败）。
            None if completed_files == 0 && skipped_files > 0 => TransferState::Skipped,
            None => TransferState::Done,
        };
        self.set_state(&state, final_state);
        self.emit(&state, true);
    }

    /// 展开任务（按方向走本地树或远端树）。
    async fn build_plan(&self, job: &TransferJob) -> Result<plan::TransferPlan, SftpError> {
        match job.direction {
            Direction::Upload => plan::plan_upload(
                std::path::Path::new(&job.source),
                &job.target,
                job.recursive,
            ),
            Direction::Download => {
                let lister: &dyn backend::RemoteLister = self.endpoints.as_ref();
                plan::plan_download(
                    lister,
                    &job.source,
                    std::path::Path::new(&job.target),
                    job.recursive,
                )
                .await
            }
            Direction::RemoteCopy => {
                self.endpoints
                    .plan_remote_copy(job.peer.as_deref(), &job.source, &job.target, job.recursive)
                    .await
            }
            Direction::LocalCopy => plan::plan_local_copy(
                std::path::Path::new(&job.source),
                std::path::Path::new(&job.target),
                job.recursive,
            ),
        }
    }

    /// 目标端的目录先建出来。
    async fn create_dirs(
        &self,
        job: &TransferJob,
        plan: &plan::TransferPlan,
    ) -> Result<(), SftpError> {
        match job.direction {
            Direction::Upload | Direction::RemoteCopy => {
                // 按深度排序再建：父目录一定排在子目录前面（不管展开顺序怎么来的）。
                let mut dirs: Vec<&String> = plan.dirs.iter().collect();
                dirs.sort_by_key(|dir| dir.matches('/').count());
                for dir in dirs {
                    self.endpoints.create_target_dir(dir).await?;
                }
                Ok(())
            }
            Direction::Download | Direction::LocalCopy => {
                for dir in &plan.dirs {
                    let path = crate::sftp::local::long_path(std::path::Path::new(dir));
                    std::fs::create_dir_all(&path)
                        .map_err(|err| crate::sftp::error::from_io(err, &dir.clone()))?;
                }
                Ok(())
            }
        }
    }

    /// 传一个文件（含冲突处理、断线重试、续传）。
    async fn run_one_file(
        self: &Arc<Self>,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        prefer_resume: bool,
    ) -> Result<FileOutcome, SftpError> {
        let mut attempt = 0u32;
        // 重试的任务：上次留下的 .zssh-part 先当续传起点试（尾部对账把着关）。
        let mut resume_from_partial = prefer_resume;
        let mut renamed_target: Option<String> = None;
        // 改名时已经抢到手的新名字的目标锁：交给下一轮用，中间不放手（见 `pick_free_name`）。
        let mut renamed_guard: Option<lock::TargetGuard> = None;
        // 用户是否**真的**同意过覆盖这个目标。开始时目标不存在（没问过）就是 false：
        // 提交用不覆盖的改名，传输期间别人建的同名文件不会被悄悄盖掉（C05）。
        let mut overwrite_authorized = false;
        // 提交时才发现目标冒出来了：重新走冲突流程，临时文件已经完整，接着它提交。
        let mut recheck_conflict = false;
        let owner = self.owner_of(state);

        loop {
            // 目标路径（可能因为冲突策略被改名 / 换了名字）。
            let target = renamed_target
                .clone()
                .unwrap_or_else(|| planned.target.clone());
            // 同一个最终目标同时只让一个任务写；这一轮结束（成功、失败、暂停）就放掉。
            let _target_guard = match renamed_guard.take() {
                Some(guard) => guard,
                None => self.lock_target(state.job.direction, &target).await?,
            };
            let pair = match self
                .endpoints
                .pair(
                    state.job.direction,
                    state.job.peer.as_deref(),
                    &planned.source,
                    &target,
                    &owner,
                )
                .await
            {
                Ok(pair) => pair,
                Err(err) if err.is_retryable() && attempt < self.options.max_retries => {
                    attempt += 1;
                    self.wait_for_retry(state, attempt, &err).await?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let source = Arc::clone(&pair.source);
            let target_endpoint = Arc::clone(&pair.target);

            // 冲突：目标已存在时问一次（同一个任务里"都这样处理"只问一次）。
            if renamed_target.is_none() {
                match self
                    .resolve_conflict(state, &source, &target_endpoint, planned, &target)
                    .await?
                {
                    ConflictOutcome::Proceed { resume, overwrite } => {
                        resume_from_partial = resume || recheck_conflict;
                        overwrite_authorized = overwrite;
                        recheck_conflict = false;
                    }
                    ConflictOutcome::Skip => {
                        if recheck_conflict {
                            // 已经传完的临时文件用不上了。
                            let _ = target_endpoint.remove_partial().await;
                        }
                        return Ok(FileOutcome::Skipped);
                    }
                    ConflictOutcome::Renamed(name, guard) => {
                        renamed_target = Some(name);
                        renamed_guard = guard;
                        continue;
                    }
                }
            }

            // 续传：先看临时文件有多少，再和源对一下尾部；对不上从头来。
            let source_size = source.size().await?.unwrap_or(0);
            let mut resume_len = if resume_from_partial {
                target_endpoint.partial_len().await?
            } else {
                0
            };
            if resume_len > source_size {
                // 临时文件比源还长（源换了 / 上次传的是别的文件）：放弃续传。
                target_endpoint.remove_partial().await?;
                resume_len = 0;
            }
            if resume_len > 0
                && !tails_match(
                    &source,
                    &target_endpoint,
                    resume_len,
                    self.options.tail_window,
                )
                .await?
            {
                self.push_note(
                    state,
                    format!("{} 的续传位置对不上，从头传", planned.target),
                );
                target_endpoint.remove_partial().await?;
                resume_len = 0;
            }

            let tuning = CopyTuning {
                // 没显式配置时用数据面建议的块大小（SFTP 那边是服务器声明的读限制）。
                chunk_len: self
                    .options
                    .chunk_len
                    .unwrap_or_else(|| self.endpoints.suggested_chunk_len()),
                in_flight: if self.options.pipeline {
                    self.options.in_flight.max(1)
                } else {
                    1
                },
            };
            let mut file_bytes_done = resume_len;
            let outcome = copy_file(
                Arc::clone(&source),
                Arc::clone(&target_endpoint),
                source_size,
                resume_len,
                tuning,
                &state.control,
                {
                    let state = Arc::clone(state);
                    let engine = Arc::clone(self);
                    move |done, _total| {
                        // 进度 = 之前文件累计 + 本文件已传（不含续传前已有的部分）。
                        let delta = done - file_bytes_done.min(done);
                        file_bytes_done = done;
                        engine.lock_progress(&state).done_bytes += delta;
                        engine.emit(&state, false);
                    }
                },
            )
            .await;

            match outcome {
                Ok(CopyOutcome::Completed { .. }) => {
                    // 收尾：把临时文件改名成最终名字。只有用户同意过覆盖才允许替换；
                    // 其余情况由系统在改名那一刻判断目标是否存在（不是先查再改）。
                    let overwrite = renamed_target.is_none() && overwrite_authorized;
                    if let Err(err) = target_endpoint.commit(source_size, overwrite).await {
                        if !overwrite
                            && renamed_target.is_none()
                            && matches!(err, SftpError::AlreadyExists { .. })
                        {
                            // 传输期间别人在目标位置建了同名文件：按冲突处理（问用户 /
                            // 沿用"对后续都这样"），不能当成失败，更不能盖掉它。
                            self.push_note(
                                state,
                                format!("传输期间 {target} 被别的程序建出来了，重新确认怎么处理"),
                            );
                            recheck_conflict = true;
                            resume_from_partial = true;
                            continue;
                        }
                        // 提交阶段断线也算可重试（临时文件还在，能接着来）。
                        if err.is_retryable() && attempt < self.options.max_retries {
                            attempt += 1;
                            self.wait_for_retry(state, attempt, &err).await?;
                            resume_from_partial = true;
                            continue;
                        }
                        return Err(err);
                    }
                    if self.options.preserve_mtime || self.options.preserve_permissions {
                        let (mtime, permissions) =
                            source.attributes().await.unwrap_or((None, None));
                        let _ = target_endpoint
                            .set_attributes(
                                if self.options.preserve_mtime {
                                    mtime
                                } else {
                                    None
                                },
                                if self.options.preserve_permissions {
                                    permissions
                                } else {
                                    None
                                },
                            )
                            .await;
                    }
                    self.lock_progress(state).current = None;
                    return Ok(FileOutcome::Done);
                }
                Ok(CopyOutcome::Paused { .. }) => return Ok(FileOutcome::Paused),
                Ok(CopyOutcome::Cancelled { .. }) => {
                    let _ = target_endpoint.remove_partial().await;
                    return Err(SftpError::Cancelled);
                }
                Err(err) => {
                    if err.is_disconnect() && attempt < self.options.max_retries {
                        attempt += 1;
                        self.endpoints.reset().await;
                        self.wait_for_retry(state, attempt, &err).await?;
                        resume_from_partial = true;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// 临时文件名里的任务标识：同一个任务的重试 / 续传不变，别的任务不同。
    fn owner_of(&self, state: &JobState) -> String {
        format!("{}{}", self.nonce, state.id)
    }

    /// 拿目标的写锁。远端锁是**进程级共享**的，键 = 数据面连的服务器身份（实际连接的
    /// 用户名@主机:端口）+ 路径：每个标签、每个文件侧栏各有一个引擎，不管怎么连上的，
    /// 上传同一台服务器的同一个路径都互斥。
    async fn lock_target(
        &self,
        direction: Direction,
        target: &str,
    ) -> Result<lock::TargetGuard, SftpError> {
        match direction {
            Direction::Download | Direction::LocalCopy => {
                lock::TargetLocks::local().try_acquire_local(std::path::Path::new(target))
            }
            Direction::Upload | Direction::RemoteCopy => {
                let server = self.endpoints.server_identity().await?;
                lock::TargetLocks::remote().try_acquire_remote(&server, target)
            }
        }
    }

    /// 取消时把这个文件在目标端的临时文件删掉（尽力而为：连不上就算了，不报错）。
    async fn discard_partial(&self, state: &Arc<JobState>, planned: &plan::PlannedFile) {
        if let Ok(pair) = self
            .endpoints
            .pair(
                state.job.direction,
                state.job.peer.as_deref(),
                &planned.source,
                &planned.target,
                &self.owner_of(state),
            )
            .await
        {
            let _ = pair.target.remove_partial().await;
        }
    }

    /// 断线 / 可重试错误：报"等待重连"，退避一下再试。
    async fn wait_for_retry(
        &self,
        state: &Arc<JobState>,
        attempt: u32,
        err: &SftpError,
    ) -> Result<(), SftpError> {
        self.set_state(state, TransferState::WaitingReconnect);
        self.push_note(state, format!("连接断了（第 {attempt} 次重试）：{err}"));
        self.emit(state, true);
        let delay = backoff_delay(attempt);
        // 退避期间也响应取消。
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if state.control.state() == file::ControlState::Cancelled {
                return Err(SftpError::Cancelled);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// 目标已存在时怎么处理。
    async fn resolve_conflict(
        &self,
        state: &Arc<JobState>,
        source: &Arc<dyn FileEndpoint>,
        target: &Arc<dyn FileEndpoint>,
        planned: &plan::PlannedFile,
        target_path: &str,
    ) -> Result<ConflictOutcome, SftpError> {
        // 暂停之后重进这个文件：之前已经问过（也答过），接着断点传，不再问。
        let remembered_here = self
            .lock_progress(state)
            .conflicted_targets
            .get(&planned.target)
            .cloned();
        match remembered_here {
            Some(RememberedConflict::Overwrite) => {
                return Ok(ConflictOutcome::Proceed {
                    resume: true,
                    overwrite: true,
                })
            }
            // 暂停前选了改名：接着往那个名字传，不能回到原目标上（那等于没经同意就覆盖）。
            Some(RememberedConflict::Renamed(name)) => {
                return Ok(ConflictOutcome::Renamed(name, None))
            }
            None => {}
        }
        let Some(existing_size) = target.existing_size().await? else {
            // 没有冲突。临时文件名里带着本任务的标识，能找到的临时文件只可能是这个任务暂停 /
            // 断线前自己留下的：接着它传（尾部对账把关，对不上从头来）。以前这里返回
            // `resume: false`，把暂停前传好的部分整个丢掉、从 0 重传（R-9 F14）。
            // 没问过用户，所以不授权覆盖。
            return Ok(ConflictOutcome::Proceed {
                resume: true,
                overwrite: false,
            });
        };
        let source_size = source.size().await?;
        // 修改时间给冲突对话框用（两端各拿一次；拿不到就是 None，不拦着）。
        let source_mtime = source.attributes().await.ok().and_then(|(mtime, _)| mtime);
        let target_mtime = target.existing_mtime().await.unwrap_or(None);
        // 续传只在"目标比源小"的时候有意义（不然没有要补的部分）。
        let can_resume = source_size.is_some_and(|source_size| existing_size < source_size);
        // 先看任务自己记的，再看批次的（同一次提交里别的任务勾过"全部应用"）。
        let batch = state
            .batch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let remembered = self
            .lock_progress(state)
            .conflict_all
            .or_else(|| batch.as_ref().and_then(|batch| batch.action()));
        let decision = match remembered {
            Some(action) => ConflictDecision {
                action,
                apply_to_all: true,
                rename_to: None,
            },
            None => {
                // 等用户回答期间让界面看得出是在等人（回答最长等 2 分钟，不能显示成「准备中」）。
                self.set_state(state, TransferState::WaitingConflict);
                self.emit(state, true);
                let suggested_target = self.suggest_free_name(state, planned, target_path).await;
                let decision = self
                    .resolver
                    .decide(ConflictInfo {
                        job: state.id,
                        direction: state.job.direction,
                        source: planned.source.clone(),
                        target: target_path.to_owned(),
                        source_size,
                        target_size: Some(existing_size),
                        source_mtime,
                        target_mtime,
                        can_resume,
                        suggested_target,
                        batch: batch.as_ref().map(|batch| batch.id()),
                    })
                    .await;
                self.set_state(state, TransferState::Running);
                self.emit(state, true);
                decision
            }
        };
        if decision.apply_to_all {
            self.lock_progress(state).conflict_all = Some(decision.action);
            if let Some(batch) = &batch {
                batch.remember(decision.action);
            }
        }
        let outcome = self
            .apply_conflict_decision(state, planned, &decision, can_resume, target_path)
            .await?;
        let remembered = match &outcome {
            ConflictOutcome::Proceed { .. } => Some(RememberedConflict::Overwrite),
            ConflictOutcome::Renamed(name, _) => Some(RememberedConflict::Renamed(name.clone())),
            ConflictOutcome::Skip => None,
        };
        if let Some(remembered) = remembered {
            self.lock_progress(state)
                .conflicted_targets
                .insert(planned.target.clone(), remembered);
        }
        Ok(outcome)
    }

    /// 把用户的冲突选择变成这个文件接下来怎么做。
    async fn apply_conflict_decision(
        &self,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        decision: &ConflictDecision,
        can_resume: bool,
        target_path: &str,
    ) -> Result<ConflictOutcome, SftpError> {
        Ok(match decision.action {
            ConflictAction::Overwrite => ConflictOutcome::Proceed {
                resume: false,
                overwrite: true,
            },
            ConflictAction::Skip => {
                self.push_note(state, format!("目标已存在，跳过 {target_path}"));
                ConflictOutcome::Skip
            }
            ConflictAction::Resume => {
                if !can_resume {
                    self.push_note(
                        state,
                        format!("{target_path} 不能续传（目标不比源小），改成覆盖"),
                    );
                }
                ConflictOutcome::Proceed {
                    resume: true,
                    overwrite: true,
                }
            }
            ConflictAction::Rename => {
                // 目标换个名字：用户给过就用用户的，没给就 `name (2).ext`。
                // 对话框传回来的只是一个**文件名**（不是路径）：要接在原目标所在的
                // 目录后面，不然会写到相对路径上（远端是家目录、本地是工作目录）。
                // 带路径分隔符的名字不接受，退回自动名字。
                let manual = decision
                    .rename_to
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
                    .map(|name| with_file_name(target_path, name));
                let (renamed, guard) = match manual {
                    Some(path) => self.claim_manual_name(state, planned, path).await?,
                    None => self.pick_free_name(state, planned, target_path).await?,
                };
                self.push_note(state, format!("目标已存在，改成 {renamed}"));
                ConflictOutcome::Renamed(renamed, Some(guard))
            }
        })
    }

    /// 第 `n` 个（从 1 数起）自动名字：上传 / 下载是 `名字 (2)`、`名字 (3)`…；
    /// 复制粘贴是 `名字 - 副本`、`名字 - 副本 2`…（和粘贴时的习惯一致）。
    fn auto_candidate(direction: Direction, target_path: &str, n: usize) -> String {
        match direction {
            Direction::RemoteCopy | Direction::LocalCopy => copy_path(target_path, n),
            Direction::Upload | Direction::Download => renamed_path(target_path, n + 1),
        }
    }

    /// 目标端这个路径上是不是已经有东西（文件、目录、链接都算占用）。
    async fn path_taken(
        &self,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        path: &str,
    ) -> Result<bool, SftpError> {
        let pair = self
            .endpoints
            .pair(
                state.job.direction,
                state.job.peer.as_deref(),
                &planned.source,
                path,
                &self.owner_of(state),
            )
            .await?;
        Ok(pair.target.existing_size().await?.is_some())
    }

    /// 自动改名：往后找第一个没被占用的名字，**同时把它的目标锁抢下来**一路带到传输。
    ///
    /// 为什么选名字和上锁要在一起：同一时刻两个任务都想改名到同一个空名字时，光看「现在
    /// 有没有这个文件」是看不出来的（先到的那个文件还没写出来）。谁先抢到锁谁用，后到的
    /// 看到 `TargetBusy` 就当这个名字被占了，接着往后找；此时原目标的锁还在手里，所以
    /// 选名到开写之间没有空隙。
    async fn pick_free_name(
        &self,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        target_path: &str,
    ) -> Result<(String, lock::TargetGuard), SftpError> {
        for n in 1..=MAX_RENAME_TRIES {
            let candidate = Self::auto_candidate(state.job.direction, target_path, n);
            let guard = match self.lock_target(state.job.direction, &candidate).await {
                Ok(guard) => guard,
                // 别的任务正要写这个名字。
                Err(SftpError::TargetBusy { .. }) => continue,
                Err(err) => return Err(err),
            };
            if self.path_taken(state, planned, &candidate).await? {
                continue;
            }
            return Ok((candidate, guard));
        }
        Err(SftpError::NoFreeName {
            path: target_path.to_owned(),
            tried: MAX_RENAME_TRIES,
        })
    }

    /// 用户在对话框里手动输入的新名字：必须没被占用（也不能是别的任务正在写的），
    /// 否则这个文件按失败处理，不静默覆盖、也不悄悄换成别的名字。
    async fn claim_manual_name(
        &self,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        path: String,
    ) -> Result<(String, lock::TargetGuard), SftpError> {
        let exists = |path: &str| SftpError::RenameTargetExists {
            name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned(),
        };
        // 先查再锁：新名字和原目标其实是同一个文件（只是大小写不同的写法）时，
        // 锁会说「别的任务在写」，实际该告诉用户「新名字也已存在」。
        if self.path_taken(state, planned, &path).await? {
            return Err(exists(&path));
        }
        let guard = self.lock_target(state.job.direction, &path).await?;
        // 拿到锁之后再看一眼：刚才查完到上锁之间，别的任务可能刚好写完这个名字。
        if self.path_taken(state, planned, &path).await? {
            return Err(exists(&path));
        }
        Ok((path, guard))
    }

    /// 对话框里预填的名字：第一个没被占用的自动名字。只是建议，不上锁；检查失败
    /// 或试到上限就退回固定的第一个名字（不能因为这个卡住提问）。
    async fn suggest_free_name(
        &self,
        state: &Arc<JobState>,
        planned: &plan::PlannedFile,
        target_path: &str,
    ) -> String {
        let direction = state.job.direction;
        for n in 1..=MAX_RENAME_TRIES {
            let candidate = Self::auto_candidate(direction, target_path, n);
            match self.path_taken(state, planned, &candidate).await {
                Ok(false) => return candidate,
                Ok(true) => {}
                Err(_) => break,
            }
        }
        Self::auto_candidate(direction, target_path, 1)
    }

    fn finish_with_error(&self, state: &Arc<JobState>, err: SftpError) {
        let text = err.to_string();
        self.set_state(state, TransferState::Failed(text));
        self.emit(state, true);
    }

    fn set_state(&self, state: &Arc<JobState>, next: TransferState) {
        let mut current = state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_terminal() {
            return;
        }
        *current = next;
    }

    fn state_of(&self, state: &Arc<JobState>) -> TransferState {
        state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn lock_progress<'a>(
        &self,
        state: &'a Arc<JobState>,
    ) -> std::sync::MutexGuard<'a, JobProgress> {
        state
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_note(&self, state: &Arc<JobState>, note: String) {
        let mut progress = self.lock_progress(state);
        if !progress.notes.contains(&note) {
            progress.notes.push(note);
        }
    }

    /// 发一条进度事件（`force = false` 时按 `progress_interval` 节流）。
    fn emit(&self, state: &Arc<JobState>, force: bool) {
        let now = Instant::now();
        let report = {
            let mut progress = self.lock_progress(state);
            let current = self.state_of(state);
            let throttled = progress
                .last_emit
                .is_some_and(|last| now.duration_since(last) < self.options.progress_interval);
            if throttled && !force && !current.is_terminal() {
                return;
            }
            progress.last_emit = Some(now);
            // 速度：拿最近一次采样算。
            match progress.sample {
                Some((at, bytes)) => {
                    let elapsed = now.duration_since(at).as_secs_f64();
                    if elapsed > 0.05 {
                        let delta = progress.done_bytes.saturating_sub(bytes) as f64;
                        let instant = (delta / elapsed) as u64;
                        // 平滑一下：新旧各一半（网络速度本来就有抖动）。
                        progress.speed_bps = (progress.speed_bps + instant) / 2;
                        progress.sample = Some((now, progress.done_bytes));
                    }
                }
                None => progress.sample = Some((now, progress.done_bytes)),
            }
            let remaining = progress.total_bytes.saturating_sub(progress.done_bytes);
            let eta_secs = remaining.checked_div(progress.speed_bps);
            ProgressReport {
                id: state.id,
                direction: state.job.direction,
                state: current.clone(),
                total_files: progress.total_files,
                done_files: progress.done_files,
                total_bytes: progress.total_bytes,
                done_bytes: progress.done_bytes,
                speed_bps: progress.speed_bps,
                eta_secs,
                current: progress.current.clone(),
                notes: progress.notes.clone(),
            }
        };
        let event = if report.state.is_terminal() {
            TransferEvent::Finished(report)
        } else {
            TransferEvent::Progress(report)
        };
        let _ = self.events.send(event);
    }

    // ---------- 对外操作 ----------

    /// 暂停一个任务（当前文件传完这一批就停，临时文件留着）。
    pub fn pause(&self, id: TransferId) -> bool {
        let Some(state) = self.job(id) else {
            return false;
        };
        state.control.pause();
        true
    }

    /// 继续（暂停或等待重连之后）。
    pub fn resume(&self, id: TransferId) -> bool {
        let Some(state) = self.job(id) else {
            return false;
        };
        state.control.resume();
        state.resume_signal.notify_waiters();
        true
    }

    /// 取消（已删的不回滚；临时文件删掉）。
    pub fn cancel(&self, id: TransferId) -> bool {
        let Some(state) = self.job(id) else {
            return false;
        };
        state.control.cancel();
        state.resume_signal.notify_waiters();
        true
    }

    /// 现在这个任务的状态（诊断 / 测试）。
    pub fn state(&self, id: TransferId) -> Option<TransferState> {
        self.job(id).map(|state| self.state_of(&state))
    }

    /// 任务的最新报告（诊断 / 测试）。
    pub fn report(&self, id: TransferId) -> Option<ProgressReport> {
        self.job(id).map(|state| self.report_of(&state))
    }

    fn job(&self, id: TransferId) -> Option<Arc<JobState>> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }
}

/// 一个文件传完之后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    Done,
    Skipped,
    Paused,
}

/// 冲突处理之后的走向。
enum ConflictOutcome {
    /// `overwrite`：用户同意过替换已有的目标（只有这时提交才用覆盖式改名）。
    Proceed {
        resume: bool,
        overwrite: bool,
    },
    Skip,
    /// 改成这个名字。带着的锁是新名字的目标锁（选名字的时候就抢下来了）；
    /// 暂停后重进时记住的旧名字没有锁（`None`），照常在下一轮重新拿。
    Renamed(String, Option<lock::TargetGuard>),
}

/// 自动改名最多往后试这么多个名字，再多就按失败处理（不能无限循环）。
const MAX_RENAME_TRIES: usize = 1000;

/// 退避：200ms、400ms、800ms…（上限 5 秒）。
pub fn backoff_delay(attempt: u32) -> Duration {
    let millis = 200u64.saturating_mul(1u64 << attempt.min(5));
    Duration::from_millis(millis.min(5_000))
}

/// 目标改名：`/a/photo.jpg` + 2 → `/a/photo (2).jpg`。
///
/// 目录分隔符 `/` 和 `\` 都认（本地 Windows 路径是反斜杠）：只认 `/` 的话，
/// `C:\my.dir\file` 这种"目录名里有点、文件没扩展名"的路径会被改在目录名上。
pub fn renamed_path(path: &str, index: usize) -> String {
    let name_start = path.rfind(['/', '\\']).map(|slash| slash + 1).unwrap_or(0);
    match path.rfind('.') {
        Some(dot) if dot > name_start => {
            format!("{} ({index}){}", &path[..dot], &path[dot..])
        }
        _ => format!("{path} ({index})"),
    }
}

/// 复制粘贴的"副本"名：`/a/b.txt` → `/a/b - 副本.txt`；`index` 大于 1 时是 `b - 副本 2.txt`。
pub fn copy_path(path: &str, index: usize) -> String {
    let name_start = path.rfind(['/', '\\']).map(|slash| slash + 1).unwrap_or(0);
    let suffix = if index <= 1 {
        " - 副本".to_owned()
    } else {
        format!(" - 副本 {index}")
    };
    match path.rfind('.') {
        Some(dot) if dot > name_start => format!("{}{suffix}{}", &path[..dot], &path[dot..]),
        _ => format!("{path}{suffix}"),
    }
}

/// 把路径的最后一段换成 `name`：`/a/b.txt` + `c.txt` → `/a/c.txt`
/// （`C:\a\b.txt` 同理）。冲突对话框里用户改的新名字只是个文件名。
pub fn with_file_name(path: &str, name: &str) -> String {
    let name_start = path.rfind(['/', '\\']).map(|slash| slash + 1).unwrap_or(0);
    format!("{}{name}", &path[..name_start])
}

#[cfg(test)]
mod tests;
