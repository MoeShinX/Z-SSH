//! 端口转发的运行部分：本地转发（ssh -L）和远程转发（ssh -R）。
//!
//! 规则存在 [`crate::store::forwards`]；这里只管跑。每条规则一个任务，任务里用
//! `JoinSet` 收着它的每一条连接：任务被取消（用户点停止）时 `JoinSet` 跟着丢掉，
//! 所有连接一起断，不会留下孤儿。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::debug;

use crate::error::ZsshError;
use crate::ssh::connection::{Connection, ForwardedChannel};

/// 多久看一次 SSH 连接还在不在。连接断了转发就没用了，要让界面知道。
const LIVENESS_TICK: Duration = Duration::from_secs(2);
/// 远程转发连本机目标的超时。
const DEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 一条规则的计数，界面轮询着显示。
#[derive(Debug, Default)]
pub struct ForwardStats {
    active: AtomicUsize,
    total: AtomicU64,
    /// 发往目标方向的字节数（从监听那一侧收到的）。
    bytes_sent: AtomicU64,
    /// 从目标回来的字节数。
    bytes_received: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ForwardCounters {
    pub active: usize,
    pub total: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl ForwardStats {
    pub fn snapshot(&self) -> ForwardCounters {
        ForwardCounters {
            active: self.active.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
        }
    }
}

/// 监听地址：`*` 表示所有网卡；IPv6 写法 `[::1]` 去掉方括号。
pub fn listen_host(bind_host: &str) -> &str {
    match bind_host {
        "*" | "" => "0.0.0.0",
        other => other
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(other),
    }
}

/// 本地转发先在本机绑定端口：被占用时马上报错，不用等到 SSH 连上。
pub async fn bind_local(bind_host: &str, bind_port: u16) -> Result<TcpListener, ZsshError> {
    TcpListener::bind((listen_host(bind_host), bind_port))
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AddrInUse {
                ZsshError::InvalidInput(format!("本机端口 {bind_port} 已经被别的程序占用"))
            } else if err.kind() == std::io::ErrorKind::PermissionDenied {
                ZsshError::InvalidInput(format!("没有权限监听本机端口 {bind_port}"))
            } else {
                ZsshError::InvalidInput(format!("监听 {bind_host}:{bind_port} 失败：{err}"))
            }
        })
}

/// 两头对拷，结束后把字节数记进计数。
async fn pump<A, B>(mut near: A, mut far: B, stats: &ForwardStats)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    stats.active.fetch_add(1, Ordering::Relaxed);
    stats.total.fetch_add(1, Ordering::Relaxed);
    match tokio::io::copy_bidirectional(&mut near, &mut far).await {
        Ok((sent, received)) => {
            stats.bytes_sent.fetch_add(sent, Ordering::Relaxed);
            stats.bytes_received.fetch_add(received, Ordering::Relaxed);
        }
        // 对端直接断开（RST）很常见，不算转发出错
        Err(err) => debug!(%err, "转发的一条连接异常结束"),
    }
    stats.active.fetch_sub(1, Ordering::Relaxed);
}

fn connection_lost() -> ZsshError {
    ZsshError::Shell("SSH 连接断开了".to_owned())
}

/// 本地转发：接受本机连接，每条都经 SSH 开一个 direct-tcpip 通道连到目标。
///
/// 一直跑到 SSH 连接断开（返回原因）或者任务被取消。
pub async fn run_local(
    connection: Arc<Connection>,
    listener: TcpListener,
    dest_host: String,
    dest_port: u16,
    stats: Arc<ForwardStats>,
) -> ZsshError {
    let mut conns = JoinSet::new();
    let mut tick = tokio::time::interval(LIVENESS_TICK);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, peer): (TcpStream, SocketAddr) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        debug!(%err, "本地转发 accept 失败");
                        continue;
                    }
                };
                let _ = socket.set_nodelay(true);
                let connection = Arc::clone(&connection);
                let dest_host = dest_host.clone();
                let stats = Arc::clone(&stats);
                conns.spawn(async move {
                    match connection.open_forward_channel(&dest_host, dest_port, peer).await {
                        Ok(channel) => pump(socket, channel, &stats).await,
                        Err(err) => debug!(%err, "本地转发开通道失败"),
                    }
                });
            }
            _ = tick.tick() => {
                if connection.is_closed() {
                    return connection_lost();
                }
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// 远程转发：服务器那边连进来的通道（见 [`Connection::start_remote_forward`]），
/// 每条都在本机连目标，连上了才接受通道，连不上就拒绝（服务器那边的客户端立刻知道）。
pub async fn run_remote(
    connection: Arc<Connection>,
    mut incoming: UnboundedReceiver<ForwardedChannel>,
    dest_host: String,
    dest_port: u16,
    stats: Arc<ForwardStats>,
) -> ZsshError {
    let mut conns = JoinSet::new();
    let mut tick = tokio::time::interval(LIVENESS_TICK);
    loop {
        tokio::select! {
            next = incoming.recv() => {
                let Some(forwarded) = next else {
                    return connection_lost();
                };
                let dest_host = dest_host.clone();
                let stats = Arc::clone(&stats);
                conns.spawn(async move {
                    let target = timeout(
                        DEST_CONNECT_TIMEOUT,
                        TcpStream::connect((dest_host.as_str(), dest_port)),
                    )
                    .await;
                    match target {
                        Ok(Ok(socket)) => {
                            let _ = socket.set_nodelay(true);
                            forwarded.reply.accept().await;
                            pump(forwarded.channel.into_stream(), socket, &stats).await;
                        }
                        _ => {
                            debug!(from = %forwarded.originator, "远程转发连不上本机目标");
                            forwarded
                                .reply
                                .reject(russh::ChannelOpenFailure::ConnectFailed)
                                .await;
                        }
                    }
                });
            }
            _ = tick.tick() => {
                if connection.is_closed() {
                    return connection_lost();
                }
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn listen_host_maps_wildcards_and_brackets() {
        assert_eq!(listen_host("*"), "0.0.0.0");
        assert_eq!(listen_host(""), "0.0.0.0");
        assert_eq!(listen_host("[::1]"), "::1");
        assert_eq!(listen_host("127.0.0.1"), "127.0.0.1");
    }

    #[tokio::test]
    async fn port_in_use_is_reported_plainly() {
        let first = bind_local("127.0.0.1", 0).await.unwrap();
        let port = first.local_addr().unwrap().port();
        let err = bind_local("127.0.0.1", port).await.unwrap_err();
        assert!(err.to_string().contains("已经被别的程序占用"), "{err}");
    }

    #[tokio::test]
    async fn pump_copies_both_ways_and_counts() {
        let (near_app, near) = tokio::io::duplex(64);
        let (far, mut far_app) = tokio::io::duplex(64);
        let stats = Arc::new(ForwardStats::default());
        let task = {
            let stats = Arc::clone(&stats);
            tokio::spawn(async move { pump(near, far, &stats).await })
        };
        let (mut near_read, mut near_write) = tokio::io::split(near_app);
        near_write.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        far_app.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        far_app.write_all(b"pong!").await.unwrap();
        let mut back = [0u8; 5];
        near_read.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"pong!");
        assert_eq!(stats.snapshot().active, 1);

        near_write.shutdown().await.unwrap();
        far_app.shutdown().await.unwrap();
        drop(far_app);
        drop(near_read);
        drop(near_write);
        task.await.unwrap();
        let counters = stats.snapshot();
        assert_eq!((counters.active, counters.total), (0, 1));
        assert_eq!((counters.bytes_sent, counters.bytes_received), (4, 5));
    }
}
