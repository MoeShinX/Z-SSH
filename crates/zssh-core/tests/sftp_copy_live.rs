//! M3-5 第 3、4 节的联网测试：复制粘贴的三种办法（`copy-data`、`cp -a`、经本机中转）、
//! 「计算大小」，都对着真实的测试服务器跑。
//!
//! 同时把「这台测试服务器有没有声明 `copy-data`」打印出来（验收报告要记）。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）；远端只用专用临时目录。
//! 用例名里带「完整」的才在 `ZSSH_LIVE_FULL=1`（`scripts/live-tests.ps1`）下跑。

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use zssh_core::sftp::server_copy::{
    copy_with_copy_data, copy_with_cp, supports_copy_data, CopyConflictDecision, CopyConflictInfo,
    CopyConflictResolver, CopyOptions, CopyProgress, CpFailure,
};
use zssh_core::sftp::size::remote_size;
use zssh_core::sftp::transfer::backend::{SftpChannelProvider, SftpEndpoints};
use zssh_core::sftp::transfer::{
    Direction, OverwriteAll, TransferEngine, TransferEvent, TransferJob, TransferOptions,
    TransferState,
};
use zssh_core::sftp::{SftpChannel, SftpError};
use zssh_core::ssh::Connection;

use common::{serial, RemoteTempDir, TempKnownHosts, TestServer};

/// 名字里带空格、单引号、`$()`、反引号、开头的横杠：任何一个没转义好都会出事。
const NASTY: &[&str] = &[
    "plain.txt",
    "with space.txt",
    "it's.txt",
    "$(touch pwned-by-name).txt",
    "`touch pwned-by-backtick`.txt",
    "-dash.txt",
];

struct Fixture {
    server: TestServer,
    connection: Arc<Connection>,
    channel: Arc<SftpChannel>,
    remote: RemoteTempDir,
    _known_hosts: TempKnownHosts,
}

async fn fixture(tag: &str) -> Option<Fixture> {
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 {tag}：{reason}");
            return None;
        }
    };
    let known_hosts = TempKnownHosts::new(tag);
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = Arc::new(common::open_sftp(&connection).await.expect("开 SFTP 通道"));
    let remote = RemoteTempDir::create(&server, &connection, &channel, tag).await;
    Some(Fixture {
        server,
        connection,
        channel,
        remote,
        _known_hosts: known_hosts,
    })
}

impl Fixture {
    async fn finish(self) {
        self.remote.cleanup(&self.connection, &self.channel).await;
        self.channel.close();
        let _ = self.connection.disconnect().await;
        let _ = &self.server;
    }

    fn path(&self, relative: &str) -> String {
        format!("{}/{relative}", self.remote.path())
    }
}

struct ScriptedMerge {
    decisions: std::sync::Mutex<std::collections::VecDeque<CopyConflictDecision>>,
}

impl CopyConflictResolver for ScriptedMerge {
    fn decide<'a>(
        &'a self,
        _info: CopyConflictInfo,
    ) -> zssh_core::ssh::BoxFuture<'a, Result<CopyConflictDecision, SftpError>> {
        Box::pin(async move {
            Ok(self
                .decisions
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(CopyConflictDecision::Skip))
        })
    }
}

struct OverwriteMerge;

impl CopyConflictResolver for OverwriteMerge {
    fn decide<'a>(
        &'a self,
        _info: CopyConflictInfo,
    ) -> zssh_core::ssh::BoxFuture<'a, Result<CopyConflictDecision, SftpError>> {
        Box::pin(async { Ok(CopyConflictDecision::Overwrite) })
    }
}

async fn put(channel: &SftpChannel, path: &str, bytes: &[u8]) {
    zssh_core::sftp::fs::write_bytes(channel, path, bytes, 1 << 20)
        .await
        .unwrap_or_else(|err| panic!("写 {path}：{err}"));
}

/// 建一棵测试树：几层目录、各种难缠的名字、一个空目录、一个较大的文件。
async fn build_tree(channel: &SftpChannel, root: &str) {
    for dir in [
        root.to_owned(),
        format!("{root}/sub dir"),
        format!("{root}/sub dir/deeper"),
        format!("{root}/empty"),
    ] {
        channel.create_dir(&dir).await.expect("建目录");
    }
    for (index, name) in NASTY.iter().enumerate() {
        put(
            channel,
            &format!("{root}/{name}"),
            format!("content-{index}").as_bytes(),
        )
        .await;
    }
    put(
        channel,
        &format!("{root}/sub dir/b.bin"),
        &vec![7u8; 300_000],
    )
    .await;
    put(channel, &format!("{root}/sub dir/deeper/c.txt"), b"deep").await;
}

/// 整棵树的快照：相对路径 → 内容（目录是 None）。
async fn snapshot(channel: &SftpChannel, root: &str) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![(root.to_owned(), String::new())];
    while let Some((dir, relative)) = pending.pop() {
        for entry in channel.read_dir(&dir).await.expect("列目录") {
            let rel = if relative.is_empty() {
                entry.name.clone()
            } else {
                format!("{relative}/{}", entry.name)
            };
            if entry.kind.is_dir() {
                out.insert(rel.clone(), None);
                pending.push((entry.path.clone(), rel));
            } else {
                let bytes = zssh_core::sftp::fs::read_bytes(channel, &entry.path, 4 << 20)
                    .await
                    .expect("读文件");
                out.insert(rel, Some(bytes));
            }
        }
    }
    out
}

async fn exists(channel: &SftpChannel, path: &str) -> bool {
    channel.lstat(path).await.is_ok()
}

fn copy_options<'a>(owner: &'a str, server: &'a str, cancel: &'a AtomicBool) -> CopyOptions<'a> {
    CopyOptions::simple(owner, server, cancel)
}

fn options() -> TransferOptions {
    TransferOptions {
        concurrency: 2,
        pipeline: true,
        in_flight: 4,
        chunk_len: None,
        preserve_mtime: false,
        preserve_permissions: false,
        max_retries: 1,
        progress_interval: Duration::from_millis(50),
        tail_window: 4096,
    }
}

async fn finished(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
    id: u64,
) -> zssh_core::sftp::transfer::ProgressReport {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等不到任务 {id} 结束");
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(TransferEvent::Finished(report))) if report.id == id => return report,
            Ok(Some(_)) => {}
            _ => panic!("事件通道关了或超时"),
        }
    }
}

/// 把测试服务器有没有 `copy-data` 打印出来（验收报告要记），并确认判断函数和能力表一致。
#[tokio::test]
async fn copy_data_declaration_on_the_test_server_is_recorded() {
    let _serial = serial().await;
    let Some(fx) = fixture("copy-caps").await else {
        return;
    };
    let declared = supports_copy_data(fx.channel.caps());
    println!("测试服务器声明 copy-data：{declared}");
    println!("测试服务器 SFTP 能力：{}", fx.channel.caps().describe());
    assert_eq!(declared, fx.channel.caps().has("copy-data"));
    fx.finish().await;
}

/// 服务器端复制：`copy-data`（服务器声明了才测）和 `cp -a` 都复制出一模一样的树，源不动，
/// 名字里的特殊字符不会被当成命令执行。
#[tokio::test]
async fn server_side_copy_copies_a_whole_tree_with_nasty_names_full() {
    if !common::full_live("server_side_copy_copies_a_whole_tree_with_nasty_names_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-tree").await else {
        return;
    };
    let source = fx.path("src");
    build_tree(&fx.channel, &source).await;
    let expected = snapshot(&fx.channel, &source).await;

    if supports_copy_data(fx.channel.caps()) {
        let target = fx.path("via-copy-data");
        let cancel = AtomicBool::new(false);
        let opts = copy_options("live-copy-data", fx.channel.server_key(), &cancel);
        let stats = copy_with_copy_data(fx.channel.as_ref(), &source, &target, &opts)
            .await
            .expect("copy-data 复制");
        assert_eq!(
            snapshot(&fx.channel, &target).await,
            expected,
            "copy-data 的副本应该和源一致"
        );
        println!(
            "copy-data 复制了 {} 个文件、{} 字节",
            stats.files, stats.bytes
        );
    } else {
        println!("服务器没有声明 copy-data：跳过 copy-data 这一半，只测 cp");
    }

    let target = fx.path("via-cp");
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp", fx.channel.server_key(), &cancel);
    let elapsed = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(60),
        &opts,
    )
    .await
    .expect("cp -a 复制");
    println!("cp -a 复制用时 {elapsed:?}");
    assert_eq!(
        snapshot(&fx.channel, &target).await,
        expected,
        "cp 的副本应该和源一致"
    );
    assert_eq!(snapshot(&fx.channel, &source).await, expected, "源不能被动");
    for name in ["pwned-by-name", "pwned-by-backtick"] {
        assert!(
            !exists(&fx.channel, &fx.path(name)).await,
            "文件名里的命令被执行了：{name}"
        );
    }
    fx.finish().await;
}

/// 合并进已存在的文件夹（冲突选了覆盖）：内容更新，不会多出 `目标/源名` 这一层嵌套。
#[tokio::test]
async fn cp_merges_into_an_existing_folder_without_nesting_full() {
    if !common::full_live("cp_merges_into_an_existing_folder_without_nesting_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-merge").await else {
        return;
    };
    let source = fx.path("src");
    build_tree(&fx.channel, &source).await;
    let target = fx.path("dst");
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp-merge", fx.channel.server_key(), &cancel);
    copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(60),
        &opts,
    )
    .await
    .expect("第一次复制");
    put(&fx.channel, &format!("{source}/plain.txt"), b"UPDATED").await;
    if !supports_copy_data(fx.channel.caps()) {
        println!("服务器没有声明 copy-data：跳过合并这一半");
        fx.finish().await;
        return;
    }
    let merge_opts = CopyOptions {
        conflict: Some(&OverwriteMerge),
        ..copy_options("live-merge", fx.channel.server_key(), &cancel)
    };
    copy_with_copy_data(fx.channel.as_ref(), &source, &target, &merge_opts)
        .await
        .expect("合并复制");
    let read = zssh_core::sftp::fs::read_bytes(&fx.channel, &format!("{target}/plain.txt"), 1024)
        .await
        .unwrap();
    assert_eq!(read, b"UPDATED");
    assert!(
        !exists(&fx.channel, &format!("{target}/src")).await,
        "合并时不能嵌套出 目标/源名"
    );
    fx.finish().await;
}

/// M3-5b 第 5 项：合并进已有文件夹时，同名文件逐个问：一个跳过、一个覆盖，结果正确。
#[tokio::test]
async fn merging_asks_per_same_name_file_on_the_server_full() {
    if !common::full_live("merging_asks_per_same_name_file_on_the_server_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-merge-ask").await else {
        return;
    };
    if !supports_copy_data(fx.channel.caps()) {
        println!("服务器没有声明 copy-data：跳过（应用里这种服务器会经本机中转）");
        fx.finish().await;
        return;
    }
    let source = fx.path("src");
    let target = fx.path("dst");
    fx.channel.create_dir(&source).await.expect("建源目录");
    fx.channel.create_dir(&target).await.expect("建目标目录");
    put(&fx.channel, &format!("{source}/a.txt"), b"NEW A").await;
    put(&fx.channel, &format!("{source}/b.txt"), b"NEW B").await;
    put(&fx.channel, &format!("{source}/c.txt"), b"NEW C").await;
    put(&fx.channel, &format!("{target}/a.txt"), b"OLD A").await;
    put(&fx.channel, &format!("{target}/b.txt"), b"OLD B").await;
    let resolver = ScriptedMerge {
        decisions: std::sync::Mutex::new(
            vec![CopyConflictDecision::Skip, CopyConflictDecision::Overwrite].into(),
        ),
    };
    let cancel = AtomicBool::new(false);
    let opts = CopyOptions {
        conflict: Some(&resolver),
        ..copy_options("live-merge-ask", fx.channel.server_key(), &cancel)
    };
    let stats = copy_with_copy_data(fx.channel.as_ref(), &source, &target, &opts)
        .await
        .expect("合并");
    assert_eq!(stats.skipped_files, 1);
    let read = |name: &str| {
        let channel = Arc::clone(&fx.channel);
        let path = format!("{target}/{name}");
        async move {
            zssh_core::sftp::fs::read_bytes(&channel, &path, 1024)
                .await
                .expect("读目标")
        }
    };
    assert_eq!(read("a.txt").await, b"OLD A", "跳过的不能被覆盖");
    assert_eq!(read("b.txt").await, b"NEW B", "覆盖的要是新内容");
    assert_eq!(read("c.txt").await, b"NEW C", "新的同名文件照常复制");
    fx.finish().await;
}

/// `cp` 跑了但失败（目标的父目录不存在）：必须是 `Failed`（带 cp 的报错），不是「命令不可用」，
/// 调用方才不会悄悄换办法再来一遍。
#[tokio::test]
async fn a_failing_cp_is_reported_as_failed_not_as_unavailable_full() {
    if !common::full_live("a_failing_cp_is_reported_as_failed_not_as_unavailable_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-fail").await else {
        return;
    };
    let source = fx.path("one.txt");
    put(&fx.channel, &source, b"x").await;
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp-fail", fx.channel.server_key(), &cancel);
    let outcome = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &fx.path("no-such-dir/two.txt"),
        Duration::from_secs(30),
        &opts,
    )
    .await;
    match outcome {
        Err(CpFailure::Failed(message)) => assert!(message.contains("cp"), "{message}"),
        other => panic!("应该是 cp 失败：{other:?}"),
    }
    fx.finish().await;
}

/// M3-5b 第 1 项：`cp` 失败时（目标目录不许新建文件）目标原来的内容不能被动过。
/// 改之前 `cp` 直接写目标，这个文件会被新内容替换掉——哪怕复制最后没有成功。
///
/// 测试服务器登录的是 root，`chmod 0555` 拦不住它（实测）；用 `chattr +i` 让目录真的
/// 不能新建文件，同时已有的文件仍然可写——正是旧实现会丢内容的情况。
#[tokio::test]
async fn a_failed_cp_never_touches_the_existing_target_full() {
    if !common::full_live("a_failed_cp_never_touches_the_existing_target_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-cp-readonly").await else {
        return;
    };
    let dir = fx.path("locked");
    let source = fx.path("source.txt");
    let target = format!("{dir}/target.txt");
    let quote = zssh_core::sftp::server_copy::shell_quote;
    fx.channel.create_dir(&dir).await.expect("建目录");
    put(&fx.channel, &source, b"NEW CONTENT").await;
    put(&fx.channel, &target, b"OLD CONTENT").await;
    if fx
        .connection
        .exec(&format!("chattr +i -- {}", quote(&dir)))
        .await
        .is_err()
    {
        println!("跳过：测试服务器不支持 chattr +i（目录没法做成不可新建）");
        fx.finish().await;
        return;
    }
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp-readonly", fx.channel.server_key(), &cancel);
    let outcome = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(30),
        &opts,
    )
    .await;
    // 先解锁，后面的读取 / 清理 / 守卫删除都才能做。
    fx.connection
        .exec(&format!("chattr -i -- {}", quote(&dir)))
        .await
        .expect("解除目录的 immutable 属性");
    assert!(
        matches!(outcome, Err(CpFailure::Failed(_))),
        "目录不许新建文件时 cp 应该失败：{outcome:?}"
    );
    let kept = zssh_core::sftp::fs::read_bytes(&fx.channel, &target, 1024)
        .await
        .expect("读目标");
    assert_eq!(kept, b"OLD CONTENT", "cp 失败之后目标原来的内容必须还在");
    let names: Vec<String> = fx
        .channel
        .read_dir(&dir)
        .await
        .expect("列目录")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec!["target.txt"], "不能留下临时文件：{names:?}");
    fx.finish().await;
}

/// M3-5b 第 4 项：`cp` 失败之后改经本机中转能成功（界面上的「改为经本机中转重试」
/// 就是带着 `force_relay` 把失败的条目再粘一次；这里直接跑中转任务那一半）。
#[tokio::test]
async fn a_failed_cp_can_be_retried_through_relay_full() {
    if !common::full_live("a_failed_cp_can_be_retried_through_relay_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-cp-retry-relay").await else {
        return;
    };
    let dir = fx.path("locked");
    let source = fx.path("source.txt");
    let target = format!("{dir}/target.txt");
    let quote = zssh_core::sftp::server_copy::shell_quote;
    fx.channel.create_dir(&dir).await.expect("建目录");
    put(&fx.channel, &source, b"NEW CONTENT").await;
    put(&fx.channel, &target, b"OLD CONTENT").await;
    if fx
        .connection
        .exec(&format!("chattr +i -- {}", quote(&dir)))
        .await
        .is_err()
    {
        println!("跳过：测试服务器不支持 chattr +i");
        fx.finish().await;
        return;
    }
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp-retry", fx.channel.server_key(), &cancel);
    let first = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(30),
        &opts,
    )
    .await;
    fx.connection
        .exec(&format!("chattr -i -- {}", quote(&dir)))
        .await
        .expect("解除 immutable");
    assert!(
        matches!(first, Err(CpFailure::Failed(_))),
        "cp 应该先失败：{first:?}"
    );

    // 用户点了「改为经本机中转重试」：同一个源、同一个目标，走引擎。
    let provider = Arc::new(SftpChannelProvider::new(Arc::clone(&fx.connection), None));
    let (engine, mut events) = TransferEngine::new(
        Arc::new(SftpEndpoints::new(provider)),
        Arc::new(OverwriteAll),
        options(),
    );
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: source.clone(),
        target: target.clone(),
        recursive: false,
        peer: None,
    });
    let report = finished(&mut events, id).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    let copied = zssh_core::sftp::fs::read_bytes(&fx.channel, &target, 1024)
        .await
        .expect("读目标");
    assert_eq!(copied, b"NEW CONTENT");
    fx.finish().await;
}

/// M3-5b 第 3 项：大文件夹复制途中取消——目标位置没有东西，临时文件夹被清掉。
#[tokio::test]
async fn a_cancelled_folder_copy_leaves_nothing_behind_full() {
    if !common::full_live("a_cancelled_folder_copy_leaves_nothing_behind_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-cancel-folder").await else {
        return;
    };
    let source = fx.path("big");
    fx.channel.create_dir(&source).await.expect("建源目录");
    for index in 0..30 {
        put(
            &fx.channel,
            &format!("{source}/f{index}.bin"),
            &vec![index as u8; 100_000],
        )
        .await;
    }
    let target = fx.path("big-copy");
    let cancel = AtomicBool::new(false);
    // 复制完第 2 个文件就取消：copy-data 在下一个文件之前停下。
    let progress = |p: CopyProgress| {
        if p.files_done >= 2 {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    };
    let opts = CopyOptions {
        progress: Some(&progress),
        ..copy_options("live-cancel-folder", fx.channel.server_key(), &cancel)
    };
    let result = copy_with_copy_data(fx.channel.as_ref(), &source, &target, &opts).await;
    assert!(
        matches!(result, Err(SftpError::Cancelled)),
        "应该按取消收场：{result:?}"
    );
    assert!(
        !exists(&fx.channel, &target).await,
        "取消之后目标位置不能出现东西"
    );
    let leftovers: Vec<String> = fx
        .channel
        .read_dir(fx.remote.path())
        .await
        .expect("列临时目录")
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| name.contains(".zssh-part"))
        .collect();
    assert!(leftovers.is_empty(), "临时文件夹没有清掉：{leftovers:?}");
    fx.finish().await;
}

/// M3-5b 第 3 项：取消 `cp` 时关闭执行通道；实测服务器上的 cp 进程在通道关掉后
/// 是活着还是被终止（OpenSSH 关通道时会给进程发 SIGHUP）。同时验证目标不变、
/// 临时文件被清掉。
#[tokio::test]
async fn cp_is_stopped_when_its_channel_closes_full() {
    if !common::full_live("cp_is_stopped_when_its_channel_closes_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-cancel-cp").await else {
        return;
    };
    // 造一棵很多文件的树，保证取消的时候 cp 还没跑完（单个大文件在这台服务器上
    // 会被 cp 的 reflink / copy_file_range 瞬间拷完，观察不到取消）。磁盘不够就跳过。
    let free_kb = fx
        .connection
        .exec("df -Pk /tmp | awk 'NR==2 {print $4}'")
        .await
        .ok()
        .and_then(|out| out.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if free_kb < 200_000 {
        println!("跳过：测试服务器 /tmp 剩余空间不足 200MB（{free_kb} KB）");
        fx.finish().await;
        return;
    }
    if fx
        .connection
        .exec("command -v python3 >/dev/null")
        .await
        .is_err()
    {
        println!("跳过：测试服务器没有 python3，造不出足够多的文件");
        fx.finish().await;
        return;
    }
    let source = fx.path("huge-tree");
    let target = fx.path("huge-tree-copy");
    fx.connection
        .exec(&format!(
            "python3 -c 'import os,sys; root=sys.argv[1]; os.makedirs(root); [open(os.path.join(root, f\"f{{i}}.bin\"), \"wb\").write(b\"x\"*256) for i in range(2000)]' {}",
            zssh_core::sftp::server_copy::shell_quote(&source)
        ))
        .await
        .expect("造 2000 个文件的源目录");
    let cancel = Arc::new(AtomicBool::new(false));
    let owner = "live-cancel-cp";
    let temp = zssh_core::sftp::transfer::endpoint::part_name(&target, owner);
    // 先等到临时目录真的出现了再取消：证明 cp 已经在服务器上跑起来，GONE 才有意义。
    let seen = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let seen_writer = Arc::clone(&seen);
    let probe = Arc::clone(&fx.channel);
    let probe_path = temp.clone();
    tokio::spawn(async move {
        for _ in 0..1000 {
            if probe.lstat(&probe_path).await.is_ok() {
                seen_writer.store(true, std::sync::atomic::Ordering::SeqCst);
                // 它已经在写临时目录了：马上取消，好落在 cp 中间。
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let opts = copy_options(owner, fx.channel.server_key(), &cancel);
    let started = std::time::Instant::now();
    let result = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(300),
        &opts,
    )
    .await;
    if result.is_ok() {
        println!(
            "cp 在取消之前就完成了（{:?}），这次没观察到取消",
            started.elapsed()
        );
        let _ = fx
            .connection
            .exec(&format!(
                "rm -rf {} {}",
                zssh_core::sftp::server_copy::shell_quote(&source),
                zssh_core::sftp::server_copy::shell_quote(&target)
            ))
            .await;
        fx.finish().await;
        return;
    }
    assert_eq!(
        result,
        Err(CpFailure::Cancelled),
        "应该按取消收场：{result:?}"
    );
    println!(
        "取消前服务器上已经出现临时目录：{}",
        if seen.load(std::sync::atomic::Ordering::SeqCst) {
            "是"
        } else {
            "否（cp 还没开始写）"
        }
    );
    // 通道关掉之后服务器上的 cp 还在不在：只输出 GONE / ALIVE，不打印进程列表。
    // 只看 comm 是 cp 的进程：检查命令自己（sh / awk）的 argv 里也有临时路径，
    // 用 grep 会把自己的命令行算进去。
    let alive = fx
        .connection
        .exec(&format!(
            "ps -eo comm=,args= | awk -v t={} '$1 == \"cp\" && index($0, t) > 0 {{ f=1 }} END {{ print f ? \"ALIVE\" : \"GONE\" }}'",
            zssh_core::sftp::server_copy::shell_quote(&temp)
        ))
        .await
        .map(|out| out.trim().to_owned())
        .unwrap_or_else(|_| "UNKNOWN".to_owned());
    println!("关闭执行通道之后服务器上的 cp：{alive}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !exists(&fx.channel, &target).await,
        "取消之后目标位置不能出现东西"
    );
    assert!(
        fx.channel.lstat(&temp).await.is_err(),
        "临时文件没有清掉：{temp}"
    );
    let _ = fx
        .connection
        .exec(&format!(
            "rm -rf {}",
            zssh_core::sftp::server_copy::shell_quote(&source)
        ))
        .await;
    fx.finish().await;
}

/// 经本机中转：引擎的 RemoteCopy 方向，同一台主机，整棵树一致。
#[tokio::test]
async fn relay_copy_through_the_engine_copies_the_tree_full() {
    if !common::full_live("relay_copy_through_the_engine_copies_the_tree_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-relay").await else {
        return;
    };
    let source = fx.path("src");
    build_tree(&fx.channel, &source).await;
    let expected = snapshot(&fx.channel, &source).await;

    let provider = Arc::new(SftpChannelProvider::new(Arc::clone(&fx.connection), None));
    let (engine, mut events) = TransferEngine::new(
        Arc::new(SftpEndpoints::new(provider)),
        Arc::new(OverwriteAll),
        options(),
    );
    let target = fx.path("relayed");
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: source.clone(),
        target: target.clone(),
        recursive: true,
        peer: None,
    });
    let report = finished(&mut events, id).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    // 空目录也要复制过去。
    assert_eq!(snapshot(&fx.channel, &target).await, expected);
    fx.finish().await;
}

/// 源在「另一台主机」（这里用同一台服务器上的另一条独立连接当作另一台）：
/// 目标这边的引擎登记那条连接的提供者，从它读、往自己写。
#[tokio::test]
async fn relay_copy_reads_from_a_registered_peer_connection_full() {
    if !common::full_live("relay_copy_reads_from_a_registered_peer_connection_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-peer").await else {
        return;
    };
    let known_hosts = TempKnownHosts::new("copy-peer-2");
    let peer_connection = Arc::new(fx.server.connect(&known_hosts).await.expect("第二条连接"));
    let source = fx.path("src");
    build_tree(&fx.channel, &source).await;
    let expected = snapshot(&fx.channel, &source).await;

    let own = Arc::new(SftpChannelProvider::new(Arc::clone(&fx.connection), None));
    let endpoints = Arc::new(SftpEndpoints::new(own));
    endpoints.register_peer(
        "other-host",
        Arc::new(SftpChannelProvider::new(Arc::clone(&peer_connection), None)),
    );
    let (engine, mut events) =
        TransferEngine::new(endpoints.clone(), Arc::new(OverwriteAll), options());
    let target = fx.path("from-peer");
    let id = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source: source.clone(),
        target: target.clone(),
        recursive: true,
        peer: Some("other-host".to_owned()),
    });
    let report = finished(&mut events, id).await;
    assert_eq!(report.state, TransferState::Done, "{report:?}");
    assert_eq!(snapshot(&fx.channel, &target).await, expected);

    // 登记名没登记过：明确失败，不能悄悄当成同一台主机。
    let missing = engine.submit(TransferJob {
        direction: Direction::RemoteCopy,
        source,
        target: fx.path("never"),
        recursive: true,
        peer: Some("not-registered".to_owned()),
    });
    let report = finished(&mut events, missing).await;
    assert!(
        matches!(report.state, TransferState::Failed(_)),
        "{report:?}"
    );
    assert!(!exists(&fx.channel, &fx.path("never")).await);

    let _ = peer_connection.disconnect().await;
    fx.finish().await;
}

/// 「计算大小」：数出的文件数、目录数、字节数和实际写的一致。
#[tokio::test]
async fn remote_size_matches_what_was_written_full() {
    if !common::full_live("remote_size_matches_what_was_written_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-size").await else {
        return;
    };
    let root = fx.path("tree");
    build_tree(&fx.channel, &root).await;
    let expected_bytes: u64 = snapshot(&fx.channel, &root)
        .await
        .values()
        .map(|content| content.as_ref().map_or(0, |bytes| bytes.len() as u64))
        .sum();
    let provider = Arc::new(SftpChannelProvider::new(Arc::clone(&fx.connection), None));
    let endpoints = SftpEndpoints::new(provider);
    let totals = remote_size(&endpoints, &root, &AtomicBool::new(false), |_| {})
        .await
        .expect("计算大小");
    assert_eq!(totals.bytes, expected_bytes);
    assert_eq!(totals.files, NASTY.len() as u64 + 2);
    assert_eq!(totals.dirs, 4, "根、sub dir、deeper、empty");
    fx.finish().await;
}

/// C09：源树里有指回自己的目录链接、指向树外的目录链接：`cp -a` 复制的是链接本身，
/// 核对时也只按链接本身算——要很快完成，不能被循环困住或数到树外。
/// 链接都只指向本测试自己的临时目录里。
#[tokio::test]
async fn cp_verification_does_not_follow_directory_links_full() {
    if !common::full_live("cp_verification_does_not_follow_directory_links_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-links").await else {
        return;
    };
    let source = fx.path("src");
    let outside = fx.path("outside");
    fx.channel.create_dir(&source).await.expect("建 src");
    fx.channel
        .create_dir(&format!("{source}/sub"))
        .await
        .expect("建 sub");
    fx.channel.create_dir(&outside).await.expect("建 outside");
    put(&fx.channel, &format!("{source}/a.txt"), b"AAAA").await;
    put(&fx.channel, &format!("{outside}/big.bin"), &[0u8; 4096]).await;
    let made = fx
        .connection
        .exec(&format!(
            "ln -s .. '{source}/sub/loop' && ln -s '{outside}' '{source}/out' && echo OK"
        ))
        .await
        .unwrap_or_default();
    assert!(made.contains("OK"), "造符号链接失败：{made}");

    let target = fx.path("dst");
    let cancel = AtomicBool::new(false);
    let opts = copy_options("live-cp-links", fx.channel.server_key(), &cancel);
    let copied = tokio::time::timeout(
        Duration::from_secs(60),
        copy_with_cp(
            fx.connection.as_ref(),
            fx.channel.as_ref(),
            &source,
            &target,
            Duration::from_secs(60),
            &opts,
        ),
    )
    .await
    .expect("核对不能被循环链接困住");
    assert!(copied.is_ok(), "{copied:?}");
    let link = fx
        .channel
        .lstat(&format!("{target}/sub/loop"))
        .await
        .expect("副本里的链接");
    assert!(link.is_symlink(), "链接按链接本身复制");
    assert!(fx
        .channel
        .lstat(&format!("{target}/out"))
        .await
        .expect("副本里指向树外的链接")
        .is_symlink());
    fx.finish().await;
}

/// C21：目标文件夹是一个指回源树里的符号链接：按字符串看不出来，按真实路径看就是
/// 复制进自己。粘贴前的检查、copy-data、cp 都要拒绝，源树里不能多出任何东西。
#[tokio::test]
async fn copying_into_a_link_back_into_the_source_is_refused_full() {
    use zssh_core::sftp::server_copy::{ensure_folder_not_inside, ensure_not_into_itself};

    if !common::full_live("copying_into_a_link_back_into_the_source_is_refused_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("copy-alias").await else {
        return;
    };
    let source = fx.path("src");
    fx.channel.create_dir(&source).await.expect("建 src");
    fx.channel
        .create_dir(&format!("{source}/sub"))
        .await
        .expect("建 sub");
    put(&fx.channel, &format!("{source}/a.txt"), b"AAAA").await;
    let alias = fx.path("alias");
    let made = fx
        .connection
        .exec(&format!("ln -s '{source}/sub' '{alias}' && echo OK"))
        .await
        .unwrap_or_default();
    assert!(made.contains("OK"), "造符号链接失败：{made}");
    let before = snapshot(&fx.channel, &source).await;

    let refused = ensure_folder_not_inside(fx.channel.as_ref(), &source, &alias).await;
    assert!(
        matches!(refused, Err(SftpError::InvalidInput(_))),
        "{refused:?}"
    );
    let target = format!("{alias}/src");
    let refused = ensure_not_into_itself(fx.channel.as_ref(), &source, &target).await;
    assert!(
        matches!(refused, Err(SftpError::InvalidInput(_))),
        "{refused:?}"
    );

    let cancel = AtomicBool::new(false);
    if supports_copy_data(fx.channel.caps()) {
        let opts = copy_options("live-alias-cd", fx.channel.server_key(), &cancel);
        let result = copy_with_copy_data(fx.channel.as_ref(), &source, &target, &opts).await;
        assert!(
            matches!(result, Err(SftpError::InvalidInput(_))),
            "{result:?}"
        );
    } else {
        println!("服务器没有声明 copy-data：跳过 copy-data 这一半");
    }
    let opts = copy_options("live-alias-cp", fx.channel.server_key(), &cancel);
    let result = copy_with_cp(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &source,
        &target,
        Duration::from_secs(60),
        &opts,
    )
    .await;
    assert!(matches!(result, Err(CpFailure::Failed(_))), "{result:?}");
    assert_eq!(
        snapshot(&fx.channel, &source).await,
        before,
        "源树里不能多出任何东西"
    );

    // 粘贴到和源不相干的文件夹照常放行。
    ensure_folder_not_inside(fx.channel.as_ref(), &source, fx.remote.path())
        .await
        .expect("粘贴到源所在的文件夹");
    fx.finish().await;
}
