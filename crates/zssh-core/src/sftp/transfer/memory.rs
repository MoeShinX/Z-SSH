//! 内存里的 [`FileEndpoint`] 替身（**只在测试里用**）。
//!
//! 传输引擎的边界情况（断线、短读、部分写、目标已有临时文件）在真实链路上很难稳定
//! 复现，这里用一个可控的内存实现把它们造出来：单测因此不需要服务器、也不靠运气。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::endpoint::{
    part_name, EndpointFuture, FileEndpoint, LocalEndpoint, ReadAt, WriteAt, PART_SUFFIX,
};
use crate::sftp::error::SftpError;

/// 往内存文件系统写一段（超出就补零）。
fn write_into(fs: &MemoryFs, path: &str, offset: u64, data: &[u8]) {
    let mut files = fs.files.lock().expect("锁");
    let entry = files.entry(path.to_owned()).or_default();
    let end = offset as usize + data.len();
    if entry.len() < end {
        entry.resize(end, 0);
    }
    entry[offset as usize..end].copy_from_slice(data);
}

/// 内存里的"文件系统"：路径 → 内容。
#[derive(Default)]
pub(crate) struct MemoryFs {
    pub(crate) files: Mutex<HashMap<String, Vec<u8>>>,
    /// 下一次收尾（commit）之前，"别的程序"先在这个路径建出这份内容（一次性）：
    /// 模拟传输期间目标位置冒出同名文件（C05）。
    pub(crate) appear_on_commit: Mutex<Option<(String, Vec<u8>)>>,
}

/// 一个内存端点（源或目标）。
///
/// 克隆出来的端点共享同一份内存（同一个文件系统），测试里可以同时当源和目标用。
#[derive(Clone)]
pub struct MemoryEndpoint {
    fs: Arc<MemoryFs>,
    path: String,
    /// 任务标识（和 `RemoteEndpoint` 一样写进临时文件名；空 = 旧写法）。
    owner: String,
    /// 写够这么多字节之后开始报"断线"（模拟传一半断网）。
    fail_after: Arc<AtomicU64>,
    /// 报过一次断线没有（只断一次，方便测"重连之后接着传"）。
    failed_once: Arc<AtomicBool>,
    /// 每次写多少字节就报一个"磁盘满"（模拟本地磁盘满）。
    no_space_after: Arc<AtomicU64>,
    /// 读的限制（模拟 SFTP 服务器返回比请求少的字节、或者源文件中途变短）。
    read_limits: Arc<ReadLimits>,
}

/// 读这一侧的失真注入。
pub(crate) struct ReadLimits {
    /// 接下来的几次读各自最多返回多少字节（用完一个就弹出一个）。
    scripted: Mutex<std::collections::VecDeque<usize>>,
    /// 每次读最多返回多少字节（`u64::MAX` = 不限）。
    every_read_cap: AtomicU64,
    /// 源文件"实际"只有这么长：stat 说更长，但读到这里就是文件末尾。
    ends_at: AtomicU64,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            scripted: Mutex::new(std::collections::VecDeque::new()),
            every_read_cap: AtomicU64::new(u64::MAX),
            ends_at: AtomicU64::new(u64::MAX),
        }
    }
}

impl ReadLimits {
    fn clip(&self, offset: u64, wanted: usize) -> usize {
        let mut len = wanted;
        let cap = self.every_read_cap.load(Ordering::SeqCst);
        if cap != u64::MAX {
            len = len.min(cap as usize);
        }
        if let Some(scripted) = self.scripted.lock().expect("锁").pop_front() {
            len = len.min(scripted);
        }
        let ends_at = self.ends_at.load(Ordering::SeqCst);
        if ends_at != u64::MAX {
            len = len.min(ends_at.saturating_sub(offset) as usize);
        }
        len
    }
}

impl MemoryEndpoint {
    pub fn new(path: &str, contents: Vec<u8>) -> Self {
        let fs = Arc::new(MemoryFs::default());
        fs.files
            .lock()
            .expect("锁")
            .insert(path.to_owned(), contents);
        Self {
            fs,
            path: path.to_owned(),
            owner: String::new(),
            fail_after: Arc::new(AtomicU64::new(u64::MAX)),
            failed_once: Arc::new(AtomicBool::new(false)),
            no_space_after: Arc::new(AtomicU64::new(u64::MAX)),
            read_limits: Arc::new(ReadLimits::default()),
        }
    }

    /// 接下来的几次读各自最多返回这么多字节（模拟偶尔的短读）。
    pub fn short_reads(&self, caps: &[usize]) {
        self.read_limits
            .scripted
            .lock()
            .expect("锁")
            .extend(caps.iter().copied());
    }

    /// 每次读最多返回 `cap` 字节（模拟服务器单次返回上限比请求块小）。
    pub fn cap_every_read(&self, cap: u64) {
        self.read_limits.every_read_cap.store(cap, Ordering::SeqCst);
    }

    /// 源文件读到 `len` 字节就是末尾（模拟传输过程中源文件被改短）。
    pub fn source_ends_at(&self, len: u64) {
        self.read_limits.ends_at.store(len, Ordering::SeqCst);
    }

    /// 和另一个端点共享同一份内存（同一个目录里的临时文件要用同一份）。
    ///
    /// 失败注入（断线 / 磁盘满）也跟着共享：`MemoryEndpoints::fail_remote_write_after`
    /// 注入的是"远端"这一侧，pair 出来的目标端点要能吃到它才有意义。
    pub fn with_fs(fs_owner: &MemoryEndpoint, path: &str) -> Self {
        Self {
            fs: Arc::clone(&fs_owner.fs),
            path: path.to_owned(),
            owner: String::new(),
            fail_after: Arc::clone(&fs_owner.fail_after),
            failed_once: Arc::clone(&fs_owner.failed_once),
            no_space_after: Arc::clone(&fs_owner.no_space_after),
            read_limits: Arc::clone(&fs_owner.read_limits),
        }
    }

    /// 属于某个任务（临时文件名带任务标识，和 `RemoteEndpoint` 一致）。
    pub fn with_task_owner(mut self, owner: &str) -> Self {
        self.owner = owner.to_owned();
        self
    }

    /// 这个端点的临时文件路径。
    fn part_key(&self) -> String {
        part_name(&self.path, &self.owner)
    }

    /// 设置"写到第 N 字节就断线"（只断一次）。
    pub fn fail_write_after(&self, bytes: u64) {
        self.fail_after.store(bytes, Ordering::SeqCst);
    }

    /// 设置"写到第 N 字节就报磁盘满"（不停机）。
    pub fn no_space_after(&self, bytes: u64) {
        self.no_space_after.store(bytes, Ordering::SeqCst);
    }

    /// 已经写过多少字节（累计，含失败前写的）。
    pub fn current_len(&self) -> u64 {
        self.read_file(&self.path).len() as u64
    }

    /// 临时文件长度。
    pub fn partial_len_sync(&self) -> u64 {
        self.read_file(&self.part_key()).len() as u64
    }

    /// 最终文件内容。
    pub fn contents(&self) -> Vec<u8> {
        self.read_file(&self.path)
    }

    /// 当成 [`FileEndpoint`] 用（引用式：不消费自己，测试里可以反复拿）。
    pub fn as_endpoint(&self) -> Arc<dyn FileEndpoint> {
        Arc::new(self.clone())
    }

    fn read_file(&self, path: &str) -> Vec<u8> {
        self.fs
            .files
            .lock()
            .expect("锁")
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// 这个路径在不在（空文件也算"在"）。
    fn present(&self, path: &str) -> bool {
        self.fs.files.lock().expect("锁").contains_key(path)
    }

    /// 预置一份临时文件（模拟"上次传到一半"）。
    pub fn seed_partial(&self, len: u64, byte: u8) {
        let part = self.part_key();
        write_into(&self.fs, &part, 0, &vec![byte; len as usize]);
    }

    /// 改临时文件里的一段字节（造"对不上"的场景用）。
    pub fn corrupt_partial(&self, offset: u64, byte: u8) {
        let part = self.part_key();
        let mut files = self.fs.files.lock().expect("锁");
        if let Some(data) = files.get_mut(&part) {
            for slot in data.iter_mut().skip(offset as usize) {
                *slot = byte;
            }
        }
    }

    /// 预置最终文件的内容。
    pub fn seed_final(&self, contents: &[u8]) {
        let path = self.path.clone();
        write_into(&self.fs, &path, 0, contents);
    }

    fn write_file(&self, path: &str, offset: u64, data: &[u8]) {
        write_into(&self.fs, path, offset, data);
    }

    fn remove_file(&self, path: &str) {
        self.fs.files.lock().expect("锁").remove(path);
    }
}

impl FileEndpoint for MemoryEndpoint {
    fn describe(&self) -> String {
        self.path.clone()
    }

    fn size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            Ok(if self.present(&self.path) {
                Some(self.read_file(&self.path).len() as u64)
            } else {
                None
            })
        })
    }

    fn open_read<'a>(&'a self) -> EndpointFuture<'a, Result<Arc<dyn ReadAt>, SftpError>> {
        Box::pin(async move {
            if !self.present(&self.path) {
                return Err(SftpError::NotFound {
                    path: self.path.clone(),
                });
            }
            let data = self.read_file(&self.path);
            Ok(Arc::new(MemoryReader {
                data,
                limits: Arc::clone(&self.read_limits),
            }) as Arc<dyn ReadAt>)
        })
    }

    fn open_write<'a>(
        &'a self,
        resume_len: u64,
    ) -> EndpointFuture<'a, Result<Arc<dyn WriteAt>, SftpError>> {
        Box::pin(async move {
            let part = self.part_key();
            if resume_len == 0 {
                self.remove_file(&part);
                // 和真实文件系统一致：打开写就先有文件（空文件也要存在）。
                write_into(&self.fs, &part, 0, &[]);
            } else {
                let existing = self.read_file(&part);
                if !self.present(&part) || (existing.len() as u64) < resume_len {
                    return Err(SftpError::InvalidInput(format!(
                        "临时文件只有 {} 字节，不能从 {resume_len} 续",
                        existing.len()
                    )));
                }
            }
            Ok(Arc::new(MemoryWriter {
                fs: Arc::clone(&self.fs),
                path: part,
                written: Arc::new(AtomicU64::new(resume_len)),
                fail_after: Arc::clone(&self.fail_after),
                failed_once: Arc::clone(&self.failed_once),
                no_space_after: Arc::clone(&self.no_space_after),
            }) as Arc<dyn WriteAt>)
        })
    }

    fn commit<'a>(
        &'a self,
        _total: u64,
        overwrite: bool,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let appear = self.fs.appear_on_commit.lock().expect("锁").take();
            if let Some((path, contents)) = appear {
                write_into(&self.fs, &path, 0, &contents);
            }
            let part = self.part_key();
            if !self.present(&part) {
                return Err(SftpError::NotFound { path: part });
            }
            let data = self.read_file(&part);
            if !overwrite && self.present(&self.path) {
                return Err(SftpError::AlreadyExists {
                    path: self.path.clone(),
                });
            }
            self.remove_file(&self.path);
            self.write_file(&self.path, 0, &data);
            self.remove_file(&part);
            Ok(())
        })
    }

    fn existing_size<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u64>, SftpError>> {
        Box::pin(async move {
            Ok(if self.present(&self.path) {
                Some(self.read_file(&self.path).len() as u64)
            } else {
                None
            })
        })
    }

    fn existing_mtime<'a>(&'a self) -> EndpointFuture<'a, Result<Option<u32>, SftpError>> {
        Box::pin(async move {
            Ok(if self.present(&self.path) {
                Some(1_700_000_000)
            } else {
                None
            })
        })
    }

    fn remove_existing<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.remove_file(&self.path);
            Ok(())
        })
    }

    fn remove_partial<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.remove_file(&self.part_key());
            Ok(())
        })
    }

    fn partial_len<'a>(&'a self) -> EndpointFuture<'a, Result<u64, SftpError>> {
        Box::pin(async move { Ok(self.partial_len_sync()) })
    }

    fn truncate_partial<'a>(&'a self, len: u64) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let part = self.part_key();
            let mut files = self.fs.files.lock().expect("锁");
            if let Some(data) = files.get_mut(&part) {
                data.resize(len as usize, 0);
            }
            Ok(())
        })
    }

    fn read_partial<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Option<Vec<u8>>, SftpError>> {
        Box::pin(async move {
            let data = self.read_file(&self.part_key());
            if (data.len() as u64) < offset + u64::from(len) {
                return Ok(None);
            }
            Ok(Some(
                data[offset as usize..offset as usize + len as usize].to_vec(),
            ))
        })
    }

    fn set_attributes<'a>(
        &'a self,
        _mtime: Option<u32>,
        _permissions: Option<u32>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { Ok(()) })
    }

    fn attributes<'a>(
        &'a self,
    ) -> EndpointFuture<'a, Result<(Option<u32>, Option<u32>), SftpError>> {
        Box::pin(async move { Ok((Some(1_700_000_000), Some(0o644))) })
    }
}

struct MemoryReader {
    data: Vec<u8>,
    limits: Arc<ReadLimits>,
}

impl ReadAt for MemoryReader {
    fn read_at<'a>(
        &'a self,
        offset: u64,
        len: u32,
    ) -> EndpointFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(async move {
            let start = offset as usize;
            if start >= self.data.len() {
                return Ok(Vec::new());
            }
            let end = (start + len as usize).min(self.data.len());
            let end = start + self.limits.clip(offset, end - start);
            Ok(self.data[start..end].to_vec())
        })
    }

    fn close<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { Ok(()) })
    }
}

struct MemoryWriter {
    fs: Arc<MemoryFs>,
    path: String,
    written: Arc<AtomicU64>,
    fail_after: Arc<AtomicU64>,
    failed_once: Arc<AtomicBool>,
    no_space_after: Arc<AtomicU64>,
}

impl MemoryWriter {
    fn maybe_fail(&self, writing: u64) -> Result<(), SftpError> {
        let written_before = self.written.load(Ordering::SeqCst);
        let limit = self.fail_after.load(Ordering::SeqCst);
        if written_before + writing > limit && !self.failed_once.swap(true, Ordering::SeqCst) {
            return Err(SftpError::Disconnected("测试注入的断线".to_owned()));
        }
        let no_space = self.no_space_after.load(Ordering::SeqCst);
        if written_before + writing > no_space {
            return Err(SftpError::LocalNoSpace {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

impl WriteAt for MemoryWriter {
    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: Vec<u8>,
    ) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.maybe_fail(data.len() as u64)?;
            write_into(&self.fs, &self.path, offset, &data);
            self.written.fetch_add(data.len() as u64, Ordering::SeqCst);
            Ok(())
        })
    }

    fn finish<'a>(&'a self) -> EndpointFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// 测试用的数据面（两端都在内存里）
// ---------------------------------------------------------------------------

/// 内存数据面：把"远端"完全放在内存里，测试不用服务器也能跑完整的引擎流程
/// （上传/下载、冲突、取消、断线重试）。
pub struct MemoryEndpoints {
    pub remote: MemoryEndpoint,
    pub resets: Arc<AtomicU64>,
    /// 这台"服务器"的身份。每个实例默认各不相同（并行跑的测试不会因为进程级的远端锁互相干扰）；
    /// 想模拟"同一台服务器"就共用一个实例，或者用 [`MemoryEndpoints::with_identity`] 指定同一个。
    identity: String,
}

impl MemoryEndpoints {
    /// `remote_root` 是"远端"的根路径前缀（内存里的键就是 `<root>/...`）。
    pub fn new(remote_root: &str) -> Self {
        Self {
            remote: MemoryEndpoint::new(remote_root, Vec::new()),
            resets: Arc::new(AtomicU64::new(0)),
            identity: format!("memory@{}:22", uuid::Uuid::new_v4().simple()),
        }
    }

    /// 指定这台"服务器"的身份（两个实例用同一个身份 = 同一台服务器）。
    pub fn with_identity(mut self, identity: &str) -> Self {
        self.identity = identity.to_owned();
        self
    }

    pub fn reset_count(&self) -> u64 {
        self.resets.load(Ordering::SeqCst)
    }

    /// 直接往"远端"放一个文件（造测试数据用）。
    pub fn put_remote(&self, path: &str, contents: Vec<u8>) {
        write_into(&self.remote.fs, path, 0, &contents);
    }

    /// 读"远端"的一个文件。
    pub fn remote_contents(&self, path: &str) -> Vec<u8> {
        self.remote
            .fs
            .files
            .lock()
            .expect("锁")
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// "远端"有没有这个文件。
    pub fn remote_exists(&self, path: &str) -> bool {
        self.remote.fs.files.lock().expect("锁").contains_key(path)
    }

    /// "远端"临时文件长度。
    pub fn remote_partial_len(&self, path: &str) -> u64 {
        // 临时文件名里带任务标识：`<path>.<标识>.zssh-part`（或旧写法 `<path>.zssh-part`）。
        let prefix = format!("{path}.");
        self.remote
            .fs
            .files
            .lock()
            .expect("锁")
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix) && key.ends_with(PART_SUFFIX))
            .map(|(_, data)| data.len() as u64)
            .max()
            .unwrap_or(0)
    }

    /// "远端"上某个目标名下所有的临时文件路径（含旧写法和带任务标识的）。
    pub fn remote_part_files(&self, path: &str) -> Vec<String> {
        let prefix = format!("{path}.");
        let mut keys: Vec<String> = self
            .remote
            .fs
            .files
            .lock()
            .expect("锁")
            .keys()
            .filter(|key| key.starts_with(&prefix) && key.ends_with(PART_SUFFIX))
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// 下一次收尾之前，让"别的程序"在 `path` 建出 `contents`（传输期间冒出同名文件）。
    pub fn appear_on_commit(&self, path: &str, contents: Vec<u8>) {
        *self.remote.fs.appear_on_commit.lock().expect("锁") = Some((path.to_owned(), contents));
    }

    /// 造一个"写到第 N 字节断线"的远端。
    pub fn fail_remote_write_after(&self, bytes: u64) {
        self.remote.fail_write_after(bytes);
    }
}

impl crate::sftp::transfer::backend::RemoteLister for MemoryEndpoints {
    fn list_dir<'a>(
        &'a self,
        path: &'a str,
    ) -> crate::ssh::BoxFuture<'a, Result<Vec<crate::sftp::fs::DirEntry>, SftpError>> {
        Box::pin(async move {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let files = self.remote.fs.files.lock().expect("锁");
            let mut names: Vec<String> = Vec::new();
            for key in files.keys() {
                if let Some(rest) = key.strip_prefix(&prefix) {
                    let name = rest.split('/').next().unwrap_or_default().to_owned();
                    if !name.is_empty() && !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            names.sort();
            let mut entries = Vec::new();
            for name in names {
                let full = format!("{prefix}{name}");
                let is_dir = files.keys().any(|key| key.starts_with(&format!("{full}/")));
                entries.push(crate::sftp::fs::DirEntry {
                    name: name.clone(),
                    path: full.clone(),
                    kind: if is_dir {
                        crate::sftp::fs::FileKind::Dir
                    } else {
                        crate::sftp::fs::FileKind::File
                    },
                    link_target: None,
                    size: files.get(&full).map(|data| data.len() as u64),
                    mtime: Some(1_700_000_000),
                    permissions: Some(0o644),
                    owner: Some("1000".to_owned()),
                    name_is_utf8: true,
                });
            }
            Ok(entries)
        })
    }

    fn stat_remote<'a>(
        &'a self,
        path: &'a str,
    ) -> crate::ssh::BoxFuture<'a, Result<russh_sftp::protocol::FileAttributes, SftpError>> {
        Box::pin(async move {
            let files = self.remote.fs.files.lock().expect("锁");
            let mut attrs = russh_sftp::protocol::FileAttributes::default();
            if let Some(data) = files.get(path) {
                attrs.size = Some(data.len() as u64);
                attrs.mtime = Some(1_700_000_000);
                attrs.permissions = Some(0o644);
                attrs.set_regular(true);
                return Ok(attrs);
            }
            let prefix = format!("{}/", path.trim_end_matches('/'));
            if files.keys().any(|key| key.starts_with(&prefix)) {
                attrs.set_dir(true);
                return Ok(attrs);
            }
            Err(SftpError::NotFound {
                path: path.to_owned(),
            })
        })
    }
}

impl crate::sftp::transfer::backend::TransferEndpoints for MemoryEndpoints {
    fn server_identity<'a>(&'a self) -> crate::ssh::BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move { Ok(self.identity.clone()) })
    }

    fn plan_remote_copy<'a>(
        &'a self,
        _peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        recursive: bool,
    ) -> crate::ssh::BoxFuture<'a, Result<crate::sftp::transfer::plan::TransferPlan, SftpError>>
    {
        Box::pin(async move {
            crate::sftp::transfer::plan::plan_remote_copy(self, source, target, recursive).await
        })
    }

    fn pair<'a>(
        &'a self,
        direction: crate::sftp::transfer::Direction,
        _peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        owner: &'a str,
    ) -> crate::ssh::BoxFuture<'a, Result<crate::sftp::transfer::backend::FilePair, SftpError>>
    {
        Box::pin(async move {
            match direction {
                // 复制：内存里的"远端"→ 同一个"远端"。
                crate::sftp::transfer::Direction::RemoteCopy => {
                    Ok(crate::sftp::transfer::backend::FilePair {
                        source: Arc::new(
                            MemoryEndpoint::with_fs(&self.remote, source).with_task_owner(owner),
                        ),
                        target: Arc::new(
                            MemoryEndpoint::with_fs(&self.remote, target).with_task_owner(owner),
                        ),
                    })
                }
                crate::sftp::transfer::Direction::LocalCopy => {
                    Ok(crate::sftp::transfer::backend::FilePair {
                        source: Arc::new(LocalEndpoint::new(std::path::Path::new(source))),
                        target: Arc::new(LocalEndpoint::with_owner(
                            std::path::Path::new(target),
                            owner,
                        )),
                    })
                }
                // 上传：本地（真实文件系统）→ 内存里的"远端"。
                crate::sftp::transfer::Direction::Upload => {
                    Ok(crate::sftp::transfer::backend::FilePair {
                        source: Arc::new(LocalEndpoint::new(std::path::Path::new(source))),
                        target: Arc::new(
                            MemoryEndpoint::with_fs(&self.remote, target).with_task_owner(owner),
                        ),
                    })
                }
                // 下载：内存里的"远端" → 本地。
                crate::sftp::transfer::Direction::Download => {
                    Ok(crate::sftp::transfer::backend::FilePair {
                        source: Arc::new(
                            MemoryEndpoint::with_fs(&self.remote, source).with_task_owner(owner),
                        ),
                        target: Arc::new(LocalEndpoint::with_owner(
                            std::path::Path::new(target),
                            owner,
                        )),
                    })
                }
            }
        })
    }

    fn create_target_dir<'a>(
        &'a self,
        _path: &'a str,
    ) -> crate::ssh::BoxFuture<'a, Result<(), SftpError>> {
        // 内存里的"目录"是从键推出来的，不需要真的建。
        Box::pin(async move { Ok(()) })
    }

    fn reset<'a>(&'a self) -> crate::ssh::BoxFuture<'a, ()> {
        Box::pin(async move {
            self.resets.fetch_add(1, Ordering::SeqCst);
        })
    }
}
