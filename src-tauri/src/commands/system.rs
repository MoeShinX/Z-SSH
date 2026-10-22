//! 读取系统层面的用户设置（界面要跟着系统走的那种）。

/// 系统没给出可用值（或不是 Windows）时用的双击时间，等于 Windows 的默认档。
const DEFAULT_DOUBLE_CLICK_MS: u32 = 500;
/// Windows 允许设置的双击时间上限（`SetDoubleClickTime` 的文档范围是 1~5000）。
const MAX_DOUBLE_CLICK_MS: u32 = 5000;

/// 系统的双击时间（毫秒）。
///
/// Windows 的「鼠标」设置里可以调（默认 500ms，最大 5000ms）。界面里依赖"两下点击算不算
/// 双击"的地方（主机树属性面板的收起延迟）要跟着它走，否则调慢双击的用户会看到面板闪烁。
/// 其他平台、读不到或读到 0 都返回 500。
#[tauri::command]
pub fn system_double_click_time() -> u32 {
    normalize_double_click_ms(raw_double_click_time())
}

/// 把系统给的原始值收进合法范围：0 当作读失败，超过上限的截到上限。
fn normalize_double_click_ms(raw: u32) -> u32 {
    match raw {
        0 => DEFAULT_DOUBLE_CLICK_MS,
        value => value.min(MAX_DOUBLE_CLICK_MS),
    }
}

#[cfg(windows)]
fn raw_double_click_time() -> u32 {
    // SAFETY: GetDoubleClickTime 没有参数，只读系统设置。
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
}

#[cfg(not(windows))]
fn raw_double_click_time() -> u32 {
    DEFAULT_DOUBLE_CLICK_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_values_are_pulled_back() {
        assert_eq!(normalize_double_click_ms(0), 500);
        assert_eq!(normalize_double_click_ms(1), 1);
        assert_eq!(normalize_double_click_ms(650), 650);
        assert_eq!(normalize_double_click_ms(5000), 5000);
        assert_eq!(normalize_double_click_ms(u32::MAX), 5000);
    }

    #[test]
    fn the_command_returns_a_usable_value_on_this_machine() {
        // 不断言具体数值（每台电脑的设置不一样），只要落在合法范围里。
        let ms = system_double_click_time();
        assert!((1..=5000).contains(&ms), "双击时间 {ms}ms 不合法");
    }
}
