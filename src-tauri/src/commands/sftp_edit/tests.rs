//! 启动时扫描上次留下的编辑目录：有未上传修改（含草稿）的留下，确认干净（或认不出但空）的清掉。
//! 旧版本外部编辑留下的目录结构相同：认得出就按「未上传的修改」处理，认不出就保留（不删用户的文件）。

use super::*;
use zssh_core::sftp::edit::recovery::EditMeta;

#[test]
fn restart_keeps_the_edit_that_was_never_uploaded() {
    let guard = tempfile::Builder::new()
        .prefix("zssh-edit-restart-")
        .tempdir()
        .unwrap();
    let data_dir = guard.path();
    // 上次运行留下的：用户改过、没来得及上传，程序就崩了。
    let dir = edit_root(data_dir).join("e1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.conf"), b"EDITS THAT WERE NEVER UPLOADED").unwrap();

    let _registry = EditRegistry::new(data_dir);
    assert_eq!(
        std::fs::read(dir.join("app.conf")).unwrap(),
        b"EDITS THAT WERE NEVER UPLOADED",
        "重启把没上传的修改删掉了"
    );
}

#[test]
fn restart_reports_unsent_edits_and_cleans_the_rest() {
    let guard = tempfile::Builder::new()
        .prefix("zssh-edit-restart2-")
        .tempdir()
        .unwrap();
    let data_dir = guard.path();
    let root = edit_root(data_dir);
    let make = |id: &str, downloaded: &[u8], now: &[u8]| {
        let dir = root.join(id);
        std::fs::create_dir_all(recovery::content_dir(&dir)).unwrap();
        std::fs::write(recovery::content_path(&dir, "a.txt"), downloaded).unwrap();
        recovery::write_meta(
            &dir,
            &EditMeta {
                remote_path: "/srv/a.txt".to_owned(),
                host_id: 3,
                host_label: "测试主机".to_owned(),
                file_name: "a.txt".to_owned(),
                link_target: None,
                base_size: downloaded.len() as u64,
                base_mtime: Some(1),
                clean_sha256: recovery::sha256_hex(downloaded),
                server_identity: None,
                local: false,
            },
        )
        .unwrap();
        std::fs::write(recovery::content_path(&dir, "a.txt"), now).unwrap();
        dir
    };
    let unsent = make("e4", b"old", b"new, never uploaded");
    let untouched = make("e2", b"same", b"same");

    let registry = EditRegistry::new(data_dir);
    assert!(unsent.exists());
    assert!(!untouched.exists());
    let left = registry.leftovers();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, "e4");
    assert_eq!(left[0].meta.as_ref().unwrap().host_id, 3);
    drop(left);
    // 新编辑的编号接在留下的后面，不会撞 e4。
    assert_eq!(registry.next_id.load(Ordering::SeqCst), 4);
}

/// 旧布局（文件直接放在 eN/ 里，没有 content/ 和 state/）的目录按"认不出"处理：保留。
#[test]
fn an_old_layout_directory_is_kept_as_unrecognised() {
    let guard = tempfile::Builder::new()
        .prefix("zssh-edit-oldlayout-")
        .tempdir()
        .unwrap();
    let dir = edit_root(guard.path()).join("e3");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), b"old layout, maybe unsaved").unwrap();
    std::fs::write(dir.join("meta.json"), b"{}").unwrap();
    let registry = EditRegistry::new(guard.path());
    assert!(dir.join("notes.txt").exists());
    let left = registry.leftovers();
    assert_eq!(left.len(), 1);
    assert!(left[0].meta.is_none());
}

/// R-8 第 5 节：重新上传成功但随后读不了恢复记录，不能当"没有修改"删目录 / 摘列表。
#[tokio::test]
async fn reupload_keeps_the_leftover_when_the_record_becomes_unreadable() {
    use std::sync::Arc;

    use super::editor::open_builtin;
    use super::harness::{Env, FakeBackend, SFTP_ID};

    let mut env = Env::new(FakeBackend::with_file("/srv/app.conf", b"original\n"));
    let dto = open_builtin(
        &env.registry,
        Arc::clone(&env.backend),
        env.sink.as_ref(),
        SFTP_ID,
        EditHost {
            host_id: 7,
            host_label: "测试主机".to_owned(),
            server_identity: super::harness::TEST_SERVER_IDENTITY.to_owned(),
        },
        "/srv/app.conf",
    )
    .await
    .expect("打开编辑器");
    let dir = env
        .data_dir
        .join("edit-tmp")
        .join(format!("e{}", dto.edit_id));
    // 用户改了内容但还没保存：恢复扫描会把它当成没上传的修改。
    std::fs::write(
        recovery::content_path(&dir, "app.conf"),
        b"edited but never uploaded\n",
    )
    .unwrap();
    drop(env.crash_and_restart());
    assert_eq!(env.registry.leftovers().len(), 1, "重启后修改还在");

    // 上传成功了，但恢复记录在这之后读不了（删掉 state/meta.json）。
    std::fs::remove_file(recovery::state_dir(&dir).join(recovery::META_FILE)).unwrap();
    let leftover = env.registry.leftovers()[0].clone();
    let result = super::reupload_leftover(
        &env.registry,
        &leftover.id,
        &leftover,
        env.backend.as_ref(),
        false,
    )
    .await
    .expect("重新上传命令本身不该出错");

    assert_eq!(result.state, "failed", "{result:?}");
    assert!(dir.exists(), "读不了恢复记录时不能删目录");
    assert_eq!(
        env.registry.leftovers().len(),
        1,
        "读不了恢复记录时列表里还要有这一项"
    );
    assert_eq!(
        env.backend.get("/srv/app.conf").unwrap(),
        b"edited but never uploaded\n"
    );
}

/// R-10 M17：崩溃后"重新上传"要核对服务器身份，只核对 host_id 会把修改写到改过地址的另一台服务器。
#[tokio::test]
async fn reupload_after_a_crash_refuses_a_different_server_or_an_unknown_one() {
    use super::editor::open_builtin;
    use super::harness::{Env, FakeBackend, SFTP_ID, TEST_SERVER_IDENTITY};

    let mut env = Env::new(FakeBackend::with_file("/srv/app.conf", b"original\n"));
    let dto = open_builtin(
        &env.registry,
        Arc::clone(&env.backend),
        env.sink.as_ref(),
        SFTP_ID,
        EditHost {
            host_id: 7,
            host_label: "测试主机".to_owned(),
            server_identity: TEST_SERVER_IDENTITY.to_owned(),
        },
        "/srv/app.conf",
    )
    .await
    .expect("打开编辑器");
    let dir = env
        .data_dir
        .join("edit-tmp")
        .join(format!("e{}", dto.edit_id));
    std::fs::write(recovery::content_path(&dir, "app.conf"), b"edited\n").unwrap();
    drop(env.crash_and_restart());
    let leftover = env.registry.leftovers()[0].clone();
    assert_eq!(
        leftover
            .meta
            .as_ref()
            .and_then(|m| m.server_identity.as_deref()),
        Some(TEST_SERVER_IDENTITY),
        "恢复记录要记下打开时的服务器身份"
    );

    // 同一个 host_id 现在连到了另一台服务器。
    *env.backend.identity_override.lock().unwrap() = Some("tester@other-host:22".to_owned());
    let err = super::reupload_leftover(
        &env.registry,
        &leftover.id,
        &leftover,
        env.backend.as_ref(),
        false,
    )
    .await
    .expect_err("换了服务器不能上传");
    assert!(err.contains("另一台服务器"), "{err}");
    assert_eq!(env.backend.get("/srv/app.conf").unwrap(), b"original\n");

    // 旧版本留下的记录没有身份：无法确认，也不上传。
    *env.backend.identity_override.lock().unwrap() = None;
    let mut old_style = leftover.clone();
    if let Some(meta) = old_style.meta.as_mut() {
        meta.server_identity = None;
    }
    let err = super::reupload_leftover(
        &env.registry,
        &old_style.id,
        &old_style,
        env.backend.as_ref(),
        false,
    )
    .await
    .expect_err("没有身份不能上传");
    assert!(err.contains("旧版本"), "{err}");
    assert_eq!(env.backend.get("/srv/app.conf").unwrap(), b"original\n");
    assert!(dir.exists(), "没上传就不能删恢复目录");

    // 同一台服务器照常能上传。
    let result = super::reupload_leftover(
        &env.registry,
        &leftover.id,
        &leftover,
        env.backend.as_ref(),
        false,
    )
    .await
    .expect("同一台服务器");
    assert_eq!(result.state, "done", "{result:?}");
    assert_eq!(env.backend.get("/srv/app.conf").unwrap(), b"edited\n");
}

// ---------------------------------------------------------------------------
// R-8 第 6 节：冲突 / 属主处理要用"这台主机当前"的 SFTP 会话
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Mutex;

use zssh_core::sftp::edit::Resolution;
use zssh_core::ssh::BoxFuture;

use super::editor::open_builtin;
use super::harness::{Env, FakeBackend, TEST_SERVER_IDENTITY};

/// 测试用的远端路径（和 editor_tests 一致；那个常量是私有的，这里自己定义一份）。
const REMOTE: &str = "/srv/app.conf";

/// 内存里的"当前打开的 SFTP 标签"：不构造真实 `SftpChannel` / `AppHandle`，只测
/// `resolve_with_source`（`sftp_edit_resolve` 去掉 AppHandle 后的原样主体）。
struct FakeTabSource {
    tabs: Mutex<Vec<(String, i64, String)>>,
    backends: Mutex<HashMap<String, Arc<FakeBackend>>>,
}

impl FakeTabSource {
    fn new() -> Self {
        Self {
            tabs: Mutex::new(Vec::new()),
            backends: Mutex::new(HashMap::new()),
        }
    }

    fn add(&self, sftp_id: &str, host_id: i64, identity: &str, backend: Arc<FakeBackend>) {
        self.tabs
            .lock()
            .unwrap()
            .push((sftp_id.to_owned(), host_id, identity.to_owned()));
        self.backends
            .lock()
            .unwrap()
            .insert(sftp_id.to_owned(), backend);
    }
}

impl super::EditTabSource for FakeTabSource {
    fn candidates(&self) -> Vec<(String, i64, String)> {
        self.tabs.lock().unwrap().clone()
    }

    fn open<'a>(&'a self, sftp_id: &'a str) -> BoxFuture<'a, Result<super::OpenedTab, String>> {
        Box::pin(async move {
            let (host_id, server_identity) = self
                .tabs
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _, _)| id == sftp_id)
                .map(|(_, host_id, identity)| (*host_id, identity.clone()))
                .ok_or_else(|| format!("SFTP 标签不存在：{sftp_id}"))?;
            let backend: Arc<dyn super::EditBackend> = self
                .backends
                .lock()
                .unwrap()
                .get(sftp_id)
                .cloned()
                .ok_or_else(|| format!("标签 {sftp_id} 没有连接"))?;
            Ok(super::OpenedTab {
                backend,
                host_id,
                server_identity,
            })
        })
    }
}

fn edit_host() -> super::EditHost {
    super::EditHost {
        host_id: 7,
        host_label: "测试主机".to_owned(),
        server_identity: TEST_SERVER_IDENTITY.to_owned(),
    }
}

/// 打开内置编辑器，然后让远端被别人改掉、保存一次拿到冲突（等用户选覆盖）。
async fn open_with_conflict() -> (Env, u64) {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open_builtin(
        &env.registry,
        Arc::clone(&env.backend),
        env.sink.as_ref(),
        super::harness::SFTP_ID,
        edit_host(),
        REMOTE,
    )
    .await
    .expect("打开编辑器");
    env.backend.put(REMOTE, b"someone else changed this\n");
    let result = super::editor::save_builtin(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        "my local edit\n",
        1,
    )
    .await
    .expect("保存命令本身不该出错");
    assert_eq!(result.state, "conflict", "{result:?}");
    (env, dto.edit_id)
}

/// 关掉旧 SFTP 标签、重开一个新标签之后，冲突处理要用新标签（按 host_id 找当前会话）。
#[tokio::test]
async fn conflict_resolution_after_the_sftp_tab_was_reopened_uses_the_current_session() {
    let (env, edit_id) = open_with_conflict().await;
    let source = FakeTabSource::new();
    // 打开编辑器时那条（旧 id）已经关了；用户重开了一个新标签（同一台主机、同一个服务器）。
    source.add(
        "sftp-new",
        7,
        TEST_SERVER_IDENTITY,
        Arc::clone(&env.backend),
    );

    super::resolve_with_source(
        &env.registry,
        &source,
        env.sink.as_ref(),
        edit_id,
        Resolution::Overwrite,
        "测试主机",
    )
    .await
    .expect("重开 SFTP 标签后选覆盖应该成功");

    let last = env
        .sink
        .0
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("要发事件");
    assert_eq!(last.state, "uploaded", "{last:?}");
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"my local edit\n");
}

/// 有标签但连到的服务器和打开编辑器时不是同一台：拒绝，不能写到别的服务器。
#[tokio::test]
async fn conflict_resolution_refuses_a_tab_that_connects_to_a_different_server() {
    let (env, edit_id) = open_with_conflict().await;
    let source = FakeTabSource::new();
    source.add(
        "sftp-new",
        7,
        "tester@other-host:22",
        Arc::clone(&env.backend),
    );

    let error = super::resolve_with_source(
        &env.registry,
        &source,
        env.sink.as_ref(),
        edit_id,
        Resolution::Overwrite,
        "测试主机",
    )
    .await
    .expect_err("服务器身份不一致必须拒绝");
    assert!(error.contains("不一致"), "{error}");
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"someone else changed this\n",
        "被拒绝时不能写远端"
    );
    assert!(env
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .all(|event| event.state != "uploaded"));
}

/// 这台主机现在没有打开任何 SFTP 标签：给用户明确的"先打开 SFTP"提示。
#[tokio::test]
async fn conflict_resolution_asks_for_an_sftp_tab_when_none_is_open() {
    let (env, edit_id) = open_with_conflict().await;
    let source = FakeTabSource::new();

    let error = super::resolve_with_source(
        &env.registry,
        &source,
        env.sink.as_ref(),
        edit_id,
        Resolution::Overwrite,
        "测试主机",
    )
    .await
    .expect_err("没有 SFTP 标签要报错");
    assert!(error.contains("请先打开"), "{error}");
    assert!(error.contains("测试主机"), "{error}");
}

/// 打开时那条标签还在的话优先用它（不用另找一个）。
#[tokio::test]
async fn conflict_resolution_prefers_the_tab_the_editor_was_opened_with() {
    let (env, edit_id) = open_with_conflict().await;
    let source = FakeTabSource::new();
    let other = FakeBackend::with_file("/unused", b"");
    source.add(
        super::harness::SFTP_ID,
        7,
        TEST_SERVER_IDENTITY,
        Arc::clone(&env.backend),
    );
    source.add("sftp-new", 7, TEST_SERVER_IDENTITY, Arc::clone(&other));

    super::resolve_with_source(
        &env.registry,
        &source,
        env.sink.as_ref(),
        edit_id,
        Resolution::Overwrite,
        "测试主机",
    )
    .await
    .expect("打开时那条还在，应该直接用它");
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"my local edit\n");
    assert_eq!(other.get(REMOTE), None, "不该用另一个标签的连接去写");
}
