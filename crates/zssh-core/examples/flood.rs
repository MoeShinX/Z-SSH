//! 刷屏压测：攒批、流控和 Ctrl+C 延迟。
//!
//! 环境变量和 `probe` 一样（`ZSSH_TEST_HOST` / `ZSSH_TEST_PORT` / `ZSSH_TEST_USER`
//! / `ZSSH_TEST_PASSWORD`）。跑三遍：
//!
//! 1. 快消费者：收到就 ack，量后端不加流控时的吞吐上限。
//! 2. 慢消费者：每批 sleep 一下再 ack，看未确认字节会不会超过高水位加一个批次。
//! 3. Ctrl+C：输出两秒后发 `\x03`，量到提示符回来的时间。
//!
//! ```text
//! cargo run -p zssh-core --example flood
//! ```

use std::process::ExitCode;
use std::time::Duration;

use tokio::time::Instant;

use std::sync::Arc;

use zssh_core::ssh::{
    AuthConfig, ConnectParams, Connection, KnownHosts, NonInteractive, ShellEvent, ShellSession,
};

/// 100MB 的 0 字节经 base64（默认每 76 字节折行）变成约 141MB 输出。
const FLOOD_COMMAND: &str = "head -c 104857600 /dev/zero | base64";
/// 灌完输出之后紧跟着跑这个，用远端算出来的 42 判断输出真的到头了。
/// 回显里只有 `$((40+2))`，所以不会误判。
const FLOOD_DONE_COMMAND: &str = "echo ZSSH_FLOOD_$((40+2))";
const FLOOD_DONE_MARKER: &str = "ZSSH_FLOOD_42";

/// Ctrl+C 之后单独发这条，确认 shell 还能用。不能和 `\x03` 一起发：
/// readline 收到 SIGINT 会把同一个读里排队的输入一起丢掉。
const ALIVE_MARKER_COMMAND: &str = "echo ZSSH_ALIVE_$((6*7))";
const ALIVE_MARKER: &str = "ZSSH_ALIVE_42";
/// 发完 `\x03` 之后单独发探针的间隔。
const ALIVE_PROBE_DELAY: Duration = Duration::from_millis(300);

/// 慢消费者每批渲完停这么久，模拟界面处理不过来。20ms/批 ≈ 3MB/s，
/// 明显低于这条链路 5~6MB/s 的上限，这样流控门才会真的成为瓶颈。
const SLOW_CONSUMER_DELAY: Duration = Duration::from_millis(20);
/// 输出开始多久之后发 Ctrl+C。
const CTRL_C_AFTER: Duration = Duration::from_secs(2);
/// 判断"输出停了"用的静默时长。只用来判断，不进入上报的延迟里。
const QUIET_AFTER_CTRL_C: Duration = Duration::from_millis(500);
/// 单轮压测的总时限，防止示例挂死。
const RUN_TIMEOUT: Duration = Duration::from_secs(300);
/// 超过这么久没有新数据就算输出结束（标记万一没匹配上时的兜底）。
const QUIET_PERIOD: Duration = Duration::from_secs(3);
/// 判断标记时只需要看输出尾部。
const TAIL_BYTES: usize = 512;

#[tokio::main]
async fn main() -> ExitCode {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    if let Err(message) = run_all(&server).await {
        eprintln!("压测失败：{message}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn run_all(server: &TestServer) -> Result<(), String> {
    println!("=== 快消费者（收到就 ack）===");
    let fast = run_flood(server, None).await?;
    print_flood_stats(&fast);

    println!();
    println!("=== 慢消费者（每批再等 {SLOW_CONSUMER_DELAY:?} 才 ack）===");
    let slow = run_flood(server, Some(SLOW_CONSUMER_DELAY)).await?;
    print_flood_stats(&slow);

    println!();
    println!("=== Ctrl+C 延迟（输出 {CTRL_C_AFTER:?} 后发 \\x03）===");
    let ctrl_c = measure_ctrl_c(server).await?;
    println!(
        "Ctrl+C → 输出停止 {:.3} 秒（目标 1 秒以内）",
        ctrl_c.stopped_after.as_secs_f64()
    );
    println!(
        "发 \\x03 前两秒吞吐 {:.1} MB/s；发出 \\x03 之后还读到 {} 字节（{:.2} MB）才停",
        ctrl_c.before_rate_mb_s,
        ctrl_c.drained_bytes,
        ctrl_c.drained_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "Ctrl+C 之后 shell {}",
        if ctrl_c.shell_alive {
            "仍然可用（探针命令有回显）"
        } else {
            "没有响应"
        }
    );

    println!();
    match peak_working_set_mb() {
        Some(mb) => println!("进程内存峰值（工作集）{mb:.1} MB"),
        None => println!("进程内存峰值：这台机器读不到"),
    }
    Ok(())
}

struct FloodStats {
    total_bytes: usize,
    batches: usize,
    elapsed: Duration,
    peak_unacked: usize,
}

fn print_flood_stats(stats: &FloodStats) {
    let seconds = stats.elapsed.as_secs_f64();
    let mb_per_second = if seconds > 0.0 {
        stats.total_bytes as f64 / (1024.0 * 1024.0) / seconds
    } else {
        0.0
    };
    println!(
        "总字节 {}（{:.1} MB），耗时 {:.2} 秒，平均吞吐 {:.1} MB/s",
        stats.total_bytes,
        stats.total_bytes as f64 / (1024.0 * 1024.0),
        seconds,
        mb_per_second
    );
    println!(
        "批次 {}，未确认字节峰值 {}（高水位 1024KB + 一个批次 64KB 之内算正常）",
        stats.batches, stats.peak_unacked
    );
}

/// 灌一轮输出，读到自己发的结束标记为止。
///
/// `slow_consumer` 有值时每批渲完先睡一会儿再 ack，模拟前端跟不上。
async fn run_flood(
    server: &TestServer,
    slow_consumer: Option<Duration>,
) -> Result<FloodStats, String> {
    let connection = server.connect().await?;
    let mut shell = open_shell(&connection).await?;

    write_command(
        &mut shell,
        &format!("{FLOOD_COMMAND}\r{FLOOD_DONE_COMMAND}\r"),
    )
    .await?;

    let started = Instant::now();
    let stats = drain(&mut shell, started, FLOOD_DONE_MARKER, slow_consumer, None).await?;
    Ok(stats)
}

/// Ctrl+C 的结果。
struct CtrlCStats {
    /// 从发出 `\x03` 到输出停下（最后一批数据到达）的时间。
    stopped_after: Duration,
    /// 发 `` 之前那两秒的平均吞吐，用来说明在途数据是快是慢。
    before_rate_mb_s: f64,
    /// 从发出 `` 到输出停下这段时间还读到了多少字节，也就是当时在途的数据量。
    drained_bytes: usize,
    /// 之后单独发的探针命令有没有回来，用来确认 shell 还停在提示符上。
    shell_alive: bool,
}

/// 输出两秒之后发 Ctrl+C，量到输出停下的时间。
///
/// 不用"发 `\x03` 时顺手带一条 echo 命令"的写法：readline 收到 SIGINT 会把同一个
/// 读里排队的输入一起丢掉，命令根本没机会执行。所以这里量的是"输出停止"，然后
/// 另外单独发一条命令确认 shell 还能用。
async fn measure_ctrl_c(server: &TestServer) -> Result<CtrlCStats, String> {
    let connection = server.connect().await?;
    let mut shell = open_shell(&connection).await?;
    write_command(&mut shell, &format!("{FLOOD_COMMAND}\r")).await?;

    // 先让输出跑起来，这两秒里按快消费者读，免得流控把服务器一起停住。
    let before_started = Instant::now();
    let until_ctrl_c = before_started + CTRL_C_AFTER;
    let before_bytes = drain_until(&mut shell, until_ctrl_c, None).await?;
    let before_seconds = before_started.elapsed().as_secs_f64().max(0.001);
    let before_rate_mb_s = before_bytes as f64 / (1024.0 * 1024.0) / before_seconds;

    let started = Instant::now();
    write_command(&mut shell, "\u{3}").await?;

    let hard_deadline = started + Duration::from_secs(10);
    let mut last_data: Option<Instant> = None;
    let mut drained_bytes = 0usize;
    loop {
        let now = Instant::now();
        if now >= hard_deadline {
            break;
        }
        let quiet_deadline = match last_data {
            Some(at) => at + QUIET_AFTER_CTRL_C,
            // 一批数据都没收到时，最多等两秒就认为本来就没有输出。
            None => started + Duration::from_secs(2),
        };

        match tokio::time::timeout_at(quiet_deadline.min(hard_deadline), shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                drained_bytes += bytes.len();
                last_data = Some(Instant::now());
            }
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break,
            Err(_) => {
                if let Some(at) = last_data {
                    if Instant::now() >= at + QUIET_AFTER_CTRL_C {
                        break;
                    }
                } else if Instant::now() >= started + Duration::from_secs(2) {
                    break;
                }
            }
        }
    }

    let stopped_after = last_data
        .map(|at| at.saturating_duration_since(started))
        .unwrap_or_default();

    // 单独发探针：能回来就说明 shell 回到了提示符而且还能执行命令。
    tokio::time::sleep(ALIVE_PROBE_DELAY).await;
    write_command(&mut shell, &format!("{ALIVE_MARKER_COMMAND}\r")).await?;

    let probe_deadline = Instant::now() + Duration::from_secs(5);
    let mut tail = Vec::new();
    let shell_alive = loop {
        match tokio::time::timeout_at(probe_deadline, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                tail.extend_from_slice(&bytes);
                if tail.len() > TAIL_BYTES {
                    tail.drain(..tail.len() - TAIL_BYTES);
                }
                if marker_found(&tail, ALIVE_MARKER) {
                    break true;
                }
            }
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break false,
            Err(_) => break false,
        }
    };

    Ok(CtrlCStats {
        before_rate_mb_s,
        stopped_after,
        drained_bytes,
        shell_alive,
    })
}

/// 一直读，直到看到 `marker`、对端结束、静默超过 [`QUIET_PERIOD`] 或者超过 `limit`。
async fn drain(
    shell: &mut ShellSession,
    started: Instant,
    marker: &str,
    slow_consumer: Option<Duration>,
    limit: Option<Instant>,
) -> Result<FloodStats, String> {
    let limit = limit.unwrap_or(started + RUN_TIMEOUT);
    let mut tail = Vec::new();
    let mut total_bytes = 0usize;
    let mut batches = 0usize;
    let mut last_data = started;

    loop {
        let now = Instant::now();
        if now >= limit {
            return Err(format!("压测超过 {RUN_TIMEOUT:?}"));
        }
        let quiet_deadline = last_data + QUIET_PERIOD;
        if now >= quiet_deadline && batches > 0 {
            // 静默太久，按输出结束处理。
            break;
        }
        let wait_until = limit.min(quiet_deadline);

        match tokio::time::timeout_at(wait_until, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                total_bytes += bytes.len();
                batches += 1;
                last_data = Instant::now();
                tail.extend_from_slice(&bytes);
                if tail.len() > TAIL_BYTES {
                    tail.drain(..tail.len() - TAIL_BYTES);
                }
                if let Some(delay) = slow_consumer {
                    tokio::time::sleep(delay).await;
                }
                // 前面每批都渲完了，这里一次性 ack 掉。
                shell.ack(bytes.len() as u32);
                if marker_found(&tail, marker) {
                    break;
                }
            }
            // 退出码和关闭都不影响统计，读干净就行。
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break,
            Err(_) => {
                if marker_found(&tail, marker) || batches == 0 {
                    break;
                }
            }
        }
    }

    Ok(FloodStats {
        total_bytes,
        batches,
        elapsed: last_data.saturating_duration_since(started),
        peak_unacked: shell.flow().peak_unacked(),
    })
}

/// 只读不统计，读到 `until` 或者看到 `marker` 为止，返回读到的字节数。
async fn drain_until(
    shell: &mut ShellSession,
    until: Instant,
    marker: Option<&str>,
) -> Result<usize, String> {
    let mut tail = Vec::new();
    let mut total = 0usize;
    loop {
        match tokio::time::timeout_at(until, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                total += bytes.len();
                if let Some(marker) = marker {
                    tail.extend_from_slice(&bytes);
                    if tail.len() > TAIL_BYTES {
                        tail.drain(..tail.len() - TAIL_BYTES);
                    }
                    if marker_found(&tail, marker) {
                        return Ok(total);
                    }
                }
                shell.ack(bytes.len() as u32);
            }
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => {
                return Ok(total)
            }
            Err(_) => return Ok(total),
        }
    }
}

fn marker_found(tail: &[u8], marker: &str) -> bool {
    String::from_utf8_lossy(tail).contains(marker)
}

async fn open_shell(connection: &Connection) -> Result<ShellSession, String> {
    connection
        .open_shell(80, 24)
        .await
        .map_err(|err| err.to_string())
}

async fn write_command(shell: &mut ShellSession, command: &str) -> Result<(), String> {
    shell
        .write(command.as_bytes())
        .await
        .map_err(|err| err.to_string())
}

/// 进程内存峰值（工作集），和任务管理器一个口径。
#[cfg(windows)]
fn peak_working_set_mb() -> Option<f64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = u32::try_from(core::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?;
    // SAFETY: 句柄是当前进程的伪句柄，结构体是本地的，大小按 size_of 给。
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).ok()? };
    Some(counters.PeakWorkingSetSize as f64 / (1024.0 * 1024.0))
}

#[cfg(not(windows))]
fn peak_working_set_mb() -> Option<f64> {
    None
}

struct TestServer {
    host: String,
    port: u16,
    username: String,
    password: String,
    /// known_hosts 的目录守卫：随 `TestServer` 一起删。
    known_hosts_dir: tempfile::TempDir,
}

impl TestServer {
    /// 返回的错误信息只提变量名，不带里面的值。
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            host: require("ZSSH_TEST_HOST")?,
            port: match std::env::var("ZSSH_TEST_PORT") {
                Ok(raw) if !raw.trim().is_empty() => raw
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "ZSSH_TEST_PORT 不是合法端口".to_owned())?,
                _ => 22,
            },
            username: require("ZSSH_TEST_USER")?,
            password: require("ZSSH_TEST_PASSWORD")?,
            known_hosts_dir: tempfile::Builder::new()
                .prefix("zssh-flood-")
                .tempdir()
                .map_err(|err| format!("建临时目录失败：{err}"))?,
        })
    }

    async fn connect(&self) -> Result<Connection, String> {
        // 压测不做交互：主机密钥"仅本次"，需要提问时直接取消
        Connection::connect(
            ConnectParams {
                host: self.host.clone(),
                port: self.port,
                username: self.username.clone(),
                auth: AuthConfig::password(Some(self.password.clone())),
                known_hosts: KnownHosts::new(self.known_hosts_dir.path().join("known_hosts")),
                legacy_algorithms: false,
            },
            Arc::new(NonInteractive),
        )
        .await
        .map_err(|err| err.to_string())
    }
}

fn require(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("缺少环境变量 {name}")),
    }
}
