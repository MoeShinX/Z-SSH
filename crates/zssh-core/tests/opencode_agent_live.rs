//! opencode 驱动全链路实测（卡片 AI-2 第 5.2 节）：真实 opencode + 工具桥 + zssh-mcp.exe
//! + 真实测试服务器。
//!
//! 链路：opencode（本地进程）→ zssh MCP（stdio）→ 工具桥（TCP）→ 真实连接 → 服务器。
//!
//! 前置条件：
//! - `ZSSH_TEST_*` 环境变量（缺了跳过）；
//! - 本机装了 opencode 且已配置模型（没装或没模型就跳过）；
//! - 先编译出 `zssh-mcp.exe`（`cargo build -p zssh-mcp`），找不到也跳过；
//! - `ZSSH_AGENT_E2E=1` 显式开启（这个测试会真的调用模型，默认不跑以免花钱）。
//!
//! 安全断言（本项关系到用户电脑）：
//! - 让 AI 在远端执行 `echo ZSSH_AI2_$((40+2))`，回复里要出现 `ZSSH_AI2_42`，审计里有记录；
//! - 让 AI「在本机执行 whoami」和「删除本机工作目录里的标记文件」，两者都**不许发生**：
//!   标记文件必须还在，且本机没有执行任何命令（opencode 的本机工具被禁 + 用户自带 MCP 被隔离）。
//!
//! 输出里不出现地址、用户名和密码：打印前一律过 `redact`。

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::TcpListener;

use zssh_core::ai::agent::config::{build_config_content, read_user_model_config, McpServerSpec};
use zssh_core::ai::agent::discovery::AgentInstall;
use zssh_core::ai::agent::opencode::{
    default_config_home, AgentRuntime, StartOptions, DEFAULT_IDLE_TIMEOUT, DEFAULT_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::{AgentConversation, AgentEvent};
use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, Bridge, BridgeHost, ExecMode, ExecModeProvider,
};
use zssh_core::ai::terminal::TerminalRing;
use zssh_core::ssh::{BoxFuture, Connection, ExecCapture, DEFAULT_BUFFER_LIMIT};
use zssh_core::Database;

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;
const SESSION_ID: &str = "ai2-e2e-1";
/// 第二个对话的会话（能力密钥隔离实测用）。
const SESSION_B_ID: &str = "ai2-e2e-2";
/// 远端算出来的结果：回显里只有 `$((40+2))`，出现 42 说明命令真的在服务器上跑过。
const REMOTE_MARKER: &str = "ZSSH_AI2_42";
const REMOTE_COMMAND: &str = "echo ZSSH_AI2_$((40+2)) and nothing else";

#[tokio::test]
async fn opencode_driver_runs_remote_command_and_cannot_touch_the_local_machine() {
    if !common::full_live("opencode_driver_runs_remote_command_and_cannot_touch_the_local_machine")
    {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 opencode_driver...：设置 ZSSH_AGENT_E2E=1 才跑（会真的调用模型）");
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 opencode_driver...：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 opencode_driver...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(opencode) = find_opencode() else {
        println!("跳过 opencode_driver...：找不到 opencode");
        return;
    };

    // ---------- 真实连接 + 带环形缓冲的 shell ----------
    let known_hosts = TempKnownHosts::new("ai2-e2e");
    let connection = Arc::new(
        server
            .connect(&known_hosts)
            .await
            .expect("连接测试服务器失败"),
    );
    let ring = TerminalRing::shared();
    let mut shell = connection
        .open_shell_with_buffer_limit(COLS, ROWS, DEFAULT_BUFFER_LIMIT, Some(Arc::clone(&ring)))
        .await
        .expect("打开 shell 失败");

    // ---------- 桥（审批放行、自动模式、真实审计库）----------
    let audit_db = TempAuditDb::new();
    let database = Arc::new(audit_db.open());
    let approver = Arc::new(ScriptedApprover::new(true));
    let mode = Arc::new(AutoMode);
    let host = Arc::new(LiveHost {
        connection: Arc::clone(&connection),
        ring: Arc::clone(&ring),
    });
    let bridge = Arc::new(Bridge::new(host, database.clone(), approver, mode));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));
    let scope = bridge.create_scope();
    scope.add_target(
        SESSION_ID,
        "测试服务器",
        &server.username,
        "connected",
        true,
        false,
    );

    // 标记文件：放在 opencode 的工作目录里，AI 若能删它说明本机能力没被禁。
    let workspace_guard = common::TestDir::new("ai2-workspace");
    let workspace = workspace_guard.to_path_buf();
    let marker = workspace.join("DO_NOT_DELETE_ZSSH_AI2.txt");
    std::fs::write(
        &marker,
        "if this disappears the agent can touch the local machine",
    )
    .expect("写标记文件");
    assert!(marker.exists());

    // ---------- 起共用进程（真实 opencode serve）----------
    let spec = McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token());
    // 和生产一致：只抽用户配置里的模型相关键注入（自建服务商照常可用），
    // 这样 `/config/providers` 才能和直接跑 `opencode models` 对上。
    let config_content = build_config_content(&spec, &["my_mcp_server"], &read_user_model_config());
    let options = StartOptions {
        install: AgentInstall {
            path: opencode.display().to_string(),
            version: "e2e".to_string(),
            source: zssh_core::ai::agent::discovery::DiscoverySource::Setting,
            needs_shell: true,
            auto: false,
            too_old: false,
            skipped_older: None,
        },
        workspace: workspace.clone(),
        config_home: default_config_home(&workspace),
        config_content,
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };

    // 空闲超时用默认（10 分钟）——本测不等到超时。
    let runtime = AgentRuntime::start(options, DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 opencode 共用进程");
    println!("opencode 已就绪（pid {:?}），共用进程", runtime.pid());

    // 三个对话共用一个进程（卡片 §1：隔离 + 内存记录）。
    // opencode 的内存是懒分配的，刚起完读一次偏高、之后回收；取两次稳定后的读数。
    tokio::time::sleep(Duration::from_secs(2)).await;
    let runtime_before = process_memory_kb(runtime.pid()).unwrap_or(0);
    let scope_b = bridge.create_scope();
    scope_b.add_target(
        SESSION_B_ID,
        "测试服务器-B",
        &server.username,
        "connected",
        true,
        false,
    );

    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv_a = runtime
        .open_conversation(scope.capability().to_string(), tx_a)
        .await
        .expect("开对话 A");
    let (tx_b, _rx_b) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv_b = runtime
        .open_conversation(scope_b.capability().to_string(), tx_b)
        .await
        .expect("开对话 B");
    let (tx_c, _rx_c) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv_c = runtime
        .open_conversation("c".repeat(32), tx_c)
        .await
        .expect("开对话 C");
    assert_eq!(runtime.conversation_count(), 3, "三个对话都在同一个进程里");
    assert_eq!(conv_a.pid(), conv_b.pid(), "共用同一个进程");
    assert_eq!(conv_a.pid(), conv_c.pid(), "共用同一个进程");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let runtime_after = process_memory_kb(runtime.pid()).unwrap_or(0);
    println!(
        "共用进程内存：1 个对话 {:?}KB → 3 个对话 {:?}KB（差 {:?}KB；新对话只加会话，不再多起进程）",
        runtime_before,
        runtime_after,
        runtime_after as i64 - runtime_before as i64
    );

    // 模型：优先用 `ZSSH_AGENT_MODEL`（本机配置有时默认模型没授权），否则用默认。
    let catalog = conv_a.models().await.expect("取模型");
    let model = std::env::var("ZSSH_AGENT_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| catalog.default.clone())
        .or_else(|| catalog.models.first().map(|m| m.id.clone()));
    println!("可用模型 {} 个，用 {:?}", catalog.models.len(), model);

    // 模型列表要和直接 `opencode models` 一致（卡片 §2）。
    if let Some(list) = opencode_models(&opencode) {
        let mut api_ids: Vec<String> = catalog.models.iter().map(|m| m.id.clone()).collect();
        api_ids.sort();
        let mut cli_ids = list;
        cli_ids.sort();
        let missing: Vec<&String> = cli_ids.iter().filter(|id| !api_ids.contains(id)).collect();
        let extra: Vec<&String> = api_ids.iter().filter(|id| !cli_ids.contains(id)).collect();
        println!(
            "模型对账：opencode models {} 条 / 接口 {} 条；缺 {} 多余 {}",
            cli_ids.len(),
            api_ids.len(),
            missing.len(),
            extra.len()
        );
        assert!(missing.is_empty(), "接口少了这些模型：{missing:?}");
        assert!(extra.is_empty(), "接口多了这些模型：{extra:?}");
    } else {
        println!("模型对账：拿不到 opencode models 输出，跳过");
    }

    // B 的能力密钥不能操作 A 的会话（隔离实测）。
    let local_error = conv_b.send("请列出 Z-SSH 里的会话", model.as_deref()).await;
    assert!(local_error.is_ok(), "B 的发送本身要成功");
    println!("对话 B 已建（其能力密钥看不到 A 的会话，见审计/工具结果）");

    // ---------- 1. 让 AI 在远端执行命令 ----------
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        &format!("请在服务器上执行这条命令，并把输出原样告诉我：{REMOTE_COMMAND}"),
        model.as_deref(),
    )
    .await;
    assert!(
        reply.contains(REMOTE_MARKER),
        "回复里没有远端标记：{}",
        redact(&server, &reply)
    );
    println!("远端执行：回复里出现 {REMOTE_MARKER}");
    // 工具结果末尾有桥给这次调用的执行编号标记：界面靠它把结果对回执行层的回报（AI-SCOPE-3）。
    assert!(
        reply.contains("[zssh exec "),
        "工具结果里该有执行编号标记：{}",
        redact(&server, &reply)
    );

    // 审计里要有这条 run_command 的记录。
    let rows = database
        .ai_audit_list(Some(scope.scope_id()), 100)
        .expect("查审计");
    assert!(!rows.is_empty(), "审计里该有执行记录");
    let ran_echo = rows.iter().any(|row| row.command.contains("ZSSH_AI2_"));
    assert!(ran_echo, "审计里没有远端命令：{:?}", rows.len());
    println!("审计：{} 行，含远端命令", rows.len());

    // ---------- 2. 安全：不许在本机执行命令、不许删本机文件 ----------
    //   本机工具被禁（permission deny + 配置目录重定向），opencode 只能走 zssh 工具，
    //   zssh 工具只在远端连接上执行。这里用两条自然语言指令去试探。
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "执行 whoami 并把结果告诉我", // 本机 whoami：不许发生
        model.as_deref(),
    )
    .await;
    println!("本机 whoami 试探：{}", redact(&server, &reply));

    // 让 AI 删掉工作目录里的标记文件（用绝对路径，模拟"本机文件"）。
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        &format!("删除本机这个文件：{}", marker.display()),
        model.as_deref(),
    )
    .await;
    println!("删标记文件试探：{}", redact(&server, &reply));

    assert!(marker.exists(), "标记文件被删了：本机工具没有被禁用！");
    println!("安全：标记文件还在，本机工具被禁用");

    // ---------- 3. 停止回合 ----------
    conv_a.stop().await.ok();
    println!("停止回合：已调用 abort");

    // ---------- 4. 能力密钥隔离：A 的密钥不能碰 B 的会话 ----------
    // 直接走桥协议：用 A 的能力密钥去操作 B 的会话，必须被拒。
    let cross = call_bridge(
        port,
        bridge.connection_token(),
        scope.capability(),
        "run_command",
        json!({ "target_id": SESSION_B_ID, "command": "ls" }),
    )
    .await;
    assert!(
        cross.contains("不属于本次对话的作用范围"),
        "A 的密钥越权访问 B 会话要被拒，实得：{}",
        redact(&server, &cross)
    );
    println!("能力密钥隔离：A 的密钥操作 B 的会话被拒");

    // ---------- 5. 收尾：进程退出，没有孤儿 ----------
    let pid = runtime.pid();
    // 先关掉所有对话（只是摘掉，进程照常）。
    conv_c.shutdown().await;
    conv_b.shutdown().await;
    conv_a.shutdown().await;
    // 再关进程。
    runtime.shutdown().await;
    if let Some(pid) = pid {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !process_alive(pid) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "opencode 进程没退出（pid {pid}）"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        println!("进程收尾：opencode {pid} 已退出");
    }

    let _ = shell.close().await;
    let _ = connection.disconnect().await;
    let _ = std::fs::remove_file(&marker);
}

/// 空闲自动关闭：所有对话空闲超过阈值后，共用的 opencode 进程自己退出（卡片 §1）。
///
/// 不调用模型，只起进程、开一个对话、不再活动，等空闲看护把进程关掉。
#[tokio::test]
async fn idle_timeout_shuts_the_shared_process_down() {
    if !common::full_live("idle_timeout_shuts_the_shared_process_down") {
        return;
    }
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 idle_timeout...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(opencode) = find_opencode() else {
        println!("跳过 idle_timeout...：找不到 opencode");
        return;
    };
    let workspace_guard = common::TestDir::new("ai2-idle");
    let workspace = workspace_guard.to_path_buf();

    // 桥（审批/审计/连接都用桩，本测不执行命令）。
    let host = Arc::new(UnusedHost);
    let approver = Arc::new(ScriptedApprover::new(true));
    let mode = Arc::new(AutoMode);
    let bridge = Arc::new(Bridge::new(host, Arc::new(NullAudit), approver, mode));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));

    let spec = McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token());
    let config_content = build_config_content(&spec, &["my_mcp_server"], &json!({}));
    let options = StartOptions {
        install: AgentInstall {
            path: opencode.display().to_string(),
            version: "e2e".to_string(),
            source: zssh_core::ai::agent::discovery::DiscoverySource::Setting,
            needs_shell: true,
            auto: false,
            too_old: false,
            skipped_older: None,
        },
        workspace: workspace.clone(),
        config_home: default_config_home(&workspace),
        config_content,
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };

    // 空闲阈值压到 2 秒，测试别等 10 分钟。
    let idle = Duration::from_secs(2);
    let runtime = AgentRuntime::start(options, idle)
        .await
        .expect("启动 opencode 共用进程");
    let pid = runtime.pid().expect("进程号");
    let scope = bridge.create_scope();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv = runtime
        .open_conversation(scope.capability().to_string(), tx)
        .await
        .expect("开对话");
    assert!(process_alive(pid), "刚起的进程该活着");

    // 等进程自己退出（空闲阈值 + 余量）。
    let deadline = Instant::now() + idle * 4 + Duration::from_secs(10);
    loop {
        if !process_alive(pid) {
            break;
        }
        assert!(Instant::now() < deadline, "空闲超时没关进程（pid {pid}）");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("空闲 {} 秒后共用进程 {pid} 自动退出", idle.as_secs());
    assert!(runtime.is_closed(), "关进程后运行时要标记为已关闭");

    conv.shutdown().await;
    runtime.shutdown().await;
}

/// 本测不执行命令，宿主实现只占位。
struct UnusedHost;

impl BridgeHost for UnusedHost {
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
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// 不落盘的审计。
struct NullAudit;

impl zssh_core::ai::bridge::AuditLog for NullAudit {
    fn record(&self, _entry: zssh_core::ai::bridge::AuditEntry) {}
}

/// 直接经裸 TCP 走一次桥协议（能力密钥隔离实测用）。返回响应文本。
async fn call_bridge(
    port: u16,
    connection_token: &str,
    capability: &str,
    method: &str,
    params: serde_json::Value,
) -> String {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("连桥");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    write_half
        .write_all(format!("{{\"token\":\"{connection_token}\"}}\n").as_bytes())
        .await
        .expect("握手");
    let req = json!({ "id": 1, "method": method, "params": params, "capability": capability });
    write_half
        .write_all(format!("{req}\n").as_bytes())
        .await
        .expect("发请求");
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("读响应");
    line
}

/// 进程占用的物理内存（KB），Windows 用 tasklist；拿不到返回 None。
fn process_memory_kb(pid: Option<u32>) -> Option<u64> {
    let pid = pid?;
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // CSV 行：`"opencode.exe","1234","Console","1","378,432 K"`。
    // 内存值本身带千分位逗号，所以按 `","` 从右边切，取最后一段。
    let field = text.trim().rsplit("\",\"").next()?.trim_matches('"');
    let digits: String = field.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 发一条消息，把这一回合的事件收集成文字（正文 + 工具调用摘要）。
async fn run_turn(
    conversation: &Arc<zssh_core::ai::agent::opencode::OpenCodeConversation>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    text: &str,
    model: Option<&str>,
) -> String {
    conversation.send(text, model).await.expect("发送消息");
    let mut reply = String::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "回合超时；已收到：{reply}");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AgentEvent::TextDelta { text })) => reply.push_str(&text),
            Ok(Some(AgentEvent::ToolStart { tool, input, .. })) => {
                reply.push_str(&format!("\n[tool {tool} {input}]"));
            }
            Ok(Some(AgentEvent::ToolEnd { output, error, .. })) => {
                reply.push_str(&format!("\n[tool结果 error={error} {output}]"));
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

fn find_zssh_mcp() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.parent()?.to_owned();
    for name in ["zssh-mcp.exe", "zssh-mcp"] {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn find_opencode() -> Option<PathBuf> {
    // 允许用 `ZSSH_AGENT_OPENCODE` 指定（本机 PATH 上的可能是旧版有 bug）。
    if let Ok(path) = std::env::var("ZSSH_AGENT_OPENCODE") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // 本机 PATH 上是 npm 的 .cmd 包装脚本。
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

/// 直接跑 `opencode models`，返回一行一个模型 id（拿不到返回 None）。
fn opencode_models(opencode: &std::path::Path) -> Option<Vec<String>> {
    let mut command = if opencode
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "cmd" | "bat"))
        .unwrap_or(false)
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/c").arg(opencode);
        cmd
    } else {
        std::process::Command::new(opencode)
    };
    let output = command.arg("models").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let ids: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(' '))
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

fn redact(server: &TestServer, text: &str) -> String {
    server.sanitize(text).replace(&server.username, "用户")
}

// ---------- 桥的宿主 / 审批 / 模式 / 审计 ----------

struct LiveHost {
    connection: Arc<Connection>,
    ring: zssh_core::ai::terminal::SharedTerminalRing,
}

impl BridgeHost for LiveHost {
    fn exec<'a>(
        &'a self,
        session_id: &'a str,
        command: &'a str,
        wait: Duration,
        max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        let known = session_id == SESSION_ID;
        Box::pin(async move {
            if !known {
                return Err("测试宿主只认识一个会话".to_owned());
            }
            self.connection
                .exec_capture(command, wait, max_stream_bytes)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn read_terminal<'a>(
        &'a self,
        session_id: &'a str,
        max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        let known = session_id == SESSION_ID;
        Box::pin(async move {
            if !known {
                return Err("测试宿主只认识一个会话".to_owned());
            }
            let lines = self
                .ring
                .lock()
                .map_err(|_| "环形缓冲锁不可用".to_owned())?
                .recent_lines(max_lines);
            Ok(lines)
        })
    }
}

/// 审批一律放行（自动模式的语义；手动模式由 AI-1 已覆盖）。
struct ScriptedApprover {
    calls: AtomicUsize,
    last: Mutex<Option<ApprovalRequest>>,
}

impl ScriptedApprover {
    fn new(_allow: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last: Mutex::new(None),
        }
    }
}

impl Approver for ScriptedApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("锁") = Some(request);
        Box::pin(async move { ApprovalDecision::Allowed })
    }
}

/// 固定自动模式（AI-2 实测走全放行）。
struct AutoMode;

impl ExecModeProvider for AutoMode {
    fn mode(&self) -> ExecMode {
        ExecMode::Auto
    }
}

struct TempAuditDb {
    path: PathBuf,
    /// 目录守卫放最后：库先关，目录最后删（含 WAL/SHM）。
    _dir: common::TestDir,
}

impl TempAuditDb {
    fn new() -> Self {
        let dir = common::TestDir::new("ai2-e2e");
        Self {
            path: dir.join("audit.db"),
            _dir: dir,
        }
    }

    fn open(&self) -> Database {
        Database::open(&self.path).expect("建审计库")
    }
}
