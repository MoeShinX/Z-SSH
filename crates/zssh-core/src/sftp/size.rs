//! 「属性」对话框里的"计算大小"（M3-5 第 4 节）：递归数文件夹里有多少文件、多少字节。
//!
//! 可以随时取消；符号链接不跟随也不计入（和复制、删除的规则一致）；读不了的子目录记下来继续
//! 数，不让一个没权限的目录毁掉整个结果。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sftp::error::SftpError;
use crate::sftp::local;
use crate::sftp::transfer::backend::RemoteLister;

/// 数出来的结果（也是中途的进度）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SizeTotals {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// 跳过的符号链接 / 目录联接。
    pub links: u64,
    /// 读不了的目录数（没权限等）。
    pub unreadable: u64,
}

/// 每数这么多项报一次进度（回调自己再限流）。
const REPORT_EVERY: u64 = 64;

/// 数一个远端路径（文件或目录）。`cancel` 置位后在下一项停下来，返回 `Cancelled`。
pub async fn remote_size(
    lister: &dyn RemoteLister,
    path: &str,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&SizeTotals),
) -> Result<SizeTotals, SftpError> {
    let mut totals = SizeTotals::default();
    let attrs = lister.stat_remote(path).await?;
    if attrs.is_symlink() {
        totals.links += 1;
        return Ok(totals);
    }
    if !attrs.is_dir() {
        totals.files = 1;
        totals.bytes = attrs.size.unwrap_or(0);
        return Ok(totals);
    }
    let mut pending = vec![path.to_owned()];
    let mut seen = 0u64;
    while let Some(dir) = pending.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(SftpError::Cancelled);
        }
        totals.dirs += 1;
        let entries = match lister.list_dir(&dir).await {
            Ok(entries) => entries,
            // 根目录读不了就是整个失败；子目录读不了记下来继续。
            Err(err) if dir == path => return Err(err),
            Err(_) => {
                totals.unreadable += 1;
                continue;
            }
        };
        for entry in entries {
            if entry.kind.is_symlink() {
                totals.links += 1;
            } else if entry.kind.is_dir() {
                pending.push(entry.path.clone());
            } else {
                totals.files += 1;
                totals.bytes += entry.size.unwrap_or(0);
            }
            seen += 1;
            if seen.is_multiple_of(REPORT_EVERY) {
                on_progress(&totals);
            }
        }
    }
    Ok(totals)
}

/// 数一个本地路径（文件或目录）。同步函数，调用方放到阻塞线程里跑。
pub fn local_size(
    path: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&SizeTotals),
) -> Result<SizeTotals, SftpError> {
    let mut totals = SizeTotals::default();
    let long = local::long_path(path);
    let meta = std::fs::symlink_metadata(&long)
        .map_err(|err| crate::sftp::error::from_io(err, &path.display().to_string()))?;
    if local::is_reparse_point(&meta) || meta.file_type().is_symlink() {
        totals.links += 1;
        return Ok(totals);
    }
    if !meta.is_dir() {
        totals.files = 1;
        totals.bytes = meta.len();
        return Ok(totals);
    }
    let mut pending = vec![path.to_path_buf()];
    let mut seen = 0u64;
    while let Some(dir) = pending.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(SftpError::Cancelled);
        }
        totals.dirs += 1;
        let listing = match std::fs::read_dir(local::long_path(&dir)) {
            Ok(listing) => listing,
            Err(err) if dir == path => {
                return Err(crate::sftp::error::from_io(err, &dir.display().to_string()))
            }
            Err(_) => {
                totals.unreadable += 1;
                continue;
            }
        };
        for entry in listing.filter_map(|entry| entry.ok()) {
            let Ok(meta) = entry.metadata() else {
                totals.unreadable += 1;
                continue;
            };
            let file_type = entry.file_type();
            if file_type.map(|kind| kind.is_symlink()).unwrap_or(false)
                || local::is_reparse_point(&meta)
            {
                totals.links += 1;
            } else if meta.is_dir() {
                pending.push(entry.path());
            } else {
                totals.files += 1;
                totals.bytes += meta.len();
            }
            seen += 1;
            if seen.is_multiple_of(REPORT_EVERY) {
                on_progress(&totals);
            }
        }
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::transfer::memory::MemoryEndpoints;

    #[tokio::test]
    async fn remote_size_counts_files_dirs_and_bytes() {
        let endpoints = MemoryEndpoints::new("/remote");
        endpoints.put_remote("/remote/app/a.txt", vec![0; 10]);
        endpoints.put_remote("/remote/app/sub/b.bin", vec![0; 25]);
        endpoints.put_remote("/remote/app/sub/deep/c.bin", vec![0; 5]);
        endpoints.put_remote("/remote/other.txt", vec![0; 999]);
        let totals = remote_size(&endpoints, "/remote/app", &AtomicBool::new(false), |_| {})
            .await
            .unwrap();
        assert_eq!((totals.files, totals.bytes), (3, 40));
        assert_eq!(totals.dirs, 3, "app、sub、deep");
    }

    #[tokio::test]
    async fn remote_size_of_a_single_file_is_its_size() {
        let endpoints = MemoryEndpoints::new("/remote");
        endpoints.put_remote("/remote/one.bin", vec![0; 77]);
        let totals = remote_size(
            &endpoints,
            "/remote/one.bin",
            &AtomicBool::new(false),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!((totals.files, totals.dirs, totals.bytes), (1, 0, 77));
    }

    #[tokio::test]
    async fn a_cancelled_calculation_stops_with_cancelled() {
        let endpoints = MemoryEndpoints::new("/remote");
        for index in 0..200 {
            endpoints.put_remote(
                &format!("/remote/big/d{}/f{index}.bin", index % 7),
                vec![0; 1],
            );
        }
        let cancel = AtomicBool::new(false);
        let outcome = remote_size(&endpoints, "/remote/big", &cancel, |_| {
            cancel.store(true, Ordering::SeqCst);
        })
        .await;
        assert!(matches!(outcome, Err(SftpError::Cancelled)), "{outcome:?}");
    }

    #[test]
    fn local_size_counts_nested_files() {
        let root = crate::test_util::TestDir::new("size-local");
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        std::fs::write(root.join("a").join("x.bin"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("a").join("b").join("y.bin"), vec![0u8; 25]).unwrap();
        let totals = local_size(&root.join("a"), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((totals.files, totals.dirs, totals.bytes), (2, 2, 35));
    }

    #[test]
    fn local_size_reports_progress_and_can_be_cancelled() {
        let root = crate::test_util::TestDir::new("size-local-cancel");
        std::fs::create_dir_all(root.join("d")).unwrap();
        for index in 0..300 {
            std::fs::write(root.join("d").join(format!("f{index}")), b"x").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut reports = 0;
        // 先报进度，再在报告里置取消：下一个目录开始前停下。
        std::fs::create_dir_all(root.join("d").join("next")).unwrap();
        let outcome = local_size(&root.join("d"), &cancel, |_| {
            reports += 1;
            cancel.store(true, Ordering::SeqCst);
        });
        assert!(reports >= 1, "300 项应该报过进度");
        assert!(matches!(outcome, Err(SftpError::Cancelled)), "{outcome:?}");
    }

    #[test]
    fn a_missing_local_path_is_an_error_not_zero() {
        let root = crate::test_util::TestDir::new("size-missing");
        let outcome = local_size(&root.join("nope"), &AtomicBool::new(false), |_| {});
        assert!(outcome.is_err());
    }
}
