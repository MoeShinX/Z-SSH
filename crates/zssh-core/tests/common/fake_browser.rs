//! 测试里扮演“系统浏览器”：Z-SSH 把授权地址交给它，它去请求模拟的授权页，跟着 302 回到本机回环地址。

#![allow(dead_code)]

use std::sync::Mutex;

use zssh_core::sync::onedrive::browser::BrowserOpener;

type Action = Box<dyn Fn(String) + Send + Sync>;

/// 记下收到的网址，再按 `action` 去“操作”。
pub struct FakeBrowser {
    urls: Mutex<Vec<String>>,
    action: Action,
}

impl FakeBrowser {
    pub fn new(action: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            urls: Mutex::new(Vec::new()),
            action: Box::new(action),
        }
    }

    /// 用户登录并同意：打开授权页，跟着 302 回到本机回环地址。
    pub fn approving() -> Self {
        Self::new(|url| {
            tokio::spawn(async move {
                let _ = browse(&url).await;
            });
        })
    }

    /// 浏览器什么也不做（用户一直没操作）。
    pub fn idle() -> Self {
        Self::new(|_| {})
    }

    pub fn urls(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }

    /// 交给浏览器的授权地址里的 `redirect_uri`。
    pub fn redirect_uri(&self) -> Option<String> {
        let url = self.urls().into_iter().next()?;
        redirect_uri_of(&url)
    }
}

pub fn redirect_uri_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
}

impl BrowserOpener for FakeBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.urls.lock().unwrap().push(url.to_owned());
        (self.action)(url.to_owned());
        Ok(())
    }
}

/// 一个会跟随重定向的“浏览器”（直连，不走代理：浏览器用的是系统自己的网络设置）。
pub async fn browse(url: &str) -> reqwest::Result<(u16, String)> {
    let client = reqwest::Client::builder().no_proxy().build()?;
    let response = client.get(url).send().await?;
    let status = response.status().as_u16();
    Ok((status, response.text().await.unwrap_or_default()))
}
