//! 进程内模拟的微软授权服务器和 Graph（OneDrive）接口，给 S-4 的集成测试用。
//!
//! 不碰真实的微软服务：监听 `127.0.0.1` 的随机端口，手写最小的 HTTP/1.1。模拟了登录和令牌：
//!
//! - `GET  /consumers/oauth2/v2.0/authorize`：浏览器里的授权页。校验 `client_id`、`redirect_uri`
//!   （必须是 `http://localhost[:端口]`）、`response_type=code`、`code_challenge_method=S256`、`scope`，
//!   记下 `code_challenge`，按 [`AuthorizeMode`] 302 回 `redirect_uri`（带授权码或错误）
//! - `POST /consumers/oauth2/v2.0/token`：`authorization_code`（**验证 PKCE**：`SHA256(verifier)` 必须等于
//!   授权时记下的 challenge、授权码只能用一次、`redirect_uri` 必须一致、不能带 `client_secret`）和
//!   `refresh_token`（**每次换发新的刷新令牌**，严格模式下旧的立刻作废；访问令牌寿命可调，方便测过期）
//! - `GET  /v1.0/me`：账号名
//!
//! 以及应用专用文件夹里的文件（按官方文档的行为）：
//!
//! - `GET  /v1.0/me/drive/special/approot:/<名字>`：元数据（带 `eTag`、`size`）
//! - `GET  /v1.0/me/drive/special/approot:/<名字>:/content`：302 到另一个主机名（`localhost`）上的下载地址，
//!   下载请求如果带了 `Authorization` 会被记下来（真实服务的下载地址是预授权的，令牌不该发过去）
//! - `POST …:/createUploadSession`：支持 `If-Match`（对不上 412）、`conflictBehavior`（默认 `fail`，
//!   已存在 409 `nameAlreadyExists`）；返回的 `uploadUrl` 不接受 `Authorization`（带了 401，和文档一致）
//! - `PUT /upload/<会话>`：按 `Content-Range` 顺序接收分片，非最后一片必须是 320 KiB 的整数倍，
//!   最后一片提交文件（`fail` 模式下此刻已存在则 409）
//! - `DELETE …/approot:/<名字>`：支持 `If-Match`
//! - 可注入：限流（429 / 503 + `Retry-After`）、空间不足（507）、“另一台电脑在我下载完之后写了”、
//!   “服务器不执行 If-Match”（用来测探测和降级）

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use zssh_core::sync::onedrive::oauth::Endpoints;

pub const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
pub const ACCOUNT_NAME: &str = "Test User";
/// 上传分片的对齐单位（官方要求 320 KiB 的整数倍）。
const ALIGN: usize = 320 * 1024;

/// 授权页收到合格请求后怎么回。
#[derive(Debug, Clone)]
pub enum AuthorizeMode {
    /// 用户点了同意：302 带授权码。
    Approve,
    /// 用户点了拒绝：302 带 `error=access_denied`。
    Deny,
    /// 其他错误（比如 `invalid_request`），带描述。
    Error(&'static str, &'static str),
}

/// 令牌接口收到的一次请求（日志）。
#[derive(Debug, Clone)]
pub struct TokenRequest {
    pub grant_type: String,
    pub had_client_secret: bool,
    pub client_id: String,
    pub scope: String,
    pub status: u16,
}

/// 授权页收到的一次请求（日志）。
#[derive(Debug, Clone)]
pub struct AuthorizeRequest {
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Logged {
    pub method: String,
    pub path: String,
    pub status: u16,
    /// 请求带了 `Authorization` 头。
    pub has_auth: bool,
    pub if_match: Option<String>,
    pub content_range: Option<String>,
    pub host: String,
}

struct CodeInfo {
    challenge: String,
    redirect_uri: String,
}

struct MockFile {
    id: String,
    bytes: Vec<u8>,
    version: u64,
}

impl MockFile {
    /// 和真实 Graph 一样：`"{GUID},版本"`，引号是值的一部分。
    fn etag(&self) -> String {
        format!("\"{{{}}},{}\"", self.id, self.version)
    }
}

struct Session {
    name: String,
    /// 文件总长：第一次上传分片时从 Content-Range 取（创建会话时不会告诉服务器）。
    total: Option<usize>,
    received: Vec<u8>,
    fail_if_exists: bool,
}

struct State {
    client_id: String,
    account_name: String,
    mode: AuthorizeMode,
    counter: u64,
    codes: HashMap<String, CodeInfo>,
    /// 还有效的刷新令牌。
    refresh_tokens: HashSet<String>,
    /// 访问令牌 → 过期时刻（Unix 秒）。
    access_tokens: HashMap<String, i64>,
    access_lifetime_secs: i64,
    /// 换发新刷新令牌时旧的是否立刻作废（真实服务不作废，严格模式用来证明我们保存了新的）。
    strict_rotation: bool,
    /// 下一次 refresh_token 请求回这个错误（`invalid_grant` 之类）。
    refresh_error: Option<(u16, &'static str, &'static str)>,
    /// 下一次 authorization_code 请求回这个错误。
    code_error: Option<(u16, &'static str, &'static str)>,
    /// 令牌接口接下来几次直接回 503（模拟服务不可用）。
    token_unavailable: usize,
    authorize_log: Vec<AuthorizeRequest>,
    token_log: Vec<TokenRequest>,
    log: Vec<Logged>,

    // -- OneDrive 应用文件夹 --
    files: BTreeMap<String, MockFile>,
    sessions: HashMap<String, Session>,
    /// 接下来的 Graph 请求先回这些限流 / 错误（状态码，Retry-After 秒数）。
    graph_failures: VecDeque<(u16, Option<u64>)>,
    /// 接下来的上传分片请求回这些错误。
    upload_failures: VecDeque<u16>,
    quota_full: bool,
    /// 服务器假装不认 `If-Match`（忽略它）。
    ignore_if_match: bool,
    /// 下载完成之后“另一台电脑”写入的内容（名字，内容）。
    external_after_download: Option<(String, Vec<u8>)>,
    /// 创建上传会话之后、提交之前“另一台电脑”写入的内容。
    external_after_session: Option<(String, Vec<u8>)>,
    /// 下载地址用和接口同一个主机名（默认换成 localhost，模拟下载服务器是另一个主机）。
    download_same_host: bool,
}

pub struct MockMicrosoft {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for MockMicrosoft {
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

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    fn oauth_error(status: u16, error: &str, description: &str) -> Self {
        Self::json(
            status,
            &json!({
                "error": error,
                "error_description": format!("{description}\r\nTrace ID: 00000000-0000-0000-0000-000000000000"),
            }),
        )
    }

    fn graph_error(status: u16, code: &str) -> Self {
        Self::json(
            status,
            &json!({"error": {"code": code, "message": "mock", "innerError": {"request-id": "0"}}}),
        )
    }

    fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".to_owned(), location.to_owned())],
            body: Vec::new(),
        }
    }

    fn empty(status: u16) -> Self {
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
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
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

async fn write_reply(stream: &mut BufReader<TcpStream>, reply: Reply) {
    let reason = match reply.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        416 => "Requested Range Not Satisfiable",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason);
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", reply.body.len()));
    let socket = stream.get_mut();
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&reply.body).await;
    let _ = socket.flush().await;
}

fn query_of(target: &str) -> BTreeMap<String, String> {
    reqwest::Url::parse(&format!("http://x{target}"))
        .map(|url| {
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn form_of(body: &[u8]) -> BTreeMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(body)))
        .map(|url| {
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn encode_query_value(text: &str) -> String {
    reqwest::Url::parse_with_params("http://x/", &[("v", text)])
        .map(|u| {
            u.query()
                .unwrap_or_default()
                .trim_start_matches("v=")
                .to_owned()
        })
        .unwrap_or_default()
}

/// 路径片段的百分号解码。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 3 <= bytes.len() {
            if let Ok(v) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn s256(verifier: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl State {
    fn next_token(&mut self, kind: &str) -> String {
        self.counter += 1;
        format!("mock-{kind}-{:06}-{}", self.counter, "x".repeat(24))
    }

    fn issue_tokens(&mut self) -> Value {
        let access = self.next_token("access");
        let refresh = self.next_token("refresh");
        self.access_tokens
            .insert(access.clone(), now_secs() + self.access_lifetime_secs);
        self.refresh_tokens.insert(refresh.clone());
        json!({
            "token_type": "Bearer",
            "scope": "Files.ReadWrite.AppFolder User.Read",
            "expires_in": self.access_lifetime_secs,
            "access_token": access,
            "refresh_token": refresh,
        })
    }

    fn access_ok(&self, request: &Request) -> bool {
        let Some(token) = request
            .headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        self.access_tokens
            .get(token)
            .is_some_and(|expires| *expires > now_secs())
    }

    fn new_id(&mut self, kind: &str) -> String {
        self.counter += 1;
        format!(
            "{kind}{:08X}-0000-4000-8000-{:012X}",
            self.counter, self.counter
        )
    }

    fn write_file(&mut self, name: &str, bytes: Vec<u8>) -> (String, bool) {
        let created = !self.files.contains_key(name);
        if created {
            let id = self.new_id("F");
            self.files.insert(
                name.to_owned(),
                MockFile {
                    id,
                    bytes,
                    version: 1,
                },
            );
        } else if let Some(file) = self.files.get_mut(name) {
            file.bytes = bytes;
            file.version += 1;
        }
        let etag = self.files.get(name).map(MockFile::etag).unwrap_or_default();
        (etag, created)
    }
}

fn handle(state: &Mutex<State>, request: Request) -> Reply {
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    let path = request
        .target
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let reply = route(&mut guard, &request, &path);
    guard.log.push(Logged {
        method: request.method.clone(),
        path,
        status: reply.status,
        has_auth: request.headers.contains_key("authorization"),
        if_match: request.headers.get("if-match").cloned(),
        content_range: request.headers.get("content-range").cloned(),
        host: request.headers.get("host").cloned().unwrap_or_default(),
    });
    reply
}

fn route(state: &mut State, request: &Request, path: &str) -> Reply {
    match (request.method.as_str(), path) {
        ("GET", "/consumers/oauth2/v2.0/authorize") => authorize(state, request),
        ("POST", "/consumers/oauth2/v2.0/token") => token(state, request),
        ("GET", "/v1.0/me") => {
            if !state.access_ok(request) {
                return Reply::graph_error(401, "InvalidAuthenticationToken");
            }
            Reply::json(
                200,
                &json!({"displayName": state.account_name, "mail": "test.user@example.invalid"}),
            )
        }
        (_, path) if path.starts_with("/upload/") => upload(state, request, path),
        ("GET", path) if path.starts_with("/download/") => download(state, path),
        (_, path) if path.starts_with("/v1.0/me/drive/special/approot:/") => {
            approot(state, request, path)
        }
        _ => Reply::graph_error(404, "itemNotFound"),
    }
}

fn authorize(state: &mut State, request: &Request) -> Reply {
    let params = query_of(&request.target);
    state.authorize_log.push(AuthorizeRequest {
        params: params.clone(),
    });
    let get = |name: &str| params.get(name).cloned().unwrap_or_default();
    let redirect = get("redirect_uri");
    let valid_redirect = redirect == "http://localhost"
        || redirect
            .strip_prefix("http://localhost:")
            .is_some_and(|port| port.parse::<u16>().is_ok());
    if get("client_id") != state.client_id
        || !valid_redirect
        || get("response_type") != "code"
        || get("code_challenge_method") != "S256"
        || get("code_challenge").len() != 43
        || get("state").is_empty()
        || !get("scope").contains("Files.ReadWrite.AppFolder")
    {
        return Reply::json(400, &json!({"error": "invalid_request"}));
    }
    let state_param = get("state");
    // 回调地址按真实行为：根路径 `/`。
    let base = format!("{redirect}/");
    match state.mode.clone() {
        AuthorizeMode::Approve => {
            let code = state.next_token("code");
            state.codes.insert(
                code.clone(),
                CodeInfo {
                    challenge: get("code_challenge"),
                    redirect_uri: redirect.clone(),
                },
            );
            Reply::redirect(&format!(
                "{base}?code={}&state={}",
                encode_query_value(&code),
                encode_query_value(&state_param)
            ))
        }
        AuthorizeMode::Deny => Reply::redirect(&format!(
            "{base}?error=access_denied&error_description={}&state={}",
            encode_query_value("AADSTS65004: The user declined to consent to access the app."),
            encode_query_value(&state_param)
        )),
        AuthorizeMode::Error(error, description) => Reply::redirect(&format!(
            "{base}?error={}&error_description={}&state={}",
            encode_query_value(error),
            encode_query_value(description),
            encode_query_value(&state_param)
        )),
    }
}

fn token(state: &mut State, request: &Request) -> Reply {
    let form = form_of(&request.body);
    let get = |name: &str| form.get(name).cloned().unwrap_or_default();
    let grant_type = get("grant_type");
    let mut entry = TokenRequest {
        grant_type: grant_type.clone(),
        had_client_secret: form.contains_key("client_secret"),
        client_id: get("client_id"),
        scope: get("scope"),
        status: 0,
    };
    let reply = token_reply(state, &form, &grant_type);
    entry.status = reply.status;
    state.token_log.push(entry);
    reply
}

fn token_reply(state: &mut State, form: &BTreeMap<String, String>, grant_type: &str) -> Reply {
    let get = |name: &str| form.get(name).cloned().unwrap_or_default();
    if state.token_unavailable > 0 {
        state.token_unavailable -= 1;
        return Reply::json(503, &json!({"error": "temporarily_unavailable"}));
    }
    if get("client_id") != state.client_id {
        return Reply::oauth_error(
            400,
            "invalid_client",
            "AADSTS700016: Application with the given identifier was not found.",
        );
    }
    if form.contains_key("client_secret") {
        return Reply::oauth_error(400, "invalid_request", "公共客户端不应该带 client_secret");
    }
    match grant_type {
        "authorization_code" => {
            if let Some((status, error, description)) = state.code_error.take() {
                return Reply::oauth_error(status, error, description);
            }
            // 授权码只能用一次。
            let Some(info) = state.codes.remove(&get("code")) else {
                return Reply::oauth_error(400, "invalid_grant", "AADSTS70000: bad or reused code");
            };
            if info.redirect_uri != get("redirect_uri") {
                return Reply::oauth_error(400, "invalid_grant", "AADSTS50011: redirect mismatch");
            }
            if s256(&get("code_verifier")) != info.challenge {
                return Reply::oauth_error(400, "invalid_grant", "AADSTS501481: PKCE mismatch");
            }
            Reply::json(200, &state.issue_tokens())
        }
        "refresh_token" => {
            if let Some((status, error, description)) = state.refresh_error.take() {
                return Reply::oauth_error(status, error, description);
            }
            let token = get("refresh_token");
            if !state.refresh_tokens.contains(&token) {
                return Reply::oauth_error(
                    400,
                    "invalid_grant",
                    "AADSTS70008: The refresh token has expired or is invalid.",
                );
            }
            if state.strict_rotation {
                state.refresh_tokens.remove(&token);
            }
            Reply::json(200, &state.issue_tokens())
        }
        _ => Reply::oauth_error(400, "unsupported_grant_type", "grant"),
    }
}

// ---------------------------------------------------------------------------
// OneDrive 应用文件夹

fn host_port(request: &Request) -> String {
    request
        .headers
        .get("host")
        .and_then(|host| host.rsplit_once(':'))
        .map(|(_, port)| port.to_owned())
        .unwrap_or_default()
}

fn drive_item(name: &str, file: &MockFile) -> Value {
    json!({
        "id": file.id,
        "name": name,
        "size": file.bytes.len(),
        "eTag": file.etag(),
        "cTag": format!("\"c:{{{}}},{}\"", file.id, file.version),
        "file": {"mimeType": "application/octet-stream"},
    })
}

/// 限流 / 注入的错误先于一切。
fn injected_failure(state: &mut State) -> Option<Reply> {
    let (status, retry_after) = state.graph_failures.pop_front()?;
    let mut reply = Reply::graph_error(
        status,
        if status == 429 || status == 503 {
            "activityLimitReached"
        } else {
            "generalException"
        },
    );
    if let Some(secs) = retry_after {
        reply = reply.with_header("Retry-After", secs.to_string());
    }
    Some(reply)
}

fn approot(state: &mut State, request: &Request, path: &str) -> Reply {
    if !state.access_ok(request) {
        return Reply::graph_error(401, "InvalidAuthenticationToken");
    }
    if let Some(reply) = injected_failure(state) {
        return reply;
    }
    let rest = &path["/v1.0/me/drive/special/approot:/".len()..];
    let (raw_name, suffix) = match rest.split_once(':') {
        Some((name, suffix)) => (name, suffix),
        None => (rest, ""),
    };
    let name = percent_decode(raw_name);
    match (request.method.as_str(), suffix) {
        ("GET", "") => match state.files.get(&name) {
            Some(file) => Reply::json(200, &drive_item(&name, file)),
            None => Reply::graph_error(404, "itemNotFound"),
        },
        ("GET", "/content") => match state.files.get(&name) {
            Some(file) => Reply::redirect(&format!(
                "http://{}:{}/download/{}",
                if state.download_same_host {
                    "127.0.0.1"
                } else {
                    "localhost"
                },
                host_port(request),
                file.id
            )),
            None => Reply::graph_error(404, "itemNotFound"),
        },
        ("POST", "/createUploadSession") => create_session(state, request, &name),
        ("DELETE", "") => {
            let Some(file) = state.files.get(&name) else {
                return Reply::graph_error(404, "itemNotFound");
            };
            if let Some(expected) = request.headers.get("if-match") {
                if !state.ignore_if_match && *expected != file.etag() {
                    return Reply::graph_error(412, "preconditionFailed");
                }
            }
            state.files.remove(&name);
            Reply::empty(204)
        }
        _ => Reply::graph_error(404, "itemNotFound"),
    }
}

fn create_session(state: &mut State, request: &Request, name: &str) -> Reply {
    if state.quota_full {
        return Reply::graph_error(507, "quotaLimitReached");
    }
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    let item = body.get("item").cloned().unwrap_or(Value::Null);
    // 官方文档：conflictBehavior 默认 fail。
    let behavior = item
        .get("@microsoft.graph.conflictBehavior")
        .and_then(Value::as_str)
        .unwrap_or("fail")
        .to_owned();
    // 真实的个人版 OneDrive 见到 item 里的 fileSize 就回 400 invalidRequest（2026-10-21 对真服务实测；
    // 官方文档反而写"只在个人版可用"）。模拟服务照真服务的行为来，防止有人照文档把它加回去。
    if item.get("fileSize").is_some() {
        return Reply::graph_error(400, "invalidRequest");
    }
    if let Some(expected) = request.headers.get("if-match") {
        if !state.ignore_if_match {
            let current = state.files.get(name).map(MockFile::etag);
            if current.as_deref() != Some(expected.as_str()) {
                return Reply::graph_error(412, "preconditionFailed");
            }
        }
    }
    let exists = state.files.contains_key(name);
    if behavior == "fail" && exists {
        return Reply::graph_error(409, "nameAlreadyExists");
    }
    let id = state.new_id("S");
    state.sessions.insert(
        id.clone(),
        Session {
            name: name.to_owned(),
            total: None,
            received: Vec::new(),
            fail_if_exists: behavior == "fail",
        },
    );
    if let Some((external_name, bytes)) = state.external_after_session.take() {
        state.write_file(&external_name, bytes);
    }
    let host = request.headers.get("host").cloned().unwrap_or_default();
    Reply::json(
        200,
        &json!({
            "uploadUrl": format!("http://{host}/upload/{id}"),
            "expirationDateTime": "2099-01-01T00:00:00.000Z",
        }),
    )
}

fn download(state: &mut State, path: &str) -> Reply {
    let id = path.trim_start_matches("/download/");
    let found = state
        .files
        .iter()
        .find(|(_, file)| file.id == id)
        .map(|(name, file)| (name.clone(), file.bytes.clone()));
    let Some((_, bytes)) = found else {
        return Reply::graph_error(404, "itemNotFound");
    };
    if let Some((name, external)) = state.external_after_download.take() {
        state.write_file(&name, external);
    }
    Reply {
        status: 200,
        headers: vec![(
            "Content-Type".to_owned(),
            "application/octet-stream".to_owned(),
        )],
        body: bytes,
    }
}

fn upload(state: &mut State, request: &Request, path: &str) -> Reply {
    // 官方文档：往 uploadUrl 传分片时带 Authorization 可能 401。
    if request.headers.contains_key("authorization") {
        return Reply::graph_error(401, "InvalidAuthenticationToken");
    }
    let id = path.trim_start_matches("/upload/").to_owned();
    if request.method == "DELETE" {
        return if state.sessions.remove(&id).is_some() {
            Reply::empty(204)
        } else {
            Reply::graph_error(404, "itemNotFound")
        };
    }
    if request.method != "PUT" {
        return Reply::graph_error(404, "itemNotFound");
    }
    if let Some(status) = state.upload_failures.pop_front() {
        return Reply::graph_error(status, "generalException");
    }
    let Some(session) = state.sessions.get_mut(&id) else {
        return Reply::graph_error(404, "itemNotFound");
    };
    // Content-Range: bytes a-b/total
    let range = request
        .headers
        .get("content-range")
        .and_then(|v| v.strip_prefix("bytes "))
        .and_then(|v| v.split_once('/'))
        .and_then(|(span, total)| {
            let (a, b) = span.split_once('-')?;
            Some((
                a.parse::<usize>().ok()?,
                b.parse::<usize>().ok()?,
                total.parse::<usize>().ok()?,
            ))
        });
    let Some((start, end, total)) = range else {
        return Reply::graph_error(400, "invalidRange");
    };
    // 第一片定下总长，之后每一片声明的总长都要一样（官方文档的要求）
    let expected_total = *session.total.get_or_insert(total);
    if total != expected_total
        || start != session.received.len()
        || end + 1 - start != request.body.len()
    {
        return Reply::graph_error(416, "invalidRange");
    }
    let is_last = end + 1 == total;
    if !is_last && !request.body.len().is_multiple_of(ALIGN) {
        return Reply::graph_error(400, "invalidRange");
    }
    session.received.extend_from_slice(&request.body);
    if !is_last {
        let next = session.received.len();
        return Reply::json(
            202,
            &json!({
                "expirationDateTime": "2099-01-01T00:00:00.000Z",
                "nextExpectedRanges": [format!("{next}-")],
            }),
        );
    }
    // 提交。
    let Some(session) = state.sessions.remove(&id) else {
        return Reply::graph_error(404, "itemNotFound");
    };
    if session.fail_if_exists && state.files.contains_key(&session.name) {
        return Reply::graph_error(409, "nameAlreadyExists");
    }
    let name = session.name.clone();
    let (_, created) = state.write_file(&name, session.received);
    let Some(file) = state.files.get(&name) else {
        return Reply::graph_error(404, "itemNotFound");
    };
    Reply::json(if created { 201 } else { 200 }, &drive_item(&name, file))
}

impl MockMicrosoft {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let addr = listener.local_addr().expect("地址");
        let state = Arc::new(Mutex::new(State {
            client_id: CLIENT_ID.to_owned(),
            account_name: ACCOUNT_NAME.to_owned(),
            mode: AuthorizeMode::Approve,
            counter: 0,
            codes: HashMap::new(),
            refresh_tokens: HashSet::new(),
            access_tokens: HashMap::new(),
            access_lifetime_secs: 3600,
            strict_rotation: true,
            refresh_error: None,
            code_error: None,
            token_unavailable: 0,
            authorize_log: Vec::new(),
            token_log: Vec::new(),
            log: Vec::new(),
            files: BTreeMap::new(),
            sessions: HashMap::new(),
            graph_failures: VecDeque::new(),
            upload_failures: VecDeque::new(),
            quota_full: false,
            ignore_if_match: false,
            external_after_download: None,
            external_after_session: None,
            download_same_host: false,
        }));
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let state = task_state.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(socket);
                    while let Some(request) = read_request(&mut reader).await {
                        let reply = handle(&state, request);
                        write_reply(&mut reader, reply).await;
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

    /// 指向这个模拟服务的授权 / 令牌 / Graph 地址。
    pub fn endpoints(&self) -> Endpoints {
        Endpoints {
            authorize: format!("{}/consumers/oauth2/v2.0/authorize", self.base_url()),
            token: format!("{}/consumers/oauth2/v2.0/token", self.base_url()),
            graph: format!("{}/v1.0", self.base_url()),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    // -- 登录 / 令牌 --

    pub fn set_mode(&self, mode: AuthorizeMode) {
        self.with(|s| s.mode = mode);
    }

    pub fn set_access_lifetime(&self, secs: i64) {
        self.with(|s| s.access_lifetime_secs = secs);
    }

    pub fn set_strict_rotation(&self, strict: bool) {
        self.with(|s| s.strict_rotation = strict);
    }

    /// 下一次刷新令牌请求回错误（比如 400 `invalid_grant`）。
    pub fn fail_next_refresh(&self, status: u16, error: &'static str, description: &'static str) {
        self.with(|s| s.refresh_error = Some((status, error, description)));
    }

    pub fn fail_next_code_exchange(
        &self,
        status: u16,
        error: &'static str,
        description: &'static str,
    ) {
        self.with(|s| s.code_error = Some((status, error, description)));
    }

    pub fn token_unavailable(&self, count: usize) {
        self.with(|s| s.token_unavailable = count);
    }

    /// 作废所有刷新令牌（模拟用户在微软账户里撤销了授权）。
    pub fn revoke_all(&self) {
        self.with(|s| {
            s.refresh_tokens.clear();
            s.access_tokens.clear();
        });
    }

    /// 让所有已发出的访问令牌在服务器看来都过期了（模拟本机时钟比服务器慢、令牌被提前作废）。
    pub fn expire_access_tokens(&self) {
        self.with(|s| {
            for expires in s.access_tokens.values_mut() {
                *expires = 0;
            }
        });
    }

    /// 直接造一组令牌（“已经登录过”的场景）。
    pub fn mint_tokens(&self) -> (String, String) {
        self.with(|s| {
            let value = s.issue_tokens();
            (
                value["access_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                value["refresh_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
    }

    pub fn authorize_requests(&self) -> Vec<AuthorizeRequest> {
        self.with(|s| s.authorize_log.clone())
    }

    pub fn token_requests(&self) -> Vec<TokenRequest> {
        self.with(|s| s.token_log.clone())
    }

    pub fn requests(&self) -> Vec<Logged> {
        self.with(|s| s.log.clone())
    }

    pub fn clear_log(&self) {
        self.with(|s| {
            s.log.clear();
            s.token_log.clear();
            s.authorize_log.clear();
        });
    }

    // -- 应用文件夹里的文件 --

    /// 另一台电脑（或别的程序）直接写了这个文件，返回新的 eTag。
    pub fn put_file(&self, name: &str, bytes: Vec<u8>) -> String {
        self.with(|s| s.write_file(name, bytes).0)
    }

    pub fn file(&self, name: &str) -> Option<Vec<u8>> {
        self.with(|s| s.files.get(name).map(|f| f.bytes.clone()))
    }

    pub fn file_etag(&self, name: &str) -> Option<String> {
        self.with(|s| s.files.get(name).map(MockFile::etag))
    }

    pub fn file_names(&self) -> Vec<String> {
        self.with(|s| s.files.keys().cloned().collect())
    }

    pub fn delete_file(&self, name: &str) {
        self.with(|s| {
            s.files.remove(name);
        });
    }

    /// 还没提交或取消的上传会话个数。
    pub fn open_sessions(&self) -> usize {
        self.with(|s| s.sessions.len())
    }

    /// 接下来的 Graph 请求依次回这些状态码（和可选的 `Retry-After` 秒数）。
    pub fn fail_graph(&self, status: u16, retry_after: Option<u64>, count: usize) {
        self.with(|s| {
            for _ in 0..count {
                s.graph_failures.push_back((status, retry_after));
            }
        });
    }

    /// 接下来的上传分片请求回这个状态码。
    pub fn fail_next_upload_fragment(&self, status: u16) {
        self.with(|s| s.upload_failures.push_back(status));
    }

    pub fn set_quota_full(&self, full: bool) {
        self.with(|s| s.quota_full = full);
    }

    pub fn set_download_same_host(&self, same: bool) {
        self.with(|s| s.download_same_host = same);
    }

    pub fn set_ignore_if_match(&self, ignore: bool) {
        self.with(|s| s.ignore_if_match = ignore);
    }

    /// 我下载完同步包之后，“另一台电脑”马上写入这个内容。
    pub fn external_write_after_next_download(&self, name: &str, bytes: Vec<u8>) {
        self.with(|s| s.external_after_download = Some((name.to_owned(), bytes)));
    }

    /// 我创建上传会话之后、提交之前，“另一台电脑”写入这个内容（官方文档里没法堵住的窗口）。
    pub fn external_write_after_next_session(&self, name: &str, bytes: Vec<u8>) {
        self.with(|s| s.external_after_session = Some((name.to_owned(), bytes)));
    }
}
