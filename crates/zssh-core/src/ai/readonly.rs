//! 只读命令判断（三档批准里的"只读自动"一档用）。
//!
//! 和 [`super::safety`] 反过来：那边是黑名单（认出危险的），这里是**白名单**——只有认得出、
//! 而且确定不改动系统的命令才算只读，其余一律要人确认。拿不准的都按"不是只读"。
//!
//! 规则：
//! - 有命令替换（`$(…)`、反引号、`<(…)`）、写文件的重定向（除了 `2>&1`、`>/dev/null` 这几种）、
//!   变量赋值前缀、`sudo` 一类的包装，都不算只读；
//! - 用 `;`、`&&`、`||`、`|` 连起来的每一段都要是只读的。
//! - 有参数筛选的命令（`sort`、`rg`、`date`、`ip`……）用**允许列表**：只放行列出来的选项，
//!   长选项必须写全，不认识的参数 / 缩写一律要确认（细节见下面的 `Spec`）。
//! - 允许列表检查的是展开前的文字，而 shell 执行前还会展开：这类命令的参数里只要有不受引号保护的
//!   `$ { } * ? [`，就不去"计算"展开结果，直接要确认（见 [`rule_for`]）；不看参数的命令不受影响。
//! - 命令是打进交互式终端的，bash 会先做历史展开（`!`），所以任何命令里可能触发历史展开的 `!`
//!   都要确认（见 [`has_unsafe_shell_syntax`]）。只按 bash 的规则，zsh / fish 的额外展开不在范围。

use super::safety::{split_segments, tokenize_marked, Word};

/// 白名单里的程序名。其中要看参数的（`rg`、`sort`、`printf`……）在 [`rule_for`] 里另有登记，
/// 那边优先；没登记的才是"带什么参数都只读"。
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
    if has_write_redirect(command) || has_unsafe_shell_syntax(command) {
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
        text = replace_at_word_end(&text, harmless);
    }
    text.contains('>')
}

/// 只替换"后面紧跟空白、命令分隔符或结尾"的匹配：`>/dev/null.x`、`>&2out` 是在写别的文件，
/// 不能当成无害的重定向。
fn replace_at_word_end(text: &str, pattern: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, matched) in text.match_indices(pattern) {
        let end = start + matched.len();
        let at_word_end = text[end..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '&' | '|' | ')'));
        if at_word_end {
            out.push_str(&text[last..start]);
            out.push(' ');
            last = end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// 去掉 `2>&1`、`2>/dev/null`、`> /dev/null` 这类不写文件的重定向，免得被当成命令参数。
/// 别的重定向在 [`has_write_redirect`] 里已经拦掉了。
fn without_harmless_redirects(tokens: Vec<Word>) -> Vec<Word> {
    let is_redirect_op = |t: &str| matches!(t, ">" | "1>" | "2>" | "&>");
    let mut out = Vec::with_capacity(tokens.len());
    let mut iter = tokens.into_iter().peekable();
    while let Some(token) = iter.next() {
        let text = token.text.as_str();
        let dup = text.len() == 4 && text.as_bytes()[1..3] == *b">&";
        if dup || text == ">&2" || text.ends_with(">/dev/null") {
            continue;
        }
        if is_redirect_op(text) && iter.peek().is_some_and(|next| next.text == "/dev/null") {
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

// ---------------------------------------------------------------------------
// 参数允许列表
//
// 原则：**不认识的参数一律要确认**。不再列"已知的危险选项"去拦（GNU 长选项可以缩写、
// 短选项可以合并或直接带值，黑名单永远补不完），而是给每个带参数筛选的命令写一份
// "这些选项我确定只读"的允许列表：
// - 长选项必须和列表里的名字**完全一致**（含 `--name=值`），缩写、没列出的一律不算；
// - 短选项合并写法（`-abc`）里每个字母都要在列表里；带值的短选项后面的字符 / 下一个参数是值；
// - 只有"必定要参数"的选项才声明成带值（会吃掉下一个参数）；拿不准的按"可选值"
//   （只认紧贴的写法），这样下一个参数仍会被当成选项或位置参数继续检查，不会被藏进"值"里。
// ---------------------------------------------------------------------------

/// 一个命令的选项允许列表。
struct Spec {
    /// 不带值的短选项字母。
    flags: &'static str,
    /// 必定要值的短选项字母（`-k2` 或 `-k 2`）。
    valued: &'static str,
    /// 值可有可无的短选项字母：只认紧贴写法（`-I` / `-Idate`），不吃下一个参数
    /// （下一个参数是纯数字时除外，如 `journalctl -n 20`）。
    optional: &'static str,
    /// 不带值的长选项（不含 `--`）。
    long_flags: &'static [&'static str],
    /// 必定要值的长选项（`--name=值` 或 `--name 值`）。
    long_valued: &'static [&'static str],
    /// 值可有可无的长选项：值只能用 `=` 紧贴。
    long_optional: &'static [&'static str],
}

fn is_plain_number(arg: &str) -> bool {
    let digits = arg.strip_prefix(['-', '+']).unwrap_or(arg);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// 按允许列表解析参数。所有选项都在列表里就返回位置参数（不含选项和选项的值），
/// 有任何一个不认识就返回 `None`（要确认）。`--` 之后全是位置参数；单独的 `-` 是位置参数。
fn parse_args<'a>(spec: &Spec, args: &'a [String]) -> Option<Vec<&'a str>> {
    let mut positional = Vec::new();
    let mut only_positional = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if only_positional || arg == "-" || !arg.starts_with('-') {
            positional.push(arg);
            continue;
        }
        if arg == "--" {
            only_positional = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            if spec.long_flags.contains(&name) {
                if value.is_some() {
                    return None;
                }
            } else if spec.long_valued.contains(&name) {
                if value.is_none() {
                    // 必定要值：下一个参数就是值，不管它长什么样
                    if i >= args.len() {
                        return None;
                    }
                    i += 1;
                }
            } else if !spec.long_optional.contains(&name) {
                return None;
            }
            continue;
        }
        let cluster = &arg[1..];
        for (index, c) in cluster.char_indices() {
            let rest = &cluster[index + c.len_utf8()..];
            if spec.flags.contains(c) {
                continue;
            }
            if spec.valued.contains(c) {
                if rest.is_empty() {
                    if i >= args.len() {
                        return None;
                    }
                    i += 1;
                }
                break;
            }
            if spec.optional.contains(c) {
                if rest.is_empty() && i < args.len() && is_plain_number(&args[i]) {
                    i += 1;
                }
                break;
            }
            return None;
        }
    }
    Some(positional)
}

const RG: Spec = Spec {
    flags: "abcFhiIlLnNopPqsSUuvwxHV0.",
    valued: "ABCdeEfgjMmrtT",
    optional: "",
    long_flags: &[
        "text",
        "byte-offset",
        "count",
        "count-matches",
        "fixed-strings",
        "follow",
        "glob-case-insensitive",
        "hidden",
        "ignore-case",
        "no-ignore",
        "no-ignore-dot",
        "no-ignore-exclude",
        "no-ignore-files",
        "no-ignore-global",
        "no-ignore-messages",
        "no-ignore-parent",
        "no-ignore-vcs",
        "no-messages",
        "no-require-git",
        "no-filename",
        "no-line-number",
        "no-heading",
        "heading",
        "files",
        "files-with-matches",
        "files-without-match",
        "invert-match",
        "line-number",
        "line-regexp",
        "max-columns-preview",
        "multiline",
        "multiline-dotall",
        "only-matching",
        "passthru",
        "pretty",
        "quiet",
        "smart-case",
        "case-sensitive",
        "stats",
        "sort-files",
        "trim",
        "unrestricted",
        "vimgrep",
        "with-filename",
        "word-regexp",
        "null",
        "null-data",
        "column",
        "no-config",
        "no-unicode",
        "crlf",
        "json",
        "include-zero",
        "one-file-system",
        "binary",
        "no-binary",
        "pcre2",
        "no-pcre2",
        "type-list",
        "version",
        "help",
    ],
    long_valued: &[
        "after-context",
        "before-context",
        "context",
        "max-count",
        "max-depth",
        "maxdepth",
        "max-filesize",
        "max-columns",
        "replace",
        "regexp",
        "file",
        "glob",
        "iglob",
        "type",
        "type-not",
        "type-add",
        "type-clear",
        "threads",
        "encoding",
        "color",
        "colors",
        "sort",
        "sortr",
        "path-separator",
        "context-separator",
        "field-context-separator",
        "field-match-separator",
        "ignore-file",
        "engine",
    ],
    long_optional: &[],
};

// -o / --output 写文件、--compress-program 执行外部程序、-T 把临时文件写到指定目录：都不在列表里
const SORT: Spec = Spec {
    flags: "bdfghiMnRrVcCmsuz",
    valued: "ktS",
    optional: "",
    long_flags: &[
        "ignore-leading-blanks",
        "dictionary-order",
        "ignore-case",
        "general-numeric-sort",
        "human-numeric-sort",
        "ignore-nonprinting",
        "month-sort",
        "numeric-sort",
        "random-sort",
        "reverse",
        "version-sort",
        "merge",
        "stable",
        "unique",
        "zero-terminated",
        "debug",
        "help",
        "version",
    ],
    long_valued: &[
        "key",
        "field-separator",
        "buffer-size",
        "parallel",
        "batch-size",
        "files0-from",
        "random-source",
        "sort",
    ],
    long_optional: &["check"],
};

// 第二个位置参数是输出文件，所以位置参数最多一个
const UNIQ: Spec = Spec {
    flags: "cdDiuz",
    valued: "fsw",
    optional: "",
    long_flags: &[
        "count",
        "repeated",
        "ignore-case",
        "unique",
        "zero-terminated",
        "help",
        "version",
    ],
    long_valued: &["skip-fields", "skip-chars", "check-chars"],
    long_optional: &["all-repeated", "group"],
};

// -o 写文件
const TREE: Spec = Spec {
    flags: "adfilxqNQpugshDFvrtcUCnSAJX",
    valued: "LPIT",
    optional: "",
    long_flags: &[
        "gitignore",
        "ignore-case",
        "matchdirs",
        "metafirst",
        "prune",
        "noreport",
        "dirsfirst",
        "filesfirst",
        "du",
        "inodes",
        "device",
        "si",
        "fromfile",
        "version",
        "help",
    ],
    long_valued: &["charset", "filelimit", "timefmt", "sort"],
    long_optional: &[],
};

// -C / --compile 编译 magic 文件（写文件）
const FILE: Spec = Spec {
    flags: "bhiklLnNprsvzZ0",
    valued: "efFmP",
    optional: "",
    long_flags: &[
        "brief",
        "mime",
        "mime-type",
        "mime-encoding",
        "dereference",
        "no-dereference",
        "keep-going",
        "preserve-date",
        "raw",
        "special-files",
        "uncompress",
        "uncompress-noreport",
        "extension",
        "no-buffer",
        "no-pad",
        "print0",
        "apple",
        "version",
        "help",
    ],
    long_valued: &[
        "exclude",
        "exclude-quiet",
        "separator",
        "files-from",
        "magic-file",
        "parameter",
    ],
    long_optional: &[],
};

// -K / --kill 杀连接、-D / --diag 把原始数据写进文件，都不在列表里
const SS: Spec = Spec {
    flags: "hVHnraloemipsZzb460tuwxSdMXO",
    valued: "AfFN",
    optional: "",
    long_flags: &[
        "help",
        "version",
        "numeric",
        "resolve",
        "all",
        "listening",
        "options",
        "extended",
        "memory",
        "processes",
        "info",
        "summary",
        "context",
        "contexts",
        "tcp",
        "udp",
        "raw",
        "unix",
        "packet",
        "sctp",
        "dccp",
        "vsock",
        "tipc",
        "xdp",
        "mptcp",
        "no-header",
        "oneline",
        "ipv4",
        "ipv6",
    ],
    long_valued: &["family", "query", "filter", "net"],
    long_optional: &[],
};

// -c 指定缓存文件、-w 写缓存、-g 回收缓存：都会写文件
const BLKID: Spec = Spec {
    flags: "dklpivVh",
    valued: "LnoOSstUu",
    optional: "",
    long_flags: &[
        "no-encoding",
        "list-filesystems",
        "list-one",
        "probe",
        "info",
        "version",
        "help",
    ],
    long_valued: &[
        "match-token",
        "match-tag",
        "output",
        "offset",
        "size",
        "usages",
        "label",
        "uuid",
        "match-types",
    ],
    long_optional: &[],
};

// -C / --clear 清记录、-S / --set 改记录、-R 换根目录：都不在列表里
const LASTLOG: Spec = Spec {
    flags: "h",
    valued: "btu",
    optional: "",
    long_flags: &["help"],
    long_valued: &["before", "time", "user"],
    long_optional: &[],
};

// 不含 -c / -C / -D / -E / -n（清缓冲区、开关控制台输出、改控制台日志级别）及其长选项
const DMESG: Spec = Spec {
    flags: "deHJkPprSTtuWwx",
    valued: "Ffls",
    optional: "L",
    long_flags: &[
        "show-delta",
        "reltime",
        "human",
        "json",
        "kernel",
        "nopager",
        "force-prefix",
        "raw",
        "syslog",
        "ctime",
        "notime",
        "userspace",
        "follow-new",
        "follow",
        "decode",
        "help",
        "version",
    ],
    long_valued: &[
        "facility",
        "level",
        "buffer-size",
        "file",
        "since",
        "until",
        "time-format",
    ],
    long_optional: &["color"],
};

// 不含 --flush / --rotate / --sync / --relinquish-var / --vacuum-* / --setup-keys /
// --update-catalog / --cursor-file（会写游标文件）/ --root / --image
const JOURNALCTL: Spec = Spec {
    flags: "aefklmqrxNh",
    valued: "uTtpgoDMcSUF",
    optional: "nb",
    long_flags: &[
        "system",
        "user",
        "merge",
        "dmesg",
        "reverse",
        "catalog",
        "quiet",
        "all",
        "follow",
        "no-tail",
        "full",
        "no-full",
        "no-pager",
        "no-hostname",
        "utc",
        "show-cursor",
        "list-boots",
        "list-namespaces",
        "disk-usage",
        "pager-end",
        "fields",
        "version",
        "help",
    ],
    long_valued: &[
        "machine",
        "directory",
        "file",
        "namespace",
        "since",
        "until",
        "cursor",
        "after-cursor",
        "unit",
        "user-unit",
        "identifier",
        "exclude-identifier",
        "priority",
        "facility",
        "grep",
        "output",
        "output-fields",
        "field",
    ],
    long_optional: &["lines", "boot", "case-sensitive"],
};

// 没有 -s / --set（改时间）。位置参数只认 `+格式`（MMDDhhmm 这种位置参数是在改时间）
const DATE: Spec = Spec {
    flags: "uR",
    valued: "dfr",
    optional: "I",
    long_flags: &[
        "utc",
        "universal",
        "rfc-email",
        "debug",
        "resolution",
        "help",
        "version",
    ],
    long_valued: &["date", "file", "reference", "rfc-3339"],
    long_optional: &["iso-8601"],
};

// 没有 -F / --file、-b / --boot（设主机名）；位置参数是在设主机名
const HOSTNAME: Spec = Spec {
    flags: "adfAiIsyvhV",
    valued: "",
    optional: "",
    long_flags: &[
        "alias",
        "domain",
        "fqdn",
        "long",
        "all-fqdns",
        "ip-address",
        "all-ip-addresses",
        "short",
        "yp",
        "nis",
        "verbose",
        "help",
        "version",
    ],
    long_valued: &[],
    long_optional: &[],
};

// systemd 的 *ctl：选项之后的第一个位置参数是子命令。选项的值要按"必定要值"吞掉，
// 否则 `systemctl -p show restart x` 会把 `show` 当成子命令。
// 不含 -H / --host、-M / --machine、--root 这类会换目标的选项。
const SYSTEMD_CTL: Spec = Spec {
    flags: "alqr",
    valued: "tpnoP",
    optional: "",
    long_flags: &[
        "user",
        "system",
        "no-pager",
        "no-legend",
        "no-ask-password",
        "all",
        "full",
        "quiet",
        "plain",
        "value",
        "show-types",
        "failed",
        "recursive",
        "static",
        "transient",
        "pretty",
    ],
    long_valued: &["type", "property", "state", "lines", "output", "json"],
    long_optional: &[],
};

const SYSTEMCTL_VERBS: &[&str] = &[
    "status",
    "show",
    "cat",
    "list-units",
    "list-unit-files",
    "list-timers",
    "list-sockets",
    "list-dependencies",
    "list-jobs",
    "is-active",
    "is-enabled",
    "is-failed",
    "is-system-running",
    "get-default",
    "show-environment",
];

const OTHER_CTL_VERBS: &[&str] = &[
    "status",
    "show",
    "list",
    "list-sessions",
    "list-users",
    "query",
    "statistics",
    "list-timezones",
];

/// `*ctl` 类命令：选项都在允许列表里，第一个位置参数是只读子命令。
/// `allow_missing_verb`：没有子命令时命令默认做的是 status 之类的查询。
fn ctl_is_read_only(args: &[String], verbs: &[&str], allow_missing_verb: bool) -> bool {
    let Some(positional) = parse_args(&SYSTEMD_CTL, args) else {
        return false;
    };
    match positional.first() {
        Some(verb) => verbs.contains(verb),
        None => allow_missing_verb,
    }
}

/// `ip [选项] 对象 [show|list|ls ...]`：只放行明确的读操作。
fn ip_is_read_only(args: &[String]) -> bool {
    // 选项：只认完整写法。-b / -batch（从文件读一批命令）、-force 不在其中
    const FLAGS: &[&str] = &[
        "-4",
        "-6",
        "-0",
        "-B",
        "-M",
        "-s",
        "-d",
        "-r",
        "-h",
        "-o",
        "-j",
        "-p",
        "-a",
        "-c",
        "-t",
        "-ts",
        "-br",
        "-brief",
        "-color",
        "-details",
        "-stats",
        "-statistics",
        "-resolve",
        "-oneline",
        "-json",
        "-pretty",
        "-human",
        "-iec",
        "-all",
        "-timestamp",
        "-tshort",
    ];
    const VALUED: &[&str] = &["-f", "-family", "-n", "-netns"];
    const OBJECTS: &[&str] = &[
        "address",
        "addr",
        "a",
        "route",
        "ro",
        "r",
        "link",
        "l",
        "neigh",
        "neighbor",
        "neighbour",
        "n",
        "rule",
        "maddress",
        "maddr",
        "netconf",
        "netns",
    ];
    // 万一出现在 show 后面也不放行的子命令词
    const WRITE_WORDS: &[&str] = &[
        "add", "del", "delete", "set", "flush", "change", "replace", "append", "exec", "monitor",
        "save", "restore",
    ];
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        let arg = args[i].as_str();
        i += 1;
        if VALUED.contains(&arg) {
            if i >= args.len() {
                return false;
            }
            i += 1;
        } else if !FLAGS.contains(&arg) {
            return false;
        }
    }
    let Some(object) = args.get(i) else {
        return false;
    };
    if !OBJECTS.contains(&object.as_str()) {
        return false;
    }
    let rest = &args[i + 1..];
    match rest.first() {
        None => true,
        Some(command) => {
            matches!(command.as_str(), "show" | "list" | "ls")
                && !rest[1..].iter().any(|a| WRITE_WORDS.contains(&a.as_str()))
        }
    }
}

/// 全局选项里允许出现的（带值的）：`kubectl -n prod get pods`。别的选项在子命令之前出现一律要确认。
fn kubectl_is_read_only(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        let arg = args[i].as_str();
        i += 1;
        let (name, has_value) = match arg.split_once('=') {
            Some((name, _)) => (name, true),
            None => (arg, false),
        };
        if !matches!(
            name,
            "-n" | "--namespace" | "--context" | "--cluster" | "--kubeconfig"
        ) {
            return false;
        }
        if !has_value {
            if i >= args.len() {
                return false;
            }
            i += 1;
        }
    }
    let Some(verb) = args.get(i).map(String::as_str) else {
        return false;
    };
    let rest = &args[i + 1..];
    match verb {
        "get" | "describe" | "logs" | "top" | "version" | "api-resources" | "api-versions"
        | "explain" => true,
        // `cluster-info dump` 会把内容写进 --output-directory
        "cluster-info" => rest.iter().all(|a| a.starts_with('-')),
        _ => false,
    }
}

/// docker / podman：选项在子命令之前出现（`--config`、`-H`、`-p` 都带值，值可能冒充子命令）
/// 一律要确认；子命令只认紧跟在后面的那个词。
fn docker_is_read_only(args: &[String]) -> bool {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(first) = words.first().copied() else {
        return false;
    };
    if first.starts_with('-') {
        return false;
    }
    let rest = &words[1..];
    match first {
        "compose" => {
            let Some(sub) = rest.first().copied() else {
                return false;
            };
            match sub {
                "ps" | "logs" | "ls" | "images" | "top" | "version" => true,
                // `config -o 文件` / `--output` 把结果写进文件
                "config" => rest[1..].iter().all(|a| {
                    matches!(
                        *a,
                        "-q" | "--quiet"
                            | "--services"
                            | "--volumes"
                            | "--images"
                            | "--profiles"
                            | "--no-interpolate"
                            | "--no-normalize"
                            | "--resolve-image-digests"
                            | "--hash"
                            | "--environment"
                    ) || !a.starts_with('-')
                }),
                _ => false,
            }
        }
        "ps" | "images" | "logs" | "inspect" | "version" | "info" | "top" | "port" | "diff"
        | "history" | "events" => true,
        "stats" => rest.contains(&"--no-stream"),
        "container" | "image" | "network" | "volume" => rest
            .first()
            .is_some_and(|sub| matches!(*sub, "ls" | "list" | "inspect")),
        _ => false,
    }
}

/// `apt` / `apt-cache`：全局选项（`-t 发行版`、`-o 配置项`、`-c 配置文件` 都带值，值可能冒充子命令）
/// 不允许出现在子命令之前；子命令后面只认几个明确的查询类选项。
fn apt_is_read_only(args: &[String]) -> bool {
    let Some(sub) = args.first().map(String::as_str) else {
        return false;
    };
    if !matches!(
        sub,
        "list" | "show" | "search" | "policy" | "depends" | "rdepends" | "madison"
    ) {
        return false;
    }
    args[1..].iter().all(|a| {
        !a.starts_with('-')
            || matches!(
                a.as_str(),
                "--installed"
                    | "--upgradable"
                    | "--all-versions"
                    | "-a"
                    | "--names-only"
                    | "-n"
                    | "--full"
                    | "-f"
                    | "--no-all-versions"
            )
    })
}

/// `rpm -q…`：只认查询类选项；`--pipe`、`--define`、`--eval` 这些能执行东西的不在其中。
fn rpm_is_read_only(args: &[String]) -> bool {
    const QUERY_LETTERS: &str = "qailfcdRpsgvV";
    const LONG: &[&str] = &[
        "--all",
        "--info",
        "--list",
        "--file",
        "--configfiles",
        "--docfiles",
        "--requires",
        "--provides",
        "--whatprovides",
        "--whatrequires",
        "--changelog",
        "--scripts",
        "--last",
        "--dump",
        "--state",
        "--package",
        "--verbose",
        "--triggers",
        "--conflicts",
        "--obsoletes",
        "--recommends",
        "--suggests",
        "--supplements",
        "--enhances",
        "--nosignature",
        "--nodigest",
    ];
    let Some(first) = args.first() else {
        return false;
    };
    if !first.starts_with("-q") {
        return false;
    }
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if !arg.starts_with('-') {
            continue;
        }
        if LONG.contains(&arg) {
            continue;
        }
        if matches!(arg, "--queryformat" | "--qf") {
            // 格式串里的 %(…) / %{lua:…} 会被当成宏执行
            let Some(format) = args.get(i) else {
                return false;
            };
            i += 1;
            let lower = format.to_ascii_lowercase();
            if lower.contains("%(") || lower.contains("lua") || lower.contains("expand") {
                return false;
            }
            continue;
        }
        if arg.len() > 1
            && !arg.starts_with("--")
            && arg[1..].chars().all(|c| QUERY_LETTERS.contains(c))
        {
            continue;
        }
        return false;
    }
    true
}

/// 一类命令的参数规则。分类集中在 [`rule_for`] 一处：要看参数的命令只能在那里登记，
/// 登记了就自动走"参数里不能有要 shell 展开的写法"这道检查，不用在每个分支各写一遍。
enum Rule {
    /// 没有任何能写文件 / 执行程序 / 改系统状态的选项：带什么参数都一样，展开成什么都无害。
    Unfiltered,
    /// 要看参数：参数里有任何需要 shell 展开才知道真实内容的词就直接要确认，
    /// 否则再交给这个函数按允许列表检查。
    Filtered(fn(&[String]) -> bool),
    /// 不在白名单里。
    Unknown,
}

fn rule_for(name: &str) -> Rule {
    match name {
        // --pre / --pre-glob / --hostname-bin 会执行外部程序，-z 会调用解压程序，都不在列表里
        "rg" => Rule::Filtered(|args| parse_args(&RG, args).is_some()),
        "sort" => Rule::Filtered(|args| parse_args(&SORT, args).is_some()),
        "uniq" => {
            Rule::Filtered(|args| parse_args(&UNIQ, args).is_some_and(|rest| rest.len() <= 1))
        }
        "tree" => Rule::Filtered(|args| parse_args(&TREE, args).is_some()),
        "file" => Rule::Filtered(|args| parse_args(&FILE, args).is_some()),
        "ss" => Rule::Filtered(|args| parse_args(&SS, args).is_some()),
        "blkid" => Rule::Filtered(|args| parse_args(&BLKID, args).is_some()),
        "lastlog" => {
            Rule::Filtered(|args| parse_args(&LASTLOG, args).is_some_and(|rest| rest.is_empty()))
        }
        // history -c 清空、-w 写文件、-d 删除……只认不带参数的
        "history" => Rule::Filtered(|args| args.is_empty()),
        "printf" => Rule::Filtered(printf_is_read_only),
        // find 的主表达式（-exec、-delete、-fprint…）必须写全，不能缩写，也不能合并，
        // 所以按完整名字拦就够了；带这些名字当值的写法（`-name -delete`）也一并要确认
        "find" => Rule::Filtered(|args| {
            !args.iter().any(|a| {
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
            })
        }),
        // 带位置参数的 hostname 是在改主机名
        "hostname" => {
            Rule::Filtered(|args| parse_args(&HOSTNAME, args).is_some_and(|rest| rest.is_empty()))
        }
        // 位置参数只认 +格式；MMDDhhmm 这种是在改时间
        "date" => Rule::Filtered(|args| {
            parse_args(&DATE, args).is_some_and(|rest| rest.iter().all(|p| p.starts_with('+')))
        }),
        "env" => Rule::Filtered(|args| args.is_empty()),
        "mount" => Rule::Filtered(|args| args.is_empty() || args.iter().all(|a| a == "-l")),
        "dmesg" => Rule::Filtered(|args| parse_args(&DMESG, args).is_some()),
        "journalctl" => Rule::Filtered(|args| parse_args(&JOURNALCTL, args).is_some()),
        // sed 的选项只认下面几个完整写法，脚本只认纯打印，见 sed_is_read_only
        "sed" => Rule::Filtered(sed_is_read_only),
        "ip" => Rule::Filtered(ip_is_read_only),
        "systemctl" => Rule::Filtered(|args| ctl_is_read_only(args, SYSTEMCTL_VERBS, false)),
        "service" => Rule::Filtered(|args| {
            (args.len() == 2 && !args[0].starts_with('-') && args[1] == "status")
                || (args.len() == 1 && args[0] == "--status-all")
        }),
        "timedatectl" | "hostnamectl" | "loginctl" | "localectl" | "networkctl" | "resolvectl" => {
            Rule::Filtered(|args| ctl_is_read_only(args, OTHER_CTL_VERBS, true))
        }
        "docker" | "podman" => Rule::Filtered(docker_is_read_only),
        "kubectl" => Rule::Filtered(kubectl_is_read_only),
        // 不带参数的 nginx 会启动服务
        "nginx" => Rule::Filtered(|args| {
            !args.is_empty()
                && args
                    .iter()
                    .all(|a| a == "-t" || a == "-T" || a == "-v" || a == "-V")
        }),
        "apt" | "apt-cache" => Rule::Filtered(apt_is_read_only),
        // 每个参数要么是下面几个完整选项，要么是不带 - 的包名 / 文件名，不会被缩写或合并写法绕过
        "dpkg" => Rule::Filtered(|args| {
            args.iter().all(|a| {
                matches!(
                    a.as_str(),
                    "-l" | "-L" | "-s" | "-S" | "--list" | "--status" | "--listfiles" | "--search"
                ) || !a.starts_with('-')
            }) && args.first().is_some_and(|a| a.starts_with('-'))
        }),
        "rpm" => Rule::Filtered(rpm_is_read_only),
        _ if ALWAYS.contains(&name) => Rule::Unfiltered,
        _ => Rule::Unknown,
    }
}

/// bash 内建 `printf -v 变量` 会给 shell 变量赋值（`PROMPT_COMMAND`、`PATH`……），
/// 是在改终端会话的行为。只有 `printf 格式 参数…` 和 `printf -- 格式 …` 算只读。
fn printf_is_read_only(args: &[String]) -> bool {
    args.first()
        .is_none_or(|first| !first.starts_with('-') || first == "--")
}

fn segment_is_read_only(segment: &str) -> bool {
    let Ok(words) = tokenize_marked(segment) else {
        return false;
    };
    let words = without_harmless_redirects(words);
    let Some(head) = words.first() else {
        return true;
    };
    // 命令名要展开（`$X file`、`{cat,x}`、`c*t x`）就不知道实际跑的是什么。
    // 单独一个 `[` 是 test 的别名，不是通配符。
    if head.expands && head.text != "[" {
        return false;
    }
    // `FOO=bar cmd` 能改命令的行为（LD_PRELOAD 之类），不算只读
    if head.text.contains('=') {
        return false;
    }
    let Some(name) = program_name(&head.text) else {
        return false;
    };
    match rule_for(name) {
        Rule::Unfiltered => true,
        Rule::Filtered(check) => {
            // 参数要靠 shell 展开才知道真实内容（`sort $'\x2do'`、`sort {-o,x}`、`sort $X`）：
            // 允许列表检查的是展开前的文字，对不上；不去"计算"展开结果，直接要确认
            if words[1..].iter().any(|word| word.expands) {
                return false;
            }
            let args: Vec<String> = words[1..].iter().map(|word| word.text.clone()).collect();
            check(&args)
        }
        Rule::Unknown => false,
    }
}

/// 整条命令按 bash 的引号规则扫一遍，找出"在解析之前 / 之外"就会改变命令含义的写法，有就要确认：
/// - 历史展开 `!`：命令是打进交互式终端的，bash 会在解析之前把以前命令的文字拼进来，
///   拼进来的 `;`、`|`、`>` 照样生效。单引号里和反斜杠转义的 `!` 不展开；
///   `!` 后面紧跟空白、行尾、`=`、`(`（双引号里还有 `"`）也不展开（`test ! -f x`、`a != b`）。
///   行首的 `^旧^新` 是同一套机制的快速替换；
/// - 引号外的 `$'…'`：里面的 `\'` 不结束字符串，而 [`split_segments`] / [`tokenize`] 不认这个规则，
///   会把引号配对错位，把真正的命令当成引号里的文字；
/// - 引号外、词首的 `#`：bash 把它到行尾当注释，分段逻辑却不认，同样会错位。
fn has_unsafe_shell_syntax(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    if chars.first() == Some(&'^') {
        return true;
    }
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_single {
            in_single = c != '\'';
            i += 1;
            continue;
        }
        if c == '\\' {
            i += 2;
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '!' if history_expansion_may_trigger(next, true) => return true,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_single = true,
            '"' => in_double = true,
            '$' if next == Some('\'') => return true,
            '!' if history_expansion_may_trigger(next, false) => return true,
            '#' if i == 0
                || chars[i - 1].is_whitespace()
                || matches!(chars[i - 1], ';' | '&' | '|' | '(' | ')') =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn history_expansion_may_trigger(next: Option<char>, in_double: bool) -> bool {
    match next {
        None | Some('=' | '(') => false,
        Some('"') if in_double => false,
        Some(c) => !c.is_whitespace(),
    }
}

/// `sed -n '10,20p' file`、`sed -n '/re/p' file`：只认这种只打印的写法。
/// 选项只认 `-n -E -r --quiet --silent` 这几个完整写法（缩写、合并写法、`-i`、`-s`、`-f` 都要确认）。
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
                && !arg[1..arg.len() - 2].contains('/')
                && !arg.contains('\n'));
        if !ok {
            return false;
        }
    }
    script_seen
}

#[cfg(test)]
mod tests {
    use super::is_read_only;

    fn assert_all_read_only(commands: &[&str]) {
        for command in commands {
            assert!(is_read_only(command), "应该算只读：{command}");
        }
    }

    fn assert_all_need_approval(commands: &[&str]) {
        for command in commands {
            assert!(!is_read_only(command), "不该算只读：{command}");
        }
    }

    #[test]
    fn everyday_read_commands_pass() {
        assert_all_read_only(&[
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
        ]);
    }

    #[test]
    fn common_read_usages_of_filtered_commands_still_pass() {
        assert_all_read_only(&[
            "sort -k2 file",
            "sort -t, -k2 file",
            "sort -t , -k 2,2nr file",
            "sort -rn -k3 file",
            "sort -u -f file",
            "sort --reverse --numeric-sort file",
            "sort --key=2 --field-separator=, file",
            "sort -c file",
            "sort --check=quiet file",
            "sort -S 50% file",
            "uniq -c",
            "uniq -f 1 file",
            "uniq -c in.txt",
            "uniq --count --ignore-case",
            "date",
            "date +%F",
            "date '+%Y-%m-%d %H:%M'",
            "date -u",
            "date -u +%s",
            "date -R",
            "date -I",
            "date -Iseconds",
            "date --iso-8601=seconds",
            "date -d '2 days ago' +%F",
            "date --date=@0",
            "date -r /etc/hostname",
            "date --reference=/etc/hostname",
            "date --utc",
            "hostname",
            "hostname -I",
            "hostname -f",
            "hostname --fqdn",
            "hostname -s",
            "hostname -i",
            "hostname -d",
            "ip a",
            "ip addr",
            "ip addr show",
            "ip address list",
            "ip addr show dev eth0",
            "ip route",
            "ip r",
            "ip route show table main",
            "ip -br link",
            "ip -br a",
            "ip -4 -o addr show",
            "ip -s link show eth0",
            "ip l",
            "ip link ls",
            "ip neigh",
            "ip n show",
            "ip rule",
            "ip rule list",
            "ip -j route",
            "tree -L 2 /etc",
            "tree -a -d --noreport",
            "file -b /bin/ls",
            "file --mime-type /bin/ls",
            "ss -tulpn",
            "ss -s",
            "ss -tan state established",
            "blkid",
            "blkid -o value -s UUID /dev/sda1",
            "lastlog",
            "lastlog -u root",
            "rg -n -i --hidden listen /etc/nginx",
            "rg -g '*.conf' -A 3 server /etc/nginx",
            "rg --files /etc/nginx",
            "rg -e foo -e bar /etc",
            "dmesg",
            "dmesg -T",
            "dmesg -T -l err,warn",
            "dmesg --level=err --ctime",
            "dmesg -H -x",
            "journalctl -n 100",
            "journalctl -xeu nginx",
            "journalctl -b -1 -p err",
            "journalctl -f -u nginx",
            "journalctl --disk-usage",
            "journalctl -u nginx --since '1 hour ago' -o json-pretty",
            "journalctl --lines=50 --no-pager",
            "systemctl list-units --type=service --state=running",
            "systemctl is-active nginx",
            "systemctl show -p MainPID nginx",
            "systemctl status nginx -l --no-pager",
            "systemctl --user status foo",
            "timedatectl",
            "timedatectl status",
            "hostnamectl",
            "loginctl list-sessions",
            "kubectl -n prod get pods",
            "kubectl get pods --namespace=prod -o wide",
            "kubectl logs -f web-1",
            "kubectl cluster-info",
            "docker ps --format '{{.Names}}'",
            "docker inspect -f '{{.State.Status}}' web",
            "docker container ls -a",
            "docker compose logs --tail 20 web",
            "docker compose config --services",
            "apt show nginx",
            "apt-cache policy nginx",
            "apt search -n nginx",
            "rpm -qa --qf '%{NAME}-%{VERSION}\\n'",
            "rpm -qi nginx",
            "rpm -ql nginx",
            "rpm -qf /usr/bin/ls",
            "service nginx status",
            "mount",
            "mount -l",
            "env",
            "history",
        ]);
    }

    #[test]
    fn anything_that_changes_the_system_needs_approval() {
        assert_all_need_approval(&[
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
        ]);
    }

    /// AUDIT-2 A01：综合审查里实际放行了的四类写法。
    #[test]
    fn audit_bypasses_need_approval() {
        assert_all_need_approval(&[
            // GNU 长选项缩写
            "sort --out=out.txt input.txt",
            "sort --o=out.txt input.txt",
            "sort --output out.txt input.txt",
            "sort --compress=/tmp/x input.txt",
            // 短选项直接带值
            "date -s2020-01-01",
            "date -u -s2020-01-01",
            "date --set='2020-01-01'",
            "date --se=2020-01-01",
            // hostname 从文件设主机名
            "hostname --file=/tmp/name",
            "hostname -F /tmp/name",
            "hostname -F/tmp/name",
            "hostname --fil=/tmp/name",
            "hostname -b",
            // ip 子命令缩写
            "ip l s dev eth0 down",
            "ip l sh",
            "ip a add 10.0.0.2/24 dev eth0",
            "ip addr a 10.0.0.2/24 dev eth0",
            "ip r del default",
            "ip route flush all",
            "ip n flush all",
            "ip link set dev eth0 down",
            "ip netns exec foo sh",
            "ip -force link set eth0 down",
            "ip -b /tmp/cmds",
            "ip -batch /tmp/cmds",
            // 黑名单遗漏：写到 /dev/null 开头的另一个文件
            "echo x >/dev/null.audit",
            "echo x > /dev/null.audit",
            "echo x 2>/dev/null.audit",
            "echo x >&2out",
        ]);
    }

    #[test]
    fn unknown_or_abbreviated_options_need_approval() {
        assert_all_need_approval(&[
            // sort：缩写 / 未知 / 合并写法里混了写文件的字母
            "sort --reve file",
            "sort --unknown-flag file",
            "sort -T /tmp file",
            "sort --temporary-directory=/tmp file",
            "sort -nrofile file",
            "sort -k2 -o out file",
            // uniq
            "uniq --cou in",
            "uniq -x in",
            "uniq -f 1 in out",
            // date / hostname：没列出的选项
            "date --dat=@0",
            "date -x",
            "date 010203042020",
            "date -d @0 010203042020",
            "hostname -z",
            "hostname --short-name",
            "hostname -sb",
            // tree / file / ss / blkid / lastlog
            "tree --output=/tmp/o /etc",
            "tree -L 2 -o /tmp/o",
            "tree --unknown /etc",
            "file --comp -m /tmp/magic",
            "file --compile",
            "file -bC",
            "ss --kill dst 10.0.0.1",
            "ss --ki dst 10.0.0.1",
            "ss -tK dst 10.0.0.1",
            "ss -D /tmp/raw.out",
            "ss --diag=/tmp/raw.out",
            "blkid -g",
            "blkid -c /tmp/cache",
            "blkid -w /tmp/cache",
            "blkid --garbage",
            "lastlog -C -u root",
            "lastlog --clear --user root",
            "lastlog -S -u root",
            "lastlog -R /tmp",
            // rg：执行外部程序的选项及其缩写 / 合并写法
            "rg --pre=/bin/sh x",
            "rg --pre-glob '*' x",
            "rg --hostname-bin /tmp/x x",
            "rg --hostname-bin=/tmp/x x",
            "rg --search-zip x",
            "rg -zn x",
            "rg --unknown-flag x",
            // dmesg：清缓冲、开关控制台、改日志级别（含 --read-clear 和缩写）
            "dmesg -C",
            "dmesg -c",
            "dmesg -cT",
            "dmesg -Tc",
            "dmesg --read-clear",
            "dmesg --read",
            "dmesg --cl",
            "dmesg --clear",
            "dmesg -D",
            "dmesg -E",
            "dmesg -n 1",
            "dmesg --console-level=1",
            "dmesg --console-off",
            "dmesg --console-o",
            // journalctl：清理、轮转、写游标等（含缩写）
            "journalctl --rotate",
            "journalctl --rot",
            "journalctl --flush",
            "journalctl --fl",
            "journalctl --sync",
            "journalctl --vacuum-size=10M",
            "journalctl --vacuum-t=1d",
            "journalctl --relinquish-var",
            "journalctl --setup-keys",
            "journalctl --update-catalog",
            "journalctl --cursor-file=/tmp/c",
            "journalctl -n --rotate",
            "journalctl --unknown",
            // sed：只认纯打印
            "sed -ni '1p' file",
            "sed --quiet --in-place '1p' file",
            "sed --in-pl 's/a/b/' file",
            "sed -n -f script.sed file",
            "sed -s -n '1p' f1 f2",
            "sed -n '1p;w out' file",
            // env / mount / find
            "env FOO=bar ls",
            "env -i ls",
            "mount /dev/sda1 /mnt",
            "mount -o remount,rw /",
            "find . -fprint out.txt",
            "find . -fprintf out.txt '%p'",
            "find . -execdir rm {} +",
            "find . -ok rm {} \\;",
            // history 只认不带参数
            "history -w",
            "history -d 1",
        ]);
    }

    /// 选项的"值"不能被当成子命令：这些写法里的子命令词是别的选项的值，真正的子命令是写操作。
    #[test]
    fn option_values_cannot_pose_as_subcommands() {
        assert_all_need_approval(&[
            "systemctl -p show restart nginx",
            "systemctl --property show restart nginx",
            "systemctl -t status restart nginx",
            "systemctl --state=failed -n status restart nginx",
            "systemctl -H host status",
            "systemctl --host=host status",
            "systemctl --reboot status",
            "loginctl -p status kill-session 1",
            "timedatectl -H host status",
            "timedatectl set-time 2020-01-01",
            "hostnamectl set-hostname x",
            "hostnamectl -H host status",
            "apt -o status install htop",
            "apt -t list install htop",
            "apt -c list install htop",
            "apt update",
            "apt-cache -o x=y policy nginx",
            "kubectl -n get delete pod x",
            "kubectl --context get delete pod x",
            "kubectl --kubeconfig get delete pod x",
            "kubectl -s https://example.invalid get pods",
            "kubectl cluster-info dump",
            "kubectl cluster-info dump --output-directory=/tmp/o",
            "docker --config ps rm -f web",
            "docker -H ps rm web",
            "docker --context ps rm web",
            "docker compose -p ps down",
            "docker compose -f x.yml ps",
            "docker compose config -o /tmp/out.yml",
            "docker compose config --output=/tmp/out.yml",
            "docker compose config -qo /tmp/out.yml",
            "docker compose up -d",
            "podman --remote ps",
            "service --full-restart status",
            "rpm -qa --define 'x %(touch /tmp/x)'",
            "rpm -qa --eval '%(id)'",
            "rpm -qa --qf '%(id)'",
            "rpm -qa --qf '%{lua:os.execute(\"id\")}'",
            "rpm -e nginx",
            "rpm -qe nginx",
            "rpm -q -e nginx",
            "dpkg -i foo.deb",
            "dpkg -l -i foo.deb",
            "dpkg --purge x",
        ]);
    }

    /// AUDIT-3：允许列表检查的是展开前的文字，shell 展开后参数可能变成选项。
    #[test]
    fn expansion_in_arguments_of_filtered_commands_needs_approval() {
        assert_all_need_approval(&[
            // 复审的三条
            "sort $'\\x2do' out.txt in.txt",
            "sort {-o,out.txt} in.txt",
            "printf -v x %s '-o out.txt'; sort $x in.txt",
            // 变量 / 花括号 / 算术 / 引用形式
            "sort ${X} in",
            "sort \"$X\" in",
            "sort $\"-o\" out in",
            "sort $((1+1)) in",
            "sort a${X}b in",
            "date $X",
            "ip $X",
            "systemctl $X",
            "hostname {a,b}",
            "rg $PATTERN /var/log",
            "journalctl -u $UNIT",
            "find $DIR -name x",
            "docker ps $X",
            "kubectl get $X",
            "sed -n $X file",
            "dpkg -l $X",
            "rpm -qa $X",
            "apt show $X",
            "uniq $X",
            "tree $X",
            "file $X",
            "ss $X",
            "blkid $X",
            "lastlog $X",
            "dmesg $X",
            "service $X status",
            "timedatectl $X",
            "nginx -t $X",
            "mount $X",
            "env $X",
            "history $X",
            // 通配符
            "sort *",
            "sort -k1 [ab]*",
            "sort -k1 a?",
            "find /var/log -name *.log",
            "rg foo *.conf",
            // 引号里有、引号外还有
            "sort '-k1' $X",
            "sort -t'$'\" $X\" in",
            "sort \"a\"$X in",
        ]);
    }

    #[test]
    fn expansion_in_the_command_name_needs_approval() {
        assert_all_need_approval(&[
            "$X file",
            "{cat,x}",
            "c*t x",
            "c?t x",
            "ca[t] x",
            "${X} file",
            "$'cat' x",
            "\"$X\" file",
            "ls; $X file",
            "ls | $X",
        ]);
    }

    /// 历史展开发生在解析之前，拼进来的 `;`、`|`、`>` 照样生效，连不看参数的命令也不能放行。
    #[test]
    fn history_expansion_needs_approval() {
        assert_all_need_approval(&[
            "cat !!",
            "cat !$",
            "ls !-1:*",
            "echo \"!!\"",
            "grep x !rm",
            "echo hi !x",
            "echo \"a !b\"",
            "echo \"it's !!\"",
            "^a^b",
            "echo x; ls !!",
            "echo x | cat !-2:*",
            "echo a#b !1",
            "echo '!' !!",
            "echo \"\\\\\" !!",
        ]);
        assert_all_read_only(&[
            "test ! -f x",
            "[ a != b ]",
            "[ ! -f /etc/hosts ]",
            "find . ! -name x",
            "find /var/log ! -name '*.gz'",
            "echo 'a!b'",
            "echo a\\!b",
            "echo \"a\\!b\"",
            "echo \"hi!\"",
            "echo hi!",
            "echo !",
            "echo '!!'",
            "echo \"it's\" 'say !!'",
            "grep 'x$' file",
            "echo a!=b",
            "echo \"!\"",
            "ls ^a^b",
        ]);
    }

    /// 引号外的 `$'…'` 里 `\'` 不结束字符串，词首的 `#` 是注释，分段逻辑都不认。
    /// 这两种会让分段错位、把真正的命令藏进"引号"里，所以整条要确认。
    #[test]
    fn quote_state_that_the_segmenter_cannot_follow_needs_approval() {
        assert_all_need_approval(&[
            "echo $'\\'' ; rm -rf x ; echo $'\\''",
            "echo $'a'",
            "echo x # '\nrm /tmp/y\n# '",
            "echo x #",
            "# hello",
            "ls;# x",
        ]);
        assert_all_read_only(&["echo a#b", "echo 'a #b'", "echo \"a #b\""]);
    }

    #[test]
    fn printf_that_assigns_shell_variables_needs_approval() {
        assert_all_need_approval(&[
            "printf -v PROMPT_COMMAND %s x",
            "printf -vX %s y",
            "printf -v PATH %s /tmp",
            "printf -x",
            "printf --help",
            "printf -",
            "printf $X",
            "printf \"$X\" y",
            // 引号包着的格式串以 - 开头，bash 照样当选项解析
            "printf '-v\\n'",
        ]);
        assert_all_read_only(&[
            "printf '%s\\n' x",
            "printf 'a\\n'",
            "printf -- '%s' x",
            "printf",
            "printf '%s %s\\n' a b | sort -k2",
            "printf '%s\\n' -v",
        ]);
    }

    /// 不看参数的命令：展开成什么都无害，日常写法必须照常自动执行。
    #[test]
    fn unfiltered_commands_still_accept_expansion() {
        assert_all_read_only(&[
            "cat /var/log/*.log",
            "ls -la $HOME",
            "echo $PATH",
            "grep foo *.conf",
            "ls {a,b}",
            "ls ~/x",
            "head -n 5 /var/log/syslog.?",
            "ls /etc/[a-c]*",
            "echo ${HOME}",
            "tail -n 20 \"$LOG\"",
            "ps aux | grep $USER",
            "[ -f /etc/hosts ]",
            "test -d $HOME",
            "[ -n \"$X\" ]",
        ]);
    }

    /// 写在引号里的写法不展开，照常放行。
    #[test]
    fn quoted_arguments_of_filtered_commands_pass() {
        assert_all_read_only(&[
            "rg 'error.*timeout' /var/log",
            "journalctl -u nginx --since '1 hour ago'",
            "sort -t'$' -k2 file",
            "sort -t '$' -k2 file",
            "date '+%F %T'",
            "find /var/log -name '*.log'",
            "find /var/log -name \"*.log\" -mtime -1",
            "sed -n '/a.*b/p' file",
            "sed -n '5,10p' file",
            "rg \"a{2}\" /etc",
            "rg 'a[0-9]+' /etc",
            "rg \"a[b]?\" /etc",
            "rg a\\*b /etc",
            "rg a\\{1\\} /etc",
            "docker ps --format \"{{.Names}}\"",
            "sort -k2 file",
            "date +%F",
            "ip a",
        ]);
    }

    /// 凡是登记成"要看参数"的命令都自动走展开检查：先确认正常写法是只读，
    /// 再在后面加一个要展开的参数，必须变成要确认。
    #[test]
    fn every_filtered_command_rejects_expansion_in_arguments() {
        let samples = [
            "rg foo /etc",
            "sort file",
            "uniq",
            "tree -L 2",
            "file x",
            "ss -s",
            "blkid",
            "lastlog",
            "history",
            "printf '%s' x",
            "find . -name x",
            "hostname",
            "date",
            "env",
            "mount",
            "dmesg",
            "journalctl -n 5",
            "sed -n 1p f",
            "ip a",
            "systemctl status x",
            "service x status",
            "timedatectl",
            "hostnamectl",
            "loginctl",
            "localectl",
            "networkctl",
            "resolvectl",
            "docker ps",
            "podman ps",
            "kubectl get pods",
            "nginx -t",
            "apt show x",
            "apt-cache policy x",
            "dpkg -l",
            "rpm -qa",
        ];
        for sample in samples {
            assert!(is_read_only(sample), "正常写法应该算只读：{sample}");
            assert!(is_read_only(&format!("/usr/bin/{sample}")), "{sample}");
            for arg in ["$X", "${X}", "$\"a\"", "{a,b}", "*", "?", "[a]"] {
                let command = format!("{sample} {arg}");
                assert!(!is_read_only(&command), "不该算只读：{command}");
            }
        }
    }
}
