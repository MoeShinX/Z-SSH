use serde::{Deserialize, Serialize};

use crate::error::ZsshError;

/// SSH 默认端口。
pub const DEFAULT_SSH_PORT: u16 = 22;

/// `Debug` 里代替密码 / 口令的占位（R-8 第 11 节：带 secret 的请求类型不能把明文打进日志）。
const SECRET_PLACEHOLDER: &str = "***";

/// 快速连接串解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTarget {
    pub username: String,
    pub host: String,
    pub port: u16,
}

impl QuickTarget {
    /// 转成 `user@host:port` 形式，用于日志和界面显示。
    pub fn display(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{}@{}:{}", self.username, host, self.port)
    }
}

/// 解析 `user@host`、`user@host:port`、`user@[::1]:2222`。
///
/// 用户名是必须的：快速连接入口只接受带用户名的写法，裸主机名留给主机库。
/// IPv6 必须加方括号，否则 `::` 里的冒号和端口分隔符无法区分。
pub fn parse_quick_target(s: &str) -> Result<QuickTarget, ZsshError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(ZsshError::QuickTargetMissingUser);
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(ZsshError::QuickTargetFormat(
            "地址中不能包含空白字符".to_owned(),
        ));
    }

    let (user, rest) = match trimmed.split_once('@') {
        Some(("", _)) | None => return Err(ZsshError::QuickTargetMissingUser),
        Some((user, rest)) => (user, rest),
    };
    if user.is_empty() {
        return Err(ZsshError::QuickTargetMissingUser);
    }

    let (host, port) = split_host_port(rest)?;
    if host.is_empty() {
        return Err(ZsshError::QuickTargetEmptyHost);
    }

    Ok(QuickTarget {
        username: user.to_owned(),
        host,
        port: match port {
            Some(raw) => parse_port(&raw)?,
            None => DEFAULT_SSH_PORT,
        },
    })
}

/// 拆出 host 和可选的端口原文，端口合法性交给 [`parse_port`]，这样错误信息
/// 能区分"没写端口"和"端口写错"。
fn split_host_port(rest: &str) -> Result<(String, Option<String>), ZsshError> {
    if let Some(stripped) = rest.strip_prefix('[') {
        // [::1] 或 [::1]:2222
        let (inner, after) = stripped
            .split_once(']')
            .ok_or_else(|| ZsshError::QuickTargetFormat("IPv6 地址缺少右方括号 ']'".to_owned()))?;
        let port = match after {
            "" => None,
            _ => {
                let port = after.strip_prefix(':').ok_or_else(|| {
                    ZsshError::QuickTargetFormat(format!("']' 之后只能是端口号，收到 '{after}'"))
                })?;
                Some(port.to_owned())
            }
        };
        return Ok((inner.to_owned(), port));
    }

    // 不加方括号时冒号只能有一个，两个及以上说明是没加括号的 IPv6。
    match rest.split_once(':') {
        None => Ok((rest.to_owned(), None)),
        Some(_) if rest.matches(':').count() > 1 => Err(ZsshError::QuickTargetFormat(
            "IPv6 地址必须加方括号，例如 root@[::1]:2222".to_owned(),
        )),
        Some((host, port)) => Ok((host.to_owned(), Some(port.to_owned()))),
    }
}

fn parse_port(raw: &str) -> Result<u16, ZsshError> {
    if raw.is_empty() {
        return Err(ZsshError::QuickTargetBadPort(String::new()));
    }
    match raw.parse::<u32>() {
        Ok(port) if (1..=65535).contains(&port) => Ok(port as u16),
        _ => Err(ZsshError::QuickTargetBadPort(raw.to_owned())),
    }
}

/// 主机的认证方式。M1-03 只有 `password` 真能连，`key` / `agent` 先把字段存下来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Password,
    Key,
    Agent,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Key => "key",
            Self::Agent => "agent",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ZsshError> {
        match raw {
            "password" => Ok(Self::Password),
            "key" => Ok(Self::Key),
            "agent" => Ok(Self::Agent),
            other => Err(ZsshError::InvalidInput(format!("未知的认证方式：{other}"))),
        }
    }
}

/// 凭据类型。私钥和 agent 的实际使用在 M1-04。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Password,
    KeyFile,
    KeyInline,
    Agent,
}

impl IdentityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::KeyFile => "key_file",
            Self::KeyInline => "key_inline",
            Self::Agent => "agent",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ZsshError> {
        match raw {
            "password" => Ok(Self::Password),
            "key_file" => Ok(Self::KeyFile),
            "key_inline" => Ok(Self::KeyInline),
            "agent" => Ok(Self::Agent),
            other => Err(ZsshError::InvalidInput(format!("未知的凭据类型：{other}"))),
        }
    }

    /// 这种凭据要不要存密文。
    pub fn keeps_secret(self) -> bool {
        !matches!(self, Self::Agent)
    }
}

/// 主机用不用代理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// 跟随全局的"默认代理"（旧数据迁成这个）
    Default,
    /// 明确不用代理
    None,
    /// 用 hosts.proxy_id 指定的那个
    Custom,
}

impl ProxyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::Custom => "custom",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ZsshError> {
        match raw {
            "default" => Ok(Self::Default),
            "none" => Ok(Self::None),
            "custom" => Ok(Self::Custom),
            other => Err(ZsshError::InvalidInput(format!("未知的代理策略：{other}"))),
        }
    }
}

/// 代理类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    Socks5,
    Http,
}

impl ProxyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Socks5 => "socks5",
            Self::Http => "http",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ZsshError> {
        match raw {
            "socks5" => Ok(Self::Socks5),
            "http" => Ok(Self::Http),
            other => Err(ZsshError::InvalidInput(format!("未知的代理类型：{other}"))),
        }
    }
}

/// 分组的新建 / 修改请求。`id` 为 `None` 表示新建。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDraft {
    pub id: Option<i64>,
    pub name: String,
    pub parent_id: Option<i64>,
    pub sort: i64,
}

/// 分组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub sort: i64,
}

/// 主机。**不含任何密码**：密码在它引用的凭据里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub id: i64,
    pub group_id: Option<i64>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    /// 认证用的凭据（M1-03 只支持密码凭据）
    pub identity_id: Option<i64>,
    pub proxy_id: Option<i64>,
    pub jump_host_id: Option<i64>,
    /// 代理策略：跟随默认 / 不用 / 指定
    pub proxy_mode: ProxyMode,
    pub tags: Vec<String>,
    pub sort: i64,
    /// 老服务器兼容开关（设计文档第 12 节）
    pub legacy_algorithms: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 主机的新建 / 修改请求。`id` 为 `None` 表示新建。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDraft {
    pub id: Option<i64>,
    pub group_id: Option<i64>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub identity_id: Option<i64>,
    pub proxy_id: Option<i64>,
    pub jump_host_id: Option<i64>,
    /// 代理策略：跟随默认 / 不用 / 指定（指定时用 proxy_id）
    pub proxy_mode: ProxyMode,
    pub tags: Vec<String>,
    pub sort: i64,
    /// 打开之后允许 SHA1 的 ssh-rsa、老的 KEX 和 CBC 加密（只给老服务器用）
    pub legacy_algorithms: bool,
}

/// 凭据。**不返回密文**，只告诉界面有没有存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: i64,
    pub name: String,
    pub kind: IdentityKind,
    pub key_path: Option<String>,
    pub has_secret: bool,
}

/// 凭据的新建 / 修改请求。
///
/// `secret` 为 `None` 表示"不修改已存的密文"；界面留空就是这么传的。
/// 新建时不带 secret 也能建（比如 agent 凭据，或者先建了再补密码）。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityDraft {
    pub id: Option<i64>,
    pub name: String,
    pub kind: IdentityKind,
    pub key_path: Option<String>,
    /// 密码 / 口令；`key_inline` 时这里是私钥内容。`None` = 不修改已存的
    pub secret: Option<String>,
    /// 只有 `key_inline` 用：和私钥内容一起存进口令（私钥本身有口令时要填）
    pub passphrase: Option<String>,
}

/// `Debug` 手写：`secret` / `passphrase` 是明文密码，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for IdentityDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityDraft")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("key_path", &self.key_path)
            .field("secret", &self.secret.as_ref().map(|_| SECRET_PLACEHOLDER))
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| SECRET_PLACEHOLDER),
            )
            .finish()
    }
}

/// 代理。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proxy {
    pub id: i64,
    pub name: String,
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub has_secret: bool,
}

/// 代理的新建 / 修改请求。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyDraft {
    pub id: Option<i64>,
    pub name: String,
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub secret: Option<String>,
}

/// `Debug` 手写：`secret` 是明文代理密码，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for ProxyDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyDraft")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("secret", &self.secret.as_ref().map(|_| SECRET_PLACEHOLDER))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> QuickTarget {
        parse_quick_target(s).expect("应该解析成功")
    }

    fn err(s: &str) -> ZsshError {
        parse_quick_target(s).expect_err("应该解析失败")
    }

    #[test]
    fn user_and_host_uses_default_port() {
        assert_eq!(
            ok("root@example.com"),
            QuickTarget {
                username: "root".to_owned(),
                host: "example.com".to_owned(),
                port: 22,
            }
        );
    }

    #[test]
    fn user_host_and_port() {
        assert_eq!(
            ok("deploy@example.com:2222"),
            QuickTarget {
                username: "deploy".to_owned(),
                host: "example.com".to_owned(),
                port: 2222,
            }
        );
    }

    #[test]
    fn ipv4_host_and_port() {
        assert_eq!(
            ok("admin@192.168.1.10:22"),
            QuickTarget {
                username: "admin".to_owned(),
                host: "192.168.1.10".to_owned(),
                port: 22,
            }
        );
    }

    #[test]
    fn bracketed_ipv6_with_port() {
        assert_eq!(
            ok("root@[::1]:2222"),
            QuickTarget {
                username: "root".to_owned(),
                host: "::1".to_owned(),
                port: 2222,
            }
        );
    }

    #[test]
    fn bracketed_ipv6_without_port() {
        assert_eq!(
            ok("root@[2001:db8::1]"),
            QuickTarget {
                username: "root".to_owned(),
                host: "2001:db8::1".to_owned(),
                port: 22,
            }
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(ok("  root@example.com:2200  ").port, 2200);
    }

    #[test]
    fn missing_username_is_rejected() {
        assert!(matches!(
            err("example.com"),
            ZsshError::QuickTargetMissingUser
        ));
    }

    #[test]
    fn empty_username_is_rejected() {
        assert!(matches!(
            err("@example.com"),
            ZsshError::QuickTargetMissingUser
        ));
    }

    #[test]
    fn empty_host_is_rejected() {
        assert!(matches!(err("root@"), ZsshError::QuickTargetEmptyHost));
    }

    #[test]
    fn bracketed_empty_host_is_rejected() {
        assert!(matches!(err("root@[]:22"), ZsshError::QuickTargetEmptyHost));
    }

    #[test]
    fn port_zero_is_rejected() {
        assert!(matches!(
            err("root@example.com:0"),
            ZsshError::QuickTargetBadPort(p) if p == "0"
        ));
    }

    #[test]
    fn port_above_range_is_rejected() {
        assert!(matches!(
            err("root@example.com:65536"),
            ZsshError::QuickTargetBadPort(p) if p == "65536"
        ));
    }

    #[test]
    fn non_numeric_port_is_rejected() {
        assert!(matches!(
            err("root@example.com:ssh"),
            ZsshError::QuickTargetBadPort(_)
        ));
    }

    #[test]
    fn empty_port_is_rejected() {
        assert!(matches!(
            err("root@example.com:"),
            ZsshError::QuickTargetBadPort(_)
        ));
    }

    #[test]
    fn unbracketed_ipv6_is_rejected() {
        assert!(matches!(err("root@::1"), ZsshError::QuickTargetFormat(_)));
    }

    #[test]
    fn unclosed_bracket_is_rejected() {
        assert!(matches!(
            err("root@[::1:2222"),
            ZsshError::QuickTargetFormat(_)
        ));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(matches!(err("   "), ZsshError::QuickTargetMissingUser));
    }

    #[test]
    fn identity_draft_debug_does_not_contain_the_password_or_passphrase() {
        // `IdentityDraft` 带明文密码 / 口令：Debug 输出一旦进了日志就是泄露（R-8 第 11 节）。
        let draft = IdentityDraft {
            id: Some(1),
            name: "prod".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("s3cret-password".to_owned()),
            passphrase: Some("s3cret-passphrase".to_owned()),
        };
        let debug = format!("{draft:?}");
        assert!(
            !debug.contains("s3cret"),
            "IdentityDraft 的 Debug 不能带密码 / 口令：{debug}"
        );
        assert!(debug.contains("***"), "密码位置要显示成 ***：{debug}");
        assert!(debug.contains("prod"), "其它字段照常显示：{debug}");

        // 没有密码时不要凭空显示 ***。
        let draft = IdentityDraft {
            secret: None,
            passphrase: None,
            ..draft
        };
        let debug = format!("{draft:?}");
        assert!(debug.contains("None"), "{debug}");
        assert!(!debug.contains("***"), "{debug}");
    }

    #[test]
    fn proxy_draft_debug_does_not_contain_the_password() {
        let draft = ProxyDraft {
            id: None,
            name: "corp".to_owned(),
            kind: ProxyKind::Socks5,
            host: "proxy.example.com".to_owned(),
            port: 1080,
            username: Some("deploy".to_owned()),
            secret: Some("s3cret-proxy-password".to_owned()),
        };
        let debug = format!("{draft:?}");
        assert!(
            !debug.contains("s3cret"),
            "ProxyDraft 的 Debug 不能带代理密码：{debug}"
        );
        assert!(debug.contains("***"), "{debug}");
        assert!(debug.contains("proxy.example.com"), "{debug}");
    }
}
