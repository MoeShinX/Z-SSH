//! R-6c R02：服务器没有 `posix-rename` 时，覆盖保存在每一步失败，服务器上都至少有一份完整内容，
//! 提示也要说对。走 AI 的 `write_file`（`execute_write` → `replace_via_temp` → `rename_over`）。

use super::testfs::MemFs;
use super::*;

const TARGET: &str = "/home/u/conf.txt";
const OLD: &[u8] = b"old content\n";
const NEW: &str = "new content\n";

async fn setup() -> (MemFs, WritePlan) {
    let fs = MemFs::new();
    fs.put(TARGET, OLD, 0o644);
    *fs.no_posix_rename.lock().unwrap() = true;
    let plan = prepare_write(&fs, "conf.txt", NEW, false).await.unwrap();
    (fs, plan)
}

/// 目录里所有文件的内容（路径 → 内容）。
fn files_in_home(fs: &MemFs) -> Vec<(String, Vec<u8>)> {
    fs.names_in("/home/u")
        .into_iter()
        .filter_map(|name| fs.get(&name).map(|data| (name, data)))
        .collect()
}

fn holds(fs: &MemFs, content: &[u8]) -> bool {
    files_in_home(fs).iter().any(|(_, data)| data == content)
}

#[tokio::test]
async fn the_success_path_replaces_the_file_and_leaves_no_backup() {
    let (fs, plan) = setup().await;
    execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    assert_eq!(fs.get(TARGET).unwrap(), NEW.as_bytes());
    let names = fs.names_in("/home/u");
    assert!(
        names
            .iter()
            .all(|n| !n.contains("zssh-old") && !n.contains("zssh-tmp")),
        "不该留下备用文件或临时文件：{names:?}"
    );
}

#[tokio::test]
async fn failing_to_move_the_original_aside_changes_nothing() {
    let (fs, plan) = setup().await;
    fs.fail_rename_calls.lock().unwrap().push(1);
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("提交前失败，原文件没有改动"), "{err}");
    assert_eq!(fs.get(TARGET).unwrap(), OLD);
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-tmp") && !n.contains("zssh-old")));
}

#[tokio::test]
async fn failing_to_install_the_new_file_restores_the_original() {
    let (fs, plan) = setup().await;
    fs.fail_rename_calls.lock().unwrap().push(2); // 第二步（新文件到位）失败，第三次（恢复）成功
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("提交前失败，原文件没有改动"), "{err}");
    assert_eq!(
        fs.get(TARGET).unwrap(),
        OLD,
        "新文件改名失败，原文件必须被改回来"
    );
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-tmp") && !n.contains("zssh-old")));
}

#[tokio::test]
async fn when_restoring_also_fails_both_copies_are_kept_and_the_message_says_where() {
    let (fs, plan) = setup().await;
    fs.fail_rename_calls.lock().unwrap().extend([2, 3]); // 到位失败，恢复也失败
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("原文件已经挪开但没能恢复"), "{err}");
    assert!(
        err.contains("/home/u/conf.txt.zssh-old-tok1"),
        "要说出原文件现在在哪：{err}"
    );
    assert!(!err.contains("原文件没有改动"), "不能说没动：{err}");
    // 两份完整内容都还在服务器上：旧的在备用名，新的在临时文件。
    assert_eq!(fs.get("/home/u/conf.txt.zssh-old-tok1").unwrap(), OLD);
    assert!(holds(&fs, NEW.as_bytes()), "新内容的临时文件不能删");
    assert!(fs.get(TARGET).is_none());
}

#[tokio::test]
async fn failing_to_delete_the_backup_still_counts_as_a_successful_replace() {
    let (fs, plan) = setup().await;
    *fs.fail_backup_remove.lock().unwrap() = true;
    execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    assert_eq!(fs.get(TARGET).unwrap(), NEW.as_bytes());
}

#[tokio::test]
async fn a_disconnect_in_the_middle_is_reported_as_uncertain_and_nothing_is_deleted() {
    // (第几次改名断线, 断线前改名是否已经发生)
    for (call, applied) in [(1, false), (1, true), (2, false), (2, true)] {
        let (fs, plan) = setup().await;
        *fs.disconnect_rename_call.lock().unwrap() = Some((call, applied));
        let err = execute_write(&fs, &plan, false, 5, "r", 5)
            .await
            .unwrap_err();
        assert!(err.contains("结果不确定"), "第 {call} 次/{applied}：{err}");
        assert!(
            !err.contains("原文件没有改动"),
            "不确定的时候不能说没动：{err}"
        );
        // 旧内容和新内容至少各有一份完整地留在服务器上（没有任何一份被删掉）。
        assert!(holds(&fs, OLD), "第 {call} 次/{applied}：旧内容没了");
        assert!(
            holds(&fs, NEW.as_bytes()),
            "第 {call} 次/{applied}：新内容没了"
        );
    }
}

#[tokio::test]
async fn a_server_with_the_extension_still_replaces_atomically() {
    let fs = MemFs::new();
    fs.put(TARGET, OLD, 0o644);
    let plan = prepare_write(&fs, "conf.txt", NEW, false).await.unwrap();
    execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    assert_eq!(fs.get(TARGET).unwrap(), NEW.as_bytes());
    assert!(
        fs.log().iter().all(|l| !l.starts_with("plain-rename")),
        "有扩展时不该走三步"
    );
}

/// 创建新文件（目标不存在）不需要备用文件，一步改名。
#[tokio::test]
async fn creating_a_new_file_needs_no_backup() {
    let fs = MemFs::new();
    *fs.no_posix_rename.lock().unwrap() = true;
    let plan = prepare_write(&fs, "fresh.txt", NEW, true).await.unwrap();
    execute_write(&fs, &plan, true, 5, "r", 5).await.unwrap();
    assert_eq!(fs.get("/home/u/fresh.txt").unwrap(), NEW.as_bytes());
    assert!(fs.log().iter().all(|l| !l.contains("zssh-old")));
}

/// R-6c R01：AI 写文件和传输队列共用同一把远端目标锁——别的任务正在写这个路径，就明确拒绝。
#[tokio::test]
async fn ai_write_is_refused_while_another_task_is_writing_the_same_remote_path() {
    use crate::sftp::transfer::lock::TargetLocks;
    let fs = MemFs::new();
    fs.put(TARGET, OLD, 0o644);
    *fs.server_key.lock().unwrap() = Some("ai-tester@lock-host:22".to_owned());
    let plan = prepare_write(&fs, "conf.txt", NEW, false).await.unwrap();

    // 另一个标签的传输引擎正在写同一个路径。
    let transfer = TargetLocks::remote()
        .try_acquire_remote("ai-tester@lock-host:22", TARGET)
        .unwrap();
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("正在写"), "{err}");
    assert_eq!(fs.get(TARGET).unwrap(), OLD, "被拒绝时什么都不能写");
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|name| !name.contains("zssh-tmp")));

    // 另一台服务器上同名路径不受影响；对方写完以后也能写。
    drop(transfer);
    execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    assert_eq!(fs.get(TARGET).unwrap(), NEW.as_bytes());
}
