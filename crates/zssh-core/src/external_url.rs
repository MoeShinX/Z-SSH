//! 终端里点开的网址：校验后交给系统默认浏览器。
//!
//! 和 OneDrive 登录用的白名单不同，这里要放行任意 `http://` / `https://` 网址（终端输出里
//! 什么站点都可能出现）。所以只靠协议限制：**不能把任意字符串交给系统去“打开”**
//! （`file:`、`javascript:`、`ms-settings:`、可执行文件路径都会被 ShellExecute 当真）。
//! 解析成 URL 之后用规范化的结果去打开，而不是原字符串。

use reqwest::Url;

use crate::sync::onedrive::browser::shell_open;

/// 网址再长也不会超过这个数（`ShellExecute` 对超长路径本来就不可靠）。
const MAX_URL_LEN: usize = 2000;

/// 校验通过就返回规范化后的网址。
pub fn validate_external_url(raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_URL_LEN {
        return Err("网址太长，没有打开".to_owned());
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("网址里有空白或控制字符，没有打开".to_owned());
    }
    let url = Url::parse(raw).map_err(|_| "网址不合法，没有打开".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("只允许打开 http:// 或 https:// 网址".to_owned());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("网址里没有主机名，没有打开".to_owned());
    }
    Ok(url)
}

/// 用系统默认浏览器打开（先过 [`validate_external_url`]）。非 Windows 返回“不支持”。
pub fn open_external_url(raw: &str) -> Result<(), String> {
    let url = validate_external_url(raw)?;
    shell_open(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_and_https_are_allowed_and_normalised() {
        let url = validate_external_url("https://example.com/a?b=1#c").expect("https");
        assert_eq!(url.as_str(), "https://example.com/a?b=1#c");
        let url = validate_external_url("http://10.0.0.1:8080/x").expect("http 带端口");
        assert_eq!(url.port(), Some(8080));
        // 大写协议和主机会被规范化，打开的是规范化后的地址。
        let url = validate_external_url("HTTPS://Example.COM").expect("大写");
        assert_eq!(url.as_str(), "https://example.com/");
    }

    #[test]
    fn other_schemes_and_paths_are_refused() {
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:network",
            "ftp://example.com/x",
            "mailto:a@example.com",
            r"C:\Windows\System32\calc.exe",
            "example.com/x",
            "//example.com/x",
            "http://",
            "",
        ] {
            assert!(validate_external_url(bad).is_err(), "{bad} 应该被拒绝");
        }
    }

    #[test]
    fn whitespace_control_characters_and_huge_urls_are_refused() {
        assert!(validate_external_url("https://example.com/a b").is_err());
        assert!(validate_external_url("https://example.com/a\r\nb").is_err());
        assert!(validate_external_url("https://example.com/a\0b").is_err());
        let long = format!("https://example.com/{}", "a".repeat(2100));
        assert!(validate_external_url(&long).is_err());
    }

    #[test]
    fn open_validates_before_touching_the_system() {
        // 被拒绝的网址在到达系统调用之前就返回（这个测试不会真的打开任何东西）。
        assert!(open_external_url("file:///C:/x.exe").is_err());
        assert!(open_external_url("https://example.com/a b").is_err());
    }
}
