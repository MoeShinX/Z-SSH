//! 敏感信息的加解密：Windows DPAPI（绑定当前用户）。
//!
//! 用 `CryptProtectData` 加一个固定的应用级 entropy，别的程序拿到密文也没法直接
//! 解出来。密文只进 `secret_blob`，绝不出现明文列。

use crate::error::ZsshError;

/// 应用级 entropy。换掉它旧密文就解不开了，所以不能随便改
/// （产品名改成了 `Z-SSH`，这里也保持原样——改这个等于换 entropy）。
#[cfg(windows)]
const ENTROPY: &[u8] = b"Z-ssh/v1/secret";

/// 加密一段敏感信息（密码、私钥口令、私钥内容）。
#[cfg(windows)]
pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, ZsshError> {
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: to_u32(plaintext.len())?,
        pbData: plaintext.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: to_u32(ENTROPY.len())?,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: 输入 blob 是本地变量，指针在整个调用期间有效；输出由 DPAPI 分配，
    // 下面用 LocalFree 释放。
    unsafe {
        CryptProtectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| ZsshError::Secret(format!("CryptProtectData 失败：{err}")))?;
    }

    Ok(unsafe { take_blob(output) })
}

/// 解密 [`protect`] 写出来的密文。
#[cfg(windows)]
pub fn unprotect(blob: &[u8]) -> Result<Vec<u8>, ZsshError> {
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: to_u32(blob.len())?,
        pbData: blob.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: to_u32(ENTROPY.len())?,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    // SAFETY: 同 protect。
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| ZsshError::Secret(format!("CryptUnprotectData 失败：{err}")))?;
    }

    Ok(unsafe { take_blob(output) })
}

/// 把 DPAPI 分配的输出 blob 拷成 `Vec`，然后释放。
///
/// # Safety
/// `blob` 必须是 DPAPI 刚分配出来、还没释放的输出。
#[cfg(windows)]
unsafe fn take_blob(blob: windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    use windows::Win32::Foundation::LocalFree;

    let bytes = if blob.pbData.is_null() {
        Vec::new()
    } else {
        // SAFETY: DPAPI 保证 pbData 指向 cbData 个字节。
        unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec()
    };
    if !blob.pbData.is_null() {
        // SAFETY: 这块内存是 DPAPI 用 LocalAlloc 分配的，只能交给 LocalFree。
        let _ = unsafe { LocalFree(Some(windows::Win32::Foundation::HLOCAL(blob.pbData.cast()))) };
    }
    bytes
}

#[cfg(windows)]
fn to_u32(len: usize) -> Result<u32, ZsshError> {
    u32::try_from(len).map_err(|_| ZsshError::Secret(format!("数据太长：{len} 字节")))
}

/// 非 Windows 平台上没有 DPAPI。本项目只做 Windows，这里让 crate 在别的平台上
/// 也能编译，真调用会明确报错。
#[cfg(not(windows))]
pub fn protect(_plaintext: &[u8]) -> Result<Vec<u8>, ZsshError> {
    Err(ZsshError::Secret(
        "这个平台没有 DPAPI，敏感信息无法加密".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, ZsshError> {
    Err(ZsshError::Secret(
        "这个平台没有 DPAPI，敏感信息无法解密".to_owned(),
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_restores_plaintext() {
        let plaintext = "一段带中文的密码 p@ssw0rd".as_bytes();
        let blob = protect(plaintext).expect("加密应该成功");

        assert_ne!(blob, plaintext, "密文不该等于明文");
        assert!(!blob.is_empty(), "密文不该是空的");
        assert_eq!(
            unprotect(&blob).expect("解密应该成功"),
            plaintext,
            "解密之后应该和原文一样"
        );
    }

    #[test]
    fn roundtrip_handles_empty_input() {
        let blob = protect(b"").expect("空数据也能加密");
        assert!(unprotect(&blob).expect("解密应该成功").is_empty());
    }

    #[test]
    fn tampered_blob_is_rejected() {
        let mut blob = protect(b"secret").expect("加密应该成功");
        // 改一个字节：DPAPI 会校验完整性，应该失败而不是解出垃圾。
        if let Some(byte) = blob.first_mut() {
            *byte ^= 0xff;
        }
        assert!(unprotect(&blob).is_err(), "改过的密文不该解得开");
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let plaintext = "a-very-distinctive-password-9f3a";
        let blob = protect(plaintext.as_bytes()).expect("加密应该成功");
        let haystack = String::from_utf8_lossy(&blob);
        assert!(!haystack.contains(plaintext), "密文里不该出现明文片段");
    }
}
