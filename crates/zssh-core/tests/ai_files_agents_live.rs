//! 三个智能体各做一次"读一个文件、改一行、写回去"（M3-4 第 1.4 节）。
//!
//! 真的调用模型，所以要同时满足：`ZSSH_LIVE_FULL=1`（走 `scripts/live-tests.ps1`）、
//! `ZSSH_AGENT_E2E=1`、本机装了对应智能体（没装或没登录就跳过并打印原因）、
//! `zssh-mcp` 已编译。服务器上只用专用临时目录（`RemoteTempDir`，任何退出路径都删）。
//!
//! 每个智能体都走完整链路：智能体 → zssh-mcp → 工具桥 → 手动模式确认（脚本化放行，
//! 并核对确认请求里带着差异）→ 备份 → 临时文件加改名。模型被要求只用 `read_file` /
//! `write_file`，不用 `run_command`。核对用测试自己的 SFTP 通道读回文件。

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::TcpListener;

use zssh_core::ai::agent::claude::{ClaudeConversation, StartOptions as ClaudeStartOptions};
use zssh_core::ai::agent::codex::{self, CodexRuntime, StartOptions as CodexStartOptions};
use zssh_core::ai::agent::config::{build_config_content, read_user_model_config, McpServerSpec};
use zssh_core::ai::agent::discovery::{self, AgentInstall, CLAUDE, CODEX};
use zssh_core::ai::agent::opencode::{
    default_config_home, AgentRuntime, StartOptions, DEFAULT_IDLE_TIMEOUT, DEFAULT_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::{AgentConversation, AgentEvent};
use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, Bridge, BridgeHost, ExecMode, ExecModeProvider,
    ScopeHandle,
};
use zssh_core::ai::files::RemoteFs;
use zssh_core::ssh::{BoxFuture, Connection, ExecCapture};
use zssh_core::Database;

use common::{serial, FileEditScenario, TempKnownHosts, TestServer};

const TARGET: &str = "agent-files-1";

struct FilesHost {
    connection: Arc<Connection>,
}

impl BridgeHost for FilesHost {
    fn exec<'a>(
        &'a self,
        _session_id: &'a str,
        _command: &'a str,
        _wait: Duration,
        _max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async { Err("这个测试只用文件工具，不该执行命令".to_owned()) })
    }

    fn read_terminal<'a>(
        &'a self,
        _session_id: &'a str,
        _max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remote_fs<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        if target_id != TARGET {
            return Box::pin(async { Err("测试宿主只认识一个目标".to_owned()) });
        }
        common::remote_fs_on(&self.connection)
    }
}

/// 手动模式 + 脚本化放行：记下每次确认请求，核对卡片上带着差异。
struct RecordingApprover {
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl Approver for RecordingApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async { ApprovalDecision::Allowed })
    }
}

struct ManualMode;

impl ExecModeProvider for ManualMode {
    fn mode(&self) -> ExecMode {
        ExecMode::Manual
    }
}

struct Rig {
    bridge: Arc<Bridge>,
    port: u16,
    scope: ScopeHandle,
    approver: Arc<RecordingApprover>,
    database: Arc<Database>,
}

async fn rig(connection: &Arc<Connection>) -> Rig {
    let database = Arc::new(Database::open_in_memory().expect("审计库"));
    let approver = Arc::new(RecordingApprover {
        requests: Mutex::new(Vec::new()),
    });
    let bridge = Arc::new(Bridge::new(
        Arc::new(FilesHost {
            connection: Arc::clone(connection),
        }),
        database.clone(),
        approver.clone(),
        Arc::new(ManualMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑端口");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));
    let scope = bridge.create_scope();
    scope.add_target(TARGET, "测试服务器", "root", "connected", true, false);
    Rig {
        bridge,
        port,
        scope,
        approver,
        database,
    }
}

/// 发一条消息，收集这一回合的文字和工具调用（工具名里要有 read_file / write_file）。
async fn run_turn<C: AgentConversation + ?Sized>(
    conversation: &C,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    text: &str,
    model: Option<&str>,
) -> String {
    conversation.send(text, model).await.expect("发送消息");
    let mut reply = String::new();
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "回合超时；已收到：{reply}");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AgentEvent::TextDelta { text })) => reply.push_str(&text),
            Ok(Some(AgentEvent::ToolStart { tool, .. })) => {
                reply.push_str(&format!("\n[tool {tool}]"));
            }
            Ok(Some(AgentEvent::ToolEnd { error, .. })) => {
                reply.push_str(&format!("\n[tool结果 error={error}]"));
            }
            Ok(Some(AgentEvent::TurnEnd)) => break,
            Ok(Some(AgentEvent::Error { message })) => {
                reply.push_str(&format!("\n[error {message}]"));
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("回合超时；已收到：{reply}"),
        }
    }
    reply
}

/// 核对：确认请求带着差异（改了一行），审计里有 write_file 且不含内容，模型用的是文件工具。
fn check_trail(rig: &Rig, reply: &str, backup: &str) {
    let asked = rig.approver.requests.lock().unwrap().clone();
    let write = asked
        .iter()
        .find(|r| r.file.is_some())
        .unwrap_or_else(|| panic!("没有 write_file 的确认请求；回复：{reply}"));
    let diff = &write.file.as_ref().unwrap().diff;
    assert_eq!((diff.added, diff.removed), (1, 1), "差异应该是改了一行");
    assert!(diff.lines.iter().any(|l| l.text == "BRAVO-EDITED"));
    assert!(
        !asked.iter().any(|r| r.file.is_none()),
        "模型不该用 run_command（会多出命令确认）：{asked:?}"
    );
    let rows = rig
        .database
        .ai_audit_list(Some(rig.scope.scope_id()), 50)
        .expect("读审计");
    assert!(
        rows.iter().any(|r| r.tool == "read_file"),
        "审计里要有 read_file"
    );
    let write_row = rows
        .iter()
        .find(|r| r.tool == "write_file" && r.decision == "allowed")
        .unwrap_or_else(|| panic!("审计里没有通过的 write_file：{rows:?}"));
    assert_eq!(write_row.backup_path.as_deref(), Some(backup));
    let dump = format!("{rows:?}");
    assert!(!dump.contains("BRAVO-EDITED"), "审计里不能有文件内容");
    println!(
        "确认请求带差异（+1 −1），审计 {} 条（含备份路径、不含内容）；回复：{}",
        rows.len(),
        reply.trim().chars().take(200).collect::<String>()
    );
}

fn e2e_enabled(name: &str) -> bool {
    if !common::full_live(name) {
        return false;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 {name}：设置 ZSSH_AGENT_E2E=1 才跑（会真的调用模型）");
        return false;
    }
    true
}

fn find_zssh_mcp() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.parent()?.to_owned();
    ["zssh-mcp.exe", "zssh-mcp"]
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.exists())
}

async fn find_install(
    env_var: &str,
    kind: &discovery::AgentSpec,
    tag: &str,
) -> Option<AgentInstall> {
    if let Ok(path) = std::env::var(env_var) {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            let resolved = discovery::resolve_launch_path(&candidate);
            if resolved.is_file() {
                return Some(AgentInstall {
                    path: resolved.display().to_string(),
                    version: "e2e".to_string(),
                    source: discovery::DiscoverySource::Setting,
                    needs_shell: false,
                    auto: false,
                    too_old: false,
                    skipped_older: None,
                });
            }
        }
    }
    let data_dir_guard = common::TestDir::new(&format!("m34-{tag}-discovery"));
    let data_dir = data_dir_guard.to_path_buf();
    match discovery::discover(kind, None, &data_dir).await {
        Ok(Some(install)) if !install.too_old => Some(install),
        _ => None,
    }
}

struct Setup {
    server: TestServer,
    mcp_exe: PathBuf,
    connection: Arc<Connection>,
    _known_hosts: TempKnownHosts,
}

async fn setup(name: &str) -> Option<Setup> {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 {name}：{reason}");
            return None;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 {name}：找不到 zssh-mcp 可执行文件（先 cargo build -p zssh-mcp）");
        return None;
    };
    let known_hosts = TempKnownHosts::new("m34-agent");
    let connection = Arc::new(
        server
            .connect(&known_hosts)
            .await
            .expect("连接测试服务器失败"),
    );
    Some(Setup {
        server,
        mcp_exe,
        connection,
        _known_hosts: known_hosts,
    })
}

#[tokio::test]
async fn opencode_reads_edits_one_line_and_writes_back() {
    let name = "opencode_reads_edits_one_line_and_writes_back";
    if !e2e_enabled(name) {
        return;
    }
    let _serial = serial().await;
    let Some(env) = setup(name).await else { return };
    let Some(opencode) = find_opencode() else {
        println!("跳过 {name}：找不到 opencode");
        return;
    };
    // 低于 1.19 的 opencode 有 SQLite 外键 bug（设计 2.5），跑了只会拿到它自己的报错。
    if !opencode_version_ok(&opencode) {
        println!(
            "跳过 {name}：这份 opencode 版本低于 1.19（有已知的 SQLite 外键 bug）；             用 ZSSH_AGENT_OPENCODE 指向 1.19 以上的 opencode.exe"
        );
        return;
    }
    let scenario = FileEditScenario::setup(&env.server, &env.connection).await;
    let rig = rig(&env.connection).await;
    let workspace_guard = common::TestDir::new("m34-opencode-workspace");
    let workspace = workspace_guard.to_path_buf();
    let spec = McpServerSpec::zssh(&env.mcp_exe, rig.port, rig.bridge.connection_token());
    let options = StartOptions {
        install: AgentInstall {
            path: opencode.display().to_string(),
            version: "e2e".to_string(),
            source: discovery::DiscoverySource::Setting,
            needs_shell: true,
            auto: false,
            too_old: false,
            skipped_older: None,
        },
        workspace: workspace.clone(),
        config_home: default_config_home(&workspace),
        config_content: build_config_content(&spec, &[], &read_user_model_config()),
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };
    let runtime = AgentRuntime::start(options, DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 opencode");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conversation = runtime
        .open_conversation(rig.scope.capability().to_string(), tx)
        .await
        .expect("开对话");
    let catalog = conversation.models().await.expect("取模型");
    let model = std::env::var("ZSSH_AGENT_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| catalog.default.clone())
        .or_else(|| catalog.models.first().map(|m| m.id.clone()));
    let reply = run_turn(
        conversation.as_ref(),
        &mut rx,
        &scenario.prompt(),
        model.as_deref(),
    )
    .await;
    println!("智能体回复（工具调用摘要）：{}", reply.trim());
    let outcome = std::panic::AssertUnwindSafe(async {
        let backup = scenario.verify().await;
        check_trail(&rig, &reply, &backup);
    });
    let result = futures_catch(outcome).await;
    conversation.shutdown().await;
    runtime.shutdown().await;
    scenario.cleanup().await;
    rig.scope.destroy();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn codex_reads_edits_one_line_and_writes_back() {
    let name = "codex_reads_edits_one_line_and_writes_back";
    if !e2e_enabled(name) {
        return;
    }
    let _serial = serial().await;
    let Some(env) = setup(name).await else { return };
    let Some(install) = find_install("ZSSH_AGENT_CODEX", &CODEX, "codex").await else {
        println!("跳过 {name}：找不到 codex");
        return;
    };
    let scenario = FileEditScenario::setup(&env.server, &env.connection).await;
    let rig = rig(&env.connection).await;
    let workspace_guard = common::TestDir::new("m34-codex-workspace");
    let workspace = workspace_guard.to_path_buf();
    let options = CodexStartOptions {
        install: install.clone(),
        workspace,
        mcp: McpServerSpec::zssh(&env.mcp_exe, rig.port, rig.bridge.connection_token()),
        probes: codex::startup::command_probes(&install.path),
        codex_home: None,
        startup_timeout: codex::DEFAULT_STARTUP_TIMEOUT,
    };
    let runtime = CodexRuntime::start(options, codex::DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 codex");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conversation = runtime
        .open_conversation(rig.scope.capability().to_string(), tx)
        .await
        .expect("开对话");
    let model = std::env::var("ZSSH_AGENT_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let reply = run_turn(
        conversation.as_ref(),
        &mut rx,
        &scenario.prompt(),
        model.as_deref(),
    )
    .await;
    println!("智能体回复（工具调用摘要）：{}", reply.trim());
    let outcome = std::panic::AssertUnwindSafe(async {
        let backup = scenario.verify().await;
        check_trail(&rig, &reply, &backup);
    });
    let result = futures_catch(outcome).await;
    conversation.shutdown().await;
    runtime.shutdown().await;
    scenario.cleanup().await;
    rig.scope.destroy();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn claude_reads_edits_one_line_and_writes_back() {
    let name = "claude_reads_edits_one_line_and_writes_back";
    if !e2e_enabled(name) {
        return;
    }
    let _serial = serial().await;
    let Some(env) = setup(name).await else { return };
    let Some(install) = find_install("ZSSH_AGENT_CLAUDE", &CLAUDE, "claude").await else {
        println!("跳过 {name}：找不到 Claude Code");
        return;
    };
    let scenario = FileEditScenario::setup(&env.server, &env.connection).await;
    let rig = rig(&env.connection).await;
    let workspace_guard = common::TestDir::new("m34-claude-workspace");
    let workspace = workspace_guard.to_path_buf();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conversation = ClaudeConversation::create(
        ClaudeStartOptions {
            install,
            workspace,
            mcp: McpServerSpec::zssh(&env.mcp_exe, rig.port, rig.bridge.connection_token()),
            capability: rig.scope.capability().to_string(),
            events: tx,
            resume_session_id: None,
            extra_env: Vec::new(),
        },
        zssh_core::ai::agent::claude::DEFAULT_IDLE_TIMEOUT,
    )
    .await
    .expect("建对话");
    let reply = run_turn(
        conversation.as_ref(),
        &mut rx,
        &scenario.prompt(),
        Some("haiku"),
    )
    .await;
    println!("智能体回复（工具调用摘要）：{}", reply.trim());
    let outcome = std::panic::AssertUnwindSafe(async {
        let backup = scenario.verify().await;
        check_trail(&rig, &reply, &backup);
    });
    let result = futures_catch(outcome).await;
    conversation.shutdown().await;
    scenario.cleanup().await;
    rig.scope.destroy();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// 断言失败也要走收尾（关智能体、删服务器临时目录）：先接住 panic，收尾完再抛。
async fn futures_catch<F: std::future::Future<Output = ()>>(
    future: std::panic::AssertUnwindSafe<F>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    use futures_util_lite::CatchUnwind;
    CatchUnwind(future).await
}

mod futures_util_lite {
    use std::future::Future;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// 极简的 `catch_unwind` future（不想为这一处测试代码加依赖）。
    pub struct CatchUnwind<F>(pub AssertUnwindSafe<F>);

    impl<F: Future<Output = ()>> Future for CatchUnwind<F> {
        type Output = Result<(), Box<dyn std::any::Any + Send>>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // SAFETY: 不移动内部 future，只在这里投影一次。
            let inner = unsafe { self.as_mut().map_unchecked_mut(|s| &mut s.0 .0) };
            match catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
                Err(panic) => Poll::Ready(Err(panic)),
            }
        }
    }
}

/// `opencode --version` 是不是 1.19 及以上（读不出来按不行处理）。
fn opencode_version_ok(path: &std::path::Path) -> bool {
    let output = std::process::Command::new(path).arg("--version").output();
    let Ok(output) = output else { return false };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.trim().split('.').map(|p| p.parse::<u32>().ok());
    matches!((parts.next().flatten(), parts.next().flatten()), (Some(major), Some(minor)) if (major, minor) >= (1, 19))
}

fn find_opencode() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ZSSH_AGENT_OPENCODE") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in ["opencode.cmd", "opencode.exe", "opencode"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[allow(dead_code)]
fn _unused(_: AtomicBool, _: Ordering) {}
