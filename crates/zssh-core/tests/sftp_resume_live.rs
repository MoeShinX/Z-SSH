//! 断线续传的线测试（M3-1 第 3、4 节）：通过一个**测试自己控制的本地转发器**连测试
//! 服务器，传到一半把转发器断掉，再恢复，任务自动从断开的位置接着传，SHA-256 一致。
//!
//! 为什么用转发器：直接拔网线/杀连接在测试里没法稳定复现；转发器是"用户的网络断了"
//! 的替身——切断时链路上的 TCP 流直接消失（和 M1 的 `disconnect_live.rs` 同一个手法）。
//!
//! 前置条件：`ZSSH_TEST_*`。远端只用 `/tmp/zssh-sftp-live-<随机串>`，结束删掉。

mod common;

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::watch;

use zssh_core::sftp::session::SftpChannel;
use zssh_core::sftp::transfer::backend::{
    SftpChannelProvider, SftpEndpoints, SftpReconnect, TransferChannelProvider,
};
use zssh_core::sftp::transfer::{
    ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction, TransferEngine,
    TransferEvent, TransferJob, TransferOptions, TransferState,
};
use zssh_core::ssh::{
    AuthConfig, BoxFuture, ConnectParams, Connection, KnownHosts, NonInteractive,
};

use common::{serial, LocalTempDir, RemoteTempDir, TempKnownHosts, TestServer};

/// 本机小转发器：把流量转给测试服务器，可以一键切断**已经建立**的连接，之后还能接新的。
///
/// "代"（generation）用来区分：切断把代加一，只有建立时代更早的连接会被掐掉，
/// 之后新建的连接不受影响。
struct CuttableProxy {
    port: u16,
    cut: watch::Sender<u64>,
    cuts: Arc<StdMutex<u32>>,
}

impl CuttableProxy {
    async fn start(target_host: &str, target_port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("监听本机端口失败");
        let port = listener.local_addr().expect("本机地址").port();
        let (cut, cut_rx) = watch::channel(0u64);
        let cuts = Arc::new(StdMutex::new(0u32));
        let cuts_for_task = Arc::clone(&cuts);

        let host = target_host.to_owned();
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    continue;
                };
                let host = host.clone();
                let mut cut_rx = cut_rx.clone();
                let generation = *cut_rx.borrow();
                tokio::spawn(async move {
                    let Ok(mut upstream) =
                        tokio::net::TcpStream::connect((host.as_str(), target_port)).await
                    else {
                        return;
                    };
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut up_read, mut up_write) = upstream.split();
                    let upstream_copy = async {
                        let _ = tokio::io::copy(&mut up_read, &mut client_write).await;
                    };
                    let down = async {
                        let _ = tokio::io::copy(&mut client_read, &mut up_write).await;
                    };
                    tokio::select! {
                        _ = upstream_copy => {}
                        _ = down => {}
                        _ = async {
                            // 等到"代"变了（被切断）就把两条流都关掉。
                            while cut_rx.changed().await.is_ok() {
                                if *cut_rx.borrow() > generation {
                                    return;
                                }
                            }
                        } => {}
                    }
                });
            }
        });

        Self {
            port,
            cut,
            cuts: cuts_for_task,
        }
    }

    /// 切断所有已建立的连接（新的连接不受影响）。
    fn cut(&self) {
        let next = *self.cut.borrow() + 1;
        let mut cuts = self.cuts.lock().expect("锁");
        *cuts += 1;
        drop(cuts);
        let _ = self.cut.send(next);
    }

    fn cut_count(&self) -> u32 {
        *self.cuts.lock().expect("锁")
    }
}

/// 走转发器的连接参数（known_hosts 直接给 [`KnownHosts`]，不碰用户的）。
fn params_via_proxy(
    server: &TestServer,
    proxy_port: u16,
    known_hosts: &KnownHosts,
) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".to_owned(),
        port: proxy_port,
        username: server.username.clone(),
        auth: AuthConfig::password(Some(server.password.clone())),
        known_hosts: known_hosts.clone(),
        legacy_algorithms: false,
    }
}

/// 重连：走同一个转发器再连一条（生产上是"用保存的凭据 + 已确认的指纹再连一次"）。
struct ReconnectViaProxy {
    server: TestServer,
    proxy_port: u16,
    known_hosts: std::path::PathBuf,
}

impl SftpReconnect for ReconnectViaProxy {
    fn reconnect<'a>(
        &'a self,
    ) -> BoxFuture<'a, Result<Arc<Connection>, zssh_core::sftp::SftpError>> {
        Box::pin(async move {
            let known_hosts = KnownHosts::new(self.known_hosts.clone());
            let connection = self
                .server
                .connect_params(
                    params_via_proxy(&self.server, self.proxy_port, &known_hosts),
                    Arc::new(NonInteractive),
                )
                .await
                .map_err(zssh_core::sftp::SftpError::Disconnected)?;
            Ok(Arc::new(connection))
        })
    }
}

/// 全量覆盖（测试用）。
struct OverwriteEverything;

impl ConflictResolver for OverwriteEverything {
    fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        Box::pin(async {
            ConflictDecision {
                action: ConflictAction::Overwrite,
                apply_to_all: true,
                rename_to: None,
            }
        })
    }
}

fn write_random(path: &std::path::Path, size: usize) {
    let mut data = Vec::with_capacity(size);
    let mut seed: u64 = 0x1234_5678_9ABC_DEF0;
    while data.len() < size {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.extend_from_slice(&seed.to_le_bytes());
    }
    data.truncate(size);
    std::fs::create_dir_all(path.parent().expect("父目录")).unwrap();
    std::fs::write(path, data).unwrap();
}

fn sha256_file(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).expect("读文件");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

#[tokio::test]
async fn disconnect_midway_is_retried_and_resumed() {
    if !common::full_live("disconnect_midway_is_retried_and_resumed") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 disconnect_resume...：{reason}");
            return;
        }
    };

    // 转发器 + 一条走转发器的连接（浏览通道也走它，收尾时要用）。
    let proxy = CuttableProxy::start(&server.host, server.port).await;
    let known_hosts = TempKnownHosts::new("sftp-resume");
    let connection = Arc::new(
        server
            .connect_params(
                params_via_proxy(&server, proxy.port, &known_hosts.known_hosts()),
                Arc::new(NonInteractive),
            )
            .await
            .expect("经转发器连接测试服务器"),
    );
    let browse = Arc::new(common::open_sftp(&connection).await.expect("开浏览通道"));

    // 临时目录都带守卫（M3-2 第 0 节）：断言失败 / panic 也会删掉。
    let remote = RemoteTempDir::create(&server, &connection, &browse, "resume").await;
    let local = LocalTempDir::create("resume");
    let source = local.join("resume.bin");
    // 8MB（门禁用小文件，M3-2 第 0 节）：够大到能在中断之前传一部分。
    write_random(&source, 8 * 1024 * 1024);
    let local_hash = sha256_file(&source);

    // 传输通道走转发器；断线之后由 SftpReconnect 重新连。
    let provider = Arc::new(SftpChannelProvider::new(
        Arc::clone(&connection),
        Some(Arc::new(ReconnectViaProxy {
            server: TestServer::from_env().expect("测试服务器信息"),
            proxy_port: proxy.port,
            known_hosts: known_hosts.path().to_path_buf(),
        })),
    ));
    // 先让传输通道建起来（在断线之前），这样断线断的就是它。
    let _ = provider.channel().await.expect("先开传输通道");
    let endpoints = Arc::new(SftpEndpoints::new(provider));

    let (engine, mut events) = TransferEngine::new(
        endpoints,
        Arc::new(OverwriteEverything),
        TransferOptions {
            concurrency: 2,
            pipeline: true,
            in_flight: 8,
            chunk_len: None,
            preserve_mtime: false,
            preserve_permissions: false,
            max_retries: 3,
            progress_interval: Duration::from_millis(50),
            tail_window: 4096,
        },
    );

    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/resume.bin", remote.path()),
        recursive: false,
        peer: None,
    });

    // 等传了一部分就掐线。
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut cut_after = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Some(TransferEvent::Progress(report)))
                if report.id == id && report.done_bytes > 512 * 1024 =>
            {
                cut_after = Some(report.done_bytes);
                proxy.cut();
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    let cut_after = cut_after.expect("要能在传一半时掐线");
    println!(
        "在第 {cut_after} 字节处掐断转发器（已掐 {} 次）",
        proxy.cut_count()
    );

    // 等任务自己发现断线、重连、续传完成。
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut saw_reconnect = false;
    let mut note_count = 0usize;
    let final_state = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "掐线之后没有自愈");
        match tokio::time::timeout(remaining.min(Duration::from_secs(5)), events.recv()).await {
            Ok(Some(TransferEvent::Progress(report))) if report.id == id => {
                if report.state == TransferState::WaitingReconnect {
                    saw_reconnect = true;
                    if note_count != report.notes.len() {
                        note_count = report.notes.len();
                        println!(
                            "等待重连（已传 {} 字节，注释 {} 条）：{:?}",
                            report.done_bytes,
                            report.notes.len(),
                            report.notes.last()
                        );
                    }
                }
            }
            Ok(Some(TransferEvent::Finished(report))) if report.id == id => {
                println!("任务结束：{:?}；notes={:?}", report.state, report.notes);
                break report.state;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("事件通道关了"),
            Err(_) => {}
        }
    };
    assert!(saw_reconnect, "应当看到「等待重连」这个中间状态");
    assert_eq!(final_state, TransferState::Done, "重连之后要传完");
    println!(
        "掐线之后自动续传完成（转发器被掐 {} 次）",
        proxy.cut_count()
    );

    // 远端 SHA-256 和本地一致。原来那条连接已经在掐线时死了，**另开一条**来核对
    // （这也是"用户的网络恢复之后还能用"的样子）。
    let fresh = Arc::new(
        server
            .connect_params(
                params_via_proxy(&server, proxy.port, &known_hosts.known_hosts()),
                Arc::new(NonInteractive),
            )
            .await
            .expect("重连一条做核对"),
    );
    let remote_hash = fresh
        .exec(&format!(
            "sha256sum '{}/resume.bin' | cut -d' ' -f1",
            remote.path()
        ))
        .await
        .expect("远端 sha256sum");
    assert_eq!(
        remote_hash.trim(),
        local_hash,
        "断线续传之后 SHA-256 要一致"
    );
    println!("断线续传 SHA-256 一致：{}", &local_hash[..16]);

    // 收尾（远端临时目录整个删掉）——用新连接，别用掐断时死掉的那条。
    // 本地目录由守卫的 Drop 删。
    let fresh_browse = SftpChannel::open(&fresh).await.expect("新连接上开 SFTP");
    remote.cleanup(&fresh, &fresh_browse).await;
    fresh_browse.close();
    browse.close();
    let _ = fresh.disconnect().await;
    let _ = connection.disconnect().await;
}
