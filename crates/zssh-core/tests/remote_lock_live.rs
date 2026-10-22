//! M3-5 第 0a 节：同一台服务器不管用什么方式连上（主机库的主机、快速连接、终端文件侧栏），
//! 远端目标锁的服务器身份都按**实际连接的**主机、端口、用户名算，互斥才不会失效。
//!
//! 这里用同一台测试服务器开**两条独立的连接**，一条当"主机库里的主机"，一条当"快速连接"：
//! 一条连接上的传输引擎正在写某个远端路径时，另一条连接上的传输引擎、AI 写文件都必须被拒。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）；远端只用专用临时目录。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use zssh_core::ai::files::{replace_via_temp, ReplaceBase, SftpRemoteFs};
use zssh_core::sftp::transfer::backend::{SftpChannelProvider, SftpEndpoints};
use zssh_core::sftp::transfer::{
    ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction, TransferEngine,
    TransferEvent, TransferJob, TransferOptions, TransferState,
};
use zssh_core::ssh::{BoxFuture, Connection};

use common::{serial, LocalTempDir, RemoteTempDir, TempKnownHosts, TestServer};

/// 第一次冲突提问时卡住（这时引擎已经拿了目标的锁），等测试放行。
#[derive(Default)]
struct HoldFirst {
    asked: AtomicUsize,
    asked_once: Notify,
    release: Notify,
}

impl ConflictResolver for HoldFirst {
    fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        Box::pin(async {
            if self.asked.fetch_add(1, Ordering::SeqCst) == 0 {
                self.asked_once.notify_one();
                self.release.notified().await;
            }
            ConflictDecision {
                action: ConflictAction::Overwrite,
                apply_to_all: false,
                rename_to: None,
            }
        })
    }
}

fn options() -> TransferOptions {
    TransferOptions {
        concurrency: 2,
        pipeline: true,
        in_flight: 4,
        chunk_len: None,
        preserve_mtime: false,
        preserve_permissions: false,
        max_retries: 1,
        progress_interval: Duration::from_millis(50),
        tail_window: 4096,
    }
}

fn engine_on(
    connection: &Arc<Connection>,
    resolver: Arc<dyn ConflictResolver>,
) -> (
    Arc<TransferEngine>,
    tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
) {
    let provider = Arc::new(SftpChannelProvider::new(Arc::clone(connection), None));
    TransferEngine::new(Arc::new(SftpEndpoints::new(provider)), resolver, options())
}

async fn finished(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
    id: u64,
) -> zssh_core::sftp::transfer::ProgressReport {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等不到任务 {id} 结束");
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(TransferEvent::Finished(report))) if report.id == id => return report,
            Ok(Some(_)) => {}
            _ => panic!("事件通道关了或超时"),
        }
    }
}

#[tokio::test]
async fn two_connections_to_one_server_share_the_remote_write_lock() {
    if !common::full_live("two_connections_to_one_server_share_the_remote_write_lock") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 two_connections...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("remote-lock");
    // 两条独立的连接：一条当"主机库里的主机"，一条当"快速连接"。
    let library = Arc::new(server.connect(&known_hosts).await.expect("连接 1"));
    let quick = Arc::new(server.connect(&known_hosts).await.expect("连接 2"));
    let browse = Arc::new(common::open_sftp(&library).await.expect("开浏览通道"));
    let remote = RemoteTempDir::create(&server, &library, &browse, "remote-lock").await;
    let local = LocalTempDir::create("remote-lock");

    let target = format!("{}/same.bin", remote.path());
    let source = local.path().join("a.bin");
    std::fs::write(&source, vec![b'A'; 40_000]).unwrap();
    let other_source = local.path().join("b.bin");
    std::fs::write(&other_source, vec![b'B'; 50_000]).unwrap();
    zssh_core::sftp::fs::write_small_text(&browse, &target, "OLD", 1024)
        .await
        .expect("先放一个旧文件（制造冲突提问）");

    // "主机库"连接上的传输引擎开始上传，卡在冲突提问：这时它已经拿了这个路径的锁。
    let resolver = Arc::new(HoldFirst::default());
    let (engine_a, mut events_a) = engine_on(&library, Arc::clone(&resolver) as _);
    let id_a = engine_a.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: target.clone(),
        recursive: false,
        peer: None,
    });
    tokio::time::timeout(Duration::from_secs(30), resolver.asked_once.notified())
        .await
        .expect("第一个引擎应该走到冲突提问");

    // "快速连接"上的传输引擎写同一个路径：必须被拒。
    let (engine_b, mut events_b) =
        engine_on(&quick, Arc::new(zssh_core::sftp::transfer::OverwriteAll));
    let id_b = engine_b.submit(TransferJob {
        direction: Direction::Upload,
        source: other_source.display().to_string(),
        target: target.clone(),
        recursive: false,
        peer: None,
    });
    let report_b = finished(&mut events_b, id_b).await;
    let TransferState::Failed(message) = &report_b.state else {
        panic!("快速连接上的传输引擎应该被拒：{report_b:?}");
    };
    assert!(message.contains("正在写"), "{message}");

    // "快速连接"上的 AI 写文件写同一个路径：同样必须被拒。
    let channel = common::open_sftp(&quick).await.expect("快速连接上开通道");
    let fs = SftpRemoteFs::new(Arc::new(channel));
    let write = replace_via_temp(
        &fs,
        &target,
        b"AI WROTE THIS",
        &ReplaceBase {
            existed: true,
            mode: None,
            uid: None,
            gid: None,
        },
        "r1",
    )
    .await;
    let err = write.expect_err("AI 写文件应该被拒，却写成功了");
    assert!(err.contains("正在写"), "{err}");

    // 放行第一个引擎：最后的内容是它的完整内容，没有被混过。
    resolver.release.notify_one();
    let report_a = finished(&mut events_a, id_a).await;
    assert_eq!(report_a.state, TransferState::Done, "{report_a:?}");
    let read = zssh_core::sftp::fs::read_bytes(&browse, &target, 1 << 20)
        .await
        .expect("读结果");
    assert_eq!(read, vec![b'A'; 40_000]);

    // 锁放掉之后，快速连接上的 AI 写文件可以写了。
    let channel = common::open_sftp(&quick).await.expect("再开通道");
    let fs = SftpRemoteFs::new(Arc::new(channel));
    replace_via_temp(
        &fs,
        &target,
        b"AI WROTE THIS",
        &ReplaceBase {
            existed: true,
            mode: None,
            uid: None,
            gid: None,
        },
        "r2",
    )
    .await
    .expect("没有别的任务在写了，应该能写");

    remote.cleanup(&library, &browse).await;
    browse.close();
    let _ = library.disconnect().await;
    let _ = quick.disconnect().await;
}
