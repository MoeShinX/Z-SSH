//! codex 的启动策略（AI-3 第 2 / 3 节）：本机能力关掉、只挂 `zssh` 一个 MCP。
//!
//! 两条规则都**实测过**（0.162.1，2026-10-16，报告 AI-3 第 2、3 节）：
//!
//! 1. **本机工具用 `--disable <特性>` 关**。清单按 `codex features list` 过滤后再传
//!    （未知特性名会让 codex 直接报错退出——0.144.3 实测），所以版本差异是安全的。
//!    `code_mode_host` **故意不禁**：它是工具调用的执行宿主，禁掉之后连 `zssh` 的
//!    MCP 工具都调不动（实测模型报 "code-mode host is disabled"）。
//! 2. **MCP 服务只留 `zssh`**：`-c mcp_servers.zssh={...}` 是**逐键合并**、不会整表
//!    替换（实测），所以用户自己配置里的每个服务都要补一条
//!    `-c mcp_servers.<名字>.enabled=false`（名字来自**只读扫** `config.toml` 的
//!    `[mcp_servers.*]` 段；`codex mcp list --json` 混着插件服务，不能直接用，见
//!    `startup.rs` 的注释）；插件提供的服务用 `--disable plugins` 关、ChatGPT 侧的
//!    应用用 `--disable apps`。不能改 `CODEX_HOME`——登录（`auth.json`）和模型服务商
//!    配置都在那里。

use super::super::config::McpServerSpec;

/// 要关掉的本机能力（按 `codex features list` 的实际名字写；启动时按可用清单过滤）。
///
/// 分组和理由：
/// - `shell_tool` / `unified_exec*` / `shell_snapshot` / `write_stdin_approval`：
///   本机命令执行与交互（codex 自带的 shell 工具一族）；
/// - `browser_use*` / `browser_annotation_api` / `in_app_browser`：浏览器控制；
/// - `computer_use` / `in_app_local_automation`：桌面控制 / 本机自动化；
/// - `apps` / `plugins`：ChatGPT 应用与插件（插件会自己挂 MCP 服务，见模块注释）；
/// - `image_generation` / `view_image`：生成图片、读本机图片；
/// - `multi_agent`：派生本机子智能体；
/// - `hooks`：生命周期钩子（会跑本机命令）；
/// - `worktrees` / `workspace_dependencies`：本机 git 工作树 / 依赖安装；
/// - `skill_mcp_dependency_install`：技能自动安装 MCP 依赖（本机写操作）。
pub const DENIED_FEATURES: &[&str] = &[
    "plugins",
    "apps",
    "shell_tool",
    "unified_exec",
    "unified_exec_tty",
    "unified_exec_zsh_fork",
    "shell_snapshot",
    "write_stdin_approval",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "browser_annotation_api",
    "in_app_browser",
    "in_app_local_automation",
    "computer_use",
    "image_generation",
    "view_image",
    "multi_agent",
    "hooks",
    "worktrees",
    "workspace_dependencies",
    "skill_mcp_dependency_install",
];

/// 要关掉的本机能力清单（`DENIED_FEATURES` 的访问器，方便单测和报告引用）。
pub fn denied_features() -> &'static [&'static str] {
    DENIED_FEATURES
}

/// 核心本机能力（AI-3 验收 0.1）：探测出来的可用特性里**至少要命中一个**，
/// 一个都没有说明 codex 的特性体系变了，我们的禁用清单不再可信，**拒绝启动**。
///
/// 为什么是这几个：它们是"模型能在本机干活"的主入口——
/// - `shell_tool` / `unified_exec`：本机命令执行；
/// - `computer_use`：桌面操控；
/// - `browser_use`：浏览器操控；
/// - `apps` / `plugins`：应用与插件（插件会自己挂本机 MCP 服务）。
///
/// 任何一代 codex 只要还允许模型动本机，至少会有其中一族；一族都没有就意味着
/// 特性名整体改名了（`--disable` 传过去要么被当成未知特性报错、要么静默失效），
/// 这种时候宁可不让 codex 起来。
pub const CORE_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "computer_use",
    "browser_use",
    "apps",
    "plugins",
];

/// 核心本机能力清单（`CORE_FEATURES` 的访问器）。
pub fn core_features() -> &'static [&'static str] {
    CORE_FEATURES
}

/// 生成 `-c mcp_servers` 值的 TOML（只含 `zssh` 一个服务）。
///
/// `default_tools_approval_mode = "approve"`：`zssh` 的工具调用让 codex 直接执行，
/// 确认由 Z-SSH 工具桥负责（手动 / 自动模式），不在 codex 这层重复弹窗（Netcatty
/// 记过：不给这个设置会把 MCP 调用转成 elicitation 请求，答不好就 "user rejected"）。
pub fn zssh_mcp_toml(spec: &McpServerSpec) -> String {
    let (command, args) = spec
        .command
        .split_first()
        .map(|(first, rest)| (first.as_str(), rest))
        .unwrap_or(("", &[]));
    let args_toml = args
        .iter()
        .map(|arg| toml_string(arg))
        .collect::<Vec<_>>()
        .join(",");
    let env_toml = spec
        .environment
        .iter()
        .map(|(key, value)| format!("{}={}", toml_key(key), toml_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "mcp_servers.{}={{command={},args=[{}],env={{{}}},default_tools_approval_mode='approve'}}",
        toml_key(&spec.name),
        toml_string(command),
        args_toml,
        env_toml
    )
}

/// TOML 键：裸键只允许 `A-Za-z0-9_-`，其余（少见）加引号。
fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_string()
    } else {
        toml_string(key)
    }
}

/// TOML 字符串：优先单引号字面串（Windows 路径不用转义）；值里有单引号时退回
/// 转义的基本串。
fn toml_string(value: &str) -> String {
    if !value.contains('\'') {
        return format!("'{value}'");
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
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

    #[test]
    fn zssh_mcp_toml_uses_literal_strings_for_windows_paths() {
        let toml = zssh_mcp_toml(&spec());
        assert!(toml.starts_with("mcp_servers.zssh={command='C:\\data\\tools\\zssh-mcp.exe'"));
        assert!(toml.contains("args=['--flag']"));
        assert!(toml.contains("ZSSH_BRIDGE_PORT='51234'"));
        assert!(toml.contains("ZSSH_BRIDGE_TOKEN='abc'"));
        assert!(toml.contains("default_tools_approval_mode='approve'"));
        assert!(!toml.contains("\\\\"), "字面串里不该有转义：{toml}");
    }

    #[test]
    fn values_with_single_quotes_fall_back_to_an_escaped_string() {
        assert_eq!(toml_string("it's"), "\"it's\"");
        assert_eq!(toml_string("a\\b"), "'a\\b'");
    }

    #[test]
    fn denied_features_do_not_include_the_tool_execution_host() {
        // 实测：禁掉 code_mode_host 会让 MCP 工具全部失败（fail closed）。
        assert!(!DENIED_FEATURES.contains(&"code_mode_host"));
        assert!(DENIED_FEATURES.contains(&"shell_tool"));
        assert!(DENIED_FEATURES.contains(&"plugins"));
        assert!(DENIED_FEATURES.contains(&"apps"));
    }
}
