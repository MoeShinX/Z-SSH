//! 集成测试（S-4）：OneDrive 存储实现，对着进程内模拟的微软 Graph 跑。
//!
//! 模拟服务器按官方文档的行为真实执行：`createUploadSession` 的 `If-Match`（对不上 412）和
//! `conflictBehavior`（默认 fail，已存在 409）、分片必须是 320 KiB 的整数倍、上传地址不接受
//! `Authorization`、`/content` 302 到另一个主机名、限流、507 等。两台电脑 = 两个数据目录。
//! 不连任何真实的微软服务。

#[path = "common/fake_browser.rs"]
mod fake_browser;
#[path = "common/harness.rs"]
mod harness;
#[path = "common/mock_cloud.rs"]
mod mock_cloud;
#[path = "common/mock_microsoft.rs"]
mod mock_microsoft;

use std::sync::Arc;
use std::time::Duration;

use fake_browser::FakeBrowser;
use harness::*;
use mock_cloud::{MockHttpProxy, MockSocks5};
use mock_microsoft::{MockMicrosoft, CLIENT_ID};
use tokio::sync::Notify;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::onedrive::graph::{self, OneDriveRaw, FRAGMENT_BYTES, OBJECT_NAME};
use zssh_core::sync::onedrive::login::{login, LoginParams};
use zssh_core::sync::onedrive::oauth::{now_secs, TokenSet, REAUTH_MESSAGE};
use zssh_core::sync::onedrive::tokens::{DbTokenStore, MemoryTokenStore, TokenStore, TOKENS_KEY};
use zssh_core::sync::remote::{ConcurrencyMode, ConditionalStore};
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::storage::{StorageError, SyncStorage};
use zssh_core::sync::SyncError;

type Store<'a> = ConditionalStore<OneDriveRaw<'a>>;

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

fn open<'a>(
    mock: &MockMicrosoft,
    tokens: &'a dyn TokenStore,
    mode: ConcurrencyMode,
    proxy: Option<&SyncProxy>,
) -> Store<'a> {
    graph::open_storage(mock.endpoints(), CLIENT_ID, proxy, mode, Arc::new(tokens))
        .expect("打开存储")
}

fn conditional<'a>(mock: &MockMicrosoft, tokens: &'a dyn TokenStore) -> Store<'a> {
    open(mock, tokens, ConcurrencyMode::Conditional, None)
}

/// 两个最耗时的完整场景（各约十几秒，Argon2 加解密很多轮）归“完整”档：门禁里不跑（五条门禁总耗时
/// 要在 120 秒以内），用 `scripts/live-tests.ps1`（设置 ZSSH_LIVE_FULL=1）跑。别的 OneDrive 用例（含
/// 登录 → 上传 → 第二台电脑 → 刷新 → 断开的端到端）照常在门禁里。
fn full_tier(name: &str) -> bool {
    let on = matches!(std::env::var("ZSSH_LIVE_FULL").as_deref(), Ok("1"));
    if !on {
        println!("跳过 {name}：完整档，用 scripts/live-tests.ps1 跑（ZSSH_LIVE_FULL=1）");
    }
    on
}

fn paths(mock: &MockMicrosoft) -> Vec<String> {
    mock.requests().into_iter().map(|r| r.path).collect()
}

// -- 完整场景 --------------------------------------------------------

#[tokio::test]
async fn onedrive_runs_the_full_scenario_with_real_conditional_writes() {
    if !full_tier("onedrive_runs_the_full_scenario_with_real_conditional_writes") {
        return;
    }
    let mock = MockMicrosoft::start().await;
    let (store_a, store_b) = (signed_in(&mock, 3600), signed_in(&mock, 3600));

    let probe = graph::probe(mock.endpoints(), CLIENT_ID, None, Arc::new(&store_a))
        .await
        .expect("测试连接");
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    let concurrency = probe.concurrency.expect("并发保护检测");
    assert_eq!(
        concurrency.mode,
        ConcurrencyMode::Conditional,
        "{concurrency:?}"
    );
    assert!(
        mock.file_names().is_empty(),
        "测试连接用的临时文件要删掉：{:?}",
        mock.file_names()
    );

    let sa = conditional(&mock, &store_a);
    let sb = conditional(&mock, &store_b);
    full_scenario(
        &sa,
        &sb,
        &|bytes| mock.external_write_after_next_download(OBJECT_NAME, bytes),
        &|| mock.file(OBJECT_NAME),
    )
    .await;

    assert_eq!(
        mock.file_names(),
        vec![OBJECT_NAME.to_owned()],
        "只碰同步包这一个文件"
    );
    assert_eq!(mock.open_sessions(), 0, "上传会话都提交了");

    // 令牌只发给 Graph：上传地址不带 Authorization（带了会 401），下载地址（另一个主机名）也不带。
    for request in mock.requests() {
        if request.path.starts_with("/upload/") || request.path.starts_with("/download/") {
            assert!(!request.has_auth, "{request:?}");
        } else if request.path.starts_with("/v1.0/") {
            assert!(request.has_auth, "{request:?}");
        }
    }
    assert!(
        mock.requests()
            .iter()
            .any(|r| r.path.starts_with("/download/") && r.host.starts_with("localhost:")),
        "下载走的是重定向到另一个主机名的地址"
    );
    // 更新都带了 If-Match；首次上传没带（用 conflictBehavior=fail）。
    let sessions: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("zssh-sync.json:/createUploadSession"))
        .collect();
    assert!(sessions[0].if_match.is_none(), "首次上传不带 If-Match");
    assert!(
        sessions[1..].iter().all(|r| r.if_match.is_some()),
        "更新必须带 If-Match：{sessions:?}"
    );
    assert!(
        sessions.iter().any(|r| r.status == 412),
        "场景里有一次 412（另一台电脑抢先写）"
    );
}

#[tokio::test]
async fn a_server_that_ignores_if_match_is_detected_and_degrades_safely() {
    if !full_tier("a_server_that_ignores_if_match_is_detected_and_degrades_safely") {
        return;
    }
    let mock = MockMicrosoft::start().await;
    mock.set_ignore_if_match(true);
    let (store_a, store_b) = (signed_in(&mock, 3600), signed_in(&mock, 3600));
    let probe = graph::probe(mock.endpoints(), CLIENT_ID, None, Arc::new(&store_a))
        .await
        .expect("测试连接");
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(
        concurrency.mode,
        ConcurrencyMode::ReadBeforeWrite,
        "{concurrency:?}"
    );
    assert!(
        concurrency
            .notes
            .iter()
            .any(|n| n.contains("忽略了 If-Match")),
        "{:?}",
        concurrency.notes
    );
    let sa = open(&mock, &store_a, ConcurrencyMode::ReadBeforeWrite, None);
    let sb = open(&mock, &store_b, ConcurrencyMode::ReadBeforeWrite, None);
    read_before_write_scenario(
        &|bytes| {
            mock.put_file(OBJECT_NAME, bytes);
        },
        &sa,
        &sb,
    )
    .await;
}

// -- 登录 → 首次上传 → 另一台电脑 → 令牌过期自动刷新 → 断开 --------------

#[tokio::test]
async fn login_upload_second_machine_token_refresh_and_disconnect() {
    let mock = MockMicrosoft::start().await;
    let endpoints = mock.endpoints();
    let a = Machine::new("od-e2e-a", "dev-a");
    let b = Machine::new("od-e2e-b", "dev-b");
    seed(&a);
    let store_a = DbTokenStore::new(&a.db, &DpapiSealer);
    let store_b = DbTokenStore::new(&b.db, &DpapiSealer);

    // 1. 两台电脑各自走一遍浏览器授权登录，令牌 DPAPI 加密存进各自的数据库。
    for store in [&store_a, &store_b] {
        let success = login(
            LoginParams {
                client_id: CLIENT_ID,
                endpoints: &endpoints,
                proxy: None,
                timeout: Duration::from_secs(20),
            },
            &FakeBrowser::approving(),
            &Notify::new(),
        )
        .await
        .expect("登录");
        store.save(&success.tokens).expect("存令牌");
    }

    // 2. A 首次上传，B 取回。
    let sa = conditional(&mock, &store_a);
    let sb = conditional(&mock, &store_b);
    sync_ok(&a, &sa, "A 首次上传").await;
    assert!(mock.file(OBJECT_NAME).is_some());
    sync_ok(&b, &sb, "B 取回").await;
    sync_ok(&a, &sa, "A 确认").await;
    assert_same_content(&a, &b, "两台电脑同步之后");

    // 3. 令牌不进同步包 / 离线备份，数据库里是密文。
    let tokens = store_a.load().unwrap().expect("已登录");
    let raw = a.db.setting_get(TOKENS_KEY).unwrap().expect("数据库里有");
    assert!(!raw.contains(&tokens.access_token) && !raw.contains(&tokens.refresh_token));
    assert!(
        !a.content().settings.contains_key(TOKENS_KEY),
        "同步内容里不能有令牌那一项"
    );
    let offline = a.engine().export_offline("offline-password-1").unwrap();
    let package = a.engine().export_package(PASSWORD).unwrap();
    for secret in [&tokens.access_token, &tokens.refresh_token] {
        assert!(
            !String::from_utf8_lossy(&offline).contains(secret.as_str()),
            "离线备份里不能有令牌"
        );
        assert!(
            !String::from_utf8_lossy(&package).contains(secret.as_str()),
            "同步包里不能有令牌"
        );
    }

    // 4. 访问令牌快过期 → 同步前自动用刷新令牌换新，新的刷新令牌存回数据库。
    let before = store_a.load().unwrap().unwrap();
    store_a
        .save(&TokenSet {
            expires_at: now_secs() + 5,
            ..before.clone()
        })
        .unwrap();
    a.add_host("after-refresh");
    sync_ok(&a, &sa, "A 刷新令牌之后").await;
    assert_eq!(refreshes(&mock), 1);
    let after = store_a.load().unwrap().unwrap();
    assert_ne!(after.access_token, before.access_token);
    assert_ne!(
        after.refresh_token, before.refresh_token,
        "刷新令牌被轮换并保存"
    );
    assert!(after.expires_at > now_secs() + 3000);

    // 5. 服务器那边令牌已经过期（401）→ 强制刷新一次，重发请求成功。
    mock.expire_access_tokens();
    a.add_host("after-401");
    sync_ok(&a, &sa, "A 遇到 401 之后").await;
    assert_eq!(refreshes(&mock), 2);
    assert!(
        mock.requests()
            .iter()
            .any(|r| r.status == 401 && r.path.starts_with("/v1.0/")),
        "确实发生过 401"
    );
    sync_ok(&b, &sb, "B 取回").await;
    assert!(b.host_names().contains(&"after-401".to_owned()));

    // 6. 断开连接：本机的令牌删掉，之后不能再同步，也不会去联系微软。
    store_a.clear().unwrap();
    assert_eq!(a.db.setting_get(TOKENS_KEY).unwrap(), None);
    mock.clear_log();
    let err = a
        .engine()
        .sync(&sa, PASSWORD, None)
        .await
        .expect_err("断开之后不能同步");
    assert!(
        matches!(&err, SyncError::AuthFailed(m) if m.contains("登录")),
        "{err:?}"
    );
    assert!(mock.requests().is_empty(), "没有令牌就不发任何请求");
}

#[tokio::test]
async fn a_revoked_login_stops_the_sync_with_an_auth_error() {
    let mock = MockMicrosoft::start().await;
    let a = Machine::new("od-revoked", "dev-a");
    seed(&a);
    let tokens = signed_in(&mock, 10);
    let storage = conditional(&mock, &tokens);
    mock.revoke_all();
    let err = a
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("已撤销");
    // 引擎 / 调度器据此暂停自动同步，界面提示重新登录。
    assert!(
        matches!(&err, SyncError::AuthFailed(m) if m == REAUTH_MESSAGE),
        "{err:?}"
    );
}

// -- 条件写入 --------------------------------------------------------

#[tokio::test]
async fn the_later_writer_gets_a_412_and_the_file_keeps_the_first_writers_content() {
    let mock = MockMicrosoft::start().await;
    let (ta, tb) = (signed_in(&mock, 3600), signed_in(&mock, 3600));
    let (sa, sb) = (conditional(&mock, &ta), conditional(&mock, &tb));
    sa.write(b"v1", None).await.expect("首次上传");
    let (ra, rb) = (
        sa.read().await.unwrap().unwrap(),
        sb.read().await.unwrap().unwrap(),
    );
    assert_eq!(ra.version, rb.version);
    assert!(
        ra.version.starts_with("\"{") && ra.version.ends_with('"'),
        "eTag 原样带引号：{}",
        ra.version
    );

    let second = sa
        .write(b"from-a", Some(&ra.version))
        .await
        .expect("先写的成功");
    assert_ne!(second, ra.version);
    let err = sb
        .write(b"from-b", Some(&rb.version))
        .await
        .expect_err("后写的");
    assert_eq!(err, StorageError::Conflict);
    assert_eq!(
        mock.file(OBJECT_NAME).as_deref(),
        Some(b"from-a".as_slice())
    );
    let rejected = mock
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/createUploadSession") && r.status == 412)
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].if_match.as_deref(), Some(rb.version.as_str()));
    assert_eq!(mock.open_sessions(), 0);
}

#[tokio::test]
async fn a_first_upload_over_an_existing_file_is_a_conflict() {
    let mock = MockMicrosoft::start().await;
    let (ta, tb) = (signed_in(&mock, 3600), signed_in(&mock, 3600));
    let (sa, sb) = (conditional(&mock, &ta), conditional(&mock, &tb));
    sa.write(b"first", None).await.expect("首次上传");
    let err = sb.write(b"second", None).await.expect_err("云端已经有了");
    assert_eq!(err, StorageError::Conflict);
    assert_eq!(mock.file(OBJECT_NAME).as_deref(), Some(b"first".as_slice()));
    let refused = mock
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/createUploadSession") && r.status == 409)
        .expect("会话创建被 409 拒绝");
    assert!(
        refused.if_match.is_none(),
        "首次上传用 conflictBehavior=fail，不带 If-Match"
    );
}

#[tokio::test]
async fn a_file_created_between_session_and_commit_is_a_conflict_not_an_overwrite() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.external_write_after_next_session(OBJECT_NAME, b"other".to_vec());
    let err = storage
        .write(b"mine", None)
        .await
        .expect_err("提交时发现已存在");
    assert_eq!(err, StorageError::Conflict);
    assert_eq!(mock.file(OBJECT_NAME).as_deref(), Some(b"other".as_slice()));
    assert_eq!(mock.open_sessions(), 0);
}

/// 已知的残余窗口（官方文档里 If-Match 只写了在创建会话时检查）：这里如实记下现在的行为，
/// 它不是想要的行为。以后微软文档说明提交时也检查，就把这个用例改成期望 Conflict。
#[tokio::test]
async fn known_window_an_update_overwrites_a_write_made_after_the_session_was_created() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    storage.write(b"v1", None).await.unwrap();
    let version = storage.read().await.unwrap().unwrap().version;
    mock.external_write_after_next_session(OBJECT_NAME, b"other".to_vec());
    storage
        .write(b"mine", Some(&version))
        .await
        .expect("窗口内被覆盖，不报错");
    assert_eq!(mock.file(OBJECT_NAME).as_deref(), Some(b"mine".as_slice()));
}

#[tokio::test]
async fn delete_uses_if_match() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    storage.write(b"v1", None).await.unwrap();
    let version = storage.read().await.unwrap().unwrap().version;
    let etag = mock.put_file(OBJECT_NAME, b"changed".to_vec());
    assert_ne!(etag, version);
    let err = storage.delete(Some(&version)).await.expect_err("版本过期");
    assert_eq!(err, StorageError::Conflict);
    assert!(mock.file(OBJECT_NAME).is_some(), "没删");
    storage.delete(Some(&etag)).await.expect("版本对，删掉");
    assert!(mock.file(OBJECT_NAME).is_none());
    let sent = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "DELETE" && r.path.contains("approot"))
        .collect::<Vec<_>>();
    assert_eq!(sent[0].if_match.as_deref(), Some(version.as_str()));
    assert_eq!(sent[0].status, 412);
    // 已经不存在也算删掉了。
    storage.delete(None).await.expect("幂等");
}

// -- 大文件、读取 ------------------------------------------------------

#[tokio::test]
async fn a_large_package_is_uploaded_in_320_kib_aligned_fragments() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    let size = 2 * FRAGMENT_BYTES + 123;
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    storage.write(&bytes, None).await.expect("上传");
    assert_eq!(mock.file(OBJECT_NAME).as_deref(), Some(bytes.as_slice()));
    let ranges: Vec<String> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .filter_map(|r| r.content_range)
        .collect();
    assert_eq!(
        ranges,
        vec![
            format!("bytes 0-{}/{size}", FRAGMENT_BYTES - 1),
            format!("bytes {FRAGMENT_BYTES}-{}/{size}", 2 * FRAGMENT_BYTES - 1),
            format!("bytes {}-{}/{size}", 2 * FRAGMENT_BYTES, size - 1),
        ]
    );
    let read = storage.read().await.unwrap().unwrap();
    assert_eq!(read.bytes, bytes, "读回来一致");
}

#[tokio::test]
async fn a_remote_file_over_64_mib_is_refused_without_downloading() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.put_file(OBJECT_NAME, vec![0u8; 64 * 1024 * 1024 + 1]);
    let err = storage.read().await.expect_err("太大");
    assert!(err.to_string().contains("64 MiB"), "{err}");
    assert!(
        !paths(&mock).iter().any(|p| p.starts_with("/download/")),
        "超过上限就不下载"
    );
}

#[tokio::test]
async fn reading_when_nothing_is_there_yet_is_not_an_error() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    assert_eq!(conditional(&mock, &tokens).read().await.unwrap(), None);
}

// -- 限流、空间、故障 ----------------------------------------------------

#[tokio::test]
async fn throttling_reports_how_long_to_wait() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.fail_graph(429, Some(37), 1);
    let err = storage.read().await.expect_err("429");
    assert_eq!(
        err,
        StorageError::RateLimited {
            service: "OneDrive".to_owned(),
            retry_after_secs: 37
        }
    );
    assert_eq!(err.to_string(), "OneDrive 限流，约 37 秒后重试");
    mock.fail_graph(503, Some(90), 1);
    let err = storage
        .read()
        .await
        .expect_err("503 + Retry-After 也是限流");
    assert!(
        matches!(
            err,
            StorageError::RateLimited {
                retry_after_secs: 90,
                ..
            }
        ),
        "{err:?}"
    );
    mock.fail_graph(429, None, 1);
    let err = storage.read().await.expect_err("429 没给时间");
    assert!(
        matches!(
            err,
            StorageError::RateLimited {
                retry_after_secs: 60,
                ..
            }
        ),
        "{err:?}"
    );
    mock.fail_graph(503, None, 1);
    let err = storage.read().await.expect_err("503");
    assert!(matches!(err, StorageError::Unavailable(_)), "{err:?}");
    // 过了就恢复正常。
    assert_eq!(storage.read().await.unwrap(), None);
}

#[tokio::test]
async fn the_engine_gives_up_on_throttling_instead_of_retrying() {
    let mock = MockMicrosoft::start().await;
    let a = Machine::new("od-throttle", "dev-a");
    seed(&a);
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.fail_graph(429, Some(45), 1);
    let err = a
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("限流");
    assert!(
        matches!(
            err,
            SyncError::RateLimited {
                retry_after_secs: 45,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(mock.requests().len(), 1, "不重试");
}

#[tokio::test]
async fn a_full_onedrive_is_reported_clearly_and_leaves_no_session() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.set_quota_full(true);
    let err = storage.write(b"x", None).await.expect_err("507");
    assert!(err.to_string().contains("空间不足"), "{err}");
    assert!(err.to_string().contains("507"), "{err}");
    assert_eq!(mock.open_sessions(), 0);
    assert!(mock.file(OBJECT_NAME).is_none());
}

#[tokio::test]
async fn a_failed_fragment_cancels_the_upload_session() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.fail_next_upload_fragment(503);
    let err = storage.write(b"payload", None).await.expect_err("分片失败");
    assert!(matches!(err, StorageError::Unavailable(_)), "{err:?}");
    assert_eq!(mock.open_sessions(), 0, "会话被取消，云端不留半截数据");
    assert!(mock.file(OBJECT_NAME).is_none());
    // 再来一次就成功。
    storage.write(b"payload", None).await.expect("重试成功");
}

#[tokio::test]
async fn authorization_errors_from_graph_ask_for_a_new_login() {
    let mock = MockMicrosoft::start().await;
    let tokens = signed_in(&mock, 3600);
    let storage = conditional(&mock, &tokens);
    mock.fail_graph(403, None, 1);
    let err = storage.read().await.expect_err("403");
    assert!(
        matches!(&err, StorageError::AuthFailed(m) if m.contains("重新登录")),
        "{err:?}"
    );
}

// -- 代理 --------------------------------------------------------------

async fn sync_through(proxy: &SyncProxy, mock: &MockMicrosoft) {
    let machine = Machine::new("od-proxy", "dev-a");
    seed(&machine);
    // 访问令牌快过期：刷新、元数据、上传会话、分片、下载都要经过代理。
    let tokens = signed_in(mock, 5);
    let storage = open(mock, &tokens, ConcurrencyMode::Conditional, Some(proxy));
    sync_ok(&machine, &storage, "经代理首次上传").await;
    sync_ok(&machine, &storage, "经代理再同步").await;
    assert_eq!(refreshes(mock), 1);
    assert!(mock.file(OBJECT_NAME).is_some());
}

#[tokio::test]
async fn all_graph_traffic_goes_through_a_socks5_proxy() {
    let mock = MockMicrosoft::start().await;
    let socks = MockSocks5::start(mock.addr(), None).await;
    let proxy = SyncProxy {
        kind: ProxyScheme::Socks5,
        host: "127.0.0.1".to_owned(),
        port: socks.addr().port(),
        username: None,
        password: None,
    };
    sync_through(&proxy, &mock).await;
    let targets = socks.targets();
    // 连接会复用：接口一个主机名、下载地址一个主机名，至少两条连接。
    assert!(targets.len() >= 2, "{targets:?}");
    assert!(targets.iter().all(|(_, port)| *port == mock.addr().port()));
    assert!(
        targets.iter().any(|(host, _)| host == "localhost"),
        "下载地址也走代理：{targets:?}"
    );
}

#[tokio::test]
async fn all_graph_traffic_goes_through_an_http_proxy_with_auth() {
    let mock = MockMicrosoft::start().await;
    // 测试里的目标是 http:// 地址，代理认证头是随每个请求发的，而 reqwest 跨主机重定向时会去掉
    // Proxy-Authorization；真实的 OneDrive 全是 https://，认证在 CONNECT 里，不受影响。
    // 所以这个用例让下载地址和接口用同一个主机名。
    mock.set_download_same_host(true);
    let server = MockHttpProxy::start(
        mock.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let proxy = SyncProxy {
        kind: ProxyScheme::Http,
        host: "127.0.0.1".to_owned(),
        port: server.addr().port(),
        username: Some("proxy-user".to_owned()),
        password: Some("proxy-pass".to_owned()),
    };
    sync_through(&proxy, &mock).await;
    let seen = server.requests();
    assert!(seen.len() >= 5, "{seen:?}");
    assert!(seen.iter().all(|r| r.authorized));
}

#[tokio::test]
async fn a_wrong_proxy_password_is_not_mistaken_for_a_bad_login() {
    let mock = MockMicrosoft::start().await;
    let server = MockHttpProxy::start(
        mock.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let proxy = SyncProxy {
        kind: ProxyScheme::Http,
        host: "127.0.0.1".to_owned(),
        port: server.addr().port(),
        username: Some("proxy-user".to_owned()),
        password: Some("wrong".to_owned()),
    };
    let tokens = signed_in(&mock, 3600);
    let storage = open(&mock, &tokens, ConcurrencyMode::Conditional, Some(&proxy));
    let err = storage.read().await.expect_err("代理拒绝");
    assert!(
        !matches!(err, StorageError::AuthFailed(_)),
        "代理的 407 不能让自动同步暂停：{err:?}"
    );
    assert!(err.to_string().contains("代理"), "{err}");
}
