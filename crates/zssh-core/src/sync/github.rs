//! GitHub Gist 存储：把同步包（已经用主密码整体加密）存进一个 Gist 的 `zssh-sync.json`。
//!
//! **Gist 没有真正“私有”的**：secret Gist 不会出现在公开列表里，但**任何拿到链接的人都能读**。
//! 同步包是密文，所以内容是安全的，但界面和文档里不能写“私有 Gist”。
//!
//! - 认证：个人访问令牌（经典令牌要 `gist` 权限；细粒度令牌要账户权限里的 Gists 读写），
//!   `Authorization: Bearer`。令牌只在内存里和请求头里，Debug 输出和错误文本里都没有它。
//! - **只动 `zssh-sync.json` 这一个文件**：写入用 `PATCH /gists/{id}` 且只带这一个文件，
//!   Gist 里的其他文件、描述都不碰。
//! - **没有条件写入**：`PATCH /gists/{id}` 不认 `If-Match`。所以写之前再读一次，比较
//!   `history` 里最新的 `version`，一致才写；写完再读一次，确认最新的版本就是刚写出去的那一版。
//!   两台电脑在“再读”和“写”之间的几十毫秒里同时写，仍然会有一边覆盖另一边（写完的确认能发现
//!   其中一种顺序，发现不了另一种），这是这个接口决定的，设置页要如实说。
//! - 大文件：接口返回的文件内容超过一定大小会被截断（`truncated: true`），这时改从 `raw_url`
//!   读完整内容，并用 `size` 核对长度。
//! - 限流：403 / 429 加上 `Retry-After` 或剩余次数为 0，返回 [`StorageError::RateLimited`]，
//!   引擎不会重试，调度器在等待期内不再自动同步。
//! - 接口地址可以配置（默认 `https://api.github.com`，GitHub Enterprise 填 `https://域名/api/v3`）。

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Client, Method, StatusCode, Url};
use serde_json::{json, Value};

use super::net::{
    build_client, map_http_error, proxy_auth_required, read_body_limited, read_error_text,
    SyncProxy, MAX_META_BYTES, MAX_PACKAGE_BYTES,
};
use super::remote::{Check, ConcurrencyMode, ConcurrencyProbe, ConnectionProbe};
use super::storage::{StorageError, StoredObject, SyncStorage};
use crate::ssh::BoxFuture;

/// Gist 里的文件名（也是这个存储唯一会碰的文件）。
pub const FILE_NAME: &str = "zssh-sync.json";
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
/// 新建 Gist 的描述。
pub const GIST_DESCRIPTION: &str = "Z-SSH 配置同步（已加密）";
/// Gist 的文件不能是空的，新建时先放一个占位内容；读到它就当作“还没有同步数据”。
pub const PLACEHOLDER: &str = "{\"zssh\":\"empty-gist-placeholder\"}";
const SERVICE: &str = "GitHub";
const API_VERSION: &str = "2022-11-28";
/// 限流等待的上限（主限流最长一小时就会重置；再大的数字多半是时钟或响应有问题）。
const MAX_WAIT_SECS: u64 = 2 * 3600;
/// 限流响应里没有任何时间信息时的默认等待。
const DEFAULT_WAIT_SECS: u64 = 60;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GithubConfig {
    /// 接口地址；空 = [`DEFAULT_API_BASE`]。
    #[serde(default)]
    pub api_base: String,
    /// 个人访问令牌（只存在本机，DPAPI 加密，不进同步包）。
    pub token: String,
    /// Gist ID。新建 Gist 之前可以是空的。
    #[serde(default)]
    pub gist_id: String,
    /// 接口地址是 `http://` 时（令牌会明文发送），用户确认过才允许。
    #[serde(default)]
    pub allow_insecure_http: bool,
}

impl std::fmt::Debug for GithubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubConfig")
            .field("api_base", &self.api_base)
            .field("gist_id", &self.gist_id)
            .finish()
    }
}

impl GithubConfig {
    pub fn is_plain_http(&self) -> bool {
        Url::parse(self.effective_api_base().trim())
            .map(|url| url.scheme() == "http")
            .unwrap_or(false)
    }

    pub fn effective_api_base(&self) -> &str {
        let base = self.api_base.trim();
        if base.is_empty() {
            DEFAULT_API_BASE
        } else {
            base
        }
    }
}

fn io(message: impl Into<String>) -> StorageError {
    StorageError::Io(message.into())
}

/// Gist ID 只会是字母数字（GitHub.com 是 32 位十六进制，Enterprise 可能是数字）。
/// 它要拼进请求路径，所以不合法的字符一律拒绝。
fn valid_gist_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub struct GithubApi {
    client: Client,
    /// 接口地址，末尾没有 `/`。
    base: String,
    /// 接口主机（`raw_url` 和它同一个主机才带令牌）。
    host: String,
    token: HeaderValue,
    gist_id: String,
}

impl GithubApi {
    /// `need_gist`：读写同步包要有 Gist ID；新建 Gist 时还没有。
    pub fn new(
        config: &GithubConfig,
        proxy: Option<&SyncProxy>,
        need_gist: bool,
    ) -> Result<Self, StorageError> {
        let raw = config.effective_api_base().trim().trim_end_matches('/');
        let url = Url::parse(raw).map_err(|err| io(format!("GitHub 接口地址不合法：{err}")))?;
        match url.scheme() {
            "https" => {}
            "http" => {
                if !config.allow_insecure_http {
                    return Err(io(
                        "GitHub 接口地址是 http://，令牌会以明文发送；需要先确认才能使用",
                    ));
                }
            }
            other => {
                return Err(io(format!(
                    "GitHub 接口地址只支持 http(s)，收到 {other}://"
                )))
            }
        }
        let host = url
            .host_str()
            .ok_or_else(|| io("GitHub 接口地址缺少主机名"))?
            .to_owned();
        if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
            return Err(io("GitHub 接口地址里不能带查询串、片段或账号密码"));
        }
        let token = config.token.trim();
        if token.is_empty() {
            return Err(io("请填写 GitHub 令牌"));
        }
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| io("令牌里有不合法的字符（空格、换行或不可见字符），请重新复制"))?;
        // 标成敏感：reqwest / hyper 的调试输出里不会带出它。
        header.set_sensitive(true);
        let gist_id = config.gist_id.trim().to_owned();
        if need_gist && !valid_gist_id(&gist_id) {
            return Err(io(if gist_id.is_empty() {
                "请填写 Gist ID，或者让 Z-SSH 新建一个"
            } else {
                "Gist ID 不合法（只能是字母和数字，从 Gist 地址最后一段复制）"
            }));
        }
        Ok(Self {
            client: build_client(proxy)?,
            base: raw.to_owned(),
            host,
            token: header,
            gist_id,
        })
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .header(AUTHORIZATION, self.token.clone())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
    }

    fn gist_path(&self) -> String {
        format!("/gists/{}", self.gist_id)
    }

    // -- 响应处理 ---------------------------------------------------------

    /// 把非成功的响应翻成存储错误；成功的原样返回。`not_found` 是 404 时的说法
    /// （读和写的 404 意思不一样：写别人的 Gist，GitHub 也回 404）。
    async fn check(
        &self,
        response: reqwest::Response,
        not_found: &str,
    ) -> Result<reqwest::Response, StorageError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if let Some(proxy) = proxy_auth_required(status) {
            return Err(proxy);
        }
        let headers = response.headers().clone();
        let body = read_error_text(response, MAX_META_BYTES).await;
        let message = body_message(&body);
        if let Some(wait) = rate_limit_wait(status, &headers, &message, now_secs()) {
            return Err(StorageError::RateLimited {
                service: SERVICE.to_owned(),
                retry_after_secs: wait,
            });
        }
        Err(match status {
            StatusCode::UNAUTHORIZED => StorageError::AuthFailed(
                "GitHub 令牌已失效，请重新填写令牌（可能已过期、被撤销或填错了）".to_owned(),
            ),
            StatusCode::FORBIDDEN => StorageError::AuthFailed(
                "GitHub 拒绝了请求：令牌没有 Gist 权限（经典令牌要勾选 gist，细粒度令牌要给 Gists 读写权限），\
                 或者令牌所在的组织限制了它"
                    .to_owned(),
            ),
            StatusCode::NOT_FOUND => io(not_found),
            StatusCode::UNPROCESSABLE_ENTITY => {
                io(format!("GitHub 拒绝了请求内容（422）：{message}"))
            }
            other => {
                if matches!(other.as_u16(), 408 | 500 | 502 | 503 | 504) {
                    StorageError::Unavailable(format!("GitHub 返回 {}", other.as_u16()))
                } else {
                    io(format!("GitHub 返回 {}", other.as_u16()))
                }
            }
        })
    }

    // -- 读 ---------------------------------------------------------------

    /// 读 Gist 的当前状态：最新版本号、属主、我们那个文件。
    async fn get_gist(&self) -> Result<GistInfo, StorageError> {
        let response = self
            .request(Method::GET, &self.gist_path())
            .send()
            .await
            .map_err(map_http_error)?;
        let response = self
            .check(response, "找不到这个 Gist：ID 填错了，或者这个令牌看不到它")
            .await?;
        let value = read_json(response, MAX_PACKAGE_BYTES).await?;
        GistInfo::parse(&value)
    }

    /// 取文件的完整内容。接口里的内容被截断时改读 `raw_url`。
    async fn content_of(&self, file: &GistFile) -> Result<Vec<u8>, StorageError> {
        if !file.truncated {
            if let Some(content) = &file.content {
                return Ok(content.as_bytes().to_vec());
            }
        }
        let raw_url = file
            .raw_url
            .as_deref()
            .ok_or_else(|| io("内容被截断了，但 GitHub 没有给出 raw_url"))?;
        // GitHub 报的大小已经超限就不必去下载了。
        if file.size.is_some_and(|size| size > MAX_PACKAGE_BYTES) {
            return Err(io(format!(
                "Gist 里的同步文件太大（超过 {} MiB），不像是 Z-SSH 的同步包，没有下载",
                MAX_PACKAGE_BYTES / (1024 * 1024)
            )));
        }
        let url = Url::parse(raw_url).map_err(|_| io("GitHub 给出的 raw_url 不合法"))?;
        if url.scheme() != "https" && url.scheme() != "http" {
            return Err(io("GitHub 给出的 raw_url 不是 http(s) 地址"));
        }
        let mut request = self
            .client
            .get(url.clone())
            .header("User-Agent", concat!("Z-SSH/", env!("CARGO_PKG_VERSION")));
        // 令牌只发给接口所在的主机。api.github.com 的 raw_url 在 gist.githubusercontent.com，
        // 那里不需要令牌（secret Gist 拿到链接就能读）。
        if url.host_str() == Some(self.host.as_str()) {
            request = request.header(AUTHORIZATION, self.token.clone());
        }
        let response = request.send().await.map_err(map_http_error)?;
        let response = self
            .check(response, "读取完整内容失败：raw_url 返回 404")
            .await?;
        let bytes = read_body_limited(response, MAX_PACKAGE_BYTES, "Gist 里的同步文件").await?;
        if let Some(size) = file.size {
            if bytes.len() as u64 != size {
                return Err(io(format!(
                    "读到的内容不完整（应该 {size} 字节，实际 {} 字节），稍后重试",
                    bytes.len()
                )));
            }
        }
        Ok(bytes)
    }

    // -- 写 ---------------------------------------------------------------

    /// 只改 `zssh-sync.json` 这一个文件（`content` 为 `None` = 删掉这个文件），返回新版本号。
    async fn patch_file(&self, content: Option<&str>) -> Result<Option<String>, StorageError> {
        let file = match content {
            Some(text) => json!({ "content": text }),
            None => Value::Null,
        };
        let body = json!({ "files": { FILE_NAME: file } });
        let response = self
            .request(Method::PATCH, &self.gist_path())
            .json(&body)
            .send()
            .await
            .map_err(map_http_error)?;
        let response = self
            .check(
                response,
                "写入失败：这个 Gist 不属于这个令牌的用户，或者令牌没有 Gist 的写权限",
            )
            .await?;
        let value = read_json(response, MAX_PACKAGE_BYTES).await?;
        Ok(latest_version(&value))
    }

    /// 写之前再读一次：版本号对得上才允许写。
    async fn precheck(&self, expected_version: Option<&str>) -> Result<GistInfo, StorageError> {
        let info = self.get_gist().await?;
        match expected_version {
            // 调用方以为云端还没有同步包：这个 Gist 里必须真的没有。
            None => {
                if info.has_package() {
                    return Err(StorageError::Conflict);
                }
            }
            Some(expected) => {
                if info.version.as_deref() != Some(expected) {
                    return Err(StorageError::Conflict);
                }
            }
        }
        Ok(info)
    }

    /// 写完再读一次：最新版本必须就是刚写的那一版，否则说明写完之后又有人写了。
    async fn confirm(&self, written: &Option<String>) -> Result<String, StorageError> {
        let after = self.get_gist().await?;
        let latest = after
            .version
            .ok_or_else(|| io("GitHub 没有返回 Gist 的版本信息"))?;
        if let Some(written) = written {
            if &latest != written {
                return Err(StorageError::Conflict);
            }
        }
        Ok(latest)
    }

    /// 新建一个 secret Gist，只放一个占位文件，返回 Gist ID。
    pub async fn create_gist(&self) -> Result<String, StorageError> {
        self.create_gist_with_description(GIST_DESCRIPTION).await
    }

    pub async fn create_gist_with_description(
        &self,
        description: &str,
    ) -> Result<String, StorageError> {
        let body = json!({
            "description": description,
            "public": false,
            "files": { FILE_NAME: { "content": PLACEHOLDER } },
        });
        let response = self
            .request(Method::POST, "/gists")
            .json(&body)
            .send()
            .await
            .map_err(map_http_error)?;
        let response = self
            .check(
                response,
                "新建失败：令牌没有创建 Gist 的权限（经典令牌要勾选 gist，细粒度令牌要给 Gists 读写权限）",
            )
            .await?;
        let value = read_json(response, MAX_META_BYTES).await?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_gist_id(id))
            .ok_or_else(|| io("GitHub 没有返回新 Gist 的 ID"))?;
        Ok(id.to_owned())
    }

    /// 删掉整个 Gist（只给“测试连接”的临时 Gist 和测试清理用，同步本身从不删 Gist）。
    pub async fn delete_gist(&self, id: &str) -> Result<(), StorageError> {
        if !valid_gist_id(id) {
            return Err(io("Gist ID 不合法"));
        }
        let response = self
            .request(Method::DELETE, &format!("/gists/{id}"))
            .send()
            .await
            .map_err(map_http_error)?;
        self.check(
            response,
            "删除失败：找不到这个 Gist，或者它不属于这个令牌的用户",
        )
        .await?;
        Ok(())
    }

    /// 令牌属于谁，以及经典令牌的权限范围（细粒度令牌没有这个响应头）。
    async fn whoami(&self) -> Result<(String, Option<String>), StorageError> {
        let response = self
            .request(Method::GET, "/user")
            .send()
            .await
            .map_err(map_http_error)?;
        let response = self
            .check(response, "GitHub 找不到这个用户（接口地址对吗？）")
            .await?;
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let value = read_json(response, MAX_META_BYTES).await?;
        let login = value
            .get("login")
            .and_then(Value::as_str)
            .ok_or_else(|| io("GitHub 没有返回用户信息"))?
            .to_owned();
        Ok((login, scopes))
    }
}

impl SyncStorage for GithubApi {
    fn read<'a>(&'a self) -> BoxFuture<'a, Result<Option<StoredObject>, StorageError>> {
        Box::pin(async move {
            let info = self.get_gist().await?;
            if info.gist_truncated && info.file.is_none() {
                return Err(io(
                    "这个 Gist 里的文件太多（接口只返回前 300 个），找不到同步文件，请换一个 Gist",
                ));
            }
            let Some(file) = info.package_file() else {
                return Ok(None);
            };
            let version = info
                .version
                .clone()
                .ok_or_else(|| io("GitHub 没有返回 Gist 的版本信息"))?;
            let bytes = self.content_of(file).await?;
            Ok(Some(StoredObject { bytes, version }))
        })
    }

    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, StorageError>> {
        Box::pin(async move {
            let text = std::str::from_utf8(bytes).map_err(|_| io("同步包不是 UTF-8 文本"))?;
            self.precheck(expected_version).await?;
            let written = self.patch_file(Some(text)).await?;
            self.confirm(&written).await
        })
    }

    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            let info = self.precheck(expected_version).await?;
            if info.file.is_none() {
                return Ok(());
            }
            let written = self.patch_file(None).await?;
            self.confirm(&written).await.map(|_| ())
        })
    }
}

// ---------------------------------------------------------------------------
// 解析

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GistFile {
    pub content: Option<String>,
    pub truncated: bool,
    pub raw_url: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GistInfo {
    /// `history` 里最新的 `version`。
    pub version: Option<String>,
    pub owner: Option<String>,
    /// 我们的文件（可能是占位内容）。
    pub file: Option<GistFile>,
    /// 整个 Gist 的文件列表被截断了（超过 300 个文件）。
    pub gist_truncated: bool,
}

pub(crate) fn latest_version(value: &Value) -> Option<String> {
    value
        .get("history")?
        .as_array()?
        .first()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

impl GistInfo {
    pub(crate) fn parse(value: &Value) -> Result<Self, StorageError> {
        if !value.is_object() {
            return Err(io("GitHub 返回的 Gist 格式不对"));
        }
        let file = value
            .get("files")
            .and_then(|files| files.get(FILE_NAME))
            .filter(|file| file.is_object())
            .map(|file| GistFile {
                content: file
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                truncated: file
                    .get("truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                raw_url: file
                    .get("raw_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                size: file.get("size").and_then(Value::as_u64),
            });
        Ok(Self {
            version: latest_version(value),
            owner: value
                .get("owner")
                .and_then(|owner| owner.get("login"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            file,
            gist_truncated: value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// 文件是不是我们的占位内容（新建 Gist 时放的，不是真的同步包）。
    fn is_placeholder(&self) -> bool {
        self.file
            .as_ref()
            .is_some_and(|file| !file.truncated && file.content.as_deref() == Some(PLACEHOLDER))
    }

    /// 里面有没有真的同步包。
    pub(crate) fn has_package(&self) -> bool {
        self.file.is_some() && !self.is_placeholder()
    }

    fn package_file(&self) -> Option<&GistFile> {
        if self.has_package() {
            self.file.as_ref()
        } else {
            None
        }
    }
}

/// 有界地读 JSON 响应。Gist 的接口响应里内嵌着同步包本身（不超过 GitHub 的截断
/// 长度），所以读 Gist 用同步包的上限，其余小响应用元数据上限。
async fn read_json(response: reqwest::Response, limit: u64) -> Result<Value, StorageError> {
    let bytes = read_body_limited(response, limit, "GitHub 的响应").await?;
    serde_json::from_slice(&bytes).map_err(|_| io("GitHub 返回的内容不是有效的 JSON"))
}

fn body_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

/// 这个响应是不是限流？是的话要等多少秒。
///
/// 判断（GitHub 文档“Rate limits for the REST API”）：状态码 403 或 429，并且满足其一——
/// 有 `Retry-After`、`x-ratelimit-remaining` 是 0、错误信息里说了 rate limit。
/// 等待时间：优先 `Retry-After`（秒）；否则 `x-ratelimit-reset`（到期的 Unix 时间）减去现在；
/// 都没有就等 60 秒。上限 2 小时，下限 1 秒。
pub(crate) fn rate_limit_wait(
    status: StatusCode,
    headers: &HeaderMap,
    message: &str,
    now: i64,
) -> Option<u64> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let retry_after = header_u64(headers, "retry-after");
    let exhausted = header_u64(headers, "x-ratelimit-remaining") == Some(0);
    let said_so = message.to_ascii_lowercase().contains("rate limit");
    if !(status == StatusCode::TOO_MANY_REQUESTS || retry_after.is_some() || exhausted || said_so) {
        return None;
    }
    let wait = if let Some(secs) = retry_after {
        secs
    } else if let Some(reset) = header_u64(headers, "x-ratelimit-reset") {
        (reset as i64 - now).max(0) as u64
    } else {
        DEFAULT_WAIT_SECS
    };
    Some(wait.clamp(1, MAX_WAIT_SECS))
}

// ---------------------------------------------------------------------------
// 给设置页用的入口

/// 打开读写同步包用的存储（需要 Gist ID）。并发控制是“写前再读”，所以不看检测结果。
pub fn open_storage(
    config: &GithubConfig,
    proxy: Option<&SyncProxy>,
) -> Result<GithubApi, StorageError> {
    GithubApi::new(config, proxy, true)
}

/// 新建一个 secret Gist，返回 ID。
pub async fn create_gist(
    config: &GithubConfig,
    proxy: Option<&SyncProxy>,
) -> Result<String, StorageError> {
    GithubApi::new(config, proxy, false)?.create_gist().await
}

/// 新建一个 secret Gist 并指定描述（联网测试用随机描述，方便认出是测试建的）。
pub async fn create_gist_with_description(
    config: &GithubConfig,
    proxy: Option<&SyncProxy>,
    description: &str,
) -> Result<String, StorageError> {
    GithubApi::new(config, proxy, false)?
        .create_gist_with_description(description)
        .await
}

/// 删掉一个 Gist（测试清理用；同步本身从不删 Gist）。
pub async fn delete_gist(
    config: &GithubConfig,
    proxy: Option<&SyncProxy>,
    id: &str,
) -> Result<(), StorageError> {
    GithubApi::new(config, proxy, false)?.delete_gist(id).await
}

fn unsupported_concurrency() -> ConcurrencyProbe {
    ConcurrencyProbe {
        mode: ConcurrencyMode::ReadBeforeWrite,
        notes: vec![
            "GitHub Gist 的修改接口（PATCH）不支持 If-Match 这类条件写入，只能写之前再读一次、比较最新的版本号"
                .to_owned(),
        ],
    }
}

fn failed(message: impl Into<String>) -> Check {
    Check {
        ok: false,
        message: message.into(),
    }
}

fn passed(message: impl Into<String>) -> Check {
    Check {
        ok: true,
        message: message.into(),
    }
}

/// 测试连接：令牌有效 → 有 Gist 权限 → Gist 存在且属于这个令牌的用户 → 写权限 → 并发保护。
///
/// 写权限：经典令牌看响应头 `X-OAuth-Scopes` 里有没有 `gist`（不用写任何东西）；细粒度令牌
/// 没有这个头，只能真的试一次——新建一个临时的 secret Gist 再立刻删掉（不碰已有的 Gist）。
pub async fn probe(
    config: &GithubConfig,
    proxy: Option<&SyncProxy>,
) -> Result<ConnectionProbe, StorageError> {
    let api = GithubApi::new(config, proxy, true)?;
    let skipped = || failed("前面的检查没通过，没有继续");

    // 1. 令牌有效吗
    let (login, scopes) = match api.whoami().await {
        Ok(found) => found,
        Err(err) => {
            return Ok(ConnectionProbe {
                read: failed(err.to_string()),
                write: skipped(),
                concurrency: None,
                has_existing_data: false,
            })
        }
    };
    if let Some(scopes) = &scopes {
        let has_gist = scopes.split(',').any(|scope| scope.trim() == "gist");
        if !has_gist {
            return Ok(ConnectionProbe {
                read: failed(format!(
                    "令牌有效，但没有 gist 权限（它的权限：{}）。请重新创建令牌并勾选 gist",
                    if scopes.trim().is_empty() {
                        "无"
                    } else {
                        scopes.trim()
                    }
                )),
                write: skipped(),
                concurrency: None,
                has_existing_data: false,
            });
        }
    }

    // 2. Gist 在不在、是不是自己的
    let info = match api.get_gist().await {
        Ok(info) => info,
        Err(err) => {
            return Ok(ConnectionProbe {
                read: failed(err.to_string()),
                write: skipped(),
                concurrency: None,
                has_existing_data: false,
            })
        }
    };
    if info.gist_truncated && info.file.is_none() {
        return Ok(ConnectionProbe {
            read: failed("这个 Gist 里的文件太多（接口只返回前 300 个），请换一个 Gist"),
            write: skipped(),
            concurrency: None,
            has_existing_data: false,
        });
    }
    let has_existing_data = info.has_package();
    let read = passed(if has_existing_data {
        "令牌有效，Gist 存在，里面已有同步数据"
    } else {
        "令牌有效，Gist 存在，里面还没有同步数据"
    });
    if info.owner.as_deref().is_some_and(|owner| owner != login) {
        return Ok(ConnectionProbe {
            read,
            write: failed(
                "这个 Gist 不属于这个令牌的用户，只能读、写不进去。请填自己名下的 Gist ID，或者让 Z-SSH 新建一个",
            ),
            concurrency: Some(unsupported_concurrency()),
            has_existing_data,
        });
    }

    // 3. 写权限
    let write = match &scopes {
        Some(_) => passed("令牌有 gist 权限（经典令牌）"),
        None => match probe_write(&api).await {
            Ok(note) => passed(note),
            Err(err) => failed(format!("写权限检查没通过：{err}")),
        },
    };
    Ok(ConnectionProbe {
        read,
        write,
        concurrency: Some(unsupported_concurrency()),
        has_existing_data,
    })
}

/// 细粒度令牌的写权限：新建一个临时 secret Gist 再删掉。
async fn probe_write(api: &GithubApi) -> Result<String, StorageError> {
    let id = api.create_gist().await?;
    match api.delete_gist(&id).await {
        Ok(()) => Ok("可以创建和删除 Gist（用一个临时 Gist 验证过，已经删掉）".to_owned()),
        Err(err) => Ok(format!(
            "可以创建 Gist，但测试用的临时 Gist 没能删掉（{err}）。可以到 GitHub 上手动删除，Gist ID：{id}"
        )),
    }
}

#[cfg(test)]
mod tests;
