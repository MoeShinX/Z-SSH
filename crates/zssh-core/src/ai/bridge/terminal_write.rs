//! AI 命令写进用户终端之前的最后一道核对（审计 C04 终端链路、C16）。
//!
//! 终端里的 AI 命令要先排进会话的命令队列，再由会话任务真正写出去。从工具桥放行到真正写入
//! 之间要等全局会话表、等队列有空位、等前面的写入完成——这期间用户可能点了停止或把目标移出
//! 对话。所以有效性检查不能只在入队之前做一次，要跟着消息一起到写入的地方：
//!
//! - 调用方（[`deliver_ai_write`]）排队时同时等"请求失效"，队列满也不会一直卡着；
//! - 消费者（会话任务）真正写之前调 [`AiWriteSlot::claim`]，失效就丢掉这条，不写；
//! - 消费者认领之前请求失效了，调用方把这条撤回（之后消费者认领会失败），立刻返回原因。
//!
//! 认领和撤回用同一个原子状态抢：两边只有一个能赢，不会出现"调用方说没写、其实写了"。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::ssh::connection::ExecGuard;

const PENDING: u8 = 0;
const CLAIMED: u8 = 1;
const REVOKED: u8 = 2;

/// 消费者认领之后、写完之前会话没了（关闭会话会丢掉正在写的这条）。
pub const WRITE_INTERRUPTED: &str = "会话在写入 AI 命令时关闭了（命令可能只写进去一部分）";
/// 会话任务已经结束，队列收不了了。
pub const SESSION_GONE: &str = "会话已经结束，命令没有写进终端";

struct Shared {
    state: AtomicU8,
    guard: Arc<dyn ExecGuard>,
}

/// 排进会话命令队列的一条 AI 写入的"凭证"：消费者真正写之前要先 [`AiWriteSlot::claim`]。
pub struct AiWriteSlot {
    shared: Arc<Shared>,
    done: oneshot::Sender<Result<(), String>>,
}

impl std::fmt::Debug for AiWriteSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiWriteSlot")
            .field("state", &self.shared.state.load(Ordering::SeqCst))
            .finish()
    }
}

/// 认领成功：可以写了。写完（成败都要）调 [`AiWriteClaim::finish`] 把结果交回调用方。
#[derive(Debug)]
pub struct AiWriteClaim {
    done: oneshot::Sender<Result<(), String>>,
}

impl AiWriteSlot {
    /// 消费者在**真正写入之前**调用。
    ///
    /// 返回 `None` 时不能写：调用方已经撤回了这条，或者请求在排队期间失效了（原因已经交回
    /// 调用方）。认领之后调用方就不能再撤回。
    pub fn claim(self) -> Option<AiWriteClaim> {
        if self
            .shared
            .state
            .compare_exchange(PENDING, CLAIMED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        // 认领之后再核对：撤回只在调用方看到失效时发生，认领前就失效的这里一定能看到。
        if let Err(reason) = self.shared.guard.check() {
            let _ = self.done.send(Err(reason));
            return None;
        }
        Some(AiWriteClaim { done: self.done })
    }
}

impl AiWriteClaim {
    /// 把写入结果交回调用方。
    pub fn finish(self, result: Result<(), String>) {
        let _ = self.done.send(result);
    }
}

/// 把一条 AI 写入排进会话的命令队列，等它真正写出去（或被拒、被撤回）。
///
/// `make` 用凭证造出队列里的那条消息。返回 `Ok(())` 表示消费者已经把它写进终端；`Err` 是
/// 给模型的原因：请求失效（停止 / 目标移出）、会话已经结束，或者写入失败。
pub async fn deliver_ai_write<T, F>(
    sender: &mpsc::Sender<T>,
    guard: Arc<dyn ExecGuard>,
    make: F,
) -> Result<(), String>
where
    F: FnOnce(AiWriteSlot) -> T,
{
    guard.check()?;
    // 队列满（会话任务卡在写入上）时也要能因为停止 / 移出而放弃，不能一直等空位（C16）。
    let permit = tokio::select! {
        permit = sender.reserve() => permit.map_err(|_| SESSION_GONE.to_owned())?,
        reason = guard.invalidated() => return Err(reason),
    };
    let shared = Arc::new(Shared {
        state: AtomicU8::new(PENDING),
        guard: Arc::clone(&guard),
    });
    let (done, mut result) = oneshot::channel();
    permit.send(make(AiWriteSlot {
        shared: Arc::clone(&shared),
        done,
    }));

    tokio::select! {
        outcome = &mut result => return outcome.unwrap_or_else(|_| Err(gone_or_interrupted(&shared))),
        reason = guard.invalidated() => {
            if shared
                .state
                .compare_exchange(PENDING, REVOKED, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Err(reason);
            }
        }
    }
    // 消费者已经认领：它要么马上按失效拒掉，要么已经在写——以它的结果为准。
    result
        .await
        .unwrap_or_else(|_| Err(gone_or_interrupted(&shared)))
}

/// 消费者没回结果就丢了凭证：没认领过是会话结束，认领过是写到一半会话关了。
fn gone_or_interrupted(shared: &Shared) -> String {
    if shared.state.load(Ordering::SeqCst) == CLAIMED {
        WRITE_INTERRUPTED.to_owned()
    } else {
        SESSION_GONE.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use tokio::sync::Notify;

    use crate::ssh::BoxFuture;

    /// 可控的有效性：`invalidate` 之后 `check` 失败、`invalidated` 返回。
    #[derive(Default)]
    struct FakeGuard {
        invalid: AtomicBool,
        notify: Notify,
    }

    impl FakeGuard {
        fn invalidate(&self) {
            self.invalid.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    impl ExecGuard for FakeGuard {
        fn check(&self) -> Result<(), String> {
            if self.invalid.load(Ordering::SeqCst) {
                Err("这台服务器已经被移出这个对话，这次操作已取消".to_owned())
            } else {
                Ok(())
            }
        }

        fn invalidated(&self) -> BoxFuture<'_, String> {
            Box::pin(async move {
                loop {
                    let notified = self.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if let Err(reason) = self.check() {
                        return reason;
                    }
                    notified.await;
                }
            })
        }

        fn stop_requested(&self) -> bool {
            false
        }
    }

    fn guard() -> Arc<FakeGuard> {
        Arc::new(FakeGuard::default())
    }

    #[tokio::test]
    async fn a_claimed_write_reports_the_consumer_result() {
        let (tx, mut rx) = mpsc::channel::<AiWriteSlot>(1);
        let consumer = tokio::spawn(async move {
            let slot = rx.recv().await.expect("要收到");
            let claim = slot.claim().expect("有效时能认领");
            claim.finish(Ok(()));
        });
        let result = deliver_ai_write(&tx, guard(), |slot| slot).await;
        assert_eq!(result, Ok(()));
        consumer.await.expect("消费者不 panic");
    }

    #[tokio::test]
    async fn removing_the_target_while_queued_drops_the_write_and_returns_at_once() {
        // 会话任务卡着没取队列：排队期间目标被移出，调用方立刻拿到原因，消费者之后取到
        // 这条也认领不了（不会再写出去）。
        let (tx, mut rx) = mpsc::channel::<AiWriteSlot>(1);
        let guard = guard();
        let deliver = {
            let guard: Arc<dyn ExecGuard> = guard.clone();
            let tx = tx.clone();
            tokio::spawn(async move { deliver_ai_write(&tx, guard, |slot| slot).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        guard.invalidate();
        let result = tokio::time::timeout(Duration::from_secs(5), deliver)
            .await
            .expect("失效后要立刻返回")
            .expect("不 panic");
        assert!(result.expect_err("已失效").contains("移出"));
        let slot = rx.recv().await.expect("消息已经在队列里");
        assert!(slot.claim().is_none(), "撤回之后消费者不能再写");
    }

    #[tokio::test]
    async fn stop_while_the_queue_is_full_gives_up_without_enqueueing() {
        // C16：队列满（会话任务卡在写入上）时请求失效，不能一直等空位。
        let (tx, mut rx) = mpsc::channel::<Option<AiWriteSlot>>(1);
        tx.send(None).await.expect("先占满队列");
        let guard = guard();
        let deliver = {
            let guard: Arc<dyn ExecGuard> = guard.clone();
            let tx = tx.clone();
            tokio::spawn(async move { deliver_ai_write(&tx, guard, Some).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!deliver.is_finished(), "队列满时要等");
        guard.invalidate();
        let result = tokio::time::timeout(Duration::from_secs(5), deliver)
            .await
            .expect("失效后不能再卡在满队列上")
            .expect("不 panic");
        assert!(result.is_err());
        assert!(matches!(rx.recv().await, Some(None)), "占位的那条");
        assert!(rx.try_recv().is_err(), "失效的命令根本没进队列");
    }

    #[tokio::test]
    async fn invalidation_after_queueing_but_before_the_write_is_seen_by_the_consumer() {
        // 消费者先取到消息、请求才失效（调用方还没来得及撤回）：认领时的核对要拦住。
        let (tx, mut rx) = mpsc::channel::<AiWriteSlot>(1);
        let guard = guard();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consumer = {
            let guard = guard.clone();
            let writes = writes.clone();
            tokio::spawn(async move {
                let slot = rx.recv().await.expect("要收到");
                guard.invalidate();
                if let Some(claim) = slot.claim() {
                    writes.fetch_add(1, Ordering::SeqCst);
                    claim.finish(Ok(()));
                }
            })
        };
        let result = deliver_ai_write(&tx, guard.clone(), |slot| slot).await;
        assert!(result.expect_err("已失效").contains("移出"));
        consumer.await.expect("不 panic");
        assert_eq!(writes.load(Ordering::SeqCst), 0, "不能写出去");
    }

    #[tokio::test]
    async fn a_claimed_write_is_not_revoked_by_a_later_invalidation() {
        // 认领之后才失效：命令已经在写了，调用方要等消费者的真实结果，不能谎报"没写"。
        let (tx, mut rx) = mpsc::channel::<AiWriteSlot>(1);
        let guard = guard();
        let (claimed_tx, claimed_rx) = oneshot::channel::<()>();
        let (finish_tx, finish_rx) = oneshot::channel::<()>();
        let consumer = tokio::spawn(async move {
            let slot = rx.recv().await.expect("要收到");
            let claim = slot.claim().expect("有效时能认领");
            let _ = claimed_tx.send(());
            let _ = finish_rx.await;
            claim.finish(Ok(()));
        });
        let deliver = {
            let guard: Arc<dyn ExecGuard> = guard.clone();
            let tx = tx.clone();
            tokio::spawn(async move { deliver_ai_write(&tx, guard, |slot| slot).await })
        };
        claimed_rx.await.expect("认领了");
        guard.invalidate();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!deliver.is_finished(), "已经认领的写入要等消费者的结果");
        let _ = finish_tx.send(());
        let result = deliver.await.expect("不 panic");
        assert_eq!(result, Ok(()), "命令确实写出去了");
        consumer.await.expect("不 panic");
    }

    #[tokio::test]
    async fn closing_the_session_mid_write_reports_an_interrupted_write() {
        let (tx, mut rx) = mpsc::channel::<AiWriteSlot>(1);
        let consumer = tokio::spawn(async move {
            let slot = rx.recv().await.expect("要收到");
            let claim = slot.claim().expect("能认领");
            // 会话关闭：正在写的这条被丢掉，没有结果。
            drop(claim);
        });
        let result = deliver_ai_write(&tx, guard(), |slot| slot).await;
        assert_eq!(result, Err(WRITE_INTERRUPTED.to_owned()));
        consumer.await.expect("不 panic");
    }

    #[tokio::test]
    async fn a_closed_queue_reports_the_session_is_gone() {
        let (tx, rx) = mpsc::channel::<AiWriteSlot>(1);
        drop(rx);
        let result = deliver_ai_write(&tx, guard(), |slot| slot).await;
        assert_eq!(result, Err(SESSION_GONE.to_owned()));
    }
}
