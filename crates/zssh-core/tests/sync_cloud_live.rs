//! 联网测试（S-2）：对着**真实的** WebDAV / S3 服务跑一遍完整同步。
//!
//! 门禁里不跑（和别的联网测试一样，要 `ZSSH_LIVE_FULL=1`，由 `scripts/live-tests.ps1` 设置）。
//! 服务地址和账号只从环境变量读，没设就跳过并打印原因：
//!
//! - WebDAV：`ZSSH_TEST_WEBDAV_URL`、`ZSSH_TEST_WEBDAV_USER`、`ZSSH_TEST_WEBDAV_PASSWORD`
//! - S3：`ZSSH_TEST_S3_ENDPOINT`、`ZSSH_TEST_S3_REGION`、`ZSSH_TEST_S3_BUCKET`、
//!   `ZSSH_TEST_S3_ACCESS_KEY`、`ZSSH_TEST_S3_SECRET`、可选 `ZSSH_TEST_S3_PATH_STYLE=0`
//!
//! `live-tests.ps1` 会在本机临时起一个真实的 WebDAV 实现（wsgidav，Python）并设好前三个
//! 变量，跑完关掉、删数据。测试只在自己的随机子目录 / 前缀下读写，结束时删干净。
//! 测试内容里的主机名、密码都是虚构的。

mod common;

use std::path::PathBuf;

use zssh_core::model::{AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyMode};
use zssh_core::store::Database;
use zssh_core::sync::engine::SyncStatus;
use zssh_core::sync::s3::S3Config;
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::service::{StorageConfig, SyncContext};
use zssh_core::sync::snapshot::snapshot_local;
use zssh_core::sync::webdav::WebDavConfig;

const PASSWORD: &str = "live-test-master-password";

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn unique_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("zssh-live-{nanos}")
}

struct Machine {
    db: Database,
    dir: PathBuf,
    device: &'static str,
    /// 目录守卫放在最后：数据库先关，目录最后删。
    _guard: tempfile::TempDir,
}

impl Machine {
    fn new(device: &'static str) -> Self {
        let guard = tempfile::Builder::new()
            .prefix(&format!("{}-{device}-", unique_name()))
            .tempdir()
            .expect("建目录");
        let dir = guard.path().to_path_buf();
        Self {
            db: Database::open(&dir.join("zssh.db")).expect("打开库"),
            dir,
            device,
            _guard: guard,
        }
    }

    fn context(&self) -> SyncContext<'_> {
        SyncContext {
            db: &self.db,
            sealer: &DpapiSealer,
            device_id: self.device.to_owned(),
            data_dir: self.dir.clone(),
            known_hosts_path: None,
            app_version: "0.1.0".to_owned(),
            onedrive_endpoints: zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
        }
    }

    fn add_host(&self, name: &str) {
        self.db
            .host_save(&HostDraft {
                id: None,
                group_id: None,
                name: name.to_owned(),
                host: format!("{name}.example.internal"),
                port: 22,
                username: "deploy".to_owned(),
                auth_method: AuthMethod::Password,
                identity_id: None,
                proxy_id: None,
                jump_host_id: None,
                proxy_mode: ProxyMode::Default,
                tags: Vec::new(),
                sort: 0,
                legacy_algorithms: false,
            })
            .expect("建主机");
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .db
            .host_list()
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        names.sort();
        names
    }
}

/// 对着一个真实存储：测试连接 → 两台电脑同步 → 一边增删改 → 再收敛。
async fn run_against(label: &str, config: StorageConfig) {
    let probe = zssh_core::sync::service::test_connection(
        &config,
        None,
        std::sync::Arc::new(zssh_core::sync::onedrive::tokens::MemoryTokenStore::new()),
        &zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    )
    .await;
    println!(
        "[{label}] 读：{} / 写：{}",
        probe.read.message, probe.write.message
    );
    assert!(probe.read.ok, "[{label}] 读失败：{}", probe.read.message);
    assert!(probe.write.ok, "[{label}] 写失败：{}", probe.write.message);
    let concurrency = probe.concurrency.expect("检测结果");
    println!(
        "[{label}] 并发保护：{:?}（{}）",
        concurrency.mode,
        concurrency.notes.join("；")
    );

    let a = Machine::new("dev-a");
    let b = Machine::new("dev-b");
    for machine in [&a, &b] {
        zssh_core::sync::service::save_config(&machine.db, &DpapiSealer, &config).unwrap();
        zssh_core::sync::service::save_concurrency(
            &machine.db,
            &zssh_core::sync::service::ConcurrencyRecord {
                mode: concurrency.mode,
                notes: concurrency.notes.clone(),
                checked_at: 1,
            },
        )
        .unwrap();
    }
    a.add_host("live-web-1");
    a.add_host("live-web-2");
    a.db.identity_save(&IdentityDraft {
        id: None,
        name: "live-cred".to_owned(),
        kind: IdentityKind::Password,
        key_path: None,
        secret: Some("live-secret-password".to_owned()),
        passphrase: None,
    })
    .unwrap();

    let report = a.context().run_sync(PASSWORD, None).await.expect("A 同步");
    assert_eq!(report.status, SyncStatus::Completed);
    b.context().run_sync(PASSWORD, None).await.expect("B 同步");
    assert_eq!(a.names(), b.names());
    let content = snapshot_local(&b.db, &DpapiSealer, "x", None, 1).unwrap();
    assert!(content
        .identities
        .iter()
        .any(|i| i.secret.as_deref() == Some("live-secret-password")));

    b.add_host("live-from-b");
    b.context().run_sync(PASSWORD, None).await.expect("B 推送");
    a.context().run_sync(PASSWORD, None).await.expect("A 拉取");
    assert_eq!(a.names(), b.names());
    assert!(a.names().contains(&"live-from-b".to_owned()));
    println!("[{label}] 两台电脑收敛，主机：{:?}", a.names());

    // 收尾：删掉云端的同步包（版本对得上才删）。
    let storage = zssh_core::sync::service::open_storage(
        &config,
        None,
        concurrency.mode,
        std::sync::Arc::new(zssh_core::sync::onedrive::tokens::MemoryTokenStore::new()),
        &zssh_core::sync::onedrive::oauth::Endpoints::microsoft(),
    )
    .expect("打开存储");
    if let Some(object) = storage.read().await.expect("读") {
        storage
            .delete(Some(&object.version))
            .await
            .expect("删除云端同步包");
    }
    assert!(
        storage.read().await.expect("再读").is_none(),
        "云端要删干净"
    );
}

#[tokio::test]
async fn webdav_live_full_sync() {
    if !common::full_live("webdav_live_full_sync") {
        return;
    }
    let (Some(url), Some(user), Some(password)) = (
        env("ZSSH_TEST_WEBDAV_URL"),
        env("ZSSH_TEST_WEBDAV_USER"),
        env("ZSSH_TEST_WEBDAV_PASSWORD"),
    ) else {
        println!("跳过 webdav_live_full_sync：没设置 ZSSH_TEST_WEBDAV_URL / USER / PASSWORD");
        return;
    };
    let unique = unique_name();
    let config = StorageConfig::Webdav(WebDavConfig {
        allow_insecure_http: url.starts_with("http://"),
        url: url.clone(),
        directory: format!("{unique}/nested"),
        username: user.clone(),
        password: password.clone(),
    });
    run_against("webdav", config).await;

    // 收尾：把测试建的目录也删掉（同步包对象已经删了，这里删空目录），再确认没有残留。
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("客户端");
    let dir_url = format!("{}/{unique}/", url.trim_end_matches('/'));
    let deleted = client
        .delete(&dir_url)
        .basic_auth(&user, Some(&password))
        .send()
        .await
        .expect("删除测试目录");
    assert!(
        deleted.status().is_success() || deleted.status() == reqwest::StatusCode::NOT_FOUND,
        "删除测试目录失败：{}",
        deleted.status()
    );
    let gone = client
        .get(&dir_url)
        .basic_auth(&user, Some(&password))
        .send()
        .await
        .expect("确认目录已删");
    assert_eq!(
        gone.status(),
        reqwest::StatusCode::NOT_FOUND,
        "测试目录要删干净"
    );
}

#[tokio::test]
async fn s3_live_full_sync() {
    if !common::full_live("s3_live_full_sync") {
        return;
    }
    let (Some(endpoint), Some(region), Some(bucket), Some(access_key), Some(secret_key)) = (
        env("ZSSH_TEST_S3_ENDPOINT"),
        env("ZSSH_TEST_S3_REGION"),
        env("ZSSH_TEST_S3_BUCKET"),
        env("ZSSH_TEST_S3_ACCESS_KEY"),
        env("ZSSH_TEST_S3_SECRET"),
    ) else {
        println!("跳过 s3_live_full_sync：没设置 ZSSH_TEST_S3_ENDPOINT / REGION / BUCKET / ACCESS_KEY / SECRET");
        return;
    };
    let config = StorageConfig::S3(S3Config {
        endpoint,
        region,
        bucket,
        access_key,
        secret_key,
        prefix: unique_name(),
        path_style: env("ZSSH_TEST_S3_PATH_STYLE").as_deref() != Some("0"),
    });
    run_against("s3", config).await;
}
