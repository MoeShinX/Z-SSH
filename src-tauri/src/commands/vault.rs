//! 主机库、凭据、代理的 Tauri 命令，只做参数转接，逻辑在 `zssh-core::store`。

use tauri::State;
use zssh_core::model::{
    Group, GroupDraft, Host, HostDraft, Identity, IdentityDraft, Proxy, ProxyDraft,
};

use crate::state::AppState;

fn to_message(err: zssh_core::ZsshError) -> String {
    err.to_string()
}

#[tauri::command]
pub async fn group_list(state: State<'_, AppState>) -> Result<Vec<Group>, String> {
    state.store.group_list().map_err(to_message)
}

#[tauri::command]
pub async fn group_save(state: State<'_, AppState>, draft: GroupDraft) -> Result<Group, String> {
    state.store.group_save(&draft).map_err(to_message)
}

#[tauri::command]
pub async fn group_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.group_delete(id).map_err(to_message)
}

#[tauri::command]
pub async fn host_list(state: State<'_, AppState>) -> Result<Vec<Host>, String> {
    state.store.host_list().map_err(to_message)
}

#[tauri::command]
pub async fn host_save(state: State<'_, AppState>, draft: HostDraft) -> Result<Host, String> {
    state.store.host_save(&draft).map_err(to_message)
}

#[tauri::command]
pub async fn host_delete(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    state.store.host_delete(id).map_err(to_message)?;
    // 主机的转发规则跟着级联删了，跑着的转发也要停（复审 F2）
    crate::commands::forward::reconcile(&app);
    Ok(())
}

/// 把一层主机（同一个分组下 / 都未分组）按传入顺序排成 1..n；整个是一个事务。
#[tauri::command]
pub async fn host_reorder(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    state.store.host_reorder(&ids).map_err(to_message)
}

/// 把一层分组（同一个父分组下）按传入顺序排成 1..n；整个是一个事务。
#[tauri::command]
pub async fn group_reorder(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    state.store.group_reorder(&ids).map_err(to_message)
}

/// 「按名字重新排序」：把某个分组下直接的主机和子分组（`group_id` 为空时只管未分组主机）的顺序清零。
#[tauri::command]
pub async fn order_reset(state: State<'_, AppState>, group_id: Option<i64>) -> Result<(), String> {
    state.store.order_reset(group_id).map_err(to_message)
}

#[tauri::command]
pub async fn identity_list(state: State<'_, AppState>) -> Result<Vec<Identity>, String> {
    state.store.identity_list().map_err(to_message)
}

#[tauri::command]
pub async fn identity_save(
    state: State<'_, AppState>,
    draft: IdentityDraft,
) -> Result<Identity, String> {
    state.store.identity_save(&draft).map_err(to_message)
}

/// 主机编辑面板里点「小眼睛」：取出这条**密码凭据**的明文显示在密码框里。
/// 只在用户点的时候调用（平时前端拿不到密码）；私钥、口令不从这里给出。
#[tauri::command]
pub async fn identity_reveal_password(
    state: State<'_, AppState>,
    id: i64,
) -> Result<String, String> {
    state
        .store
        .identity_saved_password(id)
        // 交给前端显示，前端那边已经是普通字符串了，这里不再保留 Zeroizing
        .map(|password| password.as_str().to_owned())
        .map_err(|err| match err {
            // 规则类错误只给原因本身，不带「输入不合法：」前缀
            zssh_core::ZsshError::InvalidInput(message) => message,
            other => other.to_string(),
        })
}

#[tauri::command]
pub async fn identity_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.identity_delete(id).map_err(to_message)
}

#[tauri::command]
pub async fn proxy_list(state: State<'_, AppState>) -> Result<Vec<Proxy>, String> {
    state.store.proxy_list().map_err(to_message)
}

#[tauri::command]
pub async fn proxy_save(state: State<'_, AppState>, draft: ProxyDraft) -> Result<Proxy, String> {
    state.store.proxy_save(&draft).map_err(to_message)
}

#[tauri::command]
pub async fn proxy_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.proxy_delete(id).map_err(to_message)
}

#[derive(serde::Serialize)]
pub struct PublicKeyView {
    pub public_key: String,
    pub fingerprint: String,
}

/// 生成一把新密钥，存成"导入的私钥"凭据（RSA 4096 要几秒：放到阻塞线程里）。
#[tauri::command]
pub async fn identity_generate(
    app: tauri::AppHandle,
    name: String,
    algorithm: zssh_core::ssh::keygen::KeyAlgorithm,
    comment: String,
    passphrase: Option<String>,
) -> Result<Identity, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        app.state::<AppState>().store.identity_generate(
            &name,
            algorithm,
            &comment,
            passphrase.as_deref(),
        )
    })
    .await
    .map_err(|err| format!("生成密钥的线程异常结束：{err}"))?
    .map_err(to_message)
}

/// 私钥凭据的公钥（复制给别人、或装到服务器上）。
#[tauri::command]
pub async fn identity_public_key(
    state: State<'_, AppState>,
    id: i64,
) -> Result<PublicKeyView, String> {
    let (public_key, fingerprint) = state.store.identity_public_key(id).map_err(to_message)?;
    Ok(PublicKeyView {
        public_key,
        fingerprint,
    })
}

/// 把某个私钥凭据的公钥装到一台主机的 `~/.ssh/authorized_keys`。用这台主机现在的登录方式
/// 连上（需要密码时照常弹窗），跑一条追加命令；已经有这一行就不重复加。
#[tauri::command]
pub async fn host_install_public_key(
    app: tauri::AppHandle,
    host_id: i64,
    identity_id: i64,
) -> Result<zssh_core::ssh::keygen::InstallOutcome, String> {
    use tauri::Manager;
    let state = app.state::<AppState>();
    let (public_key, _) = state
        .store
        .identity_public_key(identity_id)
        .map_err(to_message)?;
    let command = zssh_core::ssh::keygen::install_command(&public_key).map_err(to_message)?;
    // 弹窗（密码、指纹）挂在这个 id 上
    let session_id = format!("install-key-{host_id}-{}", uuid_like());
    let (connection, _cancel) =
        crate::commands::session::pooled_connection(&app, &session_id, host_id).await?;
    let output = connection
        .connection()
        .exec(&command)
        .await
        .map_err(to_message)?;
    zssh_core::ssh::keygen::parse_install_output(&output).map_err(to_message)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

/// 探测过的主机系统：主机 ID → 系统 ID（主机列表的系统图标）。
#[tauri::command]
pub async fn host_os_list(
    state: State<'_, AppState>,
) -> Result<std::collections::BTreeMap<i64, String>, String> {
    state.store.host_os_list().map_err(to_message)
}

/// 主机详情里看 AI 记下的笔记。
#[tauri::command]
pub async fn host_memory_get(
    state: State<'_, AppState>,
    host_id: i64,
) -> Result<Option<zssh_core::store::host_memory::HostMemory>, String> {
    state.store.host_memory_get(host_id).map_err(to_message)
}

/// 用户在主机详情里改 / 清空笔记（和 AI 写入走同样的清理）。
#[tauri::command]
pub async fn host_memory_set(
    state: State<'_, AppState>,
    host_id: i64,
    content: String,
) -> Result<(), String> {
    let cleaned = zssh_core::ai::host_memory::clean_memory(&content)?;
    state
        .store
        .host_memory_set(host_id, &cleaned)
        .map_err(to_message)
}
