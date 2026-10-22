//! 内置编辑器测试用的替身：内存里的"服务器"、事件收集器、数据目录。
//! 测试从 `open_builtin` / `open_local`（真实入口）开始走，不手工搭目录。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use zssh_core::ai::files::OwnerChange;
use zssh_core::sftp::edit::{
    overwrite_in_place, EditBackend, OverwriteError, OverwriteReport, OwnerMode, RemoteSnapshot,
};
use zssh_core::sftp::error::SftpError;
use zssh_core::ssh::BoxFuture;

use super::{EditEventDto, EditRegistry, EditSink};

/// 测试替身"这台服务器"的身份：和传输任务的远端锁用同一个键。
pub const TEST_SERVER_IDENTITY: &str = "tester@test-host:22";

/// 内存里的假服务器：路径 → (内容, 修改时间)。
#[derive(Default)]
pub struct FakeBackend {
    files: Mutex<HashMap<String, (Vec<u8>, u32)>>,
    clock: Mutex<u32>,
    /// 接下来有几次 `write_all` 要卡在真正写入之前（开始时通知 `write_started`，等 `release_write`）。
    hold_writes: AtomicUsize,
    pub write_started: Notify,
    write_released: Notify,
    pub fail_writes: AtomicBool,
    /// 登录用户没权限把属主改回服务账号（模拟 `chown` 被拒）。
    pub owner_trouble: AtomicBool,
    /// 换一个服务器身份（模拟同一个 host_id 改了地址 / 用户名之后连到了另一台服务器）。
    pub identity_override: Mutex<Option<String>>,
}

impl FakeBackend {
    pub fn with_file(path: &str, data: &[u8]) -> Arc<Self> {
        let backend = Arc::new(Self::default());
        backend.put(path, data);
        backend
    }

    pub fn put(&self, path: &str, data: &[u8]) {
        let mut clock = self.clock.lock().unwrap();
        *clock += 1;
        self.files
            .lock()
            .unwrap()
            .insert(path.to_owned(), (data.to_vec(), 1000 + *clock));
    }

    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|(data, _)| data.clone())
    }

    /// 让接下来的 `n` 次上传卡在写入之前（测试要在“保存还在路上”时做别的事）。
    pub fn hold_next_writes(&self, n: usize) {
        self.hold_writes.store(n, Ordering::SeqCst);
    }

    /// 放行一次被卡住的上传。
    pub fn release_write(&self) {
        self.write_released.notify_one();
    }
}

/// 服务账号 → 登录用户：测试里的"属主变化"。
pub fn sample_owner_change() -> OwnerChange {
    OwnerChange {
        from: "uid 1001 / gid 1002".to_owned(),
        to: "uid 1000 / gid 1000".to_owned(),
        error: "权限不够".to_owned(),
    }
}

impl EditBackend for FakeBackend {
    fn overwrite<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: OwnerMode,
    ) -> BoxFuture<'a, Result<OverwriteReport, OverwriteError>> {
        Box::pin(async move {
            if mode == OwnerMode::InPlace {
                // 直接写原文件：锁在共享的 overwrite_in_place 里统一拿（R-8 第 4 节）。
                return overwrite_in_place(self, path, data).await;
            }
            let trouble = self.owner_trouble.load(Ordering::SeqCst);
            if trouble && mode == OwnerMode::Require {
                // 属主保持不了：什么都不替换，让用户先选。
                return Err(OverwriteError::OwnerWouldChange(sample_owner_change()));
            }
            self.write_all(path, data)
                .await
                .map_err(OverwriteError::Failed)?;
            Ok(OverwriteReport {
                owner_change: (trouble && mode == OwnerMode::Allow).then(sample_owner_change),
            })
        })
    }

    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<(RemoteSnapshot, bool), SftpError>> {
        Box::pin(async move {
            match self.files.lock().unwrap().get(path) {
                Some((data, mtime)) => Ok((
                    RemoteSnapshot {
                        size: data.len() as u64,
                        mtime: Some(*mtime),
                    },
                    false,
                )),
                None => Err(SftpError::NotFound {
                    path: path.to_owned(),
                }),
            }
        })
    }

    fn read_all<'a>(
        &'a self,
        path: &'a str,
        _limit: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, SftpError>> {
        Box::pin(async move {
            self.get(path).ok_or_else(|| SftpError::NotFound {
                path: path.to_owned(),
            })
        })
    }

    fn write_all<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let held = self
                .hold_writes
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if held {
                self.write_started.notify_one();
                self.write_released.notified().await;
            }
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(SftpError::Remote("写入失败（测试注入）".to_owned()));
            }
            self.put(path, data);
            Ok(())
        })
    }

    fn server_identity(&self) -> Option<String> {
        Some(
            self.identity_override
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| TEST_SERVER_IDENTITY.to_owned()),
        )
    }

    fn write_in_place<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(SftpError::Remote("写入失败（测试注入）".to_owned()));
            }
            self.put(path, data);
            Ok(())
        })
    }
}

/// 收集发给界面的事件。
#[derive(Default)]
pub struct CollectSink(pub Mutex<Vec<EditEventDto>>);

impl EditSink for CollectSink {
    fn emit(&self, event: EditEventDto) {
        self.0.lock().unwrap().push(event);
    }
}

/// 测试目录的守卫：`Drop` 时重试着删掉（后台任务可能还在收尾、占着文件）。
pub struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        for _ in 0..30 {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        eprintln!("临时目录没能删掉：{}", self.0.display());
    }
}

/// 一套测试环境：数据目录、编辑登记、假服务器、事件收集器。
pub struct Env {
    pub registry: Arc<EditRegistry>,
    pub backend: Arc<FakeBackend>,
    pub sink: Arc<CollectSink>,
    pub data_dir: PathBuf,
    // 放最后：目录守卫最后 drop。
    _guard: DirGuard,
}

pub const SFTP_ID: &str = "sftp-1";

impl Drop for Env {
    fn drop(&mut self) {
        self.registry.shutdown();
    }
}

impl Env {
    pub fn new(backend: Arc<FakeBackend>) -> Self {
        let data_dir = tempfile::Builder::new()
            .prefix("zssh-edit-env-")
            .tempdir()
            .unwrap()
            .keep();
        Self::on_dir(backend, DirGuard(data_dir.clone()), data_dir)
    }

    fn on_dir(backend: Arc<FakeBackend>, guard: DirGuard, data_dir: PathBuf) -> Self {
        let registry = Arc::new(EditRegistry::new(&data_dir));
        Self {
            registry,
            backend,
            sink: Arc::new(CollectSink::default()),
            data_dir,
            _guard: guard,
        }
    }

    /// 模拟程序崩溃后重新启动：旧的登记不做任何清理就丢掉，在同一个数据目录上重新扫描。
    pub fn crash_and_restart(&mut self) -> Arc<EditRegistry> {
        std::mem::replace(
            &mut self.registry,
            Arc::new(EditRegistry::new(&self.data_dir)),
        )
    }
}
