//! 属性对话框里的"计算大小"（M3-5 第 4 节）：后台数，进度走事件，随时可以取消。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use zssh_core::sftp::size::{local_size, remote_size, SizeTotals};
use zssh_core::sftp::SftpError;

use crate::commands::sftp::{tab_of, SftpTab};

pub const SFTP_SIZE_EVENT: &str = "sftp_size_event";

/// 进度事件最多这么久发一次。
const REPORT_INTERVAL: Duration = Duration::from_millis(150);

static NEXT_CALC: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize)]
pub struct SizeEvent {
    /// 发起计算时前端生成的关联 ID，原样带回：界面同时算多个文件夹时靠它认领事件。
    pub request_id: String,
    pub calc_id: u64,
    /// running 进行中 / done 数完了 / cancelled 被取消（数字是取消前数到的） / failed
    pub state: &'static str,
    #[serde(flatten)]
    pub totals: SizeTotals,
    pub error: Option<String>,
}

/// 一次计算在事件里的身份：任务号 + 前端生成的关联 ID。
struct CalcRoute {
    calc_id: u64,
    request_id: String,
}

/// 开始数一个路径的大小，返回任务号；进度和结果走 `sftp_size_event`。
///
/// `request_id` 由前端生成：进度事件可能比这个命令的返回值先到，光靠任务号认不出
/// "这条事件是哪个文件夹的"，所以事件里原样带着它。
#[tauri::command]
pub async fn sftp_size_start(
    sftp_id: String,
    side: String,
    path: String,
    request_id: String,
    app: AppHandle,
) -> Result<u64, String> {
    if path.trim().is_empty() {
        return Err("路径不能为空".to_owned());
    }
    if request_id.trim().is_empty() {
        return Err("缺少关联 ID".to_owned());
    }
    let remote = match side.as_str() {
        "remote" => true,
        "local" => false,
        other => return Err(format!("不认识的位置：{other}")),
    };
    let tab = tab_of(&app, &sftp_id).await?;
    let calc_id = NEXT_CALC.fetch_add(1, Ordering::SeqCst);
    let cancel = Arc::new(AtomicBool::new(false));
    tab.sizes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(calc_id, Arc::clone(&cancel));
    let route = CalcRoute {
        calc_id,
        request_id,
    };
    tauri::async_runtime::spawn(async move {
        let outcome = run(&app, &sftp_id, &tab, &route, remote, path, &cancel).await;
        tab.sizes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&calc_id);
        let CalcRoute {
            calc_id,
            request_id,
        } = route;
        let event = match outcome {
            Ok(totals) => SizeEvent {
                request_id,
                calc_id,
                state: "done",
                totals,
                error: None,
            },
            Err((totals, SftpError::Cancelled)) => SizeEvent {
                request_id,
                calc_id,
                state: "cancelled",
                totals,
                error: None,
            },
            Err((totals, err)) => SizeEvent {
                request_id,
                calc_id,
                state: "failed",
                totals,
                error: Some(err.to_string()),
            },
        };
        let _ = app.emit(SFTP_SIZE_EVENT, (sftp_id, event));
    });
    Ok(calc_id)
}

/// 取消一个还在数的任务（已经结束就什么也不做）。
#[tauri::command]
pub async fn sftp_size_cancel(sftp_id: String, calc_id: u64, app: AppHandle) -> Result<(), String> {
    let tab = tab_of(&app, &sftp_id).await?;
    if let Some(flag) = tab
        .sizes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&calc_id)
    {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

async fn run(
    app: &AppHandle,
    sftp_id: &str,
    tab: &Arc<SftpTab>,
    route: &CalcRoute,
    remote: bool,
    path: String,
    cancel: &Arc<AtomicBool>,
) -> Result<SizeTotals, (SizeTotals, SftpError)> {
    let calc_id = route.calc_id;
    let request_id = route.request_id.as_str();
    let mut last = Instant::now();
    let mut seen = SizeTotals::default();
    let mut report = |totals: &SizeTotals| {
        seen = totals.clone();
        if last.elapsed() >= REPORT_INTERVAL {
            last = Instant::now();
            let _ = app.emit(
                SFTP_SIZE_EVENT,
                (
                    sftp_id.to_owned(),
                    SizeEvent {
                        request_id: request_id.to_owned(),
                        calc_id,
                        state: "running",
                        totals: totals.clone(),
                        error: None,
                    },
                ),
            );
        }
    };
    if remote {
        remote_size(tab.endpoints.as_ref(), &path, cancel, &mut report)
            .await
            .map_err(|err| (seen.clone(), err))
    } else {
        let cancel = Arc::clone(cancel);
        let handle = app.clone();
        let sftp_id = sftp_id.to_owned();
        let request_id = request_id.to_owned();
        tauri::async_runtime::spawn_blocking(move || {
            let mut last = Instant::now();
            let mut seen = SizeTotals::default();
            local_size(std::path::Path::new(&path), &cancel, |totals| {
                seen = totals.clone();
                if last.elapsed() >= REPORT_INTERVAL {
                    last = Instant::now();
                    let _ = handle.emit(
                        SFTP_SIZE_EVENT,
                        (
                            sftp_id.clone(),
                            SizeEvent {
                                request_id: request_id.clone(),
                                calc_id,
                                state: "running",
                                totals: totals.clone(),
                                error: None,
                            },
                        ),
                    );
                }
            })
            .map_err(|err| (seen, err))
        })
        .await
        .map_err(|err| (SizeTotals::default(), SftpError::Remote(err.to_string())))?
    }
}
