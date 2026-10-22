//! 一次完整的浏览器授权登录：
//!
//! 生成 PKCE 和 `state` → 绑定回环端口 → 校验网址并用系统浏览器打开授权页 → 等回调
//! （成功 / 被拒绝 / 超时 / 取消）→ 用授权码换令牌 → 读账号名。
//!
//! 换令牌和读账号名走**软件网络代理**（`proxy` 参数）；浏览器那一步是系统浏览器自己的网络设置，
//! 不经过这里。

use std::time::Duration;

use tokio::sync::Notify;
use tracing::info;

use super::browser::BrowserOpener;
use super::loopback::{CallbackOutcome, Loopback};
use super::oauth::{self, Endpoints, OAuthError, TokenSet};
use super::pkce;
use crate::sync::net::{build_client, SyncProxy};
use crate::sync::storage::StorageError;

/// 用户 5 分钟内没有完成授权就放弃等待。
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 没拿到账号名时界面上显示的占位。
pub const UNKNOWN_ACCOUNT: &str = "Microsoft 账号";

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("你在浏览器里取消了授权，没有登录")]
    Denied,
    /// 微软返回了别的错误（比如这个应用没有被允许、回调地址没注册）。
    #[error("微软返回了错误：{0}")]
    Provider(String),
    #[error("5 分钟内没有完成登录，已经停止等待，可以重新登录")]
    Timeout,
    #[error("已取消登录")]
    Cancelled,
    #[error("无法在本机开启登录回调的监听：{0}")]
    Listen(String),
    #[error("无法打开系统浏览器：{0}")]
    Browser(String),
    #[error("无法连接到微软的登录服务（网络不通或代理不可用）：{0}")]
    Network(String),
    /// 换令牌被拒绝、服务暂时不可用等。
    #[error("{0}")]
    Exchange(String),
    #[error("内部错误：{0}")]
    Internal(String),
    /// 自己从源码编译的版本没有内置的 client ID，又没在“高级”里填。
    #[error("这个版本没有内置 OneDrive 的 client ID，没法登录。可以在“高级”里填你自己在 Azure 注册的应用的 client ID")]
    NoClientId,
    #[error("client ID 的格式不对（应该形如 xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx）")]
    InvalidClientId,
}

pub struct LoginParams<'a> {
    pub client_id: &'a str,
    pub endpoints: &'a Endpoints,
    /// 软件网络代理；`None` = 直连。
    pub proxy: Option<&'a SyncProxy>,
    /// 等用户授权的最长时间（生产用 [`LOGIN_TIMEOUT`]）。
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct LoginSuccess {
    pub tokens: TokenSet,
    pub account: String,
}

/// 传输层错误里最有用的一句（去掉“存储读写失败”这类对登录没意义的前缀）。
fn transport_detail(err: &StorageError) -> String {
    match err {
        StorageError::Timeout => "请求超时".to_owned(),
        StorageError::Io(message) => message
            .rsplit('：')
            .next()
            .unwrap_or(message)
            .trim()
            .to_owned(),
        other => other.to_string(),
    }
}

fn map_oauth(err: OAuthError) -> LoginError {
    match err {
        OAuthError::Transport(err) => LoginError::Network(transport_detail(&err)),
        other => LoginError::Exchange(other.to_string()),
    }
}

fn map_provider_error(error: &str, description: &str) -> LoginError {
    if error == "access_denied" {
        return LoginError::Denied;
    }
    let detail = description.lines().next().unwrap_or_default().trim();
    if detail.is_empty() {
        LoginError::Provider(error.to_owned())
    } else {
        LoginError::Provider(format!("{error}（{detail}）"))
    }
}

/// 跑完整个登录。`cancel` 被通知时立刻放弃（关闭监听）。
pub async fn login(
    params: LoginParams<'_>,
    opener: &dyn BrowserOpener,
    cancel: &Notify,
) -> Result<LoginSuccess, LoginError> {
    let client = build_client(params.proxy).map_err(|err| LoginError::Internal(err.to_string()))?;
    let pkce = pkce::generate()?;
    let state = pkce::random_state()?;
    let loopback = Loopback::bind().await?;
    let redirect_uri = loopback.redirect_uri();
    info!(
        port = loopback.port(),
        addresses = ?loopback.addresses(),
        "OneDrive 登录：开始等待浏览器授权"
    );
    let url = oauth::authorize_url(
        params.endpoints,
        params.client_id,
        &redirect_uri,
        &pkce.challenge,
        &state,
    )
    .map_err(LoginError::Internal)?;
    // 先开好监听、再开浏览器：浏览器跳回来的时候端口一定已经在等了。
    opener.open(&url).map_err(LoginError::Browser)?;

    let outcome = loopback.wait(&state, params.timeout, cancel).await?;
    let code = match outcome {
        CallbackOutcome::Code(code) => code,
        CallbackOutcome::Error { error, description } => {
            return Err(map_provider_error(&error, &description));
        }
    };

    let exchange = async {
        let tokens = oauth::exchange_code(
            &client,
            params.endpoints,
            params.client_id,
            &code,
            &pkce.verifier,
            &redirect_uri,
        )
        .await
        .map_err(map_oauth)?;
        let account = oauth::account_label(&client, params.endpoints, &tokens.access_token)
            .await
            .unwrap_or_else(|| UNKNOWN_ACCOUNT.to_owned());
        Ok::<_, LoginError>(LoginSuccess { tokens, account })
    };
    let success = tokio::select! {
        result = exchange => result?,
        _ = cancel.notified() => return Err(LoginError::Cancelled),
    };
    info!("OneDrive 登录成功");
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_become_readable_messages() {
        assert!(matches!(
            map_provider_error("access_denied", "AADSTS65004: The user declined"),
            LoginError::Denied
        ));
        assert_eq!(
            map_provider_error(
                "invalid_request",
                "AADSTS900144: missing client_id\r\nTrace ID: x"
            )
            .to_string(),
            "微软返回了错误：invalid_request（AADSTS900144: missing client_id）"
        );
        assert_eq!(
            map_provider_error("server_error", "").to_string(),
            "微软返回了错误：server_error"
        );
    }

    #[test]
    fn every_error_has_a_chinese_message_without_secrets() {
        for err in [
            LoginError::Denied,
            LoginError::Timeout,
            LoginError::Cancelled,
            LoginError::Listen("x".to_owned()),
            LoginError::Browser("x".to_owned()),
            LoginError::Network("x".to_owned()),
        ] {
            let text = err.to_string();
            assert!(text.chars().any(|c| c as u32 > 0x4e00), "{text}");
        }
        assert!(LoginError::Timeout.to_string().contains("5 分钟"));
    }
}
