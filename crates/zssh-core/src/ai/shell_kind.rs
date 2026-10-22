// Adapted from Netcatty electron/bridges/ai/sessionShellKind.cjs and
// lib/localShellRules.json (remote login-shell probe, probe output parsing and
// the shell-name rules), rewritten in Rust for Z-SSH.
//! 远端 shell 类型探测（AI-2d 1.3）。
//!
//! 在终端里执行 AI 命令要靠 POSIX 包装脚本；把 bash 的包装脚本敲进 fish /
//! PowerShell / cmd 里会一直等不到结束标记，直到超时（Netcatty issue #1854 /
//! #2959）。所以每个会话在**第一次**需要在终端里执行之前，用后台 exec 通道
//! 静默探测一次远端登录 shell，结果缓存在
//! [`TerminalState`](crate::ai::pty::TerminalState) 里。
//!
//! 参考实现里还有 Windows OpenSSH 的 `reg query DefaultShell` 分支和
//! PowerShell / cmd 的包装脚本；Z-SSH 只在 POSIX shell 上就地执行，Windows
//! 那一支的结果同样是"走后台"，所以这里只保留 POSIX 探针：认不出（包括
//! Windows、乱码、探测失败）一律 `None` → `Other` → 后台通道。

use std::time::Duration;

/// 探针输出里登录 shell 路径那一行的标记前缀。
pub const SHELL_PROBE_MARKER: &str = "__ZSSH_SHELL_KIND__:";
/// 探测超时（和 Netcatty 的 `DEFAULT_PROBE_TIMEOUT_MS` 一致）。
pub const SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 远端 shell 的类型（只关心能不能跑 POSIX 包装脚本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// 还没探测过：不知道远端 shell 是什么，不能就地执行。
    Unknown,
    /// 可识别的 POSIX shell（bash / zsh / sh / dash / ash / ksh）：能在终端里执行。
    Posix,
    /// fish：包装脚本不兼容，走后台。
    Fish,
    /// Windows 的 cmd / PowerShell，或者探测不出 / 认不出的 shell：走后台。
    Other,
}

/// 从 shell 可执行文件名（`bash`、`/bin/zsh`、`powershell.exe`…）判断类型。
///
/// 规则对齐 Netcatty 的 `localShellRules.json`；认不出来返回 `None`。
pub fn classify_shell_name(name: &str) -> Option<ShellKind> {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim()
        .to_ascii_lowercase();
    if base.is_empty() {
        return None;
    }
    let stem = base.strip_suffix(".exe").unwrap_or(&base);
    match stem {
        "sh" | "bash" | "dash" | "ash" | "zsh" | "ksh" | "mksh" => Some(ShellKind::Posix),
        "fish" => Some(ShellKind::Fish),
        "powershell" | "pwsh" | "cmd" => Some(ShellKind::Other),
        _ => None,
    }
}

/// 探测命令：强制 POSIX `sh` 执行（登录 shell 是 fish / zsh 也能跑），打印登录
/// shell 的路径。用 `getent` 拿 passwd 里的登录 shell，拿不到退到 `$SHELL`。
pub fn build_shell_probe_command() -> String {
    format!(
        "exec sh -c 'SH=\"$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7)\"; \
         [ -n \"$SH\" ] || SH=\"${{SHELL:-}}\"; printf \"{SHELL_PROBE_MARKER}%s\\n\" \"$SH\"'"
    )
}

/// 解析探测命令的输出；认不出来（空输出、乱码、路径都不认识）返回 `None`。
pub fn parse_shell_probe_output(stdout: &str) -> Option<ShellKind> {
    for line in stdout.replace('\r', "").lines() {
        let Some(path) = line.trim().strip_prefix(SHELL_PROBE_MARKER) else {
            continue;
        };
        if let Some(kind) = classify_shell_name(path) {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_shell_names() {
        assert_eq!(classify_shell_name("bash"), Some(ShellKind::Posix));
        assert_eq!(classify_shell_name("/bin/zsh"), Some(ShellKind::Posix));
        assert_eq!(classify_shell_name("-bash"), Some(ShellKind::Posix));
        assert_eq!(classify_shell_name("/bin/sh"), Some(ShellKind::Posix));
        assert_eq!(
            classify_shell_name("/bin/busybox"),
            None,
            "busybox 本身不是 shell"
        );
        assert_eq!(classify_shell_name("/bin/ash"), Some(ShellKind::Posix));
        assert_eq!(
            classify_shell_name("/usr/local/bin/fish"),
            Some(ShellKind::Fish)
        );
        assert_eq!(classify_shell_name("fish"), Some(ShellKind::Fish));
        assert_eq!(
            classify_shell_name("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            Some(ShellKind::Other)
        );
        assert_eq!(classify_shell_name("pwsh.exe"), Some(ShellKind::Other));
        assert_eq!(
            classify_shell_name("C:\\Windows\\System32\\cmd.exe"),
            Some(ShellKind::Other)
        );
        assert_eq!(classify_shell_name("/usr/bin/nu"), None, "认不出的 shell");
        assert_eq!(classify_shell_name(""), None);
        assert_eq!(classify_shell_name("   "), None);
    }

    #[test]
    fn probe_command_is_posix_quoted_and_forces_sh() {
        let command = build_shell_probe_command();
        assert!(
            command.starts_with("exec sh -c '"),
            "要强制 POSIX sh：{command}"
        );
        assert!(command.ends_with('\''), "整段脚本要单引号包住：{command}");
        assert!(command.contains("getent passwd"));
        assert!(command.contains(SHELL_PROBE_MARKER));
        // 外层 shell（可能是 fish）不能看到未加引号的 ${SHELL:-}。
        assert!(command.contains("${SHELL:-}"));
    }

    #[test]
    fn parses_probe_output() {
        let line = |path: &str| format!("{SHELL_PROBE_MARKER}{path}");
        assert_eq!(
            parse_shell_probe_output(&format!("\n{}\n", line("/bin/bash"))),
            Some(ShellKind::Posix)
        );
        assert_eq!(
            parse_shell_probe_output(&format!("  {}\r\n", line("/usr/bin/zsh"))),
            Some(ShellKind::Posix),
            "允许行首空白 + CRLF"
        );
        assert_eq!(
            parse_shell_probe_output(&format!("SHELL=/bin/bash\n{}\n", line("/bin/sh"))),
            Some(ShellKind::Posix),
            "噪声行不影响"
        );
        assert_eq!(
            parse_shell_probe_output(&format!("{}\n", line("/bin/ash"))),
            Some(ShellKind::Posix),
            "BusyBox ash"
        );
        assert_eq!(
            parse_shell_probe_output(&format!("{}\n", line("/usr/local/bin/fish"))),
            Some(ShellKind::Fish)
        );
        assert_eq!(
            parse_shell_probe_output(&format!("{}\n", line("C:\\Windows\\System32\\cmd.exe"))),
            Some(ShellKind::Other),
            "Windows 的默认 shell 认得出但只走后台"
        );
        assert_eq!(
            parse_shell_probe_output(&format!("{}\n", line("powershell.exe"))),
            Some(ShellKind::Other)
        );
        // 乱码 / 没有标记行 / 空输出：什么都认不出。
        assert_eq!(
            parse_shell_probe_output("\u{0}\u{fffd}garbage binary"),
            None
        );
        assert_eq!(parse_shell_probe_output("SHELL=/bin/bash\n"), None);
        assert_eq!(parse_shell_probe_output("   \n\n"), None);
        assert_eq!(parse_shell_probe_output(""), None);
        // 标记行但没有路径（getent 和 $SHELL 都空）。
        assert_eq!(
            parse_shell_probe_output(&format!("{SHELL_PROBE_MARKER}\n")),
            None
        );
        // 认不出的路径。
        assert_eq!(
            parse_shell_probe_output(&format!("{}\n", line("/usr/bin/nu"))),
            None
        );
    }
}
