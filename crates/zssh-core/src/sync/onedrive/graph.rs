// Adapted from Netcatty infrastructure/services/adapters/OneDriveAdapter.ts (special/approot addressing)
//! OneDrive（Microsoft Graph）上的同步包存储。
//!
//! 同步包放在应用专用文件夹里：`/me/drive/special/approot:/<固定文件名>`（OneDrive 里的
//! `Apps/<应用名>`，只能访问这个文件夹，看不到用户的其他文件）。
//!
//! 依据的官方文档：
//! - 应用专用文件夹：<https://learn.microsoft.com/en-us/onedrive/developer/rest-api/concepts/special-folders-appfolder>
//! - 简单上传 `PUT .../content`：<https://learn.microsoft.com/en-us/graph/api/driveitem-put-content>
//!   最大 250 MB；**文档没有写它支持 `If-Match`**，所以写入不用它
//! - 上传会话：<https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession>
//!   **明确支持 `If-Match` / `If-None-Match`**（对不上返回 412）；`@microsoft.graph.conflictBehavior`
//!   默认 `fail`（已存在返回 409 `nameAlreadyExists`）；分片大小必须是 320 KiB 的整数倍、单个请求小于
//!   60 MiB、推荐 5～10 MiB；往 `uploadUrl` 上传**不要带 `Authorization`**（会 401）
//! - 删除 `DELETE`：<https://learn.microsoft.com/en-us/graph/api/driveitem-delete>，`If-Match` 对不上返回 412
//! - 限流：<https://learn.microsoft.com/en-us/graph/throttling>，429（OneDrive 也会用 503）加 `Retry-After`
//!
//! 并发控制（和 WebDAV / S3 同一套 [`ConditionalStore`]）：
//! - 版本号 = 文件的 `eTag`；更新 = `createUploadSession` 带 `If-Match: <eTag>`，412 → 云端被别人改了；
//! - 首次上传（云端应该还没有）：`If-None-Match: *` 在官方文档里**没有出处**，改用
//!   `conflictBehavior=fail`，已存在 → 409 → 同样当作“被别人改了”；
//! - 残余风险：会话创建和最后一片提交之间有几百毫秒的窗口，别人在这中间改了文件仍会被覆盖
//!   （`If-Match` 在创建会话时检查）。“测试连接”会真的验证这些条件被服务器执行，不执行就退回“写前再读”；
//! - 读取：先取元数据拿 `eTag`、再取内容。内容可能比 `eTag` 新（中间有人写了），这时下次带着旧 `eTag`
//!   写会 412、重读，是安全的方向。

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};

use super::oauth::{Endpoints, REAUTH_MESSAGE};
use super::tokens::{TokenManager, TokenStore};
use crate::ssh::BoxFuture;
use crate::sync::net::{
    build_client, map_http_error, proxy_auth_required, read_body_limited, read_error_text,
    SyncProxy, MAX_META_BYTES, MAX_PACKAGE_BYTES,
};
use crate::sync::remote::{
    probe_connection, ConcurrencyMode, Condition, ConditionalStore, ConnectionProbe, RawObject,
    RawObjectStore, RawPut,
};
use crate::sync::storage::StorageError;

/// 云端同步包的文件名（和 WebDAV 一样）。
pub const OBJECT_NAME: &str = "zssh-sync.json";
const SERVICE: &str = "OneDrive";
/// 上传分片大小：5 MiB = 16 × 320 KiB（官方要求 320 KiB 的整数倍，推荐 5～10 MiB）。
pub const FRAGMENT_BYTES: usize = 16 * 320 * 1024;
/// 限流等待的上限；响应里没给 `Retry-After` 时的默认等待。
const MAX_WAIT_SECS: u64 = 2 * 3600;
const DEFAULT_WAIT_SECS: u64 = 60;
/// 往上传会话传一片的最长时间（5 MiB，慢速网络也留足）。
const FRAGMENT_TIMEOUT: Duration = Duration::from_secs(120);

fn io(message: impl Into<String>) -> StorageError {
    StorageError::Io(message.into())
}

/// 路径片段的百分号编码（文件名只会是我们自己定的几种，但不拼未编码的字符串进地址）。
fn encode_segment(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct OneDriveRaw<'a> {
    client: Client,
    tokens: TokenManager<'a>,
    object: String,
}

impl<'a> OneDriveRaw<'a> {
    pub fn new(
        endpoints: Endpoints,
        client_id: &str,
        proxy: Option<&SyncProxy>,
        tokens: Arc<dyn TokenStore + 'a>,
    ) -> Result<Self, StorageError> {
        let client = build_client(proxy)?;
        Ok(Self {
            tokens: TokenManager::new(client.clone(), endpoints, client_id.to_owned(), tokens),
            client,
            object: OBJECT_NAME.to_owned(),
        })
    }

    /// `…/special/approot:/<文件名>`，后面再接 `:/content` 之类的后缀。
    fn item_url(&self, suffix: &str) -> String {
        let base = format!(
            "{}/me/drive/special/approot:/{}",
            self.tokens.endpoints().graph,
            encode_segment(&self.object)
        );
        if suffix.is_empty() {
            base
        } else {
            format!("{base}:{suffix}")
        }
    }

    /// 带访问令牌发一个 Graph 请求。收到 401 说明令牌被服务器拒绝（比如被提前作废）：
    /// 强制刷新一次再重发，还是 401 才算登录失效。
    async fn send(
        &self,
        build: impl Fn(&Client, &str) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, StorageError> {
        let token = self.tokens.access_token().await?;
        let response = build(&self.client, &token)
            .send()
            .await
            .map_err(map_http_error)?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let token = self.tokens.force_refresh(&token).await?;
        build(&self.client, &token)
            .send()
            .await
            .map_err(map_http_error)
    }

    // -- 响应处理 ------------------------------------------------------

    /// 把非成功的响应翻成存储错误。`ok` 里列出调用方自己处理的状态码（比如 404、412）。
    async fn check(
        response: reqwest::Response,
        action: &str,
        handled: &[StatusCode],
    ) -> Result<reqwest::Response, StorageError> {
        let status = response.status();
        if status.is_success() || handled.contains(&status) {
            return Ok(response);
        }
        Err(error_for(response, action).await)
    }

    async fn json_body(response: reqwest::Response, what: &str) -> Result<Value, StorageError> {
        let bytes = read_body_limited(response, MAX_META_BYTES, what).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| io(format!("OneDrive 返回的{what}不是有效的 JSON")))
    }

    // -- 读 ------------------------------------------------------------

    async fn metadata(&self) -> Result<Option<Metadata>, StorageError> {
        let url = self.item_url("");
        let response = self
            .send(|client, token| {
                client
                    .get(&url)
                    .bearer_auth(token)
                    .header("Accept", "application/json")
            })
            .await?;
        let response = Self::check(response, "读取", &[StatusCode::NOT_FOUND]).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let value = Self::json_body(response, "文件信息").await?;
        if value.get("folder").is_some() {
            return Err(io(format!(
                "OneDrive 的应用文件夹里有一个叫 {} 的文件夹，Z-SSH 没法用它放同步包，请改名或删除它",
                self.object
            )));
        }
        let etag = value
            .get("eTag")
            .and_then(Value::as_str)
            .filter(|etag| !etag.is_empty())
            .map(str::to_owned);
        Ok(Some(Metadata {
            etag,
            size: value.get("size").and_then(Value::as_u64),
        }))
    }

    // -- 写 ------------------------------------------------------------

    /// 创建上传会话。返回 `Ok(Err(..))` 表示条件不满足（412 / 409），调用方转成 [`RawPut::PreconditionFailed`]。
    async fn create_session(
        &self,
        condition: &Condition,
    ) -> Result<Result<String, ()>, StorageError> {
        let url = self.item_url("/createUploadSession");
        let behavior = match condition {
            // 首次上传：云端不应该有这个文件，已存在就失败（409）。
            Condition::IfNoneMatchAny => "fail",
            Condition::IfMatch(_) | Condition::None => "replace",
        };
        // 不带 fileSize：个人版 OneDrive 见到它就回 400 invalidRequest（2026-10-21 对真服务实测，
        // 官方文档却写"只在个人版可用"）。文件总长由上传分片的 Content-Range 告诉服务器。
        let body = json!({
            "item": {
                "@microsoft.graph.conflictBehavior": behavior,
                "name": self.object,
            }
        });
        let response = self
            .send(|client, token| {
                let mut request = client
                    .post(&url)
                    .bearer_auth(token)
                    .header("Accept", "application/json")
                    .json(&body);
                if let Condition::IfMatch(etag) = condition {
                    request = request.header("If-Match", etag.as_str());
                }
                request
            })
            .await?;
        let response = Self::check(
            response,
            "创建上传会话",
            &[StatusCode::PRECONDITION_FAILED, StatusCode::CONFLICT],
        )
        .await?;
        if matches!(
            response.status(),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT
        ) {
            return Ok(Err(()));
        }
        let value = Self::json_body(response, "上传会话").await?;
        let upload_url = value
            .get("uploadUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| io("OneDrive 没有返回上传地址"))?;
        let parsed =
            reqwest::Url::parse(upload_url).map_err(|_| io("OneDrive 返回的上传地址不合法"))?;
        // 上传地址是服务器给的，不能被引到 http:// 上（上传内容虽然是密文，仍不走明文）。
        if parsed.scheme() != "https" && !is_loopback(&parsed) {
            return Err(io("OneDrive 返回的上传地址不是 https"));
        }
        Ok(Ok(upload_url.to_owned()))
    }

    /// 把整个同步包按分片传给上传会话。**不带 `Authorization`**（官方文档：带了可能 401）。
    /// 中途失败尽力取消会话（`DELETE`），免得云端留着半截的临时数据。
    async fn upload_fragments(
        &self,
        upload_url: &str,
        bytes: &[u8],
    ) -> Result<Result<Option<String>, ()>, StorageError> {
        let total = bytes.len();
        let mut offset = 0usize;
        let outcome = loop {
            let end = (offset + FRAGMENT_BYTES).min(total);
            let result = self
                .client
                .put(upload_url)
                .timeout(FRAGMENT_TIMEOUT)
                .header(
                    "Content-Range",
                    format!("bytes {offset}-{}/{total}", end - 1),
                )
                .header("Content-Type", "application/octet-stream")
                .body(bytes[offset..end].to_vec())
                .send()
                .await
                .map_err(map_http_error);
            let response = match result {
                Ok(response) => response,
                Err(err) => break Err(err),
            };
            let status = response.status();
            if end == total {
                // 最后一片：成功返回 driveItem；条件在提交时才不满足（别人刚创建了同名文件）是 409 / 412。
                if matches!(
                    status,
                    StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED
                ) {
                    break Ok(Err(()));
                }
                match Self::check(response, "上传", &[]).await {
                    Ok(response) => {
                        let value = Self::json_body(response, "上传结果").await;
                        break value.map(|value| {
                            Ok(value
                                .get("eTag")
                                .and_then(Value::as_str)
                                .filter(|etag| !etag.is_empty())
                                .map(str::to_owned))
                        });
                    }
                    Err(err) => break Err(err),
                }
            }
            // 中间的分片：202 Accepted，继续下一片。
            if status != StatusCode::ACCEPTED && !status.is_success() {
                break Err(error_for(response, "上传").await);
            }
            offset = end;
        };
        if !matches!(outcome, Ok(Ok(_))) {
            self.cancel_session(upload_url).await;
        }
        outcome
    }

    async fn cancel_session(&self, upload_url: &str) {
        // 取消不是必须的（会话到期会自己清理），失败忽略。
        let _ = self
            .client
            .delete(upload_url)
            .timeout(Duration::from_secs(15))
            .send()
            .await;
    }
}

struct Metadata {
    etag: Option<String>,
    size: Option<u64>,
}

fn is_loopback(url: &reqwest::Url) -> bool {
    url.host_str()
        .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"))
}

/// `Retry-After`（秒）。只认整数秒；HTTP 日期写法很少见，按没给处理（用默认等待）。
fn retry_after_secs(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Graph 错误响应体里的 `error.code`（`quotaLimitReached`、`nameAlreadyExists` 之类，不含用户数据）。
fn graph_error_code(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.get("code")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn error_for(response: reqwest::Response, action: &str) -> StorageError {
    let status = response.status();
    if let Some(proxy) = proxy_auth_required(status) {
        return proxy;
    }
    let headers = response.headers().clone();
    let body = read_error_text(response, MAX_META_BYTES).await;
    status_error(status, &headers, &graph_error_code(&body), action)
}

/// 状态码 → 存储错误。单独成函数，方便不连网测试每一种。
fn status_error(status: StatusCode, headers: &HeaderMap, code: &str, action: &str) -> StorageError {
    let retry = retry_after_secs(headers);
    match status.as_u16() {
        // 限流：429，OneDrive 也会用 503 加 Retry-After。不能立刻重试，引擎直接放弃这次同步。
        429 => StorageError::RateLimited {
            service: SERVICE.to_owned(),
            retry_after_secs: retry.unwrap_or(DEFAULT_WAIT_SECS).clamp(1, MAX_WAIT_SECS),
        },
        503 if retry.is_some() => StorageError::RateLimited {
            service: SERVICE.to_owned(),
            retry_after_secs: retry.unwrap_or(DEFAULT_WAIT_SECS).clamp(1, MAX_WAIT_SECS),
        },
        401 => StorageError::AuthFailed(REAUTH_MESSAGE.to_owned()),
        403 => StorageError::AuthFailed(
            "OneDrive 拒绝了请求（没有权限）。授权可能被收回了，请重新登录".to_owned(),
        ),
        404 => io(format!("OneDrive {action}失败：找不到应用文件夹或文件")),
        423 => io(format!("OneDrive {action}失败：文件被锁定，稍后重试")),
        507 => io("OneDrive 空间不足（507），请清理 OneDrive 的空间后重试".to_owned()),
        408 | 500 | 502 | 503 | 504 => {
            StorageError::Unavailable(format!("OneDrive 返回 {}", status.as_u16()))
        }
        other => {
            let detail = if code.is_empty() {
                String::new()
            } else {
                format!("（{code}）")
            };
            io(format!("OneDrive {action}失败：服务器返回 {other}{detail}"))
        }
    }
}

impl RawObjectStore for OneDriveRaw<'_> {
    fn get<'a>(&'a self) -> BoxFuture<'a, Result<Option<RawObject>, StorageError>> {
        Box::pin(async move {
            let Some(meta) = self.metadata().await? else {
                return Ok(None);
            };
            if meta.size.is_some_and(|size| size > MAX_PACKAGE_BYTES) {
                return Err(io(
                    "OneDrive 上的同步文件超过 64 MiB 上限，不像是 Z-SSH 的同步包，没有下载",
                ));
            }
            // /content 会 302 到一个带临时凭据的下载地址。reqwest 跨主机重定向时会去掉
            // Authorization，令牌不会发给下载服务器。
            let url = self.item_url("/content");
            let response = self
                .send(|client, token| client.get(&url).bearer_auth(token))
                .await?;
            let response = Self::check(response, "下载", &[StatusCode::NOT_FOUND]).await?;
            if response.status() == StatusCode::NOT_FOUND {
                // 元数据和内容之间文件被删了：当作没有。
                return Ok(None);
            }
            let bytes =
                read_body_limited(response, MAX_PACKAGE_BYTES, "OneDrive 上的同步包").await?;
            Ok(Some(RawObject {
                bytes,
                etag: meta.etag,
            }))
        })
    }

    fn put<'a>(
        &'a self,
        bytes: &'a [u8],
        condition: &'a Condition,
    ) -> BoxFuture<'a, Result<RawPut, StorageError>> {
        Box::pin(async move {
            if bytes.is_empty() {
                return Err(io("同步包是空的，没有上传"));
            }
            let Ok(upload_url) = self.create_session(condition).await? else {
                return Ok(RawPut::PreconditionFailed);
            };
            match self.upload_fragments(&upload_url, bytes).await? {
                Ok(etag) => Ok(RawPut::Written { etag }),
                Err(()) => Ok(RawPut::PreconditionFailed),
            }
        })
    }

    fn delete<'a>(&'a self, condition: &'a Condition) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(async move {
            let url = self.item_url("");
            let response = self
                .send(|client, token| {
                    let mut request = client.request(Method::DELETE, &url).bearer_auth(token);
                    if let Condition::IfMatch(etag) = condition {
                        request = request.header("If-Match", etag.as_str());
                    }
                    request
                })
                .await?;
            let response = Self::check(
                response,
                "删除",
                &[StatusCode::NOT_FOUND, StatusCode::PRECONDITION_FAILED],
            )
            .await?;
            // 已经不存在也算删掉了；条件不满足（有人改过）= false。
            Ok(response.status() != StatusCode::PRECONDITION_FAILED)
        })
    }

    fn sibling(&self, object_name: &str) -> Box<dyn RawObjectStore + '_> {
        Box::new(Self {
            client: self.client.clone(),
            tokens: self.tokens.clone(),
            object: object_name.to_owned(),
        })
    }
}

/// 创建引擎用的存储。`mode` 是上次“测试连接”的检测结果；没检测过用 `ReadBeforeWrite`。
pub fn open_storage<'a>(
    endpoints: Endpoints,
    client_id: &str,
    proxy: Option<&SyncProxy>,
    mode: ConcurrencyMode,
    tokens: Arc<dyn TokenStore + 'a>,
) -> Result<ConditionalStore<OneDriveRaw<'a>>, StorageError> {
    Ok(ConditionalStore::new(
        OneDriveRaw::new(endpoints, client_id, proxy, tokens)?,
        mode,
    ))
}

/// 测试连接：登录有效、能读、能写（写一个临时文件再删掉）、`If-Match` 等条件是否真的被执行。
pub async fn probe(
    endpoints: Endpoints,
    client_id: &str,
    proxy: Option<&SyncProxy>,
    tokens: Arc<dyn TokenStore + '_>,
) -> Result<ConnectionProbe, StorageError> {
    let raw = OneDriveRaw::new(endpoints, client_id, proxy, tokens)?;
    Ok(probe_connection(&raw).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn throttling_uses_retry_after_and_is_clamped() {
        for (status, retry, expected) in [
            (429, Some("30"), 30),
            (503, Some("120"), 120),
            (429, None, DEFAULT_WAIT_SECS),
            (429, Some("0"), 1),
            (429, Some("999999"), MAX_WAIT_SECS),
            (
                429,
                Some("Wed, 21 Oct 2026 07:28:00 GMT"),
                DEFAULT_WAIT_SECS,
            ),
        ] {
            let map = match retry {
                Some(value) => headers(&[("retry-after", value)]),
                None => HeaderMap::new(),
            };
            let err = status_error(StatusCode::from_u16(status).unwrap(), &map, "", "写入");
            assert_eq!(
                err,
                StorageError::RateLimited {
                    service: "OneDrive".to_owned(),
                    retry_after_secs: expected
                },
                "{status} {retry:?}"
            );
        }
    }

    #[test]
    fn a_503_without_retry_after_is_just_unavailable() {
        let err = status_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &HeaderMap::new(),
            "",
            "读取",
        );
        assert!(matches!(err, StorageError::Unavailable(_)), "{err:?}");
        for code in [500, 502, 504, 408] {
            let err = status_error(
                StatusCode::from_u16(code).unwrap(),
                &HeaderMap::new(),
                "",
                "读取",
            );
            assert!(matches!(err, StorageError::Unavailable(_)), "{code}");
        }
    }

    #[test]
    fn auth_and_quota_errors_have_readable_messages() {
        let none = HeaderMap::new();
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED, &none, "", "读取"),
            StorageError::AuthFailed(REAUTH_MESSAGE.to_owned())
        );
        assert!(matches!(
            status_error(StatusCode::FORBIDDEN, &none, "accessDenied", "写入"),
            StorageError::AuthFailed(m) if m.contains("重新登录")
        ));
        let quota = status_error(
            StatusCode::INSUFFICIENT_STORAGE,
            &none,
            "quotaLimitReached",
            "写入",
        );
        let StorageError::Io(message) = quota else {
            panic!("507 应该是普通错误");
        };
        assert!(
            message.contains("空间不足") && message.contains("507"),
            "{message}"
        );
        let other = status_error(StatusCode::BAD_REQUEST, &none, "invalidRequest", "写入");
        assert_eq!(
            other,
            StorageError::Io("OneDrive 写入失败：服务器返回 400（invalidRequest）".to_owned())
        );
    }

    #[test]
    fn graph_error_codes_are_read_from_the_body() {
        assert_eq!(
            graph_error_code(r#"{"error":{"code":"nameAlreadyExists","message":"x"}}"#),
            "nameAlreadyExists"
        );
        assert_eq!(graph_error_code("not json"), "");
        assert_eq!(graph_error_code("{}"), "");
    }

    #[test]
    fn fragments_are_a_multiple_of_320_kib() {
        assert_eq!(FRAGMENT_BYTES % (320 * 1024), 0);
        assert_eq!(FRAGMENT_BYTES, 5 * 1024 * 1024);
    }

    #[test]
    fn file_names_are_percent_encoded_in_urls() {
        assert_eq!(encode_segment("zssh-sync.json"), "zssh-sync.json");
        assert_eq!(encode_segment("a b/c:d"), "a%20b%2Fc%3Ad");
    }
}
