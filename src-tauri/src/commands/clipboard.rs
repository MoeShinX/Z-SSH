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
//! - 带密码的内容 30 秒后自动清除：写入时顺带放一个只有本程序认识的私有格式，里面是这次写入的
//!   随机标记；到点打开剪贴板，标记还是这一个才清空，没有或不一样（期间别人复制过别的东西）就不动。
//!   不用剪贴板序号判断：实测关闭剪贴板时系统还会补几种格式、序号再涨几次，写入时拿不到最终序号，
//!   关闭之后再读又有被别的程序插进来的窗口。程序在这 30 秒内退出就不会清（后台线程随进程结束）。
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

/// 一次写入的标记长度。
const TOKEN_LEN: usize = 16;

/// 这次写入的随机标记。只要求和别的写入不重复（不是秘密），用标准库的随机哈希种子加时间、
/// 计数器拼出来就够了。
fn new_token() -> [u8; TOKEN_LEN] {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut token = [0u8; TOKEN_LEN];
    for (index, chunk) in token.chunks_mut(8).enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u64(count);
        hasher.write_usize(index);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    token
}

/// 到点要不要清：剪贴板上我们的私有格式还在、内容就是这次的标记才清。
fn token_matches(on_clipboard: Option<&[u8]>, token: &[u8; TOKEN_LEN]) -> bool {
    on_clipboard.is_some_and(|bytes| bytes.get(..TOKEN_LEN) == Some(token.as_slice()))
}

/// 持有着剪贴板（`clipboard` 是打开着的句柄，离开作用域才关闭）写入。
/// 写失败时先用 `wipe` 清空剪贴板（不留下半成品，尤其不能留下没带排除格式的密码），再关闭。
#[cfg(any(windows, test))]
fn write_holding_clipboard<C>(
    clipboard: C,
    write: impl FnOnce() -> Result<(), String>,
    wipe: impl FnOnce(),
) -> Result<(), String> {
    let result = write();
    if result.is_err() {
        wipe();
    }
    drop(clipboard);
    result
}

/// 后台等 30 秒再清；不阻塞命令返回。
fn schedule_clear(token: [u8; TOKEN_LEN]) {
    let spawned = std::thread::Builder::new()
        .name("clipboard-clear".to_owned())
        .spawn(move || {
            std::thread::sleep(CLEAR_AFTER);
            imp::clear_if_ours(&token);
        });
    if spawned.is_err() {
        // 起不了线程就没人来清：至少留一条日志（不含任何内容）
        tracing::warn!("clipboard auto-clear thread could not be started");
    }
}

/// 把文字写进剪贴板；`sensitive` 时做排除处理并安排 30 秒后清除。
async fn put_on_clipboard(text: Zeroizing<String>, sensitive: bool) -> Result<(), String> {
    // 重试时会 sleep，放到阻塞线程里；文字移进去，用完在那边丢掉（Zeroizing 清零）
    let token = sensitive.then(new_token);
    tauri::async_runtime::spawn_blocking(move || imp::write_text(&text, token.as_ref()))
        .await
        .map_err(|_| "写入剪贴板失败".to_owned())??;
    if let Some(token) = token {
        schedule_clear(token);
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
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
        SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use zeroize::Zeroizing;

    use super::{token_matches, write_holding_clipboard, TOKEN_LEN};

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

    /// 本程序的私有格式：里面放这次写入的标记，30 秒后靠它认出「还是我们写的那份」。
    fn token_format() -> u32 {
        // SAFETY: 注册（或取回）一个具名格式，没有别的副作用。
        unsafe { RegisterClipboardFormatW(w!("Z-SSH.ClipboardOwnerToken")) }
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

    /// 写入文字。带 `token`（敏感内容）时附上排除历史 / 云同步的格式和本程序的标记；
    /// 任何一步失败都把剪贴板清空，不留下半成品（尤其不能留下没带排除格式的密码）。
    pub fn write_text(text: &str, token: Option<&[u8; TOKEN_LEN]>) -> Result<(), String> {
        // UTF-16 的长度不会超过 UTF-8，容量够，不会重新分配（重新分配会在旧位置留下明文）
        let mut wide: Zeroizing<Vec<u16>> = Zeroizing::new(Vec::with_capacity(text.len() + 1));
        wide.extend(text.encode_utf16());
        wide.push(0);
        // SAFETY: u16 切片转成字节切片，长度翻倍，对齐只会放宽
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };

        let clipboard = open()?;
        write_holding_clipboard(
            clipboard,
            || {
                // SAFETY: `clipboard` 在 `write_holding_clipboard` 里一直开着，直到写完。
                unsafe {
                    EmptyClipboard()
                        .map_err(|_| "写入剪贴板失败".to_owned())
                        .and_then(|()| {
                            if let Some(token) = token {
                                set_exclusions()?;
                                let format = token_format();
                                if format == 0 {
                                    return Err("写入剪贴板失败".to_owned());
                                }
                                set_bytes(format, token)?;
                            }
                            set_bytes(CF_UNICODETEXT, bytes)
                        })
                }
            },
            || {
                // SAFETY: 同上，剪贴板还开着。
                unsafe {
                    let _ = EmptyClipboard();
                }
            },
        )
    }

    /// 剪贴板上还是这次写入的那份（我们的标记还在、而且一致）才清空。打不开就放弃，不去抢。
    pub fn clear_if_ours(token: &[u8; TOKEN_LEN]) {
        let Ok(clipboard) = open() else { return };
        let format = token_format();
        // SAFETY: 剪贴板在 `clipboard` 存活期间一直开着；数据句柄归系统所有，只锁住读一下就解锁。
        unsafe {
            let ours = format != 0
                && GetClipboardData(format).is_ok_and(|handle| {
                    let hmem = HGLOBAL(handle.0);
                    let size = GlobalSize(hmem);
                    let ptr = GlobalLock(hmem);
                    if ptr.is_null() {
                        return false;
                    }
                    let bytes = std::slice::from_raw_parts(ptr.cast::<u8>(), size);
                    let matched = token_matches(Some(bytes), token);
                    let _ = GlobalUnlock(hmem);
                    matched
                });
            if ours {
                let _ = EmptyClipboard();
            }
        }
        drop(clipboard);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TOKEN_LEN;

    pub fn write_text(_text: &str, _token: Option<&[u8; TOKEN_LEN]>) -> Result<(), String> {
        Err("当前平台不支持复制到剪贴板".to_owned())
    }

    pub fn clear_if_ours(_token: &[u8; TOKEN_LEN]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_differ_between_writes() {
        let first = new_token();
        let second = new_token();
        assert_ne!(first, second);
    }

    #[test]
    fn clears_only_when_our_token_is_still_on_the_clipboard() {
        let token = [7u8; TOKEN_LEN];
        assert!(token_matches(Some(&token), &token));
        // 系统可能把数据块向上取整，多出来的尾巴不影响
        let mut padded = token.to_vec();
        padded.extend([0u8; 8]);
        assert!(token_matches(Some(&padded), &token));
        // 没有我们的格式（别人复制了别的东西）、标记不一样、长度不够：都不清
        assert!(!token_matches(None, &token));
        assert!(!token_matches(Some(&[8u8; TOKEN_LEN]), &token));
        assert!(!token_matches(Some(&token[..4]), &token));
    }

    /// 记录事件顺序的剪贴板句柄：丢弃时记下「关闭」。
    struct Guard<'a>(&'a std::cell::RefCell<Vec<&'static str>>);

    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push("close");
        }
    }

    #[test]
    fn a_successful_write_closes_the_clipboard_without_wiping() {
        let log = std::cell::RefCell::new(Vec::new());
        let result = write_holding_clipboard(
            Guard(&log),
            || {
                log.borrow_mut().push("write");
                Ok(())
            },
            || log.borrow_mut().push("wipe"),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(*log.borrow(), ["write", "close"]);
    }

    #[test]
    fn a_failed_write_wipes_the_clipboard_before_closing() {
        let log = std::cell::RefCell::new(Vec::new());
        let result = write_holding_clipboard(
            Guard(&log),
            || {
                log.borrow_mut().push("write");
                Err("写入剪贴板失败".to_owned())
            },
            || log.borrow_mut().push("wipe"),
        );
        assert_eq!(result, Err("写入剪贴板失败".to_owned()));
        assert_eq!(*log.borrow(), ["write", "wipe", "close"]);
    }

    #[test]
    fn rule_errors_lose_the_invalid_input_prefix() {
        let message = to_message(ZsshError::InvalidInput("这台主机没有保存密码".to_owned()));
        assert_eq!(message, "这台主机没有保存密码");
    }

    /// 手动测试：真的写剪贴板，核对格式，再验证「是我们的才清、别人复制过就不动」。
    /// 运行：`cargo test -p zssh --bin zssh real_clipboard -- --ignored`（会动当前桌面的剪贴板）。
    #[cfg(windows)]
    #[test]
    #[ignore = "会改动真实的系统剪贴板，手动运行"]
    fn real_clipboard_roundtrip_and_clear() {
        use windows::core::w;
        use windows::Win32::System::DataExchange::{
            IsClipboardFormatAvailable, RegisterClipboardFormatW,
        };
        let text_on_clipboard = || unsafe { IsClipboardFormatAvailable(13).is_ok() };

        // 1. 我们写的，到点清掉
        let token = new_token();
        imp::write_text("manual-test-text", Some(&token)).expect("写剪贴板");
        // SAFETY: 只查询格式是否存在，不打开剪贴板。
        unsafe {
            assert!(text_on_clipboard(), "没有文字格式");
            for name in [
                w!("ExcludeClipboardContentFromMonitorProcessing"),
                w!("CanIncludeInClipboardHistory"),
                w!("CanUploadToCloudClipboard"),
                w!("Z-SSH.ClipboardOwnerToken"),
            ] {
                let format = RegisterClipboardFormatW(name);
                assert!(IsClipboardFormatAvailable(format).is_ok(), "缺格式");
            }
        }
        imp::clear_if_ours(&token);
        assert!(!text_on_clipboard(), "我们写的那份应当被清掉");

        // 2. 写完之后别人复制了别的东西（这里用不带标记的写入模拟）：不清
        let token = new_token();
        imp::write_text("manual-test-secret", Some(&token)).expect("写剪贴板");
        imp::write_text("someone-else-copied-this", None).expect("写剪贴板");
        imp::clear_if_ours(&token);
        assert!(text_on_clipboard(), "别人复制的内容不能被清掉");

        // 3. 标记对不上（另一次写入）：不清；最后自己收拾干净
        let other = new_token();
        imp::write_text("manual-test-secret", Some(&other)).expect("写剪贴板");
        imp::clear_if_ours(&token);
        assert!(text_on_clipboard(), "别的写入的内容不能被清掉");
        imp::clear_if_ours(&other);
        assert!(!text_on_clipboard());
    }
}
