//! 右键「复制密码」「复制连接信息」用的材料：哪些主机能复制密码、连接信息的文字怎么拼。
//!
//! 明文密码只在后端出现：这里取出来交给调用方写进系统剪贴板，从不经过前端。
//! 取密码的规则和连接前的凭据解析（`host_login`）是同一套：密码登录、引用了密码类凭据，
//! 私钥类凭据的秘密（口令 / 整段私钥）任何情况下都不当密码用。

use zeroize::Zeroizing;

use crate::error::ZsshError;
use crate::model::{AuthMethod, Host, Identity, ProxyMode};
use crate::store::db::Database;

const NO_SAVED_PASSWORD: &str = "这台主机没有保存密码";

/// 按「复制密码」的规则取这台主机保存的密码。
///
/// `secret` 只在凭据类型合格时才会被调用（解密有代价，私钥类凭据的秘密更不该被碰），
/// 返回凭据里解出来的明文；`None` / 空串都按「没存」处理。
pub(crate) fn resolve_saved_password(
    host: &Host,
    identity: Option<&Identity>,
    secret: impl FnOnce() -> Result<Option<String>, ZsshError>,
) -> Result<Zeroizing<String>, ZsshError> {
    match host.auth_method {
        AuthMethod::Key => {
            return Err(ZsshError::InvalidInput(
                "这台主机用的是密钥登录，没有密码可复制".to_owned(),
            ))
        }
        AuthMethod::Agent => {
            return Err(ZsshError::InvalidInput(
                "这台主机用的是 SSH agent 登录，没有密码可复制".to_owned(),
            ))
        }
        AuthMethod::Password => {}
    }
    let identity = identity.ok_or_else(|| ZsshError::InvalidInput(NO_SAVED_PASSWORD.to_owned()))?;
    crate::store::hosts::check_identity_matches(AuthMethod::Password, identity)?;
    if !identity.has_secret {
        return Err(ZsshError::InvalidInput(NO_SAVED_PASSWORD.to_owned()));
    }
    match secret()? {
        Some(password) if !password.is_empty() => Ok(Zeroizing::new(password)),
        _ => Err(ZsshError::InvalidInput(NO_SAVED_PASSWORD.to_owned())),
    }
}

/// 连接信息里「认证」那部分。
pub enum InfoAuth {
    /// 密码登录。`None` = 没存密码
    Password(Option<Zeroizing<String>>),
    /// 私钥登录，只带凭据名（没选凭据时为 `None`），绝不带私钥内容
    Key {
        identity: Option<String>,
    },
    Agent,
}

/// 拼「复制连接信息」文字所需的全部内容。
pub struct ConnectionInfo {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub auth: InfoAuth,
    /// 跳板机的主机名
    pub jump_host: Option<String>,
    /// 代理名（只在主机明确指定了代理时有）
    pub proxy: Option<String>,
}

/// 手写：里面可能有明文密码，不能进日志。
impl std::fmt::Debug for ConnectionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionInfo")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("port", &self.port)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl ConnectionInfo {
    /// 文字里有没有明文密码（有的话写剪贴板后要自动清除）。
    pub fn has_password(&self) -> bool {
        matches!(&self.auth, InfoAuth::Password(Some(_)))
    }
}

/// 地址的展示写法：IPv6 去掉方括号（和界面「复制地址」一致），其余原样。
/// ssh 命令里 `user@2001:db8::1` 不需要方括号，所以同一个写法两处通用。
pub fn bare_address(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        let inner = inner.trim();
        if inner.contains(':') {
            return inner.to_owned();
        }
    }
    trimmed.to_owned()
}

/// 拼连接信息文字。纯函数，返回值带明文密码时用 `Zeroizing` 包着。
pub fn format_connection_info(info: &ConnectionInfo) -> Zeroizing<String> {
    let address = bare_address(&info.address);
    let mut text = Zeroizing::new(String::new());
    let mut line = |line: &str| {
        text.push_str(line);
        text.push('\n');
    };
    line(&format!("名称：{}", info.name));
    line(&format!("地址：{address}"));
    line(&format!("端口：{}", info.port));
    line(&format!("用户名：{}", info.username));
    match &info.auth {
        InfoAuth::Password(Some(password)) => line(&format!("密码：{}", password.as_str())),
        InfoAuth::Password(None) => line("密码：未保存"),
        InfoAuth::Key {
            identity: Some(name),
        } => line(&format!("认证：私钥（凭据「{name}」）")),
        InfoAuth::Key { identity: None } => line("认证：私钥"),
        InfoAuth::Agent => line("认证：SSH agent"),
    }
    if let Some(jump) = &info.jump_host {
        line(&format!("跳板机：{jump}"));
    }
    if let Some(proxy) = &info.proxy {
        line(&format!("代理：{proxy}"));
    }
    line(&format!("ssh -p {} {}@{address}", info.port, info.username));
    // 末尾不留换行：粘贴到输入框里不会多出一个回车
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

impl Database {
    /// 「复制密码」：取出这台主机保存的密码（规则见 [`resolve_saved_password`]）。
    /// 明文只给后端用，调用方写进剪贴板后尽快丢掉。
    pub fn host_saved_password(&self, id: i64) -> Result<Zeroizing<String>, ZsshError> {
        self.with_conn(|conn| {
            let host = crate::store::hosts::host_get(conn, id)?;
            let identity = match (host.auth_method, host.identity_id) {
                (AuthMethod::Password, Some(identity_id)) => {
                    Some(crate::store::identities::identity_get(conn, identity_id)?)
                }
                _ => None,
            };
            resolve_saved_password(&host, identity.as_ref(), || match &identity {
                Some(identity) => crate::store::identities::identity_secret(conn, identity.id),
                None => Ok(None),
            })
        })
    }

    /// 「复制连接信息」：收集这台主机的连接信息；能取到密码就带上。
    pub fn host_connection_info(&self, id: i64) -> Result<ConnectionInfo, ZsshError> {
        self.with_conn(|conn| {
            let host = crate::store::hosts::host_get(conn, id)?;
            let identity = match host.identity_id {
                Some(identity_id) => {
                    Some(crate::store::identities::identity_get(conn, identity_id)?)
                }
                None => None,
            };

            let auth = match host.auth_method {
                AuthMethod::Password => {
                    let password =
                        resolve_saved_password(&host, identity.as_ref(), || match &identity {
                            Some(identity) => {
                                crate::store::identities::identity_secret(conn, identity.id)
                            }
                            None => Ok(None),
                        });
                    match password {
                        Ok(password) => InfoAuth::Password(Some(password)),
                        // 没存 / 凭据类型对不上：都算没有可复制的密码；
                        // 解密失败这类真故障不能悄悄吞掉
                        Err(ZsshError::InvalidInput(_)) => InfoAuth::Password(None),
                        Err(other) => return Err(other),
                    }
                }
                AuthMethod::Key => InfoAuth::Key {
                    identity: identity.as_ref().map(|identity| identity.name.clone()),
                },
                AuthMethod::Agent => InfoAuth::Agent,
            };

            let jump_host = match host.jump_host_id {
                Some(jump_id) => Some(crate::store::hosts::host_get(conn, jump_id)?.name),
                None => None,
            };
            let proxy = match (host.proxy_mode, host.proxy_id) {
                (ProxyMode::Custom, Some(proxy_id)) => {
                    Some(crate::store::proxies::proxy_get(conn, proxy_id)?.name)
                }
                _ => None,
            };

            Ok(ConnectionInfo {
                name: host.name,
                address: host.host,
                port: host.port,
                username: host.username,
                auth,
                jump_host,
                proxy,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IdentityKind;

    fn host(auth_method: AuthMethod, identity_id: Option<i64>) -> Host {
        Host {
            id: 1,
            group_id: None,
            name: "web-1".to_owned(),
            host: "host.example".to_owned(),
            port: 22,
            username: "root".to_owned(),
            auth_method,
            identity_id,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn identity(kind: IdentityKind, has_secret: bool) -> Identity {
        Identity {
            id: 7,
            name: "生产".to_owned(),
            kind,
            key_path: None,
            has_secret,
        }
    }

    fn message(err: ZsshError) -> String {
        match err {
            ZsshError::InvalidInput(message) => message,
            other => other.to_string(),
        }
    }

    #[test]
    fn password_login_with_a_saved_password_resolves() {
        let id = identity(IdentityKind::Password, true);
        let got = resolve_saved_password(&host(AuthMethod::Password, Some(7)), Some(&id), || {
            Ok(Some("pw-value".to_owned()))
        })
        .expect("应该取到密码");
        assert_eq!(got.as_str(), "pw-value");
    }

    #[test]
    fn key_and_agent_hosts_have_no_password() {
        let id = identity(IdentityKind::Password, true);
        let key = resolve_saved_password(&host(AuthMethod::Key, Some(7)), Some(&id), || {
            panic!("密钥登录不该去解密")
        })
        .expect_err("密钥登录没有密码");
        assert!(message(key).contains("密钥登录"));

        let agent = resolve_saved_password(&host(AuthMethod::Agent, None), None, || {
            panic!("agent 登录不该去解密")
        })
        .expect_err("agent 登录没有密码");
        assert!(message(agent).contains("agent"));
    }

    #[test]
    fn password_login_without_an_identity_has_no_saved_password() {
        let err = resolve_saved_password(&host(AuthMethod::Password, None), None, || {
            panic!("没凭据不该去解密")
        })
        .expect_err("没引用凭据");
        assert_eq!(message(err), "这台主机没有保存密码");
    }

    #[test]
    fn a_key_kind_identity_is_never_used_as_a_password() {
        for kind in [
            IdentityKind::KeyFile,
            IdentityKind::KeyInline,
            IdentityKind::Agent,
        ] {
            let id = identity(kind, true);
            let err =
                resolve_saved_password(&host(AuthMethod::Password, Some(7)), Some(&id), || {
                    panic!("私钥类凭据的秘密不能被取出来当密码")
                })
                .expect_err("非密码类凭据要拒绝");
            assert!(message(err).contains("不是密码凭据"));
        }
    }

    #[test]
    fn an_identity_without_a_secret_has_no_saved_password() {
        let id = identity(IdentityKind::Password, false);
        let err = resolve_saved_password(&host(AuthMethod::Password, Some(7)), Some(&id), || {
            panic!("没存密码不该去解密")
        })
        .expect_err("凭据里没存密码");
        assert_eq!(message(err), "这台主机没有保存密码");

        // 标记说有、解出来却是空：同样按没存处理
        let id = identity(IdentityKind::Password, true);
        let err = resolve_saved_password(&host(AuthMethod::Password, Some(7)), Some(&id), || {
            Ok(Some(String::new()))
        })
        .expect_err("空密码");
        assert_eq!(message(err), "这台主机没有保存密码");
    }

    #[test]
    fn a_decrypt_failure_is_reported_not_swallowed() {
        let id = identity(IdentityKind::Password, true);
        let err = resolve_saved_password(&host(AuthMethod::Password, Some(7)), Some(&id), || {
            Err(ZsshError::Secret("解密失败".to_owned()))
        })
        .expect_err("解密失败要报出来");
        assert!(matches!(err, ZsshError::Secret(_)));
    }

    fn info(auth: InfoAuth) -> ConnectionInfo {
        ConnectionInfo {
            name: "web-1".to_owned(),
            address: "host.example".to_owned(),
            port: 22,
            username: "root".to_owned(),
            auth,
            jump_host: None,
            proxy: None,
        }
    }

    #[test]
    fn password_info_lists_every_field_and_the_command() {
        let data = info(InfoAuth::Password(Some(Zeroizing::new(
            "pw-value".to_owned(),
        ))));
        let text = format_connection_info(&data);
        assert_eq!(
            text.as_str(),
            "名称：web-1\n地址：host.example\n端口：22\n用户名：root\n密码：pw-value\nssh -p 22 root@host.example"
        );
        assert!(data.has_password());
    }

    #[test]
    fn password_login_without_a_saved_password_says_so() {
        let data = info(InfoAuth::Password(None));
        let text = format_connection_info(&data);
        assert!(text.contains("密码：未保存"));
        assert!(!data.has_password());
    }

    #[test]
    fn key_login_names_the_identity_and_nothing_secret() {
        let named = info(InfoAuth::Key {
            identity: Some("生产".to_owned()),
        });
        let text = format_connection_info(&named);
        assert!(text.contains("认证：私钥（凭据「生产」）"));
        assert!(!text.contains("密码"));
        assert!(!named.has_password());

        let bare = format_connection_info(&info(InfoAuth::Key { identity: None }));
        assert!(bare.contains("认证：私钥\n"));
        let agent = format_connection_info(&info(InfoAuth::Agent));
        assert!(agent.contains("认证：SSH agent"));
    }

    #[test]
    fn jump_host_and_proxy_lines_come_before_the_command() {
        let mut data = info(InfoAuth::Password(None));
        data.jump_host = Some("bastion".to_owned());
        data.proxy = Some("公司代理".to_owned());
        let text = format_connection_info(&data);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "名称：web-1",
                "地址：host.example",
                "端口：22",
                "用户名：root",
                "密码：未保存",
                "跳板机：bastion",
                "代理：公司代理",
                "ssh -p 22 root@host.example",
            ]
        );
    }

    #[test]
    fn ipv6_addresses_are_written_without_brackets() {
        assert_eq!(bare_address("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(bare_address(" 2001:db8::1 "), "2001:db8::1");
        assert_eq!(bare_address("fe80::1%eth0"), "fe80::1%eth0");
        // 没冒号的不是 IPv6，方括号原样保留
        assert_eq!(bare_address("[host]"), "[host]");
        assert_eq!(bare_address("host.example"), "host.example");

        let mut data = info(InfoAuth::Agent);
        data.address = "[2001:db8::1]".to_owned();
        data.port = 2222;
        let text = format_connection_info(&data);
        assert!(text.contains("地址：2001:db8::1\n"));
        assert!(text.ends_with("ssh -p 2222 root@2001:db8::1"));
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let data = info(InfoAuth::Password(Some(Zeroizing::new(
            "pw-secret".to_owned(),
        ))));
        assert!(!format!("{data:?}").contains("pw-secret"));
    }
}

#[cfg(all(test, windows))]
mod db_tests {
    use crate::model::{
        AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind, ProxyMode,
    };
    use crate::store::db::Database;

    fn draft(name: &str, auth_method: AuthMethod, identity_id: Option<i64>) -> HostDraft {
        HostDraft {
            id: None,
            group_id: None,
            name: name.to_owned(),
            host: "host.example".to_owned(),
            port: 2222,
            username: "deploy".to_owned(),
            auth_method,
            identity_id,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
        }
    }

    fn identity(db: &Database, kind: IdentityKind, secret: Option<&str>) -> i64 {
        db.identity_save(&IdentityDraft {
            id: None,
            name: "凭据甲".to_owned(),
            kind,
            key_path: None,
            secret: secret.map(str::to_owned),
            passphrase: None,
        })
        .expect("存凭据")
        .id
    }

    #[test]
    fn saved_password_comes_out_of_the_vault() {
        let db = Database::open_in_memory().expect("内存库");
        let identity_id = identity(&db, IdentityKind::Password, Some("vault-pw"));
        let host = db
            .host_save(&draft("甲", AuthMethod::Password, Some(identity_id)))
            .expect("存主机");
        assert_eq!(
            db.host_saved_password(host.id).unwrap().as_str(),
            "vault-pw"
        );
    }

    #[test]
    fn saved_password_errors_follow_the_rules() {
        let db = Database::open_in_memory().expect("内存库");
        let no_secret = identity(&db, IdentityKind::Password, None);
        let host = db
            .host_save(&draft("乙", AuthMethod::Password, Some(no_secret)))
            .expect("存主机");
        assert!(db
            .host_saved_password(host.id)
            .expect_err("没存密码")
            .to_string()
            .contains("没有保存密码"));

        let key = identity(&db, IdentityKind::KeyInline, Some("key-body"));
        let key_host = db
            .host_save(&draft("丙", AuthMethod::Key, Some(key)))
            .expect("存主机");
        assert!(db
            .host_saved_password(key_host.id)
            .expect_err("密钥登录")
            .to_string()
            .contains("密钥登录"));
    }

    #[test]
    fn connection_info_collects_jump_host_and_proxy_names() {
        let db = Database::open_in_memory().expect("内存库");
        let identity_id = identity(&db, IdentityKind::Password, Some("vault-pw"));
        let jump = db
            .host_save(&draft("跳板", AuthMethod::Agent, None))
            .expect("存跳板机");
        let proxy = db
            .proxy_save(&ProxyDraft {
                id: None,
                name: "公司代理".to_owned(),
                kind: ProxyKind::Socks5,
                host: "proxy.example".to_owned(),
                port: 1080,
                username: None,
                secret: None,
            })
            .expect("存代理");
        let mut target = draft("目标", AuthMethod::Password, Some(identity_id));
        target.jump_host_id = Some(jump.id);
        target.proxy_id = Some(proxy.id);
        target.proxy_mode = ProxyMode::Custom;
        let target = db.host_save(&target).expect("存主机");

        let info = db.host_connection_info(target.id).expect("收集连接信息");
        assert!(info.has_password());
        assert_eq!(info.jump_host.as_deref(), Some("跳板"));
        assert_eq!(info.proxy.as_deref(), Some("公司代理"));
        let text = super::format_connection_info(&info);
        assert!(text.contains("密码：vault-pw"));
        assert!(text.ends_with("ssh -p 2222 deploy@host.example"));
    }

    #[test]
    fn connection_info_without_a_saved_password_still_works() {
        let db = Database::open_in_memory().expect("内存库");
        let host = db
            .host_save(&draft("丁", AuthMethod::Password, None))
            .expect("存主机");
        let info = db.host_connection_info(host.id).expect("收集连接信息");
        assert!(!info.has_password());
        assert!(super::format_connection_info(&info).contains("密码：未保存"));
    }
}
