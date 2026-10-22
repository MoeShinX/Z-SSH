//! 进程内的 WebDAV / S3 服务和 SOCKS5 代理，给同步的集成测试用。
//!
//! 不用 Docker、不碰任何真实云账号：监听 `127.0.0.1` 的随机端口，手写最小的
//! HTTP/1.1。服务端的行为可以调：WebDAV 返回不返回 ETag、认不认条件请求头；
//! S3 认不认条件写入（或者像某些兼容服务那样直接回 501）。
//! S3 这边**独立校验 SigV4 签名**（用文档向量验证过的实现重算，签名不对回 403）。

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use zssh_core::sync::remote::content_hash;
use zssh_core::sync::sigv4::{sha256_hex, sign, Credentials, SigningInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    /// 返回 ETag，并且认 `If-Match` / `If-None-Match`（Nextcloud 一类）。
    Full,
    /// 不返回任何 ETag，也不认条件头（有些简易 WebDAV）。
    NoEtag,
    /// 返回 ETag，但把条件头当没看见（认证过但不执行）。
    IgnoreConditions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3Conditional {
    Full,
    Ignored,
    /// 带条件头的 PUT 回 501 NotImplemented。
    NotImplemented,
}

#[derive(Debug, Clone)]
pub enum Kind {
    WebDav {
        user: String,
        password: String,
        behavior: Behavior,
        /// PUT 的响应里带不带 ETag（有的服务器不带，要再读一次）。
        put_returns_etag: bool,
    },
    S3 {
        access_key: String,
        secret_key: String,
        region: String,
        bucket: String,
        path_style: bool,
        conditional: S3Conditional,
    },
}

#[derive(Debug, Clone)]
pub struct Logged {
    pub method: String,
    pub path: String,
    pub host: String,
    pub authorization: Option<String>,
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    pub status: u16,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
    log: Vec<Logged>,
    external_before_put: Option<Vec<u8>>,
    fail_next: VecDeque<u16>,
}

pub struct MockCloud {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for MockCloud {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }
    fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

fn etag_of(bytes: &[u8]) -> String {
    format!("\"{}\"", &content_hash(bytes)[..32])
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.ok()?;
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

async fn write_response(stream: &mut BufReader<TcpStream>, response: Response) {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", response.body.len()));
    let socket = stream.get_mut();
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&response.body).await;
    let _ = socket.flush().await;
}

impl MockCloud {
    pub async fn start(kind: Kind) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let addr = listener.local_addr().expect("地址");
        let mut initial = State::default();
        // 地址指向的集合在真实服务上本来就存在（用户的网盘根目录）。
        initial.dirs.insert("/dav".to_owned());
        let state = Arc::new(Mutex::new(initial));
        let task_state = state.clone();
        let kind = Arc::new(kind);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let state = task_state.clone();
                let kind = kind.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(socket);
                    while let Some(request) = read_request(&mut reader).await {
                        let response = handle(&kind, &state, request);
                        write_response(&mut reader, response).await;
                    }
                });
            }
        });
        Self { addr, state, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Logged> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }

    /// 所有对象的路径。
    pub fn object_paths(&self) -> Vec<String> {
        self.state.lock().unwrap().objects.keys().cloned().collect()
    }

    /// 同步包对象的内容（路径以 `zssh-sync.json` 结尾的那个）。
    pub fn sync_object(&self) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state
            .objects
            .iter()
            .find(|(path, _)| path.ends_with("zssh-sync.json"))
            .map(|(_, bytes)| bytes.clone())
    }

    pub fn set_sync_object(&self, bytes: Vec<u8>) {
        let mut state = self.state.lock().unwrap();
        let path = state
            .objects
            .keys()
            .find(|path| path.ends_with("zssh-sync.json"))
            .cloned()
            .expect("还没有同步包");
        state.objects.insert(path, bytes);
    }

    pub fn dirs(&self) -> Vec<String> {
        self.state.lock().unwrap().dirs.iter().cloned().collect()
    }

    /// 下一次 PUT 到达时，先让“另一台电脑”把对象改成 `bytes`，再处理这次 PUT
    /// （带 If-Match 的会因此被拒绝）。
    pub fn inject_external_write_before_next_put(&self, bytes: Vec<u8>) {
        self.state.lock().unwrap().external_before_put = Some(bytes);
    }

    /// 接下来的 `count` 个请求直接回这个状态码（模拟 503 之类）。
    pub fn fail_next(&self, status: u16, count: usize) {
        let mut state = self.state.lock().unwrap();
        for _ in 0..count {
            state.fail_next.push_back(status);
        }
    }
}

fn handle(kind: &Kind, state: &Mutex<State>, request: Request) -> Response {
    let (raw_path, query) = match request.target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query.to_owned()),
        None => (request.target.clone(), String::new()),
    };
    let host = request.headers.get("host").cloned().unwrap_or_default();
    let mut guard = state.lock().unwrap();
    let response = if let Some(status) = guard.fail_next.pop_front() {
        Response::new(status)
    } else {
        match kind {
            Kind::WebDav {
                user,
                password,
                behavior,
                put_returns_etag,
            } => handle_webdav(
                &mut guard,
                &request,
                &raw_path,
                user,
                password,
                *behavior,
                *put_returns_etag,
            ),
            Kind::S3 { .. } => handle_s3(&mut guard, kind, &request, &raw_path, &query, &host),
        }
    };
    guard.log.push(Logged {
        method: request.method.clone(),
        path: percent_decode(&raw_path),
        host,
        authorization: request.headers.get("authorization").cloned(),
        if_match: request.headers.get("if-match").cloned(),
        if_none_match: request.headers.get("if-none-match").cloned(),
        status: response.status,
    });
    response
}

fn basic_ok(request: &Request, user: &str, password: &str) -> bool {
    use base64::Engine as _;
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    );
    request.headers.get("authorization") == Some(&expected)
}

fn parent_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(index) => trimmed[..index].to_owned(),
    }
}

fn handle_webdav(
    state: &mut State,
    request: &Request,
    raw_path: &str,
    user: &str,
    password: &str,
    behavior: Behavior,
    put_returns_etag: bool,
) -> Response {
    if !basic_ok(request, user, password) {
        return Response::new(401).with_header("WWW-Authenticate", "Basic realm=\"mock\"");
    }
    let path = percent_decode(raw_path);
    let with_etag = behavior != Behavior::NoEtag;
    let honors = behavior == Behavior::Full;
    let if_match = request.headers.get("if-match");
    let if_none_match = request.headers.get("if-none-match");
    match request.method.as_str() {
        "GET" => match state.objects.get(&path) {
            Some(bytes) => {
                let mut response = Response::new(200).with_body(bytes.clone());
                if with_etag {
                    response = response.with_header("ETag", etag_of(bytes));
                }
                response
            }
            None => Response::new(404),
        },
        "MKCOL" => {
            let dir = path.trim_end_matches('/').to_owned();
            if state.dirs.contains(&dir) {
                return Response::new(405);
            }
            let parent = parent_of(&dir);
            if parent != "/" && !state.dirs.contains(&parent) {
                return Response::new(409);
            }
            state.dirs.insert(dir);
            Response::new(201)
        }
        "PUT" => {
            if let Some(bytes) = state.external_before_put.take() {
                state.objects.insert(path.clone(), bytes);
            }
            let parent = parent_of(&path);
            if parent != "/" && !state.dirs.contains(&parent) {
                return Response::new(409);
            }
            if honors {
                let current = state.objects.get(&path).map(|b| etag_of(b));
                if if_none_match.is_some_and(|v| v == "*") && current.is_some() {
                    return Response::new(412);
                }
                if let Some(expected) = if_match {
                    if current.as_deref() != Some(expected.as_str()) {
                        return Response::new(412);
                    }
                }
            }
            let existed = state.objects.contains_key(&path);
            state.objects.insert(path.clone(), request.body.clone());
            let mut response = Response::new(if existed { 204 } else { 201 });
            if with_etag && put_returns_etag {
                response = response.with_header("ETag", etag_of(&request.body));
            }
            response
        }
        "DELETE" => {
            if honors {
                if let Some(expected) = if_match {
                    let current = state.objects.get(&path).map(|b| etag_of(b));
                    if current.as_deref() != Some(expected.as_str()) {
                        return Response::new(412);
                    }
                }
            }
            match state.objects.remove(&path) {
                Some(_) => Response::new(204),
                None => Response::new(404),
            }
        }
        _ => Response::new(405),
    }
}

fn s3_error(status: u16, code: &str) -> Response {
    Response::new(status).with_body(
        format!(
            "<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>"
        )
        .into_bytes(),
    )
}

fn handle_s3(
    state: &mut State,
    kind: &Kind,
    request: &Request,
    raw_path: &str,
    query: &str,
    host: &str,
) -> Response {
    let Kind::S3 {
        access_key,
        secret_key,
        region,
        bucket,
        path_style,
        conditional,
    } = kind
    else {
        return Response::new(500);
    };

    // --- 校验 SigV4 签名（独立重算）---
    let Some(authorization) = request.headers.get("authorization") else {
        return s3_error(403, "AccessDenied");
    };
    let Some(rest) = authorization.strip_prefix("AWS4-HMAC-SHA256 ") else {
        return s3_error(400, "AuthorizationHeaderMalformed");
    };
    let mut credential = "";
    let mut signed = "";
    let mut signature = "";
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Credential=") {
            credential = v;
        } else if let Some(v) = part.strip_prefix("SignedHeaders=") {
            signed = v;
        } else if let Some(v) = part.strip_prefix("Signature=") {
            signature = v;
        }
    }
    if !credential.starts_with(&format!("{access_key}/")) {
        return s3_error(403, "InvalidAccessKeyId");
    }
    let amz_date = request
        .headers
        .get("x-amz-date")
        .cloned()
        .unwrap_or_default();
    let payload_hash = request
        .headers
        .get("x-amz-content-sha256")
        .cloned()
        .unwrap_or_default();
    if payload_hash != sha256_hex(&request.body) {
        return s3_error(400, "XAmzContentSHA256Mismatch");
    }
    let signed_headers: Vec<(String, String)> = signed
        .split(';')
        .map(|name| {
            (
                name.to_owned(),
                request.headers.get(name).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let expected = sign(
        &Credentials {
            access_key: access_key.clone(),
            secret_key: secret_key.clone(),
        },
        &SigningInput {
            method: &request.method,
            canonical_uri: raw_path,
            canonical_query: query,
            headers: &signed_headers,
            payload_hash: &payload_hash,
            region,
            service: "s3",
            amz_date: &amz_date,
        },
    );
    if expected.signature != signature {
        return s3_error(403, "SignatureDoesNotMatch");
    }

    // --- 桶和对象键 ---
    let decoded = percent_decode(raw_path);
    let hostname = host.split(':').next().unwrap_or_default();
    let key = if *path_style {
        match decoded.strip_prefix(&format!("/{bucket}/")) {
            Some(key) => key.to_owned(),
            None => return s3_error(404, "NoSuchBucket"),
        }
    } else {
        if !hostname.starts_with(&format!("{bucket}.")) {
            return s3_error(404, "NoSuchBucket");
        }
        decoded.trim_start_matches('/').to_owned()
    };

    let if_match = request.headers.get("if-match");
    let if_none_match = request.headers.get("if-none-match");
    let conditional_header = if_match.is_some() || if_none_match.is_some();
    match request.method.as_str() {
        "GET" => match state.objects.get(&key) {
            Some(bytes) => Response::new(200)
                .with_header("ETag", etag_of(bytes))
                .with_body(bytes.clone()),
            None => s3_error(404, "NoSuchKey"),
        },
        "PUT" => {
            if let Some(bytes) = state.external_before_put.take() {
                state.objects.insert(key.clone(), bytes);
            }
            if conditional_header && *conditional == S3Conditional::NotImplemented {
                return s3_error(501, "NotImplemented");
            }
            if *conditional == S3Conditional::Full {
                let current = state.objects.get(&key).map(|b| etag_of(b));
                if if_none_match.is_some_and(|v| v == "*") && current.is_some() {
                    return s3_error(412, "PreconditionFailed");
                }
                if let Some(expected) = if_match {
                    if current.as_deref() != Some(expected.as_str()) {
                        return s3_error(412, "PreconditionFailed");
                    }
                }
            }
            state.objects.insert(key, request.body.clone());
            Response::new(200).with_header("ETag", etag_of(&request.body))
        }
        "DELETE" => {
            if *conditional == S3Conditional::Full {
                if let Some(expected) = if_match {
                    let current = state.objects.get(&key).map(|b| etag_of(b));
                    if current.as_deref() != Some(expected.as_str()) {
                        return s3_error(412, "PreconditionFailed");
                    }
                }
            }
            state.objects.remove(&key);
            Response::new(204)
        }
        _ => s3_error(405, "MethodNotAllowed"),
    }
}

// ---------------------------------------------------------------------------
// SOCKS5 代理

pub struct MockSocks5 {
    addr: SocketAddr,
    targets: Arc<Mutex<Vec<(String, u16)>>>,
    task: JoinHandle<()>,
}

impl Drop for MockSocks5 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockSocks5 {
    /// 不管客户端要连哪里，都转给 `upstream`（模拟代理能解析任意主机名）。
    /// `auth` 给了就要求用户名密码认证。
    pub async fn start(upstream: SocketAddr, auth: Option<(String, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let addr = listener.local_addr().expect("地址");
        let targets = Arc::new(Mutex::new(Vec::new()));
        let task_targets = targets.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let targets = task_targets.clone();
                let auth = auth.clone();
                tokio::spawn(async move {
                    let _ = serve_socks(socket, upstream, auth, targets).await;
                });
            }
        });
        Self {
            addr,
            targets,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 客户端请求连接的 (主机, 端口)，按顺序。
    pub fn targets(&self) -> Vec<(String, u16)> {
        self.targets.lock().unwrap().clone()
    }
}

async fn serve_socks(
    mut client: TcpStream,
    upstream: SocketAddr,
    auth: Option<(String, String)>,
    targets: Arc<Mutex<Vec<(String, u16)>>>,
) -> std::io::Result<()> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods).await?;
    let wanted = if auth.is_some() { 0x02 } else { 0x00 };
    if !methods.contains(&wanted) {
        client.write_all(&[5, 0xff]).await?;
        return Ok(());
    }
    client.write_all(&[5, wanted]).await?;
    if let Some((user, password)) = &auth {
        let mut version = [0u8; 2];
        client.read_exact(&mut version).await?;
        let mut name = vec![0u8; version[1] as usize];
        client.read_exact(&mut name).await?;
        let mut len = [0u8; 1];
        client.read_exact(&mut len).await?;
        let mut pass = vec![0u8; len[0] as usize];
        client.read_exact(&mut pass).await?;
        let ok = name == user.as_bytes() && pass == password.as_bytes();
        client.write_all(&[1, if ok { 0 } else { 1 }]).await?;
        if !ok {
            return Ok(());
        }
    }
    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            client.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut ip = [0u8; 16];
            client.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    targets
        .lock()
        .unwrap()
        .push((host, u16::from_be_bytes(port)));
    let mut upstream = TcpStream::connect(upstream).await?;
    client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP 代理（转发 + CONNECT）

/// 代理收到的一条请求。
#[derive(Debug, Clone)]
pub struct ProxyLogged {
    /// `GET` / `PUT` / `CONNECT` ……
    pub method: String,
    /// 客户端想访问的 `主机:端口`（转发请求取自绝对地址，CONNECT 取自请求行）。
    pub target: String,
    /// 带的 `Proxy-Authorization` 对不对（没要求认证时恒为 true）。
    pub authorized: bool,
}

pub struct MockHttpProxy {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<ProxyLogged>>>,
    task: JoinHandle<()>,
}

impl Drop for MockHttpProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockHttpProxy {
    /// 不管客户端要访问哪里，都转给 `upstream`（模拟代理能解析任意主机名）。
    /// `auth` 给了就要求 `Proxy-Authorization: Basic`，不对回 407。
    ///
    /// 两种用法和真实的 HTTP 代理一样：访问 `http://` 地址时客户端发绝对地址形式的请求
    /// （`GET http://主机/路径`），代理转发；访问 `https://` 地址时客户端先发 `CONNECT 主机:443`，
    /// 代理回 200 之后只搬运字节。
    pub async fn start(upstream: SocketAddr, auth: Option<(String, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let addr = listener.local_addr().expect("地址");
        let log = Arc::new(Mutex::new(Vec::new()));
        let task_log = log.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let log = task_log.clone();
                let auth = auth.clone();
                tokio::spawn(async move {
                    let _ = serve_http_proxy(socket, upstream, auth, log).await;
                });
            }
        });
        Self { addr, log, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn requests(&self) -> Vec<ProxyLogged> {
        self.log.lock().unwrap().clone()
    }

    /// 通过认证的请求要访问的 `主机:端口`（去重，按首次出现的顺序）。
    pub fn authorized_targets(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for item in self.requests() {
            if item.authorized && !out.contains(&item.target) {
                out.push(item.target);
            }
        }
        out
    }
}

async fn serve_http_proxy(
    socket: TcpStream,
    upstream: SocketAddr,
    auth: Option<(String, String)>,
    log: Arc<Mutex<Vec<ProxyLogged>>>,
) -> std::io::Result<()> {
    use base64::Engine as _;
    let expected = auth.as_ref().map(|(user, password)| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    });
    let mut reader = BufReader::new(socket);
    while let Some(request) = read_request(&mut reader).await {
        let authorized = match &expected {
            Some(expected) => request.headers.get("proxy-authorization") == Some(expected),
            None => true,
        };
        let is_connect = request.method == "CONNECT";
        // 转发请求的目标在绝对地址里：http://主机[:端口]/路径
        let (target, path) = if is_connect {
            (request.target.clone(), String::new())
        } else {
            let rest = request
                .target
                .strip_prefix("http://")
                .unwrap_or(&request.target);
            match rest.split_once('/') {
                Some((authority, path)) => {
                    let host = if authority.contains(':') {
                        authority.to_owned()
                    } else {
                        format!("{authority}:80")
                    };
                    (host, format!("/{path}"))
                }
                None => (format!("{rest}:80"), "/".to_owned()),
            }
        };
        log.lock().unwrap().push(ProxyLogged {
            method: request.method.clone(),
            target,
            authorized,
        });
        if !authorized {
            let socket = reader.get_mut();
            socket
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                      Proxy-Authenticate: Basic realm=\"mock\"\r\n\
                      Content-Length: 0\r\n\r\n",
                )
                .await?;
            continue;
        }
        if is_connect {
            let mut upstream_socket = TcpStream::connect(upstream).await?;
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            tokio::io::copy_bidirectional(reader.get_mut(), &mut upstream_socket).await?;
            return Ok(());
        }

        // 转发：改成原样的请求行（路径），去掉代理专用的头，读完响应原样送回。
        let upstream_socket = TcpStream::connect(upstream).await?;
        let mut upstream_reader = BufReader::new(upstream_socket);
        let mut head = format!("{} {} HTTP/1.1\r\n", request.method, path);
        for (name, value) in &request.headers {
            if name == "proxy-authorization" || name == "proxy-connection" {
                continue;
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        upstream_reader.get_mut().write_all(head.as_bytes()).await?;
        upstream_reader.get_mut().write_all(&request.body).await?;

        let mut response_head = String::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if upstream_reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let end = line == "\r\n";
            response_head.push_str(&line);
            if end {
                break;
            }
        }
        let mut body = vec![0u8; length];
        upstream_reader.read_exact(&mut body).await?;
        let client = reader.get_mut();
        client.write_all(response_head.as_bytes()).await?;
        client.write_all(&body).await?;
        client.flush().await?;
    }
    Ok(())
}
