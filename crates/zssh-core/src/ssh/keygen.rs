//! SSH 密钥：在 Z-SSH 里生成新密钥、从私钥算出公钥、把公钥装到服务器上。
//!
//! 生成的私钥是 OpenSSH 格式（ssh-keygen 默认的那种 openssh-key-v1 文本），存进凭据库的
//! "导入的私钥"（DPAPI 加密），和用户自己导入的私钥走同一套登录流程。

use russh::keys::ssh_key::LineEnding;
use russh::keys::{decode_secret_key, Algorithm, EcdsaCurve, HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::ZsshError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAlgorithm {
    /// 推荐：短、快、现在的服务器都支持。
    Ed25519,
    /// ECDSA P-256：有些老设备 / 合规要求只认 ECDSA。
    EcdsaP256,
    /// RSA 4096：兼容最老的服务器（生成要几秒）。
    Rsa4096,
}

impl KeyAlgorithm {
    fn algorithm(self) -> Algorithm {
        match self {
            Self::Ed25519 => Algorithm::Ed25519,
            Self::EcdsaP256 => Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            Self::Rsa4096 => Algorithm::Rsa { hash: None },
        }
    }
}

/// 生成出来的密钥。私钥文本用完就清零。
pub struct GeneratedKey {
    pub private_openssh: Zeroizing<String>,
    pub public_openssh: String,
    pub fingerprint: String,
}

/// 公钥注释只留安全字符：它会拼进装公钥的那条 shell 命令里。
pub fn sanitize_comment(comment: &str) -> String {
    let cleaned: String = comment
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+'))
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "z-ssh".to_owned()
    } else {
        cleaned
    }
}

/// 生成一把新密钥。`passphrase` 为空就不加密（私钥本来就存在 DPAPI 加密的凭据库里）。
/// RSA 4096 要几秒，调用方放到阻塞线程里。
pub fn generate(
    algorithm: KeyAlgorithm,
    comment: &str,
    passphrase: Option<&str>,
) -> Result<GeneratedKey, ZsshError> {
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, algorithm.algorithm())
        .map_err(|err| ZsshError::InvalidInput(format!("生成密钥失败：{err}")))?;
    key.set_comment(sanitize_comment(comment));
    let public_openssh = key
        .public_key()
        .to_openssh()
        .map_err(|err| ZsshError::InvalidInput(format!("导出公钥失败：{err}")))?;
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    let stored = match passphrase.filter(|p| !p.is_empty()) {
        Some(passphrase) => key
            .encrypt(&mut rng, passphrase)
            .map_err(|err| ZsshError::InvalidInput(format!("加密私钥失败：{err}")))?,
        None => key,
    };
    let private_openssh = stored
        .to_openssh(LineEnding::LF)
        .map_err(|err| ZsshError::InvalidInput(format!("导出私钥失败：{err}")))?;
    Ok(GeneratedKey {
        private_openssh,
        public_openssh,
        fingerprint,
    })
}

/// 从私钥文本（可能加了口令）算出公钥和指纹。
pub fn public_key_of(
    private_pem: &str,
    passphrase: Option<&str>,
) -> Result<(String, String), ZsshError> {
    let key =
        decode_secret_key(private_pem, passphrase.filter(|p| !p.is_empty())).map_err(|err| {
            ZsshError::InvalidInput(format!(
                "读不出这把私钥（口令不对，或者不是支持的私钥格式）：{err}"
            ))
        })?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|err| ZsshError::InvalidInput(format!("导出公钥失败：{err}")))?;
    Ok((public, key.fingerprint(HashAlg::Sha256).to_string()))
}

/// 把公钥追加到 `~/.ssh/authorized_keys` 的命令（POSIX shell）。已经有同一行就不重复加；
/// 目录和文件权限设成 sshd 要求的 700 / 600。
///
/// 公钥要是 `类型 base64 [注释]` 的样子，且只含安全字符（它在单引号里，不能带单引号）。
pub fn install_command(public_openssh: &str) -> Result<String, ZsshError> {
    let line = public_openssh.trim();
    let mut parts = line.split(' ');
    let kind = parts.next().unwrap_or_default();
    let data = parts.next().unwrap_or_default();
    let comment = parts.collect::<Vec<_>>().join(" ");
    let safe_kind = !kind.is_empty()
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '@'));
    let safe_data = !data.is_empty()
        && data
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    let safe_comment = comment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+' | ' '));
    if !(safe_kind && safe_data && safe_comment) || line.len() > 16 * 1024 {
        return Err(ZsshError::InvalidInput("公钥格式不对".to_owned()));
    }
    Ok(format!(
        "umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && \
         chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys && \
         if grep -qF '{kind} {data}' ~/.ssh/authorized_keys; then echo ZSSH_KEY_EXISTS; else \
         if [ -s ~/.ssh/authorized_keys ] && [ -n \"$(tail -c1 ~/.ssh/authorized_keys)\" ]; then \
         echo >> ~/.ssh/authorized_keys; fi; \
         printf '%s\\n' '{line}' >> ~/.ssh/authorized_keys && echo ZSSH_KEY_ADDED; fi"
    ))
}

/// 装公钥命令的输出：新加的 / 本来就有。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallOutcome {
    Added,
    AlreadyPresent,
}

pub fn parse_install_output(output: &str) -> Result<InstallOutcome, ZsshError> {
    if output.contains("ZSSH_KEY_ADDED") {
        Ok(InstallOutcome::Added)
    } else if output.contains("ZSSH_KEY_EXISTS") {
        Ok(InstallOutcome::AlreadyPresent)
    } else {
        Err(ZsshError::Shell(
            "服务器没有确认装好公钥（不是 POSIX shell，或者没有写 ~/.ssh 的权限）".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_with_and_without_passphrase() {
        for algorithm in [KeyAlgorithm::Ed25519, KeyAlgorithm::EcdsaP256] {
            let plain = generate(algorithm, "me@laptop", None).unwrap();
            assert!(plain
                .private_openssh
                .starts_with(concat!("-----BEGIN OPENSSH ", "PRIVATE KEY-----")));
            assert!(plain.public_openssh.ends_with(" me@laptop"));
            let (public, fingerprint) = public_key_of(&plain.private_openssh, None).unwrap();
            assert_eq!(public, plain.public_openssh);
            assert_eq!(fingerprint, plain.fingerprint);

            let locked = generate(algorithm, "x", Some("s3cret")).unwrap();
            assert!(
                public_key_of(&locked.private_openssh, None).is_err(),
                "没口令读不出"
            );
            assert!(public_key_of(&locked.private_openssh, Some("wrong")).is_err());
            let (public, _) = public_key_of(&locked.private_openssh, Some("s3cret")).unwrap();
            assert_eq!(public, locked.public_openssh);
        }
        let ed = generate(KeyAlgorithm::Ed25519, "", None).unwrap();
        assert!(ed.public_openssh.starts_with("ssh-ed25519 "));
        assert!(ed.fingerprint.starts_with("SHA256:"));
    }

    #[test]
    fn comments_are_sanitized() {
        assert_eq!(sanitize_comment("me@laptop"), "me@laptop");
        assert_eq!(sanitize_comment("a'; rm -rf /"), "arm-rf");
        assert_eq!(sanitize_comment("  "), "z-ssh");
    }

    #[test]
    fn install_command_only_takes_well_formed_keys() {
        let key = generate(KeyAlgorithm::Ed25519, "me@laptop", None).unwrap();
        let command = install_command(&key.public_openssh).unwrap();
        assert!(command.contains(&key.public_openssh));
        assert!(command.contains("chmod 600 ~/.ssh/authorized_keys"));
        assert!(install_command("ssh-ed25519 AAAA' ; rm -rf ~ ; '").is_err());
        assert!(install_command("ssh-ed25519 AAAA x'y").is_err());
        assert!(install_command("").is_err());
    }

    #[test]
    fn install_output_is_parsed() {
        assert_eq!(
            parse_install_output("ZSSH_KEY_ADDED\n").unwrap(),
            InstallOutcome::Added
        );
        assert_eq!(
            parse_install_output("ZSSH_KEY_EXISTS\n").unwrap(),
            InstallOutcome::AlreadyPresent
        );
        assert!(parse_install_output("").is_err());
    }
}
