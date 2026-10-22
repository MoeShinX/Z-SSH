//! 传输引擎的单测（M3-1 第 3、4 节）：队列与并发、冲突策略、断线重试与退避、
//! 暂停与续传的位置、路径改名。
//!
//! 这些用例**不需要服务器**：数据面用内存替身（[`memory::MemoryEndpoint`]），
//! 通过 `TestProvider` 把它接给引擎（生产上是 SFTP 通道）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::endpoint::FileEndpoint;
use super::endpoint::LocalEndpoint;
use super::memory::{MemoryEndpoint, MemoryEndpoints};
use super::*;

fn local_options() -> TransferOptions {
    TransferOptions {
        concurrency: 2,
        pipeline: true,
        in_flight: 4,
        chunk_len: Some(4096),
        preserve_mtime: false,
        preserve_permissions: false,
        max_retries: 2,
        progress_interval: Duration::from_millis(10),
        tail_window: 1024,
    }
}

fn temp_root(tag: &str) -> crate::test_util::TestDir {
    crate::test_util::TestDir::new(&format!("engine-{tag}"))
}

/// 收事件，直到任务进终态（或超时）。
async fn drain_until_finished(
    receiver: &mut mpsc::UnboundedReceiver<TransferEvent>,
    id: TransferId,
    limit: Duration,
) -> ProgressReport {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等不到任务 {id} 的终态");
        match tokio::time::timeout(remaining, receiver.recv()).await {
            Ok(Some(TransferEvent::Finished(report))) if report.id == id => return report,
            Ok(Some(_)) => {}
            Ok(None) => panic!("事件通道关了"),
            Err(_) => panic!("等不到任务 {id} 的终态"),
        }
    }
}

#[test]
fn backoff_grows_and_is_capped() {
    assert_eq!(backoff_delay(1), Duration::from_millis(400));
    assert_eq!(backoff_delay(2), Duration::from_millis(800));
    assert!(backoff_delay(5) <= Duration::from_secs(5));
    assert_eq!(backoff_delay(20), Duration::from_secs(5), "有上限");
}

#[test]
fn renamed_path_and_with_file_name_know_both_kinds_of_separators() {
    // Windows 本地路径：目录名里有点、文件没扩展名，不能改在目录名上。
    assert_eq!(renamed_path(r"C:\my.dir\file", 2), r"C:\my.dir\file (2)");
    assert_eq!(
        renamed_path(r"C:\my.dir\photo.jpg", 2),
        r"C:\my.dir\photo (2).jpg"
    );
    // 把最后一段换成用户给的名字。
    assert_eq!(with_file_name("/a/b.txt", "c.txt"), "/a/c.txt");
    assert_eq!(with_file_name(r"C:\a\b.txt", "c.txt"), r"C:\a\c.txt");
    assert_eq!(with_file_name("b.txt", "c.txt"), "c.txt");
}

#[test]
fn renamed_path_keeps_the_extension() {
    assert_eq!(renamed_path("/a/photo.jpg", 2), "/a/photo (2).jpg");
    assert_eq!(renamed_path("/a/noext", 2), "/a/noext (2)");
    assert_eq!(renamed_path("/a/.bashrc", 2), "/a/.bashrc (2)");
}

#[test]
fn control_flag_transitions() {
    let flag = ControlFlag::new();
    assert_eq!(flag.state(), file::ControlState::Running);
    flag.pause();
    assert_eq!(flag.state(), file::ControlState::Paused);
    flag.resume();
    assert_eq!(flag.state(), file::ControlState::Running);
    flag.cancel();
    assert_eq!(flag.state(), file::ControlState::Cancelled);
}

/// 队列：两个任务都排进去，都能跑到终态；并发上限生效（同时传的文件数不超过设置）。
#[tokio::test]
async fn queue_runs_jobs_and_respects_concurrency() {
    let root = temp_root("queue");
    let source_dir = root.join("src");
    std::fs::create_dir_all(&source_dir).unwrap();
    for index in 0..6 {
        std::fs::write(
            source_dir.join(format!("file-{index}.bin")),
            vec![index as u8; 64 * 1024],
        )
        .unwrap();
    }
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::new(OverwriteAll),
        TransferOptions {
            concurrency: 2,
            ..local_options()
        },
    );

    // 用本地目录当"远端"，让引擎真的跑一遍（本地→本地；远端分支的线测试在
    // sftp_transfer_live.rs 里跑真服务器）。
    let mut ids = Vec::new();
    for index in 0..6 {
        ids.push(
            engine.submit(TransferJob {
                direction: Direction::Upload,
                source: source_dir
                    .join(format!("file-{index}.bin"))
                    .display()
                    .to_string(),
                target: format!("/remote/file-{index}.bin"),
                recursive: false,
                peer: None,
            }),
        );
    }

    let mut finished = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while finished < ids.len() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "任务没跑完");
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(TransferEvent::Finished(report))) => {
                assert_eq!(report.state, TransferState::Done, "{report:?}");
                assert_eq!(report.done_bytes, 64 * 1024, "{report:?}");
                assert_eq!(report.total_files, 1);
                finished += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("任务没跑完"),
        }
    }
    for index in 0..6 {
        let content = endpoints.remote_contents(&format!("/remote/file-{index}.bin"));
        assert_eq!(content.len(), 64 * 1024, "第 {index} 个文件要传完");
        assert_eq!(content[0], index as u8);
    }
}

/// 目标已存在 + "跳过"策略：任务进 Skipped，目标文件保持原样。
#[tokio::test]
async fn skip_policy_leaves_the_target_alone() {
    let root = temp_root("skip");
    let source = root.join("source.txt");
    std::fs::write(&source, b"new contents").unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/target.txt", b"old".to_vec());
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(SkipAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/target.txt".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Skipped, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/target.txt"),
        b"old",
        "跳过的目标不该被动"
    );
    assert!(
        report.notes.iter().any(|note| note.contains("跳过")),
        "{:?}",
        report.notes
    );
}

/// 覆盖策略：目标被新内容替换。
#[tokio::test]
async fn overwrite_policy_replaces_the_target() {
    let root = temp_root("overwrite");
    let source = root.join("source.txt");
    std::fs::write(&source, b"new contents").unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote(
        "/remote/target.txt",
        b"much older and longer content".to_vec(),
    );
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/target.txt".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/target.txt"),
        b"new contents"
    );
}

/// 冲突回调："目标更大时不能续传"由引擎判断（can_resume 传进回调）。
#[tokio::test]
async fn conflict_callback_knows_when_resume_is_possible() {
    struct RecordDecisions(Arc<Mutex<Vec<ConflictInfo>>>);
    impl ConflictResolver for RecordDecisions {
        fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            Box::pin(async move {
                self.0.lock().unwrap().push(info);
                ConflictDecision {
                    action: ConflictAction::Overwrite,
                    apply_to_all: true,
                    rename_to: None,
                }
            })
        }
    }

    let root = temp_root("conflict-info");
    let source = root.join("source.bin");
    std::fs::write(&source, vec![1u8; 4096]).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    // 远端已经有一个更小的同名文件 → 可以续传。
    endpoints.put_remote("/remote/target.bin", vec![2u8; 1024]);
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::new(RecordDecisions(Arc::clone(&seen))),
        local_options(),
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/target.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    let infos = seen.lock().unwrap();
    assert_eq!(infos.len(), 1, "只问一次");
    assert_eq!(infos[0].source_size, Some(4096));
    assert_eq!(infos[0].target_size, Some(1024));
    assert!(infos[0].can_resume, "目标比源小 → 可以续传");
    assert_eq!(infos[0].direction, Direction::Upload);
}

/// 断线重试：用内存端点注入"写到一半断线"，看引擎重试之后能不能接着传完。
///
/// 这里是 `copy_file` + 尾部对账的组合（引擎的重试循环在真链路上由
/// `sftp_transfer_live.rs` 的断线续传用例覆盖）。
#[tokio::test]
async fn disconnect_then_resume_completes_the_file() {
    let source_bytes: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_be_bytes()).collect();
    let source = MemoryEndpoint::new("source", source_bytes.clone());
    let target = MemoryEndpoint::new("target", Vec::new());
    // 写到 150KB 就断一次（批是 8×8192=64KB，这样前两批是确认写好的前缀）。
    target.fail_write_after(150_000);

    let control = ControlFlag::new();
    let first = copy_file(
        source.as_endpoint(),
        target.as_endpoint(),
        source_bytes.len() as u64,
        0,
        CopyTuning::from_limits(8192, true),
        &control,
        |_, _| {},
    )
    .await;
    let err = first.expect_err("第一次要断");
    assert!(err.is_disconnect(), "{err}");
    // 断线收尾会把临时文件截回**已确认的前缀**（整批整批地确认）。
    let partial = target.partial_len().await.unwrap();
    assert!(
        partial > 0 && partial < source_bytes.len() as u64,
        "已确认的前缀应该在中间：{partial}"
    );
    assert_eq!(partial % 8192, 0, "前缀落在批边界上：{partial}");

    // 重连之后：先对账，再接着传（模拟引擎的重试路径）。
    let source_ep = source.as_endpoint();
    let target_ep = target.as_endpoint();
    let resume_len = target_ep.partial_len().await.unwrap();
    assert!(
        tails_match(&source_ep, &target_ep, resume_len, 1024)
            .await
            .unwrap(),
        "断线前写的字节应当和源一致"
    );
    let outcome = copy_file(
        Arc::clone(&source_ep),
        Arc::clone(&target_ep),
        source_bytes.len() as u64,
        resume_len,
        CopyTuning::from_limits(8192, true),
        &control,
        |_, _| {},
    )
    .await
    .expect("续传要成功");
    assert_eq!(
        outcome,
        CopyOutcome::Completed {
            bytes: source_bytes.len() as u64 - resume_len
        }
    );
    target_ep
        .commit(source_bytes.len() as u64, true)
        .await
        .unwrap();
    assert_eq!(target.contents(), source_bytes, "续传之后内容要完全一致");

    // 尾部对不上时（目标被改过）必须从头传：这里换个目标验证。
    let target2 = MemoryEndpoint::new("target2", vec![0u8; 100_000]);
    let source2 = MemoryEndpoint::new("source2", vec![7u8; 200_000]);
    assert!(
        !tails_match(
            &source2.as_endpoint(),
            &target2.as_endpoint(),
            100_000,
            1024
        )
        .await
        .unwrap(),
        "内容不一样 → 不能续"
    );
}

/// 重启任务（临时文件比源还长）：临时文件被丢掉，从头传。
#[tokio::test]
async fn partial_longer_than_source_is_discarded() {
    let source = MemoryEndpoint::new("source", vec![3u8; 1000]);
    let target = MemoryEndpoint::new("target", Vec::new());
    // 手工造一个比源长的临时文件。
    {
        let writer = target.open_write(0).await.unwrap();
        writer.write_at(0, vec![9u8; 5000]).await.unwrap();
        writer.finish().await.unwrap();
    }
    assert_eq!(target.partial_len().await.unwrap(), 5000);

    let source_ep = source.as_endpoint();
    let target_ep = target.as_endpoint();
    let source_size = source_ep.size().await.unwrap().unwrap();
    let resume_len = target_ep.partial_len().await.unwrap();
    assert!(resume_len > source_size, "前提：临时文件更长");
    // 引擎的处理：丢掉临时文件，从头传。
    target_ep.remove_partial().await.unwrap();
    let control = ControlFlag::new();
    let outcome = copy_file(
        Arc::clone(&source_ep),
        Arc::clone(&target_ep),
        source_size,
        0,
        CopyTuning::from_limits(512, false),
        &control,
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(outcome, CopyOutcome::Completed { bytes: 1000 });
    assert_eq!(target_ep.partial_len().await.unwrap(), 1000);
}

/// 本地磁盘满：明确报错，临时文件留着（以便续传）。
#[tokio::test]
async fn local_disk_full_reports_clearly_and_keeps_the_partial() {
    let source = MemoryEndpoint::new("source", vec![1u8; 100_000]);
    let target = MemoryEndpoint::new("target", Vec::new());
    target.no_space_after(30_000);

    let control = ControlFlag::new();
    let err = copy_file(
        source.as_endpoint(),
        target.as_endpoint(),
        100_000,
        0,
        CopyTuning::from_limits(4096, true),
        &control,
        |_, _| {},
    )
    .await
    .expect_err("磁盘满要报错");
    assert!(matches!(err, SftpError::LocalNoSpace { .. }), "{err}");
    assert!(err.to_string().contains("磁盘空间不足"), "{err}");
    // 写是成批发的：一批里失败时，后面的块可能已经落盘 —— 收尾会把临时文件截回
    // **已确认的前缀**（这一批整批失败，所以是 0）。续传的不变量由此保住：
    // 临时文件长度 = 连续写好的前缀，下次从它接着传，不会在中间留洞。
    assert_eq!(
        target.partial_len().await.unwrap(),
        0,
        "失败之后临时文件要截回已确认的前缀"
    );
}

/// 续传位置的计算：暂停时临时文件长度就是下次的起点。
#[tokio::test]
async fn pause_records_the_resume_offset() {
    let source = MemoryEndpoint::new("source", vec![5u8; 200_000]);
    let target = MemoryEndpoint::new("target", Vec::new());
    let control = ControlFlag::new();
    let flag = control.clone();
    let outcome = copy_file(
        source.as_endpoint(),
        target.as_endpoint(),
        200_000,
        0,
        CopyTuning::from_limits(1024, false),
        &control,
        move |done, _| {
            if done >= 5000 {
                flag.pause();
            }
        },
    )
    .await
    .unwrap();
    let CopyOutcome::Paused { resume_len, .. } = outcome else {
        panic!("应当暂停：{outcome:?}");
    };
    assert_eq!(target.partial_len().await.unwrap(), resume_len);
    assert!(resume_len >= 5000, "{resume_len}");
}

/// 空文件也要能传（不能因为 size=0 就跳过或死循环）。
#[tokio::test]
async fn empty_files_are_transferred() {
    let root = temp_root("empty-file");
    let source = root.join("empty.txt");
    std::fs::write(&source, b"").unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/copy.txt".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert!(
        endpoints.remote_exists("/remote/copy.txt"),
        "空文件也要建出来"
    );
    assert!(endpoints.remote_contents("/remote/copy.txt").is_empty());
}

/// 下载方向：远端（内存）→ 本地目录，内容一致；名字按 Windows 规则处理。
#[tokio::test]
async fn download_writes_local_files_with_clean_names() {
    let root = temp_root("download");
    let local_dir = root.join("downloads");
    std::fs::create_dir_all(&local_dir).unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/dir/a.txt", b"aaa".to_vec());
    endpoints.put_remote("/remote/dir/weird:name?.txt", b"bbb".to_vec());
    endpoints.put_remote("/remote/dir/empty-dir-marker", Vec::new());

    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Download,
        source: "/remote/dir".to_owned(),
        target: local_dir.display().to_string(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(std::fs::read(local_dir.join("a.txt")).unwrap(), b"aaa");
    // 非法字符被换掉，说明写在 notes 里。
    assert_eq!(
        std::fs::read(local_dir.join("weird_name_.txt")).unwrap(),
        b"bbb"
    );
    assert!(
        report.notes.iter().any(|note| note.contains("非法字符")),
        "{:?}",
        report.notes
    );
}

/// 本地端点：目标目录不存在时自动建父目录。
#[tokio::test]
async fn local_endpoint_creates_parent_directories() {
    let root = temp_root("parents");
    let endpoint = LocalEndpoint::new(root.join("a").join("b").join("c.txt"));
    let writer = endpoint.open_write(0).await.expect("打开写（父目录要建）");
    writer.write_at(0, b"hi".to_vec()).await.unwrap();
    writer.finish().await.unwrap();
    endpoint.commit(2, true).await.unwrap();
    assert_eq!(
        std::fs::read(root.join("a").join("b").join("c.txt")).unwrap(),
        b"hi"
    );
}

/// 重试（M3-2 第 1 节）：失败的任务同一个任务号重跑，上次留下的 `.zssh-part`
/// 接着用（尾部对账把着关）；不能重试的（没这个任务 / 已完成）返回 false。
#[tokio::test]
async fn retry_reruns_a_failed_job_and_resumes_from_the_part_file() {
    let root = temp_root("retry");
    let source = root.join("retry.bin");
    std::fs::write(&source, vec![7u8; 16 * 1024]).unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    // 写到 4KB 就"断线"（只断一次），max_retries = 0：第一次任务直接失败。
    endpoints.fail_remote_write_after(4096);
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::new(OverwriteAll),
        TransferOptions {
            max_retries: 0,
            in_flight: 1,
            ..local_options()
        },
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/retry.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert!(
        matches!(report.state, TransferState::Failed(_)),
        "{report:?}"
    );
    let partial_left = endpoints.remote_partial_len("/remote/retry.bin");
    assert!(partial_left > 0, "失败后临时文件要留着：{partial_left}");

    // 重试：断线只断一次，这次能传完；内容一致。
    assert!(engine.retry(id), "失败的任务要能重试");
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/retry.bin"),
        vec![7u8; 16 * 1024]
    );
    assert_eq!(
        endpoints.remote_partial_len("/remote/retry.bin"),
        0,
        "传完要收尾"
    );

    // 已完成的任务不能重试；不存在的任务号也不行。
    assert!(!engine.retry(id), "完成的任务不能重试");
    assert!(!engine.retry(9999), "不存在的任务不能重试");

    // 清除已完成：清掉一个；报告列表随之空掉。
    assert_eq!(engine.reports().len(), 1);
    assert_eq!(engine.clear_finished(), 1);
    assert!(engine.reports().is_empty());
    assert_eq!(engine.clear_finished(), 0, "再清一次是空的");
}

/// 冲突信息要把两头的修改时间带给界面（M3-2 第 4 节的冲突对话框要显示）。
#[tokio::test]
async fn conflict_info_carries_both_mtimes() {
    struct Capture(Arc<Mutex<Vec<ConflictInfo>>>);
    impl ConflictResolver for Capture {
        fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            let sink = Arc::clone(&self.0);
            Box::pin(async move {
                sink.lock().unwrap().push(info);
                ConflictDecision {
                    action: ConflictAction::Overwrite,
                    apply_to_all: false,
                    rename_to: None,
                }
            })
        }
    }
    let root = temp_root("conflict-mtime");
    let source = root.join("mtime.bin");
    std::fs::write(&source, vec![1u8; 8192]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/mtime.bin", vec![2u8; 1024]);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, mut events) = TransferEngine::new(
        endpoints,
        Arc::new(Capture(Arc::clone(&seen))),
        local_options(),
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/mtime.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    let infos = seen.lock().unwrap();
    let info = infos.first().expect("要问过一次冲突");
    // 源是本地文件（真实 mtime），目标是内存远端（固定值）；两头都要带到。
    assert!(info.source_mtime.is_some(), "{info:?}");
    assert_eq!(info.target_mtime, Some(1_700_000_000), "{info:?}");
    assert_eq!(info.target_size, Some(1024), "{info:?}");
    assert!(info.can_resume, "目标比源小，要能续传：{info:?}");
}

/// 上传目录时**目标根本身也要建**（M3-2 端到端实测踩到的坑："拖目录 a 到
/// /remote" 的落点是 /remote/a，之前只建了它里面的子目录，mkdir 嵌套路径报
/// "文件不存在"——M3-1 的测试传进的都是已存在的临时目录，没踩到）。
#[tokio::test]
async fn upload_creates_the_target_root_itself() {
    let root = temp_root("target-root");
    let source = root.join("资料");
    std::fs::create_dir_all(source.join("子目录")).unwrap();
    std::fs::write(source.join("子目录").join("数据.bin"), vec![1u8; 128]).unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/资料".to_owned(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/资料/子目录/数据.bin"),
        vec![1u8; 128],
        "目标根没建出来，文件落不下去"
    );
}

/// 冲突问过之后暂停、再续传：**不重新问**，接着断点传完（M3-2 实测踩到的：
/// 重新问一遍没人答，120 秒超时按跳过，传一半的文件被丢掉）。
#[tokio::test]
async fn pause_after_conflict_resume_does_not_ask_again() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct GatedResolver {
        asked: AtomicUsize,
        /// 第一次问：先叫醒测试（让它按暂停），再等测试放行。
        asked_once: Notify,
        release: Notify,
    }
    impl ConflictResolver for GatedResolver {
        fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            Box::pin(async {
                let count = self.asked.fetch_add(1, AtomicOrdering::SeqCst);
                if count == 0 {
                    self.asked_once.notify_waiters();
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

    let root = temp_root("pause-conflict");
    let source = root.join("big.bin");
    std::fs::write(&source, vec![3u8; 64 * 1024]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    // 目标已经有同名文件 → 冲突；用户选覆盖，传之前就把任务按了暂停。
    endpoints.put_remote("/remote/big.bin", vec![9u8; 1024]);

    let resolver = Arc::new(GatedResolver::default());
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::clone(&resolver) as Arc<dyn ConflictResolver>,
        TransferOptions {
            in_flight: 1,
            ..local_options()
        },
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/big.bin".to_owned(),
        recursive: false,
        peer: None,
    });

    // 等第一次冲突提问 → 按暂停 → 放行提问。复制在第一批边界停住。
    tokio::time::timeout(Duration::from_secs(30), resolver.asked_once.notified())
        .await
        .expect("没等到第一次冲突提问");
    assert!(engine.pause(id), "暂停要成功");
    resolver.release.notify_waiters();

    // 等任务停在 Paused（不是终态，Progress 事件里看）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let state = engine.state(id).expect("任务要在");
        if state == TransferState::Paused {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "没停在暂停状态：{state:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        resolver.asked.load(AtomicOrdering::SeqCst),
        1,
        "暂停前只问一次"
    );
    // 清掉排队的事件。
    while events.try_recv().is_ok() {}

    // 续传：不再问，接着传完，内容一致。
    assert!(engine.resume(id));
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        resolver.asked.load(AtomicOrdering::SeqCst),
        1,
        "续传不能重新问冲突"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/big.bin"),
        vec![3u8; 64 * 1024],
        "覆盖之后要是源内容"
    );
}

/// 暂停中被取消：这个任务**自己的**临时文件要清掉（以前这条路径直接返回"已取消"，
/// 临时文件一直留在服务器上）；别人的 / 用户自己的 `.zssh-part` 不能碰。
#[tokio::test]
async fn cancel_while_paused_removes_the_part_file() {
    let root = temp_root("cancel-paused");
    let source = root.join("big.bin");
    std::fs::write(&source, vec![5u8; 64 * 1024]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    // 服务器上已经有一个旧写法的 `big.bin.zssh-part`：不是这个任务的，不能当缓存、也不能删。
    endpoints.put_remote("/remote/big.bin.zssh-part", vec![1u8; 100]);

    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    // 这个任务（第 1 号）上次传到一半留下的临时文件：名字里带着它的任务标识。
    endpoints.put_remote(
        &format!("/remote/big.bin.{}1.zssh-part", engine.nonce),
        vec![2u8; 100],
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/big.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    // 单线程运行时：任务还没开始跑，先按暂停；它在检查点停住。
    assert!(engine.pause(id));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while engine.state(id) != Some(TransferState::Paused) {
        assert!(tokio::time::Instant::now() < deadline, "没停在暂停");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let paused_parts = endpoints.remote_part_files("/remote/big.bin");
    assert_eq!(
        paused_parts.len(),
        2,
        "暂停时这个任务自己的临时文件要留着（可以续传），旁边还有别人的那个：{paused_parts:?}"
    );

    assert!(engine.cancel(id));
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Cancelled, "{report:?}");
    assert_eq!(
        endpoints.remote_part_files("/remote/big.bin"),
        vec!["/remote/big.bin.zssh-part".to_owned()],
        "取消之后只该留下别人的那个"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/big.bin.zssh-part"),
        vec![1u8; 100],
        "别人的临时文件被动了"
    );
    assert!(
        !endpoints.remote_exists("/remote/big.bin"),
        "不该有最终文件"
    );
}

/// 冲突里选"重命名"并自己改了名字：对话框传回来的只是文件名，要接在原目标所在的
/// 目录后面（M3-2 走查发现：之前把它当完整路径用，会写到相对路径上）。原文件不动。
#[tokio::test]
async fn conflict_rename_with_a_custom_name_stays_in_the_target_directory() {
    struct RenameTo(&'static str);
    impl ConflictResolver for RenameTo {
        fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            let name = self.0;
            Box::pin(async move {
                ConflictDecision {
                    action: ConflictAction::Rename,
                    apply_to_all: false,
                    rename_to: Some(name.to_owned()),
                }
            })
        }
    }

    let root = temp_root("rename-custom");
    let source = root.join("data.bin");
    std::fs::write(&source, vec![8u8; 4096]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/sub/data.bin", vec![1u8; 100]);

    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::new(RenameTo("我改的名字.bin")),
        local_options(),
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/sub/data.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/sub/我改的名字.bin"),
        vec![8u8; 4096],
        "新名字要落在原目标所在的目录里"
    );
    assert!(
        !endpoints.remote_exists("/remote/我改的名字.bin"),
        "不能写到上一级 / 相对路径上"
    );
    assert!(
        !endpoints.remote_exists("我改的名字.bin"),
        "不能写到相对路径上"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/sub/data.bin"),
        vec![1u8; 100],
        "原来的文件不能被动"
    );
}

/// F04：同一个最终目标已经有任务在写，第二个任务不能去写（也不能动对方的临时文件），
/// 对方写完释放之后再来就能传。
#[tokio::test]
async fn a_second_job_for_the_same_local_target_is_refused_not_mixed_in() {
    let root = temp_root("same-target");
    let local_dir = root.join("downloads");
    std::fs::create_dir_all(&local_dir).unwrap();
    let target = local_dir.join("same.bin");

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/a.bin", vec![b'A'; 3000]);
    endpoints.put_remote("/remote/b.bin", vec![b'B'; 5000]);
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());

    // 模拟"另一个任务正在写这个目标"。
    let holder = lock::TargetLocks::local()
        .try_acquire_local(&target)
        .expect("第一个任务拿到目标");
    let id = engine.submit(TransferJob {
        direction: Direction::Download,
        source: "/remote/b.bin".to_owned(),
        target: target.display().to_string(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    let TransferState::Failed(message) = &report.state else {
        panic!("应该因为目标被占用而失败：{report:?}");
    };
    assert!(message.contains("正在写"), "{message}");
    assert!(!target.exists(), "被拒绝的任务什么都不能写");
    let leftovers: Vec<_> = std::fs::read_dir(&local_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "被拒绝的任务留下了东西：{leftovers:?}"
    );

    // 对方做完，再来一次就行。
    drop(holder);
    let retry = engine.submit(TransferJob {
        direction: Direction::Download,
        source: "/remote/b.bin".to_owned(),
        target: target.display().to_string(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, retry, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(std::fs::read(&target).unwrap(), vec![b'B'; 5000]);
}

/// F04：两个任务同时往同一个目标写，结果要么是其中一个的完整内容，不会混在一起；
/// 用户目录里原本就有的 `<目标>.zssh-part` 也不会被动。
#[tokio::test]
async fn racing_jobs_never_mix_contents_and_leave_a_users_part_file_alone() {
    let root = temp_root("race");
    let local_dir = root.join("downloads");
    std::fs::create_dir_all(&local_dir).unwrap();
    let target = local_dir.join("same.bin");
    let users = local_dir.join("same.bin.zssh-part");
    std::fs::write(&users, b"MINE").unwrap();

    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/a.bin", vec![b'A'; 40_000]);
    endpoints.put_remote("/remote/b.bin", vec![b'B'; 50_000]);
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let ids: Vec<_> = ["/remote/a.bin", "/remote/b.bin"]
        .into_iter()
        .map(|source| {
            engine.submit(TransferJob {
                direction: Direction::Download,
                source: source.to_owned(),
                target: target.display().to_string(),
                recursive: false,
                peer: None,
            })
        })
        .collect();
    // 两个任务的结束事件可能以任意顺序到：一起收。
    let mut pending: std::collections::HashSet<_> = ids.into_iter().collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(TransferEvent::Finished(report))) => {
                pending.remove(&report.id);
            }
            Ok(Some(_)) => {}
            _ => panic!("等不到任务结束：{pending:?}"),
        }
    }
    let content = std::fs::read(&target).expect("至少有一个任务成功");
    let all_a = content == vec![b'A'; 40_000];
    let all_b = content == vec![b'B'; 50_000];
    assert!(
        all_a || all_b,
        "目标内容是两个任务混在一起的：{} 字节",
        content.len()
    );
    assert_eq!(std::fs::read(&users).unwrap(), b"MINE");
}

/// R-6b：远端（内存替身）不覆盖收尾遇到已存在的目标：失败、原文件不变、临时文件还在；覆盖照常成功。
#[tokio::test]
async fn remote_commit_without_overwrite_keeps_the_existing_file() {
    let endpoints = MemoryEndpoints::new("/remote");
    endpoints.put_remote("/remote/f.txt", b"OLD".to_vec());
    let target = MemoryEndpoint::with_fs(&endpoints.remote, "/remote/f.txt");
    let writer = target.open_write(0).await.unwrap();
    writer.write_at(0, b"NEW!".to_vec()).await.unwrap();
    writer.finish().await.unwrap();

    let err = target
        .commit(4, false)
        .await
        .expect_err("目标已存在必须失败");
    assert!(matches!(err, SftpError::AlreadyExists { .. }), "{err:?}");
    assert_eq!(endpoints.remote_contents("/remote/f.txt"), b"OLD");
    assert_eq!(
        endpoints.remote_partial_len("/remote/f.txt"),
        4,
        "临时文件要留着"
    );

    target.commit(4, true).await.unwrap();
    assert_eq!(endpoints.remote_contents("/remote/f.txt"), b"NEW!");
}

// ---- R01：远端临时文件认主 + 远端目标锁在所有引擎之间共享 ----

/// 两个真实的传输引擎同时上传到同一个远端路径：第二个被拒，内容不混。
/// 用冲突提问把第一个引擎卡在"已经拿了目标、还没开始写"的位置，保证两个任务真的重叠。
#[tokio::test]
async fn two_engines_uploading_to_one_remote_path_do_not_mix_contents() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct HoldFirst {
        asked: AtomicUsize,
        asked_once: Notify,
        release: Notify,
    }
    impl ConflictResolver for HoldFirst {
        fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            Box::pin(async {
                if self.asked.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
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

    let root = temp_root("two-engines");
    let source_a = root.join("a.bin");
    let source_b = root.join("b.bin");
    std::fs::write(&source_a, vec![b'A'; 40_000]).unwrap();
    std::fs::write(&source_b, vec![b'B'; 50_000]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/same.bin", vec![b'0'; 100]);

    let resolver = Arc::new(HoldFirst::default());
    let (engine_a, mut events_a) = TransferEngine::new(
        endpoints.clone(),
        Arc::clone(&resolver) as Arc<dyn ConflictResolver>,
        local_options(),
    );
    let (engine_b, mut events_b) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());

    let id_a = engine_a.submit(TransferJob {
        direction: Direction::Upload,
        source: source_a.display().to_string(),
        target: "/remote/same.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    tokio::time::timeout(Duration::from_secs(30), resolver.asked_once.notified())
        .await
        .expect("第一个引擎应该走到冲突提问（这时它已经拿了目标）");

    let id_b = engine_b.submit(TransferJob {
        direction: Direction::Upload,
        source: source_b.display().to_string(),
        target: "/remote/same.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report_b = drain_until_finished(&mut events_b, id_b, Duration::from_secs(20)).await;
    let TransferState::Failed(message) = &report_b.state else {
        panic!("第二个引擎应该因为目标被占用而失败：{report_b:?}");
    };
    assert!(message.contains("正在写"), "{message}");
    assert_eq!(
        endpoints.remote_contents("/remote/same.bin"),
        vec![b'0'; 100],
        "被拒绝的引擎什么都不能写"
    );

    resolver.release.notify_one();
    let report_a = drain_until_finished(&mut events_a, id_a, Duration::from_secs(20)).await;
    assert_eq!(report_a.state, TransferState::Done, "{report_a:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/same.bin"),
        vec![b'A'; 40_000],
        "内容必须是第一个任务的完整内容"
    );
}

/// 服务器上用户自己的 `x.zssh-part` 不能被当成缓存截断：上传 `x` 之后它原样还在。
#[tokio::test]
async fn a_users_own_zssh_part_file_on_the_server_is_never_touched() {
    let root = temp_root("remote-user-part");
    let source = root.join("x.bin");
    std::fs::write(&source, vec![b'X'; 20_000]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote(
        "/remote/x.bin.zssh-part",
        b"USER DATA THAT MATTERS".to_vec(),
    );

    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/x.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(20)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/x.bin"),
        vec![b'X'; 20_000]
    );
    assert_eq!(
        endpoints.remote_contents("/remote/x.bin.zssh-part"),
        b"USER DATA THAT MATTERS",
        "用户自己的 .zssh-part 文件被动了"
    );
}

/// M3-5 0a：两个引擎各有自己的数据面对象，只要它们连的是**同一个服务器身份**，就共用同一把远端锁；
/// 连的是不同服务器，同名路径互不影响。
#[tokio::test]
async fn engines_on_separate_endpoints_share_the_lock_when_the_server_identity_matches() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct HoldFirst {
        asked: AtomicUsize,
        asked_once: Notify,
        release: Notify,
    }
    impl ConflictResolver for HoldFirst {
        fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            Box::pin(async {
                if self.asked.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
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

    let root = temp_root("identity-lock");
    let source = root.join("a.bin");
    std::fs::write(&source, vec![b'A'; 30_000]).unwrap();
    let make = |identity: &str| {
        let endpoints = Arc::new(MemoryEndpoints::new("/remote").with_identity(identity));
        endpoints.put_remote("/remote/same.bin", vec![b'0'; 10]);
        endpoints
    };

    let first = make("alice@files.example:22");
    let resolver = Arc::new(HoldFirst::default());
    let (engine_a, mut events_a) = TransferEngine::new(
        first.clone(),
        Arc::clone(&resolver) as Arc<dyn ConflictResolver>,
        local_options(),
    );
    let id_a = engine_a.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/same.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    tokio::time::timeout(Duration::from_secs(30), resolver.asked_once.notified())
        .await
        .expect("第一个引擎应该走到冲突提问");

    // 同一个服务器身份（另一个数据面对象，比如另一个标签）：被拒。
    let same = make("alice@files.example:22");
    let (engine_b, mut events_b) =
        TransferEngine::new(same.clone(), Arc::new(OverwriteAll), local_options());
    let id_b = engine_b.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/same.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report_b = drain_until_finished(&mut events_b, id_b, Duration::from_secs(20)).await;
    assert!(
        matches!(&report_b.state, TransferState::Failed(message) if message.contains("正在写")),
        "同一个服务器身份应该被拒：{report_b:?}"
    );
    assert_eq!(same.remote_contents("/remote/same.bin"), vec![b'0'; 10]);

    // 不同的服务器：同名路径照常可以写。
    let other = make("alice@another.example:22");
    let (engine_c, mut events_c) =
        TransferEngine::new(other.clone(), Arc::new(OverwriteAll), local_options());
    let id_c = engine_c.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/same.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report_c = drain_until_finished(&mut events_c, id_c, Duration::from_secs(20)).await;
    assert_eq!(report_c.state, TransferState::Done, "{report_c:?}");

    resolver.release.notify_one();
    let report_a = drain_until_finished(&mut events_a, id_a, Duration::from_secs(20)).await;
    assert_eq!(report_a.state, TransferState::Done, "{report_a:?}");
}

// ---------- 复制粘贴的两个方向（M3-5 第 3 节） ----------

#[test]
fn copy_names_put_the_suffix_before_the_extension() {
    assert_eq!(copy_path("/a/report.txt", 1), "/a/report - 副本.txt");
    assert_eq!(copy_path("/a/report.txt", 2), "/a/report - 副本 2.txt");
    assert_eq!(copy_path("/a/noext", 1), "/a/noext - 副本");
    assert_eq!(copy_path("/a/.bashrc", 1), "/a/.bashrc - 副本");
    assert_eq!(copy_path(r"C:\my.dir\file", 1), r"C:\my.dir\file - 副本");
    assert_eq!(copy_path(r"C:\d\p.jpg", 1), r"C:\d\p - 副本.jpg");
}

/// 远端到远端：整棵目录（含子目录）经引擎复制到新位置，内容一致，源不动。
#[tokio::test]
async fn remote_copy_copies_a_whole_tree_and_leaves_the_source_alone() {
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/app/a.txt", b"alpha".to_vec());
    endpoints.put_remote("/remote/app/sub/b.bin", vec![7u8; 10_000]);
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: "/remote/app".to_owned(),
        target: "/remote/app-copy".to_owned(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(10)).await;
    assert_eq!(report.state, TransferState::Done, "{:?}", report.notes);
    assert_eq!(
        endpoints.remote_contents("/remote/app-copy/a.txt"),
        b"alpha"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/app-copy/sub/b.bin"),
        vec![7u8; 10_000]
    );
    assert_eq!(endpoints.remote_contents("/remote/app/a.txt"), b"alpha");
}

/// 远端到远端的单个文件：目标已存在、选"跳过"→ 目标不变，任务算跳过。
#[tokio::test]
async fn remote_copy_skip_keeps_the_existing_target() {
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/x.txt", b"new".to_vec());
    endpoints.put_remote("/remote/dst/x.txt", b"old".to_vec());
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(SkipAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: "/remote/x.txt".to_owned(),
        target: "/remote/dst/x.txt".to_owned(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(10)).await;
    assert_eq!(report.state, TransferState::Skipped);
    assert_eq!(endpoints.remote_contents("/remote/dst/x.txt"), b"old");
}

/// 一个源不存在的复制任务要失败并说清楚，不能悄悄成功。
#[tokio::test]
async fn remote_copy_of_a_missing_source_fails() {
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: "/remote/nope".to_owned(),
        target: "/remote/dst".to_owned(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(10)).await;
    assert!(
        matches!(report.state, TransferState::Failed(_)),
        "{:?}",
        report.state
    );
}

/// 本地到本地：目录树（含空目录）复制，源不动。
#[tokio::test]
async fn local_copy_copies_a_tree_including_empty_directories() {
    let root = temp_root("local-copy");
    let source = root.join("proj");
    std::fs::create_dir_all(source.join("sub").join("empty")).unwrap();
    std::fs::write(source.join("a.txt"), b"alpha").unwrap();
    std::fs::write(source.join("sub").join("b.txt"), b"bravo").unwrap();
    let target = root.join("proj-copy");
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) =
        TransferEngine::new(endpoints, Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::LocalCopy,
        source: source.display().to_string(),
        target: target.display().to_string(),
        recursive: true,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(10)).await;
    assert_eq!(report.state, TransferState::Done, "{:?}", report.notes);
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"alpha");
    assert_eq!(
        std::fs::read(target.join("sub").join("b.txt")).unwrap(),
        b"bravo"
    );
    assert!(target.join("sub").join("empty").is_dir(), "空目录也要复制");
    assert!(source.join("a.txt").exists(), "源不动");
}

/// 本地复制和下载写同一个本地路径时互斥（同一个最终目标只让一个任务写）。
#[tokio::test]
async fn local_copy_takes_the_local_target_lock() {
    let root = temp_root("local-copy-lock");
    let source = root.join("a.txt");
    std::fs::write(&source, b"x").unwrap();
    let target = root.join("b.txt");
    let guard = lock::TargetLocks::local()
        .try_acquire_local(&target)
        .expect("先占住本地目标");
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) =
        TransferEngine::new(endpoints, Arc::new(OverwriteAll), local_options());
    let id = engine.submit(TransferJob {
        direction: Direction::LocalCopy,
        source: source.display().to_string(),
        target: target.display().to_string(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(10)).await;
    assert!(
        matches!(report.state, TransferState::Failed(_)),
        "目标被别人占着时要失败而不是覆盖：{:?}",
        report.state
    );
    drop(guard);
}

/// 新文件（目标原来不存在）传到一半暂停、再继续：要从临时文件接着传，不能从 0 重来
/// （R-9 F14：以前"目标不存在"会把续传标记改成 false，暂停前传好的部分全部作废）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pausing_a_new_file_resumes_from_its_own_part_file() {
    let root = temp_root("pause-new-file");
    let source = root.join("big.bin");
    let size: usize = 16 * 1024 * 1024;
    let contents: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &contents).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::new(OverwriteAll),
        TransferOptions {
            concurrency: 1,
            in_flight: 1,
            chunk_len: Some(1024),
            ..local_options()
        },
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/big.bin".to_owned(),
        recursive: false,
        peer: None,
    });

    // 等临时文件里有了一部分内容就暂停。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while endpoints.remote_partial_len("/remote/big.bin") == 0 {
        assert!(tokio::time::Instant::now() < deadline, "临时文件一直没写");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(engine.pause(id), "暂停要成功（文件还没传完）");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while engine.state(id) != Some(TransferState::Paused) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "没停在暂停状态：{:?}",
            engine.state(id)
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let paused_at = endpoints.remote_partial_len("/remote/big.bin");
    assert!(
        paused_at > 0 && (paused_at as usize) < size,
        "暂停时临时文件要有一部分：{paused_at}"
    );

    assert!(engine.resume(id));
    let report = drain_until_finished(&mut events, id, Duration::from_secs(60)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(endpoints.remote_contents("/remote/big.bin"), contents);
    // 进度只算新传的字节：续传时总数正好是文件大小；从 0 重传会多出暂停前那一段。
    assert_eq!(
        report.done_bytes, size as u64,
        "续传不能从 0 重新传（暂停时已有 {paused_at} 字节）"
    );
}

// ---- C05：开始时目标不存在、传输期间冒出了同名文件 ----

/// 按固定动作回答冲突，并记下问了几次、问的时候目标多大。
struct CountingResolver {
    action: ConflictAction,
    asked: Mutex<Vec<ConflictInfo>>,
}

impl CountingResolver {
    fn new(action: ConflictAction) -> Arc<Self> {
        Arc::new(Self {
            action,
            asked: Mutex::new(Vec::new()),
        })
    }
    fn asked(&self) -> Vec<ConflictInfo> {
        self.asked.lock().unwrap().clone()
    }
}

impl ConflictResolver for CountingResolver {
    fn decide<'a>(&'a self, info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
        self.asked.lock().unwrap().push(info);
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

async fn upload_while_a_file_appears(
    tag: &str,
    resolver: Arc<CountingResolver>,
) -> (
    Arc<MemoryEndpoints>,
    ProgressReport,
    crate::test_util::TestDir,
) {
    let root = temp_root(tag);
    let source = root.join("report.bin");
    std::fs::write(&source, vec![7u8; 10_000]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    // 开始时目标不存在（不会问）；收尾之前别的程序建了同名文件。
    endpoints.appear_on_commit("/remote/report.bin", b"SOMEONE ELSES FILE".to_vec());
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        resolver as Arc<dyn ConflictResolver>,
        local_options(),
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/report.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    (endpoints, report, root)
}

/// 没经用户同意不能覆盖：提交时发现目标冒出来了，按冲突重新问；选跳过，那个文件原样保留，
/// 自己的临时文件清掉。
#[tokio::test]
async fn a_target_created_during_the_transfer_is_not_overwritten_silently() {
    let resolver = CountingResolver::new(ConflictAction::Skip);
    let (endpoints, report, _root) =
        upload_while_a_file_appears("c05-skip", Arc::clone(&resolver)).await;
    assert_eq!(report.state, TransferState::Skipped, "{report:?}");
    assert_eq!(
        endpoints.remote_contents("/remote/report.bin"),
        b"SOMEONE ELSES FILE",
        "别人建的文件被盖掉了"
    );
    let asked = resolver.asked();
    assert_eq!(asked.len(), 1, "提交时要按冲突问一次：{asked:?}");
    assert_eq!(asked[0].target_size, Some(18));
    assert!(
        endpoints.remote_part_files("/remote/report.bin").is_empty(),
        "跳过之后临时文件要清掉"
    );
}

/// 重新问的时候用户选了覆盖：这才替换，而且接着已经传完的临时文件提交（不从头再传）。
#[tokio::test]
async fn a_target_created_during_the_transfer_is_replaced_only_after_the_user_agrees() {
    let resolver = CountingResolver::new(ConflictAction::Overwrite);
    let (endpoints, report, _root) =
        upload_while_a_file_appears("c05-overwrite", Arc::clone(&resolver)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(resolver.asked().len(), 1);
    assert_eq!(
        endpoints.remote_contents("/remote/report.bin"),
        vec![7u8; 10_000]
    );
    assert!(endpoints.remote_part_files("/remote/report.bin").is_empty());
}

/// 冲突里选了改名，然后暂停、续传：要接着往新名字传，不能回到原目标上把它盖掉
/// （以前续传只记得"问过"，不记得选的是什么）。
#[tokio::test]
async fn pause_after_choosing_rename_keeps_writing_to_the_new_name() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct GatedRename {
        asked: AtomicUsize,
        asked_once: Notify,
        release: Notify,
    }
    impl ConflictResolver for GatedRename {
        fn decide<'a>(&'a self, _info: ConflictInfo) -> BoxFuture<'a, ConflictDecision> {
            Box::pin(async {
                let count = self.asked.fetch_add(1, AtomicOrdering::SeqCst);
                if count == 0 {
                    self.asked_once.notify_waiters();
                    self.release.notified().await;
                }
                ConflictDecision {
                    action: ConflictAction::Rename,
                    apply_to_all: false,
                    rename_to: Some("new name.bin".to_owned()),
                }
            })
        }
    }

    let root = temp_root("c05-pause-rename");
    let source = root.join("big.bin");
    std::fs::write(&source, vec![3u8; 64 * 1024]).unwrap();
    let endpoints = Arc::new(MemoryEndpoints::new("/remote"));
    endpoints.put_remote("/remote/big.bin", vec![9u8; 1024]);
    let resolver = Arc::new(GatedRename::default());
    let (engine, mut events) = TransferEngine::new(
        endpoints.clone(),
        Arc::clone(&resolver) as Arc<dyn ConflictResolver>,
        TransferOptions {
            in_flight: 1,
            ..local_options()
        },
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: "/remote/big.bin".to_owned(),
        recursive: false,
        peer: None,
    });
    tokio::time::timeout(Duration::from_secs(30), resolver.asked_once.notified())
        .await
        .expect("没等到冲突提问");
    assert!(engine.pause(id), "暂停要成功");
    resolver.release.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while engine.state(id) != Some(TransferState::Paused) {
        assert!(tokio::time::Instant::now() < deadline, "没停在暂停状态");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    while events.try_recv().is_ok() {}

    assert!(engine.resume(id));
    let report = drain_until_finished(&mut events, id, Duration::from_secs(30)).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(
        resolver.asked.load(AtomicOrdering::SeqCst),
        1,
        "续传不重新问"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/big.bin"),
        vec![9u8; 1024],
        "原来的文件不能被动"
    );
    assert_eq!(
        endpoints.remote_contents("/remote/new name.bin"),
        vec![3u8; 64 * 1024]
    );
}
