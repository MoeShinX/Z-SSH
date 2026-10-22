//! 连真实服务器跑一遍最短链路：连接 → 开 shell → 发命令 → 读结果。
//!
//! 环境变量 `ZSSH_TEST_HOST` / `ZSSH_TEST_PORT`（默认 22）/ `ZSSH_TEST_USER` /
//! `ZSSH_TEST_PASSWORD` 不全时跳过，并打印原因，不算失败。

mod common;

use std::time::Duration;

use zssh_core::ssh::{ShellEvent, ShellSession};

use common::{serial, TempKnownHosts, TestServer};

/// 远端算出来的结果。回显里只有 `$((40+2))`，出现 42 说明命令真的跑过了。
const PROBE_MARKER: &str = "ZSSH_PROBE_42";
const PROBE_COMMAND: &str = "echo ZSSH_PROBE_$((40+2))";
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);
const COLS: u16 = 80;
const ROWS: u16 = 24;

#[tokio::test]
async fn live_shell_runs_command_and_returns_output() {
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 live_shell_runs_command_and_returns_output：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("live-shell");
    let connection = server
        .connect(&known_hosts)
        .await
        .expect("连接测试服务器失败");

    let key = connection.server_key();
    println!("主机密钥：{} {}", key.key_type, key.fingerprint);
    assert!(
        key.fingerprint.starts_with("SHA256:"),
        "指纹不是 OpenSSH 的 SHA256 格式：{}",
        key.fingerprint
    );

    let mut shell = connection
        .open_shell(COLS, ROWS)
        .await
        .expect("打开 shell 失败");
    shell
        .write(format!("{PROBE_COMMAND}\r").as_bytes())
        .await
        .expect("写入命令失败");
    shell.write(b"exit\r").await.expect("写入 exit 失败");

    let text = String::from_utf8_lossy(&collect_output(&mut shell).await).into_owned();
    assert!(
        text.contains(PROBE_MARKER),
        "输出里没有 {PROBE_MARKER}，收到的是：{}",
        server.sanitize(&text)
    );
}

/// 读到 Exit、Closed 或者超时为止。
async fn collect_output(shell: &mut ShellSession) -> Vec<u8> {
    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + COLLECT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => output.extend_from_slice(&bytes),
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break,
            Err(_) => break,
        }
    }
    output
}
