//! 联网测试（S-4）：对着**真实的 OneDrive** 验证令牌刷新和条件写入。
//!
//! 门禁里不跑（和别的联网测试一样，要 `ZSSH_LIVE_FULL=1`，由 `scripts/live-tests.ps1` 设置）。
//! 需要两个环境变量，缺一个就跳过并打印原因：
//!
//! - `ZSSH_ONEDRIVE_CLIENT_ID`：在 Azure 注册的应用的“应用程序(客户端) ID”（`docs/release.md` 有步骤）
//! - `ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN`：用上面这个应用手动授权一次得到的刷新令牌（只放环境变量，
//!   不写进任何文件，测试也不会打印它）。微软换发新刷新令牌时不会作废旧的，所以同一个值可以反复用，
//!   直到它 90 天没被用过而过期
//! - 可选 `ZSSH_TEST_ONEDRIVE_SOCKS5`：访问微软要走的 SOCKS5 代理，形如 `127.0.0.1:10808`
//!
//! **只碰自己新建的测试文件**：文件名带随机串（`zssh-live-test-<随机>.json`），在应用专用文件夹里读写，
//! 结束时删掉——正常结束、断言失败（panic）、提前返回都要删（`Drop` 里兜底，并在正常结束时再确认一次
//! 已经不存在）。真正的同步包 `zssh-sync.json` 一次都不碰。
//! 这个文件里的“清理逻辑”测试对着模拟服务，不需要任何环境变量，门禁里就会跑。

mod common;
#[path = "common/mock_microsoft.rs"]
mod mock_microsoft;

use std::sync::Arc;

use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::onedrive::graph::{self, OneDriveRaw, OBJECT_NAME};
use zssh_core::sync::onedrive::oauth::{Endpoints, TokenSet};
use zssh_core::sync::onedrive::tokens::{MemoryTokenStore, TokenStore};
use zssh_core::sync::remote::{Condition, RawObjectStore, RawPut};

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn proxy_from_env() -> Option<SyncProxy> {
    let raw = env("ZSSH_TEST_ONEDRIVE_SOCKS5")?;
    let (host, port) = raw.rsplit_once(':')?;
    Some(SyncProxy {
        kind: ProxyScheme::Socks5,
        host: host.to_owned(),
        port: port.parse().ok()?,
        username: None,
        password: None,
    })
}

/// 只带刷新令牌的起始状态：访问令牌留空、视为已过期，第一次请求时会自动刷新。
fn starting_tokens(refresh_token: &str) -> MemoryTokenStore {
    MemoryTokenStore::with(TokenSet {
        access_token: String::new(),
        refresh_token: refresh_token.to_owned(),
        expires_at: 0,
    })
}

/// 拿着测试文件名；不管测试怎么结束（包括 panic），`Drop` 都会删掉它。
struct TempFile {
    endpoints: Endpoints,
    client_id: String,
    refresh_token: String,
    proxy: Option<SyncProxy>,
    name: Option<String>,
}

impl TempFile {
    async fn exists(&self, raw: &dyn RawObjectStore) -> bool {
        raw.get().await.expect("读测试文件").is_some()
    }

    /// 正常结束时调用：删掉并确认已经不存在。
    async fn finish(mut self, raw: &dyn RawObjectStore) {
        let name = self.name.take().expect("还没删");
        raw.delete(&Condition::None).await.expect("删除测试文件");
        assert!(!self.exists(raw).await, "删掉之后不该还在：{name}");
        println!("[onedrive] 测试文件已删除并确认不存在：{name}");
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let Some(name) = self.name.take() else {
            return;
        };
        // 走到这里说明测试中途失败了（panic）或提前返回：在单独的线程里用新的运行时删掉，
        // 不能在已经在运行时里的线程上再 block_on。
        let (endpoints, client_id, refresh_token, proxy) = (
            self.endpoints.clone(),
            self.client_id.clone(),
            self.refresh_token.clone(),
            self.proxy.clone(),
        );
        let target = name.clone();
        let outcome = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|runtime| {
                    runtime.block_on(async {
                        let store: Arc<dyn TokenStore> = Arc::new(starting_tokens(&refresh_token));
                        let raw = OneDriveRaw::new(endpoints, &client_id, proxy.as_ref(), store)
                            .map_err(|err| err.to_string())?;
                        let file = raw.sibling(&target);
                        file.delete(&Condition::None)
                            .await
                            .map(|_| ())
                            .map_err(|err| err.to_string())
                    })
                })
        })
        .join();
        println!("[onedrive] 测试中途失败，兜底删除测试文件 {name}：{outcome:?}");
    }
}

fn unique_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("zssh-live-test-{nanos}.json")
}

#[tokio::test]
async fn onedrive_live_refresh_conditional_write_and_cleanup() {
    if !common::full_live("onedrive_live_refresh_conditional_write_and_cleanup") {
        return;
    }
    let (Some(client_id), Some(refresh_token)) = (
        env("ZSSH_ONEDRIVE_CLIENT_ID"),
        env("ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN"),
    ) else {
        println!(
            "跳过 onedrive_live_refresh_conditional_write_and_cleanup：没设置 \
ZSSH_ONEDRIVE_CLIENT_ID 和 ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN（两个都要；刷新令牌需要用户用自己的账号手动授权一次）"
        );
        return;
    };
    run_scenario(
        Endpoints::microsoft(),
        &client_id,
        &refresh_token,
        proxy_from_env(),
    )
    .await;
}

/// 真实 OneDrive 和模拟服务共用的一套检查（下面的 `scenario_logic_is_checked_against_the_mock`
/// 在门禁里对着模拟服务跑它，免得到了真实环境才发现测试本身写错了）。
async fn run_scenario(
    endpoints: Endpoints,
    client_id: &str,
    refresh_token: &str,
    proxy: Option<SyncProxy>,
) {
    let client_id = client_id.to_owned();
    let refresh_token = refresh_token.to_owned();
    let store = Arc::new(starting_tokens(&refresh_token));

    // 1. 刷新令牌 → 访问令牌：第一次请求（测试连接）会自动刷新，并把轮换后的新刷新令牌存回 store。
    let probe = graph::probe(
        endpoints.clone(),
        &client_id,
        proxy.as_ref(),
        store.clone() as Arc<dyn TokenStore>,
    )
    .await
    .expect("测试连接");
    println!(
        "[onedrive] 读：{} / 写：{}",
        probe.read.message, probe.write.message
    );
    assert!(probe.read.ok, "读失败：{}", probe.read.message);
    assert!(probe.write.ok, "写失败：{}", probe.write.message);
    let concurrency = probe.concurrency.expect("检测并发保护");
    println!(
        "[onedrive] 并发保护：{:?}（{}）",
        concurrency.mode,
        concurrency.notes.join("；")
    );
    let saved = store.snapshot().expect("刷新之后 store 里有令牌");
    assert!(!saved.access_token.is_empty(), "刷新后应该拿到访问令牌");
    assert!(!saved.refresh_token.is_empty());
    println!(
        "[onedrive] 刷新令牌成功，访问令牌有效期约 {} 秒",
        saved.expires_at - mock_microsoft::now_secs()
    );

    // 2. 在应用文件夹里读写一个测试专用文件。
    let raw = OneDriveRaw::new(
        endpoints.clone(),
        &client_id,
        proxy.as_ref(),
        store.clone() as Arc<dyn TokenStore>,
    )
    .expect("存储");
    let name = unique_name();
    assert_ne!(name, OBJECT_NAME);
    let guard = TempFile {
        endpoints: endpoints.clone(),
        client_id: client_id.clone(),
        refresh_token: refresh_token.clone(),
        proxy: proxy.clone(),
        name: Some(name.clone()),
    };
    let file = raw.sibling(&name);
    println!("[onedrive] 测试文件：{name}");

    let RawPut::Written { etag: Some(etag1) } = file
        .put(b"live-test-1", &Condition::IfNoneMatchAny)
        .await
        .expect("首次上传")
    else {
        panic!("首次上传应该成功并返回 eTag");
    };
    let got = file.get().await.expect("读").expect("存在");
    assert_eq!(got.bytes, b"live-test-1");
    assert_eq!(
        got.etag.as_deref(),
        Some(etag1.as_str()),
        "读到的 eTag 和上传返回的一致"
    );

    // 3. 条件写入冲突：已存在时再“首次上传”、带错的 If-Match，都要被拒绝；带对的才成功。
    assert_eq!(
        file.put(b"live-test-x", &Condition::IfNoneMatchAny)
            .await
            .expect("再次首次上传"),
        RawPut::PreconditionFailed,
        "已存在时 conflictBehavior=fail 要被拒绝"
    );
    assert_eq!(
        file.put(
            b"live-test-x",
            &Condition::IfMatch("\"zssh-not-the-etag\"".to_owned())
        )
        .await
        .expect("错误的 If-Match"),
        RawPut::PreconditionFailed
    );
    assert_eq!(
        file.get().await.unwrap().unwrap().bytes,
        b"live-test-1",
        "被拒绝的写入没改文件"
    );
    let RawPut::Written { etag: Some(etag2) } = file
        .put(b"live-test-2", &Condition::IfMatch(etag1.clone()))
        .await
        .expect("带对 eTag 更新")
    else {
        panic!("带对 eTag 的更新应该成功");
    };
    assert_ne!(etag1, etag2, "内容变了 eTag 要变");
    // 用过期的 eTag 删除要被拒绝（返回 false），文件还在。
    assert!(!file
        .delete(&Condition::IfMatch(etag1))
        .await
        .expect("过期 eTag 删除"));
    assert!(guard.exists(file.as_ref()).await);

    // 4. 删除清理。
    guard.finish(file.as_ref()).await;
}

#[tokio::test]
async fn scenario_logic_is_checked_against_the_mock() {
    use crate::mock_microsoft::{MockMicrosoft, CLIENT_ID};
    let mock = MockMicrosoft::start().await;
    // 真实的微软换发新刷新令牌时不会作废旧的（官方文档）。
    mock.set_strict_rotation(false);
    let (_, refresh_token) = mock.mint_tokens();
    run_scenario(mock.endpoints(), CLIENT_ID, &refresh_token, None).await;
    assert!(mock.file_names().is_empty(), "测试文件和探测文件都要删干净");
}

// ---------------------------------------------------------------------------
// 清理逻辑本身的测试（对着模拟服务，不需要任何环境变量，门禁里就会跑）：
// “任何退出路径都要删”不能只靠人相信，三种退出路径各验证一次。

mod cleanup {
    use super::*;
    use crate::mock_microsoft::{MockMicrosoft, CLIENT_ID};

    async fn setup() -> (MockMicrosoft, TempFile, String) {
        let mock = MockMicrosoft::start().await;
        // 真实的微软换发新刷新令牌时不会作废旧的（官方文档），兜底删除用的就是启动时那个旧的。
        mock.set_strict_rotation(false);
        // 账号里本来就有别的文件（包括真正的同步包）：清理只能删自己建的这一个。
        mock.put_file("zssh-sync.json", b"real sync package".to_vec());
        mock.put_file("someone-elses.txt", b"keep me".to_vec());
        let (_, refresh_token) = mock.mint_tokens();
        let endpoints = mock.endpoints();
        let raw = OneDriveRaw::new(
            endpoints.clone(),
            CLIENT_ID,
            None,
            Arc::new(starting_tokens(&refresh_token)),
        )
        .expect("存储");
        let name = unique_name();
        let file = raw.sibling(&name);
        file.put(b"temp", &Condition::IfNoneMatchAny)
            .await
            .expect("建测试文件");
        assert!(mock.file(&name).is_some());
        let guard = TempFile {
            endpoints,
            client_id: CLIENT_ID.to_owned(),
            refresh_token,
            proxy: None,
            name: Some(name.clone()),
        };
        (mock, guard, name)
    }

    fn untouched(mock: &MockMicrosoft, name: &str) {
        assert!(mock.file(name).is_none(), "测试文件要被删掉");
        assert_eq!(
            mock.file("zssh-sync.json").as_deref(),
            Some(b"real sync package".as_slice()),
            "真正的同步包不能碰"
        );
        assert!(
            mock.file("someone-elses.txt").is_some(),
            "别的文件一个都不能少"
        );
    }

    // Drop 里要在另一个线程上跑新的运行时，同时模拟服务还要在这个测试的运行时里响应，
    // 所以这几个测试用多线程运行时（真实联网时没有这个问题）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_finish_deletes_only_our_file_and_confirms_it_is_gone() {
        let (mock, guard, name) = setup().await;
        let (_, refresh_token) = mock.mint_tokens();
        let raw = OneDriveRaw::new(
            mock.endpoints(),
            CLIENT_ID,
            None,
            Arc::new(starting_tokens(&refresh_token)),
        )
        .unwrap();
        let file = raw.sibling(&name);
        guard.finish(file.as_ref()).await;
        untouched(&mock, &name);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_early_return_still_deletes_it() {
        let (mock, guard, name) = setup().await;
        {
            let _guard = guard;
            // 模拟测试提前返回：作用域结束，guard 被丢掉，没有调用 finish。
        }
        untouched(&mock, &name);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panic_in_the_middle_of_the_test_still_deletes_it() {
        let (mock, guard, name) = setup().await;
        let task = tokio::spawn(async move {
            let _held = guard;
            assert_eq!(1 + 1, 3, "模拟测试中途断言失败");
        });
        assert!(task.await.is_err(), "任务应该 panic 了");
        untouched(&mock, &name);
    }
}
