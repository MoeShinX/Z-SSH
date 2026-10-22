//! 进程内模拟的 GitHub Gist 接口，给同步的集成测试、截图脚本用。
//!
//! 不碰真实的 GitHub：监听 `127.0.0.1` 的随机端口，手写最小的 HTTP/1.1。模拟了同步用到的
//! 接口和 GitHub 的几个特殊行为：
//!
//! - `GET /user`（令牌属于谁；经典令牌带 `X-OAuth-Scopes`，细粒度令牌不带）
//! - `POST /gists`、`GET` / `PATCH` / `DELETE /gists/{id}`；每次修改都在 `history` 里加一个新版本
//!   （最新的在最前面，`version` 是 40 位十六进制，像 commit 号）；写别人的 Gist 回 404（和 GitHub 一样）
//! - 文件内容超过 [`MockGithub::set_truncate_over`] 设的大小时返回 `truncated: true` 和被截断的 `content`，
//!   完整内容要从 `raw_url` 读（`/raw/{id}/{version}/{文件名}`，不需要令牌）
//! - 限流：下面几次请求回 403 / 429 加 `Retry-After` 或 `x-ratelimit-*` 头
//! - “另一台电脑抢先写”：在下一次 GET 响应之后 / 下一次 PATCH 之后让内容悄悄变掉
//! - 接口地址可以带路径前缀（模拟 GitHub Enterprise 的 `/api/v3`）
//!
//! Gist ID 故意做成 `mock` 开头、不是 32 位十六进制，截图里就不会和真实的 Gist ID 混淆。

#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use zssh_core::sync::remote::content_hash;

#[derive(Debug, Clone)]
pub struct GithubOptions {
    /// 这个服务认的令牌。
    pub token: String,
    /// 令牌所属用户。
    pub login: String,
    /// 经典令牌的权限范围（`X-OAuth-Scopes`）；`None` = 细粒度令牌（没有这个头）。
    pub scopes: Option<String>,
    /// 接口路径前缀，例如 `/api/v3`（GitHub Enterprise）；GitHub.com 是空。
    pub prefix: String,
    /// `raw_url` 用哪个主机；`None` = 用请求的 Host（和接口同一个主机）。
    pub raw_host: Option<String>,
}

impl GithubOptions {
    pub fn classic(token: &str, login: &str) -> Self {
        Self {
            token: token.to_owned(),
            login: login.to_owned(),
            scopes: Some("gist".to_owned()),
            prefix: String::new(),
            raw_host: None,
        }
    }

    pub fn fine_grained(token: &str, login: &str) -> Self {
        Self {
            scopes: None,
            ..Self::classic(token, login)
        }
    }
}

/// 限流响应的写法。
#[derive(Debug, Clone, Copy)]
pub enum RateSpec {
    /// 403 + `Retry-After: N`
    RetryAfter(u64),
    /// 403 + `x-ratelimit-remaining: 0` + `x-ratelimit-reset: 现在 + N 秒`
    ResetIn(i64),
    /// 429，没有任何时间信息
    Status429,
}

#[derive(Debug, Clone)]
pub struct GhLogged {
    pub method: String,
    pub path: String,
    pub status: u16,
    /// 带了 `Authorization` 头。
    pub has_auth_header: bool,
    pub accept: Option<String>,
    pub api_version: Option<String>,
    pub user_agent: Option<String>,
    pub host: String,
}

struct Version {
    id: String,
    files: BTreeMap<String, String>,
}

struct Gist {
    owner: String,
    description: String,
    public: bool,
    /// 最旧的在前。
    versions: Vec<Version>,
}

type External = (String, String, String);

struct State {
    options: GithubOptions,
    gists: BTreeMap<String, Gist>,
    counter: u64,
    log: Vec<GhLogged>,
    truncate_over: Option<usize>,
    rate: VecDeque<RateSpec>,
    fail_next: VecDeque<u16>,
    after_get: Option<External>,
    after_patch: Option<External>,
    token_revoked: bool,
    write_allowed: bool,
    /// `raw_url` 返回的内容只给前 N 个字节（模拟下载到一半断了）。
    raw_cut: Option<usize>,
}

pub struct MockGithub {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for MockGithub {
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

    fn message(status: u16, text: &str) -> Self {
        Self::json(status, &json!({ "message": text }))
    }

    fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }
}

fn now_secs() -> i64 {
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
        204 => "No Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
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

fn version_id(gist: &str, n: u64) -> String {
    content_hash(format!("{gist}:{n}").as_bytes())[..40].to_owned()
}

impl State {
    fn next_version(&mut self, gist: &str) -> String {
        self.counter += 1;
        version_id(gist, self.counter)
    }

    fn push_version(&mut self, id: &str, files: BTreeMap<String, String>) {
        let version = self.next_version(id);
        if let Some(gist) = self.gists.get_mut(id) {
            gist.versions.push(Version { id: version, files });
        }
    }

    fn apply_external(&mut self, external: &External) {
        let (id, name, content) = external;
        let Some(gist) = self.gists.get(id) else {
            return;
        };
        let mut files = gist
            .versions
            .last()
            .map(|v| v.files.clone())
            .unwrap_or_default();
        files.insert(name.clone(), content.clone());
        self.push_version(id, files);
    }

    fn gist_json(&self, id: &str, host: &str) -> Option<Value> {
        let gist = self.gists.get(id)?;
        let latest = gist.versions.last()?;
        let raw_host = self
            .options
            .raw_host
            .clone()
            .unwrap_or_else(|| host.to_owned());
        let mut files = serde_json::Map::new();
        for (name, content) in &latest.files {
            let cut = self
                .truncate_over
                .map(|limit| content.len() > limit)
                .unwrap_or(false);
            let shown = if cut {
                content[..self.truncate_over.unwrap_or(0)].to_owned()
            } else {
                content.clone()
            };
            files.insert(
                name.clone(),
                json!({
                    "filename": name,
                    "size": content.len(),
                    "truncated": cut,
                    "content": shown,
                    "raw_url": format!("http://{raw_host}/raw/{id}/{}/{name}", latest.id),
                }),
            );
        }
        let history: Vec<Value> = gist
            .versions
            .iter()
            .rev()
            .map(|v| json!({ "version": v.id }))
            .collect();
        Some(json!({
            "id": id,
            "description": gist.description,
            "public": gist.public,
            "owner": { "login": gist.owner },
            "files": files,
            "history": history,
            "truncated": false,
        }))
    }
}

fn handle(state: &Mutex<State>, request: Request) -> Reply {
    let (raw_path, _query) = match request.target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query.to_owned()),
        None => (request.target.clone(), String::new()),
    };
    let host = request.headers.get("host").cloned().unwrap_or_default();
    let mut guard = state.lock().unwrap();
    let is_raw = raw_path.starts_with("/raw/");
    let reply = if is_raw {
        handle_raw(&guard, &raw_path)
    } else if let Some(status) = guard.fail_next.pop_front() {
        Reply::message(status, "injected failure")
    } else if let Some(spec) = guard.rate.pop_front() {
        match spec {
            RateSpec::RetryAfter(secs) => {
                Reply::message(403, "You have exceeded a secondary rate limit.")
                    .with_header("Retry-After", secs.to_string())
            }
            RateSpec::ResetIn(secs) => Reply::message(403, "API rate limit exceeded")
                .with_header("x-ratelimit-remaining", "0")
                .with_header("x-ratelimit-reset", (now_secs() + secs).to_string()),
            RateSpec::Status429 => Reply::message(429, "Too Many Requests"),
        }
    } else {
        handle_api(&mut guard, &request, &raw_path, &host)
    };
    guard.log.push(GhLogged {
        method: request.method.clone(),
        path: raw_path,
        status: reply.status,
        has_auth_header: request.headers.contains_key("authorization"),
        accept: request.headers.get("accept").cloned(),
        api_version: request.headers.get("x-github-api-version").cloned(),
        user_agent: request.headers.get("user-agent").cloned(),
        host,
    });
    reply
}

fn handle_raw(state: &State, path: &str) -> Reply {
    // /raw/{id}/{version}/{file}
    let parts: Vec<&str> = path.trim_start_matches("/raw/").splitn(3, '/').collect();
    let [id, version, name] = parts[..] else {
        return Reply::message(404, "Not Found");
    };
    let content = state
        .gists
        .get(id)
        .and_then(|gist| gist.versions.iter().find(|v| v.id == version))
        .and_then(|v| v.files.get(name));
    match content {
        Some(content) => Reply {
            status: 200,
            headers: vec![("Content-Type".to_owned(), "text/plain".to_owned())],
            body: match state.raw_cut {
                Some(limit) => content.as_bytes()[..limit.min(content.len())].to_vec(),
                None => content.as_bytes().to_vec(),
            },
        },
        None => Reply::message(404, "Not Found"),
    }
}

fn handle_api(state: &mut State, request: &Request, path: &str, host: &str) -> Reply {
    let Some(rest) = path.strip_prefix(state.options.prefix.as_str()) else {
        return Reply::message(404, "Not Found");
    };
    let expected = format!("Bearer {}", state.options.token);
    let authorized =
        !state.token_revoked && request.headers.get("authorization") == Some(&expected);
    if !authorized {
        return Reply::message(401, "Bad credentials");
    }
    let scopes = state.options.scopes.clone();
    let with_scopes = |reply: Reply| match &scopes {
        Some(scopes) => reply.with_header("X-OAuth-Scopes", scopes.clone()),
        None => reply,
    };
    let method = request.method.as_str();
    let login = state.options.login.clone();
    match (method, rest) {
        ("GET", "/user") => with_scopes(
            Reply::json(200, &json!({ "login": login }))
                .with_header("x-ratelimit-remaining", "4999"),
        ),
        ("POST", "/gists") => {
            if !state.write_allowed {
                return Reply::message(403, "Resource not accessible by personal access token")
                    .with_header("x-ratelimit-remaining", "4999");
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let mut files = BTreeMap::new();
            if let Some(map) = body.get("files").and_then(Value::as_object) {
                for (name, file) in map {
                    if let Some(content) = file.get("content").and_then(Value::as_str) {
                        files.insert(name.clone(), content.to_owned());
                    }
                }
            }
            if files.is_empty() {
                return Reply::message(422, "Validation Failed");
            }
            state.counter += 1;
            let id = format!("mock{:028}", state.counter);
            let version = state.next_version(&id);
            state.gists.insert(
                id.clone(),
                Gist {
                    owner: login,
                    description: body
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    public: body.get("public").and_then(Value::as_bool).unwrap_or(false),
                    versions: vec![Version { id: version, files }],
                },
            );
            let json = state.gist_json(&id, host).unwrap_or(Value::Null);
            Reply::json(201, &json)
        }
        (_, rest) if rest.starts_with("/gists/") => {
            let id = rest.trim_start_matches("/gists/").to_owned();
            let Some(owner) = state.gists.get(&id).map(|gist| gist.owner.clone()) else {
                return Reply::message(404, "Not Found");
            };
            match method {
                "GET" => {
                    let json = state.gist_json(&id, host).unwrap_or(Value::Null);
                    let reply =
                        Reply::json(200, &json).with_header("x-ratelimit-remaining", "4999");
                    // 响应已经定了，这之后“另一台电脑”才写。
                    if let Some(external) = state.after_get.take() {
                        if external.0 == id {
                            state.apply_external(&external);
                        } else {
                            state.after_get = Some(external);
                        }
                    }
                    reply
                }
                "PATCH" | "DELETE" => {
                    // 写别人的 Gist：GitHub 回 404（不告诉你它存在）。
                    if owner != login {
                        return Reply::message(404, "Not Found");
                    }
                    if !state.write_allowed {
                        return Reply::message(
                            403,
                            "Resource not accessible by personal access token",
                        )
                        .with_header("x-ratelimit-remaining", "4999");
                    }
                    if method == "DELETE" {
                        state.gists.remove(&id);
                        return Reply {
                            status: 204,
                            headers: Vec::new(),
                            body: Vec::new(),
                        };
                    }
                    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                    let mut files = state
                        .gists
                        .get(&id)
                        .and_then(|gist| gist.versions.last())
                        .map(|v| v.files.clone())
                        .unwrap_or_default();
                    if let Some(map) = body.get("files").and_then(Value::as_object) {
                        for (name, file) in map {
                            if file.is_null() {
                                files.remove(name);
                            } else if let Some(content) =
                                file.get("content").and_then(Value::as_str)
                            {
                                files.insert(name.clone(), content.to_owned());
                            }
                        }
                    }
                    if files.is_empty() {
                        return Reply::message(422, "Validation Failed");
                    }
                    state.push_version(&id, files);
                    let json = state.gist_json(&id, host).unwrap_or(Value::Null);
                    let reply = Reply::json(200, &json);
                    if let Some(external) = state.after_patch.take() {
                        if external.0 == id {
                            state.apply_external(&external);
                        } else {
                            state.after_patch = Some(external);
                        }
                    }
                    reply
                }
                _ => Reply::message(404, "Not Found"),
            }
        }
        _ => Reply::message(404, "Not Found"),
    }
}

impl MockGithub {
    pub async fn start(options: GithubOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let addr = listener.local_addr().expect("地址");
        let state = Arc::new(Mutex::new(State {
            options,
            gists: BTreeMap::new(),
            counter: 0,
            log: Vec::new(),
            truncate_over: None,
            rate: VecDeque::new(),
            fail_next: VecDeque::new(),
            after_get: None,
            after_patch: None,
            token_revoked: false,
            write_allowed: true,
            raw_cut: None,
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

    /// 填进设置的“接口地址”：服务地址 + 路径前缀。
    pub fn api_base(&self) -> String {
        let prefix = self.state.lock().unwrap().options.prefix.clone();
        format!("http://{}{prefix}", self.addr)
    }

    pub fn token(&self) -> String {
        self.state.lock().unwrap().options.token.clone()
    }

    /// 直接放一个 Gist（不经过接口）。`owner` 不是令牌的用户就是“别人的 Gist”。
    pub fn add_gist(&self, owner: &str, files: &[(&str, &str)]) -> String {
        let mut state = self.state.lock().unwrap();
        state.counter += 1;
        let id = format!("mock{:028}", state.counter);
        let version = state.next_version(&id);
        let files = files
            .iter()
            .map(|(name, content)| ((*name).to_owned(), (*content).to_owned()))
            .collect();
        state.gists.insert(
            id.clone(),
            Gist {
                owner: owner.to_owned(),
                description: String::new(),
                public: false,
                versions: vec![Version { id: version, files }],
            },
        );
        id
    }

    pub fn gist_ids(&self) -> Vec<String> {
        self.state.lock().unwrap().gists.keys().cloned().collect()
    }

    pub fn gist_files(&self, id: &str) -> BTreeMap<String, String> {
        let state = self.state.lock().unwrap();
        state
            .gists
            .get(id)
            .and_then(|gist| gist.versions.last())
            .map(|v| v.files.clone())
            .unwrap_or_default()
    }

    pub fn gist_file(&self, id: &str, name: &str) -> Option<String> {
        self.gist_files(id).get(name).cloned()
    }

    pub fn gist_description(&self, id: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        state.gists.get(id).map(|gist| gist.description.clone())
    }

    pub fn gist_is_public(&self, id: &str) -> Option<bool> {
        let state = self.state.lock().unwrap();
        state.gists.get(id).map(|gist| gist.public)
    }

    pub fn version_count(&self, id: &str) -> usize {
        let state = self.state.lock().unwrap();
        state.gists.get(id).map(|g| g.versions.len()).unwrap_or(0)
    }

    pub fn latest_version(&self, id: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        state
            .gists
            .get(id)
            .and_then(|g| g.versions.last())
            .map(|v| v.id.clone())
    }

    /// 另一台电脑（或网页上）改了这个文件：内容换掉、版本前进。
    pub fn external_write(&self, id: &str, name: &str, content: &str) {
        let mut state = self.state.lock().unwrap();
        state.apply_external(&(id.to_owned(), name.to_owned(), content.to_owned()));
    }

    /// 下一次 `GET /gists/{id}` 的响应发出之后，“另一台电脑”写一次。
    /// 这正好落在“引擎读了云端”和“写之前再读一次”之间。
    pub fn external_write_after_next_get(&self, id: &str, name: &str, content: &str) {
        self.state.lock().unwrap().after_get =
            Some((id.to_owned(), name.to_owned(), content.to_owned()));
    }

    /// 下一次 `PATCH` 处理完之后，“另一台电脑”紧接着写一次。
    /// 这正好落在“写”和“写完再读一次确认”之间。
    pub fn external_write_after_next_patch(&self, id: &str, name: &str, content: &str) {
        self.state.lock().unwrap().after_patch =
            Some((id.to_owned(), name.to_owned(), content.to_owned()));
    }

    /// `raw_url` 只返回前 N 个字节（长度和 `size` 对不上）。
    pub fn set_raw_cut(&self, limit: Option<usize>) {
        self.state.lock().unwrap().raw_cut = limit;
    }

    pub fn set_truncate_over(&self, limit: Option<usize>) {
        self.state.lock().unwrap().truncate_over = limit;
    }

    pub fn rate_limit_next(&self, count: usize, spec: RateSpec) {
        let mut state = self.state.lock().unwrap();
        for _ in 0..count {
            state.rate.push_back(spec);
        }
    }

    pub fn fail_next(&self, status: u16, count: usize) {
        let mut state = self.state.lock().unwrap();
        for _ in 0..count {
            state.fail_next.push_back(status);
        }
    }

    /// 令牌被撤销 / 过期：之后所有接口请求回 401。
    pub fn revoke_token(&self) {
        self.state.lock().unwrap().token_revoked = true;
    }

    /// 模拟只有读权限的细粒度令牌：创建、修改、删除回 403。
    pub fn set_write_allowed(&self, allowed: bool) {
        self.state.lock().unwrap().write_allowed = allowed;
    }

    pub fn requests(&self) -> Vec<GhLogged> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }

    /// 读 `raw_url` 的请求。
    pub fn raw_requests(&self) -> Vec<GhLogged> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/raw/"))
            .collect()
    }

    /// 接口请求（不含 raw）。
    pub fn api_requests(&self) -> Vec<GhLogged> {
        self.requests()
            .into_iter()
            .filter(|r| !r.path.starts_with("/raw/"))
            .collect()
    }
}
