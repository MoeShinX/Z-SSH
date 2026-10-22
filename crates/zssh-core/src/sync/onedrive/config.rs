//! OneDrive 的连接设置和 client ID 的来源。
//!
//! client ID（Azure 里“应用程序(客户端) ID”）是公共客户端的标识，**不是秘密**，但不写进代码：
//! - 发布版：构建时从环境变量 `ZSSH_ONEDRIVE_CLIENT_ID` 读取，编进程序（`src-tauri` 启动时
//!   调用 [`set_builtin_client_id`] 传进来）；
//! - 自己从源码编译的版本没有内置的：同步设置“高级”里可以填自己的（存在本机 [`OneDriveConfig`]
//!   里，随 `sync_storage` 一起 DPAPI 加密，**不同步**）。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// 一个连接上的 OneDrive 账号：非秘密的部分。令牌另存（[`super::tokens`]）。
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OneDriveConfig {
    /// 用户在“高级”里填的 client ID；空 = 用编进程序里的那个。
    #[serde(default)]
    pub client_id: String,
    /// 登录时记下的账号显示名（界面显示用）。
    #[serde(default)]
    pub account: String,
}

impl std::fmt::Debug for OneDriveConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 账号名是用户的个人信息，不进日志。
        f.debug_struct("OneDriveConfig")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// Azure 的应用程序(客户端) ID 是 GUID：`8-4-4-4-12` 个十六进制字符。
pub fn is_valid_client_id(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

static BUILTIN_CLIENT_ID: OnceLock<String> = OnceLock::new();

/// 程序启动时把构建时编进来的 client ID 交给这里。空的、格式不对的当作没有（不 panic、不使用）。
pub fn set_builtin_client_id(raw: Option<&str>) {
    if let Some(id) = raw.map(str::trim).filter(|id| is_valid_client_id(id)) {
        let _ = BUILTIN_CLIENT_ID.set(id.to_owned());
    }
}

/// 编进程序的 client ID（自己编译的版本没有）。
pub fn builtin_client_id() -> Option<&'static str> {
    BUILTIN_CLIENT_ID.get().map(String::as_str)
}

impl OneDriveConfig {
    /// 实际用的 client ID：用户填的优先，其次编进程序的。
    pub fn effective_client_id(&self) -> Option<String> {
        let custom = self.client_id.trim();
        if is_valid_client_id(custom) {
            return Some(custom.to_owned());
        }
        builtin_client_id().map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn client_ids_must_look_like_azure_application_ids() {
        assert!(is_valid_client_id(ID));
        assert!(is_valid_client_id("ABCDEF01-2345-6789-abcd-ef0123456789"));
        for bad in [
            "",
            "not-a-guid",
            "11111111222233334444555555555555",
            "11111111-2222-3333-4444-55555555555",
            "11111111-2222-3333-4444-5555555555555",
            "1111111g-2222-3333-4444-555555555555",
            " 11111111-2222-3333-4444-555555555555",
            "11111111-2222-3333-4444-555555555555\n",
        ] {
            assert!(!is_valid_client_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_custom_client_id_is_preferred_and_an_invalid_one_is_ignored() {
        let custom = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let config = OneDriveConfig {
            client_id: format!("  {custom} "),
            account: String::new(),
        };
        assert_eq!(config.effective_client_id().as_deref(), Some(custom));
        // 填错了的当作没填：回退到编进程序的（这个测试进程里没有设置，所以是 None）。
        let bad = OneDriveConfig {
            client_id: "oops".to_owned(),
            account: String::new(),
        };
        assert_eq!(
            bad.effective_client_id(),
            builtin_client_id().map(str::to_owned)
        );
    }

    #[test]
    fn invalid_builtin_ids_are_never_used() {
        // OnceLock 只能设一次，这里只验证无效值不会被接受（不影响其他用例）。
        set_builtin_client_id(Some("  "));
        set_builtin_client_id(Some("garbage"));
        set_builtin_client_id(None);
        assert_eq!(builtin_client_id(), None);
    }

    #[test]
    fn debug_output_has_no_account_name() {
        let config = OneDriveConfig {
            client_id: ID.to_owned(),
            account: "Some Person".to_owned(),
        };
        let text = format!("{config:?}");
        assert!(!text.contains("Some Person"), "{text}");
    }

    #[test]
    fn old_saved_settings_without_new_fields_still_load() {
        let config: OneDriveConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, OneDriveConfig::default());
    }
}
