//! 输出流控：记录"已经发给前端、但前端还没确认渲染完"的字节数。
//!
//! 设计文档 5.2 记了 russh 的实际背压机制：russh 在收到 `CHANNEL_DATA` 时就
//! 自动补窗口，SSH 层的窗口拦不住服务器；真正管用的是**停止读通道**——不读，
//! russh 交给通道的有界队列（`channel_buffer_size`）就会塞满，整条连接的事件
//! 循环卡在投递上，不再读 TCP，服务器那边自然停下来。
//!
//! 所以这个门只回答一个问题：现在还能不能继续读？
//!
//! - 未确认字节超过高水位就暂停；暂停之后必须降到低水位以下才恢复。中间那段
//!   靠 `paused` 标记维持滞回，避免在高水位附近反复抖。
//! - [`FlowGate::close`] 让正在等的任务立刻醒过来，会话关闭时不留下挂住的任务。

use std::sync::{Mutex, MutexGuard};

use tokio::sync::Notify;

/// 未确认字节超过它就停止读通道。
pub const HIGH_WATERMARK: usize = 1024 * 1024;
/// 暂停之后要降到这个值以下才恢复读。
pub const LOW_WATERMARK: usize = 256 * 1024;

#[derive(Debug, Default)]
struct GateState {
    /// 已发出、还没被 ack 掉的字节数。
    unacked: usize,
    /// 未确认量的历史峰值，压测时看它有没有越过高水位。
    peak_unacked: usize,
    /// 是否已经进入"暂停读"状态：超过高水位置位，降到低水位以下才清掉。
    paused: bool,
    closed: bool,
}

/// 输出流控门。`pump_channel` 读通道前问它，前端 ack 后喂它。
#[derive(Debug, Default)]
pub struct FlowGate {
    state: Mutex<GateState>,
    notify: Notify,
}

impl FlowGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 攒批事件发给前端之后调用。
    pub fn on_sent(&self, bytes: usize) {
        let mut state = self.lock();
        state.unacked = state.unacked.saturating_add(bytes);
        state.peak_unacked = state.peak_unacked.max(state.unacked);
    }

    /// 前端确认已经渲染完的字节数。
    pub fn on_ack(&self, bytes: usize) {
        {
            let mut state = self.lock();
            // 前端重复上报同一批时 ack 会超过已发出的量，按 0 处理，不能下溢。
            state.unacked = state.unacked.saturating_sub(bytes);
            if state.unacked <= LOW_WATERMARK {
                state.paused = false;
            }
        }
        // 暂停中的读任务需要重新判断一次。
        self.notify.notify_waiters();
    }

    /// 还能不能继续读。
    ///
    /// 未确认量没超高水位就直接放行；超了就在这里等，等到降到低水位以下
    /// （或者门被关掉）。返回 `false` 表示门已经关了，调用方应该收工。
    pub async fn wait_ready(&self) -> bool {
        loop {
            // 先把通知注册上再看状态，否则"读完状态到开始等"之间会丢掉唤醒。
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();

            {
                let mut state = self.lock();
                if state.closed {
                    return false;
                }
                if !state.paused && state.unacked <= HIGH_WATERMARK {
                    return true;
                }
                if state.unacked <= LOW_WATERMARK {
                    state.paused = false;
                    return true;
                }
                state.paused = true;
            }

            notified.await;
        }
    }

    /// 让所有等待者立刻返回 `false`。会话关闭、事件流被丢弃时调用。
    pub fn close(&self) {
        {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.closed = true;
        }
        self.notify.notify_waiters();
    }

    /// 当前未确认字节数。
    pub fn unacked(&self) -> usize {
        self.lock().unacked
    }

    /// 未确认字节数的历史峰值。
    pub fn peak_unacked(&self) -> usize {
        self.lock().peak_unacked
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// 锁里只有整数运算，不会 panic；万一被别处的 panic 连累中毒，拿回数据继续用。
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    /// 等一小会儿就够判断"阻塞了没有"，测试不能真的等下去。
    const TICK: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn not_blocked_below_high_watermark() {
        let gate = FlowGate::new();
        // 刚好到高水位还没超过，应该放行
        gate.on_sent(HIGH_WATERMARK);
        assert!(
            timeout(TICK, gate.wait_ready())
                .await
                .expect("未超高水位不该阻塞"),
            "门没关时应该返回 true"
        );

        gate.on_sent(1);
        // 超过高水位了，但还没人 ack
        assert!(
            timeout(TICK, gate.wait_ready()).await.is_err(),
            "超过高水位应该阻塞"
        );
    }

    #[tokio::test]
    async fn blocked_until_ack_reaches_low_watermark() {
        let gate = FlowGate::new();
        gate.on_sent(HIGH_WATERMARK + 1);
        assert!(
            timeout(TICK, gate.wait_ready()).await.is_err(),
            "超过高水位应该阻塞"
        );

        // 降到高低水位之间：还得继续等，不能在高水位附近抖
        gate.on_ack(HIGH_WATERMARK + 1 - (LOW_WATERMARK + 1));
        assert_eq!(gate.unacked(), LOW_WATERMARK + 1);
        assert!(
            timeout(TICK, gate.wait_ready()).await.is_err(),
            "高低水位之间不该放行"
        );

        // 降到低水位及以下才放行
        gate.on_ack(1);
        assert_eq!(gate.unacked(), LOW_WATERMARK);
        assert!(
            timeout(TICK, gate.wait_ready())
                .await
                .expect("降到低水位以下应该放行"),
            "门没关时应该返回 true"
        );
    }

    #[tokio::test]
    async fn ack_larger_than_sent_does_not_underflow() {
        let gate = FlowGate::new();
        gate.on_sent(1024);
        gate.on_ack(usize::MAX);

        assert_eq!(gate.unacked(), 0);
        assert!(timeout(TICK, gate.wait_ready())
            .await
            .expect("没欠账就该放行"));
    }

    #[tokio::test]
    async fn close_wakes_waiter() {
        let gate = Arc::new(FlowGate::new());
        gate.on_sent(HIGH_WATERMARK + 1);

        let waiting = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { gate.wait_ready().await })
        };
        // 让等的人先跑起来
        tokio::task::yield_now().await;

        gate.close();
        let ready = timeout(Duration::from_secs(2), waiting)
            .await
            .expect("close 之后应该马上醒来")
            .expect("任务不该 panic");
        assert!(!ready, "门关了应该返回 false");
    }

    #[tokio::test]
    async fn concurrent_sent_ack_and_wait_ready_do_not_deadlock() {
        let gate = Arc::new(FlowGate::new());

        let mut tasks = Vec::new();
        for _ in 0..3 {
            let gate = Arc::clone(&gate);
            tasks.push(tokio::spawn(async move {
                for _ in 0..500 {
                    gate.on_sent(8 * 1024);
                    gate.on_ack(8 * 1024);
                }
            }));
        }
        {
            let gate = Arc::clone(&gate);
            tasks.push(tokio::spawn(async move {
                for _ in 0..500 {
                    assert!(gate.wait_ready().await, "测试里不会关门");
                }
            }));
        }

        for task in tasks {
            timeout(Duration::from_secs(5), task)
                .await
                .expect("并发调用不该死锁")
                .expect("任务不该 panic");
        }
        // 收发相抵，账应该是平的
        assert_eq!(gate.unacked(), 0);
    }
}
