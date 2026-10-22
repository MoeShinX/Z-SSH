//! 传输引擎的数据面接口（M3-1 第 3 节）。
//!
//! 引擎不直接碰 SFTP，而是通过 [`TransferEndpoints`]：
//! - 生产实现 [`SftpEndpoints`]：向 [`TransferChannelProvider`] 要通道，建"本地 ↔ 远端"
//!   的端点对；
//! - 单测实现（`memory.rs` 里的 `MemoryEndpoints`）：两端都在内存里，能造断线、磁盘满
//!   这类边界，不用服务器。
//!
//! 展开远端目录要读目录，所以接口里也带上 [`RemoteLister`]（下载时要走它）。

use std::path::Path;
use std::sync::Arc;

use super::endpoint::{FileEndpoint, LocalEndpoint, RemoteEndpoint};
use super::Direction;
use crate::sftp::error::SftpError;
use crate::sftp::fs::DirEntry;
use crate::sftp::session::SftpChannel;
use crate::ssh::{BoxFuture, Connection};

/// 一个文件的源和目标。
pub struct FilePair {
    pub source: Arc<dyn FileEndpoint>,
    pub target: Arc<dyn FileEndpoint>,
}

/// 读远端（展开下载任务用）：属性 + 目录。
pub trait RemoteLister: Send + Sync {
    fn list_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, SftpError>>;
    /// 读属性（`lstat`；判断是不是目录）。
    fn stat_remote<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<russh_sftp::protocol::FileAttributes, SftpError>>;
}

/// 传输用得到的数据面。
pub trait TransferEndpoints: RemoteLister {
    /// 建一个文件的源 / 目标端点（按任务方向决定哪边是本地）。`owner` 是任务标识，
    /// 本地临时文件的名字里带着它，重试同一个任务时才找得回自己的临时文件。
    ///
    /// `peer`：只有 `RemoteCopy` 用，源在另一台主机时是那台主机的登记名（见
    /// [`SftpEndpoints::register_peer`]）；`None` = 源和目标在同一台主机。
    fn pair<'a>(
        &'a self,
        direction: Direction,
        peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        owner: &'a str,
    ) -> BoxFuture<'a, Result<FilePair, SftpError>>;

    /// 展开"远端 → 远端"复制任务：源树从 `peer`（或本机的连接）读，目标全是本机连接上的路径。
    fn plan_remote_copy<'a>(
        &'a self,
        peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        recursive: bool,
    ) -> BoxFuture<'a, Result<super::plan::TransferPlan, SftpError>>;

    /// 目标端的目录（上传时是远端目录）。
    fn create_target_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;

    /// 断线之后让下一次拿到的通道是新的（真实现会重开通道 / 重连）。
    fn reset<'a>(&'a self) -> BoxFuture<'a, ()>;

    /// 建议的读写块大小（SFTP 用服务器声明的读限制；其它实现给保守值）。
    fn suggested_chunk_len(&self) -> u32 {
        crate::sftp::session::CONSERVATIVE_CHUNK
    }

    /// 这个数据面连的服务器身份（用户名@主机:端口，见 `ssh::connection::server_identity`）。
    /// 远端目标锁用它当键，所以**所有**引擎、编辑、AI 写文件只要实际连到的服务器相同就互斥。
    fn server_identity<'a>(&'a self) -> BoxFuture<'a, Result<String, SftpError>>;
}

/// 传输通道从哪来（浏览/小操作用的那条是另一条；传输按需另开）。
pub trait TransferChannelProvider: Send + Sync {
    /// 拿一条传输用的通道（通常复用一条；断线之后重开会新开）。
    fn channel<'a>(&'a self) -> BoxFuture<'a, Result<Arc<SftpChannel>, SftpError>>;
    /// 开通道被服务器拒绝时改走专用连接（默认实现就是直接报错）。
    fn dedicated_channel<'a>(&'a self) -> BoxFuture<'a, Result<Arc<SftpChannel>, SftpError>> {
        self.channel()
    }
    /// 断线之后让下次 `channel()` 重新开一条。
    fn reset<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    /// 通道已经开着时，用它协商出来的读限制当块大小；还没开就用保守值。
    fn suggested_chunk_len(&self) -> u32 {
        crate::sftp::session::CONSERVATIVE_CHUNK
    }
}

/// 需要一条专用连接时怎么连（生产上是"用保存的凭据 + 已确认的指纹再连一次"）。
pub trait SftpReconnect: Send + Sync {
    fn reconnect<'a>(&'a self) -> BoxFuture<'a, Result<Arc<Connection>, SftpError>>;
}

/// 生产用的通道提供者（M3-1 第 1 节）。
///
/// - 正常情况：在**已有连接**上开一条传输通道（浏览/小操作那条是另一条，互不干扰）；
/// - 服务器拒绝开通道（`MaxSessions` 到顶）：给了 [`SftpReconnect`] 就**新连一条专用
///   连接**再开——选这个而不是"排队等通道空出来"，是因为终端通道由用户自己开关，
///   可能一整天都占着，排队会把传输无限期拖住；
/// - 连接断了：`reset()` 之后下次重开。
pub struct SftpChannelProvider {
    /// 当前用的连接（断线之后会被换成新连的那条）。
    connection: tokio::sync::Mutex<Arc<Connection>>,
    reconnect: Option<Arc<dyn SftpReconnect>>,
    cached: tokio::sync::Mutex<Option<Arc<SftpChannel>>>,
    dedicated: tokio::sync::Mutex<Option<Arc<SftpChannel>>>,
    /// 重连次数（诊断 / 测试）。
    reconnects: std::sync::atomic::AtomicU64,
}

impl SftpChannelProvider {
    pub fn new(connection: Arc<Connection>, reconnect: Option<Arc<dyn SftpReconnect>>) -> Self {
        Self {
            connection: tokio::sync::Mutex::new(connection),
            reconnect,
            cached: tokio::sync::Mutex::new(None),
            dedicated: tokio::sync::Mutex::new(None),
            reconnects: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 重连过几次（诊断 / 测试）。
    pub fn reconnect_count(&self) -> u64 {
        self.reconnects.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 断开之后换一条新连接（配置了 [`SftpReconnect`] 才有）。
    ///
    /// 为什么在这里做：连接死了之后，光把缓存的通道清掉没用——底层那条 SSH 会话
    /// 已经发不出包了（实测报 "Channel send error"），必须重新握手认证。
    async fn reconnect_if_needed(&self) -> Result<(), SftpError> {
        let stale = {
            let guard = self.connection.lock().await;
            guard.is_closed()
        };
        if !stale {
            return Ok(());
        }
        let Some(reconnect) = self.reconnect.clone() else {
            return Ok(());
        };
        let fresh = reconnect.reconnect().await?;
        *self.connection.lock().await = fresh;
        self.reconnects
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("传输连接断了：已经重连一条");
        Ok(())
    }

    /// 已经协商好的块大小（通道还没开就用保守值）。
    pub fn chunk_len_hint(&self) -> u32 {
        self.cached
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|channel| channel.caps().max_read_len()))
            .unwrap_or(crate::sftp::session::CONSERVATIVE_CHUNK)
    }
}

impl TransferChannelProvider for SftpChannelProvider {
    fn channel<'a>(&'a self) -> BoxFuture<'a, Result<Arc<SftpChannel>, SftpError>> {
        Box::pin(async move {
            if let Some(channel) = self.cached.lock().await.clone() {
                return Ok(channel);
            }
            // 连接死了就先重连（不然开通道只会报 "Channel send error"）。
            self.reconnect_if_needed().await?;
            let connection = Arc::clone(&*self.connection.lock().await);
            match SftpChannel::open(&connection).await {
                Ok(channel) => {
                    let channel = Arc::new(channel);
                    *self.cached.lock().await = Some(Arc::clone(&channel));
                    Ok(channel)
                }
                // 通道被拒（多半到上限）：改成另开一条专用连接。
                Err(SftpError::ChannelLimit(message)) => {
                    tracing::warn!(%message, "开传输通道被拒，改用专用连接");
                    if self.reconnect.is_some() {
                        self.dedicated_channel().await
                    } else {
                        Err(SftpError::ChannelLimit(message))
                    }
                }
                Err(err) => Err(err),
            }
        })
    }

    fn dedicated_channel<'a>(&'a self) -> BoxFuture<'a, Result<Arc<SftpChannel>, SftpError>> {
        Box::pin(async move {
            if let Some(channel) = self.dedicated.lock().await.clone() {
                return Ok(channel);
            }
            let Some(reconnect) = self.reconnect.clone() else {
                return Err(SftpError::ChannelLimit("没有配置专用连接的兜底".to_owned()));
            };
            let connection = reconnect.reconnect().await?;
            let channel = Arc::new(SftpChannel::open(&connection).await?);
            *self.dedicated.lock().await = Some(Arc::clone(&channel));
            Ok(channel)
        })
    }

    fn reset<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // 丢掉缓存通道；连接死了的话，下一次 `channel()` 会重连。
            self.cached.lock().await.take();
        })
    }

    fn suggested_chunk_len(&self) -> u32 {
        self.chunk_len_hint()
    }
}

/// 生产实现：本地 ↔ 远端。
pub struct SftpEndpoints {
    provider: Arc<dyn TransferChannelProvider>,
    /// "远端 → 远端"复制时，源所在的另一台主机的数据面（按登记名找）。
    peers: std::sync::Mutex<std::collections::HashMap<String, Arc<dyn TransferChannelProvider>>>,
}

impl SftpEndpoints {
    pub fn new(provider: Arc<dyn TransferChannelProvider>) -> Self {
        Self {
            provider,
            peers: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 登记另一台主机的数据面（复制粘贴的源在别的主机上时，提交任务前先登记）。
    pub fn register_peer(&self, name: &str, provider: Arc<dyn TransferChannelProvider>) {
        self.peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_owned(), provider);
    }

    fn provider_for(
        &self,
        peer: Option<&str>,
    ) -> Result<Arc<dyn TransferChannelProvider>, SftpError> {
        match peer {
            None => Ok(Arc::clone(&self.provider)),
            Some(name) => self
                .peers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(name)
                .cloned()
                .ok_or_else(|| {
                    SftpError::Remote(
                        "复制的源所在的连接已经关闭，请重新打开那个标签再试".to_owned(),
                    )
                }),
        }
    }
}

/// 在某个通道提供者上读目录 / 属性（展开复制任务的源树用）。
struct ProviderLister(Arc<dyn TransferChannelProvider>);

impl RemoteLister for ProviderLister {
    fn list_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, SftpError>> {
        Box::pin(async move { self.0.channel().await?.read_dir(path).await })
    }

    fn stat_remote<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<russh_sftp::protocol::FileAttributes, SftpError>> {
        Box::pin(async move { self.0.channel().await?.lstat(path).await })
    }
}

impl RemoteLister for SftpEndpoints {
    fn list_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, SftpError>> {
        Box::pin(async move {
            let channel = self.provider.channel().await?;
            channel.read_dir(path).await
        })
    }

    fn stat_remote<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<russh_sftp::protocol::FileAttributes, SftpError>> {
        Box::pin(async move {
            let channel = self.provider.channel().await?;
            channel.lstat(path).await
        })
    }
}

impl TransferEndpoints for SftpEndpoints {
    fn server_identity<'a>(&'a self) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move {
            let channel = self.provider.channel().await?;
            Ok(channel.server_key().to_owned())
        })
    }

    fn plan_remote_copy<'a>(
        &'a self,
        peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        recursive: bool,
    ) -> BoxFuture<'a, Result<super::plan::TransferPlan, SftpError>> {
        Box::pin(async move {
            let lister = ProviderLister(self.provider_for(peer)?);
            super::plan::plan_remote_copy(&lister, source, target, recursive).await
        })
    }

    fn pair<'a>(
        &'a self,
        direction: Direction,
        peer: Option<&'a str>,
        source: &'a str,
        target: &'a str,
        owner: &'a str,
    ) -> BoxFuture<'a, Result<FilePair, SftpError>> {
        Box::pin(async move {
            match direction {
                Direction::RemoteCopy => {
                    let from = self.provider_for(peer)?.channel().await?;
                    let to = self.provider.channel().await?;
                    Ok(FilePair {
                        source: Arc::new(RemoteEndpoint::with_owner(from, source, owner)),
                        target: Arc::new(RemoteEndpoint::with_owner(to, target, owner)),
                    })
                }
                Direction::LocalCopy => Ok(FilePair {
                    source: Arc::new(LocalEndpoint::new(Path::new(source))),
                    target: Arc::new(LocalEndpoint::with_owner(Path::new(target), owner)),
                }),
                Direction::Upload => {
                    let channel = self.provider.channel().await?;
                    Ok(FilePair {
                        source: Arc::new(LocalEndpoint::new(Path::new(source))),
                        target: Arc::new(RemoteEndpoint::with_owner(channel, target, owner)),
                    })
                }
                Direction::Download => {
                    let channel = self.provider.channel().await?;
                    Ok(FilePair {
                        source: Arc::new(RemoteEndpoint::with_owner(channel, source, owner)),
                        target: Arc::new(LocalEndpoint::with_owner(Path::new(target), owner)),
                    })
                }
            }
        })
    }

    fn create_target_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let channel = self.provider.channel().await?;
            match channel.create_dir(path).await {
                Ok(()) => Ok(()),
                // "已经存在"的说法各家服务器不一样（有的只报一个光秃秃的 Failure）：
                // stat 一次，目录真在了就算成功。
                Err(_) => match channel.lstat(path).await {
                    Ok(attrs) if attrs.permissions.is_some_and(|mode| mode & 0o40000 != 0) => {
                        Ok(())
                    }
                    Ok(_) => Err(SftpError::Remote(format!("{path} 已存在，但不是目录"))),
                    Err(err) => Err(err),
                },
            }
        })
    }

    fn reset<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.provider.reset().await })
    }

    fn suggested_chunk_len(&self) -> u32 {
        self.provider.suggested_chunk_len()
    }
}
