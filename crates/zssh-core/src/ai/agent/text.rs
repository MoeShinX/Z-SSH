//! 读写用户文件的小工具（R-2 第 2、3 节）。
//!
//! 用户目录里的东西不能假设：路径带空格和中文、文件不存在、文件是空的、格式坏了、
//! 编码不是 UTF-8、开头有 BOM——都不能让调用方崩。这里只提供"读成文本"这一步的
//! 容错，**怎么解释内容由调用方决定**（读不出来要按"不确定"处理，不能当成"没有"）。

use std::path::Path;

/// 按字节读文件，用有损转换变成文本：**非 UTF-8 不报错**，坏字节变成替换字符。
///
/// 为什么用有损转换而不是直接失败：用户配置文件里混进一两个非法字节（手改过、
/// 用了 GBK 保存）时，我们仍然想读到里面的 ASCII 结构（段头、键名），
/// 而不是整体放弃、把用户配的东西当成"没有"。
pub fn read_text_lossy(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(text_lossy(&bytes))
}

/// 同上，但内容已经在手上。
pub fn text_lossy(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    strip_bom(&text).to_string()
}

/// 吃掉 UTF-8 BOM（`EF BB BF`），否则文件开头那个键会被当成 `\u{feff}key`。
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(name: &str) -> crate::test_util::TestDir {
        crate::test_util::TestDir::new(&format!("text-{name}"))
    }

    #[test]
    fn lossy_read_keeps_ascii_structure_from_broken_encoding() {
        let dir = temp_path("encoding");
        let path = dir.join("config.toml");
        // GBK 的"中"是 0xD6 0xD0：非法 UTF-8，但段头还是能读出来。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"[mcp_servers.my_mcp_server]\n# ");
        bytes.extend_from_slice(&[0xD6, 0xD0]);
        bytes.push(b'\n');
        fs::write(&path, &bytes).unwrap();
        let text = read_text_lossy(&path).unwrap();
        assert!(text.contains("[mcp_servers.my_mcp_server]"), "{text:?}");
    }

    #[test]
    fn bom_is_stripped() {
        let dir = temp_path("bom");
        let path = dir.join("settings.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"model":"x"}"#);
        fs::write(&path, &bytes).unwrap();
        let text = read_text_lossy(&path).unwrap();
        assert!(text.starts_with('{'), "{text:?}");
    }

    #[test]
    fn missing_and_empty_files_are_none_or_empty() {
        let dir = temp_path("empty");
        assert!(read_text_lossy(&dir.join("不存在.toml")).is_none());
        let empty = dir.join("empty.json");
        fs::write(&empty, b"").unwrap();
        assert_eq!(read_text_lossy(&empty).unwrap(), "");
    }
}
