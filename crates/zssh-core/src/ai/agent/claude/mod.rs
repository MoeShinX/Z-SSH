// Adapted from Netcatty electron/bridges/aiBridge/sdk/claudeDriver.cjs（会话恢复与
// 事件循环的思路），rewritten in Rust for Z-SSH.
//! Claude Code 驱动（卡片 AI-4 第 1、2 节）：**一个对话一个 `claude` 进程**。
//!
//! 为什么不能共用进程：`claude -p` 一个进程只对应一个会话（不像 opencode / codex
//! 能在里面开多个会话）。所以每个对话自己拉一个进程，用 stream-json 输入持续发消息。
//!
//! - **空闲 10 分钟关掉这个对话的进程**（省内存）；之后用户再发消息，用
//!   `--resume <session_id>` 重新拉起，**上下文不丢**（会话 id 从 `system/init` 拿，
//!   第一条消息就记下来，中途停止也不丢）；
//! - 能力密钥通过 `--append-system-prompt` 交给模型（每个对话自己的进程，天然隔离）；
//! - 本机能力、MCP 隔离、hooks 的参数见 [`config`] 模块注释（每一项都实测过）；
//! - 停止：往 stream-json 输入写一条 `control_request`（`subtype: "interrupt"`），
//!   实测 2.0.41 / 2.1.300 都认（回 `control_response.success`）；
//! - 进程**懒启动**：建对话先不拉（面板开了不用不占资源），第一次发消息才拉。

pub mod config;
pub mod live_catalog;
pub mod login;
pub mod settings;
pub mod translate;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use super::config::McpServerSpec;
use super::discovery;
use super::process::{
    drain_tail_lines, idle_tick, kill_process_tree, push_tail, tail_buf, tail_text, TailBuf,
};
use super::retry::{retry_limit_message, retry_limit_reached, RETRY_STOP_GRACE};
use super::{AgentConversation, AgentError, AgentEvent, AgentModel, AgentModelCatalog};
use config::{ClaudeFlags, ModelChoice, StartArgs};
use translate::{TranslateState, TurnStats};

/// 默认空闲关闭时间：这个对话 10 分钟没有活动就关掉它的进程。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// `--help` 探测（判断可选参数支不支持）的超时。
const HELP_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// 一次 `send` 的启动参数。
pub struct StartOptions {
    /// 找到的 claude 安装（真实 exe）。
    pub install: super::discovery::AgentInstall,
    /// 工作目录（`<数据目录>\agent-workspace`），MCP 配置临时文件也放这里。
    pub workspace: PathBuf,
    /// 挂给 claude 的 `zssh` MCP 服务。
    pub mcp: McpServerSpec,
    /// 本对话的能力密钥（写进 `--append-system-prompt`）。
    pub capability: String,
    /// 事件出口（驱动事件发给面板）。
    pub events: mpsc::UnboundedSender<AgentEvent>,
    /// 额外环境变量（给子进程；联网测试用它指向独立的 `CLAUDE_CONFIG_DIR` 副本，
    /// 生产路径为空——登录和模型都走用户自己的配置）。
    pub extra_env: Vec<(String, String)>,
    /// 续接历史会话：上次保存的 Claude Code 会话 id（第一次发消息就 `--resume` 它）。
    /// 续不上（会话已经不在）时自动退成新会话，并发 [`AgentEvent::ResumeFailed`]。
    pub resume_session_id: Option<String>,
}

/// 对话内部状态。
struct Inner {
    /// 正在跑的进程（空闲关闭后是 None，下次发消息再拉）。
    child: Option<tokio::process::Child>,
    stdin: Option<tokio::process::ChildStdin>,
    reader: Option<JoinHandle<()>>,
    /// 会话 id（`system/init` / `result` 里取；关进程也不清，`--resume` 要用）。
    session_id: Option<String>,
    /// `system/init` 里的 MCP 服务清单（核对"只有 zssh"）。
    mcp_servers: Vec<(String, String)>,
    translate: TranslateState,
    turn: TurnStats,
    last_activity: Instant,
    /// 当前进程启动时用的模型选择（换模型要重启进程 + `--resume` 才生效）。
    spawned_choice: ModelChoice,
    /// 当前进程的 stderr 尾巴（进程意外退出时一起报出来）。
    stderr_tail: TailBuf,
    /// 当前进程的 stdout 尾巴（同上）。
    stdout_tail: TailBuf,
    /// 当前进程的启动方式（是全新会话还是 --resume；诊断用）。
    spawned_resume: Option<String>,
    /// 进程代数：每次拉起 +1。读进程的收尾只认自己那一代，
    /// 免得被杀掉的旧进程的 EOF 晚到时把新进程的状态清掉。
    gen: u64,
    /// 会话 id 来自历史记录、还没被 `system/init` 确认过（续接是否成功还不知道）。
    resume_unverified: bool,
    /// 续接没确认之前发出的那条消息：续不上时用新会话重发它，用户不用再打一遍。
    retry_text: Option<String>,
    /// 已经在退回新会话重发了（发消息那边写失败时不要再报错）。
    fallback_inflight: bool,
    /// 回合序号（每次发消息加一）：重试超限后的延时收尾任务靠它确认还是同一个回合，
    /// 别误伤用户马上发的下一条消息。
    turn_seq: u64,
}

/// 一个对话（一个进程）。
pub struct ClaudeConversation {
    /// 自引用（启动进程 / 事件循环要 `Weak<Self>`）。
    this: Mutex<Weak<ClaudeConversation>>,
    /// 真实可执行文件路径。
    install_path: String,
    workspace: PathBuf,
    /// `--mcp-config` 的临时文件（数据目录下；收尾删掉）。
    mcp_config_path: PathBuf,
    capability: String,
    /// 我们挂上去的 MCP 服务名（`system/init` 核对"只有它"用，见 [`mcp_violation`]）。
    mcp_name: String,
    /// `--help` 探测出来的可选参数支持（`--effort` / `--permission-prompts`）。
    flags: ClaudeFlags,
    events: mpsc::UnboundedSender<AgentEvent>,
    /// 额外环境变量（测试用；生产为空）。
    extra_env: Vec<(String, String)>,
    inner: Mutex<Inner>,
    /// 拉起 / 杀掉进程这段时间上锁：杀了旧的才能拉新的，
    /// 否则旧进程的收尾会把新进程的 stdin 一起关掉（进程会静悄悄退出）。
    process_lock: AsyncMutex<()>,
    /// 发消息（拉进程 + 写输入）和续接失败后的重发串行，避免同一条消息写两遍。
    send_lock: AsyncMutex<()>,
    busy: AtomicBool,
    idle_timeout: Duration,
    idle_task: Mutex<Option<JoinHandle<()>>>,
    /// 关掉之后拒绝再发消息。
    closed: AtomicBool,
}

/// 发消息时用的一条 stream-json 输入。
fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
    })
}

/// 中断当前回合的 stream-json 控制消息（实测格式）。
fn interrupt_message() -> Value {
    json!({
        "type": "control_request",
        "request_id": "zssh-interrupt",
        "request": { "subtype": "interrupt" },
    })
}

/// `system/init` 的 MCP 服务清单核对（R-2 0.5，和 codex 的 `mcp_status_violation` 对齐）：
/// 出现 `ours`（我们挂上去的 `zssh`）以外的服务就算违规，调用方立刻结束进程、失败关闭。
///
/// 只看"之外的名字"：`zssh` 自己的连接状态（`connected` / `failed`）不算违规——那是
/// 我们自己工具桥的问题，不该当成"混进了别人的服务"。
pub fn mcp_violation(servers: &[(String, String)], ours: &str) -> Option<String> {
    let (name, _) = servers.iter().find(|(name, _)| name != ours)?;
    Some(format!(
        "claude 里出现了 zssh 以外的 MCP 服务（{name}），已停止这个对话"
    ))
}

/// 模型目录：默认 + 别名（+ 思考强度，看这个版本支不支持 `--effort`）。
///
/// `default_label` 是"默认"实际对应的模型（来自用户的 Claude Code 配置，见
/// [`settings`]）；读不到就是 None。
pub fn model_catalog(flags: ClaudeFlags, default_label: Option<String>) -> AgentModelCatalog {
    let entry = |id: String, name: String| AgentModel {
        id,
        name,
        unavailable: false,
    };
    let mut models = vec![entry("default".into(), "默认（Claude Code 的设置）".into())];
    if flags.effort {
        for effort in config::EFFORT_LEVELS {
            models.push(entry(
                format!("default/{effort}"),
                format!("默认 · {effort}"),
            ));
        }
    }
    for (alias, display) in config::MODEL_ALIASES {
        models.push(entry(alias.to_string(), display.to_string()));
        if flags.effort {
            for effort in config::EFFORT_LEVELS {
                models.push(entry(
                    format!("{alias}/{effort}"),
                    format!("{display} · {effort}"),
                ));
            }
        }
    }
    AgentModelCatalog {
        models,
        default: Some("default".to_string()),
        default_label,
    }
}

/// 不建对话、直接给某个安装算模型目录（面板一打开就显示默认模型用）：只跑一次
/// `--help` 探测参数，不拉对话进程。
pub async fn catalog_for_install(install_path: &str) -> AgentModelCatalog {
    let configured = settings::configured_model_label_from_system();
    // 优先问 Claude Code 自己（真实的模型名、默认对应哪个、支持哪些强度）
    if let Some(catalog) = live_catalog::live_catalog(install_path, configured.as_deref()).await {
        return catalog;
    }
    let flags = detect_flags(install_path).await;
    model_catalog(flags, configured)
}

/// 探测 `--help` 里的可选参数（失败按"都不支持"）。
async fn detect_flags(install_path: &str) -> ClaudeFlags {
    let mut cmd = discovery::build_command(std::path::Path::new(install_path));
    cmd.arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    discovery::hide_console(&mut cmd);
    let Ok(child) = cmd.spawn() else {
        return ClaudeFlags::default();
    };
    let Ok(Ok(output)) = tokio::time::timeout(HELP_PROBE_TIMEOUT, child.wait_with_output()).await
    else {
        return ClaudeFlags::default();
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    config::detect_flags(&text)
}

/// MCP 配置临时文件名的前缀和后缀。
const MCP_CONFIG_PREFIX: &str = ".zssh-mcp-";
const MCP_CONFIG_SUFFIX: &str = ".json";

/// `.zssh-mcp-<PID>-<随机>.json`。
fn mcp_config_file_name(pid: u32, random: &str) -> String {
    format!("{MCP_CONFIG_PREFIX}{pid}-{random}{MCP_CONFIG_SUFFIX}")
}

/// 这个文件名是不是 MCP 配置临时文件；是的话返回写它的进程 PID（老版本的文件名
/// 没有 PID，返回 `Some(None)`）。
fn parse_mcp_config_file_name(name: &str) -> Option<Option<u32>> {
    let middle = name
        .strip_prefix(MCP_CONFIG_PREFIX)?
        .strip_suffix(MCP_CONFIG_SUFFIX)?;
    Some(
        middle
            .split_once('-')
            .and_then(|(pid, _)| pid.parse::<u32>().ok()),
    )
}

/// 启动时清理上次异常退出留下的 MCP 配置文件（K06）。
///
/// 为什么要清：文件里有工具桥的连接令牌，正常收尾会删，但 Z-SSH 崩溃或被强制结束时
/// 走不到收尾。只删"写它的进程确定已经不在"的文件：另一个还开着的 Z-SSH 实例
/// 正在用的不能删（判断不了就当它还在）；老版本没带 PID 的文件一律当残留。
/// 返回删掉了几个。
pub fn remove_stale_mcp_configs(workspace: &std::path::Path) -> usize {
    remove_stale_mcp_configs_with(workspace, super::process::process_is_alive)
}

fn remove_stale_mcp_configs_with(
    workspace: &std::path::Path,
    is_alive: impl Fn(u32) -> Option<bool>,
) -> usize {
    let Ok(entries) = std::fs::read_dir(workspace) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(owner) = name.to_str().and_then(parse_mcp_config_file_name) else {
            continue;
        };
        let stale = match owner {
            Some(pid) => pid != std::process::id() && is_alive(pid) == Some(false),
            None => true,
        };
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "清掉上次异常退出留下的 MCP 配置文件");
    }
    removed
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

impl ClaudeConversation {
    /// 建一个对话（先不拉进程；第一次发消息时再拉，省得面板开了不用也占资源）。
    pub async fn create(
        options: StartOptions,
        idle_timeout: Duration,
    ) -> Result<Arc<Self>, AgentError> {
        std::fs::create_dir_all(&options.workspace)
            .map_err(|e| AgentError::StartFailed(format!("创建工作目录失败：{e}")))?;
        // MCP 配置临时文件：放数据目录下的工作目录里（里面有工具桥令牌），用完删掉，
        // 不能留在系统临时目录。文件名带上本进程 PID：崩溃留下的残留由下次启动按
        // "主人进程已经不在"清掉（见 remove_stale_mcp_configs）。
        let mcp_config_path = options
            .workspace
            .join(mcp_config_file_name(std::process::id(), &random_hex(8)));
        std::fs::write(&mcp_config_path, config::mcp_config_json(&options.mcp))
            .map_err(|e| AgentError::StartFailed(format!("写 MCP 配置失败：{e}")))?;

        // 探测可选参数（`--effort` / `--permission-prompts`）：老版本没有这两个，
        // 传了会直接报错退出（2.0.41 实测）；探测失败按"都不支持"处理
        // （卡里要求的核心参数不依赖探测结果）。
        let flags = detect_flags(&options.install.path).await;

        let conversation = Arc::new(Self {
            this: Mutex::new(Weak::new()),
            install_path: options.install.path.clone(),
            workspace: options.workspace.clone(),
            mcp_config_path,
            capability: options.capability.clone(),
            mcp_name: options.mcp.name.clone(),
            flags,
            events: options.events,
            extra_env: options.extra_env.clone(),
            inner: Mutex::new(Inner {
                child: None,
                stdin: None,
                reader: None,
                session_id: options
                    .resume_session_id
                    .clone()
                    .filter(|id| !id.trim().is_empty()),
                mcp_servers: Vec::new(),
                translate: TranslateState::default(),
                turn: TurnStats::default(),
                last_activity: Instant::now(),
                spawned_choice: ModelChoice::default(),
                stderr_tail: tail_buf(),
                stdout_tail: tail_buf(),
                spawned_resume: None,
                gen: 0,
                resume_unverified: options
                    .resume_session_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty()),
                retry_text: None,
                fallback_inflight: false,
                turn_seq: 0,
            }),
            process_lock: AsyncMutex::new(()),
            send_lock: AsyncMutex::new(()),
            busy: AtomicBool::new(false),
            idle_timeout,
            idle_task: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        *conversation.this.lock().unwrap_or_else(|p| p.into_inner()) =
            Arc::downgrade(&conversation);

        // 空闲看护：拿 Weak，对话（Arc）全部释放后自己退出。
        let weak = Arc::downgrade(&conversation);
        let idle = tokio::spawn(async move {
            let tick = idle_tick(idle_timeout);
            loop {
                tokio::time::sleep(tick).await;
                let Some(conversation) = weak.upgrade() else {
                    return;
                };
                conversation.close_idle_process_if_needed().await;
            }
        });
        *conversation
            .idle_task
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(idle);

        Ok(conversation)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn touch(&self) {
        self.lock().last_activity = Instant::now();
    }

    /// 进程在不在跑（诊断 / 测试用）。
    pub fn is_running(&self) -> bool {
        self.lock().child.is_some()
    }

    /// 现在的会话 id（诊断 / `--resume` 用）。
    pub fn current_session_id(&self) -> Option<String> {
        self.lock().session_id.clone()
    }

    /// `system/init` 里读出来的 MCP 服务清单（核对"只有 zssh"用）。
    pub fn loaded_mcp_servers(&self) -> Vec<(String, String)> {
        self.lock().mcp_servers.clone()
    }

    /// 空闲够久就关掉进程（会话 id 留着，下次发消息 `--resume` 恢复）。
    async fn close_idle_process_if_needed(&self) {
        if self.closed.load(Ordering::SeqCst) || self.busy.load(Ordering::SeqCst) {
            return;
        }
        let should_close = {
            let inner = self.lock();
            inner.child.is_some() && inner.last_activity.elapsed() >= self.idle_timeout
        };
        if !should_close {
            return;
        }
        tracing::debug!("claude 对话空闲超时，关闭这个对话的进程（会话 id 留着，下次 --resume）");
        self.kill_process().await;
    }

    /// 杀掉进程并清掉进程相关的状态（会话 id / MCP 清单留着）。
    /// 整个过程和 `spawn_process` 串行：杀干净之前不允许拉新的，
    /// 不然这里的 `stdin.take()` 会把刚拉起的新进程的输入管道关掉。
    async fn kill_process(&self) {
        let _guard = self.process_lock.lock().await;
        // 主动杀：代数 +1，让这个进程的读任务晚到的 EOF（`on_process_gone`）认不出自己，
        // 免得"空闲关闭"被当成"进程意外退出"报给面板。
        let child = {
            let mut inner = self.lock();
            inner.gen += 1;
            inner.child.take()
        };
        if let Some(mut child) = child {
            let _ = kill_process_tree(&mut child).await;
        }
        let reader = self.lock().reader.take();
        if let Some(task) = reader {
            task.abort();
        }
        self.lock().stdin.take();
    }

    /// 拉起进程（`resume` 有值就 `--resume` 恢复上下文）。
    async fn spawn_process(
        &self,
        resume: Option<String>,
        choice: ModelChoice,
    ) -> Result<(), AgentError> {
        let _guard = self.process_lock.lock().await;
        if self.lock().child.is_some() {
            return Ok(());
        }
        let args = config::build_args(&StartArgs {
            mcp_name: "zssh".to_string(),
            mcp_config_path: self.mcp_config_path.to_string_lossy().into_owned(),
            capability_prompt: config::system_prompt(&self.capability),
            choice: choice.clone(),
            flags: self.flags,
            resume: resume.clone(),
        });
        let mut cmd = discovery::build_command(std::path::Path::new(&self.install_path));
        cmd.args(&args)
            .current_dir(&self.workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (key, value) in &self.extra_env {
            cmd.env(key, value);
        }
        discovery::hide_console(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| AgentError::StartFailed(format!("启动 claude 失败：{e}")))?;
        // 崩溃时也要带走整棵子进程树（K06）
        super::process::bind_to_app_lifetime(&child);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr_tail = tail_buf();
        let stdout_tail = tail_buf();
        if let Some(stderr) = child.stderr.take() {
            let _drain = drain_tail_lines(stderr, Arc::clone(&stderr_tail), "claude stderr", 20);
        }
        let gen = {
            let mut inner = self.lock();
            inner.gen += 1;
            inner.child = Some(child);
            inner.stdin = stdin;
            inner.spawned_choice = choice;
            inner.stderr_tail = Arc::clone(&stderr_tail);
            inner.stdout_tail = Arc::clone(&stdout_tail);
            inner.spawned_resume = resume;
            inner.gen
        };
        {
            let inner = self.lock();
            tracing::debug!(
                gen,
                resume = ?inner.spawned_resume,
                model = ?inner.spawned_choice.model,
                "拉起 claude 进程"
            );
        }
        if let Some(stdout) = stdout {
            let weak = self.this.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let task = tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Some(conversation) = weak.upgrade() else {
                        return;
                    };
                    conversation.record_stdout(gen, &line);
                    conversation.handle_line(gen, &line).await;
                }
                if let Some(conversation) = weak.upgrade() {
                    conversation.on_process_gone(gen);
                }
            });
            self.lock().reader = Some(task);
        }
        Ok(())
    }

    /// 把一行也存进 stdout 尾巴（诊断进程意外退出用）。
    fn record_stdout(&self, gen: u64, line: &str) {
        let inner = self.lock();
        if inner.gen == gen {
            push_tail(&inner.stdout_tail, line, 20);
        }
    }

    /// 处理一条 stdout 消息。老进程（代数对不上）的行直接丢。
    async fn handle_line(&self, gen: u64, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if self.lock().gen != gen {
            return;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            tracing::debug!(
                line = %line.chars().take(160).collect::<String>(),
                "claude 输出了解析不了的行"
            );
            return;
        };
        // 续接没确认时先来了 result（没有 system/init）：会话已经不在了。实测 2.1.302 对
        // 不存在的会话 id 会吐一条带这个 id 的 error result 再退出，不能把它当成功。
        if message["type"] == "result" && self.begin_resume_fallback(gen) {
            return;
        }
        if message["type"] == "system" && message["subtype"] == "init" {
            let mut inner = self.lock();
            if inner.gen == gen {
                inner.resume_unverified = false;
                inner.retry_text = None;
            }
        }
        // 会话 id / MCP 清单：第一条 system/init 就记下来（中途停止也不丢）。
        if let Some(id) = translate::session_id_of(&message) {
            self.lock().session_id = Some(id.to_string());
        }
        if message["type"] == "system" && message["subtype"] == "init" {
            let servers = translate::mcp_servers_of(&message);
            self.lock().mcp_servers = servers.clone();
            // 启动后核对（R-2 0.5，和 codex 的 `mcp_status_violation` 一致）：
            // `--strict-mcp-config` 是第一道防线，这是第二道。`system/init` 在收到第一条
            // 输入之后才吐出来，所以核对发生在第一个回合里——看到 `zssh` 以外的服务立刻
            // 结束进程、失败关闭，不放行这个回合。
            if let Some(violation) = mcp_violation(&servers, &self.mcp_name) {
                tracing::error!(?servers, "claude 里出现了不该加载的 MCP 服务，失败关闭");
                self.kill_process().await;
                self.busy.store(false, Ordering::SeqCst);
                let _ = self.events.send(AgentEvent::Error { message: violation });
                let _ = self.events.send(AgentEvent::TurnEnd);
                return;
            }
        }
        // 意外的权限请求：我们没有可交互的宿主（`--permission-prompts none` 只在
        // 新版本有），失败关闭：杀掉进程并报错，绝不让回合挂在等回答上。
        if message["type"] == "control_request" {
            let subtype = message["request"]["subtype"].as_str().unwrap_or("?");
            let text = format!("claude 请求了额外权限（{subtype}），已停止这次对话");
            tracing::error!(%subtype, "claude 发来权限请求，失败关闭");
            self.kill_process().await;
            self.busy.store(false, Ordering::SeqCst);
            let _ = self.events.send(AgentEvent::Error { message: text });
            let _ = self.events.send(AgentEvent::TurnEnd);
            return;
        }

        let (events, retry_stop_seq) = {
            let mut inner = self.lock();
            // 重试超限的兜底收尾可能刚把这一代进程作废了：它后面吐的行不再认。
            if inner.gen != gen {
                return;
            }
            inner.last_activity = Instant::now();
            let seq = inner.turn_seq;
            let busy = self.busy.load(Ordering::SeqCst);
            let Inner {
                translate, turn, ..
            } = &mut *inner;
            let mut events = Vec::new();
            let mut stop = false;
            for event in translate::translate(&message, translate, turn) {
                if let AgentEvent::Retrying {
                    attempt, reason, ..
                } = &event
                {
                    // 回合不在跑 / 已被停止（用户点停止，或已经因超限停掉）时晚到的重试
                    // 状态：忽略。
                    if !busy || turn.aborted {
                        continue;
                    }
                    if retry_limit_reached(*attempt) {
                        // 第 3 次重试也失败了：这条不给界面看，直接停掉这一轮。
                        turn.aborted = true;
                        turn.retry_stop = Some(retry_limit_message(reason));
                        stop = true;
                        continue;
                    }
                }
                events.push(event);
            }
            (events, stop.then_some(seq))
        };
        for event in events {
            let _ = self.events.send(event);
        }
        if message["type"] == "result" {
            self.busy.store(false, Ordering::SeqCst);
            self.touch();
        }
        if let Some(seq) = retry_stop_seq {
            self.stop_after_retry_limit(seq);
        }
    }

    /// 重试超限：走和用户点「停止」同一条路径（[`Self::stop_inner`]，写 interrupt 控制消息），
    /// 并保证这一回合一定收尾。
    ///
    /// 正常情况下 Claude Code 收到中断后会回一条 `result`，由 [`translate::translate`]
    /// 先发错误再发 TurnEnd；万一它没回（中断被吞、进程卡住），宽限期过后由
    /// [`Self::retry_stop_fallback`] 收尾。中断请求单独发，不占宽限期。
    fn stop_after_retry_limit(&self, seq: u64) {
        let weak = self.this.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let for_stop = weak.clone();
        tokio::spawn(async move {
            if let Some(conversation) = for_stop.upgrade() {
                // 这个回合已经收尾、用户开了新回合：别把中断打到新回合上。
                if conversation.lock().turn_seq != seq {
                    return;
                }
                if let Err(err) = conversation.stop_inner().await {
                    tracing::warn!(error = %err, "重试超限后中断回合失败");
                }
            }
        });
        tokio::spawn(async move {
            tokio::time::sleep(RETRY_STOP_GRACE).await;
            if let Some(conversation) = weak.upgrade() {
                conversation.retry_stop_fallback(seq).await;
            }
        });
    }

    /// 重试超限后的兜底收尾：宽限期过了还是同一个回合、而且 `result` 还没来，就由我们
    /// 发 `Error` + `TurnEnd`。
    ///
    /// 这时进程没有听中断，为了不让它晚到的 `result` 误伤用户之后发的新回合，先把这一代
    /// 进程作废并杀掉（会话 id 留着，下次发消息 `--resume` 接上）。
    async fn retry_stop_fallback(&self, seq: u64) {
        let message = {
            let mut inner = self.lock();
            if inner.turn_seq != seq || !self.busy.load(Ordering::SeqCst) {
                return;
            }
            // `result` 已经处理过会把它取走；还在就是没收尾。
            let Some(message) = inner.turn.retry_stop.take() else {
                return;
            };
            // 代数 +1：这个进程之后吐的行（包括晚到的 result）和 EOF 都不认。
            inner.gen += 1;
            message
        };
        self.kill_process().await;
        if self.busy.swap(false, Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst) {
            let _ = self.events.send(AgentEvent::Error { message });
            let _ = self.events.send(AgentEvent::TurnEnd);
        }
    }

    /// 进程没了（stdout 读完）：清状态；正在跑的回合按出错收尾。
    /// 只认自己那一代：旧进程被杀之后 EOF 晚到，不许动新进程的状态。
    fn on_process_gone(&self, gen: u64) {
        // 带着历史会话 id 拉起、还没确认就退出了：按续接失败处理（退回新会话重发）。
        if self.begin_resume_fallback(gen) {
            return;
        }
        let (was_busy, tail, retry_stop) = {
            let mut inner = self.lock();
            if inner.gen != gen {
                return;
            }
            inner.child = None;
            inner.stdin = None;
            let tail = tail_text(&inner.stdout_tail, &inner.stderr_tail);
            // 重试超限停掉之后进程退了：报我们那一条，不再冒出第二条「进程退出了」。
            let retry_stop = inner.turn.retry_stop.take();
            (self.busy.swap(false, Ordering::SeqCst), tail, retry_stop)
        };
        if was_busy && !self.closed.load(Ordering::SeqCst) {
            tracing::warn!(gen, detail = %tail, "claude 进程退出了");
            let message = retry_stop
                .unwrap_or_else(|| format!("claude 进程退出了，继续发消息会自动恢复上下文{tail}"));
            let _ = self.events.send(AgentEvent::Error { message });
            let _ = self.events.send(AgentEvent::TurnEnd);
        }
    }

    /// 发一条消息：需要的话先拉进程（有会话 id 就 `--resume`）。
    async fn send_inner(&self, text: &str, model: Option<&str>) -> Result<(), AgentError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(AgentError::Transport("这个对话已经结束".to_string()));
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(AgentError::Busy);
        }
        let _sending = self.send_lock.lock().await;
        self.touch();
        {
            let mut inner = self.lock();
            inner.turn = TurnStats::default();
            inner.translate = TranslateState::default();
            inner.turn_seq += 1;
        }

        let choice = config::parse_model_choice(model);
        // 进程不在（第一次 / 空闲关过 / 异常退出）：拉起来；有会话 id 就恢复上下文。
        // 换了模型也重启（`--resume` 保住上下文，模型/强度是启动参数）。
        let needs_restart = {
            let inner = self.lock();
            inner.child.is_some() && inner.spawned_choice != choice
        };
        if needs_restart {
            self.kill_process().await;
        }
        if !self.is_running() {
            let resume = self.current_session_id();
            {
                // 续接还没确认：记下这条消息，续不上时用新会话重发。
                let mut inner = self.lock();
                if resume.is_some() && inner.resume_unverified {
                    inner.retry_text = Some(text.to_string());
                }
            }
            if let Err(err) = self.spawn_process(resume, choice).await {
                self.busy.store(false, Ordering::SeqCst);
                return Err(err);
            }
        }
        // 注意：**不能等 `system/init` 再发消息**。实测（2.1.300）这个 init 是收到
        // 第一条输入之后才吐出来的，等它就成了死锁（进程活着、一直不吭声）。
        if let Err(err) = self.write_user_message(text).await {
            // 续接的进程已经因为"会话不在"退出了：读进程那边会接手重发，这里不报错。
            let fallback = {
                let inner = self.lock();
                inner.retry_text.is_some() || inner.fallback_inflight
            };
            if fallback {
                return Ok(());
            }
            self.busy.store(false, Ordering::SeqCst);
            self.kill_process().await;
            return Err(err);
        }
        Ok(())
    }

    /// 往当前进程的 stdin 写一条用户消息。
    async fn write_user_message(&self, text: &str) -> Result<(), AgentError> {
        let message = user_message(text);
        // 把 stdin 从锁里拿出来再写（std 的 MutexGuard 不能跨 await）。
        let mut stdin = self.lock().stdin.take();
        let write = match stdin.as_mut() {
            Some(handle) => {
                let mut line = message.to_string();
                line.push('\n');
                match handle.write_all(line.as_bytes()).await {
                    Ok(()) => handle
                        .flush()
                        .await
                        .map_err(|e| AgentError::Transport(format!("写入失败：{e}"))),
                    Err(e) => Err(AgentError::Transport(format!("写入失败：{e}"))),
                }
            }
            None => Err(AgentError::Transport("claude 的输入管道不可用".to_string())),
        };
        self.lock().stdin = stdin;
        write
    }

    /// 续接失败的接手（`gen` 那一代进程带着历史会话 id 拉起、还没确认就失败了）。
    ///
    /// 返回 true = 已接手：清掉旧会话 id、发 [`AgentEvent::ResumeFailed`]（界面提示"之前的
    /// 上下文没有带给 AI"）、在后台杀掉旧进程并用新会话重发那条消息。不是这种情况返回 false。
    /// 不把历史文本拼进提示词：续不上就是新会话。
    fn begin_resume_fallback(&self, gen: u64) -> bool {
        let (old_child, text, choice) = {
            let mut inner = self.lock();
            if inner.gen != gen || !inner.resume_unverified || inner.spawned_resume.is_none() {
                return false;
            }
            inner.resume_unverified = false;
            inner.session_id = None;
            inner.spawned_resume = None;
            // 代数 +1：这个进程后面再吐的行和 EOF 都不认。
            inner.gen += 1;
            inner.stdin = None;
            let text = inner.retry_text.take();
            inner.fallback_inflight = text.is_some();
            (inner.child.take(), text, inner.spawned_choice.clone())
        };
        tracing::info!("claude 续接历史会话失败（会话已经不在），改用新会话");
        let _ = self.events.send(AgentEvent::ResumeFailed);
        let weak = self.this.lock().unwrap_or_else(|p| p.into_inner()).clone();
        tokio::spawn(async move {
            if let Some(mut child) = old_child {
                let _ = kill_process_tree(&mut child).await;
            }
            let Some(conversation) = weak.upgrade() else {
                return;
            };
            match text {
                Some(text) => conversation.retry_fresh(&text, choice).await,
                None => {
                    conversation.busy.store(false, Ordering::SeqCst);
                    let _ = conversation.events.send(AgentEvent::TurnEnd);
                }
            }
        });
        true
    }

    /// 用新会话重发续接失败时的那条消息。
    async fn retry_fresh(&self, text: &str, choice: ModelChoice) {
        let _sending = self.send_lock.lock().await;
        let result = if self.closed.load(Ordering::SeqCst) {
            Err(AgentError::Transport("这个对话已经结束".to_string()))
        } else {
            match self.spawn_process(None, choice).await {
                Ok(()) => self.write_user_message(text).await,
                Err(err) => Err(err),
            }
        };
        self.lock().fallback_inflight = false;
        if let Err(err) = result {
            self.busy.store(false, Ordering::SeqCst);
            self.kill_process().await;
            let _ = self.events.send(AgentEvent::Error {
                message: err.to_string(),
            });
            let _ = self.events.send(AgentEvent::TurnEnd);
        }
    }

    /// 停止当前回合：写中断控制消息（终端里的 Ctrl+C 是 Tauri 层的事）。
    async fn stop_inner(&self) -> Result<(), AgentError> {
        if !self.busy.load(Ordering::SeqCst) {
            return Ok(());
        }
        {
            let mut inner = self.lock();
            inner.turn.aborted = true;
        }
        let mut stdin = self.lock().stdin.take();
        let result = match stdin.as_mut() {
            Some(handle) => {
                let mut line = interrupt_message().to_string();
                line.push('\n');
                match handle.write_all(line.as_bytes()).await {
                    Ok(()) => handle
                        .flush()
                        .await
                        .map_err(|e| AgentError::Transport(format!("写入中断失败：{e}"))),
                    Err(e) => Err(AgentError::Transport(format!("写入中断失败：{e}"))),
                }
            }
            None => Ok(()),
        };
        self.lock().stdin = stdin;
        result
    }

    /// 收尾：杀掉进程、删掉 MCP 配置临时文件、停掉看护任务。
    async fn shutdown_inner(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.kill_process().await;
        let _ = std::fs::remove_file(&self.mcp_config_path);
        let idle = self
            .idle_task
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(task) = idle {
            task.abort();
        }
    }
}

impl AgentConversation for ClaudeConversation {
    fn session_id(&self) -> &str {
        // 会话 id 是异步来的（system/init）：这里给个稳定的占位，诊断用
        // `current_session_id`。
        "claude"
    }

    fn resume_id(&self) -> Option<String> {
        self.current_session_id()
    }

    fn models<'a>(&'a self) -> crate::ssh::BoxFuture<'a, Result<AgentModelCatalog, AgentError>> {
        Box::pin(async move {
            let configured = settings::configured_model_label_from_system();
            // 面板打开时已经探测过的话这里直接用缓存，不会再起一个进程
            if let Some(catalog) =
                live_catalog::live_catalog(&self.install_path, configured.as_deref()).await
            {
                return Ok(catalog);
            }
            Ok(model_catalog(self.flags, configured))
        })
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
        Box::pin(async move { self.shutdown_inner().await })
    }
}

impl Drop for ClaudeConversation {
    fn drop(&mut self) {
        // 兜底：没走 shutdown 就丢了句柄，也要把进程和临时文件收掉。
        let child = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.child.take());
        if let Some(child) = child {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        let _ = std::fs::remove_file(&self.mcp_config_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K06：崩溃留下的含令牌 MCP 配置，下次启动只删主人进程已不在的。
    #[test]
    fn stale_mcp_configs_are_removed_only_when_their_owner_is_gone() {
        let dir = crate::test_util::TestDir::new("zssh claude 残留配置");
        let write = |name: &str| std::fs::write(dir.join(name), "{}").unwrap();
        let gone = mcp_config_file_name(111, "aa");
        let alive = mcp_config_file_name(222, "bb");
        let unknown = mcp_config_file_name(333, "cc");
        let mine = mcp_config_file_name(std::process::id(), "dd");
        let legacy = ".zssh-mcp-0123abcd.json";
        let unrelated = "notes.json";
        for name in [&gone, &alive, &unknown, &mine, legacy, unrelated] {
            write(name);
        }

        let removed = remove_stale_mcp_configs_with(&dir, |pid| match pid {
            111 => Some(false),
            222 => Some(true),
            _ => None,
        });
        assert_eq!(removed, 2);
        assert!(!dir.join(&gone).exists(), "主人进程已不在：删");
        assert!(!dir.join(legacy).exists(), "老版本没带 PID 的：删");
        assert!(dir.join(&alive).exists(), "另一个实例还在用：留");
        assert!(dir.join(&unknown).exists(), "判断不了：留");
        assert!(dir.join(&mine).exists(), "本进程自己的：留");
        assert!(dir.join(unrelated).exists(), "别的文件不碰");
    }

    #[test]
    fn missing_workspace_is_not_an_error() {
        let dir = crate::test_util::TestDir::new("zssh claude 无工作目录");
        assert_eq!(remove_stale_mcp_configs(&dir.join("没有这个目录")), 0);
    }

    #[test]
    fn catalog_has_default_first_and_alias_entries() {
        let catalog = model_catalog(ClaudeFlags::default(), None);
        assert_eq!(catalog.default.as_deref(), Some("default"));
        assert_eq!(catalog.models[0].id, "default");
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        // 老版本不支持 --effort：只有默认 + 三个别名。
        assert_eq!(ids, vec!["default", "sonnet", "opus", "haiku"]);
        // 不列 `fable`（开发时实测跑不通）。
        assert!(!ids.contains(&"fable"));
    }

    #[test]
    fn catalog_expands_effort_levels_when_supported() {
        let catalog = model_catalog(
            ClaudeFlags {
                effort: true,
                permission_prompts: true,
            },
            Some("opus · high".to_string()),
        );
        assert_eq!(catalog.default_label.as_deref(), Some("opus · high"));
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"default/high"));
        assert!(ids.contains(&"sonnet/xhigh"));
        assert!(ids.contains(&"haiku/max"));
        assert_eq!(ids.len(), 1 + 5 + 3 * 6);
    }

    /// 空闲关闭（主动杀）之后，旧进程读任务晚到的 EOF 不能报成"意外退出"，
    /// 也不能碰当前进程的状态。实测踩过这个坑：回合被误判失败、面板上冒出
    /// 一条假的"进程退出了"。
    #[tokio::test]
    async fn late_eof_from_killed_process_is_ignored() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let workspace_guard = crate::test_util::TestDir::new("claude-unit");
        let workspace = workspace_guard.to_path_buf();
        let conversation = ClaudeConversation::create(
            StartOptions {
                install: discovery::AgentInstall {
                    path: "definitely-not-a-real-claude".to_string(),
                    version: String::new(),
                    source: discovery::DiscoverySource::Path,
                    needs_shell: false,
                    auto: true,
                    too_old: false,
                    skipped_older: None,
                },
                workspace: workspace.clone(),
                mcp: McpServerSpec::zssh(std::path::Path::new("zssh-mcp"), 1, "token"),
                capability: "capability".to_string(),
                events: tx,
                resume_session_id: None,
                extra_env: Vec::new(),
            },
            // 空闲阈值给大值：别让看护插手。
            Duration::from_secs(600),
        )
        .await
        .expect("建对话");

        // 伪造"当前进程是第 3 代、正在跑回合"（主动杀进程时代数会 +1）。
        conversation.lock().gen = 3;
        conversation.busy.store(true, Ordering::SeqCst);

        // 上一代（2）的 EOF 晚到：什么都不该发生。
        conversation.on_process_gone(2);
        assert!(rx.try_recv().is_err(), "旧进程的 EOF 不该报错");
        assert!(conversation.busy.load(Ordering::SeqCst), "回合还得接着跑");

        // 当代（3）的 EOF：按意外退出收尾（Error + TurnEnd）。
        conversation.on_process_gone(3);
        match rx.try_recv() {
            Ok(AgentEvent::Error { message }) => assert!(message.contains("进程退出")),
            other => panic!("要报错才对：{other:?}"),
        }
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::TurnEnd)));
        assert!(!conversation.busy.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    /// 建一个不会真拉进程的对话（安装路径是假的：真要拉进程会启动失败）。
    async fn fake_conversation(
        resume: Option<&str>,
    ) -> (
        Arc<ClaudeConversation>,
        mpsc::UnboundedReceiver<AgentEvent>,
        crate::test_util::TestDir,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let workspace = crate::test_util::TestDir::new("claude-resume");
        let conversation = ClaudeConversation::create(
            StartOptions {
                install: discovery::AgentInstall {
                    path: "definitely-not-a-real-claude".to_string(),
                    version: String::new(),
                    source: discovery::DiscoverySource::Path,
                    needs_shell: false,
                    auto: true,
                    too_old: false,
                    skipped_older: None,
                },
                workspace: workspace.to_path_buf(),
                mcp: McpServerSpec::zssh(std::path::Path::new("zssh-mcp"), 1, "token"),
                capability: "capability".to_string(),
                events: tx,
                resume_session_id: resume.map(str::to_string),
                extra_env: Vec::new(),
            },
            Duration::from_secs(600),
        )
        .await
        .expect("建对话");
        (conversation, rx, workspace)
    }

    /// 历史会话 id 交进来就是续接用的标识；没交就是 None（第一次发消息后才知道）。
    #[tokio::test]
    async fn resume_id_comes_from_history_until_replaced() {
        let (fresh, _rx, _ws) = fake_conversation(None).await;
        assert_eq!(fresh.resume_id(), None);
        let (resumed, _rx2, _ws2) = fake_conversation(Some("sess-old")).await;
        assert_eq!(resumed.resume_id().as_deref(), Some("sess-old"));
        assert!(resumed.lock().resume_unverified);
    }

    /// 续接失败（实测：会话不在时先吐一条带旧 id 的 error result 再退出）：不能把那条
    /// result 当成功、不能报成普通错误；要清掉旧 id、发 ResumeFailed，再用新会话重发。
    #[tokio::test]
    async fn resume_failure_falls_back_to_a_new_session() {
        let (conversation, mut rx, _ws) = fake_conversation(Some("sess-old")).await;
        {
            let mut inner = conversation.lock();
            inner.gen = 3;
            inner.spawned_resume = Some("sess-old".to_string());
            inner.retry_text = Some("看看磁盘".to_string());
        }
        conversation.busy.store(true, Ordering::SeqCst);
        let failed = json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "num_turns": 0, "session_id": "sess-old"
        });
        conversation.handle_line(3, &failed.to_string()).await;
        assert!(matches!(rx.recv().await, Some(AgentEvent::ResumeFailed)));
        assert_eq!(conversation.current_session_id(), None, "旧 id 不能留着");
        // 重发要拉新进程：测试里的假安装拉不起来，按普通失败收尾（Error + TurnEnd）。
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Some(AgentEvent::Error { message })) => assert!(message.contains("启动")),
            other => panic!("重发失败要报错：{other:?}"),
        }
        assert!(matches!(rx.recv().await, Some(AgentEvent::TurnEnd)));
        assert!(!conversation.busy.load(Ordering::SeqCst));
        // 旧进程那一代后面的 EOF 不再起作用。
        conversation.on_process_gone(3);
        assert!(rx.try_recv().is_err());
    }

    /// 进程没吐任何东西就退出（续接没确认）：同样按续接失败处理。
    #[tokio::test]
    async fn silent_exit_while_resuming_is_a_resume_failure() {
        let (conversation, mut rx, _ws) = fake_conversation(Some("sess-old")).await;
        {
            let mut inner = conversation.lock();
            inner.gen = 2;
            inner.spawned_resume = Some("sess-old".to_string());
        }
        conversation.busy.store(true, Ordering::SeqCst);
        conversation.on_process_gone(2);
        assert!(matches!(rx.recv().await, Some(AgentEvent::ResumeFailed)));
        // 没有要重发的消息：直接收尾。
        assert!(matches!(rx.recv().await, Some(AgentEvent::TurnEnd)));
        assert!(!conversation.busy.load(Ordering::SeqCst));
    }

    /// `system/init` 来了就是续上了：之后进程退出按普通"进程退出了"处理，不再走续接回退。
    #[tokio::test]
    async fn init_confirms_the_resume() {
        let (conversation, mut rx, _ws) = fake_conversation(Some("sess-old")).await;
        {
            let mut inner = conversation.lock();
            inner.gen = 4;
            inner.spawned_resume = Some("sess-old".to_string());
            inner.retry_text = Some("hi".to_string());
        }
        conversation.busy.store(true, Ordering::SeqCst);
        let init = json!({
            "type": "system", "subtype": "init", "session_id": "sess-old",
            "mcp_servers": [{"name": "zssh", "status": "connected"}]
        });
        conversation.handle_line(4, &init.to_string()).await;
        assert!(!conversation.lock().resume_unverified);
        assert_eq!(conversation.lock().retry_text, None);
        assert_eq!(
            conversation.current_session_id().as_deref(),
            Some("sess-old")
        );
        conversation.on_process_gone(4);
        match rx.try_recv() {
            Ok(AgentEvent::Error { message }) => assert!(message.contains("进程退出")),
            other => panic!("要按普通退出报错：{other:?}"),
        }
    }

    // ---- AI-11：重试提示与 3 次上限 ----

    fn api_retry_line(attempt: u64) -> String {
        json!({
            "type": "system", "subtype": "api_retry", "attempt": attempt, "max_retries": 10,
            "retry_delay_ms": 2000, "error_status": 529, "error": "overloaded"
        })
        .to_string()
    }

    fn interrupted_result_line() -> String {
        json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "result": "Request was aborted"
        })
        .to_string()
    }

    /// 取走已经到的全部事件。
    fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut got = Vec::new();
        while let Ok(event) = rx.try_recv() {
            got.push(event);
        }
        got
    }

    async fn running_conversation() -> (
        Arc<ClaudeConversation>,
        mpsc::UnboundedReceiver<AgentEvent>,
        crate::test_util::TestDir,
    ) {
        let (conversation, rx, ws) = fake_conversation(None).await;
        {
            let mut inner = conversation.lock();
            inner.gen = 5;
            inner.turn_seq = 1;
        }
        conversation.busy.store(true, Ordering::SeqCst);
        (conversation, rx, ws)
    }

    #[tokio::test]
    async fn first_three_retries_reach_the_ui_and_do_not_stop_the_turn() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        for attempt in 1..=3 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        let got = drain(&mut rx);
        assert_eq!(got.len(), 3);
        for (i, event) in got.iter().enumerate() {
            match event {
                AgentEvent::Retrying {
                    attempt,
                    reason,
                    next_in_secs,
                } => {
                    assert_eq!(*attempt as usize, i + 1);
                    assert_eq!(reason, "服务商那边暂时忙不过来");
                    assert_eq!(*next_in_secs, Some(2));
                }
                other => panic!("该是 Retrying，得到 {other:?}"),
            }
        }
        assert!(
            conversation.busy.load(Ordering::SeqCst),
            "没超限，回合接着跑"
        );
        assert!(!conversation.lock().turn.aborted);
        // 等一个宽限期：不该有任何收尾。
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(300)).await;
        assert!(drain(&mut rx).is_empty());
    }

    /// 第 4 次：不给界面、走停止路径；Claude Code 回了中断 result：先 Error 后 TurnEnd，
    /// 各一次；兜底不重复。
    #[tokio::test]
    async fn fourth_retry_stops_then_result_ends_turn_with_one_error() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        for attempt in 1..=4 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        let got = drain(&mut rx);
        assert_eq!(got.len(), 3, "只有前 3 次给界面：{got:?}");
        assert!(got.iter().all(|e| matches!(e, AgentEvent::Retrying { .. })));
        {
            let inner = conversation.lock();
            assert!(inner.turn.aborted, "走的是停止路径的标记");
            assert!(inner.turn.retry_stop.is_some());
        }
        // 超限之后还有更多重试状态：忽略。
        conversation.handle_line(5, &api_retry_line(5)).await;
        assert!(drain(&mut rx).is_empty());

        // 中断后的 result 到了。
        conversation
            .handle_line(5, &interrupted_result_line())
            .await;
        let got = drain(&mut rx);
        match got.as_slice() {
            [AgentEvent::Error { message }, AgentEvent::TurnEnd] => {
                assert!(message.starts_with("重试了 3 次仍然失败："), "{message}");
                assert!(message.contains("服务商那边暂时忙不过来"));
                assert!(message.ends_with("已停止这一轮，可以稍后再试或换一个模型。"));
            }
            other => panic!("要先 Error 后 TurnEnd：{other:?}"),
        }
        assert!(!conversation.busy.load(Ordering::SeqCst));
        // 兜底到点：已经收尾了，什么都不再发。
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(600)).await;
        assert!(drain(&mut rx).is_empty(), "不能重复报错或重复收尾");
    }

    /// 中断被吞、result 一直不来：宽限期后由我们发 Error + TurnEnd，并作废旧进程。
    #[tokio::test]
    async fn fourth_retry_falls_back_to_our_own_error_and_turn_end() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        for attempt in 1..=4 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        drain(&mut rx);
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(1500)).await;
        let got = drain(&mut rx);
        match got.as_slice() {
            [AgentEvent::Error { message }, AgentEvent::TurnEnd] => {
                assert!(message.starts_with("重试了 3 次仍然失败："), "{message}");
            }
            other => panic!("要兜底 Error + TurnEnd：{other:?}"),
        }
        assert!(!conversation.busy.load(Ordering::SeqCst));
        // 旧进程晚到的 result / EOF 不再起作用（代数已作废）：不会重复收尾。
        conversation
            .handle_line(5, &interrupted_result_line())
            .await;
        conversation.on_process_gone(5);
        assert!(drain(&mut rx).is_empty());
    }

    /// 兜底不误伤新回合：宽限期内用户已经开了下一个回合，兜底什么都不做。
    #[tokio::test]
    async fn fallback_does_not_touch_a_newer_turn() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        for attempt in 1..=4 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        drain(&mut rx);
        // result 来了、回合收尾，用户马上发了下一条（回合序号 +1、状态重置）。
        conversation
            .handle_line(5, &interrupted_result_line())
            .await;
        assert_eq!(drain(&mut rx).len(), 2);
        {
            let mut inner = conversation.lock();
            inner.turn = TurnStats::default();
            inner.turn_seq += 1;
        }
        conversation.busy.store(true, Ordering::SeqCst);
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(600)).await;
        assert!(drain(&mut rx).is_empty(), "新回合不能被旧的兜底收尾");
        assert!(conversation.busy.load(Ordering::SeqCst));
        // 新回合自己的重试也从头放行。
        conversation.handle_line(5, &api_retry_line(1)).await;
        assert!(matches!(
            drain(&mut rx).as_slice(),
            [AgentEvent::Retrying { attempt: 1, .. }]
        ));
    }

    /// 回合不在跑 / 用户已经点了停止：晚到的重试状态忽略，不报错、不收尾。
    #[tokio::test]
    async fn late_retry_after_user_stop_or_turn_end_is_ignored() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        conversation.lock().turn.aborted = true; // 用户点了停止
        for attempt in 1..=5 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        assert!(drain(&mut rx).is_empty());
        assert!(conversation.lock().turn.retry_stop.is_none());

        conversation.lock().turn = TurnStats::default();
        conversation.busy.store(false, Ordering::SeqCst); // 回合已结束
        for attempt in 1..=5 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        assert!(drain(&mut rx).is_empty());
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(300)).await;
        assert!(drain(&mut rx).is_empty());
    }

    /// 重试超限停掉之后进程直接退出：报我们那一条，不再冒出「进程退出了」。
    #[tokio::test]
    async fn process_exit_after_retry_stop_reports_only_our_error() {
        let (conversation, mut rx, _ws) = running_conversation().await;
        for attempt in 1..=4 {
            conversation.handle_line(5, &api_retry_line(attempt)).await;
        }
        drain(&mut rx);
        conversation.on_process_gone(5);
        match drain(&mut rx).as_slice() {
            [AgentEvent::Error { message }, AgentEvent::TurnEnd] => {
                assert!(message.starts_with("重试了 3 次仍然失败："), "{message}");
            }
            other => panic!("{other:?}"),
        }
        tokio::time::sleep(RETRY_STOP_GRACE + Duration::from_millis(600)).await;
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn stream_json_input_shapes() {
        let user = user_message("你好");
        assert_eq!(user["type"], "user");
        assert_eq!(user["message"]["role"], "user");
        assert_eq!(user["message"]["content"][0]["type"], "text");
        assert_eq!(user["message"]["content"][0]["text"], "你好");

        let interrupt = interrupt_message();
        assert_eq!(interrupt["type"], "control_request");
        assert_eq!(interrupt["request"]["subtype"], "interrupt");
        assert!(interrupt["request_id"].is_string());
    }

    /// 启动后核对（R-2 0.5）：`system/init` 里出现别的 MCP 服务就违规。
    #[test]
    fn mcp_check_allows_only_our_server() {
        let ours = vec![("zssh".to_string(), "connected".to_string())];
        assert_eq!(mcp_violation(&ours, "zssh"), None);
        // 只有我们自己的服务、但没连上：不是"混进了别人的服务"。
        let failed = vec![("zssh".to_string(), "failed".to_string())];
        assert_eq!(mcp_violation(&failed, "zssh"), None);
        // 一个服务都没报（老版本 / 异常输出）：不误判。
        assert_eq!(mcp_violation(&[], "zssh"), None);

        // 混进了用户自己的服务：要报出名字。
        let mixed = vec![
            ("zssh".to_string(), "connected".to_string()),
            ("my_mcp_server".to_string(), "connected".to_string()),
        ];
        let message = mcp_violation(&mixed, "zssh").expect("要违规");
        assert!(message.contains("my_mcp_server"), "{message}");

        // 只有用户的服务（我们那个没起来）同样算违规。
        let other = vec![("my_mcp_server".to_string(), "connected".to_string())];
        assert!(mcp_violation(&other, "zssh").is_some());
    }
}
