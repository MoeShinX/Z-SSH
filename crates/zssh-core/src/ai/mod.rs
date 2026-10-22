//! AI 工具桥（AI-1）与智能体驱动层（AI-2）：危险命令判断、终端环形缓冲、
//! 工具桥服务端与协议；以及外部编码智能体（opencode）的受控进程封装。
//!
//! 业务逻辑都在这一层，`src-tauri` 只做转接；审批和会话归属通过 trait 交给
//! 调用方，core 不依赖 Tauri。

pub mod agent;
pub mod bridge;
pub mod command_text;
pub mod display;
pub mod files;
pub mod history;
pub mod host_memory;
pub mod protocol;
pub mod pty;
pub mod readonly;
pub mod runs;
pub mod safety;
pub mod shell_kind;
pub mod terminal;
