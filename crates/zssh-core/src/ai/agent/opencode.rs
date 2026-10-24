//! opencode 驱动（卡片 AI-2，AI-2b 改为**所有对话共用一个进程**）。
//!
//! 一个 `opencode serve` 进程服务全部对话：每个对话在此进程里建一个会话，SSE 事件流
//! 只有一条，按事件里的会话 id 分发给对应对话。对话之间靠**能力密钥**隔离（桥侧按
//! 密钥找会话表），密钥通过该对话自己的系统提示交给模型。
//!
//! 进程生命周期：第一次用 AI 时启动，所有对话空闲 [`DEFAULT_IDLE_TIMEOUT`]（10 分钟）
//! 后自动关闭，下次再用时由上层重新启动；应用退出时连子进程树（含 zssh-mcp）一起杀。
//!
//! 启动方式实测（1.19.0）：
//! - 就绪标志是 stdout 的 `opencode server listening on http://127.0.0.1:<port>`；
//! - 鉴权是 HTTP Basic（`OPENCODE_SERVER_PASSWORD` 环境变量，用户名固定 `opencode`）；
//! - npm 全局装的 `.cmd` 包装脚本先拆出真实 exe；拆不出时直接交给标准库启动（它会用
//!   cmd 专用的参数转义，见 `discovery::build_command`）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::discovery::{self, AgentInstall};
use super::retry::{retry_limit_message, retry_limit_reached, RETRY_STOP_GRACE};
use super::translate::{translate_event, TranslateState};
use super::{AgentConversation, AgentError, AgentEvent, AgentModel, AgentModelCatalog};

/// 默认启动超时：Windows 上冷启动要几十秒（实测 3~8 秒，留足余量）。
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// 默认空闲关闭时间：所有对话空闲 10 分钟后关掉共用的 opencode 进程。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 配置目录：`<工作目录>\opencode-config`。
///
/// 单独开一个空目录当 `XDG_CONFIG_HOME`，把用户自己的 opencode 配置（尤其是他们装的
/// MCP 服务器）挡在外面，只有模型相关键通过 `OPENCODE_CONFIG_CONTENT` 注入，见
/// `config.rs` 模块注释。
pub fn default_config_home(workspace: &std::path::Path) -> PathBuf {
    workspace.join("opencode-config")
}

/// 普通 HTTP 调用的超时（SSE 流不设超时）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 空回复时给用户的提示。
const EMPTY_RESPONSE_HINT: &str =
    "opencode 没有返回任何内容。请在终端运行 opencode 完成登录和模型配置，然后再试。";

/// 启动参数（进程级：所有对话共用）。
pub struct StartOptions {
    /// 找到的 opencode 安装。
    pub install: AgentInstall,
    /// 工作目录（`<数据目录>\agent-workspace`，不存在会在启动前创建）。
    pub workspace: PathBuf,
    /// 配置目录重定向用的空目录（`<数据目录>\agent-workspace\opencode-config`）。
    pub config_home: PathBuf,
    /// `OPENCODE_CONFIG_CONTENT` 的内容。
    pub config_content: String,
    /// 启动超时。
    pub startup_timeout: Duration,
}

/// 共用的 opencode 进程运行时。所有对话都从这里开。
pub struct AgentRuntime {
    shared: Arc<Shared>,
    /// SSE 读流任务（shutdown 时等它退出）。
    sse_task: Mutex<Option<JoinHandle<()>>>,
    /// 通知 SSE 任务收工。
    shutdown_tx: watch::Sender<bool>,
    /// 空闲看护任务。
    idle_task: Mutex<Option<JoinHandle<()>>>,
}

/// HTTP 连接信息（进程级）。
#[derive(Clone)]
struct Http {
    base_url: String,
    /// Basic 鉴权的 header 值（`Basic xxx`）。
    auth_header: String,
    client: reqwest::Client,
    directory: String,
}

/// 进程级共享状态。
struct Shared {
    http: Http,
    child: Mutex<Option<tokio::process::Child>>,
    /// 按会话 id 索引的对话。
    conversations: Mutex<HashMap<String, Arc<Conversation>>>,
    /// 每个回合发消息时要禁用的工具（除 zssh 之外的全部）。
    tool_gate: Vec<String>,
    /// 最近一次活动时间（空闲判断用）。
    last_activity: Mutex<Instant>,
    /// 进程已关闭（空闲关停或事件流断开）。
    closed: AtomicBool,
    /// 最近一次取到的模型目录里每个模型的思考等级（opencode 叫 variant）：`provider/model` →
    /// 等级列表。发消息时靠它分辨 `…/high` 的最后一段是等级还是模型 id 的一部分
    /// （模型 id 自己也可能带斜杠，比如 `openrouter/anthropic/xxx`）。
    variants: Mutex<HashMap<String, Vec<String>>>,
}

/// 一个对话的内部状态（进程的事件流把它的事件投递到这里）。
struct Conversation {
    events: mpsc::UnboundedSender<AgentEvent>,
    inner: Mutex<ConvInner>,
    /// 当前有没有回合在跑。
    busy: AtomicBool,
}

struct ConvInner {
    translate: TranslateState,
    turn: TurnFlags,
    /// 回合序号（每次发消息加一）：延时收尾任务靠它确认还是同一个回合，别误伤下一回合。
    turn_seq: u64,
}

/// 当前回合的状态。
#[derive(Debug, Default, Clone)]
struct TurnFlags {
    /// 已经出现过正文或工具调用（空回复判断用）。
    content: bool,
    /// 这个回合出过错。
    failed: bool,
    /// 用户点了停止（重试超限而停止时也置位：走的是同一条停止路径）。
    aborted: bool,
    /// 重试超限被我们停掉：收尾时先发这条错误再发 TurnEnd，并吞掉随后 abort 带来的
    /// `session.error`（MessageAbortedError），免得用户看到第二条莫名其妙的错误。
    retry_stop: Option<String>,
}

impl Conversation {
    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }
}

impl Shared {
    fn conversation(&self, session_id: &str) -> Option<Arc<Conversation>> {
        self.conversations
            .lock()
            .ok()
            .and_then(|map| map.get(session_id).cloned())
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
        self.conversations
            .lock()
            .map(|map| map.values().any(|c| c.busy.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }

    fn conversation_count(&self) -> usize {
        self.conversations.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// 杀掉整个进程树（opencode 底下挂着 zssh-mcp 等子进程）。
    async fn kill_process(&self) {
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut child) = child {
            let _ = kill_process_tree(&mut child).await;
        }
    }
}

impl AgentRuntime {
    /// 启动进程、等就绪、挂事件流和空闲看护。
    pub async fn start(
        options: StartOptions,
        idle_timeout: Duration,
    ) -> Result<Arc<AgentRuntime>, AgentError> {
        std::fs::create_dir_all(&options.workspace)
            .map_err(|e| AgentError::StartFailed(format!("创建工作目录失败：{e}")))?;
        std::fs::create_dir_all(&options.config_home)
            .map_err(|e| AgentError::StartFailed(format!("创建配置目录失败：{e}")))?;

        let port = free_port().map_err(AgentError::StartFailed)?;
        let password = random_hex(16);
        let directory = options.workspace.to_string_lossy().into_owned();

        let spawned = spawn_serve(&options, port, &password).await?;
        let Spawned {
            mut child,
            stdout: mut stdout_lines,
            stdout_tail,
            stderr_tail,
        } = spawned;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| AgentError::Transport(e.to_string()))?;
        let auth_header = basic_auth_header("opencode", &password);

        // 等就绪行；就绪之前进程挂了就把刚抓的输出带出去。
        let base_url = match wait_ready(&mut stdout_lines, &options.startup_timeout).await {
            Ok(url) => url,
            Err(err) => {
                let _ = kill_process_tree(&mut child).await;
                let tail = tail_text(&stdout_tail, &stderr_tail);
                return Err(AgentError::StartFailed(format!("{err}{tail}")));
            }
        };
        tracing::debug!(url = %base_url, "opencode 就绪");

        // 之后就绪行意义不大了，继续把管道读空（不然 opencode 写满管道会卡死）。
        let _stdout_drain = drain_lines(stdout_lines, "opencode stdout");

        let http = Http {
            base_url,
            auth_header,
            client,
            directory,
        };

        // 拉一遍工具 id，生成每回合的禁用表（本机工具和用户自己的 MCP 全禁，只留
        // zssh 的）。拉不到就拒绝启动（K05）：原来退回内置工具名列表，那份名单只认得
        // 开发时知道的工具，新版本多出来的本机工具会被放行——这是失败放宽。
        let tool_gate = match fetch_tool_gate_with_retry(&http).await {
            Some(gate) => gate,
            None => {
                let _ = kill_process_tree(&mut child).await;
                return Err(AgentError::StartFailed(TOOL_GATE_UNAVAILABLE.to_owned()));
            }
        };

        let shared = Arc::new(Shared {
            http,
            child: Mutex::new(Some(child)),
            conversations: Mutex::new(HashMap::new()),
            tool_gate,
            last_activity: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            variants: Mutex::new(HashMap::new()),
        });

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let sse_task = tokio::spawn(run_event_stream(Arc::clone(&shared), shutdown_rx));

        // 空闲看护：所有对话空闲超过阈值就关掉进程。
        let idle_task = tokio::spawn(idle_watch(
            Arc::clone(&shared),
            idle_timeout,
            shutdown_tx.subscribe(),
            shutdown_tx.clone(),
        ));

        Ok(Arc::new(AgentRuntime {
            shared,
            sse_task: Mutex::new(Some(sse_task)),
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

    /// 进程是否已经关停（空闲超时或事件流断开）。
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// 当前挂着的对话数。
    pub fn conversation_count(&self) -> usize {
        self.shared.conversation_count()
    }

    /// 查模型目录（不需要先开对话：面板一打开就要显示默认模型）。
    pub async fn models(&self) -> Result<AgentModelCatalog, AgentError> {
        self.shared.touch();
        fetch_models(&self.shared).await
    }

    /// 在这个进程里开一个新对话：建会话、登记、返回对话句柄。
    ///
    /// `capability` 是这个对话的能力密钥，会写进该对话的系统提示交给模型；模型调
    /// zssh 工具时带上它，桥据它找到本对话的会话表。
    pub async fn open_conversation(
        self: &Arc<Self>,
        capability: String,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Arc<OpenCodeConversation>, AgentError> {
        self.open_conversation_resuming(capability, events, None)
            .await
            .map(|(conversation, _)| conversation)
    }

    /// 同上，`resume` 给了就先试着接上那个 opencode 会话（历史会话续接）。
    ///
    /// opencode 的会话存在它自己的数据目录里（按工作目录归档，我们的工作目录固定），
    /// 重启后仍在；查不到（被用户删了、换了数据目录）就开新会话。返回值的 bool =
    /// 是否真的接上了。能力密钥每轮都随 `system` 字段重新给，不复用旧的。
    pub async fn open_conversation_resuming(
        self: &Arc<Self>,
        capability: String,
        events: mpsc::UnboundedSender<AgentEvent>,
        resume: Option<&str>,
    ) -> Result<(Arc<OpenCodeConversation>, bool), AgentError> {
        if self.is_closed() {
            return Err(AgentError::Transport("opencode 进程已关闭".into()));
        }
        let resumed = match resume.filter(|id| !id.trim().is_empty()) {
            Some(id) if session_exists(&self.shared.http, id).await => Some(id.to_string()),
            Some(_) => {
                tracing::info!("opencode 里找不到历史会话，改用新会话");
                None
            }
            None => None,
        };
        let was_resumed = resumed.is_some();
        let session_id = match resumed {
            Some(id) => id,
            None => create_session(&self.shared.http).await?,
        };
        let entry = Arc::new(Conversation {
            events,
            inner: Mutex::new(ConvInner {
                translate: TranslateState::default(),
                turn: TurnFlags::default(),
                turn_seq: 0,
            }),
            busy: AtomicBool::new(false),
        });
        if let Ok(mut map) = self.shared.conversations.lock() {
            map.insert(session_id.clone(), Arc::clone(&entry));
        }
        self.shared.touch();
        Ok((
            Arc::new(OpenCodeConversation {
                runtime: Arc::clone(self),
                session_id,
                capability,
                entry,
            }),
            was_resumed,
        ))
    }

    /// 关掉进程：停事件流、杀进程树。
    pub async fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
        let sse = self.sse_task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = sse {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
        let idle = self.idle_task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = idle {
            task.abort();
        }
        self.shared.kill_process().await;
    }
}

impl Drop for AgentRuntime {
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
pub struct OpenCodeConversation {
    runtime: Arc<AgentRuntime>,
    session_id: String,
    /// 本对话的能力密钥（写进系统提示）。
    capability: String,
    entry: Arc<Conversation>,
}

impl OpenCodeConversation {
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
        if let Ok(mut map) = self.runtime.shared.conversations.lock() {
            map.remove(&self.session_id);
        }
    }

    async fn send_inner(&self, text: &str, model: Option<&str>) -> Result<(), AgentError> {
        let shared = &self.runtime.shared;
        if shared.closed.load(Ordering::SeqCst) {
            return Err(AgentError::Transport(
                "opencode 事件流已断开，请新开一个对话".to_string(),
            ));
        }
        if self.entry.busy.swap(true, Ordering::SeqCst) {
            return Err(AgentError::Busy);
        }
        shared.touch();
        if let Ok(mut inner) = self.entry.inner.lock() {
            inner.turn = TurnFlags::default();
            inner.turn_seq += 1;
        }

        // 系统提示里带上能力密钥：模型每次调 zssh 工具都要把它作为 capability 参数。
        let mut body = json!({
            "parts": [{ "type": "text", "text": text }],
            "system": system_prompt(&self.capability),
        });
        if let Some(model_ref) = model {
            let parsed = {
                let variants = shared
                    .variants
                    .lock()
                    .map(|m| m.clone())
                    .unwrap_or_default();
                split_model_ref(model_ref, &variants)
            };
            let (provider, model_id, variant) = parsed.ok_or_else(|| {
                AgentError::Other(format!(
                    "模型 id 格式不对：{model_ref}（要 provider/model）"
                ))
            })?;
            body["model"] = json!({ "providerID": provider, "modelID": model_id });
            if let Some(variant) = variant {
                body["variant"] = Value::String(variant);
            }
        }
        if !shared.tool_gate.is_empty() {
            let mut tools = serde_json::Map::new();
            for id in &shared.tool_gate {
                tools.insert(id.clone(), Value::Bool(false));
            }
            body["tools"] = Value::Object(tools);
        }

        let url = format!(
            "{}/session/{}/prompt_async",
            shared.http.base_url, self.session_id
        );
        let result = shared
            .http
            .client
            .post(url)
            .query(&[("directory", shared.http.directory.as_str())])
            .header("Authorization", &shared.http.auth_header)
            .timeout(REQUEST_TIMEOUT)
            .json(&body)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => Ok(()),
            Ok(resp) => {
                self.entry.busy.store(false, Ordering::SeqCst);
                Err(AgentError::Transport(format!(
                    "发送失败：HTTP {}",
                    resp.status()
                )))
            }
            Err(err) => {
                self.entry.busy.store(false, Ordering::SeqCst);
                Err(AgentError::Transport(err.to_string()))
            }
        }
    }
}

impl AgentConversation for OpenCodeConversation {
    fn session_id(&self) -> &str {
        &self.session_id
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
        Box::pin(async move {
            if !self.entry.busy.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Ok(mut inner) = self.entry.inner.lock() {
                inner.turn.aborted = true;
            }
            post_abort(&self.runtime.shared.http, &self.session_id).await
        })
    }

    /// 结束这个对话：从进程里摘掉。共用进程本身照常服务其他对话。
    fn shutdown<'a>(&'a self) -> crate::ssh::BoxFuture<'a, ()> {
        Box::pin(async move { self.detach() })
    }
}

/// 通知 opencode 中止这个会话当前的回合（用户点「停止」和重试超限自动停止共用这一条）。
async fn post_abort(http: &Http, session_id: &str) -> Result<(), AgentError> {
    let url = format!("{}/session/{}/abort", http.base_url, session_id);
    let resp = http
        .client
        .post(url)
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| AgentError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(AgentError::Transport(format!(
            "停止回合失败：HTTP {}",
            resp.status()
        )));
    }
    Ok(())
}

impl Drop for OpenCodeConversation {
    fn drop(&mut self) {
        self.detach();
    }
}

/// 交给模型的系统提示：通用提示（能力密钥的用途等）+ 本机 `read` 一律被拒的说明。
fn system_prompt(capability: &str) -> String {
    crate::ai::agent::config::opencode_system_prompt(capability)
}

// ---------------------------------------------------------------------------
// 进程启动
// ---------------------------------------------------------------------------

struct Spawned {
    child: tokio::process::Child,
    stdout: Lines<tokio::process::ChildStdout>,
    /// stdout 的尾巴（和排水任务共享）。
    stdout_tail: TailBuf,
    /// stderr 的尾巴（stderr 从启动起就在排水）。
    stderr_tail: TailBuf,
}

/// 最近若干行输出（出错时一起报出去）；实现移到 [`super::process`]（AI-3 起共用）。
use super::process::{kill_process_tree, tail_buf, tail_text, TailBuf};

/// 起 `opencode serve`。
async fn spawn_serve(
    options: &StartOptions,
    port: u16,
    password: &str,
) -> Result<Spawned, AgentError> {
    let mut cmd = discovery::build_command(PathBuf::from(&options.install.path).as_path());
    cmd.arg("serve")
        .arg("--hostname")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&options.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // 重定向配置目录：用户自己的 MCP / 插件不再加载（见 config.rs 模块注释）。
    // 继承来的 OPENCODE_* 变量会绕开上面的目录重定向（K05），先全部去掉；
    // 必须在下面设置我们自己的变量之前做，env_remove 和 env 按调用顺序生效。
    for name in inherited_opencode_env_to_strip(std::env::vars_os().map(|(name, _)| name)) {
        cmd.env_remove(name);
    }
    cmd.env("XDG_CONFIG_HOME", &options.config_home)
        .env("OPENCODE_CONFIG_CONTENT", &options.config_content)
        .env("OPENCODE_SERVER_PASSWORD", password);
    discovery::hide_console(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| AgentError::StartFailed(format!("启动进程失败：{e}")))?;
    // 崩溃时也要带走整棵子进程树（K06）
    super::process::bind_to_app_lifetime(&child);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let stdout_tail: TailBuf = tail_buf();
    let stderr_tail: TailBuf = tail_buf();

    // stderr 立刻开始排水（不然写满管道会卡进程）。
    let _stderr_drain = drain_lines(
        Lines::new(stderr, Arc::clone(&stderr_tail)),
        "opencode stderr",
    );

    Ok(Spawned {
        child,
        stdout: Lines::new(stdout, Arc::clone(&stdout_tail)),
        stdout_tail,
        stderr_tail,
    })
}

/// 允许从用户环境继承的 `OPENCODE_*` 变量：只关功能或改联网方式，不会加载外部配置、
/// 放宽权限或打开新工具。
const OPENCODE_ENV_KEEP: &[&str] = &[
    "OPENCODE_DISABLE_AUTOUPDATE",
    "OPENCODE_DISABLE_MODELS_FETCH",
    "OPENCODE_MODELS_URL",
    "OPENCODE_DISABLE_LSP_DOWNLOAD",
    "OPENCODE_DISABLE_PRUNE",
];

/// 启动 opencode 前要从继承环境里去掉的变量（K05）。
///
/// 为什么按前缀整体去掉而不是列黑名单：opencode 会读的配置变量很多，而且随版本增加——
/// `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR` 直接加载外部配置文件和目录（插件、agent、
/// MCP），`OPENCODE_PERMISSION` 内联权限，`OPENCODE_SERVER_USERNAME` 会让我们的
/// Basic 鉴权对不上，`OPENCODE_ENABLE_*` / `OPENCODE_EXPERIMENTAL*` 会打开额外工具。
/// 黑名单漏一个就绕开了配置目录重定向，所以除了 [`OPENCODE_ENV_KEEP`] 一律去掉。
/// Windows 的环境变量名不分大小写，比较时也不分。
fn inherited_opencode_env_to_strip(
    names: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    names
        .filter(|name| {
            // 不是合法 Unicode 的名字有损转换后再比：前缀像就去掉（读不了 = 不确定）
            let upper = name.to_string_lossy().to_ascii_uppercase();
            upper.starts_with("OPENCODE_") && !OPENCODE_ENV_KEEP.contains(&upper.as_str())
        })
        .collect()
}

/// 按行读取缓冲。
struct Lines<R> {
    inner: BufReader<R>,
    tail: TailBuf,
    eof: bool,
}

impl<R: tokio::io::AsyncRead + Unpin> Lines<R> {
    fn new(reader: R, tail: TailBuf) -> Lines<R> {
        Lines {
            inner: BufReader::new(reader),
            tail,
            eof: false,
        }
    }

    /// 读下一行；EOF 返回 None。
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        if self.eof {
            return Ok(None);
        }
        let mut buf = String::new();
        let n = self.inner.read_line(&mut buf).await?;
        if n == 0 {
            self.eof = true;
            return Ok(None);
        }
        let line = buf.trim_end_matches(['\r', '\n']).to_string();
        if let Ok(mut tail) = self.tail.lock() {
            tail.push(line.clone());
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        Ok(Some(line))
    }
}

/// 等就绪行：`opencode server listening on http://127.0.0.1:<port>`。
async fn wait_ready<R: tokio::io::AsyncRead + Unpin>(
    stdout: &mut Lines<R>,
    timeout: &Duration,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + *timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("等待就绪超时（{} 秒）", timeout.as_secs()));
        }
        match tokio::time::timeout(remaining, stdout.next_line()).await {
            Err(_) => return Err(format!("等待就绪超时（{} 秒）", timeout.as_secs())),
            Ok(Err(err)) => return Err(format!("读 stdout 失败：{err}")),
            Ok(Ok(None)) => {
                return Err("进程提前退出".to_string());
            }
            Ok(Ok(Some(line))) => {
                if let Some(url) = parse_ready_line(&line) {
                    return Ok(url);
                }
            }
        }
    }
}

/// 从一行 stdout 里认出就绪地址。
pub fn parse_ready_line(line: &str) -> Option<String> {
    let marker = "opencode server listening on ";
    let idx = line.find(marker)?;
    let url = line[idx + marker.len()..].trim();
    let url = url.strip_suffix('.').unwrap_or(url).trim();
    if url.starts_with("http://") {
        Some(url.to_string())
    } else {
        None
    }
}

/// 把管道读空（顺带维护尾巴），防止子进程写满管道卡死。
fn drain_lines<R>(mut lines: Lines<R>, tag: &'static str) -> JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::trace!(target: "opencode", "{tag}: {line}");
        }
    })
}

/// 让系统分配一个空闲端口。
fn free_port() -> Result<u16, String> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("分配端口失败：{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读本地地址失败：{e}"))?
        .port();
    drop(listener);
    Ok(port)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn basic_auth_header(user: &str, password: &str) -> String {
    use base64::Engine as _;
    let raw = format!("{user}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

// ---------------------------------------------------------------------------
// HTTP：会话、模型、工具开关
// ---------------------------------------------------------------------------

async fn create_session(http: &Http) -> Result<String, AgentError> {
    let url = format!("{}/session", http.base_url);
    let resp = http
        .client
        .post(url)
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .json(&json!({ "title": "Z-SSH AI" }))
        .send()
        .await
        .map_err(|e| AgentError::StartFailed(format!("建会话失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AgentError::StartFailed(format!(
            "建会话失败：HTTP {}",
            resp.status()
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| AgentError::StartFailed(format!("解析会话失败：{e}")))?;
    body["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| AgentError::StartFailed("会话响应里没有 id".to_string()))
}

/// 会话还在不在（`GET /session/{id}`，返回的 id 要对得上）。查不了按"不在"。
async fn session_exists(http: &Http, id: &str) -> bool {
    // id 拼进路径：只认 opencode 会话 id 的字符，防止拼出别的接口。
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return false;
    }
    let Ok(resp) = http
        .client
        .get(format!("{}/session/{id}", http.base_url))
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<Value>()
        .await
        .is_ok_and(|body| body["id"].as_str() == Some(id))
}

/// 查可用模型（`GET /config/providers`），顺带把默认模型带上（`GET /config`）。
async fn fetch_models(shared: &Shared) -> Result<AgentModelCatalog, AgentError> {
    let http = &shared.http;
    let resp = http
        .client
        .get(format!("{}/config/providers", http.base_url))
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| AgentError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(AgentError::Transport(format!(
            "取模型列表失败：HTTP {}",
            resp.status()
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Transport(e.to_string()))?;
    let mut catalog = parse_providers(&body);
    if let Ok(mut map) = shared.variants.lock() {
        *map = variant_map(&body);
    }
    let provider_defaults = only_provider_default(&body);

    // 默认模型放在 /config 里（字段可能是字符串或对象），拿不到就不填。
    if let Ok(resp) = http
        .client
        .get(format!("{}/config", http.base_url))
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(cfg) = resp.json::<Value>().await {
                catalog.default = parse_default_model(&cfg["model"]);
            }
        }
    }
    catalog.default_label = default_label(&catalog, provider_defaults.as_deref());
    Ok(catalog)
}

/// `/config/providers` 的 `default` 表（服务商 → 它排第一的模型）里**只有一项**时，
/// 返回那个 `provider/model`。
///
/// 为什么只认一项：没配 `model` 时 opencode 取"第一个服务商排第一的模型"，`default`
/// 表正是按同一个排序算的；只有一个服务商时结果是确定的，多个服务商时哪个算"第一个"
/// 我们这边看不出来，宁可不显示也不猜。
pub fn only_provider_default(body: &Value) -> Option<String> {
    let map = body["default"].as_object()?;
    if map.len() != 1 {
        return None;
    }
    let (provider, model) = map.iter().next()?;
    let model = model.as_str().filter(|m| !m.is_empty())?;
    Some(format!("{provider}/{model}"))
}

/// "默认"实际对应的模型的显示名：配置里的 `model` 优先，其次是唯一服务商的默认；
/// 在列表里就用列表里的名字，不在就直接显示 id。
pub fn default_label(
    catalog: &AgentModelCatalog,
    provider_default: Option<&str>,
) -> Option<String> {
    let id = catalog.default.as_deref().or(provider_default)?;
    Some(
        catalog
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| id.to_string()),
    )
}

/// opencode 自己的免费模型：id 带 `-free`，或者标价输入、输出都是 0（比如 big-pickle）。
/// 只认 opencode 这家：自定义服务商没填价格时 opencode 也会给 0，不能当免费。
fn is_free_model(provider_id: &str, model_id: &str, info: &Value) -> bool {
    if provider_id != "opencode" {
        return false;
    }
    let zero = |v: &Value| v.as_f64() == Some(0.0);
    model_id.ends_with("-free") || (zero(&info["cost"]["input"]) && zero(&info["cost"]["output"]))
}

/// 从 `/config/providers` 的响应里展平出模型列表。
///
/// 排序：免费模型在前，其余在后；两组里各自按显示名 A→Z（不分大小写）。
/// 每个模型的思考等级项紧跟在它后面，不参与排序。
pub fn parse_providers(body: &Value) -> AgentModelCatalog {
    // (是否免费, 排序用的名字, 模型本身 + 它的等级项)
    let mut groups: Vec<(bool, String, Vec<AgentModel>)> = Vec::new();
    if let Some(providers) = body["providers"].as_array() {
        for provider in providers {
            let provider_id = provider["id"].as_str().unwrap_or("");
            if provider_id.is_empty() {
                continue;
            }
            let provider_name = provider["name"].as_str().unwrap_or(provider_id);
            if let Some(map) = provider["models"].as_object() {
                for (model_id, info) in map {
                    let model_name = info["name"].as_str().unwrap_or(model_id);
                    let id = format!("{provider_id}/{model_id}");
                    let name = display_model_name(provider_name, model_name);
                    // 思考等级展开成"模型/等级 + 模型名 · 等级"（和 Claude Code、codex 同一个约定），
                    // 界面据此分出右边的强度框；不带等级的那项 = 用 opencode 自己的默认
                    let variants: Vec<AgentModel> = model_variants(info)
                        .into_iter()
                        .map(|variant| AgentModel {
                            id: format!("{id}/{variant}"),
                            name: format!("{name} · {variant}"),
                            unavailable: false,
                        })
                        .collect();
                    let key = name.to_lowercase();
                    let mut group = vec![AgentModel {
                        id,
                        name,
                        unavailable: false,
                    }];
                    group.extend(variants);
                    groups.push((is_free_model(provider_id, model_id, info), key, group));
                }
            }
        }
    }
    groups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let models = groups.into_iter().flat_map(|(_, _, group)| group).collect();
    AgentModelCatalog {
        models,
        default: None,
        default_label: None,
    }
}

/// 一个模型的思考等级（`variants` 对象的键，去掉标了 `disabled` 的），按强弱排好；
/// 不认识的等级（服务商自定义的）排在后面，保持原来的顺序。
pub fn model_variants(info: &Value) -> Vec<String> {
    const ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    let Some(map) = info["variants"].as_object() else {
        return Vec::new();
    };
    let mut names: Vec<&String> = map
        .iter()
        .filter(|(name, value)| !name.is_empty() && value["disabled"].as_bool() != Some(true))
        .map(|(name, _)| name)
        .collect();
    let rank = |name: &str| ORDER.iter().position(|o| *o == name).unwrap_or(ORDER.len());
    names.sort_by_key(|name| rank(name));
    names.into_iter().cloned().collect()
}

/// `/config/providers` → `provider/model` → 思考等级列表（只收有等级的模型）。
pub fn variant_map(body: &Value) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for provider in body["providers"].as_array().into_iter().flatten() {
        let Some(provider_id) = provider["id"].as_str().filter(|p| !p.is_empty()) else {
            continue;
        };
        for (model_id, info) in provider["models"].as_object().into_iter().flatten() {
            let variants = model_variants(info);
            if !variants.is_empty() {
                out.insert(format!("{provider_id}/{model_id}"), variants);
            }
        }
    }
    out
}

/// 界面选中的 id → (服务商, 模型 id, 思考等级)。`…/等级` 的最后一段只有在目录里确实是这个
/// 模型的等级时才拆出来，否则整个当模型 id（模型 id 自己可能带斜杠）。
pub fn split_model_ref(
    model_ref: &str,
    variants: &HashMap<String, Vec<String>>,
) -> Option<(String, String, Option<String>)> {
    let (base, variant) = match model_ref.rsplit_once('/') {
        Some((base, last))
            if variants
                .get(base)
                .is_some_and(|v| v.iter().any(|x| x == last)) =>
        {
            (base, Some(last.to_string()))
        }
        _ => (model_ref, None),
    };
    let (provider, model_id) = base.split_once('/')?;
    if provider.is_empty() || model_id.is_empty() {
        return None;
    }
    Some((provider.to_string(), model_id.to_string(), variant))
}

/// 拼模型显示名；模型名里已经带服务商名时不要再拼一次
/// （否则会出现 "DeepSeek DeepSeek V4.1 Flash" 这种重复）。
fn display_model_name(provider_name: &str, model_name: &str) -> String {
    let provider = provider_name.trim();
    let model = model_name.trim();
    if provider.is_empty() || model.is_empty() {
        return model.to_string();
    }
    if model
        .to_ascii_lowercase()
        .starts_with(&provider.to_ascii_lowercase())
    {
        model.to_string()
    } else {
        format!("{provider} {model}")
    }
}

/// `/config` 的 `model` 字段：字符串 `provider/id` 或对象 `{providerID, modelID|id}`。
pub fn parse_default_model(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if value.is_object() {
        let provider = value["providerID"].as_str()?;
        let model = value["modelID"].as_str().or_else(|| value["id"].as_str())?;
        return Some(format!("{provider}/{model}"));
    }
    None
}

/// 拉工具 id 列表，生成禁用表：除 zssh 的工具外全部禁用。
///
/// 1.19.0 上工具 id 形如 `bash`（内置）和 `zssh_run_command`（MCP，服务器名做前缀）。
/// 拉不到返回 None，调用方退回内置工具名列表。
/// 拿不到工具清单时给用户的说明。
const TOOL_GATE_UNAVAILABLE: &str = "没能从 opencode 取得工具清单（/experimental/tool/ids），\
    无法确认要禁用哪些本机工具，为了不放开未知的本机工具已停止启动。\
    这通常是 opencode 版本的接口变了：请换用开发时实测过的版本（1.19.x）或反馈这个问题。";

/// 取工具清单，失败时短暂重试：刚就绪时偶尔会有一次请求失败，不该因此拒绝启动。
async fn fetch_tool_gate_with_retry(http: &Http) -> Option<Vec<String>> {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        if let Some(gate) = fetch_tool_gate(http).await {
            return Some(gate);
        }
        tracing::warn!(attempt, "取 opencode 工具清单失败");
        if attempt < ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    None
}

async fn fetch_tool_gate(http: &Http) -> Option<Vec<String>> {
    let resp = http
        .client
        .get(format!("{}/experimental/tool/ids", http.base_url))
        .query(&[("directory", http.directory.as_str())])
        .header("Authorization", &http.auth_header)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let gate = tool_gate_from_ids(body.as_array()?)?;
    tracing::debug!(count = gate.len(), "生成工具禁用表");
    Some(gate)
}

/// 工具 id 清单 → 每回合要禁用的表：除了 zssh 的 MCP 工具（含 M3-4 的文件工具
/// `zssh_list_dir` / `zssh_read_file` / `zssh_write_file` / `zssh_stat`）和占位的
/// `invalid`，其余全部禁掉——本机的 `write` / `bash` 等一个不放。
///
/// 唯一的例外是 `read`：它不进禁用表。每回合的 `tools` 禁用会把工具从发给模型的列表里
/// 删掉，而 OpenCode 的免费服务要求 `read` 在列表里（否则拒绝，实测 1.19.4）。`read` 由配置里的
/// `permission.read` 管：除一个永不匹配的哑模式外一律 `deny`（见 `config::READ_DUMMY_PATTERN`），
/// 任何真实路径都读不到。
/// 清单是空的多半是接口变了，返回 `None`，别拿空表当"全部放行"。
pub fn tool_gate_from_ids(ids: &[Value]) -> Option<Vec<String>> {
    let gate: Vec<String> = ids
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|id| !id.starts_with("zssh_") && *id != "invalid" && *id != "read")
        .map(str::to_string)
        .collect();
    if gate.is_empty() {
        return None;
    }
    Some(gate)
}

// ---------------------------------------------------------------------------
// SSE 事件流（进程级：一条流服务所有对话，按会话 id 分发）
// ---------------------------------------------------------------------------

async fn run_event_stream(shared: Arc<Shared>, mut shutdown_rx: watch::Receiver<bool>) {
    let result = read_event_stream(&shared, &mut shutdown_rx).await;
    match result {
        Ok(()) => {}
        Err(message) => {
            if !*shutdown_rx.borrow() {
                shared.closed.store(true, Ordering::SeqCst);
                // 事件流没了，所有对话都不可能再正常收尾。
                let convs: Vec<Arc<Conversation>> = shared
                    .conversations
                    .lock()
                    .map(|map| map.values().cloned().collect())
                    .unwrap_or_default();
                for conv in convs {
                    conv.busy.store(false, Ordering::SeqCst);
                    conv.emit(AgentEvent::Error {
                        message: message.clone(),
                    });
                }
            }
        }
    }
}

async fn read_event_stream(
    shared: &Shared,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let resp = shared
        .http
        .client
        .get(format!("{}/event", shared.http.base_url))
        .query(&[("directory", shared.http.directory.as_str())])
        .header("Authorization", &shared.http.auth_header)
        .send()
        .await
        .map_err(|e| format!("事件流连接失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("事件流连接失败：HTTP {}", resp.status()));
    }
    let mut resp = resp;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let chunk = tokio::select! {
            _ = shutdown_rx.changed() => return Ok(()),
            chunk = resp.chunk() => chunk.map_err(|e| format!("事件流中断：{e}"))?,
        };
        let Some(chunk) = chunk else {
            return Err("opencode 关闭了事件流".to_string());
        };
        buf.extend_from_slice(&chunk);

        // SSE 帧用空行分隔（opencode 是 \n\n）。
        while let Some(pos) = find_frame_end(&buf) {
            let frame: Vec<u8> = buf.drain(..pos).collect();
            // 清掉分隔符和可能多出来的空行/回车。
            while matches!(buf.first(), Some(b'\n') | Some(b'\r')) {
                buf.drain(..1);
            }
            if let Some(data) = frame_data(&frame) {
                if let Ok(event) = serde_json::from_str::<Value>(&data) {
                    handle_event(shared, &event);
                }
            }
        }
    }
}

fn find_frame_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

/// 从帧里取 `data:` 行（多行按 SSE 规则用 \n 拼起来）。
fn frame_data(frame: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(frame);
    let mut out = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(rest);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 把一条事件按会话 id 分发给对应的对话（没有会话 id 或没登记就丢掉）。
fn handle_event(shared: &Shared, event: &Value) {
    let Some(session_id) = event_session_id(event) else {
        return;
    };
    let Some(conv) = shared.conversation(session_id) else {
        return;
    };
    shared.touch();

    let Ok(mut inner) = conv.inner.lock() else {
        return;
    };
    let mut retry_stop_now = false;
    for out in translate_event(event, &mut inner.translate) {
        let mut emit = true;
        match &out {
            AgentEvent::TextDelta { .. } | AgentEvent::ToolStart { .. } => {
                inner.turn.content = true
            }
            AgentEvent::Error { .. } => {
                // 重试超限已经由我们停掉了：随后 abort 带来的 session.error 不再重复报。
                if inner.turn.retry_stop.is_some() {
                    emit = false;
                } else {
                    inner.turn.failed = true;
                }
            }
            AgentEvent::Retrying {
                attempt, reason, ..
            } => {
                // 回合不在跑 / 已被停止（用户点停止，或已经因超限停掉）时晚到的重试状态：忽略。
                if !conv.busy.load(Ordering::SeqCst) || inner.turn.aborted {
                    emit = false;
                } else if retry_limit_reached(*attempt) {
                    // 第 3 次重试也失败了：这条不给界面看，直接停掉这一轮。
                    emit = false;
                    inner.turn.aborted = true;
                    inner.turn.retry_stop = Some(retry_limit_message(reason));
                    retry_stop_now = true;
                }
            }
            AgentEvent::TurnEnd => {
                // 只有正在跑的回合才收尾（防止多余的 idle 产生假「空回复」）。
                emit = end_turn(&conv, &mut inner);
            }
            _ => {}
        }
        if emit {
            conv.emit(out);
        }
    }
    if retry_stop_now {
        stop_after_retry_limit(shared, session_id, &conv, &mut inner);
    }
}

/// 回合收尾的共用部分：清掉「在跑」标记、按需先发一条错误提示。返回 false = 本来就没有
/// 回合在跑（调用方不要再发 TurnEnd）。
fn end_turn(conv: &Conversation, inner: &mut ConvInner) -> bool {
    if !conv.busy.swap(false, Ordering::SeqCst) {
        return false;
    }
    let flags = std::mem::take(&mut inner.turn);
    if let Some(message) = flags.retry_stop {
        conv.emit(AgentEvent::Error { message });
    } else if !flags.content && !flags.failed && !flags.aborted {
        conv.emit(AgentEvent::Error {
            message: EMPTY_RESPONSE_HINT.to_string(),
        });
    }
    true
}

/// 重试超限：走和用户点「停止」同一条中止路径（`post_abort`），并保证这一回合一定收尾。
///
/// 正常情况下 opencode 中止后会自己发 `session.idle`，由 [`end_turn`] 先发错误再发 TurnEnd；
/// 万一它没发（中止请求失败、事件丢了），宽限期过后由我们收尾；如果当前没有 tokio
/// 运行时可用，就立刻收尾。
fn stop_after_retry_limit(
    shared: &Shared,
    session_id: &str,
    conv: &Arc<Conversation>,
    inner: &mut ConvInner,
) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        if end_turn(conv, inner) {
            conv.emit(AgentEvent::TurnEnd);
        }
        return;
    };
    let http = shared.http.clone();
    let session_id = session_id.to_string();
    let conv = Arc::clone(conv);
    let seq = inner.turn_seq;
    // 中止请求单独发，不占宽限期：它慢或失败都不影响下面的兜底收尾。
    handle.spawn(async move {
        if let Err(err) = post_abort(&http, &session_id).await {
            tracing::warn!(error = %err, "重试超限后中止回合失败");
        }
    });
    handle.spawn(async move {
        tokio::time::sleep(RETRY_STOP_GRACE).await;
        let Ok(mut inner) = conv.inner.lock() else {
            return;
        };
        // 还是同一个回合、且还没收尾，才由我们收尾。
        if inner.turn_seq == seq && end_turn(&conv, &mut inner) {
            conv.emit(AgentEvent::TurnEnd);
        }
    });
}

fn event_session_id(event: &Value) -> Option<&str> {
    let props = &event["properties"];
    props["sessionID"]
        .as_str()
        .or_else(|| props["part"]["sessionID"].as_str())
        .or_else(|| props["info"]["sessionID"].as_str())
}

// ---------------------------------------------------------------------------
// 空闲看护
// ---------------------------------------------------------------------------

/// 所有对话空闲超过阈值就关掉进程。
///
/// 空闲的定义：没有任何回合在跑，且最近一次活动（开对话 / 发消息 / 收到事件）已经过去
/// 阈值时间。看护任务在 `tick` 粒度上轮询，阈值在测试里会调得很小。
async fn idle_watch(
    shared: Arc<Shared>,
    idle_timeout: Duration,
    mut shutdown_rx: watch::Receiver<bool>,
    kill_tx: watch::Sender<bool>,
) {
    let tick = (idle_timeout / 10).clamp(Duration::from_millis(20), Duration::from_secs(5));
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
            tracing::debug!(?idle_timeout, "opencode 空闲超时，关闭共用进程");
            shared.closed.store(true, Ordering::SeqCst);
            let _ = kill_tx.send(true);
            shared.kill_process().await;
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试：解析、系统提示、事件分发（不启进程）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn parses_ready_line() {
        let line = "opencode server listening on http://127.0.0.1:45845";
        assert_eq!(
            parse_ready_line(line).as_deref(),
            Some("http://127.0.0.1:45845")
        );
        // 带前缀日志、带句号也能认。
        let noisy = "INFO 2026-10-15 opencode server listening on http://127.0.0.1:12345.";
        assert_eq!(
            parse_ready_line(noisy).as_deref(),
            Some("http://127.0.0.1:12345")
        );
        assert!(parse_ready_line("something else").is_none());
    }

    #[test]
    fn config_home_lives_under_the_workspace() {
        // 用户自带 MCP 的隔离靠这个目录（见 config.rs）：它必须在我们的工作目录下，
        // 不能碰到用户的 ~/.config/opencode。
        let home = default_config_home(std::path::Path::new("C:\\data\\agent-workspace"));
        assert_eq!(
            home,
            std::path::PathBuf::from("C:\\data\\agent-workspace\\opencode-config")
        );
    }

    /// 起一个只会回固定响应的本地 HTTP 服务，返回 (地址, 收到的请求数)。
    async fn canned_http(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn test_http(base_url: String) -> Http {
        Http {
            base_url,
            auth_header: "Basic test".to_owned(),
            client: reqwest::Client::new(),
            directory: "workspace".to_owned(),
        }
    }

    #[tokio::test]
    async fn tool_gate_failure_is_retried_then_reported_as_unavailable() {
        // K05：拿不到清单不能退回固定名单（失败放宽），重试几次后交给调用方拒绝启动
        let (url, hits) = canned_http("500 Internal Server Error", "{}").await;
        assert_eq!(fetch_tool_gate_with_retry(&test_http(url)).await, None);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "应该重试");
    }

    #[tokio::test]
    async fn tool_gate_is_built_from_the_listed_ids() {
        let (url, _) = canned_http("200 OK", r#"["bash","read","zssh_exec","invalid"]"#).await;
        let gate = fetch_tool_gate_with_retry(&test_http(url)).await;
        assert_eq!(
            gate,
            Some(vec!["bash".to_owned()]),
            "read 不进禁用表，交给 permission"
        );
    }

    #[test]
    fn inherited_opencode_config_env_is_stripped_but_harmless_flags_are_kept() {
        // K05：这些变量会让 opencode 绕开配置目录重定向读外部配置、放宽权限或打开新工具
        let names = [
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_CONFIG_CONTENT",
            "opencode_permission",
            "OPENCODE_SERVER_USERNAME",
            "OPENCODE_ENABLE_EXA",
            "OPENCODE_EXPERIMENTAL_LSP_TOOL",
            "OPENCODE_DISABLE_AUTOUPDATE",
            "OPENCODE_MODELS_URL",
            "PATH",
            "XDG_CONFIG_HOME",
            "MY_OPENCODE_CONFIG",
        ];
        let stripped: Vec<String> =
            inherited_opencode_env_to_strip(names.iter().map(std::ffi::OsString::from))
                .into_iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
        assert_eq!(
            stripped,
            [
                "OPENCODE_CONFIG",
                "OPENCODE_CONFIG_DIR",
                "OPENCODE_CONFIG_CONTENT",
                "opencode_permission",
                "OPENCODE_SERVER_USERNAME",
                "OPENCODE_ENABLE_EXA",
                "OPENCODE_EXPERIMENTAL_LSP_TOOL",
            ]
        );
    }

    #[test]
    fn tool_gate_blocks_local_tools_and_keeps_every_zssh_tool_including_file_tools() {
        let ids: Vec<Value> = [
            "bash",
            "read",
            "write",
            "edit",
            "glob",
            "invalid",
            "zssh_list_targets",
            "zssh_run_command",
            "zssh_read_terminal",
            "zssh_list_dir",
            "zssh_read_file",
            "zssh_write_file",
            "zssh_stat",
            "my_mcp_server_read_file",
        ]
        .iter()
        .map(|s| json!(s))
        .collect();
        let gate = tool_gate_from_ids(&ids).expect("有禁用表");
        for local in ["bash", "write", "edit", "glob", "my_mcp_server_read_file"] {
            assert!(gate.contains(&local.to_string()), "{local} 要被禁");
        }
        // read 要留在工具列表里（OpenCode 免费服务要求），读不读得了由 permission 的哑模式配置管。
        assert!(!gate.contains(&"read".to_string()), "read 不进禁用表");
        assert!(
            gate.iter().all(|id| !id.starts_with("zssh_")),
            "zssh 的工具一个都不禁：{gate:?}"
        );
        assert!(!gate.contains(&"invalid".to_string()));
        assert_eq!(tool_gate_from_ids(&[]), None, "空清单不当成全部放行");
    }

    #[test]
    fn system_prompt_carries_the_capability_key() {
        let prompt = system_prompt("deadbeefdeadbeef");
        assert!(prompt.contains("deadbeefdeadbeef"), "提示里要带能力密钥");
        assert!(prompt.contains("capability"), "要说清参数名");
    }

    #[test]
    fn free_models_first_then_a_to_z_with_variants_kept_together() {
        let body = json!({
            "providers": [
                {
                    "id": "openai",
                    "name": "OpenAI",
                    "models": {"gpt-5.5": {"name": "GPT-5.5", "variants": {"high": {}, "low": {}}}}
                },
                {
                    "id": "deepseek",
                    "name": "DeepSeek",
                    "models": {"deepseek-v4-pro": {"name": "DeepSeek V4 Pro"}}
                },
                {
                    "id": "opencode",
                    "name": "OpenCode Zen",
                    "models": {
                        "zeta-free": {"name": "Zeta Free"},
                        "big-pickle": {"name": "Big Pickle", "cost": {"input": 0, "output": 0}},
                        "claude-x": {"name": "Claude X", "cost": {"input": 3, "output": 15}}
                    }
                },
                {
                    // 自定义服务商价格是 0 也不算免费
                    "id": "local",
                    "name": "local",
                    "models": {"aaa": {"name": "aaa", "cost": {"input": 0, "output": 0}}}
                }
            ]
        });
        let ids: Vec<String> = parse_providers(&body)
            .models
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            ids,
            [
                "opencode/big-pickle",
                "opencode/zeta-free",
                "deepseek/deepseek-v4-pro",
                "local/aaa",
                "openai/gpt-5.5",
                "openai/gpt-5.5/low",
                "openai/gpt-5.5/high",
                "opencode/claude-x",
            ]
        );
    }

    #[test]
    fn parses_providers_catalog() {
        let body = json!({
            "providers": [
                {
                    "id": "deepseek",
                    "name": "DeepSeek",
                    "models": {
                        "deepseek-flash": {"name": "DeepSeek Flash"},
                        "deepseek-v4-pro": {"name": "DeepSeek V4 Pro"}
                    }
                },
                {"id": "openai", "models": {"gpt-5.5": {}}},
                {"id": "opencode", "models": {"big-pickle": {}, "muse-free": {"name": "Muse Free"}}}
            ],
            "default": {"deepseek": "deepseek-v4-pro"}
        });
        let catalog = parse_providers(&body);
        assert_eq!(catalog.models.len(), 5);
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"deepseek/deepseek-v4-pro"));
        assert!(ids.contains(&"openai/gpt-5.5"));
        // 没有 name 的模型用 id 当名字，provider 没 name 就用 id。
        let openai = catalog
            .models
            .iter()
            .find(|m| m.id == "openai/gpt-5.5")
            .unwrap();
        assert_eq!(openai.name, "openai gpt-5.5");
        // 免费用量模型标成不可用。
        let free = catalog
            .models
            .iter()
            .find(|m| m.id == "opencode/muse-free")
            .unwrap();
        assert!(!free.unavailable, "免费模型不再标不可用");
        let pick = catalog
            .models
            .iter()
            .find(|m| m.id == "opencode/big-pickle")
            .unwrap();
        assert!(!pick.unavailable);
    }

    #[test]
    fn variants_expand_into_effort_entries() {
        let body = json!({
            "providers": [{
                "id": "opus",
                "models": {
                    "claude-sonnet-5": {
                        "name": "Claude Sonnet 5",
                        "variants": {
                            "high": {}, "max": {}, "low": {}, "medium": {}, "extra": {},
                            "off": {"disabled": true}
                        }
                    },
                    "plain": {"name": "Plain"}
                }
            }, {
                "id": "opencode",
                "models": {"x-free": {"name": "X Free", "variants": {"low": {}, "high": {}}}}
            }]
        });
        let catalog = parse_providers(&body);
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        // 免费的 x-free（连同两个等级）排最前；模型本身在前，等级按强弱排，自定义的 extra 放最后，disabled 的不要
        assert_eq!(
            &ids[3..9],
            &[
                "opus/claude-sonnet-5",
                "opus/claude-sonnet-5/low",
                "opus/claude-sonnet-5/medium",
                "opus/claude-sonnet-5/high",
                "opus/claude-sonnet-5/max",
                "opus/claude-sonnet-5/extra",
            ]
        );
        let high = catalog
            .models
            .iter()
            .find(|m| m.id == "opus/claude-sonnet-5/high")
            .unwrap();
        assert_eq!(high.name, "opus Claude Sonnet 5 · high");
        assert!(ids.contains(&"opus/plain"));
        assert!(!ids.iter().any(|id| id.starts_with("opus/plain/")));
        // 免费模型的等级项也标不可用
        let free = catalog
            .models
            .iter()
            .find(|m| m.id == "opencode/x-free/high")
            .unwrap();
        assert!(!free.unavailable);

        let map = variant_map(&body);
        assert_eq!(map["opus/claude-sonnet-5"].len(), 5);
        assert!(!map.contains_key("opus/plain"));
    }

    #[test]
    fn model_ref_splits_off_only_known_variants() {
        let mut variants = HashMap::new();
        variants.insert(
            "deepseek/deepseek-flash".to_string(),
            vec!["low".into(), "high".into()],
        );
        variants.insert(
            "openrouter/anthropic/claude".to_string(),
            vec!["high".into()],
        );
        let split = |r: &str| split_model_ref(r, &variants);
        assert_eq!(
            split("deepseek/deepseek-flash/high"),
            Some((
                "deepseek".into(),
                "deepseek-flash".into(),
                Some("high".into())
            ))
        );
        assert_eq!(
            split("deepseek/deepseek-flash"),
            Some(("deepseek".into(), "deepseek-flash".into(), None))
        );
        // 模型 id 自己带斜杠：最后一段不是已知等级就整个当模型
        assert_eq!(
            split("openrouter/anthropic/claude"),
            Some(("openrouter".into(), "anthropic/claude".into(), None))
        );
        assert_eq!(
            split("openrouter/anthropic/claude/high"),
            Some((
                "openrouter".into(),
                "anthropic/claude".into(),
                Some("high".into())
            ))
        );
        assert_eq!(
            split("deepseek/deepseek-flash/ultra"),
            Some(("deepseek".into(), "deepseek-flash/ultra".into(), None))
        );
        assert_eq!(split("no-slash"), None);
    }

    #[test]
    fn model_name_does_not_repeat_the_provider_name() {
        // 模型名里已经带了服务商名就不再拼一次（AI-2b 验收里看到 "DeepSeek DeepSeek ..."）。
        assert_eq!(
            display_model_name("DeepSeek", "DeepSeek V4.1 Flash"),
            "DeepSeek V4.1 Flash"
        );
        assert_eq!(display_model_name("openai", "gpt-5.5"), "openai gpt-5.5");
        assert_eq!(display_model_name("", "solo"), "solo");
    }

    #[test]
    fn free_tier_models_are_listed_as_available() {
        let body = json!({
            "providers": [
                {"id": "opencode", "name": "OpenCode", "models": {
                    "some-model-free": {"name": "Some Model Free"},
                    "paid-model": {"name": "Paid Model"}
                }}
            ]
        });
        let catalog = parse_providers(&body);
        assert_eq!(catalog.models.len(), 2);
        assert!(
            catalog.models.iter().all(|m| !m.unavailable),
            "免费模型不再标不可用"
        );
    }

    #[test]
    fn system_prompt_says_the_local_read_tool_is_always_rejected() {
        let prompt = system_prompt("deadbeefdeadbeef");
        assert!(prompt.contains("deadbeefdeadbeef"));
        assert!(prompt.contains("`read` tool is listed but every call to it is always rejected"));
        assert!(prompt.contains("`read_file`"));
    }

    #[test]
    fn default_label_prefers_config_model_then_single_provider_default() {
        let body = json!({
            "providers": [
                {"id": "deepseek", "name": "DeepSeek", "models": {
                    "deepseek-v4-pro": {"name": "DeepSeek V4 Pro"},
                    "deepseek-flash": {"name": "DeepSeek Flash"}
                }}
            ],
            "default": {"deepseek": "deepseek-v4-pro"}
        });
        let mut catalog = parse_providers(&body);
        let only = only_provider_default(&body);
        assert_eq!(only.as_deref(), Some("deepseek/deepseek-v4-pro"));
        // 没配 model：唯一服务商的默认，用列表里的名字。
        assert_eq!(
            default_label(&catalog, only.as_deref()).as_deref(),
            Some("DeepSeek V4 Pro")
        );
        // 配了 model：以配置为准。
        catalog.default = Some("deepseek/deepseek-flash".to_string());
        assert_eq!(
            default_label(&catalog, only.as_deref()).as_deref(),
            Some("DeepSeek Flash")
        );
        // 配置里的模型不在列表里：直接显示 id。
        catalog.default = Some("custom/my-model".to_string());
        assert_eq!(
            default_label(&catalog, only.as_deref()).as_deref(),
            Some("custom/my-model")
        );
    }

    #[test]
    fn several_providers_without_config_model_give_no_label() {
        let body = json!({
            "providers": [
                {"id": "a", "models": {"m1": {}}},
                {"id": "b", "models": {"m2": {}}}
            ],
            "default": {"a": "m1", "b": "m2"}
        });
        let catalog = parse_providers(&body);
        assert_eq!(only_provider_default(&body), None);
        assert_eq!(default_label(&catalog, None), None);
        assert_eq!(only_provider_default(&json!({})), None);
        assert_eq!(only_provider_default(&json!({"default": {"a": ""}})), None);
    }

    #[test]
    fn parses_default_model_shapes() {
        assert_eq!(
            parse_default_model(&json!("deepseek/deepseek-v4-pro")).as_deref(),
            Some("deepseek/deepseek-v4-pro")
        );
        assert_eq!(
            parse_default_model(&json!({"providerID": "openai", "modelID": "gpt-5.5"})).as_deref(),
            Some("openai/gpt-5.5")
        );
        assert_eq!(parse_default_model(&json!(null)), None);
        assert_eq!(parse_default_model(&json!({})), None);
    }

    #[test]
    fn extracts_session_id_from_event_shapes() {
        let a = json!({"type": "session.idle", "properties": {"sessionID": "ses_1"}});
        assert_eq!(event_session_id(&a), Some("ses_1"));
        let b = json!({"type": "message.part.updated",
            "properties": {"part": {"sessionID": "ses_2"}}});
        assert_eq!(event_session_id(&b), Some("ses_2"));
        let c = json!({"type": "message.updated",
            "properties": {"info": {"sessionID": "ses_3", "id": "msg_x"}}});
        assert_eq!(event_session_id(&c), Some("ses_3"));
        let d = json!({"type": "server.connected", "properties": {}});
        assert_eq!(event_session_id(&d), None);
    }

    #[test]
    fn frame_data_handles_multiline() {
        let frame = b"event: message\ndata: {\"a\":1}\n";
        assert_eq!(frame_data(frame).as_deref(), Some("{\"a\":1}"));
        let multi = b"data: line1\ndata: line2\n";
        assert_eq!(frame_data(multi).as_deref(), Some("line1\nline2"));
        assert_eq!(frame_data(b"event: ping\n"), None);
    }

    /// 造一个「进程」的共享状态（不真起进程），只测事件分发。
    fn fake_shared() -> (Arc<Shared>, mpsc::UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            http: Http {
                base_url: "http://127.0.0.1:1".into(),
                auth_header: "Basic x".into(),
                client: reqwest::Client::new(),
                directory: ".".into(),
            },
            child: Mutex::new(None),
            conversations: Mutex::new(HashMap::new()),
            tool_gate: Vec::new(),
            last_activity: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            variants: Mutex::new(HashMap::new()),
        });
        shared.conversations.lock().unwrap().insert(
            "ses_a".into(),
            Arc::new(Conversation {
                events: tx,
                inner: Mutex::new(ConvInner {
                    translate: TranslateState::default(),
                    turn: TurnFlags::default(),
                    turn_seq: 0,
                }),
                busy: AtomicBool::new(true),
            }),
        );
        (shared, rx)
    }

    #[test]
    fn events_are_routed_to_their_own_conversation() {
        let (shared, mut rx) = fake_shared();
        // 别的会话的事件不能进 A 的通道。
        handle_event(
            &shared,
            &json!({"type": "session.idle", "properties": {"sessionID": "ses_b"}}),
        );
        assert!(rx.try_recv().is_err(), "别的会话的事件不该投递过来");
        // 本会话的 idle 收尾（这回合没有正文，所以先来一条空回复提示，再 TurnEnd）。
        handle_event(
            &shared,
            &json!({"type": "session.idle", "properties": {"sessionID": "ses_a"}}),
        );
        assert!(matches!(rx.try_recv().unwrap(), AgentEvent::Error { .. }));
        assert_eq!(rx.try_recv().unwrap(), AgentEvent::TurnEnd);
    }

    #[test]
    fn empty_turn_emits_hint_before_turn_end() {
        let (shared, mut rx) = fake_shared();
        handle_event(
            &shared,
            &json!({"type": "session.idle", "properties": {"sessionID": "ses_a"}}),
        );
        match rx.try_recv().unwrap() {
            AgentEvent::Error { message } => assert!(message.contains("没有返回任何内容")),
            other => panic!("先该有空回复提示，得到 {other:?}"),
        }
        assert_eq!(rx.try_recv().unwrap(), AgentEvent::TurnEnd);
    }

    #[tokio::test]
    async fn idle_watch_closes_the_process_after_timeout() {
        let (shared, _rx) = fake_shared();
        // 让 A 的回合结束（不再 busy），随后长时间空闲。
        shared
            .conversations
            .lock()
            .unwrap()
            .get("ses_a")
            .unwrap()
            .busy
            .store(false, Ordering::SeqCst);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let kill_rx = shutdown_tx.clone();
        let handle = tokio::spawn(idle_watch(
            Arc::clone(&shared),
            Duration::from_millis(80),
            shutdown_rx,
            shutdown_tx.clone(),
        ));
        // 等看护任务把 closed 置位。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !shared.closed.load(Ordering::SeqCst) {
            assert!(tokio::time::Instant::now() < deadline, "空闲看护没关进程");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(*kill_rx.borrow(), "关进程时也要通知 SSE 任务收工");
        let _ = handle.await;
    }

    #[tokio::test]
    async fn idle_watch_keeps_process_while_a_turn_is_busy() {
        let (shared, _rx) = fake_shared(); // ses_a 默认 busy=true
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(idle_watch(
            Arc::clone(&shared),
            Duration::from_millis(40),
            shutdown_rx,
            shutdown_tx.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !shared.closed.load(Ordering::SeqCst),
            "有回合在跑不该关进程"
        );
        handle.abort();
    }

    fn retry_event(attempt: u64, message: &str) -> Value {
        json!({
            "type": "session.status",
            "properties": {
                "sessionID": "ses_a",
                "status": {"type": "retry", "attempt": attempt, "message": message, "next": Value::Null}
            }
        })
    }

    fn text_event(delta: &str) -> Value {
        json!({
            "type": "message.part.delta",
            "properties": {"sessionID": "ses_a", "partID": "prt_1", "field": "text", "delta": delta}
        })
    }

    fn idle_event() -> Value {
        json!({"type": "session.idle", "properties": {"sessionID": "ses_a"}})
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn conv_of(shared: &Shared) -> Arc<Conversation> {
        shared.conversation("ses_a").unwrap()
    }

    /// 模拟 send_inner 开新回合：重置标记、回合序号加一、置「在跑」。
    fn begin_new_turn(shared: &Shared) {
        let conv = conv_of(shared);
        {
            let mut inner = conv.inner.lock().unwrap();
            inner.turn = TurnFlags::default();
            inner.turn_seq += 1;
        }
        conv.busy.store(true, Ordering::SeqCst);
    }

    #[test]
    fn retry_limit_pure_logic() {
        assert!(!retry_limit_reached(1));
        assert!(!retry_limit_reached(3));
        assert!(retry_limit_reached(4));
        assert!(retry_limit_reached(9));
        let msg = retry_limit_message("请求太频繁，被服务商限流了");
        assert!(msg.starts_with("重试了 3 次仍然失败：请求太频繁，被服务商限流了。"));
        assert!(msg.contains("已停止这一轮"));
    }

    #[test]
    fn retries_one_to_three_pass_through_to_the_ui() {
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=3 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        let got = drain(&mut rx);
        assert_eq!(got.len(), 3);
        for (i, ev) in got.iter().enumerate() {
            match ev {
                AgentEvent::Retrying {
                    attempt, reason, ..
                } => {
                    assert_eq!(*attempt as usize, i + 1);
                    assert_eq!(reason, "服务商那边暂时忙不过来");
                }
                other => panic!("该是 Retrying，得到 {other:?}"),
            }
        }
        assert!(
            conv_of(&shared).busy.load(Ordering::SeqCst),
            "放行阶段回合还在跑"
        );
    }

    #[test]
    fn fourth_retry_stops_the_turn_and_ends_it_cleanly_without_a_runtime() {
        // 没有 tokio 运行时：没法发中止请求，直接收尾，界面也不会卡在「思考中」。
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=4 {
            handle_event(
                &shared,
                &retry_event(attempt, "Free usage exceeded, subscribe to Go"),
            );
        }
        let got = drain(&mut rx);
        assert_eq!(got.len(), 5, "3 条重试 + 错误 + 收尾: {got:?}");
        assert!(got[..3]
            .iter()
            .all(|e| matches!(e, AgentEvent::Retrying { .. })));
        match &got[3] {
            AgentEvent::Error { message } => {
                assert!(message.starts_with("重试了 3 次仍然失败："), "{message}");
                assert!(message.contains("免费额度用完了"), "{message}");
                assert!(message.contains("已停止这一轮"), "{message}");
            }
            other => panic!("该是 Error，得到 {other:?}"),
        }
        assert_eq!(got[4], AgentEvent::TurnEnd);
        assert!(!conv_of(&shared).busy.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn fourth_retry_aborts_then_idle_ends_turn_with_error_before_turn_end() {
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=4 {
            handle_event(&shared, &retry_event(attempt, "Rate limit reached"));
        }
        // 超限这一刻只放行了前 3 条，错误要等收尾时才发。
        assert_eq!(drain(&mut rx).len(), 3);
        // abort 带来的 session.error 要被吞掉。
        handle_event(
            &shared,
            &json!({"type": "session.error", "properties": {"sessionID": "ses_a",
                "error": {"name": "MessageAbortedError", "data": {"message": "aborted"}}}}),
        );
        assert!(drain(&mut rx).is_empty());
        // opencode 中止后发 idle：先错误后收尾，各一次。
        handle_event(&shared, &idle_event());
        let got = drain(&mut rx);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(matches!(&got[0], AgentEvent::Error { message } if message.contains("请求太频繁")));
        assert_eq!(got[1], AgentEvent::TurnEnd);
        // 宽限期过后兜底收尾不能重复发。
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(600)).await;
        assert!(drain(&mut rx).is_empty(), "兜底收尾不该重复");
    }

    #[tokio::test]
    async fn fourth_retry_falls_back_to_our_own_turn_end_when_no_idle_arrives() {
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=4 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        assert_eq!(drain(&mut rx).len(), 3);
        // 一直没有 idle：宽限期后由我们补 Error + TurnEnd。
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(1500)).await;
        let got = drain(&mut rx);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            matches!(&got[0], AgentEvent::Error { message } if message.contains("暂时忙不过来"))
        );
        assert_eq!(got[1], AgentEvent::TurnEnd);
        assert!(!conv_of(&shared).busy.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn fallback_does_not_touch_a_newer_turn() {
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=4 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        drain(&mut rx);
        // 宽限期内 idle 收尾，随后用户马上开了新回合。
        handle_event(&shared, &idle_event());
        drain(&mut rx);
        begin_new_turn(&shared);
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(600)).await;
        assert!(drain(&mut rx).is_empty());
        assert!(
            conv_of(&shared).busy.load(Ordering::SeqCst),
            "新回合不能被旧的兜底收尾结束"
        );
    }

    #[test]
    fn counter_restarts_with_a_new_turn() {
        let (shared, mut rx) = fake_shared();
        for attempt in 1..=4 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        drain(&mut rx);
        // 新回合：第 1~3 次重试照常放行。
        begin_new_turn(&shared);
        for attempt in 1..=3 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        let got = drain(&mut rx);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|e| matches!(e, AgentEvent::Retrying { .. })));
    }

    #[test]
    fn late_retry_after_user_stop_or_turn_end_is_ignored() {
        // 用户点了停止：之后晚到的重试（包括第 4 次）不放行、不报错、不收尾。
        let (shared, mut rx) = fake_shared();
        conv_of(&shared).inner.lock().unwrap().turn.aborted = true;
        for attempt in 1..=5 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        assert!(drain(&mut rx).is_empty());
        assert!(conv_of(&shared).busy.load(Ordering::SeqCst));

        // 回合已经结束（不在跑）：同样忽略。
        let (shared, mut rx) = fake_shared();
        conv_of(&shared).busy.store(false, Ordering::SeqCst);
        for attempt in 1..=5 {
            handle_event(&shared, &retry_event(attempt, "Overloaded"));
        }
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn progress_after_retries_keeps_the_turn_going_normally() {
        // 重试成功后出了正文：照常结束，不会有多余错误。
        let (shared, mut rx) = fake_shared();
        handle_event(&shared, &retry_event(1, "Overloaded"));
        handle_event(&shared, &retry_event(2, "Overloaded"));
        handle_event(&shared, &text_event("好了"));
        handle_event(&shared, &idle_event());
        let got = drain(&mut rx);
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(matches!(got[0], AgentEvent::Retrying { attempt: 1, .. }));
        assert!(matches!(got[1], AgentEvent::Retrying { attempt: 2, .. }));
        assert!(matches!(&got[2], AgentEvent::TextDelta { text } if text == "好了"));
        assert_eq!(got[3], AgentEvent::TurnEnd);
        // opencode 的 attempt 按它自己的一次调用计，成功后下一次调用又从 1 开始：不会累计到超限。
        begin_new_turn(&shared);
        handle_event(&shared, &retry_event(1, "Overloaded"));
        assert!(matches!(
            drain(&mut rx)[..],
            [AgentEvent::Retrying { attempt: 1, .. }]
        ));
    }
}
