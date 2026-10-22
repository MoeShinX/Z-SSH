//! 联网测试的输出里不能带测试服务器地址（M3-3 遗留 5）。
//!
//! 前两条不联网：连接错误经 `TestServer` 的连接方法、`expect_ok`、panic 钩子出来，
//! 都要换掉地址。最后一条故意连测试服务器的一个错误端口，断言真实输出里没有
//! `ZSSH_TEST_HOST` 的值（没设环境变量就跳过）。

mod common;

use common::{TempKnownHosts, TestServer};

fn fake_server(host: &str, port: u16) -> TestServer {
    TestServer {
        host: host.to_owned(),
        port,
        username: "tester".to_owned(),
        password: "unused".to_owned(),
    }
}

/// 拿一个本机上肯定没人监听的端口。
fn closed_local_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("占端口");
    let port = listener.local_addr().expect("本机地址").port();
    drop(listener);
    port
}

#[test]
fn sanitize_and_panic_formatting_remove_the_host() {
    let server = fake_server("203.0.113.77", 22);
    common::install_scrubbing_panic_hook(&server.host);
    let raw = "connect to 203.0.113.77:22 failed: timed out";
    assert!(!server.sanitize(raw).contains("203.0.113.77"));
    let shown = common::format_panic(raw, "tests/x.rs:1:1");
    assert!(
        !shown.contains("203.0.113.77"),
        "panic 输出里还有地址：{shown}"
    );
    assert!(shown.contains("测试服务器"));
}

#[tokio::test]
async fn connect_errors_and_expect_ok_do_not_carry_the_host() {
    // 本机回环地址当作"服务器地址"登记：错误文本里若带它，必须被换掉。
    let server = fake_server("127.0.0.1", closed_local_port());
    common::install_scrubbing_panic_hook(&server.host);
    let known_hosts = TempKnownHosts::new("sanitize");
    let err = server
        .connect(&known_hosts)
        .await
        .err()
        .expect("连一个没人监听的端口应该失败");
    assert!(!err.contains("127.0.0.1"), "连接错误里还有地址：{err}");

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        server.expect_ok::<(), _>(
            Err("failed to connect to 127.0.0.1:9 (refused)".to_owned()),
            "连接测试服务器失败",
        )
    }))
    .expect_err("expect_ok 应该 panic");
    let text = panic
        .downcast_ref::<String>()
        .cloned()
        .expect("panic 内容是文本");
    assert!(!text.contains("127.0.0.1"), "panic 内容里还有地址：{text}");
    assert!(text.contains("连接测试服务器失败"));
}

#[tokio::test]
async fn wrong_port_on_the_real_test_server_leaks_nothing() {
    if !common::full_live("wrong_port_on_the_real_test_server_leaks_nothing") {
        return;
    }
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 wrong_port_on_the_real_test_server_leaks_nothing：{reason}");
            return;
        }
    };
    let host = server.host.clone();
    // 故意连错端口（1 号端口一般没人监听）。
    let wrong = TestServer {
        port: 1,
        ..fake_server(&host, 1)
    };
    let known_hosts = TempKnownHosts::new("sanitize-live");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(40),
        wrong.connect(&known_hosts),
    )
    .await;
    let err = match result {
        Ok(Err(err)) => err,
        Ok(Ok(_)) => panic!("1 号端口不应该连得上"),
        Err(_) => "40 秒超时".to_owned(),
    };
    println!("错误端口的连接结果（已过 sanitize）：{err}");
    assert!(!err.contains(&host), "连接错误里带了 ZSSH_TEST_HOST 的值");

    // 再走 expect_ok 的 panic 路径，取 panic 内容断言。
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wrong.expect_ok::<(), _>(
            Err::<(), _>(format!("connect {host}:1 failed")),
            "连接测试服务器失败",
        )
    }))
    .expect_err("应该 panic");
    let text = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        !text.contains(&host),
        "panic 内容里带了 ZSSH_TEST_HOST 的值"
    );
}
