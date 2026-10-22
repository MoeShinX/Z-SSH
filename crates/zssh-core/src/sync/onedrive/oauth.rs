// Adapted from Netcatty electron/bridges/onedriveAuthBridge.cjs (scope set, refresh-failure error codes)
//! 微软身份平台的 OAuth 请求：拼授权地址、用授权码换令牌、用刷新令牌换新令牌、读账号名。
//!
//! 依据的官方文档（逐项核对过，链接也写在 `docs/reports/S-4.md`）：
//! - 授权码流程 + PKCE：<https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow>
//!   授权地址 `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize`，令牌地址同目录的
//!   `token`；公共客户端换令牌**不带** `client_secret`；`code_challenge_method` 用 `S256`；
//!   `redirect_uri` 对桌面应用用 `http://localhost`；刷新时响应里的 `refresh_token` 要替换旧的
//! - 回调地址：<https://learn.microsoft.com/en-us/entra/identity-platform/reply-url>
//!   localhost 回调地址匹配时忽略端口，所以注册一次 `http://localhost`，运行时用随机端口
//! - 刷新令牌：<https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens>
//!   每次使用都会换发新的刷新令牌（旧的不会被立刻撤销）；默认 90 天有效；可能随时被撤销
//!
//! 租户用 `consumers`：只支持个人微软账号（见 [`super`] 的说明）。

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sync::net::{map_http_error, read_body_limited, MAX_META_BYTES};
use crate::sync::storage::StorageError;

/// 权限范围：只能访问应用专用文件夹、读账号名、拿刷新令牌。
pub const SCOPES: &str = "Files.ReadWrite.AppFolder User.Read offline_access";

/// 面向用户的统一说法：刷新令牌不能用了，只能重新登录。
pub const REAUTH_MESSAGE: &str = "OneDrive 登录已失效，请重新登录";

/// 微软服务的几个地址。生产用 [`Endpoints::microsoft`]，测试指向进程内的模拟服务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// 授权页（浏览器里打开）。
    pub authorize: String,
    /// 令牌接口。
    pub token: String,
    /// Graph 接口根，末尾没有 `/`。
    pub graph: String,
}

impl Endpoints {
    pub fn microsoft() -> Self {
        Self {
            authorize: "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize"
                .to_owned(),
            token: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token".to_owned(),
            graph: "https://graph.microsoft.com/v1.0".to_owned(),
        }
    }
}

/// 一组令牌。只存在内存和 DPAPI 加密的本机存储里，**Debug 输出里没有令牌**。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// 访问令牌过期的时刻（Unix 秒）。
    pub expires_at: i64,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// 授权请求的地址。`prompt=select_account`：浏览器里登录着好几个账号时让用户自己选，
/// 不会悄悄用上错的那个。
pub fn authorize_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> Result<String, String> {
    Url::parse_with_params(
        &endpoints.authorize,
        &[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("state", state),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ],
    )
    .map(String::from)
    .map_err(|err| format!("授权地址不合法：{err}"))
}

/// 令牌接口的失败，按“能不能靠重新登录解决”分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// 刷新令牌 / 授权码不能用了（被撤销、过期、用户改了密码、权限被收回）：只能重新登录。
    #[error("OneDrive 登录已失效，请重新登录")]
    Reauth(String),
    /// 请求本身被拒绝（client ID 不对、回调地址没注册……）：重试没用，要改配置。
    #[error("{0}")]
    Rejected(String),
    /// 服务暂时不可用（5xx、429、`temporarily_unavailable`）：稍后重试。
    #[error("微软的登录服务暂时不可用：{0}")]
    Unavailable(String),
    /// 网络不通、超时、代理不可用……
    #[error("{0}")]
    Transport(StorageError),
}

impl From<OAuthError> for StorageError {
    fn from(err: OAuthError) -> Self {
        match err {
            OAuthError::Reauth(_) => StorageError::AuthFailed(REAUTH_MESSAGE.to_owned()),
            OAuthError::Rejected(message) => StorageError::AuthFailed(message),
            OAuthError::Unavailable(message) => StorageError::Unavailable(message),
            OAuthError::Transport(err) => err,
        }
    }
}

/// 这些 `error` 值表示“令牌已经不能用，必须重新交互登录”。
/// 见 <https://learn.microsoft.com/en-us/entra/identity-platform/reference-error-codes>。
const REAUTH_ERRORS: &[&str] = &[
    "invalid_grant",
    "interaction_required",
    "consent_required",
    "login_required",
];

/// 错误响应里 `error_description` 的第一行（后面是 Trace ID、时间戳之类的排障信息，界面不用显示）。
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

async fn post_token(
    client: &Client,
    endpoints: &Endpoints,
    form: &[(&str, &str)],
) -> Result<TokenSet, OAuthError> {
    let response = client
        .post(&endpoints.token)
        .header("Accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|err| OAuthError::Transport(map_http_error(err)))?;
    let status = response.status();
    let bytes = read_body_limited(response, MAX_META_BYTES, "微软的令牌响应")
        .await
        .map_err(OAuthError::Transport)?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        return parse_token_response(&value);
    }
    let error = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let description = first_line(
        value
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if status.as_u16() == 429 || status.is_server_error() || error == "temporarily_unavailable" {
        return Err(OAuthError::Unavailable(format!(
            "服务器返回 {}",
            status.as_u16()
        )));
    }
    if REAUTH_ERRORS.contains(&error) {
        return Err(OAuthError::Reauth(description));
    }
    let detail = match (error.is_empty(), description.is_empty()) {
        (true, _) => format!("服务器返回 {}", status.as_u16()),
        (false, true) => error.to_owned(),
        (false, false) => format!("{error}：{description}"),
    };
    Err(OAuthError::Rejected(format!("微软拒绝了请求：{detail}")))
}

fn parse_token_response(value: &Value) -> Result<TokenSet, OAuthError> {
    let text = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let access_token = text("access_token")
        .ok_or_else(|| OAuthError::Rejected("微软的响应里没有访问令牌".to_owned()))?;
    let expires_in = value
        .get("expires_in")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(3600)
        .clamp(60, 24 * 3600);
    Ok(TokenSet {
        access_token,
        // 没给新的刷新令牌（不该发生）时留空，由调用方沿用旧的。
        refresh_token: text("refresh_token").unwrap_or_default(),
        expires_at: now_secs() + expires_in,
    })
}

/// 授权码换令牌。**不带 `client_secret`**（公共客户端），带上 PKCE 的 `code_verifier`。
pub async fn exchange_code(
    client: &Client,
    endpoints: &Endpoints,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenSet, OAuthError> {
    let tokens = post_token(
        client,
        endpoints,
        &[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", code_verifier),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPES),
        ],
    )
    .await?;
    if tokens.refresh_token.is_empty() {
        return Err(OAuthError::Rejected(
            "微软没有返回刷新令牌（需要 offline_access 权限），没法长期保持登录".to_owned(),
        ));
    }
    Ok(tokens)
}

/// 刷新令牌换新的访问令牌。微软会换发新的刷新令牌，返回值里的要替换掉旧的；
/// 响应里没带新的就沿用旧的。
pub async fn refresh(
    client: &Client,
    endpoints: &Endpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenSet, OAuthError> {
    let mut tokens = post_token(
        client,
        endpoints,
        &[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", SCOPES),
        ],
    )
    .await?;
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = refresh_token.to_owned();
    }
    Ok(tokens)
}

/// 取账号的显示名（`User.Read`）：优先显示名，其次邮箱。拿不到不影响登录。
pub async fn account_label(
    client: &Client,
    endpoints: &Endpoints,
    access_token: &str,
) -> Option<String> {
    let response = client
        .get(format!(
            "{}/me?$select=displayName,mail,userPrincipalName",
            endpoints.graph
        ))
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = read_body_limited(response, MAX_META_BYTES, "账号信息")
        .await
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    ["displayName", "mail", "userPrincipalName"]
        .iter()
        .find_map(|name| {
            value
                .get(*name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_pkce_state_and_the_exact_redirect() {
        let url = authorize_url(
            &Endpoints::microsoft(),
            "11111111-2222-3333-4444-555555555555",
            "http://localhost:54321",
            "CHALLENGE",
            "STATE",
        )
        .expect("拼地址");
        assert!(
            url.starts_with("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?"),
            "{url}"
        );
        let parsed = Url::parse(&url).unwrap();
        let get = |name: &str| {
            parsed
                .query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        assert_eq!(
            get("client_id").as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(get("response_type").as_deref(), Some("code"));
        assert_eq!(
            get("redirect_uri").as_deref(),
            Some("http://localhost:54321")
        );
        assert_eq!(get("code_challenge").as_deref(), Some("CHALLENGE"));
        assert_eq!(get("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(get("state").as_deref(), Some("STATE"));
        assert_eq!(get("response_mode").as_deref(), Some("query"));
        assert_eq!(
            get("scope").as_deref(),
            Some("Files.ReadWrite.AppFolder User.Read offline_access")
        );
        assert!(get("client_secret").is_none(), "公共客户端不带密钥");
        assert!(
            get("code_verifier").is_none(),
            "授权请求里不能出现 verifier"
        );
    }

    #[test]
    fn token_set_debug_hides_the_tokens() {
        let tokens = TokenSet {
            access_token: "ACCESS-SECRET".to_owned(),
            refresh_token: "REFRESH-SECRET".to_owned(),
            expires_at: 5,
        };
        let text = format!("{tokens:?}");
        assert!(!text.contains("SECRET"), "{text}");
    }

    #[test]
    fn token_responses_are_parsed_defensively() {
        let ok = parse_token_response(&serde_json::json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 3599
        }))
        .unwrap();
        assert_eq!(
            (ok.access_token.as_str(), ok.refresh_token.as_str()),
            ("a", "r")
        );
        assert!(ok.expires_at > now_secs() + 3500);
        // expires_in 是字符串、缺失、离谱的情况。
        let text =
            parse_token_response(&serde_json::json!({"access_token": "a", "expires_in": "120"}))
                .unwrap();
        assert!(text.expires_at <= now_secs() + 121);
        let missing = parse_token_response(&serde_json::json!({"access_token": "a"})).unwrap();
        assert!(missing.expires_at > now_secs() + 3000);
        let huge =
            parse_token_response(&serde_json::json!({"access_token": "a", "expires_in": 99999999}))
                .unwrap();
        assert!(huge.expires_at <= now_secs() + 24 * 3600 + 1);
        assert!(matches!(
            parse_token_response(&serde_json::json!({"refresh_token": "r"})),
            Err(OAuthError::Rejected(_))
        ));
    }

    #[test]
    fn oauth_errors_map_to_storage_errors_the_sync_engine_understands() {
        let reauth = StorageError::from(OAuthError::Reauth("AADSTS70008".to_owned()));
        assert_eq!(reauth, StorageError::AuthFailed(REAUTH_MESSAGE.to_owned()));
        assert!(matches!(
            StorageError::from(OAuthError::Unavailable("x".to_owned())),
            StorageError::Unavailable(_)
        ));
        assert!(matches!(
            StorageError::from(OAuthError::Transport(StorageError::Timeout)),
            StorageError::Timeout
        ));
        assert_eq!(first_line("AADSTS1\r\nTrace ID: x"), "AADSTS1");
    }
}
