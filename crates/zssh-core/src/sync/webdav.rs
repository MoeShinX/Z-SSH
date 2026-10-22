//! WebDAV 存储：`GET` 读、`PUT` 写（`If-Match` / `If-None-Match` 做并发保护）、
//! 目录不存在时 `MKCOL` 逐级创建。认证只有 Basic。
//!
//! 各家实现差异很大（Nextcloud、坚果云、Alist、群晖……），所以：
//! - 是否真的支持并发保护不靠猜，连接时用 [`probe`] 探测；
//! - 不支持时走 [`ConcurrencyMode::ReadBeforeWrite`]；
//! - 地址是 `http://` 时必须由调用方明确确认（`allow_insecure_http`），密码是明文发的。

use reqwest::{Client, Method, StatusCode, Url};

use super::net::{
    build_client, map_http_error, proxy_auth_required, read_body_limited, retryable_status,
    SyncProxy, MAX_PACKAGE_BYTES,
};
use super::remote::{
    probe_connection, ConcurrencyMode, Condition, ConditionalStore, ConnectionProbe, RawObject,
    RawObjectStore, RawPut,
};
use super::storage::StorageError;
use crate::ssh::BoxFuture;

/// 云端同步包的文件名。
pub const OBJECT_NAME: &str = "zssh-sync.json";

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebDavConfig {
    /// 服务地址，例如 `https://dav.example.com/remote.php/dav/files/me/`。
    pub url: String,
    /// 地址下面放同步包的子目录（可以多级，用 `/` 分隔）；空 = 直接放在地址下。
    #[serde(default)]
    pub directory: String,
    pub username: String,
    pub password: String,
    /// 用户确认过“密码会以明文发送”之后才允许 `http://`。
    #[serde(default)]
    pub allow_insecure_http: bool,
}

impl std::fmt::Debug for WebDavConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavConfig")
            .field("url", &self.url)
            .field("directory", &self.directory)
            .field("username", &self.username)
            .finish()
    }
}

impl WebDavConfig {
    /// 地址是不是明文 `http://`（设置页要警告并让用户确认）。
    pub fn is_plain_http(&self) -> bool {
        Url::parse(self.url.trim())
            .map(|url| url.scheme() == "http")
            .unwrap_or(false)
    }
}

pub struct WebDavRaw {
    client: Client,
    /// 目录集合的地址（以 `/` 结尾）。
    base: Url,
    /// base 下面要逐级创建的子目录。
    dirs: Vec<String>,
    object: String,
    username: String,
    password: String,
}

impl WebDavRaw {
    pub fn new(config: &WebDavConfig, proxy: Option<&SyncProxy>) -> Result<Self, StorageError> {
        let mut base = Url::parse(config.url.trim())
            .map_err(|err| StorageError::Io(format!("WebDAV 地址不合法：{err}")))?;
        match base.scheme() {
            "https" => {}
            "http" => {
                if !config.allow_insecure_http {
                    return Err(StorageError::Io(
                        "WebDAV 地址是 http://，密码会以明文发送；需要先确认才能使用".to_owned(),
                    ));
                }
            }
            other => {
                return Err(StorageError::Io(format!(
                    "WebDAV 地址只支持 http(s)，收到 {other}://"
                )))
            }
        }
        if base.host_str().is_none() {
            return Err(StorageError::Io("WebDAV 地址缺少主机名".to_owned()));
        }
        // 地址里不允许夹带凭据：用户名密码走 Basic 头，地址会出现在日志 / 错误里。
        base.set_username("")
            .map_err(|_| StorageError::Io("WebDAV 地址不合法".to_owned()))?;
        let _ = base.set_password(None);
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let dirs = config
            .directory
            .split('/')
            .map(str::trim)
            .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
            .map(str::to_owned)
            .collect();
        Ok(Self {
            client: build_client(proxy)?,
            base,
            dirs,
            object: OBJECT_NAME.to_owned(),
            username: config.username.clone(),
            password: config.password.clone(),
        })
    }

    fn url_for(&self, dir_depth: usize, object: Option<&str>) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty();
            for dir in self.dirs.iter().take(dir_depth) {
                segments.push(dir);
            }
            match object {
                Some(name) => {
                    segments.push(name);
                }
                None => {
                    // 集合地址以 / 结尾。
                    segments.push("");
                }
            }
        }
        url
    }

    fn object_url(&self) -> Url {
        self.url_for(self.dirs.len(), Some(&self.object))
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password))
    }

    fn apply_condition(
        builder: reqwest::RequestBuilder,
        condition: &Condition,
    ) -> reqwest::RequestBuilder {
        match condition {
            Condition::None => builder,
            Condition::IfNoneMatchAny => builder.header("If-None-Match", "*"),
            Condition::IfMatch(etag) => builder.header("If-Match", etag.as_str()),
        }
    }

    /// 逐级创建子目录。已存在（405）当作成功。
    async fn ensure_dirs(&self) -> Result<(), StorageError> {
        for depth in 1..=self.dirs.len() {
            let url = self.url_for(depth, None);
            let method = Method::from_bytes(b"MKCOL")
                .map_err(|_| StorageError::Io("内部错误：MKCOL 方法".to_owned()))?;
            let response = self
                .request(method, url)
                .send()
                .await
                .map_err(map_http_error)?;
            let status = response.status();
            if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED {
                continue;
            }
            return Err(status_error(status, "创建目录"));
        }
        Ok(())
    }

    async fn put_once(
        &self,
        bytes: &[u8],
        condition: &Condition,
    ) -> Result<(StatusCode, Option<String>), StorageError> {
        let builder = self
            .request(Method::PUT, self.object_url())
            .header("Content-Type", "application/octet-stream")
            .body(bytes.to_vec());
        let response = Self::apply_condition(builder, condition)
            .send()
            .await
            .map_err(map_http_error)?;
        let etag = header_etag(&response);
        Ok((response.status(), etag))
    }
}

fn header_etag(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .filter(|etag| !etag.is_empty())
}

fn status_error(status: StatusCode, action: &str) -> StorageError {
    if let Some(proxy) = proxy_auth_required(status) {
        return proxy;
    }
    if let Some(temporary) = retryable_status(status) {
        return temporary;
    }
    let reason = match status {
        StatusCode::UNAUTHORIZED => "用户名或密码不对".to_owned(),
        StatusCode::FORBIDDEN => "没有权限".to_owned(),
        StatusCode::NOT_FOUND => "地址不存在".to_owned(),
        StatusCode::METHOD_NOT_ALLOWED => "服务器不允许这个操作（地址可能不是 WebDAV）".to_owned(),
        StatusCode::INSUFFICIENT_STORAGE => "服务器空间不足".to_owned(),
        StatusCode::LOCKED => "文件被服务器锁定".to_owned(),
        other => format!("服务器返回 {}", other.as_u16()),
    };
    let message = format!("WebDAV {action}失败：{reason}");
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return StorageError::AuthFailed(message);
    }
    StorageError::Io(message)
}

impl RawObjectStore for WebDavRaw {
    fn get<'a>(&'a self) -> BoxFuture<'a, Result<Option<RawObject>, StorageError>> {
        Box::pin(async move {
            let response = self
                .request(Method::GET, self.object_url())
                .send()
                .await
                .map_err(map_http_error)?;
            let status = response.status();
            if status == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !status.is_success() {
                return Err(status_error(status, "读取"));
            }
            let etag = header_etag(&response);
            let bytes = read_body_limited(response, MAX_PACKAGE_BYTES, "WebDAV 上的同步包").await?;
            Ok(Some(RawObject { bytes, etag }))
        })
    }

    fn put<'a>(
        &'a self,
        bytes: &'a [u8],
        condition: &'a Condition,
    ) -> BoxFuture<'a, Result<RawPut, StorageError>> {
        Box::pin(async move {
            let (mut status, mut etag) = self.put_once(bytes, condition).await?;
            // 父目录不存在：标准是 409，有的服务器给 404。创建后重试一次。
            if (status == StatusCode::CONFLICT || status == StatusCode::NOT_FOUND)
                && !self.dirs.is_empty()
            {
                self.ensure_dirs().await?;
                (status, etag) = self.put_once(bytes, condition).await?;
            }
            if status == StatusCode::PRECONDITION_FAILED {
                return Ok(RawPut::PreconditionFailed);
            }
            if status.is_success() {
                return Ok(RawPut::Written { etag });
            }
            Err(status_error(status, "写入"))
        })
    }

    fn delete<'a>(&'a self, condition: &'a Condition) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(async move {
            let builder = self.request(Method::DELETE, self.object_url());
            let response = Self::apply_condition(builder, condition)
                .send()
                .await
                .map_err(map_http_error)?;
            let status = response.status();
            if status == StatusCode::PRECONDITION_FAILED {
                return Ok(false);
            }
            if status.is_success() || status == StatusCode::NOT_FOUND {
                return Ok(true);
            }
            Err(status_error(status, "删除"))
        })
    }

    fn sibling(&self, object_name: &str) -> Box<dyn RawObjectStore + '_> {
        Box::new(Self {
            client: self.client.clone(),
            base: self.base.clone(),
            dirs: self.dirs.clone(),
            object: object_name.to_owned(),
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }
}

/// 创建引擎用的存储。`mode` 是上次“测试连接”的检测结果；没检测过用
/// [`ConcurrencyMode::ReadBeforeWrite`]。
pub fn open_storage(
    config: &WebDavConfig,
    proxy: Option<&SyncProxy>,
    mode: ConcurrencyMode,
) -> Result<ConditionalStore<WebDavRaw>, StorageError> {
    Ok(ConditionalStore::new(WebDavRaw::new(config, proxy)?, mode))
}

/// 测试连接：能连上、能读、能写（并清理）、是否支持并发保护。
pub async fn probe(
    config: &WebDavConfig,
    proxy: Option<&SyncProxy>,
) -> Result<ConnectionProbe, StorageError> {
    let raw = WebDavRaw::new(config, proxy)?;
    Ok(probe_connection(&raw).await)
}

#[cfg(test)]
mod tests;
