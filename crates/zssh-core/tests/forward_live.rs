//! 端口转发的联网测试：本地转发（-L）和远程转发（-R）各走一遍真流量。
//!
//! - 本地转发：本机监听一个随机端口，经服务器连到服务器自己的 sshd，读到 `SSH-` 版本行就算通。
//! - 远程转发：本机起一个回声服务，请服务器监听一个端口转回来，再在服务器上用 bash 的
//!   `/dev/tcp` 连这个端口，收到回声就算通。
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use zssh_core::ssh::forward::{self, ForwardStats};

use common::{TempKnownHosts, TestServer};

const STEP_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn local_forward_reaches_the_server_side_sshd() {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 local_forward：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("forward-local");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    let listener = forward::bind_local("127.0.0.1", 0)
        .await
        .expect("监听本机端口");
    let port = listener.local_addr().expect("本机地址").port();
    let stats = Arc::new(ForwardStats::default());
    let task = tokio::spawn(forward::run_local(
        Arc::clone(&connection),
        listener,
        "127.0.0.1".to_owned(),
        server.port,
        Arc::clone(&stats),
    ));

    let mut client = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("连本机转发端口");
    let mut banner = [0u8; 4];
    timeout(STEP_TIMEOUT, client.read_exact(&mut banner))
        .await
        .expect("等 sshd 版本行超时")
        .expect("读版本行");
    assert_eq!(&banner, b"SSH-");
    assert_eq!(stats.snapshot().total, 1);

    drop(client);
    task.abort();
}

#[tokio::test]
async fn remote_forward_comes_back_to_a_local_service() {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 remote_forward：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("forward-remote");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    // 本机回声服务：收到什么回什么
    let echo = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("回声服务监听");
    let echo_port = echo.local_addr().expect("回声地址").port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 || socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    // 服务器那边的端口按进程号错开，几个测试进程同时跑也不容易撞
    let remote_port = 20000 + (std::process::id() % 20000) as u16;
    let (sink, incoming) = tokio::sync::mpsc::unbounded_channel();
    let actual = connection
        .start_remote_forward("127.0.0.1", remote_port, sink)
        .await
        .expect("请求远程转发");
    let stats = Arc::new(ForwardStats::default());
    let task = tokio::spawn(forward::run_remote(
        Arc::clone(&connection),
        incoming,
        "127.0.0.1".to_owned(),
        echo_port,
        Arc::clone(&stats),
    ));

    let command = format!(
        "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{actual} && printf zssh-fwd >&3 && head -c 8 <&3'"
    );
    let output = timeout(STEP_TIMEOUT, connection.exec(&command))
        .await
        .expect("服务器上连转发端口超时")
        .expect("执行命令");
    assert_eq!(output.trim(), "zssh-fwd");
    assert!(stats.snapshot().total >= 1);

    task.abort();
    connection.stop_remote_forward("127.0.0.1", actual).await;
}
