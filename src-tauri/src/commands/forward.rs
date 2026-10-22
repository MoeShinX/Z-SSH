//! 端口转发命令：规则的增删改查 + 开 / 停 / 状态。
//!
//! 每条开着的规则一个后台任务，任务持有一份池里的连接引用：转发开着，这条 SSH 连接就
//! 不会因为终端标签全关了而被池子回收。状态变化时发 `forward-status` 事件，界面收到后
//! 重新拉一次状态（计数器靠界面自己轮询，不逐字节发事件）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::task::JoinHandle;
use zssh_core::ssh::connection::Connection;
use zssh_core::ssh::forward::{self, ForwardCounters, ForwardStats};
use zssh_core::store::forwards::{ForwardKind, PortForward, PortForwardDraft};

use crate::commands::session::pooled_connection;
use crate::state::{AppState, CancelSignal};

pub const FORWARD_STATUS_EVENT: &str = "forward-status";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// 正在连 SSH（可能在等用户输密码 / 确认指纹）
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForwardStatus {
    id: i64,
    phase: Phase,
    error: Option<String>,
    /// 远程转发时服务器实际监听的端口（请求 0 之类时和规则里写的不一样）
    listening_port: Option<u32>,
    counters: ForwardCounters,
}

/// 阶段、出错原因、远程转发实际监听的端口。
type PhaseState = (Phase, Option<String>, Option<u32>);

/// 远程转发停掉时要告诉服务器取消监听：任务被 abort 时跑不了异步清理，记在外面。
type RemoteCleanup = Option<(Arc<Connection>, String, u32)>;

struct Running {
    phase: Arc<Mutex<PhaseState>>,
    stats: Arc<ForwardStats>,
    task: JoinHandle<()>,
    cancel: Arc<Mutex<Option<Arc<CancelSignal>>>>,
    remote: Arc<Mutex<RemoteCleanup>>,
}

#[derive(Default)]
pub struct ForwardManager {
    running: Mutex<HashMap<i64, Running>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 结束一条转发：任务取消（它持有的监听和每条连接跟着关），连接中还在等的提问取消，
/// 远程转发告诉服务器取消监听。
fn shut_down(running: Running) {
    running.task.abort();
    let cancel = lock(&running.cancel).take();
    if let Some(cancel) = cancel {
        cancel.cancel();
    }
    let remote = lock(&running.remote).take();
    if let Some((connection, bind_host, port)) = remote {
        tokio::spawn(async move { connection.stop_remote_forward(&bind_host, port).await });
    }
}

/// 数据库变了之后（删主机、同步合并、恢复备份、导入离线备份）调用：规则已经不在的转发停掉。
pub fn reconcile(app: &AppHandle) {
    let existing: HashSet<i64> = match app.state::<AppState>().store.forward_list() {
        Ok(rules) => rules.into_iter().map(|rule| rule.id).collect(),
        Err(err) => {
            tracing::warn!(%err, "读端口转发规则失败，这次不清理");
            return;
        }
    };
    for id in app.state::<ForwardManager>().stop_missing(&existing) {
        tracing::info!(id, "端口转发的规则已经不在了，停掉对应的转发");
        notify(app, id);
    }
}

fn notify(app: &AppHandle, id: i64) {
    let _ = app.emit(FORWARD_STATUS_EVENT, id);
}

impl ForwardManager {
    fn is_active(&self, id: i64) -> bool {
        lock(&self.running)
            .get(&id)
            .is_some_and(|running| lock(&running.phase).0 != Phase::Failed)
    }

    /// 停掉一条（没开也不算错）。
    fn stop(&self, id: i64) {
        let removed = lock(&self.running).remove(&id);
        if let Some(running) = removed {
            shut_down(running);
        }
    }

    /// 停掉所有"数据库里已经没有这条规则"的转发（复审 F2）：删主机会级联删掉它的规则，
    /// 同步、恢复备份、替换导入也可能删掉主机，但跑着的监听和隧道不会跟着停。
    /// 返回停掉了几条。
    fn stop_missing(&self, existing: &HashSet<i64>) -> Vec<i64> {
        let orphans: Vec<(i64, Running)> = {
            let mut running = lock(&self.running);
            let ids: Vec<i64> = running
                .keys()
                .copied()
                .filter(|id| !existing.contains(id))
                .collect();
            ids.into_iter()
                .filter_map(|id| running.remove(&id).map(|r| (id, r)))
                .collect()
        };
        orphans
            .into_iter()
            .map(|(id, running)| {
                shut_down(running);
                id
            })
            .collect()
    }

    fn statuses(&self) -> Vec<ForwardStatus> {
        lock(&self.running)
            .iter()
            .map(|(id, running)| {
                let (phase, error, listening_port) = lock(&running.phase).clone();
                ForwardStatus {
                    id: *id,
                    phase,
                    error,
                    listening_port,
                    counters: running.stats.snapshot(),
                }
            })
            .collect()
    }
}

fn start(app: &AppHandle, rule: PortForward) {
    let manager = app.state::<ForwardManager>();
    let id = rule.id;
    // 检查、清掉失败的旧记录、登记新任务都在同一把锁里：两次几乎同时的"开启"不会各起一个
    // 任务、其中一个脱离管理（复审后续实测项 2）。
    let mut running = lock(&manager.running);
    if running
        .get(&id)
        .is_some_and(|existing| lock(&existing.phase).0 != Phase::Failed)
    {
        return;
    }
    if let Some(failed) = running.remove(&id) {
        shut_down(failed);
    }

    let phase = Arc::new(Mutex::new((Phase::Starting, None, None)));
    let stats = Arc::new(ForwardStats::default());
    let cancel = Arc::new(Mutex::new(None));
    let remote = Arc::new(Mutex::new(None));
    // 先登记再起任务：任务很快失败时，它写的 Failed 状态要落在已登记的那一份上
    let task = {
        let app = app.clone();
        let phase = Arc::clone(&phase);
        let stats = Arc::clone(&stats);
        let cancel = Arc::clone(&cancel);
        let remote = Arc::clone(&remote);
        tokio::spawn(async move {
            let reason = run(&app, rule, &phase, stats, &cancel, &remote).await;
            *lock(&phase) = (Phase::Failed, Some(reason), None);
            notify(&app, id);
        })
    };
    running.insert(
        id,
        Running {
            phase,
            stats,
            task,
            cancel,
            remote,
        },
    );
    drop(running);
    notify(app, id);
}

/// 跑一条规则，返回停下来的原因（只有出错才会停；用户点停止是直接 abort）。
async fn run(
    app: &AppHandle,
    rule: PortForward,
    phase: &Mutex<PhaseState>,
    stats: Arc<ForwardStats>,
    cancel_slot: &Mutex<Option<Arc<CancelSignal>>>,
    remote_slot: &Mutex<RemoteCleanup>,
) -> String {
    // 本地转发先占端口：端口被占用时不用白连一次 SSH
    let listener = match rule.kind {
        ForwardKind::Local => match forward::bind_local(&rule.bind_host, rule.bind_port).await {
            Ok(listener) => Some(listener),
            Err(err) => return err.to_string(),
        },
        ForwardKind::Remote => None,
    };

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let session_id = format!("forward-{}-{nanos}", rule.id);
    let (session, cancel) = match pooled_connection(app, &session_id, rule.host_id).await {
        Ok(pair) => pair,
        Err(err) => return err,
    };
    *lock(cancel_slot) = Some(cancel);
    let connection = Arc::clone(session.connection());

    let reason = match (rule.kind, listener) {
        (ForwardKind::Local, Some(listener)) => {
            *lock(phase) = (Phase::Running, None, None);
            notify(app, rule.id);
            forward::run_local(connection, listener, rule.dest_host, rule.dest_port, stats).await
        }
        _ => {
            let (sink, incoming) = tokio::sync::mpsc::unbounded_channel();
            let port = match connection
                .start_remote_forward(&rule.bind_host, rule.bind_port, sink)
                .await
            {
                Ok(port) => port,
                Err(err) => return err.to_string(),
            };
            *lock(remote_slot) = Some((Arc::clone(&connection), rule.bind_host.clone(), port));
            *lock(phase) = (Phase::Running, None, Some(port));
            notify(app, rule.id);
            forward::run_remote(connection, incoming, rule.dest_host, rule.dest_port, stats).await
        }
    };
    // 连接引用要活到转发结束：放掉之后池子才可能回收这条连接
    drop(session);
    reason.to_string()
}

fn to_message(err: zssh_core::ZsshError) -> String {
    err.to_string()
}

#[tauri::command]
pub async fn forward_list(state: State<'_, AppState>) -> Result<Vec<PortForward>, String> {
    state.store.forward_list().map_err(to_message)
}

/// 保存规则。正开着的规则改了之后按新设置重开。
#[tauri::command]
pub async fn forward_save(
    app: AppHandle,
    state: State<'_, AppState>,
    manager: State<'_, ForwardManager>,
    draft: PortForwardDraft,
) -> Result<PortForward, String> {
    let saved = state.store.forward_save(&draft).map_err(to_message)?;
    if manager.is_active(saved.id) {
        manager.stop(saved.id);
        start(&app, saved.clone());
    }
    Ok(saved)
}

#[tauri::command]
pub async fn forward_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    manager: State<'_, ForwardManager>,
    id: i64,
) -> Result<(), String> {
    manager.stop(id);
    state.store.forward_delete(id).map_err(to_message)?;
    notify(&app, id);
    Ok(())
}

/// 开一条转发。马上返回；连没连上看 `forward-status` 事件 / [`forward_statuses`]。
#[tauri::command]
pub async fn forward_start(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    let rule = state.store.forward_get(id).map_err(to_message)?;
    start(&app, rule);
    Ok(())
}

#[tauri::command]
pub async fn forward_stop(
    app: AppHandle,
    manager: State<'_, ForwardManager>,
    id: i64,
) -> Result<(), String> {
    manager.stop(id);
    notify(&app, id);
    Ok(())
}

#[tauri::command]
pub async fn forward_statuses(
    manager: State<'_, ForwardManager>,
) -> Result<Vec<ForwardStatus>, String> {
    Ok(manager.statuses())
}

/// 某台主机的终端连上时调用：把这台主机上勾了"自动开启"、还没开的规则都开起来。
/// 连接已经在池里了，转发直接复用，不会再问一次密码。
pub fn auto_start_for_host(app: &AppHandle, host_id: i64) {
    let rules = match app.state::<AppState>().store.forward_list() {
        Ok(rules) => rules,
        Err(err) => {
            tracing::warn!(%err, "读端口转发规则失败，跳过自动开启");
            return;
        }
    };
    for rule in rules
        .into_iter()
        .filter(|rule| rule.host_id == host_id && rule.auto_start)
    {
        start(app, rule);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_task() -> Running {
        Running {
            phase: Arc::new(Mutex::new((Phase::Running, None, None))),
            stats: Arc::new(ForwardStats::default()),
            task: tokio::spawn(std::future::pending::<()>()),
            cancel: Arc::new(Mutex::new(None)),
            remote: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn forwards_whose_rules_are_gone_are_stopped() {
        let manager = ForwardManager::default();
        lock(&manager.running).insert(1, running_task());
        lock(&manager.running).insert(2, running_task());
        let doomed = lock(&manager.running)
            .get(&2)
            .map(|r| r.task.abort_handle())
            .unwrap();

        let stopped = manager.stop_missing(&HashSet::from([1]));

        assert_eq!(stopped, vec![2]);
        assert!(manager.is_active(1), "规则还在的不动");
        assert!(!manager.is_active(2));
        tokio::task::yield_now().await;
        assert!(
            doomed.is_finished(),
            "停掉的转发任务要真的结束（监听和连接跟着关）"
        );
    }
}
