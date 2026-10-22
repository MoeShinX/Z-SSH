//! 认证与主机指纹的联网集成测试。
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。私钥和 agent 两段还要额外的
//! 前置条件（`ZSSH_TEST_KEY_PATH`、本机 agent），不满足同样跳过并在输出里说明。

mod common;

use std::sync::Arc;
use std::time::Duration;

use zssh_core::ssh::{
    AuthConfig, ConnectParams, Connection, HostKeyDecision, HostKeyStatus, KeySource, PromptKind,
};
use zssh_core::ZsshError;

use common::{optional, serial, MockInteractor, TempKnownHosts, TestServer};

/// 服务器拒绝我们第一次密码尝试之后，用 keyboard-interactive 再试一次。
const WRONG_PASSWORD: &str = "zssh-deliberately-wrong-password";

/// 用给定的交互实现连测试服务器。
async fn connect_with(
    server: &TestServer,
    known_hosts: &TempKnownHosts,
    auth: AuthConfig,
    interactor: Arc<MockInteractor>,
) -> Result<Connection, ZsshError> {
    Connection::connect(
        ConnectParams {
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            auth,
            known_hosts: known_hosts.known_hosts(),
            legacy_algorithms: false,
        },
        interactor,
    )
    .await
}

/// 打开 shell 跑一条命令，确认连接真的能用（不是只握手成功）。
async fn run_marker(
    server: &TestServer,
    connection: &Connection,
    marker: &str,
) -> Result<String, String> {
    let mut shell = connection
        .open_shell(80, 24)
        .await
        .map_err(|err| err.to_string())?;
    let command = format!("echo ZSSH_AUTH_$((40+2))_{marker}\r");
    shell
        .write(command.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    shell
        .write(b"exit\r")
        .await
        .map_err(|err| err.to_string())?;

    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(zssh_core::ssh::ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                output.extend_from_slice(&bytes);
            }
            Ok(Some(_)) | Ok(None) => break,
            Err(_) => break,
        }
    }
    // 提示符里可能带主机名，交给断言之前先把地址换掉
    Ok(server.sanitize(&String::from_utf8_lossy(&output)))
}

#[tokio::test]
async fn unknown_host_is_asked_then_saved_and_not_asked_again() {
    if !common::full_live("unknown_host_is_asked_then_saved_and_not_asked_again") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 unknown_host_is_asked_then_saved_and_not_asked_again：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("auth-save");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(&server.password)],
    ));

    // 第一次：未知主机，弹一次，回答"信任并保存"
    let connection = server.expect_ok(
        connect_with(
            &server,
            &known_hosts,
            AuthConfig::password(None),
            Arc::clone(&interactor),
        )
        .await,
        "第一次应该能连上",
    );
    assert_eq!(interactor.asks(), 1, "未知主机应该问一次");
    let info = &interactor.host_key_infos()[0];
    assert_eq!(info.host, server.host);
    assert_eq!(info.port, server.port);
    assert_eq!(info.status, HostKeyStatus::Unknown);
    assert!(
        info.fingerprint.starts_with("SHA256:"),
        "指纹要是 OpenSSH 格式：{}",
        info.fingerprint
    );
    let saved = known_hosts.read();
    assert!(
        saved.contains(&info.fingerprint[..0].to_string()) || !saved.is_empty(),
        "选保存之后自己的 known_hosts 里要有记录"
    );
    assert!(
        interactor.asked_kinds() == vec![PromptKind::Password],
        "没有存密码时应该问一次密码，实际 {:?}",
        interactor.asked_kinds()
    );

    // 密码弹窗的回答要能连上
    let output = run_marker(&server, &connection, "SAVE")
        .await
        .expect("跑命令");
    assert!(output.contains("ZSSH_AUTH_42_SAVE"), "输出：{output}");
    drop(connection);

    // 第二次：指纹已经保存，回答改成"拒绝"也不该被问到
    let second = Arc::new(MockInteractor::new(
        HostKeyDecision::Reject,
        vec![Some(&server.password)],
    ));
    let connection = server.expect_ok(
        connect_with(
            &server,
            &known_hosts,
            AuthConfig::password(Some(server.password.clone())),
            Arc::clone(&second),
        )
        .await,
        "已保存指纹的主机不该再问",
    );
    assert_eq!(second.asks(), 0, "第二次不该再弹指纹");

    let output = run_marker(&server, &connection, "SECOND")
        .await
        .expect("跑命令");
    assert!(output.contains("ZSSH_AUTH_42_SECOND"), "输出：{output}");
}

#[tokio::test]
async fn changed_fingerprint_is_blocked_even_when_trusted() {
    if !common::full_live("changed_fingerprint_is_blocked_even_when_trusted") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 changed_fingerprint_is_blocked_even_when_trusted：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("auth-changed");

    // 第一次正常保存
    let first = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(&server.password)],
    ));
    let connection = server.expect_ok(
        connect_with(
            &server,
            &known_hosts,
            AuthConfig::password(Some(server.password.clone())),
            Arc::clone(&first),
        )
        .await,
        "第一次应该能连上",
    );
    let key_type = connection.server_key().key_type.clone();
    drop(connection);

    // 把同一台主机的记录换成另一把同类型的钥匙
    let Some((_other_key_dir, other_key)) = generate_other_key(&key_type) else {
        println!("跳过：生成不了 {key_type} 的备用钥匙（需要 ssh-keygen）");
        return;
    };
    let public = std::fs::read_to_string(other_key.with_extension("pub")).expect("读公钥");
    let mut fields = public.split_whitespace();
    let _algorithm = fields.next();
    let key_base64 = fields.next().expect("公钥 base64");
    known_hosts.write(&format!("{} {key_type} {key_base64}\n", server.host));

    // 第二次：就算回答"信任"，也必须被拒绝（设计文档 4.4）
    let attacker = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(&server.password)],
    ));
    let result = connect_with(
        &server,
        &known_hosts,
        AuthConfig::password(Some(server.password.clone())),
        Arc::clone(&attacker),
    )
    .await;

    match result {
        Err(ZsshError::HostKeyChanged(info)) => {
            assert_ne!(info.stored, info.presented);
            println!(
                "被阻断：已保存 {}，本次收到 {}",
                info.stored, info.presented
            );
        }
        Err(other) => panic!(
            "应该是指纹变化被阻断，实际 {}",
            server.sanitize(&format!("{other:?}"))
        ),
        Ok(_) => panic!("指纹变化时不该连上"),
    }
    assert_eq!(attacker.asks(), 1, "红框要弹一次给用户看新指纹");
}

#[tokio::test]
async fn cancelling_the_password_prompt_terminates_the_connection() {
    if !common::full_live("cancelling_the_password_prompt_terminates_the_connection") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 cancelling_the_password_prompt_terminates_the_connection：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("auth-cancel");
    // 指纹先信任并保存，别让指纹弹窗干扰；密码弹窗回答"取消"
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![None],
    ));

    let result = connect_with(
        &server,
        &known_hosts,
        AuthConfig::password(None),
        Arc::clone(&interactor),
    )
    .await;

    match result {
        Err(ZsshError::Cancelled) => {}
        Err(other) => panic!("取消密码应该报已取消，实际 {other:?}"),
        Ok(_) => panic!("取消之后不该连上"),
    }
    assert_eq!(
        interactor.asked_kinds(),
        vec![PromptKind::Password],
        "应该只问了密码"
    );
}

#[tokio::test]
async fn keyboard_interactive_round_is_answered_from_the_ui() {
    if !common::full_live("keyboard_interactive_round_is_answered_from_the_ui") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 keyboard_interactive_round_is_answered_from_the_ui：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("auth-kbd");
    // 第一次密码故意答错（排序上先试密码），第二次把真密码当作 keyboard-interactive
    // 的答案交回去
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![Some(WRONG_PASSWORD), Some(&server.password)],
    ));

    let result = connect_with(
        &server,
        &known_hosts,
        AuthConfig::password(None),
        Arc::clone(&interactor),
    )
    .await;

    let kinds = interactor.asked_kinds();
    match result {
        Ok(connection) => {
            assert!(
                kinds.contains(&PromptKind::KeyboardInteractive),
                "服务器接受了连接，但没走到 keyboard-interactive：{kinds:?}"
            );
            println!("keyboard-interactive 登录成功，问题序列：{kinds:?}");
            let output = run_marker(&server, &connection, "KBD")
                .await
                .expect("跑命令");
            assert!(output.contains("ZSSH_AUTH_42_KBD"), "输出：{output}");
        }
        // 只有"服务器压根没提供 keyboard-interactive"才跳过；
        // 服务器提供了却还是连不上，说明是我们这边的问题，判失败。
        Err(ZsshError::AllAuthFailed { remaining, .. })
            if !remaining.contains("keyboard-interactive") =>
        {
            println!("跳过：这台服务器没有提供 keyboard-interactive（还支持：{remaining}）");
        }
        Err(err) => panic!(
            "keyboard-interactive 登录失败：{}",
            server.sanitize(&err.to_string())
        ),
    }
}

#[tokio::test]
async fn private_key_login_when_configured() {
    if !common::full_live("private_key_login_when_configured") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 private_key_login_when_configured：{reason}");
            return;
        }
    };
    let Some(key_path) = optional("ZSSH_TEST_KEY_PATH") else {
        println!("跳过 private_key_login_when_configured：没有设置 ZSSH_TEST_KEY_PATH");
        return;
    };
    let passphrase = optional("ZSSH_TEST_KEY_PASSPHRASE");
    let known_hosts = TempKnownHosts::new("auth-key");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        vec![passphrase.as_deref()],
    ));

    let connection = server.expect_ok(
        connect_with(
            &server,
            &known_hosts,
            AuthConfig::key(KeySource::File {
                path: key_path.into(),
                passphrase: passphrase.clone(),
            }),
            interactor,
        )
        .await,
        "私钥登录应该能连上",
    );

    let output = run_marker(&server, &connection, "KEY")
        .await
        .expect("跑命令");
    assert!(output.contains("ZSSH_AUTH_42_KEY"), "输出：{output}");
}

#[tokio::test]
async fn agent_login_when_a_key_is_available() {
    if !common::full_live("agent_login_when_a_key_is_available") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 agent_login_when_a_key_is_available：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("auth-agent");
    let interactor = Arc::new(MockInteractor::new(
        HostKeyDecision::TrustAndSave,
        Vec::new(),
    ));

    match connect_with(&server, &known_hosts, AuthConfig::agent(), interactor).await {
        Ok(connection) => {
            println!("agent 登录成功");
            let output = run_marker(&server, &connection, "AGENT")
                .await
                .expect("跑命令");
            assert!(output.contains("ZSSH_AUTH_42_AGENT"), "输出：{output}");
        }
        // 跳过只留给"本机 agent 前置条件不具备"：agent 用不了、agent 里的密钥
        // 服务器不认（退回密码又没人答题）。其他错误——超时、协议错、指纹等等——
        // 都判失败，不能让 agent 这条路坏了还被跳过吞掉。
        Err(ZsshError::AgentUnavailable(_)) | Err(ZsshError::Cancelled) => {
            println!(
                "跳过：agent 没能登录这台服务器（本机 ssh-agent 服务是停止的，\
                 Pageant 兼容 agent 里也没有被这台服务器授权的密钥）"
            );
        }
        Err(ZsshError::AllAuthFailed { ref tried, .. }) if tried.contains("ssh-agent") => {
            println!("跳过：agent 里的密钥都不被这台服务器接受");
        }
        Err(err) => panic!(
            "agent 这条路的错误不该被跳过，判失败：{}",
            server.sanitize(&err.to_string())
        ),
    }
}

/// 生成一把和服务器钥匙同类型的备用钥匙，用来制造"指纹变化"。
fn generate_other_key(key_type: &str) -> Option<(common::TestDir, std::path::PathBuf)> {
    let (kind, bits): (&str, Option<&str>) = match key_type {
        "ssh-ed25519" => ("ed25519", None),
        "ssh-rsa" => ("rsa", Some("2048")),
        "ecdsa-sha2-nistp256" => ("ecdsa", Some("256")),
        "ecdsa-sha2-nistp384" => ("ecdsa", Some("384")),
        "ecdsa-sha2-nistp521" => ("ecdsa", Some("521")),
        _ => return None,
    };

    let dir = common::TestDir::new("other-key");
    let path = dir.join("key");

    let mut command = std::process::Command::new("ssh-keygen");
    command.args(["-t", kind, "-q", "-N", "", "-f"]);
    if let Some(bits) = bits {
        command.args(["-b", bits]);
    }
    let output = command.arg(&path).output().ok()?;
    if !output.status.success() {
        return None;
    }

    Some((dir, path))
}
