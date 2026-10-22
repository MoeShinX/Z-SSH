//! 本机敏感信息的封装（生产用 Windows DPAPI）。
//!
//! 同步包里放的是明文（整个包由主密码加密）；写回本地库时要换成**这台电脑**
//! 的 DPAPI 密文。测试用另一个实现模拟“另一台电脑的 DPAPI”。

use super::error::SyncError;

pub trait SecretSealer: Send + Sync {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError>;
    fn open(&self, blob: &[u8]) -> Result<Vec<u8>, SyncError>;
}

/// Windows DPAPI（绑定当前用户）。
pub struct DpapiSealer;

impl SecretSealer for DpapiSealer {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError> {
        crate::store::secrets::protect(plaintext).map_err(SyncError::from)
    }

    fn open(&self, blob: &[u8]) -> Result<Vec<u8>, SyncError> {
        crate::store::secrets::unprotect(blob).map_err(SyncError::from)
    }
}

/// 测试用的“另一台电脑的 DPAPI”：带前缀的可逆混淆。
#[cfg(test)]
pub(crate) struct TestSealer(pub u8);

#[cfg(test)]
impl SecretSealer for TestSealer {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError> {
        let mut out = vec![b'T', self.0];
        out.extend(plaintext.iter().map(|byte| byte ^ self.0));
        Ok(out)
    }

    fn open(&self, blob: &[u8]) -> Result<Vec<u8>, SyncError> {
        if blob.len() < 2 || blob[0] != b'T' || blob[1] != self.0 {
            return Err(SyncError::InvalidData("不是 TestSealer 的密文".to_owned()));
        }
        Ok(blob[2..].iter().map(|byte| byte ^ self.0).collect())
    }
}
