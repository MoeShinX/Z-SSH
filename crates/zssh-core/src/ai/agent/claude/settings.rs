//! Claude Code 自己配置的模型和思考等级（面板模型下拉框里"默认（opus · high）"用）。
//!
//! 只读、读不了就不显示、不猜。来源和优先级按 Claude Code 2.1.302 的实际行为：
//!
//! - 模型：`--model`（我们选"默认"时不传）> `ANTHROPIC_MODEL` 环境变量 > settings.json
//!   的 `model`。settings.json 的 `env` 块会在 Claude Code 启动时并进进程环境，所以
//!   `env.ANTHROPIC_MODEL` 和真实环境变量是同一层；**两边都有而且不一样时**，我们判断
//!   不了谁覆盖谁，按"不确定"不显示。
//! - 思考等级：`CLAUDE_CODE_EFFORT_LEVEL`（同样有 settings `env` 块那一层）> settings.json
//!   的 `effortLevel`（取值 low / medium / high / xhigh；`auto` 之类不认的值不显示）。
//! - 管理员下发的 managed-settings.json 优先级最高：里面写了就用它的；文件在但读不了，
//!   整体按"不确定"。
//!
//! 凭据文件（`.credentials.json`）一律不读——这里只碰 settings.json。

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::config::EFFORT_LEVELS;

/// 读一个 settings 文件的结果。
enum SettingsRead {
    /// 文件不存在。
    Missing,
    /// 文件在，但读不出来或解析不了（含空文件）：不确定。
    Unreadable,
    Json(Value),
}

fn read_settings(path: &Path) -> SettingsRead {
    if !path.exists() {
        return SettingsRead::Missing;
    }
    let Some(text) = crate::ai::agent::text::read_text_lossy(path) else {
        return SettingsRead::Unreadable;
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => SettingsRead::Json(value),
        _ => SettingsRead::Unreadable,
    }
}

/// 字符串值：去空白，空的当没有。
fn text_of(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 一层"环境变量"：真实环境变量和 settings 的 `env` 块。两边都有且不同 = 判断不了。
fn env_layer(process: Option<String>, settings_env: Option<String>) -> Result<Option<String>, ()> {
    match (process, settings_env) {
        (Some(a), Some(b)) if a != b => Err(()),
        (a, b) => Ok(b.or(a)),
    }
}

/// 算出"默认"的显示文字（`opus` / `opus · high`）。
///
/// `env` 注入环境变量（单测用）；`config_dir` 是用户的配置目录（`~\.claude` 或
/// `CLAUDE_CONFIG_DIR`）；`managed` 是管理员配置文件的候选路径。
pub fn configured_model_label(
    env: &dyn Fn(&str) -> Option<String>,
    config_dir: Option<&Path>,
    managed: &[PathBuf],
) -> Option<String> {
    let mut managed_values = Vec::new();
    for path in managed {
        match read_settings(path) {
            SettingsRead::Missing => {}
            SettingsRead::Unreadable => return None,
            SettingsRead::Json(value) => managed_values.push(value),
        }
    }
    let user = match config_dir.map(|dir| read_settings(&dir.join("settings.json"))) {
        None | Some(SettingsRead::Missing) => Value::Null,
        Some(SettingsRead::Unreadable) => return None,
        Some(SettingsRead::Json(value)) => value,
    };

    let managed_model = managed_values
        .iter()
        .find_map(|v| text_of(&v["env"]["ANTHROPIC_MODEL"]).or_else(|| text_of(&v["model"])));
    let model = match managed_model {
        Some(model) => model,
        None => {
            let from_env = env_layer(
                env("ANTHROPIC_MODEL").filter(|s| !s.trim().is_empty()),
                text_of(&user["env"]["ANTHROPIC_MODEL"]),
            )
            .ok()?;
            from_env.or_else(|| text_of(&user["model"]))?
        }
    };
    // "default" 就是"用 Claude Code 自己的默认"，等于什么都没说。
    if model.eq_ignore_ascii_case("default") {
        return None;
    }

    let managed_effort = managed_values.iter().find_map(|v| {
        text_of(&v["env"]["CLAUDE_CODE_EFFORT_LEVEL"]).or_else(|| text_of(&v["effortLevel"]))
    });
    let effort = match managed_effort {
        Some(effort) => Some(effort),
        None => match env_layer(
            env("CLAUDE_CODE_EFFORT_LEVEL").filter(|s| !s.trim().is_empty()),
            text_of(&user["env"]["CLAUDE_CODE_EFFORT_LEVEL"]),
        ) {
            Ok(Some(effort)) => Some(effort),
            Ok(None) => text_of(&user["effortLevel"]),
            // 等级判断不了只是少显示一半，模型照常显示。
            Err(()) => None,
        },
    }
    .map(|e| e.to_ascii_lowercase())
    .filter(|e| EFFORT_LEVELS.contains(&e.as_str()));

    Some(match effort {
        Some(effort) => format!("{model} · {effort}"),
        None => model,
    })
}

/// 管理员配置文件的候选路径（Windows 上 Claude Code 认的位置；按环境变量拼，不写死盘符）。
pub fn managed_settings_paths() -> Vec<PathBuf> {
    ["ProgramFiles", "ProgramData"]
        .iter()
        .filter_map(std::env::var_os)
        .filter(|dir| !dir.is_empty())
        .map(|dir| {
            PathBuf::from(dir)
                .join("ClaudeCode")
                .join("managed-settings.json")
        })
        .collect()
}

/// 用本机的真实环境和配置算一次。
pub fn configured_model_label_from_system() -> Option<String> {
    let dir = super::login::config_dir();
    configured_model_label(
        &|key| std::env::var(key).ok(),
        dir.as_deref(),
        &managed_settings_paths(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn dir_with(settings: Option<&str>) -> crate::test_util::TestDir {
        let dir = crate::test_util::TestDir::new("claude settings 模型");
        if let Some(text) = settings {
            std::fs::write(dir.join("settings.json"), text).unwrap();
        }
        dir
    }

    #[test]
    fn settings_model_and_effort_level() {
        let dir = dir_with(Some(r#"{"model":"opus","effortLevel":"high"}"#));
        let label = configured_model_label(&env_of(&[]), Some(&*dir), &[]);
        assert_eq!(label.as_deref(), Some("opus · high"));
    }

    #[test]
    fn model_only_when_effort_is_missing_or_unknown() {
        let dir = dir_with(Some(r#"{"model":"sonnet","effortLevel":"auto"}"#));
        let label = configured_model_label(&env_of(&[]), Some(&*dir), &[]);
        assert_eq!(label.as_deref(), Some("sonnet"));
    }

    #[test]
    fn env_var_beats_settings_model() {
        let dir = dir_with(Some(r#"{"model":"sonnet"}"#));
        let env = env_of(&[
            ("ANTHROPIC_MODEL", "opus"),
            ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
        ]);
        let label = configured_model_label(&env, Some(&*dir), &[]);
        assert_eq!(label.as_deref(), Some("opus · max"));
    }

    #[test]
    fn settings_env_block_counts_as_env_layer() {
        let dir = dir_with(Some(
            r#"{"model":"sonnet","env":{"ANTHROPIC_MODEL":"haiku"}}"#,
        ));
        let label = configured_model_label(&env_of(&[]), Some(&*dir), &[]);
        assert_eq!(label.as_deref(), Some("haiku"));
    }

    #[test]
    fn conflicting_env_layers_are_uncertain() {
        let dir = dir_with(Some(r#"{"env":{"ANTHROPIC_MODEL":"haiku"}}"#));
        let env = env_of(&[("ANTHROPIC_MODEL", "opus")]);
        assert_eq!(configured_model_label(&env, Some(&*dir), &[]), None);
    }

    #[test]
    fn nothing_configured_shows_nothing() {
        let dir = dir_with(None);
        assert_eq!(configured_model_label(&env_of(&[]), Some(&*dir), &[]), None);
        assert_eq!(configured_model_label(&env_of(&[]), None, &[]), None);
        // 只有等级、没有模型：不知道默认是哪个模型，不显示。
        let dir = dir_with(Some(r#"{"effortLevel":"high"}"#));
        assert_eq!(configured_model_label(&env_of(&[]), Some(&*dir), &[]), None);
        let dir = dir_with(Some(r#"{"model":"default"}"#));
        assert_eq!(configured_model_label(&env_of(&[]), Some(&*dir), &[]), None);
    }

    #[test]
    fn broken_or_empty_settings_are_uncertain_not_a_crash() {
        for text in ["", "{ not json", "[1,2]"] {
            let dir = dir_with(Some(text));
            let env = env_of(&[("ANTHROPIC_MODEL", "opus")]);
            assert_eq!(
                configured_model_label(&env, Some(&*dir), &[]),
                None,
                "坏文件 {text:?} 要按不确定处理"
            );
        }
        // 带 BOM 的照常读。
        let dir = dir_with(Some("\u{feff}{\"model\":\"opus\"}"));
        assert_eq!(
            configured_model_label(&env_of(&[]), Some(&*dir), &[]).as_deref(),
            Some("opus")
        );
    }

    #[test]
    fn managed_settings_win_and_unreadable_managed_is_uncertain() {
        let dir = dir_with(Some(r#"{"model":"sonnet","effortLevel":"low"}"#));
        let managed = dir.join("managed-settings.json");
        std::fs::write(&managed, r#"{"model":"opus","effortLevel":"xhigh"}"#).unwrap();
        let label =
            configured_model_label(&env_of(&[]), Some(&*dir), std::slice::from_ref(&managed));
        assert_eq!(label.as_deref(), Some("opus · xhigh"));

        std::fs::write(&managed, "坏的").unwrap();
        assert_eq!(
            configured_model_label(&env_of(&[]), Some(&*dir), &[managed]),
            None
        );
        // 不存在的候选路径不影响。
        let missing = dir.join("没有这个文件.json");
        assert_eq!(
            configured_model_label(&env_of(&[]), Some(&*dir), &[missing]).as_deref(),
            Some("sonnet · low")
        );
    }

    #[test]
    fn credentials_file_is_never_consulted() {
        // 凭据文件里就算写了 model 也不该被读到（这里只碰 settings.json）。
        let dir = dir_with(None);
        std::fs::write(dir.join(".credentials.json"), r#"{"model":"opus"}"#).unwrap();
        assert_eq!(configured_model_label(&env_of(&[]), Some(&*dir), &[]), None);
    }
}
