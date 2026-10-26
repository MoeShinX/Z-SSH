//! 把 core 的 [`Interactor`] 接到界面上：请求 ID + 事件 + 应答命令（设计文档 4.3）。
//!
//! core 发一次提问就注册一个 oneshot，前端调 `hostkey_answer` / `auth_answer` 应答；
//! 120 秒没回答按取消处理，会话关闭时把还没回答的提问全部取消，不留挂住的 oneshot。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use zssh_core::ssh::{
    BoxFuture, HopProgress, HostKeyDecision, HostKeyInfo, HostKeyStatus, Interactor, PromptAnswers,
    PromptKind, PromptRequest,
};

use crate::events;
use crate::state::CancelSignal;

/// 等用户回答的最长时间。
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
pub const HOSTKEY_EVENT: &str = "hostkey_prompt";
pub const AUTH_EVENT: &str = "auth_prompt";
/// 一次提问结束（回答、超时、会话取消、等待被丢弃都算）：前端据此把它从队列里摘掉。
pub const PROMPT_CLOSED_EVENT: &str = "prompt_closed";

/// `prompt_closed` 的载荷。指纹和认证共用一个 ID 序列，所以只带 req_id 就够。
#[derive(Debug, Clone, Serialize)]
pub struct PromptClosed {
    pub req_id: u64,
}

/// 等待提问的那段 future 结束时（不管是正常返回、超时，还是整个连接任务被丢弃）
/// 执行一次收尾。为什么用 Drop：连接任务可能在 await 中途被直接丢弃，只在各个返回
/// 分支里发事件会漏掉这种情况，前端队列里就会留下一条再也答不了的提问。
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
    }
}

/// 提问结束时通知前端的守卫。
fn closed_notice(app: &AppHandle, req_id: u64) -> OnDrop<impl FnOnce()> {
    let app = app.clone();
    OnDrop(Some(move || {
        if let Err(err) = app.emit(PROMPT_CLOSED_EVENT, PromptClosed { req_id }) {
            debug!(%err, "发提问结束事件失败");
        }
    }))
}

/// 发给前端的指纹提问。
#[derive(Debug, Clone, Serialize)]
pub struct HostKeyPrompt {
    pub req_id: u64,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    /// unknown / changed / revoked / other_key_types_saved
    pub status: &'static str,
    /// 指纹变化 / 吊销时已保存的旧指纹（弹窗里和新指纹对比）
    pub stored_fingerprint: Option<String>,
}

/// keyboard-interactive 的一道题。
#[derive(Debug, Clone, Serialize)]
pub struct AuthPromptItem {
    pub text: String,
    pub echo: bool,
}

/// 发给前端的认证提问。
#[derive(Debug, Clone, Serialize)]
pub struct AuthPrompt {
    pub req_id: u64,
    /// password / passphrase / keyboard_interactive
    pub kind: &'static str,
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<AuthPromptItem>,
    pub can_remember: bool,
}

/// 未决提问表。用户回答、超时、会话关闭都从这里摘。
///
/// 每条提问都记着它属于哪个会话：取消一个会话的提问（关掉正在连的会话）不能
/// 把别的会话挂着的提问也取消掉。
/// 指纹提问：`(会话 ID, 应答通道)`。
type PendingHostKey = (String, oneshot::Sender<HostKeyDecision>);
/// 认证提问：`(会话 ID, 应答通道)`。
type PendingAuth = (String, oneshot::Sender<Option<PromptAnswers>>);

#[derive(Default)]
pub struct PromptRegistry {
    host_keys: Mutex<HashMap<u64, PendingHostKey>>,
    auths: Mutex<HashMap<u64, PendingAuth>>,
    next_id: AtomicU64,
}

impl PromptRegistry {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn add_host_key(&self, session_id: &str, sender: oneshot::Sender<HostKeyDecision>) -> u64 {
        let req_id = self.next_id();
        self.host_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(req_id, (session_id.to_owned(), sender));
        req_id
    }

    fn add_auth(&self, session_id: &str, sender: oneshot::Sender<Option<PromptAnswers>>) -> u64 {
        let req_id = self.next_id();
        self.auths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(req_id, (session_id.to_owned(), sender));
        req_id
    }

    /// 摘掉某个会话挂着的全部提问，返回条数。丢掉 sender，等待方收到 Err。
    fn take_of_session<T>(map: &Mutex<HashMap<u64, (String, T)>>, session_id: &str) -> Vec<T> {
        let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mine: Vec<u64> = map
            .iter()
            .filter(|(_, (owner, _))| owner == session_id)
            .map(|(req_id, _)| *req_id)
            .collect();
        mine.into_iter()
            .filter_map(|req_id| map.remove(&req_id))
            .map(|(_, sender)| sender)
            .collect()
    }

    fn forget_host_key(&self, req_id: u64) {
        self.host_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&req_id);
    }

    /// 前端的应答：`decision` 见 [`HostKeyDecision`]。
    pub fn answer_host_key(&self, req_id: u64, decision: HostKeyDecision) -> Result<(), String> {
        let sender = self
            .host_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&req_id);
        match sender {
            Some((_session, sender)) => sender
                .send(decision)
                .map_err(|_| "这次提问已经结束了".to_owned()),
            None => Err(format!("没有这个指纹请求：{req_id}")),
        }
    }

    /// 前端的应答：`answers` 为 `None` 表示取消，`remember` 是"保存到保险库"。
    pub fn answer_auth(
        &self,
        req_id: u64,
        answers: Option<Vec<String>>,
        remember: bool,
    ) -> Result<(), String> {
        let sender = self
            .auths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&req_id);
        match sender {
            Some((_session, sender)) => {
                let answers = answers.map(|answers| PromptAnswers { answers, remember });
                sender
                    .send(answers)
                    .map_err(|_| "这次提问已经结束了".to_owned())
            }
            None => Err(format!("没有这个认证请求：{req_id}")),
        }
    }

    /// 会话关掉（或者连接失败）时调用：这个会话挂着的提问全部取消。
    ///
    /// 返回取消了几条。别的会话挂着的提问不受影响。
    pub fn cancel_session(&self, session_id: &str) -> usize {
        // 丢掉 sender，等待方会收到 Err，按取消处理
        let host_keys = Self::take_of_session(&self.host_keys, session_id);
        let auths = Self::take_of_session(&self.auths, session_id);
        let pending = host_keys.len() + auths.len();
        if pending > 0 {
            info!(%session_id, pending, "会话结束，取消还在等待的提问");
        }
        pending
    }
}

/// 接界面的交互实现。
pub struct UiInteractor {
    app: AppHandle,
    registry: Arc<PromptRegistry>,
    /// 这次连接对应的会话 id：进度事件要带上它
    session_id: String,
    /// 会话是不是已经被取消了（取消了就不再弹提问、不再发进度）
    cancel: Arc<CancelSignal>,
}

impl UiInteractor {
    pub fn new(
        app: AppHandle,
        registry: Arc<PromptRegistry>,
        session_id: String,
        cancel: Arc<CancelSignal>,
    ) -> Self {
        Self {
            app,
            registry,
            session_id,
            cancel,
        }
    }
}

impl Interactor for UiInteractor {
    fn confirm_host_key(&self, info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision> {
        let app = self.app.clone();
        let registry = Arc::clone(&self.registry);
        let session_id = self.session_id.clone();
        let cancel = Arc::clone(&self.cancel);
        Box::pin(async move {
            // 会话已经取消了就别再弹窗了
            if cancel.is_cancelled() {
                return HostKeyDecision::Reject;
            }
            let (sender, receiver) = oneshot::channel();
            let req_id = registry.add_host_key(&session_id, sender);
            let _closed = closed_notice(&app, req_id);
            let payload = HostKeyPrompt {
                req_id,
                host: info.host.clone(),
                port: info.port,
                key_type: info.key_type.clone(),
                fingerprint: info.fingerprint.clone(),
                status: status_str(info.status),
                stored_fingerprint: info.stored_fingerprint.clone(),
            };

            if let Err(err) = app.emit(HOSTKEY_EVENT, payload) {
                warn!(%err, "发指纹提问事件失败");
                registry.forget_host_key(req_id);
                return HostKeyDecision::Reject;
            }
            // 缩在托盘里时看不到指纹确认框：叫出窗口（UI-4）。
            crate::tray::call_attention(&app);

            match tokio::time::timeout(ANSWER_TIMEOUT, receiver).await {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) => {
                    debug!("指纹提问被取消");
                    HostKeyDecision::Reject
                }
                Err(_) => {
                    info!("等用户确认主机指纹超过 {ANSWER_TIMEOUT:?}，按取消处理");
                    registry.forget_host_key(req_id);
                    HostKeyDecision::Reject
                }
            }
        })
    }

    /// 开始连某一跳：发一条带跳数进度的 connecting，界面显示"第 2/3 跳"。
    fn hop_started(&self, progress: HopProgress) -> BoxFuture<'_, ()> {
        let app = self.app.clone();
        let session_id = self.session_id.clone();
        let cancel = Arc::clone(&self.cancel);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return;
            }
            events::emit_hop(&app, &session_id, &progress);
        })
    }

    /// 握手完成、开始认证：把状态换成"认证中"。
    fn authentication_started(&self) -> BoxFuture<'_, ()> {
        let app = self.app.clone();
        let session_id = self.session_id.clone();
        let cancel = Arc::clone(&self.cancel);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return;
            }
            events::emit_state(&app, &session_id, "authenticating", None);
        })
    }

    fn prompt(&self, request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>> {
        let app = self.app.clone();
        let registry = Arc::clone(&self.registry);
        let session_id = self.session_id.clone();
        let cancel = Arc::clone(&self.cancel);
        Box::pin(async move {
            // 会话已经取消了就别再弹窗了
            if cancel.is_cancelled() {
                return None;
            }
            let (sender, receiver) = oneshot::channel();
            let req_id = registry.add_auth(&session_id, sender);
            let _closed = closed_notice(&app, req_id);
            let payload = AuthPrompt {
                req_id,
                kind: kind_str(request.kind),
                name: request.name.clone(),
                instructions: request.instructions.clone(),
                prompts: request
                    .prompts
                    .iter()
                    .map(|prompt| AuthPromptItem {
                        text: prompt.text.clone(),
                        echo: prompt.echo,
                    })
                    .collect(),
                can_remember: request.can_remember,
            };

            if let Err(err) = app.emit(AUTH_EVENT, payload) {
                warn!(%err, "发认证提问事件失败");
                registry.answer_auth(req_id, None, false).ok();
                return None;
            }
            // 缩在托盘里时看不到密码 / 验证码输入框：叫出窗口（UI-4）。
            crate::tray::call_attention(&app);

            match tokio::time::timeout(ANSWER_TIMEOUT, receiver).await {
                Ok(Ok(answers)) => answers,
                Ok(Err(_)) => {
                    debug!("认证提问被取消");
                    None
                }
                Err(_) => {
                    info!("等用户回答认证提问超过 {ANSWER_TIMEOUT:?}，按取消处理");
                    registry.answer_auth(req_id, None, false).ok();
                    None
                }
            }
        })
    }
}

fn status_str(status: HostKeyStatus) -> &'static str {
    match status {
        HostKeyStatus::Unknown => "unknown",
        HostKeyStatus::Changed => "changed",
        HostKeyStatus::Revoked => "revoked",
        HostKeyStatus::OtherKeyTypesSaved => "other_key_types_saved",
    }
}

fn kind_str(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::Password => "password",
        PromptKind::Passphrase => "passphrase",
        PromptKind::KeyboardInteractive => "keyboard_interactive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取消一个会话的提问不能碰到别的会话挂着的。
    #[tokio::test]
    async fn cancel_session_only_touches_its_own_prompts() {
        let registry = PromptRegistry::default();
        let (sender_a, receiver_a) = oneshot::channel();
        let req_a = registry.add_auth("sess-1", sender_a);
        let (sender_b, receiver_b) = oneshot::channel();
        let req_b = registry.add_auth("sess-2", sender_b);

        assert_eq!(registry.cancel_session("sess-1"), 1);
        // sess-1 的等待方收到取消（sender 被丢掉）
        assert!(receiver_a.await.is_err(), "被取消的提问不该还能回答");

        // sess-2 不受影响：还能正常回答
        assert!(
            registry
                .answer_auth(req_b, Some(vec!["密码".to_owned()]), false)
                .is_ok(),
            "别的会话的提问不该被取消"
        );
        let answers = receiver_b.await.expect("应该收到回答").expect("有回答");
        assert_eq!(answers.answers, vec!["密码".to_owned()]);

        // 已经取消掉的请求再回答只会报"已经结束"
        assert!(registry.answer_auth(req_a, None, false).is_err());
    }

    /// 指纹提问也按会话取消。
    #[tokio::test]
    async fn cancel_session_clears_host_key_prompts_too() {
        let registry = PromptRegistry::default();
        let (sender, receiver) = oneshot::channel();
        let _req = registry.add_host_key("sess-9", sender);

        assert_eq!(registry.cancel_session("sess-9"), 1);
        assert!(receiver.await.is_err());
        assert_eq!(registry.cancel_session("sess-9"), 0, "再取消一次是空的");
    }

    /// 等待提问的 future 被中途丢弃（连接任务被取消）时，收尾守卫也要执行，
    /// 否则前端收不到 prompt_closed，队列里会留下一条答不了的提问。
    #[tokio::test]
    async fn on_drop_runs_when_waiting_future_is_dropped() {
        let fired = Arc::new(AtomicU64::new(0));
        let waiting = {
            let fired = Arc::clone(&fired);
            async move {
                let _guard = OnDrop(Some(move || {
                    fired.fetch_add(1, Ordering::SeqCst);
                }));
                std::future::pending::<()>().await;
            }
        };
        let task = tokio::spawn(waiting);
        tokio::task::yield_now().await;
        assert_eq!(fired.load(Ordering::SeqCst), 0, "还在等的时候不该执行");
        task.abort();
        let _ = task.await;
        assert_eq!(fired.load(Ordering::SeqCst), 1, "丢弃后正好执行一次");
    }

    /// 取消信号：等的人会立刻醒过来。
    #[tokio::test]
    async fn cancel_signal_wakes_waiters() {
        let signal = Arc::new(CancelSignal::default());
        assert!(!signal.is_cancelled());

        let waiting = {
            let signal = Arc::clone(&signal);
            tokio::spawn(async move { signal.cancelled().await })
        };
        tokio::task::yield_now().await;

        signal.cancel();
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("cancel 之后等待方应该马上醒来")
            .expect("任务不该 panic");
        assert!(signal.is_cancelled());
    }
}
