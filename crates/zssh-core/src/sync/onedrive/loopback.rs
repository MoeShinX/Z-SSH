//! 本机回环回调：浏览器授权完成后跳回 `http://localhost:<随机端口>/`，这里接住并取出授权码。
//!
//! 安全上的几条（本机上任何网页、任何进程都能向这个端口发请求，所以逐条设防）：
//! - 只监听回环地址；同时监听 `127.0.0.1` 和 `[::1]` 的同一个端口（浏览器解析
//!   `localhost` 可能走 IPv6），某一个绑不上就只用另一个，并在日志里说明；
//! - **只接受一次回调**：第一个合格的回调被认领之后，后面的请求一律 404，监听随后关闭；
//! - 只处理 `GET /`，别的方法、别的路径（比如浏览器顺手要的 `/favicon.ico`）一律 404；
//! - `state` 不对的请求回 400 并继续等：别人往这个端口乱发请求不能让真正的登录失败；
//! - `Host` 头必须是 `localhost` / `127.0.0.1` / `[::1]`（防 DNS 重绑定）；
//! - 请求头最多读 16 KiB、最多等 10 秒，一个半开的连接占不住其他连接；
//! - 成功页是纯静态中文，不加载任何外部资源，也不回显请求里的任何内容；
//! - 超时（默认 5 分钟）或用户取消就关闭监听。

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinSet;
use tracing::{info, warn};

use super::login::LoginError;
use super::pkce::constant_time_eq;

/// 回调请求头最多读这么多字节。
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// 一个连接读请求头最多等多久。
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 浏览器带回来的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// 授权码。
    Code(String),
    /// 微软返回了错误（用户拒绝授权等）。
    Error { error: String, description: String },
}

/// 已经绑好端口、还没开始等的回环监听。
pub struct Loopback {
    listeners: Vec<TcpListener>,
    port: u16,
    /// 绑定过程的说明（哪个地址没绑上），写日志用。
    notes: Vec<String>,
}

impl Loopback {
    /// 先绑 `127.0.0.1:0` 拿到随机端口，再在 `[::1]` 上绑同一个端口。
    /// IPv4 绑不上就让 `[::1]` 自己挑端口、再试着在 IPv4 上绑同一个。两个都不行才报错。
    pub async fn bind() -> Result<Self, LoginError> {
        let mut notes = Vec::new();
        let mut listeners = Vec::new();
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await;
        let (port, first_is_v4) = match &v4 {
            Ok(listener) => (listener.local_addr().map(|a| a.port()).unwrap_or(0), true),
            Err(err) => {
                notes.push(format!("127.0.0.1 绑定失败：{err}"));
                (0, false)
            }
        };
        if let Ok(listener) = v4 {
            listeners.push(listener);
        }
        let v6_port = if first_is_v4 { port } else { 0 };
        match TcpListener::bind((Ipv6Addr::LOCALHOST, v6_port)).await {
            Ok(listener) => {
                let v6_actual = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                if !first_is_v4 {
                    // IPv4 没绑上：端口由 [::1] 决定，再试着把 IPv4 绑到同一个端口。
                    match TcpListener::bind((Ipv4Addr::LOCALHOST, v6_actual)).await {
                        Ok(v4_listener) => listeners.push(v4_listener),
                        Err(err) => notes.push(format!("127.0.0.1:{v6_actual} 绑定失败：{err}")),
                    }
                }
                listeners.push(listener);
            }
            Err(err) => notes.push(format!("[::1]:{v6_port} 绑定失败：{err}")),
        }
        let Some(first) = listeners.first() else {
            return Err(LoginError::Listen(notes.join("；")));
        };
        let port = first
            .local_addr()
            .map(|a| a.port())
            .map_err(|err| LoginError::Listen(err.to_string()))?;
        for note in &notes {
            warn!(port, "OneDrive 登录回调监听：{note}，只用能绑上的地址");
        }
        Ok(Self {
            listeners,
            port,
            notes,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 授权请求里的 `redirect_uri`：注册的是 `http://localhost`，运行时带上端口
    /// （微软对 localhost 回调地址匹配时忽略端口）。
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// 实际在监听的地址（测试和日志用）。
    pub fn addresses(&self) -> Vec<SocketAddr> {
        self.listeners
            .iter()
            .filter_map(|l| l.local_addr().ok())
            .collect()
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// 等回调：成功、被拒绝、超时、取消，四选一。返回时监听已经关闭。
    pub async fn wait(
        self,
        expected_state: &str,
        timeout: Duration,
        cancel: &Notify,
    ) -> Result<CallbackOutcome, LoginError> {
        let (tx, mut rx) = mpsc::channel::<CallbackOutcome>(1);
        let claimed = Arc::new(AtomicBool::new(false));
        let state: Arc<str> = Arc::from(expected_state);
        let mut tasks = JoinSet::new();
        for listener in self.listeners {
            tasks.spawn(accept_loop(
                listener,
                state.clone(),
                self.port,
                tx.clone(),
                claimed.clone(),
            ));
        }
        drop(tx);
        let result = tokio::select! {
            outcome = rx.recv() => outcome.ok_or_else(|| {
                LoginError::Listen("回调监听意外停止".to_owned())
            }),
            _ = tokio::time::sleep(timeout) => Err(LoginError::Timeout),
            _ = cancel.notified() => Err(LoginError::Cancelled),
        };
        // JoinSet 被丢弃时会中止里面所有的任务（包括还没处理完的连接），监听随之关闭。
        drop(tasks);
        info!("OneDrive 登录回调监听已关闭");
        result
    }
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<str>,
    port: u16,
    tx: mpsc::Sender<CallbackOutcome>,
    claimed: Arc<AtomicBool>,
) {
    // 每个连接各用一个任务；accept_loop 被中止时 JoinSet 一起中止它们。
    let mut connections = JoinSet::new();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (state, tx, claimed) = (state.clone(), tx.clone(), claimed.clone());
        connections.spawn(async move {
            handle_connection(stream, &state, port, tx, claimed).await;
        });
        // 顺手回收已经结束的连接任务，别让集合无限增长。
        while connections.try_join_next().is_some() {}
    }
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > MAX_HEAD_BYTES {
            return None;
        }
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let host = lines.take_while(|line| !line.is_empty()).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("host")
            .then(|| value.trim().to_owned())
    });
    Some(Request {
        method,
        target,
        host,
    })
}

/// `Host` 头只认回环名字（带不带端口都行；带端口时必须是我们的端口）。
fn host_is_loopback(host: &str, port: u16) -> bool {
    let (name, given_port) = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((name, tail)) => (format!("[{name}]"), tail.strip_prefix(':')),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, p)) => (name.to_owned(), Some(p)),
            None => (host.to_owned(), None),
        }
    };
    if let Some(p) = given_port {
        if p.parse::<u16>().ok() != Some(port) {
            return false;
        }
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

const PAGE_STYLE: &str = "body{font-family:'Segoe UI','Microsoft YaHei',sans-serif;display:flex;\
align-items:center;justify-content:center;height:100vh;margin:0;background:#f5f5f5;color:#222}\
@media(prefers-color-scheme:dark){body{background:#1e1e1e;color:#e6e6e6}}\
main{text-align:center;padding:24px}h1{font-size:20px;font-weight:600;margin:0 0 8px}\
p{margin:0;font-size:14px;opacity:.8}";

fn page(title: &str, hint: &str) -> String {
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>Z-SSH</title><style>{PAGE_STYLE}</style></head>\
<body><main><h1>{title}</h1><p>{hint}</p></main></body></html>"
    )
}

pub fn success_page() -> String {
    page("授权成功", "可以关闭这个页面，回到 Z-SSH 了。")
}

fn failure_page() -> String {
    page(
        "授权没有完成",
        "请回到 Z-SSH 查看原因，可以关闭这个页面后重新登录。",
    )
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
X-Content-Type-Options: nosniff\r\n\
Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
Connection: close\r\n\r\n",
        body.len()
    );
    // 浏览器可能已经走了，写失败不是错误。
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

async fn handle_connection(
    mut stream: TcpStream,
    expected_state: &str,
    port: u16,
    tx: mpsc::Sender<CallbackOutcome>,
    claimed: Arc<AtomicBool>,
) {
    let Ok(Some(request)) = tokio::time::timeout(READ_TIMEOUT, read_request(&mut stream)).await
    else {
        return;
    };
    let host_ok = request
        .host
        .as_deref()
        .is_some_and(|host| host_is_loopback(host, port));
    if !host_ok {
        respond(&mut stream, 400, &failure_page()).await;
        return;
    }
    let Ok(url) = Url::parse(&format!("http://localhost{}", request.target)) else {
        respond(&mut stream, 404, &failure_page()).await;
        return;
    };
    if request.method != "GET" || url.path() != "/" {
        respond(&mut stream, 404, &failure_page()).await;
        return;
    }
    if claimed.load(Ordering::SeqCst) {
        // 已经有一个回调被认领了：只接受一次。
        respond(&mut stream, 404, &failure_page()).await;
        return;
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    let mut description = String::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" => state = Some(value.into_owned()),
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = value.into_owned(),
            _ => {}
        }
    }
    if !state
        .as_deref()
        .is_some_and(|given| constant_time_eq(given, expected_state))
    {
        // state 不对：可能是别的网页在乱发请求。不消耗这次登录，继续等真正的回调。
        warn!("OneDrive 登录回调的 state 不匹配，已忽略这次请求");
        respond(&mut stream, 400, &failure_page()).await;
        return;
    }
    let outcome = match (error, code) {
        (Some(error), _) => CallbackOutcome::Error { error, description },
        (None, Some(code)) if !code.is_empty() && code.len() <= 4096 => CallbackOutcome::Code(code),
        _ => {
            respond(&mut stream, 400, &failure_page()).await;
            return;
        }
    };
    if claimed.swap(true, Ordering::SeqCst) {
        // 两个合格的回调几乎同时到：只有先认领的那个算数。
        respond(&mut stream, 404, &failure_page()).await;
        return;
    }
    let body = match outcome {
        CallbackOutcome::Code(_) => success_page(),
        CallbackOutcome::Error { .. } => failure_page(),
    };
    // 先把页面完整写给浏览器，再把结果交出去（交出去之后监听会被关掉）。
    respond(&mut stream, 200, &body).await;
    let _ = tx.send(outcome).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send_raw(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.expect("连接");
        // 服务端可能读到一半就断开（超长请求头的用例），写失败不算测试失败。
        let _ = stream.write_all(request.as_bytes()).await;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    fn get(path: &str, port: u16) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")
    }

    fn status(response: &str) -> u16 {
        response
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    async fn start(
        state: &'static str,
        timeout: Duration,
    ) -> (
        Vec<SocketAddr>,
        u16,
        Arc<Notify>,
        tokio::task::JoinHandle<Result<CallbackOutcome, LoginError>>,
    ) {
        let loopback = Loopback::bind().await.expect("绑定");
        let addrs = loopback.addresses();
        let port = loopback.port();
        let cancel = Arc::new(Notify::new());
        let waiter_cancel = cancel.clone();
        let handle =
            tokio::spawn(async move { loopback.wait(state, timeout, &waiter_cancel).await });
        (addrs, port, cancel, handle)
    }

    #[tokio::test]
    async fn listens_on_both_loopback_addresses_with_the_same_port() {
        let loopback = Loopback::bind().await.expect("绑定");
        let addrs = loopback.addresses();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{addrs:?}");
        assert!(
            addrs.iter().all(|a| a.port() == loopback.port()),
            "{addrs:?}"
        );
        assert_eq!(
            loopback.redirect_uri(),
            format!("http://localhost:{}", loopback.port())
        );
        // 这台机器支持 IPv6 回环时两个都在；不支持时只有 IPv4，并且日志里有说明。
        if addrs.len() == 1 {
            assert!(!loopback.notes().is_empty(), "只绑上一个要有说明");
            println!(
                "[loopback] 只绑上了 {addrs:?}，说明：{:?}",
                loopback.notes()
            );
        } else {
            assert!(addrs.iter().any(|a| a.is_ipv4()) && addrs.iter().any(|a| a.is_ipv6()));
        }
    }

    #[tokio::test]
    async fn a_valid_callback_returns_the_code_and_shows_the_success_page() {
        let (addrs, port, _cancel, handle) = start("state-1", Duration::from_secs(30)).await;
        let response = send_raw(addrs[0], &get("/?code=abc123&state=state-1", port)).await;
        assert_eq!(status(&response), 200, "{response}");
        assert!(response.contains("授权成功"), "{response}");
        assert!(response.contains("可以关闭这个页面"), "{response}");
        assert!(
            !response.contains("abc123") && !response.contains("state-1"),
            "成功页不回显请求里的内容"
        );
        assert!(
            response.contains("default-src 'none'"),
            "页面带 CSP：{response}"
        );
        assert!(
            !response.contains("http://") && !response.contains("https://"),
            "页面不引用任何外部资源"
        );
        let outcome = handle.await.expect("任务").expect("结果");
        assert_eq!(outcome, CallbackOutcome::Code("abc123".to_owned()));
    }

    #[tokio::test]
    async fn the_callback_works_on_every_bound_address() {
        let (addrs, port, _cancel, handle) = start("s", Duration::from_secs(30)).await;
        // 用最后一个地址（有 IPv6 时就是 [::1]）。
        let last = *addrs.last().expect("地址");
        let response = send_raw(last, &get("/?code=c&state=s", port)).await;
        assert_eq!(status(&response), 200, "{last}：{response}");
        assert_eq!(
            handle.await.unwrap().unwrap(),
            CallbackOutcome::Code("c".to_owned())
        );
    }

    #[tokio::test]
    async fn only_the_callback_path_answers_everything_else_is_404() {
        let (addrs, port, cancel, handle) = start("s", Duration::from_secs(30)).await;
        for path in [
            "/favicon.ico",
            "/callback?code=x&state=s",
            "/x/?code=x&state=s",
        ] {
            let response = send_raw(addrs[0], &get(path, port)).await;
            assert_eq!(status(&response), 404, "{path}：{response}");
        }
        let post = format!(
            "POST /?code=x&state=s HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 0\r\n\r\n"
        );
        assert_eq!(status(&send_raw(addrs[0], &post).await), 404, "只接受 GET");
        // 这些请求都没有消耗掉这次登录。
        assert!(!handle.is_finished());
        cancel.notify_one();
        assert!(matches!(handle.await.unwrap(), Err(LoginError::Cancelled)));
    }

    #[tokio::test]
    async fn a_wrong_or_missing_state_is_refused_without_ending_the_login() {
        let (addrs, port, _cancel, handle) = start("right", Duration::from_secs(30)).await;
        for path in [
            "/?code=x&state=wrong",
            "/?code=x",
            "/?state=right",
            "/?state=right&code=",
        ] {
            let response = send_raw(addrs[0], &get(path, port)).await;
            assert_eq!(status(&response), 400, "{path}：{response}");
        }
        assert!(!handle.is_finished(), "别人乱发的请求不能让登录失败");
        let response = send_raw(addrs[0], &get("/?code=good&state=right", port)).await;
        assert_eq!(status(&response), 200);
        assert_eq!(
            handle.await.unwrap().unwrap(),
            CallbackOutcome::Code("good".to_owned())
        );
    }

    #[tokio::test]
    async fn only_the_first_callback_is_accepted() {
        let (addrs, port, _cancel, handle) = start("s", Duration::from_secs(30)).await;
        let first = send_raw(addrs[0], &get("/?code=first&state=s", port)).await;
        assert_eq!(status(&first), 200);
        // 结果拿到了，监听已经关闭：后面的连接要么被拒绝，要么拿到 404，绝不会再算一次回调。
        let outcome = handle.await.unwrap().unwrap();
        assert_eq!(outcome, CallbackOutcome::Code("first".to_owned()));
        for addr in &addrs {
            match TcpStream::connect(addr).await {
                Err(_) => {}
                Ok(mut stream) => {
                    let _ = stream
                        .write_all(get("/?code=second&state=s", port).as_bytes())
                        .await;
                    let mut out = Vec::new();
                    let _ =
                        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut out))
                            .await;
                    let text = String::from_utf8_lossy(&out);
                    assert!(
                        text.is_empty() || status(&text) == 404,
                        "关闭后不能再有 200：{text}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn two_simultaneous_callbacks_yield_exactly_one_result() {
        let (addrs, port, _cancel, handle) = start("s", Duration::from_secs(30)).await;
        let addr = addrs[0];
        let a = tokio::spawn(async move { send_raw(addr, &get("/?code=a&state=s", port)).await });
        let b = tokio::spawn(async move { send_raw(addr, &get("/?code=b&state=s", port)).await });
        let (a, b) = (a.await.unwrap(), b.await.unwrap());
        let ok = [status(&a), status(&b)]
            .iter()
            .filter(|s| **s == 200)
            .count();
        assert_eq!(ok, 1, "只有一个回调被认领：{a} / {b}");
        let code = match handle.await.unwrap().unwrap() {
            CallbackOutcome::Code(code) => code,
            other => panic!("{other:?}"),
        };
        assert!(code == "a" || code == "b");
    }

    #[tokio::test]
    async fn provider_errors_are_reported_with_the_state_checked() {
        let (addrs, port, _cancel, handle) = start("s", Duration::from_secs(30)).await;
        let response = send_raw(
            addrs[0],
            &get(
                "/?error=access_denied&error_description=The+user+has+denied+access&state=s",
                port,
            ),
        )
        .await;
        assert_eq!(status(&response), 200);
        assert!(response.contains("授权没有完成"), "{response}");
        assert!(
            !response.contains("denied"),
            "页面不回显错误内容：{response}"
        );
        assert_eq!(
            handle.await.unwrap().unwrap(),
            CallbackOutcome::Error {
                error: "access_denied".to_owned(),
                description: "The user has denied access".to_owned(),
            }
        );
        // state 不对的错误回调不算数。
        let (addrs, port, cancel, handle) = start("s", Duration::from_secs(30)).await;
        let response = send_raw(addrs[0], &get("/?error=access_denied&state=other", port)).await;
        assert_eq!(status(&response), 400);
        assert!(!handle.is_finished());
        cancel.notify_one();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn requests_with_a_foreign_host_header_are_refused() {
        let (addrs, _port, cancel, handle) = start("s", Duration::from_secs(30)).await;
        for host in [
            "evil.example",
            "evil.example:80",
            "localhost.evil.example",
            "localhost:1",
        ] {
            let request = format!(
                "GET /?code=x&state=s HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
            );
            assert_eq!(status(&send_raw(addrs[0], &request).await), 400, "{host}");
        }
        let no_host = "GET /?code=x&state=s HTTP/1.1\r\n\r\n";
        assert_eq!(status(&send_raw(addrs[0], no_host).await), 400);
        assert!(!handle.is_finished());
        cancel.notify_one();
        let _ = handle.await;
        // 回环名字的几种写法都认。
        for host in ["localhost", "127.0.0.1", "[::1]", "LOCALHOST:5"] {
            let expected_port = host.rsplit_once(':').and_then(|(_, p)| p.parse().ok());
            assert!(host_is_loopback(host, expected_port.unwrap_or(5)), "{host}");
        }
    }

    #[tokio::test]
    async fn times_out_and_closes_the_listener() {
        let (addrs, _port, _cancel, handle) = start("s", Duration::from_millis(200)).await;
        let result = handle.await.unwrap();
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        for addr in addrs {
            assert!(
                TcpStream::connect(addr).await.is_err(),
                "超时之后监听要关闭：{addr}"
            );
        }
    }

    #[tokio::test]
    async fn cancelling_closes_the_listener() {
        let (addrs, _port, cancel, handle) = start("s", Duration::from_secs(30)).await;
        cancel.notify_one();
        let result = handle.await.unwrap();
        assert!(matches!(result, Err(LoginError::Cancelled)), "{result:?}");
        for addr in addrs {
            assert!(
                TcpStream::connect(addr).await.is_err(),
                "取消之后监听要关闭"
            );
        }
    }

    #[tokio::test]
    async fn a_half_open_connection_does_not_block_the_real_callback() {
        let (addrs, port, _cancel, handle) = start("s", Duration::from_secs(30)).await;
        // 浏览器的预连接：连上之后什么也不发。
        let _idle = TcpStream::connect(addrs[0]).await.expect("连接");
        let response = send_raw(addrs[0], &get("/?code=ok&state=s", port)).await;
        assert_eq!(status(&response), 200);
        assert_eq!(
            handle.await.unwrap().unwrap(),
            CallbackOutcome::Code("ok".to_owned())
        );
    }

    #[tokio::test]
    async fn an_oversized_request_head_is_dropped() {
        let (addrs, port, cancel, handle) = start("s", Duration::from_secs(30)).await;
        let junk = "a".repeat(40 * 1024);
        let request =
            format!("GET /?code=x&state=s HTTP/1.1\r\nHost: localhost:{port}\r\nX: {junk}");
        let response = send_raw(addrs[0], &request).await;
        assert!(
            response.is_empty(),
            "超长请求头直接断开：{}",
            response.len()
        );
        assert!(!handle.is_finished());
        cancel.notify_one();
        let _ = handle.await;
    }
}
