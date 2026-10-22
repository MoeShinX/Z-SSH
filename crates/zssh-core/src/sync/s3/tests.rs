use super::*;

fn config(path_style: bool) -> S3Config {
    S3Config {
        endpoint: "https://s3.example.internal:9000".to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "my-bucket".to_owned(),
        access_key: "AK".to_owned(),
        secret_key: "SK-very-secret".to_owned(),
        prefix: "/team a/zssh/".to_owned(),
        path_style,
    }
}

#[test]
fn path_style_and_virtual_hosted_style_urls() {
    let (url, canonical) = object_location(&config(true), OBJECT_NAME).expect("路径风格");
    assert_eq!(
        url.as_str(),
        "https://s3.example.internal:9000/my-bucket/team%20a/zssh/zssh-sync.json"
    );
    assert_eq!(canonical, "/my-bucket/team%20a/zssh/zssh-sync.json");

    let (url, canonical) = object_location(&config(false), OBJECT_NAME).expect("虚拟主机风格");
    assert_eq!(
        url.as_str(),
        "https://my-bucket.s3.example.internal:9000/team%20a/zssh/zssh-sync.json"
    );
    assert_eq!(canonical, "/team%20a/zssh/zssh-sync.json");
}

#[test]
fn empty_prefix_and_endpoint_with_a_base_path() {
    let mut cfg = config(true);
    cfg.prefix = String::new();
    cfg.endpoint = "http://127.0.0.1:9000/s3/".to_owned();
    let (url, _) = object_location(&cfg, "x.tmp").expect("无前缀");
    assert_eq!(url.as_str(), "http://127.0.0.1:9000/s3/my-bucket/x.tmp");
}

#[test]
fn bad_configurations_are_rejected_at_construction() {
    let mut cfg = config(true);
    cfg.bucket = "a/b".to_owned();
    assert!(S3Raw::new(&cfg, None).is_err());
    let mut cfg = config(true);
    cfg.endpoint = "ftp://x".to_owned();
    assert!(S3Raw::new(&cfg, None).is_err());
    let mut cfg = config(true);
    cfg.region = " ".to_owned();
    assert!(S3Raw::new(&cfg, None).is_err());
    let mut cfg = config(true);
    cfg.secret_key = String::new();
    assert!(S3Raw::new(&cfg, None).is_err());
}

#[test]
fn error_codes_are_extracted_from_the_xml_body() {
    assert_eq!(
        error_code("<Error><Code>SignatureDoesNotMatch</Code><Message>x</Message></Error>")
            .as_deref(),
        Some("SignatureDoesNotMatch")
    );
    assert_eq!(error_code("not xml"), None);
}

#[test]
fn debug_output_hides_the_secret() {
    let text = format!("{:?}", config(true));
    assert!(!text.contains("very-secret"), "{text}");
}

// -- 响应体上限（C08） -----------------------------------------------------

fn local_config(addr: std::net::SocketAddr) -> S3Config {
    S3Config {
        endpoint: format!("http://{addr}"),
        prefix: String::new(),
        ..config(true)
    }
}

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
    let raw = S3Raw::new(&local_config(addr), None).expect("创建");
    let err = raw.get().await.expect_err("超过上限要报错");
    assert!(err.to_string().contains("64 MiB 上限"), "{err}");
}

#[tokio::test]
async fn huge_error_bodies_are_only_read_up_to_the_limit() {
    use crate::sync::net::test_http::{spawn, Body, Canned};
    // 错误响应体比元数据上限大：只读开头，仍按状态码给出原来的错误。
    let addr = spawn(|_| {
        Canned::new(
            403,
            Body::Chunked {
                total: 3 * 1024 * 1024,
            },
        )
    })
    .await;
    let raw = S3Raw::new(&local_config(addr), None).expect("创建");
    let err = raw.get().await.expect_err("403");
    assert!(matches!(err, StorageError::AuthFailed(_)), "{err:?}");

    let addr = spawn(|_| {
        Canned::new(
            404,
            Body::Chunked {
                total: 3 * 1024 * 1024,
            },
        )
    })
    .await;
    let raw = S3Raw::new(&local_config(addr), None).expect("创建");
    assert_eq!(raw.get().await.expect("404 = 没有对象"), None);
}
