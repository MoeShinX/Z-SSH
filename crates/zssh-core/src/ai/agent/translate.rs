//! opencode 事件 → 统一事件（[`AgentEvent`]）。
//!
//! 输入是 `GET /event` 的 SSE `data`（已 parse 成 JSON），输出零到多个统一事件。
//! 纯函数 + 一点状态，方便拿录制样本做单测（`fixtures/opencode_sse_turn.jsonl`
//! 是 1.19.0 上真实跑出来的一整个回合）。
//!
//! 实测过的形状（1.19.0）：
//! - `message.updated`：`properties.info.{id, role}`；
//! - `message.part.updated`：`properties.part.{id, messageID, type, text?}`，
//!   工具是 `part.state.{status, input, output, error}`；重发的 part 带完整文本，
//!   靠每个 part 的已发字符数去重；
//! - `message.part.delta`：`properties.{partID, field, delta}`，正文和思考的
//!   `field` 都是 `"text"`，**靠 partID 在 part.updated 里记下的类型区分**；
//! - `session.idle` 表示回合结束；`session.error` 带 `properties.error`；
//! - `session.status` 的 `status.type == "retry"` 是 opencode 在额度用完 / 限流 / 过载时的
//!   自动重试（`attempt`、`message`、`next`=下次重试的毫秒时间戳），翻成 [`AgentEvent::Retrying`]。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::AgentEvent;

/// part 的类型（只关心这几种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Text,
    Reasoning,
    Tool,
    Other,
}

/// 翻译状态。同一个对话的事件流共用一个实例。
#[derive(Default)]
pub struct TranslateState {
    part_kinds: HashMap<String, PartKind>,
    message_roles: HashMap<String, String>,
    /// partID → 已经对外发过的字符数（part.updated 会重发全文，靠它只发新增部分）。
    part_offsets: HashMap<String, usize>,
    /// 已经发过 ToolStart 的 callID。
    tool_started: HashSet<String>,
    /// 已经发过 ToolEnd 的 callID。
    tool_finished: HashSet<String>,
}

/// 把一个 opencode 事件翻译成统一事件。
pub fn translate_event(event: &Value, state: &mut TranslateState) -> Vec<AgentEvent> {
    let etype = event["type"].as_str().unwrap_or("");
    let props = &event["properties"];
    match etype {
        "message.updated" => {
            let info = &props["info"];
            if let (Some(id), Some(role)) = (info["id"].as_str(), info["role"].as_str()) {
                state.message_roles.insert(id.to_string(), role.to_string());
            }
            Vec::new()
        }
        "message.removed" => {
            if let Some(id) = props["messageID"].as_str() {
                state.message_roles.remove(id);
            }
            Vec::new()
        }
        "message.part.updated" => part_updated(props, state),
        "message.part.delta" => part_delta(props, state),
        "session.idle" => vec![AgentEvent::TurnEnd],
        "session.error" => vec![AgentEvent::Error {
            message: extract_error(&props["error"]),
        }],
        "session.status" => session_status(props),
        // session.updated / server.connected 之类不产生事件。
        _ => Vec::new(),
    }
}

/// `session.status`：只有 `status.type == "retry"` 产生事件（`busy` / `idle` 等照旧不产生）。
///
/// 形状（opencode 1.19.x）：`properties.status = {type:"retry", attempt, message, next}`，
/// `next` 是**下次重试的绝对时间，毫秒时间戳**（程序里是 `Clock 当前毫秒 + 退避毫秒`）。
fn session_status(props: &Value) -> Vec<AgentEvent> {
    let status = &props["status"];
    if status["type"].as_str() != Some("retry") {
        return Vec::new();
    }
    let attempt = status["attempt"]
        .as_u64()
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .unwrap_or(1)
        .max(1);
    let message = status["message"].as_str().unwrap_or("");
    vec![AgentEvent::Retrying {
        attempt,
        reason: friendly_retry_reason(message),
        next_in_secs: next_in_secs(&status["next"], now_millis()),
    }]
}

/// 当前时间（毫秒时间戳）。
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 把 `next` 换算成「还有几秒」。
///
/// opencode 给的是毫秒时间戳；这里再防御一下：数值小于 1e11 且大于 1e9 时当秒级时间戳；
/// 已经过去的时间算 0 秒；缺失、非数字、其他量级（认不出单位）一律 None，绝不 panic。
fn next_in_secs(next: &Value, now_ms: u64) -> Option<u64> {
    let raw = next.as_f64()?;
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    let at_ms = if raw >= 1.0e11 {
        raw
    } else if raw >= 1.0e9 {
        raw * 1000.0
    } else {
        return None;
    };
    let diff_ms = at_ms - now_ms as f64;
    if diff_ms <= 0.0 {
        return Some(0);
    }
    Some((diff_ms / 1000.0).ceil() as u64)
}

/// 把 opencode 重试状态里的原文换成人话（不区分大小写匹配，匹配不上就原样返回）。
pub fn friendly_retry_reason(raw: &str) -> String {
    let raw = raw.trim();
    let lower = raw.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| lower.contains(k));
    if has(&[
        "free usage exceeded",
        "usage limit",
        "quota",
        "credit",
        "insufficient",
    ]) {
        format!("免费额度用完了（opencode 提示：{raw}）。可以换一个模型，或到 opencode.ai 查看额度 / 订阅")
    } else if has(&["rate limit", "too many requests", "429"]) {
        "请求太频繁，被服务商限流了".to_string()
    } else if has(&[
        "overloaded",
        "unavailable",
        "500",
        "502",
        "503",
        "504",
        "524",
    ]) {
        "服务商那边暂时忙不过来".to_string()
    } else {
        raw.to_string()
    }
}

fn part_updated(props: &Value, state: &mut TranslateState) -> Vec<AgentEvent> {
    let part = &props["part"];
    if part.is_null() {
        return Vec::new();
    }
    let part_id = part["id"].as_str().unwrap_or("").to_string();
    if part_id.is_empty() {
        return Vec::new();
    }
    // 用户消息的 part 不发（正文 part 也是这个形状）。
    if let Some(message_id) = part["messageID"].as_str() {
        if state.message_roles.get(message_id).map(String::as_str) == Some("user") {
            return Vec::new();
        }
    }

    let kind = match part["type"].as_str().unwrap_or("") {
        "text" => PartKind::Text,
        "reasoning" => PartKind::Reasoning,
        "tool" => PartKind::Tool,
        _ => PartKind::Other,
    };
    state.part_kinds.insert(part_id.clone(), kind);

    match kind {
        PartKind::Text | PartKind::Reasoning => {
            // part.updated 上一般没有 delta（增量走 message.part.delta），但两边都兼容。
            let delta = props["delta"].as_str().map(str::to_string);
            let full = part["text"].as_str().unwrap_or("");
            let chunk = match delta {
                Some(d) if !d.is_empty() => {
                    advance_offset(state, &part_id, d.chars().count());
                    d
                }
                _ => take_suffix(state, &part_id, full),
            };
            if chunk.is_empty() {
                return Vec::new();
            }
            let ev = match kind {
                PartKind::Reasoning => AgentEvent::ReasoningDelta { text: chunk },
                _ => AgentEvent::TextDelta { text: chunk },
            };
            vec![ev]
        }
        PartKind::Tool => tool_events(part, state),
        PartKind::Other => Vec::new(),
    }
}

fn part_delta(props: &Value, state: &mut TranslateState) -> Vec<AgentEvent> {
    let part_id = props["partID"].as_str().unwrap_or("");
    let delta = props["delta"].as_str().unwrap_or("");
    if part_id.is_empty() || delta.is_empty() {
        return Vec::new();
    }
    let ev = match state
        .part_kinds
        .get(part_id)
        .copied()
        .unwrap_or(PartKind::Text)
    {
        PartKind::Reasoning => AgentEvent::ReasoningDelta {
            text: delta.to_string(),
        },
        PartKind::Tool => return Vec::new(),
        _ => AgentEvent::TextDelta {
            text: delta.to_string(),
        },
    };
    advance_offset(state, part_id, delta.chars().count());
    vec![ev]
}

fn tool_events(part: &Value, state: &mut TranslateState) -> Vec<AgentEvent> {
    let call_id = part["callID"]
        .as_str()
        .or_else(|| part["id"].as_str())
        .unwrap_or("")
        .to_string();
    if call_id.is_empty() {
        return Vec::new();
    }
    let tool = part["tool"].as_str().unwrap_or("").to_string();
    let st = &part["state"];
    let status = st["status"].as_str().unwrap_or("");
    let input = st["input"].clone();
    let has_input = match input.as_object() {
        Some(map) => !map.is_empty(),
        None => !input.is_null(),
    };

    let mut out = Vec::new();
    if !state.tool_started.contains(&call_id) {
        // pending 阶段 input 还是空的（实测 bash 是这样），等拿到参数再开卡片；
        // 已经结束了就直接发，免得漏。
        let ready = has_input || matches!(status, "completed" | "error");
        if ready {
            state.tool_started.insert(call_id.clone());
            out.push(AgentEvent::ToolStart {
                call_id: call_id.clone(),
                tool: tool.clone(),
                input,
            });
        }
    }
    if matches!(status, "completed" | "error") && !state.tool_finished.contains(&call_id) {
        state.tool_finished.insert(call_id.clone());
        let error = status == "error";
        let output = if error {
            value_to_text(&st["error"], &st["output"])
        } else {
            value_to_text(&st["output"], &Value::Null)
        };
        out.push(AgentEvent::ToolEnd {
            call_id,
            tool,
            output,
            error,
        });
    }
    out
}

/// 从 part 的完整文本里取还没发过的尾巴。
fn take_suffix(state: &mut TranslateState, part_id: &str, full: &str) -> String {
    let offset = state.part_offsets.get(part_id).copied().unwrap_or(0);
    let total = full.chars().count();
    if total <= offset {
        return String::new();
    }
    let chunk: String = full.chars().skip(offset).collect();
    state.part_offsets.insert(part_id.to_string(), total);
    chunk
}

fn advance_offset(state: &mut TranslateState, part_id: &str, chars: usize) {
    *state.part_offsets.entry(part_id.to_string()).or_insert(0) += chars;
}

/// 工具结果一般是字符串；MCP 返回结构化内容时序列化成 JSON。
fn value_to_text(primary: &Value, fallback: &Value) -> String {
    if let Some(s) = primary.as_str() {
        return s.to_string();
    }
    if !primary.is_null() {
        return primary.to_string();
    }
    if let Some(s) = fallback.as_str() {
        return s.to_string();
    }
    if !fallback.is_null() {
        return fallback.to_string();
    }
    String::new()
}

/// 从 `session.error` 的 error 字段里抠出可读文本。
///
/// 实测形状：`{"name":"ProviderAuthError","data":{"providerID":"opus","message":"..."}}`。
fn extract_error(error: &Value) -> String {
    if let Some(s) = error.as_str() {
        return s.to_string();
    }
    if let Some(msg) = error["data"]["message"].as_str() {
        if let Some(name) = error["name"].as_str() {
            if !name.is_empty() {
                return format!("{name}: {msg}");
            }
        }
        return msg.to_string();
    }
    if let Some(msg) = error["message"].as_str() {
        return msg.to_string();
    }
    if let Some(name) = error["name"].as_str() {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 录制样本：一次完整回合（思考增量 → bash 工具 pending/running/completed →
    /// 正文增量 → idle），来自 opencode 1.19.0 实跑，已脱敏。
    const SAMPLE: &str = include_str!("fixtures/opencode_sse_turn.jsonl");

    fn sample_events() -> Vec<Value> {
        SAMPLE
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("样本必须是合法 JSON"))
            .collect()
    }

    /// 把整段样本翻一遍，返回适合断言的摘要。
    #[derive(Debug, Default)]
    struct Digest {
        events: Vec<AgentEvent>,
        text: String,
        reasoning: String,
        started: Vec<(String, String)>,
        finished: Vec<(String, String, bool)>,
        turn_ends: usize,
        errors: Vec<String>,
    }

    fn digest(events: &[Value]) -> Digest {
        let mut state = TranslateState::default();
        let mut d = Digest::default();
        for ev in events {
            for out in translate_event(ev, &mut state) {
                match &out {
                    AgentEvent::TextDelta { text } => d.text.push_str(text),
                    AgentEvent::ReasoningDelta { text } => d.reasoning.push_str(text),
                    AgentEvent::ToolStart { call_id, tool, .. } => {
                        d.started.push((call_id.clone(), tool.clone()))
                    }
                    AgentEvent::ToolEnd {
                        call_id,
                        tool,
                        error,
                        ..
                    } => d.finished.push((call_id.clone(), tool.clone(), *error)),
                    AgentEvent::TurnEnd => d.turn_ends += 1,
                    AgentEvent::Error { message } => d.errors.push(message.clone()),
                    AgentEvent::ResumeFailed | AgentEvent::Retrying { .. } => {}
                }
                d.events.push(out);
            }
        }
        d
    }

    #[test]
    fn sample_turn_translates_end_to_end() {
        let d = digest(&sample_events());
        // 正文只出现一次，没有因为 part.updated 重发全文而翻倍。
        assert_eq!(d.text, "ZSSH_SAMPLE_42");
        // 思考增量拼起来非空。
        assert!(!d.reasoning.is_empty());
        // 工具开始/结束各一次，配对且成功。
        assert_eq!(d.started.len(), 1);
        assert_eq!(d.started[0].1, "bash");
        assert_eq!(d.finished.len(), 1);
        assert_eq!(d.finished[0].0, d.started[0].0);
        assert!(!d.finished[0].2);
        // 恰好一个回合结束，没有错误。
        assert_eq!(d.turn_ends, 1);
        assert!(d.errors.is_empty());
    }

    #[test]
    fn sample_tool_output_is_full() {
        let d = digest(&sample_events());
        let end = d
            .events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolEnd { output, .. } => Some(output.clone()),
                _ => None,
            })
            .expect("样本里有工具结束事件");
        assert!(end.contains("ZSSH_SAMPLE_42"), "工具输出: {end:?}");
    }

    #[test]
    fn tool_input_arrives_with_running_state() {
        // pending 时 input 是空的，不能在那时开卡片（否则卡片上没有命令）。
        let d = digest(&sample_events());
        let input = d
            .events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolStart { input, .. } => Some(input.clone()),
                _ => None,
            })
            .expect("样本里有工具开始事件");
        assert_eq!(input["command"].as_str(), Some("echo ZSSH_SAMPLE_42"));
    }

    #[test]
    fn user_message_parts_are_ignored() {
        let d = digest(&sample_events());
        // 样本里用户消息正文是「请用工具在…」，不能出现在助手正文里。
        assert!(!d.text.contains("请用工具"));
    }

    #[test]
    fn unknown_events_and_types_are_ignored() {
        let mut state = TranslateState::default();
        let events: Vec<Value> = vec![
            serde_json::json!({"type": "server.connected", "properties": {}}),
            serde_json::json!({"type": "catalog.updated", "properties": {}}),
            serde_json::json!({"type": "session.updated", "properties": {"sessionID": "ses_x"}}),
            serde_json::json!({"type": "session.status", "properties": {"sessionID": "ses_x", "status": {"type": "busy"}}}),
            serde_json::json!({"type": "message.part.updated", "properties": {"part": {"id": "prt_x", "type": "step-start", "messageID": "msg_a"}}}),
            serde_json::json!({"type": "message.part.updated", "properties": {"part": {"id": "prt_y", "type": "step-finish", "messageID": "msg_a"}}}),
        ];
        for ev in &events {
            assert!(
                translate_event(ev, &mut state).is_empty(),
                "不该产生事件: {ev}"
            );
        }
    }

    #[test]
    fn session_error_extracts_readable_message() {
        let mut state = TranslateState::default();
        let ev = serde_json::json!({
            "type": "session.error",
            "properties": {
                "sessionID": "ses_x",
                "error": {
                    "name": "ProviderAuthError",
                    "data": {"providerID": "opus", "message": "Anthropic API key is missing."}
                }
            }
        });
        let out = translate_event(&ev, &mut state);
        assert_eq!(
            out,
            vec![AgentEvent::Error {
                message: "ProviderAuthError: Anthropic API key is missing.".to_string()
            }]
        );
    }

    #[test]
    fn tool_error_uses_error_field() {
        let mut state = TranslateState::default();
        // 先记下 part 是工具。
        let part = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {
                "id": "prt_t", "messageID": "msg_a", "type": "tool", "tool": "zssh_run_command",
                "callID": "call_1", "state": {"status": "running", "input": {"command": "ls"}}
            }}
        });
        let start = translate_event(&part, &mut state);
        assert_eq!(start.len(), 1);
        let failed = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {
                "id": "prt_t", "messageID": "msg_a", "type": "tool", "tool": "zssh_run_command",
                "callID": "call_1",
                "state": {"status": "error", "input": {"command": "ls"}, "error": "boom"}
            }}
        });
        let out = translate_event(&failed, &mut state);
        assert_eq!(
            out,
            vec![AgentEvent::ToolEnd {
                call_id: "call_1".to_string(),
                tool: "zssh_run_command".to_string(),
                output: "boom".to_string(),
                error: true,
            }]
        );
    }

    #[test]
    fn pending_without_input_defers_tool_start() {
        let mut state = TranslateState::default();
        let pending = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {
                "id": "prt_t", "messageID": "msg_a", "type": "tool", "tool": "bash",
                "callID": "call_1", "state": {"status": "pending", "input": {}, "raw": ""}
            }}
        });
        assert!(translate_event(&pending, &mut state).is_empty());
        let completed = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {
                "id": "prt_t", "messageID": "msg_a", "type": "tool", "tool": "bash",
                "callID": "call_1",
                "state": {"status": "completed", "input": {}, "output": "ok"}
            }}
        });
        let out = translate_event(&completed, &mut state);
        // 结束前也没拿到参数，就直接补一次开始 + 结束。
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], AgentEvent::ToolStart { .. }));
        assert!(matches!(&out[1], AgentEvent::ToolEnd { output, .. } if output == "ok"));
    }

    #[test]
    fn delta_before_part_updated_defaults_to_text() {
        let mut state = TranslateState::default();
        let ev = serde_json::json!({
            "type": "message.part.delta",
            "properties": {"partID": "prt_unknown", "field": "text", "delta": "hi"}
        });
        assert_eq!(
            translate_event(&ev, &mut state),
            vec![AgentEvent::TextDelta {
                text: "hi".to_string()
            }]
        );
    }

    #[test]
    fn duplicate_full_text_after_deltas_is_not_resent() {
        let mut state = TranslateState::default();
        let created = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {"id": "prt_a", "messageID": "msg_a", "type": "text", "text": ""}}
        });
        assert!(translate_event(&created, &mut state).is_empty());
        let delta = serde_json::json!({
            "type": "message.part.delta",
            "properties": {"partID": "prt_a", "field": "text", "delta": "hello"}
        });
        assert_eq!(
            translate_event(&delta, &mut state),
            vec![AgentEvent::TextDelta {
                text: "hello".to_string()
            }]
        );
        let full = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {"id": "prt_a", "messageID": "msg_a", "type": "text", "text": "hello"}}
        });
        assert!(translate_event(&full, &mut state).is_empty());
        // 后续又增长了一点：只发尾巴。
        let grown = serde_json::json!({
            "type": "message.part.updated",
            "properties": {"part": {"id": "prt_a", "messageID": "msg_a", "type": "text", "text": "hello world"}}
        });
        assert_eq!(
            translate_event(&grown, &mut state),
            vec![AgentEvent::TextDelta {
                text: " world".to_string()
            }]
        );
    }

    fn retry_status(attempt: u64, message: &str, next: Value) -> Value {
        serde_json::json!({
            "type": "session.status",
            "properties": {
                "sessionID": "ses_x",
                "status": {"type": "retry", "attempt": attempt, "message": message, "next": next}
            }
        })
    }

    #[test]
    fn retry_status_becomes_retrying_event() {
        let mut state = TranslateState::default();
        let next = now_millis() + 4_000;
        let ev = retry_status(2, "Free usage exceeded, subscribe to Go", Value::from(next));
        let out = translate_event(&ev, &mut state);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentEvent::Retrying {
                attempt,
                reason,
                next_in_secs,
            } => {
                assert_eq!(*attempt, 2);
                assert!(reason.contains("免费额度用完了"), "{reason}");
                assert!(reason.contains("Free usage exceeded"), "{reason}");
                let secs = next_in_secs.expect("毫秒时间戳应换算出秒数");
                assert!((1..=5).contains(&secs), "约 4 秒，得到 {secs}");
            }
            other => panic!("该是 Retrying，得到 {other:?}"),
        }
    }

    #[test]
    fn busy_and_idle_status_produce_no_event() {
        let mut state = TranslateState::default();
        for status in [
            serde_json::json!({"type": "busy"}),
            serde_json::json!({"type": "idle"}),
            serde_json::json!({}),
            Value::Null,
        ] {
            let ev = serde_json::json!({"type": "session.status", "properties": {"sessionID": "ses_x", "status": status}});
            assert!(translate_event(&ev, &mut state).is_empty());
        }
        // 连 properties 都没有也不能 panic。
        assert!(
            translate_event(&serde_json::json!({"type": "session.status"}), &mut state).is_empty()
        );
    }

    #[test]
    fn retry_status_with_odd_fields_does_not_panic() {
        let mut state = TranslateState::default();
        // next 缺失 / 字符串 / 负数 / 小数字 / null：都是 None，事件照出。
        for next in [
            Value::Null,
            Value::from("soon"),
            Value::from(-5),
            Value::from(12),
            serde_json::json!({"a": 1}),
        ] {
            let out = translate_event(&retry_status(1, "overloaded", next), &mut state);
            assert_eq!(
                out,
                vec![AgentEvent::Retrying {
                    attempt: 1,
                    reason: "服务商那边暂时忙不过来".to_string(),
                    next_in_secs: None,
                }]
            );
        }
        // attempt / message 缺失：attempt 当 1，原因为空串。
        let bare = serde_json::json!({
            "type": "session.status",
            "properties": {"sessionID": "ses_x", "status": {"type": "retry"}}
        });
        assert_eq!(
            translate_event(&bare, &mut state),
            vec![AgentEvent::Retrying {
                attempt: 1,
                reason: String::new(),
                next_in_secs: None,
            }]
        );
    }

    #[test]
    fn next_in_secs_handles_ms_seconds_and_past() {
        let now = 1_800_000_000_000_u64;
        // 毫秒时间戳，向上取整
        assert_eq!(next_in_secs(&Value::from(now + 4_001), now), Some(5));
        assert_eq!(next_in_secs(&Value::from(now + 2_000), now), Some(2));
        // 秒级时间戳也认
        assert_eq!(next_in_secs(&Value::from(now / 1000 + 7), now), Some(7));
        // 已过去
        assert_eq!(next_in_secs(&Value::from(now - 10_000), now), Some(0));
        // 认不出
        assert_eq!(next_in_secs(&Value::Null, now), None);
        assert_eq!(next_in_secs(&Value::from(0), now), None);
        assert_eq!(next_in_secs(&Value::from(5000), now), None);
        assert_eq!(next_in_secs(&Value::from("x"), now), None);
    }

    #[test]
    fn friendly_reason_branches() {
        let quota = friendly_retry_reason("Free usage exceeded, subscribe to Go");
        assert!(quota.starts_with("免费额度用完了（opencode 提示：Free usage exceeded"));
        assert!(quota.contains("opencode.ai"));
        assert!(friendly_retry_reason("You exceeded your current QUOTA").contains("免费额度"));
        assert!(friendly_retry_reason("Insufficient credits").contains("免费额度"));
        assert!(friendly_retry_reason("Usage Limit reached").contains("免费额度"));
        assert_eq!(
            friendly_retry_reason("Rate limit reached for requests"),
            "请求太频繁，被服务商限流了"
        );
        assert_eq!(
            friendly_retry_reason("Too Many Requests"),
            "请求太频繁，被服务商限流了"
        );
        assert_eq!(
            friendly_retry_reason("HTTP 429"),
            "请求太频繁，被服务商限流了"
        );
        assert_eq!(
            friendly_retry_reason("Overloaded"),
            "服务商那边暂时忙不过来"
        );
        assert_eq!(
            friendly_retry_reason("service Unavailable"),
            "服务商那边暂时忙不过来"
        );
        assert_eq!(
            friendly_retry_reason("status 503"),
            "服务商那边暂时忙不过来"
        );
        assert_eq!(friendly_retry_reason("  something odd  "), "something odd");
        assert_eq!(friendly_retry_reason(""), "");
    }
}
