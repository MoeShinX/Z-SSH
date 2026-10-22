//! PKCE（RFC 7636，S256）和 `state` 的生成。
//!
//! 公共客户端（桌面应用）没有客户端密钥，授权码被截获时全靠 PKCE 兜底：授权请求里只放
//! `code_challenge`（verifier 的 SHA-256），换令牌时才交出 `code_verifier`。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::login::LoginError;

/// 一次登录用的 PKCE 材料。`verifier` 只在内存里，换完令牌就丢。
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // verifier 是换令牌的钥匙，不打印。
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish()
    }
}

fn random_token(bytes: usize) -> Result<String, LoginError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|err| LoginError::Internal(format!("系统随机数不可用：{err}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// `code_challenge = BASE64URL(SHA256(code_verifier))`（无填充）。
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 32 字节随机数 → 43 个字符的 verifier（RFC 7636 要求 43～128 个字符）。
pub fn generate() -> Result<Pkce, LoginError> {
    let verifier = random_token(32)?;
    let challenge = challenge_for(&verifier);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// 防 CSRF 的随机 `state`。
pub fn random_state() -> Result<String, LoginError> {
    random_token(32)
}

/// 常量时间比较（`state` 不算高价值秘密，但比较时间不随前缀长度变化没有代价）。
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_the_rfc_7636_example() {
        // RFC 7636 附录 B 的测试向量。
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_valid_and_matches_its_challenge() {
        let pkce = generate().expect("生成");
        assert_eq!(pkce.verifier.len(), 43, "32 字节 → 43 个 base64url 字符");
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "只能是 RFC 7636 允许的字符：{}",
            pkce.verifier
        );
        assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
        assert_eq!(pkce.challenge.len(), 43, "SHA-256 → 43 个字符");
        assert!(!pkce.challenge.contains('='), "无填充");
    }

    #[test]
    fn every_login_gets_fresh_values() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(random_state().unwrap(), random_state().unwrap());
        assert!(random_state().unwrap().len() >= 32, "state 要有足够的熵");
    }

    #[test]
    fn debug_output_never_contains_the_verifier() {
        let pkce = generate().unwrap();
        let text = format!("{pkce:?}");
        assert!(!text.contains(&pkce.verifier), "{text}");
    }

    #[test]
    fn constant_time_eq_compares_whole_values() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
