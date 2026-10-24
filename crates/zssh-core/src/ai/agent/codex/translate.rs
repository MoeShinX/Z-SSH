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
//! - **可恢复的 `error` 通知**（断线重连：`Reconnecting... N/M`，或者带
//!   `willRetry: true`）：不是回合失败，不能当错误发给界面（Netcatty
//!   `translateCodexEvent` 记过这个坑）；AI-11 起翻成 [`AgentEvent::Retrying`]，让界面在
//!   「思考中」旁提示。`attempt` 优先从文案里的 `N/M` 解析（codex 的文案是
//!   `Reconnecting... N/M`，N 是即将进行的第几次重试），解析不出就按这个回合里收到的
//!   重试通知个数数；原因取自 `error.message` / `additionalDetails` / `codexErrorInfo`
//!   （类别名 + `httpStatusCode`），交给共用的友好化。
//!
//! 工具名拼成 `<server>_<tool>`（例如 `zssh_run_command`），和 opencode 侧的写法
//! 一致，面板的友好名 / 主机名 / 退出码解析原样复用。

use std::collections::HashMap;

use serde_json::Value;

use super::super::retry::friendly_agent_retry_reason;
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
    /// 重试超限被我们停掉：要给用户看的错误文字。随后到来的 `turn/completed`（不管什么
    /// 状态）据此先发这一条 `Error` 再发 `TurnEnd`，并且只发这一次；这期间 codex 自己
    /// 再发的错误 / 重试通知一律不报。
    pub retry_stop: Option<String>,
    /// 这个回合里收到的重试通知个数（文案里没有次数时自己数；出了正文 / 工具就清零）。
    pub retry_seen: u32,
}

/// 把一条通知翻译成统一事件。
pub fn translate(
    method: &str,
    params: &Value,
    state: &mut TranslateState,
    turn: &mut TurnStats,
) -> Vec<AgentEvent> {
    // 有正文 / 思考 / 工具进展，说明上一轮重试成功了：重试计数清零。
    if matches!(
        method,
        "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/started"
            | "item/completed"
    ) {
        turn.retry_seen = 0;
    }
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
            // 重试超限被我们停掉：不管 codex 报的是中断、失败还是完成，都只报我们自己的
            // 这一条（取走，之后不会再报第二次）。
            if let Some(text) = turn.retry_stop.take() {
                return vec![AgentEvent::Error { message: text }, AgentEvent::TurnEnd];
            }
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
            // 已经因为重试超限被我们停掉：codex 随后吐的错误（中断、放弃重试之类）不再报，
            // 免得出现第二条莫名其妙的错误；用户看到的是 `turn/completed` 时我们那一条。
            if turn.retry_stop.is_some() {
                return Vec::new();
            }
            if error_is_retryable(params) {
                // 断线重连里的自动重试：回合还在继续，不是错误，翻成重试提示。
                return retrying_events(params, turn);
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

/// 可恢复的 `error` 通知 → [`AgentEvent::Retrying`]。
///
/// `attempt`：先从文案里解析（见 [`parse_retry_attempt`]），解析不出就按这个回合里收到的
/// 重试通知个数数；解析出来的也同步进计数，两种来源混着用不会倒退。codex 没有给下次
/// 重试的时间，`next_in_secs` 一律是 None。
fn retrying_events(params: &Value, turn: &mut TurnStats) -> Vec<AgentEvent> {
    let message = error_message(params);
    let attempt = match parse_retry_attempt(message) {
        Some(n) => n,
        None => turn.retry_seen.saturating_add(1),
    };
    turn.retry_seen = attempt;
    vec![AgentEvent::Retrying {
        attempt,
        reason: retry_reason(params),
        next_in_secs: None,
    }]
}

fn error_message(params: &Value) -> &str {
    params["error"]["message"]
        .as_str()
        .or_else(|| params["message"].as_str())
        .unwrap_or("")
}

/// 从重试文案里取「第几次重试」。
///
/// codex 的文案是 `Reconnecting... N/M`（N 是即将进行的第几次、M 是上限）；也认
/// `retrying N/M` 和 `attempt N`。只在 `reconnecting` / `retrying` / `retry` / `attempt`
/// 这些词后面找，免得把文案里别的 `10/24` 之类的数字当成次数；`N/M` 还要满足
/// `1 <= N <= M`，否则不认（交给调用方自己数）。
pub fn parse_retry_attempt(message: &str) -> Option<u32> {
    let lower = message.to_ascii_lowercase();
    for key in ["reconnecting", "retrying", "retry"] {
        if let Some(pos) = lower.find(key) {
            if let Some(n) = fraction_after(&lower[pos + key.len()..]) {
                return Some(n);
            }
        }
    }
    if let Some(pos) = lower.find("attempt") {
        let rest = &lower[pos + "attempt".len()..];
        let rest = rest.trim_start_matches(|c: char| !c.is_ascii_digit());
        // 「attempt」和数字之间只允许很短的分隔（空格、冒号、`#`），免得跨句乱取。
        let skipped = lower.len() - pos - "attempt".len() - rest.len();
        if skipped <= 3 {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                return Some(n.max(1));
            }
        }
    }
    None
}

/// 在 `text` 里找第一个 `N/M`（N、M 都是数字，`1 <= N <= M`），返回 N。
fn fraction_after(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'/' {
                let denom_start = i + 1;
                let mut j = denom_start;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > denom_start {
                    let n = text[start..i].parse::<u32>().ok()?;
                    let m = text[denom_start..j].parse::<u32>().ok()?;
                    if n >= 1 && n <= m {
                        return Some(n);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// 给用户看的重试原因（已经友好化）。
///
/// 用来判断大类的文字 = `error.message` + `additionalDetails` + `codexErrorInfo`
/// （类别名拆成词，如 `usageLimitExceeded` → `usage limit exceeded`；带 `httpStatusCode`
/// 就拼成 `HTTP 429`）。认不出大类时给用户看的原文：优先 `additionalDetails`（里面是
/// 底层的真实原因），其次 `message`；只有「Reconnecting... N/M」这种进度文案就说连接中断。
fn retry_reason(params: &Value) -> String {
    let error = &params["error"];
    let message = error_message(params).trim();
    let details = error["additionalDetails"].as_str().unwrap_or("").trim();
    let info = error_info_text(&error["codexErrorInfo"]);
    let classify = [message, details, info.as_str()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = message.to_ascii_lowercase();
    let progress_only = lower.contains("reconnecting") || lower.contains("retrying");
    let shown = if !details.is_empty() {
        details.to_string()
    } else if !info.is_empty() && (message.is_empty() || progress_only) {
        info.clone()
    } else if message.is_empty() || progress_only {
        "连接中断，正在重连".to_string()
    } else {
        message.to_string()
    };
    friendly_agent_retry_reason("Codex", &classify, &shown)
}

/// `codexErrorInfo` 的文字：枚举字符串或 `{类别: {httpStatusCode}}` 对象。
fn error_info_text(info: &Value) -> String {
    match info {
        Value::String(name) => camel_to_words(name),
        Value::Object(map) => {
            let Some((name, body)) = map.iter().next() else {
                return String::new();
            };
            let mut text = camel_to_words(name);
            if let Some(status) = body["httpStatusCode"].as_u64() {
                text.push_str(&format!(" HTTP {status}"));
            }
            text
        }
        _ => String::new(),
    }
}

/// `usageLimitExceeded` → `usage limit exceeded`。
fn camel_to_words(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
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

    fn only_retrying(events: &[AgentEvent]) -> (u32, String, Option<u64>) {
        match events {
            [AgentEvent::Retrying {
                attempt,
                reason,
                next_in_secs,
            }] => (*attempt, reason.clone(), *next_in_secs),
            other => panic!("该是一个 Retrying，得到 {other:?}"),
        }
    }

    #[test]
    fn retryable_errors_become_retrying_events_not_errors() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        // 文案里有「N/M」：次数直接解析。
        let reconnecting = json!({
            "threadId": "t", "turnId": "u", "willRetry": true,
            "error": { "message": "Reconnecting... 2/5" }
        });
        let events = translate("error", &reconnecting, &mut state, &mut turn);
        let (attempt, reason, next) = only_retrying(&events);
        assert_eq!(attempt, 2);
        assert_eq!(reason, "连接中断，正在重连");
        assert_eq!(next, None);
        assert!(!turn.failed, "重试不是回合失败");

        // 老文案「(attempt 2)」也认。
        let old = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "stream error: Reconnecting... (attempt 2)" }
        });
        let events = translate("error", &old, &mut state, &mut turn);
        assert_eq!(only_retrying(&events).0, 2);

        // 没有 willRetry 字段时按文案判断。
        let retrying = json!({
            "threadId": "t",
            "error": { "message": "retrying 3/5" }
        });
        let events = translate("error", &retrying, &mut state, &mut turn);
        assert_eq!(only_retrying(&events).0, 3);
        assert!(!turn.failed);
    }

    #[test]
    fn retry_attempt_is_counted_when_the_text_has_no_number() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        let plain = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "stream disconnected" }
        });
        for expected in 1..=3 {
            let events = translate("error", &plain, &mut state, &mut turn);
            assert_eq!(only_retrying(&events).0, expected);
        }
        // 出了正文：说明重试成功了，计数清零。
        let delta = json!({ "itemId": "m", "delta": "好" });
        translate("item/agentMessage/delta", &delta, &mut state, &mut turn);
        let events = translate("error", &plain, &mut state, &mut turn);
        assert_eq!(only_retrying(&events).0, 1);
        // 解析出来的次数同步进计数：后面解析不出的接着往上数。
        let numbered = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "Reconnecting... 4/5" }
        });
        assert_eq!(
            only_retrying(&translate("error", &numbered, &mut state, &mut turn)).0,
            4
        );
        assert_eq!(
            only_retrying(&translate("error", &plain, &mut state, &mut turn)).0,
            5
        );
    }

    #[test]
    fn parse_retry_attempt_only_trusts_numbers_after_retry_words() {
        assert_eq!(parse_retry_attempt("Reconnecting... 2/5"), Some(2));
        assert_eq!(parse_retry_attempt("Reconnecting... 5/5"), Some(5));
        assert_eq!(
            parse_retry_attempt("stream disconnected - retrying sampling request (1/5 in 200ms)"),
            Some(1)
        );
        assert_eq!(parse_retry_attempt("Retry attempt #3"), Some(3));
        assert_eq!(parse_retry_attempt("(attempt 2)"), Some(2));
        // 不在重试词后面的 N/M、日期、N > M、0 都不认。
        assert_eq!(parse_retry_attempt("2026/10 something"), None);
        assert_eq!(parse_retry_attempt("Reconnecting... 9/5"), None);
        assert_eq!(parse_retry_attempt("Reconnecting... 0/5"), None);
        assert_eq!(parse_retry_attempt("Reconnecting..."), None);
        assert_eq!(parse_retry_attempt("12/24 reconnecting"), None);
        assert_eq!(parse_retry_attempt(""), None);
        // 超大数字不 panic。
        assert_eq!(
            parse_retry_attempt("Reconnecting... 99999999999999999999/99999999999999999999"),
            None
        );
    }

    #[test]
    fn retry_reason_uses_details_and_error_info() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
        // 底层详情里有状态码：友好化认得出。
        let limited = json!({
            "threadId": "t", "willRetry": true,
            "error": {
                "message": "Reconnecting... 1/5",
                "additionalDetails": "exceeded retry limit, last status: 429 Too Many Requests"
            }
        });
        let (_, reason, _) = only_retrying(&translate("error", &limited, &mut state, &mut turn));
        assert_eq!(reason, "请求太频繁，被服务商限流了");

        // 类别枚举：额度类走额度那一句（通用说法，不提 opencode）。
        let quota = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "Reconnecting... 1/5", "codexErrorInfo": "usageLimitExceeded" }
        });
        let (_, reason, _) = only_retrying(&translate("error", &quota, &mut state, &mut turn));
        assert!(reason.starts_with("额度用完了（Codex 提示：usage limit exceeded）"));
        assert!(!reason.contains("opencode"));

        // 类别对象带状态码。
        let overloaded = json!({
            "threadId": "t", "willRetry": true,
            "error": {
                "message": "Reconnecting... 1/5",
                "codexErrorInfo": { "httpConnectionFailed": { "httpStatusCode": 503 } }
            }
        });
        let (_, reason, _) = only_retrying(&translate("error", &overloaded, &mut state, &mut turn));
        assert_eq!(reason, "服务商那边暂时忙不过来");

        // 认不出的详情：保留原文。
        let odd = json!({
            "threadId": "t", "willRetry": true,
            "error": { "message": "Reconnecting... 1/5", "additionalDetails": "tls handshake eof" }
        });
        let (_, reason, _) = only_retrying(&translate("error", &odd, &mut state, &mut turn));
        assert_eq!(reason, "tls handshake eof");

        // 奇怪形状不 panic。
        for weird in [
            json!({ "willRetry": true }),
            json!({ "willRetry": true, "error": null }),
            json!({ "willRetry": true, "error": { "message": 5, "codexErrorInfo": [1], "additionalDetails": {} } }),
            json!({ "willRetry": true, "error": { "codexErrorInfo": {} } }),
        ] {
            let events = translate("error", &weird, &mut state, &mut turn);
            assert_eq!(events.len(), 1, "{weird}");
        }
    }

    #[test]
    fn non_retryable_errors_still_report_immediately() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats::default();
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
        // willRetry 为 false 时，即使文案里带 retrying 也是错误。
        let mut turn = TurnStats::default();
        let still = json!({
            "threadId": "t", "willRetry": false,
            "error": { "message": "retrying failed 5/5" }
        });
        assert!(matches!(
            translate("error", &still, &mut state, &mut turn).as_slice(),
            [AgentEvent::Error { .. }]
        ));
        // 没有 willRetry 字段也没有重试字样：错误。
        let plain = json!({ "threadId": "t", "error": { "message": "boom" } });
        assert!(matches!(
            translate("error", &plain, &mut state, &mut turn).as_slice(),
            [AgentEvent::Error { .. }]
        ));
    }

    #[test]
    fn after_a_retry_stop_codex_noise_is_silent_and_turn_completed_reports_once() {
        let mut state = TranslateState::default();
        let mut turn = TurnStats {
            aborted: true,
            retry_stop: Some("重试了 3 次仍然失败：x。".to_string()),
            ..Default::default()
        };
        // 停止期间 codex 再发重试通知 / 放弃重试的错误：不报。
        let retry = json!({ "willRetry": true, "error": { "message": "Reconnecting... 4/5" } });
        let fatal = json!({ "willRetry": false, "error": { "message": "stream closed" } });
        assert!(translate("error", &retry, &mut state, &mut turn).is_empty());
        assert!(translate("error", &fatal, &mut state, &mut turn).is_empty());
        assert!(!turn.failed);
        // turn/completed 是中断：先我们的 Error，再 TurnEnd。
        let interrupted = json!({ "turn": { "id": "u", "status": "interrupted" } });
        let events = translate("turn/completed", &interrupted, &mut state, &mut turn);
        assert_eq!(
            events,
            vec![
                AgentEvent::Error {
                    message: "重试了 3 次仍然失败：x。".to_string()
                },
                AgentEvent::TurnEnd
            ]
        );
        // 只报一次：再来一条只有 TurnEnd。
        assert_eq!(
            translate("turn/completed", &interrupted, &mut state, &mut turn),
            vec![AgentEvent::TurnEnd]
        );
        // failed 状态也一样只报我们的那一条。
        let mut turn = TurnStats {
            retry_stop: Some("E".to_string()),
            ..Default::default()
        };
        let failed = json!({ "turn": { "status": "failed", "error": { "message": "other" } } });
        assert_eq!(
            translate("turn/completed", &failed, &mut state, &mut turn),
            vec![
                AgentEvent::Error {
                    message: "E".to_string()
                },
                AgentEvent::TurnEnd
            ]
        );
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
