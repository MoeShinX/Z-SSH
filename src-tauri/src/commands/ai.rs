//! 工具桥的审批通道（卡片 AI-1）：请求发事件给界面，答案走命令。
//!
//! 界面在 AI-2；本模块只提供事件和命令，并保证请求的生命周期干净：
//! 审批超时（工具桥 5 分钟）、scope 销毁、或界面始终不回答，挂着的请求都会被摘掉。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;
use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, AuditEntry, AuditLog, BridgeHost, ExecGate,
    ExecMode, ExecModeProvider, ExecPlacement, TerminalExecCapture, TerminalExecRequest,
};
use zssh_core::ai::files::{FileDiff, RemoteFs, SftpRemoteFs};
use zssh_core::ai::protocol::parse_host_target_id;
use zssh_core::ai::pty::ShellKind;
use zssh_core::ai::runs::TerminalRuns;
use zssh_core::ssh::connection::ExecGuard;
use zssh_core::ssh::{BoxFuture, ExecCapture};

use crate::state::AppState;

/// 事件名：向界面发起一条命令的确认请求。
pub const AI_APPROVAL_REQUEST: &str = "ai_approval_request";
/// 事件名：AI 需要一个还没打开的 @ 主机自动连接（界面为它新开一个标签）。
pub const AI_TARGET_CONNECT: &str = "ai_target_connect";

/// `ai_approval_request` 的负载（给 AI-2 的界面用）。
#[derive(Debug, Clone, Serialize)]
pub struct AiApprovalPayload {
    pub req_id: u64,
    pub scope: String,
    pub host_label: String,
    pub command: String,
    pub danger: String,
    pub reasons: Vec<String>,
    /// 工具名：`run_command` 或 `write_file`。
    pub tool: String,
    /// `write_file` 的细节：写哪个文件、和现有文件的差异（新建文件就是全文）。
    pub file: Option<AiApprovalFile>,
}

/// `write_file` 确认卡片里的文件细节。
#[derive(Debug, Clone, Serialize)]
pub struct AiApprovalFile {
    pub path: String,
    pub is_new: bool,
    pub new_bytes: usize,
    pub diff: FileDiff,
}

impl AiApprovalPayload {
    /// 从工具桥的审批请求生成给界面的负载。
    ///
    /// `run_command` 的命令会显示在确认卡片上；命令里的不可见控制字符转成可见的转义
    /// 形式（R-8 第 1 节审批卡片第二道防线。正常路径上控制字符已经在工具桥收到
    /// `run_command` 时被拒，这里兜底防的是别的来源或以后的改动）。
    fn from_request(req_id: u64, request: ApprovalRequest) -> AiApprovalPayload {
        AiApprovalPayload {
            req_id,
            scope: request.scope,
            host_label: request.host_label,
            command: zssh_core::ai::command_text::escape_invisible_chars(&request.command),
            danger: request.danger.as_str().to_string(),
            reasons: request.reasons,
            tool: if request.file.is_some() {
                "write_file".to_string()
            } else {
                "run_command".to_string()
            },
            file: request.file.map(|file| AiApprovalFile {
                path: file.path,
                is_new: file.is_new,
                new_bytes: file.new_bytes,
                diff: file.diff,
            }),
        }
    }
}

/// 待确认请求表：请求 ID → 等答案的一端。
#[derive(Default)]
pub struct AiApprovals {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
}

impl AiApprovals {
    fn pending(&self) -> MutexGuard<'_, HashMap<u64, oneshot::Sender<bool>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 登记一个待确认的请求，返回请求 ID 和等答案的一端。
    pub fn register(&self) -> (u64, oneshot::Receiver<bool>) {
        let req_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (sender, receiver) = oneshot::channel();
        self.pending().insert(req_id, sender);
        (req_id, receiver)
    }

    /// 界面答了：把答案送给等着的审批。返回这个请求是否存在（可能已超时）。
    pub fn answer(&self, req_id: u64, allow: bool) -> bool {
        match self.pending().remove(&req_id) {
            Some(sender) => {
                // 等的一端可能已经走了（超时 / scope 销毁），答案没人收也无所谓。
                let _ = sender.send(allow);
                true
            }
            None => false,
        }
    }

    /// 摘掉一个请求（超时、scope 销毁、已经答完）。
    pub fn cancel(&self, req_id: u64) {
        self.pending().remove(&req_id);
    }

    /// 这个请求还在等答案吗（只有测试用得到）。
    #[cfg(test)]
    pub fn is_pending(&self, req_id: u64) -> bool {
        self.pending().contains_key(&req_id)
    }
}

/// 挂着的审批：future 一结束（回答、超时、scope 销毁）就从表里摘掉，不留垃圾。
struct PendingApproval {
    approvals: Arc<AiApprovals>,
    req_id: u64,
}

impl Drop for PendingApproval {
    fn drop(&mut self) {
        self.approvals.cancel(self.req_id);
    }
}

/// 工具桥的审批实现：发 `ai_approval_request` 事件，等 `ai_approval_answer`。
pub struct TauriApprover {
    app: AppHandle,
    approvals: Arc<AiApprovals>,
}

impl TauriApprover {
    pub fn new(app: AppHandle, approvals: Arc<AiApprovals>) -> Self {
        Self { app, approvals }
    }
}

impl Approver for TauriApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        let app = self.app.clone();
        let approvals = Arc::clone(&self.approvals);
        Box::pin(async move {
            let (req_id, receiver) = approvals.register();
            let _guard = PendingApproval { approvals, req_id };
            let payload = AiApprovalPayload::from_request(req_id, request);
            if app.emit(AI_APPROVAL_REQUEST, payload).is_err() {
                // 界面还没起来（或者窗口没了）：按拒绝处理，别让命令挂住。
                return ApprovalDecision::Denied;
            }
            // 窗口缩在托盘里时用户看不到确认框，AI 会一直等：叫出窗口。
            crate::tray::call_attention(&app);
            match receiver.await {
                Ok(true) => ApprovalDecision::Allowed,
                _ => ApprovalDecision::Denied,
            }
        })
    }
}

/// 界面回答一条确认请求（AI-2 的"执行 / 拒绝"按钮调它）。
#[tauri::command]
pub async fn ai_approval_answer(
    approvals: State<'_, Arc<AiApprovals>>,
    req_id: u64,
    allow: bool,
) -> Result<(), String> {
    if approvals.answer(req_id, allow) {
        Ok(())
    } else {
        Err("没有这个待确认的命令（可能已经超时或被取消）".to_owned())
    }
}

/// 等待界面自动连接的主机：主机 ID → 建好之后送会话 ID 的一端。
///
/// 工具桥的 [`AppBridgeHost`] 和界面命令 [`ai_target_connect_result`] 通过这个共享表
/// 会合（都是 Tauri 管理的单例）。
#[derive(Default)]
pub struct TargetConnects {
    pending: Mutex<HashMap<i64, oneshot::Sender<String>>>,
}

impl TargetConnects {
    fn pending(&self) -> MutexGuard<'_, HashMap<i64, oneshot::Sender<String>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 登记一个等待；返回等答案的一端。
    fn register(&self, host_id: i64) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        self.pending().insert(host_id, tx);
        rx
    }

    fn take(&self, host_id: i64) -> Option<oneshot::Sender<String>> {
        self.pending().remove(&host_id)
    }

    #[cfg(test)]
    fn is_pending(&self, host_id: i64) -> bool {
        self.pending().contains_key(&host_id)
    }
}

/// 界面为 AI 自动连接一个 @ 主机成功了（新开的标签连上后调）。
#[tauri::command]
pub async fn ai_target_connect_result(
    connects: State<'_, Arc<TargetConnects>>,
    host_id: i64,
    session_id: String,
) -> Result<(), String> {
    match connects.take(host_id) {
        Some(tx) => {
            let _ = tx.send(session_id);
            Ok(())
        }
        None => Err("没有在等这个主机的自动连接".to_string()),
    }
}

/// 界面拒绝了自动连接（用户取消了指纹 / 密码框，或者关掉了标签）。
#[tauri::command]
pub async fn ai_target_connect_cancel(
    connects: State<'_, Arc<TargetConnects>>,
    host_id: i64,
) -> Result<(), String> {
    connects.take(host_id);
    Ok(())
}

/// 工具桥执行命令用的目标来源（AI-2c：目标 ID 可能是会话 ID，也可能是未连上的 @ 主机）。
///
/// 未连上的 @ 主机（`host-<id>`）第一次被用到时，发 [`AI_TARGET_CONNECT`] 事件让界面
/// 为它新开一个标签、走正常连接流程；这里等着那个会话出现（或用户取消）。
pub struct AppBridgeHost {
    app: AppHandle,
    connects: Arc<TargetConnects>,
}

impl AppBridgeHost {
    pub fn new(app: AppHandle, connects: Arc<TargetConnects>) -> Self {
        Self { app, connects }
    }

    /// 把一个目标 ID 解析成一个已连上的会话 ID；未连上的 @ 主机触发自动连接。
    async fn resolve_session(&self, target_id: &str, wait: Duration) -> Result<String, String> {
        let state = self.app.state::<AppState>();
        if state.exec_connection(target_id).await.is_some() {
            return Ok(target_id.to_string());
        }
        let Some(host_id) = parse_host_target_id(target_id) else {
            return Err(format!("目标不存在或者还没连上：{target_id}"));
        };
        // 同一条主机可能刚好已经连上了。
        if let Some(existing) = state.session_for_host(host_id).await {
            return Ok(existing);
        }

        let rx = self.connects.register(host_id);
        let payload = TargetConnectPayload {
            host_id,
            target_id: target_id.to_string(),
        };
        if self.app.emit(AI_TARGET_CONNECT, payload).is_err() {
            self.connects.take(host_id);
            return Err("界面没有响应自动连接请求".to_string());
        }
        let session_id = match tokio::time::timeout(wait, rx).await {
            Ok(Ok(session_id)) => session_id,
            _ => {
                self.connects.take(host_id);
                return Err("用户取消连接（或自动连接超时）".to_string());
            }
        };
        // 界面回报的是刚 `session_open` 出来的会话 ID：它还在连接中（指纹 / 认证 /
        // 开 shell 都要时间）。等它真正可以执行（或者超时）。
        let deadline = Instant::now() + wait;
        loop {
            if state.exec_connection(&session_id).await.is_some() {
                return Ok(session_id);
            }
            if Instant::now() >= deadline {
                return Err("自动连接的会话没能连上".to_string());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// 需要自动连接一个 @ 主机时发给界面的事件负载。
#[derive(Debug, Clone, Serialize)]
pub struct TargetConnectPayload {
    pub host_id: i64,
    pub target_id: String,
}

/// 某台主机的记忆被 AI 改了（主机详情里开着的记忆框要刷新）。
pub const HOST_MEMORY_CHANGED: &str = "host-memory-changed";

impl AppBridgeHost {
    /// 目标对应主机库里的哪台主机：`host-<id>` 直接解析；已连上的会话看会话记的主机。
    async fn host_id_of(&self, target_id: &str) -> Option<i64> {
        if let Some(host_id) = parse_host_target_id(target_id) {
            return Some(host_id);
        }
        self.app
            .state::<AppState>()
            .duplicate_source(target_id)
            .await
            .and_then(|(_, host_id, _, _)| host_id)
    }
}

impl BridgeHost for AppBridgeHost {
    fn exec<'a>(
        &'a self,
        target_id: &'a str,
        command: &'a str,
        wait: Duration,
        max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        let app = self.app.clone();
        let connects = self.connects.clone();
        let target_id = target_id.to_owned();
        let command = command.to_owned();
        Box::pin(async move {
            // 未连上的 @ 主机：先自动连接（发事件等界面）。
            let bridge = AppBridgeHost {
                app: app.clone(),
                connects,
            };
            let session_id = bridge
                .resolve_session(&target_id, Duration::from_secs(120))
                .await?;
            let connection = app
                .state::<AppState>()
                .exec_connection(&session_id)
                .await
                .ok_or_else(|| format!("会话不存在或者还没连上：{session_id}"))?;
            connection
                .exec_capture(&command, wait, max_stream_bytes)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn exec_cancellable<'a>(
        &'a self,
        target_id: &'a str,
        command: &'a str,
        wait: Duration,
        max_stream_bytes: usize,
        gate: &'a ExecGate,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        let app = self.app.clone();
        let connects = self.connects.clone();
        Box::pin(async move {
            gate.check()?;
            let bridge = AppBridgeHost {
                app: app.clone(),
                connects,
            };
            let session_id = bridge
                .resolve_session(target_id, Duration::from_secs(120))
                .await?;
            // 自动连接可能等了很久：这期间点了停止 / 移出了目标，就不再发命令（R-9 F11、F12）。
            gate.check()?;
            let connection = app
                .state::<AppState>()
                .exec_connection(&session_id)
                .await
                .ok_or_else(|| format!("会话不存在或者还没连上：{session_id}"))?;
            // gate 一直带到发 exec 的边界（审计 C04）：开通道期间停止 / 移出目标就不再等，
            // 通道开好之后发命令之前再完整核对一次；发出去之后只有停止才关闭执行通道。
            connection
                .exec_capture_guarded(command, wait, max_stream_bytes, gate)
                .await
        })
    }

    fn release_terminal<'a>(&'a self, target_id: &'a str) -> BoxFuture<'a, ()> {
        let app = self.app.clone();
        Box::pin(async move {
            let state = app.state::<AppState>();
            release_terminal(&state, target_id).await;
        })
    }

    fn read_terminal<'a>(
        &'a self,
        target_id: &'a str,
        max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        let app = self.app.clone();
        let connects = self.connects.clone();
        let target_id = target_id.to_owned();
        Box::pin(async move {
            // 未连上的 @ 主机一样要先把会话连上（和 run_command 一条路径）。
            let bridge = AppBridgeHost {
                app: app.clone(),
                connects,
            };
            let session_id = bridge
                .resolve_session(&target_id, Duration::from_secs(120))
                .await?;
            let ring = app
                .state::<AppState>()
                .terminal_ring(&session_id)
                .await
                .ok_or_else(|| format!("会话不存在或者还没连上：{session_id}"))?;
            let lines = ring
                .lock()
                .map_err(|_| "终端缓冲锁不可用".to_owned())?
                .recent_lines(max_lines);
            Ok(lines)
        })
    }

    fn remote_fs<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        let app = self.app.clone();
        let connects = self.connects.clone();
        let target_id = target_id.to_owned();
        Box::pin(async move {
            // 和 run_command 一条路径：未连上的 @ 主机先自动连接。
            let bridge = AppBridgeHost {
                app: app.clone(),
                connects,
            };
            let session_id = bridge
                .resolve_session(&target_id, Duration::from_secs(120))
                .await?;
            let connection = app
                .state::<AppState>()
                .exec_connection(&session_id)
                .await
                .ok_or_else(|| format!("会话不存在或者还没连上：{session_id}"))?;
            let channel = zssh_core::sftp::SftpChannel::open(&connection)
                .await
                .map_err(|err| format!("打开 SFTP 通道失败：{err}"))?;
            let fs: Arc<dyn RemoteFs> = Arc::new(SftpRemoteFs::new(Arc::new(channel)));
            Ok(fs)
        })
    }

    fn exec_placement<'a>(&'a self, target_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
        let app = self.app.clone();
        let target_id = target_id.to_owned();
        Box::pin(async move {
            let state = app.state::<AppState>();
            // 未连上的 @ 主机：先看有没有已经连上的同主机会话，有则按它的状态判。
            let session_id = if state.terminal_exec(&target_id).await.is_some() {
                target_id.clone()
            } else if let Some(host_id) = parse_host_target_id(&target_id) {
                match state.session_for_host(host_id).await {
                    Some(existing) => existing,
                    None => {
                        // 还没连：后台通道会在真正执行时触发自动连接。
                        return ExecPlacement::Background {
                            reason: "会话还没连上，先走后台通道".to_string(),
                        };
                    }
                }
            } else {
                return ExecPlacement::Background {
                    reason: "会话还没连上，先走后台通道".to_string(),
                };
            };
            let Some(terminal) = state.terminal_exec(&session_id).await else {
                return ExecPlacement::Background {
                    reason: "会话还没连上，先走后台通道".to_string(),
                };
            };
            // 1.3：第一次需要在终端里执行之前，用后台 exec 通道静默探测一次远端
            // 登录 shell 的类型（没探测过绝不就地执行）。
            if terminal.shell_kind() == ShellKind::Unknown {
                let kind = probe_remote_shell_kind(&state, &session_id).await;
                tracing::debug!(?kind, "远端 shell 探测结果");
                terminal.set_shell_kind(kind);
            }
            match terminal.try_acquire() {
                Ok(()) => ExecPlacement::Terminal,
                Err(reason) => ExecPlacement::Background { reason },
            }
        })
    }

    fn run_in_terminal<'a>(
        &'a self,
        request: TerminalExecRequest,
    ) -> BoxFuture<'a, Result<TerminalExecCapture, String>> {
        let app = self.app.clone();
        let connects = self.connects.clone();
        Box::pin(async move {
            let bridge = AppBridgeHost {
                app: app.clone(),
                connects,
            };
            run_in_terminal_on(&app, &bridge, request).await
        })
    }

    fn interrupt_terminal<'a>(&'a self, scope_id: &'a str) -> BoxFuture<'a, ()> {
        let app = self.app.clone();
        let scope_id = scope_id.to_owned();
        Box::pin(async move {
            let state = app.state::<AppState>();
            // AI-2d 1.4：只有这个对话**确实**有一条 AI 命令在终端里跑才发 Ctrl+C。
            // 没有记录就什么都不做——用户可能正在那个终端里跑自己的东西。
            let runs = app.state::<TerminalRuns>();
            let Some(run) = runs.running(&scope_id) else {
                return;
            };
            // Ctrl+C（ETX）。会话已经断开时 write_raw 自己返回 false。
            let _ = state.write_raw(&run.session_id, vec![0x03]).await;
        })
    }

    fn read_memory<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            let Some(host_id) = self.host_id_of(target_id).await else {
                return Ok(None);
            };
            let state = self.app.state::<AppState>();
            Ok(state
                .store
                .host_memory_get(host_id)
                .map_err(|err| err.to_string())?
                .map(|memory| memory.content))
        })
    }

    fn write_memory<'a>(
        &'a self,
        target_id: &'a str,
        notes: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let host_id = self
                .host_id_of(target_id)
                .await
                .ok_or_else(|| "这个目标不在主机库里（快速连接），不能保存记忆".to_string())?;
            self.app
                .state::<AppState>()
                .store
                .host_memory_set(host_id, notes)
                .map_err(|err| err.to_string())?;
            let _ = self.app.emit(HOST_MEMORY_CHANGED, host_id);
            Ok(())
        })
    }

    fn write_backup_keep(&self) -> usize {
        // 每次写入时重新读设置：用户在设置页改了份数，下一笔就生效。
        self.app
            .state::<AppState>()
            .store
            .ai_write_backup_keep()
            .unwrap_or(zssh_core::store::settings::AI_WRITE_BACKUP_KEEP_DEFAULT)
    }
}

/// 在用户的终端里执行 AI 命令：写进包装脚本，等结束标记，取回输出和退出码。
///
/// 超时**不中断**命令：把当前截到的输出返回，`timed_out = true`。
///
/// 终端占位由 [`AppBridgeHost::exec_placement`] 里的 `try_acquire` 抢下；这里在
/// 命令结束（拿到标记 / 超时 / 会话断开）后**无论成败都** `release`。
///
/// 执行期间在 [`TerminalRuns`] 登记一条记录（AI-2d 1.4）：停止按钮据此知道
/// "这个对话有条命令正在这个终端里跑"，然后才发 Ctrl+C。命令结束时清掉记录；
/// 超时返回时命令还在跑，留一个观察任务等迟到的结束标记把它清掉。
async fn run_in_terminal_on(
    app: &AppHandle,
    bridge: &AppBridgeHost,
    request: TerminalExecRequest,
) -> Result<TerminalExecCapture, String> {
    let state = app.state::<AppState>();
    let session_id = match bridge
        .resolve_session(&request.target_id, Duration::from_secs(120))
        .await
    {
        Ok(session_id) => session_id,
        Err(reason) => {
            // 占位是在 exec_placement 里抢的：这里失败也要还回去。
            release_terminal(&state, &request.target_id).await;
            return Err(reason);
        }
    };
    let Some(terminal) = state.terminal_exec(&session_id).await else {
        release_terminal(&state, &request.target_id).await;
        return Err(format!("会话不存在或者还没连上：{session_id}"));
    };

    let runs = app.state::<TerminalRuns>();
    runs.register(&request.scope_id, &session_id, &request.marker);
    // 先登记再核对：停止按钮先作废请求、再按登记表发 Ctrl+C。在这之前按的停止，这里能看到；
    // 在这之后按的，登记表里已经有这条，Ctrl+C 能找到它。等会话就绪（自动连接）期间移出了目标
    // 也在这里拦下：命令还没写进终端（R-9 F11、F12）。
    if let Err(reason) = request.gate.check() {
        runs.clear(&request.scope_id, &request.marker);
        terminal.release();
        return Err(reason);
    }
    let started = Instant::now();
    // 写入包装脚本（含前置清输入），末尾自己带回车。这条走 `WriteAiPayload`：
    // 会话任务的显示过滤器会记住它，包装脚本的回显和标记行不会显示给用户，
    // 显示的是"提示符 + 命令原文"的合成回显（AI-2d 第 2 节）。
    //
    // gate 跟着这条消息进会话的命令队列，会话任务真正写之前再核对（审计 C04）：等全局会话表、
    // 等队列空位、等前面的写入期间点了停止 / 移出了目标，这条就不写。
    let guard: Arc<dyn ExecGuard> = Arc::new(request.gate.clone());
    if let Err(reason) = state
        .write_ai_payload(
            &session_id,
            format!("{}\r", request.payload).into_bytes(),
            request.marker.clone(),
            request.command.clone(),
            guard,
        )
        .await
    {
        runs.clear(&request.scope_id, &request.marker);
        terminal.release();
        return Err(reason);
    }

    // 等结束标记：从环形缓冲的原始文本里找。
    let result = wait_for_marker(app, &session_id, &request.marker, request.wait).await;
    terminal.release();

    match &result {
        MarkerOutcome::Done { .. } | MarkerOutcome::Gone => {
            runs.clear(&request.scope_id, &request.marker);
        }
        MarkerOutcome::Timeout { .. } => {
            // AI-2e：告诉会话任务的显示过滤器"这条命令超时了"——它退出完整过滤
            //（不再压用户之后的输入），只留轻量过滤丢迟到的那一行结束标记。
            state
                .ai_command_timed_out(&session_id, request.marker.clone())
                .await;
            // 命令还在跑：留着记录（停止按钮还要用它发 Ctrl+C），起一个观察任务
            // 等它自己结束（迟到的结束标记）或会话断开再清。
            watch_late_finish(
                app.clone(),
                session_id.clone(),
                request.scope_id.clone(),
                request.marker.clone(),
            );
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    match result {
        MarkerOutcome::Done { output, exit_code } => Ok(TerminalExecCapture {
            output,
            exit_code: Some(exit_code),
            duration_ms,
            timed_out: false,
        }),
        MarkerOutcome::Timeout { output } => Ok(TerminalExecCapture {
            output,
            exit_code: None,
            duration_ms,
            timed_out: true,
        }),
        MarkerOutcome::Gone => Err("会话在命令结束前断开了".to_string()),
    }
}

/// 出错提前退出时把终端占位还回去（正常路径由 `run_in_terminal_on` 自己 release）。
///
/// 目标可能是真实会话 ID，也可能是 `host-<id>`；两种都解析一下。没抢到过占位的
/// 目标拿到的是 `None`，或者 release 一个本来就空闲的终端，都没有副作用。
async fn release_terminal(state: &AppState, target_id: &str) {
    let session_id = if state.terminal_exec(target_id).await.is_some() {
        Some(target_id.to_string())
    } else {
        match parse_host_target_id(target_id) {
            Some(host_id) => state.session_for_host(host_id).await,
            None => None,
        }
    };
    if let Some(session_id) = session_id {
        if let Some(terminal) = state.terminal_exec(&session_id).await {
            terminal.release();
        }
    }
}

/// 超时返回之后命令还在跑：盯住环形缓冲，等它的结束标记出现（或会话断开）就把
/// [`TerminalRuns`] 的记录清掉，免得停止按钮把 Ctrl+C 发给一条早就结束的命令
/// （那会打断用户自己正在跑的东西）。
///
/// 上限半小时：真跑这么久的命令会让记录一直留着，停止按钮继续对它有效，
/// 到点之后不再观察（记录留着也没有副作用——会话断开时还会清）。
fn watch_late_finish(app: AppHandle, session_id: String, scope_id: String, marker: String) {
    tauri::async_runtime::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let runs = app.state::<TerminalRuns>();
            // 记录已经被别人清掉（比如新命令覆盖 / 会话断开）就不用看了。
            if runs.running(&scope_id).map(|run| run.marker) != Some(marker.clone()) {
                return;
            }
            let state = app.state::<AppState>();
            let Some(ring) = state.terminal_ring(&session_id).await else {
                runs.clear(&scope_id, &marker);
                return;
            };
            let raw = ring.lock().map(|r| r.raw_text()).unwrap_or_default();
            if zssh_core::ai::pty::find_end_marker(&raw, &marker).is_some()
                || Instant::now() >= deadline
            {
                runs.clear(&scope_id, &marker);
                return;
            }
        }
    });
}

/// 探测远端登录 shell 的类型（AI-2d 1.3，移植自 Netcatty 的
/// `sessionShellKind.cjs`）：在后台 exec 通道里跑一小段 POSIX 脚本，拿回登录
/// shell 路径再分类。探测失败 / 超时 / 认不出一律 `Other`（走后台通道）。
async fn probe_remote_shell_kind(state: &AppState, session_id: &str) -> ShellKind {
    use zssh_core::ai::shell_kind::{
        build_shell_probe_command, parse_shell_probe_output, SHELL_PROBE_TIMEOUT,
    };
    let Some(connection) = state.exec_connection(session_id).await else {
        return ShellKind::Other;
    };
    let command = build_shell_probe_command();
    // exec_capture 自己带 wait 超时；外面再包一层，防止通道卡住不返回。
    let capture = match tokio::time::timeout(
        SHELL_PROBE_TIMEOUT + Duration::from_secs(2),
        connection.exec_capture(&command, SHELL_PROBE_TIMEOUT, 64 * 1024),
    )
    .await
    {
        Ok(Ok(capture)) => capture,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "远端 shell 探测失败，走后台通道");
            return ShellKind::Other;
        }
        Err(_) => {
            tracing::debug!("远端 shell 探测超时，走后台通道");
            return ShellKind::Other;
        }
    };
    parse_shell_probe_output(&capture.stdout).unwrap_or(ShellKind::Other)
}

/// 在环形缓冲里等结束标记。
enum MarkerOutcome {
    Done { output: String, exit_code: i32 },
    Timeout { output: String },
    Gone,
}

/// 轮询会话的原始终端输出，直到出现结束标记（或超时 / 会话断开）。
async fn wait_for_marker(
    app: &AppHandle,
    session_id: &str,
    marker: &str,
    wait: Duration,
) -> MarkerOutcome {
    use zssh_core::ai::pty::{extract_output, find_end_marker};
    let deadline = Instant::now() + wait;
    loop {
        let state = app.state::<AppState>();
        let Some(ring) = state.terminal_ring(session_id).await else {
            return MarkerOutcome::Gone;
        };
        let raw = ring.lock().map(|r| r.raw_text()).unwrap_or_default();
        if let Some(found) = find_end_marker(&raw, marker) {
            return MarkerOutcome::Done {
                output: extract_output(&raw, marker),
                exit_code: found.exit_code,
            };
        }
        if Instant::now() >= deadline {
            // 超时：命令不中断，把当前截到的输出带上。
            let body = strip_marker_noise(&raw, marker);
            return MarkerOutcome::Timeout { output: body };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 超时时把已经打出来的输出尽量整理一下（去掉包装脚本本身那几行）。
fn strip_marker_noise(raw: &str, marker: &str) -> String {
    use zssh_core::ai::pty::strip_ansi;
    let start_pattern = format!("{marker}_S");
    let body = match raw.rfind(&start_pattern) {
        Some(pos) => raw[pos..]
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or(""),
        None => raw,
    };
    strip_ansi(body).trim().to_string()
}

/// 审计写主库（`ai_audit` 表）。
pub struct StoreAudit {
    app: AppHandle,
}

impl StoreAudit {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl AuditLog for StoreAudit {
    fn record(&self, entry: AuditEntry) {
        self.app.state::<AppState>().store.record(entry);
    }
}

/// 执行模式实时读设置项 `ai_exec_mode`（默认 manual）。
pub struct StoreExecMode {
    app: AppHandle,
}

impl StoreExecMode {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ExecModeProvider for StoreExecMode {
    fn mode(&self) -> ExecMode {
        let raw = self.app.state::<AppState>().store.ai_exec_mode().ok();
        ExecMode::from_setting(raw.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_payload_serializes_for_the_frontend() {
        let payload = AiApprovalPayload {
            req_id: 7,
            scope: "abc123".to_owned(),
            host_label: "web-1".to_owned(),
            command: "rm -rf /".to_owned(),
            danger: "dangerous".to_owned(),
            reasons: vec!["递归删除".to_owned()],
            tool: "run_command".to_owned(),
            file: None,
        };
        let value = serde_json::to_value(&payload).expect("序列化");
        assert_eq!(value["req_id"], 7);
        assert_eq!(value["scope"], "abc123");
        assert_eq!(value["host_label"], "web-1");
        assert_eq!(value["danger"], "dangerous");
        assert_eq!(value["reasons"][0], "递归删除");
        assert_eq!(value["tool"], "run_command");
        assert!(value["file"].is_null());
    }

    #[test]
    fn approval_payload_escapes_invisible_control_characters_in_the_command() {
        // 第二道防线（R-8 第 1 节）：万一有不可见字符走到审批，卡片上要显示成可见的
        // 转义形式，而不是让用户看到一个和实际执行内容不一致的命令。
        let request = ApprovalRequest {
            scope: "abc123".to_owned(),
            host_label: "web-1".to_owned(),
            command: "ls\x15rm -rf ~\u{202e}".to_owned(),
            danger: zssh_core::ai::safety::DangerLevel::Dangerous,
            reasons: vec!["递归删除".to_owned()],
            file: None,
        };
        let payload = AiApprovalPayload::from_request(9, request);
        assert_eq!(payload.command, "ls\\x15rm -rf ~\\u{202e}");
        assert!(
            !payload
                .command
                .chars()
                .any(zssh_core::ai::command_text::is_forbidden_control_char),
            "卡片文本里不能留下不可见字符：{:?}",
            payload.command
        );
        assert_eq!(payload.tool, "run_command");

        // 前端从事件里拿到的也是转义后的文本。
        let value = serde_json::to_value(&payload).expect("序列化");
        assert_eq!(value["command"], "ls\\x15rm -rf ~\\u{202e}");
    }

    #[test]
    fn write_file_approval_payload_carries_the_diff() {
        let diff = zssh_core::ai::files::make_diff(
            Some(
                "a
b
",
            ),
            "a
c
",
        );
        let payload = AiApprovalPayload {
            req_id: 8,
            scope: "abc123".to_owned(),
            host_label: "web-1".to_owned(),
            command: "write_file /etc/hosts".to_owned(),
            danger: "dangerous".to_owned(),
            reasons: vec!["写入系统路径：系统配置目录 /etc".to_owned()],
            tool: "write_file".to_owned(),
            file: Some(AiApprovalFile {
                path: "/etc/hosts".to_owned(),
                is_new: false,
                new_bytes: 4,
                diff,
            }),
        };
        let value = serde_json::to_value(&payload).expect("序列化");
        assert_eq!(value["tool"], "write_file");
        assert_eq!(value["file"]["path"], "/etc/hosts");
        assert_eq!(value["file"]["is_new"], false);
        assert_eq!(value["file"]["diff"]["added"], 1);
        assert_eq!(value["file"]["diff"]["removed"], 1);
        assert_eq!(value["file"]["diff"]["lines"][0]["kind"], "hunk");
    }

    #[tokio::test]
    async fn register_and_answer_resolves_the_waiter() {
        let approvals = AiApprovals::default();
        let (req_id, receiver) = approvals.register();
        assert!(approvals.is_pending(req_id), "登记之后要在表里");
        assert!(approvals.answer(req_id, true), "请求在表里，答案要送出去");
        assert!(!approvals.is_pending(req_id), "答完就要摘掉");
        assert_eq!(receiver.await, Ok(true));
    }

    #[tokio::test]
    async fn answering_an_unknown_request_fails() {
        let approvals = AiApprovals::default();
        assert!(!approvals.answer(42, true), "没有这个请求");
        assert!(!approvals.is_pending(42));
    }

    #[tokio::test]
    async fn dropping_the_guard_clears_the_pending_request() {
        let approvals = Arc::new(AiApprovals::default());
        let (req_id, receiver) = approvals.register();
        let guard = PendingApproval {
            approvals: Arc::clone(&approvals),
            req_id,
        };
        assert!(approvals.is_pending(req_id));

        // 审批 future 被丢掉了（超时 / scope 销毁）：请求要立刻摘掉
        drop(guard);
        assert!(
            !approvals.is_pending(req_id),
            "future 结束不能留下挂着的请求"
        );
        drop(receiver);
        assert!(!approvals.answer(req_id, false), "已经摘掉的请求答不了");
    }

    #[tokio::test]
    async fn target_connect_resolves_the_waiter() {
        let connects = Arc::new(TargetConnects::default());
        let rx = connects.register(7);
        assert!(connects.is_pending(7));
        // 界面连上后回话：等的一端拿到会话 ID。
        let tx = connects.take(7).expect("在等这个主机");
        tx.send("sess-9".to_string()).unwrap();
        assert!(!connects.is_pending(7));
        assert_eq!(rx.await, Ok("sess-9".to_string()));
    }

    #[tokio::test]
    async fn target_connect_cancel_drops_the_waiter() {
        let connects = TargetConnects::default();
        let rx = connects.register(3);
        connects.take(3); // 界面取消
        assert!(!connects.is_pending(3));
        assert!(rx.await.is_err(), "取消之后等的一端应该收到断开");
    }
}
