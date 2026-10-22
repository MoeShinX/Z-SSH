//! 通道字节流的小工具（M3-1 第 1 节）。
//!
//! `russh::ChannelStream` 已经实现了 `AsyncRead + AsyncWrite`，SFTP 直接拿它当传输层。
//! 这里只多一层：**带前缀的流**——开子系统时如果服务器在应答之前先发了几个字节
//! （正常不会，但丢掉就是协议错位），把那几个字节接在最前面，别丢。

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// 一条 SSH 通道的双向字节流（SFTP 的传输层：`AsyncRead + AsyncWrite`）。
pub type ChannelByteStream = PrefixedStream<russh::ChannelStream<russh::client::Msg>>;

/// 通道双向流：先吐 `prefix` 里的字节，再交给底层通道。
pub struct PrefixedStream<S> {
    prefix: std::io::Cursor<Vec<u8>>,
    inner: S,
}

impl<S> PrefixedStream<S> {
    /// 没有前缀的普通包装。
    pub fn new(inner: S) -> Self {
        Self {
            prefix: std::io::Cursor::new(Vec::new()),
            inner,
        }
    }

    /// 带前缀：`prefix` 里的字节先被读出去，然后才是通道里的数据。
    pub fn with_prefix(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix: std::io::Cursor::new(prefix),
            inner,
        }
    }

    /// 前缀里还剩多少没读（诊断 / 测试用）。
    pub fn prefix_remaining(&self) -> usize {
        (self.prefix.get_ref().len() as u64 - self.prefix.position()) as usize
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = &mut *self;
        let remaining = me.prefix_remaining();
        if remaining > 0 {
            let start = me.prefix.position() as usize;
            let end = me.prefix.get_ref().len();
            let take = remaining.min(buf.remaining());
            let slice = me.prefix.get_ref()[start..start + take].to_vec();
            let _ = end;
            buf.put_slice(&slice);
            me.prefix.set_position((start + take) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn prefix_is_read_before_the_inner_stream() {
        let inner = std::io::Cursor::new(b"world".to_vec());
        let mut stream = PrefixedStream::with_prefix(b"hello ".to_vec(), inner);
        let mut text = String::new();
        stream.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "hello world");
        assert_eq!(stream.prefix_remaining(), 0);
    }

    #[tokio::test]
    async fn empty_prefix_delegates_fully() {
        let inner = std::io::Cursor::new(b"abc".to_vec());
        let mut stream = PrefixedStream::new(inner);
        let mut text = String::new();
        stream.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "abc");
    }
}
