//! 远端（POSIX）路径的小工具（M3-1 第 2、3 节）。
//!
//! 远端路径永远是 `/` 分隔的 POSIX 路径，和本机 Windows 的 `\` 不一样；SFTP 的
//! `realpath` 也可能直接给你一个绝对路径。这里只做**文本层面**的拼接与规范化
//! （不查服务器），服务器怎么理解交给它自己。

/// 拼路径：`join("/tmp", "a.txt") = "/tmp/a.txt"`，根目录不会拼出 `//`。
pub fn join(base: &str, name: &str) -> String {
    if base.is_empty() {
        return name.to_owned();
    }
    if name.starts_with('/') {
        return name.to_owned();
    }
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 父目录：`/a/b/c` → `/a/b`；`/a` → `/`；`/` → `/`。
pub fn parent(path: &str) -> String {
    let trimmed = trim_trailing_slash(path);
    match trimmed.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(index) => trimmed[..index].to_owned(),
        None => ".".to_owned(),
    }
}

/// 文件名：`/a/b/c` → `c`。
pub fn file_name(path: &str) -> String {
    let trimmed = trim_trailing_slash(path);
    match trimmed.rfind('/') {
        Some(index) if index + 1 < trimmed.len() => trimmed[index + 1..].to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// 去掉结尾的 `/`（根目录保留）。
pub fn trim_trailing_slash(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

/// 规范化：合并重复的 `/`、消掉 `.`、按文本规则处理 `..`（不做符号链接解析）。
///
/// 为什么不查服务器：`..` 在符号链接之后可能指向别处，只有 `realpath` 才知道；
/// 这里用于"界面上显示、拼孩子路径"这类纯文本用途，够用。
pub fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for piece in path.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                // 绝对路径：`/..` 停在根；相对路径：`..` 保留（可能跳出去）。
                if let Some(last) = parts.last() {
                    if *last == ".." && !absolute {
                        parts.push("..");
                    } else {
                        parts.pop();
                    }
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

/// 扩展名（小写，不含点）：`a/b/Report.TXT` → `txt`。
pub fn extension(path: &str) -> Option<String> {
    let name = file_name(path);
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 >= name.len() {
        return None;
    }
    Some(name[dot + 1..].to_ascii_lowercase())
}

/// 是不是绝对路径。
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `child` 在 `base` 下面的相对路径（不在下面时返回 `None`）。
pub fn relative(base: &str, child: &str) -> Option<String> {
    let base = normalize(base);
    let child = normalize(child);
    if child == base {
        return Some(String::new());
    }
    let prefix = if base.ends_with('/') {
        base.clone()
    } else {
        format!("{base}/")
    };
    child.strip_prefix(&prefix).map(|rest| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_handles_root_and_absolute_children() {
        assert_eq!(join("/tmp", "a.txt"), "/tmp/a.txt");
        assert_eq!(join("/tmp/", "a.txt"), "/tmp/a.txt");
        assert_eq!(join("/", "a.txt"), "/a.txt");
        assert_eq!(join("", "a.txt"), "a.txt");
        // 子路径是绝对路径时直接用子路径（远端 API 就是这个语义）。
        assert_eq!(join("/tmp", "/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn parent_and_file_name() {
        assert_eq!(parent("/a/b/c"), "/a/b");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/a/b/c"), "c");
        assert_eq!(file_name("/a/b/"), "b");
        assert_eq!(file_name("/"), "/");
    }

    #[test]
    fn normalize_collapses_and_resolves() {
        assert_eq!(normalize("/a//b/./c"), "/a/b/c");
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        // 绝对路径不会跳出根。
        assert_eq!(normalize("/../a"), "/a");
        assert_eq!(normalize(".."), "..");
        assert_eq!(normalize("a/../.."), "..");
        assert_eq!(normalize(""), ".");
    }

    #[test]
    fn extensions_and_relative() {
        assert_eq!(extension("/a/Report.TXT").as_deref(), Some("txt"));
        assert_eq!(extension("/a/.bashrc"), None);
        assert_eq!(extension("/a/noext"), None);
        assert_eq!(
            relative("/tmp/root", "/tmp/root/a/b.txt").as_deref(),
            Some("a/b.txt")
        );
        assert_eq!(relative("/tmp/root", "/tmp/root").as_deref(), Some(""));
        assert_eq!(relative("/tmp/root", "/tmp/other/a.txt"), None);
        assert!(is_absolute("/a"));
        assert!(!is_absolute("a"));
    }
}
