//! 用系统默认浏览器打开网址，以及打开之前的网址白名单。
//!
//! 授权地址、撤销授权的页面都要交给系统浏览器。**不能把任意字符串交给系统去“打开”**
//! （`file:`、`javascript:`、可执行文件路径都会被 ShellExecute 当真），所以统一经过
//! [`validate_open_url`]：只放行 `https://` 加白名单里的主机。
//!
//! 不用 Tauri 的 opener 插件：这里直接在 Rust 里调 `ShellExecuteW`，前端没有任何
//! “打开网址”的权限（capability 不用新增），能打开什么完全由这个白名单决定。

use reqwest::Url;

/// 允许打开的主机：微软登录页，以及个人账号的“应用和服务”授权管理页。
pub const ALLOWED_HOSTS: &[&str] = &["login.microsoftonline.com", "account.live.com"];

/// 个人账号撤销授权的页面（Microsoft 帐户 → 隐私 → 应用和服务）。
pub const REVOKE_PAGE_URL: &str = "https://account.live.com/consent/Manage";

/// 网址再长也不会超过这个数（`ShellExecute` 对超长路径本来就不可靠）。
const MAX_URL_LEN: usize = 2000;

/// 校验通过就返回规范化后的网址（用它去打开，而不是原字符串）。
pub fn validate_open_url(raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_URL_LEN {
        return Err("网址太长，没有打开".to_owned());
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("网址里有空白或控制字符，没有打开".to_owned());
    }
    let url = Url::parse(raw).map_err(|_| "网址不合法，没有打开".to_owned())?;
    if url.scheme() != "https" {
        return Err("只允许打开 https:// 网址".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("网址里不能带账号密码".to_owned());
    }
    // `Url::port()` 对默认端口（https 的 443）返回 None，写了别的端口一律拒绝。
    if url.port().is_some() {
        return Err("网址里不能指定端口".to_owned());
    }
    let host = url.host_str().unwrap_or_default();
    if !ALLOWED_HOSTS.contains(&host) {
        return Err("这个域名不在允许打开的名单里".to_owned());
    }
    Ok(url)
}

/// 打开网址的方式。生产用 [`SystemBrowser`]；测试里换成记录网址的假浏览器。
pub trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> Result<(), String>;
}

/// 系统默认浏览器（先过 [`validate_open_url`]）。
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        let url = validate_open_url(url)?;
        shell_open(url.as_str())
    }
}

#[cfg(windows)]
fn shell_open(url: &str) -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` 以 0 结尾并且在调用期间一直存活；其余参数是空指针 / 常量字符串。
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // 文档：返回值大于 32 表示成功，否则是错误码。
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("系统没能打开默认浏览器（错误码 {code}）"))
    }
}

#[cfg(not(windows))]
fn shell_open(_url: &str) -> Result<(), String> {
    Err("只支持在 Windows 上打开系统浏览器".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_login_and_revoke_pages_are_allowed() {
        let url = validate_open_url(
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?client_id=x&state=y",
        )
        .expect("登录页");
        assert!(url
            .as_str()
            .starts_with("https://login.microsoftonline.com/"));
        let url = validate_open_url(REVOKE_PAGE_URL).expect("撤销授权页");
        assert_eq!(url.host_str(), Some("account.live.com"));
        // 显式写默认端口也算同一个地址。
        validate_open_url("https://login.microsoftonline.com:443/x").expect("443");
    }

    #[test]
    fn non_https_schemes_are_refused() {
        for bad in [
            "http://login.microsoftonline.com/x",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:network",
            "C:\\Windows\\System32\\calc.exe",
            "login.microsoftonline.com/x",
            "",
        ] {
            assert!(validate_open_url(bad).is_err(), "{bad} 应该被拒绝");
        }
    }

    #[test]
    fn other_hosts_and_lookalikes_are_refused() {
        for bad in [
            "https://example.com/",
            "https://login.microsoftonline.com.evil.example/",
            "https://evil.example/login.microsoftonline.com/",
            "https://login.microsoftonline.com@evil.example/",
            "https://user:pw\x40login.microsoftonline.com/",
            "https://login.microsoftonline.com:8443/x",
            "https://sub.login.microsoftonline.com/",
            "https://microsoftonline.com/",
            "https://127.0.0.1/",
            "https://[::1]/",
        ] {
            assert!(validate_open_url(bad).is_err(), "{bad} 应该被拒绝");
        }
    }

    #[test]
    fn whitespace_control_characters_and_huge_urls_are_refused() {
        assert!(validate_open_url("https://login.microsoftonline.com/a b").is_err());
        assert!(validate_open_url("https://login.microsoftonline.com/a\r\nb").is_err());
        let long = format!("https://login.microsoftonline.com/{}", "a".repeat(2100));
        assert!(validate_open_url(&long).is_err());
    }

    #[test]
    fn system_browser_validates_before_opening() {
        // 不在名单里的网址在到达系统调用之前就被拒绝（这个测试不会真的打开任何东西）。
        let err = SystemBrowser.open("https://example.com/").unwrap_err();
        assert!(err.contains("名单"), "{err}");
        let err = SystemBrowser.open("file:///C:/x.exe").unwrap_err();
        assert!(err.contains("https"), "{err}");
    }
}
