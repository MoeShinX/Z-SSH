//! F05：编辑保存的上传不能在中途出错时把远端原文件弄残。

use std::sync::Arc;

use super::*;
use crate::ai::files::testfs::MemFs;
use crate::ai::files::RemoteFs;

/// 假远端 = 内存文件系统；上传走生产用的 `upload_via_fs`。
struct FsBackend(Arc<MemFs>);

impl EditBackend for FsBackend {
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        Box::pin(async move {
            match self.0.stat(path).await.map_err(SftpError::Remote)? {
                Some(stat) => Ok((
                    RemoteSnapshot {
                        size: stat.size,
                        mtime: stat.mtime,
                    },
                    false,
                )),
                None => Err(SftpError::NotFound {
                    path: path.to_owned(),
                }),
            }
        })
    }

    fn read_all<'a>(
        &'a self,
        path: &'a str,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(async move {
            self.0
                .read_all(path, limit)
                .await
                .map_err(SftpError::Remote)
        })
    }

    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            upload_via_fs(self.0.as_ref(), path, data, "t0", OwnerMode::Allow)
                .await
                .map(|_| ())
                .map_err(|err| match err {
                    OverwriteError::Failed(err) => err,
                    OverwriteError::OwnerWouldChange(change) => {
                        SftpError::Remote(format!("属主会改变：{change:?}"))
                    }
                })
        })
    }

    fn overwrite<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: OwnerMode,
    ) -> BoxFuture<'a, Result<OverwriteReport, OverwriteError>> {
        Box::pin(async move {
            if mode == OwnerMode::InPlace {
                // 直接写原文件：和真实的 SFTP 一样，截断重写，属主和权限不变。
                return overwrite_in_place(self, path, data).await;
            }
            upload_via_fs(self.0.as_ref(), path, data, "t0", mode).await
        })
    }

    fn server_identity(&self) -> Option<String> {
        // 显式走 RemoteFs：直接 `self.0.server_key()` 会命中 Arc 上的 EditBackend 转发（递归）。
        crate::ai::files::RemoteFs::server_key(self.0.as_ref())
    }

    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            crate::ai::files::RemoteFs::realpath(self.0.as_ref(), path)
                .await
                .ok()
        })
    }

    fn write_in_place<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            crate::ai::files::RemoteFs::write_file(self.0.as_ref(), path, data, None)
                .await
                .map_err(SftpError::Remote)
        })
    }
}

const REMOTE: &str = "/home/u/app.conf";
const ORIGINAL: &[u8] = b"ORIGINAL CONTENT THAT MUST SURVIVE\n";
const EDITED: &[u8] = b"EDITED CONTENT\n";

struct Fixture {
    fs: Arc<MemFs>,
    backend: FsBackend,
    session: EditSession,
    _dir: crate::test_util::TestDir,
}

async fn fixture() -> Fixture {
    let fs = Arc::new(MemFs::new());
    fs.put(REMOTE, ORIGINAL, 0o640);
    fs.put_owner(REMOTE, 1001, 1002);
    let backend = FsBackend(Arc::clone(&fs));
    let dir = crate::test_util::TestDir::new("edit-upload");
    let local = dir.join("app.conf");
    std::fs::write(&local, EDITED).unwrap();
    let (base, _) = backend.stat(REMOTE).await.unwrap();
    let session = EditSession::new(REMOTE.to_owned(), local, base);
    Fixture {
        fs,
        backend,
        session,
        _dir: dir,
    }
}

#[tokio::test]
async fn a_failure_halfway_through_the_upload_leaves_the_remote_file_untouched() {
    let mut fx = fixture().await;
    *fx.fs.half_write_containing.lock().unwrap() = Some("app.conf".to_owned());
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(matches!(outcome, EditOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(
        fx.fs.get(REMOTE).unwrap(),
        ORIGINAL,
        "上传中途断线，远端原文件被弄残了"
    );
    assert!(fx.session.has_pending_problem(), "失败要记着，界面才会提示");
    assert_eq!(
        fx.fs.names_in("/home/u"),
        vec!["/home/u/app.conf".to_owned()],
        "不能留下临时文件"
    );
}

#[tokio::test]
async fn a_failed_rename_leaves_the_remote_file_untouched_and_no_temp_behind() {
    let mut fx = fixture().await;
    *fx.fs.fail_rename.lock().unwrap() = true;
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(matches!(outcome, EditOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL);
    assert_eq!(
        fx.fs.names_in("/home/u"),
        vec!["/home/u/app.conf".to_owned()]
    );
}

#[tokio::test]
async fn a_failed_temp_write_leaves_the_remote_file_untouched() {
    let mut fx = fixture().await;
    *fx.fs.fail_write_containing.lock().unwrap() = Some("app.conf".to_owned());
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(matches!(outcome, EditOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL);
}

#[tokio::test]
async fn a_good_upload_replaces_the_content_and_keeps_mode_and_owner() {
    let mut fx = fixture().await;
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
    assert_eq!(fx.fs.mode_of(REMOTE), Some(0o640), "权限要保持");
    assert_eq!(fx.fs.owner_of(REMOTE), Some((1001, 1002)), "属主要保持");
    assert_eq!(
        fx.fs.names_in("/home/u"),
        vec!["/home/u/app.conf".to_owned()]
    );
}

#[tokio::test]
async fn save_as_a_new_name_is_also_written_atomically() {
    let mut fx = fixture().await;
    *fx.fs.fail_rename.lock().unwrap() = true;
    let outcome = fx
        .session
        .resolve(&fx.backend, Resolution::SaveAs("copy.conf".to_owned()))
        .await;
    assert!(matches!(outcome, EditOutcome::Failed(_)), "{outcome:?}");
    assert!(fx.fs.get("/home/u/copy.conf").is_none(), "不能留半截新文件");
    assert_eq!(
        fx.fs.names_in("/home/u"),
        vec!["/home/u/app.conf".to_owned()]
    );
}

// ---- 崩溃之后重新上传 ----

use super::recovery::{self, EditMeta};

/// 造一份"上次崩溃留下的、没上传的修改"：本地文件是用户改过的内容。
fn crashed_edit(root: &std::path::Path, remote_base: RemoteSnapshot) -> recovery::Leftover {
    let dir = root.join("e1");
    std::fs::create_dir_all(recovery::content_dir(&dir)).unwrap();
    recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: REMOTE.to_owned(),
            host_id: 1,
            host_label: "测试主机".to_owned(),
            file_name: "app.conf".to_owned(),
            link_target: None,
            base_size: remote_base.size,
            base_mtime: remote_base.mtime,
            clean_sha256: recovery::sha256_hex(ORIGINAL),
            server_identity: None,
            local: false,
        },
    )
    .unwrap();
    std::fs::write(recovery::content_path(&dir, "app.conf"), EDITED).unwrap(); // 用户改了、没来得及上传
    let found = recovery::scan(root);
    assert_eq!(found.len(), 1, "重启之后修改还在");
    found.into_iter().next().unwrap()
}

#[tokio::test]
async fn after_a_crash_the_edit_can_be_reuploaded_when_the_remote_is_unchanged() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("edit-reupload");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = crashed_edit(&root, base);

    let outcome = recovery::reupload(&leftover, &fx.backend, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
}

#[tokio::test]
async fn after_a_crash_a_remote_that_changed_meanwhile_is_a_conflict_not_an_overwrite() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("edit-reupload-conflict");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = crashed_edit(&root, base);
    // 别人在这期间改了服务器上的文件。
    fx.fs.put(
        REMOTE,
        b"SOMEONE ELSE CHANGED THIS FILE
",
        0o640,
    );

    let outcome = recovery::reupload(&leftover, &fx.backend, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EditOutcome::Conflict { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fx.fs.get(REMOTE).unwrap(),
        b"SOMEONE ELSE CHANGED THIS FILE
"
    );
    assert!(leftover.local_path.exists(), "没传上去，本地修改必须还在");

    // 用户看过之后选了覆盖。
    let outcome = recovery::reupload(&leftover, &fx.backend, true)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
}

#[tokio::test]
async fn a_leftover_without_a_record_cannot_be_reuploaded() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("edit-reupload-unknown");
    let dir = root.join("e2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mystery.txt"), b"?").unwrap();
    let leftover = recovery::scan(&root).into_iter().next().unwrap();
    assert!(recovery::reupload(&leftover, &fx.backend, false)
        .await
        .is_err());
}

/// N02：恢复对话框里"重新上传"，上传期间本地文件又变了：记录写的是传上去的那份，新的修改仍算未上传。
struct SavesAgainWhileUploading {
    inner: FsBackend,
    local: std::path::PathBuf,
}

impl EditBackend for SavesAgainWhileUploading {
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        self.inner.stat(path)
    }

    fn read_all<'a>(
        &'a self,
        path: &'a str,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>> {
        self.inner.read_all(path, limit)
    }

    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.inner.write_all(path, data).await?;
            // 内容已经传完，记录还没更新：这时用户在编辑器里又保存了一次。
            std::fs::write(&self.local, b"SAVED AGAIN DURING THE UPLOAD, longer").unwrap();
            Ok(())
        })
    }
}

#[tokio::test]
async fn reupload_records_the_uploaded_bytes_even_if_the_file_changed_meanwhile() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("edit-reupload-changed");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = crashed_edit(&root, base);
    let backend = SavesAgainWhileUploading {
        inner: FsBackend(Arc::clone(&fx.fs)),
        local: leftover.local_path.clone(),
    };

    let outcome = recovery::reupload(&leftover, &backend, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
    let meta = recovery::read_meta(&leftover.dir).unwrap();
    assert_eq!(meta.clean_sha256, recovery::sha256_hex(EDITED));
    assert!(
        recovery::has_unsent_changes(&leftover.dir, &meta),
        "上传期间的新保存不能被当成已上传"
    );
    assert_eq!(recovery::scan(&root).len(), 1, "重启后新的修改还在");
}

/// R-6c R01：编辑保存的上传和传输队列共用同一把远端目标锁。
#[tokio::test]
async fn an_edit_upload_is_refused_while_a_transfer_is_writing_the_same_remote_file() {
    use crate::sftp::transfer::lock::TargetLocks;
    let mut fx = fixture().await;
    *fx.fs.server_key.lock().unwrap() = Some("edit-tester@lock-host:22".to_owned());
    let transfer = TargetLocks::remote()
        .try_acquire_remote("edit-tester@lock-host:22", REMOTE)
        .unwrap();

    let outcome = fx.session.on_local_saved(&fx.backend).await;
    let EditOutcome::Failed(message) = &outcome else {
        panic!("应该因为目标被占用而失败：{outcome:?}");
    };
    assert!(message.contains("正在写"), "{message}");
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL, "被拒绝时远端不能变");
    assert!(fx.session.has_pending_problem(), "失败要记着，界面才会提示");

    drop(transfer);
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
}

// ---- N05：属主改不回来时不能只说"已上传" ----

use crate::ai::files::OwnerChange;

/// 登录用户（1000/1000）有目录写权限，但没权限把属主改成服务账号（1001/1002）。
async fn fixture_without_chown_permission() -> Fixture {
    let fx = fixture().await;
    *fx.fs.fail_set_owner.lock().unwrap() = true;
    fx
}

fn expect_prompt(outcome: &EditOutcome) -> &OwnerChange {
    match outcome {
        EditOutcome::OwnerWouldChange(change) => change,
        other => panic!("应该返回『属主会改变』让用户选，实际：{other:?}"),
    }
}

#[tokio::test]
async fn when_the_owner_cannot_be_kept_nothing_is_replaced_until_the_user_decides() {
    let mut fx = fixture_without_chown_permission().await;
    let outcome = fx.session.on_local_saved(&fx.backend).await;

    let change = expect_prompt(&outcome);
    assert_eq!(change.from, "uid 1001 / gid 1002");
    assert_eq!(change.to, "uid 1000 / gid 1000");
    assert_eq!(
        fx.fs.get(REMOTE).unwrap(),
        ORIGINAL,
        "还没问用户，远端文件就被替换了"
    );
    assert_eq!(fx.fs.owner_of(REMOTE), Some((1001, 1002)));
    assert_eq!(
        fx.fs.names_in("/home/u"),
        vec!["/home/u/app.conf".to_owned()],
        "不能留下临时文件"
    );
    assert!(fx.session.has_pending_problem(), "这次保存还没落到远端");
}

#[tokio::test]
async fn choosing_to_save_anyway_replaces_the_file_and_reports_the_owner_change() {
    let mut fx = fixture_without_chown_permission().await;
    let _ = fx.session.on_local_saved(&fx.backend).await;

    let outcome = fx
        .session
        .resolve(&fx.backend, Resolution::KeepNewOwner)
        .await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
    assert_eq!(fx.fs.owner_of(REMOTE), Some((1000, 1000)), "属主确实变了");
    let change = fx.session.owner_change().expect("结果里要带着属主变化");
    assert_eq!(change.from, "uid 1001 / gid 1002");
    assert_eq!(change.to, "uid 1000 / gid 1000");
    assert!(!fx.session.has_pending_problem());
}

#[tokio::test]
async fn choosing_to_write_in_place_keeps_the_owner() {
    let mut fx = fixture_without_chown_permission().await;
    let _ = fx.session.on_local_saved(&fx.backend).await;

    let outcome = fx.session.resolve(&fx.backend, Resolution::InPlace).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
    assert_eq!(
        fx.fs.owner_of(REMOTE),
        Some((1001, 1002)),
        "原地写入属主不变"
    );
    assert_eq!(fx.fs.mode_of(REMOTE), Some(0o640));
    assert!(fx.session.owner_change().is_none());
}

/// R-8 第 4 节：选「直接写入原文件」也要拿传输队列同一把进程级远端目标锁。
#[tokio::test]
async fn in_place_write_is_refused_while_a_transfer_is_writing_the_same_remote_file() {
    use crate::sftp::transfer::lock::TargetLocks;
    let mut fx = fixture_without_chown_permission().await;
    *fx.fs.server_key.lock().unwrap() = Some("edit-inplace@lock-host:22".to_owned());
    // 先走到属主询问，用户才能选「直接写入原文件」。
    let _ = fx.session.on_local_saved(&fx.backend).await;

    let transfer = TargetLocks::remote()
        .try_acquire_remote("edit-inplace@lock-host:22", REMOTE)
        .expect("测试锁");
    let outcome = fx.session.resolve(&fx.backend, Resolution::InPlace).await;
    let EditOutcome::Failed(message) = &outcome else {
        panic!("目标被占时「直接写入原文件」应该被拒绝：{outcome:?}");
    };
    assert!(message.contains("正在写"), "{message}");
    assert_eq!(
        fx.fs.get(REMOTE).unwrap(),
        ORIGINAL,
        "被拒绝时远端内容不能变"
    );
    assert!(fx.session.has_pending_problem(), "被拒的修改仍算没上传");

    // 上传任务结束以后可以正常原地写，属主不变。
    drop(transfer);
    let outcome = fx.session.resolve(&fx.backend, Resolution::InPlace).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
    assert_eq!(fx.fs.owner_of(REMOTE), Some((1001, 1002)));
}

/// 锁按真实路径算：从符号链接写入也要和直接写目标用同一把锁，而且不换掉链接本身。
#[tokio::test]
async fn in_place_write_locks_the_link_target_not_the_link_path() {
    use crate::sftp::transfer::lock::TargetLocks;
    const LINK: &str = "/home/u/link.conf";
    let fx = fixture().await;
    fx.fs
        .links
        .lock()
        .unwrap()
        .insert(LINK.to_owned(), REMOTE.to_owned());
    let backend = FsBackend(Arc::clone(&fx.fs));
    let (base, _) = backend.stat(LINK).await.unwrap();
    let dir = crate::test_util::TestDir::new("edit-inplace-link");
    let local = dir.join("link.conf");
    std::fs::write(&local, EDITED).unwrap();
    let mut session = EditSession::new(LINK.to_owned(), local, base);
    *fx.fs.server_key.lock().unwrap() = Some("edit-link@lock-host:22".to_owned());

    let transfer = TargetLocks::remote()
        .try_acquire_remote("edit-link@lock-host:22", REMOTE)
        .expect("测试锁");
    let outcome = session.resolve(&backend, Resolution::InPlace).await;
    assert!(
        matches!(outcome, EditOutcome::Failed(_)),
        "锁按真实路径算，从链接写也要被拒：{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL, "被拒绝时目标不能变");
    assert_eq!(
        fx.fs.links.lock().unwrap().get(LINK).cloned(),
        Some(REMOTE.to_owned()),
        "链接本身不能被换掉"
    );

    drop(transfer);
    let outcome = session.resolve(&backend, Resolution::InPlace).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), EDITED);
}

#[tokio::test]
async fn cancelling_leaves_the_remote_file_alone_and_the_edit_unsent() {
    let mut fx = fixture_without_chown_permission().await;
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    expect_prompt(&outcome);
    // 用户点了"取消"：界面什么都不发给后端。远端不变，这次修改仍算没上传。
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL);
    assert!(fx.session.has_pending_problem());
    // 之后再保存一次，还会再问（不会悄悄替换）。
    let again = fx.session.on_local_saved(&fx.backend).await;
    expect_prompt(&again);
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL);
}

#[tokio::test]
async fn a_chown_that_reports_success_but_does_not_take_effect_is_caught_before_replacing() {
    let mut fx = fixture().await;
    *fx.fs.ignore_set_owner.lock().unwrap() = true; // setstat 返回成功，服务器没认
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    let change = expect_prompt(&outcome);
    assert!(change.error.contains("仍然不一致"), "{change:?}");
    assert_eq!(fx.fs.get(REMOTE).unwrap(), ORIGINAL);
}

#[tokio::test]
async fn a_server_account_that_can_chown_keeps_working_without_a_prompt() {
    let mut fx = fixture().await; // 默认 set_owner 可用
    let outcome = fx.session.on_local_saved(&fx.backend).await;
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.owner_of(REMOTE), Some((1001, 1002)));
    assert!(fx.session.owner_change().is_none());
}

#[tokio::test]
async fn the_remote_changing_while_the_prompt_is_open_becomes_a_conflict() {
    let mut fx = fixture_without_chown_permission().await;
    let _ = fx.session.on_local_saved(&fx.backend).await;
    // 用户看提示的时候，别人改了服务器上的文件。
    fx.fs.put(REMOTE, b"someone else wrote this\n", 0o640);
    fx.fs.put_owner(REMOTE, 1001, 1002);
    let outcome = fx
        .session
        .resolve(&fx.backend, Resolution::KeepNewOwner)
        .await;
    assert!(
        matches!(outcome, EditOutcome::Conflict { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), b"someone else wrote this\n");
}

// ---- M3-5：内置编辑器的草稿，和本地文件的恢复 ----

use super::recovery::LocalRestore;

const DRAFT: &[u8] = b"UNSAVED DRAFT TYPED IN THE BUILT-IN EDITOR\n";

/// 内置编辑器：content/ 是最近一次和远端对齐的内容（和记录一致），草稿是还没保存的内容。
fn editor_session_with_draft(
    root: &std::path::Path,
    remote_base: RemoteSnapshot,
    local: bool,
    target: &str,
) -> recovery::Leftover {
    let dir = root.join("e1");
    std::fs::create_dir_all(recovery::content_dir(&dir)).unwrap();
    std::fs::write(recovery::content_path(&dir, "app.conf"), ORIGINAL).unwrap();
    recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: target.to_owned(),
            host_id: if local { 0 } else { 1 },
            host_label: if local { "本机" } else { "测试主机" }.to_owned(),
            file_name: "app.conf".to_owned(),
            link_target: None,
            base_size: remote_base.size,
            base_mtime: remote_base.mtime,
            clean_sha256: recovery::sha256_hex(ORIGINAL),
            server_identity: None,
            local,
        },
    )
    .unwrap();
    recovery::write_draft(&dir, "app.conf", DRAFT, 1).unwrap();
    let found = recovery::scan(root);
    assert_eq!(found.len(), 1, "有草稿就必须留下来");
    found.into_iter().next().unwrap()
}

#[tokio::test]
async fn a_draft_counts_as_an_unsent_change_and_survives_a_restart() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("draft-restart");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = editor_session_with_draft(&root, base, false, REMOTE);
    assert!(leftover.from_draft);
    assert_eq!(
        leftover.local_path,
        recovery::draft_path(&leftover.dir, "app.conf")
    );
    assert_eq!(std::fs::read(&leftover.local_path).unwrap(), DRAFT);
    // content/ 里的内容没动（还是最近一次对齐远端的那份）。
    assert_eq!(
        std::fs::read(recovery::content_path(&leftover.dir, "app.conf")).unwrap(),
        ORIGINAL
    );
    let meta = leftover.meta.as_ref().unwrap();
    assert_eq!(
        recovery::unsent_state_of(&leftover.dir, meta),
        recovery::Unsent::Yes
    );
    assert!(!recovery::safe_to_delete(&leftover.dir), "有草稿不能自动删");
}

#[tokio::test]
async fn a_draft_identical_to_the_synced_content_is_not_an_unsent_change() {
    let root = crate::test_util::TestDir::new("draft-same");
    let dir = root.join("e1");
    std::fs::create_dir_all(recovery::content_dir(&dir)).unwrap();
    std::fs::write(recovery::content_path(&dir, "app.conf"), ORIGINAL).unwrap();
    recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: REMOTE.to_owned(),
            host_id: 1,
            host_label: "测试主机".to_owned(),
            file_name: "app.conf".to_owned(),
            link_target: None,
            base_size: ORIGINAL.len() as u64,
            base_mtime: Some(1),
            clean_sha256: recovery::sha256_hex(ORIGINAL),
            server_identity: None,
            local: false,
        },
    )
    .unwrap();
    recovery::write_draft(&dir, "app.conf", ORIGINAL, 1).unwrap(); // 改回去了
    assert!(recovery::safe_to_delete(&dir));
    assert!(
        recovery::scan(&root).is_empty(),
        "草稿和已保存的一样，不用恢复"
    );
    assert!(!dir.exists());
}

#[tokio::test]
async fn an_unreadable_draft_is_unknown_not_clean() {
    let root = crate::test_util::TestDir::new("draft-unreadable");
    let dir = root.join("e1");
    std::fs::create_dir_all(recovery::content_dir(&dir)).unwrap();
    std::fs::write(recovery::content_path(&dir, "app.conf"), ORIGINAL).unwrap();
    recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: REMOTE.to_owned(),
            host_id: 1,
            host_label: "测试主机".to_owned(),
            file_name: "app.conf".to_owned(),
            link_target: None,
            base_size: 1,
            base_mtime: Some(1),
            clean_sha256: recovery::sha256_hex(ORIGINAL),
            server_identity: None,
            local: false,
        },
    )
    .unwrap();
    // 草稿的位置被一个目录占着：读不了。
    std::fs::create_dir_all(recovery::draft_path(&dir, "app.conf")).unwrap();
    assert_eq!(recovery::unsent_state(&dir), recovery::Unsent::Unknown);
    assert!(!recovery::safe_to_delete(&dir));
}

#[tokio::test]
async fn reuploading_a_draft_uploads_the_draft_and_leaves_nothing_unsent() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("draft-reupload");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = editor_session_with_draft(&root, base, false, REMOTE);

    let outcome = recovery::reupload(&leftover, &fx.backend, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EditOutcome::Uploaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.fs.get(REMOTE).unwrap(), DRAFT, "传上去的应该是草稿");
    let meta = recovery::read_meta(&leftover.dir).unwrap();
    assert_eq!(meta.clean_sha256, recovery::sha256_hex(DRAFT));
    // 草稿升成了已对齐的内容，不会又被当成「上传期间改过」。
    assert_eq!(
        std::fs::read(recovery::content_path(&leftover.dir, "app.conf")).unwrap(),
        DRAFT
    );
    assert!(!recovery::draft_path(&leftover.dir, "app.conf").exists());
    assert_eq!(
        recovery::unsent_state(&leftover.dir),
        recovery::Unsent::None
    );
    assert!(recovery::scan(&root).is_empty());
}

#[tokio::test]
async fn save_a_copy_of_a_draft_writes_the_draft() {
    let fx = fixture().await;
    let root = crate::test_util::TestDir::new("draft-copy");
    let (base, _) = fx.backend.stat(REMOTE).await.unwrap();
    let leftover = editor_session_with_draft(&root, base, false, REMOTE);
    let dest = root.join("saved");
    let saved = recovery::save_copy(&leftover, &dest).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), DRAFT);
}

#[test]
fn restoring_a_local_draft_writes_it_back_to_the_original_file() {
    let root = crate::test_util::TestDir::new("draft-local");
    let target = root.join("work").join("app.conf");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, ORIGINAL).unwrap();
    let editors = root.join("edits");
    let leftover = editor_session_with_draft(
        &editors,
        RemoteSnapshot {
            size: 1,
            mtime: Some(1),
        },
        true,
        &target.display().to_string(),
    );
    let result = recovery::restore_local(&leftover, false).unwrap();
    assert_eq!(
        result,
        LocalRestore::Written {
            size: DRAFT.len() as u64
        }
    );
    assert_eq!(std::fs::read(&target).unwrap(), DRAFT);
    // 没有留下临时文件。
    let names: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["app.conf".to_owned()]);
    // 草稿升成了已对齐的内容。
    assert_eq!(
        recovery::unsent_state(&leftover.dir),
        recovery::Unsent::None
    );
}

#[test]
fn restoring_a_local_draft_refuses_when_the_file_changed_meanwhile() {
    let root = crate::test_util::TestDir::new("draft-local-conflict");
    let target = root.join("app.conf");
    std::fs::write(&target, b"SOMEONE ELSE EDITED THE FILE\n").unwrap();
    let editors = root.join("edits");
    let leftover = editor_session_with_draft(
        &editors,
        RemoteSnapshot {
            size: 1,
            mtime: Some(1),
        },
        true,
        &target.display().to_string(),
    );
    assert_eq!(
        recovery::restore_local(&leftover, false).unwrap(),
        LocalRestore::Conflict
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"SOMEONE ELSE EDITED THE FILE\n"
    );
    assert!(leftover.local_path.exists(), "没写出去，草稿必须还在");
    // 用户看过之后选了覆盖。
    assert_eq!(
        recovery::restore_local(&leftover, true).unwrap(),
        LocalRestore::Written {
            size: DRAFT.len() as u64
        }
    );
    assert_eq!(std::fs::read(&target).unwrap(), DRAFT);
}

#[test]
fn restoring_a_local_draft_when_the_file_was_deleted_is_a_conflict() {
    let root = crate::test_util::TestDir::new("draft-local-deleted");
    let target = root.join("gone.conf");
    let leftover = editor_session_with_draft(
        &root.join("edits"),
        RemoteSnapshot {
            size: 1,
            mtime: Some(1),
        },
        true,
        &target.display().to_string(),
    );
    assert_eq!(
        recovery::restore_local(&leftover, false).unwrap(),
        LocalRestore::Conflict
    );
    assert!(!target.exists());
}

#[test]
fn restoring_something_that_is_not_a_local_record_is_refused() {
    let root = crate::test_util::TestDir::new("draft-not-local");
    let leftover = editor_session_with_draft(
        &root,
        RemoteSnapshot {
            size: 1,
            mtime: Some(1),
        },
        false,
        REMOTE,
    );
    assert!(recovery::restore_local(&leftover, true).is_err());
}

#[test]
fn write_atomic_replaces_an_existing_file_and_leaves_no_temp_file() {
    let root = crate::test_util::TestDir::new("write-atomic");
    let target = root.join("a.txt");
    std::fs::write(&target, b"old").unwrap();
    crate::sftp::local::write_atomic(&target, b"new content").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new content");
    // 目标不存在也行。
    let fresh = root.join("b.txt");
    crate::sftp::local::write_atomic(&fresh, b"x").unwrap();
    assert_eq!(std::fs::read(&fresh).unwrap(), b"x");
    let mut names: Vec<_> = std::fs::read_dir(&*root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a.txt".to_owned(), "b.txt".to_owned()]);
}

#[cfg(windows)]
#[test]
fn write_atomic_failure_leaves_the_original_file_intact() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = crate::test_util::TestDir::new("write-atomic-fail");
    let target = root.join("locked.txt");
    std::fs::write(&target, b"ORIGINAL").unwrap();
    // 别的程序独占着原文件（不允许删除 / 改名）：替换必须失败，原文件不能丢。
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ：不共享写 / 删除
        .open(&target)
        .unwrap();
    let result = crate::sftp::local::write_atomic(&target, b"NEW");
    assert!(result.is_err(), "原文件被占着，替换应该失败");
    assert_eq!(std::fs::read(&target).unwrap(), b"ORIGINAL");
    let names: Vec<_> = std::fs::read_dir(&*root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["locked.txt".to_owned()], "临时文件要清掉");
}
