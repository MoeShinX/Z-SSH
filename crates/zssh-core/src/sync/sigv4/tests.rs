use super::*;

fn creds() -> Credentials {
    Credentials {
        access_key: "AKIAIOSFODNN7EXAMPLE".to_owned(),
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
    }
}

fn header(name: &str, value: &str) -> (String, String) {
    (name.to_owned(), value.to_owned())
}

#[test]
fn hmac_matches_rfc_4231_vectors() {
    // RFC 4231 测试用例 1 和 2，以及密钥比分组长的用例 6。
    assert_eq!(
        hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hex(&hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

/// AWS 文档《Signature Calculations for the Authorization Header》里的 GET Object 示例。
#[test]
fn aws_documentation_get_object_example() {
    let headers = vec![
        header("host", "examplebucket.s3.amazonaws.com"),
        header("range", "bytes=0-9"),
        header(
            "x-amz-content-sha256",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        header("x-amz-date", "20130524T000000Z"),
    ];
    let signature = sign(
        &creds(),
        &SigningInput {
            method: "GET",
            canonical_uri: "/test.txt",
            canonical_query: "",
            headers: &headers,
            payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            region: "us-east-1",
            service: "s3",
            amz_date: "20130524T000000Z",
        },
    );
    assert_eq!(
        signature.signature,
        "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
    assert_eq!(
        signature.signed_headers,
        "host;range;x-amz-content-sha256;x-amz-date"
    );
    assert_eq!(
        signature.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
         SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
         Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
}

/// 同一份文档里的 PUT Object 示例（对象名含 `$`，带 `x-amz-storage-class`）。
#[test]
fn aws_documentation_put_object_example() {
    let payload = "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
    assert_eq!(sha256_hex(b"Welcome to Amazon S3."), payload);
    let headers = vec![
        header("date", "Fri, 24 May 2013 00:00:00 GMT"),
        header("host", "examplebucket.s3.amazonaws.com"),
        header("x-amz-content-sha256", payload),
        header("x-amz-date", "20130524T000000Z"),
        header("x-amz-storage-class", "REDUCED_REDUNDANCY"),
    ];
    let signature = sign(
        &creds(),
        &SigningInput {
            method: "PUT",
            canonical_uri: &uri_encode("/test$file.text", true),
            canonical_query: "",
            headers: &headers,
            payload_hash: payload,
            region: "us-east-1",
            service: "s3",
            amz_date: "20130524T000000Z",
        },
    );
    assert_eq!(
        signature.signature,
        "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
    );
}

#[test]
fn uri_encoding_follows_sigv4_rules() {
    assert_eq!(uri_encode("a b/c$d~e_f-g.h", true), "a%20b/c%24d~e_f-g.h");
    assert_eq!(uri_encode("a/b", false), "a%2Fb");
    assert_eq!(uri_encode("中", true), "%E4%B8%AD");
}

#[test]
fn amz_date_formats_utc_timestamps() {
    assert_eq!(amz_date(0), "19700101T000000Z");
    assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
    // 闰日和年末
    assert_eq!(amz_date(1_709_210_096), "20240229T123456Z");
    assert_eq!(amz_date(1_704_067_199), "20231231T235959Z");
}

#[test]
fn header_values_are_trimmed_and_collapsed() {
    let headers = vec![header("host", "h"), header("x-amz-meta-a", "  a   b  ")];
    let signature = sign(
        &creds(),
        &SigningInput {
            method: "GET",
            canonical_uri: "/",
            canonical_query: "",
            headers: &headers,
            payload_hash: "x",
            region: "r",
            service: "s3",
            amz_date: "20130524T000000Z",
        },
    );
    assert!(signature.canonical_request.contains("x-amz-meta-a:a b\n"));
}
