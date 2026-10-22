//! 历史会话续接实测（R11）：真实 `claude -p`，不需要测试服务器。
//!
//! 前置条件：本机装了 Claude Code 且已登录、`ZSSH_LIVE_FULL=1`、`ZSSH_AGENT_E2E=1`
//! （真的调用模型）。不满足时打印"跳过"直接返回。
//!
//! 覆盖：
//! 1. 方案 A：第一个对话让模型记住一个暗号，关掉；用保存的会话 id 新建对话，模型还记得；
//! 2. 方案 B：拿一个不存在的会话 id 续接，驱动发 `ResumeFailed` 并用新会话把消息发出去
//!    （模型照常回复，不报错）。

mod common;

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;

use zssh_core::ai::agent::claude::{ClaudeConversation, StartOptions};
use zssh_core::ai::agent::config::McpServerSpec;
use zssh_core::ai::agent::discovery::{self, AgentInstall, CLAUDE};
use zssh_core::ai::agent::{AgentConversation, AgentEvent};

const TURN_TIMEOUT: Duration = Duration::from_secs(180);

fn gate(name: &str) -> bool {
    if !common::full_live(name) {
        return false;
    }
    if std::env::var("ZSSH_AGENT_E2E").ok().as_deref() != Some("1") {
        println!("跳过 {name}：设置 ZSSH_AGENT_E2E=1 才跑（会真的调用模型）");
        return false;
    }
    true
}

async fn find_claude() -> Option<AgentInstall> {
    if let Ok(path) = std::env::var("ZSSH_AGENT_CLAUDE") {
        let resolved = discovery::resolve_launch_path(&PathBuf::from(path));
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
    let dir = common::TestDir::new("history-claude-discovery");
    match discovery::discover(&CLAUDE, None, &dir).await {
        Ok(Some(install)) if !install.too_old => Some(install),
        _ => None,
    }
}

async fn conversation(
    install: &AgentInstall,
    workspace: &std::path::Path,
    resume: Option<String>,
) -> (
    std::sync::Arc<ClaudeConversation>,
    mpsc::UnboundedReceiver<AgentEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let conversation = ClaudeConversation::create(
        StartOptions {
            install: install.clone(),
            workspace: workspace.to_path_buf(),
            // 这个用例不调工具：MCP 服务连不上不影响（只核对名字）。
            mcp: McpServerSpec::zssh(std::path::Path::new("zssh-mcp-unused"), 1, "token"),
            capability: "0123456789abcdef0123456789abcdef".to_string(),
            events: tx,
            extra_env: Vec::new(),
            resume_session_id: resume,
        },
        Duration::from_secs(600),
    )
    .await
    .expect("建对话");
    (conversation, rx)
}

/// 发一条消息，收齐这一回合：返回（正文，有没有收到 ResumeFailed）。
async fn turn(
    conversation: &ClaudeConversation,
    rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    text: &str,
) -> (String, bool) {
    conversation.send(text, None).await.expect("发消息");
    let mut reply = String::new();
    let mut resume_failed = false;
    loop {
        match tokio::time::timeout(TURN_TIMEOUT, rx.recv()).await {
            Ok(Some(AgentEvent::TextDelta { text })) => reply.push_str(&text),
            Ok(Some(AgentEvent::ResumeFailed)) => resume_failed = true,
            Ok(Some(AgentEvent::Error { message })) => panic!("回合出错：{message}"),
            Ok(Some(AgentEvent::TurnEnd)) => return (reply, resume_failed),
            Ok(Some(_)) => {}
            Ok(None) => panic!("事件流断了"),
            Err(_) => panic!("回合超时"),
        }
    }
}

#[tokio::test]
async fn claude_history_resume_keeps_context_or_falls_back() {
    let name = "claude_history_resume_keeps_context_or_falls_back";
    if !gate(name) {
        return;
    }
    let Some(install) = find_claude().await else {
        println!("跳过 {name}：找不到可用的 Claude Code");
        return;
    };
    let workspace = common::TestDir::new("history-claude-ws");

    // 方案 A：记住暗号 → 关掉 → 用保存的会话 id 续接。
    let (first, mut rx) = conversation(&install, &workspace, None).await;
    let (_, failed) = turn(
        &first,
        &mut rx,
        "请记住暗号 ZSSH-HISTORY-7731，只回复\"记住了\"。",
    )
    .await;
    assert!(!failed);
    let saved = first.resume_id().expect("第一回合后要拿到会话 id");
    first.shutdown().await;

    let (second, mut rx) = conversation(&install, &workspace, Some(saved)).await;
    let (reply, failed) = turn(&second, &mut rx, "刚才的暗号是什么？只回复暗号本身。").await;
    second.shutdown().await;
    assert!(!failed, "会话还在，不该走续接失败");
    assert!(reply.contains("7731"), "续接后要记得上文：{reply:?}");

    // 方案 B：会话 id 不存在 → ResumeFailed + 新会话照常回复。
    let bogus = "0b9e1c1e-0000-4000-8000-000000000000".to_string();
    let (third, mut rx) = conversation(&install, &workspace, Some(bogus.clone())).await;
    let (reply, failed) = turn(&third, &mut rx, "只回复两个字：你好").await;
    assert!(failed, "续不上要发 ResumeFailed");
    assert!(!reply.trim().is_empty(), "退回新会话后要照常回复");
    assert_ne!(third.resume_id().as_deref(), Some(bogus.as_str()));
    third.shutdown().await;
}
