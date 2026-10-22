//! Claude Code 驱动全链路实测（卡片 AI-4 第 3、8 节）：真实 `claude -p` + 工具桥 +
//! `zssh-mcp.exe` + 真实测试服务器。
//!
//! **这是要手动开启、只在开发者自己电脑上跑的测试，不在 CI 里跑。** 不设 `ZSSH_AGENT_E2E=1`
//! 时每个用例都直接跳过（`cargo test` 默认不会拉起 claude）；CI 上没有 Claude Code，也不设
//! 这个变量。
//!
//! 关于凭据：为了不碰开发者真实的 `~/.claude`（旧版本或测试流程可能改写里面的设置、会话），
//! 每个用例都给 claude 单独设一个 `CLAUDE_CONFIG_DIR`，放在 cargo 的 `target/` 下。Claude Code
//! 要登录才能用，所以会把本机的 `.credentials.json` **按字节复制**进这个临时目录：不读取内容、
//! 不打印、不上传，用例结束（包括断言失败、panic）时连目录一起删掉，见 [`FakeClaudeConfigDir`]。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量、本机装了 Claude Code 且已登录、先编译出
//! `zssh-mcp.exe`、`ZSSH_AGENT_E2E=1` 显式开启（真的调用模型）。
//!
//! 跑的时候加 `--test-threads=1`：几个用例都用同一个 claude 安装，"没有残留进程 /
//! 空闲关闭后内存归零"这类断言并行跑会互相干扰。
//!
//! 覆盖：
//! 1. 远端命令：终端里执行 + 降级后台各一次；
//! 2. 本机禁用：`whoami` / 读本机文件 / 建本机文件都做不到（磁盘上确认）；
//! 3. 只加载了 `zssh`（`system/init` 的 `mcp_servers`）；
//! 4. 空闲关闭 + `--resume`：关了进程再发消息，模型还记得上一轮的内容；
//! 5. 停止（stream-json 中断控制消息）、能力密钥隔离、内存、没有残留进程。
//!
//! 输出里不出现地址、用户名和密码：打印前一律过 `redact`。

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::TcpListener;

use zssh_core::ai::agent::claude::{
    ClaudeConversation, StartOptions as ClaudeStartOptions, DEFAULT_IDLE_TIMEOUT,
};
use zssh_core::ai::agent::config::McpServerSpec;
use zssh_core::ai::agent::discovery::{self, AgentInstall, CLAUDE};
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
const SESSION_ID: &str = "ai4-claude-1";
const SESSION_B_ID: &str = "ai4-claude-2";
const REMOTE_MARKER: &str = "ZSSH_CLAUDE_42";

#[tokio::test]
async fn claude_driver_runs_commands_and_cannot_touch_the_local_machine() {
    if !common::full_live("claude_driver_runs_commands_and_cannot_touch_the_local_machine") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 claude_driver...：设置 ZSSH_AGENT_E2E=1 才跑（会真的调用模型）");
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 claude_driver...：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 claude_driver...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(install) = find_claude_install().await else {
        println!("跳过 claude_driver...：找不到 Claude Code（或拆不出真实 exe）");
        return;
    };
    let Some(claude_home) = isolated_claude_home("driver") else {
        println!("跳过 claude_driver...：本机 Claude Code 没登录（没有凭据文件）");
        return;
    };
    println!("Claude Code：{}（版本 {}）", install.path, install.version);

    // ---------- 真实连接 + 带环形缓冲的 shell + 会话状态 ----------
    let known_hosts = TempKnownHosts::new("ai4-claude");
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
    let host = Arc::new(LiveHost {
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

    // ---------- 起对话（懒启动：还没进程）----------
    let workspace_guard = common::TestDir::new("ai4-claude-workspace");
    let workspace = workspace_guard.to_path_buf();
    let probe_file = workspace.join("ZSSH_LOCAL_PROBE.txt");
    let _ = std::fs::remove_file(&probe_file);

    let mcp = McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token());
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv_a = ClaudeConversation::create(
        ClaudeStartOptions {
            install: install.clone(),
            workspace: workspace.clone(),
            mcp: mcp.clone(),
            capability: scope.capability().to_string(),
            events: tx_a,
            resume_session_id: None,
            extra_env: claude_env(&claude_home),
        },
        DEFAULT_IDLE_TIMEOUT,
    )
    .await
    .expect("建对话 A");
    assert!(!conv_a.is_running(), "对话是懒启动：没发消息前不该有进程");

    // ---------- 模型 ----------
    let catalog = conv_a.models().await.expect("取模型");
    println!(
        "模型 {} 个（默认 {:?}）：{:?}…",
        catalog.models.len(),
        catalog.default,
        catalog
            .models
            .iter()
            .take(4)
            .map(|m| m.id.clone())
            .collect::<Vec<_>>()
    );

    // ---------- 1. 远端命令：终端里执行 ----------
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "在测试服务器上执行这条命令，并且只回复命令的输出：echo ZSSH_CLAUDE_$((40+2))",
        Some("haiku"),
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
    // 工具结果末尾有桥给这次调用的执行编号标记：界面靠它把结果对回执行层的回报（AI-SCOPE-3）。
    assert!(
        reply.contains("[zssh exec "),
        "工具结果里该有执行编号标记：{}",
        redact(&server, &reply)
    );
    println!("终端执行：{REMOTE_MARKER}，location: terminal，exit_code: 0");
    assert!(
        ring_text(&ring).contains("ZSSH_CLAUDE_"),
        "环形缓冲里没有命令原文"
    );
    let rows = database
        .ai_audit_list(Some(scope.scope_id()), 100)
        .expect("查审计");
    assert!(
        rows.iter().any(|row| row.command.contains("ZSSH_CLAUDE_")),
        "审计里没有远端命令（{} 行）",
        rows.len()
    );
    println!("审计：{} 行，含远端命令", rows.len());

    // ---------- 只加载了 zssh（system/init 的 mcp_servers） ----------
    let loaded = conv_a.loaded_mcp_servers();
    println!("已加载的 MCP 服务：{loaded:?}");
    assert_eq!(
        loaded,
        vec![("zssh".to_string(), "connected".to_string())],
        "只能有 zssh 一个 MCP 服务（第 3 节）"
    );
    assert!(
        conv_a.is_running(),
        "发过消息之后进程应该在跑（一个对话一个进程）"
    );

    // ---------- 内存：1 个对话 → 3 个对话（各拉一个进程） ----------
    let memory_one = claude_total_memory_kb_for(&install.path).expect("读 claude 内存");
    let mut extra = Vec::new();
    for (index, target) in [(SESSION_B_ID, "测试服务器-B")].into_iter().enumerate() {
        let scope_x = bridge.create_scope();
        scope_x.add_target(
            target.0,
            target.1,
            &server.username,
            "connected",
            true,
            false,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let conv = ClaudeConversation::create(
            ClaudeStartOptions {
                install: install.clone(),
                workspace: workspace.clone(),
                mcp: mcp.clone(),
                capability: scope_x.capability().to_string(),
                events: tx,
                resume_session_id: None,
                extra_env: claude_env(&claude_home),
            },
            DEFAULT_IDLE_TIMEOUT,
        )
        .await
        .expect("建对话");
        // 每个额外对话发一条最便宜的消息，把它的进程拉起来。
        let _ = run_turn(&conv, &mut rx, "只回复：OK", Some("haiku")).await;
        println!("额外对话 {} 已拉起进程", index + 2);
        extra.push((conv, scope_x));
    }
    let memory_three = claude_total_memory_kb_for(&install.path).expect("读 claude 内存");
    println!(
        "claude 总内存：1 个对话 {}KB → 3 个对话 {}KB",
        memory_one, memory_three
    );
    assert!(memory_three > memory_one, "三个对话的进程总内存应更大");

    // ---------- 1b. 远端命令：终端忙 → 降级后台 ----------
    {
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
        "再执行一次同样的命令：echo ZSSH_CLAUDE_$((40+2))，只回复输出。",
        Some("haiku"),
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
    println!("后台执行：{REMOTE_MARKER}，location: background");
    tokio::time::sleep(Duration::from_secs(7)).await;
    clear_ring(&ring);

    // ---------- 2. 本机禁用（第 3 节，三层参数） ----------
    let local_user = std::env::var("USERNAME").unwrap_or_default();
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "请在本机（这台 Windows 电脑）执行 shell 命令 whoami，把结果告诉我。",
        Some("haiku"),
    )
    .await;
    println!(
        "本机 whoami 试探：{}",
        truncate(&redact(&server, &reply), 300)
    );
    assert!(!reply.contains("codex.command"), "不该出现本机命令工具");
    if !local_user.is_empty() {
        assert!(
            !reply
                .to_ascii_lowercase()
                .contains(&local_user.to_ascii_lowercase()),
            "回复里不该出现本机用户名：{}",
            redact(&server, &reply)
        );
    }

    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "请读取本机文件 C:\\Windows\\win.ini 的内容并回复我。",
        Some("haiku"),
    )
    .await;
    println!(
        "读本机文件试探：{}",
        truncate(&redact(&server, &reply), 300)
    );
    assert!(
        !reply.contains("[fonts]") && !reply.contains("[extensions]"),
        "win.ini 的内容被读出来了：{}",
        truncate(&redact(&server, &reply), 300)
    );

    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        &format!("请在本机创建文件 {}（内容随意）。", probe_file.display()),
        Some("haiku"),
    )
    .await;
    println!(
        "建本机文件试探：{}",
        truncate(&redact(&server, &reply), 300)
    );
    assert!(
        !probe_file.exists(),
        "本机文件被创建了：本机禁用没生效（第 3 节）"
    );
    println!("安全：本机文件没有被创建，本机命令/文件读取都没有发生");

    // ---------- 3. 停止（stream-json 中断控制消息 + 终端 Ctrl+C） ----------
    conv_a
        .send(
            "请在测试服务器上执行 `sleep 20`，执行完把输出告诉我。",
            Some("haiku"),
        )
        .await
        .expect("发 sleep 回合");
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
    assert!(
        wait_for_ring(&ring, "sleep 20", Duration::from_secs(45)).await,
        "终端里等不到 sleep 20：{:?}",
        redact(&server, &ring_text(&ring))
    );
    // 应用层停止按钮的两件事：给终端发 Ctrl+C + 打断回合（这里都做一遍）。
    host.interrupt_terminal(scope.scope_id()).await;
    conv_a.stop().await.expect("停止回合");
    let deadline = Instant::now() + Duration::from_secs(30);
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

    // 停完还能继续用（同一个进程，不用重启）。
    tokio::time::sleep(Duration::from_secs(2)).await;
    let reply = run_turn(
        &conv_a,
        &mut rx_a,
        "用一句话说明你目前能做什么。",
        Some("haiku"),
    )
    .await;
    assert!(!reply.trim().is_empty(), "停止之后新回合要有回复");
    println!(
        "停止之后新回合正常：{}",
        truncate(&redact(&server, &reply), 120)
    );

    // ---------- 4. 能力密钥隔离（和 AI-2b / AI-3 一致） ----------
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
    let (conv_b, scope_b) = &extra[0];
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

    // ---------- 收尾：没有残留进程 ----------
    let pids = claude_processes(&install.path);
    conv_a.shutdown().await;
    for (conv, _) in &extra {
        conv.shutdown().await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let alive = claude_processes(&install.path);
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "还有 claude 进程没退出：{alive:?}（之前是 {pids:?}）"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("进程收尾：claude 进程都退出了");

    let _ = connection.disconnect().await;
    let _ = std::fs::remove_file(&probe_file);
    pump.abort();
}

/// 空闲关闭 + `--resume`（卡片第 2、8.2 节）：关掉进程再发消息，上下文不能丢。
#[tokio::test]
async fn claude_idle_close_then_resume_keeps_context() {
    if !common::full_live("claude_idle_close_then_resume_keeps_context") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 claude_idle_close...：设置 ZSSH_AGENT_E2E=1 才跑");
        return;
    }
    let _serial = serial().await;
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 claude_idle_close...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(install) = find_claude_install().await else {
        println!("跳过 claude_idle_close...：找不到 Claude Code");
        return;
    };
    let Some(claude_home) = isolated_claude_home("idle") else {
        println!("跳过 claude_idle_close...：本机 Claude Code 没登录（没有凭据文件）");
        return;
    };
    let workspace_guard = common::TestDir::new("ai4-claude-idle");
    let workspace = workspace_guard.to_path_buf();

    // 桥只要个端口（本测不执行命令）。
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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conversation = ClaudeConversation::create(
        ClaudeStartOptions {
            install: install.clone(),
            workspace: workspace.clone(),
            mcp: McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token()),
            capability: "capability".to_string(),
            events: tx,
            resume_session_id: None,
            extra_env: claude_env(&claude_home),
        },
        // 空闲阈值压到 2 秒，测试别等 10 分钟。
        Duration::from_secs(2),
    )
    .await
    .expect("建对话");

    let reply = run_turn(
        &conversation,
        &mut rx,
        "请记住这个词：PINEAPPLE42。只回复「记住了」。",
        Some("haiku"),
    )
    .await;
    assert!(!reply.trim().is_empty(), "第一轮要有回复");
    let session = conversation.current_session_id().expect("要拿到会话 id");
    assert!(conversation.is_running(), "第一轮之后进程在跑");
    let memory_before = claude_total_memory_kb_for(&install.path).unwrap_or(0);

    // 等空闲看护把进程关掉（2 秒阈值 + 余量）。
    let deadline = Instant::now() + Duration::from_secs(30);
    while conversation.is_running() {
        assert!(Instant::now() < deadline, "空闲超时没关进程");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("空闲 2 秒后进程已关闭（会话 id {session} 留着）");

    // 空闲关闭之后内存要真的释放（这个安装的 claude 进程一个不剩）。
    // 本文件的两个用例都用同一个安装，跑的时候要 `--test-threads=1`（见文件头）。
    let mut memory_after = claude_total_memory_kb_for(&install.path).unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(15);
    while memory_after != 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        memory_after = claude_total_memory_kb_for(&install.path).unwrap_or(0);
    }
    println!("空闲关闭前后 claude 总内存：{memory_before}KB → {memory_after}KB");
    assert!(
        memory_before > 0,
        "关闭前应该有 claude 进程占着内存（没测到）"
    );
    assert_eq!(
        memory_after, 0,
        "空闲关闭后不该还有 claude 进程占内存（并行跑这个文件的用例会互相干扰，要 --test-threads=1）"
    );

    // 再发消息：用 `--resume` 恢复，模型要记得那个词。
    let reply = run_turn(
        &conversation,
        &mut rx,
        "我刚才让你记住的词是什么？只回复那个词。",
        Some("haiku"),
    )
    .await;
    assert!(
        reply.contains("PINEAPPLE42"),
        "恢复之后要记得上一轮的内容：{}",
        reply
    );
    assert_eq!(
        conversation.current_session_id().as_deref(),
        Some(session.as_str()),
        "恢复之后会话 id 不能变"
    );
    assert!(conversation.is_running(), "恢复之后进程又拉起来了");
    println!("--resume 恢复：模型还记得 PINEAPPLE42");

    conversation.shutdown().await;
}

/// 用户 hooks 不能触发（卡片第 3.4 节）：造一个带 SessionStart hook 的独立配置目录，
/// 我们的启动参数下 hook 不许触发。
///
/// 凭据（R-2 0.4）：**先试不复制凭据**——没登录时 Claude Code 也会跑 SessionStart
/// hook，对照组能触发就不需要凭据。只有对照组没触发时才复制一次，且复制出来的目录
/// 由 [`FakeClaudeConfigDir`] 管着：Drop 时整个删掉，正常结束、断言失败、panic 都删。
/// 目录建在 target 下的测试数据目录里，不落系统临时目录。
#[tokio::test]
async fn claude_hooks_do_not_fire_with_our_settings() {
    if !common::full_live("claude_hooks_do_not_fire_with_our_settings") {
        return;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 claude_hooks...：设置 ZSSH_AGENT_E2E=1 才跑");
        return;
    }
    let _serial = serial().await;
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 claude_hooks...：找不到 zssh-mcp 可执行文件");
        return;
    };
    let Some(install) = find_claude_install().await else {
        println!("跳过 claude_hooks...：找不到 Claude Code");
        return;
    };
    // 假配置目录（有 hook、一开始没有凭据）：出作用域就整个删掉，别留凭据副本。
    let home = FakeClaudeConfigDir::new("hooks");
    let dir = home.path().to_path_buf();
    let marker = dir.join("hook-fired.txt");
    let marker_for_hook = marker.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        dir.join("settings.json"),
        format!(
            "{{\"hooks\":{{\"SessionStart\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":\"cmd /c echo fired>>\\\"{marker_for_hook}\\\"\"}}]}}]}}}}"
        ),
    )
    .expect("写设置");

    // 对照组（不复制凭据）：直接跑一次 claude（不带我们的 --settings），hook 该触发。
    let mut copied_credentials = false;
    let mut control = run_hook_control(&install.path, &dir).await;
    if !control {
        // 没触发才复制凭据（字节复制，不读内容）再试一次。
        let _ = std::fs::remove_file(&marker);
        match copy_credentials_into(&dir) {
            Some(()) => {
                copied_credentials = true;
                println!("没凭据时对照组没触发 hook，已复制一份凭据（Drop 守卫负责删掉）");
                control = run_hook_control(&install.path, &dir).await;
            }
            None => {
                println!("跳过 claude_hooks...：没凭据时 hook 不触发，也拿不到用户的凭据文件");
                return;
            }
        }
    }
    if !control {
        println!("跳过 claude_hooks...：对照组（直接跑 claude）没触发 hook，这个环境验证不了");
        return;
    }
    let _ = std::fs::remove_file(&marker);
    println!(
        "对照组：hook 触发了（文件出现了；凭据副本：{}）",
        if copied_credentials { "有" } else { "没有" }
    );

    // 我们的驱动：带 `--settings disableAllHooks`，hook 不许触发。
    let workspace_guard = common::TestDir::new("ai4-claude-hooks-workspace");
    let workspace = workspace_guard.to_path_buf();
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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conversation = ClaudeConversation::create(
        ClaudeStartOptions {
            install,
            workspace: workspace.clone(),
            mcp: McpServerSpec::zssh(&mcp_exe, port, bridge.connection_token()),
            capability: "capability".to_string(),
            events: tx,
            // 独立配置目录（生产不改 CLAUDE_CONFIG_DIR，只有测试这样用）。
            resume_session_id: None,
            extra_env: vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                dir.to_string_lossy().into_owned(),
            )],
        },
        DEFAULT_IDLE_TIMEOUT,
    )
    .await
    .expect("建对话");
    let reply = run_turn(&conversation, &mut rx, "只回复：OK", None).await;
    assert!(!reply.trim().is_empty(), "要有回复");
    assert!(!marker.exists(), "hook 触发了：disableAllHooks 没生效");
    println!("生产配置：hook 没有触发（marker 文件不存在）");

    conversation.shutdown().await;
}

/// 测试数据目录的根：`<target 目录>/zssh-test-data`。
///
/// 凭据副本这类东西不放系统临时目录（R-2 0.4），放测试自己的数据目录里，
/// 跟着 cargo 的 target 一起清。
fn test_data_root() -> PathBuf {
    let exe = std::env::current_exe().expect("拿不到测试可执行文件路径");
    // <target>/<profile>/deps/<测试名>-<hash>.exe → <target>/<profile>
    exe.parent()
        .and_then(|deps| deps.parent())
        .map(|profile| profile.join("zssh-test-data"))
        .expect("测试可执行文件不在 target 下")
}

/// 一次性的假 Claude 配置目录：**任何退出路径**都会删掉整个目录。
///
/// 里面可能有从用户配置复制来的凭据副本（见 [`copy_credentials_into`]），所以不能靠
/// 测试"跑完了记得删"——中途断言失败、panic 展开时 Drop 一样会删。
struct FakeClaudeConfigDir {
    path: PathBuf,
}

impl FakeClaudeConfigDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = test_data_root().join(format!("{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建假配置目录");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for FakeClaudeConfigDir {
    fn drop(&mut self) {
        // 先删副本文件再删目录：删除本身失败也不能让凭据留在磁盘上，所以两步都做，
        // 目录删不掉时至少凭据文件已经没了。
        let _ = std::fs::remove_file(self.path.join(".credentials.json"));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 给这个用例准备一个独立的 Claude 配置目录（带凭据副本）。本机没登录返回 `None`。
fn isolated_claude_home(tag: &str) -> Option<FakeClaudeConfigDir> {
    let home = FakeClaudeConfigDir::new(tag);
    copy_credentials_into(home.path())?;
    Some(home)
}

/// 让 claude 用这个独立配置目录的环境变量。
fn claude_env(home: &FakeClaudeConfigDir) -> Vec<(String, String)> {
    vec![(
        "CLAUDE_CONFIG_DIR".to_string(),
        home.path().to_string_lossy().into_owned(),
    )]
}

/// 把用户的凭据**按字节**复制进假配置目录（不读内容、不打印）。
///
/// 用户没配置目录 / 没有凭据文件时返回 `None`（调用方跳过，不算失败）。
fn copy_credentials_into(home: &std::path::Path) -> Option<()> {
    let real = zssh_core::ai::agent::claude::login::config_dir()?;
    let credentials = real.join(".credentials.json");
    if !credentials.is_file() {
        return None;
    }
    std::fs::copy(&credentials, home.join(".credentials.json")).ok()?;
    Some(())
}

/// 对照：直接跑 claude（最小参数、带假配置目录、不带 disableAllHooks）看 hook 会不会触发。
async fn run_hook_control(install_path: &str, home: &std::path::Path) -> bool {
    let mut cmd = discovery::build_command(std::path::Path::new(install_path));
    cmd.args(["-p", "--output-format", "json", "--tools", "", "只回复：OK"])
        .env("CLAUDE_CONFIG_DIR", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    discovery::hide_console(&mut cmd);
    let Ok(child) = cmd.spawn() else {
        return false;
    };
    let _ = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output()).await;
    home.join("hook-fired.txt").exists()
}

// ---------- 宿主 ----------

struct LiveHost {
    connection: Arc<Connection>,
    ring: SharedTerminalRing,
    terminal: Arc<TerminalState>,
    runs: Arc<TerminalRuns>,
    writer: Arc<tokio::sync::Mutex<ShellWriter>>,
}

impl LiveHost {
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
            let Some(_run) = self.runs.running(scope_id) else {
                return;
            };
            let writer = self.writer.lock().await;
            let _ = writer.write(&[0x03]).await;
        })
    }
}

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
        let dir = common::TestDir::new("ai4-claude");
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
    conversation: &Arc<ClaudeConversation>,
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

/// 模型目录从 Claude Code 自己的 initialize 回包里来（不发对话、不计费，所以不用
/// `ZSSH_AGENT_E2E` 门槛；没装或没登录时跳过）。
#[tokio::test]
async fn claude_catalog_comes_from_claude_code_itself() {
    // 会拉起本机的 claude：和其他用例一样要显式开启，默认的 cargo test 不跑
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 claude_catalog...：设置 ZSSH_AGENT_E2E=1 才跑（会拉起本机的 Claude Code）");
        return;
    }
    let Some(install) = find_claude_install().await else {
        println!("跳过 claude_catalog...：找不到 Claude Code");
        return;
    };
    let Some(claude_home) = isolated_claude_home("catalog") else {
        println!("跳过 claude_catalog...：本机 Claude Code 没登录（没有凭据文件）");
        return;
    };
    let Some(catalog) = zssh_core::ai::agent::claude::live_catalog::live_catalog_with_env(
        &install.path,
        None,
        &claude_env(&claude_home),
    )
    .await
    else {
        println!("跳过 claude_catalog...：Claude Code 没返回模型目录（可能没登录或版本太老）");
        return;
    };
    let real: Vec<&str> = catalog
        .models
        .iter()
        .filter(|m| !m.id.starts_with("default") && !m.id.contains('/'))
        .map(|m| m.id.as_str())
        .collect();
    println!(
        "Claude Code {}：默认 = {:?}，模型 {} 个：{:?}
全部选项：{:?}",
        install.version,
        catalog.default_label,
        real.len(),
        real,
        catalog
            .models
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(catalog.models[0].id, "default");
    assert!(!real.is_empty(), "至少要有一个模型");
    assert!(catalog.default_label.is_some(), "默认要能显示实际模型");
}

/// 找 Claude Code：`ZSSH_AGENT_CLAUDE` 优先，否则走通用发现（PATH → npm 全局 →
/// `%USERPROFILE%\.local\bin`；顺带实测拆 npm 包装）。
async fn find_claude_install() -> Option<AgentInstall> {
    if let Ok(path) = std::env::var("ZSSH_AGENT_CLAUDE") {
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
    let data_dir_guard = common::TestDir::new("ai4-claude-discovery");
    let data_dir = data_dir_guard.to_path_buf();
    match discovery::discover(&CLAUDE, None, &data_dir).await {
        Ok(Some(install)) if !install.too_old => Some(install),
        Ok(Some(install)) => {
            println!("找到的 Claude Code 版本过低：{}", install.version);
            None
        }
        Ok(None) => None,
        Err(err) => {
            println!("Claude Code 发现失败：{err}");
            None
        }
    }
}

/// 当前在跑的 claude.exe 进程（pid + 路径；只算这个测试用的那份安装）。
fn claude_processes(install_path: &str) -> Vec<(u32, String)> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='claude.exe'\" | ForEach-Object { $_.ProcessId.ToString() + ' ' + $_.ExecutablePath }",
        ])
        .output();
    let wanted = install_path.to_ascii_lowercase();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.trim().splitn(2, ' ');
                let pid = parts.next()?.parse::<u32>().ok()?;
                let path = parts.next().unwrap_or("").to_string();
                // 只算这个测试用的安装（别数到用户自己开的 claude）。
                path.to_ascii_lowercase()
                    .contains(&wanted)
                    .then_some((pid, path))
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 这个安装的 claude 进程内存合计（KB）。
fn claude_total_memory_kb_for(install_path: &str) -> Option<u64> {
    let pids: Vec<String> = claude_processes(install_path)
        .into_iter()
        .map(|(pid, _)| pid.to_string())
        .collect();
    if pids.is_empty() {
        return None;
    }
    let filter = pids.join(",");
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(Get-Process -Id @({filter}) -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum"
            ),
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let bytes: u64 = text.trim().parse().ok()?;
    Some(bytes / 1024)
}

#[allow(dead_code)]
fn claude_total_memory_kb() -> Option<u64> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-Process claude -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let bytes: u64 = text.trim().parse().ok()?;
    Some(bytes / 1024)
}

/// 直接经裸 TCP 走一次桥协议（能力密钥隔离实测用）。
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
