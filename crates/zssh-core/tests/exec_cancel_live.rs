//! 后台 exec 的取消顺序（R-9 F12）：取消标志在通道打开之后、命令发出之前置位时，命令不能发出去。
//!
//! 单元测试只能用替身模拟这个顺序；这里在测试服务器上确认真实连接
//! （`Connection::exec_capture_cancellable`）确实不会把命令发出去。归完整档。
//!
//! 审计 C04：AI 后台执行在发 exec 的边界上核对的是完整的有效性（停止 + 目标移出），
//! 这里也用真实连接确认 `Connection::exec_capture_guarded` 在开通道期间 / 开好之后
//! 目标被移出时不发命令。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{full_live, TempKnownHosts, TestServer};
use zssh_core::ssh::connection::ExecGuard;
use zssh_core::ssh::{BoxFuture, Connection};

/// 前 `valid_checks` 次 `check` 放行，之后按"目标已移出"拒绝。
///
/// `invalidated` 在 `removed` 为真时立刻返回，否则一直等。`remove_after_first_check` 让第一次
/// 核对放行之后马上标记移出：移出正好落在"开通道"那一个来回里。
struct ScriptedGuard {
    valid_checks: usize,
    checks: AtomicUsize,
    removed: AtomicBool,
    remove_after_first_check: bool,
}

const REMOVED: &str = "这台服务器已经被移出这个对话，这次操作已取消";

impl ExecGuard for ScriptedGuard {
    fn check(&self) -> Result<(), String> {
        let n = self.checks.fetch_add(1, Ordering::SeqCst);
        if n < self.valid_checks && !self.removed.load(Ordering::SeqCst) {
            if self.remove_after_first_check {
                self.removed.store(true, Ordering::SeqCst);
            }
            Ok(())
        } else {
            Err(REMOVED.to_owned())
        }
    }

    fn invalidated(&self) -> BoxFuture<'_, String> {
        Box::pin(async move {
            if self.removed.load(Ordering::SeqCst) {
                REMOVED.to_owned()
            } else {
                std::future::pending().await
            }
        })
    }

    fn stop_requested(&self) -> bool {
        false
    }
}

fn unique_marker(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("/tmp/zssh-{tag}-{nanos:x}")
}

/// 看标记文件在不在，并顺手删掉（别在服务器上留垃圾）。
async fn marker_exists(server: &TestServer, connection: &Connection, marker: &str) -> bool {
    let check = server.expect_ok(
        connection
            .exec_capture(
                &format!("test -e '{marker}' && echo present || echo absent"),
                Duration::from_secs(10),
                4096,
            )
            .await,
        "检查标记文件",
    );
    let _ = connection
        .exec_capture(&format!("rm -f '{marker}'"), Duration::from_secs(10), 4096)
        .await;
    check.stdout.trim() == "present"
}

#[tokio::test]
async fn removing_the_target_after_the_channel_opens_never_sends_the_command() {
    // C04 原场景：放行时核对通过 → 开通道 → 这期间目标被移出 → 通道开好。以前这里只看停止标志
    // （false），命令照发；现在发 exec 之前再完整核对一次。
    let name = "removing_the_target_after_the_channel_opens_never_sends_the_command";
    if !full_live(name) {
        return;
    }
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 {name}：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("exec-guard-open");
    let connection =
        Arc::new(server.expect_ok(server.connect(&known_hosts).await, "连接测试服务器"));
    let marker = unique_marker("guard-open");
    // 第一次核对（开通道前）放行，第二次（通道开好、发 exec 前）拒绝。
    let guard = ScriptedGuard {
        valid_checks: 1,
        checks: AtomicUsize::new(0),
        removed: AtomicBool::new(false),
        remove_after_first_check: false,
    };
    let opened_before = connection.channels_opened();
    let result = connection
        .exec_capture_guarded(
            &format!("touch '{marker}'"),
            Duration::from_secs(10),
            4096,
            &guard,
        )
        .await;
    let message = match result {
        Ok(capture) => panic!("目标已移出，命令不该有执行结果：{:?}", capture.exit_code),
        Err(err) => server.sanitize(&err),
    };
    assert!(message.contains("移出"), "{message}");
    assert_eq!(
        connection.channels_opened(),
        opened_before + 1,
        "通道确实开好了（拦在开通道之后、发命令之前）"
    );
    assert_eq!(
        guard.checks.load(Ordering::SeqCst),
        2,
        "开通道前后各核对一次"
    );
    assert!(
        !marker_exists(&server, &connection, &marker).await,
        "目标被移出之后命令不能被执行（标记文件不该存在）"
    );
}

#[tokio::test]
async fn removing_the_target_while_the_channel_opens_gives_up_without_running() {
    // 开通道期间就收到"目标已移出"：立刻放弃等待，不发命令；迟到的通道由后台关掉，
    // 连接照常能用。
    let name = "removing_the_target_while_the_channel_opens_gives_up_without_running";
    if !full_live(name) {
        return;
    }
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 {name}：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("exec-guard-opening");
    let connection =
        Arc::new(server.expect_ok(server.connect(&known_hosts).await, "连接测试服务器"));
    let marker = unique_marker("guard-opening");
    let guard = ScriptedGuard {
        valid_checks: usize::MAX,
        checks: AtomicUsize::new(0),
        removed: AtomicBool::new(false),
        remove_after_first_check: true,
    };
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        connection.exec_capture_guarded(
            &format!("touch '{marker}'"),
            Duration::from_secs(10),
            4096,
            &guard,
        ),
    )
    .await
    .expect("失效后要很快返回");
    let message = match result {
        Ok(capture) => panic!("目标已移出，命令不该有执行结果：{:?}", capture.exit_code),
        Err(err) => server.sanitize(&err),
    };
    assert_eq!(
        guard.checks.load(Ordering::SeqCst),
        1,
        "没等通道开好就放弃了（开好之后的那次核对没有发生）"
    );
    assert!(message.contains("移出"), "{message}");
    println!(
        "开通道期间移出目标，{} ms 后放弃",
        started.elapsed().as_millis()
    );
    // 后台收掉迟到的通道需要一个来回：等一会儿再查，连接照常可用。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !marker_exists(&server, &connection, &marker).await,
        "目标被移出之后命令不能被执行（标记文件不该存在）"
    );
}

#[tokio::test]
async fn a_command_cancelled_before_it_is_sent_never_runs() {
    if !full_live("a_command_cancelled_before_it_is_sent_never_runs") {
        return;
    }
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 a_command_cancelled_before_it_is_sent_never_runs：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("exec-cancel");
    let connection = server.expect_ok(server.connect(&known_hosts).await, "连接测试服务器");

    // 每次一个新名字：上一次失败留下的文件不会让这次误判。
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let marker = format!("/tmp/zssh-cancel-{nanos:x}");

    let cancel = AtomicBool::new(true);
    let result = connection
        .exec_capture_cancellable(
            &format!("touch '{marker}'"),
            Duration::from_secs(10),
            4096,
            &cancel,
        )
        .await;
    let message = match result {
        Ok(capture) => panic!("已取消的命令不该有执行结果：{:?}", capture.exit_code),
        Err(err) => server.sanitize(&err.to_string()),
    };
    assert!(message.contains("没有发出"), "{message}");

    let check = server.expect_ok(
        connection
            .exec_capture(
                &format!("test -e '{marker}' && echo present || echo absent"),
                Duration::from_secs(10),
                4096,
            )
            .await,
        "检查标记文件",
    );
    // 万一真的建出来了也清掉，别在服务器上留垃圾。
    let _ = connection
        .exec_capture(&format!("rm -f '{marker}'"), Duration::from_secs(10), 4096)
        .await;
    assert_eq!(
        check.stdout.trim(),
        "absent",
        "取消之后命令不能被执行（标记文件不该存在）"
    );
}
