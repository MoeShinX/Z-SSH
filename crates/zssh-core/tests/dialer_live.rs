//! 拨号链的联网集成测试：SOCKS5、跳板机，以及两者组合。
//!
//! 需要测试服务器；`ZSSH_TEST_SOCKS5` 没设置时 SOCKS5 相关的用例跳过。
//!
//! 跳过只留给"前置条件不具备"：服务器拒绝 TCP 转发（administratively prohibited）
//! 或者代理端口根本连不上。其余失败一律判失败——代理这条路坏了不能被跳过吞掉。

mod common;

use std::sync::Arc;
use std::time::Duration;

use zssh_core::model::ProxyKind;
use zssh_core::ssh::{AuthConfig, ChainHop, Connection, HostKeyDecision, ProxyConfig, ShellEvent};
use zssh_core::ZsshError;

use common::{optional, probe_tcp, serial, MockInteractor, TempKnownHosts, TestServer};

/// 跳板机测试里第二跳的地址：从测试服务器连它自己。
const LOOPBACK: &str = "127.0.0.1";
const LOOPBACK_PORT: u16 = 22;
/// 探测代理端口能不能连上的时限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 造一跳：用同一套凭据，指纹校验走同一份临时 known_hosts。
fn hop(
    server: &TestServer,
    label: &str,
    host: &str,
    port: u16,
    proxy: Option<ProxyConfig>,
    known_hosts: &TempKnownHosts,
) -> ChainHop {
    ChainHop {
        label: label.to_owned(),
        host: host.to_owned(),
        port,
        username: server.username.clone(),
        proxy,
        auth: AuthConfig::password(Some(server.password.clone())),
        known_hosts: known_hosts.known_hosts(),
        legacy_algorithms: false,
    }
}

/// 连上之后跑一条命令，确认链路真的能用。错误信息里的地址已经换成标签。
async fn run_marker(
    server: &TestServer,
    connection: &Connection,
    marker: &str,
) -> Result<String, String> {
    let mut shell = connection
        .open_shell(80, 24)
        .await
        .map_err(|err| server.sanitize(&err.to_string()))?;
    shell
        .write(format!("echo ZSSH_DIAL_$((40+2))_{marker}\r").as_bytes())
        .await
        .map_err(|err| server.sanitize(&err.to_string()))?;
    shell
        .write(b"exit\r")
        .await
        .map_err(|err| server.sanitize(&err.to_string()))?;

    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                output.extend_from_slice(&bytes);
            }
            Ok(Some(_)) | Ok(None) => break,
            Err(_) => break,
        }
    }
    // 提示符里可能带主机名，这里把地址换掉再交给断言和输出。
    Ok(server.sanitize(&String::from_utf8_lossy(&output)))
}

/// 解析 `ZSSH_TEST_SOCKS5`（形如 `127.0.0.1:10808`）。
fn socks5_from_env() -> Option<ProxyConfig> {
    let raw = optional("ZSSH_TEST_SOCKS5")?;
    let (host, port) = raw.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    Some(ProxyConfig {
        kind: ProxyKind::Socks5,
        host: host.to_owned(),
        port,
        username: None,
        password: None,
    })
}

/// 代理端口探不通就跳过（代理没在跑），探得通就返回 `true`，后面的失败都算真失败。
async fn proxy_is_listening(proxy: &ProxyConfig) -> bool {
    probe_tcp(&proxy.host, proxy.port, PROBE_TIMEOUT).await
}

/// 错误是不是"服务器禁止转发"这一类（唯一允许跳过的转发失败）。
fn is_forwarding_refused(err: &ZsshError) -> bool {
    matches!(
        err,
        ZsshError::Hop { detail, .. } if detail.contains("administratively prohibited")
    )
}

#[tokio::test]
async fn connects_through_socks5() {
    if !common::full_live("connects_through_socks5") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 connects_through_socks5：{reason}");
            return;
        }
    };
    let Some(proxy) = socks5_from_env() else {
        println!("跳过 connects_through_socks5：没有设置 ZSSH_TEST_SOCKS5");
        return;
    };
    if !proxy_is_listening(&proxy).await {
        println!(
            "跳过 connects_through_socks5：ZSSH_TEST_SOCKS5 指的本机代理端口连不上（代理没在跑？）"
        );
        return;
    }

    let known_hosts = TempKnownHosts::new("dial-socks5");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        Vec::new(),
    ));
    let hops = vec![hop(
        &server,
        &format!("代理 {}", proxy.label()),
        &server.host,
        server.port,
        Some(proxy.clone()),
        &known_hosts,
    )];

    // 代理端口是通的：从这一步开始不允许再跳过，连不上就是失败。
    let connection = match Connection::connect_chain(hops, Arc::clone(&interactor) as Arc<_>).await
    {
        Ok(connection) => connection,
        Err(err) => panic!(
            "代理端口连得上，但经它连测试服务器失败：{}",
            server.sanitize(&err.to_string())
        ),
    };

    let output = run_marker(&server, &connection, "SOCKS5")
        .await
        .expect("跑命令");
    assert!(
        output.contains("ZSSH_DIAL_42_SOCKS5"),
        "输出：{}",
        server.sanitize(&output)
    );
    println!("SOCKS5 连通，指纹提问 {} 次", interactor.asks());
}

#[tokio::test]
async fn connects_through_a_jump_host() {
    if !common::full_live("connects_through_a_jump_host") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 connects_through_a_jump_host：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("dial-jump");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        Vec::new(),
    ));
    // 第 1 跳是测试服务器自己，第 2 跳是它上面的 127.0.0.1:22
    let hops = vec![
        hop(
            &server,
            "跳板机 测试服务器",
            &server.host,
            server.port,
            None,
            &known_hosts,
        ),
        hop(
            &server,
            "目标 loopback",
            LOOPBACK,
            LOOPBACK_PORT,
            None,
            &known_hosts,
        ),
    ];

    let connection = match Connection::connect_chain(hops, Arc::clone(&interactor) as Arc<_>).await
    {
        // 只放过"服务器禁止转发"这一种：那是服务器配置问题，不是我们的 bug
        Err(ref err) if is_forwarding_refused(err) => {
            println!(
                "跳过：服务器不允许转发，两跳连不上：{}",
                server.sanitize(&err.to_string())
            );
            return;
        }
        Err(err) => panic!(
            "两跳应该能连上（除了服务器禁止转发），实际：{}",
            server.sanitize(&err.to_string())
        ),
        Ok(connection) => connection,
    };

    let output = run_marker(&server, &connection, "JUMP")
        .await
        .expect("跑命令");
    assert!(
        output.contains("ZSSH_DIAL_42_JUMP"),
        "输出：{}",
        server.sanitize(&output)
    );

    // 两跳的指纹各问一次，第二跳的主机名是 127.0.0.1；输出里只出现标签
    let infos = interactor.host_key_infos();
    let labels: Vec<String> = infos
        .iter()
        .map(|info| server.host_label(&info.host))
        .collect();
    assert_eq!(infos.len(), 2, "两跳都该过指纹校验，实际 {labels:?}");
    assert!(
        infos.iter().any(|info| info.host == LOOPBACK),
        "第二跳的主机名应该是 {LOOPBACK}：{labels:?}"
    );
    println!("两跳连通，指纹提问 {labels:?}");
}

#[tokio::test]
async fn connects_through_socks5_then_a_jump_host() {
    if !common::full_live("connects_through_socks5_then_a_jump_host") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 connects_through_socks5_then_a_jump_host：{reason}");
            return;
        }
    };
    let Some(proxy) = socks5_from_env() else {
        println!("跳过 connects_through_socks5_then_a_jump_host：没有设置 ZSSH_TEST_SOCKS5");
        return;
    };
    if !proxy_is_listening(&proxy).await {
        println!(
            "跳过 connects_through_socks5_then_a_jump_host：ZSSH_TEST_SOCKS5 指的本机代理端口连不上"
        );
        return;
    }

    let known_hosts = TempKnownHosts::new("dial-combo");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        Vec::new(),
    ));
    let hops = vec![
        // 第一跳经 SOCKS5 连到测试服务器
        hop(
            &server,
            &format!("代理 {}", proxy.label()),
            &server.host,
            server.port,
            Some(proxy.clone()),
            &known_hosts,
        ),
        // 第二跳在它上面开转发通道连到 127.0.0.1:22
        hop(
            &server,
            "目标 loopback",
            LOOPBACK,
            LOOPBACK_PORT,
            None,
            &known_hosts,
        ),
    ];

    let connection = match Connection::connect_chain(hops, Arc::clone(&interactor) as Arc<_>).await
    {
        Err(ref err) if is_forwarding_refused(err) => {
            println!(
                "跳过：服务器不允许转发：{}",
                server.sanitize(&err.to_string())
            );
            return;
        }
        // 代理端口是通的、转发也没被拒：这条组合链路失败就是真失败
        Err(err) => panic!(
            "SOCKS5 → 跳板机 → 目标 应该能连上，实际：{}",
            server.sanitize(&err.to_string())
        ),
        Ok(connection) => connection,
    };

    let output = run_marker(&server, &connection, "COMBO")
        .await
        .expect("跑命令");
    assert!(
        output.contains("ZSSH_DIAL_42_COMBO"),
        "输出：{}",
        server.sanitize(&output)
    );
    println!("SOCKS5 → 跳板机 → 目标 连通");
}
