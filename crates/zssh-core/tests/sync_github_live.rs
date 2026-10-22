//! 联网测试（S-3）：对着**真实的 GitHub** 跑一遍 Gist 同步。
//!
//! 门禁里不跑（和别的联网测试一样，要 `ZSSH_LIVE_FULL=1`，由 `scripts/live-tests.ps1` 设置）。
//! 令牌只从环境变量读，没设就跳过并打印原因：
//!
//! - `ZSSH_TEST_GITHUB_TOKEN`：个人访问令牌（经典 `gist` 权限，或细粒度的 Gists 读写）。必填。
//! - `ZSSH_TEST_GITHUB_API`：接口地址，默认 `https://api.github.com`（GitHub Enterprise 填自己的）。
//! - `ZSSH_TEST_GITHUB_SOCKS5`：访问 GitHub 要走的 SOCKS5 代理，形如 `127.0.0.1:10808`，不设就直连。
//!
//! **只碰自己新建的那一个 Gist**：测试新建一个临时的 secret Gist（描述里有随机串），在里面跑完整同步，
//! 结束时删掉它——正常结束、断言失败（panic）、提前返回都要删（`Drop` 里兜底，并在正常结束时
//! 再确认一次 404）。令牌所属账号里的其他 Gist 一个都不读、不列、不改（全程不调用列表接口）。
//! 测试数据（主机名、密码）都是虚构的。

mod common;

#[path = "common/harness.rs"]
mod harness;
#[path = "common/mock_github.rs"]
mod mock_github;

use harness::*;
use zssh_core::sync::engine::SyncStatus;
use zssh_core::sync::github::{self, GithubConfig, FILE_NAME};
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::storage::SyncStorage;

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn proxy_from_env() -> Option<SyncProxy> {
    let raw = env("ZSSH_TEST_GITHUB_SOCKS5")?;
    let (host, port) = raw.rsplit_once(':')?;
    Some(SyncProxy {
        kind: ProxyScheme::Socks5,
        host: host.to_owned(),
        port: port.parse().ok()?,
        username: None,
        password: None,
    })
}

/// 拿着测试新建的 Gist 的 ID；不管测试怎么结束（包括 panic），`Drop` 都会删掉它。
struct TempGist {
    config: GithubConfig,
    proxy: Option<SyncProxy>,
    id: Option<String>,
}

impl TempGist {
    /// 正常结束时调用：删掉并确认已经不存在。
    async fn finish(mut self) {
        let id = self.id.take().expect("还没删");
        github::delete_gist(&self.config, self.proxy.as_ref(), &id)
            .await
            .expect("删除测试建的 Gist");
        let config = GithubConfig {
            gist_id: id,
            ..self.config.clone()
        };
        let store = github::open_storage(&config, self.proxy.as_ref()).expect("打开存储");
        let err = store.read().await.expect_err("删掉之后应该读不到了");
        assert!(
            err.to_string().contains("找不到这个 Gist"),
            "删掉之后应该是 404：{err}"
        );
        println!("[github] 临时 Gist 已删除并确认 404");
    }
}

impl Drop for TempGist {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        // 走到这里说明测试中途失败了（panic）：在单独的线程里用新的运行时删掉，
        // 不能在已经在运行时里的线程上再 block_on。
        let config = self.config.clone();
        let proxy = self.proxy.clone();
        let outcome = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|runtime| runtime.block_on(github::delete_gist(&config, proxy.as_ref(), &id)))
        })
        .join();
        println!("[github] 测试中途失败，兜底删除临时 Gist：{outcome:?}");
    }
}

#[tokio::test]
async fn github_live_full_sync() {
    if !common::full_live("github_live_full_sync") {
        return;
    }
    let Some(token) = env("ZSSH_TEST_GITHUB_TOKEN") else {
        println!("跳过 github_live_full_sync：没设置 ZSSH_TEST_GITHUB_TOKEN");
        return;
    };
    let api_base = env("ZSSH_TEST_GITHUB_API").unwrap_or_default();
    let config = GithubConfig {
        allow_insecure_http: api_base.starts_with("http://"),
        api_base,
        token,
        gist_id: String::new(),
    };
    let proxy = proxy_from_env();

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let description = format!("zssh-live-test-{nanos}（临时测试，会自动删除）");
    let id = github::create_gist_with_description(&config, proxy.as_ref(), &description)
        .await
        .expect("新建临时 secret Gist");
    // 从这一行起，不管怎么结束都会删。
    let guard = TempGist {
        config: config.clone(),
        proxy: proxy.clone(),
        id: Some(id.clone()),
    };
    println!("[github] 已新建临时 secret Gist（描述：{description}）");
    let config = GithubConfig {
        gist_id: id,
        ..config
    };

    let probe = github::probe(&config, proxy.as_ref()).await.expect("探测");
    println!(
        "[github] 读：{} / 写：{}",
        probe.read.message, probe.write.message
    );
    assert!(probe.read.ok, "读失败：{}", probe.read.message);
    assert!(probe.write.ok, "写失败：{}", probe.write.message);
    assert!(!probe.has_existing_data, "新建的 Gist 里还没有同步数据");
    println!(
        "[github] 并发保护：{:?}（{}）",
        probe.concurrency.as_ref().map(|c| c.mode),
        probe
            .concurrency
            .as_ref()
            .map(|c| c.notes.join("；"))
            .unwrap_or_default()
    );

    let sa = github::open_storage(&config, proxy.as_ref()).expect("A 的存储");
    let sb = github::open_storage(&config, proxy.as_ref()).expect("B 的存储");
    assert!(sa.read().await.expect("读占位 Gist").is_none());

    let a = Machine::new("gh-live-a", "dev-a");
    let b = Machine::new("gh-live-b", "dev-b");
    seed(&a);
    sync_ok(&a, &sa, "A 首次").await;
    sync_ok(&b, &sb, "B 首次").await;
    sync_ok(&a, &sa, "A").await;
    assert_same_content(&a, &b, "首次同步之后");

    b.add_host("live-from-b");
    let report = b
        .engine()
        .sync(&sb, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("B 推送");
    assert_eq!(report.status, SyncStatus::Completed);
    sync_ok(&a, &sa, "A 拉取").await;
    assert_same_content(&a, &b, "增改之后");
    assert!(a.host_names().contains(&"live-from-b".to_owned()));
    println!("[github] 两台电脑收敛，主机：{:?}", a.host_names());

    // 云端读到的是密文，不是明文。
    let cloud = sa.read().await.unwrap().expect("有同步包");
    let cloud_text = String::from_utf8_lossy(&cloud.bytes);
    assert!(!cloud_text.contains("pw-from-a"), "云端只能是密文");
    assert!(cloud_text.contains("\"format\":\"zssh-sync\""));
    println!("[github] 文件名 {FILE_NAME}，版本 {}", cloud.version);

    guard.finish().await;
}

// ---------------------------------------------------------------------------
// 清理逻辑本身的测试（对着模拟服务，不需要令牌，门禁里就会跑）：
// “任何退出路径都要删”不能只靠人相信，三种退出路径各验证一次。

mod cleanup {
    use super::*;
    use crate::mock_github::{GithubOptions, MockGithub};

    const TOKEN: &str = "ghp_mocktoken_not_a_real_token_0123456789";

    async fn setup() -> (MockGithub, GithubConfig, String) {
        let gh = MockGithub::start(GithubOptions::classic(TOKEN, "demo-user")).await;
        // 账号里本来就有别的 Gist：清理只能删自己建的这一个。
        gh.add_gist("demo-user", &[("mine.txt", "别的 Gist，不能碰")]);
        let config = GithubConfig {
            api_base: gh.api_base(),
            token: TOKEN.to_owned(),
            gist_id: String::new(),
            allow_insecure_http: true,
        };
        let id = github::create_gist_with_description(&config, None, "zssh-live-test-x")
            .await
            .expect("新建");
        (gh, config, id)
    }

    fn guard(config: &GithubConfig, id: &str) -> TempGist {
        TempGist {
            config: config.clone(),
            proxy: None,
            id: Some(id.to_owned()),
        }
    }

    // Drop 里要在另一个线程上跑新的运行时，同时模拟服务还要在这个测试的运行时里响应，
    // 所以这几个测试用多线程运行时（真实联网时没有这个问题）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_finish_deletes_only_our_gist_and_confirms_it_is_gone() {
        let (gh, config, id) = setup().await;
        assert_eq!(gh.gist_ids().len(), 2);
        guard(&config, &id).finish().await;
        assert!(!gh.gist_ids().contains(&id), "临时 Gist 要被删掉");
        assert_eq!(gh.gist_ids().len(), 1, "账号里原来的 Gist 一个都不能少");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_early_return_still_deletes_it() {
        let (gh, config, id) = setup().await;
        {
            let _guard = guard(&config, &id);
            // 模拟测试提前返回：作用域结束，guard 被丢掉，没有调用 finish。
        }
        assert!(!gh.gist_ids().contains(&id));
        assert_eq!(gh.gist_ids().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panic_in_the_middle_of_the_test_still_deletes_it() {
        let (gh, config, id) = setup().await;
        let held = guard(&config, &id);
        let task = tokio::spawn(async move {
            let _held = held;
            assert_eq!(1 + 1, 3, "模拟测试中途断言失败");
        });
        let joined = task.await;
        assert!(joined.is_err(), "任务应该 panic 了");
        assert!(!gh.gist_ids().contains(&id), "panic 之后临时 Gist 也要被删");
        assert_eq!(gh.gist_ids().len(), 1);
    }
}
