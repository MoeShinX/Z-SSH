// Adapted from Netcatty electron/bridges/aiBridge/sdk/claudeDriver.cjs
// (translateClaudeMessage), rewritten in Rust for Z-SSH.
//! Claude Code 的 stream-json 消息 → 统一事件（[`AgentEvent`]）。
//!
//! 实测形状（2.1.300；样本在 `fixtures/claude/claude_turn.jsonl`）：
//! - `system/init`：会话 id、工具清单、MCP 服务清单（驱动侧要读）；
//! - `stream_event`：`content_block_delta` 里 `text_delta` / `thinking_delta` 是流式
//!   正文和思考；
//! - `assistant`：完整消息，工具调用以 `tool_use` 块为准（正文块不再发，流式里发过）；
//! - `user`：`tool_result` 块是工具结果（带 `is_error`）；
//! - `result`：回合结束（`subtype` / `is_error` / `result` 文本）。
//!
//! 工具名 `mcp__zssh__run_command` 归一成 `zssh_run_command`，和 opencode / codex
//! 两侧的写法一致，面板的友好名 / 主机名 / 退出码解析原样复用。

use std::collections::HashSet;

use serde_json::Value;

use super::super::AgentEvent;

/// 翻译状态（一个对话一份）。
#[derive(Debug, Default)]
pub struct TranslateState {
    /// 已经发过 ToolStart 的工具调用 id。
    tool_started: HashSet<String>,
    /// 已经发过 ToolEnd 的工具调用 id。
    tool_finished: HashSet<String>,
}

/// 一个回合的累积（`result` 时取用）。
#[derive(Debug, Default)]
pub struct TurnStats {
    /// 出现过正文或工具调用（空回复判断用）。
    pub content: bool,
    /// 中断过（用户点停止；由驱动在发中断时置位）。
    pub aborted: bool,
}

/// `system/init` / `result` 里的会话 id。
pub fn session_id_of(message: &Value) -> Option<&str> {
    message["session_id"].as_str().filter(|id| !id.is_empty())
}

/// `system/init` 里的 MCP 服务清单（只加载了 zssh 的核对用）。
pub fn mcp_servers_of(message: &Value) -> Vec<(String, String)> {
    message["mcp_servers"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let name = entry["name"].as_str()?;
                    let status = entry["status"].as_str().unwrap_or("");
                    Some((name.to_string(), status.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 把一条 stream-json 消息翻译成统一事件。
pub fn translate(
    message: &Value,
    state: &mut TranslateState,
    turn: &mut TurnStats,
) -> Vec<AgentEvent> {
    let kind = message["type"].as_str().unwrap_or("");
    match kind {
        "stream_event" => stream_event(&message["event"], turn),
        "assistant" => assistant_events(message, state, turn),
        "user" => user_events(message, state),
        "result" => result_events(message, turn),
        // system/init、system/status、system/thinking_tokens、rate_limit_event 之类
        // 不产生统一事件（驱动自己读 session_id / mcp_servers）。
        _ => Vec::new(),
    }
}

fn stream_event(event: &Value, turn: &mut TurnStats) -> Vec<AgentEvent> {
    if event["type"].as_str() != Some("content_block_delta") {
        return Vec::new();
    }
    let delta = &event["delta"];
    match delta["type"].as_str().unwrap_or("") {
        "text_delta" => {
            let text = delta["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return Vec::new();
            }
            turn.content = true;
            vec![AgentEvent::TextDelta {
                text: text.to_string(),
            }]
        }
        "thinking_delta" => {
            let text = delta["thinking"].as_str().unwrap_or("");
            if text.is_empty() {
                return Vec::new();
            }
            vec![AgentEvent::ReasoningDelta {
                text: text.to_string(),
            }]
        }
        _ => Vec::new(),
    }
}

fn assistant_events(
    message: &Value,
    state: &mut TranslateState,
    turn: &mut TurnStats,
) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let Some(blocks) = message["message"]["content"].as_array() else {
        return out;
    };
    for block in blocks {
        if block["type"].as_str() != Some("tool_use") {
            // 正文块已经在 stream_event 里流过了，不重复发。
            continue;
        }
        let Some(id) = block["id"].as_str() else {
            continue;
        };
        if state.tool_started.contains(id) {
            continue;
        }
        state.tool_started.insert(id.to_string());
        turn.content = true;
        out.push(AgentEvent::ToolStart {
            call_id: id.to_string(),
            tool: normalize_tool_name(block["name"].as_str().unwrap_or("")),
            input: block["input"].clone(),
        });
    }
    out
}

fn user_events(message: &Value, state: &mut TranslateState) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let Some(blocks) = message["message"]["content"].as_array() else {
        return out;
    };
    for block in blocks {
        if block["type"].as_str() != Some("tool_result") {
            continue;
        }
        let Some(id) = block["tool_use_id"].as_str() else {
            continue;
        };
        if state.tool_finished.contains(id) {
            continue;
        }
        state.tool_finished.insert(id.to_string());
        let output = match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        out.push(AgentEvent::ToolEnd {
            call_id: id.to_string(),
            tool: String::new(),
            output,
            error: block["is_error"].as_bool() == Some(true),
        });
    }
    out
}

fn result_events(message: &Value, turn: &mut TurnStats) -> Vec<AgentEvent> {
    let is_error = message["is_error"].as_bool() == Some(true);
    if !is_error {
        return vec![AgentEvent::TurnEnd];
    }
    if turn.aborted {
        // 我们自己发的中断：这是"停止"，不是失败。
        return vec![AgentEvent::TurnEnd];
    }
    let text = message["result"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or("claude 回合失败");
    vec![
        AgentEvent::Error {
            message: text.to_string(),
        },
        AgentEvent::TurnEnd,
    ]
}

/// `mcp__zssh__run_command` → `zssh_run_command`（非 MCP 的名字原样）。
fn normalize_tool_name(name: &str) -> String {
    let parts: Vec<&str> = name.split("__").collect();
    if name.starts_with("mcp__") && parts.len() >= 3 {
        format!("{}_{}", parts[1], parts[2..].join("__"))
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 真实录制的回合（开发时实测的版本：Claude Code 2.1.300）。
    const SAMPLE: &str = include_str!("../fixtures/claude/claude_turn.jsonl");

    #[test]
    fn translates_a_recorded_turn_end_to_end() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let mut events = Vec::new();
        let mut sessions = Vec::new();
        let mut mcp = Vec::new();
        // 样本里是两段回合：第一段正常结束，第二段被中断（result =
        // error_during_execution）。第一段先按正常回合处理；到第一个 result 之后
        // 把 aborted 置位（模拟用户点了停止）再处理第二段。
        let mut interrupted_phase = false;
        for line in SAMPLE.lines().filter(|l| !l.trim().is_empty()) {
            let message: Value = serde_json::from_str(line).expect("样本是合法 JSON");
            if message["type"] == "system" && message["subtype"] == "init" {
                if let Some(id) = session_id_of(&message) {
                    sessions.push(id.to_string());
                }
                mcp = mcp_servers_of(&message);
            }
            if interrupted_phase {
                turn.aborted = true;
            }
            events.extend(translate(&message, &mut state, &mut turn));
            if message["type"] == "result" {
                interrupted_phase = true;
            }
        }
        // 会话 id 记得住（每个 init 一份，值相同）。
        assert!(!sessions.is_empty(), "样本里有 system/init");
        assert!(sessions.iter().all(|id| id == &sessions[0]));
        // 工具清单核对：只加载了 zssh。
        assert_eq!(
            mcp,
            vec![("zssh".to_string(), "connected".to_string())],
            "样本里只允许 zssh 一个 MCP 服务"
        );

        // 正文流式拼起来 = 模型的回复。
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(text.contains("记住了"), "正文要流式出来：{text:?}");

        // 两段回合各一个 TurnEnd；被中断的那段不算错误（没有 Error）。
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnEnd))
                .count(),
            2,
            "样本里有两个回合"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::Error { .. })),
            "正常结束 + 用户中断，都不该出 Error"
        );
        assert!(turn.content, "回合里有内容");
    }

    #[test]
    fn tool_calls_and_results_pair_up_with_normalized_names() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let assistant = json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "text", "text": "先查一下" },
                { "type": "tool_use", "id": "toolu_1", "name": "mcp__zssh__list_targets", "input": { "capability": "abc" } }
            ]}
        });
        let events = translate(&assistant, &mut state, &mut turn);
        assert_eq!(events.len(), 1, "只发工具调用，正文不重复发");
        match &events[0] {
            AgentEvent::ToolStart {
                call_id,
                tool,
                input,
            } => {
                assert_eq!(call_id, "toolu_1");
                assert_eq!(tool, "zssh_list_targets", "MCP 名字要归一");
                assert_eq!(input["capability"], "abc");
            }
            other => panic!("应是 ToolStart：{other:?}"),
        }
        // 同一个 assistant 消息再来一次（重发）不重复发。
        assert!(translate(&assistant, &mut state, &mut turn).is_empty());

        let user = json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "is_error": false,
                  "content": "exit_code: 0\nZSSH_CLAUDE_42" }
            ]}
        });
        let events = translate(&user, &mut state, &mut turn);
        match &events[0] {
            AgentEvent::ToolEnd {
                call_id,
                output,
                error,
                ..
            } => {
                assert_eq!(call_id, "toolu_1");
                assert!(output.contains("ZSSH_CLAUDE_42"));
                assert!(!error);
            }
            other => panic!("应是 ToolEnd：{other:?}"),
        }
        // 失败的工具结果。
        let failed = json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "toolu_2", "is_error": true,
                  "content": [{ "type": "text", "text": "命令被拒绝" }] }
            ]}
        });
        let events = translate(&failed, &mut state, &mut turn);
        match &events[0] {
            AgentEvent::ToolEnd { error, output, .. } => {
                assert!(error);
                assert!(
                    output.contains("命令被拒绝"),
                    "结构化内容要序列化出来：{output}"
                );
            }
            other => panic!("应是 ToolEnd：{other:?}"),
        }
    }

    #[test]
    fn thinking_deltas_map_to_reasoning() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let event = json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta", "index": 0,
                       "delta": { "type": "thinking_delta", "thinking": "先看看工具" } }
        });
        assert_eq!(
            translate(&event, &mut state, &mut turn),
            vec![AgentEvent::ReasoningDelta {
                text: "先看看工具".into()
            }]
        );
    }

    #[test]
    fn failed_turns_report_error_but_interrupts_do_not() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let failed = json!({ "type": "result", "subtype": "error_during_execution",
                             "is_error": true, "result": "API 出错" });
        let events = translate(&failed, &mut state, &mut turn);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgentEvent::Error { message } if message == "API 出错"));
        assert!(matches!(&events[1], AgentEvent::TurnEnd));

        // 用户点停止（驱动把 aborted 置位）时的 error 结果按"停止"处理。
        let mut state = TranslateState::default();
        let mut turn = TurnStats {
            aborted: true,
            ..Default::default()
        };
        let interrupted = json!({ "type": "result", "subtype": "error_during_execution",
                                  "is_error": true, "result": "[Request interrupted by user]" });
        assert_eq!(
            translate(&interrupted, &mut state, &mut turn),
            vec![AgentEvent::TurnEnd]
        );
    }

    #[test]
    fn normal_result_is_just_turn_end() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let result =
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": "OK" });
        assert_eq!(
            translate(&result, &mut state, &mut turn),
            vec![AgentEvent::TurnEnd]
        );
    }
}
