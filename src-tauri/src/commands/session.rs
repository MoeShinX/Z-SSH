use std::sync::Arc;
use std::time::Duration;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zssh_core::ai::display::{DisplayFilter, DisplayOutput};
use zssh_core::ai::pty::TerminalState;
use zssh_core::ai::terminal::{SharedTerminalRing, TerminalRing};
use zssh_core::model::{Host, HostDraft, IdentityDraft, IdentityKind, ProxyMode};
use zssh_core::parse_quick_target;
use zssh_core::ssh::session_log::SessionLog;
use zssh_core::ssh::{
    AuthConfig, BoxFuture, ChainHop, Connection, ProxyConfig, RememberedSecret, ShellEvent,
    ShellEventStream, ShellSession, ShellWriter, DEFAULT_BUFFER_LIMIT,
};
use zssh_core::{Database, ZsshError};

use crate::events::emit_state;
use crate::interact::UiInteractor;
use crate::state::{
    AppState, CancelSignal, OpenSession, SessionCommand, SessionConnection, SessionEntry,
};

const COMMAND_BUFFER: usize = 32;
/// 收尾（关 shell、断连接）的时限：卡住也不能把会话任务拖死。
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// 一次"打开会话"要连的东西。
struct SessionSpec {
    /// 拨号链，最外侧的跳板机在前
    hops: Vec<ChainHop>,
    /// 池子的键和配置指纹；快速连接不进池，是 `None`
    pooled: Option<PooledKey>,
    /// 界面上的标签：主机名，快速连接是连接串
    label: String,
    /// 主机库里的主机 ID；快速连接是 `None`
    host_id: Option<i64>,
    /// 目标主机的登录用户名（工具桥 `list_sessions` 要显示）
    username: String,
}

struct PooledKey {
    host_id: i64,
    fingerprint: String,
}

/// 打开会话时可能出的两类结果。
enum OpenError {
    /// 用户把还在连接的会话关掉了
    Cancelled,
    Failed(ZsshError),
}

/// 用 `user@host:port` 直连一台主机并开一个 shell。
///
/// **立即返回**会话 ID：连接在后台进行，进度和结果走 `session_state` 事件
/// （设计文档第 7 节）。连接期间再调 `session_close` 等于取消。
#[tauri::command]
pub async fn session_open_quick(
    app: AppHandle,
    state: State<'_, AppState>,
    target: String,
    password: String,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
) -> Result<String, String> {
    let quick = parse_quick_target(&target).map_err(|err| err.to_string())?;

    // 快速连接不做跳板机，但全局的"默认代理"对它也生效
    let proxy = match state
        .store
        .default_proxy_id()
        .map_err(|err| err.to_string())?
    {
        Some(id) => Some(proxy_config(&state.store, id)?),
        None => None,
    };
    let label = match &proxy {
        Some(proxy) => format!("代理 {}", proxy.label()),
        None => format!("目标 {}", target.trim()),
    };

    let hop = ChainHop {
        label,
        host: quick.host,
        port: quick.port,
        username: quick.username.clone(),
        proxy,
        auth: AuthConfig::password(Some(password)),
        known_hosts: state.known_hosts.clone(),
        legacy_algorithms: false,
    };

    // 快速连接没有主机记录：不进池，"记住密码"也没地方存
    Ok(start_session(
        app,
        &state,
        SessionSpec {
            hops: vec![hop],
            pooled: None,
            label: target.trim().to_owned(),
            host_id: None,
            username: quick.username,
        },
        cols,
        rows,
        on_output,
    )
    .await)
}

/// 从主机库里取一台主机（地址、用户名，以及保险库里的密码）并开一个 shell。
///
/// **立即返回**会话 ID，语义和 [`session_open_quick`] 一样；凭据都在后端取，
/// 密码不经过前端。
#[tauri::command]
pub async fn session_open(
    app: AppHandle,
    state: State<'_, AppState>,
    host_id: i64,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
) -> Result<String, String> {
    let chain = build_chain(&state, host_id)?;
    let username = chain
        .hops
        .last()
        .map(|hop| hop.username.clone())
        .unwrap_or_default();
    Ok(start_session(
        app,
        &state,
        SessionSpec {
            hops: chain.hops,
            pooled: Some(PooledKey {
                host_id,
                fingerprint: chain.fingerprint,
            }),
            label: chain.label,
            host_id: Some(host_id),
            username,
        },
        cols,
        rows,
        on_output,
    )
    .await)
}

/// 在同一台主机的已有连接上再开一个 shell（分屏用），不重新握手和认证。
///
/// 和 `session_open` 一样**立即返回**：开通道在后台进行。
#[tauri::command]
pub async fn session_duplicate(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
) -> Result<String, String> {
    let Some((connection, host_id, label, username)) = state.duplicate_source(&session_id).await
    else {
        return Err(format!("会话不存在或者已经结束：{session_id}"));
    };

    let new_id = state.next_session_id();
    let cancel = Arc::new(CancelSignal::default());
    state
        .insert_connecting(new_id.clone(), Arc::clone(&cancel))
        .await;
    emit_state(&app, &new_id, "connecting", None);
    tauri::async_runtime::spawn(run_duplicate(
        app,
        new_id.clone(),
        connection,
        host_id,
        label,
        username,
        cols,
        rows,
        on_output,
        cancel,
    ));
    Ok(new_id)
}

/// 两个打开入口共用：登记会话、返回 ID，连接交给后台任务。
async fn start_session(
    app: AppHandle,
    state: &State<'_, AppState>,
    spec: SessionSpec,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
) -> String {
    let session_id = state.next_session_id();
    let cancel = Arc::new(CancelSignal::default());
    state
        .insert_connecting(session_id.clone(), Arc::clone(&cancel))
        .await;
    // 连接马上开始，进度（第几跳、认证中）和结果都走事件
    emit_state(&app, &session_id, "connecting", None);
    tauri::async_runtime::spawn(run_open(
        app,
        session_id.clone(),
        spec,
        cols,
        rows,
        on_output,
        cancel,
    ));
    session_id
}

/// 后台连接：拿连接（池里复用或者新建）→ 开 shell → 登记 → 起会话任务。
///
/// 全程可以被取消（连接期间用户把会话关了）：`select!` 掉这个 future，池子的
/// 占位会自己清理。
async fn run_open(
    app: AppHandle,
    session_id: String,
    spec: SessionSpec,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
    cancel: Arc<CancelSignal>,
) {
    let registry = Arc::clone(&app.state::<AppState>().prompts);
    let interactor = Arc::new(UiInteractor::new(
        app.clone(),
        registry,
        session_id.clone(),
        Arc::clone(&cancel),
    ));
    // 终端环形缓冲（工具桥 read_terminal 读它）：从 shell 打开那一刻起就旁路复制。
    let ring = TerminalRing::shared();

    let opened = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(OpenError::Cancelled),
        result = acquire_and_open(&app, &spec, cols, rows, interactor, Arc::clone(&ring)) => result,
    };

    let (connection, shell) = match opened {
        Ok(opened) => opened,
        Err(OpenError::Cancelled) => {
            finish_cancelled(&app, &session_id).await;
            return;
        }
        Err(OpenError::Failed(err)) => {
            fail_open(&app, &session_id, err).await;
            return;
        }
    };

    if connection.is_fresh() {
        if let Some(host_id) = spec.host_id {
            spawn_os_probe(app.clone(), host_id, Arc::clone(connection.connection()));
        }
    }

    // 用户在弹窗里勾了"保存"：连上之后才落库，免得存下一个错密码。
    // 池里复用的连接不重复存（它的密码在当初建它的时候已经存过了）。
    if connection.is_fresh() {
        if let Some(remembered) = connection.connection().remembered_secret() {
            if let Some(host_id) = spec.host_id {
                if let Err(err) =
                    save_remembered_secret(&app.state::<AppState>().store, host_id, remembered)
                {
                    warn!(%err, "保存密码 / 口令失败");
                }
            }
        }
    }

    register_and_run(
        app,
        session_id,
        connection,
        shell,
        spec.host_id,
        spec.label,
        spec.username,
        ring,
        on_output,
    )
    .await;
}

/// 主机的系统类型变了（或第一次探到）：`{ host_id, os }`。
pub const HOST_OS_EVENT: &str = "host-os";

#[derive(Clone, serde::Serialize)]
struct HostOsPayload {
    host_id: i64,
    os: &'static str,
}

/// 新连上一台主机后在后台探一次系统（主机列表的系统图标用）。失败、超时、认不出来都不管。
fn spawn_os_probe(
    app: AppHandle,
    host_id: i64,
    connection: Arc<zssh_core::ssh::connection::Connection>,
) {
    tauri::async_runtime::spawn(async move {
        let probe = connection
            .exec_capture(
                zssh_core::ssh::os_probe::OS_PROBE_COMMAND,
                Duration::from_secs(8),
                16 * 1024,
            )
            .await;
        let output = match probe {
            Ok(capture) if !capture.timed_out => capture.stdout,
            Ok(_) => return,
            Err(err) => {
                debug!(%err, "探测系统类型失败");
                return;
            }
        };
        let Some(os) = zssh_core::ssh::os_probe::classify(&output) else {
            return;
        };
        match app.state::<AppState>().store.host_os_set(host_id, os) {
            Ok(true) => {
                let _ = app.emit(HOST_OS_EVENT, HostOsPayload { host_id, os });
            }
            Ok(false) => {}
            Err(err) => warn!(%err, "记录系统类型失败"),
        }
    });
}

/// 复制会话的后台任务：在已有连接上开新 shell（只有开通道，没有握手和认证）。
#[allow(clippy::too_many_arguments)]
async fn run_duplicate(
    app: AppHandle,
    session_id: String,
    connection: SessionConnection,
    host_id: Option<i64>,
    label: String,
    username: String,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
    cancel: Arc<CancelSignal>,
) {
    let ring = TerminalRing::shared();
    let opened = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(OpenError::Cancelled),
        result = connection.connection().open_shell_with_buffer_limit(
            cols,
            rows,
            DEFAULT_BUFFER_LIMIT,
            Some(Arc::clone(&ring)),
        ) => result.map_err(OpenError::Failed),
    };

    let shell = match opened {
        Ok(shell) => shell,
        Err(OpenError::Cancelled) => {
            finish_cancelled(&app, &session_id).await;
            return;
        }
        Err(OpenError::Failed(err)) => {
            fail_open(&app, &session_id, err).await;
            return;
        }
    };

    register_and_run(
        app, session_id, connection, shell, host_id, label, username, ring, on_output,
    )
    .await;
}

/// 拿连接 + 开 shell。连接要么来自池子（同一台主机的第二个 shell 不再认证），
/// 要么新建（快速连接，或者池里没有）。
async fn acquire_and_open(
    app: &AppHandle,
    spec: &SessionSpec,
    cols: u16,
    rows: u16,
    interactor: Arc<UiInteractor>,
    ring: SharedTerminalRing,
) -> Result<(SessionConnection, ShellSession), OpenError> {
    let state = app.state::<AppState>();
    let connection = match &spec.pooled {
        Some(pooled) => {
            let guard = state
                .pool
                .acquire(
                    pooled.host_id,
                    &pooled.fingerprint,
                    spec.hops.clone(),
                    interactor,
                )
                .await
                .map_err(OpenError::Failed)?;
            SessionConnection::Pooled(guard)
        }
        None => {
            let connection = Connection::connect_chain(spec.hops.clone(), interactor)
                .await
                .map_err(OpenError::Failed)?;
            SessionConnection::Direct(Arc::new(connection))
        }
    };

    let shell = connection
        .connection()
        .open_shell_with_buffer_limit(cols, rows, DEFAULT_BUFFER_LIMIT, Some(ring))
        .await
        .map_err(OpenError::Failed)?;
    Ok((connection, shell))
}

/// 登记会话、发 connected、起会话任务。
///
/// 会话可能在"shell 开好了但还没登记"这一瞬间被关掉：登记失败就把刚开好的
/// shell 收掉，不发任何事件。
#[allow(clippy::too_many_arguments)]
async fn register_and_run(
    app: AppHandle,
    session_id: String,
    connection: SessionConnection,
    shell: ShellSession,
    host_id: Option<i64>,
    label: String,
    username: String,
    ring: SharedTerminalRing,
    on_output: Channel<InvokeResponseBody>,
) {
    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_BUFFER);
    let closing = Arc::new(CancelSignal::default());
    let log_label = label.clone();
    let open = OpenSession {
        commands: commands_tx,
        // 存一份流控门句柄：ack 和关闭要能绕开会话任务直接操作它。
        gate: shell.flow_handle(),
        connection: connection.clone(),
        host_id,
        label,
        username,
        ring,
        terminal: Arc::new(TerminalState::new()),
        closing: Arc::clone(&closing),
    };

    let promoted = app
        .state::<AppState>()
        .promote_to_open(&session_id, open)
        .await;
    if !promoted {
        close_shell_quietly(shell).await;
        debug!(%session_id, "连接期间会话被关了，收掉刚开好的 shell");
        return;
    }

    // 终端执行状态（AI-2c）：会话任务在转发路径上旁路喂它。
    let terminal = app.state::<AppState>().terminal_exec(&session_id).await;

    info!(%session_id, "会话已建立");
    emit_state(&app, &session_id, "connected", None);
    if let Some(host_id) = host_id {
        crate::commands::forward::auto_start_for_host(&app, host_id);
    }
    let log = open_session_log(&app, &log_label);
    tauri::async_runtime::spawn(run_session(
        app,
        session_id.clone(),
        connection,
        shell,
        commands_rx,
        closing,
        on_output,
        terminal,
        log,
    ));
}

/// 开了会话日志就为这个会话建一个日志文件；没开或建不了（目录没权限）返回 `None`，会话照常。
fn open_session_log(app: &AppHandle, label: &str) -> Option<SessionLog> {
    let state = app.state::<AppState>();
    if !crate::commands::settings::session_log_enabled(&state) {
        return None;
    }
    let dir = match crate::commands::settings::session_log_dir(app, &state) {
        Ok(dir) => dir,
        Err(err) => {
            warn!(%err, "找不到会话日志目录，这个会话不记日志");
            return None;
        }
    };
    match SessionLog::create(&dir, label, &zssh_core::ssh::session_log::local_stamp()) {
        Ok(log) => Some(log),
        Err(err) => {
            warn!(%err, dir = %dir.display(), "建不了会话日志文件，这个会话不记日志");
            None
        }
    }
}

/// 连接被取消：条目已经被 `session_close` 摘掉了，这里只清理挂着的提问。
async fn finish_cancelled(app: &AppHandle, session_id: &str) {
    app.state::<AppState>().prompts.cancel_session(session_id);
    debug!(%session_id, "连接被取消");
}

/// 连接失败：如果条目还是"连接中"（没被取消），摘掉并发一条 disconnected
/// 带上原因，界面在 pane 里显示。
async fn fail_open(app: &AppHandle, session_id: &str, err: ZsshError) {
    let state = app.state::<AppState>();
    state.prompts.cancel_session(session_id);
    if state.take_connecting(session_id).await {
        let reason = err.to_string();
        info!(%session_id, %reason, "连接失败");
        emit_state(app, session_id, "disconnected", Some(reason));
    }
}

/// 把刚开好、还没登记的 shell 收掉（关通道，带超时）。
async fn close_shell_quietly(mut shell: ShellSession) {
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, shell.close()).await;
}

/// 从主机库把拨号链拼出来：最外侧的跳板机在前，目标是最后一跳（设计文档 4.2）。
///
/// 每一跳用它自己那条主机记录的认证方式、凭据和老算法开关；代理按主机的
/// proxy_mode（跟随默认 / 不用 / 指定）解析。同时算一个**配置指纹**给连接池：
/// 链上任何一条主机记录或者代理变了，指纹就对不上，池里那条连接不再复用。
pub fn build_chain(state: &AppState, host_id: i64) -> Result<Chain, String> {
    let hosts = state
        .store
        .host_chain(host_id)
        .map_err(|err| err.to_string())?;
    let default_proxy_id = state
        .store
        .default_proxy_id()
        .map_err(|err| err.to_string())?;
    let total = hosts.len();

    let mut hops = Vec::with_capacity(total);
    let mut fingerprint_parts = Vec::with_capacity(total);
    for (index, host) in hosts.iter().enumerate() {
        let login = state
            .store
            .host_login(host.id)
            .map_err(|err| err.to_string())?;
        let proxy = resolve_proxy(&state.store, host, default_proxy_id)?;
        let is_target = index + 1 == total;

        // 报错和进度里显示的标签：走代理的第一跳写清代了哪个代理
        let label = match (&proxy, is_target) {
            (Some(proxy), _) if index == 0 => format!("代理 {}", proxy.label()),
            (_, true) => format!("目标 {}", host.name),
            _ => format!("跳板机 {}", host.name),
        };

        fingerprint_parts.push(hop_fingerprint(host, &proxy));
        hops.push(ChainHop {
            label,
            host: login.host,
            port: login.port,
            username: login.username,
            proxy,
            auth: login.auth,
            known_hosts: state.known_hosts.clone(),
            legacy_algorithms: login.legacy_algorithms,
        });
    }

    let label = hosts
        .last()
        .map(|host| host.name.clone())
        .unwrap_or_default();
    Ok(Chain {
        hops,
        fingerprint: fingerprint_parts.join("\n"),
        label,
    })
}

/// 拨号链 + 配置指纹 + 目标标签。
pub struct Chain {
    pub hops: Vec<ChainHop>,
    pub fingerprint: String,
    pub label: String,
}

/// 一跳的配置指纹：主机记录里的连接相关内容 + 解析出来的代理。
///
/// `updated_at` 覆盖主机记录的每次保存；代理自己的字段单列，因为改代理记录不会
/// 动主机的 `updated_at`。
fn hop_fingerprint(host: &Host, proxy: &Option<ProxyConfig>) -> String {
    let proxy_part = match proxy {
        Some(proxy) => format!(
            "{:?}:{}:{}:{}",
            proxy.kind,
            proxy.host,
            proxy.port,
            proxy.username.as_deref().unwrap_or("")
        ),
        None => "direct".to_owned(),
    };
    format!(
        "{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}",
        host.id,
        host.updated_at,
        host.host,
        host.port,
        host.username,
        host.auth_method,
        host.identity_id,
        host.jump_host_id,
        host.proxy_mode,
        host.legacy_algorithms,
        proxy_part
    )
}

/// 主机用哪个代理：跟随默认 / 不用 / 指定。
fn resolve_proxy(
    store: &Database,
    host: &Host,
    default_proxy_id: Option<i64>,
) -> Result<Option<ProxyConfig>, String> {
    let proxy_id = match host.proxy_mode {
        ProxyMode::None => return Ok(None),
        ProxyMode::Default => match default_proxy_id {
            Some(id) => id,
            None => return Ok(None),
        },
        ProxyMode::Custom => host
            .proxy_id
            .ok_or_else(|| format!("主机「{}」选了指定代理，但没有选是哪个代理", host.name))?,
    };
    Ok(Some(proxy_config(store, proxy_id)?))
}

/// 把库里的代理取成拨号用的配置（密码已经从保险库解出来了）。
fn proxy_config(store: &Database, proxy_id: i64) -> Result<ProxyConfig, String> {
    let login = store.proxy_login(proxy_id).map_err(|err| err.to_string())?;
    Ok(ProxyConfig {
        kind: login.kind,
        host: login.host,
        port: login.port,
        username: login.username,
        password: login.password,
    })
}

/// 拿一条池里的连接（SFTP 标签用，M3-2 第 1 节）。
///
/// 和终端同一套规则：同一台主机的已认证连接直接复用（不重新握手）。连接中需要
/// 用户输入（密码、指纹）时走正常弹窗，`session_id` 用调用方给的（SFTP 标签是
/// 它自己的 sftp_id，关闭时要拿来取消挂着的提问）。
pub async fn pooled_connection(
    app: &AppHandle,
    session_id: &str,
    host_id: i64,
) -> Result<(SessionConnection, Arc<CancelSignal>), String> {
    let state = app.state::<AppState>();
    let chain = build_chain(&state, host_id)?;
    let cancel = Arc::new(CancelSignal::default());
    let interactor = Arc::new(UiInteractor::new(
        app.clone(),
        Arc::clone(&state.prompts),
        session_id.to_owned(),
        Arc::clone(&cancel),
    ));
    let guard = state
        .pool
        .acquire(host_id, &chain.fingerprint, chain.hops, interactor)
        .await
        .map_err(|err| err.to_string())?;
    Ok((SessionConnection::Pooled(guard), cancel))
}

/// 把按键数据交给远端。
///
/// 按键很小，直接走普通命令，不进 Channel（设计文档 5.3）。
#[tauri::command]
pub async fn term_write(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    session_of(&state, &session_id)
        .await?
        .commands
        .send(SessionCommand::Write(data.into_bytes()))
        .await
        .map_err(|_| format!("会话已经结束：{session_id}"))
}

#[tauri::command]
pub async fn term_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    session_of(&state, &session_id)
        .await?
        .commands
        .send(SessionCommand::Resize { cols, rows })
        .await
        .map_err(|_| format!("会话已经结束：{session_id}"))
}

/// 前端确认已经渲染完的字节数，转给这个会话的流控门。
///
/// 直接调 `FlowGate::on_ack`，**不进命令队列**：设计文档 5.2 的硬规则。会话任务
/// 卡在写通道上时，队列里的 ack 永远轮不到，读取任务不恢复就死锁了。
///
/// 前端每渲染约 64KB 报一次（设计文档 5.2）；报少了不会出事，报多了也只是
/// 让后端晚一点再恢复读。
#[tauri::command]
pub async fn term_ack(
    state: State<'_, AppState>,
    session_id: String,
    bytes: u32,
) -> Result<(), String> {
    let handle = session_of(&state, &session_id).await?;
    handle.gate.on_ack(bytes as usize);
    Ok(())
}

/// 终端当前所在的目录（SFTP 面板"跟随终端目录"用，M3-3 第 3 节）。
///
/// 走独立的 exec 通道读服务器上的 `/proc/<pid>/cwd`，**不往终端里写任何东西**。
/// 取不到（不是 Linux、没有 /proc、同一连接上的终端对不上号、会话还没连上）一律
/// 返回 `None`，前端安静地退回家目录，不弹错误。
#[tauri::command]
pub async fn session_cwd(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<String>, String> {
    let Some((connection, ordinal, live)) = state.shell_position(&session_id).await else {
        return Ok(None);
    };
    let shells = zssh_core::ssh::probe_shell_cwds(&connection).await;
    Ok(zssh_core::ssh::pick_shell(&shells, ordinal, live).map(|shell| shell.cwd.clone()))
}

/// 关闭会话；会话还在连接中就是取消（中断连接，正在等的提问一起取消）。
#[tauri::command]
pub async fn session_close(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    // 先摘掉句柄：新的 term_write 立刻拿不到这个会话。
    let entry = state
        .remove(&session_id)
        .await
        .ok_or_else(|| format!("会话不存在：{session_id}"))?;

    match entry {
        SessionEntry::Connecting { cancel } => {
            // 连接中关掉 = 取消：后台任务醒来自己收尾
            cancel.cancel();
            let cancelled = state.prompts.cancel_session(&session_id);
            info!(%session_id, cancelled, "连接中的会话被取消");
            emit_state(&app, &session_id, "disconnected", Some("已取消".to_owned()));
        }
        SessionEntry::Open(session) => {
            // 先直接关流控门（设计文档 5.2 的硬规则）：读任务可能正卡在等 ack 上，
            // 也可能因为写不进去而卡住会话任务。关门之后转发任务退出，读任务也被
            // 丢弃缓冲叫醒，整条连接的事件循环就能恢复，后面的 Close 才轮得到。
            session.gate.close();
            // 还在等用户回答的弹窗也要收掉，不留挂住的 oneshot
            state.prompts.cancel_session(&session_id);
            // 关闭信号不排队：会话任务卡在写不出去的 shell 上时也能马上收到（C16）。
            session.close_task();
        }
    }
    Ok(())
}

async fn session_of(
    state: &State<'_, AppState>,
    session_id: &str,
) -> Result<crate::state::SessionHandle, String> {
    state
        .handle(session_id)
        .await
        .ok_or_else(|| format!("会话不存在或者还没连上：{session_id}"))
}

/// 前端换了一页：把上一页留下的会话全部收掉（设计见 `AppState::reset_sessions`）。
///
/// 每次界面启动（`App` 首次挂载）调用一次。`StrictMode` 下会挂载两次，所以这个
/// 命令必须幂等：第二次调用时第一次挂载开的会话通常还没连上（还在 `Connecting`），
/// 照样会被取消——这是对的，它们的 `Channel` 属于上一页，新的前端认领不了。
#[tauri::command]
pub async fn sessions_reset(
    state: State<'_, AppState>,
    registry: State<'_, crate::commands::stats::StatsRegistry>,
) -> Result<usize, String> {
    let closed = state.reset_sessions().await;
    // 旧页面的状态栏订阅也收掉：订阅 ID 已经没有前端认领，留着只会白采集（M1-10b）。
    let stopped = registry.reset().await;
    // 前端刷新不会走到 session_close：关掉几个要能在日志里对上。
    info!(closed, stopped, "前端重新加载，已关闭上一页留下的会话");
    Ok(closed)
}

/// 会话任务写 shell 的那一半（单测里换成假的，模拟"远端不再补充窗口"）。
trait SessionWriter: Sync {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, Result<(), String>>;
    fn resize(&self, cols: u16, rows: u16) -> BoxFuture<'_, Result<(), String>>;
    fn ack(&self, bytes: u32);
}

impl SessionWriter for ShellWriter {
    fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            ShellWriter::write(self, data)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn resize(&self, cols: u16, rows: u16) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            ShellWriter::resize(self, cols, rows)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn ack(&self, bytes: u32) {
        ShellWriter::ack(self, bytes);
    }
}

/// 会话任务读 shell 输出的那一半。
trait SessionEvents: Send {
    fn next_event(&mut self) -> BoxFuture<'_, Option<ShellEvent>>;
}

impl SessionEvents for ShellEventStream {
    fn next_event(&mut self) -> BoxFuture<'_, Option<ShellEvent>> {
        Box::pin(self.next())
    }
}

/// 会话任务：独占 shell 的读写两半，输出转给 Channel，指令从 mpsc 来。
///
/// 攒批和两级缓冲在 `zssh-core` 的 shell 模块里，这里只负责把事件投给前端、
/// 把前端的 ack 还给流控门。
#[allow(clippy::too_many_arguments)]
async fn run_session(
    app: AppHandle,
    session_id: String,
    connection: SessionConnection,
    shell: ShellSession,
    mut commands: mpsc::Receiver<SessionCommand>,
    closing: Arc<CancelSignal>,
    on_output: Channel<InvokeResponseBody>,
    terminal: Option<Arc<TerminalState>>,
    log: Option<SessionLog>,
) {
    let (writer, mut events) = shell.split();
    // 显示过滤器（AI-2d 第 2 节）：包装脚本的回显 / 标记行只在显示路径上过滤，
    // 环形缓冲和 TerminalState 拿到的仍是原始数据（事件流上游旁路喂它们）。
    let mut display_filter = DisplayFilter::new();
    let mut output = |data: Vec<u8>| {
        // 日志记的是过滤后的显示数据：用户看到什么就记什么，AI 的包装脚本不进日志
        if let Some(log) = &log {
            log.write(&data);
        }
        on_output
            .send(InvokeResponseBody::Raw(data))
            .map_err(|err| format!("输出通道已关闭：{err}"))
    };
    let reason = session_loop(
        &session_id,
        &writer,
        &mut events,
        &mut commands,
        &closing,
        terminal.as_deref(),
        &mut display_filter,
        &mut output,
    )
    .await;
    // 先关队列：还卡在等空位的发送方（按键、AI 命令）马上拿到"会话已结束"，不用陪着收尾等。
    drop(commands);

    // 收尾：压着的数据放行（会话断了也不能吞用户的输出）。
    let tail = display_filter.flush();
    if !tail.data.is_empty() {
        if let Some(log) = &log {
            log.write(&tail.data);
        }
        let _ = on_output.send(InvokeResponseBody::Raw(tail.data));
    }
    // 关日志（写线程把剩下的写完）放到后台：别让慢磁盘拖住会话收尾
    if let Some(log) = log {
        tokio::task::spawn_blocking(move || drop(log));
    }

    // 会话到这里已经结束了：先摘掉、告诉界面"已断开"，再做下面可能要等的清理。
    // 以前清理排在前面——服务器在重启 / 关机时，给它发 EOF / CLOSE（以及快速连接的断开）
    // 可能要等到超时（各最长 5 秒），界面就晚几秒才显示断开。
    // 正常关闭时 session_close 已经摘掉了，这里兜住远端断开的情况。
    app.state::<AppState>().remove(&session_id).await;
    info!(%session_id, %reason, "会话结束");
    emit_state(&app, &session_id, "disconnected", Some(reason));

    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, writer.close()).await;
    // 池里的连接不在这里断：最后一个会话释放引用之后，池子按空闲时长收。
    // 快速连接的连接没有池子管：只有自己用的时候（没有别的 duplicate 会话）
    // 断开它，别人还在用就交给最后一个。
    if let SessionConnection::Direct(connection) = &connection {
        if Arc::strong_count(connection) == 1 {
            let _ = tokio::time::timeout(CLEANUP_TIMEOUT, connection.disconnect()).await;
        }
    }
    // 丢掉事件流：它会关掉流控门并结束读写任务。
    drop(events);
    // 引用计数最后再减：池里的连接从"所有会话都结束了"这一刻开始算空闲。
    drop(connection);
}

/// 过滤后的显示数据发给界面；被吞掉的字节替界面 ack 掉（界面只 ack 它收到
/// 的字节，谁都没 ack 的话流控门会一直停着）。
fn emit_display(
    out: DisplayOutput,
    writer: &impl SessionWriter,
    output: &mut (dyn FnMut(Vec<u8>) -> Result<(), String> + Send),
) -> Result<(), String> {
    if out.suppressed > 0 {
        writer.ack(out.suppressed as u32);
    }
    if !out.data.is_empty() {
        output(out.data)?;
    }
    Ok(())
}

/// 等正在进行的写入；没有就永远不返回（`select!` 里这一支就不会被选中）。
async fn finish_in_flight(in_flight: &mut Option<BoxFuture<'_, ()>>) {
    match in_flight {
        Some(write) => write.await,
        None => std::future::pending().await,
    }
}

/// 会话任务的主循环，返回结束原因。
///
/// 写入（按键、AI 命令、改窗口大小）一次只进行一个，作为"在途"的 future 和关闭信号、
/// 读事件、过滤计时放在同一个 `select!` 里（审计 C16）：远端不再补充 SSH 窗口时写入会
/// 一直等，以前整个循环卡在 `write().await` 上，Close 和输出都轮不到。在途写入没完成之前
/// 不取命令队列——写入顺序不能乱，队列满了发送方等着，这本身就是背压。
#[allow(clippy::too_many_arguments)]
async fn session_loop<W: SessionWriter, E: SessionEvents>(
    session_id: &str,
    writer: &W,
    events: &mut E,
    commands: &mut mpsc::Receiver<SessionCommand>,
    closing: &CancelSignal,
    terminal: Option<&TerminalState>,
    display_filter: &mut DisplayFilter,
    output: &mut (dyn FnMut(Vec<u8>) -> Result<(), String> + Send),
) -> String {
    // 兜底计时：压着的可疑尾巴等太久就放行（不能吞用户的输出）。
    let mut filter_tick = tokio::time::interval(Duration::from_millis(250));
    filter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut in_flight: Option<BoxFuture<'_, ()>> = None;

    loop {
        tokio::select! {
            // 关闭不排在队列和写入后面：关掉会话时丢掉在途写入（没写完的 AI 命令，调用方会
            // 收到"写到一半会话关了"）。
            _ = closing.cancelled() => break "会话已关闭".to_owned(),
            () = finish_in_flight(&mut in_flight) => in_flight = None,
            event = events.next_event() => match event {
                Some(ShellEvent::Data(bytes)) => {
                    // AI-2c：旁路喂终端执行状态（备用屏幕跟踪 + 提示符尾部）。
                    if let Some(terminal) = terminal {
                        terminal.feed(&bytes);
                    }
                    // AI-2d：发给界面的数据先过显示过滤器（AI 命令的包装脚本不显示）。
                    if let Err(reason) = emit_display(display_filter.push(&bytes), writer, output) {
                        break reason;
                    }
                }
                // 退出码 M1 不上报，shell 结束按 Closed 处理。
                Some(ShellEvent::Exit(status)) => debug!(%session_id, ?status, "远端进程退出"),
                Some(ShellEvent::Closed) | None => break "远端关闭了连接".to_owned(),
            },
            command = commands.recv(), if in_flight.is_none() => match command {
                Some(SessionCommand::Write(data)) => {
                    in_flight = Some(Box::pin(async move {
                        if let Err(err) = writer.write(&data).await {
                            debug!(%session_id, %err, "写入 shell 失败");
                        }
                    }));
                }
                Some(SessionCommand::WriteAiPayload { payload, marker, command, slot }) => {
                    // 真正写之前最后核对一次（审计 C04）：排队期间点了停止 / 移出了目标，
                    // 这条旧命令就丢掉，调用方拿到原因。
                    let Some(claim) = slot.claim() else {
                        debug!(%session_id, "AI 命令在排队期间失效，没有写进终端");
                        continue;
                    };
                    // 先让过滤器记住这条命令，再写（回显紧跟着写入）。
                    let armed = display_filter.arm(
                        &marker,
                        &String::from_utf8_lossy(&payload),
                        &command,
                    );
                    if let Err(reason) = emit_display(armed, writer, output) {
                        break reason;
                    }
                    in_flight = Some(Box::pin(async move {
                        let written = writer.write(&payload).await;
                        if let Err(err) = &written {
                            debug!(%session_id, %err, "写入 shell 失败");
                        }
                        claim.finish(written);
                    }));
                }
                Some(SessionCommand::AiCommandTimedOut { marker }) => {
                    // AI-2e：超时返回之后过滤器退出完整过滤，用户接着用终端打字
                    // 不会被压住；这条命令迟到的结束标记行仍会丢掉。
                    if let Err(reason) = emit_display(display_filter.release(&marker), writer, output) {
                        break reason;
                    }
                }
                Some(SessionCommand::Resize { cols, rows }) => {
                    in_flight = Some(Box::pin(async move {
                        if let Err(err) = writer.resize(cols, rows).await {
                            debug!(%session_id, %err, "调整窗口大小失败");
                        }
                    }));
                }
                Some(SessionCommand::Close) | None => break "会话已关闭".to_owned(),
            },
            _ = filter_tick.tick() => {
                if let Err(reason) = emit_display(
                    display_filter.poll_at(std::time::Instant::now()),
                    writer,
                    output,
                ) {
                    break reason;
                }
            }
        }
    }
}

/// 把用户在弹窗里勾了"保存"的密码 / 口令写进保险库。
///
/// - 密码：主机已经有密码凭据就更新它，没有就新建一条并挂到主机上
/// - 口令：主机的私钥凭据是文件就更新它的密文；是导入的私钥就把口令写进信封
fn save_remembered_secret(
    store: &Database,
    host_id: i64,
    remembered: &RememberedSecret,
) -> Result<(), String> {
    use zssh_core::ssh::PromptKind;

    let host = store.host_get(host_id).map_err(|err| err.to_string())?;
    let linked = linked_identity(store, &host)?;

    match remembered.kind {
        PromptKind::Password => {
            match linked.filter(|identity| identity.kind == IdentityKind::Password) {
                Some(identity) => {
                    store
                        .identity_save(&IdentityDraft {
                            id: Some(identity.id),
                            name: identity.name,
                            kind: IdentityKind::Password,
                            key_path: None,
                            secret: Some(remembered.secret.clone()),
                            passphrase: None,
                        })
                        .map_err(|err| err.to_string())?;
                }
                None => {
                    let created = store
                        .identity_save(&IdentityDraft {
                            id: None,
                            name: format!("{} 的密码", host.name),
                            kind: IdentityKind::Password,
                            key_path: None,
                            secret: Some(remembered.secret.clone()),
                            passphrase: None,
                        })
                        .map_err(|err| err.to_string())?;
                    let mut draft = host_to_draft(&host);
                    draft.identity_id = Some(created.id);
                    store.host_save(&draft).map_err(|err| err.to_string())?;
                }
            }
            info!(%host_id, "已把密码存进保险库");
        }

        PromptKind::Passphrase => match linked {
            Some(identity) if identity.kind == IdentityKind::KeyFile => {
                store
                    .identity_save(&IdentityDraft {
                        id: Some(identity.id),
                        name: identity.name,
                        kind: IdentityKind::KeyFile,
                        key_path: identity.key_path,
                        secret: Some(remembered.secret.clone()),
                        passphrase: None,
                    })
                    .map_err(|err| err.to_string())?;
                info!(%host_id, "已把私钥口令存进保险库");
            }
            Some(identity) if identity.kind == IdentityKind::KeyInline => {
                store
                    .identity_set_inline_passphrase(identity.id, &remembered.secret)
                    .map_err(|err| err.to_string())?;
                info!(%host_id, "已把口令写进导入的私钥凭据");
            }
            _ => debug!(%host_id, "没有可以存口令的私钥凭据"),
        },

        // keyboard-interactive 的答案（比如 MFA 验证码）不保存
        PromptKind::KeyboardInteractive => {}
    }
    Ok(())
}

/// 主机挂着的凭据，没有就是 None。
fn linked_identity(
    store: &Database,
    host: &Host,
) -> Result<Option<zssh_core::model::Identity>, String> {
    let Some(identity_id) = host.identity_id else {
        return Ok(None);
    };
    let identities = store.identity_list().map_err(|err| err.to_string())?;
    Ok(identities.into_iter().find(|item| item.id == identity_id))
}

/// 主机记录转成保存用的草稿。
fn host_to_draft(host: &Host) -> HostDraft {
    HostDraft {
        id: Some(host.id),
        group_id: host.group_id,
        name: host.name.clone(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: host.auth_method,
        identity_id: host.identity_id,
        proxy_id: host.proxy_id,
        jump_host_id: host.jump_host_id,
        proxy_mode: host.proxy_mode,
        tags: host.tags.clone(),
        sort: host.sort,
        legacy_algorithms: host.legacy_algorithms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use tokio::sync::{Notify, Semaphore};
    use zssh_core::ai::bridge::terminal_write::{deliver_ai_write, WRITE_INTERRUPTED};
    use zssh_core::ssh::connection::ExecGuard;

    /// 假的写半边：每次写入要先拿到一个许可——不给许可就模拟"远端不再补充窗口"，写入一直等。
    struct StuckWriter {
        permits: Semaphore,
        started: Mutex<Vec<Vec<u8>>>,
        written: Mutex<Vec<Vec<u8>>>,
    }

    impl StuckWriter {
        fn new() -> StuckWriter {
            StuckWriter {
                permits: Semaphore::new(0),
                started: Mutex::new(Vec::new()),
                written: Mutex::new(Vec::new()),
            }
        }

        fn started(&self) -> usize {
            self.started.lock().map(|w| w.len()).unwrap_or(0)
        }

        fn written(&self) -> Vec<Vec<u8>> {
            self.written.lock().map(|w| w.clone()).unwrap_or_default()
        }
    }

    impl SessionWriter for StuckWriter {
        fn write<'a>(&'a self, data: &'a [u8]) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if let Ok(mut started) = self.started.lock() {
                    started.push(data.to_vec());
                }
                let permit = self
                    .permits
                    .acquire()
                    .await
                    .map_err(|_| "写入通道关了".to_owned())?;
                permit.forget();
                if let Ok(mut written) = self.written.lock() {
                    written.push(data.to_vec());
                }
                Ok(())
            })
        }

        fn resize(&self, _cols: u16, _rows: u16) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn ack(&self, _bytes: u32) {}
    }

    impl SessionEvents for mpsc::Receiver<ShellEvent> {
        fn next_event(&mut self) -> BoxFuture<'_, Option<ShellEvent>> {
            Box::pin(self.recv())
        }
    }

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
                Err("用户点了停止，这次操作已取消".to_owned())
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
            self.invalid.load(Ordering::SeqCst)
        }
    }

    /// 起一个会话任务主循环（假的读写两半），返回它的各个把手。
    struct Harness {
        writer: Arc<StuckWriter>,
        events: mpsc::Sender<ShellEvent>,
        commands: mpsc::Sender<SessionCommand>,
        closing: Arc<CancelSignal>,
        shown: Arc<Mutex<Vec<u8>>>,
        task: tokio::task::JoinHandle<String>,
    }

    fn start() -> Harness {
        let writer = Arc::new(StuckWriter::new());
        let (events_tx, mut events_rx) = mpsc::channel::<ShellEvent>(8);
        let (commands_tx, mut commands_rx) = mpsc::channel::<SessionCommand>(COMMAND_BUFFER);
        let closing = Arc::new(CancelSignal::default());
        let shown = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let writer = Arc::clone(&writer);
            let closing = Arc::clone(&closing);
            let shown = Arc::clone(&shown);
            tokio::spawn(async move {
                let mut filter = DisplayFilter::new();
                let mut output = |data: Vec<u8>| {
                    if let Ok(mut shown) = shown.lock() {
                        shown.extend_from_slice(&data);
                    }
                    Ok(())
                };
                session_loop(
                    "sess-test",
                    writer.as_ref(),
                    &mut events_rx,
                    &mut commands_rx,
                    &closing,
                    None,
                    &mut filter,
                    &mut output,
                )
                .await
            })
        };
        Harness {
            writer,
            events: events_tx,
            commands: commands_tx,
            closing,
            shown,
            task,
        }
    }

    async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("等不到：{what}"));
    }

    fn ai_payload(
        command: &str,
        marker: &str,
    ) -> impl FnOnce(zssh_core::ai::bridge::terminal_write::AiWriteSlot) -> SessionCommand {
        let command = command.to_owned();
        let marker = marker.to_owned();
        move |slot| SessionCommand::WriteAiPayload {
            payload: format!("{command}\r").into_bytes(),
            marker,
            command,
            slot,
        }
    }

    #[tokio::test]
    async fn close_preempts_a_write_stuck_waiting_for_the_ssh_window() {
        // C16：远端不再补充窗口，写入一直等；以前 Close 排在队列里永远轮不到。
        let h = start();
        h.commands
            .send(SessionCommand::Write(b"ls\r".to_vec()))
            .await
            .expect("入队");
        wait_until("写入开始", || h.writer.started() == 1).await;
        h.closing.cancel();
        let reason = tokio::time::timeout(Duration::from_secs(2), h.task)
            .await
            .expect("关闭要抢占卡住的写入")
            .expect("不 panic");
        assert_eq!(reason, "会话已关闭");
        assert!(h.writer.written().is_empty(), "那次写入没有完成");
    }

    #[tokio::test]
    async fn output_keeps_flowing_while_a_write_is_stuck() {
        // 写入卡住时读事件照常处理：以前整个循环停在 write().await 上，输出也跟着停。
        let h = start();
        h.commands
            .send(SessionCommand::Write(b"x".to_vec()))
            .await
            .expect("入队");
        wait_until("写入开始", || h.writer.started() == 1).await;
        h.events
            .send(ShellEvent::Data(b"hello".to_vec()))
            .await
            .expect("推输出");
        let shown = Arc::clone(&h.shown);
        wait_until("输出转给界面", move || {
            shown.lock().map(|s| s.ends_with(b"hello")).unwrap_or(false)
        })
        .await;
        h.closing.cancel();
        let _ = h.task.await;
    }

    #[tokio::test]
    async fn writes_stay_in_order_after_a_stuck_write_resumes() {
        let h = start();
        for chunk in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            h.commands
                .send(SessionCommand::Write(chunk))
                .await
                .expect("入队");
        }
        wait_until("第一次写入开始", || h.writer.started() >= 1).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(h.writer.started(), 1, "在途写入没完成之前不取下一条");
        h.writer.permits.add_permits(3);
        let writer = Arc::clone(&h.writer);
        wait_until("三次写入完成", move || writer.written().len() == 3).await;
        assert_eq!(
            h.writer.written(),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        h.closing.cancel();
        let _ = h.task.await;
    }

    #[tokio::test]
    async fn a_stale_ai_command_queued_behind_a_stuck_write_is_never_written() {
        // C04 终端链路：AI 命令排在一个卡住的写入后面，排队期间用户点了停止。之后写入恢复，
        // 会话任务取到这条旧命令也不能写出去；调用方在停止时就拿到原因，不用等写入恢复。
        let h = start();
        h.commands
            .send(SessionCommand::Write(b"user-typing".to_vec()))
            .await
            .expect("入队");
        wait_until("用户的写入开始", || h.writer.started() == 1).await;

        let guard = Arc::new(FakeGuard::default());
        let deliver = {
            let commands = h.commands.clone();
            let guard: Arc<dyn ExecGuard> = guard.clone();
            tokio::spawn(async move {
                deliver_ai_write(&commands, guard, ai_payload("rm -f /tmp/x", "m1")).await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        guard.invalidate();
        let result = tokio::time::timeout(Duration::from_secs(2), deliver)
            .await
            .expect("停止后调用方要立刻拿到结果")
            .expect("不 panic");
        assert!(result.expect_err("已停止").contains("停止"));

        // 写入恢复：队列里那条旧 AI 命令被取出、认领失败、丢掉。
        h.writer.permits.add_permits(5);
        h.commands
            .send(SessionCommand::Write(b"after".to_vec()))
            .await
            .expect("入队");
        let writer = Arc::clone(&h.writer);
        wait_until("后面的写入完成", move || {
            writer.written().last().map(|w| w.as_slice()) == Some(b"after".as_slice())
        })
        .await;
        assert_eq!(
            h.writer.written(),
            vec![b"user-typing".to_vec(), b"after".to_vec()],
            "停止之后排着的 AI 命令不能写进终端"
        );
        h.closing.cancel();
        let _ = h.task.await;
    }

    #[tokio::test]
    async fn a_valid_ai_command_is_written_and_reported() {
        let h = start();
        h.writer.permits.add_permits(1);
        let guard: Arc<dyn ExecGuard> = Arc::new(FakeGuard::default());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            deliver_ai_write(&h.commands, guard, ai_payload("echo hi", "m2")),
        )
        .await
        .expect("要写完");
        assert_eq!(result, Ok(()));
        assert_eq!(h.writer.written(), vec![b"echo hi\r".to_vec()]);
        h.closing.cancel();
        let _ = h.task.await;
    }

    #[tokio::test]
    async fn closing_while_an_ai_command_is_being_written_reports_an_interrupted_write() {
        // 已经认领、正在写（窗口卡住）时关闭会话：调用方要知道"可能写进去一部分"，不能当没写。
        let h = start();
        let guard: Arc<dyn ExecGuard> = Arc::new(FakeGuard::default());
        let deliver = {
            let commands = h.commands.clone();
            tokio::spawn(async move {
                deliver_ai_write(&commands, guard, ai_payload("echo hi", "m3")).await
            })
        };
        wait_until("AI 命令开始写", || h.writer.started() == 1).await;
        h.closing.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), deliver)
            .await
            .expect("关闭后调用方要拿到结果")
            .expect("不 panic");
        assert_eq!(result, Err(WRITE_INTERRUPTED.to_owned()));
        let _ = h.task.await;
    }
}
