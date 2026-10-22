//! 从 Claude Code 自己那里拿模型目录（参考 Netcatty 的 `listClaudeModels`）。
//!
//! 起一个 stream-json 进程，只发一条 `initialize` 控制请求、不发任何对话（不计费），
//! 回包里的 `models` 就是 Claude Code 当前账号能用的模型：显示名、`default` 实际对应
//! 哪个模型（`resolvedModel`）、支持哪些思考强度。拿到就关进程。
//!
//! 回包按安装路径缓存在进程内（模型列表不会频繁变）；"默认"的显示文字每次都按用户
//! 当前的配置重新算。探测失败（版本太老、没登录、超时）就返回 None，调用方退回写死的
//! 别名列表。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::config::EFFORT_LEVELS;
use crate::ai::agent::{discovery, AgentModel, AgentModelCatalog};

const PROBE_TIMEOUT: Duration = Duration::from_secs(25);
const REQUEST_ID: &str = "zssh-catalog";
/// 回包里一行最多读这么长，免得异常输出把内存撑大。
const MAX_LINE: usize = 4 * 1024 * 1024;

fn cache() -> &'static tokio::sync::Mutex<HashMap<String, Value>> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Value>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// 这个安装的模型目录；`configured` 是用户配置里的模型（`opus · high` 这类，见
/// [`super::settings`]）。探测不到返回 None。
pub async fn live_catalog(
    install_path: &str,
    configured: Option<&str>,
) -> Option<AgentModelCatalog> {
    live_catalog_with_env(install_path, configured, &[]).await
}

/// 同 [`live_catalog`]，给 claude 子进程额外设环境变量（测试用它指向独立的
/// `CLAUDE_CONFIG_DIR`，不碰用户真实的 `~/.claude`）。
pub async fn live_catalog_with_env(
    install_path: &str,
    configured: Option<&str>,
    extra_env: &[(String, String)],
) -> Option<AgentModelCatalog> {
    let mut cache = cache().lock().await;
    let models = match cache.get(install_path) {
        Some(models) => models.clone(),
        None => {
            let models = probe_models(install_path, extra_env).await?;
            cache.insert(install_path.to_owned(), models.clone());
            models
        }
    };
    drop(cache);
    catalog_from_models(&models, configured)
}

/// 起进程、发 initialize、读回包里的 `models`。
async fn probe_models(install_path: &str, extra_env: &[(String, String)]) -> Option<Value> {
    let mut cmd = discovery::build_command(std::path::Path::new(install_path));
    cmd.envs(extra_env.iter().map(|(k, v)| (k, v)));
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // 和真实对话一样隔离：不加载用户的 MCP 服务、不触发用户的 hooks。
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--settings",
        "{\"disableAllHooks\":true}",
    ])
    .current_dir(std::env::temp_dir())
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    discovery::hide_console(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    super::super::process::bind_to_app_lifetime(&child);

    let request = serde_json::json!({
        "type": "control_request",
        "request_id": REQUEST_ID,
        "request": { "subtype": "initialize" },
    });
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let read = async move {
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .ok()?;
        stdin.flush().await.ok()?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).await.ok()?;
            if read == 0 || line.len() > MAX_LINE {
                return None;
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(models) = models_of_response(&message) {
                // stdin 留到这里才放掉：提前关掉 claude 会以为输入结束、直接退出
                drop(stdin);
                return Some(models);
            }
        }
    };
    let result = tokio::time::timeout(PROBE_TIMEOUT, read)
        .await
        .ok()
        .flatten();
    let _ = child.kill().await;
    if result.is_none() {
        tracing::info!("没能从 Claude Code 取到模型目录，用内置的别名列表");
    }
    result
}

/// initialize 的成功回包 → `models` 数组。
fn models_of_response(message: &Value) -> Option<Value> {
    if message["type"] != "control_response" {
        return None;
    }
    let response = &message["response"];
    if response["request_id"] != REQUEST_ID || response["subtype"] != "success" {
        return None;
    }
    let models = &response["response"]["models"];
    models.is_array().then(|| models.clone())
}

/// 模型 id 会作为 `--model` 的参数：只收普通字符，不能以 `-` 开头（免得被当成开关）。
fn usable_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '[' | ']'))
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn efforts_of(model: &Value) -> Vec<&'static str> {
    if model["supportsEffort"] == false {
        return Vec::new();
    }
    match model["supportedEffortLevels"].as_array() {
        Some(levels) => EFFORT_LEVELS
            .into_iter()
            .filter(|level| levels.iter().any(|l| l == *level))
            .collect(),
        None => Vec::new(),
    }
}

/// `models` 数组 → 界面用的目录。纯函数，单测用样本回包。
pub fn catalog_from_models(models: &Value, configured: Option<&str>) -> Option<AgentModelCatalog> {
    let list = models.as_array()?;
    let entry = |id: String, name: String| AgentModel {
        id,
        name,
        unavailable: false,
    };
    let mut out = Vec::new();
    let mut default_entry: Option<&Value> = None;

    // 显示名重复时带上 id，免得两行一模一样
    let display = |model: &Value, id: &str| text(&model["displayName"]).unwrap_or(id).to_owned();
    let mut seen_names: HashMap<String, usize> = HashMap::new();
    for model in list {
        if let Some(id) = text(&model["value"]) {
            *seen_names.entry(display(model, id)).or_default() += 1;
        }
    }

    for model in list {
        let Some(id) = text(&model["value"]) else {
            continue;
        };
        if id == "default" {
            default_entry = Some(model);
            continue;
        }
        if !usable_model_id(id) {
            continue;
        }
        let mut name = display(model, id);
        if seen_names.get(&name).copied().unwrap_or(0) > 1 {
            name = format!("{name}（{id}）");
        }
        out.push(entry(id.to_owned(), name.clone()));
        for effort in efforts_of(model) {
            out.push(entry(
                format!("{id}/{effort}"),
                format!("{name} · {effort}"),
            ));
        }
    }
    if out.is_empty() && default_entry.is_none() {
        return None;
    }

    let mut models = vec![entry("default".into(), "默认（Claude Code 的设置）".into())];
    if let Some(default) = default_entry {
        for effort in efforts_of(default) {
            models.push(entry(
                format!("default/{effort}"),
                format!("默认 · {effort}"),
            ));
        }
    }
    models.extend(out);

    Some(AgentModelCatalog {
        models,
        default: Some("default".to_owned()),
        default_label: default_label(list, default_entry, configured),
    })
}

/// "默认"实际是哪个模型。用户配置里写了模型就按它（别名换成显示名），否则用 Claude Code
/// 报的 `default` 对应的模型；思考强度只有配置里写了才显示。
fn default_label(
    list: &[Value],
    default: Option<&Value>,
    configured: Option<&str>,
) -> Option<String> {
    let (configured_model, effort) =
        match configured.map(|c| c.split_once(" · ").unwrap_or((c, ""))) {
            Some((model, effort)) => (
                Some(model.trim()),
                Some(effort.trim()).filter(|e| !e.is_empty()),
            ),
            None => (None, None),
        };
    let name_of = |key: &str| {
        list.iter()
            .filter(|m| text(&m["value"]) != Some("default"))
            .find(|m| text(&m["value"]) == Some(key) || text(&m["resolvedModel"]) == Some(key))
            .and_then(|m| text(&m["displayName"]))
            .map(str::to_owned)
    };
    let model = match configured_model {
        Some(model) => name_of(model).unwrap_or_else(|| model.to_owned()),
        None => {
            let default = default?;
            let resolved = text(&default["resolvedModel"]);
            resolved
                .and_then(name_of)
                .or_else(|| {
                    // 例如 "Sonnet 5.5 · Efficient for routine tasks"
                    text(&default["description"])
                        .and_then(|d| d.split(" · ").next())
                        .map(|d| d.trim().to_owned())
                        .filter(|d| !d.is_empty())
                })
                .or_else(|| resolved.map(str::to_owned))?
        }
    };
    Some(match effort {
        Some(effort) => format!("{model} · {effort}"),
        None => model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!([
            {"value": "default", "resolvedModel": "claude-sonnet-5-5", "displayName": "Default (recommended)",
             "description": "Sonnet 5.5 · Efficient for routine tasks", "supportsEffort": true,
             "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
            {"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus 5.5",
             "supportsEffort": true, "supportedEffortLevels": ["low", "high"]},
            {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "displayName": "Sonnet 5.5",
             "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
            {"value": "claude-haiku-4-5-20251001", "resolvedModel": "claude-haiku-4-5-20251001",
             "displayName": "Haiku 4.5", "supportsThinkingOff": true},
            {"value": "--dangerously-skip-permissions", "displayName": "坏的"},
            {"value": "x y", "displayName": "也是坏的"}
        ])
    }

    #[test]
    fn default_shows_the_model_claude_code_resolves_it_to() {
        let catalog = catalog_from_models(&sample(), None).unwrap();
        assert_eq!(catalog.default.as_deref(), Some("default"));
        assert_eq!(catalog.default_label.as_deref(), Some("Sonnet 5.5"));
        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids[0], "default");
        assert!(ids.contains(&"default/xhigh"));
        assert!(
            ids.contains(&"opus") && ids.contains(&"opus/high") && !ids.contains(&"opus/medium")
        );
        assert!(ids.contains(&"claude-haiku-4-5-20251001"));
        assert!(
            !ids.iter()
                .any(|id| id.starts_with("claude-haiku-4-5-20251001/")),
            "不支持强度的不展开"
        );
        assert!(
            !ids.iter()
                .any(|id| id.starts_with("--") || id.contains(' ')),
            "坏 id 不进列表"
        );
        let opus = catalog.models.iter().find(|m| m.id == "opus/high").unwrap();
        assert_eq!(opus.name, "Opus 5.5 · high");
    }

    #[test]
    fn a_configured_model_wins_and_aliases_get_their_display_name() {
        let label = |c: &str| {
            catalog_from_models(&sample(), Some(c))
                .unwrap()
                .default_label
        };
        assert_eq!(label("opus · high").as_deref(), Some("Opus 5.5 · high"));
        assert_eq!(label("claude-opus-5-5").as_deref(), Some("Opus 5.5"));
        assert_eq!(
            label("some-custom-model").as_deref(),
            Some("some-custom-model")
        );
    }

    #[test]
    fn falls_back_to_the_description_then_none() {
        let only_default = json!([{"value": "default", "description": "Opus 5.5 · Best"}]);
        assert_eq!(
            catalog_from_models(&only_default, None)
                .unwrap()
                .default_label
                .as_deref(),
            Some("Opus 5.5")
        );
        assert!(catalog_from_models(&json!([]), None).is_none());
        assert!(catalog_from_models(&json!("x"), None).is_none());
    }

    #[test]
    fn only_our_successful_initialize_response_counts() {
        let ok = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": REQUEST_ID, "response": {"models": [{"value": "opus"}]}}});
        assert!(models_of_response(&ok).is_some());
        let other_id = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": "x", "response": {"models": []}}});
        assert!(models_of_response(&other_id).is_none());
        let error = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": REQUEST_ID, "error": "nope"}});
        assert!(models_of_response(&error).is_none());
        assert!(models_of_response(&json!({"type": "system", "subtype": "init"})).is_none());
    }

    #[test]
    fn duplicate_display_names_get_the_id() {
        let dup = json!([
            {"value": "a", "displayName": "Same"},
            {"value": "b", "displayName": "Same"}
        ]);
        let names: Vec<String> = catalog_from_models(&dup, None)
            .unwrap()
            .models
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(names.contains(&"Same（a）".to_owned()) && names.contains(&"Same（b）".to_owned()));
    }
}
