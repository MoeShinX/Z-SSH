// Adapted from Netcatty electron/preload.cjs (MCP marker line filtering and the
// delayed flush fallback, filterMcpChunk / flushMcpBufferedOutput) and
// electron/bridges/ai/shellUtils.cjs (formatSyntheticEcho), rewritten in Rust
// for Z-SSH.
//! 终端显示过滤（AI-2d 第 2 节，AI-2e 第 1 节修了"不退出"）：AI 命令的包装脚本
//! 不进用户的眼睛。
//!
//! 用户要看到的是"AI 直接在我的终端里操作"：提示符 + 命令原文（合成回显）、命令
//! 输出、新提示符。包装脚本的回显、开始标记行、结束标记行都不显示。
//!
//! 过滤器**只作用在发给界面的显示数据**上；环形缓冲（`read_terminal`、结束标记
//! 解析）和 [`TerminalState`](crate::ai::pty::TerminalState) 拿的都是原始数据。
//!
//! 生命周期（AI-2e 第 1 节：用完必须退出，不能拖慢用户打字）：
//!
//! - **当前命令的结束标记行**（`<marker>_E:`）处理完，过滤器立刻退出，回到直通
//!   （`push` 原样返回、不压任何东西）；
//! - **超时返回的命令**（命令还在终端里跑）：调用方 [`DisplayFilter::release`]，
//!   标记转成"待处理"，之后只做**轻量过滤**——只丢包含待处理标记的行，压尾巴按
//!   "末尾是某个待处理标记完整写法 `<标记>_E:<数字>` 的前缀"判断（见
//!   [`tail_may_become_marker_line`]；单个 `_`、`__`、`__Z` 不压）。
//!   没有待处理标记时完全直通。待处理标记在它出现、会话断开或
//!   [`PENDING_MAX_AGE`]（和 `watch_late_finish` 一致）之后清掉；
//! - "没有可见字符就压住"（丢空行用）只在吃完回显、到结束标记之间生效。
//!
//! 兜底（宁可多显示，绝不吞用户的输出）：
//! - 回显被关掉 / shell 行为和预期不同（包装脚本自己的输出先冒出来了）：退化成
//!   按行过滤（丢标记行），其余原样放行；
//! - 可疑的尾巴（可能是标记的一部分）最多压 [`HOLD_MAX_AGE`] / [`HOLD_MAX_BYTES`]，
//!   超了原样放行；
//! - 用户输出里出现**别的**标记样文字不动它（只丢我们自己用过的标记）。

use std::time::{Duration, Instant};

/// 可疑尾巴最多压多久（到点放行）。
pub const HOLD_MAX_AGE: Duration = Duration::from_secs(2);
/// 可疑尾巴最多压多少字节（超了放行）。
pub const HOLD_MAX_BYTES: usize = 4096;
/// 待处理标记最多留多久：和 `src-tauri` 里 `watch_late_finish` 的观察上限一致。
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// 等包装脚本回显的字节预算：超了就当"回显被关掉"，退化成按行过滤。
const ECHO_BUDGET_BYTES: usize = 8 * 1024;
/// 合成回显之后最多丢掉几个空行（shell 执行命令时留下的换行）。
const BLANK_LINES_AFTER_ECHO: usize = 2;
/// 标记的前缀（`__ZSSH_`）：轻量过滤判断"尾巴像不像标记行"用。
const MARKER_PREFIX: &str = "__ZSSH_";
/// 结束标记里的后缀（`<marker>_E:`）。
const END_MARKER_SUFFIX: &str = "_E:";

/// 包装脚本里的命令原文转成合成回显（换行折成 `\r\n`，末尾补一个）。
///
/// 和 Netcatty 的 `formatSyntheticEcho` 一样：xterm 里裸 `\n` 是"下移不回车"，
/// 多行命令会走成阶梯。
pub fn synthetic_echo(command: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(command.len() + 2);
    for ch in command.chars() {
        match ch {
            '\n' => out.extend_from_slice(b"\r\n"),
            '\r' => {}
            other => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// 过滤结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DisplayOutput {
    /// 要显示给用户的字节（可能为空：整段都被过滤掉了）。
    pub data: Vec<u8>,
    /// 被过滤掉的字节数。上层要替界面把这部分 ack 掉（界面只能 ack 它收到的
    /// 字节，被吞掉的没人 ack 就会把流控门卡住）。
    pub suppressed: usize,
}

/// 显示过滤器。
#[derive(Debug, Default)]
pub struct DisplayFilter {
    /// 当前正在跑的命令（见到结束标记就退出）。
    active: Option<Active>,
    /// 超时返回、结束标记还没出现的命令：迟到的那一行还要丢掉。
    pending: Vec<Pending>,
    /// 压着还没决定的尾巴（两种模式共用）。
    hold: Vec<u8>,
    hold_since: Option<Instant>,
}

#[derive(Debug)]
struct Active {
    /// 这条命令的标记。
    marker: String,
    /// 包装脚本原文（去掉清输入前缀和末尾换行）——回显要逐字节匹配它。
    echo: Vec<u8>,
    /// 合成回显（提示符后面显示的命令原文）。
    synthetic: Vec<u8>,
    matched: usize,
    phase: Phase,
    /// 进这个状态之后处理过的字节数（兜底预算）。
    budget: usize,
    /// 合成回显之后还允许丢几个空行。
    blanks_left: usize,
}

/// 一条超时命令的待处理标记。
#[derive(Debug)]
struct Pending {
    marker: String,
    since: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 还没匹配到包装脚本回显的开头。
    SeekEcho,
    /// 正在吃回显（可能被行编辑器按终端宽度折行）。
    EatEcho,
    /// 回显吃完了：正常放行，只丢标记行。
    Streaming,
}

impl DisplayFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记下一条马上要写进终端的 AI 命令（**写在写之前**）。
    ///
    /// `payload` 是整行原文（含清输入前缀，末尾可能带换行）；`command` 是用户要
    /// 看到的命令原文。返回值是上一条命令压着的尾巴（要先显示出去，不能吞）。
    pub fn arm(&mut self, marker: &str, payload: &str, command: &str) -> DisplayOutput {
        self.arm_at(marker, payload, command, Instant::now())
    }

    /// 可测的 `arm`：时间由调用方给。
    pub fn arm_at(
        &mut self,
        marker: &str,
        payload: &str,
        command: &str,
        now: Instant,
    ) -> DisplayOutput {
        let mut out = DisplayOutput::default();
        // 上一条命令还没结束（比如它超时了、AI 又发了一条）：它的标记转成"待处理"，
        // 迟到的结束标记行还要丢掉。
        if let Some(old) = self.active.take() {
            self.pending.push(Pending {
                marker: old.marker,
                since: now,
            });
        }
        // 压着的尾巴先放行（不能吞）。
        out.data = std::mem::take(&mut self.hold);
        self.hold_since = None;
        let echo = payload
            .trim_start_matches(['\x0b', '\x15'])
            .trim_end_matches(['\n', '\r']);
        self.active = Some(Active {
            marker: marker.to_string(),
            echo: echo.as_bytes().to_vec(),
            synthetic: synthetic_echo(command),
            matched: 0,
            phase: Phase::SeekEcho,
            budget: 0,
            blanks_left: BLANK_LINES_AFTER_ECHO,
        });
        out
    }

    /// 这条命令超时返回了（命令还在终端里跑，结束标记以后才出现）。
    ///
    /// 过滤器退出完整过滤（不再压用户的输入），标记转成"待处理"，之后只做轻量
    /// 过滤；压着的尾巴立即放行。调用方把返回值发给界面。
    pub fn release(&mut self, marker: &str) -> DisplayOutput {
        self.release_at(marker, Instant::now())
    }

    /// 可测的 `release`：时间由调用方给。
    pub fn release_at(&mut self, marker: &str, now: Instant) -> DisplayOutput {
        let mut out = DisplayOutput::default();
        if self.active.as_ref().is_some_and(|a| a.marker == marker) {
            if let Some(old) = self.active.take() {
                self.pending.push(Pending {
                    marker: old.marker,
                    since: now,
                });
            }
            out.data = std::mem::take(&mut self.hold);
            self.hold_since = None;
        }
        out
    }

    /// 喂一段终端原始输出，返回要显示的数据。
    pub fn push(&mut self, bytes: &[u8]) -> DisplayOutput {
        self.push_at(bytes, Instant::now())
    }

    /// 时间兜底：压着的尾巴等太久了就放行（会话任务定时调用）。
    pub fn poll_at(&mut self, now: Instant) -> DisplayOutput {
        self.prune_pending(now);
        let mut out = DisplayOutput::default();
        if let Some(since) = self.hold_since {
            if now.saturating_duration_since(since) >= HOLD_MAX_AGE {
                out.data = std::mem::take(&mut self.hold);
                self.hold_since = None;
            }
        }
        out
    }

    /// 会话结束：压着的数据全部放行，待处理标记清掉。
    pub fn flush(&mut self) -> DisplayOutput {
        let out = DisplayOutput {
            data: std::mem::take(&mut self.hold),
            ..DisplayOutput::default()
        };
        self.hold_since = None;
        self.pending.clear();
        out
    }

    /// 可测的 `push`：时间由调用方给。
    pub fn push_at(&mut self, bytes: &[u8], now: Instant) -> DisplayOutput {
        self.prune_pending(now);
        let mut out = DisplayOutput::default();

        // 压着的尾巴 + 新数据一起处理。
        let mut input = std::mem::take(&mut self.hold);
        let held_since = self.hold_since.take();
        input.extend_from_slice(bytes);
        if let Some(since) = held_since {
            if now.saturating_duration_since(since) >= HOLD_MAX_AGE {
                // 压太久了：整段放行（兜底）。
                out.data.extend_from_slice(&input);
                return out;
            }
        }

        let DisplayFilter {
            active,
            pending,
            hold,
            hold_since,
        } = self;
        let mut pos = 0usize;
        while pos < input.len() {
            let rest = &input[pos..];
            let mut finished = false;
            match active.as_mut() {
                // 没有正在跑的 AI 命令：直通；有待处理标记时做轻量过滤。
                None => {
                    if pending.is_empty() {
                        out.data.extend_from_slice(rest);
                        break;
                    }
                    let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
                        // 没有完整行：只有"还可能长成待处理标记的结束标记行"才压住，
                        // 用户敲的单个 `_`、`__`、`__Z` 一律立即放行。
                        if tail_may_become_marker_line(rest, pending) {
                            *hold = rest.to_vec();
                            *hold_since = Some(now);
                        } else {
                            out.data.extend_from_slice(rest);
                        }
                        break;
                    };
                    let line = &rest[..=nl];
                    pos += line.len();
                    let mut satisfied = Vec::new();
                    let mut from: Option<usize> = None;
                    for (index, entry) in pending.iter().enumerate() {
                        if let Some(at) = find(line, entry.marker.as_bytes()) {
                            if from.is_none_or(|current| at < current) {
                                from = Some(at);
                            }
                            if line_ends_command(line, &entry.marker) {
                                satisfied.push(index);
                            }
                        }
                    }
                    match from {
                        Some(at) => emit_marker_line(line, at, &mut out),
                        None => out.data.extend_from_slice(line),
                    }
                    for index in satisfied.into_iter().rev() {
                        pending.remove(index);
                    }
                }
                Some(act) => match act.phase {
                    Phase::SeekEcho => {
                        // 回显开头（回显本身也带着标记；顺序反了会把回显当成
                        // "没有回显"的按行模式，合成回显就丢了）。
                        let needle_len = act.echo.len().min(12);
                        let needle = act.echo[..needle_len].to_vec();
                        if let Some(idx) = find(rest, &needle) {
                            // 回显前面那些杂音（readline 清空输入时响的铃 \x07
                            // 之类）不发给界面——不然每条 AI 命令都会响一下。
                            out.suppressed += append_without_bel(&mut out.data, &rest[..idx]);
                            act.matched = needle.len();
                            act.phase = Phase::EatEcho;
                            pos += idx + needle.len();
                            continue;
                        }
                        // 回显被关掉 / shell 行为不同：包装脚本自己的输出先到了
                        // （开始标记行以标记开头）→ 直接按行过滤。
                        //
                        // 不能用"任意位置出现标记"判断：回显的开头本身是
                        // " __M__"，`contains` 会踩这个坑把回显当成输出。
                        if line_starts_with(rest, act.marker.as_bytes()) {
                            act.phase = Phase::Streaming;
                            continue;
                        }
                        // 把可能是回显开头的前缀压住，其余放行。
                        let keep = trailing_prefix_len(rest, &needle);
                        let emit = rest.len() - keep;
                        out.suppressed += append_without_bel(&mut out.data, &rest[..emit]);
                        if keep > 0 {
                            *hold = rest[emit..].to_vec();
                            *hold_since = Some(now);
                        }
                        break;
                    }
                    Phase::EatEcho => {
                        let echo_len = act.echo.len();
                        let mut i = 0usize;
                        while i < rest.len() && act.matched < echo_len {
                            let byte = rest[i];
                            if byte == act.echo[act.matched] {
                                act.matched += 1;
                                out.suppressed += 1;
                                i += 1;
                                continue;
                            }
                            if is_wrap_noise(byte) {
                                // 折行插入的换行、回显边上的控制字符：一起吃掉。
                                out.suppressed += 1;
                                i += 1;
                                continue;
                            }
                            // 不是回显的内容（转义序列、用户敲的键）：照常显示，
                            // 终端的模式切换不能吞。
                            out.data.push(byte);
                            i += 1;
                        }
                        act.budget += i;
                        pos += i;
                        if act.matched >= echo_len {
                            // 回显吃完了：发合成回显，之后正常放行。
                            act.phase = Phase::Streaming;
                            out.data.extend_from_slice(&act.synthetic);
                            continue;
                        }
                        if act.budget > echo_len + ECHO_BUDGET_BYTES {
                            // 一直匹配不上：退化成按行过滤，不再吞字节。
                            act.phase = Phase::Streaming;
                        }
                        break;
                    }
                    Phase::Streaming => {
                        let marker = act.marker.clone();
                        let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
                            // 没有完整行。压住两种情况：可能是标记的一部分，或者这一行
                            // 到目前为止没有任何要显示的东西（`\r` 先到、`\n` 在下一段
                            // ——要和整段一起按空行处理，不然块边界会改变显示结果）。
                            //
                            // 这条"没有可见字符就压住"只在**当前命令的结束标记之前**
                            // 用（AI-2e：命令结束后过滤器就退出了，用户的 vim /
                            // less 光标移动不能被压）。
                            if !has_visible_content(rest)
                                || streaming_tail_is_suspicious(rest, &marker, pending)
                            {
                                *hold = rest.to_vec();
                                *hold_since = Some(now);
                            } else {
                                out.data.extend_from_slice(rest);
                            }
                            break;
                        };
                        let line = &rest[..=nl];
                        pos += line.len();
                        if let Some(at) = find(line, marker.as_bytes()) {
                            // 当前命令的标记行：丢掉（行首整行丢，行内只丢标记）。
                            emit_marker_line(line, at, &mut out);
                            if line_ends_command(line, &marker) {
                                // 结束标记行处理完：退出过滤器，回到直通。
                                finished = true;
                            }
                        } else if let Some(at) = pending_line_hit(line, pending, &mut out) {
                            let _ = at;
                        } else if is_blank_line(line) {
                            if act.blanks_left > 0 {
                                act.blanks_left -= 1;
                                out.suppressed += line.len();
                            } else {
                                out.data.extend_from_slice(line);
                            }
                        } else {
                            act.blanks_left = 0;
                            out.data.extend_from_slice(line);
                        }
                    }
                },
            }
            if finished {
                *active = None;
                // 压着的尾巴立即放行（退出时不压任何东西）。
                if !hold.is_empty() {
                    out.data.extend_from_slice(hold);
                    *hold = Vec::new();
                    *hold_since = None;
                }
            }
        }

        // 没处理完的（压住的尾巴）留到下一段。
        if !hold.is_empty() && hold_since.is_none() {
            *hold_since = Some(now);
        } else if hold.len() > HOLD_MAX_BYTES {
            // 压得太多了：放行（兜底，绝不吞输出）。
            out.data.extend_from_slice(hold);
            *hold = Vec::new();
            *hold_since = None;
        }
        out
    }

    /// 超时的待处理标记到点就清（会话任务定时调 `poll_at` 时顺手做）。
    fn prune_pending(&mut self, now: Instant) {
        self.pending
            .retain(|entry| now.saturating_duration_since(entry.since) < PENDING_MAX_AGE);
    }
}

/// 把标记行按"丢掉标记本身、保留前面的用户输出"处理。
///
/// 标记在行首（正常情况）时 `before` 是空的或只有换行之类，整行丢掉；标记在行
/// 中间（命令输出没有换行时结束标记会跟在输出后面）时，前面的用户输出留着。
fn emit_marker_line(line: &[u8], at: usize, out: &mut DisplayOutput) {
    let before = &line[..at];
    if before.iter().all(|&b| is_wrap_noise(b)) {
        out.suppressed += line.len();
    } else {
        out.data.extend_from_slice(before);
        out.suppressed += line.len() - before.len();
    }
}

/// 行里是不是有**待处理标记**；有就按"丢标记、留前面的输出"处理并返回命中的位置。
///
/// 顺带把"这一行是那个标记的结束标记行"的待处理项清掉。
fn pending_line_hit(
    line: &[u8],
    pending: &mut Vec<Pending>,
    out: &mut DisplayOutput,
) -> Option<usize> {
    let mut satisfied = Vec::new();
    let mut from: Option<usize> = None;
    for (index, entry) in pending.iter().enumerate() {
        if let Some(at) = find(line, entry.marker.as_bytes()) {
            if from.is_none_or(|current| at < current) {
                from = Some(at);
            }
            if line_ends_command(line, &entry.marker) {
                satisfied.push(index);
            }
        }
    }
    let from = from?;
    emit_marker_line(line, from, out);
    for index in satisfied.into_iter().rev() {
        pending.remove(index);
    }
    Some(from)
}

/// `line` 里有没有 `<marker>_E:`（命令的结束标记）。
fn line_ends_command(line: &[u8], marker: &str) -> bool {
    let needle = format!("{marker}{END_MARKER_SUFFIX}");
    find(line, needle.as_bytes()).is_some()
}

/// 折行 / 回显之类可以一起吃掉的控制字节。
fn is_wrap_noise(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n' | 0x07 | 0x0b | 0x15)
}

/// 把 `bytes` 里除了 BEL（`\x07`）之外的内容追加到 `out`，返回丢掉的字节数。
///
/// 回显前面那个 BEL 是 readline 清空输入时响的铃，发给界面就会响一下/闪一下。
fn append_without_bel(out: &mut Vec<u8>, bytes: &[u8]) -> usize {
    let before = out.len();
    out.extend(bytes.iter().copied().filter(|&byte| byte != 0x07));
    bytes.len() - (out.len() - before)
}

fn is_blank_line(line: &[u8]) -> bool {
    !has_visible_content(line)
}

/// 这一段里有没有"要显示的东西"：去掉 ANSI 序列（CSI / OSC）和换行之类之后还有
/// 可见字符就是有。空行判断要连转义序列一起忽略——`\x1b[?2004l\r\r\n` 这种行
/// 在用户眼里是空行（括号粘贴的开关看不见）。
fn has_visible_content(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                // CSI：吃到终止字节（0x40..=0x7e）。
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                // OSC：吃到 BEL 或者 ESC \。
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        if !is_wrap_noise(byte) {
            return true;
        }
        i += 1;
    }
    false
}

/// `haystack` 里有没有一行以 `needle` 开头（行首 = 字符串开头或者紧跟换行 / 回车）。
fn line_starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0usize;
    while let Some(idx) = find(&haystack[from..], needle) {
        let at = from + idx;
        if at == 0 || matches!(haystack[at - 1], b'\n' | b'\r') {
            return true;
        }
        from = at + 1;
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// `haystack` 末尾有多少个字节是 `needle` 的前缀（没拼完的开头）。
fn trailing_prefix_len(haystack: &[u8], needle: &[u8]) -> usize {
    let max = (needle.len() - 1).min(haystack.len());
    (1..=max)
        .rev()
        .find(|&len| haystack.ends_with(&needle[..len]))
        .unwrap_or(0)
}

/// 还没换行的尾巴，会不会长成某个待处理标记的结束标记行（AI-3 第 0 节）。
///
/// AI-2e 的原写法只认"末尾是完整的 `__ZSSH_` 前缀 + 若干十六进制"，标记比较长、
/// 被拆在中间时（尾巴末尾成了 `__`）会被误判成普通文字放行，迟到的结束标记就漏到
/// 屏幕上。改成按**完整写法 `<标记>_E:<数字>` 的前缀**判断：
///
/// - 标记还没收全：尾巴是这个标记自己字节的前缀，且至少到了 `__ZSSH_`（7 个字符；
///   标记本身比这还短时就是整个标记）——单个 `_`、`__`、`__Z` 都不压；
/// - 标记已收全：它后面跟的是 `_E:` + 若干数字（可能只写了一半，数字后可以有一个
///   `\r`），行还没结束，压住等新行到来后整行丢掉。
fn tail_may_become_marker_line(tail: &[u8], pending: &[Pending]) -> bool {
    pending
        .iter()
        .any(|entry| marker_line_possible(tail, &entry.marker))
}

/// 尾巴末尾的一部分，会不会长成 `marker` 的结束标记行。
fn marker_line_possible(tail: &[u8], marker: &str) -> bool {
    let marker = marker.as_bytes();
    if marker.is_empty() {
        return false;
    }

    // ① 标记还没收全：尾巴末尾必须对上这个标记自己的字节，不能只看常量前缀。
    let least = MARKER_PREFIX.len().min(marker.len());
    let longest = marker.len().min(tail.len());
    for len in least..=longest {
        if tail.ends_with(&marker[..len]) {
            return true;
        }
    }

    // ② 标记已收全：后面是 `_E:` + 数字的前缀（数字后可以跟一个 `\r`）。
    let Some(at) = rfind(tail, marker) else {
        return false;
    };
    let after = &tail[at + marker.len()..];
    let suffix = END_MARKER_SUFFIX.as_bytes();
    if after.len() < suffix.len() {
        return suffix.starts_with(after);
    }
    let rest = &after[suffix.len()..];
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let end = &rest[digits..];
    end.is_empty() || end == b"\r"
}

/// 当前命令阶段里，还没换行的尾巴要不要压住：里面有当前命令的标记或待处理标记了
/// （等它的行尾），或者尾巴正好是某个标记的前缀（下一段可能就是它）。
fn streaming_tail_is_suspicious(tail: &[u8], active_marker: &str, pending: &[Pending]) -> bool {
    let markers = std::iter::once(active_marker)
        .filter(|marker| !marker.is_empty())
        .chain(pending.iter().map(|entry| entry.marker.as_str()));
    for marker in markers {
        let bytes = marker.as_bytes();
        if find(tail, bytes).is_some() || trailing_prefix_len(tail, bytes) > 0 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写进终端那一行的包装脚本（不含清输入前缀和末尾换行）。
    fn wrapped_of(marker: &str, command: &str) -> String {
        format!(
            " {marker}=0; {{ printf '\\n%s\\n' '{marker}_S'; trap ':' INT; {marker}_cmd='{command}'; eval \"${marker}_cmd\"; {marker}_rc=$?; trap - INT; printf '%s\\n' '{marker}_E:'\"${marker}_rc\"; }}"
        )
    }

    /// `arm` 时要给的整行原文（清输入前缀 + 包装脚本）。
    fn payload_of(marker: &str, command: &str) -> String {
        format!("\x0b\x15{}", wrapped_of(marker, command))
    }

    /// 一条完整的 AI 命令在终端里产生的原始输出（真实布局，见联网测试的采样）。
    ///
    /// 回显 = readline 的 BEL（清输入前缀不进回显）+ 包装脚本原文 + 回车；随后是
    /// 执行时 bash 关掉括号粘贴、开始标记行、命令输出、结束标记行、新提示符。
    fn command_stream(marker: &str, command: &str, output: &str, code: i32) -> Vec<u8> {
        let wrapped = wrapped_of(marker, command);
        format!(
            "\x07{wrapped}\r\n\x1b[?2004l\r\r\n{marker}_S\r\n{output}{marker}_E:{code}\r\n\x1b[?2004h$ "
        )
        .into_bytes()
    }

    /// 跑一遍：arm + 整段输出一次喂进去。
    fn filter_one(marker: &str, command: &str, output: &str, code: i32) -> DisplayOutput {
        let mut filter = DisplayFilter::new();
        let stream = command_stream(marker, command, output, code);
        let mut out = filter.arm(marker, &payload_of(marker, command), command);
        let pushed = filter.push(&stream);
        out.data.extend_from_slice(&pushed.data);
        out.suppressed += pushed.suppressed;
        out
    }

    /// 跑完一条完整的 AI 命令（用来测"之后"的行为）。
    fn run_one_command(filter: &mut DisplayFilter, marker: &str, command: &str) -> DisplayOutput {
        let stream = command_stream(marker, command, "hi\r\n", 0);
        let mut out = filter.arm(marker, &payload_of(marker, command), command);
        let pushed = filter.push(&stream);
        out.data.extend_from_slice(&pushed.data);
        out.suppressed += pushed.suppressed;
        out
    }

    /// 组一个"超时命令、标记待处理"的过滤器（轻量过滤的状态，AI-3 第 0 节用）。
    fn filter_with_pending_marker(marker: &str, now: Instant) -> DisplayFilter {
        let mut filter = DisplayFilter::new();
        let _ = filter.arm_at(marker, &payload_of(marker, "sleep 30"), "sleep 30", now);
        let _ = filter.push_at(
            format!(
                "\x07{}\r\n{marker}_S\r\npartial output",
                wrapped_of(marker, "sleep 30")
            )
            .as_bytes(),
            now,
        );
        let _ = filter.release_at(marker, now);
        assert_eq!(filter.pending.len(), 1, "标记要转成待处理");
        filter
    }

    #[test]
    fn synthetic_echo_normalizes_newlines() {
        assert_eq!(synthetic_echo("echo hi"), b"echo hi\r\n".to_vec());
        assert_eq!(synthetic_echo("a\nb"), b"a\r\nb\r\n".to_vec());
        assert_eq!(synthetic_echo("a\r\nb"), b"a\r\nb\r\n".to_vec());
    }

    #[test]
    fn hides_the_wrapper_and_shows_the_command() {
        let out = filter_one("__M__", "echo hi", "hi\r\n", 0);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            !text.contains("__M__"),
            "包装脚本和标记都不该显示：{text:?}"
        );
        assert!(text.contains("echo hi\r\n"), "要有合成回显：{text:?}");
        assert!(text.contains("hi\r\n"), "命令输出要在：{text:?}");
        assert!(text.contains("$ "), "最后的提示符要放行：{text:?}");
        assert!(out.suppressed > 0, "确实吞掉了包装脚本那一段");
        assert!(
            !text.contains('\x07'),
            "回显前的 BEL 不能发给界面：{text:?}"
        );
    }

    #[test]
    fn the_same_holds_when_the_stream_is_split_anywhere() {
        // 标记（和包装脚本）被拆在两段、三段数据里也不能出错。
        let stream = command_stream("__M__", "echo hi", "hi\r\n", 0);
        let reference = {
            let mut filter = DisplayFilter::new();
            let mut out = filter.arm("__M__", &payload_of("__M__", "echo hi"), "echo hi");
            let pushed = filter.push(&stream);
            out.data.extend_from_slice(&pushed.data);
            out
        };
        for cut in 1..stream.len() {
            let mut filter = DisplayFilter::new();
            let mut out = filter.arm("__M__", &payload_of("__M__", "echo hi"), "echo hi");
            for piece in [&stream[..cut], &stream[cut..]] {
                out.data.extend_from_slice(&filter.push(piece).data);
            }
            assert_eq!(
                out.data, reference.data,
                "在第 {cut} 字节切开之后显示结果要一样"
            );
        }
    }

    #[test]
    fn tolerates_a_wrapped_command_echo() {
        // 行编辑器按终端宽度折行：回显里被插入 \r\n。
        let marker = "__M__";
        let command = "echo hi";
        let wrapped = wrapped_of(marker, command);
        let mut folded = String::new();
        for (index, chunk) in wrapped.as_bytes().chunks(40).enumerate() {
            if index > 0 {
                folded.push_str("\r\n");
            }
            folded.push_str(&String::from_utf8_lossy(chunk));
        }
        let stream = format!(
            "\x07{folded}\r\n\x1b[?2004l\r\r\n{marker}_S\r\nhi\r\n{marker}_E:0\r\n\x1b[?2004h$ "
        );
        let mut filter = DisplayFilter::new();
        let mut out = filter.arm(marker, &payload_of(marker, command), command);
        out.data
            .extend_from_slice(&filter.push(stream.as_bytes()).data);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(!text.contains(marker), "折行回显也要吃掉：{text:?}");
        assert!(text.contains("echo hi\r\n"), "合成回显在：{text:?}");
        assert!(text.contains("hi\r\n"), "输出在：{text:?}");
    }

    #[test]
    fn colored_output_passes_through_untouched() {
        let out = filter_one("__M__", "ls", "\x1b[31mred\x1b[0m\r\n", 0);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            text.contains("\x1b[31mred\x1b[0m"),
            "带颜色的输出要原样放行：{text:?}"
        );
    }

    #[test]
    fn output_that_looks_like_another_marker_is_kept() {
        // 命令输出里恰好有**别的**标记样文字：原样显示。
        let out = filter_one(
            "__M__",
            "cat notes.txt",
            "这里有个 __ZSSH_deadbeefdeadbeefdeadbeef___E:7 样的字符串\r\n",
            0,
        );
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            text.contains("__ZSSH_deadbeefdeadbeefdeadbeef___E:7"),
            "不是我们这次用的标记，不能动：{text:?}"
        );
    }

    #[test]
    fn inline_end_marker_keeps_the_output_before_it() {
        // 命令输出没有以换行结尾：结束标记跟在输出后面，只丢标记本身。
        let marker = "__M__";
        let stream = command_stream(marker, "printf abc", "abc", 0);
        let mut filter = DisplayFilter::new();
        let mut out = filter.arm(marker, &payload_of(marker, "printf abc"), "printf abc");
        out.data.extend_from_slice(&filter.push(&stream).data);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(text.contains("abc"), "行内的用户输出要留着：{text:?}");
        assert!(!text.contains(marker), "行内的结束标记要丢掉：{text:?}");
        assert!(text.contains("printf abc\r\n"), "合成回显在：{text:?}");
    }

    #[test]
    fn falls_back_to_line_filtering_when_the_echo_is_off() {
        // 兜底：回显被关掉，包装脚本自己的输出直接出来了。什么都别吞，
        // 只把标记行丢掉。
        let marker = "__M__";
        let stream = format!("\n{marker}_S\r\nZSSH_ECHO_OFF\r\n{marker}_E:0\r\n\x1b[?2004h$ ");
        let mut filter = DisplayFilter::new();
        let mut out = filter.arm(
            marker,
            &payload_of(marker, "echo ZSSH_ECHO_OFF"),
            "echo ZSSH_ECHO_OFF",
        );
        out.data
            .extend_from_slice(&filter.push(stream.as_bytes()).data);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            text.contains("ZSSH_ECHO_OFF\r\n"),
            "命令输出一定要显示出来：{text:?}"
        );
        assert!(!text.contains(marker), "标记行还是要丢掉：{text:?}");
        assert!(text.contains("$ "), "提示符放行：{text:?}");
    }

    #[test]
    fn holds_suspicious_tails_but_releases_them_on_timeout() {
        // 尾巴正好是标记的前缀：先压住，等不到下文就按时间放行（兜底）。
        let marker = "__M__";
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let mut out = filter.arm(marker, &payload_of(marker, "echo hi"), "echo hi");
        // 回显 + 开始标记 + 一段以标记前缀结尾的输出（没有换行）。
        let first = format!(
            "\x07{}\r\n{marker}_S\r\npartial output __M",
            wrapped_of(marker, "echo hi")
        );
        out.data
            .extend_from_slice(&filter.push_at(first.as_bytes(), now).data);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            !text.contains("__M"),
            "可能是标记前缀的尾巴先压住：{text:?}"
        );
        // 时间到：放行。
        let later = now + HOLD_MAX_AGE + Duration::from_millis(1);
        let released = filter.poll_at(later);
        let text = String::from_utf8_lossy(&released.data).into_owned();
        assert!(
            text.contains("partial output __M"),
            "兜底必须放行，不能吞输出：{text:?}"
        );
    }

    #[test]
    fn tail_like_ordinary_text_is_shown_immediately() {
        // 普通尾巴（不是标记前缀）不压：直接显示。
        let marker = "__M__";
        let mut filter = DisplayFilter::new();
        let mut out = filter.arm(marker, &payload_of(marker, "echo hi"), "echo hi");
        let first = format!(
            "\x07{}\r\n{marker}_S\r\ndone, no newline",
            wrapped_of(marker, "echo hi")
        );
        out.data
            .extend_from_slice(&filter.push(first.as_bytes()).data);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            text.contains("done, no newline"),
            "普通尾巴要立刻显示：{text:?}"
        );
    }

    #[test]
    fn multi_line_command_echo_is_normalized() {
        let marker = "__M__";
        let command = "echo one\necho two";
        let out = filter_one(marker, command, "one\r\ntwo\r\n", 0);
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(
            text.contains("echo one\r\necho two\r\n"),
            "多行命令的合成回显要按 \r\n 折：{text:?}"
        );
    }

    // ---------- AI-2e 第 1 节：用完必须退出 ----------

    #[test]
    fn after_a_command_everything_passes_through_immediately() {
        // 跑完一条完整的 AI 命令之后，用户敲什么都要立即、原样地过去。
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        run_one_command(&mut filter, "__M__", "echo hi");
        for chunk in [
            b"_".as_slice(),
            b"__".as_slice(),
            b"__Z".as_slice(),
            b"\x1b[5;10H", // vim / less 里移动光标
            b"\r",
            b"\x1b[?25l", // 隐藏光标
            b"\x1b[2J",
        ] {
            let out = filter.push_at(chunk, now);
            assert_eq!(
                out,
                DisplayOutput {
                    data: chunk.to_vec(),
                    suppressed: 0
                },
                "命令结束之后 {:?} 要原样立即返回",
                String::from_utf8_lossy(chunk)
            );
        }
    }

    #[test]
    fn suspicious_but_harmless_tails_are_not_held_after_a_command() {
        // 同上，但按"压住"的反面测：这些块之后 poll 不应该吐出任何东西
        // （说明它们当时就发出去了，而不是被压着等超时）。
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        run_one_command(&mut filter, "__M__", "echo hi");
        for chunk in [
            b"my_var".as_slice(),
            b"\x1b[5;10H".as_slice(),
            b"_".as_slice(),
        ] {
            let _ = filter.push_at(chunk, now);
        }
        let polled = filter.poll_at(now + HOLD_MAX_AGE + Duration::from_millis(1));
        assert_eq!(polled.data, Vec::<u8>::new(), "没有东西被压着");
    }

    #[test]
    fn a_timed_out_command_keeps_only_light_filtering() {
        // 超时返回：release 之后只做轻量过滤——迟到的结束标记行丢掉，
        // 用户敲的 `_` 立即显示。
        let marker = "__M__";
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let stream = format!(
            "\x07{}\r\n{marker}_S\r\npartial output",
            wrapped_of(marker, "sleep 30")
        );
        let _ = filter.arm_at(marker, &payload_of(marker, "sleep 30"), "sleep 30", now);
        let mut out = filter.push_at(stream.as_bytes(), now);
        out.data
            .extend_from_slice(&filter.release_at(marker, now).data);

        // 用户接着用终端：`_` 必须立即出来。
        let typed = filter.push_at(b"_", now);
        assert_eq!(
            typed.data,
            b"_".to_vec(),
            "超时之后用户敲的 `_` 不能被压：{:?}",
            String::from_utf8_lossy(&typed.data)
        );
        let vim = filter.push_at(b"\x1b[5;10H", now);
        assert_eq!(vim.data, b"\x1b[5;10H".to_vec(), "光标移动要立即过去");

        // 迟到的结束标记行：丢掉；待处理标记也就清了。
        let late = format!("\r\n{marker}_E:0\r\n");
        let dropped = filter.push_at(late.as_bytes(), now);
        assert!(
            !String::from_utf8_lossy(&dropped.data).contains(marker),
            "迟到的结束标记行要丢掉：{:?}",
            String::from_utf8_lossy(&dropped.data)
        );
        assert!(dropped.suppressed > 0, "确实丢了东西");
        assert!(filter.pending.is_empty(), "标记出现之后要清掉");

        // 清掉之后再出现同样的文字（用户 cat 出来的）就直通了。
        let again = filter.push_at(late.as_bytes(), now);
        assert_eq!(
            again.data,
            late.as_bytes().to_vec(),
            "没有待处理标记时完全直通"
        );
    }

    #[test]
    fn arming_a_new_command_keeps_the_previous_marker_pending() {
        // 上一条命令没结束（超时），下一条命令又来了：两条命令的标记行都要丢。
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let first = "__M1__";
        let _ = filter.arm_at(first, &payload_of(first, "sleep 30"), "sleep 30", now);
        let _ = filter.push_at(
            format!("\x07{}\r\n{first}_S\r\n", wrapped_of(first, "sleep 30")).as_bytes(),
            now,
        );
        // 第二条命令：第一条的标记转成待处理。
        let second = "__M2__";
        let out = filter.arm_at(second, &payload_of(second, "echo hi"), "echo hi", now);
        assert_eq!(out.data, Vec::<u8>::new());
        let stream = command_stream(second, "echo hi", "hi\r\n", 0);
        let shown = filter.push_at(&stream, now);
        let text = String::from_utf8_lossy(&shown.data).into_owned();
        assert!(!text.contains(second), "第二条命令的包装脚本也不显示");
        assert!(text.contains("hi\r\n"), "第二条命令的输出在");

        // 第一条的迟到标记：丢掉。
        let late = format!("{first}_E:0\r\n");
        let dropped = filter.push_at(late.as_bytes(), now);
        assert!(
            !String::from_utf8_lossy(&dropped.data).contains(first),
            "上一条命令迟到的标记行也要丢"
        );
        // 第二条命令结束之后：完全直通。
        let after = filter.push_at(b"_", now);
        assert_eq!(after.data, b"_".to_vec());
    }

    #[test]
    fn end_marker_split_across_chunks_still_exits() {
        // 结束标记被拆在两段数据里：还是要正确丢掉并退出。
        let marker = "__M__";
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let stream = command_stream(marker, "echo hi", "hi\r\n", 0);
        let cut = stream
            .windows(marker.len() + END_MARKER_SUFFIX.len())
            .position(|w| w == format!("{marker}{END_MARKER_SUFFIX}").as_bytes())
            .expect("找得到结束标记")
            + marker.len()
            + 1; // 切在 `_E:` 的数字中间
        let mut out = filter.arm_at(marker, &payload_of(marker, "echo hi"), "echo hi", now);
        for piece in [&stream[..cut], &stream[cut..]] {
            let pushed = filter.push_at(piece, now);
            out.data.extend_from_slice(&pushed.data);
            out.suppressed += pushed.suppressed;
        }
        let text = String::from_utf8_lossy(&out.data).into_owned();
        assert!(!text.contains(marker), "结束标记不能漏出来：{text:?}");
        assert!(filter.active.is_none(), "结束标记处理完要退出");
        // 退出之后立即直通。
        let after = filter.push_at(b"\x1b[5;10H", now);
        assert_eq!(after.data, b"\x1b[5;10H".to_vec());
    }

    #[test]
    fn pending_markers_expire_after_thirty_minutes() {
        let marker = "__M__";
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let _ = filter.arm_at(marker, &payload_of(marker, "sleep 30"), "sleep 30", now);
        let _ = filter.release_at(marker, now);
        assert_eq!(filter.pending.len(), 1);
        let _ = filter.poll_at(now + PENDING_MAX_AGE + Duration::from_secs(1));
        assert!(filter.pending.is_empty(), "30 分钟后清掉");
    }

    #[test]
    fn a_late_pending_marker_split_at_any_position_does_not_show() {
        // AI-3 第 0 节：超时命令迟到的结束标记，尾巴末尾落到 `__`（标记的收尾，
        // 不再像常量前缀）时以前会被放行，标记就整个显示在屏幕上。现在按"某个
        // 待处理标记完整写法 `<标记>_E:<数字>` 的前缀"判断，从 `__ZSSH_` 这 7 个
        // 字符在同一段里收全之后的**每一个拆点**都不能漏。
        let marker = "__ZSSH_1f2e3d4c__";
        let now = Instant::now();
        let line = format!("output {marker}_E:0\r\n$ ");
        // 拆点从"第一段里已经有完整的 `__ZSSH_`"开始（更早的碎片和用户敲的
        // `_` / `__` 字节完全一样，卡里明确要求不压，见下一个测试）。
        let covered = "output ".len() + MARKER_PREFIX.len();

        for cut in covered..line.len() {
            let mut filter = filter_with_pending_marker(marker, now);
            let mut shown = filter.push_at(&line.as_bytes()[..cut], now).data;
            shown.extend_from_slice(&filter.push_at(&line.as_bytes()[cut..], now).data);
            let shown = String::from_utf8_lossy(&shown).into_owned();
            assert!(
                !shown.contains("__ZSSH_"),
                "拆在 {cut} 时标记漏出来了：{shown:?}"
            );
            assert!(
                !shown.contains("_E:"),
                "拆在 {cut} 时结束标记拖尾漏出来了：{shown:?}"
            );
            assert!(
                shown.contains("output ") && shown.contains("$ "),
                "拆在 {cut} 时画面缺内容：{shown:?}"
            );
        }

        // 卡片里报的三段式（`__ZSSH_b` / `bbbb…bb__` / `_E:0`）原样演一遍。
        let b_marker = "__ZSSH_bbbbbbbb__";
        let mut filter = filter_with_pending_marker(b_marker, now);
        let mut shown = filter.push_at(b"output __ZSSH_b", now).data;
        shown.extend_from_slice(&filter.push_at(b"bbbbbbb__", now).data);
        shown.extend_from_slice(&filter.push_at(b"_E:0\r\n$ ", now).data);
        let shown = String::from_utf8_lossy(&shown).into_owned();
        assert!(
            !shown.contains("__ZSSH_") && !shown.contains("_E:"),
            "三段式拆开时标记漏出来了：{shown:?}"
        );
    }

    #[test]
    fn a_late_pending_marker_never_shows_even_pushed_byte_by_byte() {
        // 同上，但余下部分按单个字节推进来（第 0 节要求逐字节拆一遍）。
        let marker = "__ZSSH_1f2e3d4c__";
        let now = Instant::now();
        let line = format!("output {marker}_E:0\r\n$ ");
        let (head, rest) = line.split_at("output __ZSSH_".len());
        let mut filter = filter_with_pending_marker(marker, now);
        let mut shown = filter.push_at(head.as_bytes(), now).data;
        for byte in rest.as_bytes() {
            shown.extend_from_slice(&filter.push_at(&[*byte], now).data);
        }
        let shown = String::from_utf8_lossy(&shown).into_owned();
        assert!(
            !shown.contains("__ZSSH_"),
            "逐字节推时标记漏出来了：{shown:?}"
        );
        assert!(
            !shown.contains("_E:"),
            "逐字节推时结束标记拖尾漏出来了：{shown:?}"
        );
        assert!(
            shown.contains("output ") && shown.contains("$ "),
            "逐字节推时画面缺内容：{shown:?}"
        );
    }

    #[test]
    fn other_marker_like_text_from_the_user_is_shown_immediately() {
        // 用户自己输出 `__ZSSH_cafe__` 这类别的文字：既不能丢，也不能压。
        let marker = "__ZSSH_1f2e3d4c__";
        let now = Instant::now();
        let mut filter = filter_with_pending_marker(marker, now);

        let line = b"echo __ZSSH_cafe__\r\n__ZSSH_cafe__\r\n";
        let out = filter.push_at(line, now);
        assert_eq!(out.data, line.to_vec(), "别的标记样文字要原样显示");

        // 不带换行的尾巴也一样立即放行。
        let tail = b"ab__ZSSH_cafe__";
        let out = filter.push_at(tail, now);
        assert_eq!(out.data, tail.to_vec(), "别的标记样尾巴不能压");
    }

    #[test]
    fn short_tails_are_not_held_but_the_prefix_is() {
        // 单个 `_`、`__`、`__Z`、`__ZSSH` 不压（用户敲变量名不能被拖慢）；
        // 从完整的 `__ZSSH_` 起才压，而且 2 秒兜底到点原样放行。
        let marker = "__ZSSH_1f2e3d4c__";
        let now = Instant::now();

        for short in [
            b"_".as_slice(),
            b"__".as_slice(),
            b"__Z".as_slice(),
            b"__ZSSH".as_slice(),
            b"my_".as_slice(),
        ] {
            let mut filter = filter_with_pending_marker(marker, now);
            let out = filter.push_at(short, now);
            assert_eq!(
                out.data,
                short.to_vec(),
                "{:?} 要立即放行",
                String::from_utf8_lossy(short)
            );
        }

        let mut filter = filter_with_pending_marker(marker, now);
        let held = filter.push_at(b"xx__ZSSH_", now);
        assert!(
            held.data.is_empty(),
            "完整前缀要压住：{:?}",
            String::from_utf8_lossy(&held.data)
        );
        let released = filter.poll_at(now + HOLD_MAX_AGE + Duration::from_millis(1));
        assert_eq!(released.data, b"xx__ZSSH_".to_vec(), "兜底到点要原样放行");
    }

    #[test]
    fn flush_releases_the_hold_and_clears_pending() {
        let now = Instant::now();
        let mut filter = DisplayFilter::new();
        let marker = "__M__";
        let _ = filter.arm_at(marker, &payload_of(marker, "echo hi"), "echo hi", now);
        // 尾巴是标记的前缀 `__M`：会被压住。
        let _ = filter.push_at(
            format!(
                "\x07{}\r\n{marker}_S\r\nhello __M",
                wrapped_of(marker, "echo hi")
            )
            .as_bytes(),
            now,
        );
        let marker2 = "__M2__";
        let armed = filter.arm_at(marker2, &payload_of(marker2, "echo"), "echo", now);
        assert_eq!(
            armed.data,
            b"hello __M".to_vec(),
            "换命令的时候压着的尾巴要放行"
        );
        // 上一条的标记变成待处理，flush 之后清掉。
        assert_eq!(filter.pending.len(), 1);
        let flushed = filter.flush();
        assert!(flushed.data.is_empty(), "尾巴已经放行过了");
        assert!(filter.pending.is_empty(), "会话结束清掉待处理标记");
    }
}
