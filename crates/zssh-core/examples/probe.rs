//! 手工验证用的探针：连测试服务器，开 shell，跑一条命令，看结果。
//!
//! 环境变量 `ZSSH_TEST_HOST` / `ZSSH_TEST_PORT`（默认 22）/ `ZSSH_TEST_USER` /
//! `ZSSH_TEST_PASSWORD`，缺任何一个就打印缺哪个并以 2 退出。
//!
//! ```text
//! cargo run -p zssh-core --example probe
//! ```

use std::process::ExitCode;
use std::time::Duration;

use std::sync::Arc;

use zssh_core::ssh::{
    AuthConfig, ConnectParams, Connection, KnownHosts, NonInteractive, ShellEvent, ShellSession,
};

/// 远端算出来的结果，用来区分回显和真正的执行结果。
const PROBE_MARKER: &str = "ZSSH_PROBE_42";
/// 命令里故意写成算式：PTY 回显的是 `$((40+2))`，只有远端算过之后才会出现 42。
const PROBE_COMMAND: &str = "echo ZSSH_PROBE_$((40+2))";
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);
const COLS: u16 = 80;
const ROWS: u16 = 24;

#[tokio::main]
async fn main() -> ExitCode {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    // 探针不做交互：主机密钥"仅本次"，需要提问时直接取消
    let known_hosts_dir = tempfile::Builder::new()
        .prefix("zssh-probe-")
        .tempdir()
        .expect("建临时目录");
    let params = ConnectParams {
        host: server.host,
        port: server.port,
        username: server.username,
        auth: AuthConfig::password(Some(server.password)),
        known_hosts: KnownHosts::new(known_hosts_dir.path().join("known_hosts")),
        legacy_algorithms: false,
    };

    let connection = match Connection::connect(params, Arc::new(NonInteractive)).await {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("连接失败：{err}");
            return ExitCode::from(1);
        }
    };

    let key = connection.server_key();
    println!("主机密钥：{} {}", key.key_type, key.fingerprint);

    let mut shell = match connection.open_shell(COLS, ROWS).await {
        Ok(shell) => shell,
        Err(err) => {
            eprintln!("打开 shell 失败：{err}");
            return ExitCode::from(1);
        }
    };

    if let Err(err) = shell.write(format!("{PROBE_COMMAND}\r").as_bytes()).await {
        eprintln!("写入命令失败：{err}");
        return ExitCode::from(1);
    }
    if let Err(err) = shell.write(b"exit\r").await {
        eprintln!("写入 exit 失败：{err}");
        return ExitCode::from(1);
    }

    let output = collect_output(&mut shell).await;
    let text = String::from_utf8_lossy(&output);

    if text.contains(PROBE_MARKER) {
        println!("PROBE OK");
        ExitCode::SUCCESS
    } else {
        println!("PROBE FAIL");
        println!("--- 收到的输出 ---");
        print!("{text}");
        println!("--- 输出结束 ---");
        ExitCode::from(1)
    }
}

/// 读到 Exit、Closed 或者超时为止。
///
/// 同一条通道上数据先于 exit-status 到达，所以见到 Exit 就不用再等了。
async fn collect_output(shell: &mut ShellSession) -> Vec<u8> {
    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + COLLECT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => output.extend_from_slice(&bytes),
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break,
            Err(_) => {
                eprintln!("收集输出超过 {COLLECT_TIMEOUT:?}，按已经收到的内容判断");
                break;
            }
        }
    }
    output
}

struct TestServer {
    host: String,
    port: u16,
    username: String,
    password: String,
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
                    .map_err(|_| format!("ZSSH_TEST_PORT 不是合法端口：{:?}", raw.trim()))?,
                _ => 22,
            },
            username: require("ZSSH_TEST_USER")?,
            password: require("ZSSH_TEST_PASSWORD")?,
        })
    }
}

fn require(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("缺少环境变量 {name}")),
    }
}
