//! Z-SSH MCP stdio 服务器：把 MCP 工具调用转发给本机 Z-SSH 工具桥。
//!
//! Z-SSH 启动本程序时注入环境变量 `ZSSH_BRIDGE_PORT`（工具桥端口）和
//! `ZSSH_BRIDGE_TOKEN`（连接层令牌，只证明"是 Z-SSH 启动的那个 opencode 进程"）。
//! 缺任何一个都直接报错退出。
//!
//! 这层令牌只用于连接鉴权；对话之间靠**能力密钥**隔离——能力密钥由该对话的系统
//! 提示交给模型，模型每次调用工具时作为参数 `capability` 传进来，桥按它找到对应
//! 对话的会话表。所以本程序不持有能力密钥，只把它从工具参数透传给桥。
//!
//! 本程序只做转发：工具语义、危险判断、审批、审计全在 Z-SSH 工具桥一侧。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::JsonSchema;
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};

use zssh_core::ai::protocol::{
    RequestEnvelope, ResponseEnvelope, TokenLine, METHOD_LIST_DIR, METHOD_LIST_TARGETS,
    METHOD_READ_FILE, METHOD_READ_TERMINAL, METHOD_REMEMBER_HOST, METHOD_RUN_COMMAND, METHOD_STAT,
    METHOD_WRITE_FILE,
};

/// 解析启动环境变量（拆成纯函数方便测试）。
fn bridge_env(port: Option<String>, token: Option<String>) -> Result<(u16, String), String> {
    let port = port
        .and_then(|value| value.trim().parse::<u16>().ok())
        .ok_or("缺少或非法的环境变量 ZSSH_BRIDGE_PORT")?;
    let token = token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or("缺少环境变量 ZSSH_BRIDGE_TOKEN")?;
    Ok((port, token))
}

/// 把 `list_targets` 的结果排版成给模型看的文本。
fn format_sessions(result: &Value) -> String {
    let targets = result.get("targets").and_then(Value::as_array);
    let Some(targets) = targets else {
        return "No targets are available in this Z-SSH conversation.".into();
    };
    if targets.is_empty() {
        return "No targets are available in this Z-SSH conversation.".into();
    }
    let mut lines = vec![format!("{} target(s) in this conversation:", targets.len())];
    for target in targets {
        let marker = if target.get("current").and_then(Value::as_bool) == Some(true) {
            " [current]"
        } else if target.get("pinned").and_then(Value::as_bool) == Some(true) {
            " [pinned]"
        } else {
            ""
        };
        lines.push(format!(
            "- id: {} | host: {} | user: {} | status: {}{}",
            str_field(target, "id"),
            str_field(target, "host_label"),
            str_field(target, "username"),
            str_field(target, "status"),
            marker,
        ));
        if let Some(memory) = target.get("memory").and_then(Value::as_str) {
            for line in memory.lines() {
                lines.push(format!("    | {line}"));
            }
        }
    }
    if let Some(note) = result.get("memory_note").and_then(Value::as_str) {
        lines.push(note.to_owned());
    }
    lines.join("\n")
}

fn format_remember(result: &Value) -> String {
    format!(
        "Notes saved ({} / {} characters).",
        result
            .get("saved_chars")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        result.get("max_chars").and_then(Value::as_u64).unwrap_or(0)
    )
}

/// 把 `run_command` 的结果排版成给模型看的文本。
fn format_run_result(result: &Value) -> String {
    let exit_code = match result.get("exit_code").and_then(Value::as_u64) {
        Some(code) => code.to_string(),
        None => "unknown".into(),
    };
    let danger = str_field(result, "danger");
    let duration_ms = u64_field(result, "duration_ms");
    let location = str_field(result, "location");
    let timed_out = result
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let stdout = result.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = result.get("stderr").and_then(Value::as_str).unwrap_or("");

    let mut text =
        format!("danger: {danger}\nexit_code: {exit_code}\nduration_ms: {duration_ms}\n");
    text.push_str(&format!("location: {location}\n"));
    if let Some(reason) = result.get("fallback_reason").and_then(Value::as_str) {
        text.push_str(&format!("ran in the background because: {reason}\n"));
    }
    if timed_out {
        text.push_str(
            "timed out: the command is still running in the terminal; the output below is \
             what was captured so far.\n",
        );
    }
    if stdout.is_empty() {
        text.push_str("stdout: (empty)\n");
    } else {
        text.push_str("stdout:\n");
        text.push_str(stdout);
        if !stdout.ends_with('\n') {
            text.push('\n');
        }
    }
    if stderr.is_empty() {
        text.push_str("stderr: (empty)\n");
    } else {
        text.push_str("stderr:\n");
        text.push_str(stderr);
        if !stderr.ends_with('\n') {
            text.push('\n');
        }
    }

    let stdout_truncated = u64_field(result, "stdout_truncated_bytes");
    if stdout_truncated > 0 {
        text.push_str(&format!(
            "stdout truncated: {stdout_truncated} bytes dropped\n"
        ));
    }
    let stderr_truncated = u64_field(result, "stderr_truncated_bytes");
    if stderr_truncated > 0 {
        text.push_str(&format!(
            "stderr truncated: {stderr_truncated} bytes dropped\n"
        ));
    }
    text
}

/// 把 `read_terminal` 的结果排版成给模型看的文本。
fn format_terminal_lines(result: &Value) -> String {
    match result.get("lines").and_then(Value::as_array) {
        Some(lines) if !lines.is_empty() => lines
            .iter()
            .map(|line| line.as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "The terminal buffer is empty.".into(),
    }
}

/// 把 `list_dir` 的结果排版成给模型看的文本。
fn format_list_dir(result: &Value) -> String {
    let mut text = format!("path: {}\n", str_field(result, "path"));
    let Some(entries) = result.get("entries").and_then(Value::as_array) else {
        text.push_str("(no entries)\n");
        return text;
    };
    let total = u64_field(result, "total");
    text.push_str(&format!("entries: {} of {}\n", entries.len(), total));
    if result.get("truncated").and_then(Value::as_bool) == Some(true) {
        text.push_str(&format!(
            "truncated: only the first {} entries are listed ({} more not shown)\n",
            entries.len(),
            total.saturating_sub(entries.len() as u64)
        ));
    }
    for entry in entries {
        let size = entry
            .get("size")
            .and_then(Value::as_u64)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".into());
        let link = entry
            .get("link_target")
            .and_then(Value::as_str)
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        text.push_str(&format!(
            "{} {} {} {} {}{}\n",
            str_field(entry, "type"),
            str_field(entry, "mode"),
            size,
            entry.get("modified").and_then(Value::as_str).unwrap_or("-"),
            str_field(entry, "name"),
            link,
        ));
    }
    text
}

/// 把 `stat` 的结果排版成给模型看的文本。
fn format_stat(result: &Value) -> String {
    let path = str_field(result, "path");
    if result.get("exists").and_then(Value::as_bool) != Some(true) {
        return format!("path: {path}\nexists: false\n");
    }
    format!(
        "path: {path}\nexists: true\ntype: {}\nsize: {}\nmodified: {}\nmode: {}\n",
        str_field(result, "type"),
        u64_field(result, "size"),
        result
            .get("modified")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        str_field(result, "mode"),
    )
}

/// 把 `read_file` 的结果排版成给模型看的文本。
fn format_read_file(result: &Value) -> String {
    let path = str_field(result, "path");
    if result.get("binary").and_then(Value::as_bool) == Some(true) {
        return format!(
            "path: {path}\nThis is a binary file (not valid UTF-8 text); its content is not returned.\nsize: {}\n",
            u64_field(result, "size")
        );
    }
    let mut text = format!(
        "path: {path}\nsize: {}\noffset: {}\nreturned_bytes: {}\n",
        u64_field(result, "size"),
        u64_field(result, "offset"),
        u64_field(result, "returned_bytes"),
    );
    match result.get("next_offset").and_then(Value::as_u64) {
        Some(next) => text.push_str(&format!(
            "more: yes, call read_file again with offset={next} to continue\n"
        )),
        None => text.push_str("more: no (end of file)\n"),
    }
    text.push_str("content:\n");
    text.push_str(result.get("content").and_then(Value::as_str).unwrap_or(""));
    text
}

/// 把 `write_file` 的结果排版成给模型看的文本。
fn format_write_file(result: &Value) -> String {
    let mut text = format!(
        "path: {}\nwritten_bytes: {}\n{}\n",
        str_field(result, "path"),
        u64_field(result, "bytes_written"),
        if result.get("created").and_then(Value::as_bool) == Some(true) {
            "created: a new file"
        } else {
            "replaced: the existing file was overwritten"
        }
    );
    match result.get("backup_path").and_then(Value::as_str) {
        Some(backup) => text.push_str(&format!(
            "backup: the previous version was saved on the server at {backup}\n"
        )),
        None => text.push_str("backup: none (new file)\n"),
    }
    if let Some(change) = result.get("owner_change") {
        text.push_str(&format!(
            "owner: could not restore the file's owner: it changed from {} to {}{}\n",
            str_field(change, "from"),
            str_field(change, "to"),
            change
                .get("error")
                .and_then(Value::as_str)
                .map(|err| format!(" ({err})"))
                .unwrap_or_default(),
        ));
    }
    if let Some(note) = result.get("retention_note").and_then(Value::as_str) {
        text.push_str(&format!("backups: {note}\n"));
    }
    if result.get("dangerous").and_then(Value::as_bool) == Some(true) {
        let reasons = result
            .get("danger_reasons")
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        text.push_str(&format!("danger: flagged as a system path ({reasons})\n"));
    }
    text
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// 一条连接上等响应的调用表。连接断了要把它们全部失败，免得调用方一直等。
#[derive(Default)]
struct PendingTable {
    /// 请求 id → 等这条响应的调用方。
    calls: HashMap<u64, oneshot::Sender<Result<ResponseEnvelope, String>>>,
    /// 连接已断：之后不允许再往这条连接上登记请求。
    closed: bool,
}

/// 一条到工具桥的长连接。写一半用短锁串行化（一次只占一条请求的写），
/// 后台读循环按响应里的 `id` 把结果分发给对应调用方——一条请求卡在审批上时，
/// 同一进程里的其它对话 / 同一对话的并行调用照常走（R-8 第 10 节）。
struct BridgeConnection {
    writer: Mutex<OwnedWriteHalf>,
    next_id: AtomicU64,
    pending: StdMutex<PendingTable>,
}

impl BridgeConnection {
    fn pending(&self) -> std::sync::MutexGuard<'_, PendingTable> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_closed(&self) -> bool {
        self.pending().closed
    }

    /// 把连接标成已断，并把所有等响应的调用失败（读循环退出 / 写失败时调）。
    fn fail_all_pending(&self, message: &str) {
        let waiters: Vec<_> = {
            let mut pending = self.pending();
            pending.closed = true;
            pending.calls.drain().map(|(_, waiter)| waiter).collect()
        };
        for waiter in waiters {
            let _ = waiter.send(Err(message.to_string()));
        }
    }

    /// 读循环：一行一条响应，按 `id` 找到调用方。响应可以乱序（桥那边并发处理）。
    async fn read_loop(self: Arc<Self>, mut reader: BufReader<OwnedReadHalf>) {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let response: ResponseEnvelope = match serde_json::from_str(line.trim()) {
                Ok(response) => response,
                Err(_) => {
                    // 解析不出 id，这条响应没法交给任何调用方；整条连接按不可用处理，
                    // 不然那个调用方会永远等下去（下次调用自动重连）。
                    self.fail_all_pending("Z-SSH 工具桥响应无法解析");
                    return;
                }
            };
            let waiter = self.pending().calls.remove(&response.id);
            if let Some(waiter) = waiter {
                // 调用方可能已经取消（receiver 丢了），发送失败无所谓。
                let _ = waiter.send(Ok(response));
            }
        }
        self.fail_all_pending("Z-SSH 工具桥连接中断");
    }
}

struct BridgeClient {
    port: u16,
    token: String,
    /// 当前连接；断了（读循环退出 / 写失败）后置空，下次调用重连。
    connection: Mutex<Option<Arc<BridgeConnection>>>,
}

impl BridgeClient {
    fn new(port: u16, token: String) -> BridgeClient {
        BridgeClient {
            port,
            token,
            connection: Mutex::new(None),
        }
    }

    /// 连上桥、发连接层令牌、起后台读循环。
    async fn connect(&self) -> Result<Arc<BridgeConnection>, String> {
        let stream = TcpStream::connect(("127.0.0.1", self.port))
            .await
            .map_err(|err| format!("连不上 Z-SSH 工具桥：{err}"))?;
        let (read_half, mut write_half) = stream.into_split();
        let mut line = serde_json::to_string(&TokenLine {
            token: self.token.clone(),
        })
        .map_err(|err| err.to_string())?;
        line.push('\n');
        write_half
            .write_all(line.as_bytes())
            .await
            .map_err(|err| format!("工具桥握手失败：{err}"))?;
        let connection = Arc::new(BridgeConnection {
            writer: Mutex::new(write_half),
            next_id: AtomicU64::new(1),
            pending: StdMutex::new(PendingTable::default()),
        });
        tokio::spawn({
            let connection = Arc::clone(&connection);
            async move { connection.read_loop(BufReader::new(read_half)).await }
        });
        Ok(connection)
    }

    /// 取当前连接；没有或已断就重连。并发调用共用一次重连（状态锁只在这一小段）。
    async fn connection(&self) -> Result<Arc<BridgeConnection>, String> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            if !connection.is_closed() {
                return Ok(Arc::clone(connection));
            }
        }
        let connection = self.connect().await?;
        *guard = Some(Arc::clone(&connection));
        Ok(connection)
    }

    /// 发一条请求，等它自己的响应（按 id 对应）；桥返回的 error.message 原样传给模型。
    ///
    /// 写请求只占写锁一次 `write_all`，等待响应时不占任何锁：同一连接上的其它调用
    /// 可以同时进行。`capability` 是该对话的能力密钥，随每条请求一起发给桥。
    async fn call(&self, capability: &str, method: &str, params: Value) -> Result<Value, String> {
        let connection = self.connection().await?;
        let id = connection.next_id.fetch_add(1, Ordering::SeqCst);
        let request = RequestEnvelope {
            id,
            method: method.to_string(),
            params,
            capability: capability.to_string(),
        };
        let mut line = serde_json::to_string(&request).map_err(|err| err.to_string())?;
        line.push('\n');

        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = connection.pending();
            // 连接在等锁期间可能刚断：交给下一次调用重连，不要挂在这条死连接上。
            if pending.closed {
                return Err("Z-SSH 工具桥连接中断".into());
            }
            pending.calls.insert(id, sender);
        }

        let write = async {
            let mut writer = connection.writer.lock().await;
            writer.write_all(line.as_bytes()).await
        };
        if write.await.is_err() {
            connection.pending().calls.remove(&id);
            connection.fail_all_pending("Z-SSH 工具桥连接中断");
            return Err("Z-SSH 工具桥连接中断".into());
        }

        match receiver.await {
            Ok(Ok(response)) => match response.error {
                Some(error) => Err(error.message),
                None => Ok(response.result.unwrap_or(Value::Null)),
            },
            Ok(Err(message)) => Err(message),
            // 连接断 / 响应解析失败时读循环会把错误送过来；sender 被丢掉也按中断处理。
            Err(_) => Err("Z-SSH 工具桥连接中断".into()),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunCommandArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal; to work on another server you MUST use a target id from list_targets.
    #[serde(default)]
    target_id: Option<String>,
    /// Shell command to run on that target.
    command: String,
    /// Timeout in seconds (default 60, max 600). On timeout the command keeps running in the terminal.
    #[serde(default)]
    timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadTerminalArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal.
    #[serde(default)]
    target_id: Option<String>,
    /// How many most recent lines to read (default 200, max 2000).
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListTargetsArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListDirArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal's server.
    #[serde(default)]
    target_id: Option<String>,
    /// Directory path ON THE SERVER (absolute, or relative to the login directory; ~ is the login directory).
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RememberHostArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal's server.
    #[serde(default)]
    target_id: Option<String>,
    /// The COMPLETE notes for this host (they replace the previous notes; an empty string clears them). Max 4000 characters. Facts only: overview, services, config locations, known issues, recent changes, to-dos. Never include passwords, tokens or keys.
    notes: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StatArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal's server.
    #[serde(default)]
    target_id: Option<String>,
    /// File or directory path ON THE SERVER.
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadFileArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal's server.
    #[serde(default)]
    target_id: Option<String>,
    /// File path ON THE SERVER.
    path: String,
    /// Byte offset to start reading from (default 0). Use the next_offset a previous call returned to continue.
    #[serde(default)]
    offset: Option<u64>,
    /// Maximum bytes to return (default and maximum 65536).
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WriteFileArgs {
    /// Z-SSH conversation capability key from your system prompt. Required; calls with a wrong or expired key are rejected.
    capability: String,
    /// Target id from list_targets. Omit to use the current focus terminal's server.
    #[serde(default)]
    target_id: Option<String>,
    /// File path ON THE SERVER.
    path: String,
    /// The complete new text content of the file (the whole file is replaced, max 5 MiB).
    content: String,
    /// If true, fail when the file already exists (only create new files).
    #[serde(default)]
    create_only: Option<bool>,
}

#[derive(Clone)]
struct ZsshMcpServer {
    client: Arc<BridgeClient>,
    tool_router: ToolRouter<ZsshMcpServer>,
}

#[tool_router]
impl ZsshMcpServer {
    fn new(client: Arc<BridgeClient>) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List the targets (servers) available in this Z-SSH conversation. The current focus terminal is marked [current]; servers the user added with @ are marked [pinned]. Returns target ids (use them with run_command and read_terminal), host display names, usernames and connection status. Addresses and credentials are never exposed. Requires your conversation capability key."
    )]
    async fn list_targets(&self, Parameters(args): Parameters<ListTargetsArgs>) -> CallToolResult {
        match self
            .client
            .call(&args.capability, METHOD_LIST_TARGETS, json!({}))
            .await
        {
            Ok(result) => {
                CallToolResult::success(vec![ContentBlock::text(format_sessions(&result))])
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Run a shell command on an open Z-SSH session and wait for it to finish. Output is captured; each stream is capped at 64KB and truncation is reported. In manual mode the command goes through the user's approval gate in the Z-SSH app first: it can be denied by the user, and approval can time out; in both cases the tool reports the denial instead of output. In auto mode it runs without asking. The reported danger level (safe/dangerous) is a marker only. Returns the exit code, duration and captured stdout/stderr. Requires your conversation capability key."
    )]
    async fn run_command(&self, Parameters(args): Parameters<RunCommandArgs>) -> CallToolResult {
        let mut params = json!({ "command": args.command });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        if let Some(secs) = args.timeout_sec {
            params["timeout_sec"] = json!(secs);
        }
        match self
            .client
            .call(&args.capability, METHOD_RUN_COMMAND, params)
            .await
        {
            Ok(result) => {
                CallToolResult::success(vec![ContentBlock::text(format_run_result(&result))])
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Read the most recent terminal output of a Z-SSH target (ANSI escape sequences are removed). Use max_lines to control how many lines to read (default 200, max 2000). Omit target_id to use the current focus terminal. Requires your conversation capability key."
    )]
    async fn read_terminal(
        &self,
        Parameters(args): Parameters<ReadTerminalArgs>,
    ) -> CallToolResult {
        let mut params = json!({});
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        if let Some(lines) = args.max_lines {
            params["max_lines"] = json!(lines);
        }
        match self
            .client
            .call(&args.capability, METHOD_READ_TERMINAL, params)
            .await
        {
            Ok(result) => {
                CallToolResult::success(vec![ContentBlock::text(format_terminal_lines(&result))])
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "List a directory ON THE SERVER of a Z-SSH target: name, type, size, modification time and permissions for each entry (at most 1000 entries; truncation is reported). The path is a path on the server, not on the user's computer. Omit target_id to use the current focus terminal's server. Read-only: never asks the user for confirmation. Requires your conversation capability key."
    )]
    async fn list_dir(&self, Parameters(args): Parameters<ListDirArgs>) -> CallToolResult {
        let mut params = json!({ "path": args.path });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        self.forward(&args.capability, METHOD_LIST_DIR, params, format_list_dir)
            .await
    }

    #[tool(
        description = "Read a text file ON THE SERVER of a Z-SSH target. Returns at most 64KB per call; when more remains the result says which offset to pass next so you can read the file in segments. Binary files are reported as binary and their content is not returned. The path is a path on the server, not on the user's computer. Omit target_id to use the current focus terminal's server. Read-only: never asks the user for confirmation. Requires your conversation capability key."
    )]
    async fn read_file(&self, Parameters(args): Parameters<ReadFileArgs>) -> CallToolResult {
        let mut params = json!({ "path": args.path });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        if let Some(offset) = args.offset {
            params["offset"] = json!(offset);
        }
        if let Some(max_bytes) = args.max_bytes {
            params["max_bytes"] = json!(max_bytes);
        }
        self.forward(&args.capability, METHOD_READ_FILE, params, format_read_file)
            .await
    }

    #[tool(
        description = "Write a whole text file ON THE SERVER of a Z-SSH target (the file is replaced; max 5 MiB). The path is a path on the server, not on the user's computer. Before overwriting, the original is copied to <name>.zssh-bak-<time> next to it (if that backup fails nothing is written), and the new content is written to a temporary file and renamed into place, so a half-written file is never left behind. In manual mode the user sees a diff and must approve first; writing system paths (/etc, /boot, /usr, authorized_keys, sudoers, crontab ...) is flagged in red. Set create_only to refuse overwriting an existing file. Read the file with read_file first when editing. Omit target_id to use the current focus terminal's server. Requires your conversation capability key."
    )]
    async fn write_file(&self, Parameters(args): Parameters<WriteFileArgs>) -> CallToolResult {
        let mut params = json!({ "path": args.path, "content": args.content });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        if let Some(create_only) = args.create_only {
            params["create_only"] = json!(create_only);
        }
        self.forward(
            &args.capability,
            METHOD_WRITE_FILE,
            params,
            format_write_file,
        )
        .await
    }

    #[tool(
        description = "Check a path ON THE SERVER of a Z-SSH target: whether it exists, its type (file/dir/symlink/other), size, modification time and permissions. The path is a path on the server, not on the user's computer. Omit target_id to use the current focus terminal's server. Read-only: never asks the user for confirmation. Requires your conversation capability key."
    )]
    async fn stat(&self, Parameters(args): Parameters<StatArgs>) -> CallToolResult {
        let mut params = json!({ "path": args.path });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        self.forward(&args.capability, METHOD_STAT, params, format_stat)
            .await
    }

    #[tool(
        description = "Save long-term notes about a Z-SSH host so the next conversation starts with them (they come back in list_targets). Send the COMPLETE updated notes; they replace the old ones. Keep them short and factual: what the host runs, where configs live, problems found and how they were fixed, recent changes, open to-dos. Never store passwords, tokens or keys (values that look like secrets are hidden). Notes stay on the user's computer and the user can view or edit them. Does not touch the server and never asks for confirmation. Requires your conversation capability key."
    )]
    async fn remember_host(
        &self,
        Parameters(args): Parameters<RememberHostArgs>,
    ) -> CallToolResult {
        let mut params = json!({ "notes": args.notes });
        if let Some(target_id) = args.target_id {
            params["target_id"] = json!(target_id);
        }
        self.forward(
            &args.capability,
            METHOD_REMEMBER_HOST,
            params,
            format_remember,
        )
        .await
    }

    /// 转发一次调用并把成功结果排版成文本；失败原样把桥的错误告诉模型。
    async fn forward(
        &self,
        capability: &str,
        method: &str,
        params: Value,
        format: fn(&Value) -> String,
    ) -> CallToolResult {
        match self.client.call(capability, method, params).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(format(&result))]),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ZsshMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("zssh-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Tools for the Z-SSH SSH client. Targets are the servers in this AI \
                 conversation: the current focus terminal plus any host the user added with @. \
                 Every command runs on a real server; depending on the app's execution mode it \
                 may go through the user's approval gate first. Call list_targets first to get \
                 target ids; omit target_id to use the current focus terminal. File tools                  (list_dir, read_file, write_file, stat) only touch files on those servers,                  never the user's own computer; paths are server paths.",
            )
    }
}

#[tokio::main]
async fn main() {
    let (port, token) = match bridge_env(
        std::env::var("ZSSH_BRIDGE_PORT").ok(),
        std::env::var("ZSSH_BRIDGE_TOKEN").ok(),
    ) {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("zssh-mcp: {message}");
            std::process::exit(1);
        }
    };
    let server = ZsshMcpServer::new(Arc::new(BridgeClient::new(port, token)));
    let service = match server.serve(stdio()).await {
        Ok(service) => service,
        Err(err) => {
            eprintln!("zssh-mcp: 启动失败：{err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = service.waiting().await {
        eprintln!("zssh-mcp: 退出：{err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_exposes_exactly_the_tools_the_agents_allow_list() {
        use zssh_core::ai::agent::config::ZSSH_TOOL_NAMES;
        let server = ZsshMcpServer::new(Arc::new(BridgeClient::new(1, "t".into())));
        let mut exposed: Vec<String> = server
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        exposed.sort();
        let mut expected: Vec<String> = ZSSH_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(exposed, expected);
    }

    #[test]
    fn bridge_env_requires_port_and_token() {
        assert!(bridge_env(None, Some("token".into())).is_err());
        assert!(bridge_env(Some("not-a-port".into()), Some("token".into())).is_err());
        assert!(bridge_env(Some("9000".into()), None).is_err());
        assert!(bridge_env(Some("9000".into()), Some(String::new())).is_err());
        let (port, token) =
            bridge_env(Some("9000".into()), Some("token".into())).expect("完整的变量");
        assert_eq!(port, 9000);
        assert_eq!(token, "token");
    }

    #[test]
    fn format_targets_lists_ids_and_marks_current() {
        let result = json!({ "targets": [
            { "id": "s1", "host_label": "web-1", "username": "root", "status": "connected", "current": true, "pinned": false },
            { "id": "s2", "host_label": "db-1", "username": "admin", "status": "connected", "current": false, "pinned": true },
        ]});
        let text = format_sessions(&result);
        assert!(text.contains("s1"));
        assert!(text.contains("web-1"));
        assert!(text.contains("admin"));
        assert!(text.contains("[current]"));
        assert!(text.contains("[pinned]"));

        let empty = format_sessions(&json!({ "targets": [] }));
        assert!(empty.contains("No targets"));
    }

    #[test]
    fn format_run_result_shows_danger_exit_code_streams_and_truncation() {
        let result = json!({
            "stdout": "hello\n",
            "stderr": "warn\n",
            "stdout_truncated_bytes": 128,
            "stderr_truncated_bytes": 0,
            "exit_code": 3,
            "duration_ms": 42,
            "danger": "dangerous",
            "location": "terminal",
            "timed_out": false,
        });
        let text = format_run_result(&result);
        assert!(text.contains("danger: dangerous"));
        assert!(text.contains("exit_code: 3"));
        assert!(text.contains("location: terminal"));
        assert!(text.contains("hello"));
        assert!(text.contains("warn"));
        assert!(text.contains("128"));
    }

    #[test]
    fn format_run_result_shows_fallback_reason_and_timeout() {
        let result = json!({
            "stdout": "",
            "stderr": "",
            "stdout_truncated_bytes": 0,
            "stderr_truncated_bytes": 0,
            "exit_code": null,
            "duration_ms": 10,
            "danger": "safe",
            "location": "background",
            "fallback_reason": "终端忙",
            "timed_out": true,
        });
        let text = format_run_result(&result);
        assert!(text.contains("location: background"));
        assert!(text.contains("终端忙"), "要说明为什么走后台");
        assert!(text.contains("still running"), "超时要说明命令还在跑");
    }

    #[test]
    fn format_run_result_handles_missing_exit_code() {
        let result = json!({
            "stdout": "",
            "stderr": "",
            "stdout_truncated_bytes": 0,
            "stderr_truncated_bytes": 0,
            "exit_code": null,
            "duration_ms": 10,
            "danger": "safe",
            "location": "terminal",
        });
        let text = format_run_result(&result);
        assert!(text.contains("danger: safe"));
        assert!(text.contains("exit_code: unknown"));
    }

    #[test]
    fn format_terminal_lines_joins_lines() {
        let text = format_terminal_lines(&json!({ "lines": ["a", "b"] }));
        assert_eq!(text, "a\nb");

        let empty = format_terminal_lines(&json!({ "lines": [] }));
        assert!(empty.contains("empty"));
    }

    #[test]
    fn format_list_dir_shows_entries_and_truncation() {
        let result = json!({
            "path": "/srv",
            "total": 1500,
            "truncated": true,
            "entries": [
                { "name": "a.txt", "type": "file", "size": 12, "modified": "2026-01-01 00:00:00", "mode": "0644" },
                { "name": "lnk", "type": "symlink", "size": null, "modified": null, "mode": "0777", "link_target": "a.txt" },
            ],
        });
        let text = format_list_dir(&result);
        assert!(text.contains("path: /srv"));
        assert!(text.contains("entries: 2 of 1500"));
        assert!(text.contains("truncated"));
        assert!(text.contains("file 0644 12 2026-01-01 00:00:00 a.txt"));
        assert!(text.contains("lnk -> a.txt"));
    }

    #[test]
    fn format_read_file_tells_the_model_how_to_continue_or_that_it_is_binary() {
        let more = format_read_file(&json!({
            "path": "/a", "binary": false, "content": "abc", "offset": 0,
            "returned_bytes": 3, "size": 10, "next_offset": 3,
        }));
        assert!(more.contains("offset=3"));
        assert!(more.ends_with("abc"));
        let done = format_read_file(&json!({
            "path": "/a", "binary": false, "content": "x", "offset": 9,
            "returned_bytes": 1, "size": 10,
        }));
        assert!(done.contains("end of file"));
        let bin =
            format_read_file(&json!({ "path": "/b", "binary": true, "content": "", "size": 5 }));
        assert!(bin.contains("binary file"));
        assert!(!bin.contains("content:"));
    }

    #[test]
    fn format_write_file_reports_backup_and_danger() {
        let text = format_write_file(&json!({
            "path": "/etc/hosts", "bytes_written": 10, "created": false,
            "backup_path": "/etc/hosts.zssh-bak-1", "dangerous": true,
            "danger_reasons": ["写入系统路径"],
        }));
        assert!(text.contains("/etc/hosts.zssh-bak-1"));
        assert!(text.contains("flagged as a system path"));
        let fresh = format_write_file(&json!({
            "path": "/tmp/x", "bytes_written": 1, "created": true,
            "dangerous": false, "danger_reasons": [],
        }));
        assert!(fresh.contains("a new file") && fresh.contains("backup: none"));
    }

    #[test]
    fn format_write_file_reports_owner_change_and_retention_problem() {
        let text = format_write_file(&json!({
            "path": "/etc/hosts", "bytes_written": 1, "created": false,
            "backup_path": "/etc/hosts.zssh-bak-1", "dangerous": false,
            "danger_reasons": [], "pruned_backups": 0,
            "retention_note": "旧备份清理失败（不影响这次写入）：删除被拒绝",
            "owner_change": {
                "from": "uid 0 / gid 0", "to": "uid 1000 / gid 1000",
                "error": "权限不够",
            },
        }));
        assert!(
            text.contains(
                "could not restore the file's owner: it changed from uid 0 / gid 0 to uid 1000 / gid 1000"
            ),
            "{text}"
        );
        assert!(text.contains("旧备份清理失败"), "{text}");
    }

    #[test]
    fn format_stat_handles_missing_paths() {
        assert!(format_stat(&json!({ "path": "/x", "exists": false })).contains("exists: false"));
        let hit = format_stat(&json!({
            "path": "/x", "exists": true, "type": "dir", "size": 4096,
            "modified": "2026-01-01 00:00:00", "mode": "0755",
        }));
        assert!(hit.contains("type: dir") && hit.contains("mode: 0755"));
    }

    use std::time::Duration;
    use tokio::net::TcpListener;

    fn response_line(response: Value) -> String {
        let mut line = serde_json::to_string(&response).expect("序列化响应");
        line.push('\n');
        line
    }

    /// 一条连接上并发调用：响应按 id 对应，可以乱序回来。
    #[tokio::test]
    async fn concurrent_calls_match_their_responses_by_id() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑定端口");
        let port = listener.local_addr().expect("端口").port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("接受连接");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut handshake = String::new();
            reader.read_line(&mut handshake).await.expect("读握手");
            let mut requests = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("读请求");
                requests
                    .push(serde_json::from_str::<RequestEnvelope>(line.trim()).expect("解析请求"));
            }
            // 故意倒序回：后发的先回，先发的慢一拍。
            for (index, request) in requests.iter().enumerate().rev() {
                if index == 0 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let response = json!({
                    "id": request.id,
                    "result": { "method": request.method, "params": request.params },
                });
                write_half
                    .write_all(response_line(response).as_bytes())
                    .await
                    .expect("写响应");
            }
        });

        let client = BridgeClient::new(port, "token".into());
        let (first, second) = tokio::join!(
            client.call("cap", "alpha", json!({ "n": 1 })),
            client.call("cap", "beta", json!({ "n": 2 })),
        );
        let first = first.expect("第一条响应");
        assert_eq!(first["method"], "alpha", "第一条要拿到自己的响应：{first}");
        let second = second.expect("第二条响应");
        assert_eq!(second["method"], "beta", "第二条要拿到自己的响应：{second}");
        server.await.expect("假桥任务");
    }

    /// 连接断了：等响应的调用立刻失败，下次调用自动重连。
    #[tokio::test]
    async fn a_dropped_connection_fails_waiting_calls_and_the_next_call_reconnects() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑定端口");
        let port = listener.local_addr().expect("端口").port();
        let server = tokio::spawn(async move {
            // 第一条连接：读握手和一条请求，然后不回、直接断开。
            let (stream, _) = listener.accept().await.expect("接受第一条连接");
            let (read_half, write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("读握手");
            line.clear();
            reader.read_line(&mut line).await.expect("读请求");
            drop(write_half);
            drop(reader);
            // 第二条连接：正常回一条。
            let (stream, _) = listener.accept().await.expect("接受第二条连接");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("读握手");
            line.clear();
            reader.read_line(&mut line).await.expect("读第二条请求");
            let request: RequestEnvelope = serde_json::from_str(line.trim()).expect("解析请求");
            let response = json!({
                "id": request.id,
                "result": { "params": request.params },
            });
            write_half
                .write_all(response_line(response).as_bytes())
                .await
                .expect("写响应");
        });

        let client = BridgeClient::new(port, "token".into());
        let first = client.call("cap", "run_command", json!({ "n": 1 })).await;
        assert!(first.is_err(), "连接断了，等待中的调用必须失败：{first:?}");
        let second = client
            .call("cap", "run_command", json!({ "n": 2 }))
            .await
            .expect("下次调用要自动重连");
        assert_eq!(second["params"]["n"], 2, "重连后要拿到新响应的内容");
        server.await.expect("假桥任务");
    }
}
