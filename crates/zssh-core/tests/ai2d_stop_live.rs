//! AI-2d 1.4 端到端：停止按钮真的向终端发 Ctrl+C（而且只在有 AI 命令在跑时发）。
//!
//! 链路：MCP 客户端 → `zssh-mcp.exe` → 工具桥 → 宿主（真实 SSH 连接 + 真实交互
//! shell + 环形缓冲）。停止入口调的是 `Bridge::interrupt_terminal(scope_id)`——
//! 面板停止按钮 `ai_agent_abort` 调的就是这个函数；测试里不自己写 `0x03`。
//!
//! 两条断言：
//! 1. 有一条 AI 命令正在终端里跑时，停止入口让命令被打断（结束标记出现、工具很快
//!    返回），之后终端回到提示符还能继续用；
//! 2. **没有** AI 命令在跑时，停止入口什么都不发：用户自己正在跑的 `sleep` 照常
//!    跑完，输出里没有 `^C`。
//!
//! 环境变量 `ZSSH_TEST_*` 不全时跳过并打印原因；找不到 `zssh-mcp.exe` 也跳过。
//! 输出里不出现地址和用户名（打印前一律过 `redact`）。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::process::Command;

use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, AuditEntry, AuditLog, Bridge, BridgeHost,
    ExecMode, ExecModeProvider, ExecPlacement, TerminalExecCapture, TerminalExecRequest,
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

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;
/// 桥和宿主里都只认识这一个会话。
const SESSION_ID: &str = "stop-1";

#[tokio::test]
async fn stop_sends_ctrl_c_only_when_an_ai_command_is_running() {
    if !common::full_live("stop_sends_ctrl_c_only_when_an_ai_command_is_running") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 stop_sends_ctrl_c_only_when_an_ai_command_is_running：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 stop_sends_ctrl_c_only_when_an_ai_command_is_running：找不到 zssh-mcp.exe");
        return;
    };

    // ---------- 真实连接 + shell + 环形缓冲 ----------
    let known_hosts = TempKnownHosts::new("ai2d-stop");
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
    // 会话状态（判断能不能就地执行）：生产里由会话任务在每段输出上旁路 feed，
    // 这里由事件泵代劳。
    let terminal = Arc::new(TerminalState::new());
    // 事件泵：ack 流控（不然暂停之后环形缓冲不再更新）+ 喂终端状态。
    let pump_terminal = Arc::clone(&terminal);
    let pump = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let ShellEvent::Data(bytes) = event {
                pump_terminal.feed(&bytes);
                flow.on_ack(bytes.len());
            }
        }
    });
    // 提示符擦干净（输出里不带主机名）。
    {
        let writer = writer.lock().await;
        writer.write(b"PS1='$ '\r").await.expect("设置提示符失败");
    }
    assert!(
        wait_for_ring(&ring, "$ ", Duration::from_secs(15)).await,
        "提示符没出现：{:?}",
        redact(&server, &ring_text(&ring))
    );
    clear_ring(&ring);

    // ---------- 桥 + 宿主 ----------
    let host = Arc::new(StopHost {
        connection: Arc::clone(&connection),
        ring: Arc::clone(&ring),
        terminal: Arc::clone(&terminal),
        runs: Arc::new(TerminalRuns::default()),
        writer: Arc::clone(&writer),
    });
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        Arc::new(NullAudit),
        Arc::new(AllowAll),
        Arc::new(AutoMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));
    let scope = bridge.create_scope();
    scope.add_target(SESSION_ID, "测试服务器", "root", "connected", true, false);
    let scope_id = scope.scope_id().to_string();
    let mcp = start_mcp(&mcp_exe, port, bridge.connection_token(), &scope).await;
    println!("工具桥端到端：桥端口 {port}，MCP 子进程已连上");

    // ---------- 1. 有 AI 命令在跑：停止入口发 Ctrl+C ----------
    let capability = scope.capability().to_string();
    let service = mcp.service;
    let call = tokio::spawn(async move {
        let mut args = serde_json::Map::new();
        args.insert("target_id".to_string(), json!(SESSION_ID));
        args.insert("command".to_string(), json!("sleep 30"));
        args.insert("timeout_sec".to_string(), json!(30));
        args.insert("capability".to_string(), json!(capability));
        service
            .call_tool(CallToolRequestParams::new("run_command").with_arguments(args))
            .await
    });

    // 等命令真的写进终端：登记表里有记录，回显里出现这次执行的标记前缀
    // （标记是随机的，环形缓冲里不会撞上以前的）。
    let marker = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(run) = host.runs.running(&scope_id) {
                break run.marker;
            }
            assert!(Instant::now() < deadline, "AI 命令一直没有登记到终端");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let prefix: String = marker.chars().take(16).collect();
    assert!(
        wait_for_ring(&ring, &prefix, Duration::from_secs(10)).await,
        "终端里没看到包装脚本的回显（标记 {prefix}…）"
    );
    // 还没结束（回显里那份 `_E:` 后面是引号不是数字，不算数）。
    let raw = ring_text(&ring);
    assert!(
        find_end_marker(&raw, &marker).is_none(),
        "停止之前命令应该还在跑"
    );
    let started = Instant::now();

    // 停止：调的就是面板停止按钮走到的那个函数。
    bridge.interrupt_terminal(&scope_id).await;

    // 工具很快返回：命令被打断（结束标记出现），不是等到 30s 超时。
    let result = tokio::time::timeout(Duration::from_secs(10), call).await;
    let result = result
        .expect("打断之后工具应该很快返回")
        .expect("工具调用任务不该崩")
        .expect("工具调用要走通协议");
    let text = tool_text(&result);
    let elapsed = started.elapsed();
    // 打断之后终端里是 `^C`，包装脚本的 trap 让结束标记照常打出来、退出码 130。
    assert!(
        ring_text(&ring).contains("^C"),
        "终端里要能看到 ^C：{}",
        redact(&server, &ring_text(&ring))
    );
    assert!(
        text.contains("exit_code: 130"),
        "被打断的命令退出码应该是 130：{}",
        redact(&server, &text)
    );
    assert!(elapsed < Duration::from_secs(10), "应该很快返回");
    assert!(
        !text.contains("still running in the terminal"),
        "打断之后不该按超时返回：{}",
        redact(&server, &text)
    );
    // 记录清掉：命令已经结束。
    assert!(
        host.runs.running(&scope_id).is_none(),
        "命令结束后登记表要清掉"
    );

    // 终端回到提示符，还能继续跑命令（真正的"打断"而不是把 shell 弄死）。
    let out = run_in_shell(&writer, &ring, "echo ZSSH_STOP_OK", "ZSSH_STOP_OK").await;
    assert!(
        out.contains("ZSSH_STOP_OK"),
        "打断之后终端要能继续用：{}",
        redact(&server, &out)
    );
    println!("停止（有命令在跑）：Ctrl+C 送达，命令被打断，终端回到提示符");

    // ---------- 2. 没有 AI 命令在跑：停止入口什么都不发 ----------
    // 上面那次打断留了一个 `^C` 在缓冲里，先清干净再验"这次没有 ^C"。
    clear_ring(&ring);
    // 用户自己正在终端里跑的命令：如果乱发了 Ctrl+C，`sleep` 会被打断、后面的
    // echo 不会执行（bash 里 Ctrl+C 中断整条列表）。
    {
        let writer = writer.lock().await;
        writer
            .write(b"sleep 5; echo ZSSH_USER_SURVIVED\r")
            .await
            .expect("写用户命令失败");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        host.runs.running(&scope_id).is_none(),
        "这会儿没有 AI 命令在跑"
    );
    bridge.interrupt_terminal(&scope_id).await;
    assert!(
        wait_for_ring(&ring, "ZSSH_USER_SURVIVED", Duration::from_secs(15)).await,
        "用户的命令应该照常跑完：{}",
        redact(&server, &ring_text(&ring))
    );
    let raw = ring_text(&ring);
    assert!(
        !raw.contains("^C"),
        "没有 AI 命令在跑时不该发 Ctrl+C：{}",
        redact(&server, &raw)
    );
    println!("停止（没有命令在跑）：什么都没发，用户的 sleep 照常跑完");

    // ---------- 收尾 ----------
    pump.abort();
    let writer = writer.lock().await;
    let _ = writer.close().await;
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn timeout_then_natural_finish_keeps_the_terminal_usable() {
    if !common::full_live("timeout_then_natural_finish_keeps_the_terminal_usable") {
        return;
    }
    // AI-2d 3.3：`sleep 5` 加 2 秒超时 → 返回"仍在运行"；命令还在跑时下一条 AI 命令
    // 自动降级到后台；等 sleep 自然结束以后，再下一条 AI 命令能在终端里正常执行，
    // 输出和退出码都对，不会被上一条迟到的结束标记干扰。
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 timeout_then_natural_finish_keeps_the_terminal_usable：{reason}");
            return;
        }
    };
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!("跳过 timeout_then_natural_finish_keeps_the_terminal_usable：找不到 zssh-mcp.exe");
        return;
    };

    let known_hosts = TempKnownHosts::new("ai2d-timeout");
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
        let writer = writer.lock().await;
        writer.write(b"PS1='$ '\r").await.expect("设置提示符失败");
    }
    assert!(
        wait_for_ring(&ring, "$ ", Duration::from_secs(15)).await,
        "提示符没出现：{:?}",
        redact(&server, &ring_text(&ring))
    );
    clear_ring(&ring);

    let runs = Arc::new(TerminalRuns::default());
    let host = Arc::new(StopHost {
        connection: Arc::clone(&connection),
        ring: Arc::clone(&ring),
        terminal: Arc::clone(&terminal),
        runs: Arc::clone(&runs),
        writer: Arc::clone(&writer),
    });
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        Arc::new(NullAudit),
        Arc::new(AllowAll),
        Arc::new(AutoMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));
    let scope = bridge.create_scope();
    scope.add_target(SESSION_ID, "测试服务器", "root", "connected", true, false);
    let scope_id = scope.scope_id().to_string();
    let mcp = start_mcp(&mcp_exe, port, bridge.connection_token(), &scope).await;

    // ---------- 1. sleep 5 + 2 秒超时：返回"仍在运行" ----------
    let text = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "sleep 5", "timeout_sec": 2 })
            .as_object()
            .cloned()
            .expect("参数"),
    )
    .await;
    println!("超时返回：{}", redact(&server, &text));
    assert!(
        text.contains("location: terminal"),
        "这次应该在终端里跑：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("still running in the terminal"),
        "超时要如实返回仍在运行：{}",
        redact(&server, &text)
    );
    assert!(
        runs.running(&scope_id).is_some(),
        "命令还在跑，登记表的记录要留着"
    );

    // ---------- 2. 命令还在跑：下一条 AI 命令降级到后台 ----------
    let text = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "echo ZSSH_BG_$((40+2))", "timeout_sec": 10 })
            .as_object()
            .cloned()
            .expect("参数"),
    )
    .await;
    println!("降级到后台：{}", redact(&server, &text));
    assert!(
        text.contains("location: background"),
        "终端忙时要降级到后台：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("ran in the background because:"),
        "要给出降级原因：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("ZSSH_BG_42") && text.contains("exit_code: 0"),
        "后台通道要真的跑出结果：{}",
        redact(&server, &text)
    );

    // ---------- 3. sleep 自然结束，终端空闲，再下一条命令照常在终端里跑 ----------
    let deadline = Instant::now() + Duration::from_secs(15);
    while runs.running(&scope_id).is_some() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        runs.running(&scope_id).is_none(),
        "sleep 结束后迟到的结束标记要清掉记录"
    );
    // 等提示符回来（sleep 的包装脚本结束后 shell 会重新打印提示符）。
    assert!(
        wait_for_ring(&ring, "$ ", Duration::from_secs(10)).await,
        "终端要回到提示符：{:?}",
        redact(&server, &ring_text(&ring))
    );

    let text = call_tool(
        &mcp,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "echo ZSSH_AFTER_$((40+2))", "timeout_sec": 15 })
            .as_object()
            .cloned()
            .expect("参数"),
    )
    .await;
    println!("自然结束之后再执行：{}", redact(&server, &text));
    assert!(
        text.contains("location: terminal"),
        "上一条结束之后要回到终端里执行：{}",
        redact(&server, &text)
    );
    assert!(
        text.contains("ZSSH_AFTER_42") && text.contains("exit_code: 0"),
        "输出和退出码都要对（不能被上一条迟到的标记干扰）：{}",
        redact(&server, &text)
    );

    // ---------- 收尾 ----------
    pump.abort();
    let writer = writer.lock().await;
    let _ = writer.close().await;
    let _ = connection.disconnect().await;
}

// ---------- 宿主（和 AppBridgeHost 同一套 core 调用） ----------

struct StopHost {
    connection: Arc<Connection>,
    ring: SharedTerminalRing,
    terminal: Arc<TerminalState>,
    runs: Arc<TerminalRuns>,
    writer: Arc<tokio::sync::Mutex<ShellWriter>>,
}

impl StopHost {
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

impl BridgeHost for StopHost {
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
            // 和生产一样：后台通道就是这条连接上另开一个 exec 通道。
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
            // 和 AppBridgeHost::exec_placement 同一个顺序：先探测 shell 类型，再抢占。
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
            // 等结束标记（和 tauri 的 wait_for_marker 一样看环形缓冲的原文）。
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
                    // 超时：命令还在跑，记录留着；起一个观察任务等迟到的结束标记
                    // 把它清掉（等价于 src-tauri 的 watch_late_finish）。
                    let ring = Arc::clone(&self.ring);
                    let runs = Arc::clone(&self.runs);
                    let scope_id = request.scope_id.clone();
                    let marker = request.marker.clone();
                    tokio::spawn(async move {
                        for _ in 0..600 {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            let raw = ring_text(&ring);
                            if find_end_marker(&raw, &marker).is_some() {
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
            let Some(run) = self.runs.running(scope_id) else {
                return;
            };
            let writer = self.writer.lock().await;
            let _ = writer.write(&[0x03]).await;
            let _ = run;
        })
    }
}

// ---------- 审批 / 模式 / 审计（都走最简单的实现） ----------

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

impl AuditLog for NullAudit {
    fn record(&self, entry: AuditEntry) {
        let _ = entry;
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
        // 必须是异步睡眠：`#[tokio::test]` 是单线程运行时，阻塞会让读任务 /
        // 转发任务 / 事件泵全都跑不动（环形缓冲永远不更新）。
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 在 shell 里跑一条普通命令（用户手敲的形态），等输出里出现标记。
async fn run_in_shell(
    writer: &Arc<tokio::sync::Mutex<ShellWriter>>,
    ring: &SharedTerminalRing,
    command: &str,
    expect: &str,
) -> String {
    {
        let writer = writer.lock().await;
        writer
            .write(format!("{command}\r").as_bytes())
            .await
            .expect("写命令失败");
    }
    assert!(
        wait_for_ring(ring, expect, Duration::from_secs(15)).await,
        "等不到输出 {expect}"
    );
    ring_text(ring)
}

// ---------- MCP 客户端 ----------

type McpService = RunningService<RoleClient, ()>;

struct McpClient {
    service: McpService,
    capability: String,
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

async fn start_mcp(
    exe: &Path,
    port: u16,
    connection_token: &str,
    scope: &zssh_core::ai::bridge::ScopeHandle,
) -> McpClient {
    let mut command = Command::new(exe);
    command.env("ZSSH_BRIDGE_PORT", port.to_string());
    command.env("ZSSH_BRIDGE_TOKEN", connection_token);
    let transport = TokioChildProcess::new(command).expect("起 zssh-mcp 子进程");
    let service = ().serve(transport).await.expect("MCP 握手");
    McpClient {
        service,
        capability: scope.capability().to_string(),
    }
}

async fn call_tool(
    client: &McpClient,
    name: &str,
    mut arguments: serde_json::Map<String, serde_json::Value>,
) -> String {
    arguments.insert(
        "capability".to_string(),
        serde_json::Value::String(client.capability.clone()),
    );
    let result = client
        .service
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await
        .expect("工具调用要走通协议");
    tool_text(&result)
}

fn tool_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            rmcp::model::ContentBlock::Text(text) => text.text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 打印/断言消息里不能出现地址、用户名。
fn redact(server: &TestServer, text: &str) -> String {
    server.sanitize(text).replace(&server.username, "用户")
}
