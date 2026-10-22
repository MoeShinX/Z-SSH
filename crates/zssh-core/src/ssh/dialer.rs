//! 拨号链：拿到一个能读写的字节流，交给 `russh::client::connect_stream`。
//!
//! 一个 dialer 只负责"怎么把字节流弄到手"：直连、SOCKS5、HTTP CONNECT，外加跳板机的
//! `direct-tcpip` 通道。SSH 握手、指纹校验和认证都在 `connection.rs` 那一层。

use std::borrow::Cow;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_socks::tcp::Socks5Stream;
use tokio_socks::TargetAddr;
use tracing::debug;

use crate::error::ZsshError;
use crate::model::ProxyKind;

/// 这一跳（TCP 或代理握手）的超时。跳板机那一跳的 SSH 握手另有自己的超时。
pub const HOP_TIMEOUT: Duration = Duration::from_secs(15);

/// HTTP CONNECT 响应头的上限，防止挂在一个不出头的服务器上。
const MAX_HTTP_HEAD: usize = 16 * 1024;

/// 既能读也能写的流。
///
/// trait object 只能有一个非自动 trait，`dyn AsyncRead + AsyncWrite` 不合法，所以
/// 包一层（russh 自己的 agent 流也是这个写法）。
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> ByteStream for T {}

/// 交给 russh 的字节流。直连、代理和跳板机通道的类型不一样，统一装箱。
pub type BoxedStream = Box<dyn ByteStream>;

/// 一个代理的配置。
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// `Debug` 手写：代理密码不能进日志。
impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ProxyConfig {
    /// 报错和进度里显示用，例如 `socks5 127.0.0.1:10808`。
    pub fn label(&self) -> String {
        format!("{} {}:{}", self.kind.as_str(), self.host, self.port)
    }
}

/// 直连 TCP。
///
/// 域名解析出多个地址时**依次尝试**，直到有一个连上为止（M1-01 遗留：原来只试第一个）。
/// 所有地址共用 [`HOP_TIMEOUT`] 这一份预算；全都失败时报错会列出试过的地址。
pub async fn connect_direct(host: &str, port: u16) -> Result<TcpStream, ZsshError> {
    let attempt = async {
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|source| ZsshError::Dns {
                host: host.to_owned(),
                source,
            })?
            .collect();
        if addresses.is_empty() {
            return Err(ZsshError::Dns {
                host: host.to_owned(),
                source: io::Error::new(io::ErrorKind::NotFound, "没有解析到任何地址"),
            });
        }

        let mut tried: Vec<String> = Vec::with_capacity(addresses.len());
        let mut last_error: Option<io::Error> = None;

        for address in addresses {
            tried.push(address.to_string());
            match TcpStream::connect(address).await {
                Ok(stream) => {
                    set_nodelay(&stream, host, port, &tried)?;
                    if tried.len() > 1 {
                        debug!(%host, port, address = %address, "换了一个地址连上了");
                    }
                    return Ok(stream);
                }
                Err(error) => {
                    debug!(%host, port, address = %address, %error, "这个地址连不上，试下一个");
                    last_error = Some(error);
                }
            }
        }

        Err(ZsshError::Connect {
            host: host.to_owned(),
            port,
            tried: tried.join(", "),
            source: last_error
                .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "没有可用的地址")),
        })
    };

    match timeout(HOP_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(ZsshError::ConnectTimeout {
            host: host.to_owned(),
            port,
            seconds: HOP_TIMEOUT.as_secs(),
        }),
    }
}

fn set_nodelay(
    stream: &TcpStream,
    host: &str,
    port: u16,
    tried: &[String],
) -> Result<(), ZsshError> {
    // 交互式终端不能让 Nagle 攒小包，否则每次按键最多多等 40ms。
    stream
        .set_nodelay(true)
        .map_err(|source| ZsshError::Connect {
            host: host.to_owned(),
            port,
            tried: tried.join(", "),
            source,
        })
}

/// 经代理连到 `host:port`，拿到一条能读写的流。
///
/// 目标主机名**原样交给代理**（远程 DNS），本地不解析。
pub async fn connect_via_proxy(
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<BoxedStream, ZsshError> {
    let attempt = async {
        // 代理地址自己也可能解析出多个地址（比如 localhost），同样依次尝试
        let socket = connect_direct(&proxy.host, proxy.port).await?;
        match proxy.kind {
            ProxyKind::Socks5 => socks5_handshake(socket, proxy, host, port).await,
            ProxyKind::Http => http_connect_handshake(socket, proxy, host, port).await,
        }
    };

    match timeout(HOP_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(ZsshError::Proxy {
            proxy: proxy.label(),
            detail: format!("握手超过 {} 秒", HOP_TIMEOUT.as_secs()),
        }),
    }
}

/// SOCKS5：目标按类型结构化交给 tokio-socks。IP 字面量走 IPv4/IPv6 地址类型，
/// 其余按域名原样交给代理（远程 DNS）。
async fn socks5_handshake(
    socket: TcpStream,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<BoxedStream, ZsshError> {
    let target = socks5_target(host, port).map_err(|detail| ZsshError::Proxy {
        proxy: proxy.label(),
        detail,
    })?;
    let result = match &proxy.username {
        Some(username) => Socks5Stream::connect_with_password_and_socket(
            socket,
            target,
            username,
            proxy.password.as_deref().unwrap_or(""),
        )
        .await
        .map(|stream| Box::new(stream) as BoxedStream),
        None => Socks5Stream::connect_with_socket(socket, target)
            .await
            .map(|stream| Box::new(stream) as BoxedStream),
    };

    result.map_err(|error| {
        let detail = describe_socks_error(&error);
        ZsshError::Proxy {
            proxy: proxy.label(),
            detail: if socks_error_may_be_missing_ipv6(&error) {
                with_ipv6_hint(detail, host)
            } else {
                detail
            },
        }
    })
}

/// 目标是 IPv6 地址、代理或跳板机却连不过去时补的一句：它们多半只有 IPv4 出口，
/// 真正去连目标的是它们，本机有没有 IPv6 都没用。
pub(crate) const IPV6_EGRESS_HINT: &str = "目标是 IPv6 地址，代理或跳板机可能没有 IPv6 出口：\
    可以改填目标的 IPv4 或域名，或者经一台有 IPv6 的跳板机中转";

/// 主机地址是不是 IPv6 字面量（允许方括号和 zone id，例如 `[fe80::1%3]`）。
pub(crate) fn is_ipv6_literal(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    let without_zone = bare.split('%').next().unwrap_or(bare);
    without_zone.parse::<std::net::Ipv6Addr>().is_ok()
}

/// 目标是 IPv6 地址时在报错后面补上 [`IPV6_EGRESS_HINT`]。
pub(crate) fn with_ipv6_hint(detail: String, host: &str) -> String {
    if is_ipv6_literal(host) {
        format!("{detail}。{IPV6_EGRESS_HINT}")
    } else {
        detail
    }
}

/// 代理回的这几种错误，目标是 IPv6 时多半是代理没有 IPv6 出口。
fn socks_error_may_be_missing_ipv6(error: &tokio_socks::Error) -> bool {
    use tokio_socks::Error;
    matches!(
        error,
        Error::GeneralSocksServerFailure
            | Error::NetworkUnreachable
            | Error::HostUnreachable
            | Error::AddressTypeNotSupported
            | Error::TtlExpired
    )
}

/// SOCKS5 的目标地址。
///
/// 不能拼成 `host:port` 字符串交给 tokio-socks：它对字符串先按 `SocketAddr` 解析，
/// 失败再从最后一个冒号切开当域名，`2001:db8::1:22` 这样的 IPv6 就被当成域名
/// `2001:db8::1` 发出去，代理多半解析不了。这里自己判断类型。
fn socks5_target(host: &str, port: u16) -> Result<TargetAddr<'_>, String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(TargetAddr::Ip(SocketAddr::new(ip, port)));
    }
    if bare.is_empty() {
        return Err("目标主机为空".to_owned());
    }
    // 域名不会有冒号；到这里还有冒号，多半是带 zone id 的 IPv6（`fe80::1%eth0`），
    // SOCKS5 的地址字段放不下 zone，当域名发出去也只会让代理困惑，直接说清楚。
    if bare.contains(':') {
        return Err(format!(
            "目标主机 {host:?} 不是合法的 IPv6 地址，SOCKS5 也无法携带带 zone id 的地址"
        ));
    }
    Ok(TargetAddr::Domain(Cow::Borrowed(bare), port))
}

/// 把 tokio-socks 的错误换成能看懂的中文。
fn describe_socks_error(error: &tokio_socks::Error) -> String {
    use tokio_socks::Error;
    match error {
        Error::ProxyServerUnreachable => "连不上代理服务器".to_owned(),
        Error::PasswordAuthFailure(code) => {
            format!("用户名或密码不被代理接受（子协商返回 {code}）")
        }
        Error::NoAcceptableAuthMethods => "代理不接受我们提供的认证方式".to_owned(),
        Error::AuthorizationRequired => "代理要求认证".to_owned(),
        Error::ConnectionRefused => "代理拒绝了这个目标（连接被拒绝）".to_owned(),
        Error::HostUnreachable => "代理说目标主机不可达".to_owned(),
        Error::NetworkUnreachable => "代理说网络不可达".to_owned(),
        Error::ConnectionNotAllowedByRuleset => "代理的规则不允许连这个目标".to_owned(),
        Error::CommandNotSupported => "代理不支持 CONNECT".to_owned(),
        Error::AddressTypeNotSupported => "代理不支持这种地址类型".to_owned(),
        other => format!("{other}"),
    }
}

/// HTTP CONNECT：自己拼请求，只认响应头里的状态行。
async fn http_connect_handshake(
    mut socket: TcpStream,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<BoxedStream, ZsshError> {
    let target = http_connect_authority(host, port).map_err(|detail| ZsshError::Proxy {
        proxy: proxy.label(),
        detail,
    })?;
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");

    if let Some(username) = &proxy.username {
        let credentials = format!("{username}:{}", proxy.password.as_deref().unwrap_or(""));
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes());
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_str("\r\n");

    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|error| ZsshError::Proxy {
            proxy: proxy.label(),
            detail: format!("发送 CONNECT 请求失败：{error}"),
        })?;

    let (head, leftover) = read_http_head(&mut socket, proxy).await?;
    let status_line = head.lines().next().unwrap_or_default().trim().to_owned();

    if is_success_status(&status_line) {
        debug!(proxy = %proxy.label(), %target, leftover = leftover.len(), "HTTP 代理隧道已建立");
        // 代理常把 200 和目标服务器的第一批字节（SSH 横幅）一起送来，读头时已经
        // 从 socket 里取走了，必须先交还给上层，否则 russh 收不到完整横幅。
        if leftover.is_empty() {
            return Ok(Box::new(socket));
        }
        return Ok(Box::new(PrefixedStream::new(leftover, socket)));
    }

    Err(ZsshError::Proxy {
        proxy: proxy.label(),
        detail: if status_line.contains("407") {
            if proxy.username.is_some() {
                format!("代理要求认证（{status_line}）：用户名或密码不对")
            } else {
                format!("代理要求认证（{status_line}）：这个代理需要填用户名和密码")
            }
        } else if is_gateway_failure(&status_line) {
            // 502/503/504：代理自己连不上目标；目标是 IPv6 时多半是代理没有 IPv6 出口
            with_ipv6_hint(format!("代理返回 {status_line}"), host)
        } else {
            format!("代理返回 {status_line}")
        },
    })
}

/// CONNECT 行和 Host 头里的 `host:port`。
///
/// IPv6 字面量本身带冒号，不加方括号代理就分不清哪段是端口（RFC 9110 的
/// authority 写法要求 `[addr]:port`）。主机名原样拼进请求行，里面要是有换行或
/// 空白，就能截断请求行、注入额外的请求头，所以直接拒绝。
fn http_connect_authority(host: &str, port: u16) -> Result<String, String> {
    if host.is_empty() {
        return Err("目标主机为空".to_owned());
    }
    if host.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(format!(
            "目标主机 {host:?} 里有空白或控制字符，不能放进 CONNECT 请求"
        ));
    }

    let already_bracketed = host.starts_with('[') && host.ends_with(']');
    // 普通主机名和 IPv4 不会有冒号，有冒号的只能是 IPv6（可能带 zone id，
    // `Ipv6Addr` 解析不了 zone，所以按冒号判断而不是按能否解析）。
    if !already_bracketed && host.contains(':') {
        if host.contains('[') || host.contains(']') {
            return Err(format!("目标主机 {host:?} 的方括号不成对"));
        }
        return Ok(format!("[{host}]:{port}"));
    }
    Ok(format!("{host}:{port}"))
}

/// 先吐出一段已经读到的字节，再接着读底层流；写直接透传。
///
/// 用在 HTTP CONNECT 之后：读响应头时可能顺带读走了隧道里的数据。
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    position: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            position: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.position < this.prefix.len() {
            let remaining = &this.prefix[this.position..];
            let count = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..count]);
            this.position += count;
            if this.position == this.prefix.len() {
                // 前缀吐完就释放，长连接上不必一直占着
                this.prefix = Vec::new();
                this.position = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// 状态行是不是 2xx。
fn is_success_status(status_line: &str) -> bool {
    status_code(status_line).is_some_and(|code| (200..300).contains(&code))
}

/// 代理连不上上游（502 Bad Gateway / 503 / 504 Gateway Timeout）。
fn is_gateway_failure(status_line: &str) -> bool {
    matches!(status_code(status_line), Some(502..=504))
}

fn status_code(status_line: &str) -> Option<u16> {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
}

/// 一直读到空行（响应头结束），带上限。
///
/// 返回响应头文本和分隔符 `\r\n\r\n` 之后已经读到的字节：后者属于隧道里的数据，
/// 调用方要接回流上。
async fn read_http_head(
    socket: &mut TcpStream,
    proxy: &ProxyConfig,
) -> Result<(String, Vec<u8>), ZsshError> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 512];

    let head_end = loop {
        if let Some(index) = find_head_end(&buffer) {
            break Some(index);
        }
        if buffer.len() > MAX_HTTP_HEAD {
            return Err(ZsshError::Proxy {
                proxy: proxy.label(),
                detail: "代理回的响应头太长".to_owned(),
            });
        }
        let read = socket
            .read(&mut chunk)
            .await
            .map_err(|error| ZsshError::Proxy {
                proxy: proxy.label(),
                detail: format!("读代理响应失败：{error}"),
            })?;
        if read == 0 {
            break None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    if buffer.is_empty() {
        return Err(ZsshError::Proxy {
            proxy: proxy.label(),
            detail: "代理没有回任何东西就断开了".to_owned(),
        });
    }
    let leftover = match head_end {
        Some(end) => buffer.split_off(end),
        None => Vec::new(),
    };
    Ok((String::from_utf8_lossy(&buffer).into_owned(), leftover))
}

/// 响应头结束位置（含分隔符 `\r\n\r\n`）。
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// 本地起一个回显服务，当"目标主机"用。
    async fn spawn_echo() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("监听本地端口");
        let port = listener.local_addr().expect("本地地址").port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 16];
                while let Ok(read) = socket.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if socket.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            }
        });
        port
    }

    /// 模拟 SOCKS5 代理。记录收到的目标，用来断言"主机名原样交给代理"。
    struct MockSocks5 {
        port: u16,
        /// 域名类型的目标，写成 `host:port`
        seen_targets: Arc<Mutex<Vec<String>>>,
        /// 所有 CONNECT 请求的原始地址类型和地址字节
        seen_raw: Arc<Mutex<Vec<SeenSocksTarget>>>,
        auth_attempts: Arc<AtomicUsize>,
    }

    /// 假代理收到的一次 CONNECT：ATYP 和地址字段原样记下来。
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SeenSocksTarget {
        atyp: u8,
        address: Vec<u8>,
        port: u16,
    }

    async fn spawn_socks5(require_auth: Option<(&'static str, &'static str)>) -> MockSocks5 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let port = listener.local_addr().expect("地址").port();
        let seen_targets = Arc::new(Mutex::new(Vec::new()));
        let seen_raw = Arc::new(Mutex::new(Vec::new()));
        let auth_attempts = Arc::new(AtomicUsize::new(0));

        let targets = Arc::clone(&seen_targets);
        let raw = Arc::clone(&seen_raw);
        let attempts = Arc::clone(&auth_attempts);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut header = [0u8; 2];
                if socket.read_exact(&mut header).await.is_err() {
                    continue;
                }
                let mut methods = vec![0u8; header[1] as usize];
                if socket.read_exact(&mut methods).await.is_err() {
                    continue;
                }

                match require_auth {
                    None => {
                        // 选"不需要认证"
                        if socket.write_all(&[0x05, 0x00]).await.is_err() {
                            continue;
                        }
                    }
                    Some((user, password)) => {
                        if socket.write_all(&[0x05, 0x02]).await.is_err() {
                            continue;
                        }
                        let mut auth_header = [0u8; 2];
                        if socket.read_exact(&mut auth_header).await.is_err() {
                            continue;
                        }
                        let mut username = vec![0u8; auth_header[1] as usize];
                        if socket.read_exact(&mut username).await.is_err() {
                            continue;
                        }
                        let mut password_length = [0u8; 1];
                        if socket.read_exact(&mut password_length).await.is_err() {
                            continue;
                        }
                        let mut given = vec![0u8; password_length[0] as usize];
                        if socket.read_exact(&mut given).await.is_err() {
                            continue;
                        }

                        attempts.fetch_add(1, Ordering::Relaxed);
                        if username == user.as_bytes() && given == password.as_bytes() {
                            let _ = socket.write_all(&[0x01, 0x00]).await;
                        } else {
                            let _ = socket.write_all(&[0x01, 0x01]).await;
                            continue;
                        }
                    }
                }

                // CONNECT 请求：ver cmd rsv atyp addr port
                let mut request = [0u8; 4];
                if socket.read_exact(&mut request).await.is_err() {
                    continue;
                }
                let atyp = request[3];
                let address = match atyp {
                    0x01 | 0x04 => {
                        let mut address = vec![0u8; if atyp == 0x01 { 4 } else { 16 }];
                        if socket.read_exact(&mut address).await.is_err() {
                            continue;
                        }
                        address
                    }
                    0x03 => {
                        let mut length = [0u8; 1];
                        if socket.read_exact(&mut length).await.is_err() {
                            continue;
                        }
                        let mut host = vec![0u8; length[0] as usize];
                        if socket.read_exact(&mut host).await.is_err() {
                            continue;
                        }
                        host
                    }
                    _ => {
                        let _ = socket
                            .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                            .await;
                        continue;
                    }
                };
                let mut port_bytes = [0u8; 2];
                if socket.read_exact(&mut port_bytes).await.is_err() {
                    continue;
                }
                let port = u16::from_be_bytes(port_bytes);
                if atyp == 0x03 {
                    targets.lock().expect("锁").push(format!(
                        "{}:{}",
                        String::from_utf8_lossy(&address),
                        port
                    ));
                }
                raw.lock().expect("锁").push(SeenSocksTarget {
                    atyp,
                    address,
                    port,
                });

                // 成功：绑定地址随便填
                let _ = socket
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await;

                // 之后当目标用：回显
                let mut buffer = [0u8; 32];
                while let Ok(read) = socket.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if socket.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            }
        });

        MockSocks5 {
            port,
            seen_targets,
            seen_raw,
            auth_attempts,
        }
    }

    fn socks5_proxy(port: u16, user: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".to_owned(),
            port,
            username: user.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    fn http_proxy(port: u16, user: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            kind: ProxyKind::Http,
            host: "127.0.0.1".to_owned(),
            port,
            username: user.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    /// 取出错误：`BoxedStream` 没实现 Debug，不能直接用 `expect_err`。
    fn expect_error(result: Result<BoxedStream, ZsshError>) -> ZsshError {
        match result {
            Ok(_) => panic!("应该报错"),
            Err(error) => error,
        }
    }

    /// 从一条流上写一点再读回来，确认隧道能用。
    async fn echo_through(stream: &mut BoxedStream) {
        stream.write_all(b"ping").await.expect("写");
        let mut buffer = [0u8; 4];
        stream.read_exact(&mut buffer).await.expect("读");
        assert_eq!(&buffer, b"ping");
    }

    #[tokio::test]
    async fn direct_connect_works() {
        let port = spawn_echo().await;
        let mut stream = connect_direct("127.0.0.1", port).await.expect("应该连上");
        stream.write_all(b"ping").await.expect("写");
        let mut buffer = [0u8; 4];
        stream.read_exact(&mut buffer).await.expect("读");
        assert_eq!(&buffer, b"ping");
    }

    #[tokio::test]
    async fn tries_every_resolved_address_in_order() {
        // localhost 一般解析出 ::1 和 127.0.0.1；只监听 127.0.0.1，所以第一个地址
        // 会失败，应该接着试第二个
        let port = spawn_echo().await;
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host(("localhost", port))
            .await
            .expect("解析 localhost")
            .collect();
        if addresses.len() < 2 {
            println!("跳过：这台机器上 localhost 只解析出 {addresses:?}");
            return;
        }

        let stream = connect_direct("localhost", port)
            .await
            .expect("第一个地址不行就该试下一个");
        let peer = stream.peer_addr().expect("对端地址");
        assert!(
            addresses.iter().any(|address| address == &peer),
            "连上的地址应该是解析出来的其中之一"
        );
    }

    #[tokio::test]
    async fn reports_every_address_it_tried() {
        // 挑一个没人监听的端口
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("占位");
        let port = listener.local_addr().expect("地址").port();
        drop(listener);

        let error = connect_direct("127.0.0.1", port)
            .await
            .expect_err("没人监听应该报错");
        let message = error.to_string();
        assert!(
            message.contains("127.0.0.1"),
            "错误里要列出试过的地址：{message}"
        );
        assert!(
            matches!(error, ZsshError::Connect { .. }),
            "实际：{error:?}"
        );
    }

    #[tokio::test]
    async fn dns_failure_is_reported_separately() {
        let error = connect_direct("no-such-host.invalid", 22)
            .await
            .expect_err("解析不了应该报错");
        assert!(matches!(error, ZsshError::Dns { .. }), "实际：{error:?}");
    }

    #[tokio::test]
    async fn socks5_without_auth_passes_the_hostname_through() {
        let proxy = spawn_socks5(None).await;
        let mut stream = connect_via_proxy(
            &socks5_proxy(proxy.port, None, None),
            "target.example.com",
            2222,
        )
        .await
        .expect("应该经代理连上");
        echo_through(&mut stream).await;

        let targets = proxy.seen_targets.lock().expect("锁").clone();
        assert_eq!(
            targets,
            vec!["target.example.com:2222".to_owned()],
            "目标主机名要原样交给代理，不能本地解析成 IP"
        );
    }

    #[tokio::test]
    async fn socks5_with_username_password_authenticates() {
        let proxy = spawn_socks5(Some(("user", "pw"))).await;
        let mut stream = connect_via_proxy(
            &socks5_proxy(proxy.port, Some("user"), Some("pw")),
            "target.example.com",
            22,
        )
        .await
        .expect("带用户名密码应该能连上");
        echo_through(&mut stream).await;
        assert_eq!(proxy.auth_attempts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn socks5_auth_failure_is_reported() {
        let proxy = spawn_socks5(Some(("user", "pw"))).await;
        let error = expect_error(
            connect_via_proxy(
                &socks5_proxy(proxy.port, Some("user"), Some("wrong")),
                "target.example.com",
                22,
            )
            .await,
        );

        let message = error.to_string();
        assert!(
            message.contains("socks5 127.0.0.1"),
            "要说清是哪个代理：{message}"
        );
        assert!(
            message.contains("用户名或密码"),
            "要说清是认证失败：{message}"
        );
    }

    #[tokio::test]
    async fn socks5_requiring_auth_without_credentials_is_rejected() {
        let proxy = spawn_socks5(Some(("user", "pw"))).await;
        let error = expect_error(
            connect_via_proxy(&socks5_proxy(proxy.port, None, None), "target", 22).await,
        );
        let message = error.to_string();
        assert!(message.contains("认证"), "错误里要说清认证的事：{message}");
        assert_eq!(
            proxy.auth_attempts.load(Ordering::Relaxed),
            0,
            "没配用户名密码时不会发认证子协商"
        );
    }

    #[tokio::test]
    async fn socks5_sends_ipv6_literals_as_ipv6_addresses() {
        let proxy = spawn_socks5(None).await;
        for host in ["2001:db8::1", "[2001:db8::1]"] {
            let mut stream = connect_via_proxy(&socks5_proxy(proxy.port, None, None), host, 2222)
                .await
                .expect("IPv6 目标应该能经 SOCKS5 连上");
            echo_through(&mut stream).await;
        }

        let expected_address = "2001:db8::1"
            .parse::<std::net::Ipv6Addr>()
            .expect("解析")
            .octets()
            .to_vec();
        let seen = proxy.seen_raw.lock().expect("锁").clone();
        assert_eq!(seen.len(), 2);
        for request in seen {
            assert_eq!(
                request,
                SeenSocksTarget {
                    atyp: 0x04,
                    address: expected_address.clone(),
                    port: 2222,
                },
                "IPv6 字面量要用 ATYP=IPv6 和 16 字节地址，不能当域名"
            );
        }
        assert!(
            proxy.seen_targets.lock().expect("锁").is_empty(),
            "不能有域名类型的请求"
        );
    }

    #[tokio::test]
    async fn socks5_sends_domains_as_domains_and_ipv4_as_ipv4() {
        let proxy = spawn_socks5(None).await;
        for host in ["target.example.com", "10.0.0.5"] {
            let mut stream = connect_via_proxy(&socks5_proxy(proxy.port, None, None), host, 22)
                .await
                .expect("应该经代理连上");
            echo_through(&mut stream).await;
        }

        let seen = proxy.seen_raw.lock().expect("锁").clone();
        assert_eq!(
            seen,
            vec![
                SeenSocksTarget {
                    atyp: 0x03,
                    address: b"target.example.com".to_vec(),
                    port: 22,
                },
                SeenSocksTarget {
                    atyp: 0x01,
                    address: vec![10, 0, 0, 5],
                    port: 22,
                },
            ]
        );
    }

    #[test]
    fn socks5_target_rejects_what_socks5_cannot_carry() {
        for bad in ["", "[]", "fe80::1%eth0", "not:an:address"] {
            assert!(socks5_target(bad, 22).is_err(), "{bad:?} 应该被拒绝");
        }
    }

    /// 模拟 HTTP CONNECT 代理。
    async fn spawn_http(require_basic: Option<(&'static str, &'static str)>, status: u16) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let port = listener.local_addr().expect("地址").port();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 256];
                while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    }
                }
                let request = String::from_utf8_lossy(&buffer).into_owned();
                let authorized = match require_basic {
                    None => true,
                    Some((user, password)) => {
                        let expected = base64::engine::general_purpose::STANDARD
                            .encode(format!("{user}:{password}").as_bytes());
                        request.contains(&format!("Proxy-Authorization: Basic {expected}"))
                    }
                };

                let code = if authorized { status } else { 407 };
                let reason = match code {
                    200 => "Connection established",
                    403 => "Forbidden",
                    407 => "Proxy Authentication Required",
                    _ => "Error",
                };
                let response = format!("HTTP/1.1 {code} {reason}\r\n\r\n");
                let _ = socket.write_all(response.as_bytes()).await;

                if code == 200 {
                    let mut buffer = [0u8; 32];
                    while let Ok(read) = socket.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        if socket.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn http_connect_succeeds() {
        let port = spawn_http(None, 200).await;
        let mut stream = connect_via_proxy(&http_proxy(port, None, None), "target.example.com", 22)
            .await
            .expect("200 应该能连上");
        echo_through(&mut stream).await;
    }

    #[tokio::test]
    async fn http_connect_with_basic_auth() {
        let port = spawn_http(Some(("user", "pw")), 200).await;
        let mut stream = connect_via_proxy(
            &http_proxy(port, Some("user"), Some("pw")),
            "target.example.com",
            22,
        )
        .await
        .expect("带 Basic 认证应该能连上");
        echo_through(&mut stream).await;
    }

    #[tokio::test]
    async fn http_connect_407_without_credentials() {
        let port = spawn_http(Some(("user", "pw")), 200).await;
        let error = expect_error(
            connect_via_proxy(&http_proxy(port, None, None), "target.example.com", 22).await,
        );
        let message = error.to_string();
        assert!(message.contains("407"), "错误里要带状态行：{message}");
        assert!(message.contains("认证"), "要说清是认证问题：{message}");
    }

    #[tokio::test]
    async fn http_connect_407_with_wrong_credentials() {
        let port = spawn_http(Some(("user", "pw")), 200).await;
        let error = expect_error(
            connect_via_proxy(
                &http_proxy(port, Some("user"), Some("wrong")),
                "target.example.com",
                22,
            )
            .await,
        );
        let message = error.to_string();
        assert!(message.contains("407"), "错误里要带状态行：{message}");
        assert!(message.contains("不对"), "要说清是密码不对：{message}");
    }

    #[tokio::test]
    async fn http_connect_non_2xx_keeps_the_status_line() {
        let port = spawn_http(None, 403).await;
        let error = expect_error(
            connect_via_proxy(&http_proxy(port, None, None), "target.example.com", 22).await,
        );
        let message = error.to_string();
        assert!(
            message.contains("403 Forbidden"),
            "状态行要原样带上：{message}"
        );
    }

    /// 模拟一个"200 之后立刻送来目标数据"的 HTTP 代理：响应头和 `trailing` 用同一次
    /// write 发出去，之后回显。记录收到的完整请求头。
    async fn spawn_http_with_trailing(trailing: &'static [u8]) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let port = listener.local_addr().expect("地址").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 256];
                while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    }
                }
                if !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    continue;
                }
                seen.lock()
                    .expect("锁")
                    .push(String::from_utf8_lossy(&buffer).into_owned());

                let mut response = b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec();
                response.extend_from_slice(trailing);
                if socket.write_all(&response).await.is_err() {
                    continue;
                }

                let mut buffer = [0u8; 32];
                while let Ok(read) = socket.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if socket.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            }
        });
        (port, requests)
    }

    /// 读一行（含 `\n`），SSH 横幅就是这样一行。
    async fn read_line(stream: &mut BoxedStream) -> Vec<u8> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while line.last() != Some(&b'\n') {
            stream.read_exact(&mut byte).await.expect("读横幅");
            line.push(byte[0]);
        }
        line
    }

    #[tokio::test]
    async fn http_connect_keeps_the_banner_sent_with_the_200() {
        const BANNER: &[u8] = b"SSH-2.0-MockServer_1.0\r\n";
        let (port, _) = spawn_http_with_trailing(BANNER).await;
        let mut stream = connect_via_proxy(&http_proxy(port, None, None), "target.example.com", 22)
            .await
            .expect("200 应该能连上");

        // 横幅被吃掉时这里会一直等下去，加超时让回归直接失败而不是挂住
        let banner = timeout(Duration::from_secs(5), read_line(&mut stream))
            .await
            .expect("5 秒内没读到横幅：和 200 一起到的字节被丢了");
        assert_eq!(
            banner, BANNER,
            "和 200 一起到的 SSH 横幅不能被读响应头时吃掉"
        );
        // 前缀吐完之后要接着读底层流
        echo_through(&mut stream).await;
    }

    #[tokio::test]
    async fn prefixed_stream_yields_the_prefix_then_the_inner_stream() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut stream = PrefixedStream::new(b"abcde".to_vec(), client);
        server.write_all(b"fgh").await.expect("写");

        // 故意用比前缀小的缓冲区，确认前缀能分几次吐完、不丢不重
        let mut collected = Vec::new();
        let mut buffer = [0u8; 2];
        while collected.len() < 8 {
            let read = stream.read(&mut buffer).await.expect("读");
            assert!(read > 0, "不该提前结束");
            collected.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(collected, b"abcdefgh");

        stream.write_all(b"xy").await.expect("写透传");
        let mut echoed = [0u8; 2];
        server.read_exact(&mut echoed).await.expect("对端读");
        assert_eq!(&echoed, b"xy");
    }

    #[test]
    fn find_head_end_points_past_the_separator() {
        assert_eq!(find_head_end(b"HTTP/1.1 200 OK\r\n\r\nSSH"), Some(19));
        assert_eq!(find_head_end(b"HTTP/1.1 200 OK\r\n"), None);
    }

    #[tokio::test]
    async fn http_connect_brackets_ipv6_targets() {
        let (port, requests) = spawn_http_with_trailing(b"").await;
        let mut stream = connect_via_proxy(&http_proxy(port, None, None), "2001:db8::1", 2222)
            .await
            .expect("IPv6 目标应该能经代理连上");
        echo_through(&mut stream).await;

        let requests = requests.lock().expect("锁").clone();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].starts_with("CONNECT [2001:db8::1]:2222 HTTP/1.1\r\n"),
            "请求行里的 IPv6 要加方括号：{:?}",
            requests[0]
        );
        assert!(
            requests[0].contains("\r\nHost: [2001:db8::1]:2222\r\n"),
            "Host 头里的 IPv6 也要加方括号：{:?}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn http_connect_rejects_header_injection_in_the_target() {
        let (port, requests) = spawn_http_with_trailing(b"").await;
        let error = expect_error(
            connect_via_proxy(
                &http_proxy(port, None, None),
                "target.example.com\r\nX-Injected: 1",
                22,
            )
            .await,
        );
        assert!(matches!(error, ZsshError::Proxy { .. }), "实际：{error:?}");
        assert!(
            error.to_string().contains("控制字符"),
            "要说清为什么拒绝：{error}"
        );
        assert!(
            requests.lock().expect("锁").is_empty(),
            "带换行的目标不能发给代理"
        );
    }

    #[test]
    fn http_connect_authority_formats_and_validates_hosts() {
        assert_eq!(
            http_connect_authority("target.example.com", 22).as_deref(),
            Ok("target.example.com:22")
        );
        assert_eq!(
            http_connect_authority("10.0.0.5", 2222).as_deref(),
            Ok("10.0.0.5:2222")
        );
        assert_eq!(http_connect_authority("::1", 22).as_deref(), Ok("[::1]:22"));
        assert_eq!(
            http_connect_authority("fe80::1%eth0", 22).as_deref(),
            Ok("[fe80::1%eth0]:22"),
            "带 zone id 的 IPv6 也按冒号识别"
        );
        assert_eq!(
            http_connect_authority("[2001:db8::1]", 22).as_deref(),
            Ok("[2001:db8::1]:22"),
            "已经带方括号的不要再包一层"
        );

        for bad in [
            "",
            "a b",
            "a\tb",
            "host\r\nX: 1",
            "host\n",
            "host\0",
            "[::1",
            "::1]",
        ] {
            assert!(
                http_connect_authority(bad, 22).is_err(),
                "{bad:?} 应该被拒绝"
            );
        }
    }

    #[test]
    fn success_status_parsing() {
        assert!(is_success_status("HTTP/1.1 200 Connection established"));
        assert!(is_success_status("HTTP/1.0 201 Created"));
        assert!(!is_success_status(
            "HTTP/1.1 407 Proxy Authentication Required"
        ));
        assert!(!is_success_status("HTTP/1.1 403 Forbidden"));
        assert!(!is_success_status("不是状态行"));
    }

    #[test]
    fn gateway_failure_is_502_to_504() {
        assert!(is_gateway_failure("HTTP/1.1 502 Bad Gateway"));
        assert!(is_gateway_failure("HTTP/1.1 503 Service Unavailable"));
        assert!(is_gateway_failure("HTTP/1.1 504 Gateway Timeout"));
        assert!(!is_gateway_failure("HTTP/1.1 403 Forbidden"));
        assert!(!is_gateway_failure(
            "HTTP/1.1 407 Proxy Authentication Required"
        ));
    }

    #[test]
    fn ipv6_literal_detection() {
        for host in [
            "2001:db8::1",
            "[2001:db8::1]",
            "::1",
            "fe80::1%3",
            "[fe80::1%eth0]",
        ] {
            assert!(is_ipv6_literal(host), "{host} 是 IPv6");
        }
        for host in ["10.0.0.1", "example.com", "", "[example.com]"] {
            assert!(!is_ipv6_literal(host), "{host} 不是 IPv6");
        }
    }

    #[test]
    fn ipv6_hint_only_for_ipv6_targets() {
        let hinted = with_ipv6_hint("代理说目标主机不可达".to_owned(), "2001:db8::1");
        assert!(hinted.starts_with("代理说目标主机不可达。"));
        assert!(hinted.contains(IPV6_EGRESS_HINT));
        assert_eq!(
            with_ipv6_hint("代理说目标主机不可达".to_owned(), "10.0.0.1"),
            "代理说目标主机不可达"
        );
        assert_eq!(
            with_ipv6_hint("代理说目标主机不可达".to_owned(), "example.com"),
            "代理说目标主机不可达"
        );
    }

    #[test]
    fn only_unreachable_like_socks_errors_get_the_ipv6_hint() {
        use tokio_socks::Error;
        assert!(socks_error_may_be_missing_ipv6(&Error::HostUnreachable));
        assert!(socks_error_may_be_missing_ipv6(&Error::NetworkUnreachable));
        assert!(socks_error_may_be_missing_ipv6(
            &Error::GeneralSocksServerFailure
        ));
        assert!(socks_error_may_be_missing_ipv6(
            &Error::AddressTypeNotSupported
        ));
        // 认证失败、连接被拒（已经到了目标机器）和 IPv6 出口无关
        assert!(!socks_error_may_be_missing_ipv6(
            &Error::PasswordAuthFailure(1)
        ));
        assert!(!socks_error_may_be_missing_ipv6(&Error::ConnectionRefused));
        assert!(!socks_error_may_be_missing_ipv6(
            &Error::ProxyServerUnreachable
        ));
    }
}
