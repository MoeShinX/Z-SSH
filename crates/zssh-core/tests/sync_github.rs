//! 集成测试（S-3）：GitHub Gist 存储，对着进程内模拟的 GitHub 接口跑完整同步。
//!
//! 不碰真实的 GitHub。两个数据目录模拟两台电脑，每一轮比较完整的同步内容；
//! 请求全部走 `net::build_client`（默认代理设置），经 SOCKS5 和 HTTP 代理各测一次。

#[path = "common/harness.rs"]
mod harness;
#[path = "common/mock_cloud.rs"]
mod mock_cloud;
#[path = "common/mock_github.rs"]
mod mock_github;

use std::sync::Mutex;

use harness::*;
use mock_cloud::{MockHttpProxy, MockSocks5};
use mock_github::{GithubOptions, MockGithub, RateSpec};
use zssh_core::sync::engine::SyncStatus;
use zssh_core::sync::github::{self, GithubConfig, FILE_NAME, GIST_DESCRIPTION, PLACEHOLDER};
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::service::{self, StorageConfig, SyncContext};
use zssh_core::sync::snapshot::snapshot_local;
use zssh_core::sync::storage::{StorageError, SyncStorage};
use zssh_core::sync::SyncError;

const TOKEN: &str = "ghp_mocktoken_not_a_real_token_0123456789";
const LOGIN: &str = "demo-user";

async fn start() -> MockGithub {
    MockGithub::start(GithubOptions::classic(TOKEN, LOGIN)).await
}

fn config(gh: &MockGithub, gist_id: &str) -> GithubConfig {
    GithubConfig {
        api_base: gh.api_base(),
        token: TOKEN.to_owned(),
        gist_id: gist_id.to_owned(),
        // 模拟服务是 http://，要确认过才允许。
        allow_insecure_http: true,
    }
}

fn storage(gh: &MockGithub, gist_id: &str) -> github::GithubApi {
    github::open_storage(&config(gh, gist_id), None).expect("打开存储")
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("同步包是 UTF-8 文本")
}

// ---------------------------------------------------------------------------
// 新建 Gist / 读写的基本行为

#[tokio::test]
async fn creating_a_gist_makes_a_secret_gist_with_only_the_placeholder_file() {
    let gh = start().await;
    let id = github::create_gist(&config(&gh, ""), None)
        .await
        .expect("新建");
    assert_eq!(gh.gist_ids(), vec![id.clone()]);
    assert_eq!(gh.gist_is_public(&id), Some(false), "必须是 secret Gist");
    assert_eq!(gh.gist_description(&id).as_deref(), Some(GIST_DESCRIPTION));
    let files = gh.gist_files(&id);
    assert_eq!(files.keys().collect::<Vec<_>>(), vec![FILE_NAME]);
    assert_eq!(files[FILE_NAME], PLACEHOLDER);

    // 请求头：令牌、Accept、API 版本、User-Agent（GitHub 拒绝没有 UA 的请求）。
    let post = gh
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .expect("POST /gists");
    assert!(post.has_auth_header);
    assert!(post.accept.as_deref().unwrap_or("").contains("github+json"));
    assert_eq!(post.api_version.as_deref(), Some("2022-11-28"));
    assert!(post
        .user_agent
        .as_deref()
        .unwrap_or("")
        .starts_with("Z-SSH/"));

    // 占位内容 = 还没有同步数据，不会被当成坏掉的同步包。
    assert!(storage(&gh, &id).read().await.expect("读").is_none());
}

#[tokio::test]
async fn write_rereads_before_and_after_and_a_stale_version_changes_nothing() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[("notes.txt", "别的文件")]);
    let store = storage(&gh, &id);
    gh.clear_log();

    // 第一次：云端没有同步包 → expected None。写 = 写前再读 + PATCH + 写完再读确认。
    let v1 = store.write(b"{\"n\":1}", None).await.expect("首次写");
    let methods: Vec<String> = gh.api_requests().into_iter().map(|r| r.method).collect();
    assert_eq!(methods, vec!["GET", "PATCH", "GET"], "前后各再读一次");
    assert_eq!(Some(v1.clone()), gh.latest_version(&id));
    assert_eq!(gh.gist_file(&id, FILE_NAME).as_deref(), Some("{\"n\":1}"));

    // 版本号是 history 里最新的那个，读出来和写出来的一致。
    let read = store.read().await.unwrap().expect("有同步包");
    assert_eq!(read.version, v1);
    assert_eq!(read.bytes, b"{\"n\":1}");

    // 调用方以为云端还没有，其实已经有了 → 冲突，什么都没改。
    let count = gh.version_count(&id);
    assert_eq!(
        store.write(b"{\"n\":2}", None).await.unwrap_err(),
        StorageError::Conflict
    );
    // 版本号过期（别人改过）→ 冲突，什么都没改。
    gh.external_write(&id, FILE_NAME, "{\"n\":\"someone-else\"}");
    let count_after_external = gh.version_count(&id);
    assert_eq!(count_after_external, count + 1);
    assert_eq!(
        store.write(b"{\"n\":3}", Some(&v1)).await.unwrap_err(),
        StorageError::Conflict
    );
    assert_eq!(gh.version_count(&id), count_after_external, "冲突时不能写");
    assert_eq!(
        gh.gist_file(&id, FILE_NAME).as_deref(),
        Some("{\"n\":\"someone-else\"}")
    );

    // 带上最新的版本号才能写；Gist 里别的文件始终没动。
    let latest = store.read().await.unwrap().unwrap().version;
    let v3 = store
        .write(b"{\"n\":4}", Some(&latest))
        .await
        .expect("版本对上");
    assert_eq!(Some(v3), gh.latest_version(&id));
    assert_eq!(gh.gist_file(&id, "notes.txt").as_deref(), Some("别的文件"));
}

#[tokio::test]
async fn delete_only_removes_the_sync_file_and_checks_the_version() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[("notes.txt", "keep me")]);
    let store = storage(&gh, &id);
    let version = store.write(b"{\"n\":1}", None).await.unwrap();
    assert_eq!(
        store.delete(Some("not-the-version")).await.unwrap_err(),
        StorageError::Conflict
    );
    assert!(gh.gist_file(&id, FILE_NAME).is_some());
    store.delete(Some(&version)).await.expect("版本对，删掉");
    assert!(gh.gist_file(&id, FILE_NAME).is_none());
    assert_eq!(gh.gist_file(&id, "notes.txt").as_deref(), Some("keep me"));
    assert!(store.read().await.unwrap().is_none());

    // Gist 里只剩这一个文件时 GitHub 不允许删空：给出能看懂的原因。
    let only = gh.add_gist(LOGIN, &[(FILE_NAME, "{\"n\":1}")]);
    let only_store = storage(&gh, &only);
    let v = only_store.read().await.unwrap().unwrap().version;
    let err = only_store.delete(Some(&v)).await.unwrap_err();
    assert!(err.to_string().contains("422"), "{err}");
}

// ---------------------------------------------------------------------------
// 完整同步场景

#[tokio::test]
async fn full_scenario_on_a_gist_with_another_file_that_stays_untouched() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[("notes.txt", "my notes")]);
    let sa = storage(&gh, &id);
    let sb = storage(&gh, &id);
    full_scenario(
        &sa,
        &sb,
        // “写之前另一台电脑抢先写”：落在引擎读云端和“写前再读”之间。
        &|bytes| gh.external_write_after_next_get(&id, FILE_NAME, &text(bytes)),
        &|| gh.gist_file(&id, FILE_NAME).map(String::into_bytes),
    )
    .await;

    assert_eq!(
        gh.gist_file(&id, "notes.txt").as_deref(),
        Some("my notes"),
        "只动 zssh-sync.json，别的文件一个字都不能变"
    );
    let mut names: Vec<String> = gh.gist_files(&id).into_keys().collect();
    names.sort();
    assert_eq!(names, vec!["notes.txt", FILE_NAME]);
    let methods: Vec<String> = gh.api_requests().into_iter().map(|r| r.method).collect();
    assert!(methods.iter().any(|m| m == "PATCH"));
    assert!(
        methods.iter().all(|m| m == "GET" || m == "PATCH"),
        "同步从不创建或删除 Gist：{methods:?}"
    );
    // 令牌没有出现在任何地方：只在 Authorization 头里。
    assert!(!gh.gist_file(&id, FILE_NAME).unwrap().contains("ghp_"));
}

#[tokio::test]
async fn a_new_gist_works_as_the_first_sync_target_and_a_second_computer_joins_it() {
    let gh = start().await;
    let id = github::create_gist(&config(&gh, ""), None)
        .await
        .expect("新建");
    let a = Machine::new("gh-join-a", "dev-a");
    let b = Machine::new("gh-join-b", "dev-b");
    seed(&a);
    sync_ok(&a, &storage(&gh, &id), "A 首次（占位内容当作没有数据）").await;
    sync_ok(&b, &storage(&gh, &id), "B 填同一个 Gist ID").await;
    sync_ok(&a, &storage(&gh, &id), "A 再同步").await;
    assert_same_content(&a, &b, "第二台电脑填 ID 之后");
    assert_ne!(gh.gist_file(&id, FILE_NAME).as_deref(), Some(PLACEHOLDER));
}

#[tokio::test]
async fn a_change_between_the_engine_read_and_the_write_is_detected() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let sa = storage(&gh, &id);
    let sb = storage(&gh, &id);
    read_before_write_scenario(
        &|bytes| gh.external_write(&id, FILE_NAME, &text(bytes)),
        &sa,
        &sb,
    )
    .await;
}

#[tokio::test]
async fn an_overwrite_right_after_our_write_is_caught_by_the_confirmation_read() {
    let gh = start().await;
    let id = github::create_gist(&config(&gh, ""), None).await.unwrap();
    let a = Machine::new("gh-confirm-a", "dev-a");
    let b = Machine::new("gh-confirm-b", "dev-b");
    seed(&a);
    let sa = storage(&gh, &id);
    let sb = storage(&gh, &id);
    sync_ok(&a, &sa, "A 首次").await;
    sync_ok(&b, &sb, "B 首次").await;
    sync_ok(&a, &sa, "A").await;

    // B 加一台主机并导出它的同步包；A 也加一台。A 的 PATCH 刚落地，B 的内容就紧跟着盖上来。
    b.add_host("b-host");
    let b_package = text(b.engine().export_package(PASSWORD).unwrap());
    a.add_host("a-host");
    gh.external_write_after_next_patch(&id, FILE_NAME, &b_package);
    let report = a
        .engine()
        .sync(&sa, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("写完再读发现被盖掉后重试");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(
        report.retries >= 1,
        "写完再读发现最新版本不是自己写的，要重试：{}",
        report.retries
    );
    assert!(a.host_names().contains(&"b-host".to_owned()));
    assert!(a.host_names().contains(&"a-host".to_owned()));
    sync_ok(&b, &sb, "B").await;
    sync_ok(&a, &sa, "A").await;
    assert_same_content(&a, &b, "被盖掉又重试之后");
    assert!(b.host_names().contains(&"a-host".to_owned()));
}

// ---------------------------------------------------------------------------
// 内容被截断 → 改读 raw_url

#[tokio::test]
async fn truncated_content_is_read_from_the_raw_url() {
    let gh = start().await;
    // 同步包有几 KB，超过 400 字节的内容全部被截断。
    gh.set_truncate_over(Some(400));
    let id = gh.add_gist(LOGIN, &[]);
    let a = Machine::new("gh-trunc-a", "dev-a");
    let b = Machine::new("gh-trunc-b", "dev-b");
    seed(&a);
    sync_ok(&a, &storage(&gh, &id), "A").await;
    sync_ok(&b, &storage(&gh, &id), "B（只能靠 raw_url 读到完整内容）").await;
    sync_ok(&a, &storage(&gh, &id), "A").await;
    assert_same_content(&a, &b, "截断之后");

    let raw = gh.raw_requests();
    assert!(!raw.is_empty(), "内容被截断时必须去读 raw_url");
    // raw_url 和接口是同一个主机时才带令牌。
    assert!(raw.iter().all(|r| r.has_auth_header));
    // 读出来的和云端的完整内容一致（不是被截断的前 400 字节）。
    let read = storage(&gh, &id).read().await.unwrap().unwrap();
    assert_eq!(
        read.bytes,
        gh.gist_file(&id, FILE_NAME).unwrap().into_bytes()
    );
    assert!(read.bytes.len() > 400);
}

#[tokio::test]
async fn a_short_raw_download_is_rejected_by_the_size_check() {
    let gh = start().await;
    gh.set_truncate_over(Some(100));
    let id = gh.add_gist(LOGIN, &[]);
    let long = format!("{{\"data\":\"{}\"}}", "x".repeat(600));
    gh.external_write(&id, FILE_NAME, &long);
    let store = storage(&gh, &id);
    assert_eq!(store.read().await.unwrap().unwrap().bytes, long.as_bytes());

    gh.set_raw_cut(Some(300));
    let err = store.read().await.unwrap_err();
    assert!(err.to_string().contains("不完整"), "{err}");
}

#[tokio::test]
async fn raw_url_on_another_host_does_not_get_the_token() {
    // api 地址和 raw_url 不是同一个主机（GitHub.com 就是这样：api.github.com 和
    // gist.githubusercontent.com）：令牌只能发给接口主机。经 HTTP 代理访问两个不存在的域名。
    let mut options = GithubOptions::classic(TOKEN, LOGIN);
    options.raw_host = Some("gist.zssh-test.invalid:8080".to_owned());
    let gh = MockGithub::start(options).await;
    gh.set_truncate_over(Some(200));
    let proxy = MockHttpProxy::start(gh.addr(), None).await;
    let proxied = SyncProxy {
        kind: ProxyScheme::Http,
        host: "127.0.0.1".to_owned(),
        port: proxy.addr().port(),
        username: None,
        password: None,
    };
    let cfg = GithubConfig {
        api_base: "http://api.zssh-test.invalid:8080".to_owned(),
        token: TOKEN.to_owned(),
        gist_id: String::new(),
        allow_insecure_http: true,
    };
    let id = github::create_gist(&cfg, Some(&proxied)).await.unwrap();
    let cfg = GithubConfig {
        gist_id: id.clone(),
        ..cfg
    };
    let store = github::open_storage(&cfg, Some(&proxied)).unwrap();
    let long = format!("{{\"data\":\"{}\"}}", "y".repeat(500));
    store.write(long.as_bytes(), None).await.expect("写");
    let read = store.read().await.unwrap().unwrap();
    assert_eq!(read.bytes, long.as_bytes());

    let raw = gh.raw_requests();
    assert!(!raw.is_empty());
    assert!(
        raw.iter().all(|r| !r.has_auth_header),
        "raw_url 在别的主机上，不能带令牌：{raw:?}"
    );
    assert!(gh.api_requests().iter().all(|r| r.has_auth_header));
    let targets = proxy.authorized_targets();
    assert!(targets.contains(&"api.zssh-test.invalid:8080".to_owned()));
    assert!(targets.contains(&"gist.zssh-test.invalid:8080".to_owned()));
}

// ---------------------------------------------------------------------------
// 限流

#[tokio::test]
async fn rate_limiting_stops_the_sync_without_retrying() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let store = storage(&gh, &id);
    let a = Machine::new("gh-rate-a", "dev-a");
    seed(&a);

    for (spec, expect_secs, expect_text) in [
        (RateSpec::RetryAfter(120), 120..=120, "约 2 分钟"),
        (RateSpec::ResetIn(300), 295..=300, "约 5 分钟"),
        (RateSpec::Status429, 60..=60, "约 1 分钟"),
    ] {
        gh.clear_log();
        gh.rate_limit_next(1, spec);
        let err = a
            .engine()
            .sync(&store, PASSWORD, None)
            .await
            .expect_err("限流要报错");
        match &err {
            SyncError::RateLimited {
                service,
                retry_after_secs,
            } => {
                assert_eq!(service, "GitHub");
                assert!(
                    expect_secs.contains(retry_after_secs),
                    "{spec:?}：等待 {retry_after_secs} 秒"
                );
            }
            other => panic!("应该是限流错误：{other:?}"),
        }
        let message = err.to_string();
        assert!(message.contains("GitHub 限流"), "{message}");
        assert!(message.contains(expect_text), "{message}");
        assert_eq!(
            gh.api_requests().len(),
            1,
            "遇到限流不能再发请求（不连续重试）：{:?}",
            gh.api_requests()
        );
    }
    // 限流过去之后同步照常。
    sync_ok(&a, &store, "限流过去之后").await;
}

#[tokio::test]
async fn rate_limiting_in_the_middle_of_a_write_leaves_the_gist_untouched() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let inner = storage(&gh, &id);
    let a = Machine::new("gh-rate-mid", "dev-a");
    seed(&a);
    let before = gh.version_count(&id);

    // 引擎读完云端之后、“写前再读”之前被限流。
    let hooked = AfterRead {
        inner: &inner,
        hook: Mutex::new(Some(Box::new(|| {
            gh.rate_limit_next(1, RateSpec::RetryAfter(90));
        }))),
    };
    gh.clear_log();
    let err = a
        .engine()
        .sync(&hooked, PASSWORD, None)
        .await
        .expect_err("写前再读被限流");
    assert!(matches!(err, SyncError::RateLimited { .. }), "{err:?}");
    assert_eq!(gh.version_count(&id), before, "没有写进去任何东西");
    assert!(
        gh.api_requests().iter().all(|r| r.method == "GET"),
        "没有 PATCH：{:?}",
        gh.api_requests()
    );
    // 基线没存：下次同步照常把本地内容传上去。
    sync_ok(&a, &inner, "之后").await;
    assert!(gh.gist_file(&id, FILE_NAME).is_some());
}

#[tokio::test]
async fn temporary_server_errors_are_retried_but_not_forever() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let store = storage(&gh, &id);
    let a = Machine::new("gh-503", "dev-a");
    seed(&a);
    gh.fail_next(503, 2);
    sync_ok(&a, &store, "503 两次之后成功").await;

    gh.fail_next(503, 30);
    let err = a
        .engine()
        .sync(&store, PASSWORD, None)
        .await
        .expect_err("一直 503");
    assert!(err.to_string().contains("暂时不可用"), "{err}");
}

// ---------------------------------------------------------------------------
// 测试连接 / 令牌 / 权限 / 别人的 Gist

#[tokio::test]
async fn connection_test_with_a_classic_token_checks_scopes_without_writing() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    gh.clear_log();
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(!probe.has_existing_data);
    assert!(probe.write.message.contains("经典令牌"));
    // 经典令牌看权限范围就够了，不需要真的写任何东西。
    assert!(gh.api_requests().iter().all(|r| r.method == "GET"));
    assert_eq!(gh.gist_ids(), vec![id.clone()]);
    // GitHub 不支持条件写入：如实报告。
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(
        concurrency.mode,
        zssh_core::sync::remote::ConcurrencyMode::ReadBeforeWrite
    );
    assert!(concurrency.notes[0].contains("不支持"));

    // 里面有同步包时第二台电脑要输入主密码解开。
    gh.external_write(&id, FILE_NAME, "{\"format\":\"zssh-sync\"}");
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(probe.has_existing_data);
}

#[tokio::test]
async fn connection_test_with_a_fine_grained_token_proves_write_with_a_temporary_gist() {
    let gh = MockGithub::start(GithubOptions::fine_grained(TOKEN, LOGIN)).await;
    let id = gh.add_gist(LOGIN, &[]);
    gh.clear_log();
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(probe.write.message.contains("临时 Gist"), "{probe:?}");
    let methods: Vec<String> = gh.api_requests().into_iter().map(|r| r.method).collect();
    assert!(methods.contains(&"POST".to_owned()) && methods.contains(&"DELETE".to_owned()));
    assert_eq!(
        gh.gist_ids(),
        vec![id],
        "临时 Gist 已经删掉，原来的 Gist 没动"
    );
}

#[tokio::test]
async fn a_read_only_fine_grained_token_is_told_it_cannot_write() {
    let gh = MockGithub::start(GithubOptions::fine_grained(TOKEN, LOGIN)).await;
    let id = gh.add_gist(LOGIN, &[]);
    gh.set_write_allowed(false);
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(probe.read.ok, "{probe:?}");
    assert!(!probe.write.ok);
    assert!(probe.write.message.contains("Gist"), "{probe:?}");
    assert!(gh.gist_ids() == vec![id.clone()], "没有留下任何 Gist");

    // 同步写入时同样给出能看懂的原因。
    let err = storage(&gh, &id).write(b"{}", None).await.unwrap_err();
    assert!(err.to_string().contains("权限"), "{err}");
}

#[tokio::test]
async fn a_classic_token_without_the_gist_scope_is_told_which_scope_is_missing() {
    let mut options = GithubOptions::classic(TOKEN, LOGIN);
    options.scopes = Some("repo, read:user".to_owned());
    let gh = MockGithub::start(options).await;
    let id = gh.add_gist(LOGIN, &[]);
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("没有 gist 权限"), "{probe:?}");
    assert!(probe.read.message.contains("repo"), "{probe:?}");
    assert!(!probe.write.ok);
}

#[tokio::test]
async fn a_revoked_or_expired_token_says_so_clearly() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    gh.revoke_token();
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(
        probe.read.message.contains("令牌已失效，请重新填写"),
        "{probe:?}"
    );

    let err = storage(&gh, &id).read().await.unwrap_err();
    assert!(err.to_string().contains("令牌已失效，请重新填写"), "{err}");
    let err = storage(&gh, &id).write(b"{}", None).await.unwrap_err();
    assert!(err.to_string().contains("令牌已失效"), "{err}");
    assert!(!err.to_string().contains(TOKEN));
}

#[tokio::test]
async fn a_missing_gist_is_reported_as_not_found() {
    let gh = start().await;
    let missing = "mock0000000000000000000000009999";
    let probe = github::probe(&config(&gh, missing), None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("找不到这个 Gist"), "{probe:?}");
    let err = storage(&gh, missing).read().await.unwrap_err();
    assert!(err.to_string().contains("找不到这个 Gist"), "{err}");
}

#[tokio::test]
async fn someone_elses_gist_can_be_read_but_the_user_is_told_it_cannot_be_written() {
    let gh = start().await;
    let id = gh.add_gist("another-user", &[(FILE_NAME, "{\"format\":\"zssh-sync\"}")]);
    let probe = github::probe(&config(&gh, &id), None).await.unwrap();
    assert!(probe.read.ok, "{probe:?}");
    assert!(probe.has_existing_data);
    assert!(!probe.write.ok);
    assert!(
        probe.write.message.contains("不属于这个令牌的用户"),
        "{probe:?}"
    );
    assert!(
        !probe.write.message.contains("another-user"),
        "提示里不写别人的用户名"
    );

    // 真的去写：GitHub 回 404，要翻成同样的话，不能说成“Gist 不存在”。
    let store = storage(&gh, &id);
    let version = store.read().await.unwrap().unwrap().version;
    let err = store.write(b"{}", Some(&version)).await.unwrap_err();
    assert!(err.to_string().contains("不属于这个令牌的用户"), "{err}");
    assert_eq!(
        gh.gist_file(&id, FILE_NAME).as_deref(),
        Some("{\"format\":\"zssh-sync\"}"),
        "别人的 Gist 没被改"
    );
}

// ---------------------------------------------------------------------------
// 接口地址可配置 / 令牌策略

#[tokio::test]
async fn the_api_address_is_configurable_like_github_enterprise() {
    let mut options = GithubOptions::classic(TOKEN, LOGIN);
    options.prefix = "/api/v3".to_owned();
    let gh = MockGithub::start(options).await;
    assert!(gh.api_base().ends_with("/api/v3"));
    let id = gh.add_gist(LOGIN, &[]);
    let a = Machine::new("gh-ghe", "dev-a");
    seed(&a);
    sync_ok(&a, &storage(&gh, &id), "带路径前缀的接口地址").await;
    assert!(gh.gist_file(&id, FILE_NAME).is_some());
    assert!(gh
        .api_requests()
        .iter()
        .all(|r| r.path.starts_with("/api/v3/")));

    // 填成 GitHub.com 的默认地址（不是这个服务）就连不上，不会悄悄走别的地方。
    let wrong = GithubConfig {
        api_base: format!("{}/", gh.base_url()),
        ..config(&gh, &id)
    };
    let probe = github::probe(&wrong, None).await.unwrap();
    assert!(!probe.read.ok, "地址里少了 /api/v3：{probe:?}");
}

#[tokio::test]
async fn http_api_addresses_need_the_user_to_confirm_the_plaintext_token() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let mut cfg = config(&gh, &id);
    cfg.allow_insecure_http = false;
    let err = github::open_storage(&cfg, None).err().expect("没确认");
    assert!(err.to_string().contains("明文"), "{err}");
    assert!(
        gh.requests().is_empty(),
        "没确认之前不能向服务发过任何带令牌的请求"
    );
}

// ---------------------------------------------------------------------------
// 代理：SOCKS5 和 HTTP 各一次（用不存在的域名，只有真的经过代理才连得上）

const FAKE_API: &str = "http://api.zssh-test.invalid:8080";

fn through(proxy_kind: ProxyScheme, port: u16, auth: Option<(&str, &str)>) -> SyncProxy {
    SyncProxy {
        kind: proxy_kind,
        host: "127.0.0.1".to_owned(),
        port,
        username: auth.map(|(u, _)| u.to_owned()),
        password: auth.map(|(_, p)| p.to_owned()),
    }
}

fn fake_config(gist_id: &str) -> GithubConfig {
    GithubConfig {
        api_base: FAKE_API.to_owned(),
        token: TOKEN.to_owned(),
        gist_id: gist_id.to_owned(),
        allow_insecure_http: true,
    }
}

#[tokio::test]
async fn github_goes_through_a_socks5_proxy_with_authentication() {
    let gh = start().await;
    let proxy = MockSocks5::start(
        gh.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let proxied = through(
        ProxyScheme::Socks5,
        proxy.addr().port(),
        Some(("proxy-user", "proxy-pass")),
    );

    let id = github::create_gist(&fake_config(""), Some(&proxied))
        .await
        .expect("经 SOCKS5 新建 Gist");
    let probe = github::probe(&fake_config(&id), Some(&proxied))
        .await
        .unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    let store = github::open_storage(&fake_config(&id), Some(&proxied)).unwrap();
    let a = Machine::new("gh-socks", "dev-a");
    seed(&a);
    sync_ok(&a, &store, "经 SOCKS5 同步").await;
    assert!(gh.gist_file(&id, FILE_NAME).is_some());
    assert!(
        proxy
            .targets()
            .iter()
            .all(|(host, port)| host == "api.zssh-test.invalid" && *port == 8080)
            && !proxy.targets().is_empty(),
        "所有请求都经过代理：{:?}",
        proxy.targets()
    );

    // 不走代理连不上同一个地址；代理密码错也连不上。
    let direct = github::probe(&fake_config(&id), None).await.unwrap();
    assert!(!direct.read.ok, "{direct:?}");
    let wrong = through(
        ProxyScheme::Socks5,
        proxy.addr().port(),
        Some(("proxy-user", "bad")),
    );
    let bad = github::probe(&fake_config(&id), Some(&wrong))
        .await
        .unwrap();
    assert!(!bad.read.ok);
    assert!(bad.read.message.contains("无法连接"), "{bad:?}");
}

#[tokio::test]
async fn github_goes_through_an_http_proxy_with_authentication() {
    let gh = start().await;
    gh.set_truncate_over(Some(300)); // 顺带让 raw_url 也走一遍代理
    let proxy = MockHttpProxy::start(
        gh.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let proxied = through(
        ProxyScheme::Http,
        proxy.addr().port(),
        Some(("proxy-user", "proxy-pass")),
    );

    let id = github::create_gist(&fake_config(""), Some(&proxied))
        .await
        .expect("经 HTTP 代理新建 Gist");
    let store = github::open_storage(&fake_config(&id), Some(&proxied)).unwrap();
    let a = Machine::new("gh-httpproxy-a", "dev-a");
    let b = Machine::new("gh-httpproxy-b", "dev-b");
    seed(&a);
    sync_ok(&a, &store, "经 HTTP 代理同步").await;
    sync_ok(&b, &store, "第二台电脑").await;
    sync_ok(&a, &store, "A").await;
    assert_same_content(&a, &b, "经 HTTP 代理");
    assert_eq!(
        proxy.authorized_targets(),
        vec!["api.zssh-test.invalid:8080".to_owned()]
    );
    let methods: Vec<String> = proxy.requests().into_iter().map(|r| r.method).collect();
    assert!(methods.iter().any(|m| m == "PATCH") && methods.iter().any(|m| m == "POST"));
    assert!(
        !gh.raw_requests().is_empty(),
        "raw_url 也要经过代理（截断时读它）"
    );

    // 不走代理连不上；代理密码错：407，提示去检查代理，不是令牌的问题。
    assert!(
        !github::probe(&fake_config(&id), None)
            .await
            .unwrap()
            .read
            .ok
    );
    let wrong = through(
        ProxyScheme::Http,
        proxy.addr().port(),
        Some(("proxy-user", "bad")),
    );
    let bad = github::probe(&fake_config(&id), Some(&wrong))
        .await
        .unwrap();
    assert!(!bad.read.ok);
    assert!(bad.read.message.contains("代理"), "{bad:?}");
    assert!(!bad.read.message.contains("令牌"), "{bad:?}");
    assert!(proxy.requests().iter().any(|r| !r.authorized));
}

// ---------------------------------------------------------------------------
// 设置页背后的服务层：令牌怎么存、怎么显示、会不会进同步包

fn context(machine: &Machine) -> SyncContext<'_> {
    SyncContext {
        db: &machine.db,
        sealer: &DpapiSealer,
        device_id: machine.device.to_owned(),
        data_dir: machine.dir.clone(),
        known_hosts_path: Some(machine.dir.join("known_hosts")),
        app_version: "0.1.0".to_owned(),
        onedrive_endpoints: zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    }
}

#[tokio::test]
async fn the_token_is_stored_sealed_never_shown_and_never_synced() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let a = Machine::new("gh-token", "dev-a");
    seed(&a);
    let saved = StorageConfig::Github(config(&gh, &id));
    service::save_config(&a.db, &DpapiSealer, &saved).expect("保存");

    // 设置表里是 DPAPI 封装后的密文：找不到令牌，也找不到接口地址和 Gist ID。
    let stored = a.db.setting_get(service::STORAGE_KEY).unwrap().unwrap();
    for needle in [TOKEN, "ghp_", &id, "127.0.0.1"] {
        assert!(
            !stored.contains(needle),
            "设置表里不该出现 {needle}：{stored}"
        );
    }
    assert_eq!(
        service::load_config(&a.db, &DpapiSealer).unwrap().unwrap(),
        saved,
        "读回来要一模一样"
    );

    // 界面拿到的视图：只知道“有令牌”，拿不到内容。
    let view = saved.view();
    assert_eq!(view.provider, "github");
    assert_eq!(view.gist_id, id);
    assert!(view.has_secret);
    let shown = serde_json::to_string(&view).unwrap();
    assert!(!shown.contains(TOKEN) && !shown.contains("ghp_"), "{shown}");
    let debug = format!("{saved:?}");
    assert!(!debug.contains("ghp_"), "{debug}");

    // 表单里令牌留空 = 沿用已保存的；填了新的 = 用新的。
    let mut blank = StorageConfig::Github(GithubConfig {
        token: String::new(),
        ..config(&gh, &id)
    });
    blank.merge_saved_secret(Some(&saved));
    assert_eq!(blank, saved);
    let mut replaced = StorageConfig::Github(GithubConfig {
        token: "ghp_new".to_owned(),
        ..config(&gh, &id)
    });
    replaced.merge_saved_secret(Some(&saved));
    assert!(matches!(&replaced, StorageConfig::Github(c) if c.token == "ghp_new"));

    // 令牌不进同步包：设置键 sync_storage 是 LocalOnly，同步包的明文里没有令牌。
    let snapshot = snapshot_local(&a.db, &DpapiSealer, "dev-a", None, 1).unwrap();
    let plain = serde_json::to_string(&snapshot).unwrap();
    assert!(!plain.contains(TOKEN) && !plain.contains("ghp_"));
    assert!(
        !plain.contains(&id),
        "Gist ID 也是每台电脑自己填的，不进同步包"
    );
    let sealed = snapshot.seal(PASSWORD).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("ghp_"));
}

#[tokio::test]
async fn saved_github_settings_drive_a_sync_and_github_is_never_marked_as_protected() {
    let gh = start().await;
    let id = github::create_gist(&config(&gh, ""), None).await.unwrap();
    let saved = StorageConfig::Github(config(&gh, &id));
    assert!(saved.never_protected(), "GitHub 天生不支持条件写入");
    assert!(!saved.is_plain_http() || config(&gh, &id).allow_insecure_http);

    let a = Machine::new("gh-svc-a", "dev-a");
    let b = Machine::new("gh-svc-b", "dev-b");
    seed(&a);
    // 种子数据里的默认代理是个不存在的地址；服务层会按默认代理设置发请求，这里要直连模拟服务。
    a.db.set_default_proxy_id(None).unwrap();
    for machine in [&a, &b] {
        service::save_config(&machine.db, &DpapiSealer, &saved).unwrap();
    }
    let probe = service::test_connection(
        &saved,
        None,
        std::sync::Arc::new(zssh_core::sync::onedrive::tokens::MemoryTokenStore::new()),
        &zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    )
    .await;
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    let concurrency = probe.concurrency.expect("检测结果");
    assert!(!concurrency.mode.is_protected());
    assert!(concurrency.notes[0].contains("不支持"));

    let report = context(&a).run_sync(PASSWORD, None).await.expect("A");
    assert_eq!(report.status, SyncStatus::Completed);
    context(&b).run_sync(PASSWORD, None).await.expect("B");
    context(&a)
        .run_sync(PASSWORD, None)
        .await
        .expect("A 再同步");
    assert_same_content(&a, &b, "经保存的 GitHub 设置同步");
    assert!(service::is_ready(&a.db).unwrap());

    // 限流：服务层把它原样交出来（控制器据此设置等待期）。
    gh.rate_limit_next(1, RateSpec::RetryAfter(180));
    let limited = context(&a)
        .run_sync(PASSWORD, None)
        .await
        .expect_err("限流");
    assert!(
        matches!(
            limited,
            SyncError::RateLimited {
                retry_after_secs: 180,
                ..
            }
        ),
        "{limited:?}"
    );
}

#[tokio::test]
async fn a_probe_for_a_config_that_cannot_even_be_built_is_reported_not_raised() {
    // 没确认 http:// 就不能用：测试连接把原因放进“读”里报告，不抛异常，也不发任何请求。
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    let unconfirmed = StorageConfig::Github(GithubConfig {
        allow_insecure_http: false,
        ..config(&gh, &id)
    });
    let probe = service::test_connection(
        &unconfirmed,
        None,
        std::sync::Arc::new(zssh_core::sync::onedrive::tokens::MemoryTokenStore::new()),
        &zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    )
    .await;
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("明文"), "{probe:?}");
    assert!(gh.requests().is_empty());
}

#[tokio::test]
async fn a_revoked_token_is_an_auth_failure_and_a_server_error_is_not() {
    let gh = start().await;
    let id = gh.add_gist(LOGIN, &[]);
    gh.revoke_token();
    let err = storage(&gh, &id).read().await.unwrap_err();
    assert!(matches!(err, StorageError::AuthFailed(_)), "{err:?}");
    assert!(matches!(SyncError::from(err), SyncError::AuthFailed(_)));
}
