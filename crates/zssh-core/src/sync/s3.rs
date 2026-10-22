//! S3 兼容存储（AWS、Cloudflare R2、MinIO、阿里云 OSS、腾讯云 COS……）。
//!
//! 只用到对单个对象的 `GET` / `PUT` / `DELETE`，签名用自己实现的 SigV4
//! （[`super::sigv4`]），不引入 AWS SDK。并发保护靠条件写入（`If-Match` /
//! `If-None-Match`），**各家支持程度不同**，连接时用
//! [`super::remote::probe_connection`] 探测，不支持就退回“写前再读一次比较”。
//!
//! 只按文档实现、没有在真实服务商上验证的地方在报告里逐条列出；这里的注释只写
//! 为什么这样处理。

use reqwest::{Client, Method, StatusCode, Url};

use super::net::{
    build_client, map_http_error, proxy_auth_required, read_body_limited, read_error_text,
    retryable_status, SyncProxy, MAX_META_BYTES, MAX_PACKAGE_BYTES,
};
use super::remote::{
    probe_connection, ConcurrencyMode, Condition, ConditionalStore, ConnectionProbe, RawObject,
    RawObjectStore, RawPut,
};
use super::sigv4::{amz_date, sha256_hex, sign, uri_encode, Credentials, SigningInput};
use super::storage::StorageError;
use crate::ssh::BoxFuture;

pub const OBJECT_NAME: &str = "zssh-sync.json";

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct S3Config {
    /// 例如 `https://s3.us-east-1.amazonaws.com`、`https://<账号>.r2.cloudflarestorage.com`、
    /// `http://127.0.0.1:9000`。
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// 对象路径前缀（可以多级），空 = 放在桶根。
    #[serde(default)]
    pub prefix: String,
    /// true = 路径风格 `https://endpoint/bucket/key`；false = 虚拟主机风格
    /// `https://bucket.endpoint/key`。
    #[serde(default = "default_path_style")]
    pub path_style: bool,
}

fn default_path_style() -> bool {
    true
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// 拼对象的请求地址和 SigV4 用的规范路径（两者用同一份编码，签名才对得上）。
pub fn object_location(
    config: &S3Config,
    object_name: &str,
) -> Result<(Url, String), StorageError> {
    let endpoint = Url::parse(config.endpoint.trim())
        .map_err(|err| StorageError::Io(format!("S3 Endpoint 不合法：{err}")))?;
    let scheme = endpoint.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(StorageError::Io(format!(
            "S3 Endpoint 只支持 http(s)，收到 {scheme}://"
        )));
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| StorageError::Io("S3 Endpoint 缺少主机名".to_owned()))?;
    let bucket = config.bucket.trim();
    if bucket.is_empty() || bucket.contains('/') {
        return Err(StorageError::Io("Bucket 名称不合法".to_owned()));
    }
    let key: String = config
        .prefix
        .split('/')
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .chain(std::iter::once(object_name))
        .collect::<Vec<_>>()
        .join("/");
    let endpoint_path = endpoint.path().trim_matches('/');
    let mut path = String::new();
    if !endpoint_path.is_empty() {
        path.push('/');
        path.push_str(endpoint_path);
    }
    let authority_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = endpoint.port().map(|p| format!(":{p}")).unwrap_or_default();
    let authority = if config.path_style {
        path.push('/');
        path.push_str(bucket);
        format!("{authority_host}{port}")
    } else {
        format!("{bucket}.{authority_host}{port}")
    };
    path.push('/');
    path.push_str(&key);
    let canonical_path = uri_encode(&path, true);
    let url = Url::parse(&format!("{scheme}://{authority}{canonical_path}"))
        .map_err(|err| StorageError::Io(format!("S3 地址拼接失败：{err}")))?;
    Ok((url, canonical_path))
}

pub struct S3Raw {
    client: Client,
    config: S3Config,
    object: String,
}

impl S3Raw {
    pub fn new(config: &S3Config, proxy: Option<&SyncProxy>) -> Result<Self, StorageError> {
        // 先验证地址能拼出来，错误在创建时就暴露。
        object_location(config, OBJECT_NAME)?;
        if config.region.trim().is_empty() {
            return Err(StorageError::Io("Region 不能为空".to_owned()));
        }
        if config.access_key.trim().is_empty() || config.secret_key.is_empty() {
            return Err(StorageError::Io("Access Key 和 Secret 不能为空".to_owned()));
        }
        Ok(Self {
            client: build_client(proxy)?,
            config: config.clone(),
            object: OBJECT_NAME.to_owned(),
        })
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }

    fn signed_request(
        &self,
        method: Method,
        body: &[u8],
        condition: &Condition,
    ) -> Result<reqwest::RequestBuilder, StorageError> {
        let (url, canonical_path) = object_location(&self.config, &self.object)?;
        let authority = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let date = amz_date(Self::now());
        let payload_hash = sha256_hex(body);
        let mut headers = vec![
            ("host".to_owned(), authority),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), date.clone()),
        ];
        match condition {
            Condition::None => {}
            Condition::IfNoneMatchAny => headers.push(("if-none-match".to_owned(), "*".to_owned())),
            Condition::IfMatch(etag) => headers.push(("if-match".to_owned(), etag.clone())),
        }
        let signature = sign(
            &Credentials {
                access_key: self.config.access_key.trim().to_owned(),
                secret_key: self.config.secret_key.clone(),
            },
            &SigningInput {
                method: method.as_str(),
                canonical_uri: &canonical_path,
                canonical_query: "",
                headers: &headers,
                payload_hash: &payload_hash,
                region: self.config.region.trim(),
                service: "s3",
                amz_date: &date,
            },
        );
        let mut builder = self
            .client
            .request(method, url)
            .header("x-amz-date", date)
            .header("x-amz-content-sha256", payload_hash)
            .header("Authorization", signature.authorization);
        match condition {
            Condition::None => {}
            Condition::IfNoneMatchAny => builder = builder.header("If-None-Match", "*"),
            Condition::IfMatch(etag) => builder = builder.header("If-Match", etag.as_str()),
        }
        Ok(builder)
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

/// 取 S3 错误响应 XML 里的 `<Code>`（只用来给用户一个能看懂的原因）。
fn error_code(body: &str) -> Option<String> {
    let start = body.find("<Code>")? + "<Code>".len();
    let end = body[start..].find("</Code>")? + start;
    Some(body[start..end].to_owned())
}

async fn status_error(response: reqwest::Response, action: &str) -> StorageError {
    let status = response.status();
    if let Some(proxy) = proxy_auth_required(status) {
        return proxy;
    }
    if let Some(temporary) = retryable_status(status) {
        return temporary;
    }
    let body = read_error_text(response, MAX_META_BYTES).await;
    let code = error_code(&body);
    let reason = match (status, code.as_deref()) {
        (_, Some("SignatureDoesNotMatch")) => {
            "签名不对（Secret、Region 或地址风格可能填错了）".to_owned()
        }
        (_, Some("InvalidAccessKeyId")) => "Access Key 不存在".to_owned(),
        (_, Some("NoSuchBucket")) => "Bucket 不存在".to_owned(),
        (_, Some("AccessDenied")) | (StatusCode::FORBIDDEN, _) => {
            "没有权限（桶没给列表权限时，对象不存在也会返回这个）".to_owned()
        }
        (_, Some("PermanentRedirect")) | (_, Some("AuthorizationHeaderMalformed")) => {
            "Region 或 Endpoint 填得不对".to_owned()
        }
        (StatusCode::UNAUTHORIZED, _) => "认证失败".to_owned(),
        (other, Some(code)) => format!("服务器返回 {}（{code}）", other.as_u16()),
        (other, None) => format!("服务器返回 {}", other.as_u16()),
    };
    let message = format!("S3 {action}失败：{reason}");
    let rejected_credentials = matches!(
        code.as_deref(),
        Some("SignatureDoesNotMatch" | "InvalidAccessKeyId" | "AccessDenied" | "InvalidSecurity")
    ) || matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    );
    if rejected_credentials {
        return StorageError::AuthFailed(message);
    }
    StorageError::Io(message)
}

impl RawObjectStore for S3Raw {
    fn get<'a>(&'a self) -> BoxFuture<'a, Result<Option<RawObject>, StorageError>> {
        Box::pin(async move {
            let response = self
                .signed_request(Method::GET, b"", &Condition::None)?
                .send()
                .await
                .map_err(map_http_error)?;
            let status = response.status();
            if status == StatusCode::NOT_FOUND {
                // 对象不存在是 404 NoSuchKey；桶不存在也是 404（NoSuchBucket），后者要报错。
                let body = read_error_text(response, MAX_META_BYTES).await;
                if error_code(&body).as_deref() == Some("NoSuchBucket") {
                    return Err(StorageError::Io("S3 读取失败：Bucket 不存在".to_owned()));
                }
                return Ok(None);
            }
            if !status.is_success() {
                return Err(status_error(response, "读取").await);
            }
            let etag = header_etag(&response);
            let bytes = read_body_limited(response, MAX_PACKAGE_BYTES, "S3 上的同步包").await?;
            Ok(Some(RawObject { bytes, etag }))
        })
    }

    fn put<'a>(
        &'a self,
        bytes: &'a [u8],
        condition: &'a Condition,
    ) -> BoxFuture<'a, Result<RawPut, StorageError>> {
        Box::pin(async move {
            let response = self
                .signed_request(Method::PUT, bytes, condition)?
                .body(bytes.to_vec())
                .send()
                .await
                .map_err(map_http_error)?;
            let status = response.status();
            // 412：条件不满足。409：S3 对并发的条件写返回 ConditionalRequestConflict，
            // 同样是“别人抢先写了”，重新读再合并即可。
            if status == StatusCode::PRECONDITION_FAILED || status == StatusCode::CONFLICT {
                return Ok(RawPut::PreconditionFailed);
            }
            if status.is_success() {
                return Ok(RawPut::Written {
                    etag: header_etag(&response),
                });
            }
            if status == StatusCode::NOT_IMPLEMENTED && *condition != Condition::None {
                return Ok(RawPut::ConditionNotSupported);
            }
            Err(status_error(response, "写入").await)
        })
    }

    fn delete<'a>(&'a self, condition: &'a Condition) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(async move {
            let response = self
                .signed_request(Method::DELETE, b"", condition)?
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
            Err(status_error(response, "删除").await)
        })
    }

    fn sibling(&self, object_name: &str) -> Box<dyn RawObjectStore + '_> {
        Box::new(Self {
            client: self.client.clone(),
            config: self.config.clone(),
            object: object_name.to_owned(),
        })
    }
}

pub fn open_storage(
    config: &S3Config,
    proxy: Option<&SyncProxy>,
    mode: ConcurrencyMode,
) -> Result<ConditionalStore<S3Raw>, StorageError> {
    Ok(ConditionalStore::new(S3Raw::new(config, proxy)?, mode))
}

pub async fn probe(
    config: &S3Config,
    proxy: Option<&SyncProxy>,
) -> Result<ConnectionProbe, StorageError> {
    let raw = S3Raw::new(config, proxy)?;
    Ok(probe_connection(&raw).await)
}

#[cfg(test)]
mod tests;
