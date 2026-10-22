//! AI 工具桥服务端（卡片 AI-1）。
//!
//! 职责：令牌握手、scope/会话归属校验、危险判断、审批（手动/自动）、
//! 在宿主连接上执行、写审计。协议见 [`crate::ai::protocol`]。
//!
//! 审批和会话执行通过 trait 交给调用方（`src-tauri` 或测试），core 不依赖 Tauri。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify, Semaphore};

mod file_tools;
pub mod terminal_write;

use crate::ai::files::{FileDiff, RemoteFs};
use crate::ai::protocol::{
    clamp_max_lines, clamp_timeout_secs, ErrorBody, ExecLocation, ReadTerminalParams,
    RememberHostParams, RequestEnvelope, ResponseEnvelope, RunCommandParams, RunCommandResponse,
    TargetSummary, TokenLine, CAPABILITY_BYTES, METHOD_LIST_DIR, METHOD_LIST_TARGETS,
    METHOD_READ_FILE, METHOD_READ_TERMINAL, METHOD_REMEMBER_HOST, METHOD_RUN_COMMAND, METHOD_STAT,
    METHOD_WRITE_FILE,
};
use crate::ai::safety::{assess, DangerLevel};
use crate::ssh::{BoxFuture, ExecCapture, EXEC_MAX_STREAM_BYTES};
use crate::store::audit::NewAiAudit;
use crate::store::db::Database;

/// 默认审批等待：5 分钟，超时按拒绝处理。
pub const DEFAULT_APPROVAL_WAIT: Duration = Duration::from_secs(5 * 60);
/// 单行消息上限（1 MiB），防止恶意超长行吃内存。
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// 一条连接上响应写回队列的长度（唯一写者任务从这里取，响应不会互相穿插）。
const WRITE_QUEUE_DEPTH: usize = 64;
/// 握手超时：连上后 10 秒内不发令牌就断开。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 握手行上限：令牌行只有几十个字节，没通过认证的连接不许让我们攒大块内存（R-9 F15）。
const HANDSHAKE_MAX_BYTES: usize = 4 * 1024;
/// 同时处在握手阶段（还没交出连接层令牌）的连接数。认证过的连接不受它限制：claude 每个对话
/// 一个 zssh-mcp，对话多了连接就多，不能因此拒掉正常的连接。
const MAX_PENDING_HANDSHAKES: usize = 16;
/// 一条连接上同时在处理的请求数。到上限就先不读下一条（背压），不会无限开任务。
const MAX_IN_FLIGHT_PER_CONNECTION: usize = 32;
/// 整个工具桥同时在处理的请求总数（所有连接合计，含连接断开后还在收尾的）。每条连接各有
/// 上限还不够：断开重连就能拿到新的额度，旧请求却还在跑（R-9 F15）。正常使用远到不了这个数。
const MAX_IN_FLIGHT_TOTAL: usize = 128;
/// 目标不存在或不属于当前作用范围。
pub(crate) const SCOPE_TARGET_ERROR: &str = "目标不存在或不属于本次对话的作用范围";

/// 执行模式。设置项 `ai_exec_mode`：`"auto"` 全放行，`"readonly"` 只读命令自动放行、
/// 其余要确认，其他值（默认）每条都确认。旧版本不认识 `"readonly"`，会按手动处理（往安全的方向退）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    Manual,
    /// 只读命令（[`crate::ai::readonly::is_read_only`]）自动放行；写文件、其他命令要确认。
    ReadOnly,
    Auto,
}

impl ExecMode {
    /// 从设置项原始值解析；没设置或值不认识都按手动。
    pub fn from_setting(raw: Option<&str>) -> ExecMode {
        match raw {
            Some("auto") => ExecMode::Auto,
            Some("readonly") => ExecMode::ReadOnly,
            _ => ExecMode::Manual,
        }
    }
}

/// 指令的最终处置（写进审计的 decision 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecDecision {
    /// 自动模式直接放行的指令（含危险命令；危险等级只进审计和工具结果）。
    Auto,
    /// 人工确认通过。
    Allowed,
    /// 人工拒绝（含 scope 销毁导致的拒绝）。
    Denied,
    /// 审批等待超时。
    Timeout,
}

impl ExecDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecDecision::Auto => "auto",
            ExecDecision::Allowed => "allowed",
            ExecDecision::Denied => "denied",
            ExecDecision::Timeout => "timeout",
        }
    }
}

/// 人工确认的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allowed,
    Denied,
    Timeout,
}

/// 送给确认界面看的审批请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub scope: String,
    pub host_label: String,
    /// `run_command` 是命令全文；`write_file` 是 `write_file <路径>`（卡片上用 `file` 里的细节）。
    pub command: String,
    pub danger: DangerLevel,
    pub reasons: Vec<String>,
    /// `write_file` 的细节（路径、差异）；`run_command` 为 `None`。
    pub file: Option<FileApproval>,
}

/// 确认卡片里 `write_file` 要给用户审的内容：写哪个文件、和现有文件的差异。
/// 差异只用于这张确认卡片，审计不会记录它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileApproval {
    pub path: String,
    pub is_new: bool,
    pub new_bytes: usize,
    pub diff: FileDiff,
}

/// 审批通道：`src-tauri` 走事件，测试走桩。工具桥负责超时和 scope 销毁的取消。
pub trait Approver: Send + Sync {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision>;
}

/// 终端执行的请求（交给宿主在用户终端里跑，AI-2c）。
#[derive(Debug, Clone)]
pub struct TerminalExecRequest {
    /// 目标（会话）ID。
    pub target_id: String,
    /// 这个对话的工具桥 scope id。
    ///
    /// 宿主用它在终端执行登记表里记一条"哪个对话在哪个终端跑着哪条命令"
    /// （AI-2d 1.4）：停止按钮只在这个对话确实有命令在终端里跑时才发 Ctrl+C。
    pub scope_id: String,
    /// 用户要看到的命令原文（AI-2d：包装脚本不显示，显示这一份合成回显）。
    pub command: String,
    /// 已经包装好、带结束标记的整段文本（调用方负责组装）。
    pub payload: String,
    /// 结束标记（用来在终端输出里找退出码）。
    pub marker: String,
    /// 超时（超时后命令不中断，只是把当前状态返回给 AI）。
    pub wait: Duration,
    /// 宿主往终端写命令之前要再 [`ExecGate::check`] 一次：抢占终端之后还要等会话就绪，
    /// 这期间请求可能已经失效。失效时不写、释放终端占位、返回错误。
    pub gate: ExecGate,
}

/// 终端执行的结果。
#[derive(Debug, Clone)]
pub struct TerminalExecCapture {
    /// 命令输出（已去 ANSI、截取到结束标记之前）。
    pub output: String,
    /// 退出码；还在跑（超时）时为 `None`。
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// 超时：命令仍在终端里运行。
    pub timed_out: bool,
}

/// 执行位置的决定：能就地跑就终端，否则后台 + 原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecPlacement {
    Terminal,
    Background { reason: String },
}

/// 会话执行：在会话所在的连接上开 exec 通道 / 读环形缓冲 / 能否在终端里跑。
pub trait BridgeHost: Send + Sync {
    fn exec<'a>(
        &'a self,
        session_id: &'a str,
        command: &'a str,
        wait: Duration,
        max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>>;

    /// 同 [`BridgeHost::exec`]，但受 `gate` 约束（R-9 F11、F12）：
    ///
    /// - 命令发出去之前（比如自动连接完成之后、执行通道开好之后）要 [`ExecGate::check`]，失效
    ///   就不发——ExecGate 实现了 [`crate::ssh::connection::ExecGuard`]，直接交给
    ///   [`crate::ssh::Connection::exec_capture_guarded`]，它在发 exec 的边界上核对（审计 C04）；
    /// - 发出去之后 [`ExecGate::cancel_flag`] 置位（用户点了停止）要尽快结束（关闭执行通道），
    ///   返回的 `cancelled = true`。
    ///
    /// 默认实现只在开始前检查一次，之后等命令自己结束（测试替身用）。
    fn exec_cancellable<'a>(
        &'a self,
        session_id: &'a str,
        command: &'a str,
        wait: Duration,
        max_stream_bytes: usize,
        gate: &'a ExecGate,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async move {
            gate.check()?;
            self.exec(session_id, command, wait, max_stream_bytes).await
        })
    }

    fn read_terminal<'a>(
        &'a self,
        session_id: &'a str,
        max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;

    /// 这个目标现在能不能在用户的终端里执行（终端空闲且是可识别的 POSIX shell）。
    ///
    /// 返回 `Terminal` 就地执行，返回 `Background { reason }` 走后台通道（原因写进
    /// 工具卡片和审计）。默认实现保守地一律走后台（宿主没实现就地执行时）。
    ///
    /// 返回 `Terminal` 表示宿主**已经抢占**了这个终端：实现方要在
    /// [`BridgeHost::run_in_terminal`] 结束时释放（拿到结束标记 / 超时 / 会话断开），
    /// 抢占失败的并发命令由这里直接降级到后台，不会两条命令叠在同一个终端里。
    fn exec_placement<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
        let _ = session_id;
        Box::pin(async {
            ExecPlacement::Background {
                reason: "宿主没提供终端执行能力".to_string(),
            }
        })
    }

    /// 在用户的终端里执行（AI-2c）。只有在 [`BridgeHost::exec_placement`] 返回
    /// `Terminal` 时才会被调用；实现方负责在结束时释放 [`BridgeHost::exec_placement`]
    /// 抢占的终端（成功、超时、出错都要放）。
    fn run_in_terminal<'a>(
        &'a self,
        request: TerminalExecRequest,
    ) -> BoxFuture<'a, Result<TerminalExecCapture, String>> {
        let _ = request;
        Box::pin(async { Err("宿主没提供终端执行能力".to_string()) })
    }

    /// [`BridgeHost::exec_placement`] 抢占了终端、工具桥随后又决定不执行（请求在这期间失效）时，
    /// 把占位还回去。默认空操作（没实现就地执行的宿主也没有占位）。
    fn release_terminal<'a>(&'a self, target_id: &'a str) -> BoxFuture<'a, ()> {
        let _ = target_id;
        Box::pin(async {})
    }

    /// 文件工具（M3-4）：这个目标服务器上的文件系统（开一条 SFTP 通道）。
    /// 宿主没实现就报错（文件工具不可用，不影响别的工具）。
    fn remote_fs<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        let _ = target_id;
        Box::pin(async { Err("宿主没提供文件工具能力".to_string()) })
    }

    /// 让 AI 停止这个对话正在终端里跑的 AI 命令：向对应终端发 Ctrl+C。
    ///
    /// 只有这个 scope **确实**有一条 AI 命令在终端里跑才会发；没有就什么都不做
    /// （用户自己可能正在那个终端里跑东西，乱发 Ctrl+C 会打断它）。默认空操作。
    fn interrupt_terminal<'a>(&'a self, scope_id: &'a str) -> BoxFuture<'a, ()> {
        let _ = scope_id;
        Box::pin(async {})
    }

    /// 主机长期记忆（[`crate::ai::host_memory`]）：读这个目标对应主机的笔记。
    /// 快速连接这类不在主机库里的目标没有记忆，返回 `Ok(None)`。默认没有记忆。
    fn read_memory<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        let _ = target_id;
        Box::pin(async { Ok(None) })
    }

    /// 存这个目标对应主机的笔记（已经清理过；空串 = 清空）。
    fn write_memory<'a>(
        &'a self,
        target_id: &'a str,
        notes: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target_id, notes);
        Box::pin(async { Err("这个目标不在主机库里，不能保存记忆".to_string()) })
    }

    /// `write_file` 每个文件保留几份备份（设置项 `ai_write_backup_keep`）。
    /// 默认 5 份；宿主可以按当前设置实现。
    fn write_backup_keep(&self) -> usize {
        crate::store::settings::AI_WRITE_BACKUP_KEEP_DEFAULT
    }
}

/// 执行模式来源（每次命令都重新读设置）。
pub trait ExecModeProvider: Send + Sync {
    fn mode(&self) -> ExecMode;
}

/// 一条待写审计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub at: i64,
    pub scope: String,
    pub host_label: String,
    pub command: String,
    pub danger: DangerLevel,
    pub decision: ExecDecision,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 执行位置（AI-2c）：终端 / 后台。
    pub location: Option<ExecLocation>,
    /// 降级到后台的原因（AI-2c）。
    pub fallback_reason: Option<String>,
    /// 工具名（M3-4）：`run_command` / `list_dir` / `read_file` / `write_file` / `stat`。
    pub tool: String,
    /// 文件工具操作的路径（M3-4）；`run_command` 为 `None`。
    pub path: Option<String>,
    /// 文件工具涉及的字节数：读 = 返回的字节数，写 = 写入的字节数。**只记数量，不记内容。**
    pub bytes: Option<i64>,
    /// `write_file` 覆盖时在服务器上留的备份路径。
    pub backup_path: Option<String>,
}

/// 审计落盘。写失败只记日志，不影响命令执行。
pub trait AuditLog: Send + Sync {
    fn record(&self, entry: AuditEntry);
}

impl AuditLog for Database {
    fn record(&self, entry: AuditEntry) {
        let result = self.ai_audit_insert(NewAiAudit {
            at: entry.at,
            scope: &entry.scope,
            host_label: &entry.host_label,
            command: &entry.command,
            danger: entry.danger.as_str(),
            decision: entry.decision.as_str(),
            exit_code: entry.exit_code,
            duration_ms: entry.duration_ms,
            location: entry.location.map(ExecLocation::as_str),
            fallback_reason: entry.fallback_reason.as_deref(),
            tool: &entry.tool,
            path: entry.path.as_deref(),
            bytes: entry.bytes,
            backup_path: entry.backup_path.as_deref(),
        });
        if let Err(err) = result {
            tracing::warn!(error = %err, "AI 审计写入失败");
        }
    }
}

/// [`read_bounded_line`] 的结果。
#[derive(Debug, PartialEq, Eq)]
enum BoundedLine {
    /// 一整行（不含换行符；最后一行没有换行也算）。
    Line(String),
    /// 对端关了，没有更多数据。
    Eof,
    /// 超过上限还没看到换行：调用方应断开连接。
    TooLong,
}

/// 读一行，最多读 `max` 个字节：超出时不再继续往内存里攒。
async fn read_bounded_line<R>(reader: &mut R, max: usize) -> std::io::Result<BoundedLine>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    let mut buf = Vec::new();
    // 多读一个字节：正好 `max` 字节的内容加上换行符也要能读完整。
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let read = (&mut *reader)
        .take(limit)
        .read_until(b'\n', &mut buf)
        .await?;
    if read == 0 {
        return Ok(BoundedLine::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > max {
        return Ok(BoundedLine::TooLong);
    }
    match String::from_utf8(buf) {
        Ok(line) => Ok(BoundedLine::Line(line)),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "请求不是 UTF-8",
        )),
    }
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 恒定时间比较两个令牌（R-8 第 11 节）。
///
/// 用 `==` 比较字符串时，遇到第一个不同的字节就返回，攻击者能按响应时间逐字节猜出
/// 令牌。这里逐字节异或累加，长度不同也把两边都走完再判断，不提前返回。
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let x = left.get(index).copied().unwrap_or(0);
        let y = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

/// 一个对话的作用范围：独立能力密钥 + 目标列表。
///
/// scope id 是 16 位十六进制（审计和审批事件里用它），能力密钥是
/// [`CAPABILITY_BYTES`] 字节（32 位十六进制），工具调用必须带上它。
pub struct Scope {
    pub(crate) id: String,
    capability: String,
    targets: Mutex<BTreeMap<String, TargetSummary>>,
    destroyed: AtomicBool,
    /// 停止按钮按了几次。请求开始时记下它，之后变了就说明用户叫停了这个回合（R-9 F12）。
    stop_epoch: AtomicU64,
    /// 每个目标被移出过几次。请求开始时记下它，审批之后变了（哪怕又被加回来）就不能再按
    /// 旧的批准去执行（R-9 F11）。
    removals: Mutex<HashMap<String, u64>>,
    /// 销毁、停止、移出目标都通知它：等审批 / 等执行的请求据此提前结束。
    notify: Notify,
}

/// 一条请求开始时的作用范围状态，用来判断它后来有没有失效（停止、目标移出、作用范围销毁）。
#[derive(Debug, Clone)]
pub(crate) struct RequestTicket {
    target_id: String,
    stop_epoch: u64,
    removals: u64,
}

/// 一条已经放行的请求交给宿主的"闸"：宿主在**真正产生副作用之前**（往终端写命令、发 SSH exec、
/// 替换文件）调 [`ExecGate::check`] 再确认一次请求还有效（R-9 F11、F12）。
///
/// 放行之后到真正执行之间可能还要等很久（第一次在终端执行要先探测远端 shell、未连上的主机要
/// 先自动连接）：这期间用户点了停止或把目标移出对话，旧请求就不能再执行。
#[derive(Clone)]
pub struct ExecGate {
    scope: Arc<Scope>,
    ticket: RequestTicket,
    /// 用户点了停止时由工具桥置位：已经发出去的后台命令看到它就关闭执行通道。
    cancel: Arc<AtomicBool>,
}

impl ExecGate {
    pub(crate) fn new(scope: &Arc<Scope>, ticket: &RequestTicket) -> ExecGate {
        ExecGate {
            scope: Arc::clone(scope),
            ticket: ticket.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 请求还能执行吗；不能时是给模型的原因。
    pub fn check(&self) -> Result<(), String> {
        match self.scope.ticket_refusal(&self.ticket) {
            Some(reason) => Err(reason.to_string()),
            None if self.cancel.load(Ordering::SeqCst) => {
                Err("用户点了停止，这次操作已取消".to_string())
            }
            None => Ok(()),
        }
    }

    /// 停止标志：用户点了停止时由 `run_background` 置位（测试替身和只认原子标志的调用方用）。
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }
}

/// 后台执行：发 exec 的边界在 ssh 层（[`crate::ssh::Connection::exec_capture_guarded`]），
/// 它拿这个核对完整的有效性——只传停止标志的话，开通道期间目标被移出就拦不住（审计 C04）。
impl crate::ssh::connection::ExecGuard for ExecGate {
    fn check(&self) -> Result<(), String> {
        ExecGate::check(self)
    }

    fn invalidated(&self) -> BoxFuture<'_, String> {
        Box::pin(async move { self.scope.wait_invalidated(&self.ticket).await.to_owned() })
    }

    fn stop_requested(&self) -> bool {
        // 不只看取消标志：标志由 run_background 在收到停止通知之后才置位，中间有一拍。
        self.cancel.load(Ordering::SeqCst) || self.scope.stop_refusal(&self.ticket).is_some()
    }
}

impl std::fmt::Debug for ExecGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecGate")
            .field("scope", &self.scope.id)
            .field("ticket", &self.ticket)
            .field("cancelled", &self.cancel.load(Ordering::SeqCst))
            .finish()
    }
}

impl Scope {
    fn add_target(&self, summary: TargetSummary) {
        if let Ok(mut targets) = self.targets.lock() {
            targets.insert(summary.id.clone(), summary);
        }
    }

    fn remove_target(&self, target_id: &str) {
        if let Ok(mut targets) = self.targets.lock() {
            targets.remove(target_id);
        }
        if let Ok(mut removals) = self.removals.lock() {
            *removals.entry(target_id.to_owned()).or_insert(0) += 1;
        }
        self.notify.notify_waiters();
    }

    fn removal_count(&self, target_id: &str) -> u64 {
        self.removals
            .lock()
            .map(|removals| removals.get(target_id).copied().unwrap_or(0))
            .unwrap_or(u64::MAX)
    }

    /// 停止按钮：这一刻之前开始的请求全部作废（等审批的按拒绝结束，后台命令关通道）。
    fn stop(&self) {
        self.stop_epoch.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 记下一条请求开始时的状态。
    pub(crate) fn ticket(&self, target_id: &str) -> RequestTicket {
        RequestTicket {
            target_id: target_id.to_owned(),
            stop_epoch: self.stop_epoch.load(Ordering::SeqCst),
            removals: self.removal_count(target_id),
        }
    }

    /// 请求还能不能继续；不能时返回给模型的原因。
    pub(crate) fn ticket_refusal(&self, ticket: &RequestTicket) -> Option<&'static str> {
        if let Some(reason) = self.stop_refusal(ticket) {
            return Some(reason);
        }
        let still_there = self
            .targets
            .lock()
            .map(|targets| targets.contains_key(&ticket.target_id))
            .unwrap_or(false);
        if !still_there || self.removal_count(&ticket.target_id) != ticket.removals {
            return Some("这台服务器已经被移出这个对话，这次操作已取消");
        }
        None
    }

    /// 只看停止和销毁（不看目标移出）。
    fn stop_refusal(&self, ticket: &RequestTicket) -> Option<&'static str> {
        if self.is_destroyed() {
            return Some("用户拒绝（作用范围已销毁）");
        }
        if self.stop_epoch.load(Ordering::SeqCst) != ticket.stop_epoch {
            return Some("用户点了停止，这次操作已取消");
        }
        None
    }

    /// 等到这条请求失效（停止、目标移出、作用范围销毁）为止，返回原因。可安全被 `select!` 取消。
    /// 等审批时用：卡片对应的目标已经不在对话里，就不该再等人点。
    pub(crate) async fn wait_invalidated(&self, ticket: &RequestTicket) -> &'static str {
        self.wait_for(|| self.ticket_refusal(ticket)).await
    }

    /// 等到用户点停止（或作用范围销毁）为止，返回原因。可安全被 `select!` 取消。
    ///
    /// 执行中的命令只因停止而中断：目标被移出对话（比如切换了终端标签）不打断已经在跑的命令，
    /// 半路关掉执行通道可能比让它跑完更糟（升级到一半的包管理器之类）。
    pub(crate) async fn wait_stopped(&self, ticket: &RequestTicket) -> &'static str {
        self.wait_for(|| self.stop_refusal(ticket)).await
    }

    async fn wait_for(&self, check: impl Fn() -> Option<&'static str>) -> &'static str {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(reason) = check() {
                return reason;
            }
            notified.await;
        }
    }

    fn target_list(&self) -> Vec<TargetSummary> {
        self.targets
            .lock()
            .map(|targets| {
                let mut list: Vec<TargetSummary> = targets.values().cloned().collect();
                // 当前焦点那台排第一。
                list.sort_by_key(|t| !t.current);
                list
            })
            .unwrap_or_default()
    }

    pub(crate) fn summary(&self, target_id: &str) -> Option<TargetSummary> {
        self.targets
            .lock()
            .ok()
            .and_then(|targets| targets.get(target_id).cloned())
    }

    fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    fn destroy(&self) {
        self.destroyed.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// 调用方（`src-tauri` 或测试）持有的作用范围句柄。
///
/// 克隆出来的是同一个 scope（`Arc` 内部共享），所以面板可以把它放进驱动任务、
/// 同时在命令里按会话增减成员。
#[derive(Clone)]
pub struct ScopeHandle {
    registry: Arc<Registry>,
    scope: Arc<Scope>,
}

impl ScopeHandle {
    pub fn scope_id(&self) -> &str {
        &self.scope.id
    }

    /// 这个对话的能力密钥（交给模型的系统提示里带它）。
    pub fn capability(&self) -> &str {
        &self.scope.capability
    }

    /// 把一个目标加进这个作用范围（已经连上的会话，或用户 @ 进来的主机）。
    ///
    /// `current` 标记当前焦点终端那台；`pinned` 标记用户 @ 进来的主机。
    pub fn add_target(
        &self,
        id: impl Into<String>,
        host_label: impl Into<String>,
        username: impl Into<String>,
        status: impl Into<String>,
        current: bool,
        pinned: bool,
    ) {
        self.scope.add_target(TargetSummary {
            id: id.into(),
            host_label: host_label.into(),
            username: username.into(),
            status: status.into(),
            current,
            pinned,
        });
    }

    /// 目标断开 / 被移除后从作用范围里摘掉。
    pub fn remove_target(&self, target_id: &str) {
        self.scope.remove_target(target_id);
    }

    /// 现在目标列表里有哪些（诊断 / 测试用）。
    pub fn targets(&self) -> Vec<TargetSummary> {
        self.scope.target_list()
    }

    /// 销毁作用范围：能力密钥立即失效，等待中的审批按拒绝结束。
    pub fn destroy(&self) {
        self.scope.destroy();
        self.registry.remove(&self.scope.capability);
    }
}

/// 对话作用范围表：按能力密钥索引。
#[derive(Default)]
struct Registry {
    scopes: Mutex<HashMap<String, Arc<Scope>>>,
}

impl Registry {
    fn register(&self, scope: Arc<Scope>) {
        if let Ok(mut scopes) = self.scopes.lock() {
            scopes.insert(scope.capability.clone(), scope);
        }
    }

    fn lookup(&self, capability: &str) -> Option<Arc<Scope>> {
        self.scopes
            .lock()
            .ok()
            .and_then(|scopes| scopes.get(capability).cloned())
    }

    fn find_by_id(&self, scope_id: &str) -> Option<Arc<Scope>> {
        self.scopes
            .lock()
            .ok()
            .and_then(|scopes| scopes.values().find(|scope| scope.id == scope_id).cloned())
    }

    fn remove(&self, capability: &str) {
        if let Ok(mut scopes) = self.scopes.lock() {
            scopes.remove(capability);
        }
    }
}

/// 工具桥服务端。
pub struct Bridge {
    registry: Arc<Registry>,
    /// 连接层令牌：只证明"是 Z-SSH 启动的那个 opencode 进程连过来的"。
    /// 所有对话共用同一个进程，所以只有一个令牌。
    connection_token: String,
    host: Arc<dyn BridgeHost>,
    pub(crate) audit: Arc<dyn AuditLog>,
    approver: Arc<dyn Approver>,
    mode: Arc<dyn ExecModeProvider>,
    approval_wait: Duration,
    /// 全局在途请求名额（[`MAX_IN_FLIGHT_TOTAL`]）。名额跟着请求任务走，连接断开也不提前归还。
    in_flight: Arc<Semaphore>,
}

/// 审批等待的结果（比 [`ApprovalDecision`] 多区分"超时"和"scope 销毁"）。
enum WaitOutcome {
    Decision(ApprovalDecision),
    ApprovalTimeout,
    /// 等审批期间请求失效了（作用范围销毁 / 停止 / 目标移出），带原因。
    Invalidated(&'static str),
}

impl Bridge {
    /// 建一个工具桥。返回时生成连接层令牌（所有对话共用一个开放进程）。
    pub fn new(
        host: Arc<dyn BridgeHost>,
        audit: Arc<dyn AuditLog>,
        approver: Arc<dyn Approver>,
        mode: Arc<dyn ExecModeProvider>,
    ) -> Bridge {
        Bridge {
            registry: Arc::new(Registry::default()),
            connection_token: random_hex(32),
            host,
            audit,
            approver,
            mode,
            approval_wait: DEFAULT_APPROVAL_WAIT,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT_TOTAL)),
        }
    }

    /// 连接层令牌（注入 `zssh-mcp.exe` 的环境变量）。
    pub fn connection_token(&self) -> &str {
        &self.connection_token
    }

    /// 测试用：缩短审批等待。
    pub fn with_approval_wait(mut self, wait: Duration) -> Bridge {
        self.approval_wait = wait;
        self
    }

    /// 新建一个对话作用范围（能力密钥 + 空会话表）。调用方把能力密钥写进该对话的
    /// 系统提示，模型调工具时带上它。
    pub fn create_scope(&self) -> ScopeHandle {
        let scope = Arc::new(Scope {
            id: random_hex(8),
            capability: random_hex(CAPABILITY_BYTES),
            targets: Mutex::new(BTreeMap::new()),
            destroyed: AtomicBool::new(false),
            stop_epoch: AtomicU64::new(0),
            removals: Mutex::new(HashMap::new()),
            notify: Notify::new(),
        });
        self.registry.register(Arc::clone(&scope));
        ScopeHandle {
            registry: Arc::clone(&self.registry),
            scope,
        }
    }

    /// 接受连接，直到 listener 出错。
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        let handshakes = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
        loop {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            // 没认证的连接到上限：直接关掉新连接（本机别的进程反复连、不发令牌也耗不光资源）。
            let Ok(permit) = Arc::clone(&handshakes).try_acquire_owned() else {
                tracing::warn!("工具桥等待握手的连接太多，拒绝新连接");
                drop(stream);
                continue;
            };
            let bridge = Arc::clone(&self);
            tokio::spawn(async move {
                bridge.handle_connection(stream, permit).await;
            });
        }
    }

    /// 一条连接：先连接层令牌握手，然后一条连接上可以有多个请求同时在飞。
    ///
    /// 请求按各自的能力密钥找到对话作用范围（一个进程服务多个对话）。每条请求单独
    /// `spawn` 处理：一条卡在审批上（最长 5 分钟）不会堵住同一条连接上的其它请求
    /// （R-8 第 10 节）。响应按请求里的 `id` 对应，由唯一的写者任务串行写回。
    async fn handle_connection(
        self: &Arc<Self>,
        stream: TcpStream,
        handshake_permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let (read_half, write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // 第一条消息必须是连接层令牌；10 秒不发就断开。
        let line = match tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            read_bounded_line(&mut reader, HANDSHAKE_MAX_BYTES),
        )
        .await
        {
            Ok(Ok(BoundedLine::Line(line))) => line,
            _ => return,
        };
        match serde_json::from_str::<TokenLine>(line.trim()) {
            Ok(token_line) if constant_time_eq(&token_line.token, &self.connection_token) => {}
            // 令牌不对：整条连接直接断开。
            _ => return,
        }
        // 认证通过：不再占握手名额。
        drop(handshake_permit);

        // 响应写回全部走一个写者任务，写之间不会互相穿插。
        let (write_tx, mut write_rx) = mpsc::channel::<String>(WRITE_QUEUE_DEPTH);
        tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(payload) = write_rx.recv().await {
                if write_half.write_all(payload.as_bytes()).await.is_err() {
                    return;
                }
            }
        });

        let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CONNECTION));
        loop {
            // 先读到上限就停：超长行在分配之前就截住，而不是整行读进来再比较长度（R-9 F15）。
            let line = match read_bounded_line(&mut reader, MAX_LINE_BYTES).await {
                Ok(BoundedLine::Line(line)) => line,
                Ok(BoundedLine::Eof | BoundedLine::TooLong) | Err(_) => return,
            };
            // 在途请求到上限：等有请求处理完再读下一条。
            let Ok(permit) = Arc::clone(&in_flight).acquire_owned().await else {
                return;
            };
            let request = match serde_json::from_str::<RequestEnvelope>(line.trim()) {
                Ok(request) => request,
                Err(_) => {
                    // 解析不出 id：仍然回一条（id 0），让客户端知道这条请求没被接受。
                    let response = ResponseEnvelope {
                        id: 0,
                        result: None,
                        error: Some(ErrorBody {
                            message: "无法解析请求".into(),
                        }),
                    };
                    let Ok(mut payload) = serde_json::to_string(&response) else {
                        return;
                    };
                    payload.push('\n');
                    if write_tx.send(payload).await.is_err() {
                        return;
                    }
                    continue;
                }
            };
            // 全局名额满了：直接回"忙"，不排队——排队会把这条连接上的读取一起卡住。
            let Ok(global_permit) = Arc::clone(&self.in_flight).try_acquire_owned() else {
                let response = ResponseEnvelope {
                    id: request.id,
                    result: None,
                    error: Some(ErrorBody {
                        message: "Z-SSH 工具桥正在处理的请求太多，请稍后再试".into(),
                    }),
                };
                let Ok(mut payload) = serde_json::to_string(&response) else {
                    return;
                };
                payload.push('\n');
                if write_tx.send(payload).await.is_err() {
                    return;
                }
                continue;
            };
            let bridge = Arc::clone(self);
            let write_tx = write_tx.clone();
            // 连接断开后已经开始的请求照常跑完，不在中途丢掉：终端占位、临时文件这些收尾都在
            // 请求自己的代码路径里，半路取消会把终端一直标成"忙"。它们各自有审批 / 执行超时，
            // 并且一直占着全局名额，直到真正结束。
            tokio::spawn(async move {
                let _permit = permit;
                let _global_permit = global_permit;
                let id = request.id;
                let response = match bridge.dispatch(&request).await {
                    Ok(result) => ResponseEnvelope {
                        id,
                        result: Some(result),
                        error: None,
                    },
                    Err(message) => ResponseEnvelope {
                        id,
                        result: None,
                        error: Some(ErrorBody { message }),
                    },
                };
                let Ok(mut payload) = serde_json::to_string(&response) else {
                    return;
                };
                payload.push('\n');
                // 接收端已经没了说明连接断了；这条响应不用再写。
                let _ = write_tx.send(payload).await;
            });
        }
    }

    /// 按请求里的能力密钥找到对话作用范围。
    fn scope_for(&self, request: &RequestEnvelope) -> Result<Arc<Scope>, String> {
        if request.capability.is_empty() {
            return Err("缺少对话能力密钥".into());
        }
        let scope = self
            .registry
            .lookup(&request.capability)
            .ok_or_else(|| "对话能力密钥无效或已过期".to_string())?;
        if scope.is_destroyed() {
            return Err("作用范围已销毁".into());
        }
        Ok(scope)
    }

    async fn dispatch(&self, request: &RequestEnvelope) -> Result<serde_json::Value, String> {
        let scope = self.scope_for(request)?;
        match request.method.as_str() {
            METHOD_LIST_TARGETS => self.list_targets(&scope).await,
            METHOD_REMEMBER_HOST => {
                let params: RememberHostParams = serde_json::from_value(request.params.clone())
                    .map_err(|err| format!("参数不正确：{err}"))?;
                let target_id = resolve_target(&scope, params.target_id.as_deref())?;
                let notes = crate::ai::host_memory::clean_memory(&params.notes)?;
                self.host.write_memory(&target_id, &notes).await?;
                Ok(serde_json::json!({
                    "saved_chars": notes.chars().count(),
                    "max_chars": crate::ai::host_memory::MAX_MEMORY_CHARS,
                }))
            }
            METHOD_READ_TERMINAL => {
                let params: ReadTerminalParams = serde_json::from_value(request.params.clone())
                    .map_err(|err| format!("参数不正确：{err}"))?;
                let target_id = resolve_target(&scope, params.target_id.as_deref())?;
                let max_lines = clamp_max_lines(params.max_lines);
                let lines = self.host.read_terminal(&target_id, max_lines).await?;
                Ok(serde_json::json!({ "lines": lines, "max_lines": max_lines }))
            }
            METHOD_RUN_COMMAND => self.run_command(&scope, request.params.clone()).await,
            METHOD_LIST_DIR | METHOD_READ_FILE | METHOD_WRITE_FILE | METHOD_STAT => {
                self.file_tool(&scope, request.method.as_str(), request.params.clone())
                    .await
            }
            other => Err(format!("未知方法：{other}")),
        }
    }

    /// 目标列表，带上每台主机以前记下的笔记。全部放行模式下不带：没有人工确认兜底，
    /// 笔记里被埋的指令会被直接执行。读不到笔记不影响列表。
    async fn list_targets(&self, scope: &Arc<Scope>) -> Result<serde_json::Value, String> {
        let with_memory = self.mode.mode() != ExecMode::Auto;
        let mut targets = Vec::new();
        for target in scope.target_list() {
            let mut entry = serde_json::to_value(&target).map_err(|err| err.to_string())?;
            if with_memory {
                if let Ok(Some(memory)) = self.host.read_memory(&target.id).await {
                    if !memory.trim().is_empty() {
                        entry["memory"] =
                            serde_json::json!(crate::ai::host_memory::memory_for_agent(&memory));
                    }
                }
            }
            targets.push(entry);
        }
        let mut result = serde_json::json!({ "targets": targets });
        if !with_memory {
            result["memory_note"] =
                serde_json::json!("Host notes are not shown in full-access mode.");
        }
        Ok(result)
    }

    async fn run_command(
        &self,
        scope: &Arc<Scope>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let params: RunCommandParams =
            serde_json::from_value(params).map_err(|err| format!("参数不正确：{err}"))?;
        // 命令是当键盘输入打进终端的：控制字符会变成按键（Ctrl+U 删掉包装脚本的前缀、
        // 回车提前提交……），单引号挡不住。审批和两条执行通道之前就拒绝（R-8 第 1 节）。
        crate::ai::command_text::reject_forbidden_control_chars(&params.command)?;
        let target_id = resolve_target(scope, params.target_id.as_deref())?;
        let summary = scope
            .summary(&target_id)
            .ok_or_else(|| SCOPE_TARGET_ERROR.to_string())?;
        let ticket = scope.ticket(&target_id);
        let verdict = assess(&params.command);

        // 手动模式每条命令都要批；自动模式全部直接执行，危险判断只用于标记和审计。
        let (decision, refusal) = self
            .decide(
                scope,
                &ticket,
                ApprovalRequest {
                    scope: scope.id.clone(),
                    host_label: summary.host_label.clone(),
                    command: params.command.clone(),
                    danger: verdict.level,
                    reasons: verdict.reasons.clone(),
                    file: None,
                },
            )
            .await;

        if let Some(message) = refusal {
            self.record_audit(
                scope,
                &summary,
                &params.command,
                &verdict,
                decision,
                None,
                None,
                None,
                None,
            );
            return Err(message);
        }

        let wait = clamp_timeout_secs(params.timeout_sec);
        // 放行之后到真正执行之间还可能等很久（探测 shell、自动连接）：执行前靠它再核对一次。
        let gate = ExecGate::new(scope, &ticket);

        // 决定执行位置：终端空闲就就地跑，否则降级到后台（AI-2c）。
        // 命令里有 Tab 时不走终端：行编辑器会把它当补全键，实际执行的命令会变（R-7 第 0 节）。
        let (location, fallback_reason) = if crate::ai::command_text::contains_tab(&params.command)
        {
            (
                ExecLocation::Background,
                Some(crate::ai::command_text::TAB_FALLBACK_REASON.to_string()),
            )
        } else {
            match self.host.exec_placement(&target_id).await {
                ExecPlacement::Terminal => (ExecLocation::Terminal, None),
                ExecPlacement::Background { reason } => (ExecLocation::Background, Some(reason)),
            }
        };

        if location == ExecLocation::Terminal {
            // exec_placement 可能刚探测完远端 shell（第一次要等一会儿）：这期间点了停止 / 移出了
            // 目标，就不再写命令，把抢到的终端还回去（R-9 F11、F12）。
            if let Err(reason) = gate.check() {
                self.host.release_terminal(&target_id).await;
                // 审计按"没有执行"记：批准过，但在执行前作废了。
                self.record_audit(
                    scope,
                    &summary,
                    &params.command,
                    &verdict,
                    ExecDecision::Denied,
                    None,
                    None,
                    None,
                    None,
                );
                return Err(reason);
            }
            let marker = crate::ai::pty::new_marker();
            let payload = format!(
                "{}{}",
                crate::ai::pty::build_clear_prefix(),
                crate::ai::pty::build_wrapped_command(&params.command, &marker),
            );
            let request = TerminalExecRequest {
                target_id: target_id.clone(),
                scope_id: scope.id.clone(),
                command: params.command.clone(),
                payload,
                marker,
                wait,
                gate: gate.clone(),
            };
            match self.host.run_in_terminal(request).await {
                Ok(capture) => {
                    let exit_code = capture.exit_code.map(i64::from);
                    self.record_audit(
                        scope,
                        &summary,
                        &params.command,
                        &verdict,
                        decision,
                        exit_code,
                        Some(capture.duration_ms as i64),
                        Some(ExecLocation::Terminal),
                        None,
                    );
                    let response = RunCommandResponse {
                        stdout: capture.output,
                        stderr: String::new(),
                        stdout_truncated_bytes: 0,
                        stderr_truncated_bytes: 0,
                        exit_code: capture.exit_code.map(|c| c.max(0) as u32),
                        duration_ms: capture.duration_ms,
                        danger: verdict.level,
                        location: ExecLocation::Terminal,
                        fallback_reason: None,
                        timed_out: capture.timed_out,
                    };
                    return serde_json::to_value(response).map_err(|err| err.to_string());
                }
                Err(message) => {
                    // 宿主在写命令之前发现请求失效了：命令没有执行，也不能换个通道再执行。
                    if let Err(reason) = gate.check() {
                        self.record_audit(
                            scope,
                            &summary,
                            &params.command,
                            &verdict,
                            ExecDecision::Denied,
                            None,
                            None,
                            None,
                            None,
                        );
                        return Err(reason);
                    }
                    // 写到一半会话关了：命令可能已经进了终端，不能换个通道再执行一遍。
                    if message == terminal_write::WRITE_INTERRUPTED {
                        self.record_audit(
                            scope,
                            &summary,
                            &params.command,
                            &verdict,
                            decision,
                            None,
                            None,
                            Some(ExecLocation::Terminal),
                            None,
                        );
                        return Err(message);
                    }
                    // 终端执行失败：退回后台通道，并记下原因。
                    tracing::warn!(%message, "终端执行失败，降级到后台通道");
                    return self
                        .run_background(
                            scope,
                            &gate,
                            &summary,
                            &target_id,
                            &params.command,
                            &verdict,
                            decision,
                            wait,
                            Some(format!("终端执行失败：{message}")),
                        )
                        .await;
                }
            }
        }

        self.run_background(
            scope,
            &gate,
            &summary,
            &target_id,
            &params.command,
            &verdict,
            decision,
            wait,
            fallback_reason,
        )
        .await
    }

    /// 按执行模式决定一次操作能不能做：自动模式直接放行；手动模式弹确认，等人答、超时或
    /// 请求失效（作用范围销毁、用户点了停止、目标被移出对话）。返回 `(处置, 拒绝原因)`，
    /// 有拒绝原因就不能执行。
    ///
    /// 放行之前再核对一次 `ticket`：批准卡片可能是在目标被移出 / 停止之后才点的，
    /// 不能按旧的批准去执行（R-9 F11、F12）。
    pub(crate) async fn decide(
        &self,
        scope: &Arc<Scope>,
        ticket: &RequestTicket,
        request: ApprovalRequest,
    ) -> (ExecDecision, Option<String>) {
        let mode = self.mode.mode();
        let auto = match mode {
            ExecMode::Auto => true,
            ExecMode::ReadOnly => {
                request.file.is_none()
                    && request.danger == DangerLevel::Safe
                    && crate::ai::readonly::is_read_only(&request.command)
            }
            ExecMode::Manual => false,
        };
        let (decision, refusal) = match auto {
            true => (ExecDecision::Auto, None),
            false => {
                let approve = self.approver.approve(request);
                tokio::pin!(approve);
                let outcome = tokio::select! {
                    decision = &mut approve => WaitOutcome::Decision(decision),
                    _ = tokio::time::sleep(self.approval_wait) => WaitOutcome::ApprovalTimeout,
                    reason = scope.wait_invalidated(ticket) => WaitOutcome::Invalidated(reason),
                };
                match outcome {
                    WaitOutcome::Decision(ApprovalDecision::Allowed) => {
                        (ExecDecision::Allowed, None)
                    }
                    WaitOutcome::Decision(ApprovalDecision::Denied) => {
                        (ExecDecision::Denied, Some("用户拒绝".to_string()))
                    }
                    WaitOutcome::Decision(ApprovalDecision::Timeout)
                    | WaitOutcome::ApprovalTimeout => (
                        ExecDecision::Timeout,
                        Some("审批超时，已按拒绝处理".to_string()),
                    ),
                    WaitOutcome::Invalidated(reason) => {
                        (ExecDecision::Denied, Some(reason.to_string()))
                    }
                }
            }
        };
        if refusal.is_none() {
            if let Some(reason) = scope.ticket_refusal(ticket) {
                return (ExecDecision::Denied, Some(reason.to_string()));
            }
        }
        (decision, refusal)
    }

    /// 后台 exec 通道执行（AI-1 的老路径）。
    #[allow(clippy::too_many_arguments)]
    async fn run_background(
        &self,
        scope: &Arc<Scope>,
        gate: &ExecGate,
        summary: &TargetSummary,
        target_id: &str,
        command: &str,
        verdict: &crate::ai::safety::SafetyVerdict,
        decision: ExecDecision,
        wait: Duration,
        fallback_reason: Option<String>,
    ) -> Result<serde_json::Value, String> {
        // 执行期间用户点了停止：置位取消标志——还没发出去的命令不再发，已经发出去的关掉执行通道，
        // 不再等它跑完（R-9 F12）。宿主在发命令之前还会用 gate 核对目标有没有被移出。
        let exec =
            self.host
                .exec_cancellable(target_id, command, wait, EXEC_MAX_STREAM_BYTES, gate);
        tokio::pin!(exec);
        let mut stopped = None;
        let result = tokio::select! {
            result = &mut exec => result,
            reason = scope.wait_stopped(&gate.ticket) => {
                stopped = Some(reason);
                gate.cancel.store(true, Ordering::SeqCst);
                exec.await
            }
        };
        match result {
            Ok(capture) => {
                let exit_code = if capture.timed_out {
                    None
                } else {
                    capture.exit_code.map(i64::from)
                };
                self.record_audit(
                    scope,
                    summary,
                    command,
                    verdict,
                    decision,
                    exit_code,
                    Some(capture.duration_ms as i64),
                    Some(ExecLocation::Background),
                    fallback_reason.clone(),
                );
                if let Some(reason) = stopped {
                    // 已经发生的修改不会回滚：只是不再等它、也不把结果当成正常完成交回去。
                    return Err(if capture.cancelled {
                        format!("{reason}（后台命令的执行通道已关闭，命令可能已经执行了一部分）")
                    } else {
                        format!("{reason}（命令在取消前已经执行完）")
                    });
                }
                if capture.timed_out {
                    return Err("已超时".into());
                }
                let response = RunCommandResponse {
                    stdout: capture.stdout,
                    stderr: capture.stderr,
                    stdout_truncated_bytes: capture.stdout_truncated_bytes,
                    stderr_truncated_bytes: capture.stderr_truncated_bytes,
                    exit_code: capture.exit_code,
                    duration_ms: capture.duration_ms,
                    danger: verdict.level,
                    location: ExecLocation::Background,
                    fallback_reason,
                    timed_out: false,
                };
                serde_json::to_value(response).map_err(|err| err.to_string())
            }
            Err(message) => {
                self.record_audit(
                    scope,
                    summary,
                    command,
                    verdict,
                    // 宿主在发命令之前发现请求失效了：没有执行，按拒绝记。
                    if gate.check().is_err() {
                        ExecDecision::Denied
                    } else {
                        decision
                    },
                    None,
                    None,
                    Some(ExecLocation::Background),
                    fallback_reason,
                );
                Err(message)
            }
        }
    }

    /// 让 AI 停止这个对话正在终端里跑的 AI 命令（向那个终端发 Ctrl+C）。
    ///
    /// 停止按钮（`ai_agent_abort`）的入口：宿主通过终端执行登记表
    /// （[`crate::ai::runs::TerminalRuns`]）确认这个对话真的有一条命令在跑，
    /// 没有就什么都不发。
    /// 停止按钮：作废这个对话在这一刻之前开始的所有工具请求——等审批的按拒绝结束，
    /// 后台命令关闭执行通道，还没开始写的文件不再写（R-9 F12）。终端里的命令另由
    /// [`Bridge::interrupt_terminal`] 发 Ctrl+C。已经发生的修改不会回滚。
    pub fn stop_scope(&self, scope_id: &str) {
        if let Some(scope) = self.registry.find_by_id(scope_id) {
            scope.stop();
        }
    }

    pub async fn interrupt_terminal(&self, scope_id: &str) {
        self.host.interrupt_terminal(scope_id).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn record_audit(
        &self,
        scope: &Arc<Scope>,
        summary: &TargetSummary,
        command: &str,
        verdict: &crate::ai::safety::SafetyVerdict,
        decision: ExecDecision,
        exit_code: Option<i64>,
        duration_ms: Option<i64>,
        location: Option<ExecLocation>,
        fallback_reason: Option<String>,
    ) {
        self.audit.record(AuditEntry {
            at: now_millis(),
            scope: scope.id.clone(),
            host_label: summary.host_label.clone(),
            command: command.to_string(),
            danger: verdict.level,
            decision,
            exit_code,
            duration_ms,
            location,
            fallback_reason,
            tool: "run_command".to_string(),
            path: None,
            bytes: None,
            backup_path: None,
        });
    }
}

/// 解析目标 ID：不填时用"当前焦点"那台；填了必须是目标列表里的。
pub(crate) fn resolve_target(
    scope: &Arc<Scope>,
    target_id: Option<&str>,
) -> Result<String, String> {
    let list = scope.target_list();
    match target_id {
        Some(id) if !id.is_empty() => {
            if list.iter().any(|t| t.id == id) {
                Ok(id.to_string())
            } else {
                Err(SCOPE_TARGET_ERROR.to_string())
            }
        }
        _ => list
            .iter()
            .find(|t| t.current)
            .or_else(|| list.first())
            .map(|t| t.id.clone())
            .ok_or_else(|| "本次对话没有可操作的目标（请在输入框里 @ 一台主机）".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::Semaphore;

    use super::*;

    struct FixedMode(ExecMode);

    impl ExecModeProvider for FixedMode {
        fn mode(&self) -> ExecMode {
            self.0
        }
    }

    struct ExecCall {
        session_id: String,
        command: String,
        wait: Duration,
        max_stream_bytes: usize,
    }

    fn exec_ok() -> ExecCapture {
        ExecCapture {
            stdout: "hello\n".into(),
            stderr: String::new(),
            stdout_truncated_bytes: 0,
            stderr_truncated_bytes: 0,
            exit_code: Some(0),
            timed_out: false,
            cancelled: false,
            duration_ms: 12,
        }
    }

    struct MockHost {
        exec_result: Mutex<Result<ExecCapture, String>>,
        last_exec: Mutex<Option<ExecCall>>,
        terminal_lines: Mutex<Vec<String>>,
        last_max_lines: Mutex<Option<usize>>,
        /// 终端执行的处置：默认后台（老行为）。
        placement: Mutex<ExecPlacement>,
        terminal_result: Mutex<Result<TerminalExecCapture, String>>,
        last_terminal: Mutex<Option<TerminalExecRequest>>,
        interrupted: Mutex<Vec<String>>,
        memory: Mutex<std::collections::HashMap<String, String>>,
    }

    impl MockHost {
        fn new() -> MockHost {
            MockHost {
                exec_result: Mutex::new(Ok(exec_ok())),
                last_exec: Mutex::new(None),
                terminal_lines: Mutex::new(vec!["one".into(), "two".into()]),
                memory: Mutex::new(std::collections::HashMap::new()),
                last_max_lines: Mutex::new(None),
                placement: Mutex::new(ExecPlacement::Background {
                    reason: "测试默认走后台".to_string(),
                }),
                terminal_result: Mutex::new(Ok(TerminalExecCapture {
                    output: "term-out".into(),
                    exit_code: Some(0),
                    duration_ms: 7,
                    timed_out: false,
                })),
                last_terminal: Mutex::new(None),
                interrupted: Mutex::new(Vec::new()),
            }
        }

        fn set_terminal_placement(&self) {
            *self.placement.lock().unwrap() = ExecPlacement::Terminal;
        }

        fn set_exec_result(&self, result: Result<ExecCapture, String>) {
            *self.exec_result.lock().unwrap() = result;
        }

        fn last_exec(&self) -> Option<(String, String, Duration, usize)> {
            self.last_exec.lock().unwrap().as_ref().map(|call| {
                (
                    call.session_id.clone(),
                    call.command.clone(),
                    call.wait,
                    call.max_stream_bytes,
                )
            })
        }

        fn last_max_lines(&self) -> Option<usize> {
            *self.last_max_lines.lock().unwrap()
        }
    }

    impl BridgeHost for MockHost {
        fn read_memory<'a>(
            &'a self,
            target_id: &'a str,
        ) -> BoxFuture<'a, Result<Option<String>, String>> {
            let value = self.memory.lock().unwrap().get(target_id).cloned();
            Box::pin(async move { Ok(value) })
        }

        fn write_memory<'a>(
            &'a self,
            target_id: &'a str,
            notes: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            self.memory
                .lock()
                .unwrap()
                .insert(target_id.to_owned(), notes.to_owned());
            Box::pin(async { Ok(()) })
        }

        fn exec<'a>(
            &'a self,
            session_id: &'a str,
            command: &'a str,
            wait: Duration,
            max_stream_bytes: usize,
        ) -> BoxFuture<'a, Result<ExecCapture, String>> {
            *self.last_exec.lock().unwrap() = Some(ExecCall {
                session_id: session_id.to_string(),
                command: command.to_string(),
                wait,
                max_stream_bytes,
            });
            let result = self.exec_result.lock().unwrap().clone();
            Box::pin(async move { result })
        }

        fn read_terminal<'a>(
            &'a self,
            _session_id: &'a str,
            max_lines: usize,
        ) -> BoxFuture<'a, Result<Vec<String>, String>> {
            *self.last_max_lines.lock().unwrap() = Some(max_lines);
            let lines = self.terminal_lines.lock().unwrap().clone();
            Box::pin(async move { Ok(lines) })
        }

        fn exec_placement<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
            let placement = self.placement.lock().unwrap().clone();
            Box::pin(async move { placement })
        }

        fn run_in_terminal<'a>(
            &'a self,
            request: TerminalExecRequest,
        ) -> BoxFuture<'a, Result<TerminalExecCapture, String>> {
            *self.last_terminal.lock().unwrap() = Some(request);
            let result = self.terminal_result.lock().unwrap().clone();
            Box::pin(async move { result })
        }

        fn interrupt_terminal<'a>(&'a self, scope_id: &'a str) -> BoxFuture<'a, ()> {
            self.interrupted.lock().unwrap().push(scope_id.to_string());
            Box::pin(async {})
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ApproverBehavior {
        Allow,
        Deny,
        Never,
    }

    struct MockApprover {
        behavior: ApproverBehavior,
        calls: AtomicUsize,
        last: Mutex<Option<ApprovalRequest>>,
    }

    impl MockApprover {
        fn new(behavior: ApproverBehavior) -> MockApprover {
            MockApprover {
                behavior,
                calls: AtomicUsize::new(0),
                last: Mutex::new(None),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last(&self) -> Option<ApprovalRequest> {
            self.last.lock().unwrap().clone()
        }
    }

    impl Approver for MockApprover {
        fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap() = Some(request);
            let behavior = self.behavior;
            Box::pin(async move {
                match behavior {
                    ApproverBehavior::Allow => ApprovalDecision::Allowed,
                    ApproverBehavior::Deny => ApprovalDecision::Denied,
                    ApproverBehavior::Never => std::future::pending().await,
                }
            })
        }
    }

    #[derive(Default)]
    struct MockAudit {
        entries: Mutex<Vec<AuditEntry>>,
    }

    impl MockAudit {
        fn entries(&self) -> Vec<AuditEntry> {
            self.entries.lock().unwrap().clone()
        }
    }

    impl AuditLog for MockAudit {
        fn record(&self, entry: AuditEntry) {
            self.entries.lock().unwrap().push(entry);
        }
    }

    struct Harness {
        port: u16,
        bridge: Arc<Bridge>,
        host: Arc<MockHost>,
        approver: Arc<MockApprover>,
        audit: Arc<MockAudit>,
    }

    async fn harness(
        mode: ExecMode,
        behavior: ApproverBehavior,
        approval_wait: Duration,
    ) -> Harness {
        let host = Arc::new(MockHost::new());
        let approver = Arc::new(MockApprover::new(behavior));
        let audit = Arc::new(MockAudit::default());
        let bridge = Arc::new(
            Bridge::new(
                host.clone(),
                audit.clone(),
                approver.clone(),
                Arc::new(FixedMode(mode)),
            )
            .with_approval_wait(approval_wait),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑定端口");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(Arc::clone(&bridge).serve(listener));
        Harness {
            port,
            bridge,
            host,
            approver,
            audit,
        }
    }

    struct Client {
        reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
        writer: tokio::net::tcp::OwnedWriteHalf,
        /// 请求里附带的对话能力密钥（握手成功后每条请求都要带）。
        capability: String,
    }

    impl Client {
        async fn connect(port: u16, connection_token: &str) -> Client {
            let stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("连上桥");
            let (reader, writer) = stream.into_split();
            let mut client = Client {
                reader: BufReader::new(reader),
                writer,
                capability: String::new(),
            };
            client.send(&json!({ "token": connection_token })).await;
            client
        }

        /// 连接层令牌 + 对话能力密钥都就位的客户端。
        async fn authenticated(port: u16, connection_token: &str, capability: &str) -> Client {
            let mut client = Client::connect(port, connection_token).await;
            client.capability = capability.to_string();
            client
        }

        async fn send(&mut self, value: &Value) {
            let mut line = serde_json::to_string(value).expect("序列化");
            line.push('\n');
            let _ = self.writer.write_all(line.as_bytes()).await;
            let _ = self.writer.flush().await;
        }

        async fn recv(&mut self) -> Option<Value> {
            let mut line = String::new();
            match self.reader.read_line(&mut line).await {
                Ok(0) | Err(_) => None,
                Ok(_) => serde_json::from_str(line.trim()).ok(),
            }
        }

        async fn recv_within(&mut self, wait: Duration) -> Option<Value> {
            tokio::time::timeout(wait, self.recv())
                .await
                .expect("等服务端响应超时")
        }

        async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
            self.send(&json!({
                "id": id,
                "method": method,
                "params": params,
                "capability": self.capability,
            }))
            .await;
            self.recv_within(Duration::from_secs(5))
                .await
                .expect("服务端应回响应")
        }
    }

    fn result_of(response: &Value) -> &Value {
        assert!(response.get("error").is_none(), "不该报错：{response}");
        response.get("result").expect("要有 result")
    }

    fn error_of(response: &Value) -> &str {
        response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("该报错：{response}"))
    }

    fn short_wait() -> Duration {
        Duration::from_millis(150)
    }

    #[test]
    fn exec_mode_from_setting_defaults_to_manual() {
        assert_eq!(ExecMode::from_setting(None), ExecMode::Manual);
        assert_eq!(ExecMode::from_setting(Some("manual")), ExecMode::Manual);
        assert_eq!(ExecMode::from_setting(Some("auto")), ExecMode::Auto);
        assert_eq!(ExecMode::from_setting(Some("readonly")), ExecMode::ReadOnly);
        assert_eq!(ExecMode::from_setting(Some("垃圾")), ExecMode::Manual);
    }

    #[tokio::test]
    async fn wrong_connection_token_closes_connection() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let mut client = Client::connect(h.port, "bad-token").await;
        client
            .send(&json!({
                "id": 1,
                "method": METHOD_LIST_TARGETS,
                "params": {},
                "capability": "whatever",
            }))
            .await;
        assert!(
            client.recv_within(Duration::from_secs(2)).await.is_none(),
            "连接层令牌不对必须断开"
        );
    }

    #[tokio::test]
    async fn unknown_capability_is_rejected_but_connection_survives() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), "deadbeef").await;
        let response = client.request(1, METHOD_LIST_TARGETS, json!({})).await;
        assert!(error_of(&response).contains("能力密钥无效"), "{response}");
        // 缺能力密钥也不行。
        client.capability = String::new();
        let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
        assert!(
            error_of(&response).contains("缺少对话能力密钥"),
            "{response}"
        );
        // 握手没问题，连接还活着。
        client.capability = "deadbeef".to_string();
        let response = client.request(3, METHOD_LIST_TARGETS, json!({})).await;
        assert!(error_of(&response).contains("能力密钥无效"));
    }

    #[tokio::test]
    async fn list_targets_returns_the_scope_targets_without_address() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        scope.add_target("s2", "db-1", "admin", "connected", true, false);

        let other = h.bridge.create_scope();
        other.add_target("s3", "secret", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client.request(1, METHOD_LIST_TARGETS, json!({})).await;
        let sessions = result_of(&response)["targets"]
            .as_array()
            .expect("sessions 数组")
            .clone();
        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"s1") && ids.contains(&"s2"));
        assert!(!ids.contains(&"s3"), "别的 scope 的会话不能出现");
        for session in &sessions {
            let keys: Vec<&String> = session.as_object().unwrap().keys().collect();
            assert_eq!(
                keys.len(),
                6,
                "只允许 id/host_label/username/status/current/pinned：{session}"
            );
        }
        // 当前焦点那台排第一。
        assert_eq!(sessions[0]["id"], "s1");
        assert_eq!(sessions[0]["current"], true);
        assert_eq!(sessions[0]["pinned"], false);
    }

    #[tokio::test]
    async fn cross_scope_session_access_is_rejected() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope_a = h.bridge.create_scope();
        scope_a.add_target("a-1", "web-a", "root", "connected", true, false);
        let scope_b = h.bridge.create_scope();
        scope_b.add_target("b-1", "web-b", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope_a.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "b-1", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("不属于本次对话的作用范围"));
        let response = client
            .request(2, METHOD_READ_TERMINAL, json!({ "target_id": "b-1" }))
            .await;
        assert!(error_of(&response).contains("不属于本次对话的作用范围"));
        assert_eq!(h.approver.calls(), 0, "越权请求不该走到审批");
        assert_eq!(h.host.last_exec(), None);
    }

    #[tokio::test]
    async fn manual_mode_allows_after_approval() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "ls -la" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["stdout"], "hello\n");
        assert_eq!(result["exit_code"], 0);

        assert_eq!(h.approver.calls(), 1);
        let request = h.approver.last().expect("审批请求");
        assert_eq!(request.scope, scope.scope_id());
        assert_eq!(request.host_label, "web-1");
        assert_eq!(request.command, "ls -la");
        assert_eq!(request.danger, DangerLevel::Safe);

        let (session_id, command, wait, max_stream) = h.host.last_exec().expect("执行过");
        assert_eq!(session_id, "s1");
        assert_eq!(command, "ls -la");
        assert_eq!(
            wait,
            Duration::from_secs(60),
            "没给 timeout_sec 时默认 60 秒"
        );
        assert_eq!(max_stream, EXEC_MAX_STREAM_BYTES);

        let entries = h.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].decision, ExecDecision::Allowed);
        assert_eq!(entries[0].danger, DangerLevel::Safe);
        assert_eq!(entries[0].exit_code, Some(0));
        assert_eq!(entries[0].duration_ms, Some(12));
        assert_eq!(entries[0].host_label, "web-1");
        assert_eq!(entries[0].scope, scope.scope_id());
    }

    #[tokio::test]
    async fn manual_mode_denial_returns_user_rejected() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Deny, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("用户拒绝"));
        assert_eq!(h.host.last_exec(), None, "被拒绝就不能执行");

        let entries = h.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].decision, ExecDecision::Denied);
        assert_eq!(entries[0].exit_code, None);
        assert_eq!(entries[0].duration_ms, None);
    }

    #[tokio::test]
    async fn auto_mode_runs_safe_commands_without_approval() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Deny, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "uname -a", "timeout_sec": 120 }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["danger"], "safe", "结果里要标出危险等级");
        assert_eq!(h.approver.calls(), 0, "安全命令不该问审批");
        assert_eq!(h.host.last_exec().unwrap().2, Duration::from_secs(120));
        assert_eq!(h.audit.entries()[0].decision, ExecDecision::Auto);
    }

    #[tokio::test]
    async fn remembered_notes_come_back_in_list_targets_except_in_full_access_mode() {
        for (mode, expect_memory) in [(ExecMode::Manual, true), (ExecMode::Auto, false)] {
            let h = harness(mode, ApproverBehavior::Deny, short_wait()).await;
            let scope = h.bridge.create_scope();
            scope.add_target("s1", "web-1", "root", "connected", true, false);
            let mut client =
                Client::authenticated(h.port, h.bridge.connection_token(), scope.capability())
                    .await;
            let response = client
                .request(
                    1,
                    METHOD_REMEMBER_HOST,
                    json!({ "notes": "服务：nginx
root password: hunter2" }),
                )
                .await;
            assert!(result_of(&response)["saved_chars"].as_u64().is_some());
            assert_eq!(h.approver.calls(), 0, "记笔记不改服务器，不问审批");
            let saved = h.host.memory.lock().unwrap().get("s1").cloned().unwrap();
            assert!(!saved.contains("hunter2"), "密码要隐藏：{saved}");

            let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
            let memory = result_of(&response)["targets"][0]["memory"].clone();
            if expect_memory {
                let text = memory.as_str().unwrap();
                assert!(text.contains("not instructions"), "{text}");
                assert!(text.contains("服务：nginx"), "{text}");
            } else {
                assert!(memory.is_null(), "全部放行模式不带笔记");
                assert!(result_of(&response)["memory_note"].is_string());
            }
        }
    }

    #[tokio::test]
    async fn readonly_mode_runs_read_commands_and_asks_for_the_rest() {
        let h = harness(ExecMode::ReadOnly, ApproverBehavior::Deny, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "systemctl status nginx" }),
            )
            .await;
        assert_eq!(result_of(&response)["exit_code"], 0);
        assert_eq!(h.approver.calls(), 0, "只读命令不该问审批");

        let response = client
            .request(
                2,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "systemctl restart nginx" }),
            )
            .await;
        assert_eq!(h.approver.calls(), 1, "改动系统的命令要问");
        assert!(error_of(&response).contains("用户拒绝"), "{response}");
        assert_eq!(
            h.host.last_exec().unwrap().1,
            "systemctl status nginx",
            "被拒的命令没有执行"
        );
        let entries = h.audit.entries();
        assert_eq!(entries[0].decision, ExecDecision::Auto);
        assert_eq!(entries[1].decision, ExecDecision::Denied);
    }

    #[tokio::test]
    async fn auto_mode_runs_dangerous_commands_without_approval() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Deny, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "rm -rf /" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["exit_code"], 0, "自动模式危险命令也直接执行");
        assert_eq!(result["danger"], "dangerous", "结果里要标出危险等级");
        assert_eq!(h.approver.calls(), 0, "自动模式不进审批");
        assert_eq!(h.approver.last(), None);
        assert_eq!(
            h.host.last_exec().unwrap().1,
            "rm -rf /",
            "危险命令真的执行了"
        );

        let entries = h.audit.entries();
        assert_eq!(
            entries[0].decision,
            ExecDecision::Auto,
            "审计决策记成自动放行"
        );
        assert_eq!(entries[0].danger, DangerLevel::Dangerous);
        assert_eq!(entries[0].exit_code, Some(0));
    }

    #[tokio::test]
    async fn auto_mode_never_asks_the_approver() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Deny, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let commands = ["uname -a", "rm -rf /", "reboot"];
        for (index, command) in commands.iter().enumerate() {
            let response = client
                .request(
                    index as u64 + 1,
                    METHOD_RUN_COMMAND,
                    json!({ "target_id": "s1", "command": command }),
                )
                .await;
            assert_eq!(
                result_of(&response)["exit_code"],
                0,
                "{command} 要直接执行：{response}"
            );
        }
        assert_eq!(h.approver.calls(), 0, "自动模式下审批接口一次都不该被调用");
        assert_eq!(h.approver.last(), None);

        let entries = h.audit.entries();
        assert_eq!(entries.len(), 3);
        assert!(
            entries
                .iter()
                .all(|entry| entry.decision == ExecDecision::Auto),
            "全部记成自动放行：{entries:?}"
        );
        assert_eq!(entries[0].danger, DangerLevel::Safe);
        assert_eq!(entries[1].danger, DangerLevel::Dangerous);
        assert_eq!(entries[2].danger, DangerLevel::Dangerous);
    }

    #[tokio::test]
    async fn approval_timeout_denies_command() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Never, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("审批超时"));
        assert_eq!(h.host.last_exec(), None);
        assert_eq!(h.audit.entries()[0].decision, ExecDecision::Timeout);
    }

    #[tokio::test]
    async fn destroyed_scope_denies_pending_approval_and_rejects_later_requests() {
        let h = harness(
            ExecMode::Manual,
            ApproverBehavior::Never,
            Duration::from_secs(30),
        )
        .await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        let capability = scope.capability().to_string();

        let mut pending =
            Client::authenticated(h.port, h.bridge.connection_token(), &capability).await;
        let mut later =
            Client::authenticated(h.port, h.bridge.connection_token(), &capability).await;
        assert!(
            result_of(&later.request(0, METHOD_LIST_TARGETS, json!({})).await).is_object(),
            "later 的握手要先就位"
        );

        let handle = tokio::spawn(async move {
            pending
                .request(
                    1,
                    METHOD_RUN_COMMAND,
                    json!({ "target_id": "s1", "command": "ls" }),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while h.approver.calls() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("审批要挂起");
        scope.destroy();

        let response = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("scope 销毁后审批要立刻结束")
            .expect("任务不 panic");
        assert!(error_of(&response).contains("用户拒绝"));
        assert_eq!(h.audit.entries()[0].decision, ExecDecision::Denied);

        let response = later.request(2, METHOD_LIST_TARGETS, json!({})).await;
        assert!(error_of(&response).contains("能力密钥无效或已过期"));

        // 连接层令牌仍然有效（进程没停），但能力密钥已从注册表摘除；用旧密钥的
        // 新连接一律拒绝。
        let mut fresh =
            Client::authenticated(h.port, h.bridge.connection_token(), &capability).await;
        let response = fresh.request(3, METHOD_LIST_TARGETS, json!({})).await;
        assert!(
            error_of(&response).contains("能力密钥无效或已过期"),
            "销毁后能力密钥必须失效：{response}"
        );
    }

    #[tokio::test]
    async fn exec_timeout_reports_zssh_timeout() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_exec_result(Ok(ExecCapture {
            stdout: "部分输出".into(),
            stderr: String::new(),
            stdout_truncated_bytes: 0,
            stderr_truncated_bytes: 0,
            exit_code: None,
            timed_out: true,
            cancelled: false,
            duration_ms: 999,
        }));
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "sleep 5", "timeout_sec": 1 }),
            )
            .await;
        assert!(error_of(&response).contains("已超时"));
        let entries = h.audit.entries();
        assert_eq!(entries[0].decision, ExecDecision::Auto);
        assert_eq!(entries[0].exit_code, None);
        assert_eq!(entries[0].duration_ms, Some(999));
    }

    #[tokio::test]
    async fn read_terminal_clamps_max_lines_and_returns_lines() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(1, METHOD_READ_TERMINAL, json!({ "target_id": "s1" }))
            .await;
        let result = result_of(&response);
        assert_eq!(h.host.last_max_lines(), Some(200), "默认 200 行");
        assert_eq!(result["lines"].as_array().unwrap().len(), 2);

        let response = client
            .request(
                2,
                METHOD_READ_TERMINAL,
                json!({ "session_id": "s1", "max_lines": 999_999 }),
            )
            .await;
        assert!(result_of(&response)["lines"].is_array());
        assert_eq!(h.host.last_max_lines(), Some(2000), "最多 2000 行");
    }

    #[tokio::test]
    async fn unknown_method_and_garbage_return_errors() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client.request(1, "no_such_method", json!({})).await;
        assert!(error_of(&response).contains("未知方法"));

        client.send(&json!("这不是请求")).await;
        let garbage = client
            .recv_within(Duration::from_secs(5))
            .await
            .expect("要回错误");
        assert_eq!(garbage["id"], 0);
        assert!(error_of(&garbage).contains("无法解析"));

        let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
        assert!(result_of(&response)["targets"].is_array(), "连接要活着");
    }

    #[tokio::test]
    async fn host_failure_is_passed_through_and_audited() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_exec_result(Err("会话已断开".into()));
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("会话已断开"));
        assert_eq!(h.audit.entries()[0].exit_code, None);
    }

    #[tokio::test]
    async fn removed_session_disappears_from_scope() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        scope.add_target("s2", "db-1", "root", "connected", true, false);
        scope.remove_target("s1");

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client.request(1, METHOD_LIST_TARGETS, json!({})).await;
        let sessions = result_of(&response)["targets"].as_array().unwrap().clone();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], "s2");

        let response = client
            .request(
                2,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("不属于本次对话的作用范围"));
    }

    #[tokio::test]
    async fn runs_in_terminal_when_the_host_says_it_is_free() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_terminal_placement();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "echo hi" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "terminal");
        assert_eq!(result["stdout"], "term-out");
        assert_eq!(result["exit_code"], 0);
        // 没有走后台通道。
        assert_eq!(h.host.last_exec(), None);
        // 写进终端的是包装过的命令（带清输入前缀，前面有空格）。
        let request = h
            .host
            .last_terminal
            .lock()
            .unwrap()
            .clone()
            .expect("终端执行过");
        assert!(
            request.payload.contains("\x0b\x15"),
            "要先清掉输了一半的输入"
        );
        assert!(request.payload.contains("echo hi"));
        // 请求要带上这个对话的 scope id（AI-2d 1.4：停止按钮靠它认领命令）。
        assert_eq!(request.scope_id, scope.scope_id());

        let entries = h.audit.entries();
        assert_eq!(entries[0].location, Some(ExecLocation::Terminal));
        assert_eq!(entries[0].fallback_reason, None);
    }

    #[tokio::test]
    async fn busy_terminal_falls_back_to_background_with_a_reason() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        // MockHost 默认返回 Background { reason }。
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "uname -a" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "background");
        assert_eq!(result["fallback_reason"], "测试默认走后台");
        assert_eq!(result["stdout"], "hello\n");

        let entries = h.audit.entries();
        assert_eq!(entries[0].location, Some(ExecLocation::Background));
        assert_eq!(
            entries[0].fallback_reason.as_deref(),
            Some("测试默认走后台")
        );
    }

    #[tokio::test]
    async fn target_outside_the_list_is_rejected() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        // 填了一个不在目标列表里的 ID：拒绝（哪怕它在主机库里）。
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s-other", "command": "ls" }),
            )
            .await;
        assert!(error_of(&response).contains("不属于本次对话的作用范围"));
        assert_eq!(h.host.last_exec(), None, "越权目标不该执行");
        assert!(h.host.last_terminal.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_target_id_uses_the_current_one() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        scope.add_target("s2", "db-1", "root", "connected", false, true);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        // 不填 target_id：默认用"当前"那台（s1）。
        let response = client
            .request(1, METHOD_RUN_COMMAND, json!({ "command": "ls" }))
            .await;
        assert!(result_of(&response).is_object());
        let (target, _, _, _) = h.host.last_exec().expect("执行过");
        assert_eq!(target, "s1", "不填就用当前焦点那台");
    }

    #[tokio::test]
    async fn interrupt_targets_the_scope() {
        // 停止按钮传的是 scope id：宿主据此查"这个对话有没有命令在终端里跑"。
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        h.bridge.interrupt_terminal(scope.scope_id()).await;
        assert_eq!(
            h.host.interrupted.lock().unwrap().clone(),
            vec![scope.scope_id().to_string()]
        );
    }

    /// 会让终端行编辑器产生按键效果、或者让卡片显示和实际内容不一致的字符（R-8 第 1 节）。
    ///
    /// 命令是当键盘输入打进终端的，单引号挡得住 shell 解释，挡不住 Ctrl+U（删掉已输入
    /// 的前缀）、Ctrl+C/D、ESC、DEL、回车（提前提交）这些按键；C1 和方向控制字符还能
    /// 让审批卡片显示的内容和实际执行的不一样。
    const FORBIDDEN_CONTROL_CHARS: [(&str, &str, &str); 8] = [
        ("Ctrl+U", "ls\x15rm -rf ~", "U+0015"),
        ("Ctrl+C", "ls\x03", "U+0003"),
        ("Ctrl+D", "ls\x04", "U+0004"),
        ("ESC", "ls\x1b", "U+001B"),
        ("DEL", "ls\x7f", "U+007F"),
        ("CR", "ls\r", "U+000D"),
        ("C1 CSI", "ls\u{009b}31m", "U+009B"),
        ("RTL override", "ls\u{202e}rm -rf ~", "U+202E"),
    ];

    #[tokio::test]
    async fn control_char_commands_are_rejected_before_approval_and_write_nothing_to_the_terminal()
    {
        // 手动模式 + 终端空闲：修复前这条命令会走到审批、批准后写进终端。
        for (label, command, code) in FORBIDDEN_CONTROL_CHARS {
            let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
            h.host.set_terminal_placement();
            let scope = h.bridge.create_scope();
            scope.add_target("s1", "web-1", "root", "connected", true, false);

            let mut client =
                Client::authenticated(h.port, h.bridge.connection_token(), scope.capability())
                    .await;
            let response = client
                .request(
                    1,
                    METHOD_RUN_COMMAND,
                    json!({ "target_id": "s1", "command": command }),
                )
                .await;
            let message = error_of(&response);
            assert!(
                message.contains("控制字符"),
                "{label}：错误要说明是控制字符：{message}"
            );
            assert!(
                message.contains(code),
                "{label}：错误里要有码位 {code}：{message}"
            );
            assert_eq!(h.approver.calls(), 0, "{label}：审批之前就该拒绝");
            assert!(
                h.host.last_terminal.lock().unwrap().is_none(),
                "{label}：终端里不能写入任何字节"
            );
            assert_eq!(
                h.host.last_exec(),
                None,
                "{label}：后台通道也不能执行（终端执行和后台执行都算）"
            );
        }
    }

    #[tokio::test]
    async fn control_char_commands_are_rejected_in_auto_mode_too() {
        // 自动模式没有审批这道关：控制字符也要在后台执行之前被拒。
        for (label, command, code) in FORBIDDEN_CONTROL_CHARS {
            let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
            // MockHost 默认返回 Background：这次走后台通道。
            let scope = h.bridge.create_scope();
            scope.add_target("s1", "web-1", "root", "connected", true, false);

            let mut client =
                Client::authenticated(h.port, h.bridge.connection_token(), scope.capability())
                    .await;
            let response = client
                .request(
                    1,
                    METHOD_RUN_COMMAND,
                    json!({ "target_id": "s1", "command": command }),
                )
                .await;
            let message = error_of(&response);
            assert!(
                message.contains("控制字符"),
                "{label}：错误要说明是控制字符：{message}"
            );
            assert!(
                message.contains(code),
                "{label}：错误里要有码位 {code}：{message}"
            );
            assert_eq!(h.host.last_exec(), None, "{label}：自动模式也不能执行");
            assert!(
                h.host.last_terminal.lock().unwrap().is_none(),
                "{label}：终端里不能写入任何字节"
            );
        }
    }

    #[tokio::test]
    async fn newline_and_tab_still_run_through_the_terminal() {
        let command = "printf 'a\\tb\\n'\necho done";
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_terminal_placement();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": command }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "terminal");
        assert_eq!(h.approver.calls(), 1, "放行的命令照常走审批");
        let request = h
            .host
            .last_terminal
            .lock()
            .unwrap()
            .clone()
            .expect("换行和制表符要能执行");
        assert_eq!(request.command, command);
        assert!(
            request.payload.contains("a\\tb\\n") && request.payload.contains("echo done"),
            "命令原文要原样进包装脚本：{}",
            request.payload
        );
    }

    /// 终端行编辑器收到 Tab 会做自动补全（单引号里也一样），实际执行的命令会和审批时看到的
    /// 不一样（R-7 第 0 节）。所以带真正 `\t` 的命令在终端空闲时也要改走后台通道。
    #[tokio::test]
    async fn tab_in_command_goes_to_background_not_terminal_in_manual_mode() {
        let command = "echo a\tb";
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_terminal_placement();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": command }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "background");
        assert_eq!(
            result["fallback_reason"],
            "命令里有 Tab，终端会把它当成补全键"
        );
        assert_eq!(h.approver.calls(), 1, "照常走审批");
        assert!(
            h.host.last_terminal.lock().unwrap().is_none(),
            "终端里不能写入任何字节"
        );
        let (_, executed, _, _) = h.host.last_exec().expect("后台通道执行了");
        assert_eq!(executed, command, "后台通道拿到的是原文");

        let entries = h.audit.entries();
        assert_eq!(entries[0].location, Some(ExecLocation::Background));
        assert_eq!(
            entries[0].fallback_reason.as_deref(),
            Some("命令里有 Tab，终端会把它当成补全键")
        );
    }

    #[tokio::test]
    async fn tab_in_command_goes_to_background_in_auto_mode_too() {
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_terminal_placement();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "\techo indented" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "background");
        assert!(result["fallback_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("Tab"));
        assert!(h.host.last_terminal.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn command_without_tab_still_uses_the_terminal() {
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        h.host.set_terminal_placement();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": "echo a b\necho c" }),
            )
            .await;
        let result = result_of(&response);
        assert_eq!(result["location"], "terminal");
        assert_eq!(h.host.last_exec(), None, "没有 Tab 不该走后台");
        assert!(h.host.last_terminal.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn newline_and_tab_still_run_in_the_background() {
        let command = "echo one\necho two\tend";
        let h = harness(ExecMode::Auto, ApproverBehavior::Allow, short_wait()).await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);

        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = client
            .request(
                1,
                METHOD_RUN_COMMAND,
                json!({ "target_id": "s1", "command": command }),
            )
            .await;
        assert_eq!(result_of(&response)["exit_code"], 0);
        let (_, executed, _, _) = h.host.last_exec().expect("后台通道要执行");
        assert_eq!(executed, command);
    }

    /// 一条连接的请求并发处理（R-8 第 10 节）：一条卡在审批上时，另一条必须立刻回。
    #[tokio::test]
    async fn a_pending_approval_does_not_block_other_requests_on_the_same_connection() {
        // 审批永远不回答；等待时间给足，测试自己不等它结束。
        let h = harness(
            ExecMode::Manual,
            ApproverBehavior::Never,
            Duration::from_secs(30),
        )
        .await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        let mut client =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;

        // 第一条：run_command 卡在审批上，只发请求、不在这里等响应。
        client
            .send(&json!({
                "id": 1,
                "method": METHOD_RUN_COMMAND,
                "params": { "target_id": "s1", "command": "ls" },
                "capability": scope.capability(),
            }))
            .await;
        // 等桥确实进了审批（旧实现会一直串行处理，这条请求之后才轮到下一条）。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while h.approver.calls() == 0 {
            assert!(tokio::time::Instant::now() < deadline, "第一条没进审批");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // 第二条：同一连接上的 list_targets 要立刻返回（不等第一条审批）。
        let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
        assert_eq!(response["id"], 2, "响应要按 id 对应：{response}");
        let targets = result_of(&response)["targets"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(targets.len(), 1, "第二条要拿到自己的结果：{response}");
        assert_eq!(targets[0]["id"], "s1");
    }

    /// 宿主：read_terminal 卡在信号量上，用来制造「先到的请求后回」。
    struct GatedHost {
        entered: Semaphore,
        release: Semaphore,
    }

    impl GatedHost {
        fn new() -> GatedHost {
            GatedHost {
                entered: Semaphore::new(0),
                release: Semaphore::new(0),
            }
        }
    }

    impl BridgeHost for GatedHost {
        fn exec<'a>(
            &'a self,
            _session_id: &'a str,
            _command: &'a str,
            _wait: Duration,
            _max_stream_bytes: usize,
        ) -> BoxFuture<'a, Result<ExecCapture, String>> {
            Box::pin(async { Err("本测不执行命令".to_owned()) })
        }

        fn read_terminal<'a>(
            &'a self,
            _session_id: &'a str,
            _max_lines: usize,
        ) -> BoxFuture<'a, Result<Vec<String>, String>> {
            self.entered.add_permits(1);
            Box::pin(async move {
                let _ = self.release.acquire().await;
                Ok(vec!["gated".to_string()])
            })
        }
    }

    /// 响应可以乱序：客户端按 id 认领自己的结果。
    #[tokio::test]
    async fn responses_are_matched_by_id_even_when_they_arrive_out_of_order() {
        let host = Arc::new(GatedHost::new());
        let bridge = Arc::new(Bridge::new(
            host.clone(),
            Arc::new(MockAudit::default()),
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(FixedMode(ExecMode::Auto)),
        ));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑定端口");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(Arc::clone(&bridge).serve(listener));
        let scope = bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        let mut client =
            Client::authenticated(port, bridge.connection_token(), scope.capability()).await;

        // id 1：read_terminal 卡在宿主里；只发请求。
        client
            .send(&json!({
                "id": 1,
                "method": METHOD_READ_TERMINAL,
                "params": { "target_id": "s1" },
                "capability": scope.capability(),
            }))
            .await;
        // 等宿主确实进了 read_terminal，再发第二条（保证 id 1 先到、后回）。
        host.entered
            .acquire()
            .await
            .expect("信号量不该关闭")
            .forget();

        // id 2：list_targets 先回。
        let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
        assert_eq!(response["id"], 2, "先回的是 id 2：{response}");

        // 放行 id 1，它的响应随后到，内容对得上。
        host.release.add_permits(1);
        let response = client
            .recv_within(Duration::from_secs(5))
            .await
            .expect("id 1 的响应要回来");
        assert_eq!(response["id"], 1, "后回的是 id 1：{response}");
        let lines = result_of(&response)["lines"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "gated");
    }

    #[test]
    fn token_comparison_accepts_only_the_exact_token() {
        // 恒定时间比较的行为要和 `==` 一样准（R-8 第 11 节）。
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(constant_time_eq(token, token));
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq(token, ""));
        assert!(!constant_time_eq("", token));
        assert!(!constant_time_eq(token, "bad-token"));
        // 长度不同。
        assert!(!constant_time_eq(token, &token[..token.len() - 1]));
        // 同长度、只有最后一个字符不同。
        let mut last_differs = token.to_string();
        let last = last_differs.pop().expect("非空");
        last_differs.push(if last == '0' { '1' } else { '0' });
        assert!(!constant_time_eq(token, &last_differs));
        // 同长度、只有第一个字符不同。
        let mut first_differs = token.to_string();
        let first = first_differs.remove(0);
        first_differs.insert(0, if first == '0' { '1' } else { '0' });
        assert!(!constant_time_eq(token, &first_differs));
    }

    #[tokio::test]
    async fn wrong_connection_token_with_the_same_length_closes_the_connection() {
        // 真实入口：和正确令牌等长（只是内容不同）也要被拒（恒定时间比较不能判错）。
        let h = harness(ExecMode::Manual, ApproverBehavior::Allow, short_wait()).await;
        let valid = h.bridge.connection_token().to_string();
        let mut bad = valid.clone();
        let last = bad.pop().expect("令牌非空");
        bad.push(if last == '0' { '1' } else { '0' });
        assert_eq!(bad.len(), valid.len());
        assert_ne!(bad, valid);

        let mut client = Client::connect(h.port, &bad).await;
        client
            .send(&json!({
                "id": 1,
                "method": METHOD_LIST_TARGETS,
                "params": {},
                "capability": "whatever",
            }))
            .await;
        assert!(
            client.recv_within(Duration::from_secs(2)).await.is_none(),
            "等长但内容不对的连接层令牌必须断开连接"
        );
    }

    // ----- R-9：停止 / 移出目标让在途请求作废（F11、F12），输入限长（F15） -----

    /// 审批人：等测试放行之后才回答"允许"，用来制造"批准之前目标被移出 / 点了停止"。
    struct GateApprover {
        entered: Semaphore,
        release: Semaphore,
    }

    impl Approver for GateApprover {
        fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            self.entered.add_permits(1);
            Box::pin(async move {
                let _ = self.release.acquire().await;
                ApprovalDecision::Allowed
            })
        }
    }

    fn manual_bridge(approver: Arc<dyn Approver>, host: Arc<dyn BridgeHost>) -> Arc<Bridge> {
        Arc::new(Bridge::new(
            host,
            Arc::new(MockAudit::default()),
            approver,
            Arc::new(FixedMode(ExecMode::Manual)),
        ))
    }

    fn approval(scope: &Scope) -> ApprovalRequest {
        ApprovalRequest {
            scope: scope.id.clone(),
            host_label: "web-1".into(),
            command: "ls".into(),
            danger: DangerLevel::Safe,
            reasons: Vec::new(),
            file: None,
        }
    }

    #[test]
    fn a_ticket_is_invalidated_by_removal_even_if_the_target_comes_back() {
        let bridge = manual_bridge(
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(MockHost::new()),
        );
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let ticket = handle.scope.ticket("s1");
        assert_eq!(handle.scope.ticket_refusal(&ticket), None);

        handle.remove_target("s1");
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        assert!(
            handle.scope.ticket_refusal(&ticket).is_some(),
            "移出后又加回来，也不能按移出之前的批准执行"
        );
        let fresh = handle.scope.ticket("s1");
        assert_eq!(
            handle.scope.ticket_refusal(&fresh),
            None,
            "新的请求不受影响"
        );
    }

    #[tokio::test]
    async fn an_approval_given_after_the_target_was_removed_is_refused() {
        let approver = Arc::new(GateApprover {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let bridge = manual_bridge(approver.clone(), Arc::new(MockHost::new()));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let scope = Arc::clone(&handle.scope);
        let ticket = scope.ticket("s1");

        let decide = {
            let bridge = Arc::clone(&bridge);
            let scope = Arc::clone(&scope);
            tokio::spawn(async move {
                let request = approval(&scope);
                bridge.decide(&scope, &ticket, request).await
            })
        };
        approver
            .entered
            .acquire()
            .await
            .expect("审批要挂起")
            .forget();
        // 卡片还开着时目标被移出：等审批的请求立即按拒绝结束，不用等人点。
        handle.remove_target("s1");
        let (decision, refusal) = tokio::time::timeout(Duration::from_secs(5), decide)
            .await
            .expect("移出目标后审批要立刻结束")
            .expect("任务不 panic");
        assert_eq!(decision, ExecDecision::Denied);
        assert!(refusal.unwrap_or_default().contains("移出"));
    }

    #[tokio::test]
    async fn an_approval_that_lands_right_after_a_stop_is_refused() {
        // 自动模式没有审批可等：停止发生在拿到 ticket 和放行之间，放行前的复核也要拦住。
        let bridge = Arc::new(Bridge::new(
            Arc::new(MockHost::new()),
            Arc::new(MockAudit::default()),
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(FixedMode(ExecMode::Auto)),
        ));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let ticket = handle.scope.ticket("s1");
        bridge.stop_scope(&handle.scope.id);
        let (decision, refusal) = bridge
            .decide(&handle.scope, &ticket, approval(&handle.scope))
            .await;
        assert_eq!(decision, ExecDecision::Denied);
        assert!(refusal.unwrap_or_default().contains("停止"));
    }

    #[tokio::test]
    async fn stop_ends_a_pending_approval() {
        let bridge = manual_bridge(
            Arc::new(MockApprover::new(ApproverBehavior::Never)),
            Arc::new(MockHost::new()),
        );
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let scope = Arc::clone(&handle.scope);
        let ticket = scope.ticket("s1");
        let decide = {
            let bridge = Arc::clone(&bridge);
            let scope = Arc::clone(&scope);
            tokio::spawn(async move {
                let request = approval(&scope);
                bridge.decide(&scope, &ticket, request).await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        bridge.stop_scope(&scope.id);
        let (decision, refusal) = tokio::time::timeout(Duration::from_secs(5), decide)
            .await
            .expect("停止后审批要立刻结束")
            .expect("任务不 panic");
        assert_eq!(decision, ExecDecision::Denied);
        assert!(refusal.unwrap_or_default().contains("停止"));
    }

    /// 宿主：按生产实现的顺序走——开始前核对 gate → （可选）等"自动连接"→ 连上后再核对 gate
    /// → 取消标志没置位才"发出"命令 → 命令一直跑，直到取消标志置位（模拟关闭执行通道）。
    /// `sent` 记命令真正发出去的次数；`placement_gate` / `connect_gate` 让测试卡住探测 / 连接。
    struct CancellableHost {
        started: Semaphore,
        sent: AtomicUsize,
        hold_connect: AtomicBool,
        connecting: Semaphore,
        connect_gate: Semaphore,
        hold_placement: AtomicBool,
        placing: Semaphore,
        placement_gate: Semaphore,
        terminal_runs: AtomicUsize,
        released: AtomicUsize,
        /// 设了就让 run_in_terminal 按这个原因失败（模拟会话任务那边的写入结果）。
        terminal_error: Mutex<Option<String>>,
    }

    impl CancellableHost {
        fn new() -> CancellableHost {
            CancellableHost {
                started: Semaphore::new(0),
                sent: AtomicUsize::new(0),
                hold_connect: AtomicBool::new(false),
                connecting: Semaphore::new(0),
                connect_gate: Semaphore::new(0),
                hold_placement: AtomicBool::new(false),
                placing: Semaphore::new(0),
                placement_gate: Semaphore::new(0),
                terminal_runs: AtomicUsize::new(0),
                released: AtomicUsize::new(0),
                terminal_error: Mutex::new(None),
            }
        }
    }

    impl BridgeHost for CancellableHost {
        fn exec<'a>(
            &'a self,
            _session_id: &'a str,
            _command: &'a str,
            _wait: Duration,
            _max_stream_bytes: usize,
        ) -> BoxFuture<'a, Result<ExecCapture, String>> {
            Box::pin(async { Err("本测走 exec_cancellable".to_owned()) })
        }

        fn exec_cancellable<'a>(
            &'a self,
            _session_id: &'a str,
            _command: &'a str,
            _wait: Duration,
            _max_stream_bytes: usize,
            gate: &'a ExecGate,
        ) -> BoxFuture<'a, Result<ExecCapture, String>> {
            Box::pin(async move {
                gate.check()?;
                if self.hold_connect.load(Ordering::SeqCst) {
                    self.connecting.add_permits(1);
                    let _ = self.connect_gate.acquire().await;
                }
                gate.check()?;
                if gate.cancel_flag().load(Ordering::SeqCst) {
                    return Err("已取消，命令没有发出".to_owned());
                }
                self.sent.fetch_add(1, Ordering::SeqCst);
                self.started.add_permits(1);
                while !gate.cancel_flag().load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Ok(ExecCapture {
                    exit_code: None,
                    cancelled: true,
                    ..exec_ok()
                })
            })
        }

        fn read_terminal<'a>(
            &'a self,
            _session_id: &'a str,
            _max_lines: usize,
        ) -> BoxFuture<'a, Result<Vec<String>, String>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn exec_placement<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
            Box::pin(async move {
                if !self.hold_placement.load(Ordering::SeqCst) {
                    return ExecPlacement::Background {
                        reason: "测试走后台".into(),
                    };
                }
                // 模拟第一次在终端执行前探测远端 shell：卡住，等测试放行。
                self.placing.add_permits(1);
                let _ = self.placement_gate.acquire().await;
                ExecPlacement::Terminal
            })
        }

        fn run_in_terminal<'a>(
            &'a self,
            request: TerminalExecRequest,
        ) -> BoxFuture<'a, Result<TerminalExecCapture, String>> {
            Box::pin(async move {
                request.gate.check()?;
                if let Some(message) = self.terminal_error.lock().ok().and_then(|e| e.clone()) {
                    return Err(message);
                }
                self.terminal_runs.fetch_add(1, Ordering::SeqCst);
                Ok(TerminalExecCapture {
                    output: String::new(),
                    exit_code: Some(0),
                    duration_ms: 1,
                    timed_out: false,
                })
            })
        }

        fn release_terminal<'a>(&'a self, _target_id: &'a str) -> BoxFuture<'a, ()> {
            self.released.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn auto_bridge(host: Arc<CancellableHost>) -> Arc<Bridge> {
        Arc::new(Bridge::new(
            host,
            Arc::new(MockAudit::default()),
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(FixedMode(ExecMode::Auto)),
        ))
    }

    fn spawn_run(
        bridge: &Arc<Bridge>,
        scope: &Arc<Scope>,
    ) -> tokio::task::JoinHandle<Result<serde_json::Value, String>> {
        let bridge = Arc::clone(bridge);
        let scope = Arc::clone(scope);
        tokio::spawn(async move {
            bridge
                .run_command(
                    &scope,
                    json!({ "target_id": "s1", "command": "rm -f /tmp/x" }),
                )
                .await
        })
    }

    #[tokio::test]
    async fn stop_while_auto_connecting_never_sends_the_command() {
        // R-9 F12：停止发生在批准之后、自动连接完成之前：连上之后不能再把命令发出去。
        let host = Arc::new(CancellableHost::new());
        host.hold_connect.store(true, Ordering::SeqCst);
        let bridge = auto_bridge(host.clone());
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let run = spawn_run(&bridge, &handle.scope);
        host.connecting
            .acquire()
            .await
            .expect("要开始连接")
            .forget();
        bridge.stop_scope(&handle.scope.id);
        host.connect_gate.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("要结束")
            .expect("任务不 panic");
        assert!(result.expect_err("停止了").contains("停止"));
        assert_eq!(host.sent.load(Ordering::SeqCst), 0, "命令不能发出去");
    }

    #[tokio::test]
    async fn removing_the_target_while_auto_connecting_never_sends_the_command() {
        // R-9 F11：批准之后、自动连接完成之前目标被移出：旧请求不能再在原目标执行。
        let host = Arc::new(CancellableHost::new());
        host.hold_connect.store(true, Ordering::SeqCst);
        let bridge = auto_bridge(host.clone());
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let run = spawn_run(&bridge, &handle.scope);
        host.connecting
            .acquire()
            .await
            .expect("要开始连接")
            .forget();
        handle.remove_target("s1");
        host.connect_gate.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("要结束")
            .expect("任务不 panic");
        assert!(result.expect_err("目标已移出").contains("移出"));
        assert_eq!(host.sent.load(Ordering::SeqCst), 0, "命令不能发出去");
    }

    #[tokio::test]
    async fn removing_the_target_while_probing_the_shell_never_writes_to_the_terminal() {
        // R-9 F11：exec_placement 正在探测 shell 时目标被移出：不写终端，抢到的终端要还回去。
        let host = Arc::new(CancellableHost::new());
        host.hold_placement.store(true, Ordering::SeqCst);
        let bridge = auto_bridge(host.clone());
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let run = spawn_run(&bridge, &handle.scope);
        host.placing.acquire().await.expect("要开始探测").forget();
        handle.remove_target("s1");
        host.placement_gate.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("要结束")
            .expect("任务不 panic");
        assert!(result.expect_err("目标已移出").contains("移出"));
        assert_eq!(host.terminal_runs.load(Ordering::SeqCst), 0, "不能写终端");
        assert_eq!(host.sent.load(Ordering::SeqCst), 0, "也不能换后台通道执行");
        assert_eq!(host.released.load(Ordering::SeqCst), 1, "终端占位要还回去");
    }

    #[tokio::test]
    async fn stop_while_probing_the_shell_never_writes_to_the_terminal() {
        let host = Arc::new(CancellableHost::new());
        host.hold_placement.store(true, Ordering::SeqCst);
        let bridge = auto_bridge(host.clone());
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let run = spawn_run(&bridge, &handle.scope);
        host.placing.acquire().await.expect("要开始探测").forget();
        bridge.stop_scope(&handle.scope.id);
        host.placement_gate.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("要结束")
            .expect("任务不 panic");
        assert!(result.expect_err("停止了").contains("停止"));
        assert_eq!(host.terminal_runs.load(Ordering::SeqCst), 0);
        assert_eq!(host.sent.load(Ordering::SeqCst), 0);
        assert_eq!(host.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_closes_a_running_background_command() {
        let host = Arc::new(CancellableHost::new());
        let bridge = Arc::new(Bridge::new(
            host.clone(),
            Arc::new(MockAudit::default()),
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(FixedMode(ExecMode::Auto)),
        ));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let scope = Arc::clone(&handle.scope);
        let run = {
            let bridge = Arc::clone(&bridge);
            let scope = Arc::clone(&scope);
            tokio::spawn(async move {
                bridge
                    .run_command(&scope, json!({ "target_id": "s1", "command": "sleep 600" }))
                    .await
            })
        };
        host.started.acquire().await.expect("命令要开始").forget();
        bridge.stop_scope(&scope.id);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("停止后后台命令要立刻结束")
            .expect("任务不 panic");
        let message = result.expect_err("停止的命令不能当成正常完成");
        assert!(message.contains("停止"), "{message}");
        assert!(message.contains("执行通道已关闭"), "{message}");
    }

    #[tokio::test]
    async fn removing_the_target_does_not_kill_a_command_that_is_already_running() {
        // 切换终端标签会把原来的会话移出对话：已经在跑的后台命令不能因此被关掉通道。
        let host = Arc::new(CancellableHost::new());
        let bridge = Arc::new(Bridge::new(
            host.clone(),
            Arc::new(MockAudit::default()),
            Arc::new(MockApprover::new(ApproverBehavior::Allow)),
            Arc::new(FixedMode(ExecMode::Auto)),
        ));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let scope = Arc::clone(&handle.scope);
        let run = {
            let bridge = Arc::clone(&bridge);
            let scope = Arc::clone(&scope);
            tokio::spawn(async move {
                bridge
                    .run_command(&scope, json!({ "target_id": "s1", "command": "sleep 600" }))
                    .await
            })
        };
        host.started.acquire().await.expect("命令要开始").forget();
        handle.remove_target("s1");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!run.is_finished(), "移出目标不该打断已经在跑的命令");
        // 停止仍然能让它结束。
        bridge.stop_scope(&scope.id);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("停止后要结束")
            .expect("任务不 panic");
        assert!(result.expect_err("停止的命令").contains("停止"));
    }

    /// 后台执行的发命令边界（ssh 层）拿 ExecGate 当 ExecGuard 用：开通道期间目标被移出，
    /// `invalidated` 要醒、`check` 要拒（审计 C04：以前只传停止标志，移出拦不住）。
    #[tokio::test]
    async fn removing_the_target_while_the_channel_opens_wakes_the_exec_guard() {
        use crate::ssh::connection::ExecGuard;
        let bridge = auto_bridge(Arc::new(CancellableHost::new()));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let gate = ExecGate::new(&handle.scope, &handle.scope.ticket("s1"));
        let guard: &dyn ExecGuard = &gate;
        assert_eq!(guard.check(), Ok(()));
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { ExecGuard::invalidated(&gate).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished(), "还有效时不能醒");
        handle.remove_target("s1");
        let reason = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("移出目标要叫醒等开通道的一方")
            .expect("不 panic");
        assert!(reason.contains("移出"), "{reason}");
        assert!(guard.check().expect_err("已移出").contains("移出"));
        assert!(
            !guard.stop_requested(),
            "移出目标不算停止：已经发出去的命令不因此关通道"
        );
    }

    #[tokio::test]
    async fn stop_while_the_channel_opens_wakes_the_exec_guard_and_requests_a_stop() {
        use crate::ssh::connection::ExecGuard;
        let bridge = auto_bridge(Arc::new(CancellableHost::new()));
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let gate = ExecGate::new(&handle.scope, &handle.scope.ticket("s1"));
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { ExecGuard::invalidated(&gate).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        bridge.stop_scope(&handle.scope.id);
        let reason = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("停止要叫醒等开通道的一方")
            .expect("不 panic");
        assert!(reason.contains("停止"), "{reason}");
        // 取消标志还没被 run_background 置位，也要算停止。
        assert!(!gate.cancel_flag().load(Ordering::SeqCst));
        assert!(ExecGuard::stop_requested(&gate));
    }

    #[tokio::test]
    async fn a_terminal_write_interrupted_by_closing_the_session_is_not_rerun_in_the_background() {
        // 会话任务认领了 AI 命令、写到一半会话被关：命令可能已经进了终端，不能退回后台通道
        // 再执行一遍。
        let host = Arc::new(CancellableHost::new());
        host.hold_placement.store(true, Ordering::SeqCst);
        host.placement_gate.add_permits(1);
        *host.terminal_error.lock().expect("锁") =
            Some(terminal_write::WRITE_INTERRUPTED.to_owned());
        let bridge = auto_bridge(host.clone());
        let handle = bridge.create_scope();
        handle.add_target("s1", "web-1", "root", "connected", true, false);
        let run = spawn_run(&bridge, &handle.scope);
        let result = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("要结束")
            .expect("任务不 panic");
        assert_eq!(
            result.expect_err("写到一半"),
            terminal_write::WRITE_INTERRUPTED
        );
        assert_eq!(host.sent.load(Ordering::SeqCst), 0, "不能换后台通道重跑");
    }

    #[tokio::test]
    async fn many_authenticated_connections_are_all_served() {
        // 握手名额只限制没认证的连接：claude 每个对话一个 zssh-mcp，多开对话不能被拒。
        let h = harness(
            ExecMode::Auto,
            ApproverBehavior::Allow,
            Duration::from_secs(1),
        )
        .await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        let mut clients = Vec::new();
        for _ in 0..(MAX_PENDING_HANDSHAKES + 4) {
            let mut client =
                Client::authenticated(h.port, h.bridge.connection_token(), scope.capability())
                    .await;
            let response = client.request(1, METHOD_LIST_TARGETS, json!({})).await;
            assert!(result_of(&response).is_object(), "{response}");
            clients.push(client);
        }
        for client in &mut clients {
            let response = client.request(2, METHOD_LIST_TARGETS, json!({})).await;
            assert!(result_of(&response).is_object(), "{response}");
        }
    }

    #[tokio::test]
    async fn reconnecting_does_not_grant_a_fresh_request_budget() {
        // R-9 F15：每条连接各有上限还不够——在几条连接上各压满待审批的请求、断开、再连，
        // 旧请求还在等审批，新连接不能再拿到额度。
        let h = harness(
            ExecMode::Manual,
            ApproverBehavior::Never,
            Duration::from_secs(60),
        )
        .await;
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        let per_connection = MAX_IN_FLIGHT_PER_CONNECTION;
        let connections = MAX_IN_FLIGHT_TOTAL / per_connection;
        for round in 0..connections {
            let mut client =
                Client::authenticated(h.port, h.bridge.connection_token(), scope.capability())
                    .await;
            for i in 0..per_connection {
                client
                    .send(&json!({
                        "id": i + 1,
                        "method": METHOD_RUN_COMMAND,
                        "params": { "target_id": "s1", "command": "ls" },
                        "capability": scope.capability(),
                    }))
                    .await;
            }
            let expected = (round + 1) * per_connection;
            tokio::time::timeout(Duration::from_secs(10), async {
                while h.approver.calls() < expected {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("这一批请求都要挂在审批上");
            // 断开：请求还在等审批，名额不能跟着连接一起还回去。
            drop(client);
        }
        let mut fresh =
            Client::authenticated(h.port, h.bridge.connection_token(), scope.capability()).await;
        let response = fresh.request(999, METHOD_LIST_TARGETS, json!({})).await;
        assert!(
            error_of(&response).contains("太多"),
            "全局名额用完时新连接也要被拒：{response}"
        );

        // 作用范围销毁后挂着的请求都结束，名额回来了。
        scope.destroy();
        let scope = h.bridge.create_scope();
        scope.add_target("s1", "web-1", "root", "connected", true, false);
        fresh.capability = scope.capability().to_string();
        let response = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = fresh.request(1000, METHOD_LIST_TARGETS, json!({})).await;
                if response.get("error").is_none() || !error_of(&response).contains("太多") {
                    return response;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("名额要还回来");
        assert!(result_of(&response).is_object(), "{response}");
    }

    #[tokio::test]
    async fn bounded_line_reads_stop_at_the_limit() {
        async fn read(input: &[u8], max: usize) -> BoundedLine {
            let mut reader = BufReader::new(input);
            read_bounded_line(&mut reader, max).await.expect("读")
        }
        assert_eq!(read(b"abc\nrest", 8).await, BoundedLine::Line("abc".into()));
        assert_eq!(read(b"", 8).await, BoundedLine::Eof);
        assert_eq!(
            read(b"12345678\n", 8).await,
            BoundedLine::Line("12345678".into())
        );
        assert_eq!(read(b"123456789\n", 8).await, BoundedLine::TooLong);
        assert_eq!(read(b"tail", 8).await, BoundedLine::Line("tail".into()));
    }

    #[tokio::test]
    async fn an_oversized_handshake_line_closes_the_connection() {
        let h = harness(
            ExecMode::Auto,
            ApproverBehavior::Allow,
            Duration::from_secs(1),
        )
        .await;
        let stream = TcpStream::connect(("127.0.0.1", h.port))
            .await
            .expect("连上桥");
        let (reader, mut writer) = stream.into_split();
        // 没有换行、比握手上限大得多的一串：服务端读到上限就该断开，而不是一直攒。
        let junk = vec![b'a'; HANDSHAKE_MAX_BYTES * 4];
        let _ = writer.write_all(&junk).await;
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("服务端要很快断开");
        assert!(matches!(read, Ok(0) | Err(_)), "连接应已关闭：{read:?}");
    }
}
