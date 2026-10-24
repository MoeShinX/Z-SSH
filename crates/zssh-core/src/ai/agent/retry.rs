//! 三个智能体（opencode / Claude Code / Codex）共用的「自动重试」规则（AI-10、AI-11）：
//! 最多放行几次重试、超限时给用户的话、把对方给的原因换成人话。
//!
//! 各智能体自己负责把它的重试通知翻成 [`super::AgentEvent::Retrying`]（`attempt` 的含义
//! 都是「第几次重试」，从 1 开始：第 N 次失败之后、第 N 次重试开始之前发出）；这里只放
//! 不依赖具体协议的纯逻辑。停止当前回合、保证一定收尾则各驱动用自己现有的停止路径做。

use std::time::Duration;

/// 同一回合里最多放行几次自动重试；第 4 次重试状态（attempt >= 4，说明第 3 次重试也失败
/// 了）一到就停掉这一轮。
pub const MAX_RETRIES: u32 = 3;

/// 因重试超限而停止时，等智能体自己收尾（opencode 的 idle、Claude Code 的 result、Codex 的
/// turn/completed）的宽限；到点还没收到就由我们收尾。
pub const RETRY_STOP_GRACE: Duration = Duration::from_millis(1500);

/// 这条重试状态是否已经超过放行次数。
pub fn retry_limit_reached(attempt: u32) -> bool {
    attempt > MAX_RETRIES
}

/// 重试超限时给用户的错误文字（`reason` 是已经友好化的原因；为空就别说空的冒号）。
pub fn retry_limit_message(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        return format!(
            "重试了 {MAX_RETRIES} 次仍然失败。已停止这一轮，可以稍后再试或换一个模型。"
        );
    }
    format!("重试了 {MAX_RETRIES} 次仍然失败：{reason}。已停止这一轮，可以稍后再试或换一个模型。")
}

/// 原因的大类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasonKind {
    /// 额度用完 / 欠费。
    Quota,
    /// 被限流。
    RateLimited,
    /// 服务商过载 / 暂时不可用。
    Overloaded,
    /// 认不出来。
    Other,
}

/// 按原文（不区分大小写）判断原因的大类。
fn classify(raw: &str) -> ReasonKind {
    let lower = raw.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| lower.contains(k));
    if has(&[
        "free usage exceeded",
        "usage limit",
        "quota",
        "credit",
        "insufficient",
    ]) {
        ReasonKind::Quota
    } else if has(&["rate limit", "too many requests"]) || has_code(&lower, "429") {
        ReasonKind::RateLimited
    } else if has(&["overloaded", "unavailable"])
        || ["500", "502", "503", "504", "524"]
            .iter()
            .any(|code| has_code(&lower, code))
    {
        ReasonKind::Overloaded
    } else {
        ReasonKind::Other
    }
}

/// 数字状态码按词边界匹配：`HTTP 500`、`500 Internal Server Error`、`status: 503` 认，
/// `x-500`、`5000 tokens`、`gpt-429b` 这种子串不算（不然随便一个数字都像状态码）。
fn has_code(lower: &str, code: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut start = 0;
    while let Some(at) = lower[start..].find(code) {
        let at = start + at;
        // 两侧都不能是字母数字或连字符：`HTTP 500`、`(524)`、`status: 503` 认；
        // `x-500`、`5000 tokens`、`gpt-429b`、`15003` 里的 `500` 不算。
        let boundary = |b: u8| !b.is_ascii_alphanumeric() && b != b'-';
        let before_ok = at == 0 || boundary(bytes[at - 1]);
        let end = at + code.len();
        let after_ok = end == bytes.len() || boundary(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

const RATE_LIMITED_TEXT: &str = "请求太频繁，被服务商限流了";
const OVERLOADED_TEXT: &str = "服务商那边暂时忙不过来";

/// 把 opencode 重试状态里的原文换成人话（不区分大小写匹配，匹配不上就原样返回）。
pub fn friendly_retry_reason(raw: &str) -> String {
    let raw = raw.trim();
    match classify(raw) {
        ReasonKind::Quota => format!(
            "免费额度用完了（opencode 提示：{raw}）。可以换一个模型，或到 opencode.ai 查看额度 / 订阅"
        ),
        ReasonKind::RateLimited => RATE_LIMITED_TEXT.to_string(),
        ReasonKind::Overloaded => OVERLOADED_TEXT.to_string(),
        ReasonKind::Other => raw.to_string(),
    }
}

/// 同上，给 Claude Code / Codex 用：额度那一句不提 opencode 的网站，换成通用说法。
///
/// - `agent`：智能体的名字（「Claude Code」「Codex」），出现在额度提示里；
/// - `raw`：用来判断大类的文字（可以拼了状态码、错误类别等）；
/// - `shown`：认不出大类时给用户看的原文（比 `raw` 干净，比如不带拼上去的状态码）。
pub fn friendly_agent_retry_reason(agent: &str, raw: &str, shown: &str) -> String {
    let raw = raw.trim();
    match classify(raw) {
        ReasonKind::Quota => format!(
            "额度用完了（{agent} 提示：{}）。可以换一个模型，或检查账号的额度 / 订阅",
            shown.trim()
        ),
        ReasonKind::RateLimited => RATE_LIMITED_TEXT.to_string(),
        ReasonKind::Overloaded => OVERLOADED_TEXT.to_string(),
        ReasonKind::Other => shown.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_limit_pure_logic() {
        assert!(!retry_limit_reached(0));
        assert!(!retry_limit_reached(1));
        assert!(!retry_limit_reached(3));
        assert!(retry_limit_reached(4));
        assert!(retry_limit_reached(u32::MAX));
        let msg = retry_limit_message("请求太频繁，被服务商限流了");
        assert!(msg.starts_with("重试了 3 次仍然失败："));
        assert!(msg.contains("请求太频繁"));
        assert!(msg.ends_with("已停止这一轮，可以稍后再试或换一个模型。"));
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

    #[test]
    fn status_codes_match_on_word_boundaries_only() {
        // 认得出的：HTTP 500、行首 503、status: 504、句尾 524。
        for raw in [
            "HTTP 500 Internal Server Error",
            "503 Service Unavailable",
            "status: 504",
            "gateway timeout (524)",
        ] {
            assert_eq!(friendly_retry_reason(raw), OVERLOADED_TEXT, "{raw}");
        }
        // 认不出的：是别的数字的一部分，不是状态码。
        for raw in [
            "model x-500",
            "5000 tokens remaining",
            "gpt-429b",
            "error 15003",
        ] {
            assert_eq!(friendly_retry_reason(raw), raw, "{raw}");
        }
    }

    #[test]
    fn agent_reason_uses_generic_quota_text_and_keeps_shown_original() {
        let quota = friendly_agent_retry_reason(
            "Claude Code",
            "Usage limit reached HTTP 429",
            "Usage limit reached",
        );
        assert!(quota.starts_with("额度用完了（Claude Code 提示：Usage limit reached）"));
        assert!(!quota.contains("opencode"));
        // 额度优先于限流（429 常常是额度用完的状态码）。
        assert_eq!(
            friendly_agent_retry_reason("Codex", "rate limit exceeded", "x"),
            RATE_LIMITED_TEXT
        );
        assert_eq!(
            friendly_agent_retry_reason("Codex", "server overloaded", "x"),
            OVERLOADED_TEXT
        );
        // 认不出来：给用户看 shown，不是拼接过的 raw。
        assert_eq!(
            friendly_agent_retry_reason("Codex", "weird HTTP 418", "  weird  "),
            "weird"
        );
    }
}
