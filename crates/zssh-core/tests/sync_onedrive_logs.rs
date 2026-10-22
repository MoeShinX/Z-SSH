//! 集成测试（S-4）：OneDrive 登录、令牌刷新、同步的全过程，日志里不能出现令牌、授权码、
//! PKCE verifier / state、账号名。单独一个测试二进制、一个用例：用全局订阅者抓日志，
//! 不会被别的用例已经缓存的 callsite 干扰。

#[path = "common/fake_browser.rs"]
mod fake_browser;
#[path = "common/harness.rs"]
mod harness;
#[path = "common/mock_microsoft.rs"]
mod mock_microsoft;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fake_browser::FakeBrowser;
use harness::*;
use mock_microsoft::{MockMicrosoft, ACCOUNT_NAME, CLIENT_ID};
use tokio::sync::Notify;
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::onedrive::oauth::{now_secs, TokenSet};
use zssh_core::sync::onedrive::tokens::{DbTokenStore, TokenStore, TOKENS_KEY};
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::service::{self, SyncContext};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(self.0.clone())
    }
}

#[tokio::test]
async fn logs_never_contain_tokens_codes_verifiers_or_the_account_name() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(Capture(captured.clone()))
            .finish(),
    )
    .expect("这个测试二进制里只有一个用例，全局订阅者一定装得上");
    tracing::callsite::rebuild_interest_cache();

    let mock = MockMicrosoft::start().await;
    let endpoints = mock.endpoints();
    let machine = Machine::new("od-logs", "dev-a");
    seed(&machine);
    machine
        .db
        .setting_set(
            zssh_core::sync::app_proxy::APP_NETWORK_PROXY_KEY,
            "\"none\"",
        )
        .unwrap();
    let browser = FakeBrowser::approving();

    // 登录 → 同步 → 令牌快过期自动刷新 → 再同步 → 服务器 401 强制刷新 → 再同步。
    service::onedrive_sign_in(
        &machine.db,
        &DpapiSealer,
        &endpoints,
        CLIENT_ID,
        Duration::from_secs(20),
        &browser,
        &Notify::new(),
    )
    .await
    .expect("登录");
    let ctx = SyncContext {
        db: &machine.db,
        sealer: &DpapiSealer,
        device_id: machine.device.to_owned(),
        data_dir: machine.dir.clone(),
        known_hosts_path: Some(machine.dir.join("known_hosts")),
        app_version: "0.1.0".to_owned(),
        onedrive_endpoints: endpoints.clone(),
    };
    ctx.run_sync(PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("同步");
    let store = DbTokenStore::new(&machine.db, &DpapiSealer);
    let first = store.load().unwrap().expect("令牌");
    store
        .save(&TokenSet {
            expires_at: now_secs() + 5,
            ..first.clone()
        })
        .unwrap();
    ctx.run_sync(PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("刷新后同步");
    mock.expire_access_tokens();
    ctx.run_sync(PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("401 后同步");
    let last = store.load().unwrap().expect("令牌");
    assert_ne!(first.refresh_token, last.refresh_token, "确实刷新过");

    // 把授权请求里的 PKCE challenge / state 也算进“不能出现”的东西（verifier 只在内存里，
    // 拿不到，但 challenge / state 同样不该出现在日志里）。
    let authorize = mock.authorize_requests();
    let params = &authorize[0].params;
    let mut secrets: Vec<String> = vec![
        first.access_token,
        first.refresh_token,
        last.access_token,
        last.refresh_token,
        params["state"].clone(),
        params["code_challenge"].clone(),
        ACCOUNT_NAME.to_owned(),
        "mock-code-".to_owned(),
        "mock-access-".to_owned(),
        "mock-refresh-".to_owned(),
        TOKENS_KEY.to_owned() + "\":\"",
    ];
    secrets.retain(|s| !s.is_empty());

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(
        logs.contains("OneDrive 登录成功") && logs.contains("OneDrive 登录回调监听已关闭"),
        "登录流程自己的日志没抓到：{logs:?}"
    );
    for secret in &secrets {
        assert!(
            !logs.contains(secret.as_str()),
            "日志里出现了 {secret}：\n{logs}"
        );
    }
}
