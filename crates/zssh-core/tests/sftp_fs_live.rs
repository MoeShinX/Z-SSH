//! SFTP 文件操作的线测试（M3-1 第 2 节）：列目录（含符号链接、非 UTF-8 名字）、
//! 属性、建目录、重命名（含目标已存在）、递归删除（进度 + 取消）、改权限、
//! 建空文件、小文本读写与上限。
//!
//! 前置条件：`ZSSH_TEST_*` 环境变量（没设置就跳过并打印原因）。
//! **所有远端操作都在 `/tmp/zssh-sftp-live-<随机串>` 里**，结束时整个删掉。

mod common;

use std::sync::Arc;

use zssh_core::sftp::fs::{self, DeleteProgress, RecursiveDelete};
use zssh_core::sftp::{session::SftpChannel, SftpError, SMALL_TEXT_LIMIT};

use common::{serial, TempKnownHosts, TestServer};

/// 建一个专用的临时目录（守卫：失败 / panic 也会删，M3-2 第 0 节）。
async fn temp_dir(
    server: &TestServer,
    connection: &Arc<zssh_core::ssh::Connection>,
    channel: &SftpChannel,
    tag: &str,
) -> common::RemoteTempDir {
    common::RemoteTempDir::create(server, connection, channel, tag).await
}

/// 用连接跑一条命令（测试里造符号链接 / 非 UTF-8 名字用；都在临时目录里）。
async fn exec(connection: &zssh_core::ssh::Connection, command: &str) -> String {
    connection.exec(command).await.unwrap_or_default()
}

#[tokio::test]
async fn file_operations_round_trip() {
    if !common::full_live("file_operations_round_trip") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 file_operations...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-fs");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "fs").await;

    // ---------- 建目录（多层）----------
    let nested = format!("{dir}/a/b");
    channel.create_dir(&format!("{dir}/a")).await.expect("建 a");
    channel.create_dir(&nested).await.expect("建 a/b");

    // ---------- 逐级建目录：已存在的前缀跳过，只建缺的（M3-2 实测补的）----------
    let deep = format!("{dir}/x/y/z");
    fs::create_dir_all(&channel, &deep)
        .await
        .expect("逐级建目录");
    assert!(fs::stat(&channel, &deep, true)
        .await
        .expect("读 z")
        .is_dir());
    let err = fs::create_dir_all(&channel, &deep)
        .await
        .expect_err("最后一级已存在要报错");
    assert!(matches!(err, SftpError::AlreadyExists { .. }), "{err}");
    // 中间一级是普通文件：报错，不往下建。
    let blocker = format!("{dir}/blocker");
    channel.create_file(&blocker).await.expect("建挡路的文件");
    let err = fs::create_dir_all(&channel, &format!("{blocker}/child"))
        .await
        .expect_err("中间是文件要报错");
    assert!(err.to_string().contains("不是目录"), "{err}");

    // ---------- 建空文件 + 读属性 ----------
    let empty = format!("{nested}/empty.txt");
    channel.create_file(&empty).await.expect("建空文件");
    let info = fs::stat(&channel, &empty, true).await.expect("读属性");
    assert_eq!(info.size, Some(0), "空文件大小是 0");
    assert!(!info.is_dir());

    // ---------- 小文本读写 ----------
    let text_path = format!("{nested}/note.txt");
    let written = fs::write_small_text(
        &channel,
        &text_path,
        "第一行\nsecond line\n",
        SMALL_TEXT_LIMIT,
    )
    .await
    .expect("写小文本");
    let read = fs::read_small_text(&channel, &text_path, SMALL_TEXT_LIMIT)
        .await
        .expect("读小文本");
    assert_eq!(read.text, "第一行\nsecond line\n");
    assert!(read.utf8);
    assert_eq!(written, read.size);

    // ---------- 上限：超过就报错，不截断 ----------
    let big = format!("{nested}/big.txt");
    fs::write_small_text(&channel, &big, "0123456789", SMALL_TEXT_LIMIT)
        .await
        .expect("先写个正常文件");
    let err = fs::read_small_text(&channel, &big, 4)
        .await
        .expect_err("超过上限要报错");
    assert!(matches!(err, SftpError::TooLarge { .. }), "{err}");
    assert!(err.to_string().contains("超过"), "{err}");
    // 写也同理。
    let err = fs::write_small_text(&channel, &big, "0123456789", 4)
        .await
        .expect_err("写超过上限也要报错");
    assert!(matches!(err, SftpError::TooLarge { .. }), "{err}");
    // 报错之后原文件不该被动过（没截断也没写一半）。
    let read = fs::read_small_text(&channel, &big, SMALL_TEXT_LIMIT)
        .await
        .expect("再读一次");
    assert_eq!(read.text, "0123456789", "写失败不能把文件截断");

    // ---------- 列目录：类型、排序（目录在前）----------
    let entries = channel.read_dir(dir.path()).await.expect("列目录");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    println!("{dir} 的内容：{names:?}");
    assert_eq!(names.first().copied(), Some("a"), "目录排前面");
    assert!(entries[0].kind.is_dir());
    assert!(entries.iter().all(|e| e.name_is_utf8), "这些名字都是 UTF-8");

    // ---------- 改权限（0640）----------
    let script = format!("{nested}/run.sh");
    channel.create_file(&script).await.expect("建脚本");
    channel
        .set_permissions(&script, 0o640)
        .await
        .expect("改权限");
    let info = fs::stat(&channel, &script, true).await.expect("读属性");
    let mode = info.permissions.expect("要有权限位") & 0o777;
    println!("改完权限：{mode:o}");
    assert_eq!(mode, 0o640);

    // ---------- 重命名：目标不存在 ----------
    let renamed = format!("{nested}/renamed.txt");
    let outcome = channel
        .rename(&text_path, &renamed, false)
        .await
        .expect("重命名");
    println!("重命名（目标不存在）走了：{outcome:?}");
    assert!(
        fs::stat(&channel, &text_path, true).await.is_err(),
        "旧名字不该还在"
    );
    assert!(fs::stat(&channel, &renamed, true).await.is_ok());

    // ---------- 重命名：目标已存在（要走 posix-rename 或先删后改名）----------
    let target = format!("{nested}/target.txt");
    fs::write_small_text(&channel, &target, "旧内容", SMALL_TEXT_LIMIT)
        .await
        .expect("写目标文件");
    let outcome = channel
        .rename(&renamed, &target, true)
        .await
        .expect("覆盖式重命名");
    println!("重命名（目标已存在）走了：{outcome:?}");
    let read = fs::read_small_text(&channel, &target, SMALL_TEXT_LIMIT)
        .await
        .expect("读覆盖后的文件");
    assert_eq!(read.text, "第一行\nsecond line\n", "目标应当是源文件的内容");

    // ---------- 重命名：不覆盖时目标已存在必须失败（R-6b）----------
    // posix-rename 是 rename(2) 语义，目标存在会直接替换；普通 SSH_FXP_RENAME 才会失败。
    let keep = format!("{nested}/keep.txt");
    let mover = format!("{nested}/mover.txt");
    fs::write_small_text(&channel, &keep, "原来的内容", SMALL_TEXT_LIMIT)
        .await
        .expect("写已存在的目标");
    fs::write_small_text(&channel, &mover, "要改名的内容", SMALL_TEXT_LIMIT)
        .await
        .expect("写源文件");
    let refused = channel.rename(&mover, &keep, false).await;
    println!("不覆盖改名遇到已存在目标：{refused:?}");
    let err = refused.expect_err("目标已存在，不覆盖的改名必须失败");
    assert!(
        matches!(err, SftpError::AlreadyExists { .. }),
        "要报成\"已经存在\"，界面才能弹覆盖确认：{err:?}"
    );
    let kept = fs::read_small_text(&channel, &keep, SMALL_TEXT_LIMIT)
        .await
        .expect("读目标");
    assert_eq!(kept.text, "原来的内容", "目标被盖掉了");
    assert!(
        fs::stat(&channel, &mover, true).await.is_ok(),
        "失败后源文件必须还在"
    );
    // 覆盖模式照旧成功。
    channel
        .rename(&mover, &keep, true)
        .await
        .expect("覆盖式改名");
    let replaced = fs::read_small_text(&channel, &keep, SMALL_TEXT_LIMIT)
        .await
        .expect("读覆盖后的文件");
    assert_eq!(replaced.text, "要改名的内容");

    // ---------- 递归删除：进度 + 计数 ----------
    let mut progress: Vec<DeleteProgress> = Vec::new();
    let deleter = RecursiveDelete::new();
    let deleted = deleter
        .run(&channel, dir.path(), |event| progress.push(event))
        .await
        .expect("递归删除");
    println!("递归删掉了 {deleted} 项，进度事件 {} 条", progress.len());
    assert!(deleted >= 5, "至少有 a、a/b、3 个文件");
    assert_eq!(progress.len() as u64, deleted, "每删一项报一次进度");
    assert!(matches!(
        fs::stat(&channel, dir.path(), true).await,
        Err(SftpError::NotFound { .. })
    ));

    // 递归删除已经把临时目录删掉了（上面的断言），告诉守卫别再删一遍。
    dir.mark_done();
    channel.close();
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn recursive_delete_can_be_cancelled_midway() {
    if !common::full_live("recursive_delete_can_be_cancelled_midway") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 delete_cancel...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-fs-cancel");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "cancel").await;

    // 造 20 个文件：删到第 3 个就取消。
    for index in 0..20 {
        channel
            .create_file(&format!("{dir}/file-{index:02}.txt"))
            .await
            .expect("建文件");
    }
    let deleter = RecursiveDelete::new();
    let handle = deleter.cancel_handle();
    let mut seen = 0u64;
    let result = deleter
        .run(&channel, dir.path(), |_event| {
            seen += 1;
            if seen == 3 {
                handle.cancel();
            }
        })
        .await;
    println!("取消前删了 {seen} 项，结果：{result:?}");
    assert!(matches!(result, Err(SftpError::Cancelled)), "{result:?}");
    assert_eq!(seen, 3, "取消之后不再继续删");
    // 目录还在（没删完），剩下的文件也还在。
    let entries = channel.read_dir(dir.path()).await.expect("列目录");
    assert_eq!(entries.len(), 17, "还剩 17 个");

    dir.cleanup(&connection, &channel).await;
    channel.close();
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn symlinks_are_listed_with_their_target_kind() {
    if !common::full_live("symlinks_are_listed_with_their_target_kind") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 symlink...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-fs-symlink");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "symlink").await;

    // 一个真文件、一个指向它的链接、一个指向目录的链接、一个断链（都用 shell 造）。
    fs::write_small_text(&channel, &format!("{dir}/real.txt"), "x", SMALL_TEXT_LIMIT)
        .await
        .expect("写文件");
    channel
        .create_dir(&format!("{dir}/sub"))
        .await
        .expect("建目录");
    let created = exec(
        &connection,
        &format!(
            "cd {dir} && ln -s real.txt link-file && ln -s sub link-dir && ln -s missing.txt link-broken && echo OK"
        ),
    )
    .await;
    assert!(created.contains("OK"), "造符号链接失败：{created}");

    let entries = channel.read_dir(dir.path()).await.expect("列目录");
    for entry in &entries {
        println!(
            "{}：{}（目标 {:?}，链接到 {:?}）",
            entry.name,
            entry.kind.label(),
            entry.link_target,
            entry.kind
        );
    }
    let by_name = |name: &str| entries.iter().find(|e| e.name == name).expect(name);
    assert!(
        by_name("link-file").kind.is_symlink(),
        "链接本身要显示成符号链接"
    );
    assert_eq!(
        by_name("link-file").link_target.as_deref(),
        Some("real.txt")
    );
    assert!(
        !by_name("link-file").kind.is_dir(),
        "指向文件的链接不是目录"
    );
    assert!(
        by_name("link-dir").kind.is_dir(),
        "指向目录的链接要能取到目标是目录"
    );
    assert!(
        !by_name("link-broken").kind.is_dir(),
        "断链不该被当成目录（目标是 None）"
    );
    assert_eq!(
        by_name("link-broken").link_target.as_deref(),
        Some("missing.txt")
    );

    // `follow = false` 读链接本身；`follow = true` 读目标。
    let link_stat = fs::stat(&channel, &format!("{dir}/link-file"), false)
        .await
        .expect("读链接本身");
    assert!(link_stat.kind.is_symlink());
    let target_stat = fs::stat(&channel, &format!("{dir}/link-file"), true)
        .await
        .expect("跟随链接");
    assert_eq!(target_stat.kind, fs::FileKind::File);

    dir.cleanup(&connection, &channel).await;
    channel.close();
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn non_utf8_names_are_listed_but_not_operated_on() {
    if !common::full_live("non_utf8_names_are_listed_but_not_operated_on") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 non_utf8...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-fs-gbk");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "gbk").await;

    // 造一个 GBK 名字的文件（0xB1 0xA8 = "报"）：printf 的八进制转义。
    let created = exec(
        &connection,
        &format!("cd {dir} && touch \"$(printf '\\261\\250.txt')\" && echo OK"),
    )
    .await;
    assert!(created.contains("OK"), "造 GBK 名字失败：{created}");

    let entries = channel.read_dir(dir.path()).await.expect("列目录");
    let gbk = entries
        .iter()
        .find(|e| !e.name_is_utf8)
        .expect("要认出这个非 UTF-8 名字");
    println!(
        "非 UTF-8 条目：名字显示成 {:?}，name_is_utf8={}",
        gbk.name, gbk.name_is_utf8
    );
    assert!(gbk.name.contains('\u{FFFD}'), "显示时是有损的");

    // 操作它：**明确报错**，而不是拿有损名字去删/改（那会把服务器上的名字改坏）。
    let err = channel
        .remove_file(&gbk.path)
        .await
        .expect_err("不能拿有损名字去删");
    println!("非 UTF-8 名字上做操作：{err}");
    assert!(matches!(err, SftpError::NonUtf8Name { .. }), "{err}");
    let err = channel
        .rename(&gbk.path, &format!("{dir}/new.txt"), false)
        .await
        .expect_err("不能拿有损名字去改名");
    assert!(matches!(err, SftpError::NonUtf8Name { .. }), "{err}");

    // 服务器上的原名没被碰过（还在，字节没变）。
    let listing = exec(&connection, &format!("cd {dir} && ls -b")).await;
    println!("服务器上实际的字节：{}", listing.trim());
    assert!(
        listing.contains("\\261\\250.txt"),
        "原文件还在且字节没变：{listing}"
    );

    // 用 shell 收尾（我们有原始字节，客户端没有）；守卫不知道 shell 删没删，
    // 显式 mark_done，Drop 里的兜底就不会再跑一遍（GBK 名字它删不动）。
    exec(
        &connection,
        &format!("rm -f {dir}/*.txt && rmdir {dir} && echo CLEAN"),
    )
    .await;
    dir.mark_done();
    channel.close();
    let _ = connection.disconnect().await;
}

/// R-6c R02：服务器没有 `posix-rename` 时，覆盖改名失败不能把目标删掉。
/// 这台测试服务器是有这个扩展的，这里人为把它屏蔽掉，走"没有扩展"的降级路径。
#[tokio::test]
async fn overwrite_rename_without_posix_rename_never_loses_the_target() {
    if !common::full_live("overwrite_rename_without_posix_rename_never_loses_the_target") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 overwrite_rename_without_posix_rename...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-r02");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let mut channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "r02").await;
    assert!(
        channel.caps().has(zssh_core::sftp::session::POSIX_RENAME),
        "前提：测试服务器有 posix-rename，不然这个屏蔽没有意义"
    );
    channel.hide_extension_for_tests(zssh_core::sftp::session::POSIX_RENAME);
    assert!(!channel.caps().has(zssh_core::sftp::session::POSIX_RENAME));

    let target = format!("{dir}/keep.txt");
    let fresh = format!("{dir}/fresh.txt");
    let missing = format!("{dir}/does-not-exist.txt");
    fs::write_small_text(
        &channel,
        &target,
        "旧的完整内容
",
        SMALL_TEXT_LIMIT,
    )
    .await
    .expect("写目标");
    fs::write_small_text(
        &channel,
        &fresh,
        "新的完整内容
",
        SMALL_TEXT_LIMIT,
    )
    .await
    .expect("写新文件");

    // 源文件不存在：改名一定失败。旧实现会先把目标删掉再失败。
    let failed = channel.rename(&missing, &target, true).await;
    println!("源不存在时的覆盖改名：{failed:?}");
    assert!(failed.is_err());
    let kept = fs::read_small_text(&channel, &target, SMALL_TEXT_LIMIT)
        .await
        .expect("失败之后目标必须还在");
    assert_eq!(
        kept.text,
        "旧的完整内容
",
        "目标被动了"
    );

    // 成功路径：降级走三步，结果正确，没有留下备用文件。
    let outcome = channel
        .rename(&fresh, &target, true)
        .await
        .expect("覆盖改名");
    println!("没有 posix-rename 的覆盖改名走了：{outcome:?}");
    let replaced = fs::read_small_text(&channel, &target, SMALL_TEXT_LIMIT)
        .await
        .expect("读覆盖后的文件");
    assert_eq!(
        replaced.text,
        "新的完整内容
"
    );
    assert!(
        fs::stat(&channel, &fresh, true).await.is_err(),
        "源文件应该没了"
    );
    let leftovers = exec(&connection, &format!("ls -A '{dir}'")).await;
    println!("目录里剩下：{leftovers:?}");
    assert!(
        !leftovers.contains("zssh-old"),
        "备用文件应该已经删掉：{leftovers}"
    );
}

/// C07：编辑冲突里选"放弃本地修改"时，远端已经变得超过编辑器上限：
/// 要按上限拒绝（不能整个读进内存），本地临时文件原样保留，冲突还挂着。
/// 远端大文件用 `truncate -s` 造成稀疏文件，不占服务器磁盘、也不走网络传输。
#[tokio::test]
async fn discarding_local_edits_refuses_a_remote_file_over_the_editor_limit() {
    use zssh_core::sftp::edit::{self, EditOutcome, EditSession, Resolution, MAX_EDIT_BYTES};

    if !common::full_live("discarding_local_edits_refuses_a_remote_file_over_the_editor_limit") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 discarding_local_edits...：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("sftp-edit-discard");
    let connection = Arc::new(server.connect(&known_hosts).await.expect("连接测试服务器"));
    let channel = common::open_sftp(&connection).await.expect("开 SFTP 会话");
    let dir = temp_dir(&server, &connection, &channel, "edit-discard").await;
    let remote = format!("{dir}/a.txt");
    fs::write_small_text(&channel, &remote, "old\n", SMALL_TEXT_LIMIT)
        .await
        .expect("写远端文件");

    let (data, base) = edit::download(&channel, &remote, MAX_EDIT_BYTES)
        .await
        .expect("打开");
    let local_dir = tempfile::Builder::new()
        .prefix("zssh-edit-discard-")
        .tempdir()
        .expect("本地临时目录");
    let local = local_dir.path().join("a.txt");
    std::fs::write(&local, &data).expect("写本地临时文件");
    let mut session = EditSession::new(remote.clone(), local.clone(), base);

    // 别人把远端撑大到超过上限；本地这边改了内容并保存。
    let grown = exec(
        &connection,
        &format!("truncate -s {} '{remote}' && echo OK", MAX_EDIT_BYTES * 4),
    )
    .await;
    assert!(grown.contains("OK"), "撑大远端文件失败：{grown}");
    std::fs::write(&local, b"mine\n").expect("本地保存");
    assert!(matches!(
        session.on_local_saved(&channel).await,
        EditOutcome::Conflict { .. }
    ));

    match session.resolve(&channel, Resolution::Discard).await {
        EditOutcome::Failed(reason) => assert!(reason.contains("上限"), "{reason}"),
        other => panic!("超过上限要拒绝：{other:?}"),
    }
    assert_eq!(std::fs::read(&local).expect("读本地"), b"mine\n");
    assert!(session.has_pending_problem(), "冲突还要挂着");

    dir.cleanup(&connection, &channel).await;
    channel.close();
    let _ = connection.disconnect().await;
}
