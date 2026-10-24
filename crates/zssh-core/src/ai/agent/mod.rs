//! 智能体驱动层（AI-2）：把外部编码智能体（opencode，之后还有 codex 等）跑成受控
//! 进程，统一收口为「启动/停止、新建对话、发送消息、停止回合、统一的流式事件」。
//!
//! 进程启动、SSE 翻译、配置注入、发现和下载都在这层做，`src-tauri` 只做转接。

pub mod claude;
pub mod codex;
pub mod config;
pub mod discovery;
pub mod download;
pub mod opencode;
pub mod process;
pub mod retry;
pub mod text;
pub mod translate;

use serde::{Deserialize, Serialize};

use crate::ssh::BoxFuture;

/// 驱动的统一流式事件。
///
/// 各驱动（opencode / 以后的 codex、Claude Code）都把它自己的事件流翻译成这组事件，
/// 上层（面板）只认这一套。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 助手正文增量。
    TextDelta { text: String },
    /// 思考（推理）增量；模型没有就不发。
    ReasoningDelta { text: String },
    /// 工具调用开始。
    ToolStart {
        /// 和对应的 ToolEnd 配对。
        call_id: String,
        tool: String,
        input: serde_json::Value,
    },
    /// 工具调用结束（带结果）。
    ToolEnd {
        call_id: String,
        tool: String,
        /// 正常时是输出；出错时是错误文本。
        output: String,
        error: bool,
    },
    /// 一个回合结束（回复完、被停止、或出错后服务端都会收尾）。
    TurnEnd,
    /// 出错。错误文本直接面向用户展示。
    Error { message: String },
    /// 智能体遇到额度用完 / 限流 / 服务过载之类的错误，正在带退避地自动重试（opencode 的
    /// `session.status` 重试状态、Claude Code 的 `system/api_retry`、Codex 的 `willRetry`
    /// 错误通知）。界面据此在「思考中」旁给提示，免得像卡死了。同一回合最多放行
    /// [`retry::MAX_RETRIES`] 次，超限由各驱动停掉这一轮。
    Retrying {
        /// 第几次重试（从 1 开始）。
        attempt: u32,
        /// 已经友好化的原因，直接面向用户展示。
        reason: String,
        /// 距离下次重试大约还有几秒；对方没给或格式不认识就是 None。
        next_in_secs: Option<u64>,
    },
    /// 续接历史会话失败（智能体那边的会话已经不在），已经退成新会话继续：界面提示
    /// "之前的上下文没有带给 AI，只供回看"。
    ResumeFailed,
}

/// 一个可选模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModel {
    /// 请求时用的 id，形如 `provider/model`。
    pub id: String,
    /// 界面上展示的名字。
    pub name: String,
    /// 已知不可用（目前没有哪个智能体会设它；保留给以后）。
    /// 界面上给这类模型加"不可用"标记；能不能真用仍以后端返回为准。
    #[serde(default)]
    pub unavailable: bool,
}

/// 可用模型列表。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelCatalog {
    pub models: Vec<AgentModel>,
    /// 智能体自己的默认模型（可能不在列表里）。
    pub default: Option<String>,
    /// "默认"实际对应哪个模型的显示文字（例如 `opus · high`）。
    ///
    /// 和 `default` 分开：`default` 是选"默认"时发出去的 id（Claude Code 是占位的
    /// `default`，opencode 没配 `model` 时是 None），这里只给界面看。只在能从智能体
    /// 自己的配置 / 接口确定时才填，读不到就是 None（界面只显示"默认模型"，不猜）。
    #[serde(default)]
    pub default_label: Option<String>,
}

/// 驱动出错。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 没找到可用的智能体。
    #[error("没有找到智能体：{0}")]
    NotFound(String),
    /// 进程没起来 / 没在超时内就绪。
    #[error("智能体启动失败：{0}")]
    StartFailed(String),
    /// HTTP / SSE / JSON-RPC 通信出错。
    #[error("智能体通信出错：{0}")]
    Transport(String),
    /// 上一个回合还没结束。
    #[error("上一轮回复还没结束")]
    Busy,
    #[error("{0}")]
    Other(String),
}

/// 一个对话的驱动句柄。opencode 实现它；AI-3/AI-4 的驱动也实现它。
pub trait AgentConversation: Send + Sync {
    /// 智能体侧的会话 id（opencode 就是 `ses_...`）。
    fn session_id(&self) -> &str;

    /// 续接用的会话标识（存进历史记录）。默认就是 [`Self::session_id`]；Claude Code 的
    /// 会话 id 是第一次发消息后才知道的，它自己覆盖。
    fn resume_id(&self) -> Option<String> {
        Some(self.session_id().to_string())
    }

    /// 查可用模型。
    fn models<'a>(&'a self) -> BoxFuture<'a, Result<AgentModelCatalog, AgentError>>;

    /// 发一条用户消息并启动一个回合。`model` 形如 `provider/model`，None 用默认。
    fn send<'a>(
        &'a self,
        text: &'a str,
        model: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), AgentError>>;

    /// 停止当前回合。没有回合在跑时是空操作。
    fn stop<'a>(&'a self) -> BoxFuture<'a, Result<(), AgentError>>;

    /// 结束对话：停掉事件流并杀掉进程（含子进程树）。
    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()>;
}
