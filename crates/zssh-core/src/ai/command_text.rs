//! 命令文本里的不可见字符检查与转义（R-8 第 1 节）。
//!
//! AI 的命令是**当键盘输入**打进用户终端的（[`crate::ai::pty::build_wrapped_command`]
//! 把命令放进单引号再写进终端）。单引号挡得住 shell 解释，挡不住终端行编辑：命令里
//! 混一个 Ctrl+U（`\x15`），行编辑器会先把已经输入的前缀（整段包装脚本）删掉，剩下的
//! 部分单独执行；回车会提前提交；C1 和 Unicode 方向控制字符还能让审批卡片显示的内容
//! 和实际执行的不一样。所以这类字符在工具桥收到命令时就要拒绝，审批卡片再兜底显示成
//! 可见的转义形式。
//!
//! 允许 `\n`、`\t` 通过检查：多行命令和缩进命令要用。但 `\t` 在终端行编辑里是补全键，
//! 所以带 `\t` 的命令只走后台执行通道（R-7 第 0 节，见 [`contains_tab`]）。

/// 命令里有 Tab 时改走后台通道的原因，写进结果的 `fallback_reason`。
pub const TAB_FALLBACK_REASON: &str = "命令里有 Tab，终端会把它当成补全键";

/// 命令里有没有真正的 Tab 字符（不是字面的反斜杠加 t）。
pub fn contains_tab(command: &str) -> bool {
    command.contains('\t')
}

/// 这个字符会不会在终端里产生按键效果、或者让显示的内容和实际内容不一致。
pub fn is_forbidden_control_char(ch: char) -> bool {
    let code = ch as u32;
    // 除换行、制表以外的 C0 控制字符，以及 DEL。
    (code < 0x20 && ch != '\n' && ch != '\t')
        || code == 0x7f
        // C1 控制字符。
        || (0x80..=0x9f).contains(&code)
        // 会改变显示顺序的 Unicode 方向控制字符。
        || (0x202a..=0x202e).contains(&code)
        || (0x2066..=0x2069).contains(&code)
}

/// 找出命令里第一个不允许的控制字符：返回 1 起算的字符序号和字符本身。
pub fn find_forbidden_control_char(command: &str) -> Option<(usize, char)> {
    command
        .chars()
        .enumerate()
        .find(|(_, ch)| is_forbidden_control_char(*ch))
        .map(|(index, ch)| (index + 1, ch))
}

/// 检查一条要执行的命令；含不允许的控制字符时返回给模型的错误。
///
/// 工具桥在审批和执行之前调用它（终端执行和后台执行都算），所以消息要能直接让模型
/// 知道该改什么。
pub fn reject_forbidden_control_chars(command: &str) -> Result<(), String> {
    match find_forbidden_control_char(command) {
        Some((position, ch)) => Err(format!(
            "命令里含有不允许的控制字符 U+{:04X}（第 {position} 个字符）：终端会把它当成按键，\
             可能改变实际执行的命令，已拒绝执行。请去掉这个字符后重试。",
            ch as u32
        )),
        None => Ok(()),
    }
}

/// 把不可见字符改成可见的转义形式（审批卡片的第二道防线）。
///
/// 换行保留原样（多行命令照旧显示）；Tab 显示成 `\t`（它不会进终端，见
/// [`TAB_FALLBACK_REASON`]，但用户要能看见命令里有它）；其余 C0、DEL、C1 和 Unicode 方向
/// 控制字符显示成 `\x15` / `\u{202e}`，让卡片上看到的就是实际要执行的字符。
pub fn escape_invisible_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            out.push_str("\\t");
        } else if !is_forbidden_control_char(ch) {
            out.push(ch);
        } else if (ch as u32) <= 0x7f {
            out.push_str(&format!("\\x{:02x}", ch as u32));
        } else {
            out.push_str(&format!("\\u{{{:04x}}}", ch as u32));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_characters_that_would_be_typed_into_the_terminal() {
        // 第 1 节列出的每一类都要认出来。
        for ch in [
            '\x15', '\x03', '\x04', '\x1b', '\x7f', '\r', '\u{009b}', '\u{202e}',
        ] {
            assert!(
                is_forbidden_control_char(ch),
                "U+{:04X} 要算不允许的控制字符",
                ch as u32
            );
            let command = format!("ls{ch}rm -rf ~");
            let (position, found) = find_forbidden_control_char(&command).expect("要找到");
            assert_eq!(
                position, 3,
                "序号从 1 起算、按字符数（U+{:04X}）",
                ch as u32
            );
            assert_eq!(found, ch);
        }
        // C1 全段和方向控制字符全段。
        for code in 0x80..=0x9f {
            assert!(is_forbidden_control_char(char::from_u32(code).unwrap()));
        }
        for code in 0x202a..=0x202e {
            assert!(is_forbidden_control_char(char::from_u32(code).unwrap()));
        }
        for code in 0x2066..=0x2069 {
            assert!(is_forbidden_control_char(char::from_u32(code).unwrap()));
        }
    }

    #[test]
    fn newline_and_tab_are_allowed() {
        assert!(reject_forbidden_control_chars("printf 'a\\tb\\n'\necho done").is_ok());
        assert!(reject_forbidden_control_chars("echo hi").is_ok());
        assert!(reject_forbidden_control_chars("中文命令 && echo ✓").is_ok());
        assert!(find_forbidden_control_char("echo one\necho two\tend").is_none());
    }

    #[test]
    fn tab_is_shown_as_a_visible_escape_on_the_card() {
        assert_eq!(escape_invisible_chars("echo a\tb"), "echo a\\tb");
        // 换行照旧原样保留（多行命令照旧显示）。
        assert_eq!(escape_invisible_chars("a\nb"), "a\nb");
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        // 中文占多个字节，但报给模型的位置按字符算。
        let (position, ch) = find_forbidden_control_char("中文\x03").expect("要找到");
        assert_eq!(position, 3);
        assert_eq!(ch, '\x03');
    }

    #[test]
    fn the_error_names_the_code_point_so_the_model_can_fix_it() {
        let error = reject_forbidden_control_chars("ls\x15rm -rf ~").expect_err("要拒绝");
        assert!(error.contains("U+0015"), "{error}");
        assert!(error.contains("控制字符"), "{error}");
        assert!(error.contains("第 3 个字符"), "{error}");
    }

    #[test]
    fn escaping_makes_hidden_characters_visible_and_keeps_normal_text() {
        let escaped = escape_invisible_chars("ls\x15rm -rf ~\r\u{009b}\u{202e}\n\t中文");
        assert_eq!(
            escaped, "ls\\x15rm -rf ~\\x0d\\u{009b}\\u{202e}\n\\t中文",
            "不可见字符和 Tab 要转义，换行 / 正常文本原样保留"
        );
        // 转义之后卡片上看到的都是可见字符。
        assert!(!escaped.chars().any(is_forbidden_control_char));
        // 普通命令不受影响，避免卡片上的正常内容被改写。
        assert_eq!(
            escape_invisible_chars("git status --short"),
            "git status --short"
        );
    }
}
