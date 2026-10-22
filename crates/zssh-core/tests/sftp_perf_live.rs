//! 传输不能拖慢终端（M3-1 第 3、4 节 + M1 设计文档 5.2 的硬规则）。
//!
//! 场景：同一条连接上开着终端，同时往远端传一个 64MB 的文件，终端每隔 200ms 量一次
//! `echo` 的往返延迟。参考 `flood_isolation.rs` 的做法（基线 + P95 相对预算）。
//!
//! 标 `#[ignore]`：依赖网速，不进五门禁。跑法：
//!
//! ```text
//! powershell -File scripts/perf-tests.ps1
//! ```
//!
//! 需要 `ZSSH_TEST_*`；远端只用 `/tmp/zssh-sftp-live-<随机串>`，结束删掉。

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{timeout, Instant};

use zssh_core::sftp::transfer::backend::{SftpChannelProvider, SftpEndpoints};
use zssh_core::sftp::transfer::{
    ConflictAction, ConflictDecision, ConflictInfo, ConflictResolver, Direction, TransferEngine,
    TransferEvent, TransferJob, TransferOptions, TransferState,
};
use zssh_core::ssh::{ShellEvent, ShellSession};

use common::{serial, LocalTempDir, RemoteTempDir, TempKnownHosts, TestServer};

/// 上传多大的文件（和卡片一致）。
const UPLOAD_BYTES: usize = 64 * 1024 * 1024;
/// 终端采样间隔。
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 一次 echo 往返的超时（超了记成这个时间）。
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(10);
/// 上传前量几次基线。
const BASELINE_SAMPLES: usize = 5;
/// 上传期间量多少次。
const DURING_SAMPLES: usize = 40;
/// 上传期间终端 P95 的额外延迟预算（参照 flood_isolation：相对基线算）。
const P95_EXTRA_BUDGET: Duration = Duration::from_secs(1);

struct OverwriteEverything;

impl ConflictResolver for OverwriteEverything {
    fn decide<'a>(
        &'a self,
        _info: ConflictInfo,
    ) -> zssh_core::ssh::BoxFuture<'a, ConflictDecision> {
        Box::pin(async {
            ConflictDecision {
                action: ConflictAction::Overwrite,
                apply_to_all: true,
                rename_to: None,
            }
        })
    }
}

/// 在终端里跑一次 `echo`，返回往返耗时。
async fn terminal_round_trip(shell: &mut ShellSession, seq: usize) -> Duration {
    let marker = format!("PERF_{seq}_$((40+2))");
    let expect = format!("PERF_{seq}_42");
    let started = Instant::now();
    if shell
        .write(format!("echo {marker}\r").as_bytes())
        .await
        .is_err()
    {
        return SAMPLE_TIMEOUT;
    }
    let deadline = started + SAMPLE_TIMEOUT;
    let mut seen = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return SAMPLE_TIMEOUT;
        }
        match timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains(&expect) {
                    return started.elapsed();
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return SAMPLE_TIMEOUT,
        }
    }
}

fn percentile(values: &[Duration], ratio: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = values.to_vec();
    sorted.sort();
    let index = ((sorted.len() as f64 - 1.0) * ratio).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn ms(value: Duration) -> u128 {
    value.as_millis()
}

fn write_random(path: &std::path::Path, size: usize) {
    let mut data = Vec::with_capacity(size);
    let mut seed: u64 = 0xDEAD_BEEF_CAFE_F00D;
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

#[tokio::test]
#[ignore = "性能门限：需要测试服务器和稳定的链路，用 scripts/perf-tests.ps1 跑"]
async fn sftp_upload_does_not_stall_the_terminal_on_the_same_connection() {
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 perf_sftp_upload...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-perf");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    // 终端（shell 通道）和 SFTP 通道都开在**同一条连接**上。
    let mut shell = connection.open_shell(80, 24).await.expect("开终端");
    let browse = Arc::new(common::open_sftp(&connection).await.expect("开浏览通道"));
    println!("SFTP 能力：{}", browse.caps().describe());

    // 临时目录带守卫（M3-2 第 0 节）：性能测试超时 / 失败也要删掉。
    let remote = RemoteTempDir::create(&server, &connection, &browse, "perf").await;
    let local = LocalTempDir::create("perf");
    let source = local.join("perf.bin");
    write_random(&source, UPLOAD_BYTES);

    // ---------- 基线：没有传输时的终端往返 ----------
    let mut baseline = Vec::new();
    for seq in 0..BASELINE_SAMPLES {
        baseline.push(terminal_round_trip(&mut shell, seq).await);
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    let baseline_median = percentile(&baseline, 0.5);
    println!("基线（没有传输）：中位 {}ms", ms(baseline_median));

    // ---------- 一边传 64MB，一边量终端往返 ----------
    let provider = Arc::new(SftpChannelProvider::new(Arc::clone(&connection), None));
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
            max_retries: 2,
            progress_interval: Duration::from_millis(200),
            tail_window: 4096,
        },
    );
    let id = engine.submit(TransferJob {
        direction: Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/perf.bin", remote.path()),
        recursive: false,
        peer: None,
    });

    let started = Instant::now();
    let mut during = Vec::new();
    let mut last_bytes = 0u64;
    let mut finished = None;
    while during.len() < DURING_SAMPLES {
        during.push(terminal_round_trip(&mut shell, 100 + during.len()).await);
        // 顺便收一下传输事件（看进度和速度）。
        while let Ok(event) = events.try_recv() {
            match event {
                TransferEvent::Progress(report) if report.id == id => {
                    last_bytes = report.done_bytes
                }
                TransferEvent::Finished(report) if report.id == id => finished = Some(report),
                _ => {}
            }
        }
        if finished.is_some() {
            break;
        }
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
    // 传输没结束就等它结束（上面的采样已经覆盖了"传输期间"）。
    let report = match finished {
        Some(report) => report,
        None => loop {
            match events.recv().await {
                Some(TransferEvent::Finished(report)) if report.id == id => break report,
                Some(_) => {}
                None => panic!("事件通道关了"),
            }
        },
    };
    let elapsed = started.elapsed();
    let throughput = UPLOAD_BYTES as f64 / elapsed.as_secs_f64() / 1024.0 / 1024.0;
    println!(
        "上传 {:.0}MB 用时 {:.1}s（{:.1} MB/s，末次采样 {} 字节）",
        UPLOAD_BYTES as f64 / 1024.0 / 1024.0,
        elapsed.as_secs_f64(),
        throughput,
        last_bytes
    );
    assert_eq!(report.state, TransferState::Done, "{report:?}");

    let during_median = percentile(&during, 0.5);
    let during_p95 = percentile(&during, 0.95);
    let extra_p95 = during_p95.saturating_sub(baseline_median);
    println!(
        "终端往返（传输 64MB 期间，{} 次采样）：中位 {}ms、P95 {}ms；基线 {}ms；P95 比基线多 {}ms",
        during.len(),
        ms(during_median),
        ms(during_p95),
        ms(baseline_median),
        ms(extra_p95)
    );

    // 门限：P95 比基线多不超过 1 秒（和 flood_isolation 的口径一致）。
    assert!(
        extra_p95 <= P95_EXTRA_BUDGET,
        "上传把终端拖慢了：P95 {}ms，基线 {}ms（多 {}ms，预算 {}ms）",
        ms(during_p95),
        ms(baseline_median),
        ms(extra_p95),
        ms(P95_EXTRA_BUDGET)
    );

    // 收尾（本地目录由守卫的 Drop 删）。
    remote.cleanup(&connection, &browse).await;
    let _ = shell.close().await;
    browse.close();
    let _ = connection.disconnect().await;
}

/// 顺便量一下：流水线开和关的吞吐对比（同一台服务器、同一个文件）。
#[tokio::test]
#[ignore = "性能门限：需要测试服务器和稳定的链路，用 scripts/perf-tests.ps1 跑"]
async fn pipeline_on_vs_off_throughput() {
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 perf_pipeline...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-perf-pipeline");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let browse = Arc::new(common::open_sftp(&connection).await.expect("开浏览通道"));

    let remote = RemoteTempDir::create(&server, &connection, &browse, "perfpipe").await;
    let local = LocalTempDir::create("perfpipe");
    let source = local.join("pipe.bin");
    write_random(&source, 32 * 1024 * 1024);

    let mut results = Vec::new();
    for pipeline in [true, false] {
        let provider = Arc::new(SftpChannelProvider::new(Arc::clone(&connection), None));
        let endpoints = Arc::new(SftpEndpoints::new(provider));
        let (engine, mut events) = TransferEngine::new(
            endpoints,
            Arc::new(OverwriteEverything),
            TransferOptions {
                pipeline,
                in_flight: 8,
                ..TransferOptions::default()
            },
        );
        let target = format!(
            "{}/pipe-{}.bin",
            remote.path(),
            if pipeline { "on" } else { "off" }
        );
        let started = Instant::now();
        let id = engine.submit(TransferJob {
            direction: Direction::Upload,
            source: source.display().to_string(),
            target,
            recursive: false,
            peer: None,
        });
        loop {
            match events.recv().await {
                Some(TransferEvent::Finished(report)) if report.id == id => {
                    assert_eq!(report.state, TransferState::Done, "{report:?}");
                    break;
                }
                Some(_) => {}
                None => panic!("事件通道关了"),
            }
        }
        let elapsed = started.elapsed();
        let mb_per_sec = 32.0 / elapsed.as_secs_f64();
        println!(
            "流水线{}：32MB 用时 {:.1}s → {:.1} MB/s",
            if pipeline { "开" } else { "关" },
            elapsed.as_secs_f64(),
            mb_per_sec
        );
        results.push((pipeline, mb_per_sec));
    }
    let on_speed = results[0].1;
    let off_speed = results[1].1;
    println!("对比：开 {on_speed:.1} MB/s vs 关 {off_speed:.1} MB/s");
    // 不断言快多少（链路本来就抖动）；只要两次都跑通、数字记进报告。
    assert!(on_speed > 0.0 && off_speed > 0.0);

    remote.cleanup(&connection, &browse).await;
    browse.close();
    let _ = connection.disconnect().await;
}
