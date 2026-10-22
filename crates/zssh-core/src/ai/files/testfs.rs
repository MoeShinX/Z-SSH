//! 文件工具的内存文件系统（只给测试用）：可注入失败，记录改动操作的顺序。

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;

#[derive(Clone)]
pub(crate) struct Node {
    data: Vec<u8>,
    mode: u32,
    kind: FsKind,
    mtime: u32,
    uid: u32,
    gid: u32,
}

#[derive(Default)]
pub(crate) struct MemFs {
    pub(crate) nodes: Mutex<BTreeMap<String, Node>>,
    pub(crate) links: Mutex<BTreeMap<String, String>>,
    /// 按顺序记录改动操作：`write <path>` / `rename <a> <b>` / `remove <path>`。
    log: Mutex<Vec<String>>,
    /// 写这类路径时失败（子串匹配）。
    pub(crate) fail_write_containing: Mutex<Option<String>>,
    /// 写这类路径时只写一半就失败（模拟中途断线）。
    pub(crate) half_write_containing: Mutex<Option<String>>,
    pub(crate) fail_rename: Mutex<bool>,
    /// 新写出来的文件用这套属主（模拟"临时文件是登录用户建的"）。
    pub(crate) new_file_owner: Mutex<Option<(u32, u32)>>,
    pub(crate) fail_set_owner: Mutex<bool>,
    /// 删这类路径时失败（子串匹配）。
    pub(crate) fail_remove_containing: Mutex<Option<String>>,
    /// 模拟"没有 posix-rename 的服务器"：覆盖改名走备用名三步（同一份代码 `sftp::replace`）。
    pub(crate) no_posix_rename: Mutex<bool>,
    /// 三步里普通改名被调用的次数（从 1 数起）。
    rename_calls: Mutex<usize>,
    /// 第几次普通改名失败（服务器拒绝，改名没发生）；可以多个。
    pub(crate) fail_rename_calls: Mutex<Vec<usize>>,
    /// 第几次普通改名时连接断开；`true` = 断开前改名其实已经发生。
    pub(crate) disconnect_rename_call: Mutex<Option<(usize, bool)>>,
    /// 三步里的"删备用文件"失败。
    pub(crate) fail_backup_remove: Mutex<bool>,
    /// 这台"服务器"的身份；设了才会按它加远端目标锁。
    pub(crate) server_key: Mutex<Option<String>>,
    /// `set_owner` 返回成功但其实没生效（模拟服务器忽略 setstat）。
    pub(crate) ignore_set_owner: Mutex<bool>,
}

impl MemFs {
    pub(crate) fn new() -> MemFs {
        let fs = MemFs::default();
        fs.put_dir("/");
        fs.put_dir("/home/u");
        fs
    }

    pub(crate) fn put_dir(&self, path: &str) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: Vec::new(),
                mode: 0o755,
                kind: FsKind::Dir,
                mtime: 1_760_000_000,
                uid: 1000,
                gid: 1000,
            },
        );
    }

    pub(crate) fn put(&self, path: &str, data: &[u8], mode: u32) {
        self.nodes.lock().unwrap().insert(
            path.to_owned(),
            Node {
                data: data.to_vec(),
                mode,
                kind: FsKind::File,
                mtime: 1_760_000_000,
                uid: 1000,
                gid: 1000,
            },
        );
    }

    /// 把已有文件的属主改成别的值（模拟"原文件属于 root 或别的用户"）。
    pub(crate) fn put_owner(&self, path: &str, uid: u32, gid: u32) {
        if let Some(node) = self.nodes.lock().unwrap().get_mut(path) {
            node.uid = uid;
            node.gid = gid;
        }
    }

    pub(crate) fn owner_of(&self, path: &str) -> Option<(u32, u32)> {
        self.nodes
            .lock()
            .unwrap()
            .get(path)
            .map(|node| (node.uid, node.gid))
    }

    pub(crate) fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.nodes.lock().unwrap().get(path).map(|n| n.data.clone())
    }

    pub(crate) fn mode_of(&self, path: &str) -> Option<u32> {
        self.nodes.lock().unwrap().get(path).map(|n| n.mode)
    }

    pub(crate) fn names_in(&self, dir: &str) -> Vec<String> {
        let prefix = if dir == "/" {
            "/".to_owned()
        } else {
            format!("{dir}/")
        };
        self.nodes
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(&prefix) && !k[prefix.len()..].contains('/') && *k != dir)
            .cloned()
            .collect()
    }

    pub(crate) fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub(crate) fn resolved(&self, path: &str) -> String {
        self.links
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_owned())
    }
}

impl RemoteFs for MemFs {
    fn server_key(&self) -> Option<String> {
        self.server_key.lock().unwrap().clone()
    }

    fn home<'a>(&'a self) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Ok("/home/u".to_owned()) })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<FsStat>, String>> {
        Box::pin(async move {
            let real = self.resolved(path);
            Ok(self.nodes.lock().unwrap().get(&real).map(|n| FsStat {
                kind: n.kind,
                size: n.data.len() as u64,
                mtime: Some(n.mtime),
                mode: Some(n.mode),
                uid: Some(n.uid),
                gid: Some(n.gid),
            }))
        })
    }

    fn realpath<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Ok(self.resolved(path)) })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FsEntry>, String>> {
        Box::pin(async move {
            // names_in 自己要锁 nodes，先取名字再锁，别在同一把锁上重入。
            let names = self.names_in(path);
            let nodes = self.nodes.lock().unwrap();
            Ok(names
                .into_iter()
                .filter_map(|full| {
                    let node = nodes.get(&full)?;
                    Some(FsEntry {
                        name: full.rsplit('/').next().unwrap_or("").to_owned(),
                        kind: node.kind,
                        size: Some(node.data.len() as u64),
                        mtime: Some(node.mtime),
                        mode: Some(node.mode),
                        link_target: None,
                    })
                })
                .collect())
        })
    }

    fn read_range<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let data = self.get(&self.resolved(path)).ok_or("不存在")?;
            let start = (offset as usize).min(data.len());
            let end = (start + len).min(data.len());
            Ok(data[start..end].to_vec())
        })
    }

    fn read_all<'a>(&'a self, path: &'a str, limit: u64) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let data = self.get(&self.resolved(path)).ok_or("不存在")?;
            if data.len() as u64 > limit {
                return Err("超过上限".to_owned());
            }
            Ok(data)
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(format!("write {path}"));
            if let Some(pattern) = self.fail_write_containing.lock().unwrap().as_deref() {
                if path.contains(pattern) {
                    return Err("磁盘写入失败".to_owned());
                }
            }
            let (uid, gid) = self.new_file_owner.lock().unwrap().unwrap_or((1000, 1000));
            if let Some(pattern) = self.half_write_containing.lock().unwrap().clone() {
                if path.contains(&pattern) {
                    self.nodes.lock().unwrap().insert(
                        path.to_owned(),
                        Node {
                            data: data[..data.len() / 2].to_vec(),
                            mode: mode.unwrap_or(0o644),
                            kind: FsKind::File,
                            mtime: 1_760_000_001,
                            uid,
                            gid,
                        },
                    );
                    return Err("连接断了".to_owned());
                }
            }
            let mut nodes = self.nodes.lock().unwrap();
            // 往已经存在的文件里写（截断重写）：属主和权限不变——和真实的文件系统一样。
            let (uid, gid, mode) = match nodes.get(path) {
                Some(existing) if existing.kind == FsKind::File => {
                    (existing.uid, existing.gid, mode.unwrap_or(existing.mode))
                }
                _ => (uid, gid, mode.unwrap_or(0o644)),
            };
            nodes.insert(
                path.to_owned(),
                Node {
                    data: data.to_vec(),
                    mode,
                    kind: FsKind::File,
                    mtime: 1_760_000_001,
                    uid,
                    gid,
                },
            );
            Ok(())
        })
    }

    fn rename_over<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> BoxFuture<'a, Result<(), RenameFailure>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(format!("rename {from} {to}"));
            if *self.fail_rename.lock().unwrap() {
                return Err(RenameFailure::untouched("改名被拒绝"));
            }
            if *self.no_posix_rename.lock().unwrap() {
                return crate::sftp::replace::replace_with_backup(&MemOps(self), from, to, "tok1")
                    .await
                    .map_err(|err| match err {
                        SftpError::ReplaceFailed { state, message } => {
                            RenameFailure { state, message }
                        }
                        other => RenameFailure::untouched(other.to_string()),
                    });
            }
            let mut nodes = self.nodes.lock().unwrap();
            let node = nodes
                .remove(from)
                .ok_or_else(|| RenameFailure::untouched("源不存在"))?;
            nodes.insert(to.to_owned(), node);
            Ok(())
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(format!("remove {path}"));
            if let Some(pattern) = self.fail_remove_containing.lock().unwrap().as_deref() {
                if path.contains(pattern) {
                    return Err("删除被拒绝".to_owned());
                }
            }
            self.nodes.lock().unwrap().remove(path);
            Ok(())
        })
    }

    fn set_owner<'a>(
        &'a self,
        path: &'a str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(format!(
                "owner {path} {} {}",
                uid.unwrap_or(0),
                gid.unwrap_or(0)
            ));
            if *self.fail_set_owner.lock().unwrap() {
                return Err("权限不够".to_owned());
            }
            if *self.ignore_set_owner.lock().unwrap() {
                return Ok(());
            }
            let mut nodes = self.nodes.lock().unwrap();
            let node = nodes.get_mut(path).ok_or("不存在")?;
            if let Some(uid) = uid {
                node.uid = uid;
            }
            if let Some(gid) = gid {
                node.gid = gid;
            }
            Ok(())
        })
    }
}

/// 备用名三步用到的服务器操作：普通改名（目标存在就失败）+ 可注入的失败。
struct MemOps<'a>(&'a MemFs);

impl crate::sftp::replace::RenameOps for MemOps<'_> {
    fn exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, SftpError>> {
        Box::pin(async move { Ok(self.0.nodes.lock().unwrap().contains_key(path)) })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let call = {
                let mut calls = self.0.rename_calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            self.0
                .log
                .lock()
                .unwrap()
                .push(format!("plain-rename#{call} {from} {to}"));
            if self.0.fail_rename_calls.lock().unwrap().contains(&call) {
                return Err(SftpError::RemoteStatus {
                    code: 4,
                    message: "Failure".to_owned(),
                });
            }
            let disconnect = *self.0.disconnect_rename_call.lock().unwrap();
            let mut nodes = self.0.nodes.lock().unwrap();
            let apply = |nodes: &mut BTreeMap<String, Node>| -> Result<(), SftpError> {
                if nodes.contains_key(to) {
                    // 普通 rename：目标存在就失败。
                    return Err(SftpError::RemoteStatus {
                        code: 4,
                        message: "Failure".to_owned(),
                    });
                }
                let node = nodes.remove(from).ok_or_else(|| SftpError::NotFound {
                    path: from.to_owned(),
                })?;
                nodes.insert(to.to_owned(), node);
                Ok(())
            };
            match disconnect {
                Some((n, applied)) if n == call => {
                    if applied {
                        let _ = apply(&mut nodes);
                    }
                    Err(SftpError::Disconnected("连接断了（测试注入）".to_owned()))
                }
                _ => apply(&mut nodes),
            }
        })
    }

    fn remove<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.0.log.lock().unwrap().push(format!("remove {path}"));
            if *self.0.fail_backup_remove.lock().unwrap() {
                return Err(SftpError::RemoteStatus {
                    code: 4,
                    message: "Failure".to_owned(),
                });
            }
            self.0.nodes.lock().unwrap().remove(path);
            Ok(())
        })
    }
}
