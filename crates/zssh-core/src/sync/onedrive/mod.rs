//! OneDrive 存储（S-4）：Z-SSH 发起、系统浏览器授权登录，同步包放在应用专用文件夹里。
//!
//! 只支持**个人微软账号**。依据：OneDrive 权限参考页写明 `Files.ReadWrite.AppFolder` 这个委托
//! 权限 “only valid for personal accounts”（<https://learn.microsoft.com/en-us/onedrive/developer/rest-api/concepts/permissions_reference>），
//! Microsoft Graph 的“Get special folder”页面里工作 / 学校账号一栏也没有列这个权限，而另一篇
//! app folder 概念页又说两种都支持——官方文档自相矛盾，查不清就不做（授权地址固定用 `consumers`）。
//!
//! 分层：
//! - [`pkce`] / [`browser`] / [`loopback`] / [`oauth`] / [`login`]：授权登录流程；
//! - [`tokens`]：令牌的本机存储（DPAPI）和自动刷新；
//! - [`graph`]：Graph 接口上的 `SyncStorage` 实现（条件写入、上传会话、限流）。

pub mod browser;
pub mod config;
pub mod graph;
pub mod login;
pub mod loopback;
pub mod oauth;
pub mod pkce;
pub mod tokens;
