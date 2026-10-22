//! 联网测试要的刷新令牌：用**真实**的浏览器授权登录跑一遍，把刷新令牌打印出来。
//!
//! 用法（PowerShell，仓库根目录）：
//!
//! ```text
//! $env:ZSSH_ONEDRIVE_CLIENT_ID = "<在 Azure 注册的应用的 client ID>"
//! cargo run -p zssh-core --example onedrive_refresh_token
//! ```
//!
//! 会用系统默认浏览器打开微软登录页，登录并同意之后，终端里打印一行刷新令牌。**把它只放进环境变量**
//! `ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN`，不要提交、不要写进文件、不要贴到聊天或报告里。
//! 可选 `ZSSH_TEST_ONEDRIVE_SOCKS5=127.0.0.1:10808`：换令牌要走的 SOCKS5 代理（浏览器那一步不受它影响）。
//! 这个例子和界面里的「用微软账号登录」走的是同一套登录代码（`onedrive::login::login`）。

use std::process::ExitCode;
use std::sync::Arc;

use tokio::sync::Notify;
use zssh_core::sync::net::{ProxyScheme, SyncProxy};
use zssh_core::sync::onedrive::browser::SystemBrowser;
use zssh_core::sync::onedrive::config::is_valid_client_id;
use zssh_core::sync::onedrive::login::{login, LoginParams, LOGIN_TIMEOUT};
use zssh_core::sync::onedrive::oauth::Endpoints;

fn proxy_from_env() -> Option<SyncProxy> {
    let raw = std::env::var("ZSSH_TEST_ONEDRIVE_SOCKS5").ok()?;
    let (host, port) = raw.trim().rsplit_once(':')?;
    Some(SyncProxy {
        kind: ProxyScheme::Socks5,
        host: host.to_owned(),
        port: port.parse().ok()?,
        username: None,
        password: None,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let Some(client_id) = std::env::var("ZSSH_ONEDRIVE_CLIENT_ID")
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| is_valid_client_id(id))
    else {
        eprintln!("请先设置环境变量 ZSSH_ONEDRIVE_CLIENT_ID（形如 xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx，见 docs/release.md）");
        return ExitCode::from(2);
    };
    let proxy = proxy_from_env();
    let endpoints = Endpoints::microsoft();
    eprintln!("即将用系统默认浏览器打开微软的登录页，请登录并同意授权（最多等 5 分钟）…");
    let cancel = Arc::new(Notify::new());
    let result = login(
        LoginParams {
            client_id: &client_id,
            endpoints: &endpoints,
            proxy: proxy.as_ref(),
            timeout: LOGIN_TIMEOUT,
        },
        &SystemBrowser,
        &cancel,
    )
    .await;
    match result {
        Ok(success) => {
            eprintln!("登录成功。下面这一行是刷新令牌，只放进环境变量 ZSSH_TEST_ONEDRIVE_REFRESH_TOKEN，不要提交、不要贴到聊天里：");
            println!("{}", success.tokens.refresh_token);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("登录没有成功：{err}");
            ExitCode::FAILURE
        }
    }
}
