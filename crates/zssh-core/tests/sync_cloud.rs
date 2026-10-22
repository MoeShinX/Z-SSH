//! 集成测试（S-2）：WebDAV 和 S3 兼容存储，对着进程内的模拟服务跑完整同步。
//!
//! 不用 Docker、不碰任何真实云账号。两个数据目录模拟两台电脑，每一轮比较**完整的
//! 同步内容**（分组、主机全部字段、凭据解开后的密码和私钥、代理、白名单设置、
//! known_hosts、墓碑）。请求全部走 `net::build_client`，即和正式使用同一套代理设置；
//! SOCKS5 的用例经过测试里自己起的本地代理。

#[path = "common/harness.rs"]
mod harness;
#[path = "common/mock_cloud.rs"]
mod mock_cloud;

use harness::*;

use mock_cloud::{Behavior, Kind, MockCloud, MockHttpProxy, MockSocks5, S3Conditional};
use zssh_core::model::{ProxyDraft, ProxyKind};
use zssh_core::sync::engine::SyncStatus;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::remote::{ConcurrencyMode, ConnectionProbe};
use zssh_core::sync::s3::{self, S3Config};
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::webdav::{self, WebDavConfig, WebDavRaw};

const DAV_USER: &str = "alice";
const DAV_PASSWORD: &str = "dav-test-password";

fn dav_kind(behavior: Behavior, put_returns_etag: bool) -> Kind {
    Kind::WebDav {
        user: DAV_USER.to_owned(),
        password: DAV_PASSWORD.to_owned(),
        behavior,
        put_returns_etag,
    }
}

fn dav_config(cloud: &MockCloud) -> WebDavConfig {
    WebDavConfig {
        url: format!("{}/dav/", cloud.base_url()),
        directory: "Z-SSH/sync".to_owned(),
        username: DAV_USER.to_owned(),
        password: DAV_PASSWORD.to_owned(),
        allow_insecure_http: true,
    }
}

fn s3_kind(path_style: bool, conditional: S3Conditional) -> Kind {
    Kind::S3 {
        access_key: "AKIATESTKEY".to_owned(),
        secret_key: "s3-test-secret-key".to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "zssh-test".to_owned(),
        path_style,
        conditional,
    }
}

fn s3_config(endpoint: &str, path_style: bool) -> S3Config {
    S3Config {
        endpoint: endpoint.to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "zssh-test".to_owned(),
        access_key: "AKIATESTKEY".to_owned(),
        secret_key: "s3-test-secret-key".to_owned(),
        prefix: "team a/zssh".to_owned(),
        path_style,
    }
}

async fn probe_dav(cloud: &MockCloud) -> ConnectionProbe {
    webdav::probe(&dav_config(cloud), None).await.expect("探测")
}

// ---------------------------------------------------------------------------
// WebDAV

#[tokio::test]
async fn webdav_with_etag_runs_the_full_scenario() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let probe = probe_dav(&cloud).await;
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(!probe.has_existing_data);
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(
        concurrency.mode,
        ConcurrencyMode::Conditional,
        "{concurrency:?}"
    );
    assert!(
        cloud
            .object_paths()
            .iter()
            .all(|p| !p.contains("zssh-probe")),
        "探测对象要清理干净：{:?}",
        cloud.object_paths()
    );

    let config = dav_config(&cloud);
    let sa = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    let sb = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    full_scenario(
        &sa,
        &sb,
        &|bytes| cloud.inject_external_write_before_next_put(bytes),
        &|| cloud.sync_object(),
    )
    .await;

    // 请求构造：Basic 认证、逐级 MKCOL、首次 If-None-Match、之后 If-Match。
    let log = cloud.requests();
    assert!(log.iter().all(|r| r
        .authorization
        .as_deref()
        .is_some_and(|a| a.starts_with("Basic "))));
    assert!(cloud.dirs().contains(&"/dav/Z-SSH".to_owned()));
    assert!(cloud.dirs().contains(&"/dav/Z-SSH/sync".to_owned()));
    let puts: Vec<_> = log
        .iter()
        .filter(|r| r.method == "PUT" && r.path.ends_with("zssh-sync.json"))
        .collect();
    assert!(
        puts.iter().any(|r| r.if_none_match.as_deref() == Some("*")),
        "首次创建用 If-None-Match"
    );
    assert!(
        puts.iter().any(|r| r.if_match.is_some()),
        "之后的写用 If-Match"
    );
    assert!(
        puts.iter().any(|r| r.status == 412),
        "冲突场景里服务器拒绝过一次"
    );
    assert!(
        log.iter().any(|r| r.method == "MKCOL"),
        "目录不存在时要 MKCOL"
    );
}

#[tokio::test]
async fn webdav_without_etag_is_detected_and_falls_back_to_read_before_write() {
    let cloud = MockCloud::start(dav_kind(Behavior::NoEtag, false)).await;
    let probe = probe_dav(&cloud).await;
    assert!(probe.read.ok && probe.write.ok);
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(concurrency.mode, ConcurrencyMode::ReadBeforeWrite);
    assert!(
        concurrency
            .notes
            .iter()
            .any(|n| n.contains("没有返回 ETag")),
        "{:?}",
        concurrency.notes
    );

    let config = dav_config(&cloud);
    let sa = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    let sb = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    read_before_write_scenario(&|bytes| cloud.set_sync_object(bytes), &sa, &sb).await;
}

#[tokio::test]
async fn webdav_that_ignores_conditional_headers_is_not_treated_as_protected() {
    let cloud = MockCloud::start(dav_kind(Behavior::IgnoreConditions, true)).await;
    let probe = probe_dav(&cloud).await;
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(concurrency.mode, ConcurrencyMode::ReadBeforeWrite);
    assert!(
        concurrency
            .notes
            .iter()
            .any(|n| n.contains("忽略了 If-None-Match")),
        "{:?}",
        concurrency.notes
    );
    // 有 ETag 但不执行条件：如果悄悄当成支持，这里的“抢先写”就会被覆盖掉。
    let config = dav_config(&cloud);
    let sa = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    let sb = webdav::open_storage(&config, None, concurrency.mode).unwrap();
    read_before_write_scenario(&|bytes| cloud.set_sync_object(bytes), &sa, &sb).await;
}

#[tokio::test]
async fn webdav_wrong_password_and_plain_http_policy() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let mut config = dav_config(&cloud);
    config.password = "wrong".to_owned();
    let probe = webdav::probe(&config, None).await.expect("探测本身不报错");
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("用户名或密码不对"), "{probe:?}");
    assert!(probe.concurrency.is_none());

    config.password = DAV_PASSWORD.to_owned();
    config.allow_insecure_http = false;
    let err = WebDavRaw::new(&config, None)
        .err()
        .expect("没确认不能用 http");
    assert!(err.to_string().contains("明文"));
    assert!(
        cloud
            .requests()
            .iter()
            .all(|r| r.authorization.as_deref() != Some("Basic YWxpY2U6ZGF2LXRlc3QtcGFzc3dvcmQ=")),
        "没确认之前不能发过正确的密码"
    );
}

#[tokio::test]
async fn temporary_server_errors_are_retried_and_offline_fails_quietly() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let config = dav_config(&cloud);
    let storage = webdav::open_storage(&config, None, ConcurrencyMode::Conditional).unwrap();
    let a = Machine::new("retry-a", "dev-a");
    seed(&a);
    // 读遇到两次 503：引擎最多重试 2 次，第三次成功。
    cloud.fail_next(503, 2);
    sync_ok(&a, &storage, "503 之后成功").await;

    // 一直 503：重试有上限，最后报错而不是无限重试。
    cloud.fail_next(503, 20);
    let err = a
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("一直 503");
    assert!(err.to_string().contains("暂时不可用"), "{err}");

    // 离线（端口没人监听）：很快失败并给出能看懂的原因。
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let mut offline = dav_config(&cloud);
    offline.url = format!("http://127.0.0.1:{dead}/dav/");
    let started = std::time::Instant::now();
    let probe = webdav::probe(&offline, None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("无法连接"), "{probe:?}");
    assert!(started.elapsed().as_secs() < 10);
}

// ---------------------------------------------------------------------------
// S3

#[tokio::test]
async fn s3_path_style_runs_the_full_scenario_and_signatures_verify() {
    let cloud = MockCloud::start(s3_kind(true, S3Conditional::Full)).await;
    let config = s3_config(&cloud.base_url(), true);
    let probe = s3::probe(&config, None).await.expect("探测");
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    let concurrency = probe.concurrency.expect("检测结果");
    assert_eq!(
        concurrency.mode,
        ConcurrencyMode::Conditional,
        "{concurrency:?}"
    );

    let sa = s3::open_storage(&config, None, concurrency.mode).unwrap();
    let sb = s3::open_storage(&config, None, concurrency.mode).unwrap();
    full_scenario(
        &sa,
        &sb,
        &|bytes| cloud.inject_external_write_before_next_put(bytes),
        &|| cloud.sync_object(),
    )
    .await;

    let log = cloud.requests();
    assert!(log.iter().all(|r| r
        .authorization
        .as_deref()
        .is_some_and(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=AKIATESTKEY/"))));
    assert!(log.iter().any(|r| r.if_none_match.as_deref() == Some("*")));
    assert!(log.iter().any(|r| r.if_match.is_some()));
    assert!(
        cloud
            .object_paths()
            .iter()
            .all(|p| p.starts_with("team a/zssh/")),
        "对象在前缀下面：{:?}",
        cloud.object_paths()
    );
    assert!(log.iter().all(|r| r.status != 403), "没有签名被拒绝过");
}

#[tokio::test]
async fn s3_wrong_secret_is_reported_as_a_signature_problem() {
    let cloud = MockCloud::start(s3_kind(true, S3Conditional::Full)).await;
    let mut config = s3_config(&cloud.base_url(), true);
    config.secret_key = "not-the-secret".to_owned();
    let probe = s3::probe(&config, None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("签名不对"), "{probe:?}");
    let mut config = s3_config(&cloud.base_url(), true);
    config.bucket = "other-bucket".to_owned();
    let probe = s3::probe(&config, None).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("Bucket 不存在"), "{probe:?}");
}

#[tokio::test]
async fn s3_without_conditional_writes_is_detected_for_both_failure_modes() {
    for conditional in [S3Conditional::Ignored, S3Conditional::NotImplemented] {
        let cloud = MockCloud::start(s3_kind(true, conditional)).await;
        let config = s3_config(&cloud.base_url(), true);
        let probe = s3::probe(&config, None).await.unwrap();
        assert!(probe.read.ok && probe.write.ok, "{conditional:?} {probe:?}");
        let concurrency = probe.concurrency.unwrap();
        assert_eq!(
            concurrency.mode,
            ConcurrencyMode::ReadBeforeWrite,
            "{conditional:?} {concurrency:?}"
        );
        let sa = s3::open_storage(&config, None, concurrency.mode).unwrap();
        let sb = s3::open_storage(&config, None, concurrency.mode).unwrap();
        read_before_write_scenario(&|bytes| cloud.set_sync_object(bytes), &sa, &sb).await;
        assert!(
            cloud
                .object_paths()
                .iter()
                .all(|p| !p.contains("zssh-probe")),
            "{conditional:?}：探测对象要清理干净"
        );
    }
}

// ---------------------------------------------------------------------------
// 代理

fn socks_proxy(proxy: &MockSocks5, auth: Option<(&str, &str)>) -> SyncProxy {
    SyncProxy {
        kind: ProxyScheme::Socks5,
        host: "127.0.0.1".to_owned(),
        port: proxy.addr().port(),
        username: auth.map(|(u, _)| u.to_owned()),
        password: auth.map(|(_, p)| p.to_owned()),
    }
}

#[tokio::test]
async fn webdav_goes_through_a_socks5_proxy_with_authentication() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let proxy = MockSocks5::start(
        cloud.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    // 用不存在的域名：只有经过代理（socks5h 把域名交给代理解析）才连得上。
    let mut config = dav_config(&cloud);
    config.url = "http://dav.zssh-test.invalid:8080/dav/".to_owned();

    let proxied = socks_proxy(&proxy, Some(("proxy-user", "proxy-pass")));
    let probe = webdav::probe(&config, Some(&proxied)).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(
        proxy
            .targets()
            .iter()
            .any(|(host, port)| host == "dav.zssh-test.invalid" && *port == 8080),
        "请求必须经过代理：{:?}",
        proxy.targets()
    );

    let storage =
        webdav::open_storage(&config, Some(&proxied), ConcurrencyMode::Conditional).unwrap();
    let a = Machine::new("socks-dav", "dev-a");
    seed(&a);
    sync_ok(&a, &storage, "经 SOCKS5 同步").await;
    assert!(cloud.sync_object().is_some());

    // 不走代理就连不上同一个地址 → 证明上面确实是代理在起作用。
    let direct = webdav::probe(&config, None).await.unwrap();
    assert!(!direct.read.ok, "{direct:?}");

    // 代理密码错：连不上，原因里提到代理或无法连接，不是别的错误。
    let wrong = socks_proxy(&proxy, Some(("proxy-user", "bad")));
    let probe = webdav::probe(&config, Some(&wrong)).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("无法连接"), "{probe:?}");
}

fn http_proxy(proxy: &MockHttpProxy, auth: Option<(&str, &str)>) -> SyncProxy {
    SyncProxy {
        kind: ProxyScheme::Http,
        host: "127.0.0.1".to_owned(),
        port: proxy.addr().port(),
        username: auth.map(|(u, _)| u.to_owned()),
        password: auth.map(|(_, p)| p.to_owned()),
    }
}

#[tokio::test]
async fn webdav_goes_through_an_http_proxy_with_authentication() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let proxy = MockHttpProxy::start(
        cloud.addr(),
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    // 用不存在的域名：只有真的经过代理才连得上。
    let mut config = dav_config(&cloud);
    config.url = "http://dav.zssh-test.invalid:8080/dav/".to_owned();

    let proxied = http_proxy(&proxy, Some(("proxy-user", "proxy-pass")));
    let probe = webdav::probe(&config, Some(&proxied)).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert_eq!(
        proxy.authorized_targets(),
        vec!["dav.zssh-test.invalid:8080".to_owned()],
        "请求必须经过代理，并且代理认证通过"
    );
    let methods: Vec<String> = proxy.requests().into_iter().map(|r| r.method).collect();
    assert!(methods.iter().any(|m| m == "GET") && methods.iter().any(|m| m == "PUT"));

    // 整个同步（两种条件写入、MKCOL）都走代理。
    let storage =
        webdav::open_storage(&config, Some(&proxied), ConcurrencyMode::Conditional).unwrap();
    let a = Machine::new("httpproxy-dav", "dev-a");
    seed(&a);
    sync_ok(&a, &storage, "经 HTTP 代理同步").await;
    assert!(cloud.sync_object().is_some());
    assert!(proxy.requests().iter().any(|r| r.method == "MKCOL"));

    // 代理专用的认证头不能被转发给存储服务。
    assert!(
        cloud.requests().iter().all(|r| r
            .authorization
            .as_deref()
            .is_some_and(|a| a.starts_with("Basic "))),
        "存储服务只应该看到自己的 Basic 认证"
    );

    // 不走代理就连不上同一个地址 → 证明上面确实是代理在起作用。
    let direct = webdav::probe(&config, None).await.unwrap();
    assert!(!direct.read.ok, "{direct:?}");

    // 代理密码错：407，提示要检查代理，不能被当成存储的密码错误。
    let wrong = http_proxy(&proxy, Some(("proxy-user", "bad")));
    let probe = webdav::probe(&config, Some(&wrong)).await.unwrap();
    assert!(!probe.read.ok);
    assert!(probe.read.message.contains("代理"), "{probe:?}");
    assert!(
        !probe.read.message.contains("用户名或密码不对"),
        "{probe:?}"
    );
    assert!(proxy.requests().iter().any(|r| !r.authorized));
}

#[tokio::test]
async fn https_addresses_use_connect_through_an_http_proxy() {
    // 访问 https:// 地址时 HTTP 代理收到的是 `CONNECT 主机:443`，带上代理账号密码。
    // 测试里没有 TLS 服务，所以上游是“接上就挂断”的：握手一定失败，但代理已经看到了 CONNECT。
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink.local_addr().unwrap();
    let sink_task = tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = sink.accept().await {
                drop(socket);
            }
        }
    });
    let proxy = MockHttpProxy::start(
        sink_addr,
        Some(("proxy-user".to_owned(), "proxy-pass".to_owned())),
    )
    .await;
    let config = WebDavConfig {
        url: "https://dav.zssh-test.invalid/dav/".to_owned(),
        directory: String::new(),
        username: DAV_USER.to_owned(),
        password: DAV_PASSWORD.to_owned(),
        allow_insecure_http: false,
    };

    let good = http_proxy(&proxy, Some(("proxy-user", "proxy-pass")));
    let probe = webdav::probe(&config, Some(&good)).await.unwrap();
    assert!(!probe.read.ok, "上游不是 TLS 服务，握手必然失败：{probe:?}");
    let connects: Vec<_> = proxy
        .requests()
        .into_iter()
        .filter(|r| r.method == "CONNECT")
        .collect();
    assert!(
        connects
            .iter()
            .any(|r| r.target == "dav.zssh-test.invalid:443" && r.authorized),
        "代理应该收到带正确认证的 CONNECT：{connects:?}"
    );

    let bad = http_proxy(&proxy, Some(("proxy-user", "bad")));
    let probe = webdav::probe(&config, Some(&bad)).await.unwrap();
    assert!(!probe.read.ok);
    assert!(
        proxy
            .requests()
            .iter()
            .any(|r| r.method == "CONNECT" && !r.authorized),
        "密码错的 CONNECT 要被代理拒绝（407）"
    );
    sink_task.abort();
}

#[tokio::test]
async fn s3_goes_through_an_http_proxy_and_the_signature_survives() {
    let cloud = MockCloud::start(s3_kind(false, S3Conditional::Full)).await;
    let proxy = MockHttpProxy::start(cloud.addr(), None).await;
    let config = s3_config("http://s3.zssh-test.invalid:9000", false);
    let proxied = http_proxy(&proxy, None);
    let probe = s3::probe(&config, Some(&proxied)).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(
        proxy
            .authorized_targets()
            .contains(&"zssh-test.s3.zssh-test.invalid:9000".to_owned()),
        "{:?}",
        proxy.authorized_targets()
    );
    // 代理转发时保留了 Host 头，SigV4 签名才对得上（服务端独立重算，错了会回 403）。
    assert!(cloud.requests().iter().all(|r| r.status != 403));
    let storage =
        s3::open_storage(&config, Some(&proxied), probe.concurrency.unwrap().mode).unwrap();
    let a = Machine::new("httpproxy-s3", "dev-a");
    seed(&a);
    sync_ok(&a, &storage, "经 HTTP 代理同步").await;
    assert!(cloud.sync_object().is_some());
}

#[tokio::test]
async fn s3_virtual_hosted_style_goes_through_socks5_with_the_bucket_in_the_hostname() {
    let cloud = MockCloud::start(s3_kind(false, S3Conditional::Full)).await;
    let proxy = MockSocks5::start(cloud.addr(), None).await;
    let config = s3_config("http://s3.zssh-test.invalid:9000", false);
    let proxied = socks_proxy(&proxy, None);
    let probe = s3::probe(&config, Some(&proxied)).await.unwrap();
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    assert!(
        proxy
            .targets()
            .iter()
            .any(|(host, port)| host == "zssh-test.s3.zssh-test.invalid" && *port == 9000),
        "虚拟主机风格的主机名带桶名：{:?}",
        proxy.targets()
    );
    assert!(cloud
        .requests()
        .iter()
        .all(|r| r.host == "zssh-test.s3.zssh-test.invalid:9000"));

    let storage =
        s3::open_storage(&config, Some(&proxied), probe.concurrency.unwrap().mode).unwrap();
    let a = Machine::new("socks-s3", "dev-a");
    seed(&a);
    sync_ok(&a, &storage, "经 SOCKS5 同步").await;
    assert!(cloud.sync_object().is_some());
}

// ---------------------------------------------------------------------------
// 设置页背后的服务层（保存的设置 → 同步）

use zssh_core::sync::service::{self, StorageConfig, SyncContext};

fn context<'a>(machine: &'a Machine) -> SyncContext<'a> {
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

fn raw_settings_dump(machine: &Machine) -> String {
    [
        "sync_storage",
        "sync_master_password",
        "sync_concurrency",
        "sync_ready",
    ]
    .iter()
    .filter_map(|key| {
        machine
            .db
            .setting_get(key)
            .expect("读设置")
            .map(|value| format!("{key}={value}"))
    })
    .collect::<Vec<_>>()
    .join(
        "
",
    )
}

#[tokio::test]
async fn saved_settings_drive_a_sync_through_the_default_proxy_and_secrets_stay_sealed() {
    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let proxy_server = MockSocks5::start(cloud.addr(), None).await;
    let a = Machine::new("svc-a", "dev-a");
    let b = Machine::new("svc-b", "dev-b");
    seed(&a);

    assert!(
        matches!(
            context(&a).run_sync(PASSWORD, None).await,
            Err(zssh_core::sync::SyncError::NotConfigured)
        ),
        "没连接存储时要明确报错"
    );
    assert_eq!(
        service::effective_mode(&a.db).unwrap(),
        ConcurrencyMode::ReadBeforeWrite,
        "没检测过的存储一律按不支持处理"
    );

    // 用不存在的域名：只有走默认代理（socks5h 把域名交给代理）才连得上。
    let config = StorageConfig::Webdav(WebDavConfig {
        url: "http://dav.zssh-test.invalid:8080/dav/".to_owned(),
        directory: "Z-SSH".to_owned(),
        username: DAV_USER.to_owned(),
        password: DAV_PASSWORD.to_owned(),
        allow_insecure_http: true,
    });
    for machine in [&a, &b] {
        service::save_config(&machine.db, &DpapiSealer, &config).unwrap();
        let proxy = machine
            .db
            .proxy_save(&ProxyDraft {
                id: None,
                name: "本地代理".to_owned(),
                kind: ProxyKind::Socks5,
                host: "127.0.0.1".to_owned(),
                port: proxy_server.addr().port(),
                username: None,
                secret: None,
            })
            .unwrap();
        machine.db.set_default_proxy_id(Some(proxy.id)).unwrap();
    }
    // 设置表里看不到任何明文密码 / 地址。
    let dump = raw_settings_dump(&a);
    assert!(
        !dump.contains(DAV_PASSWORD) && !dump.contains("zssh-test.invalid"),
        "{dump}"
    );
    assert_eq!(
        service::load_config(&a.db, &DpapiSealer).unwrap().unwrap(),
        config
    );

    let probe = service::test_connection(
        &config,
        service::default_proxy(&a.db).unwrap().as_ref(),
        std::sync::Arc::new(zssh_core::sync::onedrive::tokens::MemoryTokenStore::new()),
        &zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    )
    .await;
    assert!(probe.read.ok && probe.write.ok, "{probe:?}");
    service::save_concurrency(
        &a.db,
        &service::ConcurrencyRecord {
            mode: probe.concurrency.unwrap().mode,
            notes: Vec::new(),
            checked_at: 1,
        },
    )
    .unwrap();
    service::save_concurrency(
        &b.db,
        &service::ConcurrencyRecord {
            mode: ConcurrencyMode::Conditional,
            notes: Vec::new(),
            checked_at: 1,
        },
    )
    .unwrap();

    assert!(!service::is_ready(&a.db).unwrap());
    let report = context(&a).run_sync(PASSWORD, None).await.expect("A 同步");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(service::is_ready(&a.db).unwrap());
    let changes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = changes.clone();
    b.db.set_change_listener(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let report = context(&b).run_sync(PASSWORD, None).await.expect("B 同步");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(report.applied.is_some(), "B 确实写入了从云端来的数据");
    assert_eq!(
        changes.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "同步自己写本地库不能触发“数据变了”，否则会自己触发自己"
    );
    // 两台电脑各自建过一条自己的本地代理（uid 不同，不会去重），其余内容必须一致。
    let (left, right) = (a.content(), b.content());
    assert_eq!(left.hosts, right.hosts);
    assert_eq!(left.identities, right.identities);
    assert_eq!(left.groups, right.groups);
    assert!(
        proxy_server
            .targets()
            .iter()
            .all(|(host, port)| host == "dav.zssh-test.invalid" && *port == 8080)
            && !proxy_server.targets().is_empty(),
        "所有请求都经过默认代理：{:?}",
        proxy_server.targets()
    );

    // 主密码：默认不落盘；记住时用本机封装保存，关掉时删除。
    assert!(!raw_settings_dump(&a).contains("sync_master_password"));
    service::save_master_password(&a.db, &DpapiSealer, PASSWORD).unwrap();
    let dump = raw_settings_dump(&a);
    assert!(dump.contains("sync_master_password") && !dump.contains(PASSWORD));
    assert_eq!(
        service::load_master_password(&a.db, &DpapiSealer)
            .unwrap()
            .unwrap()
            .as_str(),
        PASSWORD
    );
    service::clear_master_password(&a.db).unwrap();
    assert!(service::load_master_password(&a.db, &DpapiSealer)
        .unwrap()
        .is_none());

    // 修改主密码：重新加密上传；新密码能同步，旧密码解不开。
    let new_password = "a-brand-new-master-password";
    context(&a)
        .change_master_password(PASSWORD, new_password)
        .await
        .expect("改主密码");
    let wrong = context(&b).run_sync(PASSWORD, None).await;
    assert!(
        matches!(wrong, Err(zssh_core::sync::SyncError::WrongPassword)),
        "旧密码要提示密码错误：{wrong:?}"
    );
    context(&b)
        .run_sync(new_password, None)
        .await
        .expect("新密码");
    assert!(context(&a)
        .change_master_password("wrong-old-password", "another-long-password")
        .await
        .is_err());
    assert!(context(&a)
        .change_master_password(new_password, "short")
        .await
        .is_err());
}

// ---------------------------------------------------------------------------
// 认证失败要和“可能自己恢复”的错误分开（自动同步据此暂停）

#[tokio::test]
async fn rejected_credentials_are_auth_failures_but_network_errors_are_not() {
    use zssh_core::sync::SyncError;

    let cloud = MockCloud::start(dav_kind(Behavior::Full, true)).await;
    let a = Machine::new("auth-dav", "dev-a");
    seed(&a);
    let mut config = dav_config(&cloud);
    config.password = "wrong".to_owned();
    let storage = webdav::open_storage(&config, None, ConcurrencyMode::Conditional).unwrap();
    let err = a
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("WebDAV 401");
    assert!(matches!(err, SyncError::AuthFailed(_)), "{err:?}");

    // 服务器 5xx：照常是普通的存储错误。
    let storage =
        webdav::open_storage(&dav_config(&cloud), None, ConcurrencyMode::Conditional).unwrap();
    cloud.fail_next(503, 20);
    let err = a
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("一直 503");
    assert!(matches!(err, SyncError::Storage(_)), "{err:?}");

    // 网络不通：同样不是认证失败。
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let mut offline = dav_config(&cloud);
    offline.url = format!("http://127.0.0.1:{dead}/dav/");
    let b = Machine::new("auth-offline", "dev-b");
    seed(&b);
    let storage = webdav::open_storage(&offline, None, ConcurrencyMode::Conditional).unwrap();
    let err = b
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("离线");
    assert!(matches!(err, SyncError::Storage(_)), "{err:?}");

    // S3 的密钥错误。
    let s3_cloud = MockCloud::start(s3_kind(true, S3Conditional::Full)).await;
    let mut config = s3_config(&s3_cloud.base_url(), true);
    config.secret_key = "not-the-secret".to_owned();
    let c = Machine::new("auth-s3", "dev-c");
    seed(&c);
    let storage = s3::open_storage(&config, None, ConcurrencyMode::Conditional).unwrap();
    let err = c
        .engine()
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("S3 签名不对");
    assert!(matches!(err, SyncError::AuthFailed(_)), "{err:?}");
}
