//! 单元测试用的临时目录守卫。
//!
//! 目录建在系统临时目录下、名字以 `zssh-` 开头，`Drop` 时整棵删掉——
//! 断言失败、panic、提前 return 都会走到。需要在它之前释放的句柄/子进程，
//! 要声明在持有它的结构体里**更靠前**的字段（字段按声明顺序 drop）。

use std::ops::Deref;
use std::path::Path;

/// 临时目录守卫；可以直接当 `&Path` 用（`dir.join(..)`、`&dir`）。
pub struct TestDir(std::path::PathBuf);

impl TestDir {
    pub fn new(prefix: &str) -> Self {
        let prefix = if prefix.starts_with("zssh") {
            format!("{prefix}-")
        } else {
            format!("zssh-{prefix}-")
        };
        let dir = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir()
            .expect("create temp dir");
        // 自己接管删除：Windows 上后台线程还没放掉文件句柄时第一次删会失败，要重试一会儿。
        Self(dir.keep())
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        for _ in 0..20 {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
        // 句柄被测试进程里还活着的后台任务占着（断言失败提前退出时会这样）：
        // 起一个脱离的 cmd，等本进程退出之后再删，保证任何退出路径都不留目录。
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let _ = std::process::Command::new("cmd")
                .args(["/C", "ping -n 6 127.0.0.1 >nul & rmdir /S /Q"])
                .arg(&self.0)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        eprintln!("临时目录暂时没能删掉，已交给后台清理：{}", self.0.display());
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.0.as_path()
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        self.0.as_path()
    }
}

impl AsRef<std::ffi::OsStr> for TestDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_path().as_os_str()
    }
}
