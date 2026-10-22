//! 会话日志：把终端里显示的内容存成纯文本文件（去掉颜色、光标移动这些控制序列）。
//!
//! 写文件在单独的线程里做：会话任务只把字节扔进通道，磁盘慢也不会拖住终端输出。
//! 日志里会有用户在终端里看到的一切（包括可能显示出来的敏感信息），所以默认关，
//! 只存在本机，目录由用户选。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;

use tracing::warn;

/// 把终端输出流转成纯文本。跨块调用时状态会保留（控制序列、UTF-8 字符可能被切成两半）。
#[derive(Debug, Default)]
pub struct PlainText {
    state: EscState,
    /// 还没遇到换行的当前行（退格要能删掉前一个字符，所以先攒着）
    line: Vec<u8>,
    pending_cr: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum EscState {
    #[default]
    Text,
    /// 刚收到 ESC
    Escape,
    /// `ESC [` …… 直到 0x40-0x7E 的结束字节
    Csi,
    /// `ESC ]`、`ESC P` 之类的字符串序列，直到 BEL 或 `ESC \`
    Str,
    /// 字符串序列里收到了 ESC，下一个是 `\` 就结束
    StrEscape,
}

/// 一行最多攒这么多字节：有的程序整屏都不换行（进度条一直用 \r），别让内存无限涨。
const MAX_LINE_BYTES: usize = 64 * 1024;

impl PlainText {
    /// 喂一块输出，返回已经完整的若干行（每行以 `\n` 结尾）。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &byte in bytes {
            match self.state {
                EscState::Text => self.text_byte(byte, &mut out),
                EscState::Escape => {
                    self.state = match byte {
                        b'[' => EscState::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => EscState::Str,
                        _ => EscState::Text,
                    };
                }
                EscState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = EscState::Text;
                    }
                }
                EscState::Str => match byte {
                    0x07 => self.state = EscState::Text,
                    0x1b => self.state = EscState::StrEscape,
                    _ => {}
                },
                EscState::StrEscape => {
                    self.state = if byte == b'\\' {
                        EscState::Text
                    } else {
                        EscState::Str
                    };
                }
            }
        }
        out
    }

    fn text_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        if self.pending_cr {
            self.pending_cr = false;
            // 单独的 \r（进度条、提示符重绘）也当换行：日志是按时间记的，不是屏幕快照
            self.end_line(out);
            if byte == b'\n' {
                return;
            }
        }
        match byte {
            0x1b => self.state = EscState::Escape,
            b'\r' => self.pending_cr = true,
            b'\n' => self.end_line(out),
            0x08 => self.backspace(),
            b'\t' => self.push(byte),
            byte if byte < 0x20 || byte == 0x7f => {}
            byte => self.push(byte),
        }
    }

    fn push(&mut self, byte: u8) {
        if self.line.len() < MAX_LINE_BYTES {
            self.line.push(byte);
        }
    }

    /// 删掉最后一个字符（UTF-8 多字节字符整个删）。
    fn backspace(&mut self) {
        while let Some(byte) = self.line.pop() {
            if byte & 0b1100_0000 != 0b1000_0000 {
                break;
            }
        }
    }

    fn end_line(&mut self, out: &mut Vec<u8>) {
        // 只剩空白的行（提示符重绘留下的）不写：日志里一堆空行没用
        if !self.line.iter().all(u8::is_ascii_whitespace) {
            out.extend_from_slice(&self.line);
            out.push(b'\n');
        }
        self.line.clear();
    }

    /// 会话结束：把没换行的最后一行也交出来。
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.pending_cr = false;
        self.end_line(&mut out);
        out
    }
}

/// 本地时间 `YYYYMMDD-HHMMSS`，日志文件名用（用户按自己的钟点找日志，UTC 会差几个小时）。
pub fn local_stamp() -> String {
    #[cfg(windows)]
    {
        // SAFETY: GetLocalTime 只往返回值里填系统时间，没有指针参数
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default();
        crate::ai::files::compact_timestamp(secs)
    }
}

/// 文件名里不能出现的字符换成 `_`，太长截断。
pub fn safe_file_stem(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(60)
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').to_owned();
    if trimmed.is_empty() {
        "session".to_owned()
    } else {
        trimmed
    }
}

fn write_lines(writer: &mut BufWriter<File>, bytes: &[u8], failed: &mut bool, path: &Path) {
    if *failed || bytes.is_empty() {
        return;
    }
    if let Err(err) = writer.write_all(bytes) {
        // 磁盘满、U 盘拔了：记一次，之后不再写，不影响会话
        warn!(%err, path = %path.display(), "会话日志写入失败，停止记录");
        *failed = true;
    }
}

/// 把通道里已经排着的都写掉；排空了返回 true（该刷盘了）。
fn receiver_is_idle(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    budget: &QueueBudget,
    writer: &mut BufWriter<File>,
    text: &mut PlainText,
    failed: &mut bool,
    path: &Path,
) -> bool {
    loop {
        match receiver.try_recv() {
            Ok(chunk) => {
                budget.release(chunk.len());
                write_lines(writer, &text.feed(&chunk), failed, path);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            // 通道关了：外面的 recv 循环会收尾
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

/// 日志队列里最多积压这么多还没写进文件的字节（复审 F7）。磁盘慢（U 盘、网络共享、磁盘卡住）
/// 而终端输出很快时，超出的部分丢掉并记数，赶上来后在日志里写一行说明，内存不会一直涨。
pub const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;

/// 队列的字节预算：会话线程放进去前先占位，写线程写完再还回来。
#[derive(Debug, Default)]
pub struct QueueBudget {
    limit: usize,
    queued: std::sync::atomic::AtomicUsize,
    dropped: std::sync::atomic::AtomicU64,
}

impl QueueBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// 能放进队列就占位返回 true；放不下就记成丢弃返回 false。
    pub fn try_reserve(&self, len: usize) -> bool {
        use std::sync::atomic::Ordering;
        let reserved = self
            .queued
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
                (queued.saturating_add(len) <= self.limit).then_some(queued + len)
            })
            .is_ok();
        if !reserved {
            self.dropped.fetch_add(len as u64, Ordering::SeqCst);
        }
        reserved
    }

    pub fn release(&self, len: usize) {
        self.queued
            .fetch_sub(len, std::sync::atomic::Ordering::SeqCst);
    }

    /// 取走到现在为止丢掉的字节数（取完清零）。
    pub fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, std::sync::atomic::Ordering::SeqCst)
    }
}

/// 丢过数据就在日志里补一行说明（经过 PlainText，接在当前行后面换行写）。
fn note_dropped(
    budget: &QueueBudget,
    writer: &mut BufWriter<File>,
    text: &mut PlainText,
    failed: &mut bool,
    path: &Path,
) {
    let dropped = budget.take_dropped();
    if dropped > 0 {
        let note = format!("\r\n[Z-SSH] 日志写入跟不上终端输出，这里丢掉了 {dropped} 字节\r\n");
        write_lines(writer, &text.feed(note.as_bytes()), failed, path);
    }
}

/// 一个会话的日志。丢掉（会话结束）时把剩下的写完、关文件。
pub struct SessionLog {
    sender: Option<Sender<Vec<u8>>>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
    budget: std::sync::Arc<QueueBudget>,
}

impl SessionLog {
    /// 在 `dir` 下新建 `{label}-{stamp}.log`（同名就加序号）。
    pub fn create(dir: &Path, label: &str, stamp: &str) -> std::io::Result<Self> {
        Self::create_with_limit(dir, label, stamp, MAX_QUEUED_BYTES)
    }

    /// 同 [`Self::create`]，队列上限可调（测试用）。
    pub fn create_with_limit(
        dir: &Path,
        label: &str,
        stamp: &str,
        limit: usize,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let stem = safe_file_stem(label);
        let mut sequence = 1u32;
        let (path, file) = loop {
            let name = if sequence == 1 {
                format!("{stem}-{stamp}.log")
            } else {
                format!("{stem}-{stamp}-{sequence}.log")
            };
            let candidate = dir.join(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => break (candidate, file),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && sequence < 1000 => {
                    sequence += 1;
                }
                Err(err) => return Err(err),
            }
        };

        let (sender, receiver) = channel::<Vec<u8>>();
        let worker_path = path.clone();
        let budget = std::sync::Arc::new(QueueBudget::new(limit));
        let worker_budget = std::sync::Arc::clone(&budget);
        let worker = std::thread::Builder::new()
            .name("zssh-session-log".to_owned())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                let mut text = PlainText::default();
                let mut failed = false;
                while let Ok(chunk) = receiver.recv() {
                    worker_budget.release(chunk.len());
                    write_lines(&mut writer, &text.feed(&chunk), &mut failed, &worker_path);
                    note_dropped(
                        &worker_budget,
                        &mut writer,
                        &mut text,
                        &mut failed,
                        &worker_path,
                    );
                    // 通道里暂时没东西了就刷一次盘：程序崩了日志也基本是全的
                    if !failed
                        && receiver_is_idle(
                            &receiver,
                            &worker_budget,
                            &mut writer,
                            &mut text,
                            &mut failed,
                            &worker_path,
                        )
                    {
                        let _ = writer.flush();
                    }
                }
                note_dropped(
                    &worker_budget,
                    &mut writer,
                    &mut text,
                    &mut failed,
                    &worker_path,
                );
                write_lines(&mut writer, &text.finish(), &mut failed, &worker_path);
                let _ = writer.flush();
            })?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            path,
            budget,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 记一块输出。不阻塞。
    pub fn write(&self, bytes: &[u8]) {
        let Some(sender) = &self.sender else {
            return;
        };
        // 积压超过上限就丢掉这块（记数），不让内存跟着慢磁盘一直涨
        if !self.budget.try_reserve(bytes.len()) {
            return;
        }
        if sender.send(bytes.to_vec()).is_err() {
            self.budget.release(bytes.len());
        }
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        // 先关通道，写线程把剩下的写完自己退出
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chunks: &[&[u8]]) -> String {
        let mut text = PlainText::default();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(text.feed(chunk));
        }
        out.extend(text.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn strips_colors_titles_and_cursor_moves() {
        let raw: &[u8] = b"\x1b]0;user@box: ~\x07\x1b[01;32muser@box\x1b[00m:~$ ls\r\n\x1b[0m\x1b[01;34mdir\x1b[0m  file\r\n";
        assert_eq!(plain(&[raw]), "user@box:~$ ls\ndir  file\n");
    }

    #[test]
    fn sequences_split_across_chunks_are_still_removed() {
        assert_eq!(
            plain(&[b"a\x1b[3", b"1mred\x1b", b"[0m\r", b"\n"]),
            "ared\n"
        );
    }

    #[test]
    fn backspace_removes_whole_utf8_characters() {
        assert_eq!(plain(&["错误\x08对\r\n".as_bytes()]), "错对\n");
        assert_eq!(plain(&[b"lss\x08 \x08\r\n"]), "ls\n");
    }

    #[test]
    fn lone_carriage_returns_become_lines_and_blank_lines_are_dropped() {
        assert_eq!(
            plain(&[b"10%\r50%\r100%\r\n\r\n  \r\ndone"]),
            "10%\n50%\n100%\ndone\n"
        );
    }

    #[test]
    fn local_stamp_looks_like_a_timestamp() {
        let stamp = local_stamp();
        assert_eq!(stamp.len(), 15, "{stamp}");
        assert_eq!(&stamp[8..9], "-");
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_stem("web/01: 生产*"), "web_01_ 生产_");
        assert_eq!(safe_file_stem("  ..  "), "session");
    }

    #[test]
    fn log_file_gets_plain_text_and_a_unique_name() {
        let dir = crate::test_util::TestDir::new("session-log");
        let first = SessionLog::create(&dir, "测试机", "20261020-101500").unwrap();
        let second = SessionLog::create(&dir, "测试机", "20261020-101500").unwrap();
        assert_ne!(first.path(), second.path());
        first.write(b"\x1b[1mhello\x1b[0m\r\n");
        first.write(b"bye");
        let path = first.path().to_path_buf();
        drop(first);
        drop(second);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "hello\nbye\n");
    }

    #[test]
    fn queue_budget_caps_the_backlog_and_counts_what_it_drops() {
        let budget = QueueBudget::new(10);
        assert!(budget.try_reserve(6));
        assert!(!budget.try_reserve(6), "超过上限就丢");
        assert!(budget.try_reserve(4));
        budget.release(6);
        assert!(budget.try_reserve(5), "写线程写掉一些之后又放得下");
        assert_eq!(budget.take_dropped(), 6);
        assert_eq!(budget.take_dropped(), 0);
    }

    #[test]
    fn dropped_output_is_noted_in_the_log() {
        let dir = crate::test_util::TestDir::new("session-log-full");
        // 上限比每一块都小：全部丢掉，日志里只留说明
        let log = SessionLog::create_with_limit(&dir, "测试机", "20261020-170000", 4).unwrap();
        log.write(b"0123456789\r\n");
        log.write(b"abcdefghij\r\n");
        let path = log.path().to_path_buf();
        drop(log);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("丢掉了 24 字节"), "{text}");
        assert!(!text.contains("0123456789"));
    }
}
