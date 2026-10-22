//! 集成测试（S-4）：OneDrive 的令牌刷新和存储实现，对着进程内模拟的微软服务跑。
//!
//! 模拟服务器真实执行：刷新令牌每次换发新的（严格模式下旧的立刻作废，所以没存下新令牌的实现
//! 第二次刷新就会失败）、访问令牌过期、`If-Match` / 上传会话 / 限流等。不连任何真实的微软服务。

#[path = "common/mock_cloud.rs"]
mod mock_cloud;
#[path = "common/mock_microsoft.rs"]
mod mock_microsoft;

use mock_microsoft::{MockMicrosoft, CLIENT_ID};
use zssh_core::sync::net::build_client;
use zssh_core::sync::onedrive::oauth::{now_secs, TokenSet, REAUTH_MESSAGE};
use zssh_core::sync::onedrive::tokens::{MemoryTokenStore, TokenManager, TokenStore};
use zssh_core::sync::storage::StorageError;

fn manager<'a>(mock: &MockMicrosoft, store: &'a dyn TokenStore) -> TokenManager<'a> {
    TokenManager::new(
        build_client(None).expect("HTTP 客户端"),
        mock.endpoints(),
        CLIENT_ID.to_owned(),
        std::sync::Arc::new(store),
    )
}

/// 模拟服务器发的一组令牌，放进内存存储；`expires_in` 秒后过期。
fn signed_in(mock: &MockMicrosoft, expires_in: i64) -> MemoryTokenStore {
    let (access_token, refresh_token) = mock.mint_tokens();
    MemoryTokenStore::with(TokenSet {
        access_token,
        refresh_token,
        expires_at: now_secs() + expires_in,
    })
}

fn refreshes(mock: &MockMicrosoft) -> usize {
    mock.token_requests()
        .iter()
        .filter(|r| r.grant_type == "refresh_token")
        .count()
}

// ---------------------------------------------------------------------------
// 令牌刷新

#[tokio::test]
async fn a_valid_access_token_is_used_without_calling_microsoft() {
    let mock = MockMicrosoft::start().await;
    let store = signed_in(&mock, 3600);
    let before = store.snapshot().unwrap();
    let token = manager(&mock, &store).access_token().await.expect("取令牌");
    assert_eq!(token, before.access_token);
    assert_eq!(refreshes(&mock), 0, "还没到期，不该去刷新");
}

#[tokio::test]
async fn an_expiring_access_token_is_refreshed_and_the_rotated_refresh_token_is_saved() {
    let mock = MockMicrosoft::start().await;
    // 严格轮换：旧的刷新令牌用过一次就作废，所以第二次刷新只有存下了新令牌才能成功。
    mock.set_strict_rotation(true);
    let store = signed_in(&mock, 30);
    let first = store.snapshot().unwrap();

    let access = manager(&mock, &store)
        .access_token()
        .await
        .expect("第一次刷新");
    let after_first = store.snapshot().unwrap();
    assert_ne!(access, first.access_token, "换了新的访问令牌");
    assert_eq!(access, after_first.access_token);
    assert_ne!(
        after_first.refresh_token, first.refresh_token,
        "刷新令牌被轮换"
    );
    assert!(
        after_first.expires_at > now_secs() + 3000,
        "过期时间也更新了"
    );
    assert_eq!(refreshes(&mock), 1);

    // 再让它快过期，用存下的新刷新令牌第二次刷新：旧令牌已被服务器作废，能成功说明存对了。
    store
        .save(&TokenSet {
            expires_at: now_secs() + 10,
            ..after_first.clone()
        })
        .unwrap();
    let access2 = manager(&mock, &store)
        .access_token()
        .await
        .expect("第二次刷新");
    assert_ne!(access2, access);
    assert_eq!(refreshes(&mock), 2);
    assert_ne!(
        store.snapshot().unwrap().refresh_token,
        after_first.refresh_token
    );
}

#[tokio::test]
async fn concurrent_requests_trigger_exactly_one_refresh() {
    let mock = MockMicrosoft::start().await;
    let store = signed_in(&mock, 10);
    let manager = manager(&mock, &store);
    let results = futures_join(&manager).await;
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    let tokens: std::collections::BTreeSet<_> = results.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(tokens.len(), 1, "大家拿到的是同一个新令牌");
    assert_eq!(refreshes(&mock), 1, "严格轮换下重复刷新会让后面的失败");
}

async fn futures_join(manager: &TokenManager<'_>) -> Vec<Result<String, StorageError>> {
    let (a, b, c, d) = tokio::join!(
        manager.access_token(),
        manager.access_token(),
        manager.access_token(),
        manager.access_token()
    );
    vec![a, b, c, d]
}

#[tokio::test]
async fn a_token_the_server_rejects_is_force_refreshed_once() {
    let mock = MockMicrosoft::start().await;
    let store = signed_in(&mock, 3600);
    let stale = store.snapshot().unwrap().access_token;
    mock.expire_access_tokens();
    let manager = manager(&mock, &store);
    let fresh = manager.force_refresh(&stale).await.expect("强制刷新");
    assert_ne!(fresh, stale);
    // 别的请求也撞上同一个被拒绝的令牌：存储里已经是新的，不再重复刷新。
    let again = manager.force_refresh(&stale).await.expect("复用");
    assert_eq!(again, fresh);
    assert_eq!(refreshes(&mock), 1);
}

#[tokio::test]
async fn a_revoked_refresh_token_means_sign_in_again() {
    let mock = MockMicrosoft::start().await;
    let store = signed_in(&mock, 10);
    mock.revoke_all();
    let err = manager(&mock, &store)
        .access_token()
        .await
        .expect_err("刷新令牌已被撤销");
    assert_eq!(err, StorageError::AuthFailed(REAUTH_MESSAGE.to_owned()));
    assert_eq!(REAUTH_MESSAGE, "OneDrive 登录已失效，请重新登录");
    assert!(
        store.snapshot().is_some(),
        "不替用户删本机的登录信息（要等用户重新登录或断开）"
    );
}

#[tokio::test]
async fn other_oauth_errors_are_not_mistaken_for_an_expired_login() {
    let mock = MockMicrosoft::start().await;
    let store = signed_in(&mock, 10);
    // 服务暂时不可用：稍后重试，不能暂停自动同步。
    mock.token_unavailable(1);
    let err = manager(&mock, &store)
        .access_token()
        .await
        .expect_err("503");
    assert!(matches!(err, StorageError::Unavailable(_)), "{err:?}");
    // 同一个刷新令牌还能用。
    manager(&mock, &store)
        .access_token()
        .await
        .expect("恢复后正常");

    // 网络不通：同样不是认证失败。
    let dead = TokenManager::new(
        build_client(None).unwrap(),
        zssh_core::sync::onedrive::oauth::Endpoints {
            token: "http://127.0.0.1:1/token".to_owned(),
            ..mock.endpoints()
        },
        CLIENT_ID.to_owned(),
        std::sync::Arc::new(&store),
    );
    store
        .save(&TokenSet {
            expires_at: now_secs() + 5,
            ..store.snapshot().unwrap()
        })
        .unwrap();
    let err = dead.access_token().await.expect_err("连不上");
    assert!(!matches!(err, StorageError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn no_saved_login_asks_the_user_to_sign_in() {
    let mock = MockMicrosoft::start().await;
    let store = MemoryTokenStore::new();
    let err = manager(&mock, &store)
        .access_token()
        .await
        .expect_err("没登录");
    assert!(
        matches!(&err, StorageError::AuthFailed(m) if m.contains("登录")),
        "{err:?}"
    );
    assert!(mock.token_requests().is_empty());
}
