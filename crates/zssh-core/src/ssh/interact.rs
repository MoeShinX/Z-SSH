//! 认证和主机指纹校验中途要问用户的事情。
//!
//! `zssh-core` 不能依赖 Tauri，也不该自己去弹窗，所以这里只定义一个接口
//! （[`Interactor`]），由调用方实现：Tauri 层用"请求 ID + 事件 + 应答命令"接上，
//! 测试用模拟实现预设回答。

use std::future::Future;
use std::pin::Pin;

/// `Interactor` 的返回值要用这种装箱的 future，trait 才能当 trait object 用。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 主机密钥的信任决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// 信任并写进自己的 known_hosts
    TrustAndSave,
    /// 只信任这一次，不写文件
    TrustOnce,
    /// 不信任，终止连接
    Reject,
}

/// 这台主机在 known_hosts 里的状态，决定弹窗长什么样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// 没见过这台主机
    Unknown,
    /// 有记录但指纹不一样：红色警告；只有用户明确选「更新指纹」（`TrustAndSave`）才继续，
    /// 旧记录被换成新的（服务器重装 / DD 后最常见）
    Changed,
    /// 命中了 `@revoked`：只能关闭，任何回答都不继续
    Revoked,
    /// 只记过这台主机的其它类型密钥
    OtherKeyTypesSaved,
}

/// 未知主机弹窗要显示的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    /// 例如 `ssh-ed25519`
    pub key_type: String,
    /// OpenSSH 格式的 SHA256 指纹
    pub fingerprint: String,
    pub status: HostKeyStatus,
    /// 指纹变化 / 吊销时，已保存的那条旧指纹（让用户对比）；其它情况没有
    pub stored_fingerprint: Option<String>,
}

/// 提问的种类，界面据此决定用什么控件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// 密码认证要密码
    Password,
    /// 私钥文件或导入的私钥要口令
    Passphrase,
    /// 服务器的 keyboard-interactive 提问（可能是多轮，MFA 就在这）
    KeyboardInteractive,
}

impl PromptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passphrase => "passphrase",
            Self::KeyboardInteractive => "keyboard_interactive",
        }
    }
}

/// 一道题。keyboard-interactive 的提示原文和是否回显都来自服务器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptItem {
    pub text: String,
    /// false 表示不该回显（界面上用密码框）
    pub echo: bool,
}

/// 一次提问。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRequest {
    pub kind: PromptKind,
    /// keyboard-interactive 的标题和说明原文，其它类型是空串
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<PromptItem>,
    /// 界面上给不给"保存到保险库"的勾选框
    pub can_remember: bool,
}

impl PromptRequest {
    /// 单题提问（密码、口令）。
    pub fn single(kind: PromptKind, text: &str, can_remember: bool) -> Self {
        Self {
            kind,
            name: String::new(),
            instructions: String::new(),
            prompts: vec![PromptItem {
                text: text.to_owned(),
                echo: false,
            }],
            can_remember,
        }
    }
}

/// 用户对提问的回答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAnswers {
    /// 和 [`PromptRequest::prompts`] 一一对应
    pub answers: Vec<String>,
    /// 用户勾了"保存到保险库"（由调用方落库，core 不碰存储）
    pub remember: bool,
}

/// 开始连某一跳时的进度（界面拿它显示"连接中：第 2/3 跳 jump1"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopProgress {
    /// 第几跳，从 1 开始
    pub index: usize,
    /// 一共几跳
    pub total: usize,
    /// 这一跳是谁，例如"代理 socks5 127.0.0.1:10808"、"跳板机 jump1"、"目标 web1"
    pub label: String,
}

/// 认证和指纹校验要问用户的事情。
pub trait Interactor: Send + Sync {
    /// 未知主机、指纹变化时问用户。
    ///
    /// 返回 `Reject` 表示终止连接。指纹变化时只有 `TrustAndSave`（用户明确选了「更新指纹」）
    /// 才继续，`TrustOnce` 一律拒绝——无人值守的实现（[`NonInteractive`]）回答的就是它，
    /// 不能因此悄悄接受换过的密钥；吊销的情况任何回答都拒绝（兜底）。
    fn confirm_host_key(&self, info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision>;

    /// 要密码、口令，或者回答 keyboard-interactive 的提问。
    ///
    /// 返回 `None` 表示用户取消，整次连接终止。
    fn prompt(&self, request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>>;

    /// 开始连某一跳时的通知。默认什么都不做，探针和测试不用实现。
    fn hop_started(&self, _progress: HopProgress) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// 某一跳握手完成、开始认证时的通知（界面把状态文字换成"认证中"）。
    /// 默认什么都不做。
    fn authentication_started(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// 无人值守用的实现：主机密钥"仅本次"，需要提问时直接取消。
///
/// 给探针、压测和测试用；正式连接请实现 [`Interactor`] 接上界面。
#[derive(Debug, Default, Clone, Copy)]
pub struct NonInteractive;

impl Interactor for NonInteractive {
    fn confirm_host_key(&self, _info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision> {
        Box::pin(async { HostKeyDecision::TrustOnce })
    }

    fn prompt(&self, _request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>> {
        Box::pin(async { None })
    }
}
