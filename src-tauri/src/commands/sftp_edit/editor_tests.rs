//! 内置编辑器后端的测试（M3-5 第 2 节）。都从真实入口开始：`open_builtin`（远端）和 `open_local`（本地），
//! 保存走 `save_builtin` / `save_local`，冲突和放弃走共享的 `resolve_edit`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use zssh_core::sftp::edit::recovery;
use zssh_core::sftp::edit::Resolution;

use super::editor::{
    close_editor, draft_for, open_builtin, open_local, open_local_with, reload_builtin,
    reload_editor, save_builtin, save_local, save_local_as, save_local_with, EditorError,
    EditorOpenDto, LocalLinks,
};
use super::harness::{sample_owner_change, Env, FakeBackend, SFTP_ID, TEST_SERVER_IDENTITY};
use super::{resolve_edit, EditHost, EditSink};

const REMOTE: &str = "/srv/app.conf";

fn host() -> EditHost {
    EditHost {
        host_id: 7,
        host_label: "测试主机".to_owned(),
        server_identity: TEST_SERVER_IDENTITY.to_owned(),
    }
}

async fn open(env: &Env, path: &str) -> Result<EditorOpenDto, EditorError> {
    open_builtin(
        &env.registry,
        std::sync::Arc::clone(&env.backend),
        env.sink.as_ref(),
        SFTP_ID,
        host(),
        path,
    )
    .await
}

async fn save(env: &Env, edit_id: u64, text: &str) -> super::editor::EditorSaveDto {
    save_at(env, edit_id, text, 1).await
}

async fn save_at(
    env: &Env,
    edit_id: u64,
    text: &str,
    version: u64,
) -> super::editor::EditorSaveDto {
    save_builtin(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref() as &dyn EditSink,
        edit_id,
        text,
        version,
    )
    .await
    .expect("保存命令本身不该出错")
}

fn draft(env: &Env, edit_id: u64, text: &str) -> Result<(), EditorError> {
    draft_for(&env.registry, edit_id, text, 1)
}

fn states(env: &Env) -> Vec<&'static str> {
    env.sink.0.lock().unwrap().iter().map(|e| e.state).collect()
}

fn edit_dir(env: &Env, dto: &EditorOpenDto) -> std::path::PathBuf {
    env.data_dir
        .join("edit-tmp")
        .join(format!("e{}", dto.edit_id))
}

// ---------------------------------------------------------------------------
// 打开
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opening_a_utf8_file_gives_the_text_and_keeps_the_original_bytes_on_disk() {
    let original = "server {\n  listen 80;\n  # 中文注释\n}\n";
    let env = Env::new(FakeBackend::with_file(REMOTE, original.as_bytes()));
    let dto = open(&env, REMOTE).await.expect("打开");
    assert_eq!(dto.text, original);
    assert_eq!(dto.side, "remote");
    assert_eq!(dto.eol, "LF");
    assert!(!dto.bom);
    assert_eq!(dto.file_name, "app.conf");
    assert_eq!(dto.host_id, 7);
    assert_eq!(dto.host_label, "测试主机");
    assert_eq!(dto.size, original.len() as u64);
    // 磁盘上的 content/ 是下载下来的原始字节，恢复记录可读。
    let dir = edit_dir(&env, &dto);
    assert_eq!(
        std::fs::read(recovery::content_path(&dir, "app.conf")).unwrap(),
        original.as_bytes()
    );
    let meta = recovery::read_meta(&dir).expect("恢复记录");
    assert_eq!(meta.remote_path, REMOTE);
    assert!(!meta.local);
    assert_eq!(meta.clean_sha256, recovery::sha256_hex(original.as_bytes()));
}

#[tokio::test]
async fn files_the_editor_cannot_safely_round_trip_are_refused_with_a_reason() {
    let mut big_binary = vec![b'a'; 20_000];
    big_binary.push(0);
    big_binary.extend_from_slice(b"tail");
    let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        (
            "GBK 编码",
            vec![0xD6, 0xD0, 0xCE, 0xC4, b'\n'],
            "not_utf8",
            "不是 UTF-8",
        ),
        (
            "UTF-16",
            vec![0xFF, 0xFE, b'h', 0, b'i', 0],
            "not_utf8",
            "UTF-16",
        ),
        ("二进制（NUL 在文件深处）", big_binary, "binary", "二进制"),
        (
            "换行混用",
            b"a\nb\r\nc\n".to_vec(),
            "mixed_eol",
            "换行符混用",
        ),
    ];
    for (name, data, code, hint) in cases {
        let env = Env::new(FakeBackend::with_file(REMOTE, &data));
        let err = open(&env, REMOTE).await.expect_err(name);
        assert_eq!(err.code, code, "{name}：{err:?}");
        assert!(err.message.contains(hint), "{name}：{}", err.message);
        assert!(err.message.contains("下载"), "{name}：要建议下载后再处理");
        // 拒绝之后什么都不留：没有登记，没有临时目录。
        assert!(env.registry.find_by_path(7, REMOTE).is_none(), "{name}");
        assert!(!env.data_dir.join("edit-tmp").join("e1").exists(), "{name}");
    }
}

#[tokio::test]
async fn a_file_over_the_size_limit_is_refused() {
    let big = vec![b'x'; 5 * 1024 * 1024 + 1];
    let env = Env::new(FakeBackend::with_file(REMOTE, &big));
    let err = open(&env, REMOTE).await.expect_err("超过上限");
    assert_eq!(err.code, "too_large");
    assert!(err.message.contains("下载"), "{}", err.message);
    // 刚好等于上限可以打开。
    let exact = vec![b'x'; 5 * 1024 * 1024];
    let env = Env::new(FakeBackend::with_file(REMOTE, &exact));
    assert!(open(&env, REMOTE).await.is_ok());
}

#[tokio::test]
async fn a_missing_file_or_a_directory_is_an_error_not_a_crash() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"x"));
    let err = open(&env, "/srv/nope.conf").await.expect_err("不存在");
    assert_eq!(err.code, "remote");
}

#[tokio::test]
async fn opening_the_same_file_twice_is_refused_and_does_not_make_a_second_copy() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"x\n"));
    let first = open(&env, REMOTE).await.expect("打开");
    let err = open(&env, REMOTE).await.expect_err("再打开一次");
    assert_eq!(err.code, "already_open");
    assert!(edit_dir(&env, &first).exists());
    assert!(!env.data_dir.join("edit-tmp").join("e2").exists());
}

// ---------------------------------------------------------------------------
// 保存：逐字节
// ---------------------------------------------------------------------------

#[tokio::test]
async fn saving_a_crlf_file_with_a_bom_only_changes_the_edited_line() {
    let mut original = vec![0xEF, 0xBB, 0xBF];
    original.extend_from_slice("alpha=1\r\nbeta=2\r\n名字=值\r\ngamma=3".as_bytes());
    let env = Env::new(FakeBackend::with_file(REMOTE, &original));
    let dto = open(&env, REMOTE).await.expect("打开");
    assert!(dto.bom);
    assert_eq!(dto.eol, "CRLF");
    assert_eq!(dto.text, "alpha=1\nbeta=2\n名字=值\ngamma=3");

    // 没改就保存：逐字节一样。
    let result = save(&env, dto.edit_id, &dto.text).await;
    assert_eq!(result.state, "uploaded");
    assert_eq!(env.backend.get(REMOTE).unwrap(), original);

    // 只改中间一行。
    let edited = dto.text.replace("beta=2", "beta=TWO");
    let result = save(&env, dto.edit_id, &edited).await;
    assert_eq!(result.state, "uploaded");
    let mut expected = vec![0xEF, 0xBB, 0xBF];
    expected.extend_from_slice("alpha=1\r\nbeta=TWO\r\n名字=值\r\ngamma=3".as_bytes());
    assert_eq!(env.backend.get(REMOTE).unwrap(), expected);
    env.registry.shutdown();
}

#[tokio::test]
async fn saving_an_lf_file_without_a_bom_keeps_lf_and_the_missing_final_newline() {
    let original = b"one\ntwo\nthree";
    let env = Env::new(FakeBackend::with_file(REMOTE, original));
    let dto = open(&env, REMOTE).await.expect("打开");
    assert_eq!(dto.eol, "LF");
    save(&env, dto.edit_id, "one\nTWO\nthree").await;
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"one\nTWO\nthree");
    env.registry.shutdown();
}

#[tokio::test]
async fn a_save_sends_uploading_and_uploaded_events() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    save(&env, dto.edit_id, "b\n").await;
    let events = env.sink.0.lock().unwrap().clone();
    let relevant: Vec<_> = events
        .iter()
        .filter(|event| event.edit_id == dto.edit_id)
        .map(|event| event.state)
        .collect();
    assert_eq!(relevant, vec!["uploading", "uploaded"]);
    env.registry.shutdown();
}

// ---------------------------------------------------------------------------
// 保存：冲突、失败、属主（共享的编辑会话）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn saving_after_someone_else_changed_the_remote_file_is_a_conflict_and_changes_nothing() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    env.backend.put(REMOTE, b"someone else wrote this\n");
    let result = save(&env, dto.edit_id, "my edit\n").await;
    assert_eq!(result.state, "conflict");
    assert!(result.message.unwrap().contains("被改过"));
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"someone else wrote this\n"
    );
    assert!(
        states(&env).contains(&"conflict"),
        "要发冲突事件，现有的冲突对话框才会弹出来"
    );

    // 用户选「覆盖远端」：用保存的那份内容。
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Overwrite,
    )
    .await
    .unwrap();
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"my edit\n");
    env.registry.shutdown();
}

#[tokio::test]
async fn discarding_my_changes_after_a_conflict_reloads_the_remote_text_into_the_editor() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    env.backend
        .put(REMOTE, b"remote is now this\r\nwith crlf\r\n");
    assert_eq!(save(&env, dto.edit_id, "my edit\n").await.state, "conflict");
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Discard,
    )
    .await
    .unwrap();
    assert!(states(&env).contains(&"discarded"));
    let reloaded = reload_builtin(&env.registry, dto.edit_id).expect("重新读回来");
    assert_eq!(reloaded.text, "remote is now this\nwith crlf\n");
    assert_eq!(reloaded.eol, "CRLF", "换行风格跟着新内容走");
    // 之后再保存，按新的换行风格写。
    save(
        &env,
        dto.edit_id,
        "remote is now this\nwith crlf\nplus one\n",
    )
    .await;
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"remote is now this\r\nwith crlf\r\nplus one\r\n"
    );
    env.registry.shutdown();
}

/// R-9 T02：放弃修改后重新载入失败（服务器上的新内容解不出来），编辑器里还是旧文本：
/// 这时保存 / 存草稿都要拒绝，不能把旧文本当成新基准上的修改传上去。
#[tokio::test]
async fn after_a_discard_whose_reload_failed_the_stale_text_cannot_be_saved() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    // 别人把远端改成了换行混用的内容（内置编辑器解不了）。
    env.backend.put(REMOTE, b"line one\nline two\r\n");
    assert_eq!(save(&env, dto.edit_id, "my edit\n").await.state, "conflict");
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Discard,
    )
    .await
    .unwrap();
    assert!(
        reload_builtin(&env.registry, dto.edit_id).is_err(),
        "换行混用的内容载入要失败"
    );

    // 编辑器里还是 "my edit"：再按 Ctrl+S 不能把它传上去。
    let err = save_builtin(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref() as &dyn EditSink,
        dto.edit_id,
        "my edit\n",
        3,
    )
    .await
    .expect_err("过时的文本不能保存");
    assert_eq!(err.code, "needs_reload");
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"line one\nline two\r\n",
        "服务器上的内容不能被旧文本覆盖"
    );
    assert_eq!(
        draft_for(&env.registry, dto.edit_id, "my edit\n", 4)
            .expect_err("过时的文本也不能存成草稿")
            .code,
        "needs_reload"
    );

    // 服务器上的内容修好、重新载入成功之后，照常可以保存。
    env.backend.put(REMOTE, b"fixed\n");
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Discard,
    )
    .await
    .unwrap();
    assert_eq!(
        reload_builtin(&env.registry, dto.edit_id)
            .expect("这次能载入")
            .text,
        "fixed\n"
    );
    assert_eq!(
        save(&env, dto.edit_id, "fixed and edited\n").await.state,
        "uploaded"
    );
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"fixed and edited\n");
    env.registry.shutdown();
}

/// R-9 Q01：同一个 host_id 改了地址 / 用户名，现在的会话连到了另一台服务器：普通保存也要拒绝，
/// 哪怕那台服务器上同路径的文件大小、修改时间和打开时一样。
#[tokio::test]
async fn a_plain_save_refuses_a_session_on_a_different_server() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    *env.backend.identity_override.lock().unwrap() = Some("tester@other-host:22".to_owned());
    let err = save_builtin(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref() as &dyn EditSink,
        dto.edit_id,
        "my edit\n",
        2,
    )
    .await
    .expect_err("换了服务器不能保存");
    assert_eq!(err.code, "server_changed");
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"original\n",
        "什么都不能写"
    );
    assert!(
        !states(&env).contains(&"uploading"),
        "核对在任何写入之前：{:?}",
        states(&env)
    );

    // 回到原来那台服务器就能正常保存。
    *env.backend.identity_override.lock().unwrap() = None;
    assert_eq!(save(&env, dto.edit_id, "my edit\n").await.state, "uploaded");
    env.registry.shutdown();
}

/// R-9 T03：旧草稿 A → 保存 B 遇到冲突 → 在对话框里覆盖成功：A 不能留下来，否则重启后会被当成
/// 没保存的修改（再上传就回退了已保存的 B）。对话框处理期间又打的字（更晚的草稿）要保留。
#[tokio::test]
async fn resolving_a_conflict_removes_the_drafts_the_resolved_save_covers() {
    let mut env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    let dir = edit_dir(&env, &dto);
    draft_for(&env.registry, dto.edit_id, "draft A, version 1\n", 1).unwrap();
    env.backend.put(REMOTE, b"someone else\n");
    assert_eq!(
        save_at(&env, dto.edit_id, "saved B, version 2\n", 2)
            .await
            .state,
        "conflict"
    );
    assert!(
        recovery::draft_path(&dir, "app.conf").exists(),
        "冲突还没处理，草稿要留着"
    );
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Overwrite,
    )
    .await
    .unwrap();
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"saved B, version 2\n");
    assert!(
        !recovery::draft_path(&dir, "app.conf").exists(),
        "覆盖成功之后，版本不晚于 B 的草稿要删掉"
    );
    let old = env.crash_and_restart();
    assert!(
        env.registry.leftovers().is_empty(),
        "旧草稿 A 被当成了未上传的修改"
    );
    old.shutdown();
}

#[tokio::test]
async fn a_draft_newer_than_the_resolved_save_is_kept() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    let dir = edit_dir(&env, &dto);
    env.backend.put(REMOTE, b"someone else\n");
    assert_eq!(
        save_at(&env, dto.edit_id, "saved B, version 2\n", 2)
            .await
            .state,
        "conflict"
    );
    // 冲突对话框开着的时候用户又打了字：草稿版本 3。
    draft_for(&env.registry, dto.edit_id, "typing C, version 3\n", 3).unwrap();
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::Overwrite,
    )
    .await
    .unwrap();
    assert_eq!(recovery::read_draft_version(&dir, "app.conf"), Some(3));
    env.registry.shutdown();
}

#[tokio::test]
async fn a_failed_save_keeps_the_text_and_a_later_save_works_after_reconnecting() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    env.backend.fail_writes.store(true, AtomicOrdering::SeqCst); // 断线了
    let result = save(&env, dto.edit_id, "edited while offline\n").await;
    assert_eq!(result.state, "failed");
    let message = result.message.unwrap();
    assert!(message.contains("写入失败"), "要报明确的错误：{message}");
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"original\n",
        "远端原文件完好"
    );
    // 没有保存成功的内容还在本地（恢复记录会把它算作没上传的修改）。
    let dir = edit_dir(&env, &dto);
    assert_eq!(
        recovery::unsent_state(&dir),
        recovery::Unsent::Yes,
        "断线时没保存上去的内容必须算作没上传的修改"
    );

    // 重新连上了：再保存一次就好。
    env.backend.fail_writes.store(false, AtomicOrdering::SeqCst);
    let result = save(&env, dto.edit_id, "edited while offline\n").await;
    assert_eq!(result.state, "uploaded");
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"edited while offline\n");
    assert_eq!(recovery::unsent_state(&dir), recovery::Unsent::None);
    env.registry.shutdown();
}

#[tokio::test]
async fn a_save_that_would_change_the_owner_asks_first() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    env.backend
        .owner_trouble
        .store(true, AtomicOrdering::SeqCst);
    let dto = open(&env, REMOTE).await.expect("打开");
    let result = save(&env, dto.edit_id, "edited\n").await;
    assert_eq!(result.state, "owner_prompt");
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"original\n",
        "问之前不能替换"
    );
    let prompt = env
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|event| event.state == "owner_prompt")
        .cloned()
        .expect("owner_prompt 事件");
    assert_eq!(prompt.owner, Some(sample_owner_change()));
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::KeepNewOwner,
    )
    .await
    .unwrap();
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"edited\n");
    env.registry.shutdown();
}

/// R-8 第 4 节：属主询问里选「直接写入原文件」也要拿远端目标锁；上传任务占着目标时拒绝。
#[tokio::test]
async fn writing_in_place_is_refused_while_a_transfer_is_writing_the_target() {
    use zssh_core::sftp::transfer::lock::TargetLocks;

    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    env.backend
        .owner_trouble
        .store(true, AtomicOrdering::SeqCst);
    let dto = open(&env, REMOTE).await.expect("打开");
    let result = save(&env, dto.edit_id, "edited\n").await;
    assert_eq!(result.state, "owner_prompt");
    let dir = edit_dir(&env, &dto);

    // 另一个上传任务正占着这个目标。
    let transfer = TargetLocks::remote()
        .try_acquire_remote(TEST_SERVER_IDENTITY, REMOTE)
        .unwrap();
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::InPlace,
    )
    .await
    .unwrap();
    let last = env
        .sink
        .0
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("要发事件");
    assert_eq!(last.state, "failed", "{last:?}");
    assert!(
        last.message
            .as_deref()
            .is_some_and(|message| message.contains("正在写")),
        "{last:?}"
    );
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"original\n",
        "被拒绝时远端不能变"
    );
    assert_eq!(
        recovery::unsent_state(&dir),
        recovery::Unsent::Yes,
        "被拒后这次修改还保留着"
    );

    // 上传任务结束：再选一次就能原地写，改的是原文件、属主不变。
    drop(transfer);
    resolve_edit(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        dto.edit_id,
        Resolution::InPlace,
    )
    .await
    .unwrap();
    let last = env
        .sink
        .0
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("要发事件");
    assert_eq!(last.state, "uploaded", "{last:?}");
    assert_eq!(env.backend.get(REMOTE).unwrap(), b"edited\n");
    assert_eq!(last.owner, None, "原地写不改变属主");
    env.registry.shutdown();
}

// ---------------------------------------------------------------------------
// 草稿、关闭、恢复
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsaved_text_is_kept_as_a_draft_and_shows_up_after_a_crash() {
    let mut env = Env::new(FakeBackend::with_file(REMOTE, b"saved content\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "typed but never saved\n").unwrap();
    let dir = edit_dir(&env, &dto);
    assert_eq!(
        std::fs::read(recovery::draft_path(&dir, "app.conf")).unwrap(),
        b"typed but never saved\n"
    );
    // content/ 没动：草稿不会被当成「保存」。
    assert_eq!(
        std::fs::read(recovery::content_path(&dir, "app.conf")).unwrap(),
        b"saved content\n"
    );
    assert_eq!(
        env.backend.get(REMOTE).unwrap(),
        b"saved content\n",
        "草稿不会上传"
    );

    // 程序崩溃，重启：出现在「未上传的修改」里，内容就是草稿。
    drop(env.crash_and_restart()); // 崩溃：旧的登记什么清理都没做就没了
    let leftovers = env.registry.leftovers();
    assert_eq!(leftovers.len(), 1);
    assert!(leftovers[0].from_draft);
    assert_eq!(
        std::fs::read(&leftovers[0].local_path).unwrap(),
        b"typed but never saved\n"
    );
    assert_eq!(leftovers[0].meta.as_ref().unwrap().remote_path, REMOTE);
}

#[tokio::test]
async fn a_draft_that_is_typed_back_to_the_saved_text_is_removed() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"saved\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "something else\n").unwrap();
    let dir = edit_dir(&env, &dto);
    assert!(recovery::draft_path(&dir, "app.conf").exists());
    draft(&env, dto.edit_id, "saved\n").unwrap();
    assert!(!recovery::draft_path(&dir, "app.conf").exists());
    env.registry.shutdown();
}

#[tokio::test]
async fn saving_removes_the_draft_it_just_saved() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "b\n").unwrap();
    let dir = edit_dir(&env, &dto);
    assert!(recovery::draft_path(&dir, "app.conf").exists());
    save(&env, dto.edit_id, "b\n").await;
    assert!(!recovery::draft_path(&dir, "app.conf").exists());
    assert_eq!(recovery::unsent_state(&dir), recovery::Unsent::None);
    env.registry.shutdown();
}

// ---------------------------------------------------------------------------
// 草稿版本（第 3 节）：保存了更新的版本之后，旧草稿不能留着
// ---------------------------------------------------------------------------

const SAVE_WAIT: Duration = Duration::from_secs(10);

/// 草稿 A（版本 1）落盘，改成 B（版本 2）后保存：B 成功后版本 1 的草稿必须删掉，
/// 否则重启扫描会把 A 当成“最新未保存内容”恢复出来、反过来覆盖 B。
#[tokio::test]
async fn saving_a_newer_version_removes_the_older_draft_and_leaves_no_recovery_item() {
    let mut env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    let dir = edit_dir(&env, &dto);
    // 用户编辑成 A，草稿落盘（版本 1）。
    draft_for(&env.registry, dto.edit_id, "draft A, version 1\n", 1).unwrap();
    assert_eq!(recovery::read_draft_version(&dir, "app.conf"), Some(1));
    assert!(recovery::draft_path(&dir, "app.conf").exists());

    // 再改成 B（版本 2）后保存：B 保存成功，版本 1 的草稿不能再留着。
    let result = save_at(&env, dto.edit_id, "saved B, version 2\n", 2).await;
    assert_eq!(result.state, "uploaded");
    assert!(
        !recovery::draft_path(&dir, "app.conf").exists(),
        "保存了更新的版本之后，旧草稿必须删掉"
    );
    assert!(!recovery::draft_version_path(&dir, "app.conf").exists());
    assert_eq!(recovery::unsent_state(&dir), recovery::Unsent::None);

    // 重启扫描：没有恢复项。
    let old = env.crash_and_restart();
    assert!(
        env.registry.leftovers().is_empty(),
        "旧草稿被当成了未上传的修改"
    );
    old.shutdown();
}

/// 保存版本 2 还在路上时，用户又打了字、草稿版本 3 落盘：保存成功后草稿 3 必须保留
/// （它比保存的版本新，内容还没传上去）。
#[tokio::test]
async fn a_draft_written_while_the_save_is_in_flight_is_kept() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"original\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    let dir = edit_dir(&env, &dto);
    draft_for(&env.registry, dto.edit_id, "draft A, version 1\n", 1).unwrap();

    // 让保存版本 2 的上传卡在真正写入之前。
    env.backend.hold_next_writes(1);
    let registry = Arc::clone(&env.registry);
    let backend = Arc::clone(&env.backend);
    let sink = Arc::clone(&env.sink);
    let edit_id = dto.edit_id;
    let saving = tokio::spawn(async move {
        save_builtin(
            &registry,
            backend.as_ref(),
            sink.as_ref(),
            edit_id,
            "saved B, version 2\n",
            2,
        )
        .await
    });
    tokio::time::timeout(SAVE_WAIT, env.backend.write_started.notified())
        .await
        .expect("保存应该开始上传");

    // 保存还在路上：用户又打了字，草稿版本 3 落盘。
    draft_for(&env.registry, dto.edit_id, "draft C, version 3\n", 3).unwrap();
    env.backend.release_write();
    let result = saving.await.unwrap().expect("保存命令本身不该出错");
    assert_eq!(result.state, "uploaded");

    assert!(
        recovery::draft_path(&dir, "app.conf").exists(),
        "比保存版本更新的草稿不能删"
    );
    assert_eq!(recovery::read_draft_version(&dir, "app.conf"), Some(3));
    assert_eq!(
        std::fs::read(recovery::draft_path(&dir, "app.conf")).unwrap(),
        b"draft C, version 3\n"
    );
    env.registry.shutdown();
}

#[tokio::test]
async fn closing_keeps_a_draft_unless_the_user_chose_not_to_save() {
    // 没选「不保存」就关（比如窗口被强行关闭）：有草稿，目录保留。
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    close_editor(&env.registry, dto.edit_id, false);
    assert!(edit_dir(&env, &dto).exists(), "有草稿不能删");
    assert!(env.registry.get(dto.edit_id).is_none(), "条目已经注销");

    // 用户明确选了「不保存」：连草稿一起删。
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    close_editor(&env.registry, dto.edit_id, true);
    assert!(!edit_dir(&env, &dto).exists());

    // 保存过、没有草稿：普通关闭会清理目录。
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    save(&env, dto.edit_id, "b\n").await;
    close_editor(&env.registry, dto.edit_id, false);
    assert!(!edit_dir(&env, &dto).exists());
}

#[tokio::test]
async fn an_editor_entry_is_not_tied_to_its_sftp_session() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    assert!(env.registry.get(dto.edit_id).is_some());
    assert!(edit_dir(&env, &dto).exists());
    // 换一个会话（重连后 sftp_id 变了）照样能保存。
    let result = save(&env, dto.edit_id, "unsaved\n").await;
    assert_eq!(result.state, "uploaded");
    env.registry.shutdown();
}

#[tokio::test]
async fn quitting_keeps_drafts_for_the_next_start() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let dto = open(&env, REMOTE).await.expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    env.registry.shutdown();
    assert!(edit_dir(&env, &dto).exists(), "退出时有草稿的目录要留着");
}

#[tokio::test]
async fn the_editor_commands_report_a_closed_edit_clearly() {
    let env = Env::new(FakeBackend::with_file(REMOTE, b"a\n"));
    let err = save_builtin(
        &env.registry,
        env.backend.as_ref(),
        env.sink.as_ref(),
        999,
        "x",
        1,
    )
    .await
    .expect_err("没有这个编辑");
    assert_eq!(err.code, "closed");
    assert_eq!(draft(&env, 999, "x").unwrap_err().code, "closed");
}

// ---------------------------------------------------------------------------
// 本地文件
// ---------------------------------------------------------------------------

fn local_env() -> (Env, tempfile::TempDir) {
    let env = Env::new(FakeBackend::with_file("/unused", b""));
    let work = tempfile::Builder::new()
        .prefix("zssh-local-editor-")
        .tempdir()
        .unwrap();
    (env, work)
}

#[tokio::test]
async fn a_local_file_opens_edits_and_saves_atomically_byte_for_byte() {
    let (env, work) = local_env();
    let path = work.path().join("settings.ini");
    let mut original = vec![0xEF, 0xBB, 0xBF];
    original.extend_from_slice("[main]\r\nname=值\r\n".as_bytes());
    std::fs::write(&path, &original).unwrap();

    let dto = open_local(&env.registry, &path).expect("打开本地文件");
    assert_eq!(dto.side, "local");
    assert_eq!(dto.eol, "CRLF");
    assert!(dto.bom);
    assert_eq!(dto.text, "[main]\nname=值\n");
    assert_eq!(dto.host_label, "本机");

    let result = save_local(&env.registry, dto.edit_id, "[main]\nname=新值\n", false, 1)
        .await
        .unwrap();
    assert_eq!(result.state, "uploaded");
    let mut expected = vec![0xEF, 0xBB, 0xBF];
    expected.extend_from_slice("[main]\r\nname=新值\r\n".as_bytes());
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    // 没有留下临时文件。
    let names: Vec<_> = std::fs::read_dir(work.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["settings.ini".to_owned()]);
    env.registry.shutdown();
}

#[tokio::test]
async fn saving_a_local_file_that_another_program_changed_is_a_conflict_until_forced() {
    let (env, work) = local_env();
    let path = work.path().join("a.txt");
    std::fs::write(&path, b"original\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    std::fs::write(&path, b"changed by another program\n").unwrap();

    let result = save_local(&env.registry, dto.edit_id, "my edit\n", false, 1)
        .await
        .unwrap();
    assert_eq!(result.state, "conflict");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"changed by another program\n"
    );

    let forced = save_local(&env.registry, dto.edit_id, "my edit\n", true, 1)
        .await
        .unwrap();
    assert_eq!(forced.state, "uploaded");
    assert_eq!(std::fs::read(&path).unwrap(), b"my edit\n");

    // 保存之后基准更新了：再改再存不会再被当成冲突。
    let again = save_local(&env.registry, dto.edit_id, "second edit\n", false, 1)
        .await
        .unwrap();
    assert_eq!(again.state, "uploaded");
    env.registry.shutdown();
}

#[tokio::test]
async fn a_local_file_that_was_deleted_is_a_conflict_not_a_silent_recreate() {
    let (env, work) = local_env();
    let path = work.path().join("gone.txt");
    std::fs::write(&path, b"x\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    std::fs::remove_file(&path).unwrap();
    let result = save_local(&env.registry, dto.edit_id, "y\n", false, 1)
        .await
        .unwrap();
    assert_eq!(result.state, "conflict");
    assert!(!path.exists());
    env.registry.shutdown();
}

#[tokio::test]
async fn local_files_follow_the_same_refusal_rules() {
    let (env, work) = local_env();
    let gbk = work.path().join("gbk.txt");
    std::fs::write(&gbk, [0xD6, 0xD0, 0xCE, 0xC4]).unwrap();
    assert_eq!(
        open_local(&env.registry, &gbk).unwrap_err().code,
        "not_utf8"
    );
    let bin = work.path().join("a.bin");
    std::fs::write(&bin, b"\x00\x01\x02").unwrap();
    assert_eq!(open_local(&env.registry, &bin).unwrap_err().code, "binary");
    let big = work.path().join("big.txt");
    std::fs::write(&big, vec![b'x'; 5 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        open_local(&env.registry, &big).unwrap_err().code,
        "too_large"
    );
    assert_eq!(
        open_local(&env.registry, work.path()).unwrap_err().code,
        "not_a_file"
    );
    assert_eq!(
        open_local(&env.registry, &work.path().join("missing.txt"))
            .unwrap_err()
            .code,
        "local_read"
    );
    // 被拒绝的都没留下临时目录。
    assert!(!env.data_dir.join("edit-tmp").join("e1").exists());
}

#[tokio::test]
async fn a_local_conflict_can_be_reloaded_from_the_current_disk_content() {
    let (env, work) = local_env();
    let path = work.path().join("a.txt");
    std::fs::write(&path, b"original\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    // 别的程序改了文件 → 保存冲突。
    std::fs::write(&path, b"changed by another program\r\n").unwrap();
    let result = save_local(&env.registry, dto.edit_id, "my edit\n", false, 1)
        .await
        .unwrap();
    assert_eq!(result.state, "conflict");

    // 冲突对话框里选「重新载入」：读回磁盘上现在的内容（编码 / 换行一起）。
    let reloaded = reload_editor(&env.registry, dto.edit_id);
    assert!(reloaded.is_ok(), "本地文件也要能重新载入：{reloaded:?}");
    let reloaded = reloaded.unwrap();
    assert_eq!(reloaded.text, "changed by another program\n");
    assert_eq!(reloaded.eol, "CRLF");

    // 重新载入之后基准是磁盘上的内容：直接保存不再冲突。
    let saved = save_local(
        &env.registry,
        dto.edit_id,
        "changed by another program\nfixed\n",
        false,
        1,
    )
    .await
    .unwrap();
    assert_eq!(saved.state, "uploaded");
    env.registry.shutdown();
}

/// R-10 M1：另存为的名字带盘符（`C:evil.txt`）、冒号（备用数据流）等，不能写到所在目录之外。
#[tokio::test]
async fn save_as_refuses_names_that_would_leave_the_folder() {
    let (env, work) = local_env();
    let path = work.path().join("a.txt");
    std::fs::write(&path, b"original\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    for bad in ["C:evil.txt", "D:x", "a.txt:stream", "..", "CON"] {
        let err = save_local_as(&env.registry, dto.edit_id, "x\n", bad, 1)
            .await
            .expect_err("这种名字要拒绝");
        assert_eq!(err.code, "bad_name", "{bad:?}: {}", err.message);
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"original\n");
    let names: Vec<String> = std::fs::read_dir(work.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.txt".to_owned()], "目录里不能多出别的文件");
    env.registry.shutdown();
}

#[tokio::test]
async fn saving_a_local_file_as_a_new_name_switches_the_session() {
    let (env, work) = local_env();
    let path = work.path().join("a.txt");
    std::fs::write(&path, b"original\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");

    let target = work.path().join("copy.txt");
    let saved = save_local_as(&env.registry, dto.edit_id, "new content\n", "copy.txt", 1)
        .await
        .unwrap();
    assert_eq!(saved.state, "uploaded");
    assert_eq!(
        saved.path.as_deref(),
        Some(target.display().to_string().as_str())
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"original\n",
        "原文件不能被改"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"new content\n");

    // 另存为之后这个编辑跟踪新文件：再保存写的是新文件。
    let saved_again = save_local(&env.registry, dto.edit_id, "second edit\n", false, 1)
        .await
        .unwrap();
    assert_eq!(saved_again.state, "uploaded");
    assert_eq!(std::fs::read(&target).unwrap(), b"second edit\n");
    assert_eq!(std::fs::read(&path).unwrap(), b"original\n");

    // 同名文件已经存在：不覆盖，明确报错。
    let refused = save_local_as(&env.registry, dto.edit_id, "x\n", "copy.txt", 1)
        .await
        .unwrap();
    assert_eq!(refused.state, "failed");
    assert_eq!(std::fs::read(&target).unwrap(), b"second edit\n");
    env.registry.shutdown();
}

#[tokio::test]
async fn opening_the_same_local_file_twice_is_refused() {
    let (env, work) = local_env();
    let path = work.path().join("a.txt");
    std::fs::write(&path, b"x\n").unwrap();
    open_local(&env.registry, &path).expect("第一次");
    assert_eq!(
        open_local(&env.registry, &path).unwrap_err().code,
        "already_open"
    );
    env.registry.shutdown();
}

#[tokio::test]
async fn a_local_draft_survives_a_crash_and_can_be_written_back() {
    let (mut env, work) = local_env();
    let path = work.path().join("notes.txt");
    std::fs::write(&path, b"saved\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    draft(&env, dto.edit_id, "typed but never saved\n").unwrap();
    drop(env.crash_and_restart()); // 崩溃：旧的登记什么清理都没做就没了

    let leftovers = env.registry.leftovers().clone();
    assert_eq!(leftovers.len(), 1);
    let meta = leftovers[0].meta.as_ref().unwrap();
    assert!(meta.local);
    assert_eq!(Path::new(&meta.remote_path), path.as_path());
    let restored = recovery::restore_local(&leftovers[0], false).unwrap();
    assert!(matches!(restored, recovery::LocalRestore::Written { .. }));
    assert_eq!(std::fs::read(&path).unwrap(), b"typed but never saved\n");
}

#[tokio::test]
async fn closing_a_local_editor_with_a_draft_keeps_it_unless_discarded() {
    let (env, work) = local_env();
    let path = work.path().join("n.txt");
    std::fs::write(&path, b"x\n").unwrap();
    let dto = open_local(&env.registry, &path).expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    let dir = env
        .data_dir
        .join("edit-tmp")
        .join(format!("e{}", dto.edit_id));
    close_editor(&env.registry, dto.edit_id, false);
    assert!(dir.exists());
    // 另一个文件：选「不保存」。
    let other = work.path().join("o.txt");
    std::fs::write(&other, b"x\n").unwrap();
    let dto = open_local(&env.registry, &other).expect("打开");
    draft(&env, dto.edit_id, "unsaved\n").unwrap();
    let dir2 = env
        .data_dir
        .join("edit-tmp")
        .join(format!("e{}", dto.edit_id));
    close_editor(&env.registry, dto.edit_id, true);
    assert!(!dir2.exists());
}

// ---------------------------------------------------------------------------
// 第 8 节：本地符号链接（打开时解析真实目标，保存写目标、链接本身不动）
// ---------------------------------------------------------------------------

/// 假链接表：Windows 上普通用户建不了符号链接（要管理员 / 开发者模式），用它来测链接逻辑。
/// 键是 `long_path` 之后的路径（生产代码就是这么查的）。
#[derive(Default)]
struct FakeLinks {
    targets: StdMutex<HashMap<PathBuf, PathBuf>>,
}

impl FakeLinks {
    fn set(&self, link: &Path, target: &Path) {
        self.targets.lock().unwrap().insert(
            zssh_core::sftp::local::long_path(link),
            target.to_path_buf(),
        );
    }
}

impl LocalLinks for FakeLinks {
    fn target_of(&self, path: &Path) -> std::io::Result<Option<PathBuf>> {
        Ok(self.targets.lock().unwrap().get(path).cloned())
    }
}

#[tokio::test]
async fn saving_through_a_link_writes_the_real_target_not_the_link() {
    let (env, work) = local_env();
    let target = work.path().join("real.txt");
    let link = work.path().join("link.txt");
    std::fs::write(&target, b"old\n").unwrap();
    let links = FakeLinks::default();
    links.set(&link, &target);

    let dto = open_local_with(&env.registry, &link, &links).expect("打开");
    assert_eq!(
        dto.link_target.as_deref(),
        Some(target.display().to_string().as_str()),
        "打开时要把真实目标解析出来"
    );
    assert_eq!(dto.text, "old\n");

    let saved = save_local_with(&env.registry, dto.edit_id, "new\n", false, 1, &links)
        .await
        .unwrap();
    assert_eq!(saved.state, "uploaded");
    assert_eq!(std::fs::read(&target).unwrap(), b"new\n", "写的是真实目标");
    // 假链接表里的链接仍然指向目标（真实符号链接的等价检查：链接本身没被替换）。
    assert_eq!(
        links
            .target_of(&zssh_core::sftp::local::long_path(&link))
            .unwrap(),
        Some(target.clone())
    );
}

#[tokio::test]
async fn saving_through_a_link_refuses_when_the_link_was_repointed() {
    let (env, work) = local_env();
    let target_a = work.path().join("a.txt");
    let target_b = work.path().join("b.txt");
    let link = work.path().join("link.txt");
    std::fs::write(&target_a, b"A\n").unwrap();
    std::fs::write(&target_b, b"B\n").unwrap();
    let links = FakeLinks::default();
    links.set(&link, &target_a);
    let dto = open_local_with(&env.registry, &link, &links).expect("打开");

    // 打开之后链接被改成指向另一个文件：不能再写（会写到用户没想到的文件上）。
    links.set(&link, &target_b);
    let saved = save_local_with(&env.registry, dto.edit_id, "edited\n", false, 1, &links)
        .await
        .unwrap();
    assert_eq!(saved.state, "failed");
    assert!(
        saved
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("链接"),
        "{:?}",
        saved.message
    );
    assert_eq!(
        std::fs::read(&target_a).unwrap(),
        b"A\n",
        "原来的目标不能被改"
    );
    assert_eq!(
        std::fs::read(&target_b).unwrap(),
        b"B\n",
        "新指的目标也不能被改"
    );
}

/// 真实符号链接端到端（需要创建符号链接的权限：Windows 开发者模式 / 管理员；没有就跳过）。
#[cfg(windows)]
#[tokio::test]
async fn a_real_symlink_is_still_a_link_after_saving_through_it() {
    let (env, work) = local_env();
    let target = work.path().join("real.txt");
    let link = work.path().join("link.txt");
    std::fs::write(&target, b"old\n").unwrap();
    if std::os::windows::fs::symlink_file(&target, &link).is_err() {
        eprintln!("跳过：当前用户没有创建符号链接的权限（需要 Windows 开发者模式或管理员）");
        return;
    }
    let dto = open_local(&env.registry, &link).expect("打开");
    assert!(dto.link_target.is_some(), "真实链接要解析出目标");

    let saved = save_local(&env.registry, dto.edit_id, "new\n", false, 1)
        .await
        .unwrap();
    assert_eq!(saved.state, "uploaded");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "保存不能把符号链接替换成普通文件"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"new\n",
        "内容写进真实目标"
    );
}
