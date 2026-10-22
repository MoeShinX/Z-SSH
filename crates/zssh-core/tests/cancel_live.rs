//! 取消连接（M1-06 §2.1）的联网测试。
//!
//! 连接卡在密码弹窗上时把任务取消（界面上的 `session_close` 就是这么做的：
//! 取消信号 → select 掉整个连接 future），验证：
//!
//! - 后台连接任务在有限时间内退出
//! - 挂着的提问跟着 future 一起被丢弃，不会留下一直等答案的任务
//!
//! 需要测试服务器；环境变量不全时跳过并打印原因。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{timeout, Instant};
use zssh_core::ssh::{
    AuthConfig, BoxFuture, ConnectParams, Connection, HostKeyDecision, HostKeyInfo, Interactor,
    PromptAnswers, PromptRequest,
};

use common::{serial, TempKnownHosts, TestServer};

/// 在密码弹窗上卡住的交互：问题被问过之后一直等（模拟用户还没回答就关了会话）。
struct StuckInteractor {
    asked: AtomicUsize,
    /// 提问的 future 被丢弃时置位：取消之后它应该是 true。
    prompt_dropped: AtomicBool,
}

/// future 被丢弃时置位的小守卫。
struct DropWatch<'a>(&'a AtomicBool);

impl Drop for DropWatch<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Interactor for StuckInteractor {
    fn confirm_host_key(&self, _info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision> {
        Box::pin(async { HostKeyDecision::TrustAndSave })
    }

    fn prompt(&self, _request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>> {
        Box::pin(async {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let _watch = DropWatch(&self.prompt_dropped);
            // 永远不返回：模拟弹窗一直开着没人回答
            std::future::pending::<()>().await;
            unreachable!("pending 不会返回");
        })
    }
}

#[tokio::test]
async fn cancelling_while_a_prompt_is_open_stops_the_connect_task() {
    if !common::full_live("cancelling_while_a_prompt_is_open_stops_the_connect_task") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 cancelling_while_a_prompt_is_open_stops_the_connect_task：{reason}");
            return;
        }
    };

    let known_hosts = TempKnownHosts::new("cancel-prompt");
    let interactor = Arc::new(StuckInteractor {
        asked: AtomicUsize::new(0),
        prompt_dropped: AtomicBool::new(false),
    });

    let params = ConnectParams {
        host: server.host.clone(),
        port: server.port,
        username: server.username.clone(),
        // 密码不在配置里：认证一定会走到"问用户要密码"这一步
        auth: AuthConfig::password(None),
        known_hosts: known_hosts.known_hosts(),
        legacy_algorithms: false,
    };
    let task = {
        let interactor = Arc::clone(&interactor) as Arc<dyn Interactor>;
        tokio::spawn(async move { Connection::connect(params, interactor).await.map(|_| ()) })
    };

    // 等连接走到密码弹窗
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if interactor.asked.load(Ordering::SeqCst) > 0 {
            break;
        }
        if Instant::now() >= deadline {
            panic!("等 30 秒连接也没走到密码弹窗");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("连接正卡在密码弹窗上，取消");

    // 取消 = 把这个任务掐掉。界面里的路径是 CancelSignal → select 掉连接 future，
    // 效果一样：整个 future 树被丢弃。
    task.abort();
    match timeout(Duration::from_secs(5), task).await {
        Ok(Err(err)) => assert!(err.is_cancelled(), "任务应该以取消结束：{err}"),
        Ok(Ok(_)) => panic!("弹窗没人回答，连接不该成功"),
        Err(_) => panic!("取消之后任务没有在 5 秒内退出"),
    }

    assert!(
        interactor.prompt_dropped.load(Ordering::SeqCst),
        "取消之后提问还挂着（future 没有被丢弃）"
    );
    println!("任务已退出，挂着的提问跟着一起没了");
}
