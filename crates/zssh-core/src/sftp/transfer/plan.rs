//! 任务展开（M3-1 第 3 节）：把一个"文件/文件夹"任务摊成要传的文件清单。
//!
//! - 上传：走本地树（[`super::super::local::walk_local_tree`]），
//!   **符号链接和目录联接默认跳过并记下来**；空目录也照搬。
//! - 下载：走远端树（`read_dir` 递归），符号链接默认跳过并记下来；每个目录里落地时
//!   过一次 [`LocalNamePlan`]（Windows 非法名字改名、大小写冲突加序号），改动都写进
//!   `notes`，任务里能看见。

use std::path::{Path, PathBuf};

use crate::sftp::error::SftpError;
use crate::sftp::local::{self, LocalNamePlan};
use crate::sftp::path as remote_path;

/// 一个要传的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// 源（上传：本地路径；下载：远端路径）。
    pub source: String,
    /// 目标（上传：远端路径；下载：本地路径）。
    pub target: String,
    /// 源的大小（拿不到就是 `None`，进度里按 0 计）。
    pub size: Option<u64>,
    /// 这个文件的名字被改过（下载时的 Windows 名字处理）。
    pub note: Option<String>,
}

/// 一份展开好的传输计划。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferPlan {
    pub files: Vec<PlannedFile>,
    /// 目标端要先建出来的目录（远端路径或本地路径，按方向解释）。
    pub dirs: Vec<String>,
    /// 需要告诉用户的说明（跳过链接、名字改动、空目录等）。
    pub notes: Vec<String>,
}

impl TransferPlan {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size.unwrap_or(0)).sum()
    }

    /// 大小未知的文件数（进度可能不准）。
    pub fn unknown_size_files(&self) -> usize {
        self.files.iter().filter(|file| file.size.is_none()).count()
    }
}

/// 上传：本地路径 → 远端路径。
pub fn plan_upload(
    local_source: &Path,
    remote_target: &str,
    recursive: bool,
) -> Result<TransferPlan, SftpError> {
    let mut plan = TransferPlan::default();
    if local_source.is_file() {
        let size = std::fs::metadata(local::long_path(local_source))
            .map(|meta| meta.len())
            .ok();
        plan.files.push(PlannedFile {
            source: local_source.display().to_string(),
            target: remote_target.to_owned(),
            size,
            note: None,
        });
        return Ok(plan);
    }
    if !local_source.is_dir() {
        return Err(SftpError::NotFound {
            path: local_source.display().to_string(),
        });
    }
    if !recursive {
        return Err(SftpError::InvalidInput(
            "这是个目录：要么勾选递归，要么选一个文件".to_owned(),
        ));
    }

    let tree = local::walk_local_tree(local_source)?;
    // 目标根本身也要建（"拖目录 a 到 /remote" 的落点是 /remote/a，它可能不存在；
    // M3-1 的测试传进的都是已存在的临时目录，没踩到这条）。create_target_dir
    // 对"已存在"是成功的，所以多建一次没有代价。
    plan.dirs.push(remote_target.to_owned());
    for dir in &tree.empty_dirs {
        plan.dirs.push(remote_path::join(remote_target, dir));
        plan.notes.push(format!("空目录 {dir} 也建出来"));
    }
    // 所有文件的父目录都要建。
    for file in &tree.files {
        if let Some(parent) = parent_of_relative(&file.relative) {
            let dir = remote_path::join(remote_target, &parent);
            if !plan.dirs.contains(&dir) {
                plan.dirs.push(dir);
            }
        }
    }
    for skipped in &tree.skipped_links {
        plan.notes
            .push(format!("跳过符号链接 / 目录联接：{skipped}"));
    }
    for file in tree.files {
        plan.files.push(PlannedFile {
            source: file.path.display().to_string(),
            target: remote_path::join(remote_target, &file.relative),
            size: Some(file.size),
            note: None,
        });
    }
    plan.notes.push(format!("共 {} 个文件", plan.files.len()));
    Ok(plan)
}

/// 下载：远端路径 → 本地路径。
pub async fn plan_download(
    lister: &dyn super::backend::RemoteLister,
    remote_source: &str,
    local_target: &Path,
    recursive: bool,
) -> Result<TransferPlan, SftpError> {
    let mut plan = TransferPlan::default();
    let attrs = lister.stat_remote(remote_source).await?;
    if !attrs.is_dir() {
        plan.files.push(PlannedFile {
            source: remote_source.to_owned(),
            target: local_target.display().to_string(),
            size: attrs.size,
            note: None,
        });
        return Ok(plan);
    }
    if !recursive {
        return Err(SftpError::InvalidInput(
            "这是个目录：要么勾选递归，要么选一个文件".to_owned(),
        ));
    }

    // 逐层走远端树：每层先拿到名字，再给这一层分配本地名字。
    let mut pending: Vec<(String, PathBuf)> =
        vec![(remote_source.to_owned(), local_target.to_path_buf())];
    plan.dirs.push(local_target.display().to_string());
    while let Some((dir, local_dir)) = pending.pop() {
        let entries = lister.list_dir(&dir).await?;
        let mut names = Vec::new();
        let mut sub_dirs = Vec::new();
        for entry in &entries {
            if entry.kind.is_symlink() {
                plan.notes.push(format!("跳过符号链接：{}", entry.path));
                continue;
            }
            if !entry.name_is_utf8 {
                // 名字不是 UTF-8：本地落盘会有损，明确记下来（下载下来内容是对的，
                // 名字只能用有损版本）。
                plan.notes.push(format!(
                    "{} 的名字不是 UTF-8，本地名字用有损版本 {}",
                    entry.path,
                    local::local_name(&entry.name).name
                ));
            }
            if entry.kind.is_dir() {
                sub_dirs.push((entry.path.clone(), entry.name.clone()));
            } else {
                names.push((entry.path.clone(), entry.name.clone(), entry.size));
            }
        }
        let existing = local_dir_entries(&local_dir);
        let remote_names: Vec<(String, bool)> = names
            .iter()
            .map(|(_, name, _)| (name.clone(), false))
            .chain(sub_dirs.iter().map(|(_, name)| (name.clone(), true)))
            .collect();
        let name_plan = LocalNamePlan::plan_with_kinds(&remote_names, &existing);
        let mut local_for = std::collections::HashMap::new();
        for (remote_name, assigned) in &name_plan.assignments {
            local_for.insert(remote_name.clone(), assigned.clone());
        }

        for (remote_file, remote_name, size) in names {
            let assigned = local_for
                .get(&remote_name)
                .cloned()
                .unwrap_or(local::LocalName {
                    name: remote_name.clone(),
                    note: None,
                });
            if let Some(note) = &assigned.note {
                plan.notes
                    .push(format!("{remote_name} → {}（{note}）", assigned.name));
            }
            let local_file = local_dir.join(&assigned.name);
            local::ensure_inside(local_target, &local_file)?;
            plan.files.push(PlannedFile {
                source: remote_file,
                target: local_file.display().to_string(),
                size,
                note: assigned.note.clone(),
            });
        }
        for (remote_dir, remote_name) in sub_dirs {
            let assigned = local_for
                .get(&remote_name)
                .cloned()
                .unwrap_or(local::LocalName {
                    name: remote_name.clone(),
                    note: None,
                });
            if let Some(note) = &assigned.note {
                plan.notes
                    .push(format!("{remote_name} → {}（{note}）", assigned.name));
            }
            let local_child = local_dir.join(&assigned.name);
            local::ensure_inside(local_target, &local_child)?;
            plan.dirs.push(local_child.display().to_string());
            pending.push((remote_dir, local_child));
        }
    }
    plan.notes.push(format!("共 {} 个文件", plan.files.len()));
    Ok(plan)
}

/// 远端 → 远端（复制粘贴的中转）：源树用 `lister` 读（可能是另一台主机），目标全是远端路径。
/// 符号链接跳过并记下来（和上传 / 下载一致）。
pub async fn plan_remote_copy(
    lister: &dyn super::backend::RemoteLister,
    source: &str,
    target: &str,
    recursive: bool,
) -> Result<TransferPlan, SftpError> {
    let mut plan = TransferPlan::default();
    let attrs = lister.stat_remote(source).await?;
    if attrs.is_symlink() {
        return Err(SftpError::InvalidInput(
            "符号链接不能用中转复制（会复制成链接指向的内容，和 cp -a 的行为不一致）".to_owned(),
        ));
    }
    if !attrs.is_dir() {
        plan.files.push(PlannedFile {
            source: source.to_owned(),
            target: target.to_owned(),
            size: attrs.size,
            note: None,
        });
        return Ok(plan);
    }
    if !recursive {
        return Err(SftpError::InvalidInput(
            "这是个目录：要么勾选递归，要么选一个文件".to_owned(),
        ));
    }
    plan.dirs.push(target.to_owned());
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((dir, target_dir)) = pending.pop() {
        for entry in lister.list_dir(&dir).await? {
            if entry.kind.is_symlink() {
                plan.notes.push(format!("跳过符号链接：{}", entry.path));
                continue;
            }
            let child = remote_path::join(&target_dir, &entry.name);
            if entry.kind.is_dir() {
                plan.dirs.push(child.clone());
                pending.push((entry.path.clone(), child));
            } else {
                plan.files.push(PlannedFile {
                    source: entry.path.clone(),
                    target: child,
                    size: entry.size,
                    note: None,
                });
            }
        }
    }
    plan.notes.push(format!("共 {} 个文件", plan.files.len()));
    Ok(plan)
}

/// 本地 → 本地（复制粘贴）：目录结构照搬，符号链接 / 联接跳过并记下来，空目录也复制。
pub fn plan_local_copy(
    source: &Path,
    target: &Path,
    recursive: bool,
) -> Result<TransferPlan, SftpError> {
    let mut plan = TransferPlan::default();
    if source.is_file() {
        let size = std::fs::metadata(local::long_path(source))
            .map(|meta| meta.len())
            .ok();
        plan.files.push(PlannedFile {
            source: source.display().to_string(),
            target: target.display().to_string(),
            size,
            note: None,
        });
        return Ok(plan);
    }
    if !source.is_dir() {
        return Err(SftpError::NotFound {
            path: source.display().to_string(),
        });
    }
    if !recursive {
        return Err(SftpError::InvalidInput(
            "这是个目录：要么勾选递归，要么选一个文件".to_owned(),
        ));
    }
    let tree = local::walk_local_tree(source)?;
    plan.dirs.push(target.display().to_string());
    let join = |relative: &str| -> PathBuf {
        let mut path = target.to_path_buf();
        for part in relative.split('/').filter(|part| !part.is_empty()) {
            path.push(part);
        }
        path
    };
    for dir in &tree.empty_dirs {
        plan.dirs.push(join(dir).display().to_string());
    }
    for file in &tree.files {
        if let Some(parent) = parent_of_relative(&file.relative) {
            let dir = join(&parent).display().to_string();
            if !plan.dirs.contains(&dir) {
                plan.dirs.push(dir);
            }
        }
    }
    for skipped in &tree.skipped_links {
        plan.notes
            .push(format!("跳过符号链接 / 目录联接：{skipped}"));
    }
    for file in tree.files {
        plan.files.push(PlannedFile {
            source: file.path.display().to_string(),
            target: join(&file.relative).display().to_string(),
            size: Some(file.size),
            note: None,
        });
    }
    plan.notes.push(format!("共 {} 个文件", plan.files.len()));
    Ok(plan)
}

/// 本地目录里已经有的名字（大小写冲突要避开它们）。
/// 本地目录里已有的条目：`(名字, 是不是目录)`。符号链接按它指向的东西算；读不出类型的按文件算
/// （和同名的远端文件走冲突对话框，和同名的远端目录则换名字，不会往里面写）。
fn local_dir_entries(dir: &Path) -> Vec<(String, bool)> {
    match std::fs::read_dir(local::long_path(dir)) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let is_dir = std::fs::metadata(entry.path())
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false);
                (entry.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// `a/b/c.txt` → `Some("a/b")`；`c.txt` → `None`。
fn parent_of_relative(relative: &str) -> Option<String> {
    relative
        .rfind('/')
        .map(|index| relative[..index].to_owned())
        .filter(|parent| !parent.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_plan_keeps_relative_structure_and_notes_empty_dirs() {
        let root = crate::test_util::TestDir::new("plan-up");
        std::fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"bbbb").unwrap();
        std::fs::write(root.join("sub").join("deep").join("c.txt"), b"cc").unwrap();

        let plan = plan_upload(&root, "/tmp/dest", true).expect("展开上传计划");
        let mut targets: Vec<&str> = plan.files.iter().map(|f| f.target.as_str()).collect();
        targets.sort();
        assert_eq!(
            targets,
            vec![
                "/tmp/dest/a.txt",
                "/tmp/dest/sub/b.txt",
                "/tmp/dest/sub/deep/c.txt"
            ]
        );
        assert_eq!(plan.total_bytes(), 3 + 4 + 2);
        assert!(plan.dirs.contains(&"/tmp/dest/sub".to_owned()));
        assert!(plan.dirs.contains(&"/tmp/dest/sub/deep".to_owned()));
        assert!(
            plan.dirs.contains(&"/tmp/dest/empty".to_owned()),
            "空目录也要建：{:?}",
            plan.dirs
        );
        assert!(plan.notes.iter().any(|note| note.contains("空目录")));
    }

    #[test]
    fn upload_plan_refuses_a_directory_without_recursive() {
        let root = crate::test_util::TestDir::new("plan-up2");
        let err = plan_upload(&root, "/tmp/dest", false).expect_err("目录要递归");
        assert!(err.to_string().contains("目录"), "{err}");
    }

    #[test]
    fn parent_of_relative_handles_top_level_files() {
        assert_eq!(parent_of_relative("a.txt"), None);
        assert_eq!(parent_of_relative("sub/a.txt").as_deref(), Some("sub"));
        assert_eq!(
            parent_of_relative("sub/deep/a.txt").as_deref(),
            Some("sub/deep")
        );
    }

    /// F02：递归下载时远端子项的名字再怪，也只能落在目标目录里面。
    #[tokio::test]
    async fn recursive_download_keeps_hostile_names_inside_the_target() {
        use crate::sftp::transfer::memory::MemoryEndpoints;
        let remote = MemoryEndpoints::new("/remote");
        for name in [
            "C:evil.txt",
            "CON",
            "a:b",
            "back\\slash.txt",
            "..\\..\\up.txt",
            "trail. ",
            "sub:dir/inner.txt",
            "D:/x.txt",
        ] {
            remote.put_remote(&format!("/remote/tree/{name}"), b"x".to_vec());
        }
        let root = crate::test_util::TestDir::new("plan-down-evil");
        let target = root.join("out");
        let plan = plan_download(&remote, "/remote/tree", &target, true)
            .await
            .expect("展开下载计划");
        assert!(!plan.files.is_empty());
        for file in &plan.files {
            local::ensure_inside(&target, Path::new(&file.target))
                .unwrap_or_else(|err| panic!("{} 跑出了目标目录：{err}", file.target));
        }
        for dir in plan.dirs.iter().skip(1) {
            local::ensure_inside(&target, Path::new(dir))
                .unwrap_or_else(|err| panic!("{dir} 跑出了目标目录：{err}"));
        }
    }
}
