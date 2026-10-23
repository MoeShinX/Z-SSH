//! 焦点进入「英文区」（只有终端，由前端判断，见 ui/src/terminal/imeFocus.ts）时让输入法打英文，
//! 离开终端、进别的输入框时恢复原来的状态。命令和状态机里沿用 "terminal" / "mode" 这些名字。
//!
//! 做法：给当前焦点窗口的默认输入法窗口发 `WM_IME_CONTROL`，**关闭**输入法（关闭后按键直接是
//! 英文），离开时**打开**回去。以前改的是"转换模式"（去掉中文位），微软拼音认、百度拼音等
//! 第三方输入法不认（2026-10-23 实测：转换模式读回来是写进去的值，百度的工具栏仍是「英」），
//! 打开 / 关闭两家都认，所以换成了它（映射见 `imp::Win32Ime`）。
//! 不是禁止中文：要在终端里打中文按 Ctrl+空格（或点输入法图标）打开输入法即可。
//!
//! 键盘焦点在 WebView2 的子窗口里（别的进程），所以按"前台线程的焦点窗口"找，
//! 并且只在前台窗口属于本进程时才动手，绝不碰别的程序的输入法。
//!
//! 主窗口和文件窗口各有自己的前端控制器、共用这一份状态：`SharedIme` 按窗口记录
//! 谁在英文区里，全部离开了才真正恢复。否则从一个窗口点到另一个窗口时，
//! 后者的"进入"先到、前者延迟发出的"离开"后到，会在后者的输入框里把输入法恢复成中文。
//!
//! 状态机（`TerminalIme`）和 Win32 调用分开：进出终端的判断全在状态机里，
//! 测试用替身代替 Win32。要点：
//! - "处于终端焦点"由后端自己记住，重复的进入 / 离开直接忽略（前端的调用到达顺序不可靠）；
//! - 进终端时记下当时的模式再切英文；恢复**成功**才清掉记录——前台不是本进程、
//!   消息超时这类失败保留记录和"处于终端焦点"状态，等前端下一次补一次恢复；
//! - **写完要读回来核对**：Chromium 在焦点落到不可输入的元素上时会把窗口的输入法上下文
//!   解除关联，这时 SendMessageTimeoutW 照样有返回，但没有作用对象，写进去等于没写。
//!   所以写完再读一次，读回的（中英文相关的）位和要写的不一致、或读回失败，都按失败处理：
//!   恢复失败就保留记录、仍算处于终端焦点，前端等焦点进了输入框再补一次；
//!   切英文时只在读回确认仍是原模式时判成没改过；写已送达但读回失败时结果不确定，
//!   必须保留恢复义务，不能把可能已切成英文的状态留给 AI 等输入框；
//! - **只恢复自己改过的**：进终端时本来就是英文（或者切换没成功），离开时什么都不写。
//!   否则会把读到的"英文"主动写回去——刚启动、输入法还没激活时读到的常常就是英文，
//!   写回去以后 AI 输入框等别的地方也被强制成了英文（用户 2026-10-21 反馈的现象）。

/// 对输入法的两个操作。Windows 上走 Win32，测试里用替身。
#[cfg_attr(not(windows), allow(dead_code))]
trait ImeControl {
    /// 读当前转换模式；前台窗口不是本进程、找不到输入法窗口或读失败时返回 `None`。
    fn read_mode(&mut self) -> Option<usize>;
    /// 写转换模式；成功返回 `true`（同样要求前台窗口是本进程）。
    fn write_mode(&mut self, mode: usize) -> bool;
}

/// 转换模式里和中文相关的位：去掉它们就是英文半角。
#[cfg_attr(not(windows), allow(dead_code))]
const CMODE_NATIVE: usize = 0x0001;
#[cfg_attr(not(windows), allow(dead_code))]
const CMODE_FULLSHAPE: usize = 0x0008;
/// 写入后核对时只比这两位：其余位（输入法自己维护的常驻位等）可能被它顺手调整，
/// 全位比较会把写成功了的也判成失败，进而永远不恢复。
#[cfg_attr(not(windows), allow(dead_code))]
const CMODE_MANAGED: usize = CMODE_NATIVE | CMODE_FULLSHAPE;

/// 写转换模式并读回核对：消息没送到、读回失败、读回的受管位和写的不一致都算失败。
#[cfg_attr(not(windows), allow(dead_code))]
fn write_verified(ime: &mut impl ImeControl, mode: usize) -> bool {
    ime.write_mode(mode)
        && ime
            .read_mode()
            .is_some_and(|actual| actual & CMODE_MANAGED == mode & CMODE_MANAGED)
}

/// 终端输入法状态：`saved` 为 `Some` 就是"处于终端焦点"，里面是进终端前的模式；
/// `switched` 表示需要恢复：已确认改过，或写已送达但读回失败、不能排除已改过。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Default)]
struct TerminalIme {
    saved: Option<usize>,
    switched: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl TerminalIme {
    /// 处理一次进入 / 离开，返回处理完以后是否仍处于终端焦点。
    /// 离开时返回 `true` 表示恢复没成功，前端要在合适的时机再发一次离开。
    fn apply(&mut self, focused: bool, ime: &mut impl ImeControl) -> bool {
        if focused {
            self.enter(ime)
        } else {
            self.leave(ime)
        }
    }

    fn enter(&mut self, ime: &mut impl ImeControl) -> bool {
        // 已经在终端里：重复进入（分屏之间切换、窗口重新激活）什么都不做，
        // 免得把"已经切成英文"当成原来的状态记下来
        if self.saved.is_some() {
            return true;
        }
        let Some(mode) = ime.read_mode() else {
            return false;
        };
        self.saved = Some(mode);
        let english = mode & !CMODE_MANAGED;
        // 写已送达后读回失败不代表没有生效。只有读回确认仍是原模式时才能免去恢复；
        // 结果未知也保留原模式，等离开终端、能确认恢复成功后再清掉。
        self.switched = english != mode && ime.write_mode(english);
        if self.switched {
            self.switched = ime
                .read_mode()
                .is_none_or(|actual| actual & CMODE_MANAGED != mode & CMODE_MANAGED);
        }
        true
    }

    fn leave(&mut self, ime: &mut impl ImeControl) -> bool {
        let Some(mode) = self.saved else {
            return false;
        };
        // 进终端时没动过输入法：离开时也不动，保持它自然的状态
        if !self.switched || write_verified(ime, mode) {
            self.saved = None;
            self.switched = false;
            false
        } else {
            true
        }
    }
}

/// 多个窗口共用一份 [`TerminalIme`]：记下哪些窗口在英文区里，最后一个离开才恢复。
/// 返回值对"调用的这个窗口"说话：它自己不再欠恢复就是 `false`，哪怕别的窗口还在英文区。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Default)]
struct SharedIme {
    state: TerminalIme,
    owners: std::collections::HashSet<String>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl SharedIme {
    fn apply(&mut self, window: &str, focused: bool, ime: &mut impl ImeControl) -> bool {
        if focused {
            // 进入没成功（读不到当前模式）：这个窗口不欠恢复
            if self.state.apply(true, ime) {
                self.owners.insert(window.to_owned());
                return true;
            }
            return false;
        }
        let was_owner = self.owners.remove(window);
        if !self.owners.is_empty() {
            return false;
        }
        // 没人在英文区却还留着记录：是窗口销毁时恢复没成功留下的（见 `window_destroyed`），
        // 任何窗口的离开都顺手补一次；这种补发不算这个窗口欠的
        if !was_owner && self.state.saved.is_none() {
            return false;
        }
        if self.state.apply(false, ime) {
            // 恢复没成功：留着记录，等下一次补发离开。顺手补发的窗口（不是登记者）也要
            // 得到 true：它的前端据此继续算「后端还欠恢复」，下一次进跟随区再补，
            // 不会因为这一次失败就把欠着的恢复忘了
            if was_owner {
                self.owners.insert(window.to_owned());
            }
            true
        } else {
            false
        }
    }

    /// 窗口销毁：前端来不及发离开，从这里直接把它从英文区名单里去掉。
    /// 它是最后一个在英文区的就恢复；恢复没成功时记录保留（`owners` 已空），
    /// 由之后任意窗口的下一次离开补上。没登记过的窗口什么都不做。
    /// 返回处理完以后是不是还欠着一次恢复（见 [`SharedIme::restore_owed`]）。
    fn window_destroyed(&mut self, window: &str, ime: &mut impl ImeControl) -> bool {
        if self.owners.remove(window) && self.owners.is_empty() {
            self.state.apply(false, ime);
        }
        self.restore_owed()
    }

    /// 没有任何窗口在英文区、输入法却还停在英文区设定的状态（恢复没成功）：没有窗口会
    /// 自己想起来补，必须通知所有窗口（`ime_restore_pending`），让下一次焦点进跟随区时补发。
    fn restore_owed(&self) -> bool {
        self.owners.is_empty() && self.state.saved.is_some()
    }
}

/// 进入英文区（`focused = true`）：记下当前转换模式，切到英文；离开：恢复记下的模式。
/// 返回值是"后端现在是否仍认为这个窗口处于英文区"（恢复没成功时为 `true`）。
#[tauri::command]
pub async fn ime_terminal_focus(window: tauri::Window, focused: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let label = window.label().to_owned();
        tauri::async_runtime::spawn_blocking(move || imp::apply(&label, focused))
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, focused);
        Ok(false)
    }
}

/// 事件名：销毁窗口时恢复输入法没成功，没有窗口自己还欠着这次恢复（广播给所有窗口）。
pub const IME_RESTORE_PENDING: &str = "ime_restore_pending";

/// 窗口被销毁（主窗口、文件窗口）：把它从英文区名单里移除，必要时恢复输入法。
/// 返回恢复是否仍欠着（前台不是本进程之类）：此时剩下的窗口前端并不知道，调用方要广播
/// [`IME_RESTORE_PENDING`]，否则主窗口此前没进过英文区，再也不会发离开，这次恢复永远补不上。
pub fn window_destroyed(label: &str) -> bool {
    #[cfg(windows)]
    {
        imp::window_destroyed(label)
    }
    #[cfg(not(windows))]
    {
        let _ = label;
        false
    }
}

/// 窗口销毁时处理输入法：移除名单、必要时恢复；恢复没成功就通知所有窗口补发。
pub fn on_window_destroyed(app: &tauri::AppHandle, label: &str) {
    if window_destroyed(label) {
        use tauri::Emitter;
        // 窗口可能正在关，发不出去无所谓：下一次任何窗口的离开仍会顺手补。
        let _ = app.emit(IME_RESTORE_PENDING, ());
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SendMessageTimeoutW,
        GUITHREADINFO, SMTO_ABORTIFHUNG,
    };

    use super::{ImeControl, SharedIme, CMODE_NATIVE};

    const WM_IME_CONTROL: u32 = 0x0283;
    const IMC_GETOPENSTATUS: usize = 0x0005;
    const IMC_SETOPENSTATUS: usize = 0x0006;
    /// 输入法窗口最多等这么久（毫秒）。百度拼音等第三方输入法实测偶尔要两三百毫秒才回，
    /// 200ms 时会被当成失败；这里在后台线程里等，不卡界面，给到 1 秒。
    const TIMEOUT_MS: u32 = 1000;

    static STATE: Mutex<Option<SharedIme>> = Mutex::new(None);

    pub fn apply(window: &str, focused: bool) -> bool {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .get_or_insert_with(SharedIme::default)
            .apply(window, focused, &mut Win32Ime)
    }

    pub fn window_destroyed(window: &str) -> bool {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .as_mut()
            .is_some_and(|shared| shared.window_destroyed(window, &mut Win32Ime))
    }

    struct Win32Ime;

    /// 实际操作的是输入法的**打开 / 关闭**，不是转换模式：
    /// 百度拼音等第三方输入法自己管中 / 英，不理会 `IMC_SETCONVERSIONMODE`（读回来是写进去的值，
    /// 工具栏照样是「英」或「中」，2026-10-23 实测）；但都认打开 / 关闭——关闭后按键直接是英文，
    /// 重新打开回到中文。微软拼音同样适用。
    ///
    /// 状态机按「转换模式」写的（去掉中文位 = 英文），这里把打开状态映射到中文位上：
    /// 打开 = `CMODE_NATIVE`，关闭 = 0。这样「进终端记下原状态、关掉；离开时打开回去、读回核对」
    /// 的整套逻辑和重试原样不变。
    impl ImeControl for Win32Ime {
        fn read_mode(&mut self) -> Option<usize> {
            let open = send(focused_ime_window()?, IMC_GETOPENSTATUS, 0)?;
            Some(if open != 0 { CMODE_NATIVE } else { 0 })
        }

        fn write_mode(&mut self, mode: usize) -> bool {
            let open = usize::from(mode & CMODE_NATIVE != 0);
            focused_ime_window()
                .and_then(|ime| send(ime, IMC_SETOPENSTATUS, open))
                .is_some()
        }
    }

    /// 前台窗口是本进程的：返回键盘焦点所在窗口的默认输入法窗口。
    fn focused_ime_window() -> Option<HWND> {
        // SAFETY: 都是只读查询；传入的结构体按要求设了 cbSize。
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.is_invalid() {
                return None;
            }
            let mut pid = 0u32;
            let thread = GetWindowThreadProcessId(foreground, Some(&mut pid));
            if pid != GetCurrentProcessId() {
                return None;
            }
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let focus =
                if GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
                    info.hwndFocus
                } else {
                    foreground
                };
            let ime = ImmGetDefaultIMEWnd(focus);
            (!ime.is_invalid()).then_some(ime)
        }
    }

    fn send(ime: HWND, command: usize, value: usize) -> Option<usize> {
        let mut result = 0usize;
        // SAFETY: 发给输入法窗口的标准控制消息，带超时，不会因为对方卡住而永久阻塞。
        let ok = unsafe {
            SendMessageTimeoutW(
                ime,
                WM_IME_CONTROL,
                WPARAM(command),
                LPARAM(value as isize),
                SMTO_ABORTIFHUNG,
                TIMEOUT_MS,
                Some(&mut result),
            )
        };
        (ok.0 != 0).then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 替身：`mode` 是"当前输入法状态"，`reachable` 为假时模拟前台不是本进程（读写都失败）。
    /// `apply_writes` 为假时模拟输入法上下文没关联：写消息有返回，但模式没变。
    /// `readback_fails` 为真时，写完紧接着的那次读取失败（模拟读回超时）。
    struct FakeIme {
        mode: usize,
        reachable: bool,
        apply_writes: bool,
        readback_fails: bool,
        just_wrote: bool,
        writes: Vec<usize>,
    }

    impl FakeIme {
        fn new(mode: usize) -> Self {
            Self {
                mode,
                reachable: true,
                apply_writes: true,
                readback_fails: false,
                just_wrote: false,
                writes: Vec::new(),
            }
        }
    }

    impl ImeControl for FakeIme {
        fn read_mode(&mut self) -> Option<usize> {
            if std::mem::take(&mut self.just_wrote) && self.readback_fails {
                return None;
            }
            self.reachable.then_some(self.mode)
        }

        fn write_mode(&mut self, mode: usize) -> bool {
            if !self.reachable {
                return false;
            }
            if self.apply_writes {
                self.mode = mode;
            }
            self.just_wrote = true;
            self.writes.push(mode);
            true
        }
    }

    /// 微软拼音中文态大致是 NATIVE 加一个常驻位；英文态去掉 NATIVE。
    const CHINESE: usize = 0x401;
    const ENGLISH: usize = 0x400;

    #[test]
    fn enter_switches_to_english_and_leave_restores() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        assert!(state.apply(true, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn duplicate_enter_keeps_the_original_mode() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        // 第二次进入时输入法已经是英文：不能把英文当成"原来的模式"
        state.apply(true, &mut ime);
        assert_eq!(state.saved, Some(CHINESE));
        state.apply(false, &mut ime);
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn duplicate_leave_is_ignored() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        state.apply(false, &mut ime);
        // 用户离开终端后自己切到了英文：再来一次离开不能把它改回去
        ime.mode = ENGLISH;
        ime.writes.clear();
        assert!(!state.apply(false, &mut ime));
        assert!(ime.writes.is_empty());
        assert_eq!(ime.mode, ENGLISH);
    }

    #[test]
    fn leave_without_enter_does_nothing() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        assert!(!state.apply(false, &mut ime));
        assert!(ime.writes.is_empty());
    }

    #[test]
    fn out_of_order_leave_then_enter_ends_in_terminal_state() {
        // 乱序到达：离开先到（被忽略），进入后到。最终是"处于终端焦点"，
        // 记下的是原来的中文，之后前端补发的离开能恢复回去
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(false, &mut ime);
        assert!(state.apply(true, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn restore_failure_keeps_the_saved_mode_until_it_succeeds() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        // 离开时前台已经不是本进程：恢复不了，记录和状态都保留
        ime.reachable = false;
        assert!(state.apply(false, &mut ime));
        assert_eq!(state.saved, Some(CHINESE));
        // 回到本进程后前端补一次离开：恢复成功
        ime.reachable = true;
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn enter_while_unreachable_is_not_recorded() {
        let mut ime = FakeIme::new(CHINESE);
        ime.reachable = false;
        let mut state = TerminalIme::default();
        assert!(!state.apply(true, &mut ime));
        assert_eq!(state.saved, None);
        // 之后能正常进入，记下的是真实的原模式
        ime.reachable = true;
        assert!(state.apply(true, &mut ime));
        assert_eq!(state.saved, Some(CHINESE));
    }

    #[test]
    fn already_english_is_left_alone_and_restored_as_english() {
        // 用户本来就是英文：不重复写，离开时也还是英文（"原来的状态"就是英文）
        let mut ime = FakeIme::new(ENGLISH);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        assert!(ime.writes.is_empty());
        state.apply(false, &mut ime);
        assert_eq!(ime.mode, ENGLISH);
    }

    #[test]
    fn full_width_flag_is_dropped_on_enter_and_restored_on_leave() {
        let full = CHINESE | CMODE_FULLSHAPE;
        let mut ime = FakeIme::new(full);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        assert_eq!(ime.mode, ENGLISH);
        state.apply(false, &mut ime);
        assert_eq!(ime.mode, full);
    }

    #[test]
    fn entering_in_english_never_writes_english_back_on_leave() {
        // 刚启动时读到的是英文（输入法还没激活）：离开终端时不能把英文主动写回去，
        // 否则用户之后在 AI 输入框里切成的中文会被盖掉
        let mut ime = FakeIme::new(ENGLISH);
        let mut state = TerminalIme::default();
        assert!(state.apply(true, &mut ime));
        // 用户在终端外（或系统自己）把输入法切成了中文
        ime.mode = CHINESE;
        assert!(!state.apply(false, &mut ime));
        assert!(ime.writes.is_empty(), "没切过就不该写：{:?}", ime.writes);
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn a_failed_switch_is_not_restored_either() {
        // 切英文没成功（比如消息超时）：输入法没被改过，离开时也不用写
        struct NoWrite(usize);
        impl ImeControl for NoWrite {
            fn read_mode(&mut self) -> Option<usize> {
                Some(self.0)
            }
            fn write_mode(&mut self, _mode: usize) -> bool {
                false
            }
        }
        let mut ime = NoWrite(CHINESE);
        let mut state = TerminalIme::default();
        assert!(state.apply(true, &mut ime));
        assert!(!state.switched);
        // 写一律失败，但因为没改过，离开照样算成功、清掉状态
        assert!(!state.apply(false, &mut ime));
        assert_eq!(state.saved, None);
    }

    #[test]
    fn leave_that_does_not_take_effect_keeps_the_saved_mode_and_retries() {
        // 离开时焦点在不可输入的元素上，输入法上下文没关联：写消息有返回但没生效。
        // 不能当成恢复成功清掉记录，否则之后再也没人去恢复
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        assert_eq!(ime.mode, ENGLISH);
        ime.apply_writes = false;
        assert!(state.apply(false, &mut ime), "没生效要报仍在终端");
        assert_eq!(state.saved, Some(CHINESE));
        assert!(state.switched);
        assert_eq!(ime.mode, ENGLISH);
        // 焦点进了输入框、上下文关联好以后前端再发一次离开：这次生效
        ime.apply_writes = true;
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
        assert!(!state.switched);
    }

    #[test]
    fn leave_with_failed_readback_counts_as_failure() {
        let mut ime = FakeIme::new(CHINESE);
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        ime.readback_fails = true;
        // 写进去了，但读不回来：无法确认，按失败处理，记录保留
        assert!(state.apply(false, &mut ime));
        assert_eq!(state.saved, Some(CHINESE));
        assert!(state.switched);
        ime.readback_fails = false;
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn enter_whose_switch_does_not_take_effect_is_not_marked_switched() {
        let mut ime = FakeIme::new(CHINESE);
        ime.apply_writes = false;
        let mut state = TerminalIme::default();
        // 进入本身算成功（已记下原模式、处于终端焦点），但没切成，离开时什么都不写
        assert!(state.apply(true, &mut ime));
        assert_eq!(state.saved, Some(CHINESE));
        assert!(!state.switched);
        ime.writes.clear();
        assert!(!state.apply(false, &mut ime));
        assert!(ime.writes.is_empty(), "没切过就不该写：{:?}", ime.writes);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn enter_with_failed_readback_keeps_restore_pending() {
        let mut ime = FakeIme::new(CHINESE);
        ime.readback_fails = true;
        let mut state = TerminalIme::default();
        assert!(state.apply(true, &mut ime));
        assert_eq!(ime.mode, ENGLISH, "写入已生效，只是读回失败");
        assert!(state.switched, "无法排除已切换，必须保留恢复义务");
        assert_eq!(state.saved, Some(CHINESE));
        ime.readback_fails = false;
        ime.writes.clear();
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.writes, vec![CHINESE]);
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn uncertain_enter_followed_by_failed_restore_still_retries() {
        let mut ime = FakeIme::new(CHINESE);
        ime.readback_fails = true;
        let mut state = TerminalIme::default();
        state.apply(true, &mut ime);
        assert_eq!(ime.mode, ENGLISH);
        ime.readback_fails = false;
        ime.apply_writes = false;
        assert!(state.apply(false, &mut ime));
        assert_eq!(state.saved, Some(CHINESE));
        assert_eq!(ime.mode, ENGLISH);
        ime.apply_writes = true;
        assert!(!state.apply(false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(state.saved, None);
    }

    #[test]
    fn readback_only_compares_the_managed_bits() {
        // 输入法顺手改了别的位（比如常驻位）：受管位对得上就算写成功，不能因此永远不恢复
        struct Tweaking(usize);
        impl ImeControl for Tweaking {
            fn read_mode(&mut self) -> Option<usize> {
                Some(self.0)
            }
            fn write_mode(&mut self, mode: usize) -> bool {
                self.0 = mode | 0x100;
                true
            }
        }
        let mut ime = Tweaking(CHINESE);
        let mut state = TerminalIme::default();
        assert!(state.apply(true, &mut ime));
        assert!(state.switched);
        assert!(!state.apply(false, &mut ime));
        assert_eq!(state.saved, None);
    }

    #[test]
    fn two_windows_share_one_saved_mode_and_the_last_leave_restores() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        assert!(shared.apply("main", true, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        // 主窗口失焦的延迟离开还没发，文件窗口已经进入：原模式不能被英文覆盖
        assert!(shared.apply("files", true, &mut ime));
        assert_eq!(shared.state.saved, Some(CHINESE));
        // 主窗口的离开到了：文件窗口还在英文区，不恢复，主窗口不再欠恢复
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        // 文件窗口离开：最后一个，恢复原来的中文
        assert!(!shared.apply("files", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(shared.state.saved, None);
    }

    #[test]
    fn leave_from_a_window_that_never_entered_is_ignored() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        assert!(!shared.apply("files", false, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        assert!(shared.state.saved.is_some());
    }

    #[test]
    fn failed_restore_stays_owed_by_the_same_window() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        ime.reachable = false;
        assert!(shared.apply("main", false, &mut ime));
        // 补发时才成功
        ime.reachable = true;
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn enter_that_cannot_read_the_mode_is_not_owned() {
        let mut ime = FakeIme::new(CHINESE);
        ime.reachable = false;
        let mut shared = SharedIme::default();
        assert!(!shared.apply("main", true, &mut ime));
        assert!(shared.owners.is_empty());
        ime.reachable = true;
        assert!(!shared.apply("main", false, &mut ime));
        assert!(ime.writes.is_empty());
    }

    #[test]
    fn destroying_the_only_english_window_restores_for_the_ai_box() {
        // 文件窗口在英文区时被关掉；主窗口之前的离开已被它挡住（返回 false、没恢复）
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        shared.apply("files", true, &mut ime);
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, ENGLISH);
        shared.window_destroyed("files", &mut ime);
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(shared.state.saved, None);
        assert!(shared.owners.is_empty());
    }

    #[test]
    fn destroying_one_of_two_english_windows_keeps_english() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        shared.apply("files", true, &mut ime);
        shared.window_destroyed("files", &mut ime);
        assert_eq!(ime.mode, ENGLISH);
        assert!(shared.state.saved.is_some());
        // 剩下的主窗口离开才恢复
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn destroying_an_unregistered_window_has_no_effect() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.window_destroyed("files", &mut ime);
        assert!(ime.writes.is_empty());
        shared.apply("main", true, &mut ime);
        let writes = ime.writes.len();
        shared.window_destroyed("files", &mut ime);
        assert_eq!(ime.writes.len(), writes);
        assert_eq!(ime.mode, ENGLISH);
        assert!(shared.owners.contains("main"));
    }

    #[test]
    fn failed_restore_on_destroy_is_retried_by_the_next_leave_of_any_window() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        shared.apply("files", true, &mut ime);
        shared.apply("main", false, &mut ime);
        // 销毁时前台不是本进程：恢复不了，记录保留
        ime.reachable = false;
        shared.window_destroyed("files", &mut ime);
        assert!(shared.state.saved.is_some());
        assert!(shared.owners.is_empty());
        // 主窗口回到前台后再发一次离开（它本身不欠恢复，返回 false）：补上
        ime.reachable = true;
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert_eq!(shared.state.saved, None);
    }

    #[test]
    fn enter_after_a_failed_destroy_restore_keeps_the_original_mode() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("files", true, &mut ime);
        ime.reachable = false;
        shared.window_destroyed("files", &mut ime);
        ime.reachable = true;
        assert!(shared.apply("main", true, &mut ime));
        assert_eq!(shared.state.saved, Some(CHINESE));
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn a_make_up_leave_that_fails_keeps_reporting_owed_until_it_succeeds() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("files", true, &mut ime);
        ime.reachable = false;
        assert!(shared.window_destroyed("files", &mut ime));
        // 主窗口顺手补发，但窗口不在前台：没成功，仍然欠着（返回 true，前端继续等下一次）
        assert!(shared.apply("main", false, &mut ime));
        assert!(shared.restore_owed());
        assert!(
            !shared.owners.contains("main"),
            "补发的窗口不因此成为登记者"
        );
        ime.reachable = true;
        assert!(!shared.apply("main", false, &mut ime));
        assert_eq!(ime.mode, CHINESE);
        assert!(!shared.restore_owed());
    }

    #[test]
    fn destroying_reports_whether_a_restore_is_still_owed() {
        // 恢复成功：不欠，不用通知别的窗口
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("files", true, &mut ime);
        assert!(!shared.window_destroyed("files", &mut ime));
        assert!(!shared.restore_owed());

        // 恢复没成功：欠着，要广播给剩下的窗口
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("files", true, &mut ime);
        ime.reachable = false;
        assert!(shared.window_destroyed("files", &mut ime));
        assert!(shared.restore_owed());
        // 补上之后不再欠
        ime.reachable = true;
        shared.apply("main", false, &mut ime);
        assert!(!shared.restore_owed());
        assert_eq!(ime.mode, CHINESE);
    }

    #[test]
    fn destroying_does_not_report_owed_while_another_window_is_still_in_english() {
        // 还有窗口在英文区：那个窗口自己会离开，不用通知
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("main", true, &mut ime);
        shared.apply("files", true, &mut ime);
        ime.reachable = false;
        assert!(!shared.window_destroyed("files", &mut ime));
        assert!(!shared.restore_owed());
    }

    #[test]
    fn destroying_an_unregistered_window_reports_nothing_owed() {
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        assert!(!shared.window_destroyed("files", &mut ime));
        assert!(ime.writes.is_empty());
    }

    #[test]
    fn a_leave_when_nothing_is_owed_writes_nothing() {
        // 没有欠恢复时，主窗口进 AI 输入框补发的离开不改输入法
        let mut ime = FakeIme::new(CHINESE);
        let mut shared = SharedIme::default();
        shared.apply("files", true, &mut ime);
        assert!(!shared.window_destroyed("files", &mut ime));
        ime.mode = ENGLISH; // 用户自己切成了英文
        ime.writes.clear();
        assert!(!shared.apply("main", false, &mut ime));
        assert!(ime.writes.is_empty());
        assert_eq!(ime.mode, ENGLISH);
    }
}
