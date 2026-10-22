//! 工具桥并发验收（R-8 第 10 节）：一条 `zssh-mcp` 连接上，一个调用卡在审批时，
//! 另一个调用必须能正常返回；审批放行后第一个调用也要能收到自己的结果。
//!
//! 链路：rmcp 客户端 → 真实 `zssh-mcp.exe` 子进程（stdio）→ 进程内工具桥（TCP）。
//! 不需要测试服务器：宿主的 exec 是桩，审批是可用信号量控制放行的桩。
//!
//! 前置条件：先编译出 `zssh-mcp.exe`（`cargo build -p zssh-mcp`），找不到就跳过。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Semaphore;

use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, AuditEntry, AuditLog, Bridge, BridgeHost,
    ExecMode, ExecModeProvider, ExecPlacement,
};
use zssh_core::ssh::{BoxFuture, ExecCapture};

/// 桥和宿主里都只认识这一个会话。
const SESSION_ID: &str = "conc-1";
/// run_command 在审批放行后由桩宿主返回的输出。
const COMMAND_OUTPUT: &str = "gate-ok";

type McpService = RunningService<RoleClient, ()>;

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

async fn start_mcp(exe: &std::path::Path, port: u16, connection_token: &str) -> McpService {
    let mut command = Command::new(exe);
    command.env("ZSSH_BRIDGE_PORT", port.to_string());
    command.env("ZSSH_BRIDGE_TOKEN", connection_token);
    let transport = TokioChildProcess::new(command).expect("起 zssh-mcp 子进程");
    ().serve(transport).await.expect("MCP 握手")
}

async fn call_tool(
    service: &McpService,
    capability: &str,
    name: &str,
    arguments: Value,
) -> CallToolResult {
    let mut args = arguments.as_object().cloned().unwrap_or_default();
    // 每个工具调用都要带能力密钥（缺了桥会拒）。
    args.insert(
        "capability".to_string(),
        Value::String(capability.to_string()),
    );
    service
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(args))
        .await
        .expect("工具调用要走通 MCP 协议")
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

#[tokio::test]
async fn a_pending_approval_does_not_block_other_calls_on_the_same_connection() {
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!(
            "跳过 a_pending_approval_does_not_block_other_calls_on_the_same_connection：\
             找不到 zssh-mcp 可执行文件（先 cargo build -p zssh-mcp）"
        );
        return;
    };

    let host = Arc::new(StubHost);
    let approver = Arc::new(GateApprover::default());
    let bridge = Arc::new(
        Bridge::new(
            host,
            Arc::new(NullAudit),
            approver.clone(),
            Arc::new(ManualMode),
        )
        // 审批一直挂着也可以，测试自己放行；这个值只是为了出错时别等太久。
        .with_approval_wait(Duration::from_secs(60)),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    let serve_task = tokio::spawn(Arc::clone(&bridge).serve(listener));

    let scope = bridge.create_scope();
    scope.add_target(SESSION_ID, "web-1", "root", "connected", true, false);
    let capability = scope.capability().to_string();

    let mcp = start_mcp(&mcp_exe, port, bridge.connection_token()).await;

    // 第一条调用：手动模式，卡在审批上（审批桩要等测试放行才返回）。
    let first = call_tool(
        &mcp,
        &capability,
        "run_command",
        json!({ "target_id": SESSION_ID, "command": "echo hello" }),
    );
    // 第二条调用：等确认第一条真的进了审批，再在同一连接上发 list_targets。
    // 旧实现里 zssh-mcp 从写请求到读完响应一直占着连接锁，这条会一直等到超时。
    let second = async {
        approver
            .started
            .acquire()
            .await
            .expect("审批桩信号量不该关闭")
            .forget();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            call_tool(&mcp, &capability, "list_targets", json!({})),
        )
        .await;
        // 不管第二条结果如何都要放行第一条，别把测试自己挂住。
        approver.release.add_permits(1);
        result
    };
    let (first_result, second_result) = tokio::join!(first, second);

    assert_eq!(
        approver.calls(),
        1,
        "第一条调用要进审批；第二条是只读的，不审批"
    );

    let second =
        second_result.expect("审批挂起时，同一连接上的另一个工具调用必须能返回（被卡住了）");
    assert!(
        !tool_failed(&second),
        "list_targets 不该失败：{}",
        tool_text(&second)
    );
    let second_text = tool_text(&second);
    assert!(
        second_text.contains(SESSION_ID),
        "list_targets 要返回本对话的目标：{second_text}"
    );

    // 放行审批后，第一条调用也要收到它自己的结果（响应按 id 对上，不会串）。
    let first = first_result;
    assert!(
        !tool_failed(&first),
        "审批放行后 run_command 不该失败：{}",
        tool_text(&first)
    );
    let first_text = tool_text(&first);
    assert!(
        first_text.contains(COMMAND_OUTPUT),
        "run_command 要收到自己的输出：{first_text}"
    );
    assert!(
        !second_text.contains(COMMAND_OUTPUT),
        "list_targets 不能收到 run_command 的响应：{second_text}"
    );

    let _ = mcp.cancel().await;
    serve_task.abort();
    let _ = serve_task.await;
}

/// 同一连接上两个只读调用要真的并发执行（不是被一把锁排成一前一后）。
/// 宿主每次 read_terminal 睡 1 秒：并发时两个调用总共约 1 秒，串行则约 2 秒。
#[tokio::test]
async fn concurrent_read_terminal_calls_are_executed_in_parallel() {
    let Some(mcp_exe) = find_zssh_mcp() else {
        println!(
            "跳过 concurrent_read_terminal_calls_are_executed_in_parallel：\
             找不到 zssh-mcp 可执行文件（先 cargo build -p zssh-mcp）"
        );
        return;
    };
    let host = Arc::new(SlowHost::default());
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        Arc::new(NullAudit),
        Arc::new(UnusedApprover),
        Arc::new(AutoMode),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("绑定桥端口");
    let port = listener.local_addr().expect("桥端口").port();
    let serve_task = tokio::spawn(Arc::clone(&bridge).serve(listener));

    let scope = bridge.create_scope();
    scope.add_target(SESSION_ID, "web-1", "root", "connected", true, false);
    let capability = scope.capability().to_string();
    let mcp = start_mcp(&mcp_exe, port, bridge.connection_token()).await;

    let started = std::time::Instant::now();
    let (first, second) = tokio::join!(
        call_tool(
            &mcp,
            &capability,
            "read_terminal",
            json!({ "target_id": SESSION_ID, "max_lines": 10 }),
        ),
        call_tool(
            &mcp,
            &capability,
            "read_terminal",
            json!({ "target_id": SESSION_ID, "max_lines": 20 }),
        ),
    );
    let elapsed = started.elapsed();

    for result in [&first, &second] {
        assert!(
            !tool_failed(result),
            "并发 read_terminal 不该失败：{}",
            tool_text(result)
        );
    }
    assert_eq!(
        host.max_concurrent(),
        2,
        "两个调用要同时在桥里执行（最大并发 {}）",
        host.max_concurrent()
    );
    assert!(
        elapsed < Duration::from_millis(1800),
        "两个 1 秒的调用并发应约 1 秒完成，实际 {elapsed:?}（串行会接近 2 秒）"
    );

    let _ = mcp.cancel().await;
    serve_task.abort();
    let _ = serve_task.await;
}

/// 审批桩：收到请求记一次并给 `started` 一个许可；放行由测试给 `release` 许可。
struct GateApprover {
    calls: AtomicUsize,
    started: Semaphore,
    release: Semaphore,
    last: Mutex<Option<ApprovalRequest>>,
}

impl Default for GateApprover {
    fn default() -> GateApprover {
        GateApprover {
            calls: AtomicUsize::new(0),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            last: Mutex::new(None),
        }
    }
}

impl GateApprover {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Approver for GateApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(request);
        self.started.add_permits(1);
        Box::pin(async move {
            let _ = self.release.acquire().await;
            ApprovalDecision::Allowed
        })
    }
}

/// 不落盘的审计。
struct NullAudit;

impl AuditLog for NullAudit {
    fn record(&self, _entry: AuditEntry) {}
}

/// 固定手动模式（审批才生效）。
struct ManualMode;

impl ExecModeProvider for ManualMode {
    fn mode(&self) -> ExecMode {
        ExecMode::Manual
    }
}

/// 宿主桩：只认一个会话，exec 返回固定输出；不碰真实连接。
struct StubHost;

impl BridgeHost for StubHost {
    fn exec<'a>(
        &'a self,
        session_id: &'a str,
        _command: &'a str,
        _wait: Duration,
        _max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async move {
            if session_id != SESSION_ID {
                return Err("测试宿主只认识一个会话".to_owned());
            }
            Ok(ExecCapture {
                stdout: format!("{COMMAND_OUTPUT}\n"),
                stderr: String::new(),
                stdout_truncated_bytes: 0,
                stderr_truncated_bytes: 0,
                exit_code: Some(0),
                timed_out: false,
                cancelled: false,
                duration_ms: 1,
            })
        })
    }

    fn read_terminal<'a>(
        &'a self,
        _session_id: &'a str,
        _max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn exec_placement<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ExecPlacement> {
        Box::pin(async {
            ExecPlacement::Background {
                reason: "测试宿主不提供终端执行".to_owned(),
            }
        })
    }
}

/// 记录同一时刻最多有几条 read_terminal 在跑；每次睡 1 秒，方便按总耗时判断并发/串行。
#[derive(Default)]
struct SlowHost {
    active: AtomicUsize,
    max: AtomicUsize,
}

impl SlowHost {
    fn max_concurrent(&self) -> usize {
        self.max.load(Ordering::SeqCst)
    }
}

impl BridgeHost for SlowHost {
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
        max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(vec![format!("lines={max_lines}")])
        })
    }
}

/// 自动模式：审批接口一次都不会被调用。
struct AutoMode;

impl ExecModeProvider for AutoMode {
    fn mode(&self) -> ExecMode {
        ExecMode::Auto
    }
}

/// 自动模式下不会被调用的审批桩。
struct UnusedApprover;

impl Approver for UnusedApprover {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async { ApprovalDecision::Denied })
    }
}
