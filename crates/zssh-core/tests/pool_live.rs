//! 连接复用（设计文档 4.1）的联网测试：
//!
//! - 同一主机开两个 shell，认证和指纹只发生一次
//! - 两个 shell 各跑各的命令，输出不串
//! - 最后一个会话放开之后，空闲时长（测试里 1 秒）一到连接就断；空闲期间再打开会复用
//! - 主机配置指纹变了就换一条新连接
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use zssh_core::ssh::{
    AuthConfig, ChainHop, ConnectionPool, HostKeyDecision, PoolConfig, PromptKind,
};

use common::{run_marker, serial, MockInteractor, TempKnownHosts, TestServer};

/// 测试里的空闲时长（卡片要求做成可配置，测试改成 1 秒）。
const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
/// 等池子把空闲连接收掉的时限。
const IDLE_DEADLINE: Duration = Duration::from_secs(15);
/// 跑一条命令的时限。
const MARKER_TIMEOUT: Duration = Duration::from_secs(15);

fn hop(server: &TestServer, known_hosts: &TempKnownHosts) -> ChainHop {
    ChainHop {
        label: "目标 测试服务器".to_owned(),
        host: server.host.clone(),
        port: server.port,
        username: server.username.clone(),
        proxy: None,
        // 密码故意不放在配置里：认证时要弹窗问，弹窗次数就是认证次数
        auth: AuthConfig::password(None),
        known_hosts: known_hosts.known_hosts(),
        legacy_algorithms: false,
    }
}

/// 只回答一次密码的交互：复用成功的话，第二次连接就不该再问。
fn interactor(server: &TestServer, answers: usize) -> Arc<MockInteractor> {
    Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(&server.password); answers],
    ))
}

#[tokio::test]
async fn two_shells_share_one_authentication() {
    if !common::full_live("two_shells_share_one_authentication") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 two_shells_share_one_authentication：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("pool-reuse");
    let interactor = interactor(&server, 1);
    let pool = ConnectionPool::new(PoolConfig {
        idle_timeout: IDLE_TIMEOUT,
    });

    let guard = pool
        .acquire(
            1,
            "v1",
            vec![hop(&server, &known_hosts)],
            Arc::clone(&interactor) as Arc<_>,
        )
        .await
        .expect("池子应该能建起连接");
    assert!(guard.is_fresh(), "第一条是新建的");

    // 第一个 shell
    let mut first = guard
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开第一个 shell 失败");
    let first_out = run_marker(&mut first, "POOL_A", MARKER_TIMEOUT).await;
    assert!(
        first_out.contains("ZSSH_POOL_A_42"),
        "第一个 shell 的输出不对：{}",
        server.sanitize(&first_out)
    );

    // 第二个 shell：同一条连接，不该再有认证或指纹提问
    let mut second = guard
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开第二个 shell 失败");
    let second_out = run_marker(&mut second, "POOL_B", MARKER_TIMEOUT).await;
    assert!(
        second_out.contains("ZSSH_POOL_B_42"),
        "第二个 shell 的输出不对：{}",
        server.sanitize(&second_out)
    );

    // 输出不能串
    assert!(
        !first_out.contains("ZSSH_POOL_B_42"),
        "第一个 shell 收到了第二个 shell 的输出：{}",
        server.sanitize(&first_out)
    );
    assert!(
        !second_out.contains("ZSSH_POOL_A_42"),
        "第二个 shell 收到了第一个 shell 的输出：{}",
        server.sanitize(&second_out)
    );

    assert_eq!(interactor.asks(), 1, "指纹只该问一次");
    assert_eq!(
        interactor.asked_kinds(),
        vec![PromptKind::Password],
        "密码也只该问一次（第二个 shell 不重新认证）"
    );
    println!("两个 shell 共用一次认证，输出互不干扰");
}

#[tokio::test]
async fn idle_timeout_closes_the_pooled_connection() {
    if !common::full_live("idle_timeout_closes_the_pooled_connection") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 idle_timeout_closes_the_pooled_connection：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("pool-idle");
    // 两次认证的答案：第一次建连接，第三次（空闲超时之后）重建
    let interactor = interactor(&server, 2);
    let pool = ConnectionPool::new(PoolConfig {
        idle_timeout: IDLE_TIMEOUT,
    });
    let hops = vec![hop(&server, &known_hosts)];

    let guard = pool
        .acquire(7, "v1", hops.clone(), Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("建连接失败");
    {
        let mut shell = guard
            .connection()
            .open_shell(80, 24)
            .await
            .expect("开 shell 失败");
        let out = run_marker(&mut shell, "POOL_IDLE", MARKER_TIMEOUT).await;
        assert!(
            out.contains("ZSSH_POOL_IDLE_42"),
            "输出不对：{}",
            server.sanitize(&out)
        );
    }
    drop(guard); // 引用归零，开始空闲计时

    // 空闲期间再打开：必须复用（答案只有两条，这里问就会失败）
    tokio::time::sleep(Duration::from_millis(300)).await;
    let again = pool
        .acquire(7, "v1", hops.clone(), Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("空闲期间再打开应该复用池里的连接");
    assert!(!again.is_fresh(), "空闲期间打开的应该是复用的连接");
    assert_eq!(interactor.asks(), 1, "复用不该再问指纹");
    drop(again);

    // 空转满空闲时长：池子断开并摘掉
    let deadline = Instant::now() + IDLE_DEADLINE;
    loop {
        if pool.connection_of(7).is_none() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("空闲超时之后池里还留着连接");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(pool.pooled_hosts().is_empty(), "条目也要摘干净");

    // 再打开：新建一条，重新认证。
    // 指纹这次不会弹窗（第一次已经存进 known_hosts 了），认证据"密码又问了一次"。
    let third = pool
        .acquire(7, "v1", hops, Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("超时之后应该能重新建连接");
    assert!(third.is_fresh(), "超时之后的连接是新建的");
    assert_eq!(
        interactor.asked_kinds(),
        vec![PromptKind::Password, PromptKind::Password],
        "重建时要重新认证（密码再问一次）"
    );
    println!("空闲 1 秒后连接断开，期间打开能复用，之后重新建连");
}

#[tokio::test]
async fn a_changed_fingerprint_builds_a_new_connection() {
    if !common::full_live("a_changed_fingerprint_builds_a_new_connection") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 a_changed_fingerprint_builds_a_new_connection：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("pool-fingerprint");
    let interactor = interactor(&server, 2);
    let pool = ConnectionPool::new(PoolConfig {
        idle_timeout: IDLE_TIMEOUT,
    });
    let hops = vec![hop(&server, &known_hosts)];

    let old = pool
        .acquire(9, "v1", hops.clone(), Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("建连接失败");
    // 第二个 shell 还在用旧连接（模拟"改了配置但会话还开着"）
    let _shell = old
        .connection()
        .open_shell(80, 24)
        .await
        .expect("开 shell 失败");

    // 配置指纹变了：下一次打开要新建，而不是复用
    let new = pool
        .acquire(9, "v2", hops, Arc::clone(&interactor) as Arc<_>)
        .await
        .expect("指纹变了应该能建新的");
    assert!(new.is_fresh(), "指纹变了要新建连接");
    assert_ne!(
        Arc::as_ptr(new.connection()),
        Arc::as_ptr(old.connection()),
        "新旧不能是同一条连接"
    );
    // 指纹已经存过，不再弹窗；"重新认证过"看密码问题出现了两次
    assert_eq!(
        interactor.asked_kinds(),
        vec![PromptKind::Password, PromptKind::Password],
        "新连接要重新认证"
    );
    println!("配置指纹变化后新建了连接，旧连接留给了还开着的会话");
}
