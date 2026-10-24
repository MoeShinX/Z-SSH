//! AI 面板的后端转接（卡片 AI-2 / AI-2b，AI-3 起支持多个智能体）：发现、管理
//! **共用的**智能体进程、起停对话、把驱动的统一事件转成 Tauri 事件发给前端。
//!
//! 业务逻辑在 `zssh_core::ai::agent`；这里只做参数搬运、状态登记和事件转发。
//!
//! 每个智能体（opencode / codex）各自**共用一个**进程：第一次用 AI 时启动，所有
//! 对话空闲 10 分钟后自动关闭，下次再用时重新启动。每个对话另有自己的工具桥
//! scope（含 128 位能力密钥）和事件转发任务；标签关闭时销毁 scope、摘掉对话，进程
//! 留给其他对话继续用。应用退出时连进程树一起收掉。智能体是**每个对话**固定的：
//! 起对话时选，不能中途换（换 = 新开一个对话）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;
use zssh_core::ai::agent::claude;
use zssh_core::ai::agent::codex::{
    self, CodexRuntime, DEFAULT_IDLE_TIMEOUT as CODEX_IDLE_TIMEOUT,
    DEFAULT_STARTUP_TIMEOUT as CODEX_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::config::{build_config_content, read_user_model_config, McpServerSpec};
use zssh_core::ai::agent::discovery::{self, AgentInstall, AgentSpec, CLAUDE, CODEX, OPENCODE};
use zssh_core::ai::agent::download;
use zssh_core::ai::agent::opencode::{
    default_config_home, AgentRuntime, StartOptions as OpenCodeStartOptions, DEFAULT_IDLE_TIMEOUT,
    DEFAULT_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::{AgentConversation, AgentEvent, AgentModelCatalog};
use zssh_core::ai::bridge::{Bridge, ScopeHandle};
use zssh_core::ai::history::{self, HistoryDraft, HistoryRecord, HistoryStore, HistorySummary};
use zssh_core::ai::protocol::TargetSummary;

use crate::state::AppState;

/// 事件名：一个对话的流式事件（文字/思考增量、工具调用、回合结束、错误）。
pub const AI_AGENT_EVENT: &str = "ai_agent_event";
/// 事件名：一键下载的进度。
pub const AI_DOWNLOAD_PROGRESS: &str = "ai_download_progress";

/// 工具桥句柄：scope 从这里创建，桥端口和连接层令牌给 `zssh-mcp` 连回用。
pub struct ToolBridge {
    bridge: Arc<Bridge>,
    port: u16,
}

impl ToolBridge {
    pub fn new(bridge: Arc<Bridge>, port: u16) -> Self {
        Self { bridge, port }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 连接层令牌（注入 `zssh-mcp.exe`；同一种智能体的对话共用一个进程）。
    pub fn connection_token(&self) -> &str {
        self.bridge.connection_token()
    }
}

/// 支持的智能体（AI-3 加 codex；Claude Code 在 AI-4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    OpenCode,
    Codex,
    Claude,
}

impl AgentKind {
    /// 前端用的 id（也是注册表里进程槽位的键）。
    pub fn id(self) -> &'static str {
        match self {
            AgentKind::OpenCode => "opencode",
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
        }
    }

    /// 通用发现模块里的规则。
    pub fn spec(self) -> &'static AgentSpec {
        match self {
            AgentKind::OpenCode => &OPENCODE,
            AgentKind::Codex => &CODEX,
            AgentKind::Claude => &CLAUDE,
        }
    }
}

/// 解析前端传来的智能体 id。
pub fn parse_agent(raw: &str) -> Result<AgentKind, String> {
    match raw.trim() {
        "opencode" | "" => Ok(AgentKind::OpenCode),
        "codex" => Ok(AgentKind::Codex),
        "claude" => Ok(AgentKind::Claude),
        other => Err(format!("不认识的智能体：{other}")),
    }
}

/// 一个共用的智能体进程（两种驱动各自一个槽位）。
enum SharedRuntime {
    OpenCode(Arc<AgentRuntime>),
    Codex(Arc<CodexRuntime>),
}

impl SharedRuntime {
    fn is_closed(&self) -> bool {
        match self {
            SharedRuntime::OpenCode(runtime) => runtime.is_closed(),
            SharedRuntime::Codex(runtime) => runtime.is_closed(),
        }
    }

    async fn shutdown(&self) {
        match self {
            SharedRuntime::OpenCode(runtime) => runtime.shutdown().await,
            SharedRuntime::Codex(runtime) => runtime.shutdown().await,
        }
    }
}

/// 共用的智能体进程槽位。
struct RuntimeSlot {
    runtime: SharedRuntime,
}

/// 一个正在跑的对话。
struct Conversation {
    conversation: Arc<dyn AgentConversation>,
    /// 工具桥 scope；关对话时销毁（能力密钥立即失效、挂着的审批按拒绝结束）。
    scope: ScopeHandle,
    /// 事件转发任务。
    pump: tauri::async_runtime::JoinHandle<()>,
}

/// 对话表：前端用 `session_key`（标签 id）认领自己的对话。
#[derive(Default)]
pub struct AgentRegistry {
    next_key: AtomicU64,
    /// 每种智能体一个共用进程（懒启动；空闲超时或连接断开后会变成 None）。
    runtimes: Mutex<HashMap<&'static str, RuntimeSlot>>,
    conversations: Mutex<HashMap<String, Conversation>>,
    /// 正在跑的下载的取消标志。
    download_cancel: Arc<AtomicBool>,
    /// 启动共用进程时串行：面板一打开就预取模型目录，可能和用户点"+"同时到，
    /// 不串行的话两边都看到"没有进程"，会各起一个。
    start_lock: tokio::sync::Mutex<()>,
}

impl AgentRegistry {
    fn conversations(&self) -> std::sync::MutexGuard<'_, HashMap<String, Conversation>> {
        self.conversations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 某种智能体的共用进程句柄（诊断 / 登录判断用）。
    fn runtime(&self, kind: AgentKind) -> Option<SharedRuntime> {
        let runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtimes.get(kind.id()).map(|slot| match &slot.runtime {
            SharedRuntime::OpenCode(runtime) => SharedRuntime::OpenCode(Arc::clone(runtime)),
            SharedRuntime::Codex(runtime) => SharedRuntime::Codex(Arc::clone(runtime)),
        })
    }

    /// 生成一个对话键（前端拿它和标签绑定）。
    fn next_key(&self) -> String {
        let id = self.next_key.fetch_add(1, Ordering::SeqCst) + 1;
        format!("ai-{id}")
    }
}

/// 给前端的驱动事件（带对话键，前端据此归位）。
#[derive(Debug, Clone, Serialize)]
pub struct AgentEventPayload {
    pub session_key: String,
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// 一次成功的启动返回给前端的信息。
#[derive(Debug, Clone, Serialize)]
pub struct AgentStartInfo {
    pub session_key: String,
    /// 工具桥 scope id；审批事件里带的是它，前端据此认领本对话的确认请求。
    pub scope_id: String,
    /// 智能体侧的会话 id（诊断用）。
    pub agent_session_id: String,
    /// 本对话用的智能体（每个对话固定，不能中途换）。
    pub agent: String,
    /// 要求续接历史会话时：是否接上了智能体自己的会话。false = 作为新会话继续
    /// （界面提示"之前的上下文没有带给 AI"）。Claude Code 要到第一次发消息才知道，
    /// 这里先按 true，续不上时另发 `resume_failed` 事件。
    pub resumed: bool,
}

/// @ 主机选择弹窗用的一项（主机库里的一台主机；**不含地址和凭据**）。
#[derive(Debug, Clone, Serialize)]
pub struct HostPickerEntry {
    pub host_id: i64,
    pub name: String,
    pub username: String,
}

/// 给 @ 选择弹窗列主机库里的主机（只有名字和用户名，不给地址）。
#[tauri::command]
pub async fn ai_host_picker(state: State<'_, AppState>) -> Result<Vec<HostPickerEntry>, String> {
    let hosts = state.store.host_list().map_err(|e| e.to_string())?;
    Ok(hosts
        .into_iter()
        .map(|host| HostPickerEntry {
            host_id: host.id,
            name: host.name,
            username: host.username,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// 发现 / 探测
// ---------------------------------------------------------------------------

/// 找可用的智能体：设置里的路径（手动兜底）→ 自动发现（tools 目录 → PATH → 常见位置）。
///
/// `agent` 是 `"opencode"` 或 `"codex"`。`Err` 只在设置里的路径不可用时返回
/// （用户明确指定的坏了要报出来，且不回退）。
#[tauri::command]
pub async fn ai_agent_discover(
    app: AppHandle,
    state: State<'_, AppState>,
    agent: String,
) -> Result<Option<AgentInstall>, String> {
    let kind = parse_agent(&agent)?;
    let path = agent_setting_path(&state, kind)?;
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    discovery::discover(kind.spec(), path.as_deref(), &data_dir).await
}

/// 设置里手动指定的路径（空串/没设置 = 自动发现）。
fn agent_setting_path(
    state: &State<'_, AppState>,
    kind: AgentKind,
) -> Result<Option<String>, String> {
    let path = match kind {
        AgentKind::OpenCode => state.store.ai_opencode_path(),
        AgentKind::Codex => state.store.ai_codex_path(),
        AgentKind::Claude => state.store.ai_claude_path(),
    };
    path.map_err(|e| e.to_string())
}

/// 探测一个指定路径（设置页「检测」按钮 / 手动指定）。
#[tauri::command]
pub async fn ai_agent_probe_path(agent: String, path: String) -> Result<AgentInstall, String> {
    let kind = parse_agent(&agent)?;
    discovery::probe_path(kind.spec(), std::path::Path::new(&path)).await
}

// ---------------------------------------------------------------------------
// 共用进程：取用 / 启动
// ---------------------------------------------------------------------------

/// opencode 的注入配置：挂上 Z-SSH 工具桥的 MCP，并禁掉本机内置工具。
///
/// 用户自带的 MCP 靠重定向 `XDG_CONFIG_HOME` 整体隔离（见 core 的
/// `ai/agent/opencode.rs` / `config.rs` 说明），用户的全局配置不会被加载，所以这里
/// 不需要、也不能按名字去禁用某个用户自带的服务器。
fn opencode_config_content(mcp: &McpServerSpec, user_model_config: &serde_json::Value) -> String {
    build_config_content(mcp, &[], user_model_config)
}

/// 取用某种智能体的现成共用进程；没有或已关停就重新启动一个。
async fn runtime_for(
    registry: &AgentRegistry,
    data_dir: &std::path::Path,
    tool_bridge: &ToolBridge,
    kind: AgentKind,
    install: AgentInstall,
) -> Result<SharedRuntime, String> {
    let _starting = registry.start_lock.lock().await;
    {
        let runtimes = registry
            .runtimes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = runtimes.get(kind.id()) {
            if !slot.runtime.is_closed() {
                return Ok(match &slot.runtime {
                    SharedRuntime::OpenCode(runtime) => {
                        SharedRuntime::OpenCode(Arc::clone(runtime))
                    }
                    SharedRuntime::Codex(runtime) => SharedRuntime::Codex(Arc::clone(runtime)),
                });
            }
        }
    }

    let mcp_binary = mcp_binary_path()?;
    let spec = McpServerSpec::zssh(
        &mcp_binary,
        tool_bridge.port(),
        tool_bridge.connection_token(),
    );
    let workspace = data_dir.join("agent-workspace");

    let runtime = match kind {
        // claude 不用共用进程（一个对话一个进程），走不到这儿。
        AgentKind::Claude => {
            return Err("claude 不使用共用进程".to_string());
        }
        AgentKind::OpenCode => {
            // 只带上用户配置里的模型相关键（自建服务商照常可用），其余一律不带。
            let config_content = opencode_config_content(&spec, &read_user_model_config());
            let config_home = default_config_home(&workspace);
            let options = OpenCodeStartOptions {
                install,
                workspace,
                config_home,
                config_content,
                startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            };
            SharedRuntime::OpenCode(
                AgentRuntime::start(options, idle_timeout(DEFAULT_IDLE_TIMEOUT))
                    .await
                    .map_err(|e| e.to_string())?,
            )
        }
        AgentKind::Codex => {
            let options = codex::StartOptions {
                install: install.clone(),
                workspace,
                mcp: spec,
                probes: codex::startup::command_probes(&install.path),
                codex_home: None,
                startup_timeout: CODEX_STARTUP_TIMEOUT,
            };
            SharedRuntime::Codex(
                CodexRuntime::start(options, idle_timeout(CODEX_IDLE_TIMEOUT))
                    .await
                    .map_err(|e| e.to_string())?,
            )
        }
    };

    let mut runtimes = registry
        .runtimes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    runtimes.insert(kind.id(), RuntimeSlot { runtime });
    let slot = runtimes.get(kind.id()).expect("刚插入的槽位");
    Ok(match &slot.runtime {
        SharedRuntime::OpenCode(runtime) => SharedRuntime::OpenCode(Arc::clone(runtime)),
        SharedRuntime::Codex(runtime) => SharedRuntime::Codex(Arc::clone(runtime)),
    })
}

/// claude 的空闲关闭时间（同样的环境变量覆盖；默认 10 分钟）。
fn claude_idle_timeout() -> std::time::Duration {
    idle_timeout(claude::DEFAULT_IDLE_TIMEOUT)
}

/// 空闲关闭时间：`ZSSH_AGENT_IDLE_SECS` 可覆盖（测试里调小）。
fn idle_timeout(fallback: std::time::Duration) -> std::time::Duration {
    std::env::var("ZSSH_AGENT_IDLE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .filter(|d| !d.is_zero())
        .unwrap_or(fallback)
}

/// 找某种智能体的安装：给定路径优先，否则设置里的路径（手动兜底）→ 自动发现。
async fn resolve_install(
    state: &State<'_, AppState>,
    data_dir: &std::path::Path,
    kind: AgentKind,
    install_path: Option<String>,
) -> Result<Option<AgentInstall>, String> {
    match install_path.filter(|p| !p.trim().is_empty()) {
        Some(path) => discovery::probe_path(kind.spec(), std::path::Path::new(&path))
            .await
            .map(Some)
            .map_err(|e| format!("指定的 {} 不能用：{e}", kind.id())),
        None => {
            let setting = agent_setting_path(state, kind)?;
            discovery::discover(kind.spec(), setting.as_deref(), data_dir).await
        }
    }
}

// ---------------------------------------------------------------------------
// 登录判断（AI-3 第 5 节）
// ---------------------------------------------------------------------------

/// 核对 codex 的本机工具能不能确认关掉（AI-3 验收 0.1，设置页 / 面板显示用）。
///
/// `Ok(None)` = 通过；`Ok(Some(原因))` = 拒绝启动的完整文案（带统一前缀）。
/// 启动路径自己也会检查一遍（`startup_plan`），这里只是让界面**提前**显示原因。
#[tauri::command]
pub async fn ai_agent_guard(
    app: AppHandle,
    state: State<'_, AppState>,
    agent: String,
    install_path: Option<String>,
) -> Result<Option<String>, String> {
    let kind = parse_agent(&agent)?;
    if kind != AgentKind::Codex {
        return Ok(None);
    }
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let Some(install) = resolve_install(&state, &data_dir, kind, install_path).await? else {
        return Ok(None);
    };
    match codex::startup::guard_check(std::path::Path::new(&install.path)).await {
        Ok(()) => Ok(None),
        Err(reason) => Ok(Some(reason)),
    }
}

/// Claude Code 的登录判断（AI-4 第 4 节）：`claude auth status` → 兜底
/// （环境变量 / 凭据文件 / settings.json 的 env 块）。**绝不读取或打印密钥的值**。
async fn claude_login(
    app: &AppHandle,
    state: &State<'_, AppState>,
    agent: &str,
) -> Result<AgentLoginStatus, String> {
    let kind = parse_agent(agent)?;
    let data_dir = crate::resolve_data_dir(app).map_err(|e| e.to_string())?;
    let Some(install) = resolve_install(state, &data_dir, kind, None).await? else {
        return Ok(AgentLoginStatus {
            logged_in: None,
            hint: Some(
                "没有找到 Claude Code。安装方法：npm i -g @anthropic-ai/claude-code".to_string(),
            ),
        });
    };
    // ① auth status（开发时实测 2.1.300 有这个子命令）。
    if let Some(login) = claude::login::login_status_cli(&install.path).await {
        return Ok(AgentLoginStatus {
            logged_in: login.logged_in,
            hint: login.hint,
        });
    }
    // ② 兜底：环境变量 / 凭据文件 / settings.json 的 env 块。读不出来时
    //    `logged_in: None`（不确定）：提示用户，但不拦着他用（R-2 第 3 节）。
    if let Some(login) = claude::login::login_status_fallback() {
        return Ok(AgentLoginStatus {
            logged_in: login.logged_in,
            hint: login.hint,
        });
    }
    Ok(AgentLoginStatus {
        logged_in: Some(false),
        hint: Some("请先在终端里运行 `claude` 完成登录".to_string()),
    })
}

/// 登录状态。`logged_in` 为 `None` 表示这个智能体没有"登录状态"这个概念
/// （opencode 的登录在它自己的界面里做）。
#[derive(Debug, Clone, Serialize)]
pub struct AgentLoginStatus {
    pub logged_in: Option<bool>,
    pub hint: Option<String>,
}

/// 判断智能体能不能用（登录了没有）。**只判断有没有，绝不读取或打印密钥的值。**
///
/// codex 的顺序：共用进程活着就问 `account/read`（最权威）→ `codex login status`
/// → 用户 `config.toml` 的服务商配置兜底。
#[tauri::command]
pub async fn ai_agent_login(
    app: AppHandle,
    state: State<'_, AppState>,
    registry: State<'_, AgentRegistry>,
    agent: String,
) -> Result<AgentLoginStatus, String> {
    let kind = parse_agent(&agent)?;
    if kind == AgentKind::Claude {
        return claude_login(&app, &state, &agent).await;
    }
    if kind != AgentKind::Codex {
        return Ok(AgentLoginStatus {
            logged_in: None,
            hint: None,
        });
    }

    // ① 共用进程还活着：直接问它。
    if let Some(SharedRuntime::Codex(runtime)) = registry.runtime(kind) {
        if !runtime.is_closed() {
            return match runtime.account_logged_in().await {
                Ok(true) => Ok(AgentLoginStatus {
                    logged_in: Some(true),
                    hint: None,
                }),
                Ok(false) => Ok(AgentLoginStatus {
                    logged_in: Some(false),
                    hint: Some("请先在终端里运行 `codex login` 完成登录".to_string()),
                }),
                Err(err) => Err(err.to_string()),
            };
        }
    }

    // ② 没装：把安装提示带出去（界面据此显示引导）。
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let Some(install) = resolve_install(&state, &data_dir, kind, None).await? else {
        return Ok(AgentLoginStatus {
            logged_in: None,
            hint: Some("没有找到 codex。安装方法：npm i -g @openai/codex".to_string()),
        });
    };

    // ③ `codex login status`。
    if let Some(login) = codex::startup::login_status_cli(std::path::Path::new(&install.path)).await
    {
        if login.logged_in {
            return Ok(AgentLoginStatus {
                logged_in: Some(true),
                hint: None,
            });
        }
    }

    // ④ 用户配置里的自建服务商兜底（env_key 没设置时给可操作提示）。
    //    配置文件**存在但读不出来**时报"不确定"（`None`）：读不到不等于没登录
    //    （R-2 第 3 节），不能因此把人挡住。
    match codex::startup::read_config_toml(None) {
        codex::startup::ConfigRead::Read(text) => {
            if let Some(by_config) = codex::startup::login_status_from_config_text(
                &text,
                &|key| std::env::var_os(key).is_some(),
            ) {
                return Ok(AgentLoginStatus {
                    logged_in: Some(by_config.logged_in),
                    hint: by_config.hint,
                });
            }
        }
        codex::startup::ConfigRead::Unreadable(reason) => {
            return Ok(AgentLoginStatus {
                logged_in: None,
                hint: Some(format!(
                    "没能确认登录状态（{reason}）。已经登录过就能直接用；不行就在终端里跑一次 `codex login`"
                )),
            })
        }
        codex::startup::ConfigRead::Missing => {}
    }

    Ok(AgentLoginStatus {
        logged_in: Some(false),
        hint: Some("请先在终端里运行 `codex login` 完成登录".to_string()),
    })
}

// ---------------------------------------------------------------------------
// 起停对话
// ---------------------------------------------------------------------------

/// 启动一个对话：复用（或启动）该智能体的共用进程，在里面开一个会话，把流式
/// 事件转发给前端。
#[tauri::command]
pub async fn ai_agent_start(
    app: AppHandle,
    state: State<'_, AppState>,
    registry: State<'_, AgentRegistry>,
    agent: String,
    install_path: Option<String>,
    resume_history_id: Option<String>,
) -> Result<AgentStartInfo, String> {
    let kind = parse_agent(&agent)?;
    let bridge = app.state::<ToolBridge>();
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    // 续接：只从我们自己存的历史里取智能体会话标识（前端只给历史 id）。读不到 = 新会话。
    let resume = match resume_history_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => HistoryStore::new(&data_dir)
            .load(kind.id(), &id)
            .map_err(|err| tracing::info!(error = %err, "续接时读不到历史会话"))
            .ok()
            .and_then(|record| record.agent_session_id),
        None => None,
    };

    let install = resolve_install(&state, &data_dir, kind, install_path)
        .await?
        .ok_or_else(|| {
            format!(
                "没有找到 {}。请在设置里指定路径，或按官方说明安装。",
                kind.id()
            )
        })?;

    // 每个对话一个工具桥 scope：128 位能力密钥通过系统提示 / 指令交给模型。
    let scope = bridge.bridge.create_scope();
    let session_key = registry.next_key();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let capability = scope.capability().to_string();
    let (conversation, resumed): (Arc<dyn AgentConversation>, bool) = match kind {
        // opencode / codex：共用进程里开一个会话（有历史标识就先试着接上）。
        AgentKind::OpenCode | AgentKind::Codex => {
            let runtime = runtime_for(&registry, &data_dir, bridge.inner(), kind, install).await?;
            match &runtime {
                SharedRuntime::OpenCode(runtime) => {
                    let (conversation, resumed) = runtime
                        .open_conversation_resuming(capability, tx, resume.as_deref())
                        .await
                        .map_err(|e| e.to_string())?;
                    conversation
                        .bind_tool_scope(scope.clone())
                        .map_err(|e| e.to_string())?;
                    (conversation as Arc<dyn AgentConversation>, resumed)
                }
                SharedRuntime::Codex(runtime) => {
                    let (conversation, resumed) = runtime
                        .open_conversation_resuming(capability, tx, resume.as_deref())
                        .await
                        .map_err(|e| e.to_string())?;
                    (conversation as Arc<dyn AgentConversation>, resumed)
                }
            }
        }
        // claude：一个对话一个进程（懒启动：第一次发消息才拉；空闲 10 分钟关，
        // 下次发消息 `--resume` 恢复上下文）。
        AgentKind::Claude => {
            let mcp = McpServerSpec::zssh(
                &mcp_binary_path()?,
                bridge.port(),
                bridge.connection_token(),
            );
            let options = claude::StartOptions {
                install,
                workspace: data_dir.join("agent-workspace"),
                mcp,
                capability,
                events: tx,
                extra_env: Vec::new(),
                resume_session_id: resume.clone(),
            };
            let conversation = claude::ClaudeConversation::create(options, claude_idle_timeout())
                .await
                .map_err(|e| e.to_string())?
                as Arc<dyn AgentConversation>;
            (conversation, resume.is_some())
        }
    };

    // 事件转发：驱动事件 → Tauri 事件（带对话键）。
    let emit_app = app.clone();
    let emit_key = session_key.clone();
    let pump = tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let payload = AgentEventPayload {
                session_key: emit_key.clone(),
                event,
            };
            let _ = emit_app.emit(AI_AGENT_EVENT, payload);
        }
    });

    let info = AgentStartInfo {
        session_key: session_key.clone(),
        scope_id: scope.scope_id().to_string(),
        agent_session_id: conversation.session_id().to_string(),
        agent: kind.id().to_string(),
        resumed,
    };
    registry.conversations().insert(
        session_key,
        Conversation {
            conversation,
            scope,
            pump,
        },
    );
    Ok(info)
}

/// 结束一个对话：摘掉会话、销毁 scope。共用进程留给别的对话继续用（空闲后自动关）。
#[tauri::command]
pub async fn ai_agent_stop(
    registry: State<'_, AgentRegistry>,
    session_key: String,
) -> Result<(), String> {
    let conversation = registry.conversations().remove(&session_key);
    if let Some(conversation) = conversation {
        conversation.conversation.shutdown().await;
        conversation.pump.abort();
        conversation.scope.destroy();
    }
    Ok(())
}

/// 发一条消息。`model` 形如 `provider/model`，None 用默认。
#[tauri::command]
pub async fn ai_agent_send(
    registry: State<'_, AgentRegistry>,
    session_key: String,
    text: String,
    model: Option<String>,
) -> Result<(), String> {
    let conversation = conversation_of(&registry, &session_key)?;
    conversation
        .send(&text, model.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// 停止当前回合。
///
/// AI-2d 1.4：这个对话如果**确实**有一条 AI 命令正在某个终端里跑，先向那个终端
/// 发 Ctrl+C（入口 [`Bridge::interrupt_terminal`]，宿主用
/// [`zssh_core::ai::runs::TerminalRuns`] 判断有没有在跑的命令；没有就什么都不发，
/// 不会打断用户自己跑的东西），再让 agent 停回合。
#[tauri::command]
pub async fn ai_agent_abort(
    registry: State<'_, AgentRegistry>,
    bridge: State<'_, ToolBridge>,
    session_key: String,
) -> Result<(), String> {
    let conversation = conversation_of(&registry, &session_key)?;
    let scope_id = scope_id_of(&registry, &session_key)?;
    // 先作废这个对话里已经发出的工具请求（等审批的按拒绝结束、后台命令关通道、没开始写的
    // 文件不再写），再中断终端里的命令和智能体的回合（R-9 F12）。
    bridge.bridge.stop_scope(&scope_id);
    bridge.bridge.interrupt_terminal(&scope_id).await;
    conversation.stop().await.map_err(|e| e.to_string())
}

/// 查可用模型。
#[tauri::command]
pub async fn ai_agent_models(
    registry: State<'_, AgentRegistry>,
    session_key: String,
) -> Result<AgentModelCatalog, String> {
    let conversation = conversation_of(&registry, &session_key)?;
    conversation.models().await.map_err(|e| e.to_string())
}

/// 不开对话直接查某个智能体的模型目录（面板一打开 / 切换智能体时显示默认模型用）。
///
/// opencode / codex 要用共用进程：没在跑就照常启动一个，之后没人用会按空闲超时自己
/// 关掉，不常驻。Claude Code 不拉对话进程，只跑一次 `--help` 探测参数。
#[tauri::command]
pub async fn ai_agent_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
    registry: State<'_, AgentRegistry>,
    agent: String,
    install_path: Option<String>,
) -> Result<AgentModelCatalog, String> {
    let kind = parse_agent(&agent)?;
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let install = resolve_install(&state, &data_dir, kind, install_path)
        .await?
        .ok_or_else(|| format!("没有找到 {}", kind.id()))?;
    if kind == AgentKind::Claude {
        return Ok(claude::catalog_for_install(&install.path).await);
    }
    let bridge = app.state::<ToolBridge>();
    let runtime = runtime_for(&registry, &data_dir, bridge.inner(), kind, install).await?;
    match runtime {
        SharedRuntime::OpenCode(runtime) => runtime.models().await,
        SharedRuntime::Codex(runtime) => runtime.models().await,
    }
    .map_err(|e| e.to_string())
}

/// 目标进出 scope 的同步（AI-2c）：当前焦点终端 + @ 进来的主机。
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeTargetChange {
    /// 目标 ID：主机库里的主机一律是 `host-<host_id>`（开了几个终端都不变）；快速连接这类
    /// 没有主机记录的会话是会话 ID。
    pub target_id: String,
    pub host_label: String,
    pub username: String,
    pub status: String,
    /// 当前焦点终端那台。
    #[serde(default)]
    pub current: bool,
    /// 用户 @ 进来的主机。
    #[serde(default)]
    pub pinned: bool,
    /// 这个目标绑定的会话（这台主机焦点所在 / 最近获得焦点的终端）。执行时优先用它；
    /// 没有（@ 了没打开的主机）或已经关了，再找这台主机别的已连会话 / 自动连接。
    #[serde(default)]
    pub session_id: Option<String>,
}

/// 同步一个对话的目标列表：`added` 加进来，`removed` 摘掉。同一个 ID 再同步会更新。
#[tauri::command]
pub async fn ai_agent_scope_sync(
    registry: State<'_, AgentRegistry>,
    session_key: String,
    added: Vec<ScopeTargetChange>,
    removed: Vec<String>,
) -> Result<(), String> {
    let scope = {
        let conversations = registry.conversations();
        let conversation = conversations
            .get(&session_key)
            .ok_or_else(|| "对话不存在或已结束".to_string())?;
        conversation.scope.clone()
    };
    for change in added {
        scope.add_target_summary(TargetSummary {
            id: change.target_id,
            host_label: change.host_label,
            username: change.username,
            status: change.status,
            current: change.current,
            pinned: change.pinned,
            session_id: change.session_id.filter(|id| !id.is_empty()),
        });
    }
    for target_id in removed {
        scope.remove_target(&target_id);
    }
    Ok(())
}

/// 释放一个对话（对话已经停了时的兜底）：清 scope、摘会话。
#[tauri::command]
pub async fn ai_agent_release(
    registry: State<'_, AgentRegistry>,
    session_key: String,
) -> Result<(), String> {
    let conversation = registry.conversations().remove(&session_key);
    if let Some(conversation) = conversation {
        conversation.scope.destroy();
        conversation.conversation.shutdown().await;
        conversation.pump.abort();
    }
    Ok(())
}

fn conversation_of(
    registry: &State<'_, AgentRegistry>,
    key: &str,
) -> Result<Arc<dyn AgentConversation>, String> {
    registry
        .conversations()
        .get(key)
        .map(|c| Arc::clone(&c.conversation))
        .ok_or_else(|| "对话不存在或已结束".to_string())
}

/// 查一个对话的工具桥 scope id（停止按钮用它找"这个对话在终端里跑着的命令"）。
fn scope_id_of(registry: &State<'_, AgentRegistry>, key: &str) -> Result<String, String> {
    registry
        .conversations()
        .get(key)
        .map(|c| c.scope.scope_id().to_string())
        .ok_or_else(|| "对话不存在或已结束".to_string())
}

// ---------------------------------------------------------------------------
// 历史会话（R11）：存储、上限、脱敏都在 core 的 `ai::history`，这里只转接
// ---------------------------------------------------------------------------

fn history_store(app: &AppHandle) -> Result<HistoryStore, String> {
    let data_dir = crate::resolve_data_dir(app).map_err(|e| e.to_string())?;
    Ok(HistoryStore::new(&data_dir))
}

/// 某个智能体的历史列表（新的在前）。
#[tauri::command]
pub async fn ai_history_list(app: AppHandle, agent: String) -> Result<Vec<HistorySummary>, String> {
    let kind = parse_agent(&agent)?;
    let store = history_store(&app)?;
    tauri::async_runtime::spawn_blocking(move || store.list(kind.id()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// 读一个历史会话（回看）。
#[tauri::command]
pub async fn ai_history_load(
    app: AppHandle,
    agent: String,
    id: String,
) -> Result<HistoryRecord, String> {
    let kind = parse_agent(&agent)?;
    let store = history_store(&app)?;
    tauri::async_runtime::spawn_blocking(move || store.load(kind.id(), &id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// 保存当前会话（每轮结束后一次）。智能体会话标识从还活着的对话里取，前端不经手；
/// 这个对话的能力密钥和连接令牌交给 core 从正文里抹掉。
#[tauri::command]
pub async fn ai_history_save(
    app: AppHandle,
    registry: State<'_, AgentRegistry>,
    bridge: State<'_, ToolBridge>,
    agent: String,
    session_key: Option<String>,
    draft: HistoryDraft,
) -> Result<Option<HistorySummary>, String> {
    let kind = parse_agent(&agent)?;
    let (resume_id, capability, driver_secrets) = match session_key.as_deref() {
        Some(key) => registry
            .conversations()
            .get(key)
            .map(|c| {
                (
                    c.conversation.resume_id(),
                    Some(c.scope.capability().to_string()),
                    c.conversation.history_secrets(),
                )
            })
            .unwrap_or((None, None, Vec::new())),
        None => (None, None, Vec::new()),
    };
    let token = bridge.connection_token().to_string();
    let store = history_store(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut secrets: Vec<&str> = vec![token.as_str()];
        if let Some(capability) = capability.as_deref() {
            secrets.push(capability);
        }
        secrets.extend(driver_secrets.iter().map(String::as_str));
        store.save(kind.id(), draft, resume_id, &secrets, history::now_ms())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// 删一个历史会话。
#[tauri::command]
pub async fn ai_history_delete(app: AppHandle, agent: String, id: String) -> Result<(), String> {
    let kind = parse_agent(&agent)?;
    history_store(&app)?
        .delete(kind.id(), &id)
        .map_err(|e| e.to_string())
}

/// 清空所有智能体的历史（设置页按钮），返回删了几个。
#[tauri::command]
pub async fn ai_history_clear(app: AppHandle) -> Result<usize, String> {
    let store = history_store(&app)?;
    tauri::async_runtime::spawn_blocking(move || store.clear())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// `zssh-mcp.exe` 的路径：和主程序放在同一个目录。
fn mcp_binary_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("拿不到主程序路径：{e}"))?;
    let dir = exe.parent().ok_or_else(|| "主程序没有父目录".to_string())?;
    Ok(dir.join("zssh-mcp.exe"))
}

// ---------------------------------------------------------------------------
// 一键下载
// ---------------------------------------------------------------------------

/// 下载进度事件负载。
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub received: u64,
    pub total: u64,
    /// 已经完成（解压 + 校验通过）。
    pub done: bool,
}

/// 查最新 Release 的下载信息（来源、版本、大小），供前端弹确认框。
#[tauri::command]
pub async fn ai_agent_download_info(app: AppHandle) -> Result<download::ReleaseInfo, String> {
    let proxy = proxy_url(&app).await;
    let client = download::build_client(proxy.as_deref())?;
    download::latest_release(&client).await
}

/// 一键下载 opencode：下载 zip → 解压到 `<数据目录>\tools\opencode\` → `--version` 校验。
///
/// 下载前先在界面上确认来源和大小（前端调 `ai_agent_download_info` 拿信息弹框）。
#[tauri::command]
pub async fn ai_agent_download(
    app: AppHandle,
    registry: State<'_, AgentRegistry>,
) -> Result<AgentInstall, String> {
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let proxy = proxy_url(&app).await;
    let client = download::build_client(proxy.as_deref())?;

    let release = download::latest_release(&client).await?;
    let tools_dir = discovery::tools_dir(&data_dir, &OPENCODE);
    std::fs::create_dir_all(&tools_dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let zip_path = tools_dir.join("opencode-download.zip");

    // 取消标志先清零（上一次取消过）。
    registry.download_cancel.store(false, Ordering::SeqCst);
    let cancel = Arc::clone(&registry.download_cancel);
    let emit_app = app.clone();
    download::download_to(
        &client,
        &release.download_url,
        &zip_path,
        release.size,
        move |received, total| {
            let _ = emit_app.emit(
                AI_DOWNLOAD_PROGRESS,
                DownloadProgress {
                    received,
                    total,
                    done: false,
                },
            );
        },
        &cancel,
    )
    .await
    .map_err(|e| e.to_string())?;

    // 先核对大小和 SHA-256，一致才解压、才运行（R-10 M8）；不一致时文件已经删掉。
    download::verify_download(&zip_path, release.size, &release.sha256)?;
    let exe = download::extract_exe(&zip_path, &tools_dir)?;
    let _ = std::fs::remove_file(&zip_path);

    // 探测版本：跑一次 --version（摘要已经核对过，这一步不是校验，只确认能跑、读出版本）。
    let install = discovery::probe_path(&OPENCODE, &exe)
        .await
        .map_err(|e| format!("下载完成但运行 --version 失败：{e}"))?;

    let _ = app.emit(
        AI_DOWNLOAD_PROGRESS,
        DownloadProgress {
            received: 0,
            total: 0,
            done: true,
        },
    );
    Ok(install)
}

/// 取消正在跑的下载。
#[tauri::command]
pub async fn ai_agent_download_cancel(registry: State<'_, AgentRegistry>) -> Result<(), String> {
    registry.download_cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// 软件网络代理的 URL（带账号密码；**不能打印**）。没有就是直连。
async fn proxy_url(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    let proxy = zssh_core::sync::app_proxy::app_proxy(&state.store)
        .ok()
        .flatten()?;
    Some(proxy.url_with_credentials())
}

/// 应用退出时把还活着的对话和共用进程全部收掉（不能留下孤儿进程）。
pub async fn shutdown_all(app: &AppHandle) {
    let Some(registry) = app.try_state::<AgentRegistry>() else {
        return;
    };
    let conversations: Vec<(String, Conversation)> = {
        let mut map = registry.conversations();
        map.drain().collect()
    };
    for (_, conversation) in conversations {
        conversation.conversation.shutdown().await;
        conversation.pump.abort();
        conversation.scope.destroy();
    }
    let runtimes: Vec<SharedRuntime> = {
        let mut map = registry
            .runtimes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.drain().map(|(_, slot)| slot.runtime).collect()
    };
    for runtime in runtimes {
        runtime.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_target_change_carries_the_bound_session_and_stays_compatible_without_it() {
        let with: ScopeTargetChange = serde_json::from_value(serde_json::json!({
            "target_id": "host-7",
            "host_label": "web-1",
            "username": "root",
            "status": "connected",
            "current": true,
            "session_id": "sess-2",
        }))
        .unwrap();
        assert_eq!(with.session_id.as_deref(), Some("sess-2"));
        assert!(with.current && !with.pinned);
        let without: ScopeTargetChange = serde_json::from_value(serde_json::json!({
            "target_id": "host-7",
            "host_label": "web-1",
            "username": "root",
            "status": "pinned",
            "session_id": null,
        }))
        .unwrap();
        assert_eq!(without.session_id, None);
    }

    #[test]
    fn download_progress_serializes() {
        let p = DownloadProgress {
            received: 1024,
            total: 4096,
            done: false,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["received"], 1024);
        assert_eq!(v["total"], 4096);
        assert_eq!(v["done"], false);
    }

    #[test]
    fn agent_event_payload_flattens_the_event() {
        let payload = AgentEventPayload {
            session_key: "ai-1".to_string(),
            event: AgentEvent::TextDelta {
                text: "hi".to_string(),
            },
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["session_key"], "ai-1");
        assert_eq!(v["kind"], "text_delta");
        assert_eq!(v["text"], "hi");
    }

    #[test]
    fn registry_hands_out_distinct_keys() {
        let registry = AgentRegistry::default();
        assert_ne!(registry.next_key(), registry.next_key());
    }

    #[test]
    fn opencode_config_only_mounts_the_zssh_mcp_server() {
        // 生产配置不能写死一个占位 MCP 名字去"禁用"用户自带的服务器：opencode 的配置目录
        // 已经被重定向到空目录（core 的 opencode.rs 里设 XDG_CONFIG_HOME），用户的 MCP
        // 根本不会加载，写死的名字只会让人误以为这里在禁用某个真实配置（R-8 第 11 节）。
        let spec = McpServerSpec::zssh(std::path::Path::new("zssh-mcp.exe"), 4567, "tok");
        let content = opencode_config_content(&spec, &serde_json::json!({}));
        let value: serde_json::Value = serde_json::from_str(&content).expect("合法 JSON");
        let mcp = value["mcp"].as_object().expect("mcp 对象");
        assert_eq!(
            mcp.keys().cloned().collect::<Vec<_>>(),
            vec!["zssh".to_string()],
            "只挂 zssh，不能写死占位名：{mcp:?}"
        );
        assert_eq!(mcp["zssh"]["enabled"], true);
    }
}
