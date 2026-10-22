//! 工具桥的线上协议（行分隔 JSON，自定义）。
//!
//! 连接流程：
//!
//! 1. `zssh-mcp.exe` 连上 `127.0.0.1:<port>`，第一行必须是
//!    `{"token": "<64 位十六进制>"}`；令牌不对直接断开。这个令牌只证明"是 Z-SSH
//!    启动的那个 opencode 进程连过来的"（连接层）。
//! 2. 之后每行一条请求：`{"id": <u64>, "method": "<名字>", "capability": "<32 位>",
//!    "params": {...}}`。`capability` 是**对话层**的能力密钥：一个对话一个随机
//!    128 位密钥，工具桥按它找到对应对话的会话表。缺失 / 过期一律拒绝。
//! 3. 每行一条响应：`{"id": <u64>, "result": {...}}` 或
//!    `{"id": <u64>, "error": {"message": "..."}}`。
//!
//! 同一条连接上可以同时有多条请求在飞（R-8 第 10 节）：响应靠 `id` 对应，
//! 可能乱序回来。桥按每条请求的 `capability` 独立处理，一条卡在审批上不会堵住
//! 同一连接上的其它请求。
//!
//! 方法：`list_targets`、`run_command`、`read_terminal`，以及 M3-4 的文件工具
//! `list_dir`、`read_file`、`write_file`、`stat`（只操作目标列表里服务器上的文件）。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ai::safety::DangerLevel;

pub const METHOD_LIST_TARGETS: &str = "list_targets";
pub const METHOD_RUN_COMMAND: &str = "run_command";
pub const METHOD_READ_TERMINAL: &str = "read_terminal";
pub const METHOD_LIST_DIR: &str = "list_dir";
pub const METHOD_READ_FILE: &str = "read_file";
pub const METHOD_WRITE_FILE: &str = "write_file";
pub const METHOD_STAT: &str = "stat";
pub const METHOD_REMEMBER_HOST: &str = "remember_host";

/// `run_command` 不带 `timeout_sec` 时的默认超时（秒）。
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;
/// `run_command` 允许的最长超时（秒）。
pub const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
/// `read_terminal` 不带 `max_lines` 时的默认行数。
pub const DEFAULT_READ_LINES: usize = 200;
/// `read_terminal` 允许的最多行数。
pub const MAX_READ_LINES: usize = 2000;
/// 对话能力密钥的字节数（128 位）与十六进制位数。
pub const CAPABILITY_BYTES: usize = 16;
pub const CAPABILITY_HEX_LEN: usize = CAPABILITY_BYTES * 2;

/// 响应结果里的执行编号字段名（成功时在 `result` 对象里，失败时在 `error` 里）。
pub const EXEC_ID_FIELD: &str = "exec_id";

/// 会带执行编号的方法：需要解析目标的工具（`list_targets` / `remember_host` 不带）。
pub fn method_carries_exec_id(method: &str) -> bool {
    matches!(
        method,
        METHOD_RUN_COMMAND
            | METHOD_READ_TERMINAL
            | METHOD_LIST_DIR
            | METHOD_READ_FILE
            | METHOD_WRITE_FILE
            | METHOD_STAT
    )
}

const EXEC_MARKER_PREFIX: &str = "[zssh exec ";

/// 追加在工具结果文本末尾的一行固定格式标记，例如 `[zssh exec 3f2a1b-7]`。
///
/// 智能体的工具结束事件里只有这段文本，界面靠这一行把结果对回执行层回报的那一次执行
/// （`ai_exec_target`）。编号只是关联用的，不是凭据：知道它不能做任何事。界面渲染卡片输出时
/// 会把这行去掉。
pub fn exec_marker(exec_id: &str) -> String {
    format!("{EXEC_MARKER_PREFIX}{exec_id}]")
}

/// 给文本末尾加上执行编号标记（独占一行）。
pub fn with_exec_marker(text: &str, exec_id: &str) -> String {
    let marker = exec_marker(exec_id);
    if text.is_empty() {
        marker
    } else if text.ends_with('\n') {
        format!("{text}{marker}")
    } else {
        format!("{text}\n{marker}")
    }
}

/// 第一条消息：连接令牌。
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenLine {
    pub token: String,
}

/// `Debug` 手写：连接令牌是共享密钥，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for TokenLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenLine").field("token", &"***").finish()
    }
}

/// 一条请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub method: String,
    /// 对话能力密钥（32 位十六进制）；连接层令牌之外的对话层鉴权。
    #[serde(default)]
    pub capability: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// 错误体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub message: String,
    /// 这次工具调用的执行编号（见 [`exec_marker`]）；没有编号的方法为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_id: Option<String>,
}

/// 一条响应，`result` 和 `error` 二选一。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// `run_command` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCommandParams {
    /// 目标 ID（`list_targets` 返回的那个）。不填时用"当前焦点"那台。
    #[serde(default)]
    pub target_id: Option<String>,
    pub command: String,
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

/// `remember_host` 的参数：这台主机的完整笔记（整份替换；空串 = 清空）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberHostParams {
    #[serde(default)]
    pub target_id: Option<String>,
    pub notes: String,
}

/// `read_terminal` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadTerminalParams {
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

/// `list_dir` / `stat` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathParams {
    #[serde(default)]
    pub target_id: Option<String>,
    pub path: String,
}

/// `read_file` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileParams {
    #[serde(default)]
    pub target_id: Option<String>,
    pub path: String,
    /// 从第几个字节开始读（默认 0）。
    #[serde(default)]
    pub offset: Option<u64>,
    /// 最多读多少字节（默认和上限都是 64KB）。
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

/// `write_file` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileParams {
    #[serde(default)]
    pub target_id: Option<String>,
    pub path: String,
    pub content: String,
    /// 只允许新建：文件已经存在就报错。
    #[serde(default)]
    pub create_only: bool,
}

/// 给智能体看的目标摘要（一个对话的目标列表里的一项）。**不含地址和凭据。**
///
/// 目标 ID 有两种（AI-SCOPE-2：一台主机一个逻辑目标）：
/// - 主机库里的主机：`host-<host_id>`，不管它现在开了几个终端、有没有连上，ID 都不变；
/// - 快速连接这类没有主机记录的会话：会话 ID（`sess-N`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSummary {
    pub id: String,
    pub host_label: String,
    pub username: String,
    pub status: String,
    /// 是不是当前焦点终端的会话。
    #[serde(default)]
    pub current: bool,
    /// 是不是用户 @ 进来的（非当前）。
    #[serde(default)]
    pub pinned: bool,
    /// 界面绑定给这个目标的会话（这台主机焦点所在 / 最近获得焦点的终端）。执行时优先用它，
    /// 没有或已经关了再找这台主机别的已连会话 / 自动连接。不给模型看（模型只认目标 ID）。
    #[serde(default, skip_serializing)]
    pub session_id: Option<String>,
}

/// @ 进来、还没连上的主机的目标 ID 前缀。
pub const HOST_TARGET_PREFIX: &str = "host-";

/// 组一个未连接主机的目标 ID。
pub fn host_target_id(host_id: i64) -> String {
    format!("{HOST_TARGET_PREFIX}{host_id}")
}

/// 解析主机目标 ID（不是这种 ID 返回 `None`）。
pub fn parse_host_target_id(target_id: &str) -> Option<i64> {
    target_id
        .strip_prefix(HOST_TARGET_PREFIX)
        .and_then(|rest| rest.parse().ok())
}

/// 命令执行位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecLocation {
    /// 直接在用户的终端里执行。
    Terminal,
    /// 后台 exec 通道。
    Background,
}

impl ExecLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecLocation::Terminal => "terminal",
            ExecLocation::Background => "background",
        }
    }
}

/// `run_command` 成功时的结果体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCommandResponse {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated_bytes: u64,
    pub stderr_truncated_bytes: u64,
    pub exit_code: Option<u32>,
    pub duration_ms: u64,
    /// 危险等级（`safe` / `dangerous`）。只用来标记：自动模式不因为危险而拦截。
    pub danger: DangerLevel,
    /// 在哪儿执行的。
    pub location: ExecLocation,
    /// 降级到后台的原因（`location == Background` 时可能有；`None` 表示本来就该后台）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// 超时（命令还在终端里跑，没有中断）。
    #[serde(default)]
    pub timed_out: bool,
}

/// `timeout_sec` 的钳制：默认 60 秒，最少 1 秒，最长 600 秒。
pub fn clamp_timeout_secs(requested: Option<u64>) -> Duration {
    let secs = requested
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
        .clamp(1, MAX_COMMAND_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// `max_lines` 的钳制：默认 200 行，最少 1 行，最多 2000 行。
pub fn clamp_max_lines(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_READ_LINES)
        .clamp(1, MAX_READ_LINES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_defaults_to_sixty_seconds() {
        assert_eq!(clamp_timeout_secs(None), Duration::from_secs(60));
    }

    #[test]
    fn timeout_is_clamped_to_one_through_six_hundred() {
        assert_eq!(clamp_timeout_secs(Some(0)), Duration::from_secs(1));
        assert_eq!(clamp_timeout_secs(Some(120)), Duration::from_secs(120));
        assert_eq!(clamp_timeout_secs(Some(600)), Duration::from_secs(600));
        assert_eq!(clamp_timeout_secs(Some(99_999)), Duration::from_secs(600));
    }

    #[test]
    fn max_lines_defaults_to_two_hundred() {
        assert_eq!(clamp_max_lines(None), 200);
    }

    #[test]
    fn max_lines_is_clamped_to_one_through_two_thousand() {
        assert_eq!(clamp_max_lines(Some(0)), 1);
        assert_eq!(clamp_max_lines(Some(50)), 50);
        assert_eq!(clamp_max_lines(Some(2000)), 2000);
        assert_eq!(clamp_max_lines(Some(usize::MAX)), 2000);
    }

    #[test]
    fn exec_marker_is_one_trailing_line() {
        assert_eq!(exec_marker("ab12-3"), "[zssh exec ab12-3]");
        assert_eq!(with_exec_marker("out", "ab12-3"), "out\n[zssh exec ab12-3]");
        assert_eq!(
            with_exec_marker("out\n", "ab12-3"),
            "out\n[zssh exec ab12-3]"
        );
        assert_eq!(with_exec_marker("", "ab12-3"), "[zssh exec ab12-3]");
    }

    #[test]
    fn only_target_tools_carry_an_exec_id() {
        for method in [
            METHOD_RUN_COMMAND,
            METHOD_READ_TERMINAL,
            METHOD_LIST_DIR,
            METHOD_READ_FILE,
            METHOD_WRITE_FILE,
            METHOD_STAT,
        ] {
            assert!(method_carries_exec_id(method), "{method}");
        }
        assert!(!method_carries_exec_id(METHOD_LIST_TARGETS));
        assert!(!method_carries_exec_id(METHOD_REMEMBER_HOST));
        assert!(!method_carries_exec_id("nope"));
    }

    #[test]
    fn token_line_debug_hides_the_connection_token() {
        // 连接层令牌是共享密钥，Debug 输出不能带它（R-8 第 11 节）。
        let line = TokenLine {
            token: "s3cret-connection-token".to_owned(),
        };
        let debug = format!("{line:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("***"), "{debug}");
    }
}
