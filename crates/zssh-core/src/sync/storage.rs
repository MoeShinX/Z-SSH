//! 云存储的统一接口（S-1 只做内存实现给测试用）。
//!
//! 云端只存一个对象（同步包）。读取返回内容 + 版本号；写入要带上次读到的版本号，
//! 对不上就是“已被别人改过”（乐观并发控制），引擎重新读取、重新合并、再写。
//! 内存实现可以注入版本冲突、读超时、写超时和“写到一半失败”。

use std::sync::Mutex;

use crate::ssh::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub bytes: Vec<u8>,
    /// 版本号由存储决定（WebDAV 的 ETag、S3 的版本、Gist 的更新时间……）。
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    #[error("云端数据已被别人改过")]
    Conflict,
    #[error("存储读写超时")]
    Timeout,
    /// 服务暂时不可用（HTTP 429 / 5xx）：和超时一样值得稍后重试。
    #[error("存储服务暂时不可用：{0}")]
    Unavailable(String),
    /// 服务明确说“请求太多了”，要等一段时间。**不能立刻重试**：引擎遇到它直接放弃这次
    /// 同步，调度器在这段时间内也不再自动同步。
    #[error("{service} 限流，{}后重试", wait_text(*retry_after_secs))]
    RateLimited {
        service: String,
        retry_after_secs: u64,
    },
    #[error("存储读写失败：{0}")]
    Io(String),
    /// 凭据被服务拒绝（401、签名 / 密钥错误、没有权限）：换了配置才可能好，自动同步要暂停。
    #[error("存储读写失败：{0}")]
    AuthFailed(String),
}

/// “还要等多久”的说法：不到 1 分钟说秒，之后按分钟向上取整，超过 2 小时说小时。
pub fn wait_text(secs: u64) -> String {
    if secs < 60 {
        format!("约 {} 秒", secs.max(1))
    } else if secs <= 2 * 3600 {
        format!("约 {} 分钟", secs.div_ceil(60))
    } else {
        format!("约 {} 小时", secs.div_ceil(3600))
    }
}

pub trait SyncStorage: Send + Sync {
    fn read<'a>(&'a self) -> BoxFuture<'a, Result<Option<StoredObject>, StorageError>>;
    /// `expected_version` 为 `None` 表示“云端应该还没有对象”（首次上传）。
    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, StorageError>>;
    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StorageError>>;
}

#[derive(Default)]
struct MemoryState {
    bytes: Option<Vec<u8>>,
    version: u64,
}

#[derive(Default)]
struct Injection {
    read_timeouts: usize,
    write_timeouts: usize,
    write_conflicts: usize,
    partial_writes: usize,
}

/// 测试用的内存存储。
#[derive(Default)]
pub struct MemoryStorage {
    state: Mutex<MemoryState>,
    inject: Mutex<Injection>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bytes(bytes: Vec<u8>) -> Self {
        let storage = Self::default();
        storage.put_external(bytes);
        storage
    }

    /// 模拟另一台电脑/另一个客户端写了云端：内容换掉、版本号前进。
    pub fn put_external(&self, bytes: Vec<u8>) -> String {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.bytes = Some(bytes);
        state.version += 1;
        state.version.to_string()
    }

    pub fn current_version(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.bytes.as_ref().map(|_| state.version.to_string())
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bytes
            .clone()
    }

    pub fn inject_read_timeouts(&self, count: usize) {
        self.inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read_timeouts = count;
    }

    pub fn inject_write_timeouts(&self, count: usize) {
        self.inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_timeouts = count;
    }

    /// 下一次写入时版本号已经变了（模拟另一个客户端抢先写入）。
    pub fn inject_write_conflicts(&self, count: usize) {
        self.inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_conflicts = count;
    }

    /// 下一次写入只写一半就报错（模拟写到一半连接断了）。
    pub fn inject_partial_writes(&self, count: usize) {
        self.inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .partial_writes = count;
    }

    fn take(inject: &Mutex<Injection>, field: fn(&mut Injection) -> &mut usize) -> bool {
        let mut inject = inject.lock().unwrap_or_else(|e| e.into_inner());
        let count = field(&mut inject);
        if *count == 0 {
            return false;
        }
        *count -= 1;
        true
    }
}

impl SyncStorage for MemoryStorage {
    fn read<'a>(&'a self) -> BoxFuture<'a, Result<Option<StoredObject>, StorageError>> {
        Box::pin(async move {
            if Self::take(&self.inject, |i| &mut i.read_timeouts) {
                return Err(StorageError::Timeout);
            }
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            Ok(state.bytes.as_ref().map(|bytes| StoredObject {
                bytes: bytes.clone(),
                version: state.version.to_string(),
            }))
        })
    }

    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, StorageError>> {
        Box::pin(async move {
            if Self::take(&self.inject, |i| &mut i.write_timeouts) {
                return Err(StorageError::Timeout);
            }
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let current = state.bytes.as_ref().map(|_| state.version.to_string());
            if current.as_deref() != expected_version {
                return Err(StorageError::Conflict);
            }
            if Self::take(&self.inject, |i| &mut i.write_conflicts) {
                // 另一个客户端抢先写了：版本前进，内容不变（对本测试足够）。
                state.version += 1;
                return Err(StorageError::Conflict);
            }
            if Self::take(&self.inject, |i| &mut i.partial_writes) {
                let half = bytes.len() / 2;
                state.bytes = Some(bytes[..half].to_vec());
                state.version += 1;
                return Err(StorageError::Io("写到一半失败".to_owned()));
            }
            state.bytes = Some(bytes.to_vec());
            state.version += 1;
            Ok(state.version.to_string())
        })
    }

    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let current = state.bytes.as_ref().map(|_| state.version.to_string());
            if current.as_deref() != expected_version {
                return Err(StorageError::Conflict);
            }
            state.bytes = None;
            state.version += 1;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("运行时")
            .block_on(future)
    }

    #[test]
    fn version_conflict_and_timeouts_are_injectable() {
        let storage = MemoryStorage::new();
        let version = block_on(storage.write(b"one", None)).expect("首次写");
        assert_eq!(version, "1");
        let conflict = block_on(storage.write(b"two", None)).expect_err("没带版本");
        assert_eq!(conflict, StorageError::Conflict);
        block_on(storage.write(b"two", Some("1"))).expect("带对版本");

        storage.inject_write_conflicts(1);
        let conflict = block_on(storage.write(b"three", Some("2"))).expect_err("注入冲突");
        assert_eq!(conflict, StorageError::Conflict);
        assert_eq!(storage.current_version().as_deref(), Some("3"));
        block_on(storage.write(b"three", Some("3"))).expect("重新读之后能写");

        storage.inject_read_timeouts(1);
        assert_eq!(block_on(storage.read()).unwrap_err(), StorageError::Timeout);
        storage.inject_write_timeouts(1);
        assert_eq!(
            block_on(storage.write(b"x", Some("4"))).unwrap_err(),
            StorageError::Timeout
        );
        storage.inject_partial_writes(1);
        assert!(matches!(
            block_on(storage.write(b"abcdef", Some("4"))).unwrap_err(),
            StorageError::Io(_)
        ));
        assert_eq!(
            storage.bytes().as_deref(),
            Some(b"abc".as_slice()),
            "只写了一半"
        );
    }

    #[test]
    fn delete_checks_the_version() {
        let storage = MemoryStorage::with_bytes(b"one".to_vec());
        assert_eq!(
            block_on(storage.delete(Some("999"))).unwrap_err(),
            StorageError::Conflict
        );
        block_on(storage.delete(Some("1"))).expect("版本对，删掉");
        assert!(storage.bytes().is_none());
    }
}
