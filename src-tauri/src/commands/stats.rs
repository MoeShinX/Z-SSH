//! 服务器状态采集的编排（Tauri 层，参照 Netcatty `useServerStats` 的共享采集）。
//!
//! 关键点：
//! - **每个连接一个采集任务**（不是每个会话）：同一台主机分屏出来的多个终端
//!   （duplicate 出的会话）共享同一条池连接，订阅按连接键去重，只跑一个任务。
//! - **有订阅才采集**：最后一个订阅者走了（标签全关了、状态栏被关了）就停。
//! - 采集不碰会话：读是从 [`zssh_core::ssh::Connection::exec`] 的独立通道来的，
//!   不会阻塞会话的读写和 ack（设计文档 5.2 的硬规则）。
//!
//! M1-10b：采集动作（[`StatsHost`]）、任务 spawn（[`TaskSpawner`]）和任务句柄
//! （[`TaskHandle`]）都过了可替换的接口，测试里用假实现数着调用次数验证
//! "同一连接两个订阅只跑一个任务 / 退订最后一个任务真的结束 / 断线全清 /
//! `sessions_reset` 清掉旧页面的订阅"。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, info};
use zssh_core::ssh::{ServerStats, StatsCollector};

use crate::events::APP_EVENT_STATS;

/// 默认 3 秒一次；设置里允许 1～30 秒。
pub const DEFAULT_INTERVAL_SECS: u64 = 3;
pub const MIN_INTERVAL_SECS: u64 = 1;
pub const MAX_INTERVAL_SECS: u64 = 30;

/// 采集循环的 future（spawn 的入参）。
type CollectorFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// 一次采集的 future（[`StatsHost::collect`] 的返回）。
type CollectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<ServerStats>, String>> + Send + 'a>>;

/// 采集任务的句柄：能停、能问"真的结束了没有"。
///
/// 生产实现包 `tauri::async_runtime::JoinHandle`；测试实现包 tokio 的
/// `AbortHandle`，`is_finished` 就是卡上要的"`JoinHandle` 已完成"。
pub trait TaskHandle: Send + Sync {
    fn abort(&self);
    fn is_finished(&self) -> bool;
}

/// spawn 采集任务的可替换入口（生产 = tauri 运行时；测试 = tokio::spawn + 记账）。
pub trait TaskSpawner: Send + Sync + 'static {
    fn spawn(&self, task: CollectorFuture) -> Box<dyn TaskHandle>;
}

/// 一台连接的采集来源：间隔、断线判断、采集动作、推事件。
///
/// 生产实现 [`LiveStatsHost`] 包着共享连接；测试实现是假数据 + 计数器。
pub trait StatsHost: Send + Sync + 'static {
    /// 当前希望的采集间隔（每拍重读，设置改了下一拍生效）。
    fn interval(&self) -> Duration;
    /// 连接是不是已经断了。
    fn is_closed(&self) -> bool;
    /// 采集一次（记前值算差值的状态在实现里自持）。
    fn collect(&self) -> CollectFuture<'_>;
    /// 把一个订阅者的状态推给前端。
    fn emit(&self, subscription_id: u64, stats: ServerStats);
}

/// 订阅者的信息：interval 是订阅那一刻设置里的值（仅记录用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subscription {
    /// 间隔秒数（订阅方的设置）。
    pub interval_secs: u64,
}

/// 一条连接的采集任务状态。
struct Collector {
    /// 采集循环的任务句柄（abort 掉它就停了）。
    task: Box<dyn TaskHandle>,
    /// 订阅者：订阅 ID → 设置。
    subscribers: HashMap<u64, Subscription>,
}

/// 全局的采集编排器。
pub struct StatsRegistry {
    collectors: Arc<Mutex<HashMap<String, Collector>>>,
    next_sub_id: AtomicU64,
    spawner: Arc<dyn TaskSpawner>,
}

impl Default for StatsRegistry {
    fn default() -> Self {
        Self::with_spawner(Arc::new(TauriSpawner))
    }
}

/// 推给前端的状态事件。
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatsEvent {
    /// 订阅 ID（前端用 subscription_id 回执对应 pane）。
    pub subscription_id: u64,
    pub stats: ServerStats,
}

impl StatsRegistry {
    fn with_spawner(spawner: Arc<dyn TaskSpawner>) -> Self {
        Self {
            collectors: Arc::new(Mutex::new(HashMap::new())),
            next_sub_id: AtomicU64::new(0),
            spawner,
        }
    }

    fn next_subscription_id(&self) -> u64 {
        self.next_sub_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 订阅一台连接的状态采集。
    ///
    /// 已经有任务在跑就只登记订阅者；没有就 spawn 一个循环。同一连接的多个
    /// 订阅者只共享一个任务。
    pub async fn subscribe(&self, host: Arc<dyn StatsHost>, connection_key: String) -> u64 {
        let subscription_id = self.next_subscription_id();
        let subscription = Subscription {
            interval_secs: host
                .interval()
                .as_secs()
                .clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS),
        };

        // 持着锁 spawn + 登记：采集循环第一拍也要拿这把锁，等它拿到时条目一定在了。
        let mut collectors = self.collectors.lock().await;
        // 任务已经自己结束的残条目（连接断掉时的收尾窗口）不能挂新订阅：重建。
        let reuse = matches!(
            collectors.get(&connection_key),
            Some(collector) if !collector.task.is_finished()
        );
        let created = if reuse {
            if let Some(collector) = collectors.get_mut(&connection_key) {
                collector.subscribers.insert(subscription_id, subscription);
            }
            false
        } else {
            if collectors.remove(&connection_key).is_some() {
                debug!(connection_key = %connection_key, "旧的采集任务已经结束，重建");
            }
            let mut subscribers = HashMap::new();
            subscribers.insert(subscription_id, subscription);
            let task = self.spawner.spawn(Box::pin(run_collector(
                Arc::clone(&self.collectors),
                connection_key.clone(),
                Arc::clone(&host),
            )));
            collectors.insert(connection_key.clone(), Collector { task, subscribers });
            true
        };
        drop(collectors);

        if created {
            info!(connection_key = %connection_key, %subscription_id, "开始采集服务器状态（连接上第一个订阅者）");
        } else {
            debug!(connection_key = %connection_key, %subscription_id, "共享已有的采集任务");
        }
        subscription_id
    }

    /// 取消订阅。没有订阅者了就停掉采集任务。
    pub async fn unsubscribe(&self, subscription_id: u64) -> bool {
        let mut collectors = self.collectors.lock().await;
        let Some(key) = collectors
            .iter()
            .find(|(_, collector)| collector.subscribers.contains_key(&subscription_id))
            .map(|(key, _)| key.clone())
        else {
            return false;
        };
        let empty = match collectors.get_mut(&key) {
            Some(collector) => {
                collector.subscribers.remove(&subscription_id);
                collector.subscribers.is_empty()
            }
            None => false,
        };
        if empty {
            if let Some(collector) = collectors.remove(&key) {
                // 循环自己下一拍也会发现没人了，这里直接 abort 让它马上结束。
                collector.task.abort();
                info!(connection_key = %key, "最后一个订阅者走了，停止采集");
            }
        }
        true
    }

    /// 把**所有**连接的订阅和采集任务清掉（前端换页用，M1-08c）。
    ///
    /// 旧页面的订阅 ID 已经没有前端认领了，留着它们只会白白采集。
    /// 返回停掉的采集任务数。
    pub async fn reset(&self) -> usize {
        let mut collectors = self.collectors.lock().await;
        let count = collectors.len();
        for (key, collector) in collectors.drain() {
            collector.task.abort();
            debug!(connection_key = %key, "页面重置：停止采集");
        }
        count
    }
}

/// 采集循环：固定间隔在共享连接上跑一次 exec，把结果按订阅者发出去。
async fn run_collector(
    collectors: Arc<Mutex<HashMap<String, Collector>>>,
    connection_key: String,
    host: Arc<dyn StatsHost>,
) {
    let mut current_interval = host.interval();
    let mut ticker = interval(current_interval);
    loop {
        // 间隔是设置项（1～30 秒）：改了设置的下一拍就生效，不用重启任务。
        let wanted = host.interval();
        if wanted != current_interval {
            debug!(connection_key = %connection_key, from = ?current_interval, to = ?wanted, "采集间隔变化");
            current_interval = wanted;
            ticker = interval(current_interval);
        }

        ticker.tick().await;

        // 还订阅着吗（没订阅者、或者条目被 reset 清掉，就退出）。
        let still_needed = {
            let collectors = collectors.lock().await;
            collectors
                .get(&connection_key)
                .map(|entry| !entry.subscribers.is_empty())
                .unwrap_or(false)
        };
        if !still_needed {
            debug!(connection_key = %connection_key, "没有订阅者了，采集循环退出");
            return;
        }

        if host.is_closed() {
            debug!(connection_key = %connection_key, "连接已经断了，停止采集");
            collectors.lock().await.remove(&connection_key);
            return;
        }

        let stats = match host.collect().await {
            Ok(Some(stats)) => stats,
            Ok(None) => {
                debug!(connection_key = %connection_key, "这次没采到（超时或不支持的系统），下次再试");
                continue;
            }
            Err(err) => {
                debug!(connection_key = %connection_key, %err, "采集脚本执行失败");
                continue;
            }
        };

        // 把结果发给所有订阅者（每个订阅者一个事件，subscription_id 用来认领）。
        let subscription_ids: Vec<u64> = {
            let collectors = collectors.lock().await;
            collectors
                .get(&connection_key)
                .map(|entry| entry.subscribers.keys().copied().collect())
                .unwrap_or_default()
        };
        for subscription_id in subscription_ids {
            host.emit(subscription_id, stats.clone());
        }
    }
}

/// 生产环境的 spawner：跑在 tauri 的异步运行时上。
struct TauriSpawner;

impl TaskSpawner for TauriSpawner {
    fn spawn(&self, task: CollectorFuture) -> Box<dyn TaskHandle> {
        Box::new(TauriTask(tauri::async_runtime::spawn(task)))
    }
}

struct TauriTask(tauri::async_runtime::JoinHandle<()>);

impl TaskHandle for TauriTask {
    fn abort(&self) {
        self.0.abort();
    }

    fn is_finished(&self) -> bool {
        self.0.inner().is_finished()
    }
}

/// 生产环境的采集来源：一条共享连接 + 设置里的间隔 + AppHandle 推事件。
struct LiveStatsHost {
    app: AppHandle,
    connection: Arc<zssh_core::ssh::Connection>,
    collector: Mutex<StatsCollector>,
}

impl StatsHost for LiveStatsHost {
    fn interval(&self) -> Duration {
        Duration::from_secs(read_interval_secs(
            &self.app.state::<crate::state::AppState>().store,
        ))
    }

    fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }

    fn collect(&self) -> CollectFuture<'_> {
        Box::pin(async move {
            let mut collector = self.collector.lock().await;
            collector.collect(&self.connection).await
        })
    }

    fn emit(&self, subscription_id: u64, stats: ServerStats) {
        let payload = ServerStatsEvent {
            subscription_id,
            stats,
        };
        if let Err(err) = self.app.emit(APP_EVENT_STATS, payload) {
            debug!(%subscription_id, %err, "发送服务器状态事件失败");
        }
    }
}

/// 订阅一台会话的状态采集（通过会话找到背后的共享连接）。
///
/// 同一回话 duplicate 出来的多个前端 pane 各自有一个订阅 ID，但底层只跑一个
/// 采集任务：连接键是 `Arc<Connection>` 的指针地址。
#[tauri::command]
pub async fn stats_subscribe(
    app: AppHandle,
    sessions: tauri::State<'_, crate::state::AppState>,
    registry: tauri::State<'_, StatsRegistry>,
    session_id: String,
) -> Result<u64, String> {
    let Some((connection_key, connection)) = sessions.stats_connection(&session_id).await else {
        return Err(format!("会话不存在或者还没连上：{session_id}"));
    };
    let host = Arc::new(LiveStatsHost {
        app,
        connection,
        collector: Mutex::new(StatsCollector::new()),
    });
    Ok(registry.subscribe(host, connection_key).await)
}

/// 取消订阅。
#[tauri::command]
pub async fn stats_unsubscribe(
    registry: tauri::State<'_, StatsRegistry>,
    subscription_id: u64,
) -> Result<(), String> {
    registry.unsubscribe(subscription_id).await;
    Ok(())
}

/// 状态栏设置里的间隔（1～30 秒，库里没存就是默认 3 秒）。
pub fn read_interval_secs(store: &zssh_core::Database) -> u64 {
    const KEY: &str = "stats_refresh_secs";
    let value = store
        .setting_get(KEY)
        .ok()
        .flatten()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS);
    value.clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// 假 spawner：用 tokio::spawn 真跑，同时记账 spawn 了几次、句柄留着可查。
    #[derive(Default)]
    struct TestSpawner {
        spawned: AtomicUsize,
        tasks: std::sync::Mutex<Vec<tokio::task::AbortHandle>>,
    }

    impl TestSpawner {
        fn spawned_count(&self) -> usize {
            self.spawned.load(Ordering::SeqCst)
        }

        fn task_handles(&self) -> Vec<tokio::task::AbortHandle> {
            self.tasks.lock().unwrap().clone()
        }
    }

    impl TaskSpawner for TestSpawner {
        fn spawn(&self, task: CollectorFuture) -> Box<dyn TaskHandle> {
            let handle = tokio::spawn(task);
            let abort = handle.abort_handle();
            self.spawned.fetch_add(1, Ordering::SeqCst);
            self.tasks.lock().unwrap().push(abort.clone());
            Box::new(TestTask(abort))
        }
    }

    /// 测试用的任务句柄：底下是 tokio 的 AbortHandle。
    struct TestTask(tokio::task::AbortHandle);

    impl TaskHandle for TestTask {
        fn abort(&self) {
            self.0.abort();
        }

        fn is_finished(&self) -> bool {
            self.0.is_finished()
        }
    }

    /// 假采集来源：数采集和推送的次数，断线状态可以手动翻。
    #[derive(Default)]
    struct FakeHostInner {
        collect_calls: AtomicUsize,
        emitted: AtomicUsize,
        closed: AtomicBool,
        interval: std::sync::Mutex<Duration>,
    }

    struct FakeHost {
        inner: Arc<FakeHostInner>,
    }

    impl FakeHost {
        fn new(interval: Duration) -> Arc<Self> {
            Arc::new(Self {
                inner: Arc::new(FakeHostInner {
                    interval: std::sync::Mutex::new(interval),
                    ..Default::default()
                }),
            })
        }

        fn collect_calls(&self) -> usize {
            self.inner.collect_calls.load(Ordering::SeqCst)
        }

        fn emitted_count(&self) -> usize {
            self.inner.emitted.load(Ordering::SeqCst)
        }

        fn set_closed(&self) {
            self.inner.closed.store(true, Ordering::SeqCst);
        }
    }

    impl StatsHost for FakeHost {
        fn interval(&self) -> Duration {
            *self.inner.interval.lock().unwrap()
        }

        fn is_closed(&self) -> bool {
            self.inner.closed.load(Ordering::SeqCst)
        }

        fn collect(&self) -> CollectFuture<'_> {
            self.inner.collect_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(Some(ServerStats {
                    cpu_percent: Some(7),
                    ..Default::default()
                }))
            })
        }

        fn emit(&self, _subscription_id: u64, _stats: ServerStats) {
            self.inner.emitted.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 把假的采集来源提升成 trait 对象（`Arc<FakeHost>` → `Arc<dyn StatsHost>`）。
    fn as_host(host: &Arc<FakeHost>) -> Arc<dyn StatsHost> {
        let cloned: Arc<FakeHost> = Arc::clone(host);
        cloned
    }

    /// 测试里 20ms 一拍，比生产快的多，但链路上还是真的走 tokio 定时器。
    fn fast_interval() -> Duration {
        Duration::from_millis(20)
    }

    /// 等一个任务句柄真的结束（abort / 自然退出都算）。
    async fn wait_finished(handle: &tokio::task::AbortHandle) {
        for _ in 0..300 {
            if handle.is_finished() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("任务等不到结束");
    }

    fn make_registry(spawner: &Arc<TestSpawner>) -> StatsRegistry {
        StatsRegistry::with_spawner(Arc::clone(spawner) as Arc<dyn TaskSpawner>)
    }

    #[tokio::test]
    async fn two_subscriptions_share_one_collector_task() {
        let spawner = Arc::new(TestSpawner::default());
        let registry = make_registry(&spawner);
        let host = FakeHost::new(fast_interval());

        let first = registry.subscribe(as_host(&host), "conn-1".into()).await;
        let second = registry.subscribe(as_host(&host), "conn-1".into()).await;
        assert_ne!(first, second, "两个订阅者要有各自的订阅 ID");

        tokio::time::sleep(Duration::from_millis(120)).await;

        assert_eq!(
            spawner.spawned_count(),
            1,
            "同一个连接只该 spawn 一个采集任务"
        );
        let calls = host.collect_calls();
        // 20ms 一拍、120ms ≈ 6 拍；要是跑成两个任务会接近翻倍。
        assert!(
            (3..=9).contains(&calls),
            "采集频率应该像一个任务：calls={calls}"
        );
        // 每个拍两个订阅者各一条事件。
        assert!(
            host.emitted_count() >= 6,
            "两个订阅者都该收到推送：emitted={}",
            host.emitted_count()
        );
    }

    #[tokio::test]
    async fn unsubscribing_one_keeps_collecting() {
        let spawner = Arc::new(TestSpawner::default());
        let registry = make_registry(&spawner);
        let host = FakeHost::new(fast_interval());

        let first = registry.subscribe(as_host(&host), "conn-1".into()).await;
        let second = registry.subscribe(as_host(&host), "conn-1".into()).await;
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert!(registry.unsubscribe(first).await, "第一个订阅读得到");
        let before = host.collect_calls();
        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(
            host.collect_calls() > before,
            "还有订阅者（第二个）：采集要继续"
        );
        let collectors = registry.collectors.lock().await;
        let entry = collectors.get("conn-1").expect("条目还在");
        assert_eq!(entry.subscribers.len(), 1);
        assert!(entry.subscribers.contains_key(&second));
    }

    #[tokio::test]
    async fn unsubscribing_last_stops_the_task() {
        let spawner = Arc::new(TestSpawner::default());
        let registry = make_registry(&spawner);
        let host = FakeHost::new(fast_interval());

        let only = registry.subscribe(as_host(&host), "conn-1".into()).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(host.collect_calls() > 0, "采集该跑起来了");

        assert!(registry.unsubscribe(only).await);
        let handles = spawner.task_handles();
        assert_eq!(handles.len(), 1);
        wait_finished(&handles[0]).await;

        let frozen = host.collect_calls();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(host.collect_calls(), frozen, "任务结束后不能再采集");
        assert!(registry.collectors.lock().await.is_empty(), "条目要清掉");
    }

    #[tokio::test]
    async fn closed_connection_drops_all_subscriptions() {
        let spawner = Arc::new(TestSpawner::default());
        let registry = make_registry(&spawner);
        let host = FakeHost::new(fast_interval());

        registry.subscribe(as_host(&host), "conn-1".into()).await;
        registry.subscribe(as_host(&host), "conn-1".into()).await;
        tokio::time::sleep(Duration::from_millis(60)).await;

        host.set_closed();
        let handles = spawner.task_handles();
        wait_finished(&handles[0]).await;

        assert!(
            registry.collectors.lock().await.get("conn-1").is_none(),
            "连接断了，订阅和条目都要清掉"
        );
        let frozen = host.collect_calls();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(host.collect_calls(), frozen, "断线后不能再采集");
    }

    #[tokio::test]
    async fn reset_stops_every_collector() {
        // 前端换页（sessions_reset）的等价操作：旧页面的订阅全清。
        let spawner = Arc::new(TestSpawner::default());
        let registry = make_registry(&spawner);
        let host_one = FakeHost::new(fast_interval());
        let host_two = FakeHost::new(fast_interval());

        registry
            .subscribe(as_host(&host_one), "conn-1".into())
            .await;
        registry
            .subscribe(as_host(&host_two), "conn-2".into())
            .await;
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(registry.reset().await, 2, "两个连接的任务都停了");
        for handle in spawner.task_handles() {
            wait_finished(&handle).await;
        }

        assert!(registry.collectors.lock().await.is_empty());
        let frozen = (host_one.collect_calls(), host_two.collect_calls());
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            (host_one.collect_calls(), host_two.collect_calls()),
            frozen,
            "reset 之后不能再采集"
        );
    }
}
