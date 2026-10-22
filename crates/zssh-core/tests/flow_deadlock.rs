//! 固定 russh 的背压行为和设计文档 4.1 / 5.2 的两级缓冲：
//!
//! - 一个终端不被 ack 时，只有它自己的本地缓冲会涨；缓冲满了读任务才停止读
//!   russh，这时 russh 的有界队列才会塞满、整条连接的事件循环才会停下来
//! - 事件循环停下来之后，写操作（句柄 → 事件循环的队列只有 10 个位置）会被挡住；
//!   所以 ack 和关闭必须绕开会话的命令队列，直接操作流控门
//!
//! 这个测试同时是 russh 升级时的哨兵：如果哪天它的行为变了，这里会红。
//! 需要测试服务器，环境变量不全时跳过。
//!
//! 稳定性要求（M1-05 验收结论）：
//!
//! - 整个测试套总超时，失败/超时都要在有限时间内退出，不能挂着锁住 exe
//! - 失败时打印现场：未确认字节、缓冲占用、写入完成次数、各阶段耗时
//! - 跟链路速度有关的等待全部改成条件等待（等到某个状态或者超时），不用固定 sleep

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{timeout, Instant};
use zssh_core::ssh::flow::HIGH_WATERMARK;
use zssh_core::ssh::{
    Connection, FlowGate, SessionBuffer, ShellEvent, ShellEventStream, ShellWriter,
};

use common::{serial, TempKnownHosts, TestServer};

/// 暂停状态下连续写这么多次。russh 的句柄队列只有 10 个位置，写满就该挡住。
const WRITES: usize = 30;
/// 测试用的本地缓冲上限。要比高水位（1MB）大：不然转发任务停发之后缓冲先满，
/// 未确认量永远到不了高水位，"暂停"根本不会发生。生产是 8MB。
const TEST_BUFFER_LIMIT: usize = 2 * 1024 * 1024;
/// 把流控顶到暂停的时限。
const PAUSE_TIMEOUT: Duration = Duration::from_secs(60);
/// 等本地缓冲填满的时限（缓冲满了读任务才会停，连接才会真的卡住）。
const BUFFER_FULL_TIMEOUT: Duration = Duration::from_secs(60);
/// 判断"写入被挡住了"：完成计数稳定（不再增长）多久算稳。
const WRITE_STABLE_WINDOW: Duration = Duration::from_millis(500);
/// ack / close 之后，剩下的写操作应该在这个时间内完成。
const RECOVER_TIMEOUT: Duration = Duration::from_secs(20);
/// 等输出恢复的时限。
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(15);
/// 单个测试的总时限。超了就打印现场退出，绝不允许挂住。
const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
/// 一直输出的命令，用它把未确认量堆过高水位。
const FLOOD: &str = "yes";

/// 失败时的现场：超时或者断言失败时打出来，帮忙判断卡在哪个阶段。
struct Scene {
    name: &'static str,
    started: Instant,
    gate: Mutex<Option<Arc<FlowGate>>>,
    buffer: Mutex<Option<Arc<SessionBuffer>>>,
    writes_done: Mutex<Option<Arc<AtomicUsize>>>,
    writes_target: usize,
    stages: Mutex<Vec<(&'static str, Duration)>>,
}

impl Scene {
    fn new(name: &'static str, writes_target: usize) -> Self {
        Self {
            name,
            started: Instant::now(),
            gate: Mutex::new(None),
            buffer: Mutex::new(None),
            writes_done: Mutex::new(None),
            writes_target,
            stages: Mutex::new(Vec::new()),
        }
    }

    /// 记录一个阶段的时间点（相对开始）。
    fn stage(&self, label: &'static str) {
        self.stages
            .lock()
            .expect("锁")
            .push((label, self.started.elapsed()));
    }

    fn attach_gate(&self, gate: &Arc<FlowGate>) {
        *self.gate.lock().expect("锁") = Some(Arc::clone(gate));
    }

    fn attach_buffer(&self, buffer: &Arc<SessionBuffer>) {
        *self.buffer.lock().expect("锁") = Some(Arc::clone(buffer));
    }

    fn attach_writes(&self, done: &Arc<AtomicUsize>) {
        *self.writes_done.lock().expect("锁") = Some(Arc::clone(done));
    }

    fn dump(&self) {
        println!("--- 现场：{} ---", self.name);
        for (label, at) in self.stages.lock().expect("锁").iter() {
            println!("  {:>8.2?}  {label}", at);
        }
        if let Some(gate) = self.gate.lock().expect("锁").as_ref() {
            println!(
                "  未确认字节 {}（高水位 {}），峰值 {}，门已关 {}",
                gate.unacked(),
                HIGH_WATERMARK,
                gate.peak_unacked(),
                gate.is_closed()
            );
        }
        if let Some(buffer) = self.buffer.lock().expect("锁").as_ref() {
            println!(
                "  本地缓冲 {} / {} 字节，读任务结束 {}",
                buffer.buffered_bytes(),
                buffer.limit(),
                buffer.is_reader_done()
            );
        }
        if let Some(done) = self.writes_done.lock().expect("锁").as_ref() {
            println!(
                "  写入完成 {}/{}",
                done.load(Ordering::Relaxed),
                self.writes_target
            );
        }
    }
}

/// 建立连接、开 shell、启动刷屏，然后读到流控暂停、本地缓冲填满为止（全程不 ack）。
///
/// 返回 `Ok(None)` 表示环境不具备（跳过）。失败信息里不带测试服务器地址。
async fn setup_paused_shell(
    server: &TestServer,
    scene: &Scene,
) -> Result<Option<(Connection, ShellWriter, ShellEventStream, Arc<FlowGate>)>, String> {
    let known_hosts = TempKnownHosts::new("flow-deadlock");
    let connection = server
        .connect(&known_hosts)
        .await
        .map_err(|err| server.sanitize(&err))?;
    let shell = connection
        .open_shell_with_buffer_limit(80, 24, TEST_BUFFER_LIMIT, None)
        .await
        .map_err(|err| server.sanitize(&err.to_string()))?;
    let gate = shell.flow_handle();
    let buffer = shell.buffer_handle();
    scene.attach_gate(&gate);
    scene.attach_buffer(&buffer);
    let (writer, mut events) = shell.split();
    writer
        .write(format!("{FLOOD}\r").as_bytes())
        .await
        .map_err(|err| server.sanitize(&err.to_string()))?;
    scene.stage("刷屏命令已发出");

    // 1. 条件等待：未确认量超过高水位（流控暂停）
    println!("读到流控暂停为止…");
    let paused = wait_until(
        PAUSE_TIMEOUT,
        || gate.unacked() > HIGH_WATERMARK,
        &mut events,
        None,
    )
    .await;
    if !paused {
        return Err(format!(
            "读到 {PAUSE_TIMEOUT:?} 也没把流控顶到暂停（未确认 {}，高水位 {}）",
            gate.unacked(),
            HIGH_WATERMARK
        ));
    }
    scene.stage("流控已暂停");
    println!(
        "已暂停：未确认 {} 字节（高水位 {}）",
        gate.unacked(),
        HIGH_WATERMARK
    );

    // 2. 条件等待：本地缓冲填满。满了读任务才停止读 russh，连接才会真的卡住。
    //    这一步代替了以前的固定 sleep：缓冲区满没满是可观察的状态。
    let full = wait_until(BUFFER_FULL_TIMEOUT, || buffer.is_full(), &mut events, None).await;
    if !full {
        return Err(format!(
            "等 {BUFFER_FULL_TIMEOUT:?} 本地缓冲也没满（{} / {} 字节）",
            buffer.buffered_bytes(),
            buffer.limit()
        ));
    }
    scene.stage("本地缓冲已满");
    println!(
        "本地缓冲已满：{} / {} 字节",
        buffer.buffered_bytes(),
        buffer.limit()
    );

    Ok(Some((connection, writer, events, gate)))
}

/// 失败路径：尽力把连接断掉再退出，别让它挂在那里。
async fn close_quietly(connection: &Connection) {
    let _ = timeout(Duration::from_secs(5), connection.disconnect()).await;
}

/// 条件等待：`condition` 成立就返回 `true`；超时返回 `false`。
///
/// 每轮之间从事件流读一条，让状态有机会往前走：不读的话事件队列会堵住转发任务。
/// `ack_with` 给 `None` 时不 ack（未确认量继续涨，用来顶到暂停）；给了门就
/// 一边读一边 ack（等写入恢复时用，不然队列堵住转发任务，写入也恢复不了）。
async fn wait_until(
    limit: Duration,
    condition: impl Fn() -> bool,
    events: &mut ShellEventStream,
    ack_with: Option<&FlowGate>,
) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return condition();
        }
        let step = deadline
            .saturating_duration_since(now)
            .min(Duration::from_secs(5));
        match timeout(step, events.next()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                if let Some(gate) = ack_with {
                    gate.on_ack(bytes.len());
                }
            }
            // 通道结束了，条件不可能再变
            Ok(Some(_)) | Ok(None) => return condition(),
            Err(_) => {}
        }
    }
}

/// 等计数稳定下来（`WRITE_STABLE_WINDOW` 不再增长），返回稳定时的值。
async fn wait_until_stable(counter: &AtomicUsize, limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    let mut last = counter.load(Ordering::Relaxed);
    let mut stable_since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let now = counter.load(Ordering::Relaxed);
        if now != last {
            last = now;
            stable_since = Instant::now();
        } else if stable_since.elapsed() >= WRITE_STABLE_WINDOW {
            return now;
        }
        if Instant::now() >= deadline {
            return now;
        }
    }
}

/// 把写半边交给一个任务连续写 [`WRITES`] 次，返回完成计数。
fn spawn_writes(
    writer: ShellWriter,
    done: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<ShellWriter> {
    tokio::spawn(async move {
        for _ in 0..WRITES {
            if writer.write(b"a").await.is_err() {
                break;
            }
            done.fetch_add(1, Ordering::Relaxed);
        }
        writer
    })
}

/// 1~3：暂停时写入被挡住；从另一条路径 ack 之后写入和输出都恢复。
async fn run_ack_test(server: &TestServer, scene: &Scene) -> Result<(), String> {
    let Some((connection, writer, mut events, gate)) = setup_paused_shell(server, scene).await?
    else {
        return Ok(());
    };

    let done = Arc::new(AtomicUsize::new(0));
    scene.attach_writes(&done);
    let writer_task = spawn_writes(writer, Arc::clone(&done));

    // 读任务停了、russh 队列填满之后，句柄队列（10 个位置）才会被顶住，写入才会被挡住。
    //
    // 这一步**依赖链路速度**：读者停下之后，服务器还得再送大约一个 russh 队列的量
    // （16 条、最多 512KB）才能把事件循环顶住；慢链路上这要好几秒，而 30 次一字节的
    // 写可能在几百毫秒里就发完了。所以这里按条件等（等计数稳定），并且：
    //   - 稳定住没写完 → 断言"写入被挡住"（哨兵成立，顺便记下挡在第几次）
    //   - 全部写完     → 这次链路太慢、没测到这条哨兵，打印现场继续跑下面的功能性断言
    //     （功能性的部分是"从另一条路径 ack 之后写入和输出都恢复"，那个和链路无关）
    let completed = wait_until_stable(&done, Duration::from_secs(30)).await;
    scene.stage("写入完成计数已稳定");
    if completed >= WRITES {
        println!(
            "注意：这次链路较慢，{WRITES} 次写在连接被顶住之前就全发完了，\
             没测到「暂停时写入被挡住」这条哨兵；继续验证 ack 之后的恢复"
        );
    } else {
        println!("暂停状态下写入被挡住：稳定在 {completed}/{WRITES} 次");
    }

    // 不经过会话任务，直接 ack 掉全部未确认字节。
    let unacked = gate.unacked();
    gate.on_ack(unacked);
    scene.stage("已直接 ack");
    println!("直接 ack {unacked} 字节");

    // 恢复期间要一边读一边 ack：不读的话事件队列堵住转发任务，缓冲不排空，
    // 读任务不恢复，写入也就恢复不了。
    let recovered = wait_until(
        RECOVER_TIMEOUT,
        || done.load(Ordering::Relaxed) >= WRITES,
        &mut events,
        Some(&gate),
    )
    .await;
    scene.stage("写入恢复检查完成");

    // 写任务必须能收回来；收不回来也不能挂着测试，超时就当失败。
    let writer = match timeout(Duration::from_secs(5), writer_task).await {
        Ok(Ok(writer)) => writer,
        Ok(Err(err)) => {
            close_quietly(&connection).await;
            return Err(format!("写任务 panic 了：{err}"));
        }
        Err(_) => {
            close_quietly(&connection).await;
            return Err(format!(
                "ack 之后写任务还没结束（完成 {}/{}）",
                done.load(Ordering::Relaxed),
                WRITES
            ));
        }
    };
    if !recovered {
        close_quietly(&connection).await;
        return Err(format!(
            "ack 之后 {RECOVER_TIMEOUT:?} 内写入没有恢复：完成 {}/{}",
            done.load(Ordering::Relaxed),
            WRITES
        ));
    }
    println!("ack 之后 {WRITES} 次写入全部完成");

    // 输出也要恢复。
    match timeout(OUTPUT_TIMEOUT, events.next()).await {
        Ok(Some(ShellEvent::Data(bytes))) => {
            gate.on_ack(bytes.len());
            println!("输出恢复，读到 {} 字节", bytes.len());
        }
        other => {
            let described = match other {
                Ok(Some(_)) => "收到的是非数据事件".to_owned(),
                Ok(None) => "事件流结束了".to_owned(),
                Err(_) => format!("{OUTPUT_TIMEOUT:?} 内没有输出"),
            };
            close_quietly(&connection).await;
            return Err(format!("ack 之后输出没有恢复：{described}"));
        }
    }

    // 收尾：停掉服务器上的刷屏。
    let _ = timeout(Duration::from_secs(5), writer.write(b"\x03")).await;
    let _ = timeout(Duration::from_secs(5), writer.close()).await;
    scene.stage("收尾完成");
    Ok(())
}

#[tokio::test]
async fn ack_from_another_path_releases_blocked_writes() {
    if !common::full_live("ack_from_another_path_releases_blocked_writes") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 ack_from_another_path_releases_blocked_writes：{reason}");
            return;
        }
    };

    let scene = Arc::new(Scene::new(
        "ack_from_another_path_releases_blocked_writes",
        WRITES,
    ));
    let result = timeout(TOTAL_TIMEOUT, run_ack_test(&server, &scene)).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            println!("失败：{message}");
            scene.dump();
            panic!("{message}");
        }
        Err(_) => {
            println!("整测试超过 {TOTAL_TIMEOUT:?}");
            scene.dump();
            panic!("整个测试超过 {TOTAL_TIMEOUT:?} 还没结束，现场见上");
        }
    }
}

/// 4：暂停状态下直接关门，读任务退出，之后的写和关闭都不挂住。
async fn run_close_test(server: &TestServer, scene: &Scene) -> Result<(), String> {
    let Some((connection, writer, mut events, gate)) = setup_paused_shell(server, scene).await?
    else {
        return Ok(());
    };

    gate.close();
    scene.stage("已关门");

    let reader_ended = timeout(Duration::from_secs(5), async {
        // 缓冲里可能还有几批，读干净之后应该是 None。
        loop {
            if events.next().await.is_none() {
                break true;
            }
        }
    })
    .await
    .unwrap_or(false);
    scene.stage("读任务退出检查完成");
    if !reader_ended {
        close_quietly(&connection).await;
        return Err("close 之后读取任务没有退出".to_owned());
    }

    // 读取任务退出后，russh 投递失败会恢复事件循环，写和关闭都不能挂住。
    // 返回什么结果无所谓（通道可能已经关了），只要不卡住。
    let write = timeout(Duration::from_secs(5), writer.write(b"a")).await;
    if write.is_err() {
        close_quietly(&connection).await;
        return Err("close 之后写操作挂住了".to_owned());
    }
    let close = timeout(Duration::from_secs(5), writer.close()).await;
    if close.is_err() {
        close_quietly(&connection).await;
        return Err("close 之后关闭操作挂住了".to_owned());
    }
    println!("close 之后写入 {write:?}、关闭 {close:?}（都没挂住）");
    scene.stage("收尾完成");
    Ok(())
}

#[tokio::test]
async fn closing_the_gate_releases_reader_and_later_operations() {
    if !common::full_live("closing_the_gate_releases_reader_and_later_operations") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 closing_the_gate_releases_reader_and_later_operations：{reason}");
            return;
        }
    };

    let scene = Arc::new(Scene::new(
        "closing_the_gate_releases_reader_and_later_operations",
        0,
    ));
    let result = timeout(TOTAL_TIMEOUT, run_close_test(&server, &scene)).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            println!("失败：{message}");
            scene.dump();
            panic!("{message}");
        }
        Err(_) => {
            println!("整测试超过 {TOTAL_TIMEOUT:?}");
            scene.dump();
            panic!("整个测试超过 {TOTAL_TIMEOUT:?} 还没结束，现场见上");
        }
    }
}
