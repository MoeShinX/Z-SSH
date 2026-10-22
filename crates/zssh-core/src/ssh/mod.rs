//! SSH 连接与 shell 通道。

pub mod auth;
pub mod connection;
pub mod cwd;
pub mod dialer;
pub mod flow;
pub mod forward;
pub mod interact;
pub mod keygen;
pub mod known_hosts;
pub mod os_probe;
pub mod pool;
pub mod session_log;
pub mod shell;
pub mod stats;
pub mod stream;

pub use auth::{AuthConfig, KeySource, RememberedSecret};
pub use connection::{
    keepalive_interval, set_keepalive_interval, ChainHop, ConnectParams, Connection, ExecCapture,
    ServerKey, EXEC_MAX_STREAM_BYTES,
};
pub use cwd::{parse_probe_output, pick_shell, probe_command, probe_shell_cwds, ShellCwd};
pub use dialer::{connect_direct, BoxedStream, ByteStream, ProxyConfig, HOP_TIMEOUT};
pub use flow::FlowGate;
pub use interact::{
    BoxFuture, HopProgress, HostKeyDecision, HostKeyInfo, HostKeyStatus, Interactor,
    NonInteractive, PromptAnswers, PromptItem, PromptKind, PromptRequest,
};
pub use known_hosts::KnownHosts;
pub use pool::{ConnectionGuard, ConnectionPool, PoolConfig, Pooled};
pub use shell::{
    SessionBuffer, ShellEvent, ShellEventStream, ShellSession, ShellWriter, DEFAULT_BUFFER_LIMIT,
};
pub use stats::{
    CpuCounters, NetCounters, NetInterfaceSpeed, PreviousSample, ProcessInfo, ServerStats,
    StatsCollector, StatsSample, COLLECT_TIMEOUT,
};
