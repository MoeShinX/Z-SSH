//! 服务器状态栏（M1-10）的联网测试：在测试服务器上开 exec 通道采 3 次统计，
//! 断言范围合理性；同一条连接上开两个 shell，终端往返不受采集影响。
//!
//! 环境变量 `ZSSH_TEST_*` 不全时跳过（common::TestServer::from_env 已处理）。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zssh_core::ssh::{ShellEvent, StatsCollector};

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// 连续采集 3 次，数值都在合理范围里。
#[tokio::test]
async fn stats_collect_three_samples_with_sane_values() {
    if !common::full_live("stats_collect_three_samples_with_sane_values") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 stats_collect_three_samples_with_sane_values：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("stats-collect");
    let connection = server
        .connect(&known_hosts)
        .await
        .expect("连接测试服务器失败");

    let mut collector = StatsCollector::new();
    let mut samples = Vec::new();
    for index in 0..3 {
        let started = Instant::now();
        let stats = collector
            .collect(&connection)
            .await
            .expect("采集不该报错")
            .unwrap_or_else(|| panic!("第 {} 次采集没采到数据", index + 1));
        // 两次采样之间隔一点（网速的分母要真实的时间）。
        if index + 1 < 3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!(
            "第 {} 次采集（{} ms）：CPU {:?}，内存 {:?} MiB，磁盘 {:?}%，网速 {:.0}/{:.0} B/s，系统 {:?}",
            index + 1,
            started.elapsed().as_millis(),
            stats.cpu_percent,
            stats.mem_total_mib,
            stats.disk_percent,
            stats.net_rx_speed,
            stats.net_tx_speed,
            server.sanitize(&stats.os_name),
        );
        samples.push(stats);
    }

    // 采集脚本给的是差值：第一次 cpu_percent 是 None（没有前值），后两次有。
    assert!(
        samples[0].cpu_percent.is_none(),
        "第一次采样不该有 CPU 差值"
    );
    for (index, stats) in samples.iter().enumerate().skip(1) {
        let percent = stats.cpu_percent.expect("第二次以后应该有 CPU 差值");
        assert!(
            (0..=100).contains(&percent),
            "CPU 使用率 {} 超出 0～100（第 {} 次）",
            percent,
            index + 1
        );
    }
    // 型号只在第一次探测，后面从缓存补：每次都一样（服务器没有型号时三次都是 None）。
    println!("CPU 型号：{:?}", samples[0].cpu_model);
    for (index, stats) in samples.iter().enumerate().skip(1) {
        assert_eq!(
            stats.cpu_model,
            samples[0].cpu_model,
            "第 {} 次的 CPU 型号和第一次不一致",
            index + 1
        );
    }
    // 全部分区（概览面板）：至少有根分区，伪文件系统和 docker 层都滤掉了。
    let mounts: Vec<String> = samples[0]
        .mounts
        .iter()
        .map(|m| format!("{} {} {}%", m.mount, m.fs_type, m.percent))
        .collect();
    println!("分区：{mounts:?}");
    assert!(
        samples[0].mounts.iter().any(|m| m.mount == "/"),
        "分区里要有根分区：{mounts:?}"
    );
    for m in &samples[0].mounts {
        assert!(
            !matches!(
                m.fs_type.as_str(),
                "tmpfs" | "overlay" | "devtmpfs" | "proc" | "sysfs"
            ),
            "伪文件系统没滤掉：{mounts:?}"
        );
        assert!(m.percent <= 100 && m.total_mib > 0.0);
    }
    let mem_total = samples[0].mem_total_mib.expect("内存总量应该有");
    assert!(mem_total > 0, "内存总量要大于 0");
    for (index, stats) in samples.iter().enumerate() {
        assert!(
            stats.net_rx_speed >= 0.0 && stats.net_tx_speed >= 0.0,
            "网速不能是负数（第 {} 次）",
            index + 1
        );
    }
}

/// 同一条连接上开两个 shell：一个 shell 在采集进行时照常往返。
///
/// 依赖网速的延迟门限测试：**不进五门禁**（AI-2b 第 5 节）；用
/// `scripts/perf-tests.ps1`（`cargo test -- --ignored`）单独跑。
#[tokio::test]
#[ignore = "依赖网速的延迟门限；用 scripts/perf-tests.ps1 单独跑"]
async fn stats_do_not_disturb_session_round_trip() {
    if !common::full_live("stats_do_not_disturb_session_round_trip") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 stats_do_not_disturb_session_round_trip：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("stats-latency");
    let connection = Arc::new(
        server
            .connect(&known_hosts)
            .await
            .expect("连接测试服务器失败"),
    );

    // 后台采集任务：两次之间隔 300ms，覆盖整个往返窗口。
    let collector_connection = Arc::clone(&connection);
    let collecting = Arc::new(AtomicUsize::new(0));
    let collecting_handle = Arc::clone(&collecting);
    let collector_task = tokio::spawn(async move {
        let mut collector = StatsCollector::new();
        for _ in 0..10 {
            if let Ok(Some(_)) = collector.collect(&collector_connection).await {
                collecting_handle.fetch_add(1, Ordering::Relaxed);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });

    // 开 shell 量往返：echo 回显时间作为延迟口径（M1-06 的 B 端往返）。
    let mut shell = connection
        .open_shell(COLS, ROWS)
        .await
        .expect("打开 shell 失败");

    // 基线：采集还没开始时量 3 次。
    let mut baseline = Vec::new();
    for index in 0..3 {
        let round_trip = round_trip_ms(&mut shell, index).await;
        baseline.push(round_trip);
    }
    baseline.sort();
    let baseline_median = baseline[1];
    println!("基线往返（3 次取中位）：{baseline_median:?}");

    // 采集进行时再量 3 次：门限相对基线算（链路慢的服务器上固定 1s 会误报）。
    let mut during = Vec::new();
    for index in 3..6 {
        let round_trip = round_trip_ms(&mut shell, index).await;
        during.push(round_trip);
    }
    during.sort();
    let during_median = during[1];
    println!("采集期间往返（3 次取中位）：{during_median:?}");
    println!("采集成功次数：{}", collecting.load(Ordering::Relaxed));

    // 采集成功至少 1 次（服务器在线的话必成）。
    assert!(
        collecting.load(Ordering::Relaxed) >= 1,
        "采集任务一次都不该错过"
    );
    // 往返不能明显变差：允许 2 倍基线 + 200ms 的抖动余量（跨机房链路 281ms 的基线）。
    let limit = baseline_median * 2 + Duration::from_millis(200);
    assert!(
        during_median < limit,
        "采集时的往返 {during_median:?} 超过了门限 {limit:?}（基线 {baseline_median:?}）"
    );

    collector_task.abort();
    let _ = shell
        .write(
            b"exit
",
        )
        .await;
}

/// 在 shell 里跑一条 echo，量到看到输出为止的往返。
async fn round_trip_ms(shell: &mut zssh_core::ssh::ShellSession, index: usize) -> Duration {
    let command = format!("echo ZSSH_STATS_$((40+2))_{index}\r");
    let expect = format!("ZSSH_STATS_42_{index}");
    let started = Instant::now();
    if shell.write(command.as_bytes()).await.is_err() {
        return Duration::from_secs(5);
    }
    let deadline = started + Duration::from_secs(5);
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Duration::from_secs(5);
        }
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                tail.extend_from_slice(&bytes);
                if tail.len() > 256 {
                    tail.drain(..tail.len() - 256);
                }
                if String::from_utf8_lossy(&tail).contains(&expect) {
                    return started.elapsed();
                }
            }
            Ok(Some(_)) | Ok(None) | Err(_) => return Duration::from_secs(5),
        }
    }
}
