//! 粘贴命令里不需要连接就能测的部分：拒绝规则、副本名、大小核对。

use super::*;

fn folder(path: &str) -> PasteItem {
    PasteItem {
        path: path.to_owned(),
        is_dir: true,
    }
}

fn file(path: &str) -> PasteItem {
    PasteItem {
        path: path.to_owned(),
        is_dir: false,
    }
}

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("zssh-paste-{tag}-"))
        .tempdir()
        .unwrap()
}

#[test]
fn a_remote_folder_cannot_be_pasted_into_itself_or_a_child() {
    assert!(refuse_into_self(true, &folder("/srv/app"), "/srv/app").is_some());
    assert!(refuse_into_self(true, &folder("/srv/app"), "/srv/app/sub/deeper").is_some());
    // 规范化之后才比：多余的斜杠、`.`、`..` 绕不过去。
    assert!(refuse_into_self(true, &folder("/srv/app/"), "/srv//app/./x").is_some());
    assert!(refuse_into_self(true, &folder("/srv/app"), "/srv/app/../app/x").is_some());
    // 名字只是前缀相同的兄弟目录没问题。
    assert!(refuse_into_self(true, &folder("/srv/app"), "/srv/application").is_none());
    assert!(refuse_into_self(true, &folder("/srv/app"), "/srv").is_none());
    // 文件不受限制。
    assert!(refuse_into_self(true, &file("/srv/app/a.txt"), "/srv/app").is_none());
}

#[test]
fn the_refusal_names_the_folder() {
    let reason = refuse_into_self(true, &folder("/srv/my app"), "/srv/my app/x").unwrap();
    assert!(reason.contains("my app"), "{reason}");
}

#[test]
fn a_local_folder_cannot_be_pasted_into_itself_even_with_case_or_slash_differences() {
    let root = scratch("self");
    let folder_path = root.path().join("Project");
    let child = folder_path.join("sub").join("deeper");
    std::fs::create_dir_all(&child).unwrap();
    let item = folder(&folder_path.display().to_string());

    assert!(refuse_into_self(false, &item, &folder_path.display().to_string()).is_some());
    assert!(refuse_into_self(false, &item, &child.display().to_string()).is_some());
    // Windows 路径不分大小写，斜杠方向也不算数。
    let shouted = child.display().to_string().to_uppercase();
    assert!(refuse_into_self(false, &item, &shouted).is_some());
    let slashed = child.display().to_string().replace('\\', "/");
    assert!(refuse_into_self(false, &item, &slashed).is_some());
    // 同级目录没问题。
    let sibling = root.path().join("Project2");
    std::fs::create_dir_all(&sibling).unwrap();
    assert!(refuse_into_self(false, &item, &sibling.display().to_string()).is_none());
}

#[test]
fn copy_names_skip_the_ones_already_taken() {
    let taken = ["/d/a - 副本.txt", "/d/a - 副本 2.txt"];
    let free = first_free_copy_path("/d/a.txt", |candidate| taken.contains(&candidate));
    assert_eq!(free, "/d/a - 副本 3.txt");
    assert_eq!(
        first_free_copy_path("/d/a.txt", |_| false),
        "/d/a - 副本.txt"
    );
}

#[test]
fn name_of_handles_both_separators_and_trailing_ones() {
    assert_eq!(name_of("/a/b/c.txt"), "c.txt");
    assert_eq!(name_of("/a/b/"), "b");
    assert_eq!(name_of(r"C:\work\file.txt"), "file.txt");
    assert_eq!(name_of(r"C:\work\dir\"), "dir");
}

// ---------------------------------------------------------------------------
// 冲突批次（SFTP-7）：一次粘贴里"先在这里问"的冲突和引擎的任务共用批次
// ---------------------------------------------------------------------------

use std::sync::Mutex as StdMutex;

use zssh_core::sftp::transfer::memory::MemoryEndpoints;
use zssh_core::sftp::transfer::{TransferEngine, TransferOptions};

/// 记下被问了几次、按固定动作回答的 resolver。
struct Fixed {
    asked: StdMutex<Vec<ConflictInfo>>,
    action: ConflictAction,
    apply_to_all: bool,
}

impl Fixed {
    fn new(action: ConflictAction, apply_to_all: bool) -> Self {
        Self {
            asked: StdMutex::new(Vec::new()),
            action,
            apply_to_all,
        }
    }

    fn count(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl ConflictResolver for Fixed {
    fn decide<'a>(&'a self, info: ConflictInfo) -> zssh_core::ssh::BoxFuture<'a, ConflictDecision> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(info);
            ConflictDecision {
                action: self.action,
                apply_to_all: self.apply_to_all,
                rename_to: Some("手改的名字".to_owned()),
            }
        })
    }
}

fn paste_state() -> ConflictState {
    let (engine, _events) = TransferEngine::new(
        Arc::new(MemoryEndpoints::new("/remote")),
        Arc::new(zssh_core::sftp::transfer::OverwriteAll),
        TransferOptions::default(),
    );
    ConflictState {
        batch: engine.new_batch(),
    }
}

fn info() -> ConflictInfo {
    ConflictInfo {
        job: 0,
        direction: Direction::RemoteCopy,
        source: "/a/x.txt".to_owned(),
        target: "/b/x.txt".to_owned(),
        source_size: Some(1),
        target_size: Some(2),
        source_mtime: None,
        target_mtime: None,
        can_resume: false,
        suggested_target: "/b/x - 副本.txt".to_owned(),
        batch: None,
    }
}

#[tokio::test]
async fn apply_to_all_asked_here_is_remembered_in_the_pastes_batch() {
    let mut state = paste_state();
    let resolver = Fixed::new(ConflictAction::Skip, true);
    let first = ask(&resolver, &mut state, info(), false).await;
    assert_eq!(first.action, ConflictAction::Skip);
    // 同一次粘贴里后面的同名条目直接用记住的动作，不再问。
    let second = ask(&resolver, &mut state, info(), false).await;
    assert_eq!(second.action, ConflictAction::Skip);
    assert_eq!(resolver.count(), 1);
    // 引擎里的任务看的是同一个批次。
    assert_eq!(state.batch.action(), Some(ConflictAction::Skip));
}

#[tokio::test]
async fn a_choice_made_by_an_engine_job_answers_the_precheck_too() {
    let mut state = paste_state();
    // 引擎里某个任务勾了"全部应用"（它和这里共用批次）。
    state.batch.remember(ConflictAction::Rename);
    let resolver = Fixed::new(ConflictAction::Overwrite, false);
    let decision = ask(&resolver, &mut state, info(), false).await;
    assert_eq!(decision.action, ConflictAction::Rename);
    assert_eq!(
        decision.rename_to, None,
        "手改的名字只用于被问的那个，其余用自动名字"
    );
    assert_eq!(resolver.count(), 0);
}

#[tokio::test]
async fn not_ticking_apply_to_all_asks_every_time() {
    let mut state = paste_state();
    let resolver = Fixed::new(ConflictAction::Overwrite, false);
    ask(&resolver, &mut state, info(), false).await;
    ask(&resolver, &mut state, info(), false).await;
    assert_eq!(resolver.count(), 2);
    assert_eq!(state.batch.action(), None);
}

#[tokio::test]
async fn overwriting_itself_is_never_remembered_for_the_rest() {
    let mut state = paste_state();
    let resolver = Fixed::new(ConflictAction::Overwrite, true);
    // 粘贴到自己所在的文件夹：用它自己覆盖自己没有意义，这个选择不能带给后面的条目。
    ask(&resolver, &mut state, info(), true).await;
    assert_eq!(state.batch.action(), None);
    ask(&resolver, &mut state, info(), false).await;
    assert_eq!(resolver.count(), 2);
}
