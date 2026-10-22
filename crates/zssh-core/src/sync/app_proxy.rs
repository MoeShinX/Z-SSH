//! 软件自身联网（检查 / 下载更新、配置同步、一键下载 opencode）用哪个代理。
//!
//! 和 SSH 连接的"默认代理"分开：SSH 连的是用户的服务器，软件联网连的是 GitHub、
//! 同步服务这类公网地址，两者需要的代理常常不一样。设置只在本机，不同步。

use serde::Serialize;

use super::error::SyncError;
use super::net::{ProxyScheme, SyncProxy};
use super::service::default_proxy;
use crate::model::ProxyKind;
use crate::store::Database;

/// 设置键。值是 JSON 字符串：`"ssh_default"`、`"system"`、`"none"` 或 `"proxy:<id>"`。
pub const APP_NETWORK_PROXY_KEY: &str = "app_network_proxy";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppProxyMode {
    /// 跟 SSH 的默认代理走（升级上来的默认行为：很多人靠它访问 GitHub）。
    SshDefault,
    /// 读 Windows 的系统代理。
    System,
    /// 直连，也不读系统代理和环境变量。
    Direct,
    /// 代理库里的某一个。
    Proxy(i64),
}

impl AppProxyMode {
    /// 设置里的值 → 模式。没设置、读不懂都按 `SshDefault`，保持升级前的行为。
    pub fn parse(raw: Option<&str>) -> Self {
        let text = raw
            .and_then(|raw| serde_json::from_str::<String>(raw).ok())
            .unwrap_or_default();
        match text.as_str() {
            "system" => Self::System,
            "none" => Self::Direct,
            other => other
                .strip_prefix("proxy:")
                .and_then(|id| id.parse().ok())
                .map(Self::Proxy)
                .unwrap_or(Self::SshDefault),
        }
    }
}

/// 读到的 Windows 系统代理。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SystemProxy {
    /// 系统没开代理。
    None,
    /// 手动设置的代理服务器。
    Manual {
        scheme: String,
        host: String,
        port: u16,
    },
    /// 用的是自动配置脚本（PAC），暂不支持，按直连处理。
    AutoConfig { url: String },
}

/// 解析 Internet 选项里的代理设置。`server` 可能是 `host:port`，也可能是分协议写法
/// `http=h:p;https=h:p;socks=h:p`（https 优先：软件联网都是 https）。
pub fn parse_system_proxy(
    enabled: bool,
    server: Option<&str>,
    auto_config: Option<&str>,
) -> SystemProxy {
    let auto_config = auto_config.map(str::trim).filter(|url| !url.is_empty());
    let server = server.map(str::trim).filter(|server| !server.is_empty());
    if enabled {
        if let Some(found) = server.and_then(pick_manual) {
            return found;
        }
    }
    match auto_config {
        Some(url) => SystemProxy::AutoConfig {
            url: url.to_owned(),
        },
        None => SystemProxy::None,
    }
}

fn pick_manual(server: &str) -> Option<SystemProxy> {
    if !server.contains('=') {
        return manual("http", server);
    }
    let entry = |name: &str| {
        server.split(';').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    };
    entry("https")
        .and_then(|value| manual("http", value))
        .or_else(|| entry("http").and_then(|value| manual("http", value)))
        .or_else(|| entry("socks").and_then(|value| manual("socks5", value)))
}

fn manual(scheme: &str, address: &str) -> Option<SystemProxy> {
    // 有的写法带 scheme（"http://127.0.0.1:7890"），去掉再拆。
    let address = address
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(address)
        .trim_end_matches('/');
    let (host, port) = address.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port: u16 = port.parse().ok()?;
    if host.is_empty() || port == 0 || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(SystemProxy::Manual {
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        port,
    })
}

/// 读当前用户的系统代理（HKCU 的 Internet Settings）。读不到按"没开代理"。
pub fn read_system_proxy() -> SystemProxy {
    #[cfg(windows)]
    {
        let enabled = registry::read_dword("ProxyEnable").unwrap_or(0) != 0;
        let server = registry::read_string("ProxyServer");
        let auto_config = registry::read_string("AutoConfigURL");
        parse_system_proxy(enabled, server.as_deref(), auto_config.as_deref())
    }
    #[cfg(not(windows))]
    {
        SystemProxy::None
    }
}

/// 软件自身联网要用的代理；`None` 表示直连（不读系统代理和环境变量）。
pub fn app_proxy(db: &Database) -> Result<Option<SyncProxy>, SyncError> {
    let raw = db
        .setting_get(APP_NETWORK_PROXY_KEY)
        .map_err(SyncError::from)?;
    match AppProxyMode::parse(raw.as_deref()) {
        AppProxyMode::SshDefault => default_proxy(db),
        AppProxyMode::Direct => Ok(None),
        AppProxyMode::Proxy(id) => {
            let login = db.proxy_login(id).map_err(SyncError::from)?;
            Ok(Some(SyncProxy {
                kind: match login.kind {
                    ProxyKind::Socks5 => ProxyScheme::Socks5,
                    ProxyKind::Http => ProxyScheme::Http,
                },
                host: login.host,
                port: login.port,
                username: login.username,
                password: login.password,
            }))
        }
        AppProxyMode::System => Ok(match read_system_proxy() {
            SystemProxy::Manual { scheme, host, port } => Some(SyncProxy {
                kind: if scheme == "socks5" {
                    ProxyScheme::Socks5
                } else {
                    ProxyScheme::Http
                },
                host,
                port,
                username: None,
                password: None,
            }),
            SystemProxy::AutoConfig { .. } => {
                tracing::warn!("系统代理用的是自动配置脚本（PAC），暂不支持，按直连处理");
                None
            }
            SystemProxy::None => None,
        }),
    }
}

#[cfg(windows)]
mod registry {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    pub fn read_dword(name: &str) -> Option<u32> {
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY：输出缓冲就是一个 u32，大小如实传入。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(KEY),
                &HSTRING::from(name),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut value as *mut u32).cast()),
                Some(&mut size),
            )
        };
        status.is_ok().then_some(value)
    }

    pub fn read_string(name: &str) -> Option<String> {
        let key = HSTRING::from(KEY);
        let name = HSTRING::from(name);
        let mut size = 0u32;
        // SAFETY：第一次只取长度，不给缓冲。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        };
        if status.is_err() || size < 2 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        // SAFETY：缓冲按上一步返回的字节数分配，大小如实传入。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_and_falls_back_to_the_ssh_default() {
        assert_eq!(AppProxyMode::parse(None), AppProxyMode::SshDefault);
        assert_eq!(
            AppProxyMode::parse(Some("\"ssh_default\"")),
            AppProxyMode::SshDefault
        );
        assert_eq!(
            AppProxyMode::parse(Some("\"system\"")),
            AppProxyMode::System
        );
        assert_eq!(AppProxyMode::parse(Some("\"none\"")), AppProxyMode::Direct);
        assert_eq!(
            AppProxyMode::parse(Some("\"proxy:7\"")),
            AppProxyMode::Proxy(7)
        );
        assert_eq!(
            AppProxyMode::parse(Some("\"proxy:x\"")),
            AppProxyMode::SshDefault
        );
        assert_eq!(
            AppProxyMode::parse(Some("garbage")),
            AppProxyMode::SshDefault
        );
    }

    fn manual(scheme: &str, host: &str, port: u16) -> SystemProxy {
        SystemProxy::Manual {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn system_proxy_strings_in_every_windows_form() {
        assert_eq!(
            parse_system_proxy(true, Some("127.0.0.1:7890"), None),
            manual("http", "127.0.0.1", 7890)
        );
        assert_eq!(
            parse_system_proxy(
                true,
                Some("http=10.0.0.1:8080;https=10.0.0.2:8443;socks=10.0.0.3:1080"),
                None
            ),
            manual("http", "10.0.0.2", 8443),
            "分协议写法优先用 https 那一项"
        );
        assert_eq!(
            parse_system_proxy(true, Some("socks=127.0.0.1:1080"), None),
            manual("socks5", "127.0.0.1", 1080)
        );
        assert_eq!(
            parse_system_proxy(true, Some("http://127.0.0.1:7890/"), None),
            manual("http", "127.0.0.1", 7890)
        );
        assert_eq!(
            parse_system_proxy(true, Some("[::1]:7890"), None),
            manual("http", "::1", 7890)
        );
        assert_eq!(
            parse_system_proxy(false, Some("127.0.0.1:7890"), None),
            SystemProxy::None,
            "没勾选就不用"
        );
        assert_eq!(
            parse_system_proxy(true, Some("bad"), None),
            SystemProxy::None
        );
        assert_eq!(
            parse_system_proxy(false, None, Some("http://wpad/proxy.pac")),
            SystemProxy::AutoConfig {
                url: "http://wpad/proxy.pac".to_owned()
            }
        );
        assert_eq!(
            parse_system_proxy(true, Some("127.0.0.1:7890"), Some("http://wpad/proxy.pac")),
            manual("http", "127.0.0.1", 7890),
            "手动代理和 PAC 都有时用手动的"
        );
    }

    #[test]
    fn app_proxy_follows_the_setting_and_is_separate_from_the_ssh_default() {
        use crate::model::{ProxyDraft, ProxyKind};
        let db = Database::open_in_memory().unwrap();
        let draft = |name: &str, port: u16| ProxyDraft {
            id: None,
            name: name.to_owned(),
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            secret: None,
        };
        let ssh = db.proxy_save(&draft("ssh", 1080)).unwrap();
        let app = db.proxy_save(&draft("app", 7890)).unwrap();
        db.set_default_proxy_id(Some(ssh.id)).unwrap();

        let port = |db: &Database| app_proxy(db).unwrap().map(|proxy| proxy.port);
        assert_eq!(
            port(&db),
            Some(1080),
            "没设置时和 SSH 默认代理一样（升级前的行为）"
        );
        db.setting_set(APP_NETWORK_PROXY_KEY, &format!("\"proxy:{}\"", app.id))
            .unwrap();
        assert_eq!(port(&db), Some(7890));
        db.setting_set(APP_NETWORK_PROXY_KEY, "\"none\"").unwrap();
        assert_eq!(port(&db), None, "不使用代理就是直连，不管 SSH 默认代理");
        assert_eq!(
            db.default_proxy_id().unwrap(),
            Some(ssh.id),
            "SSH 的默认代理不受影响"
        );
    }
}
