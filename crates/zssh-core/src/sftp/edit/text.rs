//! 内置编辑器的文本编码与换行（M3-5 第 2 节）。
//!
//! 只接受**合法 UTF-8**（可以带 BOM）。不是 UTF-8、含 NUL 字节（看起来是二进制）、换行符混用的文件
//! **不硬打开**——硬打开再保存会损坏内容或者悄悄改掉整个文件的换行。保存时按打开时的样子写回：
//! BOM 带回去，LF 还是 LF、CRLF 还是 CRLF，没改动的部分逐字节不变。
//!
//! 编辑器里的文本一律用 `\n` 换行（CodeMirror 的文档就是这样），换行风格记在 [`TextFormat`] 里，
//! 保存时再还原。

/// 换行风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
        }
    }
}

/// 打开时文件的样子（保存时原样还原）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextFormat {
    /// 文件开头有 UTF-8 BOM。
    pub bom: bool,
    pub eol: Eol,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            bom: false,
            eol: Eol::Lf,
        }
    }
}

/// 解码成功的结果：文本（换行已统一成 `\n`）+ 文件的样子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub format: TextFormat,
}

/// 内置编辑器不打开这个文件的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// 含 NUL 字节，看起来是二进制。
    Binary,
    /// 不是合法 UTF-8（GBK、UTF-16 等）。
    NotUtf8 { utf16: bool },
    /// 换行符混用（LF 和 CRLF 都有，或者有单独的 CR）。
    MixedEol,
}

impl Refusal {
    /// 给界面的稳定代码（界面据此决定提示的标题）。
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Binary => "binary",
            Refusal::NotUtf8 { .. } => "not_utf8",
            Refusal::MixedEol => "mixed_eol",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Refusal::Binary => {
                "这个文件含有二进制内容（NUL 字节），不能用内置编辑器打开；请下载后再处理".to_owned()
            }
            Refusal::NotUtf8 { utf16: true } => {
                "这个文件是 UTF-16 编码，不是 UTF-8。用内置编辑器保存可能损坏内容，请下载后再处理"
                    .to_owned()
            }
            Refusal::NotUtf8 { utf16: false } => {
                "这个文件不是 UTF-8 编码。用内置编辑器保存可能损坏内容，请下载后再处理".to_owned()
            }
            Refusal::MixedEol => {
                "这个文件的换行符混用了（有的行是 LF，有的是 CRLF 或单独的 CR）。用内置编辑器保存会统一换行、改动整个文件，请下载后再处理".to_owned()
            }
        }
    }
}

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// 把文件内容解成可编辑的文本；不适合内置编辑器的返回 [`Refusal`]。
pub fn decode(bytes: &[u8]) -> Result<Decoded, Refusal> {
    // UTF-16 的 BOM：说得具体一点（这种文件里也有 NUL，先于二进制判断）。
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(Refusal::NotUtf8 { utf16: true });
    }
    if bytes.contains(&0) {
        return Err(Refusal::Binary);
    }
    let (bom, body) = match bytes.strip_prefix(&UTF8_BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let text = std::str::from_utf8(body).map_err(|_| Refusal::NotUtf8 { utf16: false })?;

    // 换行：单独的 CR（后面不是 LF）、LF 和 CRLF 混用都拒绝。
    let mut lf = 0usize;
    let mut crlf = 0usize;
    let raw = body;
    let mut index = 0usize;
    while index < raw.len() {
        match raw[index] {
            b'\r' => {
                if raw.get(index + 1) == Some(&b'\n') {
                    crlf += 1;
                    index += 1;
                } else {
                    return Err(Refusal::MixedEol);
                }
            }
            b'\n' => lf += 1,
            _ => {}
        }
        index += 1;
    }
    let eol = if crlf == 0 {
        Eol::Lf
    } else if lf == 0 {
        Eol::Crlf
    } else {
        return Err(Refusal::MixedEol);
    };

    let text = match eol {
        Eol::Lf => text.to_owned(),
        Eol::Crlf => text.replace("\r\n", "\n"),
    };
    Ok(Decoded {
        text,
        format: TextFormat { bom, eol },
    })
}

/// 把编辑器里的文本还原成文件内容（BOM、换行按打开时的样子）。
pub fn encode(text: &str, format: &TextFormat) -> Vec<u8> {
    // 编辑器里本来只有 `\n`；万一混进 `\r\n` / 单独的 `\r`，先统一再还原，不会出现 `\r\r\n`。
    let normalized;
    let text = if text.contains('\r') {
        normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        normalized.as_str()
    } else {
        text
    };
    let mut out = Vec::with_capacity(text.len() + 8);
    if format.bom {
        out.extend_from_slice(&UTF8_BOM);
    }
    match format.eol {
        Eol::Lf => out.extend_from_slice(text.as_bytes()),
        Eol::Crlf => {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(line.as_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(bytes: &[u8]) -> Decoded {
        decode(bytes).expect("应该能解")
    }

    #[test]
    fn plain_utf8_lf_round_trips_byte_for_byte() {
        let bytes = "server {\n  listen 80;\n  # 中文注释\n}\n".as_bytes();
        let result = decoded(bytes);
        assert_eq!(
            result.format,
            TextFormat {
                bom: false,
                eol: Eol::Lf
            }
        );
        assert_eq!(result.text.as_bytes(), bytes);
        assert_eq!(encode(&result.text, &result.format), bytes);
    }

    #[test]
    fn crlf_is_normalised_for_editing_and_restored_on_save() {
        let bytes = b"one\r\ntwo\r\nthree\r\n";
        let result = decoded(bytes);
        assert_eq!(result.format.eol, Eol::Crlf);
        assert_eq!(result.text, "one\ntwo\nthree\n");
        assert_eq!(encode(&result.text, &result.format), bytes);
    }

    #[test]
    fn a_bom_is_stripped_for_editing_and_put_back_on_save() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("名字=值\r\n".as_bytes());
        let result = decoded(&bytes);
        assert!(result.format.bom);
        assert_eq!(result.text, "名字=值\n");
        assert_eq!(encode(&result.text, &result.format), bytes);
    }

    #[test]
    fn only_the_edited_part_changes_with_bom_and_crlf() {
        let mut original = vec![0xEF, 0xBB, 0xBF];
        original.extend_from_slice(b"alpha\r\nbeta\r\ngamma\r\n");
        let result = decoded(&original);
        // 只改中间一行。
        let edited = result.text.replace("beta", "BETA!");
        let saved = encode(&edited, &result.format);
        let mut expected = vec![0xEF, 0xBB, 0xBF];
        expected.extend_from_slice(b"alpha\r\nBETA!\r\ngamma\r\n");
        assert_eq!(saved, expected);
        // 没有改动：和原文件完全一样。
        assert_eq!(encode(&result.text, &result.format), original);
    }

    #[test]
    fn a_missing_final_newline_stays_missing() {
        for bytes in [
            &b"no newline at the end"[..],
            &b"a\r\nb"[..],
            &b""[..],
            &b"\n"[..],
        ] {
            let result = decoded(bytes);
            assert_eq!(encode(&result.text, &result.format), bytes, "{bytes:?}");
        }
    }

    #[test]
    fn a_file_without_any_newline_is_lf() {
        assert_eq!(decoded(b"just one line").format.eol, Eol::Lf);
        assert_eq!(decoded(b"").format, TextFormat::default());
    }

    #[test]
    fn invalid_utf8_is_refused_with_a_clear_reason() {
        // GBK 编码的「中文」。
        let gbk = [0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode(&gbk), Err(Refusal::NotUtf8 { utf16: false }));
        // 在合法文本中间混进一个坏字节。
        assert_eq!(
            decode(b"ok line\nbad \xFF byte\n"),
            Err(Refusal::NotUtf8 { utf16: false })
        );
        let message = Refusal::NotUtf8 { utf16: false }.message();
        assert!(message.contains("不是 UTF-8"), "{message}");
        assert!(message.contains("下载"), "{message}");
    }

    #[test]
    fn utf16_is_recognised_and_named() {
        let utf16le = [0xFF, 0xFE, b'h', 0, b'i', 0];
        assert_eq!(decode(&utf16le), Err(Refusal::NotUtf8 { utf16: true }));
        let utf16be = [0xFE, 0xFF, 0, b'h', 0, b'i'];
        assert_eq!(decode(&utf16be), Err(Refusal::NotUtf8 { utf16: true }));
        assert!(Refusal::NotUtf8 { utf16: true }
            .message()
            .contains("UTF-16"));
    }

    #[test]
    fn nul_bytes_mean_binary_anywhere_in_the_file() {
        assert_eq!(decode(b"\x7fELF\x02\x01\x00"), Err(Refusal::Binary));
        // 不只是开头：文件深处的 NUL 也算（整个文件最多 5 MB，扫得起）。
        let mut data = vec![b'a'; 20_000];
        data.push(0);
        assert_eq!(decode(&data), Err(Refusal::Binary));
        assert_eq!(Refusal::Binary.code(), "binary");
    }

    #[test]
    fn mixed_line_endings_are_refused() {
        assert_eq!(decode(b"a\nb\r\nc\n"), Err(Refusal::MixedEol));
        assert_eq!(decode(b"a\r\nb\nc\r\n"), Err(Refusal::MixedEol));
        // 老 Mac 的单独 CR。
        assert_eq!(decode(b"a\rb\rc"), Err(Refusal::MixedEol));
        // 末尾的单独 CR 也算。
        assert_eq!(decode(b"line\r"), Err(Refusal::MixedEol));
        assert_eq!(Refusal::MixedEol.code(), "mixed_eol");
    }

    #[test]
    fn a_lone_lf_file_with_many_lines_is_fine() {
        let text = "x\n".repeat(10_000);
        let result = decoded(text.as_bytes());
        assert_eq!(result.format.eol, Eol::Lf);
        assert_eq!(encode(&result.text, &result.format), text.as_bytes());
    }

    #[test]
    fn stray_carriage_returns_from_the_editor_never_double_up() {
        let format = TextFormat {
            bom: false,
            eol: Eol::Crlf,
        };
        assert_eq!(encode("a\r\nb\rc\nd", &format), b"a\r\nb\r\nc\r\nd");
        assert_eq!(encode("a\r\nb", &TextFormat::default()), b"a\nb");
    }

    #[test]
    fn emoji_and_multibyte_text_survive() {
        let text = "名字 🚀 값 ñ\r\n第二行\r\n";
        let result = decoded(text.as_bytes());
        assert_eq!(result.text, "名字 🚀 값 ñ\n第二行\n");
        assert_eq!(encode(&result.text, &result.format), text.as_bytes());
    }

    #[test]
    fn every_valid_input_round_trips_exactly() {
        // 一批形状各异的合法输入：解码再编码必须逐字节相同。
        let samples: [&[u8]; 8] = [
            b"",
            b"\n",
            b"\r\n",
            b"\n\n\n",
            b"\r\n\r\n",
            b"trailing spaces   \n  leading\n",
            "\u{FEFF}in-text BOM char after the real BOM".as_bytes(),
            b"tab\tseparated\tvalues\n",
        ];
        for bytes in samples {
            let result = decoded(bytes);
            assert_eq!(encode(&result.text, &result.format), bytes, "{bytes:?}");
        }
    }
}
