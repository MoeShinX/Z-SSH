//! 集成测试（S-4）：OneDrive 的浏览器授权登录，对着进程内模拟的微软授权服务器跑完整流程。
//!
//! “浏览器”由测试里的 [`FakeBrowser`] 扮演：Z-SSH 把授权地址交给它，它去请求模拟的授权页，
//! 跟着 302 跳回本机回环地址（`http://localhost:<端口>/`，localhost 同时会解析到 127.0.0.1 和
//! [::1]，所以也验证了两个地址上的监听）。换令牌的请求验证 PKCE、授权码只能用一次、不带
//! `client_secret`。不连任何真实的微软服务。

#[path = "common/fake_browser.rs"]
mod fake_browser;
#[path = "common/mock_cloud.rs"]
mod mock_cloud;
#[path = "common/mock_microsoft.rs"]
mod mock_microsoft;

use std::sync::Arc;
use std::time::Duration;

use fake_browser::{browse, FakeBrowser};
use mock_cloud::{MockHttpProxy, MockSocks5};
use mock_microsoft::{AuthorizeMode, MockMicrosoft, ACCOUNT_NAME, CLIENT_ID};
use tokio::sync::Notify;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::onedrive::login::{login, LoginError, LoginParams, LoginSuccess};
use zssh_core::sync::onedrive::oauth::{Endpoints, SCOPES};

fn params<'a>(endpoints: &'a Endpoints, proxy: Option<&'a SyncProxy>) -> LoginParams<'a> {
    LoginParams {
        client_id: CLIENT_ID,
        endpoints,
        proxy,
        timeout: Duration::from_secs(20),
    }
}

async fn run(
    mock: &MockMicrosoft,
    browser: &FakeBrowser,
    proxy: Option<&SyncProxy>,
) -> Result<LoginSuccess, LoginError> {
    let endpoints = mock.endpoints();
    login(params(&endpoints, proxy), browser, &Notify::new()).await
}

#[tokio::test]
async fn login_runs_the_full_authorization_code_flow_with_pkce() {
    let mock = MockMicrosoft::start().await;
    let browser = FakeBrowser::approving();
    let success = run(&mock, &browser, None).await.expect("登录");

    assert_eq!(success.account, ACCOUNT_NAME);
    assert!(!success.tokens.access_token.is_empty());
    assert!(!success.tokens.refresh_token.is_empty());
    assert!(success.tokens.expires_at > 0);

    // 交给浏览器的网址：只有一个，指向授权页，带 PKCE 和 state，不带 verifier / 密钥。
    let urls = browser.urls();
    assert_eq!(urls.len(), 1);
    assert!(
        urls[0].starts_with(&mock.endpoints().authorize),
        "{}",
        urls[0]
    );
    let authorize = mock.authorize_requests();
    assert_eq!(authorize.len(), 1);
    let p = &authorize[0].params;
    assert_eq!(p["client_id"], CLIENT_ID);
    assert_eq!(p["response_type"], "code");
    assert_eq!(p["code_challenge_method"], "S256");
    assert_eq!(p["scope"], SCOPES);
    assert_eq!(p["response_mode"], "query");
    assert!(p["redirect_uri"].starts_with("http://localhost:"), "{p:?}");
    assert!(p["state"].len() >= 32);
    assert!(!p.contains_key("client_secret") && !p.contains_key("code_verifier"));

    // 换令牌：PKCE 由模拟服务器真实验证过（verifier 对不上会 400），不带客户端密钥。
    let tokens = mock.token_requests();
    assert_eq!(tokens.len(), 1, "{tokens:?}");
    assert_eq!(tokens[0].grant_type, "authorization_code");
    assert_eq!(tokens[0].status, 200);
    assert!(!tokens[0].had_client_secret);
    assert_eq!(tokens[0].client_id, CLIENT_ID);
}

#[tokio::test]
async fn the_user_denying_access_is_reported_and_no_token_is_requested() {
    let mock = MockMicrosoft::start().await;
    mock.set_mode(AuthorizeMode::Deny);
    let err = run(&mock, &FakeBrowser::approving(), None)
        .await
        .expect_err("用户拒绝");
    assert!(matches!(err, LoginError::Denied), "{err:?}");
    assert!(err.to_string().contains("取消了授权"), "{err}");
    assert!(mock.token_requests().is_empty(), "没授权就不该去换令牌");
}

#[tokio::test]
async fn other_provider_errors_are_shown_with_their_description() {
    let mock = MockMicrosoft::start().await;
    mock.set_mode(AuthorizeMode::Error(
        "unauthorized_client",
        "AADSTS700016: 应用没有被允许\r\nTrace ID: abc",
    ));
    let err = run(&mock, &FakeBrowser::approving(), None)
        .await
        .expect_err("微软报错");
    let text = err.to_string();
    assert!(text.contains("unauthorized_client"), "{text}");
    assert!(text.contains("AADSTS700016"), "{text}");
    assert!(!text.contains("Trace ID"), "排障用的尾巴不显示：{text}");
}

#[tokio::test]
async fn login_gives_up_after_the_timeout_and_closes_the_port() {
    let mock = MockMicrosoft::start().await;
    let browser = FakeBrowser::idle();
    let endpoints = mock.endpoints();
    let started = std::time::Instant::now();
    let err = login(
        LoginParams {
            timeout: Duration::from_millis(300),
            ..params(&endpoints, None)
        },
        &browser,
        &Notify::new(),
    )
    .await
    .expect_err("超时");
    assert!(matches!(err, LoginError::Timeout), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // 超时之后端口不再有人监听：浏览器迟到的回调连不上。
    let redirect = mock.authorize_requests();
    assert!(redirect.is_empty(), "浏览器没动过");
    let url = browser.urls().remove(0);
    let redirect_uri = reqwest::Url::parse(&url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    let late = browse(&format!("{redirect_uri}/?code=late&state=x")).await;
    assert!(late.is_err(), "监听已关闭：{late:?}");
}

#[tokio::test]
async fn cancelling_stops_the_login_and_closes_the_port() {
    let mock = MockMicrosoft::start().await;
    let browser = Arc::new(FakeBrowser::idle());
    let cancel = Arc::new(Notify::new());
    let endpoints = mock.endpoints();
    let (browser2, cancel2) = (browser.clone(), cancel.clone());
    let task =
        tokio::spawn(
            async move { login(params(&endpoints, None), browser2.as_ref(), &cancel2).await },
        );
    // 等浏览器拿到网址，再点“取消”。
    for _ in 0..100 {
        if !browser.urls().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(browser.urls().len(), 1);
    cancel.notify_one();
    let err = task.await.unwrap().expect_err("取消");
    assert!(matches!(err, LoginError::Cancelled), "{err:?}");
    let redirect_uri = reqwest::Url::parse(&browser.urls()[0])
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    assert!(browse(&format!("{redirect_uri}/?code=x&state=y"))
        .await
        .is_err());
}

#[tokio::test]
async fn a_forged_callback_does_not_break_the_real_login() {
    let mock = MockMicrosoft::start().await;
    // 另一个网页先往回环端口发了一个 state 不对的回调，真正的授权随后才到。
    let browser = FakeBrowser::new(|url| {
        tokio::spawn(async move {
            let redirect_uri = reqwest::Url::parse(&url)
                .unwrap()
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            let (status, _) = browse(&format!("{redirect_uri}/?code=forged&state=forged"))
                .await
                .expect("伪造的请求");
            assert_eq!(status, 400, "state 不对要被拒绝");
            let _ = browse(&url).await;
        });
    });
    let success = run(&mock, &browser, None)
        .await
        .expect("真正的登录照常完成");
    assert_eq!(success.account, ACCOUNT_NAME);
    assert_eq!(
        mock.token_requests().len(),
        1,
        "伪造的授权码没有被拿去换令牌"
    );
}

#[tokio::test]
async fn a_rejected_code_exchange_is_reported_without_leaking_secrets() {
    let mock = MockMicrosoft::start().await;
    mock.fail_next_code_exchange(
        400,
        "invalid_grant",
        "AADSTS54005: OAuth2 Authorization code was already redeemed.",
    );
    let err = run(&mock, &FakeBrowser::approving(), None)
        .await
        .expect_err("换令牌失败");
    assert!(matches!(err, LoginError::Exchange(_)), "{err:?}");
    let text = err.to_string();
    assert!(!text.contains("mock-"), "错误里不能带令牌：{text}");
}

#[tokio::test]
async fn the_microsoft_calls_go_through_a_socks5_proxy() {
    let mock = MockMicrosoft::start().await;
    let socks = MockSocks5::start(mock.addr(), None).await;
    let proxy = SyncProxy {
        kind: ProxyScheme::Socks5,
        host: "127.0.0.1".to_owned(),
        port: socks.addr().port(),
        username: None,
        password: None,
    };
    let success = run(&mock, &FakeBrowser::approving(), Some(&proxy))
        .await
        .expect("经 SOCKS5 登录");
    assert_eq!(success.account, ACCOUNT_NAME);
    // 换令牌和读账号名（2 个请求）走了代理；浏览器的授权页请求直连，不经过代理。
    let via_proxy = socks.targets();
    assert!(!via_proxy.is_empty(), "换令牌要经过 SOCKS5 代理");
    assert!(via_proxy
        .iter()
        .all(|(_, port)| *port == mock.addr().port()));
    let token_logged = mock.token_requests();
    assert_eq!(token_logged.len(), 1);
}

#[tokio::test]
async fn the_microsoft_calls_go_through_an_http_proxy() {
    let mock = MockMicrosoft::start().await;
    let proxy_server = MockHttpProxy::start(
        mock.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let proxy = SyncProxy {
        kind: ProxyScheme::Http,
        host: "127.0.0.1".to_owned(),
        port: proxy_server.addr().port(),
        username: Some("proxy-user".to_owned()),
        password: Some("proxy-pass".to_owned()),
    };
    let success = run(&mock, &FakeBrowser::approving(), Some(&proxy))
        .await
        .expect("经 HTTP 代理登录");
    assert_eq!(success.account, ACCOUNT_NAME);
    let requests = proxy_server.requests();
    assert!(!requests.is_empty(), "换令牌要经过 HTTP 代理");
    assert!(requests.iter().all(|r| r.authorized), "{requests:?}");
}
