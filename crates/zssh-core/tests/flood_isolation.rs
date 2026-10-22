//! 刷屏隔离（设计文档 4.1 的两级缓冲）的联网测试。
//!
//! 同一条连接上开两个 shell：A 刷屏，B 每 200ms 跑一次 `echo` 量往返延迟。
//!
//! 1. 快消费者：A 正常 ack（模拟界面渲染得过来），B 的 P95 要小于 1 秒
//! 2. 不消费者：A 完全不 ack，只有 A 自己的本地缓冲会涨。缓冲填满之前 B 的
//!    延迟同样记录；填满之后整条连接会被卡住（设计如此），变慢的数据也记下来
//! 3. A 恢复 ack 之后 B 的延迟要回到正常，验证卡住是暂时的
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{timeout, Instant};
use zssh_core::ssh::flow::HIGH_WATERMARK;
use zssh_core::ssh::{FlowGate, SessionBuffer, ShellEvent, ShellSession};

use common::{serial, TempKnownHosts, TestServer};

/// B 两次采样之间的间隔。
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 一次 echo 往返超过这个时间算超时（记成这个值）。
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);
/// 刷屏开始前量几次基线往返（取中位数）。
const BASELINE_SAMPLES: usize = 5;
/// 基线采样的间隔。
const BASELINE_INTERVAL: Duration = Duration::from_millis(120);
/// 快消费者阶段采多少个样本。
const FAST_SAMPLES: usize = 15;
/// 缓冲满了之后再多采几个样本。
const AFTER_FULL_SAMPLES: usize = 6;
/// 验收目标：P95 比基线多不超过 1 秒。链路本身慢的时候（验收时量到最小
/// 往返 281ms）用固定 1 秒会误报，所以要相对基线算。
const P95_BUDGET: Duration = Duration::from_secs(1);

/// 在途数据的量级：SSH 接收窗口（russh 默认 2MB）+ russh 交给通道的有界队列
/// （16 条、最多 512KB）。
///
/// B 的 echo 排在 A 的刷屏数据后面，所以它至少得等这么多数据先流完：链路快时
/// 是零点几秒（M1-02/M1-07 都印证过），链路慢时线性变长——2026-10-14 下午这条
/// 链路掉到 1.3MB/s，光排这 2.5MB 就要 ~1.9 秒。所以预算 = 基线 + 1 秒 +
/// 在途数据 / 实测吞吐；超了才是设计或者代码有问题。
const IN_FLIGHT_BYTES: usize = 2 * 1024 * 1024 + 512 * 1024;

/// 量吞吐用多久：刷屏跑起来之后按快消费者读这段时间，数消化了多少字节。
const THROUGHPUT_WINDOW: Duration = Duration::from_secs(2);
/// 等本地缓冲填满的时限。
const FILL_DEADLINE: Duration = Duration::from_secs(90);
/// 一直输出的命令。
const FLOOD: &str = "yes";
/// 判断标记时只留的尾部长度。
const TAIL_BYTES: usize = 4096;

/// 一直读 A 的输出。`acking` 为真时按快消费者 ack；为假时只读不 ack
/// （未确认量涨起来，流控门会把转发任务停掉，A 的本地缓冲开始堆）。
fn spawn_drain(
    mut events: zssh_core::ssh::ShellEventStream,
    acking: Arc<AtomicBool>,
    gate: Arc<FlowGate>,
    digested: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let ShellEvent::Data(bytes) = event {
                digested.fetch_add(bytes.len(), Ordering::Relaxed);
                if acking.load(Ordering::Relaxed) {
                    gate.on_ack(bytes.len());
                }
            }
        }
    })
}

/// 在 B 上跑一条 echo，量到看到输出为止的往返时间。
async fn sample(shell: &mut ShellSession, index: usize) -> Duration {
    let command = format!("echo ZSSH_ISO_$((40+2))_{index}\r");
    let expect = format!("ZSSH_ISO_42_{index}");

    let started = Instant::now();
    if shell.write(command.as_bytes()).await.is_err() {
        return SAMPLE_TIMEOUT;
    }

    let mut tail: Vec<u8> = Vec::new();
    let deadline = started + SAMPLE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return SAMPLE_TIMEOUT;
        }
        match timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                tail.extend_from_slice(&bytes);
                if tail.len() > TAIL_BYTES {
                    tail.drain(..tail.len() - TAIL_BYTES);
                }
                if String::from_utf8_lossy(&tail).contains(&expect) {
                    return started.elapsed();
                }
            }
            Ok(Some(_)) | Ok(None) | Err(_) => return SAMPLE_TIMEOUT,
        }
    }
}

/// 中位数（排序后取中间那个）。基线用它。
fn median(samples: &[Duration]) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

/// P95（排序后取 95% 位置那个）。
fn p95(samples: &[Duration]) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    let position = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[position.min(sorted.len() - 1)]
}

fn describe(samples: &[Duration]) -> String {
    if samples.is_empty() {
        return "（没有样本）".to_owned();
    }
    let max = samples.iter().max().copied().unwrap_or_default();
    let min = samples.iter().min().copied().unwrap_or_default();
    format!(
        "{} 个样本，P95 {:.3?}，最大 {:.3?}，最小 {:.3?}",
        samples.len(),
        p95(samples),
        max,
        min
    )
}

/// 依赖网速的吞吐/延迟门限测试：**不进五门禁**（AI-2b 第 5 节）。
///
/// 门限相对基线算，但基线本身随链路抖动（2026-10-14 掉到 1.3MB/s 时误报过一次）。
/// 用 `scripts/perf-tests.ps1`（`cargo test -- --ignored`）单独跑，结果抄进报告。
#[tokio::test]
#[ignore = "依赖网速的吞吐/延迟门限；用 scripts/perf-tests.ps1 单独跑"]
async fn a_flooding_shell_does_not_drag_the_other_one_down() {
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 a_flooding_shell_does_not_drag_the_other_one_down：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("flood-isolation");
    let connection = server
        .connect(&known_hosts)
        .await
        .expect("连接测试服务器失败");

    // 同一条连接上的两个 shell
    let shell_a = connection
        .open_shell(80, 24)
        .await
        .expect("开 A shell 失败");
    let gate_a = shell_a.flow_handle();
    let buffer_a: Arc<SessionBuffer> = shell_a.buffer_handle();
    let mut shell_b = connection
        .open_shell(80, 24)
        .await
        .expect("开 B shell 失败");

    // 先量基线：A 还没开始刷屏，B 的空闲往返是多少。链路本身慢的时候
    // （验收时量到最小往返 281ms）门限要相对它算，不能写死 1 秒。
    let test_started = Instant::now();
    let mut baseline_samples = Vec::new();
    for index in 0..BASELINE_SAMPLES {
        let rtt = sample(&mut shell_b, 900 + index).await;
        baseline_samples.push(rtt);
        tokio::time::sleep(BASELINE_INTERVAL).await;
    }
    let baseline = median(&baseline_samples);
    println!(
        "基线往返（A 空闲，{} 次取中位数）：{baseline:.3?}",
        baseline_samples.len()
    );

    // A 开始刷屏
    let (writer_a, events_a) = shell_a.split();
    writer_a
        .write(format!("{FLOOD}\r").as_bytes())
        .await
        .expect("写入刷屏命令失败");

    let acking = Arc::new(AtomicBool::new(true));
    let digested = Arc::new(AtomicUsize::new(0));
    let drain = spawn_drain(
        events_a,
        Arc::clone(&acking),
        Arc::clone(&gate_a),
        Arc::clone(&digested),
    );

    // 量吞吐：让 A 先跑 THROUGHPUT_WINDOW（这段也在为下面的采样热身），
    // 用快消费者消化的字节数算链路速度，预算里要加"排空在途数据"的时间。
    let throughput_started = Instant::now();
    let digested_before = digested.load(Ordering::Relaxed);
    tokio::time::sleep(THROUGHPUT_WINDOW).await;
    let digested_after = digested.load(Ordering::Relaxed);
    let throughput = (digested_after.saturating_sub(digested_before)) as f64
        / throughput_started.elapsed().as_secs_f64().max(0.001);
    let in_flight_drain = Duration::from_secs_f64(IN_FLIGHT_BYTES as f64 / throughput.max(1.0));
    let budget = baseline + P95_BUDGET + in_flight_drain;
    println!(
        "实测吞吐 {:.2} MB/s，在途 {:.2} MB 要排 {in_flight_drain:.3?}，预算 = 基线 {baseline:.3?} + {P95_BUDGET:?} + {in_flight_drain:.3?} = {budget:.3?}",
        throughput / (1024.0 * 1024.0),
        IN_FLIGHT_BYTES as f64 / (1024.0 * 1024.0),
    );

    // 第 1 阶段：快消费者。A 正常 ack，B 的往返延迟要达标。
    let mut fast = Vec::new();
    for index in 0..FAST_SAMPLES {
        tokio::time::sleep(SAMPLE_INTERVAL).await;
        fast.push(sample(&mut shell_b, index).await);
    }
    println!(
        "快消费者（A 正常 ack）：{}，基线 {baseline:.3?}，P95 超出基线 {:.3?}",
        describe(&fast),
        p95(&fast).saturating_sub(baseline)
    );
    assert!(
        p95(&fast) < budget,
        "A 正常刷屏时 B 的 P95 应该小于 {budget:.3?}（基线 {baseline:.3?} + 1s + 排空在途数据）：{}",
        describe(&fast)
    );

    // 第 2 阶段：A 完全不 ack。缓冲满之前 B 应该照旧；满了之后允许变慢（记录）。
    acking.store(false, Ordering::Relaxed);
    let mut before_full = Vec::new();
    let mut after_full = Vec::new();
    let fill_deadline = Instant::now() + FILL_DEADLINE;
    let mut index = FAST_SAMPLES;
    while after_full.len() < AFTER_FULL_SAMPLES && Instant::now() < fill_deadline {
        tokio::time::sleep(SAMPLE_INTERVAL).await;
        index += 1;
        let buffered_before = buffer_a.buffered_bytes();
        let unacked_before = gate_a.unacked();
        let stopped_before = buffer_a.is_reader_stopped();
        let rtt = sample(&mut shell_b, index).await;
        // 采样期间缓冲区满的也算"满之后"：跨过那条线的样本不放进"没满"这一桶，
        // 否则会把"满了才卡"的锅算到"没满"头上。
        let post_full = buffer_a.is_full();
        // 时间戳 + 读任务状态：下次再出现"M1-06 报告 4.7"那种异常时，
        // 一眼能看出 A 当时是在读还是停读。
        println!(
            "  阶段2 #{index} @{:.1?}：缓冲 {} → {} / {}，未确认 {unacked_before}，读任务{} → {}，往返 {rtt:.3?}{}",
            test_started.elapsed(),
            buffered_before,
            buffer_a.buffered_bytes(),
            buffer_a.limit(),
            if stopped_before { "停读" } else { "在读" },
            if buffer_a.is_reader_stopped() { "停读" } else { "在读" },
            if post_full { "（缓冲已满）" } else { "" }
        );
        if post_full {
            after_full.push(rtt);
        } else {
            before_full.push(rtt);
        }
    }

    println!("A 不 ack、本地缓冲还没满时：{}", describe(&before_full));
    println!("A 不 ack、缓冲已满后：{}", describe(&after_full));
    println!(
        "本地缓冲占用 {} / {} 字节，未确认 {} 字节",
        buffer_a.buffered_bytes(),
        buffer_a.limit(),
        gate_a.unacked()
    );

    assert!(
        !after_full.is_empty(),
        "等 {FILL_DEADLINE:?} 本地缓冲也没满（{} / {} 字节），没测到缓冲满了之后的情况",
        buffer_a.buffered_bytes(),
        buffer_a.limit()
    );
    // 卡片只要求"缓冲没满之前同样记录"（硬指标 P95 < 1 秒挂在快消费者那一段）。
    // 这里把数据打出来：没满之前 B 应该基本不受影响，慢了会显式提醒。
    let slow_before_full = before_full.iter().filter(|rtt| **rtt >= budget).count();
    if slow_before_full > 0 {
        println!(
            "注意：缓冲没满时有 {slow_before_full}/{} 个样本超过 {budget:.3?}（基线 + {P95_BUDGET:?}）",
            before_full.len()
        );
    }
    println!("（A 不 ack 期间，缓冲没满的样本只做记录，不作断言）");

    // 第 3 阶段：A 恢复 ack，连接应该从卡住中恢复。
    //
    // 恢复要等管线真的重新流动起来再采样：ack 之后转发任务得先把本地缓冲排空
    // （最多 8MB），服务器那边还有一窗口的数据在途，这些都在 B 的 echo 前面排队。
    // 固定等 1 秒在慢链路上不够，所以这里等条件（缓冲不再是满的、未确认量下来了）。
    acking.store(true, Ordering::Relaxed);
    gate_a.on_ack(gate_a.unacked());
    let settle_deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < settle_deadline {
        if !buffer_a.is_full() && gate_a.unacked() <= HIGH_WATERMARK {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut recovered = Vec::new();
    for _ in 0..5 {
        index += 1;
        recovered.push(sample(&mut shell_b, index).await);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
    println!("A 恢复 ack 之后：{}", describe(&recovered));
    assert!(
        p95(&recovered) < budget,
        "A 恢复 ack 之后 B 的 P95 应该回到 {budget:.3?} 以内（基线 {baseline:.3?} + 1s + 排空在途数据）：{}",
        describe(&recovered)
    );

    // 收尾：停掉 A 的刷屏
    drain.abort();
    let _ = timeout(Duration::from_secs(5), writer_a.write(b"\x03")).await;
    println!("刷屏隔离验证完成");
}
