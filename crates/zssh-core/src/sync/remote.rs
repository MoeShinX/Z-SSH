//! WebDAV 和 S3 共用的“远端单个对象”逻辑：条件写入、并发保护检测、降级。
//!
//! 两种存储只需要实现 [`RawObjectStore`]（读 / 带条件写 / 带条件删），其余——
//! 把 ETag 当版本号、没有并发保护时退回“写前再读一次比较内容哈希”、连接时
//! 探测服务器到底支不支持——都在这里。**不支持就是不支持**：检测结果由上层存下来
//! 并在设置页提示，这里绝不会在没检测过的情况下假定服务器支持。

use sha2::{Digest, Sha256};

use super::storage::{StorageError, StoredObject, SyncStorage};
use crate::ssh::BoxFuture;

/// 写 / 删的前置条件（对应 HTTP 的条件请求头）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// 不带条件，直接覆盖。
    None,
    /// `If-None-Match: *`：对象不存在才写。
    IfNoneMatchAny,
    /// `If-Match: <ETag>`：对象的 ETag 还是这个才写。
    IfMatch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObject {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawPut {
    Written {
        etag: Option<String>,
    },
    /// 服务器明确拒绝了条件（HTTP 412，S3 的并发写也可能是 409）。
    PreconditionFailed,
    /// 服务器明说不认条件请求头（例如 S3 兼容服务返回 501 NotImplemented）。
    ConditionNotSupported,
}

pub trait RawObjectStore: Send + Sync {
    fn get<'a>(&'a self) -> BoxFuture<'a, Result<Option<RawObject>, StorageError>>;
    fn put<'a>(
        &'a self,
        bytes: &'a [u8],
        condition: &'a Condition,
    ) -> BoxFuture<'a, Result<RawPut, StorageError>>;
    /// 返回 `Ok(false)` 表示条件不满足。
    fn delete<'a>(&'a self, condition: &'a Condition) -> BoxFuture<'a, Result<bool, StorageError>>;
    /// 同一个位置换一个对象名（探测并发保护用，不碰真正的同步包）。
    /// 返回值可以借用 `self`（OneDrive 的存储要借着本机令牌库）。
    fn sibling(&self, object_name: &str) -> Box<dyn RawObjectStore + '_>;
}

/// 怎么做乐观并发控制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyMode {
    /// 服务器认 `If-Match` / `If-None-Match`，由服务器保证原子。
    Conditional,
    /// 服务器不认：写之前再读一次，内容哈希一致才写。两台电脑几乎同时同步时仍有
    /// 很小的概率互相覆盖（读和写之间的窗口）。
    ReadBeforeWrite,
}

impl ConcurrencyMode {
    pub fn is_protected(self) -> bool {
        self == Self::Conditional
    }
}

/// 一次检测的结果，逐项列出来给设置页显示。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConcurrencyProbe {
    pub mode: ConcurrencyMode,
    /// 每一步探测的结论，例如“服务器返回了 ETag”“If-None-Match 被拒绝（正确）”。
    pub notes: Vec<String>,
}

pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 在存储的真实位置旁边写一个临时探测对象，验证服务器是否真的执行条件请求。
///
/// 只相信**服务器的行为**，不看它有没有返回 ETag 头：
/// 1. 第一次 `If-None-Match: *` 创建应成功，并能拿到 ETag；
/// 2. 对象已存在时再来一次 `If-None-Match: *` 必须被拒绝（412）；
/// 3. 带一个错误的 `If-Match` 必须被拒绝；
/// 4. 带正确 ETag 的 `If-Match` 必须成功。
///
/// 任何一步不符合就判定为不支持。探测对象无论如何都会尽力删掉。
pub async fn probe_concurrency(
    store: &dyn RawObjectStore,
) -> Result<ConcurrencyProbe, StorageError> {
    let name = format!("zssh-probe-{}.tmp", uuid::Uuid::new_v4().simple());
    let probe = store.sibling(&name);
    let result = run_probe(probe.as_ref()).await;
    // 清理：不管探测结果如何都删（无条件删除，失败也不影响结论）。
    let _ = probe.delete(&Condition::None).await;
    result
}

async fn run_probe(probe: &dyn RawObjectStore) -> Result<ConcurrencyProbe, StorageError> {
    let mut notes = Vec::new();
    let unsupported = |notes: Vec<String>| ConcurrencyProbe {
        mode: ConcurrencyMode::ReadBeforeWrite,
        notes,
    };

    let first = probe.put(b"probe-1", &Condition::IfNoneMatchAny).await?;
    let first_etag = match first {
        RawPut::Written { etag } => etag,
        RawPut::PreconditionFailed => {
            notes.push("新建探测对象时被拒绝，无法判断".to_owned());
            return Ok(unsupported(notes));
        }
        RawPut::ConditionNotSupported => {
            // 服务器明说不认条件头。再用无条件写确认写权限没问题。
            probe.put(b"probe-0", &Condition::None).await?;
            notes.push("服务器明确表示不支持条件写入".to_owned());
            return Ok(unsupported(notes));
        }
    };
    let etag = match first_etag {
        Some(etag) => Some(etag),
        None => probe.get().await?.and_then(|object| object.etag),
    };
    let Some(etag) = etag else {
        notes.push("服务器没有返回 ETag".to_owned());
        return Ok(unsupported(notes));
    };
    notes.push("服务器返回了 ETag".to_owned());

    match probe.put(b"probe-2", &Condition::IfNoneMatchAny).await? {
        RawPut::PreconditionFailed => {
            notes.push("对象已存在时 If-None-Match 被正确拒绝".to_owned());
        }
        RawPut::Written { .. } => {
            notes.push("服务器忽略了 If-None-Match（对象已存在仍写成功）".to_owned());
            return Ok(unsupported(notes));
        }
        RawPut::ConditionNotSupported => {
            notes.push("服务器不支持 If-None-Match".to_owned());
            return Ok(unsupported(notes));
        }
    }

    let wrong = Condition::IfMatch("\"zssh-not-the-etag\"".to_owned());
    match probe.put(b"probe-3", &wrong).await? {
        RawPut::PreconditionFailed => {
            notes.push("错误的 If-Match 被正确拒绝".to_owned());
        }
        RawPut::Written { .. } => {
            notes.push("服务器忽略了 If-Match（错误的 ETag 也写成功）".to_owned());
            return Ok(unsupported(notes));
        }
        RawPut::ConditionNotSupported => {
            notes.push("服务器不支持 If-Match".to_owned());
            return Ok(unsupported(notes));
        }
    }

    // 第 2、3 步都被拒绝，所以此刻对象的 ETag 还是第 1 步拿到的那个。
    match probe.put(b"probe-4", &Condition::IfMatch(etag)).await? {
        RawPut::Written { .. } => {
            notes.push("正确的 If-Match 可以写入".to_owned());
        }
        RawPut::PreconditionFailed | RawPut::ConditionNotSupported => {
            notes.push("正确的 ETag 也被拒绝（ETag 的写法服务器不认）".to_owned());
            return Ok(unsupported(notes));
        }
    }

    Ok(ConcurrencyProbe {
        mode: ConcurrencyMode::Conditional,
        notes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Check {
    pub ok: bool,
    pub message: String,
}

impl Check {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }
    fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

/// “测试连接”的完整结果：能连上并读、有写权限、是否支持并发保护，分别显示。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConnectionProbe {
    pub read: Check,
    pub write: Check,
    /// 读写都不通时没有这一项。
    pub concurrency: Option<ConcurrencyProbe>,
    /// 云端已经有同步包（第二台电脑需要输入主密码解开）。
    pub has_existing_data: bool,
}

/// 读一次同步包的位置（不改任何东西）→ 写一个临时探测对象（并删掉）→ 检测并发保护。
pub async fn probe_connection(store: &dyn RawObjectStore) -> ConnectionProbe {
    let (read, has_existing_data) = match store.get().await {
        Ok(Some(_)) => (Check::ok("已连接，云端已有同步数据"), true),
        Ok(None) => (Check::ok("已连接，云端还没有同步数据"), false),
        Err(err) => {
            return ConnectionProbe {
                read: Check::failed(err.to_string()),
                write: Check::failed("读取失败，没有继续检查写权限"),
                concurrency: None,
                has_existing_data: false,
            }
        }
    };
    match probe_concurrency(store).await {
        Ok(concurrency) => ConnectionProbe {
            read,
            write: Check::ok("可以写入"),
            concurrency: Some(concurrency),
            has_existing_data,
        },
        Err(err) => ConnectionProbe {
            read,
            write: Check::failed(format!("写入失败：{err}")),
            concurrency: None,
            has_existing_data,
        },
    }
}

/// 把 [`RawObjectStore`] 包成引擎要的 [`SyncStorage`]。
pub struct ConditionalStore<R: RawObjectStore> {
    raw: R,
    mode: ConcurrencyMode,
}

impl<R: RawObjectStore> ConditionalStore<R> {
    /// `mode` 来自上次“测试连接”的检测结果；没检测过就传 `ReadBeforeWrite`。
    pub fn new(raw: R, mode: ConcurrencyMode) -> Self {
        Self { raw, mode }
    }

    pub fn mode(&self) -> ConcurrencyMode {
        self.mode
    }

    pub fn raw(&self) -> &R {
        &self.raw
    }
}

impl<R: RawObjectStore> SyncStorage for ConditionalStore<R> {
    fn read<'a>(&'a self) -> BoxFuture<'a, Result<Option<StoredObject>, StorageError>> {
        Box::pin(async move {
            let Some(object) = self.raw.get().await? else {
                return Ok(None);
            };
            let version = match self.mode {
                ConcurrencyMode::Conditional => object.etag.clone().ok_or_else(|| {
                    StorageError::Io("服务器这次没有返回 ETag，请重新测试连接".to_owned())
                })?,
                ConcurrencyMode::ReadBeforeWrite => content_hash(&object.bytes),
            };
            Ok(Some(StoredObject {
                bytes: object.bytes,
                version,
            }))
        })
    }

    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, StorageError>> {
        Box::pin(async move {
            match self.mode {
                ConcurrencyMode::Conditional => {
                    let condition = match expected_version {
                        Some(version) => Condition::IfMatch(version.to_owned()),
                        None => Condition::IfNoneMatchAny,
                    };
                    match self.raw.put(bytes, &condition).await? {
                        RawPut::PreconditionFailed => Err(StorageError::Conflict),
                        RawPut::ConditionNotSupported => Err(StorageError::Io(
                            "服务器不支持条件写入，请重新测试连接".to_owned(),
                        )),
                        RawPut::Written { etag: Some(etag) } => Ok(etag),
                        RawPut::Written { etag: None } => {
                            // 有的服务器 PUT 的响应里不带 ETag，再读一次拿。
                            let object = self.raw.get().await?;
                            object.and_then(|o| o.etag).ok_or_else(|| {
                                StorageError::Io("写入成功但服务器没有返回 ETag".to_owned())
                            })
                        }
                    }
                }
                ConcurrencyMode::ReadBeforeWrite => {
                    let current = self.raw.get().await?;
                    let current_version = current.as_ref().map(|o| content_hash(&o.bytes));
                    if current_version.as_deref() != expected_version {
                        return Err(StorageError::Conflict);
                    }
                    match self.raw.put(bytes, &Condition::None).await? {
                        RawPut::Written { .. } => Ok(content_hash(bytes)),
                        RawPut::PreconditionFailed | RawPut::ConditionNotSupported => {
                            Err(StorageError::Conflict)
                        }
                    }
                }
            }
        })
    }

    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            match self.mode {
                ConcurrencyMode::Conditional => {
                    let condition = match expected_version {
                        Some(version) => Condition::IfMatch(version.to_owned()),
                        None => Condition::None,
                    };
                    if self.raw.delete(&condition).await? {
                        Ok(())
                    } else {
                        Err(StorageError::Conflict)
                    }
                }
                ConcurrencyMode::ReadBeforeWrite => {
                    let current = self.raw.get().await?;
                    let current_version = current.as_ref().map(|o| content_hash(&o.bytes));
                    if current_version.as_deref() != expected_version {
                        return Err(StorageError::Conflict);
                    }
                    self.raw.delete(&Condition::None).await?;
                    Ok(())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
