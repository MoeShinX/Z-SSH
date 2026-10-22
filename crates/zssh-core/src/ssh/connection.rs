use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use russh::client::{self, Config, Handle};
use russh::keys::{Algorithm, HashAlg, PublicKey, PublicKeyOrCertificate};
use russh::Channel;
use russh::{cipher, kex, Disconnect};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::ai::terminal::SharedTerminalRing;
use crate::error::{HostKeyMismatch, ZsshError};
use crate::ssh::auth::{self, AuthConfig, AuthOutcome, RememberedSecret};
use crate::ssh::dialer::{self, BoxedStream, ProxyConfig, HOP_TIMEOUT};
use crate::ssh::interact::{BoxFuture, HopProgress};
use crate::ssh::interact::{HostKeyDecision, HostKeyInfo, HostKeyStatus, Interactor};
use crate::ssh::known_hosts::{HostKeyLookup, KnownHosts, StoredIn};
use crate::ssh::shell::{ShellSession, DEFAULT_BUFFER_LIMIT};
use crate::ssh::stream::{ChannelByteStream, PrefixedStream};

/// 建立连接（DNS + TCP）的总超时。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// SSH 协议握手（版本交换 + 密钥交换 + 主机密钥确认）的总超时。
///
/// 15 秒是协议本身该有的预算，另外还要留出用户确认未知主机指纹的时间（界面侧
/// 120 秒超时），所以整段给 150 秒。协议中途卡死的保护仍然在：超时会掐掉连接。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(150);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_MAX: usize = 3;

/// 进程级的 keepalive 间隔（秒）。0 表示关掉探测。
///
/// russh 的 `Config` 是每次连接现建的，没有别的地方能把它传进来；设置页改完
/// 通过 [`set_keepalive_interval`] 落在这里，之后新建的连接按新值走。
static KEEPALIVE_SECS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(KEEPALIVE_INTERVAL.as_secs());

/// 设置 keepalive 间隔（0 = 关闭），对新建立的连接生效。
pub fn set_keepalive_interval(seconds: u64) {
    KEEPALIVE_SECS.store(seconds, std::sync::atomic::Ordering::SeqCst);
}

/// 当前的 keepalive 间隔。
pub fn keepalive_interval() -> Option<Duration> {
    match KEEPALIVE_SECS.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

/// russh 交给通道的有界队列长度，默认 100。
///
/// 我们不读通道时，控制字符就排在这个队列后面：队列越长，Ctrl+C 之类的回复
/// 等得越久（默认 100 条、每包最多 32KB，最坏 3.2MB 排在前面）。调到 16 条
/// 最多 512KB，正常吞吐靠 TCP 自己的缓冲不受影响；实测延迟见 M1-02 报告。
const CHANNEL_BUFFER_SIZE: usize = 16;

/// 建立一条连接需要的全部信息。
///
/// `Debug` 是手写的：密码、口令和私钥内容不能出现在日志或 panic 信息里。
#[derive(Clone, Debug)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 认证方式与已有材料
    pub auth: AuthConfig,
    /// known_hosts 校验用的文件
    pub known_hosts: KnownHosts,
    /// 老服务器兼容开关：额外允许 SHA1 和 CBC 之类
    pub legacy_algorithms: bool,
}

/// 拨号链里的一跳：连到哪儿、怎么连、用哪份凭据。
#[derive(Clone)]
pub struct ChainHop {
    /// 报错和进度里显示用，例如"代理 socks5 127.0.0.1:10808"、"跳板机 jump1"、"目标 web1"
    pub label: String,
    /// 这一跳要连的地址（跳板机或目标）
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 只有第一跳的代理真正生效
    pub proxy: Option<ProxyConfig>,
    /// 这一跳用它自己那条主机记录的认证方式与凭据
    pub auth: AuthConfig,
    pub known_hosts: KnownHosts,
    pub legacy_algorithms: bool,
}

impl std::fmt::Debug for ChainHop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainHop")
            .field("label", &self.label)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("proxy", &self.proxy)
            .field("auth", &self.auth)
            .field("legacy_algorithms", &self.legacy_algorithms)
            .finish()
    }
}

/// 把某一跳的失败包上"第几跳、连的是谁"。
fn hop_failure(index: usize, label: &str, stage: &str, error: &ZsshError) -> ZsshError {
    ZsshError::Hop {
        index,
        label: label.to_owned(),
        detail: format!("{stage}失败：{error}"),
    }
}

/// 打开转发通道被拒时，把 russh 的错误码说成人话。
/// `target` 是要经跳板机去连的地址：是 IPv6 时 connect failed 多半是跳板机没有 IPv6 出口。
fn describe_channel_failure(error: russh::Error, target: &str) -> ZsshError {
    match error {
        russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::AdministrativelyProhibited) => {
            ZsshError::InvalidInput(
                "服务器禁止了 TCP 转发（administratively prohibited），跳板机用不了".to_owned(),
            )
        }
        russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed) => {
            ZsshError::InvalidInput(dialer::with_ipv6_hint(
                "跳板机连不上目标地址（connect failed）".to_owned(),
                target,
            ))
        }
        other => ZsshError::Ssh(other),
    }
}

/// 服务器的公钥信息。M1-04 之后指纹要过 known_hosts 校验，这里记录的是本次连接
/// 实际用的那把。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerKey {
    /// 例如 `ssh-ed25519`。
    pub key_type: String,
    /// OpenSSH 格式的 SHA256 指纹，例如 `SHA256:AbCd...`。
    pub fingerprint: String,
}

/// 一条已认证的 SSH 连接。
///
/// 内部持有 russh 的 `Handle`，由连接背后那个 tokio 任务独占通信。M1 后续会在
/// 这上面开多个 shell / SFTP 通道（见设计文档 4.1）。
pub struct Connection {
    /// 字段顺序是有意的：drop 时先关目标这条连接，再关跳板机。
    handle: Handle<ClientHandler>,
    /// 远程转发（ssh -R）：服务器监听的端口 → 把连进来的通道交给谁处理。
    remote_forwards: RemoteForwardTable,
    server_key: ServerKey,
    auth_outcome: AuthOutcome,
    /// 拨号链上的中间跳（跳板机）。目标会话结束、这个结构被丢掉时一起关，不会泄漏。
    /// M1-06 做连接复用时再考虑共享跳板机连接。
    jumps: Vec<Connection>,
    /// 这条连接上一共开过多少个通道（shell / exec / sftp / 转发）。
    /// 诊断和测试用：SFTP 要证明自己是开在**已有连接**上，不是偷偷又连了一条。
    channels_opened: AtomicUsize,
    /// 这条连接**实际连到的**服务器：用户名@主机:端口（见 [`server_identity`]）。
    /// 主机、用户名是空的（不应该发生）时是 `None`。
    identity: Option<String>,
}

/// 服务器身份：`用户名@主机:端口`，主机名不分大小写、去掉结尾的点。
///
/// 远端写入的互斥锁（传输队列、编辑器保存、内置编辑器保存、AI 写文件、粘贴）用它当键：
/// 同一台服务器不管是从主机库的主机、快速连接还是终端旁边的文件侧栏连上的，只要实际连到的
/// 主机、端口、用户名相同，就是同一个身份。拨号链（跳板机）取**目标那一跳**的。
pub fn server_identity(host: &str, port: u16, username: &str) -> String {
    format!(
        "{username}@{}:{port}",
        host.trim().trim_end_matches('.').to_ascii_lowercase()
    )
}

impl Connection {
    /// 直连 TCP，校验主机指纹，然后按设计文档 4.3 的顺序认证。
    pub async fn connect(
        params: ConnectParams,
        interactor: Arc<dyn Interactor>,
    ) -> Result<Self, ZsshError> {
        let ConnectParams {
            host,
            port,
            username,
            auth,
            known_hosts,
            legacy_algorithms,
        } = params;

        let stream: BoxedStream = Box::new(dialer::connect_direct(&host, port).await?);
        Self::handshake_and_authenticate(
            stream,
            &host,
            port,
            &username,
            &auth,
            known_hosts,
            legacy_algorithms,
            &interactor,
        )
        .await
    }

    /// 拨号链：按顺序连每一跳，中间跳留着（会话结束时一起关），最后一跳返回给调用方。
    ///
    /// 只有**第一跳**的代理真正生效：后面各跳的流量走在前一跳的 SSH 通道里，配了代理也
    /// 忽略（设计文档 4.2）。
    pub async fn connect_chain(
        hops: Vec<ChainHop>,
        interactor: Arc<dyn Interactor>,
    ) -> Result<Self, ZsshError> {
        if hops.is_empty() {
            return Err(ZsshError::InvalidInput("拨号链至少要有一跳".to_owned()));
        }
        let total = hops.len();
        // (显示名, 连接)：中间跳要留到最后一起关
        let mut established: Vec<(String, Connection)> = Vec::with_capacity(total);

        for (index, hop) in hops.into_iter().enumerate() {
            let number = index + 1;
            interactor
                .hop_started(HopProgress {
                    index: number,
                    total,
                    label: hop.label.clone(),
                })
                .await;

            if index > 0 && hop.proxy.is_some() {
                debug!(
                    label = %hop.label,
                    "这一跳配了代理，但流量走在前一跳的 SSH 通道里，忽略它"
                );
            }

            let stream: BoxedStream = match established.last() {
                // 第一跳：直连或者走它自己的代理
                None => match &hop.proxy {
                    Some(proxy) => dialer::connect_via_proxy(proxy, &hop.host, hop.port)
                        .await
                        .map_err(|err| hop_failure(number, &hop.label, "连接", &err))?,
                    None => Box::new(
                        dialer::connect_direct(&hop.host, hop.port)
                            .await
                            .map_err(|err| hop_failure(number, &hop.label, "连接", &err))?,
                    ),
                },
                // 后面各跳：在上一跳的连接上开 direct-tcpip 通道
                Some((previous_label, previous)) => {
                    let label = previous_label.clone();
                    previous
                        .open_direct_tcpip(&hop.host, hop.port)
                        .await
                        .map_err(|err| ZsshError::Hop {
                            index: number,
                            label: hop.label.clone(),
                            detail: format!("经跳板机 {label} 打开转发通道失败：{err}"),
                        })?
                }
            };

            let connection = Self::handshake_and_authenticate(
                stream,
                &hop.host,
                hop.port,
                &hop.username,
                &hop.auth,
                hop.known_hosts,
                hop.legacy_algorithms,
                &interactor,
            )
            .await
            .map_err(|err| hop_failure(number, &hop.label, "SSH 握手或认证", &err))?;

            established.push((hop.label, connection));
        }

        let (_, mut target) = established
            .pop()
            .ok_or_else(|| ZsshError::InvalidInput("拨号链至少要有一跳".to_owned()))?;
        target.jumps = established
            .into_iter()
            .map(|(_, connection)| connection)
            .collect();
        Ok(target)
    }

    /// SSH 握手 + 指纹校验 + 认证。直连和拨号链的每一跳都走这里。
    #[allow(clippy::too_many_arguments)]
    async fn handshake_and_authenticate(
        stream: BoxedStream,
        host: &str,
        port: u16,
        username: &str,
        auth: &AuthConfig,
        known_hosts: KnownHosts,
        legacy_algorithms: bool,
        interactor: &Arc<dyn Interactor>,
    ) -> Result<Self, ZsshError> {
        let server_key_slot = Arc::new(OnceLock::new());
        let config = Arc::new(client_config(legacy_algorithms));
        let remote_forwards = RemoteForwardTable::default();

        let handler = ClientHandler {
            remote_forwards: Arc::clone(&remote_forwards),
            host: host.to_owned(),
            port,
            known_hosts,
            interactor: Arc::clone(interactor),
            server_key: Arc::clone(&server_key_slot),
        };

        let mut handle = match timeout(
            HANDSHAKE_TIMEOUT,
            client::connect_stream(config, stream, handler),
        )
        .await
        {
            Ok(Ok(handle)) => handle,
            Ok(Err(err)) => return Err(map_handshake_error(err, host, port)),
            Err(_) => {
                return Err(ZsshError::ConnectTimeout {
                    host: host.to_owned(),
                    port,
                    seconds: HANDSHAKE_TIMEOUT.as_secs(),
                })
            }
        };

        let server_key = server_key_slot
            .get()
            .cloned()
            .ok_or(ZsshError::MissingServerKey)?;

        // 握手完成、开始认证：界面用这个事件把状态文字从"连接中"换成"认证中"。
        interactor.authentication_started().await;

        let auth_outcome =
            auth::authenticate(&mut handle, username, auth, legacy_algorithms, interactor).await?;

        info!(
            %host,
            port,
            key_type = %server_key.key_type,
            fingerprint = %server_key.fingerprint,
            "SSH 连接已建立"
        );

        let identity = (!host.trim().is_empty() && !username.trim().is_empty())
            .then(|| server_identity(host, port, username));
        Ok(Self {
            handle,
            remote_forwards,
            server_key,
            auth_outcome,
            jumps: Vec::new(),
            channels_opened: AtomicUsize::new(0),
            identity,
        })
    }

    /// 在这条连接上开一个 `direct-tcpip` 通道连到 `host:port`，拿它当下一跳的字节流。
    ///
    /// 服务器关了 TCP 转发时这里会失败（`administratively prohibited`），错误会带上去。
    async fn open_direct_tcpip(&self, host: &str, port: u16) -> Result<BoxedStream, ZsshError> {
        let open = self
            .handle
            .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0);
        match timeout(HOP_TIMEOUT, open).await {
            Ok(Ok(channel)) => Ok(Box::new(channel.into_stream())),
            Ok(Err(err)) => Err(describe_channel_failure(err, host)),
            Err(_) => Err(ZsshError::ConnectTimeout {
                host: host.to_owned(),
                port,
                seconds: HOP_TIMEOUT.as_secs(),
            }),
        }
    }

    /// 本地转发（ssh -L）用：经服务器连到 `host:port`，返回通道的字节流。
    ///
    /// `originator` 是本机这边连进来的客户端地址，只是告诉服务器（写进它的日志）。
    pub async fn open_forward_channel(
        &self,
        host: &str,
        port: u16,
        originator: std::net::SocketAddr,
    ) -> Result<russh::ChannelStream<client::Msg>, ZsshError> {
        let open = self.handle.channel_open_direct_tcpip(
            host,
            u32::from(port),
            originator.ip().to_string(),
            u32::from(originator.port()),
        );
        let channel = match timeout(HOP_TIMEOUT, open).await {
            Ok(Ok(channel)) => channel,
            Ok(Err(russh::Error::ChannelOpenFailure(
                russh::ChannelOpenFailure::AdministrativelyProhibited,
            ))) => {
                return Err(ZsshError::InvalidInput(
                    "服务器禁止了 TCP 转发（AllowTcpForwarding no）".to_owned(),
                ))
            }
            Ok(Err(russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed))) => {
                return Err(ZsshError::InvalidInput(format!(
                    "服务器连不上 {host}:{port}（connect failed）"
                )))
            }
            Ok(Err(err)) => return Err(ZsshError::Ssh(err)),
            Err(_) => {
                return Err(ZsshError::ConnectTimeout {
                    host: host.to_owned(),
                    port,
                    seconds: HOP_TIMEOUT.as_secs(),
                })
            }
        };
        self.channels_opened.fetch_add(1, Ordering::Relaxed);
        Ok(channel.into_stream())
    }

    /// 远程转发（ssh -R）：请服务器在 `bind_host:bind_port` 上监听。服务器那边有人连进来时，
    /// 通道交给 `sink`；返回服务器实际监听的端口。
    pub async fn start_remote_forward(
        &self,
        bind_host: &str,
        bind_port: u16,
        sink: tokio::sync::mpsc::UnboundedSender<ForwardedChannel>,
    ) -> Result<u32, ZsshError> {
        let port = u32::from(bind_port);
        // 先登记再发请求：服务器回复之前就可能有连接进来。端口已经有人登记了就直接拒绝，
        // 不碰原来那条（复审 F3：以前直接覆盖，失败后又删掉，原来的转发跟着坏了）。
        claim_remote_port(&self.remote_forwards, port, &sink)?;
        match timeout(HOP_TIMEOUT, self.handle.tcpip_forward(bind_host, port)).await {
            Ok(Ok(actual)) => {
                if actual != 0 && actual != port {
                    release_remote_port(&self.remote_forwards, port, &sink);
                    if let Err(err) = claim_remote_port(&self.remote_forwards, actual, &sink) {
                        let _ = timeout(
                            HOP_TIMEOUT,
                            self.handle.cancel_tcpip_forward(bind_host, actual),
                        )
                        .await;
                        return Err(err);
                    }
                    return Ok(actual);
                }
                Ok(port)
            }
            Ok(Err(err)) => {
                release_remote_port(&self.remote_forwards, port, &sink);
                Err(match err {
                    russh::Error::RequestDenied => ZsshError::InvalidInput(format!(
                        "服务器拒绝在 {bind_host}:{bind_port} 上监听（端口被占用，或者服务器关了转发）"
                    )),
                    other => ZsshError::Ssh(other),
                })
            }
            Err(_) => {
                release_remote_port(&self.remote_forwards, port, &sink);
                Err(ZsshError::ConnectTimeout {
                    host: bind_host.to_owned(),
                    port: bind_port,
                    seconds: HOP_TIMEOUT.as_secs(),
                })
            }
        }
    }

    /// 取消远程转发。连接已经断了也不算错。
    pub async fn stop_remote_forward(&self, bind_host: &str, port: u32) {
        lock_table(&self.remote_forwards).remove(&port);
        if !self.is_closed() {
            let _ = timeout(
                HOP_TIMEOUT,
                self.handle.cancel_tcpip_forward(bind_host, port),
            )
            .await;
        }
    }

    /// 这条连接实际连到的服务器身份（用户名@主机:端口）；主机或用户名是空的时是 `None`。
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// 服务器的主机密钥信息。
    pub fn server_key(&self) -> &ServerKey {
        &self.server_key
    }

    /// 连接是不是已经断了（对端断开或者 keepalive 超时）。连接池用它判断能不能复用。
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// 认证成功时用户勾了"保存"的密码或口令，调用方负责落库。
    pub fn remembered_secret(&self) -> Option<&RememberedSecret> {
        self.auth_outcome.remembered.as_ref()
    }

    /// 在这条连接上开一个新的 shell 通道，不重新认证。
    pub async fn open_shell(&self, cols: u16, rows: u16) -> Result<ShellSession, ZsshError> {
        self.open_shell_with_buffer_limit(cols, rows, DEFAULT_BUFFER_LIMIT, None)
            .await
    }

    /// 同 [`Connection::open_shell`]，但指定通道本地缓冲的上限和最近输出的环形缓冲。
    ///
    /// 压测要能快速把缓冲顶满（不然得先灌 8MB），所以上限做成参数；生产路径
    /// 一律用 [`DEFAULT_BUFFER_LIMIT`]。`ring` 给 `Some` 时，转发路径上会旁路
    /// 复制一份最近输出，供工具桥 `read_terminal` 读取。
    pub async fn open_shell_with_buffer_limit(
        &self,
        cols: u16,
        rows: u16,
        buffer_limit: usize,
        ring: Option<SharedTerminalRing>,
    ) -> Result<ShellSession, ZsshError> {
        let channel = self.open_session_channel().await?;
        ShellSession::open_with_buffer_limit(channel, cols, rows, buffer_limit, ring).await
    }

    /// 开一个会话通道，并把"服务器拒绝"翻译成能分辨的错误。
    ///
    /// 服务器达到 `MaxSessions` 上限时回的是 `administratively prohibited`（有时是
    /// `resource shortage`）——上层（SFTP 传输）据此改走专用连接，而不是把它当成
    /// "SFTP 坏了"。
    pub async fn open_session_channel(&self) -> Result<Channel<client::Msg>, ZsshError> {
        let opened = self
            .handle
            .channel_open_session()
            .await
            .map_err(|err| match err {
                russh::Error::ChannelOpenFailure(reason) => {
                    ZsshError::ChannelLimit(format!("{reason:?}"))
                }
                other => ZsshError::Ssh(other),
            })?;
        self.channels_opened.fetch_add(1, Ordering::Relaxed);
        Ok(opened)
    }

    /// 这条连接上一共开过多少个通道（诊断 / 测试用）。
    pub fn channels_opened(&self) -> usize {
        self.channels_opened.load(Ordering::Relaxed)
    }

    /// 在这条连接上开 `sftp` 子系统通道，返回双向字节流（不重新认证）。
    ///
    /// - 服务器没开 SFTP：子系统请求被拒 → [`ZsshError::SftpUnavailable`]；
    /// - 通道数到上限：→ [`ZsshError::ChannelLimit`]（上层改走专用连接）。
    ///
    /// 子系统应答（Success/Failure）之前万一先到了数据，会把那段字节接在流前面——
    /// 正常服务器是收到 INIT 才回 VERSION，不会先发数据，但丢字节比多几行代码严重。
    pub async fn open_sftp_channel(&self) -> Result<ChannelByteStream, ZsshError> {
        const SUBSYSTEM_TIMEOUT: Duration = Duration::from_secs(20);
        let mut channel = self.open_session_channel().await?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(ZsshError::Ssh)?;

        let mut early: Vec<u8> = Vec::new();
        let deadline = Instant::now() + SUBSYSTEM_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ZsshError::Shell("等 sftp 子系统的应答超时".to_owned()));
            }
            match timeout(remaining, channel.wait()).await {
                Ok(Some(russh::ChannelMsg::Success)) => break,
                // 明确被拒 / 通道被关：就是没开 SFTP。
                Ok(Some(russh::ChannelMsg::Failure)) | Ok(None) => {
                    return Err(ZsshError::SftpUnavailable)
                }
                Ok(Some(russh::ChannelMsg::Data { data })) => early.extend_from_slice(&data),
                Ok(Some(russh::ChannelMsg::ExtendedData { data, .. })) => {
                    early.extend_from_slice(&data)
                }
                Ok(Some(_)) => {}
                Err(_) => return Err(ZsshError::Shell("等 sftp 子系统的应答超时".to_owned())),
            }
        }
        let stream = channel.into_stream();
        Ok(PrefixedStream::with_prefix(early, stream))
    }

    /// 在这条连接上执行一条命令（exec 通道，不带 PTY），返回 stdout。
    ///
    /// 服务器状态栏这类"一次一发"的采集用它：不碰会话的 shell，也不进会话的
    /// 流控（见设计文档 5.2 的硬规则：采集不能阻塞会话的读写和 ack——它是
    /// **独立通道**，最多卡死自己的 exec，卡不到别的）。
    pub async fn exec(&self, command: &str) -> Result<String, ZsshError> {
        let mut channel = self.open_session_channel().await?;
        channel
            .exec(true, command)
            .await
            .map_err(|err| ZsshError::Shell(format!("执行命令失败：{err}")))?;
        let mut output = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_ok = true;
        while let Some(event) = channel.wait().await {
            match event {
                russh::ChannelMsg::Data { ref data } => output.extend_from_slice(data),
                russh::ChannelMsg::ExtendedData { ref data, .. } => stderr.extend_from_slice(data),
                russh::ChannelMsg::ExitStatus { exit_status } => {
                    exit_ok = exit_status == 0;
                }
                // 同 exec_capture：EOF 可能先于 exit-status 到达（OpenSSH 的竞争窗口），
                // EOF 不是终态，等 Close / 通道结束。
                russh::ChannelMsg::Eof => {}
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        if !exit_ok {
            let stderr_text = String::from_utf8_lossy(&stderr);
            return Err(ZsshError::Shell(format!(
                "命令退出码非 0：{}",
                stderr_text.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    /// 同 [`Connection::exec`]，但带超时和每个流的字节上限，返回完整结果。
    ///
    /// AI-1 工具桥的 `run_command` 用它。超时（`timed_out`）时通道会被关掉，
    /// stdout / stderr 里是超时前拿到的部分，退出码为 `None`。
    pub async fn exec_capture(
        &self,
        command: &str,
        wait: Duration,
        max_stream_bytes: usize,
    ) -> Result<ExecCapture, ZsshError> {
        self.exec_capture_inner(command, wait, max_stream_bytes, None)
            .await
    }

    /// 同 [`Connection::exec_capture`]，但可以在等待期间取消。
    ///
    /// 取消时**关闭执行通道**（服务器上的进程会收到通道关闭，OpenSSH 给它发 SIGHUP；
    /// 实测结果见 M3-5b 报告），返回的 `cancelled = true`，退出码为 `None`。
    pub async fn exec_capture_cancellable(
        &self,
        command: &str,
        wait: Duration,
        max_stream_bytes: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ExecCapture, ZsshError> {
        self.exec_capture_inner(command, wait, max_stream_bytes, Some(cancel))
            .await
    }

    /// 同 [`Connection::exec_capture_cancellable`]，但发命令之前核对的是**完整的**请求有效性
    /// （AI 工具桥：停止、目标被移出对话、对话销毁），而不只是停止标志（审计 C04 后台链路）。
    ///
    /// - 开通道要等一个来回：这期间请求失效就不再等，立刻返回原因；通道迟到地开好了由后台
    ///   任务关掉，不在服务器上留空通道；
    /// - 通道开好之后、发 exec 之前再 [`ExecGuard::check`] 一次——放行之后到这里可能等了很久；
    /// - 命令发出去之后只有 [`ExecGuard::stop_requested`] 会关闭执行通道：目标被移出不打断
    ///   已经在跑的命令（半路关掉可能比让它跑完更糟）。
    ///
    /// 出错和拒绝都返回给人看的原因（拒绝原因原样转交，不再包一层"打开 shell 失败"）。
    pub async fn exec_capture_guarded(
        self: &Arc<Self>,
        command: &str,
        wait: Duration,
        max_stream_bytes: usize,
        guard: &dyn ExecGuard,
    ) -> Result<ExecCapture, String> {
        guard.check()?;
        let connection = Arc::clone(self);
        // 开通道放进独立任务：放弃等待时不能直接丢掉开通道的 future——russh 已经把开通道
        // 请求发出去了，丢掉它服务器那边的通道就没人关，会一直占着 MaxSessions 名额。
        let open = tokio::spawn(async move { connection.open_session_channel().await });
        let channel = open_unless_invalidated(open, guard, |channel: Channel<client::Msg>| {
            Box::pin(async move {
                let _ = channel.close().await;
            })
        })
        .await?
        .map_err(|err| err.to_string())?;
        if let Err(reason) = guard.check() {
            let _ = channel.close().await;
            return Err(reason);
        }
        let stopped = || guard.stop_requested();
        run_exec(channel, command, wait, max_stream_bytes, Some(&stopped))
            .await
            .map_err(|err| err.to_string())
    }

    async fn exec_capture_inner(
        &self,
        command: &str,
        wait: Duration,
        max_stream_bytes: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ExecCapture, ZsshError> {
        let channel = self.open_session_channel().await?;
        // 开通道要等一个来回：这期间取消了就不发命令。以前到接收循环才第一次看取消标志，
        // 命令已经发出去了，快的命令在关通道之前就做完了（R-9 F12）。
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = channel.close().await;
            return Err(ZsshError::Shell("已取消，命令没有发出".to_owned()));
        }
        match cancel {
            Some(flag) => {
                let stopped = || flag.load(Ordering::SeqCst);
                run_exec(channel, command, wait, max_stream_bytes, Some(&stopped)).await
            }
            None => run_exec(channel, command, wait, max_stream_bytes, None).await,
        }
    }

    /// 主动断开，尽力发出 SSH 层的 disconnect 再让对方收尾。
    pub async fn disconnect(&self) -> Result<(), ZsshError> {
        self.handle
            .disconnect(Disconnect::ByApplication, "closed by user", "en")
            .await?;
        Ok(())
    }
}

/// 工具桥 exec 通道每个流的输出上限（卡片 AI-1：每个流最多 64KB）。
pub const EXEC_MAX_STREAM_BYTES: usize = 64 * 1024;

/// [`Connection::exec_capture`] 的一次执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecCapture {
    pub stdout: String,
    pub stderr: String,
    /// stdout 被截掉的字节数，0 表示没截。
    pub stdout_truncated_bytes: u64,
    /// stderr 被截掉的字节数，0 表示没截。
    pub stderr_truncated_bytes: u64,
    /// 退出码；超时、取消或服务器没给时是 `None`。
    pub exit_code: Option<u32>,
    pub timed_out: bool,
    /// 用户取消：通道已经关掉（服务器上的进程是否被终止见调用方的实测说明）。
    pub cancelled: bool,
    pub duration_ms: u64,
}

/// 发 exec 之前要再核对一次的"这次请求还有效吗"（AI 工具桥的 `ExecGate` 实现它）。
///
/// 放在 ssh 层而不是 ai 层：发 exec 的边界在这里，ssh 层不该知道"对话""目标列表"这些概念。
pub trait ExecGuard: Send + Sync {
    /// 现在还能发命令吗；不能时是原因（原样交给调用方）。
    fn check(&self) -> Result<(), String>;
    /// 等到请求失效为止，返回原因。必须能被安全地丢弃（和开通道一起 `select!`）。
    fn invalidated(&self) -> BoxFuture<'_, String>;
    /// 命令发出去之后要不要关闭执行通道（只看停止，不看目标移出）。
    fn stop_requested(&self) -> bool;
}

/// 等开通道的任务，同时等请求失效：先失效就不再等，把迟到的通道交给 `discard` 收掉。
///
/// 外层 `Err` 是失效原因（或开通道的任务异常结束），内层是开通道本身的结果。
async fn open_unless_invalidated<T, E, D>(
    mut open: tokio::task::JoinHandle<Result<T, E>>,
    guard: &dyn ExecGuard,
    discard: D,
) -> Result<Result<T, E>, String>
where
    T: Send + 'static,
    E: Send + 'static,
    D: FnOnce(T) -> BoxFuture<'static, ()> + Send + 'static,
{
    tokio::select! {
        joined = &mut open => joined.map_err(|err| format!("打开执行通道的任务异常结束：{err}")),
        reason = guard.invalidated() => {
            tokio::spawn(async move {
                if let Ok(Ok(late)) = open.await {
                    discard(late).await;
                }
            });
            Err(reason)
        }
    }
}

/// 在开好的通道上发命令、收输出，直到结束 / 超时 / `stopped()` 为真（关闭通道）。
async fn run_exec(
    mut channel: Channel<client::Msg>,
    command: &str,
    wait: Duration,
    max_stream_bytes: usize,
    stopped: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<ExecCapture, ZsshError> {
    channel
        .exec(true, command)
        .await
        .map_err(|err| ZsshError::Shell(format!("执行命令失败：{err}")))?;

    let started = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_truncated = 0u64;
    let mut stderr_truncated = 0u64;
    let mut exit_code = None;
    let mut cancelled = false;
    let timed_out = loop {
        if stopped.is_some_and(|stopped| stopped()) {
            cancelled = true;
            break false;
        }
        let remaining = wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break true;
        }
        // 能取消时按 100ms 一段等，否则一整段等（保持原来的行为）。
        let slice = if stopped.is_some() {
            remaining.min(Duration::from_millis(100))
        } else {
            remaining
        };
        match timeout(slice, channel.wait()).await {
            Err(_) if stopped.is_some() => continue,
            Err(_) => break true,
            Ok(Some(event)) => match event {
                russh::ChannelMsg::Data { ref data } => {
                    append_capped(&mut stdout, &mut stdout_truncated, data, max_stream_bytes);
                }
                russh::ChannelMsg::ExtendedData { ref data, .. } => {
                    append_capped(&mut stderr, &mut stderr_truncated, data, max_stream_bytes);
                }
                russh::ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = Some(exit_status);
                }
                // 有些服务器（OpenSSH 就会）先发 EOF 再发 exit-status，顺序是赛跑
                // 出来的：EOF 不是终态，等 Close / 通道结束才算完，否则会丢退出码。
                russh::ChannelMsg::Eof => {}
                russh::ChannelMsg::Close => break false,
                _ => {}
            },
            Ok(None) => break false,
        }
    };
    if timed_out || cancelled {
        let _ = channel.close().await;
    }

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(ExecCapture {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated_bytes: stdout_truncated,
        stderr_truncated_bytes: stderr_truncated,
        exit_code,
        timed_out,
        cancelled,
        duration_ms,
    })
}

/// 把"记录在哪儿"翻译成给人看的两段文字。
fn describe_source(stored_in: &StoredIn, host: &str, port: u16) -> (String, String) {
    (stored_in.describe(), stored_in.advice(host, port))
}

fn append_capped(buffer: &mut Vec<u8>, truncated: &mut u64, data: &[u8], cap: usize) {
    let remaining = cap.saturating_sub(buffer.len());
    if data.len() <= remaining {
        buffer.extend_from_slice(data);
    } else {
        buffer.extend_from_slice(&data[..remaining]);
        *truncated += (data.len() - remaining) as u64;
    }
}

impl ClientHandler {
    /// 指纹变化时还是让界面弹一次红框（让用户看到新指纹），但决定权不在用户手里。
    async fn tell_user_key_changed(&self, server_key: &ServerKey) {
        let info = HostKeyInfo {
            host: self.host.clone(),
            port: self.port,
            key_type: server_key.key_type.clone(),
            fingerprint: server_key.fingerprint.clone(),
            status: HostKeyStatus::Changed,
        };
        let decision = self.interactor.confirm_host_key(info).await;
        warn!(host = %self.host, ?decision, "主机指纹变化，阻断连接");
    }
}

/// 客户端配置。老算法兼容开关只影响算法列表。
pub(crate) fn client_config(legacy_algorithms: bool) -> Config {
    let mut config = Config {
        keepalive_interval: keepalive_interval(),
        keepalive_max: KEEPALIVE_MAX,
        channel_buffer_size: CHANNEL_BUFFER_SIZE,
        ..Config::default()
    };
    config.preferred = if legacy_algorithms {
        legacy_preferred()
    } else {
        modern_preferred()
    };
    config
}

/// 默认（不开兼容开关）的算法列表。
///
/// russh 0.64 的默认 `key` 列表末尾仍带 SHA1 的 `ssh-rsa`，服务器只给 ssh-rsa 主机
/// 密钥时会静默降级。项目的策略是"现代默认、显式兼容"：SHA1 只在用户打开兼容
/// 开关时才允许，所以这里显式去掉，RSA 主机密钥仍可走 rsa-sha2-256/512。
fn modern_preferred() -> russh::Preferred {
    let mut preferred = russh::Preferred::default();
    let keys: Vec<Algorithm> = preferred
        .key
        .iter()
        .filter(|algorithm| **algorithm != Algorithm::Rsa { hash: None })
        .cloned()
        .collect();
    preferred.key = keys.into();
    preferred
}

/// 老服务器（比如 CentOS 7 上的 OpenSSH 7.4，甚至更老的）需要的算法。
///
/// 在 russh 默认列表后面追加：默认的现代算法仍然优先，只有服务器不支持时才会
/// 退到这些。打开这个开关等于接受 SHA1 和 CBC 的安全强度，界面上要写清楚。
fn legacy_preferred() -> russh::Preferred {
    let mut preferred = russh::Preferred::default();

    let mut kexes = preferred.kex.into_owned();
    kexes.push(kex::DH_G14_SHA1);
    kexes.push(kex::DH_G1_SHA1);
    preferred.kex = kexes.into();

    // `key` 同时用于主机密钥和用户密钥。默认列表里已经有 `ssh-rsa`（SHA1），
    // 这里显式再放一次，保证打开开关就一定在列表里。
    let mut keys = preferred.key.into_owned();
    if !keys.contains(&Algorithm::Rsa { hash: None }) {
        keys.push(Algorithm::Rsa { hash: None });
    }
    preferred.key = keys.into();

    let mut ciphers = preferred.cipher.into_owned();
    ciphers.push(cipher::AES_128_CBC);
    ciphers.push(cipher::AES_192_CBC);
    ciphers.push(cipher::AES_256_CBC);
    preferred.cipher = ciphers.into();

    preferred
}

/// 把 russh 的算法协商错误换成能看懂、能指导用户的错误。
fn map_handshake_error(err: ZsshError, host: &str, port: u16) -> ZsshError {
    match err {
        ZsshError::Ssh(russh::Error::NoCommonAlgo { kind, ours, theirs }) => {
            ZsshError::NoCommonAlgorithm(Box::new(crate::error::NoCommonAlgorithmInfo {
                host: host.to_owned(),
                port,
                kind: format!("{kind:?}"),
                ours: ours.join(", "),
                theirs: theirs.join(", "),
            }))
        }
        other => other,
    }
}

/// russh 客户端回调：校验主机密钥（设计文档 4.4）。
/// 服务器那边连进来的一条远程转发连接，交给转发任务去连本机这边的目标。
pub struct ForwardedChannel {
    pub channel: Channel<client::Msg>,
    pub reply: client::ChannelOpenHandle,
    /// 服务器那边连进来的客户端，`地址:端口`。
    pub originator: String,
}

type RemoteForwardTable =
    Arc<std::sync::Mutex<HashMap<u32, tokio::sync::mpsc::UnboundedSender<ForwardedChannel>>>>;

/// 登记一个远程转发端口；已经有别的转发登记了这个端口就报错，原来的不动。
fn claim_remote_port(
    table: &RemoteForwardTable,
    port: u32,
    sink: &tokio::sync::mpsc::UnboundedSender<ForwardedChannel>,
) -> Result<(), ZsshError> {
    match lock_table(table).entry(port) {
        std::collections::hash_map::Entry::Occupied(_) => Err(ZsshError::InvalidInput(format!(
            "这条连接上已经有一个远程转发在监听端口 {port}"
        ))),
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(sink.clone());
            Ok(())
        }
    }
}

/// 撤销自己的登记：只删自己登记的那一项，别的转发登记的同一端口不动。
fn release_remote_port(
    table: &RemoteForwardTable,
    port: u32,
    sink: &tokio::sync::mpsc::UnboundedSender<ForwardedChannel>,
) {
    let mut table = lock_table(table);
    if table
        .get(&port)
        .is_some_and(|existing| existing.same_channel(sink))
    {
        table.remove(&port);
    }
}

fn lock_table(
    table: &RemoteForwardTable,
) -> std::sync::MutexGuard<'_, HashMap<u32, tokio::sync::mpsc::UnboundedSender<ForwardedChannel>>> {
    table
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct ClientHandler {
    remote_forwards: RemoteForwardTable,
    host: String,
    port: u16,
    known_hosts: KnownHosts,
    interactor: Arc<dyn Interactor>,
    server_key: Arc<OnceLock<ServerKey>>,
}

impl client::Handler for ClientHandler {
    type Error = ZsshError;

    /// 远程转发进来的连接：按服务器报的监听端口找到对应的转发任务，交给它去连本机目标。
    /// 这里不能等连接建立（会卡住整条 SSH 连接的消息循环），只转手。没人认领就拒绝。
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let sink = lock_table(&self.remote_forwards)
            .get(&connected_port)
            .cloned();
        let forwarded = ForwardedChannel {
            channel,
            reply,
            originator: format!("{originator_address}:{originator_port}"),
        };
        match sink {
            Some(sink) => {
                if let Err(rejected) = sink.send(forwarded) {
                    rejected
                        .0
                        .reply
                        .reject(russh::ChannelOpenFailure::ConnectFailed)
                        .await;
                }
            }
            None => {
                forwarded
                    .reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
            }
        }
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 证书和普通公钥都归一到 KeyData：指纹算的是被认证的那把公钥。
        let key_data = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.key_data(),
            PublicKeyOrCertificate::Certificate(cert) => cert.public_key(),
        };
        let key_type = key_data.algorithm().to_string();
        let fingerprint = key_data.fingerprint(HashAlg::Sha256).to_string();
        let public = PublicKey::new(key_data.clone(), "");

        // rekey：同一条连接上第二次收到主机密钥时必须是同一把，否则断开
        if let Some(first) = self.server_key.get() {
            if first.key_type != key_type || first.fingerprint != fingerprint {
                warn!(host = %self.host, "rekey 时主机密钥变了，断开");
                return Err(ZsshError::HostKeyRekeyMismatch {
                    host: self.host.clone(),
                    port: self.port,
                    first: first.fingerprint.clone(),
                    again: fingerprint,
                });
            }
            return Ok(true);
        }

        let server_key = ServerKey {
            key_type,
            fingerprint,
        };
        // 第二次回调不会覆盖第一次的结果（上面已经拦住了）
        let _ = self.server_key.set(server_key.clone());

        info!(
            host = %self.host,
            key_type = %server_key.key_type,
            fingerprint = %server_key.fingerprint,
            "收到服务器主机密钥"
        );

        let lookup = self.known_hosts.lookup(&self.host, self.port, &public);
        match lookup {
            HostKeyLookup::Match => Ok(true),

            // 没见过，或者只记过别的密钥类型：问用户
            HostKeyLookup::Unknown | HostKeyLookup::OtherKeyTypes => {
                let status = if matches!(lookup, HostKeyLookup::OtherKeyTypes) {
                    HostKeyStatus::OtherKeyTypesSaved
                } else {
                    HostKeyStatus::Unknown
                };
                let info = HostKeyInfo {
                    host: self.host.clone(),
                    port: self.port,
                    key_type: server_key.key_type.clone(),
                    fingerprint: server_key.fingerprint.clone(),
                    status,
                };

                match self.interactor.confirm_host_key(info).await {
                    HostKeyDecision::TrustAndSave => {
                        self.known_hosts.append(&self.host, self.port, &public)?;
                        info!(host = %self.host, "已保存主机指纹");
                        Ok(true)
                    }
                    HostKeyDecision::TrustOnce => {
                        info!(host = %self.host, "本次信任主机指纹，不保存");
                        Ok(true)
                    }
                    HostKeyDecision::Reject => Err(ZsshError::HostKeyRejected {
                        host: self.host.clone(),
                        port: self.port,
                        fingerprint: server_key.fingerprint.clone(),
                    }),
                }
            }

            // 指纹变化：弹窗只让用户关掉，就算回答"信任"也一律拒绝（设计文档 4.4）
            HostKeyLookup::Changed {
                stored_fingerprint,
                stored_in,
            } => {
                self.tell_user_key_changed(&server_key).await;
                let (stored_in, advice) = describe_source(&stored_in, &self.host, self.port);
                Err(ZsshError::HostKeyChanged(Box::new(HostKeyMismatch {
                    host: self.host.clone(),
                    port: self.port,
                    key_type: server_key.key_type,
                    stored: stored_fingerprint,
                    presented: server_key.fingerprint,
                    stored_in,
                    advice,
                })))
            }

            // @revoked：命中就当指纹变化处理
            HostKeyLookup::Revoked {
                stored_fingerprint,
                stored_in,
            } => {
                self.tell_user_key_changed(&server_key).await;
                let (stored_in, advice) = describe_source(&stored_in, &self.host, self.port);
                Err(ZsshError::HostKeyRevoked(Box::new(
                    crate::error::HostKeyRevokedInfo {
                        host: self.host.clone(),
                        port: self.port,
                        stored: stored_fingerprint,
                        presented: server_key.fingerprint,
                        stored_in,
                        advice,
                    },
                )))
            }

            // 有一份 known_hosts 读不了：不知道里面有没有这台主机的记录，按拒绝处理，
            // 不能退化成"没见过"去问用户信不信任（R-9 F10）。
            HostKeyLookup::Unreadable { path, error } => {
                warn!(host = %self.host, %error, "known_hosts 读不了，拒绝连接");
                Err(ZsshError::HostKeyStore(format!(
                    "读不了 {}：{error}。无法确认这台主机的指纹是否已登记、是否被吊销，已拒绝连接；\
                     请检查这个文件的权限后重试",
                    path.display()
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_second_remote_forward_on_the_same_port_leaves_the_first_alone() {
        let table = RemoteForwardTable::default();
        let (first, _first_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second, _second_rx) = tokio::sync::mpsc::unbounded_channel();
        claim_remote_port(&table, 15432, &first).unwrap();
        let err = claim_remote_port(&table, 15432, &second).unwrap_err();
        assert!(err.to_string().contains("15432"), "{err}");
        // 第二个失败后的清理不能删掉第一个的登记
        release_remote_port(&table, 15432, &second);
        assert!(lock_table(&table).get(&15432).unwrap().same_channel(&first));
        release_remote_port(&table, 15432, &first);
        assert!(lock_table(&table).is_empty());
    }
    #[test]
    fn server_identity_uses_the_actual_host_port_and_user() {
        assert_eq!(
            server_identity("files.example", 22, "alice"),
            "alice@files.example:22"
        );
        // 主机名不分大小写、去掉结尾的点、去掉两端空白：同一台服务器的不同写法是同一个身份。
        assert_eq!(
            server_identity(" Files.Example. ", 22, "alice"),
            server_identity("files.example", 22, "alice")
        );
        // 端口、用户名不同就是不同的身份。
        assert_ne!(
            server_identity("files.example", 2222, "alice"),
            server_identity("files.example", 22, "alice")
        );
        assert_ne!(
            server_identity("files.example", 22, "bob"),
            server_identity("files.example", 22, "alice")
        );
        // 主机库的主机和快速连接填的是同一个主机、端口、用户名，就是同一个身份——
        // 身份只由实际连接的三样决定，和是从哪条路径连上的无关。
        assert_eq!(
            server_identity("10.0.0.5", 22, "deploy"),
            server_identity("10.0.0.5", 22, "deploy")
        );
    }

    use super::*;

    #[test]
    fn legacy_mode_appends_old_algorithms_after_the_modern_ones() {
        let modern = client_config(false);
        let legacy = client_config(true);

        // 默认不带的老算法
        assert!(!modern.preferred.kex.contains(&kex::DH_G1_SHA1));
        assert!(!modern.preferred.kex.contains(&kex::DH_G14_SHA1));
        assert!(!modern.preferred.cipher.contains(&cipher::AES_128_CBC));

        // 打开开关之后追加在后面，现代算法仍然排在前面
        assert!(legacy.preferred.kex.contains(&kex::DH_G1_SHA1));
        assert!(legacy.preferred.kex.contains(&kex::DH_G14_SHA1));
        assert!(legacy.preferred.cipher.contains(&cipher::AES_128_CBC));
        assert!(legacy.preferred.cipher.contains(&cipher::AES_256_CBC));
        assert_eq!(
            legacy.preferred.kex.first(),
            modern.preferred.kex.first(),
            "老算法只能追加在默认列表后面"
        );
        assert!(
            legacy
                .preferred
                .key
                .contains(&Algorithm::Rsa { hash: None }),
            "老服务器要能用 SHA1 的 ssh-rsa 主机密钥和用户密钥"
        );
    }

    #[test]
    fn legacy_mode_is_off_by_default() {
        let config = client_config(false);
        assert_eq!(
            config.keepalive_interval,
            keepalive_interval().or(Some(KEEPALIVE_INTERVAL)),
            "默认 30 秒，除非测试把全局值改过"
        );
        assert_eq!(config.channel_buffer_size, CHANNEL_BUFFER_SIZE);
        assert_eq!(
            config.preferred.kex.len(),
            russh::Preferred::default().kex.len(),
            "不开开关时算法列表要和 russh 默认完全一致"
        );
    }

    /// 可控的有效性（ExecGuard 测试替身）。
    #[derive(Default)]
    struct FakeGuard {
        invalid: std::sync::atomic::AtomicBool,
        notify: tokio::sync::Notify,
    }

    impl FakeGuard {
        fn invalidate(&self) {
            self.invalid.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    impl ExecGuard for FakeGuard {
        fn check(&self) -> Result<(), String> {
            if self.invalid.load(Ordering::SeqCst) {
                Err("这台服务器已经被移出这个对话，这次操作已取消".to_owned())
            } else {
                Ok(())
            }
        }

        fn invalidated(&self) -> BoxFuture<'_, String> {
            Box::pin(async move {
                loop {
                    let notified = self.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if let Err(reason) = self.check() {
                        return reason;
                    }
                    notified.await;
                }
            })
        }

        fn stop_requested(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn removing_the_target_while_the_channel_opens_stops_waiting_and_closes_the_late_channel()
    {
        // 审计 C04：开通道（一个来回）期间目标被移出，要立刻放弃等待；通道迟到地开好了也要
        // 关掉，不能在服务器上留一条空通道。
        let (open_tx, open_rx) = tokio::sync::oneshot::channel::<u32>();
        let open = tokio::spawn(async move { open_rx.await.map_err(|_| "开通道失败") });
        let guard = Arc::new(FakeGuard::default());
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<u32>();
        let waiting = {
            let guard = Arc::clone(&guard);
            tokio::spawn(async move {
                open_unless_invalidated(open, guard.as_ref(), move |late: u32| {
                    Box::pin(async move {
                        let _ = closed_tx.send(late);
                    })
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished(), "通道还没开好、请求还有效：要等");
        guard.invalidate();
        let outcome = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("失效后不再等开通道")
            .expect("不 panic");
        assert!(outcome.expect_err("已失效").contains("移出"));
        // 通道这时才开好：交给收尾的人关掉。
        let _ = open_tx.send(7);
        let closed = tokio::time::timeout(Duration::from_secs(5), closed_rx)
            .await
            .expect("迟到的通道要被关掉")
            .expect("收尾回调要被调用");
        assert_eq!(closed, 7);
    }

    #[tokio::test]
    async fn a_channel_that_opens_while_still_valid_is_handed_back() {
        let open = tokio::spawn(async { Ok::<u32, &'static str>(3) });
        let guard = FakeGuard::default();
        let outcome = open_unless_invalidated(open, &guard, |_late: u32| Box::pin(async {})).await;
        assert_eq!(outcome, Ok(Ok(3)));
    }

    #[test]
    fn modern_mode_drops_sha1_ssh_rsa_but_keeps_rsa_sha2() {
        let sha1_rsa = Algorithm::Rsa { hash: None };
        let modern = client_config(false);
        assert!(
            !modern.preferred.key.contains(&sha1_rsa),
            "默认配置不能协商 SHA1 的 ssh-rsa：{:?}",
            modern.preferred.key
        );
        for wanted in [
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
            Algorithm::Ed25519,
        ] {
            assert!(
                modern.preferred.key.contains(&wanted),
                "现代算法 {wanted:?} 要保留"
            );
        }
        // 只去掉这一项，其余顺序不变
        let expected: Vec<Algorithm> = russh::Preferred::default()
            .key
            .iter()
            .filter(|algorithm| **algorithm != sha1_rsa)
            .cloned()
            .collect();
        assert_eq!(modern.preferred.key.as_ref(), expected.as_slice());

        let legacy = client_config(true);
        assert!(
            legacy.preferred.key.contains(&sha1_rsa),
            "兼容模式要保留 SHA1 的 ssh-rsa"
        );
    }

    #[test]
    fn append_capped_keeps_only_the_first_cap_bytes() {
        let mut buffer = Vec::new();
        let mut truncated = 0u64;
        append_capped(&mut buffer, &mut truncated, b"ab", 4);
        append_capped(&mut buffer, &mut truncated, b"cd", 4);
        assert_eq!(buffer, b"abcd");
        assert_eq!(truncated, 0);
        append_capped(&mut buffer, &mut truncated, b"ef", 4);
        assert_eq!(buffer, b"abcd", "超过上限的字节不再进缓冲");
        assert_eq!(truncated, 2);
    }

    #[test]
    fn append_capped_splits_a_chunk_that_crosses_the_limit() {
        let mut buffer = Vec::new();
        let mut truncated = 0u64;
        append_capped(&mut buffer, &mut truncated, b"abcdef", 4);
        assert_eq!(buffer, b"abcd");
        assert_eq!(truncated, 2);
    }

    #[test]
    fn append_capped_counts_overflow_across_chunks() {
        let mut buffer = Vec::new();
        let mut truncated = 0u64;
        for chunk in [b"a".as_slice(), b"bcd", b"ef", b"gh"] {
            append_capped(&mut buffer, &mut truncated, chunk, 4);
        }
        assert_eq!(buffer, b"abcd");
        assert_eq!(truncated, 4);
    }
}
