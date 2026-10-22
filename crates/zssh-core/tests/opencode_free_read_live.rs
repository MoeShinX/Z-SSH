//! opencode 本机 `read` 工具一律被拒的实测（卡片 AI-OC-FREE 第 2 节，UI-6b 第 4 项重做），
//! 外加免费模型能正常回复的独立检查。
//!
//! ## 为什么用假模型
//!
//! 以前靠真实的免费模型"听话地去调 read"，模型不调、回合报错、命令超时都会得到 0 次调用，
//! 而判定只看「调用数 > 被拒数」，0 次也算通过——安全检查根本没跑到。现在起一个本机回环的
//! 假模型服务（OpenAI 兼容的 `chat/completions`，流式），作为自定义 provider 写进隔离的
//! opencode 配置（和用户自建服务商走同一条 `user_model_config` 路径），**强制它对每种路径各发
//! 一次 `read` 工具调用**，由真实 opencode 做权限判断。这样不依赖免费模型听不听话，
//! 也不消耗外部服务，结果是确定的。
//!
//! 判定见 [`judge`]：每种路径必须同时满足——至少 1 次有效的 `read` 调用、所有有效调用都被
//! 权限规则拒绝、输出里没有标记、回合没有报错、`read` 确实在发给模型的工具列表里；
//! 做不到（没调用 / 超时 / 回合报错）一律判失败（标成「未验证」），不会当成安全通过。
//! 假模型自己还会看 opencode 回传的工具结果里有没有标记，和事件流里的检查互为印证。
//! 对照组：把 `read` 放开，同样的做法必须读到标记，证明上面的检查是灵的。
//!
//! 每种路径走两条路：Z-SSH 的生产路径（同一份配置 `build_config_content` + 同一个运行时
//! `AgentRuntime`，含每回合的工具禁用表和系统提示），以及 `opencode run`（同一份配置、
//! 没有 Z-SSH 的系统提示）。
//!
//! ## 隔离
//!
//! `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME` / `XDG_STATE_HOME` 和家目录
//! （`USERPROFILE` / `HOME`）全部指到系统临时目录下的一个新目录，工作目录也在里面，
//! 不读不改用户自己的 opencode 配置、凭据和会话。另外设置 `OPENCODE_DISABLE_CLAUDE_CODE*`，
//! 免得 opencode 顺手去扫用户的 `~/.claude`。假模型只监听 127.0.0.1 的随机端口。
//!
//! 条件（不满足就打印「跳过」）：`ZSSH_LIVE_FULL=1`、`ZSSH_AGENT_E2E=1`、PATH 上有
//! 可用的 opencode（也可以用 `ZSSH_AGENT_OPENCODE` 指定）。免费模型那一条还要目录里能列出
//! 免费模型、网络通、模型在当前地区可用。输出里不打印任何文件内容，也不打印 opencode 的
//! 错误全文（里面可能带本机路径），只打印「有没有出现标记」和调用计数。

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use zssh_core::ai::agent::config::{build_config_content, McpServerSpec};
use zssh_core::ai::agent::discovery::{AgentInstall, DiscoverySource};
use zssh_core::ai::agent::opencode::{
    AgentRuntime, OpenCodeConversation, StartOptions, DEFAULT_IDLE_TIMEOUT, DEFAULT_STARTUP_TIMEOUT,
};
use zssh_core::ai::agent::{AgentConversation, AgentEvent};

const RUN_TIMEOUT: Duration = Duration::from_secs(150);

/// 假模型在配置里的服务商 id / 模型 id（组合成 `zfake/m1`）。
const FAKE_PROVIDER: &str = "zfake";
const FAKE_MODEL: &str = "m1";

/// 这个文件里的测试都要改进程环境变量（家目录、XDG 目录），不能并行。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 一种读法：名字 + 交给模型的路径写法 + 对应的金丝雀标记。
struct Case {
    name: &'static str,
    path: String,
    marker: String,
    /// 对照组（read 放开）里这种写法应该能读到标记。opencode 的 read 工具不展开 `~`
    /// （放开权限后读 `~/x` 只会得到「文件不存在」），所以那一种读不到是正常的，
    /// 它的价值在于证明权限规则在解析路径之前就拒绝了。
    control_reads: bool,
}

/// 一次回合（或一次 `opencode run`）观察到的东西。
#[derive(Default, Debug, Clone)]
struct Outcome {
    /// 输出（正文 + 工具结果）里出现了标记。
    leaked: bool,
    /// 有效的 read 调用次数：真的执行了，或被权限规则拒绝。参数不合法之类既没读到也不是
    /// 拒绝的失败不算。
    read_calls: usize,
    /// 其中被权限规则拒绝的次数。
    read_rejected: usize,
    /// 回合以错误结束（免费服务拒绝、回合超时、命令超时、启动失败……）。
    turn_error: Option<String>,
    /// 助手正文长度（字符数），排查「没回复」用。
    text_chars: usize,
}

/// 判定一次观察的结果：`Ok` 才算这种路径被证明读不到；`Err` 是失败原因。
/// `offered_read` 是假模型自己看到的：`read` 是否在发给模型的工具列表里。
fn judge(outcome: &Outcome, offered_read: bool, server_saw_marker: bool) -> Result<(), String> {
    if outcome.leaked || server_saw_marker {
        return Err("输出里出现了标记".to_string());
    }
    if let Some(error) = &outcome.turn_error {
        return Err(format!("未验证：回合没有正常结束（{error}）"));
    }
    if outcome.read_calls == 0 {
        return Err("未验证：没有有效的 read 调用".to_string());
    }
    if !offered_read {
        return Err("未验证：read 不在发给模型的工具列表里".to_string());
    }
    if outcome.read_calls > outcome.read_rejected {
        return Err(format!(
            "有 read 调用没被拒（{} 次调用，{} 次被拒）",
            outcome.read_calls, outcome.read_rejected
        ));
    }
    Ok(())
}

fn random_marker(tag: &str) -> String {
    format!("ZSSH_CANARY_{tag}_{}", uuid::Uuid::new_v4().simple())
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

/// 用户缓存里的模型清单（只读这一个文件）：`<家目录>/.cache/opencode/models.json`。
/// 只有免费模型那条检查需要它（免得联网重新拉）；必须在改家目录之前调用。
fn user_models_json() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let path = PathBuf::from(home)
        .join(".cache")
        .join("opencode")
        .join("models.json");
    path.is_file().then_some(path)
}

fn dummy_mcp_spec() -> McpServerSpec {
    // 工具桥不在这个测试里：挂一个起不来的 zssh MCP 就行，本测只关心本机 read 工具。
    McpServerSpec {
        name: "zssh".to_string(),
        command: vec!["zssh-mcp-not-present".to_string()],
        environment: vec![
            ("ZSSH_BRIDGE_PORT".to_string(), "1".to_string()),
            ("ZSSH_BRIDGE_TOKEN".to_string(), "unused".to_string()),
        ],
    }
}

/// 错误文本是不是权限规则拒绝的（而不是「文件不存在」之类别的失败）。
fn is_permission_denial(text: &str) -> bool {
    text.contains("specified a rule which prevents") || text.contains("rejected permission")
}

fn contains_marker(text: &str, marker: &str) -> bool {
    text.contains(marker)
}

// ---------------------------------------------------------------------------
// 隔离环境
// ---------------------------------------------------------------------------

/// 一个隔离的 opencode 环境：所有目录都在系统临时目录下的一个新目录里，用完整个删掉。
struct Isolation {
    guard: common::TestDir,
    workspace: PathBuf,
    outside: PathBuf,
    home: PathBuf,
    cfg_home: PathBuf,
}

impl Isolation {
    /// 建目录并把本进程的家目录 / XDG 目录指过去（以及它起的 opencode 子进程）。
    /// 调用者要持有 [`ENV_LOCK`]。
    fn create(tag: &str, models_json: Option<&Path>) -> Isolation {
        let guard = common::TestDir::new(tag);
        let root = guard.to_path_buf();
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        let home = root.join("home");
        let cfg_home = root.join("xdg-config");
        let data_home = root.join("xdg-data");
        let cache_home = root.join("xdg-cache");
        let state_home = root.join("xdg-state");
        for dir in [&workspace, &outside, &home, &data_home, &state_home] {
            std::fs::create_dir_all(dir).expect("建隔离目录");
        }
        if let Some(src) = models_json {
            let dst_dir = cache_home.join("opencode");
            std::fs::create_dir_all(&dst_dir).expect("建缓存目录");
            std::fs::copy(src, dst_dir.join("models.json")).expect("复制 models.json");
        }
        std::env::set_var("XDG_DATA_HOME", &data_home);
        std::env::set_var("XDG_CACHE_HOME", &cache_home);
        std::env::set_var("XDG_STATE_HOME", &state_home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("HOME", &home);
        // opencode 默认还会兼容地扫 `~/.claude`（技能、提示）：不让它碰用户的目录。
        std::env::set_var("OPENCODE_DISABLE_CLAUDE_CODE", "1");
        std::env::set_var("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS", "1");
        std::env::set_var("OPENCODE_DISABLE_CLAUDE_CODE_PROMPT", "1");
        Isolation {
            guard,
            workspace,
            outside,
            home,
            cfg_home,
        }
    }

    fn root(&self) -> PathBuf {
        self.guard.to_path_buf()
    }

    /// 四个带随机标记的金丝雀文件 + 对应的四种读法。
    fn plant_canaries(&self) -> Vec<Case> {
        let m_rel = random_marker("REL");
        let m_abs = random_marker("ABS");
        let m_home = random_marker("HOME");
        let m_up = random_marker("UP");
        std::fs::write(self.workspace.join("canary_rel.txt"), &m_rel).unwrap();
        std::fs::write(self.outside.join("canary_abs.txt"), &m_abs).unwrap();
        std::fs::write(self.home.join("canary_home.txt"), &m_home).unwrap();
        std::fs::write(self.outside.join("canary_up.txt"), &m_up).unwrap();
        vec![
            Case {
                name: "工作目录内相对路径",
                path: "canary_rel.txt".to_string(),
                marker: m_rel,
                control_reads: true,
            },
            Case {
                name: "工作目录外绝对路径",
                path: self.outside.join("canary_abs.txt").display().to_string(),
                marker: m_abs,
                control_reads: true,
            },
            Case {
                name: "~ 开头的路径",
                path: "~/canary_home.txt".to_string(),
                marker: m_home,
                control_reads: false,
            },
            Case {
                name: ".. 跳出工作目录",
                path: "../outside/canary_up.txt".to_string(),
                marker: m_up,
                control_reads: true,
            },
        ]
    }

    fn start_options(&self, opencode: &Path, config_content: &str) -> StartOptions {
        StartOptions {
            install: AgentInstall {
                path: opencode.display().to_string(),
                version: "e2e".to_string(),
                source: DiscoverySource::Setting,
                needs_shell: true,
                auto: false,
                too_old: false,
                skipped_older: None,
            },
            workspace: self.workspace.clone(),
            config_home: self.cfg_home.clone(),
            config_content: config_content.to_string(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        }
    }
}

// ---------------------------------------------------------------------------
// 假模型：本机回环的 OpenAI 兼容服务
// ---------------------------------------------------------------------------

/// 假模型收到的一次请求里和本测试有关的部分。
#[derive(Debug, Clone, Default)]
struct RequestLog {
    /// 请求里的工具列表有没有 `read`。
    offers_read: bool,
    /// 最后一条消息的角色（`tool` 说明是上一次工具调用的结果回传）。
    last_role: String,
    /// 最后一条是工具结果时，它的文本。
    tool_result: String,
}

#[derive(Default)]
struct FakeState {
    /// 下一次 read 工具调用要读的路径。
    read_path: String,
    requests: Vec<RequestLog>,
}

struct FakeModel {
    port: u16,
    state: Arc<Mutex<FakeState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeModel {
    async fn start() -> FakeModel {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("绑定回环端口");
        let port = listener.local_addr().expect("本地地址").port();
        let state = Arc::new(Mutex::new(FakeState::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(stream, shared).await;
                });
            }
        });
        FakeModel { port, state, task }
    }

    fn set_read_path(&self, path: &str) {
        self.state.lock().unwrap().read_path = path.to_string();
    }

    /// 现在已经收到几次请求（配合 [`FakeModel::since`] 取某一段时间内的请求）。
    fn mark(&self) -> usize {
        self.state.lock().unwrap().requests.len()
    }

    fn since(&self, mark: usize) -> Vec<RequestLog> {
        self.state.lock().unwrap().requests[mark..].to_vec()
    }

    /// 写进 opencode 配置的自定义服务商（`provider` 键）。
    fn provider_config(&self) -> Value {
        json!({
            "provider": {
                FAKE_PROVIDER: {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": FAKE_PROVIDER,
                    "options": {
                        "baseURL": format!("http://127.0.0.1:{}/v1", self.port),
                        "apiKey": "unused",
                    },
                    "models": {
                        FAKE_MODEL: {
                            "name": FAKE_MODEL,
                            "tool_call": true,
                            "limit": { "context": 100000, "output": 4096 },
                        }
                    }
                }
            }
        })
    }

    fn model_ref() -> String {
        format!("{FAKE_PROVIDER}/{FAKE_MODEL}")
    }
}

/// 读一条完整的 HTTP 请求（头 + 按 Content-Length 读的正文）。
async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[header_end..header_end + length].to_vec();
    Ok((method, target, body))
}

async fn serve_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<FakeState>>,
) -> std::io::Result<()> {
    let (method, target, body) = read_http_request(&mut stream).await?;
    let path = target.split('?').next().unwrap_or("");
    let (status, content_type, payload) = if method == "POST" && path.ends_with("/chat/completions")
    {
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let read_path = state.lock().unwrap().read_path.clone();
        let (sse, log) = fake_reply(&request, &read_path);
        state.lock().unwrap().requests.push(log);
        ("200 OK", "text/event-stream", sse)
    } else {
        ("404 Not Found", "application/json", "{}".to_string())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// 一个 `chat/completions` 流式响应块。
fn sse_chunk(delta: Value, finish: Option<&str>) -> String {
    let chunk = json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": FAKE_MODEL,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    });
    format!("data: {chunk}\n\n")
}

/// 假模型怎么回：请求带 `read` 工具、而且最后一条不是工具结果 → 对 `read_path` 发一次 `read`
/// 调用；其余（标题生成之类没有工具的请求、工具结果回传之后）→ 回一句话。
fn fake_reply(request: &Value, read_path: &str) -> (String, RequestLog) {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    let last_role = last["role"].as_str().unwrap_or("").to_string();
    let offers_read = request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .any(|tool| tool["function"]["name"].as_str() == Some("read"))
        })
        .unwrap_or(false);
    let tool_result = if last_role == "tool" {
        match &last["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    } else {
        String::new()
    };

    let mut sse = String::new();
    if offers_read && last_role != "tool" {
        let arguments = json!({ "filePath": read_path }).to_string();
        sse.push_str(&sse_chunk(
            json!({
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_fake_read",
                    "type": "function",
                    "function": { "name": "read", "arguments": arguments },
                }],
            }),
            None,
        ));
        sse.push_str(&sse_chunk(json!({}), Some("tool_calls")));
    } else {
        sse.push_str(&sse_chunk(
            json!({ "role": "assistant", "content": "ok" }),
            None,
        ));
        sse.push_str(&sse_chunk(json!({}), Some("stop")));
    }
    let usage = json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": FAKE_MODEL,
        "choices": [],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
    });
    sse.push_str(&format!("data: {usage}\n\n"));
    sse.push_str("data: [DONE]\n\n");
    (
        sse,
        RequestLog {
            offers_read,
            last_role,
            tool_result,
        },
    )
}

// ---------------------------------------------------------------------------
// 驱动真实 opencode
// ---------------------------------------------------------------------------

/// 在 Z-SSH 的对话里发一条消息，收事件到回合结束，统计 read 调用。
async fn run_turn_checked(
    conv: &Arc<OpenCodeConversation>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    prompt: &str,
    model: &str,
    marker: &str,
) -> Outcome {
    let mut out = Outcome::default();
    if let Err(err) = conv.send(prompt, Some(model)).await {
        out.turn_error = Some(format!("发送失败：{err}"));
        return out;
    }
    let mut read_call_ids: Vec<String> = Vec::new();
    let mut reply = String::new();
    let mut thinking = String::new();
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            out.turn_error = Some("回合超时".to_string());
            let _ = conv.stop().await;
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AgentEvent::TextDelta { text })) => {
                out.text_chars += text.chars().count();
                // 增量会把标记切成几段，要拼起来再找。
                reply.push_str(&text);
                out.leaked |= contains_marker(&reply, marker);
            }
            Ok(Some(AgentEvent::ReasoningDelta { text })) => {
                thinking.push_str(&text);
                out.leaked |= contains_marker(&thinking, marker);
            }
            Ok(Some(AgentEvent::ToolStart {
                call_id,
                tool,
                input,
            })) => {
                out.leaked |= contains_marker(&input.to_string(), marker);
                if tool == "read" {
                    read_call_ids.push(call_id);
                }
            }
            Ok(Some(AgentEvent::ToolEnd {
                call_id,
                tool,
                output,
                error,
            })) => {
                out.leaked |= contains_marker(&output, marker);
                if tool == "read" || read_call_ids.contains(&call_id) {
                    // 只数「真的执行了或被权限拒绝」的调用；参数不合法之类的失败既没读到也不是拒绝，不算。
                    if !error {
                        out.read_calls += 1;
                    } else if is_permission_denial(&output) {
                        out.read_calls += 1;
                        out.read_rejected += 1;
                    }
                }
            }
            Ok(Some(AgentEvent::TurnEnd)) => break,
            Ok(Some(AgentEvent::Error { message })) => {
                out.leaked |= contains_marker(&message, marker);
                out.turn_error = Some(message);
                // 出错后服务端还会收尾（TurnEnd）：等一下，免得下一条消息撞上「上一轮还没结束」。
                let _ = tokio::time::timeout(Duration::from_secs(15), async {
                    while let Some(event) = rx.recv().await {
                        if matches!(event, AgentEvent::TurnEnd) {
                            break;
                        }
                    }
                })
                .await;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                out.turn_error = Some("回合超时".to_string());
                let _ = conv.stop().await;
                break;
            }
        }
    }
    out
}

/// `opencode run`：同一份配置，没有 Z-SSH 的系统提示。
async fn run_cli_case(
    opencode: &Path,
    config_content: &str,
    workspace: &Path,
    model: &str,
    prompt: &str,
    marker: &str,
) -> Outcome {
    let mut out = Outcome::default();
    let mut cmd = zssh_core::ai::agent::discovery::build_command(opencode);
    cmd.arg("run")
        .arg("--format")
        .arg("json")
        .arg("-m")
        .arg(model)
        .arg(prompt)
        .arg("--dir")
        .arg(workspace)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("PWD", workspace)
        .env("OPENCODE_CONFIG_CONTENT", config_content)
        .env_remove("OPENCODE_CONFIG")
        .env_remove("OPENCODE_CONFIG_DIR")
        .env_remove("OPENCODE_PERMISSION")
        .kill_on_drop(true);
    zssh_core::ai::agent::discovery::hide_console(&mut cmd);
    let output = match tokio::time::timeout(RUN_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            out.turn_error = Some(format!("启动失败：{err}"));
            return out;
        }
        Err(_) => {
            out.turn_error = Some("命令超时".to_string());
            return out;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    out.leaked = contains_marker(&stdout, marker) || contains_marker(&stderr, marker);
    // 事件是一行一个 JSON；只数 read 工具的调用和它们的状态，不看内容。
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"] == "error" {
            out.turn_error = Some("opencode 报告了错误事件".to_string());
        }
        let part = &value["part"];
        if part["tool"] == "read" {
            let status = part["state"]["status"].as_str().unwrap_or("");
            // 只数真的执行了（completed）或被权限拒绝的调用。
            if status == "completed" {
                out.read_calls += 1;
            } else if status == "error" {
                let text = part["state"]["error"].as_str().unwrap_or("");
                if is_permission_denial(text) {
                    out.read_calls += 1;
                    out.read_rejected += 1;
                }
            }
        }
    }
    if out.turn_error.is_none() && !output.status.success() {
        out.turn_error = Some(format!("命令退出码 {:?}", output.status.code()));
    }
    out
}

// ---------------------------------------------------------------------------
// 不联网也不起 opencode 的单元检查：判定逻辑和假模型协议
// ---------------------------------------------------------------------------

#[test]
fn judge_rejects_unverified_runs_instead_of_passing_them() {
    let rejected = Outcome {
        read_calls: 1,
        read_rejected: 1,
        ..Outcome::default()
    };
    assert!(judge(&rejected, true, false).is_ok());

    // 0 次调用（模型没调 / 回合失败）：不能算通过
    let none = Outcome::default();
    assert!(judge(&none, true, false).unwrap_err().contains("未验证"));
    // 超时 / 回合报错：即使看到过一次被拒的调用，也是「未验证」
    let timed_out = Outcome {
        turn_error: Some("命令超时".to_string()),
        ..Outcome::default()
    };
    assert!(judge(&timed_out, true, false)
        .unwrap_err()
        .contains("未验证"));
    let errored = Outcome {
        turn_error: Some("回合超时".to_string()),
        read_calls: 1,
        read_rejected: 1,
        ..Outcome::default()
    };
    assert!(judge(&errored, true, false).unwrap_err().contains("未验证"));
    // read 不在工具列表里，调用多半是 invalid 工具：不算数
    assert!(judge(&rejected, false, false)
        .unwrap_err()
        .contains("未验证"));
    // 有调用没被拒
    let allowed = Outcome {
        read_calls: 2,
        read_rejected: 1,
        ..Outcome::default()
    };
    assert!(judge(&allowed, true, false).unwrap_err().contains("没被拒"));
    // 泄露：事件流里看到，或者假模型从回传的工具结果里看到
    let leaked = Outcome {
        leaked: true,
        read_calls: 1,
        read_rejected: 1,
        ..Outcome::default()
    };
    assert!(judge(&leaked, true, false).unwrap_err().contains("标记"));
    assert!(judge(&rejected, true, true).unwrap_err().contains("标记"));
}

#[test]
fn fake_model_forces_one_read_call_then_answers_after_the_tool_result() {
    let tools = json!([
        { "type": "function", "function": { "name": "bash" } },
        { "type": "function", "function": { "name": "read" } },
    ]);
    // 带 read 工具、最后一条是用户消息：发一次对指定路径的 read 调用
    let request = json!({
        "stream": true,
        "tools": tools,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let (sse, log) = fake_reply(&request, "some/file.txt");
    assert!(log.offers_read);
    assert_eq!(log.last_role, "user");
    assert!(sse.contains("\"name\":\"read\""));
    assert!(sse.contains("some/file.txt"));
    assert!(sse.contains("tool_calls"));
    assert!(sse.trim_end().ends_with("data: [DONE]"));

    // 工具结果回传之后：不再调工具，回一句话，并记下工具结果文本
    let after = json!({
        "stream": true,
        "tools": tools,
        "messages": [
            { "role": "user", "content": "hi" },
            { "role": "assistant", "tool_calls": [] },
            { "role": "tool", "content": "denied by rule" },
        ],
    });
    let (sse, log) = fake_reply(&after, "some/file.txt");
    assert_eq!(log.last_role, "tool");
    assert_eq!(log.tool_result, "denied by rule");
    assert!(!sse.contains("\"name\":\"read\""));
    assert!(sse.contains("\"content\":\"ok\""));

    // 没有工具的请求（比如标题生成）：只回一句话；也如实记下 read 不在列表里
    let title = json!({ "stream": true, "messages": [{ "role": "user", "content": "x" }] });
    let (sse, log) = fake_reply(&title, "some/file.txt");
    assert!(!log.offers_read);
    assert!(!sse.contains("tool_calls"));
}

#[tokio::test]
async fn fake_model_serves_chat_completions_over_loopback() {
    let model = FakeModel::start().await;
    model.set_read_path("p/q.txt");
    let body = json!({
        "stream": true,
        "tools": [{ "type": "function", "function": { "name": "read" } }],
        "messages": [{ "role": "user", "content": "hi" }],
    })
    .to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", model.port)).await.unwrap();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("p/q.txt"));
    let seen = model.since(0);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].offers_read);

    // 别的路径一律 404，不记请求
    let mut stream = TcpStream::connect(("127.0.0.1", model.port)).await.unwrap();
    stream
        .write_all(b"GET /v1/models HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 404"));
    assert_eq!(model.mark(), 1);
}

// ---------------------------------------------------------------------------
// 联网（实际是本机回环 + 真实 opencode）的实测
// ---------------------------------------------------------------------------

/// 门槛都满足就返回 opencode 的路径，否则打印「跳过」。
fn live_gate(name: &str) -> Option<PathBuf> {
    if !common::full_live(name) {
        return None;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 {name}：设置 ZSSH_AGENT_E2E=1 才跑（会真的起 opencode）");
        return None;
    }
    let found = find_opencode();
    if found.is_none() {
        println!("跳过 {name}：找不到 opencode");
    }
    found
}

#[tokio::test]
async fn opencode_read_is_rejected_for_every_path_form() {
    const NAME: &str = "opencode_read_is_rejected_for_every_path_form";
    let Some(opencode) = live_gate(NAME) else {
        return;
    };
    let _env = ENV_LOCK.lock().await;
    let iso = Isolation::create("oc-read-fake", None);
    let cases = iso.plant_canaries();
    let fake = FakeModel::start().await;
    let model_ref = FakeModel::model_ref();

    // 生产同款配置：假模型当作用户自建的服务商并进去（和真实用户的自建服务商同一条路径）。
    let config_content = build_config_content(&dummy_mcp_spec(), &[], &fake.provider_config());
    let runtime = AgentRuntime::start(
        iso.start_options(&opencode, &config_content),
        DEFAULT_IDLE_TIMEOUT,
    )
    .await
    .expect("启动 opencode");

    let mut failures: Vec<String> = Vec::new();

    // ---------- A. Z-SSH 生产路径（带工具禁用表和系统提示），每种路径一个新对话 ----------
    for (index, case) in cases.iter().enumerate() {
        fake.set_read_path(&case.path);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let conv = runtime
            .open_conversation(format!("{index:032x}"), tx)
            .await
            .expect("开对话");
        let mark = fake.mark();
        let out = run_turn_checked(
            &conv,
            &mut rx,
            "Read the file with the read tool, then tell me what it says.",
            &model_ref,
            &case.marker,
        )
        .await;
        let seen = fake.since(mark);
        let offered = seen.iter().any(|r| r.offers_read);
        let server_leak = seen
            .iter()
            .any(|r| contains_marker(&r.tool_result, &case.marker));
        let verdict = judge(&out, offered, server_leak);
        println!(
            "Z-SSH 对话 / {}：标记泄露={}（假模型侧 {}），read 调用 {} 次（被拒 {} 次），read 在工具列表里={}，回合错误={}，结论：{}",
            case.name,
            out.leaked,
            server_leak,
            out.read_calls,
            out.read_rejected,
            offered,
            out.turn_error.is_some(),
            verdict.as_ref().map_or_else(|e| e.clone(), |_| "已拒绝".to_string())
        );
        if let Err(reason) = verdict {
            failures.push(format!("Z-SSH 对话 / {}：{reason}", case.name));
        }
        drop(conv);
    }

    // ---------- B. `opencode run`，同一份配置，没有系统提示 ----------
    for case in &cases {
        fake.set_read_path(&case.path);
        let mark = fake.mark();
        let out = run_cli_case(
            &opencode,
            &config_content,
            &iso.workspace,
            &model_ref,
            "Read the file with the read tool, then tell me what it says.",
            &case.marker,
        )
        .await;
        let seen = fake.since(mark);
        let offered = seen.iter().any(|r| r.offers_read);
        let server_leak = seen
            .iter()
            .any(|r| contains_marker(&r.tool_result, &case.marker));
        let verdict = judge(&out, offered, server_leak);
        println!(
            "opencode run / {}：标记泄露={}（假模型侧 {}），read 调用 {} 次（被拒 {} 次），read 在工具列表里={}，回合错误={}，结论：{}",
            case.name,
            out.leaked,
            server_leak,
            out.read_calls,
            out.read_rejected,
            offered,
            out.turn_error.is_some(),
            verdict.as_ref().map_or_else(|e| e.clone(), |_| "已拒绝".to_string())
        );
        if let Err(reason) = verdict {
            failures.push(format!("opencode run / {}：{reason}", case.name));
        }
    }

    // ---------- C. 对照组：把 read 放开，同样的做法必须读到标记（证明上面的检查是灵的）----------
    let mut open: Value = serde_json::from_str(&config_content).expect("配置是 JSON");
    open["permission"]["read"] = json!("allow");
    open["permission"]["external_directory"] = json!("allow");
    let open_content = open.to_string();
    for case in &cases {
        fake.set_read_path(&case.path);
        let mark = fake.mark();
        let out = run_cli_case(
            &opencode,
            &open_content,
            &iso.workspace,
            &model_ref,
            "Read the file with the read tool, then tell me what it says.",
            &case.marker,
        )
        .await;
        let server_leak = fake
            .since(mark)
            .iter()
            .any(|r| contains_marker(&r.tool_result, &case.marker));
        let detected = out.leaked || server_leak;
        println!(
            "对照组（read 放开）/ {}：读到标记={}（假模型侧 {}），read 调用 {} 次，回合错误={}{}",
            case.name,
            out.leaked,
            server_leak,
            out.read_calls,
            out.turn_error.is_some(),
            if case.control_reads {
                ""
            } else {
                "（opencode 的 read 不展开 ~，读不到是正常的）"
            }
        );
        if !detected && case.control_reads {
            failures.push(format!(
                "对照组（read 放开）/ {}：没读到标记，上面的检查可能不灵",
                case.name
            ));
        }
    }

    runtime.shutdown().await;
    drop(fake);
    let root = iso.root();
    drop(iso);
    assert!(!root.exists(), "隔离目录要删干净");
    assert!(failures.is_empty(), "实测失败：{failures:#?}");
}

/// 免费模型能正常回复（独立检查，需要网络和免费模型在当前地区可用）。
/// 这一条只验证服务可用，和 read 的安全检查无关；网络不通 / 地区不可用时跳过。
#[tokio::test]
async fn opencode_free_models_reply_normally() {
    const NAME: &str = "opencode_free_models_reply_normally";
    let Some(opencode) = live_gate(NAME) else {
        return;
    };
    let _env = ENV_LOCK.lock().await;
    // 先取用户缓存里的公开模型清单（只读），再把家目录指到隔离目录。
    let models_json = user_models_json();
    let iso = Isolation::create("oc-free-reply", models_json.as_deref());

    let config_content = build_config_content(&dummy_mcp_spec(), &[], &json!({}));
    let runtime = AgentRuntime::start(
        iso.start_options(&opencode, &config_content),
        DEFAULT_IDLE_TIMEOUT,
    )
    .await
    .expect("启动 opencode");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let conv = runtime
        .open_conversation("a".repeat(32), tx)
        .await
        .expect("开对话");

    let catalog = conv.models().await.expect("取模型");
    let free_models: Vec<String> = match std::env::var("ZSSH_AGENT_MODEL") {
        Ok(model) if !model.trim().is_empty() => vec![model],
        _ => catalog
            .models
            .iter()
            .filter(|m| m.id.starts_with("opencode/") && m.id.contains("-free"))
            .filter(|m| m.id.matches('/').count() == 1)
            .map(|m| m.id.clone())
            .take(4)
            .collect(),
    };
    if free_models.is_empty() {
        println!("跳过 {NAME}：隔离环境里没有可用的免费模型");
        runtime.shutdown().await;
        return;
    }
    assert!(
        catalog
            .models
            .iter()
            .filter(|m| m.id.contains("-free"))
            .all(|m| !m.unavailable),
        "免费模型不能再标不可用"
    );
    println!(
        "免费模型 {} 个可选，实测用 {:?}（隔离环境里没有任何服务商凭据，没有非免费模型可跑）",
        catalog
            .models
            .iter()
            .filter(|m| m.id.contains("-free"))
            .count(),
        free_models
    );

    let mut replied = false;
    for model in &free_models {
        let pong = random_marker("PONG");
        let out = run_turn_checked(
            &conv,
            &mut rx,
            &format!("Reply with exactly this token and nothing else: {pong}"),
            model,
            &pong,
        )
        .await;
        println!(
            "[{model}] 免费模型正常回复：{}（正文 {} 字，回合错误={}）",
            if out.leaked { "是" } else { "否" },
            out.text_chars,
            out.turn_error.is_some()
        );
        let error = out.turn_error.as_deref().unwrap_or("");
        if error.contains("not available in your country") {
            // 个别免费模型有地区限制，和 Z-SSH 无关：换下一个。
            println!("[{model}] 跳过：这个模型在当前网络所在地区不可用");
            continue;
        }
        if error.contains("1.18.0 or newer is required") {
            println!("跳过 {NAME}：opencode 版本低于 1.18，免费服务不可用");
            runtime.shutdown().await;
            return;
        }
        assert!(
            out.leaked && out.turn_error.is_none(),
            "{model}：免费模型没有正常回复（回合错误={:?}）",
            out.turn_error
        );
        replied = true;
        break;
    }

    drop(conv);
    runtime.shutdown().await;
    drop(iso);
    if !replied {
        println!("跳过 {NAME}：试过的免费模型在当前网络都不可用");
    }
}
