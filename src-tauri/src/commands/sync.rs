//! 配置同步的 Tauri 命令：只做参数转接，逻辑在 `zssh-core::sync`，
//! 运行时（串行、后台线程、自动同步）在 `crate::sync_controller`。
//!
//! 调用链：界面 `api/sync.ts` → 这里的命令 → `SyncController` / `SyncContext`
//! （`zssh_core::sync::service`）→ `SyncEngine` → `WebDavRaw` / `S3Raw`。
//! 改本地数据的操作（同步、恢复备份、改用冲突里的另一份、改主密码）都在
//! `spawn_blocking` 里做，并且和同步任务互斥。

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use zssh_core::sync::backup::BackupDetail;
use zssh_core::sync::conflicts::ConflictView;
use zssh_core::sync::github::{self, GithubConfig};
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::onedrive::browser::REVOKE_PAGE_URL;
use zssh_core::sync::onedrive::login::LOGIN_TIMEOUT;
use zssh_core::sync::onedrive::tokens::{DbTokenStore, TokenStore};
use zssh_core::sync::remote::ConnectionProbe;
use zssh_core::sync::s3::S3Config;
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::service::{self, AutoSettings, StorageConfig};
use zssh_core::sync::webdav::WebDavConfig;

use crate::state::AppState;
use crate::sync_controller::{
    build_context, concurrency_record, onedrive_endpoints, onedrive_opener, RunView, StatusView,
    SyncController,
};

fn text<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

/// 界面提交的连接信息。密码 / Secret 留空 = 沿用已保存的。
#[derive(Deserialize)]
pub struct ConfigInput {
    pub provider: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "default_true")]
    pub path_style: bool,
    // GitHub：接口地址（空 = api.github.com）、令牌（留空 = 沿用已保存的）、Gist ID
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub gist_id: String,
}

/// `Debug` 手写：密码 / Secret / 令牌是明文秘密，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for ConfigInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secret = |value: &str| if value.is_empty() { "" } else { "***" };
        f.debug_struct("ConfigInput")
            .field("provider", &self.provider)
            .field("url", &self.url)
            .field("directory", &self.directory)
            .field("username", &self.username)
            .field("password", &secret(&self.password))
            .field("allow_insecure_http", &self.allow_insecure_http)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &self.access_key)
            .field("secret_key", &secret(&self.secret_key))
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("api_base", &self.api_base)
            .field("token", &secret(&self.token))
            .field("gist_id", &self.gist_id)
            .finish()
    }
}

fn default_true() -> bool {
    true
}

impl ConfigInput {
    fn into_config(self) -> Result<StorageConfig, String> {
        match self.provider.as_str() {
            "webdav" => Ok(StorageConfig::Webdav(WebDavConfig {
                url: self.url.trim().to_owned(),
                directory: self.directory.trim().to_owned(),
                username: self.username.trim().to_owned(),
                password: self.password,
                allow_insecure_http: self.allow_insecure_http,
            })),
            "s3" => Ok(StorageConfig::S3(S3Config {
                endpoint: self.endpoint.trim().to_owned(),
                region: self.region.trim().to_owned(),
                bucket: self.bucket.trim().to_owned(),
                access_key: self.access_key.trim().to_owned(),
                secret_key: self.secret_key,
                prefix: self.prefix.trim().to_owned(),
                path_style: self.path_style,
            })),
            "github" => Ok(StorageConfig::Github(GithubConfig {
                api_base: self.api_base.trim().to_owned(),
                token: self.token.trim().to_owned(),
                gist_id: self.gist_id.trim().to_owned(),
                allow_insecure_http: self.allow_insecure_http,
            })),
            "onedrive" => Err("OneDrive 要通过“用微软账号登录”连接，不能直接保存".to_owned()),
            other => Err(format!("不支持的存储类型：{other}")),
        }
    }
}

#[tauri::command]
pub async fn sync_status(
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
) -> Result<StatusView, String> {
    Ok(sync.status_view(&state))
}

/// 保存连接信息（DPAPI 加密存本机）。内容没变就什么都不动；变了要重新测试连接、
/// 重新输入主密码，因为换了位置。
#[tauri::command]
pub async fn sync_config_save(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    input: ConfigInput,
) -> Result<StatusView, String> {
    if save_config(&state, input)? {
        sync.clear_rate_limit();
    }
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// 校验并保存连接信息，返回内容有没有变（变了：换了存储 / 令牌，之前的限流等待不再适用）。
fn save_config(state: &AppState, input: ConfigInput) -> Result<bool, String> {
    let mut config = input.into_config()?;
    let saved = service::load_config(&state.store, &DpapiSealer).map_err(text)?;
    config.merge_saved_secret(saved.as_ref());
    validate_config(&config)?;
    if saved.as_ref() == Some(&config) {
        return Ok(false);
    }
    service::save_config(&state.store, &DpapiSealer, &config).map_err(text)?;
    Ok(true)
}

fn validate_config(config: &StorageConfig) -> Result<(), String> {
    if let StorageConfig::Github(github) = config {
        if github.token.trim().is_empty() {
            return Err("请填写 GitHub 令牌".to_owned());
        }
        if github.gist_id.trim().is_empty() {
            return Err("请填写 Gist ID，或者点「新建 Gist」".to_owned());
        }
        if github.is_plain_http() && !github.allow_insecure_http {
            return Err("接口地址是 http://，令牌会以明文发送；需要先勾选确认".to_owned());
        }
    }
    if let StorageConfig::Webdav(webdav) = config {
        if webdav.url.is_empty() {
            return Err("请填写 WebDAV 地址".to_owned());
        }
        if webdav.is_plain_http() && !webdav.allow_insecure_http {
            return Err("地址是 http://，密码会以明文发送；需要先勾选确认".to_owned());
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CreatedGist {
    pub gist_id: String,
    pub status: StatusView,
}

/// 新建一个 secret Gist（描述“Z-SSH 配置同步（已加密）”，里面只有一个占位文件），
/// 把 ID 连同令牌一起保存，返回 ID 给界面显示。
///
/// 调用链：界面 `syncGithubCreateGist` → 这里 → `github::create_gist`（`POST /gists`，走默认代理）
/// → `service::save_config`（令牌用 DPAPI 加密存本机）。
#[tauri::command]
pub async fn sync_github_create_gist(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    input: ConfigInput,
) -> Result<CreatedGist, String> {
    let mut config = input.into_config()?;
    let saved = service::load_config(&state.store, &DpapiSealer).map_err(text)?;
    config.merge_saved_secret(saved.as_ref());
    let StorageConfig::Github(mut github) = config else {
        return Err("只有 GitHub 存储需要新建 Gist".to_owned());
    };
    if github.token.trim().is_empty() {
        return Err("请先填写 GitHub 令牌".to_owned());
    }
    if github.is_plain_http() && !github.allow_insecure_http {
        return Err("接口地址是 http://，令牌会以明文发送；需要先勾选确认".to_owned());
    }
    let proxy = zssh_core::sync::app_proxy::app_proxy(&state.store).map_err(text)?;
    let id = github::create_gist(&github, proxy.as_ref())
        .await
        .map_err(text)?;
    github.gist_id = id.clone();
    service::save_config(&state.store, &DpapiSealer, &StorageConfig::Github(github))
        .map_err(text)?;
    sync.clear_rate_limit();
    sync.emit_status(&app);
    Ok(CreatedGist {
        gist_id: id,
        status: sync.status_view(&state),
    })
}

/// OneDrive 登录：校验网址后用系统默认浏览器打开微软登录页，等回环回调，换令牌，保存。
///
/// 调用链：界面 `syncOnedriveLogin` → 这里 → `service::onedrive_sign_in` → `login::login`
/// （PKCE + 随机 state → `Loopback::bind`（127.0.0.1 与 [::1]）→ `SystemBrowser::open`
/// （白名单校验 + ShellExecuteW）→ `Loopback::wait` → `oauth::exchange_code`，走软件网络代理）
/// → `DbTokenStore::save`（DPAPI）→ `service::save_config`。成功后把主窗口带到前台。
/// 这个命令会一直等到登录完成 / 被拒绝 / 超时（5 分钟）/ 被取消。
#[tauri::command]
pub async fn sync_onedrive_login(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    client_id: Option<String>,
) -> Result<StatusView, String> {
    let cancel = sync.begin_onedrive_login()?;
    sync.emit_status(&app);
    let opener = onedrive_opener();
    let result = service::onedrive_sign_in(
        &state.store,
        &DpapiSealer,
        &onedrive_endpoints(),
        client_id.as_deref().unwrap_or_default(),
        LOGIN_TIMEOUT,
        opener.as_ref(),
        &cancel,
    )
    .await;
    sync.end_onedrive_login();
    match result {
        Ok(_) => {
            // 换了账号 / 重新登录：之前的限流等待和“凭据被拒”的暂停都不再适用。
            sync.clear_rate_limit();
            crate::focus_main_window(&app);
            sync.emit_status(&app);
            Ok(sync.status_view(&state))
        }
        Err(err) => {
            sync.emit_status(&app);
            Err(err.to_string())
        }
    }
}

/// OneDrive 登录中点“取消”：关闭回环监听，登录命令随即以“已取消登录”返回。
#[tauri::command]
pub async fn sync_onedrive_login_cancel(sync: State<'_, SyncController>) -> Result<(), String> {
    sync.cancel_onedrive_login();
    Ok(())
}

/// 断开 OneDrive：删除本机保存的令牌和连接设置（云端的同步包不动）。
/// 个人账号没有简单的撤销接口，授权要用户自己在微软账户的“应用和服务”页面里撤销
/// （`sync_onedrive_open_revoke_page` 打开那个页面）。
#[tauri::command]
pub async fn sync_onedrive_disconnect(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
) -> Result<StatusView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let current = service::load_config(&state.store, &DpapiSealer).map_err(text)?;
    if !matches!(current, Some(StorageConfig::Onedrive(_))) {
        return Err("当前连接的不是 OneDrive".to_owned());
    }
    service::onedrive_disconnect(&state.store).map_err(text)?;
    sync.clear_rate_limit();
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// 用系统浏览器打开微软账户里管理“应用和服务”授权的页面（固定网址，同样先过白名单校验）。
#[tauri::command]
pub async fn sync_onedrive_open_revoke_page() -> Result<(), String> {
    onedrive_opener().open(REVOKE_PAGE_URL)
}

#[derive(Debug, Serialize)]
pub struct TestResult {
    pub probe: ConnectionProbe,
    pub status: StatusView,
}

/// 测试连接。带 `input` 就先保存再测；不带就测已保存的。检测到的并发保护能力
/// 存起来，之后的同步按它走（没检测过的一律按不支持处理）。
#[tauri::command]
pub async fn sync_test_connection(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    input: Option<ConfigInput>,
) -> Result<TestResult, String> {
    if let Some(input) = input {
        if save_config(&state, input)? {
            sync.clear_rate_limit();
        }
    }
    let config = service::load_config(&state.store, &DpapiSealer)
        .map_err(text)?
        .ok_or_else(|| "还没有填写存储信息".to_owned())?;
    let proxy = zssh_core::sync::app_proxy::app_proxy(&state.store).map_err(text)?;
    let tokens: std::sync::Arc<dyn TokenStore + '_> =
        std::sync::Arc::new(DbTokenStore::new(&state.store, &DpapiSealer));
    let probe =
        service::test_connection(&config, proxy.as_ref(), tokens, &onedrive_endpoints()).await;
    if let Some(concurrency) = &probe.concurrency {
        service::save_concurrency(
            &state.store,
            &concurrency_record(concurrency.mode, concurrency.notes.clone()),
        )
        .map_err(text)?;
    }
    sync.emit_status(&app);
    Ok(TestResult {
        probe,
        status: sync.status_view(&state),
    })
}

/// 设置 / 输入主密码并同步一次。
///
/// - `confirm` 给了（第一次设置）：两次输入必须一致，且不少于 8 个字符；
/// - 云端已有数据（第二台电脑）：用输入的主密码解开，错了就明确提示，不保存；
/// - `remember`：在这台电脑上用 DPAPI 加密保存；关掉时删除保存的那份。
#[tauri::command]
pub async fn sync_set_password(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    password: String,
    confirm: Option<String>,
    remember: bool,
) -> Result<RunView, String> {
    if password.is_empty() {
        return Err("请输入主密码".to_owned());
    }
    if let Some(confirm) = &confirm {
        if confirm != &password {
            return Err("两次输入的主密码不一致".to_owned());
        }
        if password.chars().count() < service::MIN_MASTER_PASSWORD_CHARS {
            return Err(format!(
                "主密码至少 {} 个字符",
                service::MIN_MASTER_PASSWORD_CHARS
            ));
        }
    }
    let previous = sync.has_password();
    sync.set_password(Some(password.clone()));
    let result = sync.run_sync(&app, false, None).await;
    match &result {
        Ok(_) => {
            if remember {
                service::save_master_password(&state.store, &DpapiSealer, &password)
                    .map_err(text)?;
            } else {
                service::clear_master_password(&state.store).map_err(text)?;
            }
        }
        Err(_) => {
            // 没验证通过的主密码不留在内存里（这一轮失败前没有别的密码在用）。
            if !previous {
                sync.set_password(None);
            }
        }
    }
    sync.emit_status(&app);
    // 返回给界面的状态要在“记住”保存之后重新取：同步结束时那份还不知道记住了没有，
    // 界面拿它当最新状态会把开关显示成关。
    result.map(|mut view| {
        view.status_view = sync.status_view(&state);
        view
    })
}

/// “在这台电脑上记住主密码”开关。打开需要本次已经输入过主密码。
#[tauri::command]
pub async fn sync_remember_password(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    remember: bool,
) -> Result<StatusView, String> {
    if remember {
        let password = sync
            .password_for_remember()
            .ok_or_else(|| "请先输入主密码".to_owned())?;
        service::save_master_password(&state.store, &DpapiSealer, &password).map_err(text)?;
    } else {
        service::clear_master_password(&state.store).map_err(text)?;
    }
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// 修改主密码：旧密码解开云端包，新密码重新加密后上传。
#[tauri::command]
pub async fn sync_change_password(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    old_password: String,
    new_password: String,
) -> Result<StatusView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let handle = tokio::runtime::Handle::current();
    let job_app = app.clone();
    let data_dir = sync.data_dir();
    let (old, new) = (old_password, new_password.clone());
    tokio::task::spawn_blocking(move || {
        let state = job_app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        handle
            .block_on(ctx.change_master_password(&old, &new))
            .map_err(text)
    })
    .await
    .map_err(text)??;
    sync.replace_password(&state, &new_password);
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// 立即同步。`decision`：`continue` / `use_cloud` / `cancel`（只在防误删确认时用）。
#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    decision: Option<String>,
) -> Result<RunView, String> {
    let decision = match decision.as_deref() {
        None => None,
        Some("continue") => Some(DeletionDecision::Continue),
        Some("use_cloud") => Some(DeletionDecision::UseCloud),
        Some("cancel") => {
            // 取消不需要联网：待确认时本地和云端都还没动。
            let view = sync.cancel_pending(&state);
            sync.emit_status(&app);
            return Ok(view);
        }
        Some(other) => return Err(format!("不认识的选择：{other}")),
    };
    sync.run_sync(&app, false, decision).await
}

#[tauri::command]
pub async fn sync_auto_set(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    enabled: bool,
    interval_minutes: u32,
) -> Result<StatusView, String> {
    service::save_auto(
        &state.store,
        &AutoSettings {
            enabled,
            interval_minutes,
        },
    )
    .map_err(text)?;
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

#[tauri::command]
pub async fn sync_conflicts_list(
    app: AppHandle,
    sync: State<'_, SyncController>,
) -> Result<Vec<ConflictView>, String> {
    let data_dir = sync.data_dir();
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        ctx.engine().list_conflicts().map_err(text)
    })
    .await
    .map_err(text)?
}

/// 改用另一份 / 恢复被删的记录。之后正常参与下次同步。
#[tauri::command]
pub async fn sync_conflict_use_other(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    id: i64,
) -> Result<StatusView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let data_dir = sync.data_dir();
    let job_app = app.clone();
    tokio::task::spawn_blocking(move || {
        let state = job_app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        ctx.engine().use_conflict_other(id).map_err(text)
    })
    .await
    .map_err(text)??;
    // 本地数据变了：让自动同步在 30 秒后把它推上去。
    sync.notify_change();
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// “我知道了”：清掉一条冲突。
#[tauri::command]
pub async fn sync_conflict_dismiss(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    id: i64,
) -> Result<StatusView, String> {
    let data_dir = sync.data_dir();
    let ctx = build_context(&state, &data_dir).map_err(text)?;
    ctx.engine().dismiss_conflict(id).map_err(text)?;
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

#[tauri::command]
pub async fn sync_backups_list(
    app: AppHandle,
    sync: State<'_, SyncController>,
) -> Result<Vec<BackupDetail>, String> {
    let data_dir = sync.data_dir();
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        ctx.engine().list_backups().map_err(text)
    })
    .await
    .map_err(text)?
}

#[derive(Debug, Serialize)]
pub struct RestoreView {
    pub summary: String,
    pub safety_backup: String,
    pub status: StatusView,
}

/// 本地备份最多保留几份（1～200，默认 20）。调小以后，多出来的旧备份在下一次备份时才清理，
/// 改设置本身不删任何东西。
#[tauri::command]
pub async fn sync_backup_keep_set(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    keep: u32,
) -> Result<StatusView, String> {
    let keep = (keep as usize).clamp(1, zssh_core::store::settings::SYNC_BACKUP_KEEP_MAX);
    state
        .store
        .setting_set(
            zssh_core::store::settings::SYNC_BACKUP_KEEP_KEY,
            &keep.to_string(),
        )
        .map_err(text)?;
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

/// 恢复到一份本地备份。恢复前引擎会自动备份当前状态。
#[tauri::command]
pub async fn sync_backup_restore(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    path: String,
) -> Result<RestoreView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let data_dir = sync.data_dir();
    let job_app = app.clone();
    let report = tokio::task::spawn_blocking(move || {
        let state = job_app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        ctx.engine()
            .restore_backup(std::path::Path::new(&path))
            .map_err(text)
    })
    .await
    .map_err(text)??;
    // 恢复 / 导入可能删掉了主机（连带它的转发规则）：跑着的转发跟着停（复审 F2）
    crate::commands::forward::reconcile(&app);
    sync.notify_change();
    sync.emit_status(&app);
    Ok(RestoreView {
        summary: format!(
            "已恢复。恢复前的状态已自动备份为 {}",
            report.safety_backup.name
        ),
        safety_backup: report.safety_backup.name,
        status: sync.status_view(&state),
    })
}

/// 导出离线备份：主机、凭据（含密码和私钥）、代理、可同步的设置、known_hosts，用密码加密。
///
/// 先写临时文件再改名：写到一半失败（磁盘满、U 盘拔了）不会留下一个打不开的半截文件。
#[tauri::command]
pub async fn sync_offline_export(
    app: AppHandle,
    sync: State<'_, SyncController>,
    path: String,
    password: String,
) -> Result<String, String> {
    let data_dir = sync.data_dir();
    let password = zeroize::Zeroizing::new(password);
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        let bytes = ctx.engine().export_offline(&password).map_err(text)?;
        let target = std::path::PathBuf::from(&path);
        let temp = target.with_extension("zssh-tmp");
        std::fs::write(&temp, &bytes).map_err(|err| format!("写文件失败：{err}"))?;
        std::fs::rename(&temp, &target).map_err(|err| {
            let _ = std::fs::remove_file(&temp);
            format!("写文件失败：{err}")
        })?;
        Ok(format!("已导出到 {path}"))
    })
    .await
    .map_err(text)?
}

/// 导入离线备份。`mode` 是 `merge`（两边都留）或 `replace`（本机变成和文件一样）。
/// 导入前自动备份当前状态。
#[tauri::command]
pub async fn sync_offline_import(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
    path: String,
    password: String,
    mode: zssh_core::sync::offline::OfflineImportMode,
) -> Result<RestoreView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let data_dir = sync.data_dir();
    let password = zeroize::Zeroizing::new(password);
    let job_app = app.clone();
    let report = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|err| format!("读文件失败：{err}"))?;
        let state = job_app.state::<AppState>();
        let ctx = build_context(&state, &data_dir).map_err(text)?;
        ctx.engine()
            .import_offline(&bytes, &password, mode)
            .map_err(text)
    })
    .await
    .map_err(text)??;
    // 恢复 / 导入可能删掉了主机（连带它的转发规则）：跑着的转发跟着停（复审 F2）
    crate::commands::forward::reconcile(&app);
    sync.notify_change();
    sync.emit_status(&app);
    let applied = &report.applied;
    let mut summary = format!(
        "已导入：主机新增 {} / 更新 {} / 删除 {}，凭据新增 {}，代理新增 {}。",
        applied.hosts.inserted,
        applied.hosts.updated,
        applied.hosts.deleted,
        applied.identities.inserted,
        applied.proxies.inserted,
    );
    if report.conflicts > 0 {
        summary.push_str(&format!(
            "{} 条两边都改过，保留了较新的，另一份在冲突页。",
            report.conflicts
        ));
    }
    summary.push_str(&format!(
        "导入前的状态已自动备份为 {}",
        report.safety_backup.name
    ));
    Ok(RestoreView {
        summary,
        safety_backup: report.safety_backup.name,
        status: sync.status_view(&state),
    })
}

/// 重建同步基线：删掉损坏的基线，下次同步按首次同步处理（两边的数据都保留）。
#[tauri::command]
pub async fn sync_reset_baseline(
    app: AppHandle,
    state: State<'_, AppState>,
    sync: State<'_, SyncController>,
) -> Result<StatusView, String> {
    let _guard = sync.lock_for_exclusive()?;
    let data_dir = sync.data_dir();
    let ctx = build_context(&state, &data_dir).map_err(text)?;
    ctx.engine().reset_baseline().map_err(text)?;
    sync.refresh_baseline_flag(&state);
    sync.emit_status(&app);
    Ok(sync.status_view(&state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_input_debug_hides_the_password_and_tokens() {
        // 界面提交的连接信息带明文密码 / 令牌，Debug 输出不能带它们（R-8 第 11 节）。
        let input = ConfigInput {
            provider: "webdav".to_owned(),
            url: "https://dav.example.com/".to_owned(),
            directory: "zssh".to_owned(),
            username: "deploy".to_owned(),
            password: "s3cret-password".to_owned(),
            allow_insecure_http: false,
            endpoint: "https://s3.example.com".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "backup".to_owned(),
            access_key: "access-key-id".to_owned(),
            secret_key: "s3cret-access-key".to_owned(),
            prefix: "zssh".to_owned(),
            path_style: true,
            api_base: "https://api.github.com".to_owned(),
            token: "s3cret-token".to_owned(),
            gist_id: "abc".to_owned(),
        };
        let debug = format!("{input:?}");
        for secret in ["s3cret-password", "s3cret-access-key", "s3cret-token"] {
            assert!(!debug.contains(secret), "Debug 里不能有 {secret}：{debug}");
        }
        assert!(debug.contains("***"), "{debug}");
        // 非密码字段照常显示，方便排查。
        assert!(debug.contains("webdav"), "{debug}");
        assert!(debug.contains("deploy"), "{debug}");
    }
}
