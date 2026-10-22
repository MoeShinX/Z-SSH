// Adapted from Netcatty electron/bridges/aiBridge/codexAppServer/connection.cjs and
// runtime.cjs (JSON-RPC handshake, shared process, per-thread bookkeeping, interrupt and
// approval plumbing), rewritten in Rust for Z-SSH.
//! codex 驱动（卡片 AI-3 第 1 / 4 节）：所有对话共用一个 `codex app-server` 进程。
//!
//! 传输是 stdin/stdout 上的 JSON-RPC（一行一个对象）。一个 app-server 进程里每个对话
//! 是一个 **thread**；thread 的系统提示（`developerInstructions`）里带这个对话的
//! 能力密钥，模型调 `zssh` 工具时必须把它当 `capability` 参数带上（实测 0.162.1：
//! 模型会照做）。对话之间靠密钥隔离（桥侧按密钥找会话表），不是靠进程隔离。
//!
//! 进程生命周期和 opencode 一样：第一次用 AI 时启动，所有对话空闲
//! [`DEFAULT_IDLE_TIMEOUT`]（10 分钟）后自动关闭，下次再启动；应用退出时杀进程树。
//!
//! 启动参数（禁用本机工具、只挂 `zssh` 一个 MCP）在 [`startup`] 里组，见该模块注释。
//!
//! 审批：codex 在 `approvalPolicy: "on-request"` 下会把"执行本机命令 / 改文件 / 要权限"
//! 作为**服务端请求**发过来，由 [`approval::decide`] 一律拒绝（第 2 节第 3 层）；
//! `zssh` 服务的工具调用在配置里已设 `default_tools_approval_mode: "approve"` 自动
//! 放行，不在这层重复弹窗。

pub mod approval;
pub mod policy;
pub mod protocol;
pub mod startup;
pub mod translate;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::config::McpServerSpec;
use super::discovery::{self, AgentInstall};
use super::process::{
    drain_tail_lines, idle_tick, kill_process_tree, tail_buf, tail_text, TailBuf, TAIL_LINES,
};
use super::{AgentConversation, AgentError, AgentEvent, AgentModel, AgentModelCatalog};
use approval::Decision;
use protocol::{method, ErrorResponse, Incoming, Request, Response, RpcError};
use startup::StartupPlan;
use translate::{TranslateState, TurnStats};

/// 默认启动超时（codex app-server 冷启动很快，留足余量）。
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// 默认空闲关闭时间：所有对话空闲 10 分钟后关掉共用进程。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 普通请求的超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 打断回合的超时（打断不成就报错，回合自己会结束）。
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(5);

/// 等 MCP 启动状态的最长时间（AI-3 验收 0.2；超时按失败关闭处理）。
const MCP_GATE_TIMEOUT: Duration = Duration::from_secs(30);

/// MCP 服务的启动核对状态（AI-3 验收 0.2）。
///
/// 只有先确认"起来的 MCP 服务只有 `zssh`"，才放行第一个回合——不然在核对的窗口期里
/// 模型已经能调用用户自己配的服务（`my_mcp_server` 这类能操控电脑的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpGate {
    /// 还在等启动状态：回合不放行。
    Pending,
    /// `zssh` 起来了、没有别的服务：可以开始回合。
    Ready,
    /// 有 Z-SSH 以外的服务：进程已被结束，之后所有回合一律拒绝（带原因）。
    Violation(String),
}

/// 收到一条 `mcpServer/startupStatus/updated` 后 gate 应该变成什么。
///
/// - 出现 `zssh` 以外的服务（starting / ready）→ 违规（带上要显示的原因）；
/// - `zssh` 自己 ready（或 failed——它没起来只是工具用不了，没有别的服务被加载）
///   → Ready；
/// - 其余（比如 `zssh` 还在 starting）→ 不变。
pub fn next_mcp_gate(current: &McpGate, ours: &str, name: &str, status: &str) -> Option<McpGate> {
    if matches!(current, McpGate::Violation(_)) {
        return None;
    }
    let active = matches!(status, "starting" | "ready");
    if name != ours && active {
        return Some(McpGate::Violation(format!(
            "codex 加载了 Z-SSH 以外的 MCP 服务（{name}），已停止"
        )));
    }
    if name == ours && matches!(status, "ready" | "failed") {
        return Some(McpGate::Ready);
    }
    None
}

/// 启动核对（UI-1 0.3）：`mcpServerStatus/list` 里除 `zssh` 以外，
/// 只允许**我们明确禁用过**的服务名出现，而且它们只能停在"配置里存在"的样子——
/// 任何运行时痕迹（已就绪 / 有工具目录 / 启动失败）都算违规。
///
/// 为什么要这一道：`mcpServer/startupStatus/updated` 是异步的，别的服务的
/// `starting` 通知可能比 `zssh` 的 `ready` 晚到；只靠通知的话，模型在窗口期里
/// 已经能调用那个服务。放行第一个回合之前查一次完整列表，漏网的服务在这里就被抓住。
///
/// 字段语义（0.162.1 实测）：已就绪 = `serverInfo` 非空且有 `tools`；启动失败 =
/// `toolsError` 有文本；被禁用 = 全空（和"还在启动中"在列表里长得一样，
/// 所以名单本身是第一道判据）。
pub fn mcp_status_violation(entries: &[Value], ours: &str, disabled: &[String]) -> Option<String> {
    for entry in entries {
        let name = entry["name"].as_str().unwrap_or("");
        if name.is_empty() || name == ours {
            continue;
        }
        if !disabled.iter().any(|known| known == name) {
            return Some(format!(
                "codex 里出现了没有禁用的 MCP 服务（{name}），已停止"
            ));
        }
        let ready = !entry["serverInfo"].is_null();
        let has_tools = entry["tools"]
            .as_object()
            .is_some_and(|tools| !tools.is_empty());
        let failed = entry["toolsError"]
            .as_str()
            .is_some_and(|error| !error.is_empty());
        if ready || has_tools || failed {
            return Some(format!(
                "codex 里被禁用的 MCP 服务（{name}）仍然启动了，已停止"
            ));
        }
    }
    None
}

/// 空回复时给用户的提示。
const EMPTY_RESPONSE_HINT: &str =
    "codex 没有返回任何内容。请先在终端运行 `codex login` 完成登录，然后再试。";

/// 启动参数（进程级：所有对话共用）。
pub struct StartOptions {
    /// 找到的 codex 安装。
    pub install: AgentInstall,
    /// 工作目录（`<数据目录>\agent-workspace`）。
    pub workspace: PathBuf,
    /// 挂给 codex 的 `zssh` MCP 服务（命令 + 环境变量）。
    pub mcp: McpServerSpec,
    /// 启动前跑的探测（禁用清单过滤、用户 MCP 服务枚举），测试里可以注入假的。
    pub probes: Arc<dyn StartupProbes>,
    /// 覆盖 codex 的配置目录（`CODEX_HOME`）。`None` = 用户自己的（登录和模型配置
    /// 都在那里）；联网测试用独立的副本时设置它，探测扫的 config.toml 也看同一份。
    pub codex_home: Option<PathBuf>,
    /// 启动超时。
    pub startup_timeout: Duration,
}

/// 启动前的探测接口（真实现跑 `codex features list` + 只读扫用户的 `config.toml`，
/// 见 [`startup::CommandProbes`]；单测注入假的）。
pub trait StartupProbes: Send + Sync {
    /// 可用的特性名（禁用清单取交集，未知名字会让 codex 启动失败）。
    fn available_features(&self) -> crate::ssh::BoxFuture<'_, Vec<String>>;
    /// 用户配置里的 MCP 服务名（要逐个 `enabled=false`）。
    ///
    /// **读不出来时返回 `Err`**：返回空表等于"用户没配 MCP"，本该禁用的服务就会
    /// 照常加载；读不出来就拒绝启动（R-2 第 3 节）。
    fn configured_mcp_servers(&self) -> crate::ssh::BoxFuture<'_, Result<Vec<String>, String>>;
}

/// 一个 thread（= 一个对话）的内部状态。
struct ThreadEntry {
    events: mpsc::UnboundedSender<AgentEvent>,
    inner: Mutex<ThreadInner>,
    /// 当前有没有回合在跑。
    busy: AtomicBool,
    /// 正在跑的回合 id（停止按钮用它）。
    turn_id: Mutex<Option<String>>,
    /// 停止请求比回合 id 先到（send 拿到 id 后补打断）。
    stop_requested: AtomicBool,
}

struct ThreadInner {
    state: TranslateState,
    turn: TurnStats,
}

impl ThreadEntry {
    fn emit(&self, events: Vec<AgentEvent>) {
        for event in events {
            let _ = self.events.send(event);
        }
    }
}

/// 进程级共享状态。
struct Shared {
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    child: Mutex<Option<tokio::process::Child>>,
    /// 我们发出去的请求：id → 应答通道。
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>,
    next_id: AtomicU64,
    threads: Mutex<HashMap<String, Arc<ThreadEntry>>>,
    /// 启动过的 MCP 服务名（`mcpServer/startupStatus/updated`；诊断 / 隔离实测用）。
    mcp_started: Mutex<Vec<String>>,
    /// MCP 启动核对（AI-3 验收 0.2）：Ready 之前不放行回合。
    mcp_gate: watch::Sender<McpGate>,
    /// 我们自己的 MCP 服务名（`zssh`）。
    mcp_name: String,
    /// 我们明确禁用掉的 MCP 服务名（启动核对：列表里只允许这些 + zssh）。
    mcp_disabled: Vec<String>,
    /// 放行第一个回合前的 `mcpServerStatus/list` 核对只做一次。
    mcp_status_checked: AtomicBool,
    workspace: PathBuf,
    last_activity: Mutex<Instant>,
    closed: AtomicBool,
    stderr_tail: TailBuf,
}

impl Shared {
    fn thread(&self, thread_id: &str) -> Option<Arc<ThreadEntry>> {
        self.threads
            .lock()
            .ok()
            .and_then(|map| map.get(thread_id).cloned())
    }

    fn touch(&self) {
        if let Ok(mut at) = self.last_activity.lock() {
            *at = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|at| at.elapsed())
            .unwrap_or_default()
    }

    fn any_busy(&self) -> bool {
        self.threads
            .lock()
            .map(|map| map.values().any(|t| t.busy.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }

    fn thread_count(&self) -> usize {
        self.threads.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// 写一行 JSON 到 app-server 的 stdin。
    async fn write_line(&self, value: &Value) -> Result<(), AgentError> {
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(AgentError::Transport(
                "codex app-server 的输入已关闭".into(),
            ));
        };
        let mut line = serde_json::to_string(value)
            .map_err(|e| AgentError::Transport(format!("消息序列化失败：{e}")))?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AgentError::Transport(format!("写入失败：{e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Transport(format!("写入失败：{e}")))
    }

    /// 发一个请求、等应答。
    async fn request(
        &self,
        method_name: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, AgentError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(AgentError::Transport(
                "codex 事件流已断开，请新开一个对话".to_string(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, tx);
        }
        let message = serde_json::to_value(Request {
            id,
            method: method_name,
            params,
        })
        .map_err(|e| AgentError::Transport(format!("消息序列化失败：{e}")))?;
        if let Err(err) = self.write_line(&message).await {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            return Err(err);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(AgentError::Transport(format!(
                "{method_name} 失败：{}",
                error.message
            ))),
            // 发送端没了（进程退出）。
            Ok(Err(_)) => Err(AgentError::Transport("codex app-server 连接断开".into())),
            Err(_) => {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                Err(AgentError::Transport(format!("{method_name} 超时")))
            }
        }
    }

    /// 发一个通知（不需要应答）。
    async fn notify(&self, method_name: &str, params: Value) -> Result<(), AgentError> {
        let message = serde_json::to_value(protocol::OutNotification {
            method: method_name,
            params,
        })
        .map_err(|e| AgentError::Transport(format!("消息序列化失败：{e}")))?;
        self.write_line(&message).await
    }

    /// 连接断开：叫醒所有等应答的请求。
    fn fail_all_pending(&self, reason: &str) {
        if let Ok(mut map) = self.pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(Err(RpcError {
                    code: -32000,
                    message: reason.to_string(),
                    data: None,
                }));
            }
        }
    }

    /// 判定并执行"违规"：先关门（后续 send 全拒），再杀掉进程树，再通知所有对话。
    ///
    /// 返回 true 表示这次真的触发了（用于去重）。调用方已经写好 gate。
    async fn raise_violation(&self, message: &str) {
        tracing::error!(%message, "codex 加载了非预期的 MCP 服务，结束进程");
        self.closed.store(true, Ordering::SeqCst);
        self.fail_all_pending(message);
        self.kill_process().await;
        if let Ok(threads) = self.threads.lock() {
            for thread in threads.values() {
                thread.emit(vec![AgentEvent::Error {
                    message: message.to_string(),
                }]);
            }
        }
    }

    /// 杀掉整个进程树。
    async fn kill_process(&self) {
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut child) = child {
            let _ = kill_process_tree(&mut child).await;
        }
    }
}

/// 共用的 codex app-server 运行时。所有对话都从这里开。
pub struct CodexRuntime {
    shared: Arc<Shared>,
    reader_task: Mutex<Option<JoinHandle<()>>>,
    shutdown_tx: watch::Sender<bool>,
    idle_task: Mutex<Option<JoinHandle<()>>>,
}

impl CodexRuntime {
    /// 启动进程、握手、挂事件循环和空闲看护。
    pub async fn start(
        options: StartOptions,
        idle_timeout: Duration,
    ) -> Result<Arc<CodexRuntime>, AgentError> {
        std::fs::create_dir_all(&options.workspace)
            .map_err(|e| AgentError::StartFailed(format!("创建工作目录失败：{e}")))?;

        // 启动参数：禁用清单（按可用特性过滤）+ 只挂 zssh 的 MCP 覆盖（见 startup 模块）。
        let plan = startup::startup_plan(&options)
            .await
            .map_err(AgentError::StartFailed)?;
        let StartupPlan {
            args,
            disabled_servers,
        } = plan;

        let mut cmd = discovery::build_command(std::path::Path::new(&options.install.path));
        cmd.args(&args)
            .current_dir(&options.workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(home) = &options.codex_home {
            cmd.env("CODEX_HOME", home);
        }
        discovery::hide_console(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| AgentError::StartFailed(format!("启动进程失败：{e}")))?;
        // 崩溃时也要带走整棵子进程树（K06）
        super::process::bind_to_app_lifetime(&child);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stderr_tail = tail_buf();
        if let Some(stderr) = stderr {
            let _drain =
                drain_tail_lines(stderr, Arc::clone(&stderr_tail), "codex stderr", TAIL_LINES);
        }

        let (mcp_gate_tx, _mcp_gate_rx) = watch::channel(McpGate::Pending);
        let shared = Arc::new(Shared {
            stdin: tokio::sync::Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            threads: Mutex::new(HashMap::new()),
            mcp_started: Mutex::new(Vec::new()),
            mcp_gate: mcp_gate_tx,
            mcp_name: options.mcp.name.clone(),
            mcp_disabled: disabled_servers,
            mcp_status_checked: AtomicBool::new(false),
            workspace: options.workspace.clone(),
            last_activity: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            stderr_tail,
        });

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let reader_task = match stdout {
            Some(stdout) => tokio::spawn(read_loop(Arc::clone(&shared), stdout)),
            None => {
                shared.closed.store(true, Ordering::SeqCst);
                return Err(AgentError::StartFailed(
                    "拿不到 app-server 的输出管道".into(),
                ));
            }
        };

        // 握手：initialize → initialized。版本以进程自报的为准，低于最低版本直接拒绝。
        let init = shared
            .request(
                method::INITIALIZE,
                protocol::initialize_params("zssh", env!("CARGO_PKG_VERSION")),
                options.startup_timeout,
            )
            .await;
        match init {
            Ok(value) => {
                let version =
                    startup::parse_agent_version(value["userAgent"].as_str().unwrap_or_default());
                if startup::version_too_old(version, startup::MIN_VERSION) {
                    shared.closed.store(true, Ordering::SeqCst);
                    shared.kill_process().await;
                    reader_task.abort();
                    let shown = value["userAgent"].as_str().unwrap_or("未知版本");
                    return Err(AgentError::StartFailed(format!(
                        "codex 版本过低（{shown}）：需要 {} 及以上，请先 `codex update`。",
                        startup::format_version(startup::MIN_VERSION)
                    )));
                }
            }
            Err(err) => {
                let detail = tail_text(&tail_buf(), &shared.stderr_tail);
                shared.closed.store(true, Ordering::SeqCst);
                shared.kill_process().await;
                reader_task.abort();
                return Err(AgentError::StartFailed(format!("{err}{detail}")));
            }
        }
        if let Err(err) = shared.notify(method::INITIALIZED, json!({})).await {
            shared.closed.store(true, Ordering::SeqCst);
            shared.kill_process().await;
            reader_task.abort();
            return Err(err);
        }
        tracing::debug!("codex app-server 已就绪");

        let idle_task = tokio::spawn(idle_watch(
            Arc::clone(&shared),
            idle_timeout,
            shutdown_tx.subscribe(),
            shutdown_tx.clone(),
        ));

        Ok(Arc::new(CodexRuntime {
            shared,
            reader_task: Mutex::new(Some(reader_task)),
            shutdown_tx,
            idle_task: Mutex::new(Some(idle_task)),
        }))
    }

    /// 进程号（诊断用）。
    pub fn pid(&self) -> Option<u32> {
        self.shared
            .child
            .lock()
            .ok()
            .and_then(|c| c.as_ref().and_then(|c| c.id()))
    }

    /// 进程是否已经关停（空闲超时或连接断开）。
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// 当前挂着的对话数。
    pub fn thread_count(&self) -> usize {
        self.shared.thread_count()
    }

    /// 问一次 codex 的 MCP 服务列表，并按"隔离有没有被破坏"核对（诊断 / 测试用）。
    ///
    /// 生产路径用的是放行第一个回合前那次核对（[`Self::check_mcp_server_status`]），
    /// 这里只是把它抽出来给测试和排障用：返回（列表里的服务名，违规说明）。
    pub async fn mcp_status_snapshot(&self) -> Result<(Vec<String>, Option<String>), AgentError> {
        let shared = &self.shared;
        let value = shared
            .request(method::MCP_SERVER_STATUS_LIST, json!({}), REQUEST_TIMEOUT)
            .await?;
        let entries = value["data"].as_array().cloned().unwrap_or_default();
        let names = entries
            .iter()
            .filter_map(|entry| entry["name"].as_str().map(str::to_string))
            .collect();
        Ok((
            names,
            mcp_status_violation(&entries, &shared.mcp_name, &shared.mcp_disabled),
        ))
    }

    /// 启动过的 MCP 服务名（按首次启动顺序；隔离实测断言只剩 `zssh`）。
    pub fn started_mcp_servers(&self) -> Vec<String> {
        self.shared
            .mcp_started
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }

    /// 问一次账户状态（`account/read`）：登录了返回 true。
    ///
    /// 只判断"有没有登录"，**账号信息（邮箱等）一律不读、不带出**（AI-3 第 5 节）。
    pub async fn account_logged_in(&self) -> Result<bool, AgentError> {
        let value = self
            .shared
            .request(method::ACCOUNT_READ, json!({}), REQUEST_TIMEOUT)
            .await?;
        Ok(!value["account"].is_null())
    }

    /// 查模型目录（不需要先开对话：面板一打开就要显示默认模型）。
    pub async fn models(&self) -> Result<AgentModelCatalog, AgentError> {
        self.shared.touch();
        fetch_models(&self.shared).await
    }

    /// 在这个进程里开一个新对话：建 thread、登记、返回对话句柄。
    ///
    /// `capability` 是这个对话的能力密钥，写进 thread 的 developerInstructions
    /// （实测模型会把它带进 `zssh` 工具调用的 `capability` 参数）。
    pub async fn open_conversation(
        self: &Arc<Self>,
        capability: String,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Arc<CodexConversation>, AgentError> {
        self.open_conversation_resuming(capability, events, None)
            .await
            .map(|(conversation, _)| conversation)
    }

    /// 同上，`resume` 给了就先 `thread/resume` 那个 thread（历史会话续接）；续不上
    /// （thread 不在了、协议报错）就 `thread/start` 新开。返回值的 bool = 是否接上了。
    pub async fn open_conversation_resuming(
        self: &Arc<Self>,
        capability: String,
        events: mpsc::UnboundedSender<AgentEvent>,
        resume: Option<&str>,
    ) -> Result<(Arc<CodexConversation>, bool), AgentError> {
        if self.is_closed() {
            return Err(AgentError::Transport("codex 进程已关闭".into()));
        }
        let cwd = self.shared.workspace.to_string_lossy().into_owned();
        let instructions = system_prompt(&capability);
        let mut resumed = None;
        if let Some(id) = resume.filter(|id| !id.trim().is_empty()) {
            match self
                .shared
                .request(
                    method::THREAD_RESUME,
                    protocol::thread_resume_params(id, &cwd, &instructions),
                    REQUEST_TIMEOUT,
                )
                .await
            {
                Ok(value) => resumed = value["thread"]["id"].as_str().map(str::to_string),
                Err(err) => {
                    tracing::info!(error = %err, "codex 续接历史 thread 失败，改用新 thread")
                }
            }
        }
        let was_resumed = resumed.is_some();
        let thread_id = match resumed {
            Some(id) => id,
            None => {
                let value = self
                    .shared
                    .request(
                        method::THREAD_START,
                        protocol::thread_start_params(&cwd, None, &instructions),
                        REQUEST_TIMEOUT,
                    )
                    .await?;
                value["thread"]["id"]
                    .as_str()
                    .ok_or_else(|| AgentError::Transport("thread/start 没有返回 thread id".into()))?
                    .to_string()
            }
        };
        let entry = Arc::new(ThreadEntry {
            events,
            inner: Mutex::new(ThreadInner {
                state: TranslateState::default(),
                turn: TurnStats::default(),
            }),
            busy: AtomicBool::new(false),
            turn_id: Mutex::new(None),
            stop_requested: AtomicBool::new(false),
        });
        if let Ok(mut map) = self.shared.threads.lock() {
            map.insert(thread_id.clone(), Arc::clone(&entry));
        }
        self.shared.touch();
        Ok((
            Arc::new(CodexConversation {
                runtime: Arc::clone(self),
                thread_id,
                capability,
                entry,
            }),
            was_resumed,
        ))
    }

    /// 关掉进程：停事件循环、杀进程树。
    pub async fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
        let reader = self.reader_task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = reader {
            task.abort();
        }
        let idle = self.idle_task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = idle {
            task.abort();
        }
        // 关掉 stdin（app-server 读到 EOF 会自己退出），再兜底杀树。
        if let Ok(mut guard) = self.shared.stdin.try_lock() {
            guard.take();
        }
        self.shared.kill_process().await;
    }
}

impl Drop for CodexRuntime {
    fn drop(&mut self) {
        // 兜底：没走 shutdown 就丢了句柄，也要把进程树杀掉，不留孤儿。
        let _ = self.shutdown_tx.send(true);
        let child = self.shared.child.lock().ok().and_then(|mut c| c.take());
        if let Some(child) = child {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}

/// 一个对话的句柄（共用一个进程）。
pub struct CodexConversation {
    runtime: Arc<CodexRuntime>,
    thread_id: String,
    /// 本对话的能力密钥（写进 thread 的指令）。
    capability: String,
    entry: Arc<ThreadEntry>,
}

impl CodexConversation {
    /// 进程号（诊断用，取的是共用进程的）。
    pub fn pid(&self) -> Option<u32> {
        self.runtime.pid()
    }

    /// 当前有没有回合在跑。
    pub fn is_busy(&self) -> bool {
        self.entry.busy.load(Ordering::SeqCst)
    }

    /// 本对话的能力密钥。
    pub fn capability(&self) -> &str {
        &self.capability
    }

    fn detach(&self) {
        if let Ok(mut map) = self.runtime.shared.threads.lock() {
            map.remove(&self.thread_id);
        }
    }

    /// 等"MCP 里只有 zssh"确认完毕；违规 / 超时都按失败关闭拒绝回合。
    ///
    /// 通知握手之后（gate 变 Ready）**再查一次完整列表**（UI-1 0.3）：别的服务的
    /// `starting` 通知可能比 `zssh` 的 `ready` 晚到，只靠通知的话放行第一个回合时
    /// 它已经能被模型调用了。这次核对每个进程只做一遍。
    async fn await_mcp_ready(&self) -> Result<(), AgentError> {
        let mut rx = self.runtime.shared.mcp_gate.subscribe();
        let deadline = Instant::now() + MCP_GATE_TIMEOUT;
        loop {
            match rx.borrow().clone() {
                McpGate::Ready => break,
                McpGate::Violation(message) => return Err(AgentError::Transport(message)),
                McpGate::Pending => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AgentError::Transport(startup::startup_refusal(
                    "等待 MCP 启动状态超时",
                )));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(_)) => {
                    return Err(AgentError::Transport(
                        "codex 事件流已断开，请新开一个对话".to_string(),
                    ))
                }
                Err(_) => {
                    return Err(AgentError::Transport(startup::startup_refusal(
                        "等待 MCP 启动状态超时",
                    )))
                }
            }
        }
        self.check_mcp_server_status().await
    }

    /// 放行第一个回合前的完整列表核对（每个进程一次）。
    async fn check_mcp_server_status(&self) -> Result<(), AgentError> {
        let shared = &self.runtime.shared;
        if shared.mcp_status_checked.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let listed = shared
            .request(method::MCP_SERVER_STATUS_LIST, json!({}), REQUEST_TIMEOUT)
            .await;
        let violation = match listed {
            Ok(value) => {
                let entries = value["data"].as_array().cloned().unwrap_or_default();
                mcp_status_violation(&entries, &shared.mcp_name, &shared.mcp_disabled)
            }
            // 查不到列表：按失败关闭（宁可不让回合开始）。
            Err(err) => Some(startup::startup_refusal(&format!(
                "查 MCP 服务列表失败：{err}"
            ))),
        };
        let Some(message) = violation else {
            return Ok(());
        };
        shared
            .mcp_gate
            .send_replace(McpGate::Violation(message.clone()));
        shared.raise_violation(&message).await;
        Err(AgentError::Transport(message))
    }

    async fn send_inner(&self, text: &str, model: Option<&str>) -> Result<(), AgentError> {
        let shared = &self.runtime.shared;
        if shared.closed.load(Ordering::SeqCst) {
            if let McpGate::Violation(message) = shared.mcp_gate.borrow().clone() {
                return Err(AgentError::Transport(message));
            }
            return Err(AgentError::Transport(
                "codex 事件流已断开，请新开一个对话".to_string(),
            ));
        }
        // 确认 MCP 里只有 zssh 之后才放行回合（AI-3 验收 0.2）。
        self.await_mcp_ready().await?;
        if self.entry.busy.swap(true, Ordering::SeqCst) {
            return Err(AgentError::Busy);
        }
        shared.touch();
        if let Ok(mut inner) = self.entry.inner.lock() {
            inner.turn = TurnStats::default();
        }
        *self.entry.turn_id.lock().unwrap_or_else(|p| p.into_inner()) = None;

        let cwd = shared.workspace.to_string_lossy().into_owned();
        let (model_id, effort) = parse_model_ref(model);
        let params = protocol::turn_start_params(&self.thread_id, &cwd, text, model_id, effort);
        let result = shared
            .request(method::TURN_START, params, REQUEST_TIMEOUT)
            .await;
        match result {
            Ok(value) => {
                let turn_id = value["turn"]["id"].as_str().map(str::to_string);
                *self.entry.turn_id.lock().unwrap_or_else(|p| p.into_inner()) = turn_id.clone();
                // 停止请求早于回合 id 到达：现在补打断。
                if self.entry.stop_requested.swap(false, Ordering::SeqCst) {
                    if let Some(turn_id) = turn_id {
                        let _ = shared
                            .request(
                                method::TURN_INTERRUPT,
                                protocol::turn_interrupt_params(&self.thread_id, &turn_id),
                                INTERRUPT_TIMEOUT,
                            )
                            .await;
                    }
                }
                Ok(())
            }
            Err(err) => {
                self.entry.busy.store(false, Ordering::SeqCst);
                Err(err)
            }
        }
    }

    async fn stop_inner(&self) -> Result<(), AgentError> {
        if !self.entry.busy.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Ok(mut inner) = self.entry.inner.lock() {
            inner.turn.aborted = true;
        }
        let turn_id = self
            .entry
            .turn_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match turn_id {
            Some(turn_id) => self
                .runtime
                .shared
                .request(
                    method::TURN_INTERRUPT,
                    protocol::turn_interrupt_params(&self.thread_id, &turn_id),
                    INTERRUPT_TIMEOUT,
                )
                .await
                .map(|_| ()),
            None => {
                // 回合还在启动：标记一下，send 拿到 id 后立即打断。
                self.entry.stop_requested.store(true, Ordering::SeqCst);
                Ok(())
            }
        }
    }
}

impl AgentConversation for CodexConversation {
    fn session_id(&self) -> &str {
        &self.thread_id
    }

    fn models<'a>(&'a self) -> crate::ssh::BoxFuture<'a, Result<AgentModelCatalog, AgentError>> {
        Box::pin(async move { fetch_models(&self.runtime.shared).await })
    }

    fn send<'a>(
        &'a self,
        text: &'a str,
        model: Option<&'a str>,
    ) -> crate::ssh::BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move { self.send_inner(text, model).await })
    }

    fn stop<'a>(&'a self) -> crate::ssh::BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move { self.stop_inner().await })
    }

    fn shutdown<'a>(&'a self) -> crate::ssh::BoxFuture<'a, ()> {
        Box::pin(async move { self.detach() })
    }
}

impl Drop for CodexConversation {
    fn drop(&mut self) {
        self.detach();
    }
}

/// 交给模型的系统提示：说明能力密钥的用途（三个智能体共用一份）。
fn system_prompt(capability: &str) -> String {
    crate::ai::agent::config::system_prompt(capability)
}

/// 解析模型引用：`model` 或 `model/effort`（effort 是模型列表里给的思考强度）。
fn parse_model_ref(model: Option<&str>) -> (Option<&str>, Option<&str>) {
    match model {
        None => (None, None),
        Some(raw) => match raw.rsplit_once('/') {
            Some((id, effort)) if !id.is_empty() && !effort.is_empty() => (Some(id), Some(effort)),
            _ => (Some(raw), None),
        },
    }
}

/// 查模型列表（`model/list` 分页拉全；`hidden` 的不给用户看）。
async fn fetch_models(shared: &Shared) -> Result<AgentModelCatalog, AgentError> {
    let mut all: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let value = shared
            .request(
                method::MODEL_LIST,
                protocol::model_list_params(cursor.as_deref()),
                REQUEST_TIMEOUT,
            )
            .await?;
        if let Some(list) = value["data"].as_array() {
            all.extend(list.iter().cloned());
        }
        cursor = value["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(map_models(&all))
}

/// 把 `model/list` 的原始条目映射成界面用的模型表。
///
/// 每个模型把"思考强度"展开成独立选项（id 形如 `模型/effort`，`turn/start` 时拆开
/// 分别填 `model` 和 `effort`），默认项取 `isDefault` 那条。
pub fn map_models(raw: &[Value]) -> AgentModelCatalog {
    let mut models = Vec::new();
    let mut default = None;
    for entry in raw {
        let id = entry["id"].as_str().unwrap_or("");
        if id.is_empty() || entry["hidden"].as_bool() == Some(true) {
            continue;
        }
        let name = entry["displayName"].as_str().unwrap_or(id);
        let efforts: Vec<&str> = entry["supportedReasoningEfforts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|e| e["reasoningEffort"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let default_effort = entry["defaultReasoningEffort"].as_str();
        let is_default = entry["isDefault"].as_bool() == Some(true);
        if efforts.is_empty() {
            models.push(AgentModel {
                id: id.to_string(),
                name: name.to_string(),
                unavailable: false,
            });
            if is_default {
                default = Some(id.to_string());
            }
        } else {
            for effort in &efforts {
                models.push(AgentModel {
                    id: format!("{id}/{effort}"),
                    name: format!("{name} · {effort}"),
                    unavailable: false,
                });
                if is_default && Some(*effort) == default_effort {
                    default = Some(format!("{id}/{effort}"));
                }
            }
        }
    }
    // 默认项的显示名就是它在列表里的名字（`model/list` 自己标的 isDefault）。
    let default_label = default
        .as_ref()
        .and_then(|id| models.iter().find(|m| &m.id == id))
        .map(|m| m.name.clone());
    AgentModelCatalog {
        models,
        default,
        default_label,
    }
}

/// 读 stdout：分发应答 / 服务端请求 / 通知。
async fn read_loop(shared: Arc<Shared>, stdout: tokio::process::ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match Incoming::parse(&line) {
            Ok(Incoming::Response { id, result }) => {
                if let Ok(mut map) = shared.pending.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(Ok(result));
                    }
                }
            }
            Ok(Incoming::Error { id, error }) => {
                if let Ok(mut map) = shared.pending.lock() {
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(Err(error));
                    }
                }
            }
            Ok(Incoming::ServerRequest { id, method, params }) => {
                // 审批：一律按策略应答（本机命令 / 改文件拒绝，zssh 工具放行）。
                match approval::decide(&method, &params) {
                    Decision::Respond(result) => {
                        let message = serde_json::to_value(Response { id, result });
                        if let Ok(message) = message {
                            let _ = shared.write_line(&message).await;
                        }
                    }
                    Decision::Fail(error) => {
                        let message = serde_json::to_value(ErrorResponse { id, error });
                        if let Ok(message) = message {
                            let _ = shared.write_line(&message).await;
                        }
                    }
                }
            }
            Ok(Incoming::Notification { method, params }) => {
                handle_notification(&shared, &method, &params).await;
            }
            Err(err) => {
                tracing::debug!(%err, line = %line.chars().take(200).collect::<String>(), "codex 输出了解析不了的行");
            }
        }
    }
    // stdout 结束 = 进程退出了。
    tracing::debug!("codex app-server 的 stdout 结束");
    shared.closed.store(true, Ordering::SeqCst);
    shared.fail_all_pending("codex app-server 已退出");
}

/// 通知分发：按 threadId 找到对话，翻译成统一事件；顺带维护回合状态。
async fn handle_notification(shared: &Shared, method_name: &str, params: &Value) {
    // MCP 服务的启动状态是进程级信息（核对 / 诊断），不依赖 thread 登记。
    if method_name == "mcpServer/startupStatus/updated" {
        if let Some(name) = params["name"].as_str() {
            let status = params["status"].as_str().unwrap_or("");
            if matches!(status, "starting" | "ready") {
                if let Ok(mut names) = shared.mcp_started.lock() {
                    if !names.iter().any(|have| have == name) {
                        names.push(name.to_string());
                    }
                }
            }
            // 启动核对（AI-3 验收 0.2）：只有 zssh 能被加载。发现别的服务立刻结束
            // 进程——在确认之前 gate 是 Pending，没有任何回合被放行过。
            let next = next_mcp_gate(
                &shared.mcp_gate.borrow().clone(),
                &shared.mcp_name,
                name,
                status,
            );
            if let Some(next) = next {
                let violation = match &next {
                    McpGate::Violation(message) => Some(message.clone()),
                    _ => None,
                };
                // `send` 在没有接收者时会失败（启动函数返回后临时接收者会被丢弃），
                // 用 `send_replace` 保证值一定写进去（后面 subscribe 的都能看到）。
                shared.mcp_gate.send_replace(next);
                if let Some(message) = violation {
                    // 先关门再杀：并发的 send 会立刻拿到拒绝，不会走到 turn/start。
                    shared.raise_violation(&message).await;
                }
            }
        }
    }
    let Some(thread_id) = params["threadId"].as_str() else {
        return;
    };
    let Some(thread) = shared.thread(thread_id) else {
        return;
    };
    shared.touch();
    let events = {
        let mut inner = thread
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ThreadInner { state, turn } = &mut *inner;
        translate::translate(method_name, params, state, turn)
    };
    thread.emit(events);
    if method_name == method::TURN_COMPLETED {
        thread.busy.store(false, Ordering::SeqCst);
        *thread.turn_id.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

async fn idle_watch(
    shared: Arc<Shared>,
    idle_timeout: Duration,
    mut shutdown_rx: watch::Receiver<bool>,
    kill_tx: watch::Sender<bool>,
) {
    let tick = idle_tick(idle_timeout);
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => return,
            _ = tokio::time::sleep(tick) => {}
        }
        if *shutdown_rx.borrow() {
            return;
        }
        if shared.any_busy() {
            continue;
        }
        if shared.idle_for() >= idle_timeout {
            tracing::debug!(?idle_timeout, "codex 空闲超时，关闭共用进程");
            shared.closed.store(true, Ordering::SeqCst);
            let _ = kill_tx.send(true);
            shared.kill_process().await;
            return;
        }
    }
}

/// 空回复提示（上层在回合结束但没有内容时用）。
pub fn empty_response_hint() -> &'static str {
    EMPTY_RESPONSE_HINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_refs_split_into_id_and_effort() {
        assert_eq!(parse_model_ref(None), (None, None));
        assert_eq!(
            parse_model_ref(Some("gpt-6.1-sol")),
            (Some("gpt-6.1-sol"), None)
        );
        assert_eq!(
            parse_model_ref(Some("gpt-6.1-sol/high")),
            (Some("gpt-6.1-sol"), Some("high"))
        );
        // 名字里带斜杠的怪 id：只有最后一段当 effort（effort 是我们自己拼的）。
        assert_eq!(
            parse_model_ref(Some("vendor/model/high")),
            (Some("vendor/model"), Some("high"))
        );
    }

    #[test]
    fn models_expand_reasoning_efforts() {
        let raw = vec![
            json!({
                "id": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "hidden": false,
                "supportedReasoningEfforts": [
                    { "reasoningEffort": "low" }, { "reasoningEffort": "high" }
                ],
                "defaultReasoningEffort": "high",
                "isDefault": true
            }),
            json!({
                "id": "gpt-hidden", "displayName": "隐藏", "hidden": true,
                "supportedReasoningEfforts": []
            }),
            json!({
                "id": "plain-model", "displayName": "Plain", "hidden": false,
                "supportedReasoningEfforts": []
            }),
        ];
        let catalog = map_models(&raw);
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["gpt-6.1-sol/low", "gpt-6.1-sol/high", "plain-model"]
        );
        assert_eq!(catalog.models[0].name, "GPT-6.1-Sol · low");
        assert_eq!(catalog.default.as_deref(), Some("gpt-6.1-sol/high"));
        // "默认（…）"里显示的是默认项在列表里的名字。
        assert_eq!(catalog.default_label.as_deref(), Some("GPT-6.1-Sol · high"));
    }

    #[test]
    fn mcp_gate_transitions_fail_closed() {
        // zssh 还在启动：不放行。
        assert_eq!(
            next_mcp_gate(&McpGate::Pending, "zssh", "zssh", "starting"),
            None
        );
        // 别的服务起来：违规（带名字）。
        let violated =
            next_mcp_gate(&McpGate::Pending, "zssh", "my_mcp_server", "starting").expect("要违规");
        match violated {
            McpGate::Violation(message) => {
                assert!(message.contains("my_mcp_server"));
                assert!(message.contains("已停止"));
            }
            other => panic!("应是违规：{other:?}"),
        }
        // zssh 就绪：放行；失败也放行（它没起来只是工具用不了，没有别的服务加载）。
        assert_eq!(
            next_mcp_gate(&McpGate::Pending, "zssh", "zssh", "ready"),
            Some(McpGate::Ready)
        );
        assert_eq!(
            next_mcp_gate(&McpGate::Pending, "zssh", "zssh", "failed"),
            Some(McpGate::Ready)
        );
        // 违规之后不再改变（保持关着）。
        let violation = McpGate::Violation("x".into());
        assert_eq!(next_mcp_gate(&violation, "zssh", "zssh", "ready"), None);
    }

    #[test]
    fn mcp_status_check_catches_servers_we_did_not_disable() {
        let disabled = vec!["my_mcp_server".to_string()];
        // 正常：zssh 已就绪（有 serverInfo 和工具），被禁用的停在"配置里存在"的样子。
        let clean = vec![
            json!({"name": "zssh", "serverInfo": {"name": "zssh"}, "tools": {"a": {}}}),
            json!({"name": "my_mcp_server", "serverInfo": null, "tools": {}, "toolsError": null}),
        ];
        assert_eq!(mcp_status_violation(&clean, "zssh", &disabled), None);

        // 漏网的服务名：直接违规（这道判据不依赖通知先后）。
        let stranger = vec![json!({"name": "fake_server", "serverInfo": null, "tools": {}})];
        let message = mcp_status_violation(&stranger, "zssh", &disabled).expect("要违规");
        assert!(message.contains("fake_server"));

        // 名字在被禁用名单里、但已经有运行时痕迹（就绪 / 有工具 / 启动失败）：也违规。
        let running = vec![
            json!({"name": "my_mcp_server", "serverInfo": {"name": "my_mcp_server"}, "tools": {"x": {}}}),
        ];
        assert!(mcp_status_violation(&running, "zssh", &disabled).is_some());
        let failed = vec![json!({"name": "my_mcp_server", "tools": {}, "toolsError": "boom"})];
        assert!(mcp_status_violation(&failed, "zssh", &disabled).is_some());
    }

    #[test]
    fn late_mcp_notifications_still_fail_closed() {
        // 卡片里说的顺序：zssh 先 ready（gate 放行）→ 别的服务的 starting 通知晚到。
        let ready = next_mcp_gate(&McpGate::Pending, "zssh", "zssh", "ready").expect("ready");
        assert_eq!(ready, McpGate::Ready);
        let late =
            next_mcp_gate(&ready, "zssh", "late_server", "starting").expect("晚到的通知也要违规");
        match late {
            McpGate::Violation(message) => assert!(message.contains("late_server")),
            other => panic!("应是违规：{other:?}"),
        }
    }

    /// R-2 第 3 节：用户 `config.toml` 的**格式坏了**、名字没扫出来的时候，用户自己
    /// 的 MCP 服务依然进不来：启动后核对（`mcpServerStatus/list`）看到"没禁用过"
    /// 的服务就把进程拦下来（失败关闭），不靠"扫到了哪些名字"。
    #[test]
    fn broken_config_still_fails_closed_for_foreign_servers() {
        // 段头没闭合：扫不出名字（这正是不敢只靠扫配置的原因）。
        let broken = "[mcp_servers\ncommand = 'x'\n";
        let configured = crate::ai::agent::codex::startup::parse_config_toml_mcp_names(broken);
        let disabled = crate::ai::agent::codex::startup::servers_to_disable(&configured, "zssh");
        assert!(disabled.is_empty(), "坏格式扫不出名字（前提）");

        let entries = vec![json!({
            "name": "my_mcp_server",
            "serverInfo": {"name": "my_mcp_server"},
            "tools": {"x": {}},
        })];
        let message = mcp_status_violation(&entries, "zssh", &disabled).expect("用户的服务要拦住");
        assert!(message.contains("my_mcp_server"), "{message}");
    }

    #[test]
    fn system_prompt_carries_the_capability() {
        let prompt = system_prompt("abc123");
        assert!(prompt.contains("`zssh` tools"));
        assert!(prompt.contains("abc123"));
        assert!(prompt.contains("list_targets"));
    }
}
