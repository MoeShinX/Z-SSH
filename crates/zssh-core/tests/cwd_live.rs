//! 取终端当前目录（M3-3 第 3 节）：连测试服务器，开两个终端，各自 `cd` 到不同目录，
//! 用 exec 通道探测，核对每个终端取到的是自己的目录，并且探测没有往终端里写任何东西。
//!
//! 在完整联网档里跑（`scripts/live-tests.ps1`）。服务器不是 Linux / 没有 `/proc` 时
//! 打印原因并跳过。

mod common;

use std::time::Duration;

use zssh_core::ssh::{pick_shell, probe_shell_cwds, ShellEvent};

use common::{full_live, run_marker, serial, settle_terminal, TempKnownHosts, TestServer};

#[tokio::test]
async fn each_terminal_reports_its_own_directory_without_typing_into_it() {
    if !full_live("each_terminal_reports_its_own_directory_without_typing_into_it") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 cwd_live：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("cwd-live");
    let connection = server.connect(&known_hosts).await.expect("连接测试服务器");

    let mut first = connection.open_shell(100, 30).await.expect("开第一个终端");
    // 第二个终端晚一点开：pid 更大，顺序才和打开顺序一致。
    let mut second = connection.open_shell(100, 30).await.expect("开第二个终端");

    first.write(b"cd /tmp\r").await.expect("写入");
    let out = run_marker(&mut first, "cwd1", Duration::from_secs(15)).await;
    assert!(
        out.contains("ZSSH_cwd1_42"),
        "第一个终端没响应：{}",
        server.sanitize(&out)
    );
    second.write(b"cd /usr\r").await.expect("写入");
    let out = run_marker(&mut second, "cwd2", Duration::from_secs(15)).await;
    assert!(
        out.contains("ZSSH_cwd2_42"),
        "第二个终端没响应：{}",
        server.sanitize(&out)
    );

    // 两个终端上一条命令的提示符可能还没到（机器忙时晚到几百毫秒），先等它们安静，
    // 否则下面“探测后多出数据”会误报成探测写入了东西。
    settle_terminal(
        &mut first,
        Duration::from_millis(500),
        Duration::from_secs(8),
    )
    .await;
    settle_terminal(
        &mut second,
        Duration::from_millis(500),
        Duration::from_secs(8),
    )
    .await;

    let shells = probe_shell_cwds(&connection).await;
    println!("探测到 {} 个带 tty 的 shell", shells.len());
    if shells.is_empty() {
        println!("跳过断言：服务器上取不到终端目录（没有 /proc 或 ps），按设计安静退回家目录");
        return;
    }
    assert_eq!(shells.len(), 2, "两个终端应该探出两个 shell");
    let a = pick_shell(&shells, 0, 2).expect("第一个终端");
    let b = pick_shell(&shells, 1, 2).expect("第二个终端");
    println!("第一个终端目录：{}；第二个终端目录：{}", a.cwd, b.cwd);
    assert_eq!(a.cwd, "/tmp");
    assert_eq!(b.cwd, "/usr");
    // 数量对不上就不猜。
    assert!(pick_shell(&shells, 0, 1).is_none());

    // cd 之后再探一次，跟得上。
    first.write(b"cd /var\r").await.expect("写入");
    let out = run_marker(&mut first, "cwd3", Duration::from_secs(15)).await;
    assert!(out.contains("ZSSH_cwd3_42"));
    let shells = probe_shell_cwds(&connection).await;
    assert_eq!(
        pick_shell(&shells, 0, 2).map(|s| s.cwd.as_str()),
        Some("/var")
    );

    // 探测没有往终端里写东西：等一会儿，两个终端都不该再冒出数据。
    for (name, shell) in [("第一个", &mut first), ("第二个", &mut second)] {
        let extra = tokio::time::timeout(Duration::from_millis(600), shell.next_event()).await;
        match extra {
            Err(_) => {}
            Ok(Some(ShellEvent::Data(bytes))) => {
                panic!(
                    "{name}终端在探测后多出了数据：{:?}",
                    String::from_utf8_lossy(&bytes)
                )
            }
            Ok(_) => {}
        }
    }
    let _ = connection.disconnect().await;
}
