//! 同步包的加密信封：主密码 → Argon2id → 256 位密钥 → XChaCha20-Poly1305。
//!
//! 头部写上 KDF 的全部参数，以后可以调高；`key_check` 只用来把“主密码错误”和
//! “文件被改过”分开（AEAD 本身分不清这两者）。密文和头部都参与认证：改一个字节
//! 就会解密失败。
//!
//! 参数选择：Argon2id **128 MiB / 8 次 / 1 并行**。
//!
//! 云端的同步包等于把所有 SSH 密码和私钥交给了存储服务商，主密码是唯一的保护，
//! 所以在“用户等得起”的前提下尽量加强：目标是解一次 300～500 ms，内存不超过
//! 256 MiB。**必须用 release 构建量**（debug 构建慢十几倍，S-1 当时在 debug 下量
//! 到 220 ms 的 19 MiB / 2 次，release 下其实只要 15 ms，等于没设防）。
//! 在开发机上 release 实测（`kdf_parameter_survey`，5 次取中位）：
//!
//! | m (MiB) | t | 耗时 |
//! | --- | --- | --- |
//! | 64 | 3 | 73 ms |
//! | 128 | 3 | 152 ms |
//! | 128 | 6 | 272 ms |
//! | 128 | 8 | 352 ms |
//! | 128 | 10 | 433 ms |
//! | 256 | 4 | 407 ms |
//!
//! 选 128 MiB × 8：落在目标区间中段，留出余量——别人的电脑更慢时也不至于超过
//! 半秒太多；内存只要 128 MiB，低配机器和后台线程都吃得消。同样的耗时下，内存
//! 越大对 GPU / ASIC 暴力破解越不利，但 256 MiB 已经是上限，且 8 次迭代比 256 MiB
//! × 4 次更容易在慢机器上守住耗时。参数写在包头，旧包（19 MiB / 2 次）照样能解；
//! 以后再调只需要改这里的默认值。

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::error::SyncError;

pub const FORMAT: &str = "zssh-sync";
/// 加密信封的版本。比它新的包一律不解析。
pub const FORMAT_VERSION: u32 = 1;
pub const KEY_BYTES: usize = 32;
pub const SALT_BYTES: usize = 16;
pub const NONCE_BYTES: usize = 24;
const KEY_CHECK_BYTES: usize = 16;
const KDF_ALGORITHM: &str = "argon2id";

/// 写进包头的 KDF 参数。单位：KiB、次数、并行度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub output_bytes: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            algorithm: KDF_ALGORITHM.to_owned(),
            memory_kib: 128 * 1024,
            iterations: 8,
            parallelism: 1,
            output_bytes: KEY_BYTES as u32,
        }
    }
}

impl KdfParams {
    fn validate(&self) -> Result<(), SyncError> {
        if self.algorithm != KDF_ALGORITHM {
            return Err(SyncError::UnsupportedKdf(self.algorithm.clone()));
        }
        if self.output_bytes as usize != KEY_BYTES {
            return Err(SyncError::UnsupportedKdf(format!(
                "输出 {} 字节（只支持 {KEY_BYTES}）",
                self.output_bytes
            )));
        }
        // 防止恶意包让解一次要几 GB / 几小时。
        if !(8 * 1024..=256 * 1024).contains(&self.memory_kib)
            || !(1..=64).contains(&self.iterations)
            || !(1..=64).contains(&self.parallelism)
        {
            return Err(SyncError::Format(format!(
                "KDF 参数超出范围（memory_kib={}, iterations={}, parallelism={}）",
                self.memory_kib, self.iterations, self.parallelism
            )));
        }
        Ok(())
    }

    fn derive(&self, password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_BYTES]>, SyncError> {
        self.validate()?;
        let params = Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(self.output_bytes as usize),
        )
        .map_err(|err| SyncError::Internal(format!("Argon2 参数不合法：{err}")))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = Zeroizing::new([0u8; KEY_BYTES]);
        argon
            .hash_password_into(password.as_bytes(), salt, key.as_mut())
            .map_err(|err| SyncError::Internal(format!("Argon2 派生失败：{err}")))?;
        Ok(key)
    }
}

/// 加密信封。字段顺序固定，AAD 就是这些字段的规范化 JSON。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub format: String,
    pub version: u32,
    pub kdf: KdfParams,
    /// base64，16 字节
    pub salt: String,
    /// base64，24 字节
    pub nonce: String,
    /// base64，XChaCha20-Poly1305 密文（含 16 字节认证标签）
    pub ciphertext: String,
    /// base64，16 字节；只用于区分“密码错”和“文件被改”。
    pub key_check: String,
}

#[derive(Serialize)]
struct Header<'a> {
    format: &'a str,
    version: u32,
    kdf: &'a KdfParams,
    salt: &'a str,
    nonce: &'a str,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(raw: &str) -> Result<Vec<u8>, SyncError> {
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| SyncError::Format("包里有不是 base64 的字段".to_owned()))
}

/// key_check：`SHA-256(标签 || 密钥 || 头部 JSON)` 的前 16 字节。
///
/// 密钥是 Argon2 的输出（高熵），这里是“密码验证器”，不是用来保护数据的 MAC；
/// 数据完整性由 AEAD 负责。攻击者能从它离线验证密码猜测，但从 AEAD 密文本来也能，
/// 所以没有额外损失。
fn key_check(key: &[u8; KEY_BYTES], aad: &[u8]) -> [u8; KEY_CHECK_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(b"zssh-sync-key-check-v1");
    hasher.update(key);
    hasher.update(aad);
    let digest = hasher.finalize();
    let mut out = [0u8; KEY_CHECK_BYTES];
    out.copy_from_slice(&digest[..KEY_CHECK_BYTES]);
    out
}

fn equal_constant_time(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 用给定的盐和 nonce 加密（固定测试向量用；生产走 [`seal`] 的随机值）。
pub fn seal_with(
    plaintext: &[u8],
    password: &str,
    kdf: KdfParams,
    salt: &[u8],
    nonce: &[u8],
) -> Result<Vec<u8>, SyncError> {
    if salt.len() != SALT_BYTES {
        return Err(SyncError::Internal("盐必须是 16 字节".to_owned()));
    }
    if nonce.len() != NONCE_BYTES {
        return Err(SyncError::Internal("nonce 必须是 24 字节".to_owned()));
    }
    let key = kdf.derive(password, salt)?;
    let header = Header {
        format: FORMAT,
        version: FORMAT_VERSION,
        kdf: &kdf,
        salt: &b64(salt),
        nonce: &b64(nonce),
    };
    let aad = serde_json::to_vec(&header)
        .map_err(|err| SyncError::Internal(format!("头部序列化失败：{err}")))?;
    let check = key_check(&key, &aad);

    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|err| SyncError::Internal(format!("密钥长度不对：{err}")))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| SyncError::Internal("加密失败".to_owned()))?;

    let envelope = Envelope {
        format: FORMAT.to_owned(),
        version: FORMAT_VERSION,
        kdf,
        salt: b64(salt),
        nonce: b64(nonce),
        ciphertext: b64(&ciphertext),
        key_check: b64(&check),
    };
    serde_json::to_vec(&envelope)
        .map_err(|err| SyncError::Internal(format!("同步包序列化失败：{err}")))
}

/// 随机盐 + 随机 nonce，用默认的 Argon2id 参数加密。
pub fn seal(plaintext: &[u8], password: &str) -> Result<Vec<u8>, SyncError> {
    let mut salt = [0u8; SALT_BYTES];
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::fill(&mut salt)
        .map_err(|err| SyncError::Internal(format!("系统随机数不可用：{err}")))?;
    getrandom::fill(&mut nonce)
        .map_err(|err| SyncError::Internal(format!("系统随机数不可用：{err}")))?;
    seal_with(plaintext, password, KdfParams::default(), &salt, &nonce)
}

/// 解开同步包，返回明文（调用方用完由 [`Zeroizing`] 清掉）。
///
/// 检查顺序很重要：**先看格式和版本，版本太新直接拒绝**，不做 Argon2、不解析明文。
pub fn open(bytes: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>, SyncError> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|err| SyncError::Format(format!("不是有效的加密包：{err}")))?;
    if envelope.format != FORMAT {
        return Err(SyncError::Format(format!(
            "不是 Z-SSH 同步包（format={}）",
            envelope.format
        )));
    }
    if envelope.version > FORMAT_VERSION {
        return Err(SyncError::FormatTooNew {
            found: envelope.version,
            supported: FORMAT_VERSION,
        });
    }
    if envelope.version < FORMAT_VERSION {
        return Err(SyncError::FormatTooOld {
            found: envelope.version,
        });
    }
    let salt = unb64(&envelope.salt)?;
    let nonce = unb64(&envelope.nonce)?;
    let expected_check = unb64(&envelope.key_check)?;
    let ciphertext = unb64(&envelope.ciphertext)?;
    if salt.len() != SALT_BYTES || nonce.len() != NONCE_BYTES {
        return Err(SyncError::Format("盐或 nonce 长度不对".to_owned()));
    }
    let key = envelope.kdf.derive(password, &salt)?;
    // 用**包里的**头部字段重算 AAD；任何一个字符被改过都会导致认证失败。
    let header = Header {
        format: FORMAT,
        version: FORMAT_VERSION,
        kdf: &envelope.kdf,
        salt: &envelope.salt,
        nonce: &envelope.nonce,
    };
    let aad = serde_json::to_vec(&header)
        .map_err(|err| SyncError::Internal(format!("头部序列化失败：{err}")))?;
    let check = key_check(&key, &aad);
    if !equal_constant_time(&check, &expected_check) {
        return Err(SyncError::WrongPassword);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|err| SyncError::Internal(format!("密钥长度不对：{err}")))?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| SyncError::Tampered)?;
    Ok(Zeroizing::new(plaintext))
}

/// 修改主密码：用旧密码解开整个包，再用新密码重新加密（明文只在内存里）。
pub fn change_password(
    package: &[u8],
    old_password: &str,
    new_password: &str,
) -> Result<Vec<u8>, SyncError> {
    let plaintext = open(package, old_password)?;
    seal(&plaintext, new_password)
}

#[cfg(test)]
mod tests;
