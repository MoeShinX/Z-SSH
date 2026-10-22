//! 测试用的本机 HTTP 桩：按请求行返回预先写好的响应，用来验证同步的 HTTP 读取
//! 对超大 / 撒谎的响应体有上限。只监听 127.0.0.1，不碰外网。

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 响应体怎么发。
#[derive(Clone)]
pub enum Body {
    /// 按实际长度写 `Content-Length`。
    Exact(Vec<u8>),
    /// `Content-Length` 声明成 `declared`，但只发 `sent` 字节就关连接
    /// （验证“声明超限就不读”）。
    Declared { declared: u64, sent: Vec<u8> },
    /// 不声明长度，用 chunked 一块块发，总共 `total` 字节（验证边读边数）。
    Chunked { total: usize },
}

#[derive(Clone)]
pub struct Canned {
    pub status: u16,
    pub body: Body,
}

impl Canned {
    pub fn new(status: u16, body: Body) -> Self {
        Self { status, body }
    }
}

type Route = Arc<dyn Fn(&str) -> Canned + Send + Sync>;

/// 起一个桩，返回地址。`route` 收到请求行（如 `GET /a HTTP/1.1`）。
pub async fn spawn(route: impl Fn(&str) -> Canned + Send + Sync + 'static) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口");
    let addr = listener.local_addr().expect("取地址");
    let route: Route = Arc::new(route);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let route = route.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                let canned = route(&line);
                let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", canned.status);
                // 客户端读够上限会提前断开，后面的写失败是预期的，忽略。
                match canned.body {
                    Body::Exact(bytes) => {
                        out.push_str(&format!("Content-Length: {}\r\n\r\n", bytes.len()));
                        let _ = stream.write_all(out.as_bytes()).await;
                        let _ = stream.write_all(&bytes).await;
                    }
                    Body::Declared { declared, sent } => {
                        out.push_str(&format!("Content-Length: {declared}\r\n\r\n"));
                        let _ = stream.write_all(out.as_bytes()).await;
                        let _ = stream.write_all(&sent).await;
                    }
                    Body::Chunked { total } => {
                        out.push_str("Transfer-Encoding: chunked\r\n\r\n");
                        if stream.write_all(out.as_bytes()).await.is_err() {
                            return;
                        }
                        let block = vec![b'x'; 64 * 1024];
                        let mut left = total;
                        while left > 0 {
                            let n = left.min(block.len());
                            let piece = format!("{n:x}\r\n");
                            if stream.write_all(piece.as_bytes()).await.is_err()
                                || stream.write_all(&block[..n]).await.is_err()
                                || stream.write_all(b"\r\n").await.is_err()
                            {
                                return;
                            }
                            left -= n;
                        }
                        let _ = stream.write_all(b"0\r\n\r\n").await;
                    }
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}
