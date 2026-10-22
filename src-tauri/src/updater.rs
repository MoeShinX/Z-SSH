//! 软件自动更新的运行时：检查、下载、安装、状态推送。下载和验签由 Tauri 官方 updater
//! 插件做（minisign 签名，公钥在 `tauri.conf.json`）；这里负责：
//!
//! - 走 Z-SSH 的默认代理（和配置同步一样；国内访问 GitHub Releases 常要代理）；
//! - 启动时自动检查（设置里可关）、“跳过这个版本”（判断在 `zssh_core::updater`）；
//! - 自动检查到新版本后在后台下载并验签，用户退出 Z-SSH 时顺手安装（`install_on_exit`）；
//! - 把状态推给界面（状态栏提示、关于页）。更新包在内存里，验签通过后才允许安装。
//!
//! 插件自带的 JS 命令**不开权限**（capability 里没有 `updater:*`），界面只能调下面这几条
//! 命令，所以界面绕不开代理设置，也装不了没验过签的包。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tracing::{info, warn};
use url::Url;
use zssh_core::updater::{self, RestartBusy};

use crate::state::AppState;

pub const STATUS_EVENT: &str = "update://status";
/// 启动后等多久做自动检查（让界面先出来、同步先跑）。
const STARTUP_DELAY: Duration = Duration::from_secs(8);
/// 程序一直开着时，每隔多久再自动检查一次（用户 2026-10-21 定 6 小时）。
const RECHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 下载进度最多多久推一次：界面看着连续（每秒约 6 次），又不至于每个网络块都推。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

/// 在哪一步出的错：界面据此决定放在哪里提示。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Check,
    Download,
    Install,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate {
        checked_at: i64,
    },
    Available {
        version: String,
        notes: Option<String>,
        date: Option<String>,
    },
    Downloading {
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    Ready {
        version: String,
        /// 更新说明：下好后弹出的对话框里照样给用户看改了什么。
        notes: Option<String>,
    },
    Failed {
        stage: Stage,
        message: String,
    },
}

/// 定时自动检查要不要跑：没查过、已是最新、上次检查失败、自动下载失败时查；
/// 手动查到新版本但用户没点下载（`Available`）也查——这一轮会标成自动，接着后台下载，退出时安装。
/// 正在检查 / 下载、下载好等退出时安装、安装失败（用户点了重启安装没成功，要他处理）时不查。
fn periodic_check_allowed(status: &UpdateStatus) -> bool {
    matches!(
        status,
        UpdateStatus::Idle
            | UpdateStatus::Available { .. }
            | UpdateStatus::UpToDate { .. }
            | UpdateStatus::Failed {
                stage: Stage::Check | Stage::Download,
                ..
            }
    )
}

/// 自动检查查到新版本之后要不要接着自动下载：只有"这次检查是自动发起的、查到了可用版本
/// （没被跳过的才会是 `Available`）、不是免安装版"才下载。手动检查走对话框，由用户点了才下载。
fn should_auto_download(status: &UpdateStatus, portable: bool, auto: bool) -> bool {
    auto && !portable && matches!(status, UpdateStatus::Available { .. })
}

/// 退出 Z-SSH 时要不要顺手安装已经下好的更新。
///
/// 必须同时满足：状态是 `Ready`、更新信息和包都还在内存里、不是免安装版、
/// 「启动时自动检查」开关是开着的（关掉 = 用户不要自动更新；读不出来按"不确定"处理，不装）。
/// 还在 `Downloading` / 只是 `Available` 时不装也不等：包不完整，装不了。
fn should_install_on_exit(
    status: &UpdateStatus,
    has_update: bool,
    has_bytes: bool,
    portable: bool,
    auto_update_enabled: bool,
) -> bool {
    matches!(status, UpdateStatus::Ready { .. })
        && has_update
        && has_bytes
        && !portable
        && auto_update_enabled
}

/// 推给界面的状态事件：状态本身加上"这是自动流程产生的"标记（`auto`）。
/// 自动流程的状态底部不提示；手动检查 / 手动点了下载引出的状态照旧提示。
#[derive(Serialize, Clone)]
struct StatusEvent<'a> {
    #[serde(flatten)]
    status: &'a UpdateStatus,
    auto: bool,
}

/// `tauri.conf.json` 里公钥还没换成真的时的占位串（见 docs/release.md）。
pub const PUBKEY_PLACEHOLDER: &str = "REPLACE_WITH_UPDATER_PUBLIC_KEY";

/// 配置里的更新公钥（没有就是空串）。
fn configured_pubkey<R: Runtime>(app: &AppHandle<R>) -> String {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|value| value.get("pubkey"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// 配置里的第一个更新地址（推发布页用）。
fn configured_endpoint<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    app.config()
        .plugins
        .0
        .get("updater")?
        .get("endpoints")?
        .get(0)?
        .as_str()
        .map(str::to_owned)
}

/// 本程序所在目录是不是免安装版的布局。拿不到自身路径时按安装版处理：
/// 判错成免安装版会让安装版用户失去自动更新，判错成安装版最多是更新装成另一份。
fn detect_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(updater::is_portable_layout))
        .unwrap_or(false)
}

/// 免安装版点"更新"时的提示。
pub const PORTABLE_MESSAGE: &str =
    "免安装版不能自动更新：请到发布页下载新的免安装 zip，解压覆盖原来的文件夹（数据在应用数据目录里，不受影响）";

/// 一次检查要用的参数。生产环境只带代理；端点和公钥来自 `tauri.conf.json`。
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// 仅测试：覆盖更新地址。
    pub endpoint: Option<Url>,
    /// 仅测试：覆盖验签公钥。
    pub pubkey: Option<String>,
    /// 默认代理（带账号密码的完整地址）。
    pub proxy: Option<Url>,
}

/// 调试构建里允许用环境变量指向本地的测试服务器和测试公钥；发布版不认这两个变量，
/// 免得别人靠改环境变量把更新源换掉。
#[cfg(debug_assertions)]
fn test_overrides() -> (Option<Url>, Option<String>) {
    let endpoint = std::env::var("ZSSH_UPDATER_ENDPOINT")
        .ok()
        .and_then(|raw| Url::parse(&raw).ok());
    let pubkey = std::env::var("ZSSH_UPDATER_PUBKEY").ok();
    (endpoint, pubkey)
}

#[cfg(not(debug_assertions))]
fn test_overrides() -> (Option<Url>, Option<String>) {
    (None, None)
}

/// 把插件的错误变成给用户看的话。**签名不对**要说清楚，这是最重要的一类。
pub fn friendly_error(err: &tauri_plugin_updater::Error, stage: Stage) -> String {
    use tauri_plugin_updater::Error as E;
    match err {
        E::Minisign(_) | E::Base64(_) | E::SignatureUtf8(_) => {
            "更新包的签名校验没有通过，已拒绝安装（文件可能被改动过，或不是官方发布的）".to_owned()
        }
        E::ReleaseNotFound => "更新服务器没有返回有效的版本信息".to_owned(),
        E::Reqwest(inner) => {
            if inner.is_timeout() {
                "连接更新服务器超时（可以在「连接 → 默认代理」里设置代理后重试）".to_owned()
            } else if inner.is_connect() {
                "无法连接到更新服务器（网络不通或代理不可用；可以在「连接 → 默认代理」里设置代理）"
                    .to_owned()
            } else {
                match stage {
                    Stage::Download => "下载更新包失败，网络可能中断了".to_owned(),
                    _ => "检查更新失败，网络请求出错".to_owned(),
                }
            }
        }
        E::Network(_) => "下载更新包失败，网络可能中断了".to_owned(),
        E::Io(_) => "读写更新文件失败".to_owned(),
        other => match stage {
            Stage::Check => format!("检查更新失败：{other}"),
            Stage::Download => format!("下载更新失败：{other}"),
            Stage::Install => format!("安装更新失败：{other}"),
        },
    }
}

/// 向服务器问一次有没有新版本（不碰界面状态，方便测试）。
pub async fn check_with<R: Runtime>(
    app: &AppHandle<R>,
    options: &CheckOptions,
) -> Result<Option<Update>, tauri_plugin_updater::Error> {
    let mut builder = app.updater_builder().timeout(REQUEST_TIMEOUT);
    if let Some(endpoint) = &options.endpoint {
        builder = builder.endpoints(vec![endpoint.clone()])?;
    }
    if let Some(pubkey) = &options.pubkey {
        builder = builder.pubkey(pubkey.clone());
    }
    builder = match &options.proxy {
        Some(proxy) => builder.proxy(proxy.clone()),
        // 没配代理 = 直连，不去读系统 / 环境变量里的代理（和配置同步一致）。
        None => builder.no_proxy(),
    };
    builder.build()?.check().await
}

/// 下载更新包并验签；验不过就是 `Err`，不会返回字节。
pub async fn download_verified(
    update: &Update,
    mut on_progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>, tauri_plugin_updater::Error> {
    let mut downloaded = 0u64;
    update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                on_progress(downloaded, total);
            },
            || {},
        )
        .await
}

struct Inner {
    status: Mutex<UpdateStatus>,
    update: Mutex<Option<Update>>,
    bytes: Mutex<Option<Vec<u8>>>,
    /// 检查 / 下载同一时间只跑一个。
    running: tokio::sync::Mutex<()>,
    /// 免安装版：照常检查并提示新版本，但不下载、不安装，让用户去发布页下载 zip。
    portable: bool,
    /// 当前这一轮"检查 → 下载"是不是自动发起的。自动检查开始时置 true，
    /// 用户手动检查 / 手动点下载时置 false；随状态一起推给界面，界面据此决定底部提示条显不显示。
    /// 放在后端而不是前端：状态和来源在同一处更新，不会因为事件先后而串。
    auto: AtomicBool,
}

#[derive(Clone)]
pub struct UpdateController {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl Default for UpdateController {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateController {
    pub fn new() -> Self {
        Self::with_portable(detect_portable())
    }

    pub fn with_portable(portable: bool) -> Self {
        if portable {
            info!("免安装版：有新版本时只提示手动下载");
        }
        Self {
            inner: Arc::new(Inner {
                status: Mutex::new(UpdateStatus::Idle),
                update: Mutex::new(None),
                bytes: Mutex::new(None),
                running: tokio::sync::Mutex::new(()),
                portable,
                auto: AtomicBool::new(true),
            }),
        }
    }

    pub fn portable(&self) -> bool {
        self.inner.portable
    }

    /// 当前状态是不是自动流程产生的（界面据此隐藏底部提示条）。
    pub fn is_auto(&self) -> bool {
        self.inner.auto.load(Ordering::SeqCst)
    }

    fn mark_auto(&self, auto: bool) {
        self.inner.auto.store(auto, Ordering::SeqCst);
    }

    /// 免安装版提示里"打开下载页"要打开的地址：由配置的更新地址和新版本号推出来。
    pub fn download_page<R: Runtime>(&self, app: &AppHandle<R>) -> Option<String> {
        let version = match self.status() {
            UpdateStatus::Available { version, .. } => version,
            _ => app.package_info().version.to_string(),
        };
        updater::release_page_url(&configured_endpoint(app)?, &version)
    }

    /// 免安装版不下载：把状态改成"请手动下载"并返回它；安装版返回 `None`。
    fn refuse_if_portable<R: Runtime>(&self, app: &AppHandle<R>) -> Option<UpdateStatus> {
        if !self.inner.portable {
            return None;
        }
        let status = UpdateStatus::Failed {
            stage: Stage::Download,
            message: PORTABLE_MESSAGE.to_owned(),
        };
        self.set_status(app, status.clone());
        Some(status)
    }

    pub fn status(&self) -> UpdateStatus {
        lock(&self.inner.status).clone()
    }

    fn set_status<R: Runtime>(&self, app: &AppHandle<R>, status: UpdateStatus) {
        *lock(&self.inner.status) = status.clone();
        let event = StatusEvent {
            status: &status,
            auto: self.is_auto(),
        };
        if let Err(err) = app.emit(STATUS_EVENT, event) {
            warn!(%err, "推送更新状态失败");
        }
        // 托盘菜单跟着变（有下好的更新时多一项「重启安装」）；内容没变不会重建。
        crate::tray::refresh(app, &status);
    }

    /// 软件网络代理 → 带账号密码的代理地址。读不了就当没有（直连）。
    fn proxy_url<R: Runtime>(app: &AppHandle<R>) -> Option<Url> {
        let state = app.try_state::<AppState>()?;
        let proxy = zssh_core::sync::app_proxy::app_proxy(&state.store)
            .ok()
            .flatten()?;
        Url::parse(&proxy.url_with_credentials()).ok()
    }

    fn options<R: Runtime>(app: &AppHandle<R>) -> CheckOptions {
        let (endpoint, pubkey) = test_overrides();
        CheckOptions {
            endpoint,
            pubkey,
            proxy: Self::proxy_url(app),
        }
    }

    /// 启动后自动检查一次，之后程序开着时每 6 小时再查一次（都可在设置里关）。
    /// 定时检查只在"还没发现新版本"时做：已经有新版本、正在下载、下载好等重启时不重复查，
    /// 免得底部提示条反复闪、或者把用户点过「稍后」的提示又顶出来。
    pub fn start(&self, app: &AppHandle) {
        let controller = self.clone();
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(STARTUP_DELAY).await;
            loop {
                let enabled = handle.try_state::<AppState>().is_some_and(|state| {
                    updater::load_prefs(&state.store)
                        .map(|prefs| updater::should_auto_check(&prefs))
                        .unwrap_or(false)
                });
                if !enabled {
                    info!("跳过自动检查更新（开关关了）");
                } else if periodic_check_allowed(&controller.status()) {
                    controller.auto_check(&handle).await;
                }
                tokio::time::sleep(RECHECK_INTERVAL).await;
            }
        });
    }

    /// 自动检查：查到可用版本（没被跳过、不是免安装版）就接着在后台下载并验签，
    /// 下好后等用户退出 Z-SSH 时安装（见 [`Self::install_on_exit`]）。下载失败什么也不弹，
    /// 状态记 `Failed{Download}`，定时检查会在下个周期重来。
    pub async fn auto_check<R: Runtime>(&self, app: &AppHandle<R>) {
        self.auto_check_using(app, Self::options(app)).await;
    }

    async fn auto_check_using<R: Runtime>(&self, app: &AppHandle<R>, options: CheckOptions) {
        let status = self.check_using(app, false, options).await;
        if should_auto_download(&status, self.inner.portable, self.is_auto()) {
            self.download_inner(app, true).await;
        }
    }

    /// 检查更新。`manual` = 用户点了按钮（不受“跳过这个版本”影响，失败也要告诉他）。
    pub async fn check<R: Runtime>(&self, app: &AppHandle<R>, manual: bool) -> UpdateStatus {
        self.check_using(app, manual, Self::options(app)).await
    }

    async fn check_using<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        manual: bool,
        options: CheckOptions,
    ) -> UpdateStatus {
        let Ok(_running) = self.inner.running.try_lock() else {
            return self.status(); // 正在检查或下载，不重复
        };
        // 已经下载好、等退出时安装的不再重新检查。
        if matches!(self.status(), UpdateStatus::Ready { .. }) {
            return self.status();
        }
        // 真正开始一轮检查：这一轮是自动的还是手动的，后面的所有状态都带着这个标记。
        self.mark_auto(!manual);
        let key = options
            .pubkey
            .clone()
            .unwrap_or_else(|| configured_pubkey(app));
        if key.trim().is_empty() || key == PUBKEY_PLACEHOLDER {
            let status = UpdateStatus::Failed {
                stage: Stage::Check,
                message: "这个版本没有配置更新签名公钥，无法检查更新（请到发布页手动下载新版）"
                    .to_owned(),
            };
            self.set_status(app, status.clone());
            return status;
        }
        self.set_status(app, UpdateStatus::Checking);
        let result = check_with(app, &options).await;
        let now = now_secs();
        if let Some(state) = app.try_state::<AppState>() {
            let _ = updater::record_check(&state.store, now);
        }
        let current = app.package_info().version.to_string();
        let status = match result {
            Ok(Some(update)) => {
                let announce = app.try_state::<AppState>().is_none_or(|state| {
                    let prefs = updater::load_prefs(&state.store).unwrap_or_default();
                    updater::should_announce(&prefs, &current, &update.version, manual)
                });
                if announce {
                    let status = UpdateStatus::Available {
                        version: update.version.clone(),
                        notes: update.body.clone(),
                        date: update.date.map(|d| d.to_string()),
                    };
                    *lock(&self.inner.update) = Some(update);
                    status
                } else {
                    info!(version = %update.version, "有新版本但已被跳过（或并不比当前新）");
                    UpdateStatus::UpToDate { checked_at: now }
                }
            }
            Ok(None) => UpdateStatus::UpToDate { checked_at: now },
            Err(err) => {
                warn!(%err, manual, "检查更新失败");
                UpdateStatus::Failed {
                    stage: Stage::Check,
                    message: friendly_error(&err, Stage::Check),
                }
            }
        };
        self.set_status(app, status.clone());
        status
    }

    /// 用户点了「立即更新」/「重试下载」：下载（并验签）已经发现的新版本。进度通过状态事件推给界面。
    pub async fn download<R: Runtime>(&self, app: &AppHandle<R>) -> UpdateStatus {
        self.download_inner(app, false).await
    }

    /// `auto` = 自动检查接着发起的下载；用户发起的下载把这一轮标成手动，
    /// 底部提示条才会显示进度和"已下载好"。
    async fn download_inner<R: Runtime>(&self, app: &AppHandle<R>, auto: bool) -> UpdateStatus {
        if !auto {
            self.mark_auto(false);
        }
        if let Some(status) = self.refuse_if_portable(app) {
            return status;
        }
        let Ok(_running) = self.inner.running.try_lock() else {
            return self.status();
        };
        let Some(update) = lock(&self.inner.update).clone() else {
            return self.status();
        };
        let version = update.version.clone();
        let notes = update.body.clone();
        self.set_status(
            app,
            UpdateStatus::Downloading {
                version: version.clone(),
                downloaded: 0,
                total: None,
            },
        );
        let mut throttle = updater::ProgressThrottle::new(PROGRESS_INTERVAL);
        let controller = self.clone();
        let progress_app = app.clone();
        let progress_version = version.clone();
        let result = download_verified(&update, move |downloaded, total| {
            if throttle.should_emit(Instant::now(), downloaded, total) {
                controller.set_status(
                    &progress_app,
                    UpdateStatus::Downloading {
                        version: progress_version.clone(),
                        downloaded,
                        total,
                    },
                );
            }
        })
        .await;
        if let Ok(bytes) = &result {
            // 服务器没给总大小时，下载中一直是"不确定进度"；收尾补一条 100%，
            // 界面的进度条在变成"已下载好"之前先走满，不会从半截直接跳走。
            let size = bytes.len() as u64;
            self.set_status(
                app,
                UpdateStatus::Downloading {
                    version: version.clone(),
                    downloaded: size,
                    total: Some(size),
                },
            );
        }
        let status = match result {
            Ok(bytes) => {
                *lock(&self.inner.bytes) = Some(bytes);
                UpdateStatus::Ready { version, notes }
            }
            Err(err) => {
                warn!(%err, "下载更新失败");
                // 失败后还留着“有新版本”，用户可以重试。
                UpdateStatus::Failed {
                    stage: Stage::Download,
                    message: friendly_error(&err, Stage::Download),
                }
            }
        };
        self.set_status(app, status.clone());
        status
    }

    /// 重启前要问用户的事：AI 命令的数量在这里数；传输和编辑器里没保存的修改的数量界面自己有
    /// （它们的状态在前端的标签里），界面把三项合起来判断。
    pub fn busy_from_backend<R: Runtime>(app: &AppHandle<R>) -> RestartBusy {
        let ai_commands = app
            .try_state::<zssh_core::ai::runs::TerminalRuns>()
            .map(|runs| runs.count())
            .unwrap_or_default();
        RestartBusy {
            transfers: 0,
            ai_commands,
            unsaved_edits: 0,
        }
    }

    /// 安装已经下载并验过签的更新。Windows 上会启动安装程序并退出本程序。
    /// 调用前界面必须已经做过“重启前检查”。
    pub async fn install(&self, app: &AppHandle) -> Result<(), String> {
        if self.inner.portable {
            return Err(PORTABLE_MESSAGE.to_owned());
        }
        let Some(update) = lock(&self.inner.update).clone() else {
            return Err("没有可安装的更新".to_owned());
        };
        let Some(bytes) = lock(&self.inner.bytes).clone() else {
            return Err("更新包还没有下载好".to_owned());
        };
        // 安装成功会直接结束进程、不走 RunEvent::Exit，窗口状态插件来不及写盘：先存一次。
        crate::save_window_state(app);
        let result = tokio::task::spawn_blocking(move || update.install(bytes))
            .await
            .map_err(|err| format!("安装线程异常结束：{err}"))?;
        match result {
            // Windows 上 install 成功会直接退出进程，走不到这里；其他平台不支持。
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(%err, "安装更新失败");
                let message = friendly_error(&err, Stage::Install);
                // 用户点了「立即重启安装」才走到这里：失败要让他看到，不论包是不是自动下的。
                self.mark_auto(false);
                self.set_status(
                    app,
                    UpdateStatus::Failed {
                        stage: Stage::Install,
                        message: message.clone(),
                    },
                );
                Err(message)
            }
        }
    }

    /// 退出 Z-SSH 时顺手安装已经下好并验过签的更新。**只能在退出流程的最后调用**：
    /// Windows 上安装成功会启动安装程序并直接结束进程，所以智能体、编辑器临时文件等清理
    /// 必须已经做完，否则安装程序覆盖文件时可能被占用。
    ///
    /// 用 `restart_after_install(false)`：用户是要关程序，装完不能再把它打开。
    /// 失败只记日志：不弹窗、不能挡住退出。还在下载中就不装也不等。
    pub fn install_on_exit<R: Runtime>(&self, app: &AppHandle<R>) {
        // 读不出开关按"不确定"处理：不装。
        let enabled = app.try_state::<AppState>().is_some_and(|state| {
            updater::load_prefs(&state.store)
                .map(|prefs| updater::should_auto_check(&prefs))
                .unwrap_or(false)
        });
        let update = lock(&self.inner.update).clone();
        let has_bytes = lock(&self.inner.bytes).is_some();
        if !should_install_on_exit(
            &self.status(),
            update.is_some(),
            has_bytes,
            self.inner.portable,
            enabled,
        ) {
            return;
        }
        let (Some(update), Some(bytes)) = (update, lock(&self.inner.bytes).take()) else {
            return;
        };
        info!(version = %update.version, "退出时安装已下载的更新");
        if let Err(err) = update.restart_after_install(false).install(bytes) {
            warn!(%err, "退出时安装更新失败，下次启动再试");
        }
    }

    /// “跳过这个版本”：记下来，之后自动检查不再提示它（更新的版本照常提示）。
    pub fn skip(&self, app: &AppHandle, version: &str) -> Result<(), String> {
        if let Some(state) = app.try_state::<AppState>() {
            updater::skip_version(&state.store, version).map_err(|err| err.to_string())?;
        }
        *lock(&self.inner.update) = None;
        *lock(&self.inner.bytes) = None;
        self.set_status(
            app,
            UpdateStatus::UpToDate {
                checked_at: now_secs(),
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
