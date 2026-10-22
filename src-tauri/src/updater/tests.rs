//! 自动更新的端到端测试：本地起静态文件服务（以及假的 HTTP / SOCKS5 代理），用**测试密钥**
//! 签过名的更新包走一遍“检查 → 下载 → 验签”；签名不对的必须被拒绝。
//! 真正“安装并重启到新版本”自动化做不了，写在 docs/manual-test.md 里手测。

use super::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// 测试用的更新签名：公钥 + 用对应私钥签过的文件（私钥不在仓库里）。
/// 生成方法见 tests/fixtures/updater/README.md。
const TEST_PUBKEY: &str = include_str!("../../tests/fixtures/updater/test.key.pub");
const PAYLOAD: &[u8] = include_bytes!("../../tests/fixtures/updater/payload.bin");
const PAYLOAD_SIG: &str = include_str!("../../tests/fixtures/updater/payload.bin.sig");
const ATTACKER_SIG: &str = include_str!("../../tests/fixtures/updater/payload.attacker.sig");
const OTHER_SIG: &str = include_str!("../../tests/fixtures/updater/other.bin.sig");

type Routes = Arc<HashMap<String, Vec<u8>>>;
type SharedTable = Arc<Mutex<HashMap<String, Vec<u8>>>>;

fn respond(stream: &mut TcpStream, routes: &Routes, path: &str) {
    let (status, body) = match routes.get(path) {
        Some(body) => ("200 OK", body.clone()),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if path.starts_with("/slow") {
        // 模拟慢网络：小块慢慢给，客户端才会收到很多次进度回调（本机回环一次就灌完了）。
        for piece in body.chunks(4096) {
            let _ = stream.write_all(piece);
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        return;
    }
    let _ = stream.write_all(&body);
}

/// 读到请求头结束，返回第一行。
fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
        if buf.len() > 16 * 1024 {
            return None;
        }
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .map(str::to_owned)
}

fn path_of(line: &str) -> String {
    line.split_whitespace().nth(1).unwrap_or("/").to_owned()
}

/// 本地静态文件服务；路由表在端口确定之后才能填（latest.json 里要写下载地址的端口），
/// 所以先返回端口和一个可以之后填内容的共享表。
fn start_static() -> (u16, SharedTable) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let table: SharedTable = Arc::default();
    let shared = Arc::clone(&table);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let snapshot: Routes = Arc::new(shared.lock().unwrap().clone());
            std::thread::spawn(move || {
                if let Some(line) = read_request_line(&mut stream) {
                    respond(&mut stream, &snapshot, &path_of(&line));
                }
            });
        }
    });
    (port, table)
}

/// 假的 HTTP 代理：只认绝对地址的请求，把请求的主机名记下来，内容从路由表里出。
fn start_http_proxy(routes: Routes, seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = Arc::clone(&routes);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let Some(line) = read_request_line(&mut stream) else {
                    return;
                };
                let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                if let Ok(url) = Url::parse(&target) {
                    seen.lock()
                        .unwrap()
                        .push(url.host_str().unwrap_or("").to_owned());
                    respond(&mut stream, &routes, url.path());
                }
            });
        }
    });
    port
}

/// 假的 SOCKS5（无认证）：记下请求的目标主机名，然后在这条连接上直接当 HTTP 服务器答。
fn start_socks5(routes: Routes, seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = Arc::clone(&routes);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let mut head = [0u8; 2];
                if stream.read_exact(&mut head).is_err() {
                    return;
                }
                let mut methods = vec![0u8; head[1] as usize];
                let _ = stream.read_exact(&mut methods);
                let _ = stream.write_all(&[5, 0]);
                let mut req = [0u8; 4];
                if stream.read_exact(&mut req).is_err() {
                    return;
                }
                let host = match req[3] {
                    3 => {
                        let mut len = [0u8; 1];
                        let _ = stream.read_exact(&mut len);
                        let mut name = vec![0u8; len[0] as usize];
                        let _ = stream.read_exact(&mut name);
                        String::from_utf8_lossy(&name).into_owned()
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        let _ = stream.read_exact(&mut ip);
                        format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
                    }
                    _ => return,
                };
                let mut dest_port = [0u8; 2];
                let _ = stream.read_exact(&mut dest_port);
                seen.lock().unwrap().push(host);
                let _ = stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
                if let Some(line) = read_request_line(&mut stream) {
                    respond(&mut stream, &routes, &path_of(&line));
                }
            });
        }
    });
    port
}

fn manifest(version: &str, signature: &str, url: &str) -> Vec<u8> {
    serde_json::json!({
        "version": version,
        "notes": "测试用的更新说明",
        "pub_date": "2026-10-18T00:00:00Z",
        "platforms": { "windows-x86_64": { "signature": signature.trim(), "url": url } }
    })
    .to_string()
    .into_bytes()
}

/// 带更新插件的假应用：公钥是测试公钥，允许 http 地址（只在测试里）。
fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".to_owned(),
        serde_json::json!({
            "pubkey": TEST_PUBKEY.trim(),
            "endpoints": [],
            "dangerousInsecureTransportProtocol": true
        }),
    );
    tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)
        .expect("build mock app")
}

fn options(endpoint: String, proxy: Option<Url>) -> CheckOptions {
    CheckOptions {
        endpoint: Some(Url::parse(&endpoint).expect("endpoint")),
        pubkey: Some(TEST_PUBKEY.trim().to_owned()),
        proxy,
    }
}

/// 起一个服务，放一份给定签名 / 内容的更新；返回 latest.json 地址。
fn publish(version: &str, signature: &str, body: Vec<u8>) -> String {
    let (port, table) = start_static();
    let mut table = table.lock().unwrap();
    table.insert(
        "/latest.json".to_owned(),
        manifest(
            version,
            signature,
            &format!("http://127.0.0.1:{port}/payload.bin"),
        ),
    );
    table.insert("/payload.bin".to_owned(), body);
    format!("http://127.0.0.1:{port}/latest.json")
}

#[tokio::test]
async fn newer_signed_release_is_found_downloaded_and_verified() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app();
    let handle = app.handle().clone();
    let update = check_with(&handle, &options(endpoint, None))
        .await
        .expect("检查成功")
        .expect("应该发现新版本");
    assert_eq!(update.version, "9.9.9");
    assert_eq!(update.body.as_deref(), Some("测试用的更新说明"));

    let mut last = 0u64;
    let bytes = download_verified(&update, |done, _| last = done)
        .await
        .expect("签名正确，下载应该成功");
    assert_eq!(bytes, PAYLOAD);
    assert_eq!(last, PAYLOAD.len() as u64, "进度回调要走到 100%");
}

#[tokio::test]
async fn same_or_older_version_is_not_an_update() {
    for version in ["0.1.0", "0.0.9"] {
        let endpoint = publish(version, PAYLOAD_SIG, PAYLOAD.to_vec());
        let app = mock_app();
        let found = check_with(&app.handle().clone(), &options(endpoint, None))
            .await
            .expect("检查成功");
        assert!(found.is_none(), "{version} 不比当前新，不该当成更新");
    }
}

/// 签名不对的三种情形：别人的私钥签的、签的是另一个文件、包被改过一个字节。
/// 都必须在下载阶段被拒绝，拿不到字节。
#[tokio::test]
async fn packages_with_bad_signatures_are_rejected() {
    let mut tampered = PAYLOAD.to_vec();
    tampered[0] ^= 0x01;
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("别人的私钥签的", ATTACKER_SIG, PAYLOAD.to_vec()),
        ("签的是另一个文件", OTHER_SIG, PAYLOAD.to_vec()),
        ("包被改过一个字节", PAYLOAD_SIG, tampered),
    ];
    for (label, signature, body) in cases {
        let endpoint = publish("9.9.9", signature, body);
        let app = mock_app();
        let handle = app.handle().clone();
        let update = check_with(&handle, &options(endpoint, None))
            .await
            .expect("检查本身成功")
            .expect("有新版本");
        let err = download_verified(&update, |_, _| {})
            .await
            .expect_err(label);
        let message = friendly_error(&err, Stage::Download);
        assert!(message.contains("签名校验没有通过"), "{label}：{message}");
    }
}

#[tokio::test]
async fn unreachable_server_gives_a_readable_error() {
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let app = mock_app();
    let Err(err) = check_with(
        &app.handle().clone(),
        &options(format!("http://127.0.0.1:{dead}/latest.json"), None),
    )
    .await
    else {
        panic!("连不上服务器却检查成功了");
    };
    let message = friendly_error(&err, Stage::Check);
    assert!(message.contains("无法连接到更新服务器"), "{message}");
}

/// 检查和下载都走默认代理：服务器域名根本解析不了（`.test`），只有代理能把它们送到。
#[tokio::test]
async fn check_and_download_go_through_http_and_socks5_proxies() {
    for kind in ["http", "socks5h"] {
        let mut table = HashMap::new();
        table.insert(
            "/latest.json".to_owned(),
            manifest("9.9.9", PAYLOAD_SIG, "http://updates.test/payload.bin"),
        );
        table.insert("/payload.bin".to_owned(), PAYLOAD.to_vec());
        let table: Routes = Arc::new(table);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let proxy_port = if kind == "http" {
            start_http_proxy(Arc::clone(&table), Arc::clone(&seen))
        } else {
            start_socks5(Arc::clone(&table), Arc::clone(&seen))
        };
        let proxy = Url::parse(&format!("{kind}://127.0.0.1:{proxy_port}")).unwrap();
        let app = mock_app();
        let handle = app.handle().clone();
        let update = check_with(
            &handle,
            &options("http://updates.test/latest.json".to_owned(), Some(proxy)),
        )
        .await
        .unwrap_or_else(|err| panic!("{kind} 代理：检查失败：{err}"))
        .expect("有新版本");
        let bytes = download_verified(&update, |_, _| {})
            .await
            .unwrap_or_else(|err| panic!("{kind} 代理：下载失败：{err}"));
        assert_eq!(bytes, PAYLOAD);
        let hosts = seen.lock().unwrap().clone();
        assert!(
            hosts.len() >= 2 && hosts.iter().all(|host| host == "updates.test"),
            "{kind}：检查和下载都该经过代理，代理看到的主机：{hosts:?}"
        );
    }
}

#[tokio::test]
async fn a_dead_proxy_fails_instead_of_silently_going_direct() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let proxy = Url::parse(&format!("http://127.0.0.1:{dead}")).unwrap();
    let app = mock_app();
    let result = check_with(&app.handle().clone(), &options(endpoint, Some(proxy))).await;
    assert!(result.is_err(), "代理不可用时不能悄悄改成直连");
}

#[test]
fn shipped_config_points_at_github_releases_over_https() {
    let conf: serde_json::Value =
        serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    let updater = &conf["plugins"]["updater"];
    assert!(!updater["pubkey"].as_str().unwrap().is_empty());
    let endpoint = updater["endpoints"][0].as_str().unwrap();
    assert!(endpoint.starts_with("https://"), "更新地址必须是 https");
    assert!(endpoint.ends_with("/releases/latest/download/latest.json"));
    // 公钥不能是测试公钥
    assert_ne!(
        updater["pubkey"].as_str().unwrap().trim(),
        TEST_PUBKEY.trim()
    );
}

#[test]
fn a_portable_copy_never_downloads_and_points_to_the_release_page() {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".to_owned(),
        serde_json::json!({
            "pubkey": TEST_PUBKEY.trim(),
            "endpoints": ["https://github.com/example-owner/example-repo/releases/latest/download/latest.json"]
        }),
    );
    let app = tauri::test::mock_builder()
        .build(context)
        .expect("build mock app");
    let handle = app.handle();

    let portable = UpdateController::with_portable(true);
    *lock(&portable.inner.status) = UpdateStatus::Available {
        version: "0.9.0".to_owned(),
        notes: None,
        date: None,
    };
    assert_eq!(
        portable.download_page(handle).as_deref(),
        Some("https://github.com/example-owner/example-repo/releases/tag/v0.9.0")
    );
    let refused = portable
        .refuse_if_portable(handle)
        .expect("免安装版要拒绝下载");
    assert!(
        matches!(&refused, UpdateStatus::Failed { stage: Stage::Download, message } if message == PORTABLE_MESSAGE),
        "{refused:?}"
    );
    assert_eq!(
        portable.status(),
        refused,
        "状态要改成请手动下载，界面才看得到"
    );

    let installed = UpdateController::with_portable(false);
    assert!(
        installed.refuse_if_portable(handle).is_none(),
        "安装版照常下载"
    );
}

/// 关于页的进度条靠状态事件：节流后事件数要远少于网络块数，最后一条"下载中"必须是 100%。
#[tokio::test]
async fn download_progress_is_throttled_and_ends_at_100_percent() {
    use tauri::Listener;
    let app = mock_app();
    let handle = app.handle().clone();
    let events: Arc<Mutex<Vec<UpdateStatus>>> = Arc::default();
    let sink = Arc::clone(&events);
    handle.listen_any(STATUS_EVENT, move |event| {
        let value: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        if value["phase"] == "downloading" {
            sink.lock().unwrap().push(UpdateStatus::Downloading {
                version: value["version"].as_str().unwrap().to_owned(),
                downloaded: value["downloaded"].as_u64().unwrap(),
                total: value["total"].as_u64(),
            });
        }
    });

    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let update = check_with(&handle, &options(endpoint, None))
        .await
        .expect("检查成功")
        .expect("有新版本");
    let controller = UpdateController::with_portable(false);
    *lock(&controller.inner.update) = Some(update);
    let status = controller.download(&handle).await;
    assert!(
        matches!(&status, UpdateStatus::Ready { version, .. } if version == "9.9.9"),
        "{status:?}"
    );
    let seen = events.lock().unwrap().clone();
    let size = PAYLOAD.len() as u64;
    match seen.last() {
        Some(UpdateStatus::Downloading {
            downloaded, total, ..
        }) => {
            assert_eq!(*downloaded, size, "最后一条进度要到 100%：{seen:?}");
            assert_eq!(*total, Some(size));
        }
        other => panic!("没有进度事件：{other:?}"),
    }

    // 慢慢到达的包：网络块很多，事件数要被节流住。
    events.lock().unwrap().clear();
    let (port, table) = start_static();
    {
        let mut table = table.lock().unwrap();
        table.insert(
            "/latest.json".to_owned(),
            manifest(
                "9.9.9",
                PAYLOAD_SIG,
                &format!("http://127.0.0.1:{port}/slow.bin"),
            ),
        );
        table.insert("/slow.bin".to_owned(), vec![7u8; 512 * 1024]);
    }
    let endpoint = format!("http://127.0.0.1:{port}/latest.json");
    let update = check_with(&handle, &options(endpoint, None))
        .await
        .expect("检查成功")
        .expect("有新版本");
    let mut chunks = 0usize;
    let _ = download_verified(&update, |_, _| chunks += 1).await;
    *lock(&controller.inner.update) = Some(update);
    let _ = controller.download(&handle).await;
    let pushed = events.lock().unwrap().len();
    assert!(chunks > 20, "慢包应该分成很多块到达（实际 {chunks} 块）");
    assert!(
        pushed < chunks / 2,
        "进度事件没有节流：{chunks} 个网络块推了 {pushed} 次"
    );
}

#[test]
fn periodic_check_skips_busy_ready_and_failed_install() {
    let failed = |stage| UpdateStatus::Failed {
        stage,
        message: "x".to_owned(),
    };
    // 还没发现新版本：要查
    assert!(periodic_check_allowed(&UpdateStatus::Idle));
    assert!(periodic_check_allowed(&UpdateStatus::UpToDate {
        checked_at: 1
    }));
    assert!(periodic_check_allowed(&failed(Stage::Check)));
    // 手动查到新版本但没下载：也查（这一轮转成自动，接着后台下载）
    assert!(periodic_check_allowed(&UpdateStatus::Available {
        version: "9.9.9".to_owned(),
        notes: None,
        date: None,
    }));
    // 正在忙 / 下好等安装 / 安装失败等用户处理：不重复查
    assert!(!periodic_check_allowed(&UpdateStatus::Checking));
    assert!(!periodic_check_allowed(&UpdateStatus::Downloading {
        version: "9.9.9".to_owned(),
        downloaded: 0,
        total: None,
    }));
    assert!(!periodic_check_allowed(&UpdateStatus::Ready {
        version: "9.9.9".to_owned(),
        notes: None,
    }));
    assert!(!periodic_check_allowed(&failed(Stage::Install)));
}

#[test]
fn recheck_interval_is_six_hours() {
    assert_eq!(RECHECK_INTERVAL, Duration::from_secs(6 * 60 * 60));
}

/// 自动下载失败要允许 6 小时后重查；只有安装失败仍然不重查（要等用户处理）。
#[test]
fn failed_download_allows_the_periodic_recheck_but_failed_install_does_not() {
    let failed = |stage| UpdateStatus::Failed {
        stage,
        message: "x".to_owned(),
    };
    assert!(periodic_check_allowed(&failed(Stage::Download)));
    assert!(!periodic_check_allowed(&failed(Stage::Install)));
}

fn available() -> UpdateStatus {
    UpdateStatus::Available {
        version: "9.9.9".to_owned(),
        notes: None,
        date: None,
    }
}

fn ready() -> UpdateStatus {
    UpdateStatus::Ready {
        version: "9.9.9".to_owned(),
        notes: None,
    }
}

#[test]
fn auto_download_only_for_automatic_available_on_installed_builds() {
    // 自动发起 + 查到可用版本 + 安装版：下载
    assert!(should_auto_download(&available(), false, true));
    // 手动发起：不自动下载（弹对话框，用户点了才下）
    assert!(!should_auto_download(&available(), false, false));
    // 免安装版：不下载
    assert!(!should_auto_download(&available(), true, true));
    // 没有可用版本（已是最新 / 被跳过 / 检查失败 / 已经下好）：不下载
    for status in [
        UpdateStatus::Idle,
        UpdateStatus::Checking,
        UpdateStatus::UpToDate { checked_at: 1 },
        ready(),
        UpdateStatus::Failed {
            stage: Stage::Check,
            message: "x".to_owned(),
        },
    ] {
        assert!(!should_auto_download(&status, false, true), "{status:?}");
    }
}

#[test]
fn install_on_exit_needs_a_downloaded_package_on_an_installed_build() {
    // Ready + 有更新信息 + 有包 + 安装版 + 开关开着：装
    assert!(should_install_on_exit(&ready(), true, true, false, true));
    // 还在下载 / 只是发现了新版本：包不完整，不装
    let downloading = UpdateStatus::Downloading {
        version: "9.9.9".to_owned(),
        downloaded: 10,
        total: Some(100),
    };
    assert!(!should_install_on_exit(
        &downloading,
        true,
        false,
        false,
        true
    ));
    assert!(!should_install_on_exit(
        &available(),
        true,
        false,
        false,
        true
    ));
    // 免安装版：不装
    assert!(!should_install_on_exit(&ready(), true, true, true, true));
    // 状态是 Ready 但包或更新信息不在内存里（比如被跳过清掉了）：不装
    assert!(!should_install_on_exit(&ready(), true, false, false, true));
    assert!(!should_install_on_exit(&ready(), false, true, false, true));
    // 用户把「启动时自动检查」关了（或读不出来）：不装
    assert!(!should_install_on_exit(&ready(), true, true, false, false));
    // 其他状态都不装
    for status in [
        UpdateStatus::Idle,
        UpdateStatus::Checking,
        UpdateStatus::UpToDate { checked_at: 1 },
        UpdateStatus::Failed {
            stage: Stage::Download,
            message: "x".to_owned(),
        },
    ] {
        assert!(
            !should_install_on_exit(&status, true, true, false, true),
            "{status:?}"
        );
    }
}

/// 带内存库的假应用：自动检查要读"跳过的版本"，所以需要一个 AppState。
fn mock_app_with_state() -> tauri::App<tauri::test::MockRuntime> {
    let app = mock_app();
    let store = zssh_core::Database::open_in_memory().expect("内存库");
    let dir = tempfile::Builder::new()
        .prefix("zssh-test-known-hosts-")
        .tempdir()
        .expect("临时目录");
    let known_hosts = zssh_core::ssh::KnownHosts::new(dir.path().join("known_hosts"));
    app.manage(AppState::new(
        store,
        known_hosts,
        zssh_core::ssh::PoolConfig::default(),
    ));
    app
}

fn bytes_in_memory(controller: &UpdateController) -> bool {
    lock(&controller.inner.bytes).is_some()
}

#[tokio::test]
async fn automatic_check_downloads_in_the_background_and_stays_automatic() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(false);
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    assert!(
        matches!(controller.status(), UpdateStatus::Ready { ref version, .. } if version == "9.9.9"),
        "{:?}",
        controller.status()
    );
    assert!(bytes_in_memory(&controller), "包应该在内存里等退出时安装");
    assert!(controller.is_auto(), "整个流程都是自动的，底部不提示");
}

#[tokio::test]
async fn automatic_flow_events_carry_the_auto_flag() {
    use tauri::Listener;
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    handle.listen_any(STATUS_EVENT, move |event| {
        let value: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        sink.lock().unwrap().push((
            value["phase"].as_str().unwrap().to_owned(),
            value["auto"].as_bool().unwrap(),
        ));
    });
    let controller = UpdateController::with_portable(false);
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    let seen = seen.lock().unwrap().clone();
    let phases: Vec<&str> = seen.iter().map(|(phase, _)| phase.as_str()).collect();
    assert!(phases.contains(&"available"), "{phases:?}");
    assert!(phases.contains(&"downloading"), "{phases:?}");
    assert_eq!(phases.last(), Some(&"ready"), "{phases:?}");
    assert!(seen.iter().all(|(_, auto)| *auto), "{seen:?}");
}

#[tokio::test]
async fn manual_check_stops_at_available_and_is_marked_manual() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(false);
    let status = controller
        .check_using(&handle, true, options(endpoint, None))
        .await;
    assert!(
        matches!(status, UpdateStatus::Available { .. }),
        "{status:?}"
    );
    assert!(!bytes_in_memory(&controller), "手动检查不自动下载");
    assert!(!controller.is_auto(), "手动检查引出的状态要照旧提示");

    // 用户点了「立即更新」：下载，状态仍是手动的。
    let status = controller.download(&handle).await;
    assert!(matches!(status, UpdateStatus::Ready { .. }), "{status:?}");
    assert!(!controller.is_auto());
}

#[tokio::test]
async fn a_new_automatic_round_clears_the_manual_mark() {
    let endpoint = publish("0.1.0", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(false);
    controller
        .check_using(&handle, true, options(endpoint.clone(), None))
        .await;
    assert!(!controller.is_auto());
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    assert!(controller.is_auto(), "定时检查开始后又是自动流程");
}

#[tokio::test]
async fn a_skipped_version_is_neither_downloaded_nor_installed() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let state = handle.state::<AppState>();
    updater::skip_version(&state.store, "9.9.9").expect("记下跳过的版本");
    let controller = UpdateController::with_portable(false);
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    assert!(
        matches!(controller.status(), UpdateStatus::UpToDate { .. }),
        "{:?}",
        controller.status()
    );
    assert!(!bytes_in_memory(&controller));
    assert!(lock(&controller.inner.update).is_none());
}

#[tokio::test]
async fn a_portable_copy_announces_but_never_downloads_automatically() {
    let endpoint = publish("9.9.9", PAYLOAD_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(true);
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    assert!(
        matches!(controller.status(), UpdateStatus::Available { .. }),
        "{:?}",
        controller.status()
    );
    assert!(!bytes_in_memory(&controller));
}

#[tokio::test]
async fn automatic_download_failure_is_recorded_quietly_and_can_be_retried() {
    // 签名对不上：下载阶段被拒绝
    let endpoint = publish("9.9.9", ATTACKER_SIG, PAYLOAD.to_vec());
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(false);
    controller
        .auto_check_using(&handle, options(endpoint, None))
        .await;
    let status = controller.status();
    assert!(
        matches!(
            &status,
            UpdateStatus::Failed {
                stage: Stage::Download,
                ..
            }
        ),
        "{status:?}"
    );
    assert!(controller.is_auto(), "自动下载失败不提示");
    assert!(!bytes_in_memory(&controller));
    assert!(periodic_check_allowed(&status), "6 小时后要能重新检查");
}

/// 没有包时 `install_on_exit` 必须直接返回（真正的安装步骤不在单元测试里跑）。
#[tokio::test]
async fn exiting_without_a_downloaded_package_installs_nothing() {
    let app = mock_app_with_state();
    let handle = app.handle().clone();
    let controller = UpdateController::with_portable(false);
    controller.install_on_exit(&handle);
    // Ready 但内存里没有包 / 更新信息：同样不装
    *lock(&controller.inner.status) = ready();
    controller.install_on_exit(&handle);
}
