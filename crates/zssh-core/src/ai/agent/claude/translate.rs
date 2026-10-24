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
//! - `result`：回合结束（`subtype` / `is_error` / `result` 文本）；
//! - `system/api_retry`：API 请求遇到可重试的错误（限流 / 过载 / 连接问题），等一会儿
//!   再重试（AI-11）。字段 `attempt`（第几次重试，从 1 开始：第 N 次失败之后、第 N 次
//!   重试之前发出，依据见 [`api_retry_events`]）、`max_retries`、`retry_delay_ms`、
//!   `error_status`（HTTP 状态码，连接错误为 null）、`error`（类别字符串）。
//!
//! 工具名 `mcp__zssh__run_command` 归一成 `zssh_run_command`，和 opencode / codex
//! 两侧的写法一致，面板的友好名 / 主机名 / 退出码解析原样复用。

use std::collections::HashSet;

use serde_json::Value;

use super::super::retry::friendly_agent_retry_reason;
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
    /// 中断过（用户点停止，或重试超限被我们停掉；由驱动在发中断时置位）。
    pub aborted: bool,
    /// 重试超限被我们停掉：要给用户看的错误文字。随后到来的 `result`（不管成功还是
    /// 中断错误）据此先发这一条 `Error` 再发 `TurnEnd`，并且只发这一次。
    pub retry_stop: Option<String>,
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
        "system" if message["subtype"].as_str() == Some("api_retry") => api_retry_events(message),
        // system/init、system/status、system/thinking_tokens、rate_limit_event 之类
        // 不产生统一事件（驱动自己读 session_id / mcp_servers）。
        _ => Vec::new(),
    }
}

/// `system/api_retry` → [`AgentEvent::Retrying`]。
///
/// `attempt` 的含义（读本机 Claude Code 2.1.x 程序文本得出，没有抓到真实事件）：重试循环
/// 里 `attempt` 从 1 起，请求失败后先判断 `attempt <= max_retries`，再把当前 `attempt`
/// 连同退避时间发出来，然后等待、重试。所以它是「第几次重试」（和 opencode 一致）：
/// 第 1 次失败后发 `attempt=1`，`attempt == max_retries` 是它自己的最后一次重试。程序的
/// 日志里也写作 `API error (attempt N/max+1)`。缺失或为 0 时按 1 处理。
///
/// 原因：`error_status` 拼成 `HTTP 429` 这样的文字，`error` 是类别（`rate_limit` /
/// `overloaded` / `server_error` / `authentication_failed` / `unknown` 等，下划线换成空格），
/// 交给共用的友好化；认不出就保留原文；没有状态码又没有类别是连接错误 / 超时。
fn api_retry_events(message: &Value) -> Vec<AgentEvent> {
    let attempt = message["attempt"]
        .as_u64()
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .unwrap_or(1)
        .max(1);
    let shown = api_retry_text(message);
    vec![AgentEvent::Retrying {
        attempt,
        reason: friendly_agent_retry_reason("Claude Code", &classify_text(message, &shown), &shown),
        next_in_secs: delay_secs(&message["retry_delay_ms"]),
    }]
}

/// 给用户看的原文：类别 + 状态码（都没有就是连接问题）。
fn api_retry_text(message: &Value) -> String {
    let category = message["error"]
        .as_str()
        .map(|s| s.trim().replace('_', " "))
        .filter(|s| !s.is_empty() && s != "unknown")
        .unwrap_or_default();
    let status = message["error_status"].as_u64();
    let mut parts = Vec::new();
    if !category.is_empty() {
        parts.push(category);
    }
    if let Some(status) = status {
        parts.push(format!("HTTP {status}"));
    }
    if parts.is_empty() {
        "网络连接出错或超时".to_string()
    } else {
        parts.join(" ")
    }
}

/// 用来分类的文字：529 是 Anthropic 的「过载」状态码，补上这个词好让友好化认出来。
fn classify_text(message: &Value, shown: &str) -> String {
    if message["error_status"].as_u64() == Some(529) {
        format!("{shown} overloaded")
    } else {
        shown.to_string()
    }
}

/// 毫秒 → 向上取整的秒；缺失 / 不是数字 / 负数 / 非有限数是 None。
fn delay_secs(value: &Value) -> Option<u64> {
    let ms = value.as_f64()?;
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    Some((ms / 1000.0).ceil() as u64)
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
            // MCP 工具的结果是文本块数组：拼成纯文本，不然卡片上看到的是 JSON，末尾的执行编号
            // 标记（AI-SCOPE-3）也被转义进了字符串里，界面认不出来。
            Value::Array(parts) => {
                joined_text_blocks(parts).unwrap_or_else(|| block["content"].to_string())
            }
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

/// 全是文本块的内容数组拼成纯文本（块之间换行）；混有别的块或为空返回 `None`。
fn joined_text_blocks(parts: &[Value]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let mut texts = Vec::with_capacity(parts.len());
    for part in parts {
        if part["type"].as_str() != Some("text") {
            return None;
        }
        texts.push(part["text"].as_str()?);
    }
    Some(texts.join(
        "
",
    ))
}

fn result_events(message: &Value, turn: &mut TurnStats) -> Vec<AgentEvent> {
    // 重试超限被我们停掉：不管这条 result 是成功、中断还是错误，都只报我们自己的这一条
    // （取走，之后不会再报第二次）。
    if let Some(text) = turn.retry_stop.take() {
        return vec![AgentEvent::Error { message: text }, AgentEvent::TurnEnd];
    }
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
    fn mcp_text_block_results_become_plain_text_with_the_exec_marker_intact() {
        // claude 把 MCP 工具结果当文本块数组给出来：输出要是纯文本，末尾的执行编号标记
        // 才能被界面认出来并去掉（AI-SCOPE-3）。
        let mut state = TranslateState::default();
        let user = json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "toolu_7", "is_error": false,
                  "content": [{ "type": "text", "text": "exit_code: 0
ok
[zssh exec ab12cd-3]" }] },
                { "type": "tool_result", "tool_use_id": "toolu_8", "is_error": true,
                  "content": [{ "type": "text", "text": "用户拒绝" },
                              { "type": "text", "text": "[zssh exec ab12cd-4]" }] },
                { "type": "tool_result", "tool_use_id": "toolu_9", "is_error": false,
                  "content": [{ "type": "image", "source": {} }] }
            ]}
        });
        let outputs: Vec<String> = user_events(&user, &mut state)
            .into_iter()
            .map(|event| match event {
                AgentEvent::ToolEnd { output, .. } => output,
                other => panic!("应是 ToolEnd：{other:?}"),
            })
            .collect();
        assert_eq!(
            outputs[0],
            "exit_code: 0
ok
[zssh exec ab12cd-3]"
        );
        assert_eq!(
            outputs[1],
            "用户拒绝
[zssh exec ab12cd-4]"
        );
        assert!(
            outputs[2].starts_with("[{"),
            "不认识的块仍按 JSON 原样给出：{}",
            outputs[2]
        );
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

    fn retry_msg(attempt: u64, status: Value, error: &str, delay_ms: Value) -> Value {
        json!({
            "type": "system", "subtype": "api_retry",
            "attempt": attempt, "max_retries": 10,
            "retry_delay_ms": delay_ms, "error_status": status, "error": error,
            "uuid": "u", "session_id": "s"
        })
    }

    fn one_retry(message: &Value) -> (u32, String, Option<u64>) {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let events = translate(message, &mut state, &mut turn);
        match events.as_slice() {
            [AgentEvent::Retrying {
                attempt,
                reason,
                next_in_secs,
            }] => (*attempt, reason.clone(), *next_in_secs),
            other => panic!("该是一个 Retrying，得到 {other:?}"),
        }
    }

    #[test]
    fn api_retry_becomes_retrying_with_friendly_reason_and_rounded_delay() {
        // 限流：状态码 429 / 类别 rate_limit；延迟 2500ms 向上取整成 3 秒。
        let (attempt, reason, next) =
            one_retry(&retry_msg(2, json!(429), "rate_limit", json!(2500)));
        assert_eq!(attempt, 2);
        assert_eq!(reason, "请求太频繁，被服务商限流了");
        assert_eq!(next, Some(3));

        // 过载：529 + overloaded；整秒不多进一位。
        let (_, reason, next) = one_retry(&retry_msg(1, json!(529), "overloaded", json!(2000)));
        assert_eq!(reason, "服务商那边暂时忙不过来");
        assert_eq!(next, Some(2));

        // 只有状态码 529、类别认不出：也能认成过载。
        let (_, reason, _) = one_retry(&retry_msg(1, json!(529), "unknown", json!(500)));
        assert_eq!(reason, "服务商那边暂时忙不过来");

        // 服务端错误 500。
        let (_, reason, next) = one_retry(&retry_msg(3, json!(500), "server_error", json!(0)));
        assert_eq!(reason, "服务商那边暂时忙不过来");
        assert_eq!(next, Some(0));
    }

    #[test]
    fn api_retry_unrecognised_reasons_keep_the_original_text() {
        // 认不出的类别 + 状态码：保留原文（下划线换成空格）。
        let (_, reason, _) = one_retry(&retry_msg(
            1,
            json!(401),
            "authentication_failed",
            json!(1000),
        ));
        assert_eq!(reason, "authentication failed HTTP 401");
        // 连接错误：状态码为 null、类别 unknown。
        let (_, reason, _) = one_retry(&retry_msg(1, Value::Null, "unknown", json!(1000)));
        assert_eq!(reason, "网络连接出错或超时");
    }

    #[test]
    fn api_retry_quota_text_reaches_the_quota_sentence() {
        // 如果 error 里带了额度类文字，走额度那一句（用通用说法，不提 opencode）。
        let (_, reason, _) = one_retry(&retry_msg(
            1,
            json!(429),
            "usage limit reached",
            json!(1000),
        ));
        assert!(reason.starts_with("额度用完了（Claude Code 提示：usage limit reached HTTP 429）"));
        assert!(!reason.contains("opencode"));
    }

    #[test]
    fn api_retry_with_missing_or_odd_fields_never_panics() {
        // 全缺：attempt 当 1，原因是连接问题，延迟 None。
        let bare = json!({ "type": "system", "subtype": "api_retry" });
        let (attempt, reason, next) = one_retry(&bare);
        assert_eq!((attempt, next), (1, None));
        assert_eq!(reason, "网络连接出错或超时");
        // attempt 为 0 / 负数 / 字符串 / 超大：都落在 >= 1。
        for odd in [json!(0), json!(-3), json!("2"), json!(null), json!(1.5)] {
            let mut msg = retry_msg(1, json!(429), "rate_limit", json!(1000));
            msg["attempt"] = odd;
            assert!(one_retry(&msg).0 >= 1);
        }
        let mut huge = retry_msg(1, json!(429), "rate_limit", json!(1000));
        huge["attempt"] = json!(u64::MAX);
        assert_eq!(one_retry(&huge).0, u32::MAX);
        // 延迟：负数 / 字符串 / null 都是 None。
        for odd in [json!(-5), json!("x"), json!(null)] {
            assert_eq!(
                one_retry(&retry_msg(1, json!(429), "rate_limit", odd)).2,
                None
            );
        }
        // 状态码不是整数、类别不是字符串：不 panic。
        let weird = json!({
            "type": "system", "subtype": "api_retry", "attempt": 1,
            "error_status": "429", "error": 7, "retry_delay_ms": 100
        });
        let _ = one_retry(&weird);
    }

    #[test]
    fn other_system_messages_still_produce_no_events() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        for message in [
            json!({ "type": "system", "subtype": "init", "session_id": "s" }),
            json!({ "type": "system", "subtype": "status", "status": "requesting" }),
            json!({ "type": "system", "subtype": "api_error" }),
            json!({ "type": "system" }),
            json!({ "type": "rate_limit_event" }),
        ] {
            assert!(
                translate(&message, &mut state, &mut turn).is_empty(),
                "{message}"
            );
        }
    }

    #[test]
    fn result_after_a_retry_stop_reports_our_error_exactly_once() {
        let mut state = TranslateState::default();
        // 中断带来的错误 result。
        let interrupted = json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "result": "Request was aborted"
        });
        let mut turn = TurnStats {
            aborted: true,
            retry_stop: Some("重试了 3 次仍然失败：x。".to_string()),
            ..Default::default()
        };
        let events = translate(&interrupted, &mut state, &mut turn);
        assert_eq!(
            events,
            vec![
                AgentEvent::Error {
                    message: "重试了 3 次仍然失败：x。".to_string()
                },
                AgentEvent::TurnEnd
            ]
        );
        assert_eq!(turn.retry_stop, None);
        // 再来一条 result（不该有，但要保证不会再报第二条错误）。
        assert_eq!(
            translate(&interrupted, &mut state, &mut turn),
            vec![AgentEvent::TurnEnd]
        );

        // 即使 result 是成功的，也先报我们的错误。
        let mut turn = TurnStats {
            aborted: true,
            retry_stop: Some("E".to_string()),
            ..Default::default()
        };
        let ok = json!({ "type": "result", "subtype": "success", "is_error": false });
        assert_eq!(
            translate(&ok, &mut state, &mut turn),
            vec![
                AgentEvent::Error {
                    message: "E".to_string()
                },
                AgentEvent::TurnEnd
            ]
        );
    }
}
