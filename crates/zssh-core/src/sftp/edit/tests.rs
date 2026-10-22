use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::*;

/// 内存里的假远端：路径 → (内容, mtime)。路径以 `/dir` 结尾的当目录。
#[derive(Default)]
struct FakeRemote {
    files: Mutex<HashMap<String, (Vec<u8>, u32)>>,
    clock: Mutex<u32>,
}

impl FakeRemote {
    fn put(&self, path: &str, data: &[u8]) {
        let mut clock = self.clock.lock().unwrap();
        *clock += 1;
        self.files
            .lock()
            .unwrap()
            .insert(path.to_owned(), (data.to_vec(), 1000 + *clock));
    }
    fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).map(|(d, _)| d.clone())
    }
    fn remove(&self, path: &str) {
        self.files.lock().unwrap().remove(path);
    }
}

impl EditBackend for FakeRemote {
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        Box::pin(async move {
            match self.files.lock().unwrap().get(path) {
                Some((data, mtime)) => Ok((
                    RemoteSnapshot {
                        size: data.len() as u64,
                        mtime: Some(*mtime),
                    },
                    path.ends_with("/dir"),
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
            let data = self.get(path).ok_or_else(|| SftpError::NotFound {
                path: path.to_owned(),
            })?;
            if data.len() as u64 > limit {
                return Err(SftpError::TooLarge {
                    path: path.to_owned(),
                    size: data.len() as u64,
                    limit,
                });
            }
            Ok(data)
        })
    }
    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.put(path, data);
            Ok(())
        })
    }
}

fn temp_file(tag: &str, content: &[u8]) -> (crate::test_util::TestDir, PathBuf) {
    let dir = crate::test_util::TestDir::new(&format!("edit-test-{tag}"));
    let path = dir.join("a.txt");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

async fn open(remote: &FakeRemote, tag: &str) -> (EditSession, crate::test_util::TestDir) {
    remote.put("/srv/a.txt", b"old");
    let (data, base) = download(remote, "/srv/a.txt", MAX_EDIT_BYTES)
        .await
        .unwrap();
    let (guard, local) = temp_file(tag, &data);
    (
        EditSession::new("/srv/a.txt".to_owned(), local, base),
        guard,
    )
}

#[tokio::test]
async fn unchanged_remote_uploads_and_updates_the_baseline() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "ok").await;
    std::fs::write(&session.local_path, b"new content").unwrap();
    let outcome = session.on_local_saved(&remote).await;
    assert_eq!(outcome, EditOutcome::Uploaded { size: 11 });
    assert_eq!(remote.get("/srv/a.txt").unwrap(), b"new content");
    // 基线更新了：再保存一次不会被当成冲突。
    std::fs::write(&session.local_path, b"third").unwrap();
    assert_eq!(
        session.on_local_saved(&remote).await,
        EditOutcome::Uploaded { size: 5 }
    );
    assert!(!session.has_pending_problem());
}

#[tokio::test]
async fn remote_changed_by_someone_else_is_a_conflict_and_nothing_is_uploaded() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "conflict").await;
    remote.put("/srv/a.txt", b"someone else was here");
    std::fs::write(&session.local_path, b"my edit").unwrap();
    let outcome = session.on_local_saved(&remote).await;
    assert!(
        matches!(outcome, EditOutcome::Conflict { remote: Some(_) }),
        "{outcome:?}"
    );
    assert_eq!(remote.get("/srv/a.txt").unwrap(), b"someone else was here");
    assert!(session.has_pending_problem());
}

#[tokio::test]
async fn same_size_but_newer_mtime_still_counts_as_changed() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "mtime").await;
    remote.put("/srv/a.txt", b"new"); // 同样 3 字节，mtime 变了
    std::fs::write(&session.local_path, b"mine").unwrap();
    assert!(matches!(
        session.on_local_saved(&remote).await,
        EditOutcome::Conflict { remote: Some(_) }
    ));
}

#[tokio::test]
async fn remote_deleted_is_reported_as_conflict_without_remote_state() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "gone").await;
    remote.remove("/srv/a.txt");
    std::fs::write(&session.local_path, b"mine").unwrap();
    assert_eq!(
        session.on_local_saved(&remote).await,
        EditOutcome::Conflict { remote: None }
    );
}

#[tokio::test]
async fn resolve_overwrite() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "res1").await;
    remote.put("/srv/a.txt", b"theirs");
    std::fs::write(&session.local_path, b"mine").unwrap();
    session.on_local_saved(&remote).await;
    assert_eq!(
        session.resolve(&remote, Resolution::Overwrite).await,
        EditOutcome::Uploaded { size: 4 }
    );
    assert_eq!(remote.get("/srv/a.txt").unwrap(), b"mine");
    assert!(!session.has_pending_problem());
}

#[tokio::test]
async fn resolve_discard_restores_the_remote_content_locally() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "res2").await;
    remote.put("/srv/a.txt", b"theirs");
    std::fs::write(&session.local_path, b"mine").unwrap();
    session.on_local_saved(&remote).await;
    assert_eq!(
        session.resolve(&remote, Resolution::Discard).await,
        EditOutcome::Discarded
    );
    assert_eq!(std::fs::read(&session.local_path).unwrap(), b"theirs");
    assert_eq!(remote.get("/srv/a.txt").unwrap(), b"theirs");
    assert!(!session.has_pending_problem());
}

/// C07：放弃本地修改时远端已经超过编辑器上限：不能无上限地读进来，本地修改原样保留，
/// 冲突还挂着（用户可以改选别的）。
#[tokio::test]
async fn resolve_discard_refuses_a_remote_file_over_the_editor_limit() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "res-big").await;
    let big = vec![b'x'; (MAX_EDIT_BYTES + 1) as usize];
    remote.put("/srv/a.txt", &big);
    std::fs::write(&session.local_path, b"mine").unwrap();
    assert!(matches!(
        session.on_local_saved(&remote).await,
        EditOutcome::Conflict { .. }
    ));
    let base_before = session.base();
    match session.resolve(&remote, Resolution::Discard).await {
        EditOutcome::Failed(reason) => {
            assert!(reason.contains("上限"), "{reason}");
            assert!(reason.contains(&MAX_EDIT_BYTES.to_string()), "{reason}");
        }
        other => panic!("应该拒绝：{other:?}"),
    }
    assert_eq!(std::fs::read(&session.local_path).unwrap(), b"mine");
    assert_eq!(session.base(), base_before, "基准不能被换成没读进来的远端");
    assert!(session.has_pending_problem());

    // 远端恢复到上限以内后，再选放弃就能正常读回。
    remote.put("/srv/a.txt", b"theirs");
    assert_eq!(
        session.resolve(&remote, Resolution::Discard).await,
        EditOutcome::Discarded
    );
    assert_eq!(std::fs::read(&session.local_path).unwrap(), b"theirs");
}

#[tokio::test]
async fn resolve_save_as_keeps_the_original_and_follows_the_new_file() {
    let remote = FakeRemote::default();
    let (mut session, _guard) = open(&remote, "res3").await;
    remote.put("/srv/a.txt", b"theirs");
    std::fs::write(&session.local_path, b"mine").unwrap();
    session.on_local_saved(&remote).await;
    let outcome = session
        .resolve(&remote, Resolution::SaveAs("a (副本).txt".to_owned()))
        .await;
    assert_eq!(outcome, EditOutcome::Uploaded { size: 4 });
    assert_eq!(remote.get("/srv/a.txt").unwrap(), b"theirs");
    assert_eq!(remote.get("/srv/a (副本).txt").unwrap(), b"mine");
    assert_eq!(session.remote_path, "/srv/a (副本).txt");
    // 名字已存在 / 非法：不覆盖、不上传
    let again = session
        .resolve(&remote, Resolution::SaveAs("a (副本).txt".to_owned()))
        .await;
    assert!(matches!(again, EditOutcome::Failed(_)));
    let bad = session
        .resolve(&remote, Resolution::SaveAs("x/y".to_owned()))
        .await;
    assert!(matches!(bad, EditOutcome::Failed(_)));
}

#[tokio::test]
async fn too_large_and_directory_are_refused_with_a_reason() {
    let remote = FakeRemote::default();
    remote.put("/srv/big.txt", &[b'a'; 100]);
    assert!(matches!(
        download(&remote, "/srv/big.txt", 50).await,
        Err(OpenError::TooLarge {
            size: 100,
            limit: 50
        })
    ));
    remote.put("/srv/dir", b"");
    assert!(matches!(
        download(&remote, "/srv/dir", MAX_EDIT_BYTES).await,
        Err(OpenError::NotAFile)
    ));
    assert!(matches!(
        download(&remote, "/srv/nope.txt", MAX_EDIT_BYTES).await,
        Err(OpenError::Remote(SftpError::NotFound { .. }))
    ));
    // GBK 之类的非 UTF-8 文本没有 NUL：下载这一步不拦，交给 text::decode 判断。
    remote.put("/srv/gbk.txt", &[0xc4, 0xe3, 0xba, 0xc3]);
    assert!(download(&remote, "/srv/gbk.txt", MAX_EDIT_BYTES)
        .await
        .is_ok());
}

#[test]
fn names_and_paths() {
    assert_eq!(sibling_remote_path("/srv/a.txt", "b.txt"), "/srv/b.txt");
    assert_eq!(sibling_remote_path("/a.txt", "b.txt"), "/b.txt");
    assert_eq!(default_save_as_name("/srv/a.txt"), "a (副本).txt");
    assert_eq!(default_save_as_name("/srv/Makefile"), "Makefile (副本)");
    assert_eq!(default_save_as_name("/srv/.env"), ".env (副本)");
    assert_eq!(sanitize_local_name("a:b*c?.txt"), "a_b_c_.txt");
    assert_eq!(sanitize_local_name("con.txt"), "_con.txt");
    assert_eq!(sanitize_local_name("名字 带空格.conf"), "名字 带空格.conf");
    assert_eq!(sanitize_local_name("end. "), "end");
    assert!(validate_remote_name("ok.txt").is_ok());
    assert!(validate_remote_name("").is_err());
    assert!(validate_remote_name("a/b").is_err());
    assert!(validate_remote_name("..").is_err());
}

#[test]
fn temp_root_is_under_the_data_dir_not_the_system_temp() {
    let root = edit_root(Path::new("C:/zssh-shots/x"));
    assert!(root.starts_with("C:/zssh-shots/x"));
    assert!(!root.starts_with(std::env::temp_dir()));
}
