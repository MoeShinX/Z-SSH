//! 每会话的终端环形缓冲（AI-1）。
//!
//! 保留最近 256KB 原始输出字节，供工具桥的 `read_terminal` 读取。
//! 输出转发路径上只是一次旁路拷贝，不阻塞读取、转发和 ack（M1 设计 5.2）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const RING_CAPACITY: usize = 256 * 1024;

/// 跨任务共享的环形缓冲句柄：会话转发任务往里写，工具桥 `read_terminal` 读。
pub type SharedTerminalRing = Arc<Mutex<TerminalRing>>;

impl TerminalRing {
    /// 建一个可以跨任务共享的句柄。
    pub fn shared() -> SharedTerminalRing {
        Arc::new(Mutex::new(Self::new()))
    }
}

pub struct TerminalRing {
    capacity: usize,
    segments: VecDeque<Vec<u8>>,
    len: usize,
    total_received: u64,
}

impl TerminalRing {
    pub fn new() -> Self {
        Self::with_capacity(RING_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            segments: VecDeque::new(),
            len: 0,
            total_received: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total_received += bytes.len() as u64;
        if bytes.len() >= self.capacity {
            self.segments.clear();
            self.segments
                .push_back(bytes[bytes.len() - self.capacity..].to_vec());
            self.len = self.capacity;
            return;
        }
        self.segments.push_back(bytes.to_vec());
        self.len += bytes.len();
        while self.len > self.capacity {
            let overflow = self.len - self.capacity;
            let front_len = self.segments.front().map_or(0, |s| s.len());
            if front_len <= overflow {
                self.segments.pop_front();
                self.len -= front_len;
            } else {
                let front = self.segments.front_mut().unwrap();
                front.drain(..overflow);
                self.len -= overflow;
            }
        }
    }

    pub fn recent_lines(&self, max_lines: usize) -> Vec<String> {
        if max_lines == 0 || self.len == 0 {
            return Vec::new();
        }
        let mut data = Vec::with_capacity(self.len);
        for segment in &self.segments {
            data.extend_from_slice(segment);
        }
        let stripped = strip_ansi_escapes::strip(&data);
        let text = String::from_utf8_lossy(&stripped);
        let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        if lines.len() > max_lines {
            lines.drain(..lines.len() - max_lines);
        }
        lines
    }

    /// 保留的原始字节（含 ANSI），转成字符串。
    ///
    /// 给 AI-2c 的终端执行用：在原始文本里找随机结束标记（不能先去 ANSI，标记行可能
    /// 带颜色）。标记是随机串，不会和用户输出撞上。
    pub fn raw_text(&self) -> String {
        let mut data = Vec::with_capacity(self.len);
        for segment in &self.segments {
            data.extend_from_slice(segment);
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    pub fn total_received(&self) -> u64 {
        self.total_received
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for TerminalRing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn ring(capacity: usize) -> TerminalRing {
        TerminalRing::with_capacity(capacity)
    }

    #[test]
    fn starts_empty() {
        let r = ring(RING_CAPACITY);
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.total_received(), 0);
        assert!(r.recent_lines(10).is_empty());
    }

    #[test]
    fn keeps_recent_data_and_counts_total() {
        let mut r = ring(RING_CAPACITY);
        r.push(b"hello\nworld\n");
        assert_eq!(r.len(), 12);
        assert_eq!(r.total_received(), 12);
        assert_eq!(r.recent_lines(10), vec!["hello", "world"]);
    }

    #[test]
    fn drops_oldest_when_over_capacity() {
        let mut r = ring(10);
        r.push(b"0123456789");
        r.push(b"abc");
        assert_eq!(r.len(), 10);
        assert_eq!(r.total_received(), 13);
        assert_eq!(r.recent_lines(10), vec!["3456789abc"]);
    }

    #[test]
    fn single_chunk_larger_than_capacity_keeps_tail() {
        let mut r = ring(10);
        r.push(b"0123456789ABCDEF");
        assert_eq!(r.len(), 10);
        assert_eq!(r.total_received(), 16);
        assert_eq!(r.recent_lines(10), vec!["6789ABCDEF"]);
    }

    #[test]
    fn strips_ansi_sequences() {
        let mut r = ring(RING_CAPACITY);
        r.push(b"\x1b[31mred\x1b[0m plain\n");
        assert_eq!(r.recent_lines(10), vec!["red plain"]);
    }

    #[test]
    fn limits_to_max_lines() {
        let mut r = ring(RING_CAPACITY);
        r.push(b"l1\nl2\nl3\nl4\n");
        assert_eq!(r.recent_lines(2), vec!["l3", "l4"]);
        assert_eq!(r.recent_lines(0), Vec::<String>::new());
    }

    #[test]
    fn handles_crlf_lines() {
        let mut r = ring(RING_CAPACITY);
        r.push(b"a\r\nb\r\n");
        assert_eq!(r.recent_lines(10), vec!["a", "b"]);
    }

    #[test]
    fn keeps_invalid_utf8_without_panicking() {
        let mut r = ring(RING_CAPACITY);
        r.push(b"\xff\xfe hello\n");
        let lines = r.recent_lines(10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn trim_across_multiple_pushes_can_split_lines() {
        let mut r = ring(8);
        r.push(b"aaaa");
        r.push(b"bbbb");
        r.push(b"cccc");
        assert_eq!(r.len(), 8);
        assert_eq!(r.recent_lines(10), vec!["bbbbcccc"]);
    }

    #[test]
    fn concurrent_writer_and_reader_do_not_deadlock() {
        let ring = Arc::new(Mutex::new(TerminalRing::with_capacity(4096)));
        let writer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                for i in 0..2_000u32 {
                    let chunk = format!("line-{i}\n");
                    ring.lock().unwrap().push(chunk.as_bytes());
                }
            })
        };
        for _ in 0..2_000 {
            let r = ring.lock().unwrap();
            let _ = r.recent_lines(50);
            let _ = r.len();
        }
        writer.join().unwrap();
        let r = ring.lock().unwrap();
        assert_eq!(
            r.total_received(),
            (0..2_000u32)
                .map(|i| format!("line-{i}\n").len() as u64)
                .sum::<u64>()
        );
        assert_eq!(r.recent_lines(1), vec!["line-1999"]);
    }
}
