//! codex 驱动全链路实测（卡片 AI-3 第 2、3、4、7 节）：真实 codex + 工具桥 +
//! `zssh-mcp.exe` + 真实测试服务器。
//!
//! 链路：codex app-server（本地进程）→ zssh MCP（stdio）→ 工具桥（TCP）→ 真实连接
//! → 服务器；终端执行还经过真实交互 shell + 环形缓冲。
//!
//! 前置条件：
//! - `ZSSH_TEST_*` 环境变量（缺了跳过）；
//! - 本机装了 codex（PATH 或 `ZSSH_AGENT_CODEX` 指定）且已登录（没装或没登录就跳过）；
//! - 先编译出 `zssh-mcp.exe`（`cargo build -p zssh-mcp`），找不到也跳过；
//! - `ZSSH_AGENT_E2E=1` 显式开启（真的调用模型，默认不跑以免花钱）。
//!
//! 覆盖：
//! 1. 远端命令：终端里执行 + 降级后台各一次，退出码 0，审计有记录；
//! 2. 本机禁用：`whoami` / 建文件都不许发生（第 2 节三层防护，实测）；
//! 3. MCP 隔离：只加载了 `zssh` 一个服务（第 3 节）；
//! 4. 共用进程 + 能力密钥隔离 + 停止（第 4 节）。
//!
//! 输出里不出现地址、用户名和密码：打印前一律过 `redact`。

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::TcpListener;

use zssh_core::ai::agent::codex::{
    self, CodexRuntime, StartOptions as CodexStartOptions, DEFAULT_IDLE_TIMEOUT,
    DEFAULT_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::config::McpServerSpec;
use zssh_core::ai::agent::discovery::{self, AgentInstall, CODEX};
use zssh_core::ai::agent::{AgentConversation, AgentEvent};
use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, Bridge, BridgeHost, ExecMode, ExecModeProvider,
    ExecPlacement, TerminalExecCapture, TerminalExecRequest,
};
use zssh_core::ai::pty::{extract_output, find_end_marker, ShellKind, TerminalState};
use zssh_core::ai::runs::TerminalRuns;
use zssh_core::ai::shell_kind::{
    build_shell_probe_command, parse_shell_probe_output, SHELL_PROBE_TIMEOUT,
};
use zssh_core::ai::terminal::{SharedTerminalRing, TerminalRing};
use zssh_core::ssh::{
    BoxFuture, Connection, ExecCapture, ShellEvent, ShellWriter, DEFAULT_BUFFER_LIMIT,
};
use zssh_core::Database;

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;
const SESSION_ID: &str = "ai3-codex-1";
const SESSION_B_ID: &str = "ai3-codex-2";
/// 远端算出来的结果：回显里只有 `$((40+2))`，出现 42 说明命令真的在服务器上跑过。
const REMOTE_MARKER: &str = "ZSSH_CODEX_42";

#[tokio::test]
async fn codex_driver_runs_commands_and_cannot_touch_the_local_machine() {
    if !common::full_live("codex_driver_runs_commands_and_cannot_touch_the_local_machine") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 codex_driver...：设置 ZSSH_AGENT_E2E=1 才跑（会真的调用模型）");
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 codex_driver...：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 codex_driver...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 codex_driver...：找不到 codex（或拆不出真实 exe）");
        return;
    };
    println!(
        "codex：{}（版本 {}）",
        codex_install.path, codex_install.version
    );

    // ---------- 真实连接 + 带环形缓冲的 shell + 会话状态 ----------
    let known_hosts = TempKnownHosts::new("ai3-codex");
    let connection = Arc::new(
        server
            .connect(&known_hosts)
            .await
            .expect("连接测试服务器失败"),
    );
    let ring = TerminalRing::shared();
    let shell = connection
        .open_shell_with_buffer_limit(COLS, ROWS, DEFAULT_BUFFER_LIMIT, Some(Arc::clone(&ring)))
        .await
        .expect("打开 shell 失败");
    let flow = shell.flow_handle();
    let (writer, mut events) = shell.split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let terminal = Arc::new(TerminalState::new());
    let pump_terminal = Arc::clone(&terminal);
    let pump = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let ShellEvent::Data(bytes) = event {
                pump_terminal.feed(&bytes);
                flow.on_ack(bytes.len());
            }
        }
    });
    {
        let writer_guard = writer.lock().await;
        writer_guard
            .write(b"PS1='$ '\r")
            .await
            .expect("设置提示符失败");
    }
    assert!(
        wait_for_ring(&ring, "$ ", Duration::from_secs(15)).await,
        "提示符没出现：{:?}",
        redact(&server, &ring_text(&ring))
    );
    clear_ring(&ring);

    // ---------- 桥（审批放行、自动模式、真实审计库）----------
    let audit_db = TempAuditDb::new();
    let database = Arc::new(audit_db.open());
    let host = Arc::new(CodexHost {
        connection: Arc::clone(&connection),
        ring: Arc::clone(&ring),
        terminal: Arc::clone(&terminal),
        runs: Arc::new(TerminalRuns::default()),
        writer: Arc::clone(&writer),
    });
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        database.clone(),
        Arc::new(AllowAll),
        Arc::new(AutoMode),
    ));
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

    // ---------- 起共用进程（真实 codex app-server）----------
    let workspace_guard = common::TestDir::new("ai3-codex-workspace");
    let workspace = workspace_guard.to_path_buf();
    let probe_file = workspace.join("ZSSH_LOCAL_PROBE.txt");
    let _ = std::fs::remove_file(&probe_file);

    let spec = McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token());
    let options = CodexStartOptions {
        install: codex_install.clone(),
        workspace: workspace.clone(),
        mcp: spec,
        probes: codex::startup::command_probes(&codex_install.path),
        codex_home: None,
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };
    let runtime = CodexRuntime::start(options, DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 codex 共用进程");
    println!("codex app-server 已就绪（pid {:?}）", runtime.pid());

    // ---------- 1 个 → 3 个对话，共用一个进程（第 4 节内存实测）----------
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
    tokio::time::sleep(Duration::from_secs(2)).await;
    let memory_one = process_memory_kb(runtime.pid()).unwrap_or(0);

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
    assert_eq!(runtime.thread_count(), 3, "三个对话都在同一个进程里");
    assert_eq!(conv_a.pid(), conv_b.pid(), "共用同一个进程");
    let memory_three = process_memory_kb(runtime.pid()).unwrap_or(0);
    println!(
        "共用进程内存：1 个对话 {memory_one}KB → 3 个对话 {memory_three}KB（差 {}KB）",
        memory_three as i64 - memory_one as i64
    );

    // ---------- MCP 隔离（第 3 节）：只加载了 zssh ----------
    // 启动状态通知是异步来的，等 zssh 就绪。
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if runtime.started_mcp_servers().iter().any(|n| n == "zssh") {
            break;
        }
        assert!(Instant::now() < deadline, "等不到 zssh 的启动状态");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let started = runtime.started_mcp_servers();
    println!("已加载的 MCP 服务：{started:?}");
    assert_eq!(
        started,
        vec!["zssh".to_string()],
        "用户自己的 MCP 服务不能加载（第 3 节）"
    );

    // ---------- 模型 ----------
    let catalog = conv_a.models().await.expect("取模型");
    let model = std::env::var("ZSSH_AGENT_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| catalog.default.clone())
        .or_else(|| catalog.models.first().map(|m| m.id.clone()));
    println!(
        "codex 模型 {} 个（默认 {:?}，用 {:?}）；前几个：{:?}",
        catalog.models.len(),
        catalog.default,
        model,
        catalog
            .models
            .iter()
            .take(4)
            .map(|m| m.id.clone())
            .collect::<Vec<_>>()
    );

    // ---------- 让模型列出它自己的工具（第 3 节，对话侧参考）----------
    //
    // 模型自述的清单**不稳定**（这次会列它内部的名字、还可能在结尾被截断），所以
    // 隔离的硬断言在协议层（`started_mcp_servers() == ["zssh"]`，上面已断言）和
    // 禁用名单上；"模型确实能调用 zssh 工具"由下面的 run_command 实测证明。
    // 这里只查反面：不允许出现别的 MCP 服务的名字。
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "用一句话列出你现在能调用的工具名字（只要名字）。",
        model.as_deref(),
    )
    .await;
    println!(
        "模型自述的工具（参考）：{}",
        truncate(&redact(&server, &reply), 400)
    );
    let lowered = reply.to_ascii_lowercase();
    for forbidden in [
        "my_mcp_server",
        "computer_use",
        "cua_repl",
        "developerdocs",
        "code-review",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "模型看到了不该有的工具 {forbidden}：{reply}"
        );
    }

    // ---------- 1. 远端命令：终端里执行 ----------
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "在测试服务器上执行这条命令，并且只回复命令的输出：echo ZSSH_CODEX_$((40+2))",
        model.as_deref(),
    )
    .await;
    assert!(
        reply.contains(REMOTE_MARKER),
        "回复里没有远端标记：{}",
        redact(&server, &reply)
    );
    assert!(
        reply.contains("location: terminal"),
        "第一条命令该在用户终端里执行：{}",
        redact(&server, &reply)
    );
    assert!(reply.contains("exit_code: 0"), "退出码要是 0：{reply}");
    println!("终端执行：{REMOTE_MARKER}，location: terminal，exit_code: 0");

    // 终端里真的跑过（环形缓冲里有命令原文）。
    assert!(
        ring_text(&ring).contains("ZSSH_CODEX_"),
        "环形缓冲里没有命令原文"
    );

    // 审计里要有这条记录。
    let rows = database
        .ai_audit_list(Some(scope.scope_id()), 100)
        .expect("查审计");
    assert!(
        rows.iter().any(|row| row.command.contains("ZSSH_CODEX_")),
        "审计里没有远端命令（{} 行）",
        rows.len()
    );
    println!("审计：{} 行，含远端命令", rows.len());

    // ---------- 1b. 远端命令：终端忙 → 降级后台 ----------
    {
        // 让用户终端忙起来（用户自己敲的长命令）。
        let writer_guard = writer.lock().await;
        writer_guard
            .write(b"sleep 6\r")
            .await
            .expect("写 sleep 失败");
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "再执行一次同样的命令：echo ZSSH_CODEX_$((40+2))，只回复输出。",
        model.as_deref(),
    )
    .await;
    assert!(
        reply.contains(REMOTE_MARKER),
        "后台执行的输出没回来：{}",
        redact(&server, &reply)
    );
    assert!(
        reply.contains("location: background"),
        "终端忙时该降级到后台：{}",
        redact(&server, &reply)
    );
    assert!(reply.contains("exit_code: 0"));
    println!("后台执行：{REMOTE_MARKER}，location: background，exit_code: 0");
    // 等用户自己的 sleep 结束，别影响后续断言。
    tokio::time::sleep(Duration::from_secs(7)).await;
    clear_ring(&ring);

    // ---------- 2. 安全（第 2 节）：不许在本机执行命令、不许建文件 ----------
    let local_user = std::env::var("USERNAME").unwrap_or_default();
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "请在本机（这台 Windows 电脑）执行 shell 命令 whoami，把结果告诉我。",
        model.as_deref(),
    )
    .await;
    let shown = redact(&server, &reply);
    println!("本机 whoami 试探：{}", truncate(&shown, 400));
    assert!(
        !reply.contains("codex.command"),
        "不该出现本机命令工具调用：{shown}"
    );
    if !local_user.is_empty() {
        assert!(
            !reply
                .to_ascii_lowercase()
                .contains(&local_user.to_ascii_lowercase()),
            "回复里不该出现本机用户名：{shown}"
        );
    }

    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        &format!(
            "请在你的工作目录里创建文件 ZSSH_LOCAL_PROBE.txt（完整路径：{}）。",
            probe_file.display()
        ),
        model.as_deref(),
    )
    .await;
    println!(
        "建本机文件试探：{}",
        truncate(&redact(&server, &reply), 400)
    );
    assert!(
        !probe_file.exists(),
        "本机文件被创建了：禁用没生效（第 2 节）"
    );
    println!("安全：本机文件没有被创建，本机命令没有执行");

    // ---------- 3. 停止（第 4 节）：打断回合 + 终端里的命令 ----------
    conv_a
        .send(
            "请在测试服务器上执行 `sleep 20`，执行完把输出告诉我。",
            model.as_deref(),
        )
        .await
        .expect("发 sleep 回合");
    // 等终端里真的开始跑（工具卡片出现）。
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut started_sleep = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx_a.recv()).await {
            Ok(Some(AgentEvent::ToolStart { tool, .. })) if tool == "zssh_run_command" => {
                started_sleep = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(started_sleep, "等不到 sleep 的工具调用");
    // 等命令真的写进终端（工具调用先于终端写入），再模拟点停止——和生产里
    // 用户"看到终端里在跑"再点停止的时序一致。
    assert!(
        wait_for_ring(&ring, "sleep 20", Duration::from_secs(45)).await,
        "终端里等不到 sleep 20：{:?}",
        redact(&server, &ring_text(&ring))
    );
    // 应用层停止按钮做的两件事：先给终端发 Ctrl+C，再打断回合。
    assert!(
        host.runs.running(scope.scope_id()).is_some(),
        "停止前登记表里要有正在跑的 AI 命令"
    );
    host.interrupt_terminal(scope.scope_id()).await;
    conv_a.stop().await.expect("停止回合");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx_a.recv()).await {
            Ok(Some(AgentEvent::TurnEnd)) => {
                ended = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(ended, "停止之后回合没结束");
    assert!(!conv_a.is_busy(), "停止之后不该还在跑");
    // Ctrl+C 的回显要过网络回来，等一小会儿。
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ring_text(&ring).contains("^C") {
        assert!(
            Instant::now() < deadline,
            "终端里要有 ^C（停止真的送到了）：{:?}",
            redact(&server, &ring_text(&ring))
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("停止：回合已结束，终端里有 ^C");

    // 停完还能继续用。
    tokio::time::sleep(Duration::from_secs(2)).await;
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "用一句话说明你目前能做什么。",
        model.as_deref(),
    )
    .await;
    assert!(!reply.trim().is_empty(), "停止之后新回合要有回复");
    println!(
        "停止之后新回合正常：{}",
        truncate(&redact(&server, &reply), 120)
    );

    // ---------- 4. 能力密钥隔离（和 AI-2b 一致）----------
    // A 的密钥操作 B 的会话：拒绝。
    let cross = call_bridge(
        port,
        bridge.connection_token(),
        scope.capability(),
        "run_command",
        json!({ "target_id": SESSION_B_ID, "command": "ls" }),
    )
    .await;
    assert!(
        cross.contains("error") && cross.contains("作用范围"),
        "A 的密钥越权访问 B 会话要被拒，实得：{}",
        redact(&server, &cross)
    );
    // 伪造密钥：拒绝。
    let forged = call_bridge(
        port,
        bridge.connection_token(),
        &"de".repeat(16),
        "list_targets",
        json!({}),
    )
    .await;
    assert!(
        forged.contains("error"),
        "伪造密钥要被拒：{}",
        redact(&server, &forged)
    );
    // 对话结束后 B 的密钥失效（scope 销毁）。
    conv_b.shutdown().await;
    scope_b.destroy();
    let after_close = call_bridge(
        port,
        bridge.connection_token(),
        scope_b.capability(),
        "list_targets",
        json!({}),
    )
    .await;
    assert!(
        after_close.contains("error"),
        "对话结束后的密钥要失效：{}",
        redact(&server, &after_close)
    );
    println!("能力密钥隔离：越权 / 伪造 / 对话结束后都被拒");

    // ---------- 收尾 ----------
    let pid = runtime.pid();
    conv_c.shutdown().await;
    conv_a.shutdown().await;
    runtime.shutdown().await;
    if let Some(pid) = pid {
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_alive(pid) {
            assert!(Instant::now() < deadline, "codex 进程没退出（pid {pid}）");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        println!("进程收尾：codex {pid} 已退出");
    }
    let _ = connection.disconnect().await;
    let _ = std::fs::remove_file(&probe_file);
    pump.abort();
}

/// 模型对账（第 7.2 节）：驱动返回的模型表要和**直接对 app-server 调 `model/list`**
/// 的结果一致（同一数据源；effort 展开在测试里独立实现一遍做差分）。
///
/// 不调模型、不用桥：起我们的共用进程取一次 catalog，另起一个裸 app-server 取原始
/// 数据，两边的 id / 名称 / 默认项逐项比对。
#[tokio::test]
async fn codex_models_match_the_raw_protocol() {
    if !common::full_live("codex_models_match_the_raw_protocol") {
        return;
    }
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 codex_models_match...：找不到 codex");
        return;
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 codex_models_match...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let workspace_guard = common::TestDir::new("ai3-codex-models");
    let workspace = workspace_guard.to_path_buf();

    // 我们的运行时（不起 thread 就不会加载 MCP 服务，桥也不需要）。
    let options = CodexStartOptions {
        install: codex_install.clone(),
        workspace: workspace.clone(),
        mcp: McpServerSpec::zssh(&mcp_exe, 1, "unused"),
        probes: codex::startup::command_probes(&codex_install.path),
        codex_home: None,
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };
    let runtime = CodexRuntime::start(options, DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 codex 共用进程");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv = runtime
        .open_conversation("modules".to_string(), tx)
        .await
        .expect("开对话");
    let catalog = conv.models().await.expect("取模型");

    // 裸 app-server：initialize + model/list（分页），独立实现一遍期望值。
    let raw = raw_model_list(&codex_install.path)
        .await
        .expect("裸协议取模型");
    let mut expected: Vec<String> = Vec::new();
    let mut expected_default: Option<String> = None;
    for entry in &raw {
        let id = entry["id"].as_str().unwrap_or("");
        if id.is_empty() || entry["hidden"].as_bool() == Some(true) {
            continue;
        }
        let efforts: Vec<&str> = entry["supportedReasoningEfforts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|e| e["reasoningEffort"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let is_default = entry["isDefault"].as_bool() == Some(true);
        let default_effort = entry["defaultReasoningEffort"].as_str();
        if efforts.is_empty() {
            expected.push(id.to_string());
            if is_default {
                expected_default = Some(id.to_string());
            }
        } else {
            for effort in &efforts {
                expected.push(format!("{id}/{effort}"));
                if is_default && Some(*effort) == default_effort {
                    expected_default = Some(format!("{id}/{effort}"));
                }
            }
        }
    }
    expected.sort();

    let mut ours: Vec<String> = catalog.models.iter().map(|m| m.id.clone()).collect();
    ours.sort();
    assert_eq!(ours, expected, "驱动的模型表要和裸协议一致");
    assert_eq!(catalog.default, expected_default, "默认模型要对上");
    println!(
        "模型对账：两边都是 {} 项，默认 {:?}（对账方式：同一 app-server 的 model/list，effort 展开在测试里独立实现）",
        ours.len(),
        catalog.default
    );

    // 名字抽查：显示的 name 都是 displayName 派生的（没有隐藏项混进来）。
    assert!(catalog.models.iter().all(|m| !m.name.is_empty()));

    conv.shutdown().await;
    runtime.shutdown().await;
}

/// 裸驱动一个 app-server 取 `model/list` 的原始条目（对账用）。
async fn raw_model_list(exe: &str) -> Option<Vec<serde_json::Value>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut cmd = discovery::build_command(std::path::Path::new(exe));
    cmd.args(["app-server", "--stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    discovery::hide_console(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut lines = BufReader::new(stdout).lines();

    let send = |value: serde_json::Value| {
        let mut text = value.to_string();
        text.push('\n');
        text
    };
    let init = send(json!({
        "id": 1, "method": "initialize",
        "params": { "clientInfo": { "name": "zssh-recon", "version": "0" }, "capabilities": {} }
    }));
    stdin.write_all(init.as_bytes()).await.ok()?;
    let mut got_init = false;
    let mut data: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut request_id = 2u64;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if got_init && cursor.is_none() && data.is_empty() {
            // 第一页还没发过。
            let list = send(json!({
                "id": request_id, "method": "model/list",
                "params": { "cursor": null, "limit": 100 }
            }));
            stdin.write_all(list.as_bytes()).await.ok()?;
            request_id += 1;
        }
        let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await
        else {
            break;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match value["id"].as_u64() {
            Some(1) => {
                got_init = true;
                // 初始化完成通知（可选）。
                let _ = stdin
                    .write_all(send(json!({ "method": "initialized", "params": {} })).as_bytes())
                    .await;
            }
            Some(id) if id >= 2 => {
                if let Some(items) = value["result"]["data"].as_array() {
                    data.extend(items.iter().cloned());
                }
                if let Some(next) = value["result"]["nextCursor"].as_str() {
                    let list = send(json!({
                        "id": request_id, "method": "model/list",
                        "params": { "cursor": next, "limit": 100 }
                    }));
                    stdin.write_all(list.as_bytes()).await.ok()?;
                    request_id += 1;
                    cursor = Some(next.to_string());
                } else {
                    let _ = child.kill().await;
                    return Some(data);
                }
            }
            _ => {}
        }
    }
    let _ = child.kill().await;
    None
}

/// MCP 启动核对（AI-3 验收 0.2）：config.toml 里用"点号键"新写法配一个假 MCP
/// 服务——段头扫描**扫不到**它（这正是要兜的洞）——运行时必须在任何回合开始前
/// 发现它、结束进程并报错。全程用独立的 `CODEX_HOME` 副本，不碰用户真实配置。
#[tokio::test]
async fn codex_refuses_mcp_servers_it_did_not_expect() {
    if !common::full_live("codex_refuses_mcp_servers_it_did_not_expect") {
        return;
    }
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 codex_refuses_mcp...：找不到 codex");
        return;
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 codex_refuses_mcp...：找不到 zssh-mcp 可执行文件");
        return;
    };
    // 独立的 CODEX_HOME 副本：只放一个"新写法"配置的假 MCP 服务（点号键，
    // 段头扫描扫不到）。
    let home_guard = common::TestDir::new("ai3-codexhome");
    let home = home_guard.to_path_buf();
    std::fs::write(
        home.join("config.toml"),
        "mcp_servers.fake_server.command = \"cmd.exe\"
         mcp_servers.fake_server.args = [\"/c\", \"ping -n 60 127.0.0.1 >nul\"]
",
    )
    .expect("写配置");
    let workspace_guard = common::TestDir::new("ai3-codexhome-workspace");
    let workspace = workspace_guard.to_path_buf();

    let options = CodexStartOptions {
        install: codex_install.clone(),
        workspace: workspace.clone(),
        mcp: McpServerSpec::zssh(&mcp_exe, 1, "unused"),
        probes: codex::startup::command_probes_with_home(&codex_install.path, Some(home.clone())),
        codex_home: Some(home.clone()),
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };
    let runtime = CodexRuntime::start(options, DEFAULT_IDLE_TIMEOUT)
        .await
        .expect("启动 codex 共用进程");
    let pid = runtime.pid();

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv = runtime
        .open_conversation("capability".to_string(), tx)
        .await
        .expect("开对话");

    // 进程必须在没有任何回合之前就被结束。
    let deadline = Instant::now() + Duration::from_secs(30);
    while !runtime.is_closed() {
        assert!(
            Instant::now() < deadline,
            "没有发现假 MCP 服务（进程还活着）"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // 回合被拒绝，报错里带服务名。
    let err = conv
        .send("你好", None)
        .await
        .expect_err("加载了别的 MCP 服务时必须拒绝回合");
    let message = err.to_string();
    assert!(
        message.contains("fake_server") && message.contains("已停止"),
        "报错要说清是哪个服务：{message}"
    );
    println!("MCP 核对：{message}");

    // 进程真的被结束。
    if let Some(pid) = pid {
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_alive(pid) {
            assert!(Instant::now() < deadline, "codex 进程没被结束（pid {pid}）");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        println!("进程收尾：codex {pid} 已被结束");
    }

    conv.shutdown().await;
    runtime.shutdown().await;
}

/// 登录判断（第 5 节）：`codex login status` 的解析（开发时实测已登录，只判断有没有，
/// 不读取也不打印任何凭据内容）。
#[tokio::test]
async fn codex_login_status_reads_the_cli() {
    if !common::full_live("codex_login_status_reads_the_cli") {
        return;
    }
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 codex_login_status...：找不到 codex");
        return;
    };
    let status = codex::startup::login_status_cli(std::path::Path::new(&codex_install.path)).await;
    match status {
        Some(login) => {
            println!("codex login status：logged_in = {}", login.logged_in);
            // 开发时实测是已登录的；真没登录的话给提示也不算错，只要求别 panic。
            if !login.logged_in {
                println!("（本机没登录话的提示：{:?}）", login.hint);
            }
        }
        None => println!("codex login status 跑不起来，跳过"),
    }
}

/// 空闲自动关闭（第 4 节）：所有对话空闲超过阈值后，共用的 app-server 自己退出。
///
/// 不调用模型，只起进程、开一个对话、不再活动，等空闲看护把进程关掉。
///
/// `ZSSH_AGENT_E2E=1` 门槛（R-2 0.3）：它会起真的 codex 进程，`cargo test --workspace`
/// 并行跑多个测试二进制时 app-server 偶尔起不来（AI-4 报告遗留 2）。门禁里不能有
/// 偶发失败的测试，所以和别的真实智能体用例一样加门槛。
#[tokio::test]
async fn codex_idle_timeout_shuts_the_shared_process_down() {
    if !common::full_live("codex_idle_timeout_shuts_the_shared_process_down") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 codex_idle_timeout...：设置 ZSSH_AGENT_E2E=1 才跑（会起真实 codex 进程）");
        return;
    }
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 codex_idle_timeout...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 codex_idle_timeout...：找不到 codex");
        return;
    };
    let workspace_guard = common::TestDir::new("ai3-codex-idle");
    let workspace = workspace_guard.to_path_buf();

    // 桥的宿主只占位（本测不执行命令）。
    let bridge = Arc::new(Bridge::new(
        Arc::new(UnusedHost),
        Arc::new(NullAudit),
        Arc::new(AllowAll),
        Arc::new(AutoMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));

    let options = CodexStartOptions {
        install: codex_install.clone(),
        workspace: workspace.clone(),
        mcp: McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token()),
        probes: codex::startup::command_probes(&codex_install.path),
        codex_home: None,
        startup_timeout: DEFAULT_STARTUP_TIMEOUT,
    };
    // 空闲阈值压到 2 秒，测试别等 10 分钟。
    let idle = Duration::from_secs(2);
    let runtime = CodexRuntime::start(options, idle)
        .await
        .expect("启动 codex 共用进程");
    let pid = runtime.pid().expect("进程号");
    let scope = bridge.create_scope();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv = runtime
        .open_conversation(scope.capability().to_string(), tx)
        .await
        .expect("开对话");
    assert!(process_alive(pid), "刚起的进程该活着");

    let deadline = Instant::now() + idle * 4 + Duration::from_secs(15);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "空闲超时没关进程（pid {pid}）");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("空闲 {} 秒后共用进程 {pid} 自动退出", idle.as_secs());
    assert!(runtime.is_closed(), "关进程后运行时要标记为已关闭");

    conv.shutdown().await;
    runtime.shutdown().await;
}

// ---------- 宿主 ----------

struct CodexHost {
    connection: Arc<Connection>,
    ring: SharedTerminalRing,
    terminal: Arc<TerminalState>,
    runs: Arc<TerminalRuns>,
    writer: Arc<tokio::sync::Mutex<ShellWriter>>,
}

impl CodexHost {
    async fn probe_shell_kind(&self) -> ShellKind {
        let command = build_shell_probe_command();
        match self
            .connection
            .exec_capture(&command, SHELL_PROBE_TIMEOUT, 64 * 1024)
            .await
        {
            Ok(capture) => parse_shell_probe_output(&capture.stdout).unwrap_or(ShellKind::Other),
            Err(_) => ShellKind::Other,
        }
    }
}

impl BridgeHost for CodexHost {
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
        let ring = Arc::clone(&self.ring);
        Box::pin(async move {
            if !known {
                return Err("测试宿主只认识一个会话".to_owned());
            }
            let lines = ring
                .lock()
                .map_err(|_| "环形缓冲锁不可用".to_owned())?
                .recent_lines(max_lines);
            Ok(lines)
        })
    }

    fn exec_placement<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
        let known = session_id == SESSION_ID;
        Box::pin(async move {
            if !known {
                return ExecPlacement::Background {
                    reason: "测试宿主只认识一个会话".to_string(),
                };
            }
            if self.terminal.shell_kind() == ShellKind::Unknown {
                let kind = self.probe_shell_kind().await;
                self.terminal.set_shell_kind(kind);
            }
            match self.terminal.try_acquire() {
                Ok(()) => ExecPlacement::Terminal,
                Err(reason) => ExecPlacement::Background { reason },
            }
        })
    }

    fn run_in_terminal<'a>(
        &'a self,
        request: TerminalExecRequest,
    ) -> BoxFuture<'a, Result<TerminalExecCapture, String>> {
        Box::pin(async move {
            let started = Instant::now();
            self.runs
                .register(&request.scope_id, SESSION_ID, &request.marker);
            {
                let writer = self.writer.lock().await;
                writer
                    .write(format!("{}\r", request.payload).as_bytes())
                    .await
                    .map_err(|err| err.to_string())?;
            }
            let deadline = Instant::now() + request.wait;
            loop {
                let raw = ring_text(&self.ring);
                if let Some(found) = find_end_marker(&raw, &request.marker) {
                    self.terminal.release();
                    self.runs.clear(&request.scope_id, &request.marker);
                    return Ok(TerminalExecCapture {
                        output: extract_output(&raw, &request.marker),
                        exit_code: Some(found.exit_code),
                        duration_ms: started.elapsed().as_millis() as u64,
                        timed_out: false,
                    });
                }
                if Instant::now() >= deadline {
                    self.terminal.release();
                    let ring = Arc::clone(&self.ring);
                    let runs = Arc::clone(&self.runs);
                    let scope_id = request.scope_id.clone();
                    let marker = request.marker.clone();
                    tokio::spawn(async move {
                        for _ in 0..600 {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            if find_end_marker(&ring_text(&ring), &marker).is_some() {
                                runs.clear(&scope_id, &marker);
                                return;
                            }
                        }
                    });
                    return Ok(TerminalExecCapture {
                        output: String::new(),
                        exit_code: None,
                        duration_ms: started.elapsed().as_millis() as u64,
                        timed_out: true,
                    });
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    }

    fn interrupt_terminal<'a>(&'a self, scope_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // 只有这个对话确实有一条 AI 命令在跑才发（和生产一致）。
            let Some(_run) = self.runs.running(scope_id) else {
                return;
            };
            let writer = self.writer.lock().await;
            let _ = writer.write(&[0x03]).await;
        })
    }
}

/// 空闲测试用的占位宿主（不执行命令）。
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

// ---------- 审批 / 模式 / 审计 ----------

struct AllowAll;

impl Approver for AllowAll {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        let _ = request;
        Box::pin(async { ApprovalDecision::Allowed })
    }
}

struct AutoMode;

impl ExecModeProvider for AutoMode {
    fn mode(&self) -> ExecMode {
        ExecMode::Auto
    }
}

struct NullAudit;

impl zssh_core::ai::bridge::AuditLog for NullAudit {
    fn record(&self, _entry: zssh_core::ai::bridge::AuditEntry) {}
}

struct TempAuditDb {
    path: PathBuf,
    /// 目录守卫放最后：库先关，目录最后删（含 WAL/SHM）。
    _dir: common::TestDir,
}

impl TempAuditDb {
    fn new() -> Self {
        let dir = common::TestDir::new("ai3-codex");
        Self {
            path: dir.join("audit.db"),
            _dir: dir,
        }
    }

    fn open(&self) -> Database {
        Database::open(&self.path).expect("建审计库")
    }
}

// ---------- 小工具 ----------

fn ring_text(ring: &SharedTerminalRing) -> String {
    ring.lock().map(|ring| ring.raw_text()).unwrap_or_default()
}

fn clear_ring(ring: &SharedTerminalRing) {
    if let Ok(mut ring) = ring.lock() {
        *ring = TerminalRing::new();
    }
}

async fn wait_for_ring(ring: &SharedTerminalRing, needle: &str, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if ring_text(ring).contains(needle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 发一条消息，把这一回合的事件收集成文字（正文 + 工具调用摘要）。
async fn run_turn(
    conversation: &Arc<zssh_core::ai::agent::codex::CodexConversation>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    text: &str,
    model: Option<&str>,
) -> String {
    conversation.send(text, model).await.expect("发送消息");
    let mut reply = String::new();
    let deadline = Instant::now() + Duration::from_secs(240);
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

/// 找 codex：`ZSSH_AGENT_CODEX` 优先，否则走通用发现（PATH → npm 全局；顺带实测拆包装）。
async fn find_codex_install() -> Option<AgentInstall> {
    if let Ok(path) = std::env::var("ZSSH_AGENT_CODEX") {
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
    let data_dir_guard = common::TestDir::new("ai3-codex-discovery");
    let data_dir = data_dir_guard.to_path_buf();
    match discovery::discover(&CODEX, None, &data_dir).await {
        Ok(Some(install)) if !install.too_old => Some(install),
        Ok(Some(install)) => {
            println!("找到的 codex 版本过低：{}", install.version);
            None
        }
        Ok(None) => None,
        Err(err) => {
            println!("codex 发现失败：{err}");
            None
        }
    }
}

/// R-2 第 3 节：用户配置文件坏掉的时候，用户自己的 MCP 服务依然不会被加载。
///
/// 起真的 codex（**不调模型**，只要 `ZSSH_AGENT_E2E=1`，因为要起真进程）：
/// 给一个坏掉的 `CODEX_HOME`——里面写着用户自己的 MCP 服务，但 `config.toml`
/// 读不出来（是个目录）或格式坏了。两种情况下都断言"用户的服务绝不被放行"：
/// 要么我们在启动前就拒绝（扫不出要禁用哪些服务时不启动 codex），要么 codex
/// 自己起不来，要么起来了但启动后核对会把它拦下来。
/// 这个用例要造的几种 `config.toml`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 读不出来（是个目录）。
    Unreadable,
    /// 读得到但格式坏了（扫不出名字）。
    Broken,
    /// 正常空配置（对照组）。
    Valid,
    /// 正常配置 + 一个用户自己的 MCP 服务。
    WithUserServer,
}

impl Mode {
    /// 对照组的启动通知应当看得见（用来确认断言不是"什么都没看见"）。
    fn control(self) -> bool {
        matches!(self, Mode::Valid | Mode::WithUserServer)
    }
}

#[tokio::test]
async fn broken_user_config_never_loads_the_users_mcp_server() {
    if !common::full_live("broken_user_config_never_loads_the_users_mcp_server") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 broken_user_config...：设置 ZSSH_AGENT_E2E=1 才跑（会起真实 codex 进程）");
        return;
    }
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 broken_user_config...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(codex_install) = find_codex_install().await else {
        println!("跳过 broken_user_config...：找不到 codex");
        return;
    };
    let bridge = Arc::new(Bridge::new(
        Arc::new(UnusedHost),
        Arc::new(NullAudit),
        Arc::new(AllowAll),
        Arc::new(AutoMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));

    // ③ 是**对照组**（正常的空配置）：确认"我们真的能看到服务启动通知"，
    //    否则前面两条的"没有别人的服务"说明不了问题。
    for (tag, mode) in [
        // ① config.toml 是个目录：读不出来 → 扫不出要禁用的服务 → 不启动。
        ("config.toml 是目录", Mode::Unreadable),
        // ② 格式坏了（段头没闭合）：读得到但扫不出名字 → 交给启动后核对兜底。
        ("格式坏了", Mode::Broken),
        // ③ 对照组：正常空配置，zssh 应当被看到。
        ("正常配置（对照组）", Mode::Valid),
        // ④ 正常配置 + 用户自己的服务（要禁用的那种）：只允许 zssh 被加载。
        ("用户有服务（对照组）", Mode::WithUserServer),
    ] {
        let home_guard = common::TestDir::new("ai3-broken-config");
        let home = home_guard.to_path_buf();
        let config = home.join("config.toml");
        match mode {
            Mode::Unreadable => {
                std::fs::create_dir_all(&config).expect("把 config.toml 造成目录");
            }
            Mode::Broken => {
                std::fs::write(
                    &config,
                    "[mcp_servers.my_mcp_server
command = '/should/not/exist.exe'
",
                )
                .expect("写坏配置");
            }
            Mode::Valid => {
                std::fs::write(
                    &config,
                    "model = \"gpt-x\"
",
                )
                .expect("写正常配置");
            }
            Mode::WithUserServer => {
                // 用户自己的服务（要禁用的那种）：启动参数里会 `enabled=false`。
                std::fs::write(
                    &config,
                    "model = \"gpt-x\"
[mcp_servers.my_mcp_server]
command = '/should/not/exist.exe'
",
                )
                .expect("写带用户服务的配置");
            }
        }

        let workspace_guard = common::TestDir::new("ai3-broken-ws");
        let workspace = workspace_guard.to_path_buf();
        let options = CodexStartOptions {
            install: codex_install.clone(),
            workspace: workspace.clone(),
            mcp: McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token()),
            probes: codex::startup::command_probes_with_home(
                &codex_install.path,
                Some(home.clone()),
            ),
            codex_home: Some(home.clone()),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        };
        match CodexRuntime::start(options, Duration::from_secs(600)).await {
            Err(err) => {
                println!("{tag}：启动被拒/起不来（失败关闭）——{err}");
            }
            Ok(runtime) => {
                // 起来了：直接问 codex 要完整的 MCP 服务列表（和放行回合前那次核对
                // 用同一个请求、同一套判据），不靠"启动通知"（那个要等回合才到齐）。
                let mut last: Option<(Vec<String>, Option<String>)> = None;
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    match runtime.mcp_status_snapshot().await {
                        Ok(snapshot) => {
                            let done = snapshot.1.is_some() || !snapshot.0.is_empty();
                            last = Some(snapshot);
                            if done || Instant::now() >= deadline {
                                break;
                            }
                        }
                        Err(err) => {
                            println!("{tag}：查 MCP 列表失败（按失败关闭算）：{err}");
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                if let Some((names, violation)) = last {
                    println!(
                        "{tag}：进程起来了，MCP 服务列表 {names:?}，核对结果 {}",
                        violation
                            .clone()
                            .unwrap_or_else(|| "干净（只有 zssh）".to_string())
                    );
                    assert!(
                        violation.is_none(),
                        "{tag}：用户自己的 MCP 服务被加载了（R-2 第 3 节）"
                    );
                    if mode.control() {
                        assert!(
                            names.iter().any(|name| name == "zssh"),
                            "对照组里应当看到 zssh（否则这个断言说明不了问题）：{names:?}"
                        );
                    }
                }
                runtime.shutdown().await;
            }
        }
    }
    println!("坏配置下用户自己的 MCP 服务没有被加载（三种收尾方式都算通过）");
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
    let request = json!({ "id": 1, "method": method, "params": params, "capability": capability });
    write_half
        .write_all(format!("{request}\n").as_bytes())
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
    let field = text.trim().rsplit("\",\"").next()?.trim_matches('"');
    let digits: String = field.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
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

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}
