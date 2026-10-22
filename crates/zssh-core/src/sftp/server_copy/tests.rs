//! 服务器端复制的内存实现和单测：复制中途注入失败，验证目标不被破坏、临时文件被清掉。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[derive(Clone)]
struct Node {
    data: Vec<u8>,
    is_dir: bool,
    is_symlink: bool,
    mode: u32,
    mtime: u32,
}

struct Handle {
    path: String,
}

#[derive(Default)]
struct FakeFs {
    nodes: Mutex<BTreeMap<String, Node>>,
    handles: Mutex<HashMap<String, Handle>>,
    next_handle: AtomicU64,
    /// 源路径里包含这个子串时，`copy_data` 写一半就失败。
    fail_copy_containing: Mutex<Option<String>>,
    /// 指向目录的符号链接：链接路径 → 目标目录（绝对路径）。列目录和 realpath 会穿过它们，
    /// 像真服务器一样（C09 / C21）。
    dir_links: Mutex<HashMap<String, String>>,
}

impl FakeFs {
    fn new() -> Self {
        let fs = Self::default();
        fs.put_dir("/");
        fs.put_dir("/d");
        fs
    }

    fn put_dir(&self, path: &str) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: Vec::new(),
                is_dir: true,
                is_symlink: false,
                mode: 0o755,
                mtime: 1_700_000_000,
            },
        );
    }

    fn put_file(&self, path: &str, data: &[u8]) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: data.to_vec(),
                is_dir: false,
                is_symlink: false,
                mode: 0o644,
                mtime: 1_700_000_000,
            },
        );
    }

    fn put_link(&self, path: &str) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: Vec::new(),
                is_dir: false,
                is_symlink: true,
                mode: 0o777,
                mtime: 1_700_000_000,
            },
        );
    }

    /// 一个指向目录的符号链接（列目录时它的类型显示成目录，和 `FileKind` 一致）。
    fn put_dir_link(&self, path: &str, target: &str) {
        self.put_link(path);
        self.dir_links
            .lock()
            .unwrap()
            .insert(path.to_owned(), target.to_owned());
    }

    /// 把路径里经过的目录链接都换成它们的目标（最多换 40 次，和内核的限制一样）。
    fn resolve(&self, path: &str) -> String {
        let links = self.dir_links.lock().unwrap().clone();
        let mut current = path.to_owned();
        for _ in 0..40 {
            let hit = links
                .iter()
                .find(|(link, _)| current == **link || current.starts_with(&format!("{link}/")));
            match hit {
                Some((link, target)) => current = format!("{target}{}", &current[link.len()..]),
                None => return current,
            }
        }
        current
    }

    fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.nodes.lock().unwrap().get(path).map(|n| n.data.clone())
    }

    fn exists(&self, path: &str) -> bool {
        self.nodes.lock().unwrap().contains_key(path)
    }

    /// 某个目录下直接子项的名字（用来断言没有临时文件残留）。
    fn names_in(&self, dir: &str) -> Vec<String> {
        let prefix = if dir == "/" {
            "/".to_owned()
        } else {
            format!("{dir}/")
        };
        let mut names: Vec<String> = self
            .nodes
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(&prefix) && !key[prefix.len()..].contains('/'))
            .map(|key| key[prefix.len()..].to_owned())
            .collect();
        names.sort();
        names
    }

    fn fail_copy_after(&self, source_containing: &str) {
        *self.fail_copy_containing.lock().unwrap() = Some(source_containing.to_owned());
    }

    fn write_node(&self, path: &str, data: &[u8]) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: data.to_vec(),
                is_dir: false,
                is_symlink: false,
                mode: 0o644,
                mtime: 1_700_000_000,
            },
        );
    }

    fn remove_tree_sync(&self, path: &str) {
        let prefix = format!("{path}/");
        let mut nodes = self.nodes.lock().unwrap();
        nodes.retain(|key, _| key != path && !key.starts_with(&prefix));
    }

    fn copy_tree(&self, source: &str, target: &str) -> Result<(), SftpError> {
        let node = self
            .nodes
            .lock()
            .unwrap()
            .get(source)
            .cloned()
            .ok_or_else(|| SftpError::NotFound {
                path: source.to_owned(),
            })?;
        if node.is_symlink {
            // cp -a 复制链接本身：指向哪里也原样带过去。
            let link_target = self.dir_links.lock().unwrap().get(source).cloned();
            match link_target {
                Some(link_target) => self.put_dir_link(target, &link_target),
                None => self.put_link(target),
            }
            return Ok(());
        }
        if !node.is_dir {
            self.put_file(target, &node.data);
            return Ok(());
        }
        self.put_dir(target);
        let prefix = format!("{source}/");
        let children: Vec<(String, Node)> = self
            .nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix) && !key[prefix.len()..].contains('/'))
            .map(|(key, node)| (key.clone(), node.clone()))
            .collect();
        for (child, _) in children {
            let name = &child[prefix.len()..];
            self.copy_tree(&child, &format!("{target}/{name}"))?;
        }
        Ok(())
    }
}

impl CopyDataOps for FakeFs {
    fn lstat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<CopyAttrs, SftpError>> {
        Box::pin(async move {
            // 只穿过父路径上的链接，最后一段按链接本身看（lstat 的语义）。
            let real = match path.rfind('/') {
                Some(cut) if cut > 0 => {
                    format!("{}/{}", self.resolve(&path[..cut]), &path[cut + 1..])
                }
                _ => path.to_owned(),
            };
            let nodes = self.nodes.lock().unwrap();
            let node = nodes.get(&real).ok_or_else(|| SftpError::NotFound {
                path: path.to_owned(),
            })?;
            Ok(CopyAttrs {
                is_dir: node.is_dir,
                is_symlink: node.is_symlink,
                size: (!node.is_dir).then_some(node.data.len() as u64),
                mtime: Some(node.mtime),
                permissions: Some(node.mode),
            })
        })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<CopyEntry>, SftpError>> {
        Box::pin(async move {
            let real = self.resolve(path);
            let links = self.dir_links.lock().unwrap().clone();
            let nodes = self.nodes.lock().unwrap();
            let node = nodes.get(&real).ok_or_else(|| SftpError::NotFound {
                path: path.to_owned(),
            })?;
            if !node.is_dir {
                return Err(SftpError::Remote("不是文件夹".into()));
            }
            let prefix = if real == "/" {
                "/".to_owned()
            } else {
                format!("{real}/")
            };
            let mut out = Vec::new();
            for (key, child) in nodes.iter() {
                if !key.starts_with(&prefix) || key[prefix.len()..].contains('/') {
                    continue;
                }
                let name = key[prefix.len()..].to_owned();
                out.push(CopyEntry {
                    path: if path == "/" {
                        format!("/{name}")
                    } else {
                        format!("{path}/{name}")
                    },
                    name,
                    is_dir: child.is_dir || links.contains_key(key),
                    is_symlink: child.is_symlink,
                    size: (!child.is_dir).then_some(child.data.len() as u64),
                });
            }
            Ok(out)
        })
    }

    fn create_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let mut nodes = self.nodes.lock().unwrap();
            if nodes.contains_key(path) {
                return Err(SftpError::AlreadyExists {
                    path: path.to_owned(),
                });
            }
            nodes.insert(
                path.to_owned(),
                Node {
                    data: Vec::new(),
                    is_dir: true,
                    is_symlink: false,
                    mode: 0o755,
                    mtime: 1_700_000_000,
                },
            );
            Ok(())
        })
    }

    fn open_read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move {
            if !self.nodes.lock().unwrap().contains_key(path) {
                return Err(SftpError::NotFound {
                    path: path.to_owned(),
                });
            }
            let id = self.next_handle.fetch_add(1, Ordering::SeqCst);
            let handle = format!("h{id}");
            self.handles.lock().unwrap().insert(
                handle.clone(),
                Handle {
                    path: path.to_owned(),
                },
            );
            Ok(handle)
        })
    }

    fn open_write_truncate<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move {
            let mut nodes = self.nodes.lock().unwrap();
            match nodes.get(path) {
                Some(node) if node.is_dir => {
                    return Err(SftpError::Remote("目标是文件夹".into()));
                }
                _ => {}
            }
            nodes.insert(
                path.to_owned(),
                Node {
                    data: Vec::new(),
                    is_dir: false,
                    is_symlink: false,
                    mode: 0o644,
                    mtime: 1_700_000_000,
                },
            );
            let id = self.next_handle.fetch_add(1, Ordering::SeqCst);
            let handle = format!("h{id}");
            self.handles.lock().unwrap().insert(
                handle.clone(),
                Handle {
                    path: path.to_owned(),
                },
            );
            Ok(handle)
        })
    }

    fn copy_data<'a>(
        &'a self,
        read_handle: &'a str,
        write_handle: &'a str,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let handles = self.handles.lock().unwrap();
            let source = handles
                .get(read_handle)
                .ok_or_else(|| SftpError::Remote("读句柄不存在".to_owned()))?;
            let target = handles
                .get(write_handle)
                .ok_or_else(|| SftpError::Remote("写句柄不存在".to_owned()))?;
            let data = self
                .nodes
                .lock()
                .unwrap()
                .get(&source.path)
                .map(|node| node.data.clone())
                .unwrap_or_default();
            let fail = self
                .fail_copy_containing
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|needle| source.path.contains(needle));
            // 失败时先写一半（和真的 copy-data 一样，目标里会留下半截内容）。
            let written = if fail { data.len() / 2 } else { data.len() };
            if let Some(node) = self.nodes.lock().unwrap().get_mut(&target.path) {
                node.data = data[..written].to_vec();
            }
            if fail {
                return Err(SftpError::Remote("注入的 copy-data 失败".to_owned()));
            }
            Ok(())
        })
    }

    fn fstat_size<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<u64, SftpError>> {
        Box::pin(async move {
            let handles = self.handles.lock().unwrap();
            let handle = handles
                .get(handle)
                .ok_or_else(|| SftpError::Remote("句柄不存在".to_owned()))?;
            Ok(self
                .nodes
                .lock()
                .unwrap()
                .get(&handle.path)
                .map(|node| node.data.len() as u64)
                .unwrap_or(0))
        })
    }

    fn close_handle<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.handles.lock().unwrap().remove(handle);
            Ok(())
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            match self.nodes.lock().unwrap().remove(path) {
                Some(node) if !node.is_dir => Ok(()),
                _ => Err(SftpError::NotFound {
                    path: path.to_owned(),
                }),
            }
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let prefix = format!("{path}/");
            if self
                .nodes
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.starts_with(&prefix))
            {
                return Err(SftpError::Remote("目录不是空的".into()));
            }
            match self.nodes.lock().unwrap().remove(path) {
                Some(node) if node.is_dir => Ok(()),
                _ => Err(SftpError::NotFound {
                    path: path.to_owned(),
                }),
            }
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let mut nodes = self.nodes.lock().unwrap();
            if nodes.get(from).is_none() {
                return Err(SftpError::NotFound {
                    path: from.to_owned(),
                });
            }
            if !overwrite && nodes.contains_key(to) {
                return Err(SftpError::AlreadyExists {
                    path: to.to_owned(),
                });
            }
            // 把 from 及其子树一起搬过去。
            let moved: Vec<(String, Node)> = {
                let prefix = format!("{from}/");
                nodes
                    .iter()
                    .filter(|(key, _)| *key == from || key.starts_with(&prefix))
                    .map(|(key, node)| (key.clone(), node.clone()))
                    .collect()
            };
            for (key, _) in &moved {
                nodes.remove(key);
            }
            for (key, node) in moved {
                let suffix = key.strip_prefix(from).unwrap_or("");
                nodes.insert(format!("{to}{suffix}"), node);
            }
            Ok(())
        })
    }

    fn set_permissions<'a>(
        &'a self,
        path: &'a str,
        mode: u32,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            if let Some(node) = self.nodes.lock().unwrap().get_mut(path) {
                node.mode = mode;
                Ok(())
            } else {
                Err(SftpError::NotFound {
                    path: path.to_owned(),
                })
            }
        })
    }

    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move {
            let real = self.resolve(path);
            if self.nodes.lock().unwrap().contains_key(&real) {
                Ok(real)
            } else {
                Err(SftpError::NotFound {
                    path: path.to_owned(),
                })
            }
        })
    }

    fn set_mtime<'a>(&'a self, path: &'a str, mtime: u32) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            if let Some(node) = self.nodes.lock().unwrap().get_mut(path) {
                node.mtime = mtime;
                Ok(())
            } else {
                Err(SftpError::NotFound {
                    path: path.to_owned(),
                })
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FakeCpMode {
    /// 正常复制（文件 / 目录都支持）。
    Copy,
    /// 写一半目标再返回失败（模拟 cp 中途失败）。
    HalfThenFail,
    /// 一直等，直到取消标志置位（模拟大文件复制）。
    LongRunning,
}

struct FakeCp {
    fs: Arc<FakeFs>,
    mode: Mutex<FakeCpMode>,
    commands: Mutex<Vec<String>>,
}

impl FakeCp {
    fn new(fs: Arc<FakeFs>, mode: FakeCpMode) -> Self {
        Self {
            fs,
            mode: Mutex::new(mode),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

/// 从 `cp -a -- 'src' 'dst'` 里取出源和目标（测试里的路径不含单引号）。
fn cp_paths(command: &str) -> (String, String) {
    let quoted: Vec<&str> = command.split('\'').collect();
    // [cp -a -- , src,  , dst, ]
    let source = quoted.get(1).copied().unwrap_or_default().to_owned();
    let target = quoted.get(3).copied().unwrap_or_default().to_owned();
    (source, target)
}

impl CpRunner for FakeCp {
    fn remove_tree<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.fs.remove_tree_sync(path);
            Ok(true)
        })
    }

    fn exec<'a>(
        &'a self,
        command: &'a str,
        _wait: Duration,
        cancel: &'a AtomicBool,
    ) -> BoxFuture<'a, Result<CpRun, CpFailure>> {
        Box::pin(async move {
            self.commands.lock().unwrap().push(command.to_owned());
            let mode = *self.mode.lock().unwrap();
            let (source, target) = cp_paths(command);
            match mode {
                FakeCpMode::Copy => {
                    self.fs
                        .copy_tree(&source, &target)
                        .map_err(|err| CpFailure::Failed(err.to_string()))?;
                }
                FakeCpMode::HalfThenFail => {
                    // cp 先打开/截断目标，写一半，然后失败。
                    let data = self.fs.get(&source).unwrap_or_default();
                    self.fs.write_node(&target, &data[..data.len() / 2]);
                    return Ok(CpRun {
                        exit_code: Some(1),
                        stderr: "cp: 注入的失败".to_owned(),
                        timed_out: false,
                        cancelled: false,
                    });
                }
                FakeCpMode::LongRunning => {
                    for _ in 0..100 {
                        if cancel.load(Ordering::SeqCst) {
                            return Ok(CpRun {
                                exit_code: None,
                                stderr: String::new(),
                                timed_out: false,
                                cancelled: true,
                            });
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            Ok(CpRun {
                exit_code: Some(0),
                stderr: String::new(),
                timed_out: false,
                cancelled: false,
            })
        })
    }
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn options<'a>(owner: &'a str, cancel: &'a AtomicBool) -> CopyOptions<'a> {
    // 锁是进程级共享的：每个测试用自己的服务器键，免得并发跑时互相挡。
    CopyOptions::simple(owner, owner, cancel)
}

// ---------------------------------------------------------------------------
// 第 1 项：覆盖失败不能动原来的目标
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overwrite_failure_keeps_the_existing_target_and_leaves_no_temp() {
    let fs = FakeFs::new();
    fs.put_file("/d/target.txt", b"OLD CONTENT");
    fs.put_file("/d/source.txt", b"NEW CONTENT THAT IS LONGER");
    fs.fail_copy_after("/d/source.txt");
    let cancel = no_cancel();

    let result = copy_with_copy_data(
        &fs,
        "/d/source.txt",
        "/d/target.txt",
        &options("o1", &cancel),
    )
    .await;
    assert!(result.is_err(), "注入的失败应该报错：{result:?}");
    assert_eq!(
        fs.get("/d/target.txt").as_deref(),
        Some(&b"OLD CONTENT"[..]),
        "覆盖失败之后目标原来的内容必须还在"
    );
    assert_eq!(
        fs.names_in("/d"),
        vec!["source.txt", "target.txt"],
        "不能留下临时文件"
    );
}

#[tokio::test]
async fn a_failed_folder_copy_leaves_no_half_folder_behind() {
    let fs = FakeFs::new();
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"AAAA");
    fs.put_file("/src/b.txt", b"BBBBBBBBBB");
    fs.fail_copy_after("/src/b.txt");
    let cancel = no_cancel();

    let result = copy_with_copy_data(&fs, "/src", "/dst", &options("o2", &cancel)).await;
    assert!(result.is_err(), "注入的失败应该报错：{result:?}");
    assert!(!fs.exists("/dst"), "失败时目标位置不能出现半个文件夹");
    assert!(
        fs.names_in("/")
            .iter()
            .all(|name| !name.contains(".zssh-part")),
        "不能留下临时文件夹：{:?}",
        fs.names_in("/")
    );
}

#[tokio::test]
async fn a_failed_cp_does_not_touch_the_existing_target() {
    let fs = Arc::new(FakeFs::new());
    fs.put_file("/d/target.txt", b"OLD CONTENT");
    fs.put_file("/d/source.txt", b"NEW CONTENT THAT IS LONGER");
    let runner = FakeCp::new(Arc::clone(&fs), FakeCpMode::HalfThenFail);
    let cancel = no_cancel();

    let result = copy_with_cp(
        &runner,
        fs.as_ref(),
        "/d/source.txt",
        "/d/target.txt",
        Duration::from_secs(30),
        &options("o3", &cancel),
    )
    .await;
    assert!(result.is_err(), "注入的失败应该报错：{result:?}");
    assert_eq!(
        fs.get("/d/target.txt").as_deref(),
        Some(&b"OLD CONTENT"[..]),
        "cp 失败之后目标原来的内容必须还在"
    );
    assert_eq!(
        fs.names_in("/d"),
        vec!["source.txt", "target.txt"],
        "不能留下临时文件"
    );
}

struct OverwriteAll;

impl CopyConflictResolver for OverwriteAll {
    fn decide<'a>(
        &'a self,
        _info: CopyConflictInfo,
    ) -> BoxFuture<'a, Result<CopyConflictDecision, SftpError>> {
        Box::pin(async { Ok(CopyConflictDecision::Overwrite) })
    }
}

// ---------------------------------------------------------------------------
// 第 2 项：服务器端复制拿远端写锁
// ---------------------------------------------------------------------------

/// 上传任务正在写 `dir/a.txt`：往 `dir` 里合并一个文件夹（copy-data）必须被拒，
/// 而且一个文件都不写。
#[tokio::test]
async fn a_merge_into_a_folder_refuses_while_an_upload_writes_inside_it() {
    let fs = FakeFs::new();
    fs.put_dir("/src");
    fs.put_file("/src/new.txt", b"NEW");
    fs.put_dir("/dst");
    fs.put_file("/dst/keep.txt", b"KEEP");
    let server = "merge-busy-server";
    // 模拟上传任务正在写 dir/a.txt。
    let upload = TargetLocks::remote()
        .try_acquire_remote(server, "/dst/a.txt")
        .expect("上传先拿到锁");
    let cancel = no_cancel();
    let opts = CopyOptions {
        conflict: Some(&OverwriteAll),
        ..CopyOptions::simple("lock-merge", server, &cancel)
    };
    let result = copy_with_copy_data(&fs, "/src", "/dst", &opts).await;
    assert!(
        matches!(result, Err(SftpError::TargetBusy { .. })),
        "有上传在写目标文件夹里的文件时，合并必须被拒：{result:?}"
    );
    assert!(!fs.exists("/dst/new.txt"), "被拒时不能写任何东西");
    assert_eq!(fs.get("/dst/keep.txt").as_deref(), Some(&b"KEEP"[..]));

    // 上传结束之后可以。
    drop(upload);
    copy_with_copy_data(&fs, "/src", "/dst", &opts)
        .await
        .expect("上传结束之后合并");
    assert_eq!(fs.get("/dst/new.txt").as_deref(), Some(&b"NEW"[..]));
}

/// `cp` 整个文件夹时，目标前缀下面有任何路径正被别的任务写，就不开始（连 cp 都不执行）。
#[tokio::test]
async fn a_cp_of_a_folder_refuses_before_running_cp_when_the_target_tree_is_busy() {
    let fs = Arc::new(FakeFs::new());
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"AAAA");
    let runner = FakeCp::new(Arc::clone(&fs), FakeCpMode::Copy);
    let server = "cp-tree-busy-server";
    let upload = TargetLocks::remote()
        .try_acquire_remote(server, "/dst/in-progress.bin")
        .expect("上传先拿到锁");
    let cancel = no_cancel();
    let opts = options("lock-cp-tree", &cancel);
    let opts = CopyOptions { server, ..opts };
    let result = copy_with_cp(
        &runner,
        fs.as_ref(),
        "/src",
        "/dst",
        Duration::from_secs(30),
        &opts,
    )
    .await;
    assert!(
        matches!(result, Err(CpFailure::Failed(ref message)) if message.contains("另一个传输任务正在写")),
        "目标前缀被占着时 cp 不能开始：{result:?}"
    );
    assert!(runner.commands().is_empty(), "cp 命令不该被执行");
    assert!(!fs.exists("/dst"), "目标位置不能出现东西");

    drop(upload);
    copy_with_cp(
        &runner,
        fs.as_ref(),
        "/src",
        "/dst",
        Duration::from_secs(30),
        &opts,
    )
    .await
    .expect("上传结束之后可以 cp");
    assert_eq!(fs.get("/dst/a.txt").as_deref(), Some(&b"AAAA"[..]));
}

/// 单个文件覆盖：同名文件正被上传时拒绝，上传结束后可以。
/// 取消 `cp`：临时文件被清掉，目标原来的内容不动，返回 Cancelled。
#[tokio::test]
async fn a_cancelled_cp_cleans_its_temp_and_keeps_the_target() {
    let fs = Arc::new(FakeFs::new());
    fs.put_file("/d/target.txt", b"OLD CONTENT");
    fs.put_file("/d/source.txt", b"NEW CONTENT");
    let runner = FakeCp::new(Arc::clone(&fs), FakeCpMode::LongRunning);
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&cancel);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        flag.store(true, Ordering::SeqCst);
    });
    let server = "cancel-cp-server";
    let opts = CopyOptions::simple("cancel-cp", server, &cancel);
    let result = copy_with_cp(
        &runner,
        fs.as_ref(),
        "/d/source.txt",
        "/d/target.txt",
        Duration::from_secs(30),
        &opts,
    )
    .await;
    assert_eq!(result, Err(CpFailure::Cancelled));
    assert_eq!(
        fs.get("/d/target.txt").as_deref(),
        Some(&b"OLD CONTENT"[..])
    );
    assert_eq!(
        fs.names_in("/d"),
        vec!["source.txt", "target.txt"],
        "临时文件要清掉"
    );
}

#[tokio::test]
async fn a_single_file_copy_refuses_while_an_upload_writes_the_same_target() {
    let fs = FakeFs::new();
    fs.put_file("/d/source.txt", b"NEW");
    fs.put_file("/d/target.txt", b"OLD");
    let server = "single-busy-server";
    let upload = TargetLocks::remote()
        .try_acquire_remote(server, "/d/target.txt")
        .expect("上传先拿到锁");
    let cancel = no_cancel();
    let opts = CopyOptions::simple("lock-single", server, &cancel);
    let result = copy_with_copy_data(&fs, "/d/source.txt", "/d/target.txt", &opts).await;
    assert!(
        matches!(result, Err(SftpError::TargetBusy { .. })),
        "同名文件正被写时必须被拒：{result:?}"
    );
    assert_eq!(fs.get("/d/target.txt").as_deref(), Some(&b"OLD"[..]));

    drop(upload);
    copy_with_copy_data(&fs, "/d/source.txt", "/d/target.txt", &opts)
        .await
        .expect("上传结束之后可以覆盖");
    assert_eq!(fs.get("/d/target.txt").as_deref(), Some(&b"NEW"[..]));
}

// ---------------------------------------------------------------------------
// 第 4 项：没有确认 POSIX 就不执行 cp
// ---------------------------------------------------------------------------

#[test]
fn only_a_confirmed_posix_shell_may_use_cp() {
    use crate::ai::shell_kind::ShellKind;
    // 有 copy-data：先用它，和 shell 无关。
    assert_eq!(
        choose_server_copy(true, false, Some(ShellKind::Other)),
        CopyMethod::CopyData
    );
    // 没有 copy-data、远端是 POSIX：可以用 cp。
    assert_eq!(
        choose_server_copy(false, false, Some(ShellKind::Posix)),
        CopyMethod::ServerCp
    );
    // 没有 copy-data、远端不是 POSIX（Windows cmd / PowerShell、fish、认不出）：不能用 cp。
    for kind in [
        Some(ShellKind::Other),
        Some(ShellKind::Fish),
        Some(ShellKind::Unknown),
        None,
    ] {
        assert_eq!(
            choose_server_copy(false, false, kind),
            CopyMethod::Relay,
            "shell {kind:?} 不能执行 cp"
        );
    }
    // 用户点了「改为经本机中转重试」：不管探测结果都用中转。
    assert_eq!(
        choose_server_copy(true, true, Some(ShellKind::Posix)),
        CopyMethod::Relay
    );
}

// ---------------------------------------------------------------------------
// 第 5 项：合并进已有文件夹时，同名文件逐个问
// ---------------------------------------------------------------------------

struct ScriptedResolver {
    decisions: Mutex<std::collections::VecDeque<CopyConflictDecision>>,
    asked: Mutex<Vec<CopyConflictInfo>>,
}

impl ScriptedResolver {
    fn new(decisions: Vec<CopyConflictDecision>) -> Self {
        Self {
            decisions: Mutex::new(decisions.into()),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<CopyConflictInfo> {
        self.asked.lock().unwrap().clone()
    }
}

impl CopyConflictResolver for ScriptedResolver {
    fn decide<'a>(
        &'a self,
        info: CopyConflictInfo,
    ) -> BoxFuture<'a, Result<CopyConflictDecision, SftpError>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(info);
            let decision = self
                .decisions
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(CopyConflictDecision::Skip);
            Ok(decision)
        })
    }
}

#[tokio::test]
async fn merging_asks_for_each_same_name_file_and_honours_skip_and_overwrite() {
    let fs = FakeFs::new();
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"NEW A");
    fs.put_file("/src/b.txt", b"NEW B");
    fs.put_file("/src/c.txt", b"NEW C");
    fs.put_dir("/dst");
    fs.put_file("/dst/a.txt", b"OLD A");
    fs.put_file("/dst/b.txt", b"OLD B");
    let resolver = ScriptedResolver::new(vec![
        CopyConflictDecision::Skip,
        CopyConflictDecision::Overwrite,
    ]);
    let cancel = no_cancel();
    let opts = CopyOptions {
        conflict: Some(&resolver),
        ..options("merge-ask", &cancel)
    };
    let stats = copy_with_copy_data(&fs, "/src", "/dst", &opts)
        .await
        .expect("合并");
    assert_eq!(stats.skipped_files, 1);
    // 只问了两个同名文件（c.txt 是新的，不问）。
    let asked = resolver.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].target.ends_with("/a.txt"));
    assert!(asked[1].target.ends_with("/b.txt"));
    // 跳过的不动，覆盖的换成新内容，新的照常复制。
    assert_eq!(fs.get("/dst/a.txt").as_deref(), Some(&b"OLD A"[..]));
    assert_eq!(fs.get("/dst/b.txt").as_deref(), Some(&b"NEW B"[..]));
    assert_eq!(fs.get("/dst/c.txt").as_deref(), Some(&b"NEW C"[..]));
    assert!(
        fs.names_in("/dst")
            .iter()
            .all(|name| !name.contains(".zssh-part")),
        "不能留下临时文件"
    );
}

#[tokio::test]
async fn merging_can_rename_a_same_name_file() {
    let fs = FakeFs::new();
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"NEW");
    fs.put_dir("/dst");
    fs.put_file("/dst/a.txt", b"OLD");
    let resolver = ScriptedResolver::new(vec![CopyConflictDecision::Rename(
        "/dst/a - 副本.txt".to_owned(),
    )]);
    let cancel = no_cancel();
    let opts = CopyOptions {
        conflict: Some(&resolver),
        ..options("merge-rename", &cancel)
    };
    copy_with_copy_data(&fs, "/src", "/dst", &opts)
        .await
        .expect("合并");
    assert_eq!(fs.get("/dst/a.txt").as_deref(), Some(&b"OLD"[..]));
    assert_eq!(fs.get("/dst/a - 副本.txt").as_deref(), Some(&b"NEW"[..]));
}

// ---------------------------------------------------------------------------
// C09：核对 cp 副本时不跟随目录链接，并且有遍历预算
// ---------------------------------------------------------------------------

/// 源里有指回自己的目录链接、指向树外的目录链接：核对按链接本身算，不进去，
/// 不会被循环困住，也不会数到树外面（以前这里会一直转下去）。
#[tokio::test]
async fn cp_verification_counts_directory_links_without_following_them() {
    let fs = Arc::new(FakeFs::new());
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"AAAA");
    fs.put_dir("/src/sub");
    fs.put_file("/src/sub/b.txt", b"BB");
    fs.put_dir_link("/src/sub/loop", "/src");
    fs.put_dir("/outside");
    fs.put_file("/outside/big.bin", &[0u8; 64]);
    fs.put_dir_link("/src/out", "/outside");
    let runner = FakeCp::new(Arc::clone(&fs), FakeCpMode::Copy);
    let cancel = no_cancel();

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        copy_with_cp(
            &runner,
            fs.as_ref(),
            "/src",
            "/dst",
            Duration::from_secs(30),
            &options("c09-links", &cancel),
        ),
    )
    .await
    .expect("核对不能被循环链接困住");
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(fs.get("/dst/sub/b.txt").as_deref(), Some(&b"BB"[..]));

    // 直接数：两个文件 + 两个链接，链接按自己算，树外的 big.bin 不算进来。
    let totals = tree_totals(fs.as_ref(), "/src", &cancel, TreeBudget::DEFAULT)
        .await
        .expect("数源树");
    assert_eq!(totals, (4, 6));
}

/// 条目数、层数超过预算：按"无法确认"报错，不继续往下列；cp 的副本也不会被当成核对通过。
#[tokio::test]
async fn tree_totals_gives_up_when_the_budget_runs_out() {
    let fs = Arc::new(FakeFs::new());
    fs.put_dir("/wide");
    for index in 0..5 {
        fs.put_file(&format!("/wide/f{index}"), b"x");
    }
    fs.put_dir("/deep");
    fs.put_dir("/deep/1");
    fs.put_dir("/deep/1/2");
    fs.put_dir("/deep/1/2/3");
    let cancel = no_cancel();

    let few = TreeBudget {
        max_entries: 3,
        max_depth: MAX_TREE_DEPTH,
    };
    let err = tree_totals(fs.as_ref(), "/wide", &cancel, few)
        .await
        .expect_err("条目超出预算");
    assert!(err.to_string().contains("无法确认"), "{err}");
    assert_eq!(
        tree_totals(fs.as_ref(), "/wide", &cancel, TreeBudget::DEFAULT)
            .await
            .expect("预算够时照常数"),
        (5, 5)
    );

    let shallow = TreeBudget {
        max_entries: MAX_VERIFY_ENTRIES,
        max_depth: 2,
    };
    let err = tree_totals(fs.as_ref(), "/deep", &cancel, shallow)
        .await
        .expect_err("层数超出预算");
    assert!(err.to_string().contains("无法确认"), "{err}");
}

// ---------------------------------------------------------------------------
// C21：经符号链接粘贴回源树里要拒绝；枚举源时跳过自己的临时文件夹；递归有上限
// ---------------------------------------------------------------------------

/// 目标文件夹 `/d/alias` 是指向源树里 `/src/sub` 的链接：按字符串看不在源里面，
/// 按真实路径看就是复制进自己。copy-data 和 cp 都要在动手之前拒绝，一个字节都不写。
#[tokio::test]
async fn copying_into_a_link_that_points_back_into_the_source_is_refused() {
    let fs = Arc::new(FakeFs::new());
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"AAAA");
    fs.put_dir("/src/sub");
    fs.put_dir_link("/d/alias", "/src/sub");
    let cancel = no_cancel();
    assert!(
        !is_same_or_inside("/src", "/d/alias/src"),
        "字符串比较看不出来"
    );

    let result = copy_with_copy_data(
        fs.as_ref(),
        "/src",
        "/d/alias/src",
        &options("c21-copy-data", &cancel),
    )
    .await;
    assert!(
        matches!(result, Err(SftpError::InvalidInput(ref message)) if message.contains("/src/sub/src")),
        "{result:?}"
    );
    assert!(fs.names_in("/src/sub").is_empty(), "不能建出任何东西");
    assert!(fs.names_in("/d").iter().all(|name| name == "alias"));

    let runner = FakeCp::new(Arc::clone(&fs), FakeCpMode::Copy);
    let result = copy_with_cp(
        &runner,
        fs.as_ref(),
        "/src",
        "/d/alias/src",
        Duration::from_secs(30),
        &options("c21-cp", &cancel),
    )
    .await;
    assert!(
        matches!(result, Err(CpFailure::Failed(ref message)) if message.contains("它自己")),
        "{result:?}"
    );
    assert!(runner.commands().is_empty(), "cp 命令不该被执行");
    assert!(fs.names_in("/src/sub").is_empty());

    // 源路径的上级走了别名（`/d/via` 指向 `/real`）也一样认得出来。
    fs.put_dir("/real");
    fs.put_dir("/real/tree");
    fs.put_dir("/real/tree/inner");
    fs.put_dir_link("/d/via", "/real");
    let result = ensure_not_into_itself(fs.as_ref(), "/d/via/tree", "/real/tree/inner/copy").await;
    assert!(
        matches!(result, Err(SftpError::InvalidInput(_))),
        "{result:?}"
    );
    // 源本身就是一个链接：两种办法都不跟随它（copy-data 跳过、cp -a 复制链接本身），放行。
    fs.put_dir_link("/d/src-link", "/src");
    ensure_not_into_itself(fs.as_ref(), "/d/src-link", "/src/sub/copy")
        .await
        .expect("源是链接时不会递归");

    // 粘贴前按"目标文件夹"问：经链接指回源树的文件夹要拒绝，源所在的文件夹（起副本名）放行。
    let result = ensure_folder_not_inside(fs.as_ref(), "/src", "/d/alias").await;
    assert!(
        matches!(result, Err(SftpError::InvalidInput(_))),
        "{result:?}"
    );
    ensure_folder_not_inside(fs.as_ref(), "/src", "/")
        .await
        .expect("粘贴到源所在的文件夹");

    // 不相干的目标照常放行；解析不了真实位置就按"无法确认"拒绝。
    ensure_not_into_itself(fs.as_ref(), "/src", "/d/copy")
        .await
        .expect("和源不相干的目标");
    let result = ensure_not_into_itself(fs.as_ref(), "/src", "/nowhere/copy").await;
    assert!(
        matches!(result, Err(SftpError::Remote(ref message)) if message.contains("无法确认")),
        "{result:?}"
    );
}

/// 就算临时文件夹落在了源树里（检查之后才出现的别名），枚举源时也要跳过它，
/// 不能把刚复制出来的东西当成源继续复制（以前会一层套一层地长下去）。
#[tokio::test]
async fn copying_a_folder_never_copies_its_own_temp_folder() {
    let fs = FakeFs::new();
    fs.put_dir("/src");
    fs.put_file("/src/a.txt", b"AAAA");
    fs.put_dir("/src/sub");
    fs.put_file("/src/sub/b.txt", b"BB");
    let cancel = no_cancel();
    let opts = options("c21-own-temp", &cancel);
    let attrs = fs.lstat("/src").await.unwrap();
    let mut stats = CopyDataStats::default();

    // 绕过入口检查，直接让目标落在源树里。
    copy_dir_atomic(&fs, "/src", "/src/sub/copy", &attrs, &opts, &mut stats)
        .await
        .expect("复制");
    assert_eq!(fs.get("/src/sub/copy/a.txt").as_deref(), Some(&b"AAAA"[..]));
    assert_eq!(
        fs.get("/src/sub/copy/sub/b.txt").as_deref(),
        Some(&b"BB"[..])
    );
    assert_eq!(fs.names_in("/src/sub/copy/sub"), vec!["b.txt"]);
    assert_eq!(stats.files, 2);
    let leftovers: Vec<String> = fs
        .nodes
        .lock()
        .unwrap()
        .keys()
        .filter(|key| key.contains(".zssh-part"))
        .cloned()
        .collect();
    assert!(leftovers.is_empty(), "不能留下临时文件：{leftovers:?}");
}

/// 嵌套超过上限时停下来报错（并清掉临时文件夹），不无止境地往下走。
#[tokio::test]
async fn copying_stops_at_the_depth_limit() {
    let fs = FakeFs::new();
    let mut path = "/src".to_owned();
    fs.put_dir(&path);
    for _ in 0..=MAX_COPY_DEPTH {
        path.push_str("/d");
        fs.put_dir(&path);
    }
    let cancel = no_cancel();
    let result = copy_with_copy_data(&fs, "/src", "/dst", &options("c21-depth", &cancel)).await;
    assert!(
        matches!(result, Err(SftpError::Remote(ref message)) if message.contains("层")),
        "{result:?}"
    );
    assert!(!fs.exists("/dst"));
    assert!(fs
        .names_in("/")
        .iter()
        .all(|name| !name.contains(".zssh-part")));
}
