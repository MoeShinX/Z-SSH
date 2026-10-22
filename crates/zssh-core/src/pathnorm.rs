//! 路径比较前的规范化。
//!
//! Windows 上同一个目录可以有好几种写法：8.3 短路径（`C:\Users\RUNNER~1\AppData`，用户名长、
//! 带空格或中文时 `%TEMP%` 和用户目录常常就是这样）、长路径、不同大小写、带 `.` / `..`、
//! `\\?\` 前缀。**按字符串比较路径就会把同一个目录当成两个**。凡是要判断"是不是同一个路径 /
//! 在不在某个目录里"的地方，都先过这里。

use std::path::{Component, Path, PathBuf};

/// 规范化：先做词法整理（`.`、`..`），再把**已经存在的那一部分**解析成长路径
/// （展开 8.3 短名、符号链接，去掉 `\\?\` 前缀），不存在的尾巴原样拼回去。
///
/// 目标文件还没创建时也能比较：只解析它已存在的最近一级父目录。
/// 带盘符但没有根的相对写法（`C:evil.txt`）不碰文件系统，原样返回词法整理的结果。
pub fn normalize(path: &Path) -> PathBuf {
    let cleaned = clean_lexically(path);
    if !cleaned.has_root() || !cleaned.is_absolute() {
        return cleaned;
    }
    // 从长到短找第一个存在的祖先。
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = cleaned.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(current) {
            let mut out = simplify_verbatim(real);
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                current = parent;
            }
            _ => return cleaned,
        }
    }
}

/// 两个路径是不是同一个位置（Windows 上不分大小写，短路径和长路径算同一个）。
pub fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// `path` 是不是 `dir` 本身或在它下面。
pub fn is_within(dir: &Path, path: &Path) -> bool {
    let dir = parts(dir);
    let path = parts(path);
    path.starts_with(&dir)
}

/// 比较用的键。
pub fn key(path: &Path) -> String {
    parts(path).join("\\")
}

fn parts(path: &Path) -> Vec<String> {
    normalize(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

fn clean_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 越过根就停在根上，不往外跑。
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir) | Some(Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 去掉 Windows 的 `\\?\` 前缀（`\\?\UNC\server\share` 还原成 `\\server\share`）。
pub fn simplify_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// 取一个**已存在**路径的 8.3 短路径写法（测试用）。文件系统关了 8.3 名字时返回 `None`
/// （短路径和长路径是同一个字符串，没法造出"两种写法"）。
#[cfg(all(windows, test))]
pub(crate) fn short_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` 以 0 结尾，调用期间一直活着；缓冲区长度和传入的一致。
    let needed = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), None) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    let written = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buffer)) };
    if written == 0 {
        return None;
    }
    let short = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..written as usize]));
    // 没有变化 = 这个路径本来就没有 8.3 形式（目录名够短，或者卷上关了 8.3）。
    (short.as_os_str().to_string_lossy().to_lowercase()
        != path.as_os_str().to_string_lossy().to_lowercase())
    .then_some(short)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个名字很长、一定有 8.3 形式的目录；卷上关了 8.3 就返回 `None` 并打印原因。
    #[cfg(windows)]
    fn long_and_short() -> Option<(crate::test_util::TestDir, PathBuf, PathBuf)> {
        let dir = crate::test_util::TestDir::new("pathnorm-a-rather-long-directory-name");
        let sub = dir.join("Another Long Sub Directory");
        std::fs::create_dir_all(&sub).unwrap();
        let long = normalize(&sub);
        match short_path(&sub) {
            Some(short) => Some((dir, long, short)),
            None => {
                eprintln!(
                    "跳过：这个卷上没有 8.3 短名字（可能关闭了 8.3 名字生成），造不出短路径写法"
                );
                None
            }
        }
    }

    #[test]
    fn normalize_cleans_dots_and_parent_components() {
        assert_eq!(
            normalize(Path::new(r"C:\a\.\b\..\c")),
            PathBuf::from(r"C:\a\c")
        );
        // 越过根停在根上。
        assert_eq!(normalize(Path::new(r"C:\..\..\x")), PathBuf::from(r"C:\x"));
    }

    #[test]
    fn a_drive_relative_name_does_not_touch_the_file_system() {
        // `C:evil.txt` 不能被展开成"C 盘当前目录下的 evil.txt"。
        assert_eq!(
            normalize(Path::new("C:evil.txt")),
            PathBuf::from("C:evil.txt")
        );
    }

    #[cfg(windows)]
    #[test]
    fn short_and_long_spellings_are_the_same_path() {
        let Some((_dir, long, short)) = long_and_short() else {
            return;
        };
        assert_ne!(short, long, "测试前提：两种写法字符串不同");
        assert!(same_path(&short, &long));
        assert_eq!(key(&short), key(&long));
        // 不分大小写。
        assert!(same_path(
            &PathBuf::from(long.to_string_lossy().to_uppercase()),
            &short
        ));
        // 目标还不存在：只解析已存在的父目录，再拼上文件名。
        assert!(same_path(&short.join("new.txt"), &long.join("new.txt")));
        assert!(is_within(&short, &long.join("deep").join("new.txt")));
        assert!(is_within(&long, &short.join("x")));
        assert!(!is_within(&short, &long.parent().unwrap().join("other")));
    }
}
