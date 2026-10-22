//! 断线检测（设计文档 4.5）的联网测试。
//!
//! 用本机一个可以随时切断的 TCP 转发器把连接掐断（测试自己的手段，不改产品代码），
//! 验证：
//!
//! - 挂在这条连接上的所有 shell 都收到结束（界面据此显示"已断开"）
//! - 池子里那条连接被剔除
//! - 再打开会新建连接、重新认证
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{timeout, Instant};
use zssh_core::ssh::{
    AuthConfig, ChainHop, ConnectionPool, HostKeyDecision, PoolConfig, PromptKind, ShellEvent,
    ShellSession,
};

use common::{run_marker, serial, MockInteractor, TempKnownHosts, TestServer};

/// 跑一条命令的时限。
const MARKER_TIMEOUT: Duration = Duration::from_secs(15);
/// 掐线之后等会话发现的时限。keepalive 大约 90 秒能发现；TCP 断开是立刻的。
const NOTICE_TIMEOUT: Duration = Duration::from_secs(30);
/// 等池子剔除死连接的时限。
const POOL_TIMEOUT: Duration = Duration::from_secs(15);

/// 本机的小 TCP 转发器：把流量转给测试服务器，可以把**已经建立**的连接一键切断，
/// 之后还能继续接新的连接。
///
/// 它是"用户的网络断了"的替身：切断时链路上的 TCP 流直接消失。
///
/// "代"（generation）用来区分：切断把代加一，只有建立时代更早的连接会被掐掉，
/// 之后新建的连接不受影响。
struct CuttableProxy {
    port: u16,
    cut: watch::Sender<u64>,
}

impl CuttableProxy {
    async fn start(target_host: &str, target_port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("监听本机端口失败");
        let port = listener.local_addr().expect("本机地址").port();
        let (cut, cut_rx) = watch::channel(0u64);

        let host = target_host.to_owned();
        tokio::spawn(async move {
            loop {
                let accepted = listener.accept().await;
                let Ok((client, _)) = accepted else { continue };
                let host = host.clone();
                let mut cut = cut_rx.clone();
                // 这个连接属于哪一代：之后代变大了才被掐
                let generation = *cut.borrow();
                tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect((host.as_str(), target_port)).await else {
                        return;
                    };
                    let (mut client, mut server) = (client, server);
                    tokio::select! {
                        _ = copy_bidirectional(&mut client, &mut server) => {}
                        // 切断：两个流一起丢掉，链路就断了
                        _ = wait_next_generation(&mut cut, generation) => {}
                    }
                });
            }
        });

        Self { port, cut }
    }

    fn cut(&self) {
        self.cut.send_modify(|generation| *generation += 1);
    }
}

/// 等到代不再是 `mine`（也就是发生了切断）。
async fn wait_next_generation(rx: &mut watch::Receiver<u64>, mine: u64) {
    loop {
        if *rx.borrow() != mine {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// 等这个 shell 的事件流结束（对端断开时是 Closed 或者 None）。
async fn wait_shell_ends(shell: &mut ShellSession, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Closed)) | Ok(None) => return true,
            // 断开前可能还有一点数据，读掉再看
            Ok(Some(_)) => continue,
            Err(_) => return false,
        }
    }
}

#[tokio::test]
async fn a_cut_connection_takes_down_all_shells_and_leaves_the_pool() {
    if !common::full_live("a_cut_connection_takes_down_all_shells_and_leaves_the_pool") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 a_cut_connection_takes_down_all_shells_and_leaves_the_pool：{reason}");
            return;
        }
    };

    let proxy = CuttableProxy::start(&server.host, server.port).await;
    let known_hosts = TempKnownHosts::new("disconnect");
    // 两条密码答案：第一条建连接，第二条是断线重建
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(&server.password), Some(&server.password)],
    ));
    let pool = ConnectionPool::new(PoolConfig {
        idle_timeout: Duration::from_secs(1),
    });

    // 穿过本机转发器连测试服务器：链路中间有一个可以被切断的点
    let hop = ChainHop {
        label: "目标 测试服务器（经本机转发）".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: proxy.port,
        username: server.username.clone(),
        proxy: None,
        auth: AuthConfig::password(None),
        known_hosts: known_hosts.known_hosts(),
        legacy_algorithms: false,
    };

    let guard = pool
        .acquire(
            5,
            "v1",
            vec![hop.clone()],
            Arc::clone(&interactor) as Arc<_>,
        )
        .await
        .expect("经本机转发建连接失败");

    // 同一条连接上开两个 shell，先确认都能用
    let mut first = guard
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开第一个 shell 失败");
    let mut second = guard
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开第二个 shell 失败");
    let out_first = run_marker(&mut first, "CUT_A", MARKER_TIMEOUT).await;
    assert!(
        out_first.contains("ZSSH_CUT_A_42"),
        "第一个 shell 的输出不对：{}",
        server.sanitize(&out_first)
    );
    let out_second = run_marker(&mut second, "CUT_B", MARKER_TIMEOUT).await;
    assert!(
        out_second.contains("ZSSH_CUT_B_42"),
        "第二个 shell 的输出不对：{}",
        server.sanitize(&out_second)
    );
    println!("两个 shell 都在跑，掐线");

    // 掐断
    proxy.cut();

    // 两个 shell 都要在有限时间内发现
    let first_ended = wait_shell_ends(&mut first, NOTICE_TIMEOUT).await;
    let second_ended = wait_shell_ends(&mut second, NOTICE_TIMEOUT).await;
    assert!(first_ended, "第一个 shell 没有发现断开");
    assert!(second_ended, "第二个 shell 没有发现断开");
    println!("两个 shell 都收到了断开");

    // 池子里这条连接要被剔除
    let deadline = Instant::now() + POOL_TIMEOUT;
    loop {
        if pool.connection_of(5).is_none() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("断开的连接还留在池子里");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop((first, second));
    drop(guard);

    // 再打开：新建连接、重新认证（指纹已经存过，看密码问题出现了两次）
    let again = pool
        .acquire(5, "v1", vec![hop], Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("断线之后应该能重新建连接");
    assert!(again.is_fresh(), "断线之后打开的必须是新连接");
    assert_eq!(
        interactor.asked_kinds(),
        vec![PromptKind::Password, PromptKind::Password],
        "重建时要重新认证"
    );
    let mut shell = again
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开 shell 失败");
    let out = run_marker(&mut shell, "CUT_AGAIN", MARKER_TIMEOUT).await;
    assert!(
        out.contains("ZSSH_CUT_AGAIN_42"),
        "重建后的会话不可用：{}",
        server.sanitize(&out)
    );
    println!("断线后重新打开会新建连接并重新认证");
}
