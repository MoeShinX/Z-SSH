use super::*;

fn config() -> GithubConfig {
    GithubConfig {
        api_base: String::new(),
        token: "ghp_exampletoken0123456789".to_owned(),
        gist_id: "0123456789abcdef0123456789abcdef".to_owned(),
        allow_insecure_http: false,
    }
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

// -- 构造 / 配置 ---------------------------------------------------------

#[test]
fn defaults_to_api_github_com_and_trims_the_trailing_slash() {
    let api = GithubApi::new(&config(), None, true).expect("默认地址");
    assert_eq!(api.base, "https://api.github.com");
    assert_eq!(api.host, "api.github.com");
    assert_eq!(api.gist_path(), "/gists/0123456789abcdef0123456789abcdef");

    let mut enterprise = config();
    enterprise.api_base = " https://ghe.example.internal/api/v3/ ".to_owned();
    let api = GithubApi::new(&enterprise, None, true).expect("GitHub Enterprise");
    assert_eq!(api.base, "https://ghe.example.internal/api/v3");
    assert_eq!(api.host, "ghe.example.internal");
}

#[test]
fn plain_http_needs_confirmation_because_the_token_would_be_sent_in_clear() {
    let mut cfg = config();
    cfg.api_base = "http://ghe.example.internal/api/v3".to_owned();
    assert!(cfg.is_plain_http());
    let err = GithubApi::new(&cfg, None, true).err().expect("没确认");
    assert!(err.to_string().contains("明文"), "{err}");
    cfg.allow_insecure_http = true;
    assert!(GithubApi::new(&cfg, None, true).is_ok());
    assert!(!config().is_plain_http());
}

#[test]
fn bad_addresses_ids_and_tokens_are_rejected_up_front() {
    for base in [
        "ftp://x.example.internal",
        "not a url",
        "https://u:p@h.example.internal",
    ] {
        let mut cfg = config();
        cfg.api_base = base.to_owned();
        assert!(GithubApi::new(&cfg, None, true).is_err(), "{base}");
    }
    let mut cfg = config();
    cfg.api_base = "https://h.example.internal/api?x=1".to_owned();
    assert!(GithubApi::new(&cfg, None, true).is_err(), "查询串");

    // Gist ID 要拼进路径：任何能改变路径的字符都不行。
    for id in ["", "a/b", "../x", "a b", "a?b", "a#b", &"x".repeat(65)] {
        let mut cfg = config();
        cfg.gist_id = id.to_owned();
        assert!(GithubApi::new(&cfg, None, true).is_err(), "{id:?}");
    }
    // 新建 Gist 时还没有 ID，不检查。
    let mut cfg = config();
    cfg.gist_id = String::new();
    assert!(GithubApi::new(&cfg, None, false).is_ok());

    let mut cfg = config();
    cfg.token = "  ".to_owned();
    assert!(GithubApi::new(&cfg, None, true).is_err());
    cfg.token = "ghp_bad\ntoken".to_owned();
    let err = GithubApi::new(&cfg, None, true).err().expect("换行");
    assert!(err.to_string().contains("不合法的字符"), "{err}");
}

#[test]
fn the_token_is_trimmed_marked_sensitive_and_never_printed() {
    let mut cfg = config();
    cfg.token = "  ghp_exampletoken0123456789\n".to_owned();
    let api = GithubApi::new(&cfg, None, true).expect("首尾空白去掉");
    assert!(api.token.is_sensitive());
    assert_eq!(
        api.token.to_str().unwrap(),
        "Bearer ghp_exampletoken0123456789"
    );
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("ghp_"), "{debug}");
    let err = GithubApi::new(
        &GithubConfig {
            gist_id: "bad id".to_owned(),
            ..cfg
        },
        None,
        true,
    )
    .err()
    .unwrap();
    assert!(
        !err.to_string().contains("ghp_"),
        "错误里也不能有令牌：{err}"
    );
}

// -- 限流 ----------------------------------------------------------------

#[test]
fn retry_after_wins_over_the_reset_time() {
    let wait = rate_limit_wait(
        StatusCode::FORBIDDEN,
        &headers(&[
            ("retry-after", "120"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1000000"),
        ]),
        "",
        1_000_000 - 50,
    );
    assert_eq!(wait, Some(120));
}

#[test]
fn the_reset_time_is_used_when_the_remaining_count_is_zero() {
    let now = 1_700_000_000;
    let wait = rate_limit_wait(
        StatusCode::FORBIDDEN,
        &headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &(now + 300).to_string()),
        ]),
        "API rate limit exceeded",
        now,
    );
    assert_eq!(wait, Some(300));
    // 重置时间已经过去（时钟有偏差）：至少等 1 秒，不会变成 0 / 负数而连续重试。
    let past = rate_limit_wait(
        StatusCode::FORBIDDEN,
        &headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &(now - 500).to_string()),
        ]),
        "",
        now,
    );
    assert_eq!(past, Some(1));
}

#[test]
fn status_429_and_secondary_limits_are_recognised() {
    assert_eq!(
        rate_limit_wait(StatusCode::TOO_MANY_REQUESTS, &headers(&[]), "", 0),
        Some(DEFAULT_WAIT_SECS),
        "429 没有任何时间信息：默认等 60 秒"
    );
    assert_eq!(
        rate_limit_wait(
            StatusCode::FORBIDDEN,
            &headers(&[]),
            "You have exceeded a secondary rate limit.",
            0
        ),
        Some(DEFAULT_WAIT_SECS),
        "次级限流有时只在信息里说"
    );
}

#[test]
fn an_ordinary_403_is_not_a_rate_limit_and_the_wait_is_capped() {
    assert_eq!(
        rate_limit_wait(
            StatusCode::FORBIDDEN,
            &headers(&[("x-ratelimit-remaining", "4999")]),
            "Resource not accessible by personal access token",
            0
        ),
        None,
        "还有剩余次数的 403 是权限问题"
    );
    assert_eq!(
        rate_limit_wait(
            StatusCode::NOT_FOUND,
            &headers(&[("retry-after", "5")]),
            "",
            0
        ),
        None
    );
    assert_eq!(
        rate_limit_wait(
            StatusCode::FORBIDDEN,
            &headers(&[("retry-after", "999999")]),
            "",
            0
        ),
        Some(MAX_WAIT_SECS)
    );
    assert_eq!(
        rate_limit_wait(
            StatusCode::FORBIDDEN,
            &headers(&[("retry-after", "0")]),
            "",
            0
        ),
        Some(1)
    );
}

#[test]
fn the_wait_is_described_in_seconds_minutes_or_hours() {
    use crate::sync::storage::wait_text;
    assert_eq!(wait_text(0), "约 1 秒");
    assert_eq!(wait_text(45), "约 45 秒");
    assert_eq!(wait_text(60), "约 1 分钟");
    assert_eq!(wait_text(61), "约 2 分钟");
    assert_eq!(wait_text(300), "约 5 分钟");
    assert_eq!(wait_text(2 * 3600), "约 120 分钟");
    assert_eq!(wait_text(3 * 3600), "约 3 小时");
    let err = StorageError::RateLimited {
        service: "GitHub".to_owned(),
        retry_after_secs: 300,
    };
    assert_eq!(err.to_string(), "GitHub 限流，约 5 分钟后重试");
}

// -- 解析 ----------------------------------------------------------------

fn gist_json(content: Option<&str>, truncated: bool, extra_file: bool) -> Value {
    let mut files = serde_json::Map::new();
    if let Some(content) = content {
        files.insert(
            FILE_NAME.to_owned(),
            json!({
                "filename": FILE_NAME,
                "size": content.len(),
                "truncated": truncated,
                "content": content,
                "raw_url": "https://gist.githubusercontent.com/someone/abc/raw/v2/zssh-sync.json",
            }),
        );
    }
    if extra_file {
        files.insert("notes.txt".to_owned(), json!({"content": "别的文件"}));
    }
    json!({
        "id": "abc",
        "owner": { "login": "someone" },
        "files": files,
        "history": [ {"version": "v2"}, {"version": "v1"} ],
    })
}

#[test]
fn the_version_is_the_newest_entry_in_history() {
    let info = GistInfo::parse(&gist_json(Some("{\"a\":1}"), false, false)).unwrap();
    assert_eq!(
        info.version.as_deref(),
        Some("v2"),
        "history 里最新的在最前面"
    );
    assert_eq!(info.owner.as_deref(), Some("someone"));
    assert!(info.has_package());
    assert!(!info.gist_truncated);

    let none = GistInfo::parse(&json!({"files": {}, "history": []})).unwrap();
    assert_eq!(none.version, None);
    assert!(GistInfo::parse(&json!("not an object")).is_err());
}

#[test]
fn a_missing_file_and_the_placeholder_both_mean_no_sync_data_yet() {
    let missing = GistInfo::parse(&gist_json(None, false, true)).unwrap();
    assert!(missing.file.is_none() && !missing.has_package());
    assert!(missing.package_file().is_none());

    let placeholder = GistInfo::parse(&gist_json(Some(PLACEHOLDER), false, false)).unwrap();
    assert!(placeholder.file.is_some());
    assert!(!placeholder.has_package(), "占位内容不是同步包");

    let real =
        GistInfo::parse(&gist_json(Some("{\"format\":\"zssh-sync\"}"), false, false)).unwrap();
    assert!(real.has_package());
}

#[test]
fn truncated_files_are_flagged_and_carry_the_raw_url() {
    let value = gist_json(Some("{\"cut\":"), true, false);
    let info = GistInfo::parse(&value).unwrap();
    let file = info.file.as_ref().unwrap();
    assert!(file.truncated);
    assert_eq!(
        file.raw_url.as_deref(),
        Some("https://gist.githubusercontent.com/someone/abc/raw/v2/zssh-sync.json")
    );
    assert_eq!(file.size, Some(7));
    // 被截断的内容就算恰好等于占位内容也不能当占位（它只是前半截）。
    let placeholder_prefix = GistInfo::parse(&gist_json(Some(PLACEHOLDER), true, false)).unwrap();
    assert!(placeholder_prefix.has_package());
}

#[test]
fn the_gist_level_truncation_flag_is_read() {
    let mut value = gist_json(None, false, true);
    value["truncated"] = json!(true);
    let info = GistInfo::parse(&value).unwrap();
    assert!(info.gist_truncated && info.file.is_none());
}

#[test]
fn error_messages_are_taken_from_the_json_body_only() {
    assert_eq!(
        body_message("{\"message\":\"Bad credentials\"}"),
        "Bad credentials"
    );
    assert_eq!(body_message("<html>502</html>"), "");
    assert_eq!(body_message(""), "");
}

#[test]
fn gist_ids_are_validated_before_they_reach_a_request_path() {
    assert!(valid_gist_id("0123456789abcdef0123456789abcdef"));
    assert!(valid_gist_id("12345"));
    assert!(valid_gist_id("mock-gist_1"));
    assert!(!valid_gist_id(""));
    assert!(!valid_gist_id("a/b"));
    assert!(!valid_gist_id("a.b"));
}

// -- 响应体上限（C08） -----------------------------------------------------

fn local_api(addr: std::net::SocketAddr) -> GithubApi {
    let cfg = GithubConfig {
        api_base: format!("http://{addr}"),
        allow_insecure_http: true,
        ..config()
    };
    GithubApi::new(&cfg, None, true).expect("创建")
}

fn truncated_gist(addr: std::net::SocketAddr, size: Option<u64>) -> Vec<u8> {
    let mut file = json!({
        "filename": FILE_NAME,
        "truncated": true,
        "content": "截断的开头",
        "raw_url": format!("http://{addr}/raw/zssh-sync.json"),
    });
    if let Some(size) = size {
        file["size"] = json!(size);
    }
    json!({
        "id": "abc",
        "owner": { "login": "someone" },
        "files": { FILE_NAME: file },
        "history": [ {"version": "v2"} ],
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn an_oversized_gist_response_is_refused() {
    use crate::sync::net::test_http::{spawn, Body, Canned};
    let addr = spawn(|_| {
        Canned::new(
            200,
            Body::Declared {
                declared: 8 * 1024 * 1024 * 1024,
                sent: b"{".to_vec(),
            },
        )
    })
    .await;
    let err = local_api(addr).read().await.expect_err("超过上限要报错");
    assert!(err.to_string().contains("64 MiB 上限"), "{err}");
}

#[tokio::test]
async fn the_raw_download_is_bounded_too() {
    use crate::sync::net::test_http::{spawn, Body, Canned};
    use std::sync::{Arc, Mutex};

    // raw_url 返回的内容声明了超大长度，Gist 接口没给 size：靠读取上限挡住。
    let me: Arc<Mutex<Option<std::net::SocketAddr>>> = Arc::default();
    let me_in = me.clone();
    let addr = spawn(move |line| {
        let addr = me_in.lock().unwrap().expect("地址已写入");
        if line.contains("/raw/") {
            Canned::new(
                200,
                Body::Declared {
                    declared: 8 * 1024 * 1024 * 1024,
                    sent: b"xx".to_vec(),
                },
            )
        } else {
            Canned::new(200, Body::Exact(truncated_gist(addr, None)))
        }
    })
    .await;
    *me.lock().unwrap() = Some(addr);
    let err = local_api(addr).read().await.expect_err("raw 超限");
    assert!(
        err.to_string()
            .contains("Gist 里的同步文件超过 64 MiB 上限"),
        "{err}"
    );

    // GitHub 报的 size 已经超限：根本不去下载 raw_url。
    let raw_hits = Arc::new(Mutex::new(0usize));
    let hits = raw_hits.clone();
    let me: Arc<Mutex<Option<std::net::SocketAddr>>> = Arc::default();
    let me_in = me.clone();
    let addr = spawn(move |line| {
        let addr = me_in.lock().unwrap().expect("地址已写入");
        if line.contains("/raw/") {
            *hits.lock().unwrap() += 1;
            Canned::new(200, Body::Exact(b"x".to_vec()))
        } else {
            Canned::new(
                200,
                Body::Exact(truncated_gist(addr, Some(MAX_PACKAGE_BYTES + 1))),
            )
        }
    })
    .await;
    *me.lock().unwrap() = Some(addr);
    let err = local_api(addr).read().await.expect_err("size 超限");
    assert!(err.to_string().contains("太大"), "{err}");
    assert_eq!(*raw_hits.lock().unwrap(), 0, "超限时不能去下载 raw_url");
}

#[tokio::test]
async fn huge_error_bodies_do_not_hide_the_status() {
    use crate::sync::net::test_http::{spawn, Body, Canned};
    let addr = spawn(|_| {
        Canned::new(
            401,
            Body::Chunked {
                total: 3 * 1024 * 1024,
            },
        )
    })
    .await;
    let err = local_api(addr).read().await.expect_err("401");
    assert!(matches!(err, StorageError::AuthFailed(_)), "{err:?}");
}
