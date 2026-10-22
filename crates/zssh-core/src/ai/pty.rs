// Adapted from Netcatty electron/bridges/ai/ptyExecHelpers.cjs and
// electron/bridges/ai/ptyExec.cjs (command wrapper, end-marker parsing and
// output normalization), rewritten in Rust for Z-SSH.
//! 在用户终端里执行 AI 命令的纯逻辑（卡片 AI-2c 第 2 节）。
//!
//! 这里只有可以单测的纯函数和状态扫描，不碰进程 / 网络：
//!
//! - [`build_clear_prefix`]：写入命令前先清掉用户输了一半的输入（Ctrl+K Ctrl+U）。
//! - [`build_wrapped_command`]：把命令包进一段带随机结束标记的 POSIX 脚本，前面加一个
//!   空格（`HISTCONTROL=ignorespace` 下不进历史），用结束标记拿退出码；命令本身的
//!   引号、管道、多行都不会被破坏。
//! - [`find_end_marker`] / [`extract_output`]：从终端输出里找结束标记、取退出码、
//!   截取命令输出。
//! - [`TerminalScan`]：边喂终端字节边跟踪备用屏幕（vim / top / less 这类全屏程序会
//!   切到备用屏幕，此时不能往终端里注入命令）。
//!
//! 参考实现里还有 fish / PowerShell / cmd 的包装脚本和远端 shell 探测；Z-SSH 只对
//! 已确认的 POSIX shell 在终端里执行，其余一律走后台通道，所以这里只保留 POSIX。

/// 结束标记那一行的前缀（`<marker>_E:<code>`）。
pub const END_MARKER_SUFFIX: &str = "_E:";
/// 开始标记那一行的后缀（`<marker>_S`）。
pub const START_MARKER_SUFFIX: &str = "_S";
/// 命令行编辑器（BusyBox 的 512 字节）限制下的单行上限；超过就改用分段 `printf` 拼。
const LONG_COMMAND_LINE_BYTES: usize = 300;
/// 保留的终端尾部字节上限：找提示符、判空闲只看最后几行。
const TAIL_KEEP_BYTES: usize = 4096;

/// 截掉多出来的前缀，只保留最后至多 `max` 个字节。
///
/// 不能直接 `text[text.len() - max..]`：字节下标落在多字节字符（中文输出）中间时
/// 切片会 panic。这里把下标往后对齐到最近的字符边界，宁可多截几个字节。
fn truncate_front(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.replace_range(..cut, "");
}

/// 生成的结束标记：`__ZSSH_<随机十六进制>__`。
pub fn new_marker() -> String {
    let mut buf = [0u8; 12];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("__ZSSH_{hex}__")
}

/// 写命令前先清掉输了一半的输入。
///
/// Ctrl+K 删到行尾、Ctrl+U 删到行首（POSIX 终端默认绑定）。Netcatty 的
/// `buildPendingInputClearPrefix` 同款做法。
pub fn build_clear_prefix() -> &'static str {
    "\x0b\x15"
}

/// 单个参数包成 POSIX 单引号串（`'` 用 `'\''` 转义）。
fn posix_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// 把命令包进 POSIX 包装脚本。
///
/// 结构（<m> 是 marker）：
///
/// ```text
///  <m>=0; { printf '\n%s\n' '<m>_S'; trap ':' INT; <m>_cmd=<命令>; eval "$<m>_cmd"; <m>_rc=$?; trap - INT; printf '%s\n' '<m>_E:'"$<m>_rc"; }
/// ```
///
/// 前面一个空格让 bash/zsh 在 `HISTCONTROL=ignorespace` / `HIST_IGNORE_SPACE` 下
/// 不记这条命令；`eval` 让命令里的引号、管道、多行原样生效；整段放在 `{ ... }` 里。
///
/// `trap ':' INT`（Netcatty 的 ptyExecHelpers 同款做法）让 shell 自己在 SIGINT
/// 下不中断这段复合命令，子进程照常收到 SIGINT：命令被打断时 shell 把 130 当
/// 退出码，结束标记才打得出来。没有这个 trap 时，bash 会把整段复合命令连同结束
/// 标记一起丢掉——用户在面板点停止之后，工具会一直等到超时才返回，还会把一条
/// 已经被打断的命令报成"仍在运行"。
///
/// 命令比 TTY 编辑器单行上限长时，改用分段 `printf` 把每行拼进变量（BusyBox）。
pub fn build_wrapped_command(command: &str, marker: &str) -> String {
    let body = if command.len() <= LONG_COMMAND_LINE_BYTES {
        format!("{marker}_cmd={}", posix_single_quote(command))
    } else {
        // 逐行 `printf`：单引号串可以包含换行，所以这里按行拆只是为了让每条物理
        // 行都短于编辑器上限。
        let lines: Vec<&str> = command.split('\n').collect();
        let mut parts = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let format = if index < lines.len() - 1 {
                "%s\\n"
            } else {
                "%s"
            };
            parts.push(format!("printf '{format}' {}", posix_single_quote(line)));
        }
        format!("{marker}_cmd=$({})", parts.join("; "))
    };
    format!(
        " {marker}=0; {{ printf '\\n%s\\n' '{marker}_S'; trap ':' INT; {body}; eval \"${marker}_cmd\"; {marker}_rc=$?; trap - INT; printf '%s\\n' '{marker}_E:'\"${marker}_rc\"; }}\n"
    )
}

/// 找到结束标记的位置和退出码。
///
/// 允许标记出现在行中间（AI-2e 第 2 节）：`printf abc` 这种没有换行结尾的输出，
/// 结束标记会紧跟在输出后面（`abc__ZSSH_…___E:0`）；只认行首的话这类命令要一直
/// 等到超时，还会把一条早就结束的命令报成"仍在运行"。
///
/// 命令回显里那一份包装脚本也含 `<marker>_E:` 字样，靠"`_E:` 后面必须是数字"
/// 排除（回显里紧跟的是引号，不是退出码）。`start` 指向标记本身，这样
/// [`extract_output`] 能把用户输出截在标记前面（行内场景保留 `abc`）。
pub fn find_end_marker(output: &str, marker: &str) -> Option<EndMarker> {
    let pattern = format!("{marker}{END_MARKER_SUFFIX}");
    let mut from = 0;
    while from < output.len() {
        let idx = output[from..].find(&pattern)? + from;
        let after = &output[idx + pattern.len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            // 数字后面要么是行尾，要么是非数字（引号 / 回车之类）。
            let rest = &after[digits.len()..];
            let bounded =
                rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_ascii_digit());
            if bounded {
                return Some(EndMarker {
                    start: idx,
                    exit_code: digits.parse().ok()?,
                });
            }
        }
        from = idx + 1;
    }
    None
}

/// 结束标记的位置和退出码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndMarker {
    /// 标记在文本里的字节下标。
    pub start: usize,
    pub exit_code: i32,
}

/// 从终端输出里截取命令本身的输出：开始标记之后、结束标记之前，去 ANSI、去空行。
///
/// `raw` 是自清零以来该会话的原始输出（已带结束标记）。取不到开始标记就从开头算。
pub fn extract_output(raw: &str, marker: &str) -> String {
    let end = find_end_marker(raw, marker)
        .map(|m| m.start)
        .unwrap_or(raw.len());
    let before = &raw[..end];
    // 从最后一个开始标记之后截取（回显的包装脚本里也有 `_S`）。
    let start_pattern = format!("{marker}{START_MARKER_SUFFIX}");
    let start = before.rfind(&start_pattern);
    let body = match start {
        Some(pos) => {
            // 跳过开始标记所在的那一整行。
            let after = &before[pos..];
            match after.find('\n') {
                Some(newline) => &after[newline + 1..],
                None => "",
            }
        }
        None => before,
    };
    strip_ansi(body).trim().to_string()
}

/// 去掉 ANSI 转义序列（CSI / OSC），保留可读文本。
pub fn strip_ansi(text: &str) -> String {
    let bytes = strip_ansi_escapes::strip(text.as_bytes());
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 边喂终端字节边跟踪备用屏幕。
///
/// 进入备用屏幕的常见写法：`\x1b[?1049h`（vim / top / less），也有 `\x1b[?47h` /
/// `\x1b[?1047h`。离开用对应的 `l`。处在备用屏幕里时不能往终端注入命令。
///
/// 状态由**最后出现**的序列决定，不是"找没找到"：退出 `less` 的离开序列和紧跟
/// 进入 `vim` 的进入序列可能落在同一段（或相邻两段）输出里，按"先找进入再找离开"
/// 会拿旧的离开序列把 vim 的开着状态改错。
#[derive(Debug, Default, Clone)]
pub struct TerminalScan {
    alt_screen: bool,
    /// 尚未识别过的尾巴：识别过的字节直接丢掉；跨块拼接只需要
    /// 「最长序列 − 1」个字节（可能是一个尚未拼完的序列前缀）。
    tail: Vec<u8>,
}

impl TerminalScan {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段终端输出。
    pub fn push(&mut self, bytes: &[u8]) {
        self.tail.extend_from_slice(bytes);
        // 找到最后一个序列，连它的结束位置一起记下来。
        let mut last: Option<(usize, bool, usize)> = None;
        for at in 0..self.tail.len() {
            for seq in ALT_SCREEN_ENTER {
                if matches_at(&self.tail, at, seq) && last.is_none_or(|(p, _, _)| at >= p) {
                    last = Some((at, true, seq.len()));
                }
            }
            for seq in ALT_SCREEN_LEAVE {
                if matches_at(&self.tail, at, seq) && last.is_none_or(|(p, _, _)| at >= p) {
                    last = Some((at, false, seq.len()));
                }
            }
        }
        if let Some((at, enter, len)) = last {
            self.alt_screen = enter;
            // 这个序列（含它之前的所有字节）已经识别完，之后不再重复识别。
            self.tail.drain(..at + len);
        }
        // 剩下的都没识别过：只有末尾「最长序列 − 1」个字节可能是拼了一半的前缀。
        let keep = ALT_SCREEN_MAX_LEN.saturating_sub(1);
        if self.tail.len() > keep {
            let cut = self.tail.len() - keep;
            self.tail.drain(..cut);
        }
    }

    /// 现在是否处在备用屏幕（全屏程序里）。
    pub fn is_alt_screen(&self) -> bool {
        self.alt_screen
    }
}

const ALT_SCREEN_ENTER: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
const ALT_SCREEN_LEAVE: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];
/// 最长的一个序列 `\x1b[?1049h` 的长度（8 字节）。
const ALT_SCREEN_MAX_LEN: usize = 8;

fn matches_at(haystack: &[u8], at: usize, needle: &[u8]) -> bool {
    haystack.len() >= at + needle.len() && haystack[at..at + needle.len()] == *needle
}

/// 会话 shell 的类型：[`ShellKind`]。
///
/// 定义在 [`crate::ai::shell_kind`]（远端探测也放那里）；这里转发一下，
/// 让 `pty` 的调用方（含测试）不换路径。
pub use crate::ai::shell_kind::ShellKind;

/// 从终端尾部文本判断"停在提示符上、没有输入到一半的命令"。
///
/// 启发式：看最后一行（光标所在行）。shell 跑完命令会打印提示符但不换行，所以尾部
/// 通常以提示符结尾（`$`、`#`、`%`、`>`，可能后面跟一个空格）；如果用户输入了一半，
/// 这一行是"提示符 + 半截命令"。这里只做保守判断：最后一行去掉 ANSI 后以提示符符号
/// 结尾（允许结尾有一个空格）就认为空闲。
///
/// 这是个启发式，只用来决定"要不要就地执行"；判定为忙就走后台通道，不会出错。
pub fn looks_idle(tail: &str) -> bool {
    let text = strip_ansi(tail);
    let text = text.replace('\r', "\n");
    // 以换行结尾时取它前面那一行（末尾的空行不是提示符）。
    let candidate = if text.ends_with('\n') {
        text.trim_end_matches('\n')
            .rsplit('\n')
            .next()
            .unwrap_or("")
    } else {
        text.rsplit('\n').next().unwrap_or("")
    };
    let candidate = candidate.trim_end_matches(' ');
    if candidate.is_empty() {
        return false;
    }
    candidate.ends_with('$')
        || candidate.ends_with('#')
        || candidate.ends_with('%')
        || candidate.ends_with('>')
}

/// 一个会话在终端里执行 AI 命令所需的状态（AI-2c）。
///
/// 由会话任务在每段输出上旁路喂 [`TerminalState::feed`]；工具桥读
/// [`TerminalState::placement`] 判断能不能就地执行。AI 命令运行期间
/// [`TerminalState::set_busy`]，避免两条 AI 命令叠在同一个终端里。
#[derive(Debug)]
pub struct TerminalState {
    scan: std::sync::Mutex<TerminalScan>,
    tail: std::sync::Mutex<String>,
    shell_kind: std::sync::Mutex<ShellKind>,
    ai_running: std::sync::atomic::AtomicBool,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalState {
    /// 新建：shell 类型**未知**——探测出来之前一律不就地执行（AI-2d 1.3）。
    ///
    /// 曾经默认当 POSIX 试，结果是 PowerShell / cmd / fish 的会话被直接敲进包装
    /// 脚本，一直等不到结束标记。
    pub fn new() -> Self {
        Self {
            scan: std::sync::Mutex::new(TerminalScan::new()),
            tail: std::sync::Mutex::new(String::new()),
            shell_kind: std::sync::Mutex::new(ShellKind::Unknown),
            ai_running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 喂一段终端输出（会话任务在转发路径上旁路调用）。
    pub fn feed(&self, bytes: &[u8]) {
        if let Ok(mut scan) = self.scan.lock() {
            scan.push(bytes);
        }
        if let Ok(mut tail) = self.tail.lock() {
            tail.push_str(&String::from_utf8_lossy(bytes));
            truncate_front(&mut tail, TAIL_KEEP_BYTES);
        }
    }

    pub fn is_alt_screen(&self) -> bool {
        self.scan.lock().map(|s| s.is_alt_screen()).unwrap_or(false)
    }

    /// shell 空闲（不在备用屏幕、停在提示符上）。
    pub fn is_idle(&self) -> bool {
        if self.is_alt_screen() {
            return false;
        }
        let tail = self.tail.lock().map(|t| t.clone()).unwrap_or_default();
        looks_idle(&tail)
    }

    pub fn shell_kind(&self) -> ShellKind {
        self.shell_kind
            .lock()
            .map(|k| *k)
            .unwrap_or(ShellKind::Other)
    }

    /// 记下探测出来的 shell 类型（每个会话探测一次；探测失败也记 `Other`，
    /// 免得每次执行都重复探测）。
    pub fn set_shell_kind(&self, kind: ShellKind) {
        if let Ok(mut slot) = self.shell_kind.lock() {
            *slot = kind;
        }
    }

    /// 标记 AI 命令开始 / 结束。
    ///
    /// 新代码用 [`TerminalState::try_acquire`] / [`TerminalState::release`]；这个
    /// 保留给测试和"已知没有并发"的场景。
    pub fn set_busy(&self, busy: bool) {
        self.ai_running
            .store(busy, std::sync::atomic::Ordering::SeqCst);
    }

    /// 原子地检查并抢占这个终端：能就地执行就占位并返回 `Ok`，否则返回原因。
    ///
    /// 检查和占位必须一次做完。分成"先 [`TerminalState::placement`] 检查、后
    /// [`TerminalState::set_busy`] 占位"两步时，两个对话会挤在中间同时通过检查，
    /// 两条 AI 命令一起写进同一个终端。抢到的调用方**必须**在命令结束（拿到结束
    /// 标记、超时、会话断开）之后调用 [`TerminalState::release`]。
    pub fn try_acquire(&self) -> Result<(), String> {
        if self
            .ai_running
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return Err("这个终端上已有一条 AI 命令在运行".to_string());
        }
        // 已经占位：下面的检查失败要把占位还回去，否则终端永远"忙"。
        match self.executable_reason() {
            None => Ok(()),
            Some(reason) => {
                self.release();
                Err(reason)
            }
        }
    }

    /// 释放终端占位（AI 命令结束）。只有抢到的调用方才能调。
    pub fn release(&self) {
        self.ai_running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// 不占位地检查能不能就地执行（诊断 / 测试用）。
    pub fn placement(&self) -> Result<(), String> {
        if self.ai_running.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("这个终端上已有一条 AI 命令在运行".to_string());
        }
        match self.executable_reason() {
            None => Ok(()),
            Some(reason) => Err(reason),
        }
    }

    /// shell 空闲、不在全屏程序里、shell 类型能跑 POSIX 包装；否则给出原因。
    fn executable_reason(&self) -> Option<String> {
        match self.shell_kind() {
            ShellKind::Posix => {}
            ShellKind::Unknown => {
                return Some("还没探测出远端 shell 的类型，已改用后台通道".to_string())
            }
            ShellKind::Fish => {
                return Some("fish 暂不支持在终端里执行，已改用后台通道".to_string())
            }
            ShellKind::Other => return Some("认不出这个 shell 的类型，已改用后台通道".to_string()),
        }
        if self.is_alt_screen() {
            return Some("终端里开着全屏程序（vim / top 之类），已改用后台通道".to_string());
        }
        if !self.is_idle() {
            return Some("终端正忙（有命令在跑或者输入到一半），已改用后台通道".to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_random_and_prefixed() {
        let a = new_marker();
        let b = new_marker();
        assert!(a.starts_with("__ZSSH_") && a.ends_with("__"));
        assert_ne!(a, b, "每次都要不一样");
    }

    #[test]
    fn wrapper_starts_with_a_space_and_carries_the_marker() {
        let wrapper = build_wrapped_command("echo hi", "__M__");
        assert!(wrapper.starts_with(' '), "前面要有空格，才不进历史");
        assert!(wrapper.contains("__M___S"));
        assert!(wrapper.contains("trap ':' INT"), "打断要靠 trap：{wrapper}");
        assert!(wrapper.contains("__M___E:"));
        assert!(wrapper.contains("echo hi"));
    }

    #[test]
    fn wrapper_keeps_quotes_pipes_and_ampersands() {
        let command = r#"echo 'a b' | grep a && echo "c|d""#;
        let wrapper = build_wrapped_command(command, "__M__");
        // 命令整体被单引号包住，内部的引号用 '\'' 转义，所以管道 / && 不会在包装层被解释。
        assert!(wrapper.contains(r#"'a b'"#));
        assert!(wrapper.contains(r#""c|d""#));
        assert!(
            wrapper.contains(r#"'\''a b'\''"#),
            "单引号要转义：{wrapper}"
        );
    }

    #[test]
    fn wrapper_keeps_multiline_and_dollar_question() {
        let command = "echo one\necho two; echo $?";
        let wrapper = build_wrapped_command(command, "__M__");
        // 短命令整段单引号包住，换行和 $? 原样保留，交给 eval 时展开。
        assert!(wrapper.contains("echo one\necho two; echo $?"));
    }

    #[test]
    fn long_command_uses_chunked_printf() {
        let command = "echo ".to_string() + &"x".repeat(400);
        let wrapper = build_wrapped_command(&command, "__M__");
        assert!(
            wrapper.contains("_cmd=$(printf"),
            "长命令要分段拼：{wrapper}"
        );
    }

    #[test]
    fn echoed_wrapper_is_not_an_end_marker() {
        // 回显里那一份是 `<m>_E:'"$<m>_rc"`：`_E:` 后面紧跟的是引号而不是数字。
        // AI-2e 第 2 节允许行内标记之后，"后面必须是数字"这条规则是唯一的防线，
        // 用真实形状的回显锁住它。
        let marker = "__M__";
        let echoed = format!(
            " {marker}=0; {{ printf '\\n%s\\n' '{marker}{START_MARKER_SUFFIX}'; trap ':' INT; \
             {marker}_cmd='printf abc'; eval \"${marker}_cmd\"; {marker}_rc=$?; trap - INT; \
             printf '%s\\n' '{marker}{END_MARKER_SUFFIX}'\"${marker}_rc\"; }}"
        );
        assert!(find_end_marker(&echoed, marker).is_none(), "回显不算完成");
    }

    #[test]
    fn finds_inline_end_marker_after_output_without_newline() {
        // `printf abc`：输出没有换行结尾，结束标记紧跟在输出后面（不在行首）。
        // 修复前这种形状要等到超时（AI-2d 报告遗留问题 1，AI-2e 第 2 节）。
        let marker = "__M__";
        let raw = format!(
            " {marker}=0; {{ printf '\\n%s\\n' '{marker}_S'; … }}\r\n{marker}_S\r\nabc{marker}{END_MARKER_SUFFIX}0\r\n"
        );
        let found = find_end_marker(&raw, marker).expect("行内标记要能找到");
        assert_eq!(found.exit_code, 0);
        assert_eq!(
            &raw[found.start..found.start + marker.len()],
            marker,
            "start 要指向标记本身（这样 extract_output 才能把 abc 留住）"
        );
        assert_eq!(extract_output(&raw, marker), "abc");
    }

    #[test]
    fn parses_exit_code_from_end_marker() {
        let marker = "__M__";
        let text = format!("output here\n{marker}{END_MARKER_SUFFIX}0\n");
        let found = find_end_marker(&text, marker).expect("要找到结束标记");
        assert_eq!(found.exit_code, 0);

        let text = format!("\n{marker}{END_MARKER_SUFFIX}137\r\n");
        assert_eq!(find_end_marker(&text, marker).unwrap().exit_code, 137);
    }

    #[test]
    fn end_marker_split_across_chunks_still_parses_when_whole() {
        // 解析是作用在累积文本上；这里模拟两段拼起来之后。
        let marker = "__M__";
        let part1 = "line one\n";
        let part2 = format!("{marker}{END_MARKER_SUFFIX}3\n");
        assert_eq!(
            find_end_marker(&format!("{part1}{part2}"), marker)
                .unwrap()
                .exit_code,
            3
        );
    }

    #[test]
    fn output_looks_like_a_marker_but_is_not() {
        // 用户输出里恰好有类似标记的文字（少了 `_E:` 前缀）不能被误认。
        let text = "the marker __M___E is not a real end marker\n";
        assert!(find_end_marker(text, "__M__").is_none());
        // 有 `_E:` 但后面不是数字也不算。
        let text = "prefix __M___E:abc\n";
        assert!(find_end_marker(text, "__M__").is_none());
    }

    #[test]
    fn extracts_output_between_markers_and_strips_ansi() {
        let marker = "__M__";
        let raw = format!(
            " echo hi\r\n{marker}_S\r\n\x1b[31mhello\x1b[0m\r\n{marker}{END_MARKER_SUFFIX}0\r\n"
        );
        let out = extract_output(&raw, marker);
        assert_eq!(out, "hello", "要取标记之间、去掉颜色：{out:?}");
    }

    #[test]
    fn extracts_empty_output_between_markers() {
        let marker = "__M__";
        let raw = format!("{marker}_S\n{marker}{END_MARKER_SUFFIX}1\n");
        assert_eq!(extract_output(&raw, marker), "");
    }

    #[test]
    fn terminal_scan_detects_alt_screen() {
        let mut scan = TerminalScan::new();
        scan.push(b"normal output\r\n$ ");
        assert!(!scan.is_alt_screen());
        scan.push(b"\x1b[?1049h");
        assert!(scan.is_alt_screen(), "进了备用屏幕");
        scan.push(b"\x1b[?1049l");
        assert!(!scan.is_alt_screen(), "退出备用屏幕");
    }

    #[test]
    fn terminal_scan_handles_split_escape_sequence() {
        let mut scan = TerminalScan::new();
        scan.push(b"before\x1b[?10");
        scan.push(b"49h after");
        assert!(scan.is_alt_screen());
    }

    #[test]
    fn alt_screen_is_decided_by_the_last_sequence_in_one_chunk() {
        // 退出 less 的离开序列和进入 vim 的进入序列挨在一起：后出现的说了算。
        let mut scan = TerminalScan::new();
        scan.push(b"\x1b[?1049l\x1b[?1049h");
        assert!(scan.is_alt_screen(), "离开在前、进入在后：在备用屏幕");

        let mut scan = TerminalScan::new();
        scan.push(b"\x1b[?1049h\x1b[?1049l");
        assert!(!scan.is_alt_screen(), "进入在前、离开在后：不在备用屏幕");
    }

    #[test]
    fn alt_screen_is_decided_by_the_last_sequence_across_chunks() {
        // 两段输出各带一个序列（AI-2d 1.2 的场景：旧序列留在尾巴里被重复识别）。
        let mut scan = TerminalScan::new();
        scan.push(b"less exited\r\n\x1b[?1049l");
        scan.push(b"vim starting\r\n\x1b[?1049h");
        assert!(scan.is_alt_screen(), "后到的进入序列要生效");

        let mut scan = TerminalScan::new();
        scan.push(b"vim exiting\r\n\x1b[?1049h");
        scan.push(b"back in the shell\r\n\x1b[?1049l");
        assert!(!scan.is_alt_screen(), "后到的离开序列要生效");
    }

    #[test]
    fn identified_alt_screen_bytes_are_not_rescanned() {
        // 进入序列识别过以后，即使尾巴里还留着它，也不该被后续的小块输出再次"发现"。
        let mut scan = TerminalScan::new();
        scan.push(b"\x1b[?1049h");
        assert!(scan.is_alt_screen());
        // 小块输出（不足以把旧序列挤出窗口）+ 一个离开序列：结果必须是不在备用屏幕。
        scan.push(b"\x1b[?47l$ ");
        assert!(!scan.is_alt_screen(), "旧的进入序列不能翻案");

        // 离开序列同理：识别过之后不能把后来的状态改回去。
        let mut scan = TerminalScan::new();
        scan.push(b"\x1b[?47l");
        assert!(!scan.is_alt_screen());
        scan.push(b"\x1b[?1049h");
        assert!(scan.is_alt_screen(), "新的进入序列要生效");
    }

    #[test]
    fn shell_kind_defaults_to_unknown_until_probed() {
        // AI-2d 1.3：探测之前不是 Posix，不能就地执行。
        let state = TerminalState::new();
        assert_eq!(state.shell_kind(), ShellKind::Unknown);
        state.feed(b"$ ");
        let reason = state.try_acquire().expect_err("没探测过就不能就地执行");
        assert!(
            reason.contains("还没探测出"),
            "原因要说明是没探测：{reason}"
        );
        // 探测结果一记，就能正常执行了。
        state.set_shell_kind(ShellKind::Posix);
        assert!(state.try_acquire().is_ok());
        state.release();
    }

    #[test]
    fn looks_idle_when_stopped_at_a_prompt() {
        assert!(looks_idle("$ "));
        assert!(looks_idle("root@web-1:~# "));
        assert!(looks_idle("output line\n$ "));
        assert!(looks_idle("\x1b[32m$\x1b[0m "));
    }

    #[test]
    fn not_idle_when_a_command_is_half_typed() {
        assert!(!looks_idle("$ ls -la"));
        assert!(!looks_idle(""));
        assert!(!looks_idle("still running...\n"));
    }

    #[test]
    fn truncates_the_tail_on_a_char_boundary() {
        // 截断位置落在 3 字节字符内部的每一种偏移都要安全，内容要是原串的后缀。
        for offset in 0..4 {
            let mut text = "a".repeat(offset);
            let body = "中文".repeat(3000);
            text.push_str(&body);
            let full = text.clone();
            truncate_front(&mut text, 4096);
            assert!(text.len() <= 4096, "超过上限：{}", text.len());
            assert!(
                full.ends_with(&text),
                "截出来的必须是后缀（offset={offset}）"
            );
            assert!(text.ends_with("中文"), "尾部内容要完整（offset={offset}）");
        }
    }

    #[test]
    fn truncate_front_keeps_short_text_untouched() {
        let mut text = "只有中文".to_string();
        truncate_front(&mut text, 4096);
        assert_eq!(text, "只有中文");
    }

    #[test]
    fn feeding_megabytes_of_chinese_does_not_panic() {
        // AI-2d 1.1：以前按字节下标切 tail，中文输出累计超过 4KB 就 panic。
        let state = TerminalState::new();
        for _ in 0..40 {
            state.feed("中文输出测试".repeat(100).as_bytes());
        }
        // 截断之后尾部还能正常工作：喂一个提示符就判定空闲。
        state.feed(b"\r\n$ ");
        assert!(state.is_idle());
    }

    #[test]
    fn feeding_split_multibyte_sequences_does_not_panic() {
        // 多字节字符被切在两段输出里（UTF-8 序列跨块）也不能崩。
        let state = TerminalState::new();
        let bytes = "中文".as_bytes();
        for _ in 0..1500 {
            state.feed(&bytes[..1]);
            state.feed(&bytes[1..]);
        }
        state.feed(b"\r\n$ ");
        assert!(state.is_idle());
    }

    #[test]
    fn only_one_thread_wins_the_acquire() {
        // AI-2d 1.5：检查和占位要原子；多个对话同时抢只能有一个成功。
        let state = std::sync::Arc::new(TerminalState::new());
        state.set_shell_kind(ShellKind::Posix); // 探测过，其他检查也都过
        state.feed(b"$ ");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = std::sync::Arc::clone(&state);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                state.try_acquire().is_ok()
            }));
        }
        let wins = handles
            .into_iter()
            .map(|h| h.join().expect("线程不能崩"))
            .filter(|won| *won)
            .count();
        assert_eq!(wins, 1, "同一个终端只能有一条 AI 命令抢到");

        // 释放之后可以再抢。
        state.release();
        assert!(state.try_acquire().is_ok());
        state.release();
    }

    #[test]
    fn failed_acquire_releases_the_reservation() {
        // 检查失败（终端忙）时不能把占位留在手里。
        let state = TerminalState::new();
        state.set_shell_kind(ShellKind::Posix);
        state.feed(b"$ ");
        state.feed(b"ls -l"); // 输入到一半：忙
        assert!(state.try_acquire().is_err());
        // 占位要还回去了：界面回到提示符后还能再抢。
        state.feed(b"\r\n$ ");
        assert!(state.try_acquire().is_ok(), "失败的那次不能把终端占死");
        state.release();
    }

    #[test]
    fn terminal_state_allows_execution_only_when_idle() {
        let state = TerminalState::new();
        state.set_shell_kind(ShellKind::Posix);
        // 还没输出：不算空闲（保守）。
        assert!(state.placement().is_err(), "没看到提示符前不就地执行");
        state.feed(b"$ ");
        assert!(state.placement().is_ok(), "停在提示符上可以就地执行");

        // 输入到一半：忙。
        state.feed(b"ls -l");
        assert!(state.placement().is_err());

        // 全屏程序：忙（备用屏幕）。
        state.feed(b"\x1b[?1049h");
        assert!(state.placement().is_err(), "备用屏幕里不就地执行");

        // 有 AI 命令在跑：忙。
        state.feed(b"\x1b[?1049l$ ");
        state.set_busy(true);
        assert!(state.placement().is_err(), "同一终端上不叠两条 AI 命令");
        state.set_busy(false);
        state.feed(b"$ ");
        assert!(state.placement().is_ok());
    }
}
