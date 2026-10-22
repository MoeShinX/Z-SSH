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
