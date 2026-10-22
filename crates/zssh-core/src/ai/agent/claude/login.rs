// Adapted from Netcatty electron/bridges/ai/codexHelpers.cjs / claudeAuth.cjs（登录判断
// 的组合思路），rewritten in Rust for Z-SSH.
//! Claude Code 的登录判断（AI-4 第 4 节）。
//!
//! **只判断"有没有登录，绝不读取或打印密钥的值"**：`auth status` 的 JSON 里只取
//! `loggedIn`；环境变量 / 凭据文件 / settings.json 的 env 块都只做"存不存在"判断，
//! 值一律不外传、不打日志。
//!
//! 顺序（卡片给的）：
//! 1. `claude auth status`（2.1.300 有这个子命令，输出 JSON）；
//! 2. 兜底：`ANTHROPIC_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_AUTH_TOKEN`
//!    环境变量；`~/.claude/.credentials.json`；**`settings.json` 的 `env` 块里有没有
//!    这几个键**（Netcatty 漏了这一条，本卡补上）。
//!
//! 不改 `CLAUDE_CONFIG_DIR` / `HOME`（登录和模型都用用户自己的配置）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::discovery;

/// `auth status` 探测的超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// 登录状态。
///
/// `logged_in` 为 `None` = **不确定**（用户的配置在、但读不出来：编码坏了、没有读权限
/// ……）——这时只提示、不阻断（R-2 第 3 节）；`Some(false)` 才是"确定没登录"。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ClaudeLogin {
    pub logged_in: Option<bool>,
    pub hint: Option<String>,
}

/// settings.json 的 env 块里算"有登录信息"的键（值不读、不打印）。
pub const AUTH_ENV_KEYS: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_AUTH_TOKEN",
];

/// 用户的配置目录：`CLAUDE_CONFIG_DIR`（用户自己设了就按它找）→ `~\.claude`。
///
/// 注意这里只是**顺应用户的设置去读**，驱动自己不会改这个环境变量（登录和模型都用
/// 用户自己的配置，见 AI-4 第 4 节）。
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    let home = std::env::var_os("USERPROFILE")?;
    Some(PathBuf::from(home).join(".claude"))
}

/// 跑 `claude auth status` 判断（拿不到 / 解析不出返回 `None`，交给兜底）。
pub async fn login_status_cli(install_path: &str) -> Option<ClaudeLogin> {
    let mut cmd = discovery::build_command(Path::new(install_path));
    cmd.args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    discovery::hide_console(&mut cmd);
    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let logged_in = json["loggedIn"].as_bool()?;
    Some(if logged_in {
        ClaudeLogin {
            logged_in: Some(true),
            hint: None,
        }
    } else {
        ClaudeLogin {
            logged_in: Some(false),
            hint: Some("请先在终端里运行 `claude` 完成登录".to_string()),
        }
    })
}

/// 兜底判断：环境变量 / 凭据文件 / settings.json 的 env 块。
///
/// `env_set` 注入环境变量判断（单测用）；`config_dir` 注入配置目录（单测用假的）。
///
/// 容错（R-2 第 3 节）：配置文件**不存在 / 空 / 格式坏了 / 编码不对 / 带 BOM** 都不崩；
/// 其中"文件在但读不出来（含空的、解析不了的）"算**不确定**（`logged_in: None`）：
/// 提示用户、但不拦着他用（他自己的 Claude Code 可能早就登录好了，读不出来只是我们
/// 的读法受限）。只有"什么都没有"才是确定的未登录。
pub fn login_status_from_local(
    env_set: &dyn Fn(&str) -> bool,
    config_dir: Option<&Path>,
) -> Option<ClaudeLogin> {
    // ① 环境变量。
    if AUTH_ENV_KEYS.iter().any(|key| env_set(key)) {
        return Some(ClaudeLogin {
            logged_in: Some(true),
            hint: None,
        });
    }
    let dir = config_dir?;
    // ② 凭据文件（存在就算登录；内容不读）。
    if dir.join(".credentials.json").is_file() {
        return Some(ClaudeLogin {
            logged_in: Some(true),
            hint: None,
        });
    }
    // ③ settings.json 的 env 块里有没有这几个键（值不读、不打印）。
    let settings = dir.join("settings.json");
    let Some(text) = crate::ai::agent::text::read_text_lossy(&settings) else {
        // 读不出来：文件在（不是普通文件 / 没权限）就是"不确定"；压根没有才算确定没登录。
        if settings.exists() {
            return Some(uncertain(Some("读不了".to_string())));
        }
        return Some(ClaudeLogin {
            logged_in: Some(false),
            hint: Some("请先在终端里运行 `claude` 完成登录".to_string()),
        });
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Some(uncertain(Some("解析不了".to_string())));
    };
    let env = &json["env"];
    if AUTH_ENV_KEYS.iter().any(|key| env.get(key).is_some()) {
        return Some(ClaudeLogin {
            logged_in: Some(true),
            hint: None,
        });
    }
    // 读到了、也解析得了，里面没有凭据键：这才算确定没登录。
    Some(ClaudeLogin {
        logged_in: Some(false),
        hint: Some("请先在终端里运行 `claude` 完成登录".to_string()),
    })
}

/// 不确定的状态（配置文件读不出来）：提示用户，但不阻断使用。
fn uncertain(what: Option<String>) -> ClaudeLogin {
    let detail = what.unwrap_or_else(|| "读不了".to_string());
    ClaudeLogin {
        logged_in: None,
        hint: Some(format!(
            "没能确认登录状态（settings.json {detail}）。已经登录过就能直接用；不行就在终端里跑一次 `claude`"
        )),
    }
}

/// 真实的兜底判断（读进程环境 + 用户配置目录）。
pub fn login_status_fallback() -> Option<ClaudeLogin> {
    login_status_from_local(
        &|key| std::env::var_os(key).is_some(),
        config_dir().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> crate::test_util::TestDir {
        crate::test_util::TestDir::new(name)
    }

    #[test]
    fn env_vars_are_enough() {
        let dir = temp_dir("zssh-claude-login-env");
        let login = login_status_from_local(&|key| key == "ANTHROPIC_API_KEY", Some(&dir));
        assert_eq!(login.unwrap().logged_in, Some(true));
        // 只有无关的环境变量、目录里什么也没有：算确定没登录。
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, Some(false));
    }

    #[test]
    fn credentials_file_counts_but_is_never_read() {
        let dir = temp_dir("zssh-claude-login-creds");
        fs::write(dir.join(".credentials.json"), "{\"secret\":\"占位\"}").unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, Some(true), "有凭据文件就算登录");
        // （值有没有被读没法直接断言，实现里只 is_file —— 见代码。）
    }

    #[test]
    fn settings_json_env_block_counts() {
        let dir = temp_dir("zssh-claude-login-settings");
        fs::write(
            dir.join("settings.json"),
            r#"{"model":"opus","env":{"ANTHROPIC_AUTH_TOKEN":"占位值"}}"#,
        )
        .unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(
            login.logged_in,
            Some(true),
            "settings.json 的 env 块里有凭据键就算登录"
        );

        // env 块里没有这几个键：算确定没登录。
        fs::write(
            dir.join("settings.json"),
            r#"{"model":"opus","env":{"SOME_OTHER":"x"}}"#,
        )
        .unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, Some(false));
    }

    #[test]
    fn nothing_found_reports_logged_out_with_hint() {
        let dir = temp_dir("zssh-claude-login-empty");
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, Some(false));
        assert!(login.hint.unwrap().contains("claude"));
    }

    /// R-2 第 3 节：配置文件坏了 / 空的 / 编码不对 / 带 BOM 都不能崩，
    /// 而且"读不出来"要算**不确定**（不是"没登录"），不能把人挡住。
    #[test]
    fn broken_settings_json_reports_uncertain_not_logged_out() {
        let dir = temp_dir("zssh-claude-login-broken");

        // 格式坏了。
        fs::write(dir.join("settings.json"), b"{ this is not json ").unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, None, "解析不了要报不确定");
        assert!(login.hint.unwrap().contains("没能确认登录状态"));

        // 空文件。
        fs::write(dir.join("settings.json"), b"").unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, None, "空文件要报不确定");

        // 编码不是 UTF-8（GBK 的"中"），里面没有凭据键：不确定，但不崩。
        fs::write(dir.join("settings.json"), [b'{', 0xD6, 0xD0, b'}']).unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, None);

        // 带 BOM 的正常 JSON：要认得（BOM 吃掉之后能解析）。
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"env":{"ANTHROPIC_API_KEY":"placeholder"}}"#);
        fs::write(dir.join("settings.json"), &bytes).unwrap();
        let login = login_status_from_local(&|_| false, Some(&dir)).unwrap();
        assert_eq!(login.logged_in, Some(true), "带 BOM 也要认出来");

        // settings.json 是个目录（读不了）：不确定，不崩。
        let weird = temp_dir("zssh-claude-login-dir");
        fs::create_dir_all(weird.join("settings.json")).unwrap();
        let login = login_status_from_local(&|_| false, Some(&weird)).unwrap();
        assert_eq!(login.logged_in, None);
    }

    /// 配置目录读不到（`config_dir` 拿不到）时，兜底判断不了就返回 `None`
    /// （调用方按"不确定"处理），不能崩。
    #[test]
    fn missing_config_dir_is_not_a_crash() {
        assert!(login_status_from_local(&|_| false, None).is_none());
    }
}
