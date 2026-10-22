//! 主机库到真实连接的集成测试：把测试服务器写成一条主机记录（临时目录里的库），
//! 再用主机 ID 取凭据连上去跑一条命令。
//!
//! 环境变量不全时跳过，并打印原因。

mod common;

use std::time::Duration;

use std::sync::Arc;
use zssh_core::model::{AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyMode};

use zssh_core::ssh::{ConnectParams, Connection, NonInteractive, ShellEvent};
use zssh_core::{Database, ZsshError};

use common::{TempKnownHosts, TestServer};

/// 远端算出来的结果，用来确认命令真的在远端跑过（回显里只有 `$((40+2))`）。
const MARKER: &str = "ZSSH_VAULT_42";
const COMMAND: &str = "echo ZSSH_VAULT_$((40+2))";
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 临时目录里的库，测完删掉。
struct TempDb {
    path: std::path::PathBuf,
    /// 目录守卫：放在最后，连同 WAL/SHM 一起删。
    _dir: common::TestDir,
}

impl TempDb {
    fn new() -> Self {
        let dir = common::TestDir::new("vault-test");
        let path = dir.join("vault.db");
        Self { path, _dir: dir }
    }
}

#[tokio::test]
async fn connects_to_a_host_stored_in_the_vault() {
    if !common::full_live("connects_to_a_host_stored_in_the_vault") {
        return;
    }
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 connects_to_a_host_stored_in_the_vault：{reason}");
            return;
        }
    };

    let temp = TempDb::new();
    let db = Database::open(&temp.path).expect("临时库应该能建起来");

    // 密码进保险库（DPAPI 加密后落 secret_blob）
    let identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "集成测试凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some(server.password.clone()),

            passphrase: None,
        })
        .expect("保存凭据");
    assert!(identity.has_secret);

    // 主机记录指向测试服务器
    let host = db
        .host_save(&HostDraft {
            id: None,
            group_id: None,
            name: "集成测试主机".to_owned(),
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            auth_method: AuthMethod::Password,
            identity_id: Some(identity.id),
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: vec!["集成测试".to_owned()],
            sort: 0,

            legacy_algorithms: false,
        })
        .expect("保存主机");

    // 列表接口不能带出明文密码
    let json = serde_json::to_string(&db.host_list().expect("列主机")).unwrap();
    assert!(!json.contains(&server.password), "主机列表不该带出密码");
    let json = serde_json::to_string(&db.identity_list().expect("列凭据")).unwrap();
    assert!(!json.contains(&server.password), "凭据列表不该带出密码");

    // 通过主机 ID 取登录信息并连接
    let login = db.host_login(host.id).expect("从库里取登录信息");
    assert_eq!(login.username, server.username);
    assert_eq!(
        login.auth.password.as_deref(),
        Some(server.password.as_str()),
        "解出来的密码要和存进去的一样"
    );

    let known_hosts = TempKnownHosts::new("vault-host");
    let connection = server
        .connect_params(
            ConnectParams {
                host: login.host,
                port: login.port,
                username: login.username,
                auth: login.auth,
                known_hosts: known_hosts.known_hosts(),
                legacy_algorithms: login.legacy_algorithms,
            },
            Arc::new(NonInteractive),
        )
        .await
        .expect("用库里的凭据应该能连上");

    let mut shell = connection
        .open_shell(80, 24)
        .await
        .expect("打开 shell 失败");
    shell
        .write(format!("{COMMAND}\r").as_bytes())
        .await
        .expect("写入命令失败");
    shell.write(b"exit\r").await.expect("写入 exit 失败");

    let deadline = tokio::time::Instant::now() + COLLECT_TIMEOUT;
    let mut output = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(bytes))) => output.extend_from_slice(&bytes),
            Ok(Some(ShellEvent::Exit(_))) | Ok(Some(ShellEvent::Closed)) | Ok(None) => break,
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains(MARKER),
        "输出里没有 {MARKER}：{}",
        server.sanitize(&text)
    );

    // 主机改坏之后（引用没密码的凭据）要明确报错，而不是拿空密码去连
    let empty_identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "没密码的凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: None,

            passphrase: None,
        })
        .expect("保存凭据");
    let mut draft = HostDraft {
        id: Some(host.id),
        group_id: None,
        name: host.name.clone(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: AuthMethod::Password,
        identity_id: Some(empty_identity.id),
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: ProxyMode::Default,
        tags: Vec::new(),
        sort: 0,

        legacy_algorithms: false,
    };
    db.host_save(&draft).expect("改主机");
    let login = db.host_login(host.id).expect("没密码也能取到登录信息");
    assert!(
        login.auth.password.is_none(),
        "凭据里没有密码时应该留给连接时问用户"
    );
    let refused = Connection::connect(
        ConnectParams {
            host: login.host,
            port: login.port,
            username: login.username,
            auth: login.auth,
            known_hosts: known_hosts.known_hosts(),
            legacy_algorithms: false,
        },
        Arc::new(NonInteractive),
    )
    .await;
    match refused {
        Err(ZsshError::Cancelled) => {}
        Err(other) => panic!(
            "应该是已取消，实际：{}",
            server.sanitize(&format!("{other:?}"))
        ),
        Ok(_) => panic!("没人回答密码提示时不应该连上"),
    }

    // 清理：删掉主机之后凭据才能删（引用检查）
    draft.identity_id = None;
    db.host_save(&draft).expect("改回没有凭据");
    db.host_delete(host.id).expect("删主机");
}
