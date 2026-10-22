//! 终端获得焦点时把输入法切到英文模式，离开终端时恢复原来的中英文状态。
//!
//! 不是禁止中文：效果等于替用户按了一下 Shift，要打中文照样按 Shift 切回去。
//! 做法是给当前焦点窗口的默认输入法窗口发 `WM_IME_CONTROL`（读 / 设"转换模式"，
//! 去掉 `IME_CMODE_NATIVE` 就是英文），和 im-select 一类工具用的是同一个办法。
//! 微软拼音 / 五笔认这个消息；第三方输入法（百度、搜狗……）认不认看各家实现，
//! 不认的就什么都不发生，不影响输入。
//!
//! 键盘焦点在 WebView2 的子窗口里（别的进程），所以按"前台线程的焦点窗口"找，
//! 并且只在前台窗口属于本进程时才动手，绝不碰别的程序的输入法。
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

/// 终端获得焦点（`focused = true`）：记下当前转换模式，切到英文；失去焦点：恢复记下的模式。
/// 返回值是"后端现在是否仍认为处于终端焦点"（恢复没成功时为 `true`）。
#[tauri::command]
pub async fn ime_terminal_focus(focused: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || imp::apply(focused))
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = focused;
        Ok(false)
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

    use super::{ImeControl, TerminalIme};

    const WM_IME_CONTROL: u32 = 0x0283;
    const IMC_GETCONVERSIONMODE: usize = 0x0001;
    const IMC_SETCONVERSIONMODE: usize = 0x0002;
    /// 输入法窗口卡住时最多等这么久（毫秒），不让界面线程跟着卡。
    const TIMEOUT_MS: u32 = 200;

    static STATE: Mutex<TerminalIme> = Mutex::new(TerminalIme {
        saved: None,
        switched: false,
    });

    pub fn apply(focused: bool) -> bool {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.apply(focused, &mut Win32Ime)
    }

    struct Win32Ime;

    impl ImeControl for Win32Ime {
        fn read_mode(&mut self) -> Option<usize> {
            send(focused_ime_window()?, IMC_GETCONVERSIONMODE, 0)
        }

        fn write_mode(&mut self, mode: usize) -> bool {
            focused_ime_window()
                .and_then(|ime| send(ime, IMC_SETCONVERSIONMODE, mode))
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
}
