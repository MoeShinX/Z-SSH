//! codex app-server 的 JSON-RPC 协议层（AI-3 第 1 节）。
//!
//! 形状以本机 codex 的 `codex app-server generate-json-schema` 导出为准（导出文件在
//! `fixtures/codex/`，README 里写了版本和重新导出的命令）。这一层只做**消息和参数
//! 的构造 / 解析**，纯函数，方便拿导出的 schema 做单测（见文件末尾的测试）。
//!
//! 传输是 stdin/stdout 上一行一个 JSON 对象（JSON-RPC 2.0 风格，但不带 `jsonrpc`
//! 字段）。四类入站消息靠有没有 `id` / `method` 区分。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 出站请求（要回应的，带自增 id）。
#[derive(Debug, Serialize)]
pub struct Request<'a> {
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

/// 出站通知（不需要回应）。
#[derive(Debug, Serialize)]
pub struct OutNotification<'a> {
    pub method: &'a str,
    pub params: Value,
}

/// 出站应答（回答服务端发来的请求）。
#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    pub result: Value,
}

/// 出站错误应答。
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub id: u64,
    pub error: RpcError,
}

/// JSON-RPC 错误体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 一条入站消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// 我们发的请求的应答。
    Response { id: u64, result: Value },
    /// 我们发的请求出错了。
    Error { id: u64, error: RpcError },
    /// 服务端请求（审批、要用户输入等）：必须回应。
    ServerRequest {
        id: u64,
        method: String,
        params: Value,
    },
    /// 通知（事件流）。
    Notification { method: String, params: Value },
}

impl Incoming {
    /// 解析一行 JSON。形状不认识（比如裸数组、没有 method 也没有数字 id）算错误。
    pub fn parse(line: &str) -> Result<Incoming, serde_json::Error> {
        let value: Value = serde_json::from_str(line)?;
        let id = value.get("id").and_then(Value::as_u64);
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(match (id, method) {
            (Some(id), Some(method)) => Incoming::ServerRequest {
                id,
                method,
                params: value.get("params").cloned().unwrap_or(Value::Null),
            },
            (Some(id), None) => match value.get("error") {
                Some(error) if !error.is_null() => Incoming::Error {
                    id,
                    error: serde_json::from_value(error.clone())?,
                },
                _ => Incoming::Response {
                    id,
                    result: value.get("result").cloned().unwrap_or(Value::Null),
                },
            },
            (None, Some(method)) => Incoming::Notification {
                method,
                params: value.get("params").cloned().unwrap_or(Value::Null),
            },
            (None, None) => {
                return Err(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "既没有 id 也没有 method 的消息",
                )))
            }
        })
    }
}

/// 方法名常量（和 schema 里的字符串一一对应；拼错会被 schema 单测拦住）。
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "initialized";
    pub const THREAD_START: &str = "thread/start";
    pub const THREAD_RESUME: &str = "thread/resume";
    pub const TURN_START: &str = "turn/start";
    pub const TURN_INTERRUPT: &str = "turn/interrupt";
    pub const MODEL_LIST: &str = "model/list";
    pub const ACCOUNT_READ: &str = "account/read";
    pub const MCP_SERVER_STATUS_LIST: &str = "mcpServerStatus/list";

    pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
    pub const FILE_CHANGE_APPROVAL: &str = "item/fileChange/requestApproval";
    pub const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
    pub const REQUEST_USER_INPUT: &str = "item/tool/requestUserInput";
    pub const MCP_ELICITATION: &str = "mcpServer/elicitation/request";

    pub const ITEM_STARTED: &str = "item/started";
    pub const ITEM_COMPLETED: &str = "item/completed";
    pub const AGENT_MESSAGE_DELTA: &str = "item/agentMessage/delta";
    pub const REASONING_SUMMARY_DELTA: &str = "item/reasoning/summaryTextDelta";
    pub const COMMAND_OUTPUT_DELTA: &str = "item/commandExecution/outputDelta";
    pub const TURN_STARTED: &str = "turn/started";
    pub const TURN_COMPLETED: &str = "turn/completed";
    pub const ERROR: &str = "error";
}

/// `initialize` 的参数。
///
/// `capabilities.experimentalApi` 留 false：只用导出的**非实验**协议面。
pub fn initialize_params(client_name: &str, version: &str) -> Value {
    json!({
        "clientInfo": { "name": client_name, "title": "Z-SSH", "version": version },
        "capabilities": { "experimentalApi": false, "requestAttestation": false },
    })
}

/// 启动一个 thread（= 一个对话）的参数。
///
/// - `approval_policy`：`"on-request"`——审批请求发给我们（第 2 节：本机命令 / 改文件
///   一律拒绝；放行由 zssh 服务自己的审批设置负责，不在这一层重复弹窗）；
/// - `sandbox`：`"read-only"`——本机工作目录只读兜底；
/// - `developer_instructions`：本对话的系统提示（能力密钥从这里交给模型）。
pub fn thread_start_params(cwd: &str, model: Option<&str>, developer_instructions: &str) -> Value {
    json!({
        "cwd": cwd,
        "model": model,
        "approvalPolicy": "on-request",
        "approvalsReviewer": "user",
        "sandbox": "read-only",
        "developerInstructions": developer_instructions,
        "config": { "model_reasoning_summary": "concise" },
    })
}

/// 续接一个已有 thread（历史会话）的参数：审批、沙箱和 [`thread_start_params`] 完全一样，
/// 指令换成这次对话的（新的能力密钥），旧密钥已经作废。`excludeTurns`：不要把整段历史
/// 发回来（界面用的是我们自己存的历史）。
pub fn thread_resume_params(thread_id: &str, cwd: &str, developer_instructions: &str) -> Value {
    json!({
        "threadId": thread_id,
        "cwd": cwd,
        "approvalPolicy": "on-request",
        "approvalsReviewer": "user",
        "sandbox": "read-only",
        "developerInstructions": developer_instructions,
        "config": { "model_reasoning_summary": "concise" },
        "excludeTurns": true,
    })
}

/// 发一条用户消息、开一个回合的参数。
pub fn turn_start_params(
    thread_id: &str,
    cwd: &str,
    text: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{ "type": "text", "text": text, "text_elements": [] }],
        "cwd": cwd,
        "approvalPolicy": "on-request",
        "approvalsReviewer": "user",
        "sandboxPolicy": { "type": "readOnly", "networkAccess": false },
        "model": model,
        "effort": effort,
        "summary": "concise",
    })
}

/// 打断一个回合。
pub fn turn_interrupt_params(thread_id: &str, turn_id: &str) -> Value {
    json!({ "threadId": thread_id, "turnId": turn_id })
}

/// 列模型（一页）。
pub fn model_list_params(cursor: Option<&str>) -> Value {
    json!({ "cursor": cursor, "limit": 100 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// 导出的协议 schema（codex 0.162.1，见 fixtures/codex/README.md）。
    const MAIN_SCHEMA: &str =
        include_str!("../fixtures/codex/codex_app_server_protocol.schemas.json");
    const V2_SCHEMA: &str =
        include_str!("../fixtures/codex/codex_app_server_protocol.v2.schemas.json");
    const CLIENT_REQUEST: &str = include_str!("../fixtures/codex/ClientRequest.json");
    const SERVER_REQUEST: &str = include_str!("../fixtures/codex/ServerRequest.json");
    const SERVER_NOTIFICATION: &str = include_str!("../fixtures/codex/ServerNotification.json");

    fn schema_value(raw: &str) -> Value {
        serde_json::from_str(raw).expect("schema 是合法 JSON")
    }

    /// ClientRequest / ServerRequest / ServerNotification 的方法名清单。
    fn method_names(raw: &str) -> BTreeSet<String> {
        let schema = schema_value(raw);
        let mut out = BTreeSet::new();
        for variant in schema["oneOf"].as_array().expect("oneOf") {
            let method = &variant["properties"]["method"];
            if let Some(list) = method["enum"].as_array() {
                for item in list {
                    out.insert(item.as_str().unwrap_or_default().to_string());
                }
            } else if let Some(text) = method["const"].as_str() {
                out.insert(text.to_string());
            }
        }
        out
    }

    /// 一个定义的属性名集合（`required` 也一并校验）。
    fn properties_of(schema: &Value, definition: &str) -> BTreeSet<String> {
        let def = &schema["definitions"][definition];
        // $ref 间接引用的先解一层。
        let def = def["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix("#/definitions/"))
            .map(|name| &schema["definitions"][name])
            .unwrap_or(def);
        def["properties"]
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 我们发出去的每个参数对象，键必须在 schema 的属性里（防字段写错 / 名字变了）。
    fn assert_keys_belong(params: &Value, allowed: &BTreeSet<String>, what: &str) {
        for key in params.as_object().expect("对象").keys() {
            assert!(
                allowed.contains(key),
                "{what}：多出了 schema 里没有的键 {key}"
            );
        }
    }

    #[test]
    fn every_method_we_send_exists_in_the_exported_schema() {
        let client = method_names(CLIENT_REQUEST);
        for method in [
            method::INITIALIZE,
            method::THREAD_START,
            method::THREAD_RESUME,
            method::TURN_START,
            method::TURN_INTERRUPT,
            method::MODEL_LIST,
            method::ACCOUNT_READ,
            method::MCP_SERVER_STATUS_LIST,
        ] {
            assert!(client.contains(method), "schema 里没有客户端方法 {method}");
        }

        let server = method_names(SERVER_REQUEST);
        for method in [
            method::COMMAND_APPROVAL,
            method::FILE_CHANGE_APPROVAL,
            method::PERMISSIONS_APPROVAL,
            method::REQUEST_USER_INPUT,
            method::MCP_ELICITATION,
        ] {
            assert!(server.contains(method), "schema 里没有服务端请求 {method}");
        }

        let notifications = method_names(SERVER_NOTIFICATION);
        for method in [
            method::ITEM_STARTED,
            method::ITEM_COMPLETED,
            method::AGENT_MESSAGE_DELTA,
            method::REASONING_SUMMARY_DELTA,
            method::COMMAND_OUTPUT_DELTA,
            method::TURN_STARTED,
            method::TURN_COMPLETED,
            method::ERROR,
        ] {
            assert!(notifications.contains(method), "schema 里没有通知 {method}");
        }
    }

    #[test]
    fn initialize_params_match_the_schema() {
        let main = schema_value(MAIN_SCHEMA);
        let params = initialize_params("zssh", "0.1.0");
        assert_keys_belong(
            &params,
            &properties_of(&main, "InitializeParams"),
            "initialize",
        );
        // clientInfo 的必填项。
        let client = properties_of(&main, "ClientInfo");
        for key in ["name", "version"] {
            assert!(
                params["clientInfo"].get(key).is_some(),
                "clientInfo 缺 {key}（schema 属性：{client:?}）"
            );
        }
        // capabilities 的键也要在 schema 里。
        assert_keys_belong(
            &params["capabilities"],
            &properties_of(&main, "InitializeCapabilities"),
            "initialize.capabilities",
        );
    }

    #[test]
    fn thread_and_turn_params_match_the_schema() {
        let main = schema_value(MAIN_SCHEMA);
        let v2 = schema_value(V2_SCHEMA);

        let thread = thread_start_params("C:\\data\\workspace", None, "指令");
        assert_keys_belong(
            &thread,
            &properties_of(&v2, "ThreadStartParams"),
            "thread/start",
        );
        assert_eq!(thread["approvalPolicy"], json!("on-request"));
        assert_eq!(thread["approvalsReviewer"], json!("user"));
        assert_eq!(thread["sandbox"], json!("read-only"));
        assert_eq!(thread["developerInstructions"], json!("指令"));

        // 续接：键都在 schema 里，审批 / 沙箱和新开 thread 完全一样，指令换成新的。
        let resume = thread_resume_params("t-9", "C:/data/workspace", "新指令");
        assert_keys_belong(
            &resume,
            &properties_of(&v2, "ThreadResumeParams"),
            "thread/resume",
        );
        assert_eq!(resume["threadId"], json!("t-9"));
        for key in ["approvalPolicy", "approvalsReviewer", "sandbox", "config"] {
            assert_eq!(
                resume[key], thread[key],
                "续接的 {key} 要和新开 thread 一样"
            );
        }
        assert_eq!(resume["developerInstructions"], json!("新指令"));
        assert_eq!(resume["excludeTurns"], json!(true));

        let turn = turn_start_params(
            "t-1",
            "C:\\data\\workspace",
            "你好",
            Some("gpt-x"),
            Some("low"),
        );
        assert_keys_belong(&turn, &properties_of(&v2, "TurnStartParams"), "turn/start");
        assert_eq!(turn["threadId"], json!("t-1"));
        assert_eq!(turn["input"][0]["type"], json!("text"));
        assert_eq!(turn["input"][0]["text"], json!("你好"));
        assert_eq!(turn["sandboxPolicy"]["type"], json!("readOnly"));
        assert_eq!(turn["sandboxPolicy"]["networkAccess"], json!(false));
        assert_eq!(turn["effort"], json!("low"));

        let interrupt = turn_interrupt_params("t-1", "turn-1");
        assert_keys_belong(
            &interrupt,
            &properties_of(&v2, "TurnInterruptParams"),
            "turn/interrupt",
        );
        let _ = &main;
    }

    #[test]
    fn parses_the_four_incoming_shapes() {
        let response = Incoming::parse(r#"{"id":7,"result":{"ok":true}}"#).unwrap();
        assert_eq!(
            response,
            Incoming::Response {
                id: 7,
                result: json!({ "ok": true })
            }
        );

        let error =
            Incoming::parse(r#"{"id":8,"error":{"code":-32600,"message":"bad","data":{"x":1}}}"#)
                .unwrap();
        match error {
            Incoming::Error { id, error } => {
                assert_eq!(id, 8);
                assert_eq!(error.code, -32600);
                assert_eq!(error.data, Some(json!({ "x": 1 })));
            }
            other => panic!("应是 Error：{other:?}"),
        }

        let request = Incoming::parse(
            r#"{"id":9,"method":"item/commandExecution/requestApproval","params":{"command":"whoami"}}"#,
        )
        .unwrap();
        match request {
            Incoming::ServerRequest { id, method, params } => {
                assert_eq!(id, 9);
                assert_eq!(method, method::COMMAND_APPROVAL);
                assert_eq!(params["command"], json!("whoami"));
            }
            other => panic!("应是 ServerRequest：{other:?}"),
        }

        let notification =
            Incoming::parse(r#"{"method":"turn/completed","params":{"turn":{"id":"t"}}}"#).unwrap();
        match notification {
            Incoming::Notification { method, params } => {
                assert_eq!(method, "turn/completed");
                assert_eq!(params["turn"]["id"], json!("t"));
            }
            other => panic!("应是 Notification：{other:?}"),
        }

        assert!(
            Incoming::parse(r#"{"foo":1}"#).is_err(),
            "既没 id 也没 method 要报错"
        );
        assert!(Incoming::parse("not json").is_err());
    }

    #[test]
    fn outbound_messages_serialize_to_the_expected_shape() {
        let request = Request {
            id: 1,
            method: method::TURN_INTERRUPT,
            params: turn_interrupt_params("t", "u"),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["id"], json!(1));
        assert_eq!(value["method"], json!("turn/interrupt"));
        assert_eq!(value["params"]["threadId"], json!("t"));

        let notification = OutNotification {
            method: method::INITIALIZED,
            params: json!({}),
        };
        let value = serde_json::to_value(&notification).unwrap();
        assert!(value.get("id").is_none(), "通知不带 id");
        assert_eq!(value["method"], json!("initialized"));

        let response = Response {
            id: 9,
            result: json!({ "decision": "decline" }),
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["result"]["decision"], json!("decline"));

        let error = ErrorResponse {
            id: 9,
            error: RpcError {
                code: -32601,
                message: "unsupported".into(),
                data: None,
            },
        };
        let value = serde_json::to_value(&error).unwrap();
        assert!(value["error"].get("data").is_none(), "data 为空时不出现");
    }

    #[test]
    fn rejectable_error_response_shape_is_rpc_compatible() {
        // 未知方法回 -32601（Method not found，和 Netcatty 一致）。
        let error = RpcError {
            code: -32601,
            message: "不支持的请求：item/tool/call".into(),
            data: None,
        };
        let back: RpcError = serde_json::from_value(serde_json::to_value(&error).unwrap()).unwrap();
        assert_eq!(back, error);
    }
}
