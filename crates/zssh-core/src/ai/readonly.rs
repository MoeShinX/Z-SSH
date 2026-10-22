//! 只读命令判断（三档批准里的"只读自动"一档用）。
//!
//! 和 [`super::safety`] 反过来：那边是黑名单（认出危险的），这里是**白名单**——只有认得出、
//! 而且确定不改动系统的命令才算只读，其余一律要人确认。拿不准的都按"不是只读"。
//!
//! 规则：
//! - 有命令替换（`$(…)`、反引号、`<(…)`）、写文件的重定向（除了 `2>&1`、`>/dev/null` 这几种）、
//!   变量赋值前缀、`sudo` 一类的包装，都不算只读；
//! - 用 `;`、`&&`、`||`、`|` 连起来的每一段都要是只读的。

use super::safety::{split_segments, tokenize};

/// 无论带什么参数都只读的命令。
const ALWAYS: &[&str] = &[
    "ls",
    "ll",
    "dir",
    "cat",
    "tac",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "zgrep",
    "zcat",
    "rg",
    "wc",
    "cut",
    "tr",
    "column",
    "nl",
    "stat",
    "file",
    "du",
    "df",
    "free",
    "uptime",
    "w",
    "who",
    "whoami",
    "id",
    "groups",
    "uname",
    "pwd",
    "echo",
    "printf",
    "printenv",
    "which",
    "whereis",
    "type",
    "ps",
    "pgrep",
    "pstree",
    "top",
    "lsof",
    "ss",
    "netstat",
    "ping",
    "dig",
    "nslookup",
    "host",
    "traceroute",
    "tracepath",
    "mtr",
    "lsblk",
    "blkid",
    "findmnt",
    "lscpu",
    "lsmem",
    "lspci",
    "lsusb",
    "lsmod",
    "nproc",
    "arch",
    "locale",
    "tree",
    "realpath",
    "readlink",
    "basename",
    "dirname",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "cksum",
    "base64",
    "hexdump",
    "od",
    "strings",
    "diff",
    "cmp",
    "last",
    "lastlog",
    "getent",
    "history",
    "cd",
    "true",
    "false",
    "test",
    "[",
    "sleep",
    "vmstat",
    "iostat",
    "mpstat",
    "pidstat",
    "iotop",
    "uniq",
    "jq",
    "sort",
];

pub fn is_read_only(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || command.contains('`') || command.contains("$(") {
        return false;
    }
    if command.contains("<(") || command.contains(">(") {
        return false;
    }
    if has_write_redirect(command) {
        return false;
    }
    let segments = split_segments(command);
    !segments.is_empty() && segments.iter().all(|segment| segment_is_read_only(segment))
}

/// 去掉无害的重定向（`2>&1`、`>&2`、`>/dev/null`、`2>/dev/null`、`&>/dev/null`）之后，
/// 还有 `>` 就是在写文件。按字符看，引号里的 `>`（比如 grep 的模式）也算——宁可多问一句。
fn has_write_redirect(command: &str) -> bool {
    let mut text = command.to_owned();
    for harmless in [
        "&>/dev/null",
        "&> /dev/null",
        "2>/dev/null",
        "2> /dev/null",
        "1>/dev/null",
        ">/dev/null",
        "> /dev/null",
        "2>&1",
        "1>&2",
        ">&2",
    ] {
        text = text.replace(harmless, " ");
    }
    text.contains('>')
}

/// 去掉 `2>&1`、`2>/dev/null`、`> /dev/null` 这类不写文件的重定向，免得被当成命令参数。
/// 别的重定向在 [`has_write_redirect`] 里已经拦掉了。
fn without_harmless_redirects(tokens: Vec<String>) -> Vec<String> {
    let is_redirect_op = |t: &str| matches!(t, ">" | "1>" | "2>" | "&>");
    let mut out = Vec::with_capacity(tokens.len());
    let mut iter = tokens.into_iter().peekable();
    while let Some(token) = iter.next() {
        let dup = token.len() == 4 && token.as_bytes()[1..3] == *b">&";
        if dup || token == ">&2" || token.ends_with(">/dev/null") {
            continue;
        }
        if is_redirect_op(&token) && iter.peek().is_some_and(|next| next == "/dev/null") {
            iter.next();
            continue;
        }
        out.push(token);
    }
    out
}

/// 系统程序的标准目录。带路径的程序名只认这些目录下的，`/tmp/ls`、`./cat` 不算
/// （用户可写目录里的同名程序能做任何事）。
const STANDARD_BIN_DIRS: &[&str] = &[
    "/bin/",
    "/usr/bin/",
    "/sbin/",
    "/usr/sbin/",
    "/usr/local/bin/",
    "/usr/local/sbin/",
];

/// 命令名：不带路径的原样返回；带路径的只认标准目录下的，否则 `None`（不算只读）。
fn program_name(head: &str) -> Option<&str> {
    if !head.contains('/') {
        return Some(head);
    }
    let dir_ok = STANDARD_BIN_DIRS.iter().any(|dir| {
        head.strip_prefix(dir)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
    });
    dir_ok.then(|| head.rsplit('/').next().unwrap_or(head))
}

fn any_arg(args: &[String], bad: impl Fn(&str) -> bool) -> bool {
    args.iter().any(|a| bad(a.as_str()))
}

/// 选项是不是 `long`（含 `--long=值` 写法）。
fn is_long(arg: &str, long: &str) -> bool {
    arg == long
        || arg
            .strip_prefix(long)
            .is_some_and(|rest| rest.starts_with('='))
}

/// 白名单里的命令再按参数筛一遍：有些选项能执行别的程序、写文件、改系统状态
/// （`rg --pre`、`sort --compress-program`、`tree -o`、`file -C`、`ss -K` 之类）。
/// 只列我们知道的危险选项；命令本身不在白名单的照常要确认。
fn always_args_ok(name: &str, args: &[String]) -> bool {
    match name {
        // --pre / --pre-glob 对每个文件跑一个外部程序；-z 会调用解压程序
        "rg" => !any_arg(args, |a| {
            is_long(a, "--pre")
                || is_long(a, "--pre-glob")
                || is_long(a, "--search-zip")
                || short_cluster_has(a, 'z')
        }),
        // -o / --output 写文件（-no 这类合并写法也算）；--compress-program 执行别的程序
        "sort" => !any_arg(args, |a| {
            short_cluster_contains(a, 'o')
                || is_long(a, "--output")
                || is_long(a, "--compress-program")
        }),
        // 第二个文件参数是输出文件
        "uniq" => args.iter().filter(|a| !a.starts_with('-')).count() <= 1,
        // -o 把结果写进文件
        "tree" => !any_arg(args, |a| short_cluster_contains(a, 'o')),
        // -C 编译 magic 文件（写文件）
        "file" => !any_arg(args, |a| a == "-C" || is_long(a, "--compile")),
        // -K 杀掉匹配的连接；-D 把原始数据写进文件
        "ss" => !any_arg(args, |a| {
            is_long(a, "--kill")
                || is_long(a, "--diag")
                || short_cluster_has(a, 'K')
                || short_cluster_has(a, 'D')
        }),
        // -g 回收缓存（写 /etc/blkid.tab）
        "blkid" => !any_arg(args, |a| a == "-g" || is_long(a, "--garbage-collect")),
        // history -c 清空、-w 写文件、-d 删除……只认不带参数的
        "history" => args.is_empty(),
        _ => true,
    }
}

/// 短选项簇里有没有某个字母（`-nz`、`-z`）。长选项（`--x`）和非选项不算。
/// 比 `short_cluster_has` 更保守：短选项串里任何位置出现这个字母都算（包括选项参数里的）。
fn short_cluster_contains(arg: &str, letter: char) -> bool {
    arg.strip_prefix('-')
        .is_some_and(|rest| !rest.starts_with('-') && rest.contains(letter))
}

fn short_cluster_has(arg: &str, letter: char) -> bool {
    arg.strip_prefix('-').is_some_and(|rest| {
        !rest.starts_with('-')
            && rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .any(|c| c == letter)
    })
}

fn segment_is_read_only(segment: &str) -> bool {
    let Ok(tokens) = tokenize(segment) else {
        return false;
    };
    let tokens = without_harmless_redirects(tokens);
    let Some(head) = tokens.first() else {
        return true;
    };
    // `FOO=bar cmd` 能改命令的行为（LD_PRELOAD 之类），不算只读
    if head.contains('=') {
        return false;
    }
    let Some(name) = program_name(head) else {
        return false;
    };
    let args = &tokens[1..];
    if ALWAYS.contains(&name) {
        return always_args_ok(name, args);
    }
    match name {
        "find" => !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-delete"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        // 带参数的 hostname / date 是在改主机名、改时间
        "hostname" => args.iter().all(|a| a.starts_with('-')),
        "date" => args
            .iter()
            .all(|a| a.starts_with('+') || (a.starts_with('-') && !is_flag(a, "s", "--set"))),
        "env" => args.is_empty(),
        "mount" => args.is_empty() || args.iter().all(|a| a == "-l"),
        "dmesg" => !args.iter().any(|a| {
            a == "-c"
                || a == "-C"
                || a == "-n"
                || a.starts_with("--clear")
                || a.starts_with("--console")
        }),
        "journalctl" => !args.iter().any(|a| {
            a.starts_with("--vacuum")
                || a.starts_with("--rotate")
                || a.starts_with("--flush")
                || a.starts_with("--sync")
                || a.starts_with("--relinquish")
                || a.starts_with("--setup-keys")
        }),
        "sed" => sed_is_read_only(args),
        "ip" => !args.iter().any(|a| {
            // -b / -batch 从文件读一批命令执行（可能是 add / del）
            a == "-b"
                || a == "-batch"
                || a == "-force"
                || matches!(
                    a.as_str(),
                    "add"
                        | "del"
                        | "delete"
                        | "set"
                        | "flush"
                        | "change"
                        | "replace"
                        | "append"
                        | "exec"
                        | "monitor"
                        | "save"
                        | "restore"
                )
        }),
        "systemctl" => first_word(args).is_some_and(|sub| {
            matches!(
                sub,
                "status"
                    | "show"
                    | "cat"
                    | "list-units"
                    | "list-unit-files"
                    | "list-timers"
                    | "list-sockets"
                    | "list-dependencies"
                    | "list-jobs"
                    | "is-active"
                    | "is-enabled"
                    | "is-failed"
                    | "is-system-running"
                    | "get-default"
                    | "show-environment"
            )
        }),
        "service" => {
            (args.len() == 2 && args[1] == "status")
                || (args.len() == 1 && args[0] == "--status-all")
        }
        "timedatectl" | "hostnamectl" | "loginctl" | "localectl" | "networkctl" | "resolvectl" => {
            first_word(args).is_none_or(|sub| {
                matches!(
                    sub,
                    "status"
                        | "show"
                        | "list"
                        | "list-sessions"
                        | "list-users"
                        | "query"
                        | "statistics"
                        | "list-timezones"
                )
            })
        }
        "docker" | "podman" => docker_is_read_only(args),
        "kubectl" => first_word(args).is_some_and(|sub| {
            matches!(
                sub,
                "get"
                    | "describe"
                    | "logs"
                    | "top"
                    | "version"
                    | "cluster-info"
                    | "api-resources"
                    | "api-versions"
                    | "explain"
            )
        }),
        // 不带参数的 nginx 会启动服务
        "nginx" => {
            !args.is_empty()
                && args
                    .iter()
                    .all(|a| a == "-t" || a == "-T" || a == "-v" || a == "-V")
        }
        "apt" | "apt-cache" => first_word(args).is_some_and(|sub| {
            matches!(
                sub,
                "list" | "show" | "search" | "policy" | "depends" | "rdepends" | "madison"
            )
        }),
        "dpkg" => {
            args.iter().all(|a| {
                matches!(
                    a.as_str(),
                    "-l" | "-L" | "-s" | "-S" | "--list" | "--status" | "--listfiles" | "--search"
                ) || !a.starts_with('-')
            }) && args.first().is_some_and(|a| a.starts_with('-'))
        }
        // --pipe 把输出交给一个外部命令执行
        "rpm" => {
            args.first().is_some_and(|a| a.starts_with("-q"))
                && !any_arg(args, |a| is_long(a, "--pipe"))
        }
        _ => false,
    }
}

fn is_flag(arg: &str, short: &str, long: &str) -> bool {
    arg == format!("-{short}") || arg.starts_with(long)
}

/// 第一个不是选项的参数（子命令）。
fn first_word(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-'))
}

/// `sed -n '10,20p' file`、`sed -n '/re/p' file`：只认这种只打印的写法。
fn sed_is_read_only(args: &[String]) -> bool {
    let mut script_seen = false;
    for arg in args {
        if arg.starts_with('-') {
            if !matches!(arg.as_str(), "-n" | "-E" | "-r" | "--quiet" | "--silent") {
                return false;
            }
            continue;
        }
        if script_seen {
            continue;
        }
        script_seen = true;
        let ok = arg
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '$' | 'p' | ' '))
            && arg.ends_with('p')
            || (arg.starts_with('/')
                && arg.ends_with("/p")
                && !arg[1..arg.len() - 2].contains('/'));
        if !ok {
            return false;
        }
    }
    script_seen
}

fn docker_is_read_only(args: &[String]) -> bool {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    match words.as_slice() {
        ["compose", sub, ..] => matches!(
            *sub,
            "ps" | "logs" | "config" | "ls" | "images" | "top" | "version"
        ),
        [sub, ..] => {
            matches!(
                *sub,
                "ps" | "images"
                    | "logs"
                    | "inspect"
                    | "version"
                    | "info"
                    | "top"
                    | "port"
                    | "diff"
                    | "history"
                    | "events"
            ) || (*sub == "stats" && args.iter().any(|a| a == "--no-stream"))
                || (matches!(*sub, "container" | "image" | "network" | "volume")
                    && words
                        .get(1)
                        .is_some_and(|s| matches!(*s, "ls" | "list" | "inspect")))
        }
        [] => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_read_only;

    #[test]
    fn everyday_read_commands_pass() {
        for command in [
            "ls -la /var/log",
            "cat /etc/os-release",
            "tail -n 200 /var/log/nginx/error.log",
            "systemctl status nginx",
            "journalctl -u nginx --since today --no-pager",
            "ps aux | grep nginx | grep -v grep",
            "/usr/bin/ls -la /",
            "rg -n listen /etc/nginx",
            "ss -tlnp",
            "df -h && free -m && uptime",
            "ss -tlnp",
            "docker ps -a",
            "docker logs --tail 100 web",
            "docker compose ps",
            "kubectl get pods -A",
            "find /etc -name '*.conf' -mtime -1",
            "sed -n '10,20p' /etc/nginx/nginx.conf",
            "grep -rn listen /etc/nginx 2>/dev/null",
            "nginx -t 2>&1",
            "ip addr show",
            "date +%F",
            "sort -u names.txt",
            "apt list --installed",
            "dpkg -l",
            "rpm -qa",
        ] {
            assert!(is_read_only(command), "应该算只读：{command}");
        }
    }

    #[test]
    fn anything_that_changes_the_system_needs_approval() {
        for command in [
            "rm -f /tmp/x",
            "mkdir /srv/new",
            "systemctl restart nginx",
            "apt install -y htop",
            "echo hi > /etc/motd",
            "cat a >> b",
            "ls | tee out.txt",
            "find / -name core -delete",
            "find . -exec rm {} \\;",
            "sed -i 's/a/b/' file",
            "sed 's/a/b/w out' file",
            "sudo cat /etc/shadow",
            "LD_PRELOAD=/x.so ls",
            "ls $(cat list)",
            "ls `pwd`",
            "diff <(ls a) <(ls b)",
            "date -s '2020-01-01'",
            "hostname newname",
            "env bash -c 'rm -rf /'",
            "ip link set eth0 down",
            "ip route add default via 10.0.0.1",
            "docker rm -f web",
            "docker stats",
            "kubectl delete pod x",
            "git checkout main",
            "dmesg -c",
            "journalctl --vacuum-time=1d",
            "sort -o out.txt in.txt",
            "sort -no /tmp/x /tmp/y",
            "sort -rno out in",
            "tree -ao out.txt",
            "nginx",
            "sudo nginx",
            "uniq in.txt out.txt",
            "ls; reboot",
            "ps aux | xargs kill",
            "awk '{print}' f",
            "curl -o /tmp/x http://example.invalid",
            // 复审 F1：白名单命令带了能执行程序 / 写文件的参数，或者路径不是系统目录
            "rg --pre /bin/sh x /tmp/zssh-audit.sh",
            "rg -nz secret /var/log",
            "xxd -r -p /tmp/a.hex /tmp/a.out",
            "sort --compress-program=/tmp/x big.txt",
            "tree -o /tmp/out.txt /etc",
            "file -C -m /tmp/magic",
            "ss -K dst 10.0.0.1",
            "history -c",
            "less '+!id' /etc/hosts",
            "/tmp/ls -la",
            "./cat x",
            "/usr/bin/../../tmp/ls",
            "git status",
            "ip -batch /tmp/cmds",
            "rpm -qa --pipe 'sh'",
            "yq -i '.a = 1' x.yml",
            "",
        ] {
            assert!(!is_read_only(command), "不该算只读：{command}");
        }
    }
}
