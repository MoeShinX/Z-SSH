// Adapted from Netcatty electron/bridges/aiBridge/codexAppServer/runtime.cjs (approval
// decision mapping in #safeInteractionResponse), rewritten in Rust for Z-SSH.
//! codex 的审批请求 → 我们的回答（AI-3 第 2 节第 3 层）。
//!
//! codex 在 `approvalPolicy: "on-request"` 下会把"要执行本机命令 / 改文件 / 要权限"
//! 这类请求发给客户端（app-server 的**服务端请求**），由我们回答。Z-SSH 的策略：
//!
//! - **本机命令、改文件、要权限：一律拒绝**（codex 不能动用户的 Windows 电脑）；
//! - **只有 `zssh` 这个 MCP 服务的工具调用放行**——正常路径上它由
//!   `default_tools_approval_mode: "approve"` 自动放行、根本不发请求；真发到我们
//!   这里（比如设置被改）也放行，确认照旧由 Z-SSH 工具桥负责，不在这一层重复弹窗。
//!   其它 MCP 服务（理论上不该加载）的工具调用拒绝；
//! - 要用户输入（`item/tool/requestUserInput`）：回空答案（我们没有给模型的追问通道，
//!   让它自己收尾，比永远挂着强）；
//! - 不认识的服务端请求：回 `-32601`（method not found），和 Netcatty 一致。
//!
//! 决策是纯函数（请求形状进、应答 JSON 出），单测覆盖每一条。

use serde_json::{json, Value};

use super::protocol::{method, RpcError};

/// 允许工具调用放行的 MCP 服务名。
pub const ALLOWED_MCP_SERVER: &str = "zssh";

/// 一条服务端请求的回答。
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// 正常应答（要写回去的 result）。
    Respond(Value),
    /// 回错误（不认识的请求）。
    Fail(RpcError),
}

/// 这个请求的决策。
pub fn decide(method_name: &str, params: &Value) -> Decision {
    match method_name {
        method::COMMAND_APPROVAL => Decision::Respond(json!({ "decision": "decline" })),
        method::FILE_CHANGE_APPROVAL => Decision::Respond(json!({ "decision": "decline" })),
        method::PERMISSIONS_APPROVAL => {
            // 一个权限都不给（network / fileSystem 都不授权），作用域只是本回合。
            Decision::Respond(json!({ "permissions": {}, "scope": "turn" }))
        }
        method::REQUEST_USER_INPUT => Decision::Respond(json!({ "answers": {} })),
        method::MCP_ELICITATION => {
            let server = params["serverName"].as_str().unwrap_or("");
            if server == ALLOWED_MCP_SERVER {
                Decision::Respond(json!({ "action": "accept", "content": {} }))
            } else {
                Decision::Respond(json!({ "action": "decline" }))
            }
        }
        other => Decision::Fail(RpcError {
            code: -32601,
            message: format!("不支持的请求：{other}"),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_command_and_file_change_are_declined() {
        let command = json!({ "command": "whoami", "cwd": "C:\\data", "itemId": "i1" });
        assert_eq!(
            decide(method::COMMAND_APPROVAL, &command),
            Decision::Respond(json!({ "decision": "decline" }))
        );
        let file = json!({ "itemId": "i2", "reason": "write" });
        assert_eq!(
            decide(method::FILE_CHANGE_APPROVAL, &file),
            Decision::Respond(json!({ "decision": "decline" }))
        );
    }

    #[test]
    fn permissions_are_not_granted() {
        let params = json!({
            "permissions": { "network": { "enabled": true }, "fileSystem": null },
            "cwd": "C:\\data"
        });
        match decide(method::PERMISSIONS_APPROVAL, &params) {
            Decision::Respond(value) => {
                assert_eq!(value["permissions"], json!({}), "一个权限都不给");
                assert_eq!(value["scope"], json!("turn"));
            }
            other => panic!("应正常应答：{other:?}"),
        }
    }

    #[test]
    fn only_the_zssh_mcp_server_is_allowed() {
        let ours = json!({ "serverName": "zssh", "threadId": "t" });
        assert_eq!(
            decide(method::MCP_ELICITATION, &ours),
            Decision::Respond(json!({ "action": "accept", "content": {} }))
        );
        let other = json!({ "serverName": "my_mcp_server", "threadId": "t" });
        assert_eq!(
            decide(method::MCP_ELICITATION, &other),
            Decision::Respond(json!({ "action": "decline" }))
        );
        // 没有 serverName 的（形状不认识）：拒绝。
        assert_eq!(
            decide(method::MCP_ELICITATION, &json!({})),
            Decision::Respond(json!({ "action": "decline" }))
        );
    }

    #[test]
    fn user_input_gets_empty_answers() {
        let params = json!({ "questions": [{ "id": "q1", "question": "选一个" }] });
        assert_eq!(
            decide(method::REQUEST_USER_INPUT, &params),
            Decision::Respond(json!({ "answers": {} }))
        );
    }

    #[test]
    fn unknown_requests_fail_with_method_not_found() {
        match decide("item/tool/call", &json!({})) {
            Decision::Fail(error) => {
                assert_eq!(error.code, -32601);
                assert!(error.message.contains("item/tool/call"));
            }
            other => panic!("应回错误：{other:?}"),
        }
    }
}
