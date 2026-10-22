//! 传输引擎的线测试（M3-1 第 3、4 节）：上传 / 下载、大文件 SHA-256、文件夹递归、
//! 进度、暂停与续传、冲突策略。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）；远端只用一个专用临时
//! 目录 `/tmp/zssh-sftp-live-<随机串>`（守卫对象兜底删除，失败 / panic 也删，M3-2 第 0 节），
//! **不碰服务器上的其它文件**。
//!
//! 门禁里只用小文件（几 MB）验证正确性；64MB 大文件和几百个文件的批量是
//! `#[ignore]` 的吞吐/批量用例，由 `scripts/perf-tests.ps1` 跑（AGENTS.md：受网速
//! 影响的测试不进五门禁）。

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use zssh_core::sftp::session::SftpChannel;
use zssh_core::sftp::transfer::backend::{SftpChannelProvider, SftpEndpoints};
use zssh_core::sftp::transfer::{
    ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction, ProgressReport,
    TransferEngine, TransferEvent, TransferJob, TransferOptions, TransferState,
};
use zssh_core::ssh::BoxFuture;

use common::{serial, LocalTempDir, RemoteTempDir, TempKnownHosts, TestServer};

/// 一个只读连接要用的东西：手上这条连接 + 一条浏览通道。
struct Harness {
    connection: Arc<zssh_core::ssh::Connection>,
    browse: Arc<SftpChannel>,
}

/// 建一个"远端临时目录 + 本地临时目录"的测试台（目录都带守卫：任何退出路径都会删）。
async fn harness(tag: &str) -> Option<(Harness, RemoteTempDir, LocalTempDir)> {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过：{reason}");
            return None;
        }
    };
    let known_hosts = TempKnownHosts::new(tag);
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let browse = Arc::new(common::open_sftp(&connection).await.expect("开浏览通道"));

    let remote = RemoteTempDir::create(&server, &connection, &browse, tag).await;
    let local = LocalTempDir::create(tag);
    Some((Harness { connection, browse }, remote, local))
}

/// 收尾：用浏览通道删掉远端临时目录（本地目录由守卫的 Drop 删）。
async fn teardown(harness: Harness, remote: &RemoteTempDir) {
    remote.cleanup(&harness.connection, &harness.browse).await;
    harness.browse.close();
    let _ = harness.connection.disconnect().await;
}

/// 建一个引擎（传输通道走 provider：在已有连接上另开一条）。
fn engine_for(
    harness: &Harness,
    resolver: Arc<dyn ConflictResolver>,
) -> (
    Arc<TransferEngine>,
    tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
) {
    let provider = Arc::new(SftpChannelProvider::new(
        Arc::clone(&harness.connection),
        None,
    ));
    let endpoints = Arc::new(SftpEndpoints::new(provider));
    TransferEngine::new(
        endpoints,
        resolver,
        TransferOptions {
            concurrency: 3,
            pipeline: true,
            in_flight: 8,
            chunk_len: None,
            preserve_mtime: false,
            preserve_permissions: false,
            max_retries: 2,
            progress_interval: Duration::from_millis(50),
            tail_window: 4096,
        },
    )
}

/// 等任务结束，顺便收集进度事件。
async fn wait_finished(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
    id: u64,
    limit: Duration,
) -> (ProgressReport, Vec<ProgressReport>) {
    let mut progress = Vec::new();
    let deadline = Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "等不到任务 {id} 结束；已收到 {progress:?}"
        );
        match tokio::time::timeout(remaining.min(Duration::from_secs(5)), events.recv()).await {
            Ok(Some(TransferEvent::Progress(report))) if report.id == id => progress.push(report),
            Ok(Some(TransferEvent::Finished(report))) if report.id == id => {
                return (report, progress)
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("事件通道关了"),
            Err(_) => {}
        }
    }
}

/// 写一个随机文件（内容用固定种子，方便复查）。
fn write_random(path: &std::path::Path, size: usize) {
    let mut data = Vec::with_capacity(size);
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
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

fn local_sha256(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).expect("读文件");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

/// 服务器目录里 `<name>` 的临时文件（`<name>.<任务标识>.zssh-part`）：(文件名, 大小)。
async fn part_files(harness: &Harness, dir: &str, name: &str) -> Vec<(String, u64)> {
    let prefix = format!("{name}.");
    harness
        .browse
        .read_dir(dir)
        .await
        .expect("列目录")
        .into_iter()
        .filter(|entry| entry.name.starts_with(&prefix) && entry.name.ends_with(".zssh-part"))
        .map(|entry| (entry.name, entry.size.unwrap_or(0)))
        .collect()
}

/// 远端用 `sha256sum` 算（服务器上算，和本地对比）。
async fn remote_sha256(harness: &Harness, path: &str) -> String {
    let output = harness
        .connection
        .exec(&format!("sha256sum '{path}' | cut -d' ' -f1"))
        .await
        .expect("远端 sha256sum");
    output.trim().to_owned()
}

/// 全量覆盖的冲突策略（测试默认）。
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

/// 上传再下载一个随机文件，两头 SHA-256 对账（`size` 由调用方定：门禁用几 MB，
/// 64MB 的吞吐用例是 `#[ignore]` 的，见文件头）。
async fn upload_download_roundtrip(tag: &str, size: usize) {
    let Some((harness, remote, local)) = harness(tag).await else {
        return;
    };

    let source = local.join("payload.bin");
    write_random(&source, size);
    let local_hash = local_sha256(&source);

    let (engine, mut events) = engine_for(&harness, Arc::new(OverwriteEverything));
    let started = Instant::now();
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/payload.bin", remote.path()),
        recursive: false,
        peer: None,
    });
    let (report, progress) = wait_finished(&mut events, id, Duration::from_secs(600)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(report.total_bytes, size as u64);
    assert_eq!(report.done_bytes, size as u64);
    assert!(!progress.is_empty(), "要有进度事件");
    let last_progress = progress.last().expect("进度");
    assert!(
        last_progress.done_bytes >= last_progress.total_bytes / 2,
        "进度要接近完成：{last_progress:?}"
    );
    println!(
        "上传 {}MB：用时 {:.1}s（{:.1} MB/s），进度事件 {} 条",
        size as f64 / 1024.0 / 1024.0,
        started.elapsed().as_secs_f64(),
        report.speed_bps as f64 / 1024.0 / 1024.0,
        progress.len()
    );

    // 远端算出来的哈希必须和本地一致。
    let remote_hash = remote_sha256(&harness, &format!("{}/payload.bin", remote.path())).await;
    assert_eq!(remote_hash, local_hash, "上传之后 SHA-256 要一致");
    println!("上传 SHA-256 一致：{}", &local_hash[..16]);

    // 下载回来再比一次。
    let back = local.join("payload-back.bin");
    let id = engine.submit(TransferJob {
        direction: Direction::Download,
        source: format!("{}/payload.bin", remote.path()),
        target: back.display().to_string(),
        recursive: false,
        peer: None,
    });
    let (report, _) = wait_finished(&mut events, id, Duration::from_secs(600)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(local_sha256(&back), local_hash, "下载回来也要一致");
    println!("下载 SHA-256 一致：{}", &local_hash[..16]);

    teardown(harness, &remote).await;
}

/// 门禁用的小文件用例（几 MB 就够验证正确性，M3-2 第 0 节）。
#[tokio::test]
async fn upload_and_download_a_file_with_sha256() {
    let _serial = serial().await;
    upload_download_roundtrip("transfer-small", 4 * 1024 * 1024).await;
}

/// 64MB 大文件（受网速影响，不进五门禁）：`scripts/perf-tests.ps1` 跑。
#[tokio::test]
#[ignore = "大文件吞吐：依赖网速，用 scripts/perf-tests.ps1 跑"]
async fn upload_and_download_64mb_with_sha256() {
    if !common::full_live("upload_and_download_64mb_with_sha256") {
        return;
    }
    let _serial = serial().await;
    upload_download_roundtrip("transfer-64mb", 64 * 1024 * 1024).await;
}

/// 文件夹树的上传 / 下载对账（`count` 个小文件 + 多层 + 空目录）。
async fn folder_tree_roundtrip(tag: &str, count: usize) {
    let Some((harness, remote, local)) = harness(tag).await else {
        return;
    };

    let source = local.join("tree");
    std::fs::create_dir_all(source.join("sub").join("deep")).unwrap();
    std::fs::create_dir_all(source.join("empty-dir")).unwrap();
    std::fs::create_dir_all(source.join("many")).unwrap();
    std::fs::write(source.join("top.txt"), b"top").unwrap();
    std::fs::write(source.join("sub").join("mid.txt"), b"mid").unwrap();
    std::fs::write(
        source.join("sub").join("deep").join("bottom.bin"),
        vec![7u8; 4096],
    )
    .unwrap();
    for index in 0..count {
        std::fs::write(
            source.join("many").join(format!("f-{index:03}.txt")),
            format!("file {index}"),
        )
        .unwrap();
    }

    let total = count + 3;
    // 先看一眼展开出来的东西（排查用）。
    let plan = zssh_core::sftp::transfer::plan::plan_upload(&source, remote.path(), true)
        .expect("展开上传计划");
    println!(
        "计划：{} 个文件（期望 {total}）、目录 {:?}",
        plan.files.len(),
        plan.dirs.len()
    );
    assert_eq!(plan.files.len(), total, "顶层 2 个 + deep 1 个 + many 里的");

    let (engine, mut events) = engine_for(&harness, Arc::new(OverwriteEverything));
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: remote.path().to_owned(),
        recursive: true,
        peer: None,
    });
    let (report, _) = wait_finished(&mut events, id, Duration::from_secs(600)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(report.total_files, total as u64);
    assert_eq!(report.done_files, report.total_files);
    println!("文件夹上传：{} 个文件", report.done_files);

    // 远端对账：文件数 + 抽查内容（含空目录存在）。
    let listing = harness
        .connection
        .exec(&format!(
            "cd {} && find . -type f | wc -l && find . -type d | wc -l && cat sub/deep/bottom.bin | wc -c && cat many/f-{:03}.txt",
            remote.path(),
            count - 1
        ))
        .await
        .expect("远端对账");
    let lines: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    println!(
        "远端：文件 {} 个、目录 {} 个、bottom.bin {} 字节、最后一个={}",
        lines[0], lines[1], lines[2], lines[3]
    );
    assert_eq!(lines[0], total.to_string(), "文件数要一致");
    assert!(
        lines[1].parse::<u32>().expect("目录数") >= 5,
        "目录要建全（含空目录）：{}",
        lines[1]
    );
    assert_eq!(lines[2], "4096");
    assert_eq!(lines[3], format!("file {}", count - 1));

    // 下载回另一个本地目录，逐个比对。
    let back = local.join("tree-back");
    let id = engine.submit(TransferJob {
        direction: Direction::Download,
        source: remote.path().to_owned(),
        target: back.display().to_string(),
        recursive: true,
        peer: None,
    });
    let (report, _) = wait_finished(&mut events, id, Duration::from_secs(600)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(report.total_files, total as u64);

    for relative in [
        "top.txt",
        "sub/mid.txt",
        "sub/deep/bottom.bin",
        "many/f-000.txt",
    ] {
        let original = std::fs::read(source.join(relative)).expect("原文件");
        let copied = std::fs::read(back.join(relative))
            .unwrap_or_else(|err| panic!("{relative} 没下载回来：{err}"));
        assert_eq!(original, copied, "{relative} 内容要一致");
    }
    assert!(back.join("empty-dir").is_dir(), "空目录也要建出来");
    println!("下载回来逐项比对通过（含空目录）");

    teardown(harness, &remote).await;
}

/// 门禁用的小规模文件夹用例（20 个小文件 + 多层 + 空目录：每个文件要好几个往返，
/// 文件多了门禁就被链路速度拖慢；300 个的批量在下面 #[ignore] 里）。
#[tokio::test]
async fn upload_and_download_a_folder_tree() {
    if !common::full_live("upload_and_download_a_folder_tree") {
        return;
    }
    let _serial = serial().await;
    folder_tree_roundtrip("transfer-dir", 20).await;
}

/// 几百个文件的批量（受网速 / 往返延迟影响，不进五门禁）。
#[tokio::test]
#[ignore = "批量吞吐：依赖网速和往返延迟，用 scripts/perf-tests.ps1 跑"]
async fn upload_and_download_a_folder_tree_many_files() {
    if !common::full_live("upload_and_download_a_folder_tree_many_files") {
        return;
    }
    let _serial = serial().await;
    folder_tree_roundtrip("transfer-dir-many", 300).await;
}

#[tokio::test]
async fn pause_then_resume_finishes_with_the_same_hash() {
    if !common::full_live("pause_then_resume_finishes_with_the_same_hash") {
        return;
    }
    let _serial = serial().await;
    let Some((harness, remote, local)) = harness("transfer-pause").await else {
        return;
    };

    // 8MB（门禁用小文件，M3-2 第 0 节）：暂停在批之间生效，一块批约 2MB，
    // 文件太小会在暂停生效之前就传完，8MB 够停得住。
    let size = 8 * 1024 * 1024;
    let source = local.join("paused.bin");
    write_random(&source, size);
    let local_hash = local_sha256(&source);

    let (engine, mut events) = engine_for(&harness, Arc::new(OverwriteEverything));
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/paused.bin", remote.path()),
        recursive: false,
        peer: None,
    });

    // 等到传了一部分就暂停。
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut paused_at = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            // 传了一部分就暂停（后面还有大半，肯定停得住）。
            Ok(Some(TransferEvent::Progress(report)))
                if report.id == id && report.done_bytes > 0 =>
            {
                assert!(engine.pause(id), "暂停要成功");
                paused_at = Some(report.done_bytes);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    let paused_at = paused_at.expect("要能在中途暂停");
    println!(
        "暂停时已传 {paused_at} 字节；当前状态 {:?}",
        engine.state(id)
    );
    if let Some(report) = engine.report(id) {
        println!("报告：{report:?}");
    }

    // 等状态变成 Paused（可能先经过 Running），并且临时文件在。
    let deadline = Instant::now() + Duration::from_secs(60);
    while engine.state(id) != Some(TransferState::Paused) && Instant::now() < deadline {
        let state = engine.state(id);
        assert!(
            state == Some(TransferState::Running)
                || state == Some(TransferState::Queued)
                || state == Some(TransferState::Planning),
            "暂停过程中不该跑到别的状态：{state:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        engine.state(id),
        Some(TransferState::Paused),
        "要停在暂停状态"
    );
    let parts = part_files(&harness, remote.path(), "paused.bin").await;
    assert_eq!(
        parts.len(),
        1,
        "应该正好有这个任务自己的一个临时文件：{parts:?}"
    );
    let part_len = parts[0].1;
    println!("临时文件 {part_len} 字节（续传的起点）");
    assert!(part_len > 0, "暂停之后临时文件要留着");
    assert!(
        harness
            .browse
            .lstat(&format!("{}/paused.bin", remote.path()))
            .await
            .is_err(),
        "没传完之前不该出现最终文件"
    );

    // 继续：从临时文件接着传，最终哈希一致。
    assert!(engine.resume(id), "续传要成功");
    let (report, _) = wait_finished(&mut events, id, Duration::from_secs(300)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        remote_sha256(&harness, &format!("{}/paused.bin", remote.path())).await,
        local_hash,
        "暂停续传之后 SHA-256 要一致"
    );
    // 临时文件已经被改名掉。
    assert!(
        part_files(&harness, remote.path(), "paused.bin")
            .await
            .is_empty(),
        "收尾之后不该还有临时文件"
    );
    println!("暂停续传完成，SHA-256 一致：{}", &local_hash[..16]);

    teardown(harness, &remote).await;
}

#[tokio::test]
async fn cancel_stops_the_transfer_and_cleans_the_part_file() {
    if !common::full_live("cancel_stops_the_transfer_and_cleans_the_part_file") {
        return;
    }
    let _serial = serial().await;
    let Some((harness, remote, local)) = harness("transfer-cancel").await else {
        return;
    };
    let source = local.join("cancelled.bin");
    write_random(&source, 8 * 1024 * 1024);

    let (engine, mut events) = engine_for(&harness, Arc::new(OverwriteEverything));
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/cancelled.bin", remote.path()),
        recursive: false,
        peer: None,
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Some(TransferEvent::Progress(report)))
                if report.id == id && report.done_bytes > 0 =>
            {
                assert!(engine.cancel(id));
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    let (report, _) = wait_finished(&mut events, id, Duration::from_secs(120)).await;
    println!("取消之后的状态：{:?}", report.state);
    assert!(
        matches!(
            report.state,
            TransferState::Cancelled | TransferState::Failed(_)
        ),
        "{report:?}"
    );
    assert!(
        harness
            .browse
            .lstat(&format!("{}/cancelled.bin", remote.path()))
            .await
            .is_err(),
        "取消之后不该有最终文件"
    );
    assert!(
        part_files(&harness, remote.path(), "cancelled.bin")
            .await
            .is_empty(),
        "取消之后临时文件也要删掉"
    );

    teardown(harness, &remote).await;
}

// ---- C05：开始时目标不存在、传输期间别的程序在目标位置建了同名文件 ----

mod c05 {
    use std::sync::{Arc, Mutex};

    use zssh_core::sftp::session::SftpChannel;
    use zssh_core::sftp::transfer::backend::{FilePair, RemoteLister, TransferEndpoints};
    use zssh_core::sftp::transfer::endpoint::{
        EndpointFuture, FileAttributesPair, FileEndpoint, ReadAt, WriteAt,
    };
    use zssh_core::sftp::transfer::plan::TransferPlan;
    use zssh_core::sftp::transfer::{
        ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction,
    };
    use zssh_core::sftp::SftpError;
    use zssh_core::ssh::BoxFuture;

    /// 真实的数据面外面包一层：第一次收尾之前，用浏览通道在目标位置写一个文件
    /// （模拟别的程序在传输期间建了同名文件）。其余全部原样转给真实实现。
    pub struct AppearBeforeCommit {
        pub inner: Arc<dyn TransferEndpoints>,
        pub browse: Arc<SftpChannel>,
        pub contents: &'static [u8],
        pub done: Arc<Mutex<bool>>,
    }

    struct Target {
        inner: Arc<dyn FileEndpoint>,
        path: String,
        browse: Arc<SftpChannel>,
        contents: &'static [u8],
        done: Arc<Mutex<bool>>,
    }

    impl FileEndpoint for Target {
        fn describe(&self) -> String {
            self.inner.describe()
        }
        fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
            self.inner.size()
        }
        fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>> {
            self.inner.open_read()
        }
        fn open_write<'a>(
            &'a self,
            resume_len: u64,
        ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>> {
            self.inner.open_write(resume_len)
        }
        fn commit<'a>(
            &'a self,
            total: u64,
            overwrite: bool,
        ) -> EndpointFuture<'a, Result<(), SftpError>> {
            Box::pin(async move {
                let first = {
                    let mut done = self.done.lock().unwrap();
                    !std::mem::replace(&mut *done, true)
                };
                if first {
                    zssh_core::sftp::fs::write_bytes(
                        &self.browse,
                        &self.path,
                        self.contents,
                        1 << 20,
                    )
                    .await
                    .expect("别的程序建同名文件");
                }
                self.inner.commit(total, overwrite).await
            })
        }
        fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
            self.inner.existing_size()
        }
        fn existing_mtime<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u32>, SftpError>> {
            self.inner.existing_mtime()
        }
        fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.remove_existing()
        }
        fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.remove_partial()
        }
        fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>> {
            self.inner.partial_len()
        }
        fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.truncate_partial(len)
        }
        fn read_partial<'a>(
            &'a self,
            offset: u64,
            len: u32,
        ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>> {
            self.inner.read_partial(offset, len)
        }
        fn set_attributes<'a>(
            &'a self,
            mtime: Option<u32>,
            permissions: Option<u32>,
        ) -> EndpointFuture<'a, Result<(), SftpError>> {
            self.inner.set_attributes(mtime, permissions)
        }
        fn attributes<'a>(&'a self) -> EndpointFuture<'a, Result<FileAttributesPair, SftpError>> {
            self.inner.attributes()
        }
    }

    impl RemoteLister for AppearBeforeCommit {
        fn list_dir<'a>(
            &'a self,
            path: &'a str,
        ) -> BoxFuture<'a, Result<Vec<zssh_core::sftp::fs::DirEntry>, SftpError>> {
            self.inner.list_dir(path)
        }
        fn stat_remote<'a>(
            &'a self,
            path: &'a str,
        ) -> BoxFuture<'a, Result<russh_sftp::protocol::FileAttributes, SftpError>> {
            self.inner.stat_remote(path)
        }
    }

    impl TransferEndpoints for AppearBeforeCommit {
        fn pair<'a>(
            &'a self,
            direction: Direction,
            peer: Option<&'a str>,
            source: &'a str,
            target: &'a str,
            owner: &'a str,
        ) -> BoxFuture<'a, Result<FilePair, SftpError>> {
            Box::pin(async move {
                let pair = self
                    .inner
                    .pair(direction, peer, source, target, owner)
                    .await?;
                Ok(FilePair {
                    source: pair.source,
                    target: Arc::new(Target {
                        inner: pair.target,
                        path: target.to_owned(),
                        browse: Arc::clone(&self.browse),
                        contents: self.contents,
                        done: Arc::clone(&self.done),
                    }),
                })
            })
        }
        fn plan_remote_copy<'a>(
            &'a self,
            peer: Option<&'a str>,
            source: &'a str,
            target: &'a str,
            recursive: bool,
        ) -> BoxFuture<'a, Result<TransferPlan, SftpError>> {
            self.inner.plan_remote_copy(peer, source, target, recursive)
        }
        fn create_target_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
            self.inner.create_target_dir(path)
        }
        fn reset<'a>(&'a self) -> BoxFuture<'a, ()> {
            self.inner.reset()
        }
        fn suggested_chunk_len(&self) -> u32 {
            self.inner.suggested_chunk_len()
        }
        fn server_identity<'a>(&'a self) -> BoxFuture<'a, Result<String, SftpError>> {
            self.inner.server_identity()
        }
    }

    /// 按固定动作回答冲突，记下问的时候目标多大。
    pub struct Answer {
        pub action: ConflictAction,
        pub asked: Mutex<Vec<Option<u64>>>,
    }

    impl ConflictResolver for Answer {
        fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            self.asked.lock().unwrap().push(info.target_size);
            let action = self.action;
            Box::pin(async move {
                ConflictDecision {
                    action,
                    apply_to_all: false,
                    rename_to: None,
                }
            })
        }
    }
}

/// C05：开始上传时目标不存在（不会问），传完准备收尾之前别的程序在服务器上建了同名文件。
/// 收尾必须用不覆盖的改名：服务器拒绝后按冲突重新问；选跳过时那个文件原样保留、
/// 自己的临时文件清掉；选覆盖时才替换成上传的内容。
#[tokio::test]
async fn a_remote_file_created_during_upload_is_not_overwritten_without_asking() {
    if !common::full_live("a_remote_file_created_during_upload_is_not_overwritten_without_asking") {
        return;
    }
    let _serial = serial().await;
    let Some((harness, remote, local)) = harness("transfer-c05").await else {
        return;
    };
    let source = local.join("report.bin");
    write_random(&source, 256 * 1024);
    let local_hash = local_sha256(&source);
    const THEIRS: &[u8] = b"created by another program";

    for action in [ConflictAction::Skip, ConflictAction::Overwrite] {
        let target = format!("{}/report-{action:?}.bin", remote.path());
        let provider = Arc::new(SftpChannelProvider::new(
            Arc::clone(&harness.connection),
            None,
        ));
        let endpoints = Arc::new(c05::AppearBeforeCommit {
            inner: Arc::new(SftpEndpoints::new(provider)),
            browse: Arc::clone(&harness.browse),
            contents: THEIRS,
            done: Arc::new(std::sync::Mutex::new(false)),
        });
        let resolver = Arc::new(c05::Answer {
            action,
            asked: std::sync::Mutex::new(Vec::new()),
        });
        let (engine, mut events) = TransferEngine::new(
            endpoints,
            Arc::clone(&resolver) as Arc<dyn ConflictResolver>,
            TransferOptions {
                concurrency: 1,
                pipeline: true,
                in_flight: 4,
                chunk_len: None,
                preserve_mtime: false,
                preserve_permissions: false,
                max_retries: 2,
                progress_interval: Duration::from_millis(50),
                tail_window: 4096,
            },
        );
        let id = engine.submit(TransferJob {
            direction: Direction::Upload,
            source: source.display().to_string(),
            target: target.clone(),
            recursive: false,
            peer: None,
        });
        let (report, _) = wait_finished(&mut events, id, Duration::from_secs(120)).await;
        let asked = resolver.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![Some(THEIRS.len() as u64)],
            "{action:?}：开始时不问，收尾时发现冲突要问一次"
        );
        if action == ConflictAction::Skip {
            assert_eq!(report.state, TransferState::Skipped, "{report:?}");
            let now = zssh_core::sftp::fs::read_bytes(&harness.browse, &target, 1 << 20)
                .await
                .expect("读目标");
            assert_eq!(now, THEIRS, "别的程序建的文件被盖掉了");
        } else {
            assert_eq!(report.state, TransferState::Done, "{report:?}");
            assert_eq!(remote_sha256(&harness, &target).await, local_hash);
        }
        let name = target.rsplit('/').next().unwrap_or_default().to_owned();
        assert!(
            part_files(&harness, remote.path(), &name).await.is_empty(),
            "{action:?}：不能留下临时文件"
        );
    }

    teardown(harness, &remote).await;
}
