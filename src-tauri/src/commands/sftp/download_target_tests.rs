//! F02：顶层下载的本地落点必须落在用户选的目录里。

use super::*;
use std::path::{Component, Path, PathBuf};

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 独立于被测代码的判断：把路径按词法展开（处理 `.`、`..`），看是不是落在 `root` 的直接下面。
fn lexically_directly_inside(root: &Path, target: &Path) -> bool {
    let out = lexical(target);
    let root_norm = lexical(root);
    // 目标的名字里不能带盘符、冒号这类东西。
    let name_ok = out
        .file_name()
        .map(|name| {
            let name = name.to_string_lossy();
            !name.contains([':', '\\', '/']) && name != ".." && name != "."
        })
        .unwrap_or(false);
    name_ok
        && out.parent().map(|p| p.as_os_str().to_ascii_lowercase())
            == Some(root_norm.as_os_str().to_ascii_lowercase())
}

const HOSTILE: &[&str] = &[
    "/home/u/C:evil.txt",
    "C:evil.txt",
    "/home/u/..",
    "/home/u/../..",
    "/home/u/CON",
    "/home/u/con.txt",
    "/home/u/NUL",
    "/home/u/a:b",
    "/home/u/a\\b.txt",
    "/home/u/..\\..\\evil.txt",
    "/home/u/\\\\server\\share\\x",
    "/home/u/name.",
    "/home/u/name ",
    "/home/u/D:",
    "/home/u/a?b",
];

#[test]
fn top_level_download_stays_inside_the_chosen_directory() {
    let dir = r"C:\Users\someone\Downloads";
    for source in HOSTILE {
        // 报错也算安全；一旦给出路径，就必须在所选目录的直接下面。
        if let Ok(target) = download_target(dir, source) {
            assert!(
                lexically_directly_inside(Path::new(dir), Path::new(&target)),
                "{source:?} 落到了所选目录之外：{target}"
            );
        }
    }
}

#[test]
fn ordinary_names_are_kept() {
    let dir = r"C:\Users\someone\Downloads";
    assert_eq!(
        download_target(dir, "/srv/data/report 1.txt").unwrap(),
        r"C:\Users\someone\Downloads\report 1.txt"
    );
    assert_eq!(
        download_target(dir, "/srv/data/报告.pdf").unwrap(),
        r"C:\Users\someone\Downloads\报告.pdf"
    );
    // 目录（带结尾斜杠）取它自己的名字。
    assert_eq!(
        download_target(dir, "/srv/data/logs/").unwrap(),
        r"C:\Users\someone\Downloads\logs"
    );
}

#[test]
fn a_backslash_in_a_posix_name_is_part_of_the_name_not_a_separator() {
    let dir = r"C:\d";
    let target = download_target(dir, "/srv/a\\b.txt").unwrap();
    // 名字是 `a\b.txt`（一个文件）：Windows 上换成合法字符，仍然在 C:\d 直接下面。
    assert!(lexically_directly_inside(
        Path::new(dir),
        Path::new(&target)
    ));
    assert!(target.ends_with("a_b.txt"), "{target}");
}

#[test]
fn the_root_has_no_name_to_download_to() {
    assert!(download_target(r"C:\d", "/").is_err());
    assert!(download_target(r"C:\d", "").is_err());
}
