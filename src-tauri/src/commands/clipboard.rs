//! 主机右键「复制密码」「复制连接信息」。
//!
//! 明文密码只在后端出现：这里按规则从保险库取出来，直接写进系统剪贴板，返回给前端的只有
//! 成功 / 失败（和「连接信息里有没有密码」这个布尔值），前端的内存、日志、事件里都不会有密码。
//!
//! 写剪贴板（Windows）：
//! - 除了 `CF_UNICODETEXT`，同时放上 `ExcludeClipboardContentFromMonitorProcessing`、
//!   `CanIncludeInClipboardHistory`（0）、`CanUploadToCloudClipboard`（0），让它不进剪贴板历史
//!   （Win+V）、不同步到云剪贴板、剪贴板监视程序也会忽略；
//! - `OpenClipboard` 被别的程序占着时短暂重试（总共约 1 秒），还打不开就报「剪贴板被占用」；
//! - 带密码的内容 30 秒后自动清除：写完记下剪贴板序号，到点序号没变（期间没人复制过别的东西）
//!   才清空，变了就不动。程序在这 30 秒内退出就不会清（后台线程随进程结束）。
//!
//! 不带密码的连接信息只是普通文字，照常写入，不做排除、不自动清除。
//!
//! 其他平台没有对应实现，返回「当前平台不支持」。

use std::time::Duration;

use serde::Serialize;
use tauri::State;
use zeroize::Zeroizing;
use zssh_core::ZsshError;

use crate::state::AppState;

/// 带密码的内容在剪贴板里保留多久。
const CLEAR_AFTER: Duration = Duration::from_secs(30);

/// 「复制连接信息」的结果：前端按有没有带密码选提示语。
#[derive(Serialize)]
pub struct CopyInfoOutcome {
    pub with_password: bool,
}

/// 给前端看的错误文字：参数 / 规则类错误只留原因本身，不带「输入不合法：」前缀。
fn to_message(err: ZsshError) -> String {
    match err {
        ZsshError::InvalidInput(message) => message,
        other => other.to_string(),
    }
}

/// 到点要不要清：只有剪贴板序号和我们写完时一样才清（说明期间没有任何程序改过剪贴板）。
fn should_clear(written_sequence: u32, current_sequence: u32) -> bool {
    written_sequence == current_sequence
}

/// 等 `delay` 之后，序号没变才调用 `clear`。拆成纯逻辑方便测试（测试里 delay 给 0）。
fn clear_after_delay(
    delay: Duration,
    written_sequence: u32,
    current_sequence: impl FnOnce() -> u32,
    clear: impl FnOnce(),
) -> bool {
    std::thread::sleep(delay);
    if should_clear(written_sequence, current_sequence()) {
        clear();
        true
    } else {
        false
    }
}

/// 后台等 30 秒再清；不阻塞命令返回。
fn schedule_clear(written_sequence: u32) {
    let spawned = std::thread::Builder::new()
        .name("clipboard-clear".to_owned())
        .spawn(move || {
            clear_after_delay(CLEAR_AFTER, written_sequence, imp::sequence, || {
                imp::clear_if_unchanged(written_sequence)
            });
        });
    if spawned.is_err() {
        // 起不了线程就没人来清：至少留一条日志（不含任何内容）
        tracing::warn!("clipboard auto-clear thread could not be started");
    }
}

/// 把文字写进剪贴板；`sensitive` 时做排除处理并安排 30 秒后清除。
async fn put_on_clipboard(text: Zeroizing<String>, sensitive: bool) -> Result<(), String> {
    // 重试时会 sleep，放到阻塞线程里；文字移进去，用完在那边丢掉（Zeroizing 清零）
    let sequence = tauri::async_runtime::spawn_blocking(move || imp::write_text(&text, sensitive))
        .await
        .map_err(|_| "写入剪贴板失败".to_owned())??;
    if sensitive {
        schedule_clear(sequence);
    }
    Ok(())
}

/// 右键「复制密码」：取这台主机保存的密码，直接写进剪贴板（30 秒后自动清除）。
#[tauri::command]
pub async fn host_copy_password(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let password = state.store.host_saved_password(id).map_err(to_message)?;
    put_on_clipboard(password, true).await
}

/// 右键「复制连接信息」：拼好文字直接写进剪贴板。带密码时和「复制密码」一样 30 秒后清除。
#[tauri::command]
pub async fn host_copy_info(
    state: State<'_, AppState>,
    id: i64,
) -> Result<CopyInfoOutcome, String> {
    let info = state.store.host_connection_info(id).map_err(to_message)?;
    let with_password = info.has_password();
    let text = zssh_core::store::connection_info::format_connection_info(&info);
    drop(info);
    put_on_clipboard(text, with_password).await?;
    Ok(CopyInfoOutcome { with_password })
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use windows::core::w;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use zeroize::Zeroizing;

    /// `CF_UNICODETEXT`
    const CF_UNICODETEXT: u32 = 13;
    /// 剪贴板被占用时的重试：10 次 × 100ms，总共约 1 秒。
    const OPEN_ATTEMPTS: u32 = 10;
    const OPEN_RETRY_GAP: Duration = Duration::from_millis(100);
    const BUSY: &str = "剪贴板被占用，请稍后再试";

    /// 打开着的剪贴板，离开作用域自动关。
    struct Open;

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: 只在 `open` 成功之后才会构造，当前线程持有剪贴板。
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    fn open() -> Result<Open, String> {
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: 不关联窗口，只在当前线程使用，用完由 `Open` 关闭。
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Open);
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(OPEN_RETRY_GAP);
            }
        }
        Err(BUSY.to_owned())
    }

    pub fn sequence() -> u32 {
        // SAFETY: 没有参数，只读系统状态。
        unsafe { GetClipboardSequenceNumber() }
    }

    /// 把一段字节作为 `format` 放进剪贴板（剪贴板必须已经打开）。
    ///
    /// # Safety
    /// 调用时当前线程必须持有已打开的剪贴板。
    unsafe fn set_bytes(format: u32, bytes: &[u8]) -> Result<(), String> {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|_| "写入剪贴板失败（内存不足）".to_owned())?;
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hmem));
            return Err("写入剪贴板失败".to_owned());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(hmem);
        match SetClipboardData(format, Some(HANDLE(hmem.0))) {
            // 成功后这块内存归系统所有，不能再释放
            Ok(_) => Ok(()),
            Err(_) => {
                wipe_and_free(hmem, bytes.len());
                Err("写入剪贴板失败".to_owned())
            }
        }
    }

    /// 写入失败时把还没交给系统的那块内存清零再释放。
    ///
    /// # Safety
    /// `hmem` 必须是 `GlobalAlloc` 分配、尚未交给剪贴板的内存，长度为 `len`。
    unsafe fn wipe_and_free(hmem: HGLOBAL, len: usize) {
        let ptr = GlobalLock(hmem);
        if !ptr.is_null() {
            std::ptr::write_bytes(ptr.cast::<u8>(), 0, len);
            let _ = GlobalUnlock(hmem);
        }
        let _ = GlobalFree(Some(hmem));
    }

    /// 给「不要进历史 / 不要上传 / 监视程序忽略」这几个约定格式放上数据。
    ///
    /// # Safety
    /// 调用时当前线程必须持有已打开的剪贴板。
    unsafe fn set_exclusions() -> Result<(), String> {
        let zero = 0u32.to_le_bytes();
        let formats = [
            RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
        ];
        for format in formats {
            if format == 0 {
                return Err("写入剪贴板失败".to_owned());
            }
            set_bytes(format, &zero)?;
        }
        Ok(())
    }

    /// 写入文字，返回写完之后的剪贴板序号。`sensitive` 时附上排除历史 / 云同步的格式；
    /// 任何一步失败都把剪贴板清空，不留下半成品（尤其不能留下没带排除格式的密码）。
    pub fn write_text(text: &str, sensitive: bool) -> Result<u32, String> {
        // UTF-16 的长度不会超过 UTF-8，容量够，不会重新分配（重新分配会在旧位置留下明文）
        let mut wide: Zeroizing<Vec<u16>> = Zeroizing::new(Vec::with_capacity(text.len() + 1));
        wide.extend(text.encode_utf16());
        wide.push(0);
        // SAFETY: u16 切片转成字节切片，长度翻倍，对齐只会放宽
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };

        let clipboard = open()?;
        // SAFETY: 上面刚打开了剪贴板，`clipboard` 在整个块里都活着。
        let result = unsafe {
            EmptyClipboard()
                .map_err(|_| "写入剪贴板失败".to_owned())
                .and_then(|()| {
                    if sensitive {
                        set_exclusions()?;
                    }
                    set_bytes(CF_UNICODETEXT, bytes)
                })
        };
        if result.is_err() {
            // SAFETY: 同上，剪贴板还开着。
            unsafe {
                let _ = EmptyClipboard();
            }
        }
        drop(clipboard);
        result?;
        Ok(sequence())
    }

    /// 序号还是 `expected`（期间没人动过剪贴板）才清空。打不开就放弃，不去抢。
    pub fn clear_if_unchanged(expected: u32) {
        let Ok(clipboard) = open() else { return };
        if sequence() == expected {
            // SAFETY: 剪贴板在 `clipboard` 存活期间一直开着。
            unsafe {
                let _ = EmptyClipboard();
            }
        }
        drop(clipboard);
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn sequence() -> u32 {
        0
    }

    pub fn write_text(_text: &str, _sensitive: bool) -> Result<u32, String> {
        Err("当前平台不支持复制到剪贴板".to_owned())
    }

    pub fn clear_if_unchanged(_expected: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn clears_only_when_the_sequence_is_unchanged() {
        assert!(should_clear(41, 41));
        assert!(!should_clear(41, 42));
    }

    #[test]
    fn auto_clear_runs_when_nobody_touched_the_clipboard() {
        let cleared = Cell::new(false);
        let ran = clear_after_delay(Duration::ZERO, 7, || 7, || cleared.set(true));
        assert!(ran);
        assert!(cleared.get());
    }

    #[test]
    fn auto_clear_leaves_a_newer_copy_alone() {
        let cleared = Cell::new(false);
        let ran = clear_after_delay(Duration::ZERO, 7, || 9, || cleared.set(true));
        assert!(!ran);
        assert!(!cleared.get());
    }

    #[test]
    fn rule_errors_lose_the_invalid_input_prefix() {
        let message = to_message(ZsshError::InvalidInput("这台主机没有保存密码".to_owned()));
        assert_eq!(message, "这台主机没有保存密码");
    }

    /// 手动测试：真的写一次剪贴板，再核对内容和序号，最后清掉。
    /// 运行：`cargo test -p zssh clipboard -- --ignored`（会动当前桌面的剪贴板）。
    #[cfg(windows)]
    #[test]
    #[ignore = "会改动真实的系统剪贴板，手动运行"]
    fn real_clipboard_roundtrip_and_clear() {
        let sequence = imp::write_text("manual-test-text", true).expect("写剪贴板");
        assert_eq!(sequence, imp::sequence());
        // 文字和三个排除格式都在剪贴板上
        use windows::core::w;
        use windows::Win32::System::DataExchange::{
            IsClipboardFormatAvailable, RegisterClipboardFormatW,
        };
        // SAFETY: 只查询格式是否存在，不打开剪贴板。
        unsafe {
            assert!(IsClipboardFormatAvailable(13).is_ok(), "没有文字格式");
            for name in [
                w!("ExcludeClipboardContentFromMonitorProcessing"),
                w!("CanIncludeInClipboardHistory"),
                w!("CanUploadToCloudClipboard"),
            ] {
                let format = RegisterClipboardFormatW(name);
                assert!(IsClipboardFormatAvailable(format).is_ok(), "缺排除格式");
            }
        }
        imp::clear_if_unchanged(sequence);
        assert_ne!(sequence, imp::sequence(), "清空后序号应当变化");
    }
}
