// Adapted from Netcatty electron/bridges/sshBridge/sessionOps.cjs（getSessionPwd，POSIX 脚本和"取前台 shell"的思路）
//! 取终端当前所在目录（M3-3 第 3 节），用来让 SFTP 面板的远端栏跟随终端。
//!
//! 不往用户的终端里敲命令、也不留下任何痕迹：另开一条 **exec 通道**（不带 PTY），
//! 在服务器上跑一段 POSIX sh 脚本，读 `/proc/<pid>/cwd`。
//!
//! 思路参考 Netcatty `electron/bridges/sshBridge/sessionOps.cjs` 的 `getSessionPwd`：
//! 同一条 SSH 连接上，PTY 的 shell 和 exec 通道的进程是同一个 sshd 进程的子进程（兄弟），
//! 所以按 `$PPID` 找带 tty 的 shell；再沿进程树取最深的前台 shell（`su` / `sudo -s` /
//! 嵌套 shell 之后用户实际在用的那个）；最后 `readlink /proc/<pid>/cwd`。
//!
//! 有效范围（写进报告）：Linux（要 `/proc` 和 `ps`），OpenSSH 的"每个通道一个子进程"
//! 模型；bash / zsh / fish / ksh / dash 等 shell。**无效**时（Windows 服务器、没有
//! `/proc`、非 OpenSSH、同一连接上有多个终端而对不上号）返回 `None`，调用方安静地退回
//! 家目录，不报错。
//!
//! 同一条连接上有多个终端（本程序的分屏会共用一条池连接）时：脚本列出**所有**带 tty 的
//! shell，按 pid 排序；本程序也知道这条连接上有几个终端、目标是第几个打开的。两个数量
//! 对得上才按顺序取，对不上就不猜（返回 `None`）。

use std::time::Duration;

use super::connection::Connection;

/// 探测超时：服务器不回就放弃，不卡住面板。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 一个 shell 的所在目录（`login` 是带 tty 的登录 shell 的 pid，用来排序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCwd {
    pub login_pid: u32,
    pub cwd: String,
}

/// 服务器上跑的脚本。输出：每个带 tty 的 shell 一行 `登录 shell 的 pid<TAB>当前目录`。
///
/// 为什么是脚本里的 awk 而不是多条命令：一次往返就拿全，不在服务器上留下一串进程。
const PROBE_SCRIPT: &str = r#"PP=${ZSSH_SSHD_PPID:-$PPID}
SELF=$$
ps -e -o pid=,ppid=,tty=,stat=,comm= 2>/dev/null | awk -v pp="$PP" -v self="$SELF" '
function isshell(c) { sub(/^.*\//, "", c); sub(/^-/, "", c); return c ~ /^(ba|z|fi|k|da|a|c|tc)?sh$/ }
function depth(q, start,   d) { d = 0; while (q != "" && d < 64) { if (q == start) return d; q = ppid[q]; d++ } return -1 }
{ ppid[$1] = $2; tty[$1] = $3; st[$1] = $4; cm[$1] = $5; ord[NR] = $1 }
END {
  for (i = 1; i <= NR; i++) {
    p = ord[i]
    if (p == self || ppid[p] != pp || !isshell(cm[p]) || tty[p] ~ /^[?-]+$/) continue
    best = p; bd = -1
    for (j = 1; j <= NR; j++) {
      q = ord[j]
      if (!isshell(cm[q]) || index(st[q], "+") == 0) continue
      d = depth(q, p)
      if (d > bd) { bd = d; best = q }
    }
    print p, best
  }
}' | while read -r login pid; do
  cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null)
  [ -n "$cwd" ] || cwd=$(readlink "/proc/$login/cwd" 2>/dev/null)
  [ -n "$cwd" ] && printf '%s\t%s\n' "$login" "$cwd"
done
exit 0"#;

/// 交给 exec 通道的命令行。
///
/// 外层 `exec sh -c`：把用户的登录 shell（fish / csh 之类）换成 POSIX sh，这样脚本的
/// `$PPID` 就是 sshd，也不用担心登录 shell 自己解析不了 POSIX 语法。
pub fn probe_command() -> String {
    format!("exec sh -c {}", shell_quote(PROBE_SCRIPT))
}

/// POSIX 单引号转义：`'` → `'\''`。
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// 解析脚本输出，按登录 shell 的 pid 升序（≈ 打开顺序）。
pub fn parse_probe_output(output: &str) -> Vec<ShellCwd> {
    let mut found: Vec<ShellCwd> = output
        .lines()
        .filter_map(|line| {
            let (pid, cwd) = line.split_once('\t')?;
            let login_pid = pid.trim().parse::<u32>().ok()?;
            let cwd = cwd.trim_end_matches('\r');
            // 只认绝对路径：别的形状（报错信息混进来了）一律丢掉。
            if !cwd.starts_with('/') {
                return None;
            }
            Some(ShellCwd {
                login_pid,
                cwd: cwd.to_owned(),
            })
        })
        .collect();
    found.sort_by_key(|item| item.login_pid);
    found.dedup_by_key(|item| item.login_pid);
    found
}

/// 在一堆 shell 里挑出目标终端那个。
///
/// `ordinal`：目标是这条连接上第几个打开的终端（从 0 数）；`live`：这条连接上现在有几个终端。
/// 数量对不上说明有我们不知道的 shell（或者有的已经关了还没清）——不猜，返回 `None`。
pub fn pick_shell(candidates: &[ShellCwd], ordinal: usize, live: usize) -> Option<&ShellCwd> {
    if live == 0 || candidates.len() != live {
        return None;
    }
    candidates.get(ordinal)
}

/// 在一条连接上探测所有终端 shell 的目录。
///
/// 失败（没有 `/proc`、`ps` 不存在、超时、通道打不开）一律返回空列表——"取不到"
/// 不是错误。
pub async fn probe_shell_cwds(connection: &Connection) -> Vec<ShellCwd> {
    let command = probe_command();
    match tokio::time::timeout(PROBE_TIMEOUT, connection.exec(&command)).await {
        Ok(Ok(output)) => parse_probe_output(&output),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_one_exec_sh_with_balanced_quotes() {
        let command = probe_command();
        assert!(command.starts_with("exec sh -c '"));
        // 脚本里的单引号全被转义：去掉 `'\''` 之后外层只剩一对引号。
        let stripped = command.replace(r"'\''", "");
        assert_eq!(stripped.matches('\'').count(), 2, "{command}");
    }

    #[test]
    fn command_never_writes_to_a_terminal() {
        // 不带 PTY 的 exec 通道里跑，脚本里也不碰 /dev/tty、不用 stty、不发转义序列。
        for forbidden in ["/dev/tty", "stty", "\x1b"] {
            assert!(!PROBE_SCRIPT.contains(forbidden), "不该出现 {forbidden}");
        }
    }

    #[test]
    fn parse_keeps_absolute_paths_with_spaces_and_sorts_by_pid() {
        let output = "300\t/home/u/my dir\n120\t/tmp\n";
        assert_eq!(
            parse_probe_output(output),
            vec![
                ShellCwd {
                    login_pid: 120,
                    cwd: "/tmp".into()
                },
                ShellCwd {
                    login_pid: 300,
                    cwd: "/home/u/my dir".into()
                },
            ]
        );
    }

    #[test]
    fn parse_drops_garbage_lines() {
        let output = "sh: ps: not found\n\n12\trelative/path\nabc\t/x\n7\t/ok\r\n";
        assert_eq!(
            parse_probe_output(output),
            vec![ShellCwd {
                login_pid: 7,
                cwd: "/ok".into()
            }]
        );
    }

    #[test]
    fn parse_empty_output_means_unavailable() {
        assert!(parse_probe_output("").is_empty());
    }

    #[test]
    fn pick_by_open_order_when_counts_match() {
        let shells = vec![
            ShellCwd {
                login_pid: 10,
                cwd: "/a".into(),
            },
            ShellCwd {
                login_pid: 20,
                cwd: "/b".into(),
            },
        ];
        assert_eq!(
            pick_shell(&shells, 0, 2).map(|s| s.cwd.as_str()),
            Some("/a")
        );
        assert_eq!(
            pick_shell(&shells, 1, 2).map(|s| s.cwd.as_str()),
            Some("/b")
        );
    }

    #[test]
    fn pick_refuses_to_guess_when_counts_differ() {
        let shells = vec![
            ShellCwd {
                login_pid: 10,
                cwd: "/a".into(),
            },
            ShellCwd {
                login_pid: 20,
                cwd: "/b".into(),
            },
        ];
        assert!(pick_shell(&shells, 0, 1).is_none());
        assert!(pick_shell(&shells, 0, 3).is_none());
        assert!(pick_shell(&shells, 0, 0).is_none());
        assert!(pick_shell(&[], 0, 1).is_none());
    }
}
