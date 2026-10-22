//! 连接池：同一条已认证的连接给这个主机的多个通道复用（设计文档 4.1）。
//!
//! - 键是主机 ID；快速连接不进池
//! - 池里命中就直接开新通道，不再握手和认证
//! - 引用计数：最后一个通道关闭后，空闲 [`PoolConfig::idle_timeout`] 再断开
//! - 主机配置变了（地址、端口、认证、代理、跳板机）就换一条新的：调用方每次
//!   算一个配置指纹进来，对不上就退休旧连接
//! - 连接断了自己会被剔除：`acquire` 和 `connection_of` 都不认死连接
//!
//! 池只依赖 [`Pooled`] 这个最小接口，单测能塞假的连接进去；生产路径是
//! `Connection`。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;
use tracing::debug;

use crate::error::ZsshError;
use crate::ssh::connection::{ChainHop, Connection};
use crate::ssh::interact::{BoxFuture, Interactor};

/// 池的配置。
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// 最后一个通道关闭后，连接再空转多久才断开（设计文档 4.1：60 秒）。
    ///
    /// 做成可配置是为了测试：集成测试里改成 1 秒。
    pub idle_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// 池里能放的连接。做成 trait 是为了单测能塞一个假连接进去。
pub trait Pooled: Send + Sync + 'static {
    /// 连接是不是已经断了（对端断开或者 keepalive 超时）。
    fn is_closed(&self) -> bool;
    /// 尽力关闭连接（已经断了也不能报错）。
    fn close(&self) -> BoxFuture<'_, ()>;
}

impl Pooled for Connection {
    fn is_closed(&self) -> bool {
        Connection::is_closed(self)
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let _ = Connection::disconnect(self).await;
        })
    }
}

/// 池里的一个条目：一条连接，或者一个正在建的坑。
struct Entry<C> {
    host_id: i64,
    /// 配置指纹：对不上说明主机配置改了，这个条目不能再用
    fingerprint: String,
    /// 空闲时长（毫秒），和池子共享：设置页改完对新进入空闲的连接立刻生效
    idle_millis: Arc<AtomicU64>,
    state: Mutex<EntryState<C>>,
    notify: Notify,
}

struct EntryState<C> {
    connection: Option<Arc<C>>,
    /// 正在建连接：同一台主机的其它 `acquire` 先等一等，别各建一条
    connecting: bool,
    /// 持有这个连接的会话（通道）数
    refs: usize,
    /// 引用归零后的断开时刻
    idle_deadline: Option<Instant>,
    /// 已不再复用（配置变了或者连接断了）
    retired: bool,
    /// 在建连接时等结果的其它 `acquire`。注册和检查 `connecting` 在同一把锁里，
    /// 不存在丢唤醒。
    waiters: Vec<tokio::sync::oneshot::Sender<()>>,
}

impl<C> Entry<C> {
    fn new(host_id: i64, fingerprint: String, idle_millis: Arc<AtomicU64>) -> Self {
        Self {
            host_id,
            fingerprint,
            idle_millis,
            state: Mutex::new(EntryState {
                connection: None,
                connecting: false,
                refs: 0,
                idle_deadline: None,
                retired: false,
                waiters: Vec::new(),
            }),
            notify: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, EntryState<C>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 一个引用（会话）没了。归零就开始空闲计时，回收任务会接手。
    fn release(&self) {
        {
            let mut state = self.lock();
            state.refs = state.refs.saturating_sub(1);
            if state.refs == 0 {
                // 用此刻的设置：改完空闲时长之后，接下来进入空闲的连接按新的算
                let millis = self.idle_millis.load(Ordering::SeqCst);
                state.idle_deadline = Some(Instant::now() + Duration::from_millis(millis));
            }
        }
        self.notify.notify_waiters();
    }

    /// 标记不能再复用（配置变了、连接断了、空闲超时）。
    fn retire(&self) {
        self.lock().retired = true;
        self.notify.notify_waiters();
    }

    /// 把连接拿走（关闭之前调用，保证只会被拿走一次）。
    fn take_connection(&self) -> Option<Arc<C>> {
        self.lock().connection.take()
    }

    /// 登记一个"等连接建好"的等待者（调用时持有状态锁）。
    fn register_waiter(&self, state: &mut EntryState<C>) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state.waiters.push(sender);
        receiver
    }

    /// 连接建好了（或者失败了）：把等待者都叫醒，它们自己重查状态。
    fn wake_waiters(&self) {
        let waiters = std::mem::take(&mut self.lock().waiters);
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }
}

/// 一个会话对池里一条连接的引用。
///
/// 这个守卫不实现 `Clone`：引用计数就是持有它的会话数，要多个会话共享就套
/// `Arc<ConnectionGuard>`，最后一个 `Arc` 释放时连接进入空闲计时。
pub struct ConnectionGuard<C: Pooled = Connection> {
    connection: Arc<C>,
    entry: Arc<Entry<C>>,
    /// 这条连接是不是这一次调用才新建的（复用的不算）。调用方用它决定
    /// "保存密码"之类的动作要不要做。
    fresh: bool,
}

impl<C: Pooled> ConnectionGuard<C> {
    /// 池里那条连接，拿来开新通道。
    pub fn connection(&self) -> &Arc<C> {
        &self.connection
    }

    /// 这条连接是这次新建的，还是池里复用的。
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    pub fn host_id(&self) -> i64 {
        self.entry.host_id
    }

    /// 拿这条连接时的配置指纹。
    pub fn fingerprint(&self) -> &str {
        &self.entry.fingerprint
    }
}

impl<C: Pooled> Drop for ConnectionGuard<C> {
    fn drop(&mut self) {
        self.entry.release();
    }
}

/// 连接池。克隆出来的句柄共享同一份池子。
pub struct ConnectionPool<C: Pooled = Connection> {
    inner: Arc<Inner<C>>,
}

impl<C: Pooled> Clone for ConnectionPool<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner<C> {
    /// 当前的空闲时长（毫秒）。池子里的条目都读它，所以能随时改。
    idle_millis: Arc<AtomicU64>,
    entries: Mutex<HashMap<i64, Arc<Entry<C>>>>,
    /// 测试用的同步点：在第 1 步"池里没有"和第 2 步"占位"之间调用一次。
    ///
    /// 这两步之间没有 await，单测没法从外面把两个任务的时序卡开；有这个钩子
    /// 才能稳定复现"两个任务抢着建连接"的时序。生产路径上永远是 `None`。
    #[cfg(test)]
    hook_before_create: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl<C: Pooled> ConnectionPool<C> {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                idle_millis: Arc::new(AtomicU64::new(
                    config.idle_timeout.as_millis().min(u128::from(u64::MAX)) as u64,
                )),
                entries: Mutex::new(HashMap::new()),
                #[cfg(test)]
                hook_before_create: Mutex::new(None),
            }),
        }
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<i64, Arc<Entry<C>>>> {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 池里这条主机现在还能用的连接，没有（或者已经断了）就是 `None`。
    pub fn connection_of(&self, host_id: i64) -> Option<Arc<C>> {
        let entry = self.entries().get(&host_id).cloned()?;
        let state = entry.lock();
        let connection = state.connection.clone();
        drop(state);
        connection.filter(|connection| !connection.is_closed())
    }

    /// 改空闲时长（设置页用）。已经在空闲计时里的连接不动，新进入空闲的按新的算。
    pub fn set_idle_timeout(&self, timeout: Duration) {
        let millis = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        self.inner.idle_millis.store(millis, Ordering::SeqCst);
    }

    /// 池里现在有哪些主机（排查和测试用）。
    pub fn pooled_hosts(&self) -> Vec<i64> {
        self.entries().keys().copied().collect()
    }

    /// 拿一条可用连接。池里命中就加一个引用直接给；否则用 `connect` 建一条新的。
    ///
    /// `fingerprint` 是调用方算的主机配置指纹：和池里那条对不上就换新的。
    pub async fn acquire_with<F, Fut>(
        &self,
        host_id: i64,
        fingerprint: &str,
        connect: F,
    ) -> Result<Arc<ConnectionGuard<C>>, ZsshError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<C, ZsshError>>,
    {
        let mut connect = Some(connect);

        loop {
            // 1. 先在池里找。命中复用；正在建就等；死了或者配置变了就退休、新建。
            let waiting: Option<tokio::sync::oneshot::Receiver<()>> = {
                let mut entries = self.entries();
                match entries.get(&host_id).cloned() {
                    // 配置指纹对不上：退休旧的，下面新建
                    Some(entry) if entry.fingerprint != fingerprint => {
                        entries.remove(&host_id);
                        entry.retire();
                        None
                    }
                    Some(entry) => {
                        let mut state = entry.lock();
                        match state.connection.clone() {
                            Some(connection) if !connection.is_closed() => {
                                state.refs += 1;
                                state.idle_deadline = None;
                                return Ok(Arc::new(ConnectionGuard {
                                    connection,
                                    entry: Arc::clone(&entry),
                                    fresh: false,
                                }));
                            }
                            // 别人正在建：登记等下（注册和检查在同一把锁里）
                            _ if state.connecting => Some(entry.register_waiter(&mut state)),
                            _ => {
                                // 连接死了：退休，下面新建
                                entries.remove(&host_id);
                                state.retired = true;
                                None
                            }
                        }
                    }
                    None => None,
                }
            };

            if let Some(waiting) = waiting {
                debug!(host_id, "等别的任务把连接建好");
                let _ = waiting.await;
                continue;
            }

            // 测试用的同步点（生产是 None）：让单测能把"没找到"和"占位"卡开。
            #[cfg(test)]
            {
                let hook = self
                    .inner
                    .hook_before_create
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                if let Some(hook) = hook {
                    hook();
                }
            }

            // 2. 新建：占位和"再查一次"在同一把锁里完成，同一时刻只有一个任务能占到。
            //
            //    池里已经有条目（多数是刚才有人抢先建好了、或者正在建）就回到循环
            //    开头，让第 1 步去处理：复用它的连接、或者登记成等待者。**绝不能
            //    覆盖别人的占位**——覆盖会让两个任务各建一条连接、各认证一次
            //    （用户快速双击同一台主机就是两个密码弹窗）。
            let entry = {
                let mut entries = self.entries();
                // 查和占位在同一把锁里：两个任务不可能同时占到位。空位才插，
                // 有人抢先（建好了 / 正在建 / 换了指纹）就回到循环开头，让第 1 步
                // 去处理——绝不覆盖别人的占位（覆盖会建出两条连接、认证两次）。
                match entries.entry(host_id) {
                    std::collections::hash_map::Entry::Occupied(_) => None,
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        let entry = Arc::new(Entry::new(
                            host_id,
                            fingerprint.to_owned(),
                            Arc::clone(&self.inner.idle_millis),
                        ));
                        entry.lock().connecting = true;
                        slot.insert(Arc::clone(&entry));
                        Some(entry)
                    }
                }
            };
            let Some(entry) = entry else {
                continue;
            };

            // 只有走到新建这一步才会用到连接工厂，所以它只建一次。
            let Some(connect) = connect.take() else {
                return Err(ZsshError::Internal("连接池一次只该建一次连接".to_owned()));
            };
            // 占位已经插进去了：如果这个 future 在中途被 select 掉（用户取消连接），
            // Drop 会把占位摘掉并叫醒等待者，不留下一个永远的坑。
            let mut placeholder = Placeholder {
                pool: self,
                entry: Arc::clone(&entry),
                armed: true,
            };
            match connect().await {
                Ok(connection) => {
                    let connection = Arc::new(connection);
                    {
                        let mut state = entry.lock();
                        state.connection = Some(Arc::clone(&connection));
                        state.connecting = false;
                        state.refs = 1;
                        state.idle_deadline = None;
                    }
                    placeholder.disarm();
                    entry.wake_waiters();
                    // 回收任务：引用归零后负责空闲超时断开
                    tokio::spawn(reap(Arc::downgrade(&self.inner), Arc::clone(&entry)));
                    debug!(host_id, "池里新增一条连接");
                    return Ok(Arc::new(ConnectionGuard {
                        connection,
                        entry,
                        fresh: true,
                    }));
                }
                // 失败和取消都走同一条清理：Drop 摘占位、叫醒等待者
                Err(err) => return Err(err),
            }
        }
    }

    /// 一条连接的最后引用没了之后：等空闲时长，然后断开并从池里摘掉。
    async fn reap_entry(&self, entry: Arc<Entry<C>>) {
        loop {
            // 先看现在该做什么。等通知的话，"注册 + 检查状态"必须在同一把锁里
            // 完成，否则会丢掉唤醒；锁在块结束时释放，await 在块外。
            enum Step {
                Done,
                Sleep(Instant),
                Idle,
            }
            let (step, mut notified) = {
                let state = entry.lock();
                if state.refs == 0 && state.connection.is_none() {
                    (Step::Done, None)
                } else {
                    match state.idle_deadline {
                        Some(at) if state.refs == 0 => (Step::Sleep(at), None),
                        _ => {
                            let mut notified = Box::pin(entry.notify.notified());
                            notified.as_mut().enable();
                            (Step::Idle, Some(notified))
                        }
                    }
                }
            };

            let deadline = match step {
                Step::Done => return,
                Step::Sleep(deadline) => deadline,
                Step::Idle => {
                    if let Some(notified) = notified.as_mut() {
                        notified.as_mut().await;
                    }
                    continue;
                }
            };

            tokio::time::sleep_until(deadline).await;

            let close_now = {
                let mut state = entry.lock();
                if state.refs > 0 {
                    // 空闲期间又有人用了：不算数了，重新等
                    state.idle_deadline = None;
                    false
                } else if state.idle_deadline == Some(deadline) {
                    state.retired = true;
                    state.idle_deadline = None;
                    true
                } else {
                    false
                }
            };
            if !close_now {
                continue;
            }

            {
                let mut entries = self.entries();
                if let Some(mapped) = entries.get(&entry.host_id) {
                    if Arc::ptr_eq(mapped, &entry) {
                        entries.remove(&entry.host_id);
                    }
                }
            }

            if let Some(connection) = entry.take_connection() {
                debug!(host_id = entry.host_id, "空闲超时，断开池里的连接");
                connection.close().await;
            }
            return;
        }
    }
}

/// 回收任务的入口：拿着弱引用，池子没了就自己退出。
async fn reap<C: Pooled>(inner: Weak<Inner<C>>, entry: Arc<Entry<C>>) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let pool = ConnectionPool { inner };
    pool.reap_entry(entry).await;
}

/// `acquire` 建连接期间的占位清理（见 `acquire_with` 里的注释）。
struct Placeholder<'a, C: Pooled> {
    pool: &'a ConnectionPool<C>,
    entry: Arc<Entry<C>>,
    armed: bool,
}

impl<C: Pooled> Placeholder<'_, C> {
    /// 连接建成了：不用清理了。
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<C: Pooled> Drop for Placeholder<'_, C> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        {
            let mut state = self.entry.lock();
            state.connecting = false;
            state.retired = true;
        }
        {
            let mut entries = self.pool.entries();
            if let Some(mapped) = entries.get(&self.entry.host_id) {
                if Arc::ptr_eq(mapped, &self.entry) {
                    entries.remove(&self.entry.host_id);
                }
            }
        }
        self.entry.wake_waiters();
        debug!(host_id = self.entry.host_id, "acquire 被取消，占位已清理");
    }
}

impl ConnectionPool<Connection> {
    /// 生产路径：池里没有就用 `hops` 建一条新的（直连或者拨号链）。
    pub async fn acquire(
        &self,
        host_id: i64,
        fingerprint: &str,
        hops: Vec<ChainHop>,
        interactor: Arc<dyn Interactor>,
    ) -> Result<Arc<ConnectionGuard>, ZsshError> {
        self.acquire_with(host_id, fingerprint, || {
            Connection::connect_chain(hops, interactor)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// 假连接：能手动置成"断了"，共享一份状态方便测试从外面看。
    #[derive(Clone)]
    struct FakeConnection {
        state: Arc<FakeState>,
    }

    struct FakeState {
        closed: AtomicBool,
        closes: Arc<AtomicUsize>,
    }

    impl FakeConnection {
        fn cut(&self) {
            self.state.closed.store(true, Ordering::SeqCst);
        }
    }

    impl Pooled for FakeConnection {
        fn is_closed(&self) -> bool {
            self.state.closed.load(Ordering::SeqCst)
        }

        fn close(&self) -> BoxFuture<'_, ()> {
            let closes = Arc::clone(&self.state.closes);
            Box::pin(async move {
                closes.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    /// 可数的连接工厂。
    #[derive(Clone)]
    struct Factory {
        connects: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
        made: Arc<Mutex<Vec<FakeConnection>>>,
    }

    impl Factory {
        fn new() -> Self {
            Self {
                connects: Arc::new(AtomicUsize::new(0)),
                closes: Arc::new(AtomicUsize::new(0)),
                made: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn make(&self) -> FakeConnection {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let connection = FakeConnection {
                state: Arc::new(FakeState {
                    closed: AtomicBool::new(false),
                    closes: Arc::clone(&self.closes),
                }),
            };
            self.made.lock().unwrap().push(connection.clone());
            connection
        }

        fn count(&self) -> usize {
            self.connects.load(Ordering::SeqCst)
        }

        fn close_count(&self) -> usize {
            self.closes.load(Ordering::SeqCst)
        }

        /// 最近一条建出来的假连接。
        fn last(&self) -> FakeConnection {
            self.made.lock().unwrap().last().unwrap().clone()
        }
    }

    async fn acquire(
        pool: &ConnectionPool<FakeConnection>,
        factory: &Factory,
        host_id: i64,
        fingerprint: &str,
    ) -> Arc<ConnectionGuard<FakeConnection>> {
        let factory = factory.clone();
        pool.acquire_with(
            host_id,
            fingerprint,
            move || async move { Ok(factory.make()) },
        )
        .await
        .expect("建连接不该失败")
    }

    #[tokio::test]
    async fn second_acquire_reuses_the_same_connection() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        let first = acquire(&pool, &factory, 7, "v1").await;
        let second = acquire(&pool, &factory, 7, "v1").await;

        assert_eq!(factory.count(), 1, "第二次不该重新建连接");
        assert!(
            Arc::ptr_eq(first.connection(), second.connection()),
            "两次拿到的应该是同一条连接"
        );
        assert!(first.is_fresh(), "第一条是新建的");
        assert!(!second.is_fresh(), "第二条是复用的");
    }

    #[tokio::test]
    async fn concurrent_acquires_build_only_one_connection() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        let (a, b, c) = tokio::join!(
            acquire(&pool, &factory, 1, "v1"),
            acquire(&pool, &factory, 1, "v1"),
            acquire(&pool, &factory, 1, "v1"),
        );

        assert_eq!(factory.count(), 1, "并发也只该建一条");
        assert!(Arc::ptr_eq(a.connection(), b.connection()));
        assert!(Arc::ptr_eq(b.connection(), c.connection()));
    }

    #[tokio::test]
    async fn a_changed_fingerprint_replaces_the_connection() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        let old = acquire(&pool, &factory, 3, "v1").await;
        let new = acquire(&pool, &factory, 3, "v2").await;

        assert_eq!(factory.count(), 2, "配置变了要新建");
        assert!(!Arc::ptr_eq(old.connection(), new.connection()));
        assert!(
            pool.connection_of(3).map(|c| !c.is_closed()) == Some(true),
            "池里只留新的那条"
        );
        assert!(!old.connection().is_closed(), "旧连接还有人用，不能马上断");
    }

    #[tokio::test]
    async fn the_old_connection_is_closed_once_nobody_uses_it() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig {
            idle_timeout: Duration::from_millis(50),
        });

        let old = acquire(&pool, &factory, 3, "v1").await;
        let _new = acquire(&pool, &factory, 3, "v2").await;
        assert_eq!(factory.close_count(), 0);

        drop(old);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(factory.close_count(), 1, "退休的连接没人用了才断");
    }

    #[tokio::test]
    async fn idle_connection_is_closed_after_the_timeout() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig {
            idle_timeout: Duration::from_millis(50),
        });

        let guard = acquire(&pool, &factory, 5, "v1").await;
        drop(guard);

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(factory.close_count(), 1, "空闲超时该断开");
        assert!(pool.connection_of(5).is_none(), "断开的连接要从池里摘掉");
        assert!(pool.pooled_hosts().is_empty(), "条目也要摘掉");
        assert!(factory.last().is_closed() || factory.close_count() == 1);
    }

    #[tokio::test]
    async fn acquiring_during_idle_reuses_the_connection() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig {
            idle_timeout: Duration::from_millis(300),
        });

        let guard = acquire(&pool, &factory, 5, "v1").await;
        drop(guard);
        // 空闲期间再打开：应该复用，不是新建
        tokio::time::sleep(Duration::from_millis(20)).await;
        let guard = acquire(&pool, &factory, 5, "v1").await;

        assert_eq!(factory.count(), 1, "空闲期间打开要复用");
        assert_eq!(factory.close_count(), 0, "不该断开");

        // 超过空闲时长再看：还有人用着，不能断
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(factory.close_count(), 0, "有人用着的连接不能断");
        drop(guard);
    }

    #[tokio::test]
    async fn a_dead_connection_is_not_reused() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        let first = acquire(&pool, &factory, 9, "v1").await;
        first.connection().cut();
        drop(first);

        let second = acquire(&pool, &factory, 9, "v1").await;
        assert_eq!(factory.count(), 2, "死连接不能复用，要新建");
        assert!(!second.connection().is_closed());
        assert!(pool.connection_of(9).is_some());
    }

    #[tokio::test]
    async fn a_connect_failure_does_not_leave_a_hole() {
        let pool: ConnectionPool<FakeConnection> = ConnectionPool::new(PoolConfig::default());

        let result = pool
            .acquire_with(11, "v1", || async {
                Err(ZsshError::InvalidInput("装出来的失败".to_owned()))
            })
            .await;
        assert!(result.is_err());
        assert!(pool.pooled_hosts().is_empty(), "失败的占位要摘掉");

        // 失败之后换个成功的工厂，还是能建起来
        let factory = Factory::new();
        let guard = acquire(&pool, &factory, 11, "v1").await;
        assert_eq!(factory.count(), 1);
        drop(guard);
    }

    /// 复现并盯住 M1-06 验收发现的时序：两个任务抢着建同一台主机的连接。
    ///
    /// 时序（靠 `hook_before_create` 卡住，不靠线程调度碰运气）：
    ///
    /// 1. 任务 2 先跑：第 1 步没找到条目，进钩子挂住
    /// 2. 任务 1 开始建连接，插好占位（它的 connector 跑起来了）
    /// 3. 放行任务 2：它现在才走到第 2 步
    ///
    /// 修复前，任务 2 会用自己的占位覆盖任务 1 的，最后建出两条连接、认证两次
    /// （用户双击快一点就是两个密码弹窗）。修复后任务 2 会登记成等待者。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_task_never_overwrites_someone_elses_placeholder() {
        /// 两个任务抢的同一台主机。
        const RACE_HOST: i64 = 4;

        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        // 任务 2 的钩子里先报"我到了"（第 1 步没找到条目），再等任务 1 的占位插好
        let (hooked_tx, hooked_rx) = std::sync::mpsc::channel::<()>();
        let (release_hook_tx, release_hook_rx) = std::sync::mpsc::channel::<()>();
        let release_hook_rx = Arc::new(Mutex::new(release_hook_rx));
        // 只有第一个走到这里的任务要挂住（那是任务 2）；任务 1 自己也会经过这个
        // 钩子，它得直接过去。
        let first_caller = Arc::new(AtomicBool::new(true));
        {
            let mut hook = pool
                .inner
                .hook_before_create
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *hook = Some(Arc::new({
                let release_hook_rx = Arc::clone(&release_hook_rx);
                let first_caller = Arc::clone(&first_caller);
                move || {
                    let _ = hooked_tx.send(());
                    if !first_caller.swap(false, Ordering::SeqCst) {
                        return;
                    }
                    // 挂到任务 1 开始建连接（占位已插入）为止
                    let _ = release_hook_rx
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .recv_timeout(Duration::from_secs(5));
                }
            }));
        }

        // 任务 1 的 connector 一跑就放行钩子里的任务 2（这时占位已经插好了），
        // 然后自己也挂住：保证任务 2 走到第 2 步时任务 1 的连接还没建好。
        let (release_1_tx, release_1_rx) = tokio::sync::oneshot::channel::<()>();

        // 任务 2：先跑，会挂进钩子里
        let second = {
            let pool = pool.clone();
            let factory = factory.clone();
            tokio::spawn(async move {
                pool.acquire_with(RACE_HOST, "v1", move || async move { Ok(factory.make()) })
                    .await
            })
        };
        // 等任务 2 真的进了钩子（第 1 步没找到条目）
        let hooked = tokio::task::spawn_blocking({
            let hooked_rx = Arc::new(Mutex::new(hooked_rx));
            move || {
                hooked_rx
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv_timeout(Duration::from_secs(5))
                    .is_ok()
            }
        })
        .await
        .expect("等钩子的阻塞任务");
        assert!(hooked, "任务 2 没走到第 1 步和第 2 步之间的钩子");

        // 任务 1：现在开始建连接，占位插好之后 connector 会放行任务 2
        let first = {
            let pool = pool.clone();
            let factory = factory.clone();
            tokio::spawn(async move {
                pool.acquire_with(RACE_HOST, "v1", move || async move {
                    let _ = release_hook_tx.send(());
                    let _ = release_1_rx.await;
                    Ok(factory.make())
                })
                .await
            })
        };

        // 等任务 2 对第 2 步做出反应：修复后会登记成等待者（waiters 非空），
        // 修复前会自己去建连接（任务 1 还卡着，此刻建出来的只可能是任务 2 的）。
        // 两个状态必居其一，所以这里是确定的，不是碰运气。
        let mut task2_past_step_2 = false;
        let past_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < past_deadline {
            let registered = {
                let entries = pool.entries();
                match entries.get(&RACE_HOST) {
                    Some(entry) => !entry.lock().waiters.is_empty(),
                    None => false,
                }
            };
            if registered || factory.count() >= 1 {
                task2_past_step_2 = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(task2_past_step_2, "任务 2 没有走过第 2 步");

        // 放行任务 1；两个任务都收回来
        let _ = release_1_tx.send(());
        let first = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .expect("任务 1 应该很快结束")
            .expect("任务 1 不该 panic")
            .expect("任务 1 不该失败");
        let second = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("任务 2 应该很快结束")
            .expect("任务 2 不该 panic")
            .expect("任务 2 不该失败");

        assert_eq!(
            factory.count(),
            1,
            "两个任务抢着建连接，只该建一条（修复前这里会是 2）"
        );
        assert!(
            Arc::ptr_eq(first.connection(), second.connection()),
            "两个任务拿到的是同一条连接"
        );
    }

    #[tokio::test]
    async fn different_hosts_do_not_share_a_connection() {
        let factory = Factory::new();
        let pool = ConnectionPool::new(PoolConfig::default());

        let a = acquire(&pool, &factory, 1, "v1").await;
        let b = acquire(&pool, &factory, 2, "v1").await;

        assert_eq!(factory.count(), 2);
        assert!(!Arc::ptr_eq(a.connection(), b.connection()));
        assert_eq!(a.host_id(), 1);
        assert_eq!(b.host_id(), 2);
    }
}
