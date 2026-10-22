//! SFTP 会话的线测试（M3-1 第 1 节）：开在已有连接上、和终端共存的通道预算、
//! 明确的错误类型。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）。
//!
//! 规矩（M3-1 第 4 节）：**所有远端操作都在一个专用的临时目录里**
//! （`/tmp/zssh-sftp-live-<随机串>`），测试结束删掉，**不碰服务器上的其它任何文件**。

mod common;

use std::sync::Arc;
use std::time::Duration;

use zssh_core::sftp::session::SftpChannel;
use zssh_core::sftp::SftpError;
use zssh_core::ssh::Connection;

use common::{serial, TempKnownHosts, TestServer};

/// 临时目录：`/tmp/zssh-sftp-live-<随机串>`（守卫：失败 / panic 也会删，M3-2 第 0 节）。
pub async fn temp_dir(
    server: &TestServer,
    connection: &Arc<Connection>,
    channel: &SftpChannel,
    tag: &str,
) -> common::RemoteTempDir {
    common::RemoteTempDir::create(server, connection, channel, tag).await
}

#[tokio::test]
async fn sftp_channel_opens_on_the_existing_connection() {
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 sftp_channel...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-live");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    // 开 SFTP 之前，这条连接上一个通道都没开。
    assert_eq!(connection.channels_opened(), 0);

    let channel = SftpChannel::open(&connection).await.expect("开 SFTP 会话");
    assert_eq!(
        connection.channels_opened(),
        1,
        "SFTP 应该开在**这条**连接上（通道数 +1），不是偷偷又连一条"
    );
    assert_eq!(
        channel.connection_id(),
        Arc::as_ptr(&connection) as usize,
        "会话记的连接就是这条连接（没有重新认证、也没有新连接）"
    );

    // 协商出来的能力（报告里贴的就是这一段）。
    println!("SFTP 能力：{}", channel.caps().describe());
    println!(
        "扩展：{}",
        if channel.caps().extensions.is_empty() {
            "（服务器没报扩展）".to_owned()
        } else {
            channel.caps().extensions.join(", ")
        }
    );
    assert_eq!(channel.caps().version, 3, "SFTP v3 是通用基线");

    // realpath：chroot 的服务器给的是它眼里的绝对路径，我们如实带回来。
    let real = channel.realpath(".").await.expect("realpath");
    println!("realpath(.) = {real}");
    assert!(!real.is_empty());

    // 冒烟组的"列一次目录"：家目录总能列出来，条目的路径都以它为前缀。
    let entries = channel.read_dir(&real).await.expect("列家目录");
    println!("家目录条目数：{}", entries.len());
    assert!(entries.iter().all(|entry| entry.path.starts_with(&real)));

    channel.close();
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn sftp_still_works_with_terminals_open_on_the_same_connection() {
    if !common::full_live("sftp_still_works_with_terminals_open_on_the_same_connection") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 sftp_with_terminals...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-live-terminals");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    // 先开 3 个终端（"同一条连接上已经开着若干终端"），再开 SFTP。
    let mut shells = Vec::new();
    for index in 0..3 {
        let mut shell = connection
            .open_shell(80, 24)
            .await
            .unwrap_or_else(|err| panic!("开第 {index} 个终端失败：{err}"));
        let output =
            common::run_marker(&mut shell, &format!("SFTP{index}"), Duration::from_secs(20)).await;
        assert!(
            output.contains(&format!("ZSSH_SFTP{index}_42")),
            "第 {index} 个终端没跑起来：{output}"
        );
        shells.push(shell);
    }
    println!("同一条连接上已开着 {} 个终端", shells.len());

    // 开 SFTP：通道够用（OpenSSH 默认 MaxSessions 10，这里用掉 3+1）。
    let channel = common::open_sftp(&connection)
        .await
        .expect("开着终端时开 SFTP");
    let dir = temp_dir(&server, &connection, &channel, "walk").await;
    // 目录能列（空目录），且在临时目录里操作。
    let entries = channel.read_dir_raw(dir.path()).await.expect("列目录");
    let names: Vec<&str> = entries.iter().map(|e| e.filename.as_str()).collect();
    // 原始列表里服务器一般会带上 `.` / `..`（过滤是上层的事）。
    assert!(
        names.iter().all(|name| *name == "." || *name == ".."),
        "刚建的目录应当是空的：{names:?}"
    );
    println!("SFTP 在临时目录 {} 里可用", dir.path());

    // 终端还得能用（SFTP 没把它们的通道挤掉）。
    let output = common::run_marker(&mut shells[0], "AFTER", Duration::from_secs(20)).await;
    assert!(
        output.contains("ZSSH_AFTER_42"),
        "SFTP 开着时终端也要正常：{output}"
    );

    dir.cleanup(&connection, &channel).await;
    channel.close();
    for shell in &mut shells {
        let _ = shell.close().await;
    }
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn missing_path_reports_not_found() {
    if !common::full_live("missing_path_reports_not_found") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 sftp_not_found...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-live-errors");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");

    let err = channel
        .stat("/tmp/zssh-sftp-live-肯定不存在-9f8e7d6c.txt")
        .await
        .expect_err("不存在的路径要报错");
    println!("不存在的路径：{err}");
    assert!(matches!(err, SftpError::NotFound { .. }), "{err}");
    assert!(err.to_string().contains("不存在"), "{err}");

    // 权限错误也要能分辨（`/etc/shadow` 一般只有 root 能读；读得到就跳过这条断言）。
    if let Err(err) = channel.open_read("/etc/shadow").await {
        println!("/etc/shadow：{err}");
        assert!(
            matches!(
                err,
                SftpError::PermissionDenied { .. } | SftpError::NotFound { .. }
            ),
            "{err}"
        );
    }

    channel.close();
    let _ = connection.disconnect().await;
}

// ---------- 通道数上限的实测（M3-2 第 0 节，M3-1 遗留 1） ----------

/// 专用连接的重连（测试版）：再连一次测试服务器。生产上是"复用主机库里保存的
/// 凭据和已确认的指纹"（M3-2 第 1 节接进 `src-tauri` 时实现）。
struct ReconnectFresh {
    params: zssh_core::ssh::ConnectParams,
}

impl zssh_core::sftp::transfer::backend::SftpReconnect for ReconnectFresh {
    fn reconnect<'a>(
        &'a self,
    ) -> zssh_core::ssh::BoxFuture<'a, Result<Arc<Connection>, SftpError>> {
        Box::pin(async move {
            let connection = Connection::connect(
                self.params.clone(),
                Arc::new(zssh_core::ssh::NonInteractive),
            )
            .await
            .map_err(|err| {
                // 连接错误里带服务器地址，进测试输出前先换掉。
                SftpError::Disconnected(err.to_string().replace(&self.params.host, "测试服务器"))
            })?;
            Ok(Arc::new(connection))
        })
    }
}

/// 把远端文件整个读回来（内容核对用；这条连接满了不能用 `exec sha256sum`）。
async fn read_whole_remote_file(channel: &SftpChannel, path: &str) -> Vec<u8> {
    let attrs = channel.lstat(path).await.expect("读远端属性");
    let len = attrs.size.unwrap_or(0) as usize;
    let handle = channel.open_read(path).await.expect("开读句柄");
    let mut out = Vec::with_capacity(len);
    let mut offset = 0u64;
    loop {
        let chunk = channel
            .read_at(&handle, offset, 128 * 1024)
            .await
            .expect("读一块");
        if chunk.is_empty() {
            break;
        }
        offset += chunk.len() as u64;
        out.extend_from_slice(&chunk);
    }
    channel.close_handle(&handle).await.expect("关句柄");
    out
}

/// 等一个任务到终态。
async fn wait_transfer_finished(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<zssh_core::sftp::transfer::TransferEvent>,
    id: u64,
    limit: Duration,
) -> zssh_core::sftp::transfer::ProgressReport {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "任务 {id} 没有在时限内结束");
        match tokio::time::timeout(remaining.min(Duration::from_secs(5)), events.recv()).await {
            Ok(Some(zssh_core::sftp::transfer::TransferEvent::Finished(report)))
                if report.id == id =>
            {
                return report
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("事件通道关了"),
            Err(_) => {}
        }
    }
}

/// 同一条连接上开终端直到服务器拒绝（**上限不写死，以实际被拒绝为准**），
/// 再发起 SFTP 传输：传输通道开不出来，必须改走专用连接并成功；已经开着的
/// 终端一个都不受影响。不改服务器配置，用的就是服务器默认的 `MaxSessions`。
#[tokio::test]
async fn channel_limit_moves_transfers_to_a_dedicated_connection() {
    if !common::full_live("channel_limit_moves_transfers_to_a_dedicated_connection") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 channel_limit...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-channel-limit");
    let params = server.params(&known_hosts);
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));

    // 浏览通道先开好（它自己占一个通道；满了之后想开也开不出来）。
    let browse = common::open_sftp(&connection).await.expect("开浏览通道");
    let dir = temp_dir(&server, &connection, &browse, "channel-limit").await;
    let local = common::LocalTempDir::create("channel-limit");
    let source = local.join("small.bin");
    std::fs::write(&source, vec![0xA5u8; 1024 * 1024]).expect("写本地文件");

    // 开终端，直到服务器拒绝。上限不写死：循环到"实际被拒绝"为止。
    let mut shells = Vec::new();
    let mut refusal: Option<String> = None;
    for _ in 0..64 {
        match connection.open_shell(80, 24).await {
            Ok(shell) => shells.push(shell),
            Err(zssh_core::ZsshError::ChannelLimit(reason)) => {
                refusal = Some(reason);
                break;
            }
            Err(other) => panic!("开终端时出了别的问题（不是通道上限）：{other}"),
        }
    }
    let Some(refusal) = refusal else {
        println!(
            "跳过 channel_limit...：开了 {} 个终端（加浏览通道 {} 个）都没有被拒绝，\
             这台服务器没有 MaxSessions 上限可测",
            shells.len(),
            shells.len() + 1
        );
        for shell in &mut shells {
            let _ = shell.close().await;
        }
        dir.cleanup(&connection, &browse).await;
        browse.close();
        let _ = connection.disconnect().await;
        return;
    };
    println!(
        "浏览通道 + {} 个终端（共 {} 个通道）之后，服务器拒绝再开：{refusal}",
        shells.len(),
        shells.len() + 1
    );
    assert!(
        !shells.is_empty(),
        "第一个终端就被拒绝：那连浏览通道都开不出来，这台服务器没法测"
    );
    // 满员之后这条连接上不该再有任何新通道。
    let channels_before = connection.channels_opened();

    // 满员状态下终端要还能用。
    let output = common::run_marker(&mut shells[0], "FULL", Duration::from_secs(20)).await;
    assert!(
        output.contains("ZSSH_FULL_42"),
        "满员时已开着的终端也要正常：{output}"
    );

    // 发起传输：通道被拒 → 改走专用连接（重连就是"再连一次"，用同一套凭据）。
    let provider = Arc::new(
        zssh_core::sftp::transfer::backend::SftpChannelProvider::new(
            Arc::clone(&connection),
            Some(Arc::new(ReconnectFresh { params })),
        ),
    );
    let endpoints = Arc::new(zssh_core::sftp::transfer::backend::SftpEndpoints::new(
        provider,
    ));
    let (engine, mut events) = zssh_core::sftp::transfer::TransferEngine::new(
        endpoints,
        Arc::new(zssh_core::sftp::transfer::OverwriteAll),
        zssh_core::sftp::transfer::TransferOptions::default(),
    );

    let id = engine.submit(zssh_core::sftp::transfer::TransferJob {
        direction: zssh_core::sftp::transfer::Direction::Upload,
        source: source.display().to_string(),
        target: format!("{}/small.bin", dir.path()),
        recursive: false,
        peer: None,
    });
    let report = wait_transfer_finished(&mut events, id, Duration::from_secs(120)).await;
    println!(
        "满员连接上的传输：{:?}（{} 字节）",
        report.state, report.done_bytes
    );
    assert_eq!(
        report.state,
        zssh_core::sftp::transfer::TransferState::Done,
        "{report:?}"
    );
    // 传输没有从这条满员的连接上抢通道（被拒的 open 不计数，所以数值不变）。
    assert_eq!(
        connection.channels_opened(),
        channels_before,
        "传输不该占用满员连接上的通道"
    );

    // 内容核对（连接满了，用浏览通道读回来比，不走 exec）。
    let back = read_whole_remote_file(&browse, &format!("{}/small.bin", dir.path())).await;
    let original = std::fs::read(&source).expect("读本地文件");
    assert_eq!(back, original, "走专用连接传上去的内容要一致");

    // 再传第二个：专用连接已经建好，直接复用（这条也成功）。
    let second = local.join("small2.bin");
    std::fs::write(&second, vec![0x3Cu8; 256 * 1024]).expect("写第二个文件");
    let id = engine.submit(zssh_core::sftp::transfer::TransferJob {
        direction: zssh_core::sftp::transfer::Direction::Upload,
        source: second.display().to_string(),
        target: format!("{}/small2.bin", dir.path()),
        recursive: false,
        peer: None,
    });
    let report = wait_transfer_finished(&mut events, id, Duration::from_secs(120)).await;
    assert_eq!(
        report.state,
        zssh_core::sftp::transfer::TransferState::Done,
        "第二个传输（复用专用连接）：{report:?}"
    );

    // 传输全程结束后，终端依然个个都能用。
    for (index, shell) in shells.iter_mut().enumerate() {
        let output =
            common::run_marker(shell, &format!("AFTER{index}"), Duration::from_secs(20)).await;
        assert!(
            output.contains(&format!("ZSSH_AFTER{index}_42")),
            "传输之后第 {index} 个终端不正常：{output}"
        );
    }
    println!(
        "传输完成后 {} 个终端全部正常（原连接通道数 {}）",
        shells.len(),
        connection.channels_opened()
    );

    for shell in &mut shells {
        let _ = shell.close().await;
    }
    dir.cleanup(&connection, &browse).await;
    browse.close();
    let _ = connection.disconnect().await;
}
