//! 同步用的 HTTP 客户端：走 Z-SSH 的默认代理设置（直连 / HTTP / SOCKS5），证书用
//! 系统证书库（Windows 上 reqwest 的 `default-tls` 就是 schannel），不提供“忽略
//! 证书错误”的开关。

use std::time::Duration;

use super::storage::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Socks5,
    Http,
}

/// 一条代理（来自默认代理设置，凭据已经用 DPAPI 解开）。
#[derive(Clone, PartialEq, Eq)]
pub struct SyncProxy {
    pub kind: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl std::fmt::Debug for SyncProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印密码。
        f.debug_struct("SyncProxy")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .finish()
    }
}

impl SyncProxy {
    fn url(&self) -> String {
        // socks5h：主机名交给代理解析（本机 DNS 往往解析不了，或被污染）。
        let scheme = match self.kind {
            ProxyScheme::Socks5 => "socks5h",
            ProxyScheme::Http => "http",
        };
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{scheme}://{host}:{}", self.port)
    }
}

impl SyncProxy {
    /// 带账号密码的代理地址（给只收 URL 的 HTTP 客户端用，例如自动更新插件）。
    /// 账号密码做百分号编码；**含密码，不能打印进日志**。
    pub fn url_with_credentials(&self) -> String {
        let base = self.url();
        let Some(username) = &self.username else {
            return base;
        };
        let encode = |text: &str| -> String {
            text.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect()
        };
        let userinfo = format!(
            "{}:{}@",
            encode(username),
            encode(self.password.as_deref().unwrap_or(""))
        );
        match base.split_once("://") {
            Some((scheme, rest)) => format!("{scheme}://{userinfo}{rest}"),
            None => base,
        }
    }
}

/// 单次请求的超时（含读完响应体）。同步包很小，30 秒足够；更长只会让界面看起来卡死。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub fn build_client(proxy: Option<&SyncProxy>) -> Result<reqwest::Client, StorageError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("Z-SSH/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        // 不跟随 https → http 的重定向（会把密码明文送出去）。
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("重定向次数太多")
            } else if attempt.url().scheme() == "http"
                && attempt
                    .previous()
                    .last()
                    .is_some_and(|url| url.scheme() == "https")
            {
                attempt.error("拒绝从 https 重定向到 http")
            } else {
                attempt.follow()
            }
        }));
    match proxy {
        Some(proxy) => {
            let mut reqwest_proxy = reqwest::Proxy::all(proxy.url())
                .map_err(|err| StorageError::Io(format!("代理设置不合法：{err}")))?;
            if let Some(username) = &proxy.username {
                reqwest_proxy =
                    reqwest_proxy.basic_auth(username, proxy.password.as_deref().unwrap_or(""));
            }
            builder = builder.proxy(reqwest_proxy);
        }
        // 没配代理 = 直连，不去读系统 / 环境变量里的代理（用户在 Z-SSH 里选了“不用代理”）。
        None => builder = builder.no_proxy(),
    }
    builder
        .build()
        .map_err(|err| StorageError::Io(format!("HTTP 客户端创建失败：{err}")))
}

/// 407：HTTP 代理要认证（访问 `http://` 地址时代理的响应会原样回到这里；访问 `https://`
/// 地址时 CONNECT 被拒会在建立连接时就失败，走 [`map_http_error`]）。
pub fn proxy_auth_required(status: reqwest::StatusCode) -> Option<StorageError> {
    (status == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED).then(|| {
        StorageError::Io(
            "代理要求认证，或者代理的用户名 / 密码不对（在「连接 → 默认代理」里检查）".to_owned(),
        )
    })
}

/// 429 / 408 / 5xx（501 除外，那是“不支持”）：稍后重试有意义。
pub fn retryable_status(status: reqwest::StatusCode) -> Option<StorageError> {
    let code = status.as_u16();
    let temporary = matches!(code, 408 | 429 | 500 | 502 | 503 | 504);
    temporary.then(|| StorageError::Unavailable(format!("服务器返回 {code}")))
}

/// 同步包（含 Gist 接口里内嵌同步包的 JSON）最多读这么多。真实的同步包只有几十 KB
/// 到几 MB；超过这个数只可能是存储里放了别的东西或服务异常。请求超时限制不了体积：
/// 本机或局域网的存储 30 秒内能灌进几个 GB，所以必须边读边数。
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
/// 错误响应体、不含同步包的小 JSON（用户信息、新建 Gist 的回执）最多读这么多。
pub const MAX_META_BYTES: u64 = 1024 * 1024;

fn size_text(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} 字节")
    }
}

fn too_large(what: &str, limit: u64) -> StorageError {
    StorageError::Io(format!(
        "{what}超过 {} 上限，已停止读取（存储里的对象可能不是 Z-SSH 的同步包，或者服务返回了异常内容）",
        size_text(limit)
    ))
}

/// 边读边数地读完响应体，超过 `limit` 立即报错、不再继续读。
///
/// 先看 `Content-Length`：声明就超限的不用读第一个字节；没声明（chunked）或声明
/// 不实的，靠累计字节数兜底。
pub async fn read_body_limited(
    mut response: reqwest::Response,
    limit: u64,
    what: &str,
) -> Result<Vec<u8>, StorageError> {
    let declared = response.content_length();
    if declared.is_some_and(|len| len > limit) {
        return Err(too_large(what, limit));
    }
    // 声明的长度已经不超过上限，可以放心预留；没声明就从零开始长。
    let mut body = Vec::with_capacity(declared.unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await.map_err(map_http_error)? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(too_large(what, limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 读错误响应体给错误提示用：最多读 `limit` 字节，多出的直接丢掉、不报错——
/// 错误提示只需要开头的错误码 / message，没必要因为响应体太长把原本的状态码错误
/// 换成“太大”。读失败就当没有响应体。
pub async fn read_error_text(mut response: reqwest::Response, limit: u64) -> String {
    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut body: Vec<u8> = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = cap.saturating_sub(body.len());
        if chunk.len() >= room {
            body.extend_from_slice(&chunk[..room]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// 把 reqwest 的错误翻成存储错误。错误文本里去掉 URL（里面可能有路径前缀）。
pub fn map_http_error(err: reqwest::Error) -> StorageError {
    let err = err.without_url();
    if err.is_timeout() {
        return StorageError::Timeout;
    }
    if err.is_connect() {
        return StorageError::Io(format!("无法连接到存储服务（网络不通或代理不可用）：{err}"));
    }
    StorageError::Io(format!("请求失败：{err}"))
}

#[cfg(test)]
pub(crate) mod test_http;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_url_with_credentials_is_percent_encoded() {
        let proxy = SyncProxy {
            kind: ProxyScheme::Socks5,
            host: "proxy.example.internal".to_owned(),
            port: 1080,
            username: Some("me@work".to_owned()),
            password: Some("p:w/d".to_owned()),
        };
        assert_eq!(
            proxy.url_with_credentials(),
            "socks5h://me%40work:p%3Aw%2Fd@proxy.example.internal:1080"
        );
        let plain = SyncProxy {
            kind: ProxyScheme::Http,
            username: None,
            password: None,
            ..proxy
        };
        assert_eq!(
            plain.url_with_credentials(),
            "http://proxy.example.internal:1080"
        );
    }

    use super::test_http::{spawn, Body, Canned};

    async fn get(addr: std::net::SocketAddr) -> reqwest::Response {
        build_client(None)
            .unwrap()
            .get(format!("http://{addr}/obj"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_within_the_limit_is_read_whole() {
        let addr = spawn(|_| Canned::new(200, Body::Exact(b"hello".to_vec()))).await;
        let body = read_body_limited(get(addr).await, 5, "同步包")
            .await
            .unwrap();
        assert_eq!(body, b"hello");
        let addr = spawn(|_| Canned::new(200, Body::Chunked { total: 1000 })).await;
        let body = read_body_limited(get(addr).await, 1000, "同步包")
            .await
            .unwrap();
        assert_eq!(body.len(), 1000);
    }

    #[tokio::test]
    async fn a_declared_length_over_the_limit_is_refused_before_reading() {
        let addr = spawn(|_| {
            Canned::new(
                200,
                Body::Declared {
                    declared: 10 * 1024 * 1024 * 1024,
                    sent: Vec::new(),
                },
            )
        })
        .await;
        let err = read_body_limited(get(addr).await, MAX_PACKAGE_BYTES, "同步包")
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("同步包超过 64 MiB 上限"), "{text}");
    }

    #[tokio::test]
    async fn an_undeclared_body_is_cut_off_once_it_passes_the_limit() {
        // 没有 Content-Length，只能边读边数。
        let addr = spawn(|_| {
            Canned::new(
                200,
                Body::Chunked {
                    total: 4 * 1024 * 1024,
                },
            )
        })
        .await;
        let err = read_body_limited(get(addr).await, 256 * 1024, "同步包")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("262144 字节 上限"), "{err}");
    }

    #[tokio::test]
    async fn error_text_is_truncated_not_failed() {
        let addr = spawn(|_| {
            Canned::new(
                403,
                Body::Chunked {
                    total: 3 * 1024 * 1024,
                },
            )
        })
        .await;
        let text = read_error_text(get(addr).await, 1000).await;
        assert_eq!(text.len(), 1000);
        let addr = spawn(|_| Canned::new(403, Body::Exact(b"<Code>X</Code>".to_vec()))).await;
        assert_eq!(
            read_error_text(get(addr).await, MAX_META_BYTES).await,
            "<Code>X</Code>"
        );
    }
}
