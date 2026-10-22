// Adapted from Netcatty electron/bridges/aiBridge/sdk/claudeDriver.cjs (启动参数、
// 工具白/黑名单、模型选择拆分的思路), rewritten in Rust for Z-SSH.
//! Claude Code 的启动参数与模型目录（AI-4 第 1、3、5 节）。
//!
//! 全部是纯函数 / 纯数据，参数清单用单测锁住（卡片第 8.1 节要求"锁住第 3 节的
//! 每一个参数"）。实测依据（2.1.300，2026-10-16，详见报告 AI-4）：
//!
//! - `--tools ""`：不给任何内置工具（本机命令 / 文件工具全没有）；
//! - `--disallowedTools`：兜底再列一遍本机能力和"会等界面回答"的工具；
//! - `--allowedTools mcp__zssh__*`：只放行 zssh 的工具——**不用
//!   `bypassPermissions`**，实测这个组合下 MCP 工具照常能调（Netcatty 用了
//!   bypass，我们不跟）；
//! - `--mcp-config <数据目录下的临时文件> --strict-mcp-config`：只加载 zssh 一个
//!   MCP 服务，用户自己的配置一律不读；
//! - `--settings '{"disableAllHooks":true}'`：用户 settings.json 里的 hooks 不触发
//!   （实测：带 SessionStart hook 的独立配置目录，不传这个就触发、传了不触发），
//!   同时**不覆盖**用户设置（这是"额外一层"，用户的 env / model 照常生效，实测）；
//! - 不用 `--bare`（它会跳过 OAuth 登录）；不用 `--restricted`（它会忽略用户
//!   settings，和"模型/登录用用户的配置"冲突）。

use std::ffi::OsString;

use super::super::config::{McpServerSpec, ZSSH_TOOL_NAMES};

/// 最低支持的 Claude Code 版本。依据（报告 AI-4 第 1 节）：`--tools`（卡里要求拿来
/// 关掉全部内置工具的开关）从 2.0.41 开始有（2.0.40 及以前没有）；2.0.41 上把完整
/// 配方 + 真实会话（MCP 调用、本机拒绝、中断）实测跑通。
pub const MIN_VERSION: (u64, u64, u64) = (2, 0, 41);

/// 兜底禁用清单：本机能力 + 会等界面回答的工具（Netcatty 的 UI_DISALLOWED_TOOLS
/// 那两个也列上）。`--tools ""` 已经一个内置工具都不给，这里是第二层。
pub const DISALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Write",
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "NotebookEdit",
    "Task",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
];

/// 放行的工具：只有 zssh 这一个 MCP 服务的工具（`mcp__<名字>__<工具>`，清单见
/// [`ZSSH_TOOL_NAMES`]，含 M3-4 的四个文件工具）。
pub fn allowed_tools(mcp_name: &str) -> String {
    ZSSH_TOOL_NAMES
        .iter()
        .map(|tool| format!("mcp__{mcp_name}__{tool}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 这个 claude 版本支持哪些可选参数（`--help` 里探测；未知参数会让 claude 直接
/// 报错退出——2.0.41 实测 `error: unknown option`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaudeFlags {
    /// 支持 `--effort <level>`（2.1.x 才有）。
    pub effort: bool,
    /// 支持 `--permission-prompts none`（2.1.x 才有）。
    pub permission_prompts: bool,
}

/// 从 `claude --help` 的输出里探测可选参数。
pub fn detect_flags(help_text: &str) -> ClaudeFlags {
    ClaudeFlags {
        effort: help_text.contains("--effort"),
        permission_prompts: help_text.contains("--permission-prompts"),
    }
}

/// 一次 `send` 的模型选择：`模型/强度`（强度缺省 = 用 Claude Code 自己的默认）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelChoice {
    /// `None` = 不传 `--model`（用用户配置里的模型）。
    pub model: Option<String>,
    /// `None` = 不传 `--effort`。
    pub effort: Option<String>,
}

/// 解析模型 id：`"default"` / `"sonnet"` / `"opus/high"` 这类。
pub fn parse_model_choice(model: Option<&str>) -> ModelChoice {
    let Some(raw) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return ModelChoice::default();
    };
    let (name, effort) = match raw.rsplit_once('/') {
        Some((name, effort)) if !name.is_empty() && EFFORT_LEVELS.contains(&effort) => {
            (name, Some(effort.to_string()))
        }
        _ => (raw, None),
    };
    ModelChoice {
        model: (name != "default").then(|| name.to_string()),
        effort,
    }
}

/// 思考强度（`--effort` 的取值，来自 `claude --help`）。
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// 模型别名：开发时实测（2.1.300）能跑通的（默认 + 三个别名；`fable` 实测跑不通，
/// 不放进列表）。
pub const MODEL_ALIASES: [(&str, &str); 3] =
    [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")];

/// 生成 `--mcp-config` 文件的内容（只含 zssh 一个服务）。
pub fn mcp_config_json(mcp: &McpServerSpec) -> String {
    let (command, args) = mcp
        .command
        .split_first()
        .map(|(first, rest)| (first.as_str(), rest))
        .unwrap_or(("", &[]));
    let env: serde_json::Map<String, serde_json::Value> = mcp
        .environment
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect();
    let mut servers = serde_json::Map::new();
    servers.insert(
        mcp.name.clone(),
        serde_json::json!({
            "type": "stdio",
            "command": command,
            "args": args,
            "env": env,
        }),
    );
    serde_json::json!({ "mcpServers": servers }).to_string()
}

/// 启动参数的组装（纯函数，单测锁住卡里要求的每一项）。
pub struct StartArgs {
    /// MCP 服务名（放行清单里要拼成 `mcp__<名字>__<工具>`）。
    pub mcp_name: String,
    pub mcp_config_path: String,
    pub capability_prompt: String,
    pub choice: ModelChoice,
    pub flags: ClaudeFlags,
    /// 恢复会话：`Some` 时加 `--resume <id>`（上下文不丢）。
    pub resume: Option<String>,
}

/// 交给模型的系统提示：说明能力密钥的用途。
pub fn system_prompt(capability: &str) -> String {
    crate::ai::agent::config::system_prompt(capability)
}

/// 组装命令行参数（`claude` 之后的全部参数）。
pub fn build_args(args: &StartArgs) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        // 本机工具：一个不给（第一层），列黑名单兜底（第二层）。
        "--tools".into(),
        "".into(),
        "--disallowedTools".into(),
        DISALLOWED_TOOLS.join(",").into(),
        // 只放行 zssh 的 MCP 工具（不用 bypassPermissions）。
        "--allowedTools".into(),
        allowed_tools(&args.mcp_name).into(),
        // 只加载 zssh 的 MCP 配置。
        "--mcp-config".into(),
        args.mcp_config_path.clone().into(),
        "--strict-mcp-config".into(),
        // 用户的 hooks 不触发；不覆盖用户设置（额外一层）。
        "--settings".into(),
        "{\"disableAllHooks\":true}".into(),
        "--append-system-prompt".into(),
        args.capability_prompt.clone().into(),
    ];
    if args.flags.permission_prompts {
        // 任何要弹权限的东西一律自动拒绝（我们这边没有可交互的宿主）。
        out.push("--permission-prompts".into());
        out.push("none".into());
    }
    if args.flags.effort {
        if let Some(effort) = &args.choice.effort {
            out.push("--effort".into());
            out.push(effort.clone().into());
        }
    }
    if let Some(model) = &args.choice.model {
        out.push("--model".into());
        out.push(model.clone().into());
    }
    if let Some(session) = &args.resume {
        out.push("--resume".into());
        out.push(session.clone().into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> McpServerSpec {
        McpServerSpec {
            name: "zssh".to_string(),
            command: vec![
                "C:\\data\\tools\\zssh-mcp.exe".to_string(),
                "--flag".to_string(),
            ],
            environment: vec![
                ("ZSSH_BRIDGE_PORT".to_string(), "51234".to_string()),
                ("ZSSH_BRIDGE_TOKEN".to_string(), "abc".to_string()),
            ],
        }
    }

    fn args_with(flags: ClaudeFlags) -> StartArgs {
        StartArgs {
            mcp_name: "zssh".to_string(),
            mcp_config_path: "C:\\data\\agent-workspace\\mcp.json".to_string(),
            capability_prompt: system_prompt("cap123"),
            choice: ModelChoice::default(),
            flags,
            resume: None,
        }
    }

    /// 卡片第 8.1 节：锁住第 3 节的每一个参数。
    #[test]
    fn lockdown_flags_are_pinned() {
        let args = build_args(&args_with(ClaudeFlags::default()));
        let shown: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(shown.starts_with(&["-p".to_string()]));
        // 1. 关掉全部内置工具 + 黑名单兜底。
        let tools_at = shown
            .iter()
            .position(|a| a == "--tools")
            .expect("要有 --tools");
        assert_eq!(shown[tools_at + 1], "", "--tools 要给空串");
        let denied_at = shown
            .iter()
            .position(|a| a == "--disallowedTools")
            .expect("要有 --disallowedTools");
        for tool in [
            "Bash", "Edit", "Write", "Read", "Glob", "Grep", "WebFetch", "Task",
        ] {
            assert!(
                shown[denied_at + 1].contains(tool),
                "黑名单里要有 {tool}：{}",
                shown[denied_at + 1]
            );
        }
        // 2. 只放行 zssh 的工具；绝对不能出现 bypassPermissions。
        let allowed_at = shown
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("要有 --allowedTools");
        assert_eq!(
            shown[allowed_at + 1],
            "mcp__zssh__list_targets,mcp__zssh__run_command,mcp__zssh__read_terminal,".to_owned()
                + "mcp__zssh__list_dir,mcp__zssh__read_file,mcp__zssh__write_file,mcp__zssh__stat,"
                + "mcp__zssh__remember_host"
        );
        // 放行清单里每一项都以 mcp__zssh__ 开头：不放行任何别的东西。
        assert!(
            shown[allowed_at + 1]
                .split(',')
                .all(|tool| tool.starts_with("mcp__zssh__")),
            "只放行 zssh 的工具：{}",
            shown[allowed_at + 1]
        );
        assert!(
            !shown
                .iter()
                .any(|a| a.contains("bypassPermissions")
                    || a.contains("dangerously-skip-permissions")),
            "不许出现 bypass 类参数"
        );
        // 3. MCP 只加载我们给的那份配置。
        let mcp_at = shown
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("要有 --mcp-config");
        assert!(shown[mcp_at + 1].ends_with("mcp.json"));
        assert!(shown.contains(&"--strict-mcp-config".to_string()));
        // 4. hooks 关掉（并且是"额外一层"，不覆盖用户设置）。
        let settings_at = shown
            .iter()
            .position(|a| a == "--settings")
            .expect("要有 --settings");
        assert_eq!(shown[settings_at + 1], "{\"disableAllHooks\":true}");
        // 5. 能力密钥走 append-system-prompt。
        let prompt_at = shown
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("要有 --append-system-prompt");
        assert!(shown[prompt_at + 1].contains("cap123"));
        // 6. 不用 --bare（会跳过 OAuth 登录）/ --restricted（会忽略用户设置）。
        assert!(!shown.iter().any(|a| a == "--bare" || a == "--restricted"));
    }

    #[test]
    fn optional_flags_follow_the_detected_support() {
        // 老版本（2.0.41）不支持 --effort / --permission-prompts：一个都不传。
        let old = build_args(&args_with(ClaudeFlags::default()));
        let shown: Vec<String> = old
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!shown.contains(&"--effort".to_string()));
        assert!(!shown.contains(&"--permission-prompts".to_string()));

        // 新版本（2.1.300）：要啥有啥。
        let mut args = args_with(ClaudeFlags {
            effort: true,
            permission_prompts: true,
        });
        args.choice = ModelChoice {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
        };
        let shown: Vec<String> = build_args(&args)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(shown.contains(&"--permission-prompts".to_string()));
        assert_eq!(
            shown[shown.iter().position(|a| a == "--effort").unwrap() + 1],
            "high"
        );
        assert_eq!(
            shown[shown.iter().position(|a| a == "--model").unwrap() + 1],
            "sonnet"
        );

        // 探测到支持但这次没选强度：不传 --effort。
        let mut no_effort = args_with(ClaudeFlags {
            effort: true,
            permission_prompts: true,
        });
        no_effort.choice = ModelChoice {
            model: None,
            effort: None,
        };
        let shown: Vec<String> = build_args(&no_effort)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!shown.contains(&"--effort".to_string()));
        assert!(!shown.contains(&"--model".to_string()));
    }

    #[test]
    fn resume_is_passed_through() {
        let mut args = args_with(ClaudeFlags::default());
        args.resume = Some("sess-123".into());
        let shown: Vec<String> = build_args(&args)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = shown
            .iter()
            .position(|a| a == "--resume")
            .expect("要有 --resume");
        assert_eq!(shown[at + 1], "sess-123");
    }

    #[test]
    fn detects_optional_flags_from_help_text() {
        // 2.1.300 的 help 里两个都有；2.0.41 的都没有。
        let new_help = "... --effort <level>  Effort level ... --permission-prompts <target> ...";
        assert_eq!(
            detect_flags(new_help),
            ClaudeFlags {
                effort: true,
                permission_prompts: true
            }
        );
        assert_eq!(
            detect_flags("--tools <tools...>  ..."),
            ClaudeFlags::default()
        );
    }

    #[test]
    fn model_ids_split_into_model_and_effort() {
        assert_eq!(parse_model_choice(None), ModelChoice::default());
        assert_eq!(parse_model_choice(Some("")), ModelChoice::default());
        assert_eq!(parse_model_choice(Some("default")), ModelChoice::default());
        assert_eq!(
            parse_model_choice(Some("sonnet")),
            ModelChoice {
                model: Some("sonnet".into()),
                effort: None
            }
        );
        assert_eq!(
            parse_model_choice(Some("opus/high")),
            ModelChoice {
                model: Some("opus".into()),
                effort: Some("high".into())
            }
        );
        assert_eq!(
            parse_model_choice(Some("default/max")),
            ModelChoice {
                model: None,
                effort: Some("max".into())
            },
            "default/强度 = 不传模型、只传强度"
        );
        // 尾巴不是已知强度就当模型名本身。
        assert_eq!(
            parse_model_choice(Some("sonnet/turbo")).model.as_deref(),
            Some("sonnet/turbo")
        );
    }

    #[test]
    fn mcp_config_contains_only_our_server() {
        let json: serde_json::Value = serde_json::from_str(&mcp_config_json(&spec())).unwrap();
        let servers = json["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1, "只能有 zssh 一个服务");
        let zssh = &servers["zssh"];
        assert_eq!(zssh["type"], "stdio");
        assert_eq!(zssh["command"], "C:\\data\\tools\\zssh-mcp.exe");
        assert_eq!(zssh["args"][0], "--flag");
        assert_eq!(zssh["env"]["ZSSH_BRIDGE_PORT"], "51234");
    }
}
