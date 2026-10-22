//! 文件窗口：像 Xftp 那样的独立窗口，整个程序最多一个，里面用标签放多个 SFTP。
//!
//! - 主窗口把 SFTP 标签"移过来"、或者文件窗口开着时新开 SFTP：都先放进待接管队列，
//!   再通知文件窗口来取。窗口刚建好时前端还没开始监听，所以不能只靠事件，文件窗口
//!   启动后也会主动取一次。
//! - SFTP 会话在后端（`SftpRegistry`），挪窗口不用重连，正在跑的传输也不断。
//! - 关文件窗口 = 断开里面的 SFTP（前端先问、再逐个关；万一前端没来得及，窗口销毁时
//!   这里按登记的 id 兜底关掉）。主窗口关了整个程序退出，文件窗口跟着走。

use std::collections::HashSet;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tracing::{info, warn};

pub const LABEL: &str = "files";
/// 待接管队列里有新东西了（只发给文件窗口）。
pub const PENDING_EVENT: &str = "files://pending";
/// 文件窗口开了 / 关了（发给主窗口：决定新开的 SFTP 去哪）。
pub const STATE_EVENT: &str = "files://state";

/// 交给文件窗口的一个 SFTP 标签。`sftp_id` 为空 = 还没连，文件窗口自己去开。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileTab {
    #[serde(default)]
    pub sftp_id: String,
    pub host_id: i64,
    pub title: String,
    /// 带过去的两栏目录（空 = 默认）。
    #[serde(default)]
    pub local: String,
    #[serde(default)]
    pub remote: String,
}

#[derive(Default)]
pub struct FileWindowState {
    pending: Mutex<Vec<FileTab>>,
    /// 文件窗口里现在有哪些 SFTP 会话（窗口销毁时兜底关掉）。
    owned: Mutex<HashSet<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FileWindowState {
    pub fn push(&self, tab: FileTab) {
        lock(&self.pending).push(tab);
    }

    pub fn take(&self) -> Vec<FileTab> {
        std::mem::take(&mut *lock(&self.pending))
    }

    pub fn set_owned(&self, ids: Vec<String>) {
        *lock(&self.owned) = ids.into_iter().filter(|id| !id.is_empty()).collect();
    }

    pub fn take_owned(&self) -> Vec<String> {
        lock(&self.owned).drain().collect()
    }

    pub fn clear_pending(&self) {
        lock(&self.pending).clear();
    }
}

pub fn is_open<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(LABEL).is_some()
}

/// 文件窗口开着就叫到前面，没开就建一个。
pub fn ensure_open<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Z-SFTP")
        // 和主窗口一样自绘标题栏：系统标题栏只跟系统深浅色走，不跟 Z-SSH 的主题
        .decorations(false)
        .inner_size(1100.0, 720.0)
        .min_inner_size(640.0, 420.0)
        // 同一份前端，靠这个标记渲染文件窗口而不是主界面
        .initialization_script("window.__ZSSH_WINDOW__ = 'files';")
        .build()
        .map_err(|err| format!("打开文件窗口失败：{err}"))?;
    // WebView2 自带右键菜单只留编辑项（NATIVE-1）
    crate::native_menu::install(&window);
    info!("文件窗口已打开");
    let _ = app.emit_to("main", STATE_EVENT, true);
    Ok(())
}

/// 窗口事件：文件窗口销毁 → 兜底关掉它的 SFTP、通知主窗口；主窗口销毁 → 整个程序退出。
pub fn on_window_destroyed(app: &AppHandle, label: &str) {
    if label == LABEL {
        let state = app.state::<FileWindowState>();
        state.clear_pending();
        let leftover = state.take_owned();
        if !leftover.is_empty() {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                for sftp_id in leftover {
                    if let Err(err) =
                        crate::commands::sftp::sftp_close(handle.clone(), sftp_id).await
                    {
                        warn!(%err, "文件窗口关闭后收尾 SFTP 失败");
                    }
                }
            });
        }
        let _ = app.emit_to("main", STATE_EVENT, false);
        info!("文件窗口已关闭");
    } else if label == "main" && is_open(app) {
        // 主窗口没了，文件窗口单独留着没有意义（编辑器、终端都在主窗口）。
        app.exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_tabs_are_handed_over_once() {
        let state = FileWindowState::default();
        let tab = |host_id| FileTab {
            sftp_id: String::new(),
            host_id,
            title: "web-1".to_owned(),
            local: String::new(),
            remote: String::new(),
        };
        state.push(tab(1));
        state.push(tab(2));
        assert_eq!(
            state.take().iter().map(|t| t.host_id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(state.take().is_empty(), "取过一次就清空，不会重复接管");
    }

    #[test]
    fn owned_sessions_ignore_empty_ids_and_drain() {
        let state = FileWindowState::default();
        state.set_owned(vec!["s1".into(), String::new(), "s2".into()]);
        let mut owned = state.take_owned();
        owned.sort();
        assert_eq!(owned, ["s1", "s2"]);
        assert!(state.take_owned().is_empty());
    }

    #[test]
    fn the_file_window_is_created_once_and_then_reused() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app");
        let handle = app.handle();
        assert!(!is_open(handle));
        ensure_open(handle).expect("第一次建窗口");
        assert!(is_open(handle));
        ensure_open(handle).expect("第二次只是叫到前面");
        let files: Vec<_> = handle
            .webview_windows()
            .into_keys()
            .filter(|label| label == LABEL)
            .collect();
        assert_eq!(files.len(), 1, "整个程序最多一个文件窗口");
    }
}
