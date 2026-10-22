use std::time::Instant;

use super::*;

const FIXED_PASSWORD: &str = "fixed-vector-password-不是真的";
const FIXED_PLAINTEXT: &[u8] = b"zssh-sync fixed test vector v1\n";

fn fixed_salt() -> Vec<u8> {
    (0u8..16).collect()
}

fn fixed_nonce() -> Vec<u8> {
    (16u8..40).collect()
}

fn fixed_envelope() -> Vec<u8> {
    seal_with(
        FIXED_PLAINTEXT,
        FIXED_PASSWORD,
        KdfParams::default(),
        &fixed_salt(),
        &fixed_nonce(),
    )
    .expect("固定向量应该能加密")
}

#[test]
fn roundtrip_returns_the_plaintext() {
    let sealed = seal(b"hello sync", "master-password").expect("加密");
    let opened = open(&sealed, "master-password").expect("解密");
    assert_eq!(opened.as_slice(), b"hello sync");
    // 密文里不能出现明文片段。
    let raw = String::from_utf8_lossy(&sealed);
    assert!(!raw.contains("hello sync"));
}

#[test]
fn wrong_password_and_tampering_are_reported_separately() {
    let sealed = seal(b"payload", "right-password").expect("加密");
    let err = open(&sealed, "wrong-password").expect_err("密码错要拒绝");
    assert!(matches!(err, SyncError::WrongPassword), "{err}");

    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    let mut ciphertext = base64::engine::general_purpose::STANDARD
        .decode(&envelope.ciphertext)
        .unwrap();
    ciphertext[0] ^= 0x01;
    envelope.ciphertext = base64::engine::general_purpose::STANDARD.encode(&ciphertext);
    let tampered = serde_json::to_vec(&envelope).unwrap();
    let err = open(&tampered, "right-password").expect_err("改过要拒绝");
    assert!(matches!(err, SyncError::Tampered), "{err}");
}

#[test]
fn format_version_too_new_is_rejected_before_any_parsing() {
    let sealed = seal(b"payload", "pw").expect("加密");
    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.version = FORMAT_VERSION + 1;
    envelope.ciphertext = "not-base64-and-not-touched".to_owned();
    let bytes = serde_json::to_vec(&envelope).unwrap();
    let err = open(&bytes, "pw").expect_err("版本太新要拒绝");
    assert!(
        matches!(
            err,
            SyncError::FormatTooNew {
                found: 2,
                supported: 1
            }
        ),
        "{err}"
    );
    // 错误信息要提示升级，且不能出现任何密文/明文。
    assert!(err.to_string().contains("升级"), "{err}");
}

#[test]
fn format_version_too_old_and_wrong_format_are_rejected() {
    let sealed = seal(b"payload", "pw").expect("加密");
    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.version = 0;
    let err = open(&serde_json::to_vec(&envelope).unwrap(), "pw").expect_err("太旧要拒绝");
    assert!(matches!(err, SyncError::FormatTooOld { found: 0 }), "{err}");

    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.format = "something-else".to_owned();
    let err = open(&serde_json::to_vec(&envelope).unwrap(), "pw").expect_err("格式不对");
    assert!(matches!(err, SyncError::Format(_)), "{err}");
}

#[test]
fn unknown_or_absurd_kdf_parameters_are_rejected() {
    let sealed = seal(b"payload", "pw").expect("加密");
    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.kdf.algorithm = "pbkdf2".to_owned();
    let err = open(&serde_json::to_vec(&envelope).unwrap(), "pw").expect_err("算法不认识");
    assert!(matches!(err, SyncError::UnsupportedKdf(_)), "{err}");

    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.kdf.memory_kib = 64 * 1024 * 1024;
    let err = open(&serde_json::to_vec(&envelope).unwrap(), "pw").expect_err("参数离谱");
    assert!(matches!(err, SyncError::Format(_)), "{err}");
}

#[test]
fn changing_the_header_is_detected() {
    let sealed = seal(b"payload", "pw").expect("加密");
    let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    envelope.nonce = base64::engine::general_purpose::STANDARD.encode([9u8; NONCE_BYTES]);
    let err = open(&serde_json::to_vec(&envelope).unwrap(), "pw").expect_err("改 nonce");
    assert!(
        matches!(err, SyncError::Tampered | SyncError::WrongPassword),
        "{err}"
    );
}

#[test]
fn changing_the_master_password_reencrypts_the_same_plaintext() {
    let old = seal(b"my data", "old-password").expect("加密");
    let opened = open(&old, "old-password").expect("解开");
    let new = seal(&opened, "new-password").expect("重新加密");
    assert_eq!(
        open(&new, "new-password").expect("新密码解开").as_slice(),
        b"my data"
    );
    assert!(matches!(
        open(&new, "old-password").expect_err("旧密码不能解"),
        SyncError::WrongPassword
    ));
}

#[test]
fn changing_the_password_reencrypts_the_whole_package() {
    let old = seal(b"data that must survive", "old-password").expect("加密");
    let new = change_password(&old, "old-password", "new-password").expect("换密码");
    assert_eq!(
        open(&new, "new-password").expect("新密码").as_slice(),
        b"data that must survive"
    );
    assert!(matches!(
        open(&new, "old-password").expect_err("旧密码不能解"),
        SyncError::WrongPassword
    ));
    assert!(matches!(
        change_password(&old, "wrong", "new").expect_err("旧密码错"),
        SyncError::WrongPassword
    ));
}

#[test]
fn fixed_vector_locks_the_envelope_format() {
    // 这组值是用固定盐（00..0f）和固定 nonce（10..27）生成的。以后改算法、参数、
    // 字段顺序或 AAD 都会让这里的值变；变之前先想清楚旧包还能不能解开。
    let sealed = fixed_envelope();
    let envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    assert_eq!(envelope.format, FORMAT);
    assert_eq!(envelope.version, FORMAT_VERSION);
    assert_eq!(envelope.kdf, KdfParams::default());
    assert_eq!(envelope.salt, "AAECAwQFBgcICQoLDA0ODw==");
    assert_eq!(envelope.nonce, "EBESExQVFhcYGRobHB0eHyAhIiMkJSYn");
    assert_eq!(
        envelope.ciphertext,
        "oIlyzXI0HEfc/9KVx8Pv9E90ZRcWUTcFloA4x9sxyau2Qyer0neexPN0ogw/R9U="
    );
    assert_eq!(envelope.key_check, "Nu66cN0bNjAHUz3u2ByMjA==");
    assert_eq!(
        open(&sealed, FIXED_PASSWORD)
            .expect("解开固定向量")
            .as_slice(),
        FIXED_PLAINTEXT
    );
    // 同样的输入必须逐字节相同（格式稳定，不是靠随机值碰巧对上）。
    assert_eq!(sealed, fixed_envelope());
}

#[test]
fn default_parameters_are_128_mib_8_passes() {
    let params = KdfParams::default();
    assert_eq!(
        (params.memory_kib, params.iterations, params.parallelism),
        (131_072, 8, 1)
    );
}

#[test]
fn packages_written_with_the_old_19_mib_parameters_still_open() {
    // S-1 的默认参数（19 MiB / 2 次）写出来的包：参数在包头里，升级默认值后照样能解。
    let old = KdfParams {
        memory_kib: 19 * 1024,
        iterations: 2,
        ..KdfParams::default()
    };
    let sealed = seal_with(
        FIXED_PLAINTEXT,
        FIXED_PASSWORD,
        old.clone(),
        &fixed_salt(),
        &fixed_nonce(),
    )
    .expect("旧参数加密");
    let envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
    assert_eq!(envelope.kdf, old);
    assert_eq!(
        envelope.ciphertext,
        "OzArlYmw/478cDe8ICFsfujBSEpdJbUbqdHiiaz4unIH5CgO6o60042e7KSjx/g="
    );
    assert_eq!(envelope.key_check, "YZqzWQDUPx7asLoNbjUs1A==");
    assert_eq!(
        open(&sealed, FIXED_PASSWORD)
            .expect("旧参数的包能解")
            .as_slice(),
        FIXED_PLAINTEXT
    );
}

/// 生成固定测试向量用：`cargo test -p zssh-core print_fixed_vector -- --ignored --nocapture`
#[test]
#[ignore]
fn print_fixed_vector() {
    let sealed = fixed_envelope();
    let envelope: Envelope = serde_json::from_slice(&sealed).unwrap();
    println!("ciphertext = {}", envelope.ciphertext);
    println!("key_check = {}", envelope.key_check);
    println!("salt = {}", envelope.salt);
    println!("nonce = {}", envelope.nonce);
}

#[test]
fn argon2id_decryption_is_fast_enough() {
    let sealed = seal(b"payload", "pw").expect("加密");
    let started = Instant::now();
    let opened = open(&sealed, "pw").expect("解密");
    let elapsed = started.elapsed();
    println!(
        "Argon2id 参数：m={} KiB, t={}, p={}；解一次耗时 {:?}",
        KdfParams::default().memory_kib,
        KdfParams::default().iterations,
        KdfParams::default().parallelism,
        elapsed
    );
    assert_eq!(opened.as_slice(), b"payload");
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "解密超过 2 秒，参数太激进：{elapsed:?}"
    );
}

/// 选参数用：`cargo test --release -p zssh-core kdf_parameter_survey -- --ignored --nocapture`。
/// 一定要用 release 构建量，debug 构建会慢十几倍。
#[test]
#[ignore]
fn kdf_parameter_survey() {
    for (memory_kib, iterations) in [
        (64 * 1024, 3),
        (128 * 1024, 3),
        (128 * 1024, 6),
        (128 * 1024, 8),
        (128 * 1024, 10),
        (192 * 1024, 6),
        (256 * 1024, 4),
        (256 * 1024, 5),
    ] {
        let params = KdfParams {
            memory_kib,
            iterations,
            ..KdfParams::default()
        };
        let mut samples = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            params.derive("pw", &[7u8; 16]).expect("派生");
            samples.push(started.elapsed());
        }
        samples.sort();
        println!(
            "m={memory_kib} KiB t={iterations} p=1：中位 {:?}，最慢 {:?}",
            samples[2], samples[4]
        );
    }
}
