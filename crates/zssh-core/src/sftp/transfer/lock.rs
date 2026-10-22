//! 同一个最终目标同时只允许一个传输任务在写（F04）。
//!
//! 两个任务写同一个文件，哪怕各自的临时文件互不相干，最后一个改名的也会悄悄盖掉前一个的结果。
//! 这里按"规范化后的目标路径"登记：第二个任务拿不到就直接报"另一个任务正在写"，由用户决定
//! 等一等再重试，而不是互相覆盖。

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::sftp::error::SftpError;

// 远端锁的键里带着服务器身份（见 `ssh::connection::server_identity`）：不同服务器上同名的路径
// 互不影响，同一台服务器不管从哪个标签 / 侧栏 / 编辑 / AI / 粘贴来都是同一把锁。

/// 一组正在被写的目标。
#[derive(Default)]
pub struct TargetLocks {
    held: Mutex<HashSet<String>>,
}

/// 持有期间，这个目标别的任务写不了；`Drop` 时释放（任何退出路径都会释放）。
pub struct TargetGuard {
    locks: Arc<TargetLocks>,
    key: String,
}

impl Drop for TargetGuard {
    fn drop(&mut self) {
        self.locks
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

impl TargetLocks {
    /// 本机所有任务共用的一组：本地文件在整个进程里只有一个文件系统。
    pub fn local() -> Arc<TargetLocks> {
        static LOCAL: OnceLock<Arc<TargetLocks>> = OnceLock::new();
        Arc::clone(LOCAL.get_or_init(Arc::default))
    }

    /// 整个进程里所有传输引擎、编辑器保存、AI 写文件共用的远端目标锁。
    ///
    /// 每打开一个 SFTP 标签、每个终端的文件侧栏都会新建一个传输引擎，锁不能挂在引擎上，
    /// 否则两个标签上传同一个远端文件时各拿各的锁，会同时写同一个文件。
    pub fn remote() -> Arc<TargetLocks> {
        static REMOTE: OnceLock<Arc<TargetLocks>> = OnceLock::new();
        Arc::clone(REMOTE.get_or_init(Arc::default))
    }

    /// 登记本地路径（规范化之后比较：大小写、`.`、`..` 不同写法算同一个）。
    pub fn try_acquire_local(self: &Arc<Self>, path: &Path) -> Result<TargetGuard, SftpError> {
        let key = crate::sftp::local::lexical_key(path);
        self.try_acquire_key(key, &path.display().to_string())
    }

    /// 登记远端路径（POSIX，区分大小写），键 = 服务器身份 + 规范化后的路径。
    pub fn try_acquire_remote(
        self: &Arc<Self>,
        server: &str,
        path: &str,
    ) -> Result<TargetGuard, SftpError> {
        let key = crate::sftp::path::normalize(path);
        self.try_acquire_key(format!("remote:{server}:{key}"), path)
    }

    fn try_acquire_key(
        self: &Arc<Self>,
        key: String,
        display: &str,
    ) -> Result<TargetGuard, SftpError> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 目标锁是**树形**的：文件夹锁（`/a`）和它下面任何路径（`/a/b.txt`）冲突，
        // 反过来也一样。上传一个文件只占那一个名字；服务器端复制整个文件夹时占住
        // 目标前缀，复制期间别人不能往里面写。
        if held.iter().any(|existing| keys_conflict(existing, &key)) {
            return Err(SftpError::TargetBusy {
                path: display.to_owned(),
            });
        }
        held.insert(key.clone());
        Ok(TargetGuard {
            locks: Arc::clone(self),
            key,
        })
    }
}

/// 两个锁键是不是同一个路径，或者一个在另一个下面（路径段边界才算）。
fn keys_conflict(left: &str, right: &str) -> bool {
    left == right || under(left, right) || under(right, left)
}

fn under(child: &str, parent: &str) -> bool {
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes().get(parent.len()) == Some(&b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_target_in_different_spellings_is_one_lock() {
        let locks = Arc::new(TargetLocks::default());
        let first = locks
            .try_acquire_local(Path::new("C:\\Users\\x\\Downloads\\a.txt"))
            .unwrap();
        for same in [
            "c:\\users\\X\\downloads\\A.TXT",
            "C:\\Users\\x\\Downloads\\.\\a.txt",
            "C:\\Users\\x\\Downloads\\sub\\..\\a.txt",
        ] {
            let err = locks.try_acquire_local(Path::new(same)).err();
            assert!(
                matches!(err, Some(SftpError::TargetBusy { .. })),
                "{same} 应该和第一个是同一个目标"
            );
        }
        // 别的目标不受影响。
        let other = locks
            .try_acquire_local(Path::new("C:\\Users\\x\\Downloads\\b.txt"))
            .unwrap();
        drop(first);
        // 释放之后可以再拿。
        let again = locks.try_acquire_local(Path::new("C:\\Users\\x\\Downloads\\a.txt"));
        assert!(again.is_ok());
        drop(other);
    }

    #[test]
    fn remote_targets_are_case_sensitive() {
        let locks = Arc::new(TargetLocks::default());
        let _a = locks.try_acquire_remote("u@h:22", "/srv/File").unwrap();
        assert!(locks.try_acquire_remote("u@h:22", "/srv/file").is_ok());
        assert!(matches!(
            locks.try_acquire_remote("u@h:22", "/srv//File").err(),
            Some(SftpError::TargetBusy { .. })
        ));
        // 另一台服务器上同名的路径互不影响。
        assert!(locks.try_acquire_remote("u@other:22", "/srv/File").is_ok());
    }

    #[test]
    fn a_folder_lock_covers_everything_below_it_and_the_other_way_around() {
        let locks = Arc::new(TargetLocks::default());
        let folder = locks.try_acquire_remote("u@h:22", "/srv/dir").unwrap();
        for inside in ["/srv/dir/a.txt", "/srv/dir/sub/b.txt"] {
            assert!(
                matches!(
                    locks.try_acquire_remote("u@h:22", inside).err(),
                    Some(SftpError::TargetBusy { .. })
                ),
                "{inside} 在已占用的文件夹下面，应该冲突"
            );
        }
        // 反过来：占着一个文件时，再占它所在的文件夹也冲突。
        drop(folder);
        let file = locks
            .try_acquire_remote("u@h:22", "/srv/dir/a.txt")
            .unwrap();
        assert!(matches!(
            locks.try_acquire_remote("u@h:22", "/srv/dir").err(),
            Some(SftpError::TargetBusy { .. })
        ));
        // 名字只是前缀相同的兄弟目录不冲突。
        assert!(locks.try_acquire_remote("u@h:22", "/srv/dir2").is_ok());
        assert!(locks
            .try_acquire_remote("u@h:22", "/srv/dir/a.txt2")
            .is_ok());
        drop(file);
    }

    /// 同一个目标用短路径和长路径写，必须算同一把锁，否则两个任务能同时写同一个文件。
    #[cfg(windows)]
    #[test]
    fn short_and_long_paths_to_one_file_share_a_lock() {
        let dir = crate::test_util::TestDir::new("lock-a-rather-long-directory-name");
        let long_dir = crate::pathnorm::normalize(&dir.join("Long Target Directory"));
        std::fs::create_dir_all(&long_dir).unwrap();
        let Some(short_dir) = crate::pathnorm::short_path(&long_dir) else {
            eprintln!("跳过：这个卷上没有 8.3 短名字，造不出短路径写法");
            return;
        };
        let locks = Arc::new(TargetLocks::default());
        let _first = locks.try_acquire_local(&long_dir.join("same.bin")).unwrap();
        assert!(matches!(
            locks.try_acquire_local(&short_dir.join("same.bin")).err(),
            Some(SftpError::TargetBusy { .. })
        ));
    }
}
