//! 工具桥全链路实测（卡片 AI-1 第 7 节）：真实 SSH 服务器 + 桥 + zssh-mcp.exe + MCP 协议。
//!
//! 链路：MCP 客户端（rmcp）→ zssh-mcp 子进程（stdio）→ 工具桥（TCP）→ 真实连接。
//! 审批走脚本化的桩（Tauri 里换成人机事件，语义相同），审计写真实的临时数据库。
//!
//! 前置条件：
//! - 环境变量 `ZSSH_TEST_HOST` / `ZSSH_TEST_PORT` / `ZSSH_TEST_USER` /
//!   `ZSSH_TEST_PASSWORD`（和别的 live 测试一样，缺了就跳过）；
//! - 先编译出 `zssh-mcp.exe`（`cargo build -p zssh-mcp`；`cargo test --workspace`
//!   和五门禁都会把它带上），找不到也跳过。
//!
//! 输出里不出现地址、用户名和密码：打印前一律过 `redact`。

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, Bridge, BridgeHost, ExecMode, ExecModeProvider,
};
use zssh_core::ai::safety::{assess, DangerLevel};
use zssh_core::ai::terminal::TerminalRing;
use zssh_core::ssh::{BoxFuture, Connection, ExecCapture, DEFAULT_BUFFER_LIMIT};
use zssh_core::Database;

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;
/// 桥和宿主里都只认识这一个会话。
const SESSION_ID: &str = "e2e-1";
/// 远端算出来的结果：回显里只有 `$((40+2))`，出现 42 说明命令真的在服务器上跑过。
const ECHO_MARKER: &str = "ZSSH_AI_E2E_42";
const ECHO_COMMAND: &str = "echo ZSSH_AI_E2E_$((40+2))";
/// 终端标记：先打进 shell（进环形缓冲），再从 MCP 读出来。
const RING_MARKER: &str = "ZSSH_AI_E2E_RING_42";

#[tokio::test]
async fn bridge_end_to_end_over_mcp() {
    if !common::full_live("bridge_end_to_end_over_mcp") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 bridge_end_to_end_over_mcp：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!(
            "跳过 bridge_end_to_end_over_mcp：找不到 zssh-mcp 可执行文件（先 cargo build -p zssh-mcp）"
        );
        return;
    };

    // ---------- 真实连接 + 带环形缓冲的 shell ----------
    let known_hosts = TempKnownHosts::new("ai-bridge");
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

    // ---------- 桥 + 审批桩 + 真实审计库 ----------
    let audit_db = TempAuditDb::new();
    let database = Arc::new(audit_db.open());
    let approver = Arc::new(ScriptedApprover::new(true));
    let mode = Arc::new(SwitchableMode::manual());
    let host = Arc::new(LiveHost {
        connection: Arc::clone(&connection),
        ring: Arc::clone(&ring),
    });
    let bridge = Arc::new(Bridge::new(
        host,
        database.clone(),
        approver.clone(),
        mode.clone(),
    ));

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    let serve_task = tokio::spawn(Arc::clone(&bridge).serve(listener));

    let scope = bridge.create_scope();
    scope.add_target(SESSION_ID, "测试服务器", "root", "connected", true, false);
    // 连接层令牌只有进程级一个；对话用能力密钥（工具调用时随参数带上）。
    let mcp = start_mcp(&mcp_exe, port, bridge.connection_token(), &scope).await;
    println!("工具桥端到端：桥端口 {port}，MCP 子进程已连上");

    // ---------- 1. 工具表 ----------
    let tools = mcp.service.list_all_tools().await.expect("列工具");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in ["list_targets", "run_command", "read_terminal"] {
        assert!(names.contains(&expected), "工具表里缺少 {expected}");
    }
    println!("工具表：{:?}", names);

    // ---------- 2. list_targets：只给显示信息 ----------
    let result = call_tool(&mcp, "list_targets", json!({})).await;
    let text = tool_text(&result);
    assert!(
        !tool_failed(&result),
        "不该失败：{}",
        redact(&server, &text)
    );
    assert!(text.contains(SESSION_ID), "要列出会话 id");
    assert!(text.contains("测试服务器"), "要列出主机显示名");
    println!("list_targets：{}", redact(&server, &text));

    // ---------- 3. 手动模式：审批通过后真实执行 ----------
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": ECHO_COMMAND }),
    )
    .await;
    let text = tool_text(&result);
    assert!(
        !tool_failed(&result),
        "审批通过后执行不该失败：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains(ECHO_MARKER),
        "输出里没有标记：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("exit_code: 0"),
        "退出码应为 0：{}",
        redact(&server, &text)
    );
    assert_eq!(approver.calls(), 1, "手动模式要过审批");
    let request = approver.last().expect("审批请求");
    assert_eq!(request.command, ECHO_COMMAND);
    assert_eq!(request.host_label, "测试服务器");
    assert_eq!(request.danger, DangerLevel::Safe);
    println!("手动模式执行：exit_code 0，输出含标记 {ECHO_MARKER}");

    // ---------- 4. 非零退出码：结果仍是成功，退出码如实上报 ----------
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "sh -c \"exit 3\"" }),
    )
    .await;
    let text = tool_text(&result);
    assert!(!tool_failed(&result), "退出码非 0 不算工具错误");
    assert!(
        text.contains("exit_code: 3"),
        "退出码要如实上报：{}",
        redact(&server, &text)
    );
    println!("非零退出码：exit_code 3 如实上报");

    // ---------- 5. 大输出截断：64KB 上限，丢的字节数如实上报 ----------
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "seq 1 20000" }),
    )
    .await;
    let text = tool_text(&result);
    assert!(!tool_failed(&result), "截断不算工具错误");
    assert!(
        text.contains("stdout truncated:"),
        "大输出必须报截断：{}",
        redact(&server, &text)
    );
    println!("64KB 截断：报告了丢弃的字节数");

    // ---------- 6. 执行超时：sleep 5 / timeout_sec 1 ----------
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "sleep 5", "timeout_sec": 1 }),
    )
    .await;
    let text = tool_text(&result);
    assert!(tool_failed(&result), "超时要按错误上报");
    assert!(
        text.contains("已超时"),
        "超时文案不对：{}",
        redact(&server, &text)
    );
    println!("执行超时：timeout_sec=1 生效，返回「已超时」");

    // ---------- 7. 手动模式：审批拒绝 ----------
    approver.allow(false);
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "uname -a" }),
    )
    .await;
    let text = tool_text(&result);
    assert!(tool_failed(&result), "拒绝要按错误上报");
    assert!(
        text.contains("用户拒绝"),
        "拒绝文案不对：{}",
        redact(&server, &text)
    );
    println!("手动拒绝：返回「用户拒绝」");
    approver.allow(true);

    // ---------- 8. 自动模式：全部直接执行、不进审批；危险只标记 ----------
    mode.auto();
    let calls_before = approver.calls();
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "uname -a" }),
    )
    .await;
    let text = tool_text(&result);
    assert!(!tool_failed(&result), "自动模式安全命令要直接跑");
    assert!(
        text.contains("danger: safe"),
        "结果里要标出安全：{}",
        redact(&server, &text)
    );
    assert_eq!(approver.calls(), calls_before, "安全命令不该问审批");

    // 危险但无害的命令（目标用户不存在，执行没有任何副作用）。
    // 执行前先确认它真的会被判为危险，这样"危险命令也直接放行"才有说服力。
    let harmless_dangerous = "crontab -r -u nonexistent-user-zssh 2>/dev/null; true";
    assert!(
        assess(harmless_dangerous).is_dangerous(),
        "这条命令要被判为危险，否则换一条"
    );
    let result = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": harmless_dangerous }),
    )
    .await;
    let text = tool_text(&result);
    assert!(
        !tool_failed(&result),
        "自动模式危险命令也要直接执行：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("danger: dangerous"),
        "结果里要标出危险：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("exit_code: 0"),
        "无害危险命令执行成功：{}",
        redact(&server, &text)
    );
    assert_eq!(
        approver.calls(),
        calls_before,
        "自动模式下审批接口一次都不该被调用"
    );
    println!("自动模式：安全直放；{harmless_dangerous} 被判危险但仍直接执行（不进审批）");

    // ---------- 9. read_terminal：从环形缓冲里读回终端输出 ----------
    let output = common::run_marker(&mut shell, "AI_E2E_RING", Duration::from_secs(15)).await;
    assert!(
        output.contains(RING_MARKER),
        "shell 里没读到标记：{}",
        redact(&server, &output)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = call_tool(
            &mcp,
            "read_terminal",
            json!({ "target_id": SESSION_ID, "max_lines": 50 }),
        )
        .await;
        let text = tool_text(&result);
        if !tool_failed(&result) && text.contains(RING_MARKER) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "环形缓冲里等不到标记：{}",
            redact(&server, &text)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("read_terminal：从环形缓冲读回终端标记");

    // ---------- 10. 错误连接层令牌：裸 TCP 直接被断开 ----------
    let mut raw = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("裸连桥");
    raw.write_all(b"{\"token\":\"wrong-token\"}\n")
        .await
        .expect("发错误令牌");
    let mut buffer = [0u8; 64];
    match tokio::time::timeout(Duration::from_secs(2), raw.read(&mut buffer)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        other => panic!("错误令牌必须被断开，实际：{other:?}"),
    }
    println!("错误连接层令牌：连接被断开");

    // ---------- 11. A/B 能力密钥隔离 + 销毁 ----------
    let scope_b = bridge.create_scope();
    let mcp_b = start_mcp(&mcp_exe, port, bridge.connection_token(), &scope_b).await;
    let result = call_tool(
        &mcp_b,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "ls" }),
    )
    .await;
    let text = tool_text(&result);
    assert!(tool_failed(&result));
    assert!(
        text.contains("不属于本次对话的作用范围"),
        "跨 scope 越权要被拒：{}",
        redact(&server, &text)
    );

    // B 的能力密钥不该能操作 A 的会话；反过来 A 也看不到 B。
    let result = call_tool(&mcp_b, "list_targets", json!({})).await;
    assert!(
        tool_text(&result).contains("No targets"),
        "B 看不到 A 的会话"
    );
    assert!(
        tool_text(&call_tool(&mcp, "list_targets", json!({})).await).contains(SESSION_ID),
        "A 仍看得到自己的会话"
    );

    scope_b.destroy();
    let result = call_tool(&mcp_b, "list_targets", json!({})).await;
    let text = tool_text(&result);
    assert!(tool_failed(&result));
    assert!(
        text.contains("能力密钥无效或已过期"),
        "销毁后请求要被拒：{}",
        redact(&server, &text)
    );
    println!("能力密钥隔离：B 越权被拒；B 看不到 A 的会话；销毁后 B 的密钥失效");

    // 用一个伪造的能力密钥（同一个连接层令牌）也要被拒。
    let forged = start_mcp_with(
        &mcp_exe,
        port,
        bridge.connection_token(),
        "deadbeefdeadbeefdeadbeefdeadbeef",
    )
    .await;
    let result = call_tool(&forged, "list_targets", json!({})).await;
    assert!(tool_failed(&result));
    assert!(
        tool_text(&result).contains("能力密钥无效或已过期"),
        "伪造的能力密钥要被拒：{}",
        redact(&server, &tool_text(&result))
    );
    let _ = forged.service.cancel().await;
    println!("伪造能力密钥：被拒");

    // ---------- 12. 审计：真实数据库里的行 ----------
    let rows = database
        .ai_audit_list(Some(scope.scope_id()), 100)
        .expect("查审计");
    assert!(rows.len() >= 6, "至少 6 条审计，实际 {}", rows.len());
    let decisions: Vec<&str> = rows.iter().map(|row| row.decision.as_str()).collect();
    for expected in ["allowed", "denied", "auto"] {
        assert!(
            decisions.contains(&expected),
            "缺少 {expected} 决策：{decisions:?}"
        );
    }
    let timeout_row = rows
        .iter()
        .find(|row| row.command == "sleep 5")
        .expect("超时那条审计");
    assert_eq!(timeout_row.decision, "allowed", "执行超时不改审批决策");
    assert_eq!(timeout_row.exit_code, None);
    assert!(timeout_row.duration_ms.is_some());
    let dangerous_row = rows
        .iter()
        .find(|row| row.command == "crontab -r -u nonexistent-user-zssh 2>/dev/null; true")
        .expect("无害危险命令的审计");
    assert_eq!(dangerous_row.decision, "auto", "自动模式决策记成自动放行");
    assert_eq!(dangerous_row.danger, "dangerous", "危险照常记 dangerous");
    assert!(
        database
            .ai_audit_list(Some(scope_b.scope_id()), 10)
            .expect("查 B 审计")
            .is_empty(),
        "被拒的 scope 不该有执行审计"
    );
    println!("审计（scope {}）共 {} 行：", scope.scope_id(), rows.len());
    for row in &rows {
        println!(
            "  #{:<2} {:<7} {:<12} exit={:<5} duration={}",
            row.id,
            row.decision,
            row.danger,
            row.exit_code
                .map_or("-".to_owned(), |code| code.to_string()),
            row.duration_ms
                .map_or("-".to_owned(), |ms| format!("{ms}ms")),
        );
    }

    // ---------- 收尾 ----------
    let _ = mcp_b.service.cancel().await;
    let _ = mcp.service.cancel().await;
    let _ = shell.close().await;
    let _ = connection.disconnect().await;
    // 桥的服务任务还抱着审计库：停掉它、让出一次调度，库文件才关得上、临时目录才删得掉。
    serve_task.abort();
    let _ = serve_task.await;
}

// ---------- MCP 客户端 ----------

type McpService = RunningService<RoleClient, ()>;

/// 一个 MCP 子进程客户端：连接层令牌在启动时给了，能力密钥在每次工具调用时带上。
struct McpClient {
    service: McpService,
    capability: String,
}

/// 找编译产物 `zssh-mcp(.exe)`：测试二进制在 `target/debug/deps/`，上两级就是 `target/debug/`。
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

/// 起一个 `zssh-mcp.exe` 子进程并完成 MCP 握手（用给定作用范围的能力密钥 +
/// 桥的连接层令牌）。
async fn start_mcp(
    exe: &Path,
    port: u16,
    connection_token: &str,
    scope: &zssh_core::ai::bridge::ScopeHandle,
) -> McpClient {
    start_mcp_with(exe, port, connection_token, scope.capability()).await
}

/// 起一个 MCP 子进程，能力密钥由调用方另行决定（伪造密钥测试用）。
async fn start_mcp_with(
    exe: &Path,
    port: u16,
    connection_token: &str,
    capability: &str,
) -> McpClient {
    let mut command = Command::new(exe);
    command.env("ZSSH_BRIDGE_PORT", port.to_string());
    command.env("ZSSH_BRIDGE_TOKEN", connection_token);
    let transport = TokioChildProcess::new(command).expect("起 zssh-mcp 子进程");
    let service = ().serve(transport).await.expect("MCP 握手");
    McpClient {
        service,
        capability: capability.to_string(),
    }
}

async fn call_tool(client: &McpClient, name: &str, arguments: Value) -> CallToolResult {
    let mut args = arguments.as_object().cloned().unwrap_or_default();
    // 每个工具调用都要带能力密钥（缺了桥会拒）。
    args.insert(
        "capability".to_string(),
        Value::String(client.capability.clone()),
    );
    client
        .service
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .expect("工具调用要走通协议")
}

fn tool_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_failed(result: &CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

/// 打印/断言消息里不能出现地址、用户名：过一遍再输出。
fn redact(server: &TestServer, text: &str) -> String {
    server.sanitize(text).replace(&server.username, "用户")
}

// ---------- 桥的宿主 / 审批 / 模式 / 审计 ----------

/// 真实宿主：exec 开在真实连接上，read_terminal 读真实环形缓冲。
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

/// 脚本化审批：允许/拒绝可切换，记录调用次数和最后一次请求。
struct ScriptedApprover {
    allow: AtomicBool,
    calls: AtomicUsize,
    last: Mutex<Option<ApprovalRequest>>,
}

impl ScriptedApprover {
    fn new(allow: bool) -> Self {
        Self {
            allow: AtomicBool::new(allow),
            calls: AtomicUsize::new(0),
            last: Mutex::new(None),
        }
    }

    fn allow(&self, value: bool) {
        self.allow.store(value, Ordering::SeqCst);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last(&self) -> Option<ApprovalRequest> {
        self.last.lock().unwrap().clone()
    }
}

impl Approver for ScriptedApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = Some(request);
        let decision = if self.allow.load(Ordering::SeqCst) {
            ApprovalDecision::Allowed
        } else {
            ApprovalDecision::Denied
        };
        Box::pin(async move { decision })
    }
}

/// 可切换的执行模式（模拟设置项被改）。
struct SwitchableMode(AtomicBool);

impl SwitchableMode {
    fn manual() -> Self {
        Self(AtomicBool::new(false))
    }

    fn auto(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ExecModeProvider for SwitchableMode {
    fn mode(&self) -> ExecMode {
        if self.0.load(Ordering::SeqCst) {
            ExecMode::Auto
        } else {
            ExecMode::Manual
        }
    }
}

/// 临时文件审计库，测完自己删。
struct TempAuditDb {
    path: PathBuf,
    /// 目录守卫放最后：库先关，目录最后删（含 WAL/SHM）。
    _dir: common::TestDir,
}

impl TempAuditDb {
    fn new() -> Self {
        let dir = common::TestDir::new("ai-e2e");
        Self {
            path: dir.join("audit.db"),
            _dir: dir,
        }
    }

    fn open(&self) -> Database {
        Database::open(&self.path).expect("建审计库")
    }
}
