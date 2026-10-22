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
/// 目标 ID 有两种：
/// - 已经连上的会话：就是会话 ID（`sess-N`）；
/// - 用户 @ 进来、还没连上的主机：`host-<host_id>`（AI 第一次用它时自动连接）。
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
