//! 配置同步的运行时：串行化同步任务、持有会话里的主密码、自动同步的调度，以及
//! 把结果推给界面。业务逻辑全在 `zssh-core::sync`，这里只管“什么时候跑、结果放哪”。
//!
//! 同步、恢复、改主密码都放进阻塞线程（`spawn_blocking`）：里面有 Argon2id（约
//! 0.35 秒）、DPAPI 和 SQLite 事务，直接放在异步任务里会卡住界面的命令响应。
//! 网络请求仍由运行时驱动（阻塞线程里用 `Handle::block_on`）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tracing::{info, warn};
use zeroize::Zeroizing;
use zssh_core::sync::engine::{SyncReport, SyncStatus};
use zssh_core::sync::guard::{DeletedSample, DeletionDecision, DeletionPlan};
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::service::{
    self, AutoSettings, ConcurrencyRecord, ConfigView, LastResult, SyncContext,
};
use zssh_core::sync::SyncError;

use crate::state::AppState;

pub const STATUS_EVENT: &str = "sync://status";
/// 配置变化后等多久才自动同步（每次新的变化都会重新计时）。
pub const DEBOUNCE: Duration = Duration::from_secs(30);
/// 应用启动后等多久做启动时的那次自动同步（让界面先出来）。
const STARTUP_DELAY: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_secs(2);

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// 自动同步停下来等用户确认的那次“会删掉很多”。
#[derive(Debug, Clone, Serialize)]
pub struct PendingConfirmation {
    pub at: i64,
    pub auto: bool,
    pub summary: String,
    pub local_records: usize,
    pub deleted_records: usize,
    pub local_hosts: usize,
    pub deleted_hosts: usize,
    pub samples: Vec<DeletedSample>,
}

impl PendingConfirmation {
    fn new(plan: &DeletionPlan, auto: bool) -> Self {
        Self {
            at: now_secs(),
            auto,
            summary: plan.summary(),
            local_records: plan.local_records,
            deleted_records: plan.deleted_records,
            local_hosts: plan.local_hosts,
            deleted_hosts: plan.deleted_hosts,
            samples: plan.samples.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurrencyView {
    pub protected: bool,
    pub notes: Vec<String>,
    pub checked_at: i64,
}

/// 同步页和状态栏用的整体状态。
#[derive(Debug, Clone, Serialize)]
pub struct StatusView {
    pub configured: bool,
    pub config: Option<ConfigView>,
    /// 没测试过连接就是 `None`——界面要提示“还没检测并发保护”，不能当成支持。
    pub concurrency: Option<ConcurrencyView>,
    /// 这台电脑已经用主密码和云端对上过。
    pub ready: bool,
    pub password_in_memory: bool,
    pub password_remembered: bool,
    pub running: bool,
    pub last: Option<LastResult>,
    pub auto: AutoSettings,
    pub pending: Option<PendingConfirmation>,
    pub baseline_broken: bool,
    pub conflict_count: usize,
    /// 本地自动备份最多保留几份。
    pub backup_keep: usize,
    /// 凭据被存储服务拒绝后暂停了自动同步（里面是服务给的原因）；改了存储配置或手动
    /// 同步之后恢复。
    pub auth_paused: Option<String>,
    /// 这个版本编进了 OneDrive 的 client ID（没有的话“登录”要用户在“高级”里填自己的）。
    pub onedrive_builtin_client: bool,
    /// 正在浏览器里登录 OneDrive。
    pub onedrive_logging_in: bool,
}

/// 一次手动同步的结果（界面据此弹对话框或显示提示）。
#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    pub status: String,
    pub summary: String,
    pub conflicts: usize,
    pub nulled_references: Vec<String>,
    pub pending: Option<PendingConfirmation>,
    pub status_view: StatusView,
}

struct Inner {
    data_dir: PathBuf,
    run_lock: tokio::sync::Mutex<()>,
    running: AtomicBool,
    password: Mutex<Option<Zeroizing<String>>>,
    pending: Mutex<Option<PendingConfirmation>>,
    baseline_broken: AtomicBool,
    /// 最近一次“同步数据变了”的时间；`None` = 没有待同步的变化。
    dirty_since: Mutex<Option<Instant>>,
    last_run: Mutex<Instant>,
    startup_due: AtomicBool,
    /// 存储服务限流（GitHub 会）：到这个时刻之前不再发请求（手动和自动都不发），和服务名。
    rate_limit: Mutex<Option<(Instant, String)>>,
    /// 认证失败（凭据被拒）：换了配置才可能好，自动同步暂停到用户改配置或手动同步。
    auth_paused: Mutex<Option<String>>,
    /// 正在进行的 OneDrive 浏览器登录：里面是“取消”的信号（同一时刻只允许一个）。
    onedrive_login: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

#[derive(Clone)]
pub struct SyncController {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SyncController {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                data_dir,
                run_lock: tokio::sync::Mutex::new(()),
                running: AtomicBool::new(false),
                password: Mutex::new(None),
                pending: Mutex::new(None),
                baseline_broken: AtomicBool::new(false),
                dirty_since: Mutex::new(None),
                last_run: Mutex::new(Instant::now()),
                startup_due: AtomicBool::new(true),
                rate_limit: Mutex::new(None),
                auth_paused: Mutex::new(None),
                onedrive_login: Mutex::new(None),
            }),
        }
    }

    /// 数据库登记的回调：用户改了会进同步包的数据。
    pub fn notify_change(&self) {
        *lock(&self.inner.dirty_since) = Some(Instant::now());
    }

    /// 还在限流等待期内的话，返回（服务名，还要等多少秒）。过期的顺手清掉。
    pub fn rate_limit_remaining(&self) -> Option<(String, u64)> {
        let mut guard = lock(&self.inner.rate_limit);
        match guard.as_ref() {
            Some((until, service)) => {
                let now = Instant::now();
                if *until > now {
                    Some((service.clone(), (*until - now).as_secs().max(1)))
                } else {
                    *guard = None;
                    None
                }
            }
            None => None,
        }
    }

    /// 换了存储 / 令牌之后，之前的限流等待和认证失败的暂停都不再适用。
    pub fn clear_rate_limit(&self) {
        *lock(&self.inner.rate_limit) = None;
        *lock(&self.inner.auth_paused) = None;
    }

    pub fn auth_paused(&self) -> Option<String> {
        lock(&self.inner.auth_paused).clone()
    }

    /// 开始一次 OneDrive 登录：同一时刻只允许一个。返回“取消”信号。
    pub fn begin_onedrive_login(&self) -> Result<Arc<tokio::sync::Notify>, String> {
        let mut guard = lock(&self.inner.onedrive_login);
        if guard.is_some() {
            return Err("正在浏览器中登录，请先完成或取消".to_owned());
        }
        let cancel = Arc::new(tokio::sync::Notify::new());
        *guard = Some(cancel.clone());
        Ok(cancel)
    }

    pub fn end_onedrive_login(&self) {
        *lock(&self.inner.onedrive_login) = None;
    }

    /// 界面上的“取消”：通知正在等浏览器的登录停下来（关闭回环监听）。没有登录在进行就什么也不做。
    pub fn cancel_onedrive_login(&self) {
        if let Some(cancel) = lock(&self.inner.onedrive_login).as_ref() {
            cancel.notify_one();
        }
    }

    pub fn onedrive_logging_in(&self) -> bool {
        lock(&self.inner.onedrive_login).is_some()
    }

    pub fn has_password(&self) -> bool {
        lock(&self.inner.password).is_some()
    }

    pub fn set_password(&self, password: Option<String>) {
        *lock(&self.inner.password) = password.map(Zeroizing::new);
    }

    /// “记住主密码”要保存的内容（只给命令层用，不进日志）。
    pub fn password_for_remember(&self) -> Option<Zeroizing<String>> {
        self.password()
    }

    /// 防误删确认时用户选了“取消”：本地和云端都没动过，丢掉待确认状态就行。
    pub fn cancel_pending(&self, state: &AppState) -> RunView {
        self.set_pending(None);
        let last = LastResult {
            at: now_secs(),
            status: "cancelled".to_owned(),
            summary: "已取消这次同步，本地和云端都没有改动".to_owned(),
            version: None,
            conflicts: 0,
            auto: false,
        };
        if let Err(err) = service::save_last_result(&state.store, &last) {
            warn!(%err, "保存同步结果失败");
        }
        RunView {
            status: last.status,
            summary: last.summary,
            conflicts: 0,
            nulled_references: Vec::new(),
            pending: None,
            status_view: self.status_view(state),
        }
    }

    fn password(&self) -> Option<Zeroizing<String>> {
        lock(&self.inner.password).clone()
    }

    pub fn pending(&self) -> Option<PendingConfirmation> {
        lock(&self.inner.pending).clone()
    }

    fn set_pending(&self, pending: Option<PendingConfirmation>) {
        *lock(&self.inner.pending) = pending;
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }

    /// 启动时：把“记住”的主密码读进内存，检查基线是不是坏了，然后起调度任务。
    pub fn start(&self, app: &AppHandle) {
        let state = app.state::<AppState>();
        match service::load_master_password(&state.store, &DpapiSealer) {
            Ok(Some(password)) => self.set_password(Some(password.to_string())),
            Ok(None) => {}
            Err(err) => warn!(%err, "读取本机保存的主密码失败（需要重新输入）"),
        }
        self.refresh_baseline_flag(&state);

        let controller = self.clone();
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            controller.scheduler(handle).await;
        });
    }

    /// 重新检查基线文件能不能读（坏了要提供“重建基线”）。
    pub fn refresh_baseline_flag(&self, state: &AppState) {
        let broken = match build_context(state, &self.inner.data_dir) {
            Ok(ctx) => ctx.engine().load_baseline().is_err(),
            Err(_) => false,
        };
        self.inner.baseline_broken.store(broken, Ordering::SeqCst);
    }

    pub fn baseline_broken(&self) -> bool {
        self.inner.baseline_broken.load(Ordering::SeqCst)
    }

    pub fn status_view(&self, state: &AppState) -> StatusView {
        let config = service::load_config(&state.store, &DpapiSealer)
            .ok()
            .flatten();
        let concurrency = service::load_concurrency(&state.store)
            .ok()
            .flatten()
            .map(|record| ConcurrencyView {
                protected: record.mode.is_protected(),
                notes: record.notes,
                checked_at: record.checked_at,
            })
            .or_else(|| {
                // GitHub 的 Gist 接口天生不支持条件写入，用不着先测试连接才知道：
                // 直接如实告诉用户，不能显示成“还没检测”。
                config
                    .as_ref()
                    .filter(|config| config.never_protected())
                    .map(|_| ConcurrencyView {
                        protected: false,
                        notes: vec![
                            "GitHub Gist 的修改接口不支持 If-Match 这类条件写入，只能写之前再读一次、比较版本号"
                                .to_owned(),
                        ],
                        checked_at: 0,
                    })
            });
        let conflict_count = build_context(state, &self.inner.data_dir)
            .ok()
            .and_then(|ctx| ctx.engine().list_conflicts().ok())
            .map(|list| list.len())
            .unwrap_or_default();
        StatusView {
            configured: config.is_some(),
            config: config.map(|config| config.view()),
            concurrency,
            ready: service::is_ready(&state.store).unwrap_or(false),
            password_in_memory: self.has_password(),
            password_remembered: matches!(
                state.store.setting_get(service::MASTER_PASSWORD_KEY),
                Ok(Some(_))
            ),
            running: self.is_running(),
            last: service::load_last_result(&state.store).ok().flatten(),
            auto: service::load_auto(&state.store).unwrap_or_default(),
            pending: self.pending(),
            baseline_broken: self.baseline_broken(),
            conflict_count,
            backup_keep: state.store.sync_backup_keep().unwrap_or_default(),
            auth_paused: self.auth_paused(),
            onedrive_builtin_client: zssh_core::sync::onedrive::config::builtin_client_id()
                .is_some(),
            onedrive_logging_in: self.onedrive_logging_in(),
        }
    }

    pub fn emit_status(&self, app: &AppHandle) {
        let state = app.state::<AppState>();
        let view = self.status_view(&state);
        if let Err(err) = app.emit(STATUS_EVENT, view) {
            warn!(%err, "推送同步状态失败");
        }
    }

    /// 取得“同步任务”的独占权。拿不到不排队：自动任务直接放弃，手动任务提示用户。
    fn acquire(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        self.inner
            .run_lock
            .try_lock()
            .map_err(|_| "正在同步中，请稍候".to_owned())
    }

    /// 在阻塞线程里按保存的设置同步一次，并把结果存起来、推给界面。
    pub async fn run_sync(
        &self,
        app: &AppHandle,
        auto: bool,
        decision: Option<DeletionDecision>,
    ) -> Result<RunView, String> {
        let _guard = self.acquire()?;
        let Some(password) = self.password() else {
            return Err("需要先输入主密码".to_owned());
        };
        // 还在限流等待期内：不去碰服务（再试一次只会让限流更久），直接告诉用户还要等多久。
        if let Some((service, retry_after_secs)) = self.rate_limit_remaining() {
            return Err(SyncError::RateLimited {
                service,
                retry_after_secs,
            }
            .to_string());
        }
        if !auto {
            // 用户亲自点了“立即同步”：再试一次；还是被拒就会重新暂停。
            *lock(&self.inner.auth_paused) = None;
        }
        self.inner.running.store(true, Ordering::SeqCst);
        self.emit_status(app);

        let handle = tokio::runtime::Handle::current();
        let job_app = app.clone();
        let data_dir = self.inner.data_dir.clone();
        let job_password = password.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let state = job_app.state::<AppState>();
            let ctx = build_context(&state, &data_dir)?;
            handle.block_on(ctx.run_sync(&job_password, decision))
        })
        .await;
        let result = match joined {
            Ok(result) => result,
            Err(err) => Err(SyncError::Internal(format!("同步线程异常结束：{err}"))),
        };

        self.inner.running.store(false, Ordering::SeqCst);
        *lock(&self.inner.last_run) = Instant::now();
        let state = app.state::<AppState>();
        let view = self.finish(&state, result, auto, decision);
        // 合并可能删掉了主机（连带它的转发规则）：跑着的转发跟着停（复审 F2）
        crate::commands::forward::reconcile(app);
        self.emit_status(app);
        view
    }

    /// 把引擎的结果变成“上次结果”、待确认状态和给界面的返回值。
    fn finish(
        &self,
        state: &AppState,
        result: Result<SyncReport, SyncError>,
        auto: bool,
        decision: Option<DeletionDecision>,
    ) -> Result<RunView, String> {
        let now = now_secs();
        let mut last = LastResult {
            at: now,
            status: "completed".to_owned(),
            summary: String::new(),
            version: None,
            conflicts: 0,
            auto,
        };
        let outcome = match result {
            Ok(report) => {
                last.summary = report.summary.clone();
                last.version = report.cloud_version.clone();
                last.conflicts = report.conflicts.len();
                let pending = match report.status {
                    SyncStatus::Completed => {
                        last.status = "completed".to_owned();
                        self.set_pending(None);
                        None
                    }
                    SyncStatus::NeedsConfirmation => {
                        last.status = "needs_confirmation".to_owned();
                        let pending = PendingConfirmation::new(&report.deletion, auto);
                        self.set_pending(Some(pending.clone()));
                        Some(pending)
                    }
                    SyncStatus::Cancelled => {
                        last.status = "cancelled".to_owned();
                        self.set_pending(None);
                        None
                    }
                    SyncStatus::LocalChanged => {
                        // 云端已更新，本地没动：稍后自动再来一轮。
                        last.status = "local_changed".to_owned();
                        self.notify_change();
                        None
                    }
                };
                if report.status == SyncStatus::Completed {
                    self.clear_rate_limit();
                }
                if report.status == SyncStatus::Completed && decision.is_some() {
                    self.set_pending(None);
                }
                Ok((report, pending))
            }
            Err(err) => {
                let text = err.to_string();
                last.status = "error".to_owned();
                last.summary = text.clone();
                match &err {
                    SyncError::RateLimited {
                        service,
                        retry_after_secs,
                    } => {
                        // 等到限流结束之前谁都不再发请求；结束之后再自动来一轮（标记“数据变了”）。
                        *lock(&self.inner.rate_limit) = Some((
                            Instant::now() + Duration::from_secs(*retry_after_secs),
                            service.clone(),
                        ));
                        self.notify_change();
                    }
                    SyncError::AuthFailed(message) => {
                        *lock(&self.inner.auth_paused) = Some(message.clone());
                    }
                    SyncError::WrongPassword => {
                        // 记住的密码过期（在别的电脑上改过主密码）或输错：丢掉内存里的，
                        // 让界面重新要。本机保存的那份由用户在界面里处理。
                        self.set_password(None);
                    }
                    SyncError::Format(message) if message.contains("基线") => {
                        self.inner.baseline_broken.store(true, Ordering::SeqCst);
                    }
                    _ => {}
                }
                self.refresh_baseline_flag(state);
                Err(text)
            }
        };
        if let Err(err) = service::save_last_result(&state.store, &last) {
            warn!(%err, "保存同步结果失败");
        }
        info!(status = %last.status, auto, "同步结束");
        match outcome {
            Ok((report, pending)) => Ok(RunView {
                status: last.status.clone(),
                summary: last.summary.clone(),
                conflicts: report.conflicts.len(),
                nulled_references: report.nulled_references.clone(),
                pending,
                status_view: self.status_view(state),
            }),
            Err(text) => Err(text),
        }
    }

    /// 主密码改完后的善后：内存里换成新的；记住的话本机保存的也换。
    pub fn replace_password(&self, state: &AppState, new_password: &str) {
        self.set_password(Some(new_password.to_owned()));
        if matches!(
            state.store.setting_get(service::MASTER_PASSWORD_KEY),
            Ok(Some(_))
        ) {
            if let Err(err) =
                service::save_master_password(&state.store, &DpapiSealer, new_password)
            {
                warn!(%err, "更新本机保存的主密码失败");
            }
        }
    }

    /// 恢复备份、改主密码、改用冲突里的另一份这些会改本地数据的操作，不能和同步同时进行。
    pub fn lock_for_exclusive(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        self.acquire()
    }

    /// 调度器：启动时一次、配置变化后 30 秒、之后每隔 N 分钟。
    /// 需要用户处理的状态（待确认、没有主密码、基线损坏、没配存储）下一律不自动跑。
    async fn scheduler(&self, app: AppHandle) {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if let Some(reason) = self.due(&app) {
                match self.run_sync(&app, true, None).await {
                    Ok(view) => info!(reason, status = %view.status, "自动同步"),
                    Err(err) => info!(reason, %err, "自动同步没有成功（只在状态栏显示）"),
                }
            }
            tokio::time::sleep(TICK).await;
        }
    }

    fn due(&self, app: &AppHandle) -> Option<&'static str> {
        let state = app.state::<AppState>();
        let auto = service::load_auto(&state.store).unwrap_or_default();
        let inputs = DueInputs {
            auto_enabled: auto.enabled,
            running: self.is_running(),
            pending_confirmation: self.pending().is_some(),
            baseline_broken: self.baseline_broken(),
            has_password: self.has_password(),
            ready: service::is_ready(&state.store).unwrap_or(false),
            configured: service::load_config(&state.store, &DpapiSealer)
                .ok()
                .flatten()
                .is_some(),
            rate_limited: self.rate_limit_remaining().is_some(),
            auth_paused: self.auth_paused().is_some(),
            startup_due: self.inner.startup_due.load(Ordering::SeqCst),
            since_last_change: lock(&self.inner.dirty_since).map(|since| since.elapsed()),
            since_last_run: lock(&self.inner.last_run).elapsed(),
            interval: Duration::from_secs(u64::from(auto.interval_minutes) * 60),
        };
        let reason = decide(&inputs)?;
        match reason {
            "启动时" => self.inner.startup_due.store(false, Ordering::SeqCst),
            "配置变化后" => *lock(&self.inner.dirty_since) = None,
            _ => {}
        }
        Some(reason)
    }
}

/// 调度器每一拍看到的情况。
#[derive(Debug, Clone)]
pub struct DueInputs {
    pub auto_enabled: bool,
    pub running: bool,
    pub pending_confirmation: bool,
    pub baseline_broken: bool,
    pub has_password: bool,
    pub ready: bool,
    pub configured: bool,
    /// 存储服务限流，还没到可以再发请求的时候。
    pub rate_limited: bool,
    /// 凭据被拒，等用户改配置或手动同步。
    pub auth_paused: bool,
    pub startup_due: bool,
    /// 距离最近一次“同步数据变了”多久；没有待同步的变化是 `None`。
    pub since_last_change: Option<Duration>,
    pub since_last_run: Duration,
    pub interval: Duration,
}

/// 这一拍要不要自动同步，原因是什么。
///
/// 需要用户处理的状态下**一律不自动跑**：开关关了、正在同步、停在防误删确认上
/// （不自动替用户选）、存储服务限流还没过、凭据被拒（认证失败）、基线损坏、没有主密码、还没用主密码和云端
/// 对上过（第一次必须手动）、没配存储。
pub fn decide(inputs: &DueInputs) -> Option<&'static str> {
    if !inputs.auto_enabled
        || inputs.running
        || inputs.pending_confirmation
        || inputs.rate_limited
        || inputs.auth_paused
        || inputs.baseline_broken
        || !inputs.has_password
        || !inputs.ready
        || !inputs.configured
    {
        return None;
    }
    if inputs.startup_due {
        return Some("启动时");
    }
    if inputs.since_last_change.is_some_and(|d| d >= DEBOUNCE) {
        return Some("配置变化后");
    }
    if inputs.since_last_run >= inputs.interval {
        return Some("定时");
    }
    None
}

/// 微软服务的地址。发布版永远是真实的微软地址；**调试构建**里可以用环境变量
/// `ZSSH_ONEDRIVE_TEST_BASE=http://127.0.0.1:<端口>` 指到本机的模拟服务（截图 / 端到端脚本用）。
pub fn onedrive_endpoints() -> zssh_core::sync::onedrive::oauth::Endpoints {
    #[cfg(debug_assertions)]
    if let Some(endpoints) =
        debug_endpoints(std::env::var("ZSSH_ONEDRIVE_TEST_BASE").ok().as_deref())
    {
        return endpoints;
    }
    zssh_core::sync::onedrive::oauth::Endpoints::microsoft()
}

/// 只认 `http://127.0.0.1:<端口>`：这个钩子只能指向本机，不能把登录引到别处去。
#[cfg(debug_assertions)]
fn debug_endpoints(base: Option<&str>) -> Option<zssh_core::sync::onedrive::oauth::Endpoints> {
    let base = base?.trim().trim_end_matches('/');
    let port = base.strip_prefix("http://127.0.0.1:")?;
    port.parse::<u16>().ok()?;
    Some(zssh_core::sync::onedrive::oauth::Endpoints {
        authorize: format!("{base}/consumers/oauth2/v2.0/authorize"),
        token: format!("{base}/consumers/oauth2/v2.0/token"),
        graph: format!("{base}/v1.0"),
    })
}

/// 用什么打开网址。发布版永远是系统默认浏览器（先过白名单校验）；**调试构建**里设了
/// `ZSSH_ONEDRIVE_TEST_URL_FILE=<文件>` 就只把网址写进那个文件、不打开任何浏览器（截图脚本自己扮演浏览器）。
pub fn onedrive_opener() -> Box<dyn zssh_core::sync::onedrive::browser::BrowserOpener> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("ZSSH_ONEDRIVE_TEST_URL_FILE") {
        return Box::new(UrlFileOpener(std::path::PathBuf::from(path)));
    }
    Box::new(zssh_core::sync::onedrive::browser::SystemBrowser)
}

#[cfg(debug_assertions)]
struct UrlFileOpener(std::path::PathBuf);

#[cfg(debug_assertions)]
impl zssh_core::sync::onedrive::browser::BrowserOpener for UrlFileOpener {
    fn open(&self, url: &str) -> Result<(), String> {
        std::fs::write(&self.0, url).map_err(|err| format!("写网址文件失败：{err}"))
    }
}

/// 从应用状态拼出同步引擎需要的本机信息。
pub fn build_context<'a>(
    state: &'a AppState,
    data_dir: &std::path::Path,
) -> Result<SyncContext<'a>, SyncError> {
    static SEALER: DpapiSealer = DpapiSealer;
    let device_id = state.store.sync_device_id().map_err(SyncError::from)?;
    Ok(SyncContext {
        db: &state.store,
        sealer: &SEALER,
        device_id,
        data_dir: data_dir.to_path_buf(),
        known_hosts_path: Some(state.known_hosts.our_file().to_path_buf()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        onedrive_endpoints: onedrive_endpoints(),
    })
}

/// 把 `ConcurrencyProbe` 的结果存成记录（测试连接成功时调用）。
pub fn concurrency_record(
    mode: zssh_core::sync::remote::ConcurrencyMode,
    notes: Vec<String>,
) -> ConcurrencyRecord {
    ConcurrencyRecord {
        mode,
        notes,
        checked_at: now_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> DueInputs {
        DueInputs {
            auto_enabled: true,
            running: false,
            pending_confirmation: false,
            baseline_broken: false,
            has_password: true,
            ready: true,
            configured: true,
            rate_limited: false,
            auth_paused: false,
            startup_due: false,
            since_last_change: None,
            since_last_run: Duration::from_secs(1),
            interval: Duration::from_secs(15 * 60),
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_debug_endpoint_hook_only_accepts_a_local_mock() {
        let ok = debug_endpoints(Some("http://127.0.0.1:5555/")).expect("本机模拟服务");
        assert_eq!(
            ok.authorize,
            "http://127.0.0.1:5555/consumers/oauth2/v2.0/authorize"
        );
        assert_eq!(ok.graph, "http://127.0.0.1:5555/v1.0");
        for bad in [
            None,
            Some(""),
            Some("https://login.microsoftonline.com"),
            Some("http://example.com:80"),
            Some("http://127.0.0.1"),
            Some("http://127.0.0.1:abc"),
            Some("http://127.0.0.1.evil.example:80"),
            Some("http://localhost:5555"),
        ] {
            assert!(debug_endpoints(bad).is_none(), "{bad:?} 不该被接受");
        }
    }

    #[tokio::test]
    async fn only_one_onedrive_login_at_a_time_and_cancel_reaches_it() {
        let controller = SyncController::new(std::env::temp_dir());
        assert!(!controller.onedrive_logging_in());
        // 没有登录在进行时点“取消”什么也不会发生。
        controller.cancel_onedrive_login();

        let cancel = controller.begin_onedrive_login().expect("第一个登录");
        assert!(controller.onedrive_logging_in());
        let second = controller
            .begin_onedrive_login()
            .expect_err("同时只允许一个");
        assert!(second.contains("正在浏览器中登录"), "{second}");

        // 取消信号能送到正在等待的登录（Notify 会记住提前到达的通知，不怕先后顺序）。
        controller.cancel_onedrive_login();
        tokio::time::timeout(Duration::from_secs(2), cancel.notified())
            .await
            .expect("取消信号要送到");

        controller.end_onedrive_login();
        assert!(!controller.onedrive_logging_in());
        controller
            .begin_onedrive_login()
            .expect("结束之后可以再登录");
    }

    #[test]
    fn nothing_is_due_right_after_a_run() {
        assert_eq!(decide(&ready()), None);
    }

    #[test]
    fn startup_change_debounce_and_interval_each_trigger() {
        let mut inputs = ready();
        inputs.startup_due = true;
        assert_eq!(decide(&inputs), Some("启动时"));

        let mut inputs = ready();
        inputs.since_last_change = Some(Duration::from_secs(29));
        assert_eq!(decide(&inputs), None, "30 秒防抖没到");
        inputs.since_last_change = Some(Duration::from_secs(30));
        assert_eq!(decide(&inputs), Some("配置变化后"));

        let mut inputs = ready();
        inputs.since_last_run = Duration::from_secs(15 * 60);
        assert_eq!(decide(&inputs), Some("定时"));
        inputs.interval = Duration::from_secs(60 * 60);
        assert_eq!(decide(&inputs), None, "间隔可设置");
    }

    #[test]
    fn only_errors_that_need_a_new_config_pause_auto_sync() {
        use zssh_core::sync::storage::StorageError;
        // 换了配置才可能好的：凭据被服务拒绝（WebDAV 401、S3 密钥错误、GitHub 401 都归这里）。
        let rejected = SyncError::from(StorageError::AuthFailed("令牌已失效".to_owned()));
        assert!(matches!(rejected, SyncError::AuthFailed(_)));
        // 可能自己恢复的：网络不通、超时、服务器 5xx，照常重试，不暂停。
        for transient in [
            StorageError::Timeout,
            StorageError::Unavailable("服务器返回 503".to_owned()),
            StorageError::Io("无法连接到存储服务".to_owned()),
        ] {
            assert!(
                !matches!(SyncError::from(transient), SyncError::AuthFailed(_)),
                "网络类错误不能暂停自动同步"
            );
        }
    }

    #[test]
    fn states_that_need_the_user_never_run_automatically() {
        let mut triggers = ready();
        triggers.startup_due = true;
        triggers.since_last_change = Some(Duration::from_secs(999));
        triggers.since_last_run = Duration::from_secs(99_999);
        assert!(decide(&triggers).is_some());
        let stops: [fn(&mut DueInputs); 9] = [
            |i| i.auto_enabled = false,
            |i| i.running = true,
            |i| i.pending_confirmation = true,
            |i| i.rate_limited = true,
            |i| i.auth_paused = true,
            |i| i.baseline_broken = true,
            |i| i.has_password = false,
            |i| i.ready = false,
            |i| i.configured = false,
        ];
        for (index, stop) in stops.iter().enumerate() {
            let mut inputs = triggers.clone();
            stop(&mut inputs);
            assert_eq!(decide(&inputs), None, "第 {index} 种情况下不能自动同步");
        }
    }
}
