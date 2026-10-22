// Adapted from Netcatty electron/bridges/aiBridge/sdk/codexDriver.cjs (translateCodexEvent
// and isCodexRetryableStreamError), rewritten in Rust for Z-SSH.
//! codex app-server 通知 → 统一事件（[`AgentEvent`]）。
//!
//! 输入是服务端通知（`method` + `params`），输出零到多个统一事件。纯函数 + 一点
//! 状态（同一个 thread 的通知按顺序喂进来），拿录制的真实回合当单测样本
//! （`fixtures/codex/codex_turn_events.json`）。
//!
//! 实测形状（0.162.1）：
//! - 正文增量 `item/agentMessage/delta`；思考增量 `item/reasoning/summaryTextDelta`；
//! - `item/started` / `item/completed` 带完整的 item（`mcpToolCall` 有 server / tool /
//!   arguments / result / status，`commandExecution` 有 command / aggregatedOutput /
//!   exitCode）；
//! - 回合结束 `turn/completed`（`turn.status` 是 `completed` / `failed` / `interrupted`）；
//! - **可恢复的 `error` 通知**（断线重连：`Reconnecting...` / `retrying N/M`，或者带
//!   `willRetry: true`）：只是提醒，不是回合失败，不能当错误发给界面（Netcatty
//!   `translateCodexEvent` 记过这个坑）。
//!
//! 工具名拼成 `<server>_<tool>`（例如 `zssh_run_command`），和 opencode 侧的写法
//! 一致，面板的友好名 / 主机名 / 退出码解析原样复用。

use std::collections::HashMap;

use serde_json::Value;

use super::super::AgentEvent;

/// 翻译状态：同一个 thread 共用一个实例。
#[derive(Debug, Default)]
pub struct TranslateState {
    /// 已经发过 ToolStart 的 item id。
    tool_started: HashMap<String, ()>,
    /// 已经发过 ToolEnd 的 item id。
    tool_finished: HashMap<String, ()>,
    /// 正文 item 已经发出去的字符数（completed 会带全文，靠它只发尾巴）。
    text_sent: HashMap<String, String>,
    /// 思考 item 已经发出去的文本（同样用来只发尾巴）。
    reasoning_sent: HashMap<String, String>,
}

/// 一个回合的状态累积（`turn/completed` 时取用）。
#[derive(Debug, Default)]
pub struct TurnStats {
    /// 出现过正文或工具调用（空回复判断用）。
    pub content: bool,
    /// 中断过（用户点停止）。
    pub aborted: bool,
    /// 出过致命错误。
    pub failed: bool,
}

/// 把一条通知翻译成统一事件。
pub fn translate(
    method: &str,
    params: &Value,
    state: &mut TranslateState,
    turn: &mut TurnStats,
) -> Vec<AgentEvent> {
    match method {
        "item/agentMessage/delta" => {
            let delta = params["delta"].as_str().unwrap_or("");
            if delta.is_empty() {
                return Vec::new();
            }
            turn.content = true;
            note_delta(&mut state.text_sent, item_id_of(params), delta);
            vec![AgentEvent::TextDelta {
                text: delta.to_string(),
            }]
        }
        "item/reasoning/summaryTextDelta" => {
            let delta = params["delta"].as_str().unwrap_or("");
            if delta.is_empty() {
                return Vec::new();
            }
            note_delta(&mut state.reasoning_sent, item_id_of(params), delta);
            vec![AgentEvent::ReasoningDelta {
                text: delta.to_string(),
            }]
        }
        "item/started" => item_events(&params["item"], false, state, turn),
        "item/completed" => item_events(&params["item"], true, state, turn),
        "turn/completed" => {
            let status = params["turn"]["status"].as_str().unwrap_or("completed");
            match status {
                "failed" => {
                    turn.failed = true;
                    let message = params["turn"]["error"]["message"]
                        .as_str()
                        .unwrap_or("codex 回合失败");
                    vec![
                        AgentEvent::Error {
                            message: message.to_string(),
                        },
                        AgentEvent::TurnEnd,
                    ]
                }
                "interrupted" | "cancelled" | "canceled" => {
                    turn.aborted = true;
                    vec![AgentEvent::TurnEnd]
                }
                _ => vec![AgentEvent::TurnEnd],
            }
        }
        "error" => {
            if error_is_retryable(params) {
                // 断线重连里的重试提示：回合还在继续，不发错误。
                return Vec::new();
            }
            turn.failed = true;
            let message = params["error"]["message"].as_str().unwrap_or("codex 出错");
            vec![AgentEvent::Error {
                message: message.to_string(),
            }]
        }
        // warning / thread/tokenUsage/updated / item/commandExecution/outputDelta
        // 之类不产生统一事件（用量在统一事件里没有对应项，先不发）。
        _ => Vec::new(),
    }
}

/// 可恢复的错误：断线重连过程中的提示（Netcatty `isCodexRetryableStreamError`）。
pub fn error_is_retryable(params: &Value) -> bool {
    if params["willRetry"].as_bool() == Some(true) {
        return true;
    }
    if params["willRetry"].as_bool() == Some(false) {
        return false;
    }
    let message = params["error"]["message"]
        .as_str()
        .or_else(|| params["message"].as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    message.contains("reconnecting") || message.contains("retrying")
}

fn item_events(
    item: &Value,
    completed: bool,
    state: &mut TranslateState,
    turn: &mut TurnStats,
) -> Vec<AgentEvent> {
    let item_type = item["type"].as_str().unwrap_or("");
    let item_id = item["id"].as_str().unwrap_or("");
    if item_id.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    match item_type {
        "agentMessage" => {
            if completed {
                turn.content = true;
                let full = item["text"].as_str().unwrap_or("");
                let sent = state.text_sent.get(item_id).cloned().unwrap_or_default();
                let suffix = suffix_after(&sent, full);
                if !suffix.is_empty() {
                    out.push(AgentEvent::TextDelta { text: suffix });
                }
                // 记成"已发全文"：重复的 completed 通知不会重发。
                state
                    .text_sent
                    .insert(item_id.to_string(), full.to_string());
            }
        }
        "reasoning" => {
            if completed {
                let full = summary_text(item);
                let sent = state
                    .reasoning_sent
                    .get(item_id)
                    .cloned()
                    .unwrap_or_default();
                let suffix = suffix_after(&sent, &full);
                if !suffix.is_empty() {
                    out.push(AgentEvent::ReasoningDelta { text: suffix });
                }
                state.reasoning_sent.insert(item_id.to_string(), full);
            }
        }
        "mcpToolCall" => {
            let tool = tool_name(
                item["server"].as_str().unwrap_or("mcp"),
                item["tool"].as_str().unwrap_or("tool"),
            );
            emit_tool_start(item_id, &tool, &item["arguments"], state, &mut out, turn);
            if completed {
                emit_tool_end(
                    item_id,
                    &tool,
                    &mcp_tool_output(item),
                    item_failed(item),
                    state,
                    &mut out,
                );
            }
        }
        "commandExecution" => {
            // 本机命令：正常配置下不会出现（工具被禁），真出现也如实翻译。
            let tool = "codex.command".to_string();
            let input = serde_json::json!({
                "command": item["command"].clone(),
                "cwd": item["cwd"].clone(),
            });
            emit_tool_start(item_id, &tool, &input, state, &mut out, turn);
            if completed {
                let mut output = item["aggregatedOutput"].as_str().unwrap_or("").to_string();
                if let Some(code) = item["exitCode"].as_i64() {
                    output.push_str(&format!("\nexit_code: {code}"));
                }
                let failed = item_failed(item);
                emit_tool_end(item_id, &tool, &output, failed, state, &mut out);
            }
        }
        "fileChange" => {
            let tool = "codex.file_change".to_string();
            emit_tool_start(item_id, &tool, &item["changes"], state, &mut out, turn);
            if completed {
                let output = item["changes"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|c| c["path"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let failed = item_failed(item);
                emit_tool_end(item_id, &tool, &output, failed, state, &mut out);
            }
        }
        "webSearch" => {
            let tool = "codex.web_search".to_string();
            let input = serde_json::json!({ "query": item["query"].clone() });
            emit_tool_start(item_id, &tool, &input, state, &mut out, turn);
            if completed {
                emit_tool_end(item_id, &tool, "", item_failed(item), state, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn emit_tool_start(
    item_id: &str,
    tool: &str,
    input: &Value,
    state: &mut TranslateState,
    out: &mut Vec<AgentEvent>,
    turn: &mut TurnStats,
) {
    if state.tool_started.contains_key(item_id) {
        return;
    }
    state.tool_started.insert(item_id.to_string(), ());
    turn.content = true;
    out.push(AgentEvent::ToolStart {
        call_id: item_id.to_string(),
        tool: tool.to_string(),
        input: input.clone(),
    });
}

fn emit_tool_end(
    item_id: &str,
    tool: &str,
    output: &str,
    error: bool,
    state: &mut TranslateState,
    out: &mut Vec<AgentEvent>,
) {
    if state.tool_finished.contains_key(item_id) {
        return;
    }
    state.tool_finished.insert(item_id.to_string(), ());
    out.push(AgentEvent::ToolEnd {
        call_id: item_id.to_string(),
        tool: tool.to_string(),
        output: output.to_string(),
        error,
    });
}

/// `<server>_<tool>`：和 opencode 侧的工具名写法一致（面板原样复用）。
fn tool_name(server: &str, tool: &str) -> String {
    format!("{server}_{tool}")
}

/// item 是不是失败状态。
fn item_failed(item: &Value) -> bool {
    if item["status"].as_str() == Some("failed") {
        return true;
    }
    item.get("error").is_some_and(|e| !e.is_null())
}

/// mcpToolCall 的输出文本：优先 error.message，否则把 result.content 里的文本拼起来。
fn mcp_tool_output(item: &Value) -> String {
    if let Some(message) = item["error"]["message"].as_str() {
        if !message.is_empty() {
            return message.to_string();
        }
    }
    let content = &item["result"]["content"];
    if let Some(list) = content.as_array() {
        let mut parts = Vec::new();
        for entry in list {
            if let Some(text) = entry["text"].as_str() {
                parts.push(text.to_string());
            } else if !entry.is_null() {
                parts.push(entry.to_string());
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    if let Some(structured) = item["result"]["structuredContent"].as_str() {
        return structured.to_string();
    }
    String::new()
}

/// reasoning 的最终文本：summary 数组拼起来。
fn summary_text(item: &Value) -> String {
    item["summary"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 已经发过 `sent` 的情况下，`full` 里还没发出去的部分。
///
/// codex 有时会重发累计文本（不是增量）。如果 `full` 不以 `sent` 开头（形状不认识），
/// 就整个当新内容发，宁可重复也不能丢。
fn suffix_after(sent: &str, full: &str) -> String {
    if full.is_empty() {
        return String::new();
    }
    if sent.is_empty() {
        return full.to_string();
    }
    match full.strip_prefix(sent) {
        Some(rest) => rest.to_string(),
        None => full.to_string(),
    }
}

/// 记录增量（completed 的合计口径和增量累加要对上）。
fn note_delta(map: &mut HashMap<String, String>, item_id: &str, delta: &str) {
    if item_id.is_empty() {
        return;
    }
    map.entry(item_id.to_string()).or_default().push_str(delta);
}

/// 一个通知带不带 item id（正文 / 思考增量要按 id 记账）。
pub fn item_id_of(params: &Value) -> &str {
    params["itemId"].as_str().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn drain(events: &Value, state: &mut TranslateState, turn: &mut TurnStats) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        for event in events.as_array().expect("数组") {
            let method = event["method"].as_str().unwrap_or("");
            let params = &event["params"];
            out.extend(translate(method, params, state, turn));
        }
        out
    }

    /// 用真实录制的回合（独立数据目录里跑的只读回合）做整体校验。
    #[test]
    fn translates_a_recorded_turn_end_to_end() {
        let raw: Value =
            serde_json::from_str(include_str!("../fixtures/codex/codex_turn_events.json"))
                .expect("样本是合法 JSON");
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let events = drain(&raw, &mut state, &mut turn);

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            text.contains("list_targets"),
            "正文里要有模型的回复：{text:?}"
        );

        // 工具调用：zssh_list_targets，失败（工具桥没开），输出里有连不上的说明。
        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolStart { tool, .. } => Some(tool.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec!["zssh_list_targets".to_string()]);
        let ends: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolEnd {
                    tool,
                    error,
                    output,
                    ..
                } => Some((tool.clone(), *error, output.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1, "一次工具调用只能结束一次");
        let (tool, error, output) = &ends[0];
        assert_eq!(tool, "zssh_list_targets");
        assert!(error, "工具桥没开，应该是失败：{output:?}");

        // 回合结束恰好一次；没有错误事件（这个回合是正常结束）。
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::TurnEnd))
                .count(),
            1
        );
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
        assert!(turn.content && !turn.failed && !turn.aborted);
    }

    #[test]
    fn retryable_errors_do_not_become_error_events() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let reconnecting = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "stream error: Reconnecting... (attempt 2)" }
        });
        assert!(translate("error", &reconnecting, &mut state, &mut turn).is_empty());
        assert!(!turn.failed);

        // 没有 willRetry 字段时按文案判断。
        let retrying = json!({
            "threadId": "t",
            "error": { "message": "retrying 1/5" }
        });
        assert!(translate("error", &retrying, &mut state, &mut turn).is_empty());

        // 真错误照常发。
        let fatal = json!({
            "threadId": "t", "willRetry": false,
            "error": { "message": "usage limit reached" }
        });
        let events = translate("error", &fatal, &mut state, &mut turn);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::Error { message } if message == "usage limit reached")
        );
        assert!(turn.failed);
    }

    #[test]
    fn agent_message_completed_only_sends_the_suffix() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let started = json!({ "item": { "id": "m1", "type": "agentMessage", "text": "" } });
        assert!(translate("item/started", &started, &mut state, &mut turn).is_empty());
        let delta = json!({ "itemId": "m1", "delta": "你好" });
        let events = translate("item/agentMessage/delta", &delta, &mut state, &mut turn);
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                text: "你好".into()
            }]
        );
        // 完成后重发全文：只补尾巴。
        let completed =
            json!({ "item": { "id": "m1", "type": "agentMessage", "text": "你好，世界" } });
        let events = translate("item/completed", &completed, &mut state, &mut turn);
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                text: "，世界".into()
            }]
        );
        // 再完成一次（重复通知）：不重复发。
        let events = translate("item/completed", &completed, &mut state, &mut turn);
        assert!(events.is_empty());
    }

    #[test]
    fn reasoning_summary_deltas_stream_and_close_out() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let delta = json!({ "itemId": "r1", "delta": "**检查工具**" });
        let events = translate(
            "item/reasoning/summaryTextDelta",
            &delta,
            &mut state,
            &mut turn,
        );
        assert_eq!(
            events,
            vec![AgentEvent::ReasoningDelta {
                text: "**检查工具**".into()
            }]
        );
        let completed = json!({ "item": { "id": "r1", "type": "reasoning", "summary": ["**检查工具**", "再看一眼"] } });
        let events = translate("item/completed", &completed, &mut state, &mut turn);
        assert_eq!(
            events,
            vec![AgentEvent::ReasoningDelta {
                text: "\n再看一眼".into()
            }]
        );
    }

    #[test]
    fn turn_completed_failed_reports_error_then_end() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let failed = json!({ "turn": { "id": "u1", "status": "failed", "error": { "message": "模型拒绝" } } });
        let events = translate("turn/completed", &failed, &mut state, &mut turn);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgentEvent::Error { message } if message == "模型拒绝"));
        assert!(matches!(&events[1], AgentEvent::TurnEnd));
        assert!(turn.failed);

        // 用户停止：只是回合结束。
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let interrupted = json!({ "turn": { "id": "u2", "status": "interrupted" } });
        let events = translate("turn/completed", &interrupted, &mut state, &mut turn);
        assert_eq!(events, vec![AgentEvent::TurnEnd]);
        assert!(turn.aborted && !turn.failed);
    }

    #[test]
    fn mcp_tool_output_prefers_error_then_content() {
        let with_error = json!({
            "error": { "message": "工具执行失败" },
            "result": { "content": [{ "type": "text", "text": "ignored" }] }
        });
        assert_eq!(mcp_tool_output(&with_error), "工具执行失败");

        let with_content = json!({
            "error": null,
            "result": { "content": [
                { "type": "text", "text": "exit_code: 0" },
                { "type": "text", "text": "第二段" }
            ] }
        });
        assert_eq!(mcp_tool_output(&with_content), "exit_code: 0\n第二段");

        let empty = json!({ "error": null, "result": null });
        assert_eq!(mcp_tool_output(&empty), "");
    }

    #[test]
    fn command_execution_items_translate_with_exit_code() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let started = json!({ "item": {
            "id": "c1", "type": "commandExecution", "command": "whoami",
            "cwd": "C:\\data", "status": "inProgress"
        }});
        let events = translate("item/started", &started, &mut state, &mut turn);
        assert!(
            matches!(&events[0], AgentEvent::ToolStart { tool, .. } if tool == "codex.command")
        );
        let completed = json!({ "item": {
            "id": "c1", "type": "commandExecution", "command": "whoami",
            "cwd": "C:\\data", "status": "completed",
            "aggregatedOutput": "user\\name\r\n", "exitCode": 0
        }});
        let events = translate("item/completed", &completed, &mut state, &mut turn);
        match &events[0] {
            AgentEvent::ToolEnd { output, error, .. } => {
                assert!(output.contains("exit_code: 0"));
                assert!(!error);
            }
            other => panic!("应是 ToolEnd：{other:?}"),
        }
    }
}
