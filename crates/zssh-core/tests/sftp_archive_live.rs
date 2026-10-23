//! UI-13 的联网测试：远端压缩 / 解压对着真实的测试服务器跑。
//!
//! 覆盖：tar.gz 和 zip 的压缩 → 解压往返（含难缠的名字）、解压到新建文件夹 / 已有文件夹、
//! 同名文件「覆盖 / 跳过」、含绝对路径或 `..` 的包被拒绝、取消后服务器上没有进程和残留。
//! 同时把这台服务器上 tar / zip / unzip 的情况、`--skip-old-files` 是否可用打印出来（验收报告要记）。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）；远端只用专用临时目录。
//! 用例名里带「完整」的才在 `ZSSH_LIVE_FULL=1`（`scripts/live-tests.ps1`）下跑。

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zssh_core::sftp::archive::{
    compress, extract, plan_extract, probe_caps, ArchiveCaps, ArchiveError, CompressFormat,
    CompressRequest, ConflictPolicy, ExtractPlan, ExtractRequest,
};
use zssh_core::sftp::SftpChannel;
use zssh_core::ssh::Connection;

use common::{serial, RemoteTempDir, TempKnownHosts, TestServer};

/// 名字里带空格、单引号、`$()`、反引号、开头的横杠、中文：任何一个没转义好都会出事。
const NASTY: &[&str] = &[
    "plain.txt",
    "with space.txt",
    "it's.txt",
    "$(touch pwned-by-name).txt",
    "`touch pwned-by-backtick`.txt",
    "-dash.txt",
    "中文 名字.txt",
];

struct Fixture {
    server: TestServer,
    connection: Arc<Connection>,
    channel: Arc<SftpChannel>,
    remote: RemoteTempDir,
    caps: ArchiveCaps,
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
    let caps = probe_caps(&connection).await;
    Some(Fixture {
        server,
        connection,
        channel,
        remote,
        caps,
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

    async fn sh(&self, command: &str) -> (Option<u32>, String) {
        let result = self
            .connection
            .exec_capture(command, Duration::from_secs(120), 1 << 20)
            .await
            .expect("执行命令");
        (result.exit_code, result.stdout)
    }

    async fn put(&self, path: &str, bytes: &[u8]) {
        zssh_core::sftp::fs::write_bytes(&self.channel, path, bytes, 1 << 20)
            .await
            .unwrap_or_else(|err| panic!("写 {path}：{err}"));
    }

    async fn read(&self, path: &str) -> Vec<u8> {
        zssh_core::sftp::fs::read_bytes(&self.channel, path, 4 << 20)
            .await
            .unwrap_or_else(|err| panic!("读 {path}：{err}"))
    }

    async fn exists(&self, path: &str) -> bool {
        self.channel.lstat(path).await.is_ok()
    }

    /// 建一棵测试树：难缠的名字、子目录、空目录。
    async fn build_tree(&self, root: &str) {
        for dir in [
            root.to_owned(),
            format!("{root}/sub dir"),
            format!("{root}/sub dir/deeper"),
            format!("{root}/empty"),
        ] {
            self.channel.create_dir(&dir).await.expect("建目录");
        }
        for (index, name) in NASTY.iter().enumerate() {
            self.put(
                &format!("{root}/{name}"),
                format!("content-{index}").as_bytes(),
            )
            .await;
        }
        self.put(&format!("{root}/sub dir/b.bin"), &vec![7u8; 200_000])
            .await;
        self.put(&format!("{root}/sub dir/deeper/c.txt"), b"deep")
            .await;
    }

    /// 整棵树的快照：相对路径 → 内容（目录是 None）。
    async fn snapshot(&self, root: &str) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut pending = vec![(root.to_owned(), String::new())];
        while let Some((dir, relative)) = pending.pop() {
            for entry in self.channel.read_dir(&dir).await.expect("列目录") {
                let rel = if relative.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{relative}/{}", entry.name)
                };
                if entry.kind.is_dir() {
                    out.insert(rel.clone(), None);
                    pending.push((entry.path.clone(), rel));
                } else {
                    out.insert(rel, Some(self.read(&entry.path).await));
                }
            }
        }
        out
    }

    /// 目录里所有名字里带 `.zssh-part` 的残留。
    async fn leftovers(&self, dir: &str) -> Vec<String> {
        self.channel
            .read_dir(dir)
            .await
            .expect("列目录")
            .into_iter()
            .filter(|entry| entry.name.contains(".zssh-part"))
            .map(|entry| entry.name)
            .collect()
    }
}

fn compress_request<'a>(
    fx: &'a Fixture,
    names: &'a [String],
    format: CompressFormat,
    owner: &'a str,
) -> CompressRequest<'a> {
    CompressRequest {
        dir: fx.remote.path(),
        names,
        format,
        owner,
        server: fx.channel.server_key(),
    }
}

async fn plan(fx: &Fixture, archive: &str, dest: &str) -> Result<ExtractPlan, ArchiveError> {
    let cancel = AtomicBool::new(false);
    plan_extract(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        archive,
        dest,
        &cancel,
    )
    .await
}

async fn run_extract(
    fx: &Fixture,
    plan: &ExtractPlan,
    archive: &str,
    dest: &str,
    policy: ConflictPolicy,
    owner: &str,
) -> Result<zssh_core::sftp::archive::ExtractOutcome, ArchiveError> {
    let cancel = AtomicBool::new(false);
    let request = ExtractRequest {
        archive,
        dest,
        policy,
        plan_fingerprint: plan.fingerprint,
        plan_dest_exists: plan.dest_exists,
        plan_total_files: plan.total_files,
        plan_conflict_count: plan.conflict_count,
        owner,
        server: fx.channel.server_key(),
    };
    extract(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &request,
        &cancel,
    )
    .await
}

/// 把这台测试服务器的情况打印出来（验收报告要记）。
#[tokio::test]
async fn archive_capabilities_of_the_test_server_are_recorded() {
    let _serial = serial().await;
    let Some(fx) = fixture("arch-caps").await else {
        return;
    };
    println!("测试服务器压缩能力：{:?}", fx.caps);
    let (_, version) = fx.sh("tar --version 2>&1 | head -1").await;
    println!("tar：{}", version.trim());
    let (code, out) = fx
        .sh("d=$(mktemp -d) && cd \"$d\" && echo x > a && tar --skip-old-files -cf t.tar a 2>&1; echo \"skip-old-files 退出码 $?\"; cd / && rm -rf \"$d\"")
        .await;
    println!("{}（exec 退出码 {code:?}）", out.trim());
    assert!(fx.caps.posix, "测试服务器应该是 POSIX shell");
    assert!(fx.caps.tar, "测试服务器应该有 tar");
    fx.finish().await;
}

/// tar.gz 往返：压缩出 `logs.tar.gz`，包里是相对路径，解压到新建的同名文件夹后和原来一致，
/// 名字里的命令没有被执行，没有临时文件残留。
#[tokio::test]
async fn tar_gz_round_trip_with_nasty_names_full() {
    if !common::full_live("tar_gz_round_trip_with_nasty_names_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-targz").await else {
        return;
    };
    let source = fx.path("logs");
    fx.build_tree(&source).await;
    fx.put(&fx.path("single file.txt"), b"hello").await;
    let expected = fx.snapshot(&source).await;

    let cancel = AtomicBool::new(false);
    let list = vec!["logs".to_owned()];
    let outcome = compress(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &compress_request(&fx, &list, CompressFormat::TarGz, "live1"),
        &cancel,
        None,
    )
    .await
    .expect("压缩 tar.gz");
    assert_eq!(outcome.name, "logs.tar.gz");
    assert!(fx.exists(&fx.path("logs.tar.gz")).await);
    assert!(
        fx.leftovers(fx.remote.path()).await.is_empty(),
        "临时文件应该已经改名"
    );

    // 包里是相对路径
    let (_, listing) = fx
        .sh(&format!("tar -tzf '{}'", fx.path("logs.tar.gz")))
        .await;
    assert!(
        listing.lines().all(|line| !line.starts_with('/')),
        "{listing}"
    );
    assert!(listing.contains("logs/"), "{listing}");

    // 再压一次：重名自动加 (1)，不覆盖
    let before = fx.read(&fx.path("logs.tar.gz")).await;
    let again = compress(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &compress_request(&fx, &list, CompressFormat::TarGz, "live2"),
        &cancel,
        None,
    )
    .await
    .expect("再压一次");
    assert_eq!(again.name, "logs (1).tar.gz");
    assert_eq!(
        fx.read(&fx.path("logs.tar.gz")).await,
        before,
        "原来的包不动"
    );

    // 多个条目：用当前目录名作主名；以 - 开头的名字也要能压
    fx.put(&fx.path("-leading dash"), b"dash").await;
    let many = vec!["single file.txt".to_owned(), "-leading dash".to_owned()];
    let multi = compress(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &compress_request(&fx, &many, CompressFormat::TarGz, "live3"),
        &cancel,
        None,
    )
    .await
    .expect("压缩多个");
    assert!(
        multi.name.ends_with(".tar.gz") && multi.name.starts_with("zssh-sftp-live-arch-targz"),
        "{}",
        multi.name
    );

    // 解压到新建的同名文件夹
    let dest = fx.path("out-logs");
    let archive = fx.path("logs.tar.gz");
    let checked = plan(&fx, &archive, &dest).await.expect("检查压缩包");
    assert!(!checked.dest_exists);
    assert_eq!(checked.conflict_count, 0);
    let done = run_extract(
        &fx,
        &checked,
        &archive,
        &dest,
        ConflictPolicy::Overwrite,
        "live4",
    )
    .await
    .expect("解压");
    println!(
        "tar.gz 解压：解出 {} 个文件，跳过 {}",
        done.extracted, done.skipped
    );
    assert_eq!(fx.snapshot(&format!("{dest}/logs")).await, expected);
    assert!(fx.leftovers(fx.remote.path()).await.is_empty());
    for name in ["pwned-by-name", "pwned-by-backtick"] {
        assert!(
            !fx.exists(&fx.path(name)).await,
            "文件名里的命令被执行了：{name}"
        );
    }
    fx.finish().await;
}

/// zip 往返（服务器没有 zip / unzip 就跳过并说明）。
#[tokio::test]
async fn zip_round_trip_with_nasty_names_full() {
    if !common::full_live("zip_round_trip_with_nasty_names_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-zip").await else {
        return;
    };
    if !(fx.caps.zip && fx.caps.unzip) {
        println!(
            "跳过 zip 往返：服务器上 zip={} unzip={}",
            fx.caps.zip, fx.caps.unzip
        );
        fx.finish().await;
        return;
    }
    let source = fx.path("pack");
    fx.build_tree(&source).await;
    let expected = fx.snapshot(&source).await;
    let cancel = AtomicBool::new(false);
    let list = vec!["pack".to_owned()];
    let outcome = compress(
        fx.connection.as_ref(),
        fx.channel.as_ref(),
        &compress_request(&fx, &list, CompressFormat::Zip, "livez1"),
        &cancel,
        None,
    )
    .await
    .expect("压缩 zip");
    assert_eq!(outcome.name, "pack.zip");
    assert!(fx.leftovers(fx.remote.path()).await.is_empty());

    let archive = fx.path("pack.zip");
    let dest = fx.path("unzipped");
    let checked = plan(&fx, &archive, &dest).await.expect("检查 zip");
    let done = run_extract(
        &fx,
        &checked,
        &archive,
        &dest,
        ConflictPolicy::Overwrite,
        "livez2",
    )
    .await
    .expect("解压 zip");
    println!("zip 解压：解出 {} 个文件", done.extracted);
    assert_eq!(fx.snapshot(&format!("{dest}/pack")).await, expected);
    fx.finish().await;
}

/// 解压到已有文件夹的同名文件：检查能数出冲突；覆盖会换成包里的内容；跳过保留原来的内容，
/// 其余文件照常解出。tar.gz 和 zip 各做一遍。
#[tokio::test]
async fn conflicts_overwrite_and_skip_full() {
    if !common::full_live("conflicts_overwrite_and_skip_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-conflict").await else {
        return;
    };
    let source = fx.path("data");
    for dir in [source.clone(), format!("{source}/sub")] {
        fx.channel.create_dir(&dir).await.unwrap();
    }
    fx.put(&format!("{source}/a.txt"), b"from-archive-a").await;
    fx.put(&format!("{source}/b.txt"), b"from-archive-b").await;
    fx.put(&format!("{source}/sub/c.txt"), b"from-archive-c")
        .await;
    let list = vec!["data".to_owned()];
    let cancel = AtomicBool::new(false);

    let mut formats = vec![("tar.gz", CompressFormat::TarGz)];
    if fx.caps.zip && fx.caps.unzip {
        formats.push(("zip", CompressFormat::Zip));
    } else {
        println!("服务器没有 zip / unzip：冲突用例只测 tar.gz");
    }
    for (label, format) in formats {
        let packed = compress(
            fx.connection.as_ref(),
            fx.channel.as_ref(),
            &compress_request(&fx, &list, format, "livec1"),
            &cancel,
            None,
        )
        .await
        .expect("压缩");
        let archive = fx.path(&packed.name);

        // 目标：data 已经存在，里面有同名的 a.txt（内容不同）和一个多余的文件
        for (case, policy) in [
            ("overwrite", ConflictPolicy::Overwrite),
            ("skip", ConflictPolicy::SkipExisting),
        ] {
            let dest = fx.path(&format!("dest-{label}-{case}"));
            fx.channel.create_dir(&dest).await.unwrap();
            fx.channel
                .create_dir(&format!("{dest}/data"))
                .await
                .unwrap();
            fx.put(&format!("{dest}/data/a.txt"), b"LOCAL-a").await;
            fx.put(&format!("{dest}/data/extra.txt"), b"extra").await;
            let checked = plan(&fx, &archive, &dest).await.expect("检查");
            assert!(checked.dest_exists);
            assert_eq!(
                checked.conflict_count, 1,
                "{label}: 只有 data/a.txt 重名：{checked:?}"
            );
            assert_eq!(checked.conflict_sample.len(), 1);
            assert!(checked.conflict_sample[0].ends_with("data/a.txt"));
            let done = run_extract(&fx, &checked, &archive, &dest, policy, "livec2")
                .await
                .unwrap_or_else(|err| panic!("{label} {case}：{err}"));
            println!(
                "{label} {case}：解出 {}，跳过 {}，警告 {:?}",
                done.extracted, done.skipped, done.warning
            );
            let a = fx.read(&format!("{dest}/data/a.txt")).await;
            match policy {
                ConflictPolicy::Overwrite => {
                    assert_eq!(a, b"from-archive-a", "{label}: 覆盖后应是包里的内容");
                    assert_eq!(done.skipped, 0);
                }
                ConflictPolicy::SkipExisting => {
                    assert_eq!(a, b"LOCAL-a", "{label}: 跳过后应保留原来的内容");
                    assert_eq!(done.skipped, 1);
                }
            }
            assert_eq!(
                fx.read(&format!("{dest}/data/b.txt")).await,
                b"from-archive-b"
            );
            assert_eq!(
                fx.read(&format!("{dest}/data/sub/c.txt")).await,
                b"from-archive-c"
            );
            assert_eq!(
                fx.read(&format!("{dest}/data/extra.txt")).await,
                b"extra",
                "多余的文件不动"
            );
        }
    }
    assert!(fx.leftovers(fx.remote.path()).await.is_empty());
    fx.finish().await;
}

/// 含绝对路径或 `..` 的包被拒绝：什么都没解出来，目标位置也没有多出文件夹。
#[tokio::test]
async fn archives_that_escape_the_destination_are_refused_full() {
    if !common::full_live("archives_that_escape_the_destination_are_refused_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-evil").await else {
        return;
    };
    let base = fx.remote.path().to_owned();
    // 绝对路径包：-P 保留开头的 /
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}' && mkdir victim && echo secret > victim/s.txt && tar -cPzf abs.tar.gz '{base}/victim/s.txt' 2>&1"
        ))
        .await;
    assert_eq!(code, Some(0), "造绝对路径包失败：{out}");
    // `..` 包：GNU tar 的 --transform 给每个名字加 ../ 前缀
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}' && echo evil > evil.txt && tar -czf dotdot.tar.gz --transform='s,^,../,' evil.txt 2>&1"
        ))
        .await;
    if code != Some(0) {
        println!("服务器的 tar 不支持 --transform，跳过 .. 包：{out}");
    }
    for archive in ["abs.tar.gz", "dotdot.tar.gz"] {
        if !fx.exists(&fx.path(archive)).await {
            continue;
        }
        let dest = fx.path(&format!("out-{archive}"));
        let err = plan(&fx, &fx.path(archive), &dest).await.unwrap_err();
        assert_eq!(err, ArchiveError::UnsafeEntries, "{archive}");
        assert!(!fx.exists(&dest).await, "拒绝后不能留下目标文件夹");
    }
    assert!(fx.leftovers(&base).await.is_empty());
    fx.finish().await;
}

/// 取消：压缩一个较大的随机数据文件，2 秒后取消。返回 Cancelled，服务器上没有临时文件、也没有
/// 最终压缩包，tar 进程也不在了（按临时文件名在 ps 里找）。
#[tokio::test]
async fn cancelling_a_compress_stops_the_server_process_and_leaves_nothing_full() {
    if !common::full_live("cancelling_a_compress_stops_the_server_process_and_leaves_nothing_full")
    {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-cancel-c").await else {
        return;
    };
    let big = fx.path("big.bin");
    let (code, out) = fx
        .sh(&format!(
            "head -c 600000000 /dev/urandom > '{big}' && echo done"
        ))
        .await;
    assert_eq!(code, Some(0), "造大文件失败：{out}");

    let cancel = Arc::new(AtomicBool::new(false));
    let canceller = {
        let cancel = Arc::clone(&cancel);
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let list = vec!["big.bin".to_owned()];
    let request = compress_request(&fx, &list, CompressFormat::TarGz, "livex1");
    let (result, ()) = tokio::join!(
        compress(
            fx.connection.as_ref(),
            fx.channel.as_ref(),
            &request,
            &cancel,
            None
        ),
        canceller
    );
    assert_eq!(result.unwrap_err(), ArchiveError::Cancelled);
    assert!(
        !fx.exists(&fx.path("big.bin.tar.gz")).await,
        "取消后不能有最终压缩包"
    );
    assert!(
        fx.leftovers(fx.remote.path()).await.is_empty(),
        "取消后不能有临时文件"
    );

    // 服务器上的进程：给它一点时间，再按临时名（命令行里有）找
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, alive) = fx
        .sh("ps -eo args 2>/dev/null | grep -F 'livex1.zssh-part' | grep -v grep | wc -l")
        .await;
    let (_, detail) = fx
        .sh("ps -eo pid,ppid,etime,args 2>/dev/null | grep -F livex1.zssh-part | grep -v grep")
        .await;
    println!("残留进程详情：{detail}");
    println!("取消 2 秒后服务器上还在跑的 tar 进程数：{}", alive.trim());
    assert_eq!(alive.trim(), "0", "取消后服务器上的 tar 还在跑");
    assert!(
        fx.leftovers(fx.remote.path()).await.is_empty(),
        "进程若还活着会重新写出临时文件"
    );
    fx.finish().await;
}

/// 取消解压：解压一个较大的包到新建文件夹，中途取消。没有目标文件夹、没有临时文件夹，进程也不在了。
#[tokio::test]
async fn cancelling_an_extract_removes_the_temp_folder_and_stops_tar_full() {
    if !common::full_live("cancelling_an_extract_removes_the_temp_folder_and_stops_tar_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-cancel-x").await else {
        return;
    };
    let base = fx.remote.path().to_owned();
    // 一个有很多中等文件的包，解压要持续几秒
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}' && mkdir src && i=0 && while [ $i -lt 40 ]; do head -c 20000000 /dev/urandom > src/f$i.bin; i=$((i+1)); done && tar -czf big.tar.gz src && rm -rf src && echo done"
        ))
        .await;
    assert_eq!(code, Some(0), "造大包失败：{out}");
    let archive = fx.path("big.tar.gz");
    let dest = fx.path("big");
    let checked = plan(&fx, &archive, &dest).await.expect("检查");

    let cancel = Arc::new(AtomicBool::new(false));
    let canceller = {
        let cancel = Arc::clone(&cancel);
        async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let request = ExtractRequest {
        archive: &archive,
        dest: &dest,
        policy: ConflictPolicy::Overwrite,
        plan_fingerprint: checked.fingerprint,
        plan_dest_exists: checked.dest_exists,
        plan_total_files: checked.total_files,
        plan_conflict_count: checked.conflict_count,
        owner: "livex2",
        server: fx.channel.server_key(),
    };
    let (result, ()) = tokio::join!(
        extract(
            fx.connection.as_ref(),
            fx.channel.as_ref(),
            &request,
            &cancel
        ),
        canceller
    );
    match result {
        Err(ArchiveError::Cancelled) => {}
        other => panic!("应该被取消：{other:?}（可能解压太快，调大测试数据）"),
    }
    assert!(!fx.exists(&dest).await, "取消后不能留下目标文件夹");
    assert!(
        fx.leftovers(&base).await.is_empty(),
        "取消后不能有临时文件夹"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, alive) = fx
        .sh("ps -eo args 2>/dev/null | grep -F 'big.tar.gz' | grep -v grep | wc -l")
        .await;
    println!("取消 2 秒后服务器上还在跑的解压进程数：{}", alive.trim());
    assert_eq!(alive.trim(), "0", "取消后服务器上的 tar 还在跑");
    assert!(fx.leftovers(&base).await.is_empty());
    assert!(!fx.exists(&dest).await);
    fx.finish().await;
}

/// CRC-32（zip 用的那个），测试里自己造 zip 要用。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// 按 zip 格式写一个最小的包（只存储、不压缩）。条目名原样写进去，所以可以写 Windows 风格的
/// 反斜杠路径（服务器上的 zip 命令造不出这种包）。
fn minimal_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let crc = crc32(data);
        let size = u32::try_from(data.len()).unwrap();
        let name_len = u16::try_from(name.len()).unwrap();
        // 本地文件头
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&10u16.to_le_bytes()); // 解压所需版本
        out.extend_from_slice(&0u16.to_le_bytes()); // 标志
        out.extend_from_slice(&0u16.to_le_bytes()); // 存储，不压缩
        out.extend_from_slice(&0u16.to_le_bytes()); // 时间
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // 日期：1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 额外字段
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // 中央目录项（制作系统记成 DOS，和 Windows 资源管理器打的包一样）
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // 制作版本，高字节 0 = DOS
        central.extend_from_slice(&10u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // 额外字段
        central.extend_from_slice(&0u16.to_le_bytes()); // 注释
        central.extend_from_slice(&0u16.to_le_bytes()); // 起始磁盘
        central.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
        central.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = u32::try_from(out.len()).unwrap();
    let central_size = u32::try_from(central.len()).unwrap();
    let count = u16::try_from(entries.len()).unwrap();
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// 目录里的条目名（只列名字，用来确认「目录外没有多出东西」）。
async fn names_in(fx: &Fixture, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = fx
        .channel
        .read_dir(dir)
        .await
        .expect("列目录")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    names
}

/// 目标目录里已有指向目录外的符号链接：包里经过它（或就是它）的条目会被整包拒绝，
/// 目录外一个文件都不会多出来。tar.gz 和 zip（含反斜杠写法）各来一遍。
#[tokio::test]
async fn symlinks_inside_the_destination_refuse_the_whole_archive_full() {
    if !common::full_live("symlinks_inside_the_destination_refuse_the_whole_archive_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-symlink").await else {
        return;
    };
    let base = fx.remote.path().to_owned();
    // outside 是目录外的目录；dest 里 link 指向它，fileLink 指向它里面的一个还不存在的文件
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}' && mkdir outside dest src src/link && echo a > src/link/a.txt && echo ok > src/ok.txt \
             && ln -s ../outside dest/link && ln -s ../outside/target.txt dest/fileLink && echo made"
        ))
        .await;
    assert_eq!(code, Some(0), "造测试目录失败：{out}");
    let outside = fx.path("outside");
    let dest = fx.path("dest");
    let before = names_in(&fx, &outside).await;
    assert!(before.is_empty());

    // tar.gz：经过 link 的文件
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}/src' && tar -czf ../through.tar.gz ok.txt link 2>&1"
        ))
        .await;
    assert_eq!(code, Some(0), "造 tar 失败：{out}");
    // tar.gz：条目本身就是目标里已有的文件链接
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}' && mkdir src2 && echo new > src2/fileLink && cd src2 && tar -czf ../self.tar.gz fileLink 2>&1"
        ))
        .await;
    assert_eq!(code, Some(0), "造 tar 失败：{out}");
    // tar.gz：目录条目本身就是已有的目录链接
    let (code, out) = fx
        .sh(&format!(
            "cd '{base}/src' && tar -czf ../dirlink.tar.gz --no-recursion link 2>&1"
        ))
        .await;
    assert_eq!(code, Some(0), "造 tar 失败：{out}");

    for (archive, label) in [
        ("through.tar.gz", "经过链接"),
        ("self.tar.gz", "条目是链接"),
        ("dirlink.tar.gz", "目录条目是链接"),
    ] {
        let err = plan(&fx, &fx.path(archive), &dest).await.unwrap_err();
        match &err {
            ArchiveError::DestSymlink(path) => {
                println!("{label}：{err}");
                assert!(
                    path == "link/a.txt" || path == "fileLink" || path == "link/",
                    "{label}: {path}"
                );
            }
            other => panic!("{label}: 应该因目标里的符号链接被拒绝：{other:?}"),
        }
    }

    // zip：正常写法和 Windows 反斜杠写法
    let backslash = minimal_zip(&[("link\\a.txt", b"evil"), ("ok.txt", b"ok")]);
    fx.put(&fx.path("backslash-link.zip"), &backslash).await;
    let err = plan(&fx, &fx.path("backslash-link.zip"), &dest)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ArchiveError::DestSymlink("link/a.txt".to_owned()),
        "反斜杠写法也要认出链接"
    );
    if fx.caps.zip && fx.caps.unzip {
        let (code, out) = fx
            .sh(&format!(
                "cd '{base}/src' && zip -r -q ../through.zip ok.txt link 2>&1"
            ))
            .await;
        assert_eq!(code, Some(0), "造 zip 失败：{out}");
        let err = plan(&fx, &fx.path("through.zip"), &dest).await.unwrap_err();
        assert!(matches!(err, ArchiveError::DestSymlink(_)), "{err:?}");
    } else {
        println!("服务器没有 zip / unzip：只测了 tar.gz 和手写的 zip");
    }

    // 目录外什么都没多出来，目标里的两个链接也原样还在
    assert_eq!(names_in(&fx, &outside).await, before, "目录外多出了文件");
    assert!(fx.exists(&format!("{dest}/link")).await);
    assert!(fx.exists(&format!("{dest}/fileLink")).await);
    assert!(fx.leftovers(&base).await.is_empty());
    fx.finish().await;
}

/// Windows 打的 zip（路径用反斜杠）：检查能认出和目标里已有文件的冲突；选「跳过」时保留原文件，
/// 其余文件解到对应的子目录里；没有冲突时用「跳过」解压，检查之后才出现的同名文件也不会被覆盖。
#[tokio::test]
async fn backslash_zip_conflicts_are_detected_and_skip_never_overwrites_full() {
    if !common::full_live("backslash_zip_conflicts_are_detected_and_skip_never_overwrites_full") {
        return;
    }
    let _serial = serial().await;
    let Some(fx) = fixture("arch-backslash").await else {
        return;
    };
    if !(fx.caps.zip && fx.caps.unzip) {
        println!("跳过反斜杠 zip：服务器上没有 unzip");
        fx.finish().await;
        return;
    }
    let archive = fx.path("win.zip");
    fx.put(
        &archive,
        &minimal_zip(&[
            ("dir\\file.txt", b"from-zip-file"),
            ("dir\\sub\\new.txt", b"from-zip-new"),
            ("top.txt", b"from-zip-top"),
        ]),
    )
    .await;

    // 1) 目标里已有 dir/file.txt：必须报成冲突（以前按原样查 dir\file.txt，会漏报）
    let dest = fx.path("dest-conflict");
    fx.channel.create_dir(&dest).await.unwrap();
    fx.channel.create_dir(&format!("{dest}/dir")).await.unwrap();
    fx.put(&format!("{dest}/dir/file.txt"), b"LOCAL-file").await;
    let checked = plan(&fx, &archive, &dest).await.expect("检查 zip");
    println!("反斜杠 zip 检查结果：{checked:?}");
    assert_eq!(checked.total_files, 3);
    assert_eq!(checked.conflict_count, 1, "{checked:?}");
    assert_eq!(checked.conflict_sample, ["dir/file.txt"]);
    let done = run_extract(
        &fx,
        &checked,
        &archive,
        &dest,
        ConflictPolicy::SkipExisting,
        "livebs1",
    )
    .await
    .expect("跳过解压");
    println!(
        "跳过解压：解出 {}，跳过 {}，警告 {:?}",
        done.extracted, done.skipped, done.warning
    );
    assert_eq!(
        fx.read(&format!("{dest}/dir/file.txt")).await,
        b"LOCAL-file"
    );
    assert_eq!(
        fx.read(&format!("{dest}/dir/sub/new.txt")).await,
        b"from-zip-new"
    );
    assert_eq!(fx.read(&format!("{dest}/top.txt")).await, b"from-zip-top");

    // 2) 检查时没有冲突；检查之后、解压之前有人新建了同名文件：「跳过」不能覆盖它
    let dest2 = fx.path("dest-race");
    fx.channel.create_dir(&dest2).await.unwrap();
    let checked = plan(&fx, &archive, &dest2).await.expect("检查 zip");
    assert_eq!(checked.conflict_count, 0, "{checked:?}");
    fx.channel
        .create_dir(&format!("{dest2}/dir"))
        .await
        .unwrap();
    fx.put(&format!("{dest2}/dir/file.txt"), b"LATE-local")
        .await;
    run_extract(
        &fx,
        &checked,
        &archive,
        &dest2,
        ConflictPolicy::SkipExisting,
        "livebs2",
    )
    .await
    .expect("跳过解压");
    assert_eq!(
        fx.read(&format!("{dest2}/dir/file.txt")).await,
        b"LATE-local",
        "检查之后才出现的文件不能被覆盖"
    );
    assert_eq!(
        fx.read(&format!("{dest2}/dir/sub/new.txt")).await,
        b"from-zip-new"
    );

    // 3) 明确选「覆盖」才会换成包里的内容
    let dest3 = fx.path("dest-overwrite");
    fx.channel.create_dir(&dest3).await.unwrap();
    fx.channel
        .create_dir(&format!("{dest3}/dir"))
        .await
        .unwrap();
    fx.put(&format!("{dest3}/dir/file.txt"), b"LOCAL-file")
        .await;
    let checked = plan(&fx, &archive, &dest3).await.expect("检查 zip");
    assert_eq!(checked.conflict_count, 1);
    run_extract(
        &fx,
        &checked,
        &archive,
        &dest3,
        ConflictPolicy::Overwrite,
        "livebs3",
    )
    .await
    .expect("覆盖解压");
    assert_eq!(
        fx.read(&format!("{dest3}/dir/file.txt")).await,
        b"from-zip-file"
    );
    assert!(fx.leftovers(fx.remote.path()).await.is_empty());
    fx.finish().await;
}
