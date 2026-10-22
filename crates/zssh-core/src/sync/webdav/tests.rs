use super::*;

fn config(url: &str) -> WebDavConfig {
    WebDavConfig {
        url: url.to_owned(),
        directory: "Z-SSH/sync".to_owned(),
        username: "u".to_owned(),
        password: "p-very-secret".to_owned(),
        allow_insecure_http: false,
    }
}

#[test]
fn http_addresses_are_refused_until_the_user_confirms() {
    let plain = config("http://dav.example.internal/dav");
    assert!(plain.is_plain_http());
    let err = WebDavRaw::new(&plain, None)
        .err()
        .expect("没确认不能用 http");
    assert!(err.to_string().contains("明文"), "{err}");
    let mut confirmed = plain;
    confirmed.allow_insecure_http = true;
    assert!(WebDavRaw::new(&confirmed, None).is_ok());
    assert!(!config("https://dav.example.internal/").is_plain_http());
}

#[test]
fn other_schemes_and_embedded_credentials_are_rejected_or_stripped() {
    assert!(WebDavRaw::new(&config("ftp://example.internal/"), None).is_err());
    assert!(WebDavRaw::new(&config("not a url"), None).is_err());
    let raw = WebDavRaw::new(
        &config("https://user:secret@dav.example.internal/base"),
        None,
    )
    .expect("地址里的凭据被丢掉");
    let url = raw.object_url().to_string();
    assert!(!url.contains("secret") && !url.contains("user@"), "{url}");
}

#[test]
fn object_and_directory_urls_are_built_with_escaping() {
    let mut cfg = config("https://dav.example.internal/remote.php/dav/files/me");
    cfg.directory = "/Z SSH/同步/".to_owned();
    let raw = WebDavRaw::new(&cfg, None).expect("创建");
    assert_eq!(
        raw.object_url().as_str(),
        "https://dav.example.internal/remote.php/dav/files/me/Z%20SSH/%E5%90%8C%E6%AD%A5/zssh-sync.json"
    );
    assert_eq!(
        raw.url_for(1, None).as_str(),
        "https://dav.example.internal/remote.php/dav/files/me/Z%20SSH/"
    );
    assert_eq!(
        raw.url_for(2, None).as_str(),
        "https://dav.example.internal/remote.php/dav/files/me/Z%20SSH/%E5%90%8C%E6%AD%A5/"
    );
    // 路径穿越片段被丢掉
    cfg.directory = "../a/./b".to_owned();
    let raw = WebDavRaw::new(&cfg, None).expect("创建");
    assert_eq!(
        raw.object_url().as_str(),
        "https://dav.example.internal/remote.php/dav/files/me/a/b/zssh-sync.json"
    );
}

#[test]
fn debug_output_hides_the_password() {
    let text = format!("{:?}", config("https://dav.example.internal/"));
    assert!(!text.contains("very-secret"), "{text}");
}

// -- 响应体上限（C08） -----------------------------------------------------

#[tokio::test]
async fn an_oversized_package_is_refused_instead_of_read_into_memory() {
    use crate::sync::net::test_http::{spawn, Body, Canned};
    let addr = spawn(|_| {
        Canned::new(
            200,
            Body::Declared {
                declared: 8 * 1024 * 1024 * 1024,
                sent: b"xx".to_vec(),
            },
        )
    })
    .await;
    let mut cfg = config(&format!("http://{addr}/dav"));
    cfg.allow_insecure_http = true;
    let raw = WebDavRaw::new(&cfg, None).expect("创建");
    let err = raw.get().await.expect_err("超过上限要报错");
    assert!(err.to_string().contains("64 MiB 上限"), "{err}");
}
