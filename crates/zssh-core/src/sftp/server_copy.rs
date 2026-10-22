//! 同一台主机上的"服务器端复制"（M3-5 第 3 节，M3-5b 修正）：数据不经过本机。
//!
//! 按顺序试三种办法，**前一种用不了才走后一种，前一种做了一半失败不会悄悄换下一种**
//! （换了会让用户看到两份半成品）：
//!
//! 1. `copy-data` SFTP 扩展（OpenSSH 9.0 起的 sftp-server 支持）：逐个文件让服务器自己拷；
//! 2. 后台 exec 通道里跑 `cp -a -- '源' '目标'`：服务器上有 `cp` 就行；
//! 3. 经本机中转：走传输队列（`Direction::RemoteCopy`），见 `transfer` 模块。
//!
//! 本模块只管 1 和 2，以及选哪种的判断；3 由传输引擎做。
//!
//! 两种服务器端办法都**先写到同目录的临时名，核对大小之后才替换目标**（R-6c R01 / R02
//! 的规则，M3-5b 第 1 项）：中途失败、取消时只删自己的临时文件，目标原来的内容不动。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::ai::shell_kind::{build_shell_probe_command, parse_shell_probe_output, ShellKind};
use crate::sftp::error::SftpError;
use crate::sftp::path as remote_path;
use crate::sftp::session::{SftpCaps, SftpChannel, COPY_DATA};
use crate::sftp::transfer::endpoint::{part_name, PART_SUFFIX};
use crate::sftp::transfer::lock::TargetLocks;
use crate::ssh::{BoxFuture, Connection};

/// 实际用了哪种办法（报告 / 界面提示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMethod {
    /// SFTP `copy-data` 扩展。
    CopyData,
    /// exec 通道里的 `cp -a`。
    ServerCp,
    /// 经本机中转。
    Relay,
}

impl CopyMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::CopyData => "服务器端复制（copy-data）",
            Self::ServerCp => "服务器端复制（cp）",
            Self::Relay => "经本机中转",
        }
    }
}

/// 服务器声明了 `copy-data` 吗。
pub fn supports_copy_data(caps: &SftpCaps) -> bool {
    caps.has(COPY_DATA)
}

/// 探测远端登录 shell（复用 AI 功能的探针）：只在没有 `copy-data`、要决定能不能
/// 用 `cp` 时问一次。认不出（Windows、乱码、超时）当作不是 POSIX。
pub async fn probe_remote_shell(connection: &Connection) -> Option<ShellKind> {
    let result = connection
        .exec_capture(
            &build_shell_probe_command(),
            crate::ai::shell_kind::SHELL_PROBE_TIMEOUT,
            16 * 1024,
        )
        .await
        .ok()?;
    if result.timed_out || result.cancelled {
        return None;
    }
    parse_shell_probe_output(&result.stdout)
}

/// 没有 `copy-data` 时用 `cp` 还是经本机中转：只有确认远端是 POSIX shell 才执行 `cp`
/// （Windows 服务器上 cmd 会把 `cp` 跑成退出码 1，用户只能放弃；M3-5b 第 4 项）。
pub fn choose_server_copy(
    has_copy_data: bool,
    force_relay: bool,
    shell: Option<ShellKind>,
) -> CopyMethod {
    if force_relay {
        return CopyMethod::Relay;
    }
    if has_copy_data {
        return CopyMethod::CopyData;
    }
    if shell == Some(ShellKind::Posix) {
        CopyMethod::ServerCp
    } else {
        CopyMethod::Relay
    }
}

/// 探测结果给用户看的说法。
pub fn shell_kind_label(kind: Option<ShellKind>) -> &'static str {
    match kind {
        Some(ShellKind::Posix) => "POSIX shell",
        Some(ShellKind::Fish) => "fish",
        Some(ShellKind::Other) => "Windows / 认不出的 shell",
        Some(ShellKind::Unknown) | None => "认不出",
    }
}

/// 单引号转义，让任意路径都能安全放进 shell 命令：`it's` → `'it'\''s'`。
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// 复制一个条目的 `cp` 命令。
///
/// - 目标还不存在：`cp -a -- '源' '目标'`（目录、文件都一样）；
/// - 目标是已有目录、要"覆盖"（合并）：`cp -a -- '源/.' '目标'`，否则 `cp` 会把源复制到
///   `目标/源名` 里面去。
pub fn cp_command(source: &str, target: &str, merge_into_existing_dir: bool) -> String {
    if merge_into_existing_dir {
        let dot = format!("{}/.", remote_path::trim_trailing_slash(source));
        format!("cp -a -- {} {}", shell_quote(&dot), shell_quote(target))
    } else {
        format!("cp -a -- {} {}", shell_quote(source), shell_quote(target))
    }
}

/// `candidate` 是不是 `folder` 本身或者在它里面（按规范化后的路径比，不查服务器）。
/// 把文件夹粘贴到它自己或它的子目录里会无限递归，要拒绝。
pub fn is_same_or_inside(folder: &str, candidate: &str) -> bool {
    remote_path::relative(folder, candidate).is_some()
}

/// 核对 `cp` 副本时最多进多少层文件夹。正常的树远到不了（Linux 路径最长 4096 字节）；
/// 超过它多半是别名或循环在作怪，停下来比一直往下走安全。
pub const MAX_TREE_DEPTH: usize = 1024;

/// copy-data 逐层复制 / 合并时最多进多少层。比 [`MAX_TREE_DEPTH`] 小：这两条路是递归的
/// async 函数，每进一层都要多一截栈，层数太多会先把工作线程的栈用光（实测调试构建
/// 一千层就溢出）。真实目录树很少超过这个深度。
pub const MAX_COPY_DEPTH: usize = 128;

/// 核对 `cp` 副本时一棵树最多数多少个条目。超过就当"无法确认副本完整"，不能无止境地列下去。
pub const MAX_VERIFY_ENTRIES: u64 = 2_000_000;

/// 把 `source` 复制成 `target` 会不会复制进它自己（C21）。
///
/// 只比字符串不够：目标所在的文件夹可能是一个指回源树的符号链接（或者源路径本身走了别名），
/// 那样 copy-data 会把刚建的临时文件夹又当成源的一部分继续复制。所以对源和目标的父文件夹
/// 都做一次 `realpath` 再比较；解析不了就按"无法确认"拒绝，不冒无限复制的险。
/// 源本身是符号链接时不用查：两种服务器端办法都不跟随它（copy-data 跳过，`cp -a` 复制链接本身）。
pub async fn ensure_not_into_itself(
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
) -> Result<(), SftpError> {
    let parent = remote_path::parent(target);
    let name = remote_path::file_name(target);
    check_not_inside(ops, source, target, &parent, Some(&name)).await
}

/// 粘贴前的同一检查，按"粘贴到哪个文件夹"来问：`target_dir` 的真实位置是源本身或在源里面
/// 就拒绝。和 [`ensure_not_into_itself`] 的区别是不拼名字——粘贴到源所在的文件夹（会起
/// "副本"名）是允许的。
pub async fn ensure_folder_not_inside(
    ops: &dyn CopyDataOps,
    source: &str,
    target_dir: &str,
) -> Result<(), SftpError> {
    check_not_inside(ops, source, target_dir, target_dir, None).await
}

async fn check_not_inside(
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
    resolve: &str,
    name: Option<&str>,
) -> Result<(), SftpError> {
    let refuse = |real_target: &str| {
        SftpError::InvalidInput(format!(
            "不能把文件夹 {source} 复制到它自己或它的子文件夹里（目标实际位置是 {real_target}）"
        ))
    };
    if is_same_or_inside(source, target) {
        return Err(refuse(target));
    }
    let attrs = ops.lstat(source).await?;
    if !attrs.is_dir || attrs.is_symlink {
        return Ok(());
    }
    let unsure = |path: &str, err: SftpError| {
        SftpError::Remote(format!(
            "无法确认目标不在源文件夹里（解析不了 {path} 的真实位置：{err}），没有复制"
        ))
    };
    let real_source = ops
        .real_path(source)
        .await
        .map_err(|err| unsure(source, err))?;
    let real_resolved = ops
        .real_path(resolve)
        .await
        .map_err(|err| unsure(resolve, err))?;
    let real_target = match name {
        Some(name) => remote_path::join(&real_resolved, name),
        None => real_resolved,
    };
    if is_same_or_inside(&real_source, &real_target) {
        return Err(refuse(&real_target));
    }
    Ok(())
}

/// 这一次复制自己的临时文件 / 临时文件夹（名字以 `.<任务标识>.zssh-part` 结尾）。
/// 枚举源时要跳过它们：万一目标经别名落在源树里，不能把刚建的副本当成源继续复制。
fn is_own_temp(name: &str, owner: &str) -> bool {
    !owner.is_empty() && name.ends_with(&format!(".{owner}{PART_SUFFIX}"))
}

fn too_deep(path: &str) -> SftpError {
    SftpError::Remote(format!(
        "文件夹嵌套超过 {MAX_COPY_DEPTH} 层，停止复制（可能是符号链接或别名造成的循环）：{path}"
    ))
}

/// `cp` 没做成的两种情况：命令根本没法用（可以换办法）和跑了但失败（必须报错，不能换办法）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpFailure {
    /// 服务器上没有 `cp`（退出码 127）、不让开 exec 通道：命令没有真正执行过。
    NotAvailable(String),
    /// `cp` 跑了、报错了：目标可能已经写了一部分，调用方要把原因告诉用户。
    Failed(String),
    /// 用户取消：通道已经关掉，临时文件按第 1 项清理过。
    Cancelled,
}

impl std::fmt::Display for CpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable(message) | Self::Failed(message) => f.write_str(message),
            Self::Cancelled => f.write_str("已取消"),
        }
    }
}

/// 一条 `cp` 跑完之后的结果（测试可以注入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpRun {
    pub exit_code: Option<u32>,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

/// 跑 `cp` 的方式（生产上是 SSH exec 通道；测试用内存实现，记录命令、注入失败）。
pub trait CpRunner: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        wait: Duration,
        cancel: &'a AtomicBool,
    ) -> BoxFuture<'a, Result<CpRun, CpFailure>>;

    /// 尽力用 shell 删掉一棵树（cp 只在 POSIX 上跑，`rm -rf` 比逐项 SFTP 快得多）。
    /// 返回 `Ok(false)` 表示没删（调用方改用 SFTP 逐项删）。
    fn remove_tree<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        let _ = path;
        Box::pin(async { Ok(false) })
    }
}

impl CpRunner for Connection {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        wait: Duration,
        cancel: &'a AtomicBool,
    ) -> BoxFuture<'a, Result<CpRun, CpFailure>> {
        Box::pin(async move {
            let result = self
                .exec_capture_cancellable(command, wait, 16 * 1024, cancel)
                .await
                .map_err(|err| CpFailure::NotAvailable(format!("没法在服务器上执行命令：{err}")))?;
            Ok(CpRun {
                exit_code: result.exit_code,
                stderr: result.stderr,
                timed_out: result.timed_out,
                cancelled: result.cancelled,
            })
        })
    }

    fn remove_tree<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let command = format!("rm -rf -- {}", shell_quote(path));
            match self
                .exec_capture(&command, Duration::from_secs(60), 4 * 1024)
                .await
            {
                Ok(result) if result.exit_code == Some(0) && !result.timed_out => Ok(true),
                Ok(_) => Ok(false),
                Err(err) => Err(err.to_string()),
            }
        })
    }
}

/// 把 `cp` 的结果（退出码和 stderr）分类。
pub fn classify_cp_result(
    exit_code: Option<u32>,
    stderr: &str,
    timed_out: bool,
) -> Result<(), CpFailure> {
    if timed_out {
        return Err(CpFailure::Failed(
            "服务器上复制超时，没有等到结束（服务器上可能还在继续复制）".to_owned(),
        ));
    }
    match exit_code {
        Some(0) => Ok(()),
        Some(127) => Err(CpFailure::NotAvailable("服务器上找不到 cp 命令".to_owned())),
        Some(code) => Err(CpFailure::Failed(format!(
            "服务器上的 cp 失败（退出码 {code}）：{}",
            stderr.trim()
        ))),
        None => Err(CpFailure::Failed(
            "服务器上的 cp 没有返回退出码，不知道有没有复制成功".to_owned(),
        )),
    }
}

/// 一条路径的属性（服务器端复制用得到的部分）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyAttrs {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

impl CopyAttrs {
    fn is_file(&self) -> bool {
        !self.is_dir && !self.is_symlink
    }
}

/// 目录里的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// 目录列表里带的大小（核对 cp 副本时不用再逐项 lstat）。
    pub size: Option<u64>,
}

/// 服务器端复制要用的 SFTP 操作（真实实现是一条 [`SftpChannel`]；测试用内存实现，
/// 可以在复制中途注入失败）。
pub trait CopyDataOps: Send + Sync {
    fn lstat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<CopyAttrs, SftpError>>;
    fn read_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<CopyEntry>, SftpError>>;
    fn create_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn open_read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>>;
    fn open_write_truncate<'a>(&'a self, path: &'a str)
        -> BoxFuture<'a, Result<String, SftpError>>;
    /// 服务器自己把读句柄的内容拷到写句柄（一直拷到文件尾）。
    fn copy_data<'a>(
        &'a self,
        read_handle: &'a str,
        write_handle: &'a str,
    ) -> BoxFuture<'a, Result<(), SftpError>>;
    fn fstat_size<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<u64, SftpError>>;
    fn close_handle<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn remove_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    /// 改名；`overwrite` 时目标存在也替换（服务器有 `posix-rename` 用它，没有走三步）。
    fn rename<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), SftpError>>;
    fn set_permissions<'a>(
        &'a self,
        path: &'a str,
        mode: u32,
    ) -> BoxFuture<'a, Result<(), SftpError>>;
    fn set_mtime<'a>(&'a self, path: &'a str, mtime: u32) -> BoxFuture<'a, Result<(), SftpError>>;
    /// 解析符号链接后的真实路径（SFTP `realpath`）：判断"是不是复制进自己"用。
    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>>;
}

impl CopyDataOps for SftpChannel {
    fn lstat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<CopyAttrs, SftpError>> {
        Box::pin(async move {
            let attrs = SftpChannel::lstat(self, path).await?;
            Ok(CopyAttrs {
                is_dir: attrs.is_dir(),
                is_symlink: attrs.is_symlink(),
                size: attrs.size,
                mtime: attrs.mtime,
                permissions: attrs.permissions,
            })
        })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<CopyEntry>, SftpError>> {
        Box::pin(async move {
            let entries = SftpChannel::read_dir(self, path).await?;
            Ok(entries
                .into_iter()
                .map(|entry| CopyEntry {
                    is_dir: entry.kind.is_dir(),
                    is_symlink: entry.kind.is_symlink(),
                    name: entry.name,
                    path: entry.path,
                    size: entry.size,
                })
                .collect())
        })
    }

    fn create_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::create_dir(self, path).await })
    }

    fn open_read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move { SftpChannel::open_read(self, path).await })
    }

    fn open_write_truncate<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move { SftpChannel::open_write_truncate(self, path).await })
    }

    fn copy_data<'a>(
        &'a self,
        read_handle: &'a str,
        write_handle: &'a str,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(
            async move { SftpChannel::copy_data(self, read_handle, 0, 0, write_handle, 0).await },
        )
    }

    fn fstat_size<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<u64, SftpError>> {
        Box::pin(async move {
            let attrs = SftpChannel::fstat(self, handle).await?;
            Ok(attrs.size.unwrap_or(0))
        })
    }

    fn close_handle<'a>(&'a self, handle: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::close_handle(self, handle).await })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::remove_file(self, path).await })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::remove_dir(self, path).await })
    }

    fn rename<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            SftpChannel::rename(self, from, to, overwrite)
                .await
                .map(|_| ())
        })
    }

    fn set_permissions<'a>(
        &'a self,
        path: &'a str,
        mode: u32,
    ) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::set_permissions(self, path, mode).await })
    }

    fn real_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move { SftpChannel::realpath(self, path).await })
    }

    fn set_mtime<'a>(&'a self, path: &'a str, mtime: u32) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { SftpChannel::set_mtime(self, path, mtime).await })
    }
}

/// 复制进度（界面显示"第 N 个文件"用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyProgress {
    pub files_done: u64,
    pub bytes_done: u64,
    /// 正在复制的文件（最终目标路径）。
    pub current: Option<String>,
}

/// 合并进已有文件夹时，同名文件交给上层决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyConflictInfo {
    pub source: String,
    pub target: String,
    pub source_size: Option<u64>,
    pub target_size: Option<u64>,
    pub source_mtime: Option<u32>,
    pub target_mtime: Option<u32>,
}

/// 同名文件怎么处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyConflictDecision {
    Overwrite,
    Skip,
    /// 换个名字（完整路径）。
    Rename(String),
}

/// 合并时的冲突回调（界面弹冲突对话框；测试用固定策略）。
pub trait CopyConflictResolver: Send + Sync {
    fn decide<'a>(
        &'a self,
        info: CopyConflictInfo,
    ) -> BoxFuture<'a, Result<CopyConflictDecision, SftpError>>;
}

/// 服务器端复制的参数。
pub struct CopyOptions<'a> {
    /// 临时文件名里的任务标识（同 R-6c R01）。
    pub owner: &'a str,
    /// 远端写锁用的服务器身份（`SftpChannel::server_key`）。
    pub server: &'a str,
    pub cancel: &'a AtomicBool,
    /// 每复制完一个文件报一次（可以做限流）。
    pub progress: Option<&'a (dyn Fn(CopyProgress) + Send + Sync)>,
    /// 合并进已有文件夹时用；没有它时遇到同名文件直接报错（不能悄悄覆盖）。
    pub conflict: Option<&'a dyn CopyConflictResolver>,
}

impl<'a> CopyOptions<'a> {
    pub fn simple(owner: &'a str, server: &'a str, cancel: &'a AtomicBool) -> Self {
        Self {
            owner,
            server,
            cancel,
            progress: None,
            conflict: None,
        }
    }

    fn check_cancel(&self) -> Result<(), SftpError> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(SftpError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn report(&self, stats: &CopyDataStats, current: Option<&str>) {
        if let Some(progress) = self.progress {
            progress(CopyProgress {
                files_done: stats.files,
                bytes_done: stats.bytes,
                current: current.map(str::to_owned),
            });
        }
    }
}

/// 用 `cp` 复制一个条目。返回花了多久。
pub async fn copy_with_cp(
    runner: &dyn CpRunner,
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
    wait: Duration,
    options: &CopyOptions<'_>,
) -> Result<Duration, CpFailure> {
    let started = Instant::now();
    let source_attrs = ops
        .lstat(source)
        .await
        .map_err(|err| CpFailure::Failed(format!("读不了源：{err}")))?;
    let target_exists = match ops.lstat(target).await {
        Ok(attrs) => Some(attrs),
        Err(SftpError::NotFound { .. }) => None,
        Err(err) => return Err(CpFailure::Failed(format!("看不了目标：{err}"))),
    };
    if target_exists.is_some_and(|attrs| attrs.is_dir) {
        return Err(CpFailure::Failed(
            "目标已存在且是文件夹；合并要用 copy-data 或经本机中转".to_owned(),
        ));
    }
    if source_attrs.is_dir {
        ensure_not_into_itself(ops, source, target)
            .await
            .map_err(|err| CpFailure::Failed(err.to_string()))?;
    }
    // 复制期间占着整个目标：文件夹用前缀锁（树里任何路径都不许别的任务写），
    // 文件用同一个路径的锁。拿不到就一个字节都不写。
    let _guard =
        lock_target(options.server, target).map_err(|err| CpFailure::Failed(err.to_string()))?;
    if options.cancel.load(Ordering::SeqCst) {
        return Err(CpFailure::Cancelled);
    }
    // 先复制到同目录的临时名，核对大小之后再替换目标（M3-5b 第 1 项）。
    let temp = part_name(target, options.owner);
    remove_tree(ops, &temp).await;
    let command = cp_command(source, &temp, false);
    let run = runner
        .exec(&command, wait, options.cancel)
        .await
        .map_err(|err| match err {
            CpFailure::Cancelled => CpFailure::Cancelled,
            CpFailure::NotAvailable(message) => CpFailure::NotAvailable(message),
            CpFailure::Failed(message) => CpFailure::Failed(message),
        })?;
    if run.cancelled {
        remove_cp_temp(runner, ops, &temp).await;
        return Err(CpFailure::Cancelled);
    }
    if let Err(err) = classify_cp_result(run.exit_code, &run.stderr, run.timed_out) {
        // 失败只删自己的临时文件，目标原来的内容不动。
        remove_cp_temp(runner, ops, &temp).await;
        return Err(err);
    }
    let verified = if source_attrs.is_dir {
        let expected = tree_totals(ops, source, options.cancel, TreeBudget::DEFAULT).await;
        let actual = tree_totals(ops, &temp, options.cancel, TreeBudget::DEFAULT).await;
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                if expected == actual {
                    Ok(())
                } else {
                    Err(format!(
                        "服务器上复制后大小对不上（源 {} 个文件 / {} 字节，副本 {} 个文件 / {} 字节）",
                        expected.0, expected.1, actual.0, actual.1
                    ))
                }
            }
            (Err(err), _) | (_, Err(err)) => Err(format!("核对大小失败：{err}")),
        }
    } else if let Some(expected) = source_attrs.size {
        match ops.lstat(&temp).await {
            Ok(actual) if actual.size == Some(expected) => Ok(()),
            Ok(actual) => Err(format!(
                "服务器上复制后大小对不上（源 {expected} 字节，副本 {} 字节）",
                actual.size.unwrap_or(0)
            )),
            Err(err) => Err(format!("核对副本大小失败：{err}")),
        }
    } else {
        Ok(())
    };
    if let Err(message) = verified {
        remove_cp_temp(runner, ops, &temp).await;
        return Err(CpFailure::Failed(message));
    }
    let overwrite = target_exists.is_some();
    match ops.rename(&temp, target, overwrite).await {
        Ok(()) => {}
        Err(err) => {
            // R-6c R02：三步覆盖失败、原文件被挪到备用名时，新内容（临时文件）不能删。
            if may_remove_temp(&err) {
                remove_cp_temp(runner, ops, &temp).await;
            }
            return Err(CpFailure::Failed(err.to_string()));
        }
    }
    Ok(started.elapsed())
}

/// 用 `copy-data` 复制一个条目（文件或整棵目录）。符号链接不跟随，也不复制（记在返回值里）。
///
/// 返回复制的文件数和跳过的符号链接。
pub async fn copy_with_copy_data(
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
    options: &CopyOptions<'_>,
) -> Result<CopyDataStats, SftpError> {
    let mut stats = CopyDataStats::default();
    let attrs = ops.lstat(source).await?;
    if attrs.is_symlink {
        stats.skipped_links.push(source.to_owned());
        return Ok(stats);
    }
    if attrs.is_file() {
        let target_attrs = match ops.lstat(target).await {
            Ok(attrs) => Some(attrs),
            Err(SftpError::NotFound { .. }) => None,
            Err(err) => return Err(err),
        };
        if target_attrs.is_some_and(|attrs| attrs.is_dir) {
            return Err(SftpError::Remote(format!(
                "目标是个文件夹，不能用文件覆盖它：{target}"
            )));
        }
        let bytes = copy_file_atomic(ops, source, target, &attrs, options, true).await?;
        stats.files += 1;
        stats.bytes += bytes;
        options.report(&stats, Some(target));
        return Ok(stats);
    }
    ensure_not_into_itself(ops, source, target).await?;
    // 整个文件夹的复制占住目标前缀：目标文件夹（以及它下面）有任何路径正被别的任务
    // 写着就不开始；复制期间别人也不能往里写（M3-5b 第 2 项）。
    let _guard = lock_target(options.server, target)?;
    options.check_cancel()?;
    let target_attrs = match ops.lstat(target).await {
        Ok(attrs) => Some(attrs),
        Err(SftpError::NotFound { .. }) => None,
        Err(err) => return Err(err),
    };
    match target_attrs {
        Some(existing) if existing.is_dir => {
            let Some(resolver) = options.conflict else {
                return Err(SftpError::Remote(format!(
                    "目标文件夹已经存在，合并要先决定同名文件怎么处理：{target}"
                )));
            };
            merge_dir(ops, source, target, options, resolver, &mut stats, 0).await?;
            Ok(stats)
        }
        Some(_) => Err(SftpError::Remote(format!(
            "目标是个文件，不能用文件夹覆盖它：{target}"
        ))),
        None => {
            copy_dir_atomic(ops, source, target, &attrs, options, &mut stats).await?;
            Ok(stats)
        }
    }
}

/// 复制一棵树到还不存在的目标：先复制到同目录的临时文件夹，全部成功之后一次改名到位
/// （M3-5b 第 1 项）。失败或取消时删掉临时文件夹，目标位置不会出现半个文件夹。
async fn copy_dir_atomic(
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
    attrs: &CopyAttrs,
    options: &CopyOptions<'_>,
    stats: &mut CopyDataStats,
) -> Result<(), SftpError> {
    options.check_cancel()?;
    let temp = part_name(target, options.owner);
    // 同一个任务标识的临时文件夹只可能是上一次失败的残留，先清掉（别人的名字里没有这个标识）。
    remove_tree(ops, &temp).await;
    ops.create_dir(&temp).await?;
    let copied = copy_dir_contents(ops, source, target, &temp, options, stats, 0).await;
    if let Err(err) = copied {
        remove_tree(ops, &temp).await;
        return Err(err);
    }
    // 目录权限 / 时间放最后设：源目录只读的话，先设了就写不进去。
    if let Some(mode) = attrs.permissions {
        let _ = ops.set_permissions(&temp, mode & 0o7777).await;
    }
    if let Some(mtime) = attrs.mtime {
        let _ = ops.set_mtime(&temp, mtime).await;
    }
    if let Err(err) = options.check_cancel() {
        remove_tree(ops, &temp).await;
        return Err(err);
    }
    if let Err(err) = ops.rename(&temp, target, false).await {
        remove_tree(ops, &temp).await;
        return Err(err);
    }
    Ok(())
}

/// 把 `source_dir` 的内容复制到 `temp_dir`（整棵树还是临时名）。`final_dir` 是将来
/// 改名之后的位置：每个文件按它拿远端写锁，进度也显示它。
async fn copy_dir_contents(
    ops: &dyn CopyDataOps,
    source_dir: &str,
    final_dir: &str,
    temp_dir: &str,
    options: &CopyOptions<'_>,
    stats: &mut CopyDataStats,
    depth: usize,
) -> Result<(), SftpError> {
    if depth > MAX_COPY_DEPTH {
        return Err(too_deep(source_dir));
    }
    for entry in ops.read_dir(source_dir).await? {
        options.check_cancel()?;
        if is_own_temp(&entry.name, options.owner) {
            continue;
        }
        if entry.is_symlink {
            stats.skipped_links.push(entry.path.clone());
            continue;
        }
        let final_child = remote_path::join(final_dir, &entry.name);
        let temp_child = remote_path::join(temp_dir, &entry.name);
        if entry.is_dir {
            let child_attrs = ops.lstat(&entry.path).await?;
            ops.create_dir(&temp_child).await?;
            Box::pin(copy_dir_contents(
                ops,
                &entry.path,
                &final_child,
                &temp_child,
                options,
                stats,
                depth + 1,
            ))
            .await?;
            if let Some(mode) = child_attrs.permissions {
                let _ = ops.set_permissions(&temp_child, mode & 0o7777).await;
            }
            if let Some(mtime) = child_attrs.mtime {
                let _ = ops.set_mtime(&temp_child, mtime).await;
            }
        } else {
            let child_attrs = ops.lstat(&entry.path).await?;
            let bytes = copy_file_contents(ops, &entry.path, &temp_child, &child_attrs).await?;
            stats.files += 1;
            stats.bytes += bytes;
            options.report(stats, Some(&final_child));
        }
    }
    Ok(())
}

/// 合并进已有文件夹：目录递归合并，同名的**文件**逐个问用户（M3-5b 第 5 项）。
async fn merge_dir(
    ops: &dyn CopyDataOps,
    source_dir: &str,
    target_dir: &str,
    options: &CopyOptions<'_>,
    resolver: &dyn CopyConflictResolver,
    stats: &mut CopyDataStats,
    depth: usize,
) -> Result<(), SftpError> {
    if depth > MAX_COPY_DEPTH {
        return Err(too_deep(source_dir));
    }
    for entry in ops.read_dir(source_dir).await? {
        options.check_cancel()?;
        if is_own_temp(&entry.name, options.owner) {
            continue;
        }
        if entry.is_symlink {
            stats.skipped_links.push(entry.path.clone());
            continue;
        }
        let dest = remote_path::join(target_dir, &entry.name);
        let existing = match ops.lstat(&dest).await {
            Ok(attrs) => Some(attrs),
            Err(SftpError::NotFound { .. }) => None,
            Err(err) => return Err(err),
        };
        if entry.is_dir {
            let source_attrs = ops.lstat(&entry.path).await?;
            match existing {
                Some(attrs) if attrs.is_dir => {
                    Box::pin(merge_dir(
                        ops,
                        &entry.path,
                        &dest,
                        options,
                        resolver,
                        stats,
                        depth + 1,
                    ))
                    .await?;
                }
                Some(_) => {
                    return Err(SftpError::Remote(format!(
                        "目标是个文件，不能用文件夹覆盖它：{dest}"
                    )))
                }
                None => {
                    ops.create_dir(&dest).await?;
                    Box::pin(merge_dir(
                        ops,
                        &entry.path,
                        &dest,
                        options,
                        resolver,
                        stats,
                        depth + 1,
                    ))
                    .await?;
                    if let Some(mode) = source_attrs.permissions {
                        let _ = ops.set_permissions(&dest, mode & 0o7777).await;
                    }
                    if let Some(mtime) = source_attrs.mtime {
                        let _ = ops.set_mtime(&dest, mtime).await;
                    }
                }
            }
            continue;
        }
        let source_attrs = ops.lstat(&entry.path).await?;
        match existing {
            None => {
                let bytes =
                    copy_file_atomic(ops, &entry.path, &dest, &source_attrs, options, false)
                        .await?;
                stats.files += 1;
                stats.bytes += bytes;
                options.report(stats, Some(&dest));
            }
            Some(target_attrs) if target_attrs.is_dir => {
                return Err(SftpError::Remote(format!(
                    "目标是个文件夹，不能用文件覆盖它：{dest}"
                )))
            }
            Some(target_attrs) => {
                let decision = resolver
                    .decide(CopyConflictInfo {
                        source: entry.path.clone(),
                        target: dest.clone(),
                        source_size: source_attrs.size,
                        target_size: target_attrs.size,
                        source_mtime: source_attrs.mtime,
                        target_mtime: target_attrs.mtime,
                    })
                    .await?;
                let chosen = match decision {
                    CopyConflictDecision::Skip => {
                        stats.skipped_files += 1;
                        continue;
                    }
                    CopyConflictDecision::Overwrite => dest,
                    CopyConflictDecision::Rename(path) => {
                        if ops.lstat(&path).await.is_ok() {
                            return Err(SftpError::AlreadyExists { path });
                        }
                        path
                    }
                };
                let bytes =
                    copy_file_atomic(ops, &entry.path, &chosen, &source_attrs, options, false)
                        .await?;
                stats.files += 1;
                stats.bytes += bytes;
                options.report(stats, Some(&chosen));
            }
        }
    }
    Ok(())
}

/// 复制单个文件到最终目标：先写同目录的临时名、核对大小、再替换目标（M3-5b 第 1 项）。
async fn copy_file_atomic(
    ops: &dyn CopyDataOps,
    source: &str,
    target: &str,
    attrs: &CopyAttrs,
    options: &CopyOptions<'_>,
    // 调用方是否已经占着这个目标的锁（合并 / 整个文件夹复制时外层已经占了）。
    lock: bool,
) -> Result<u64, SftpError> {
    let _guard = if lock {
        Some(lock_target(options.server, target)?)
    } else {
        None
    };
    options.check_cancel()?;
    let temp = part_name(target, options.owner);
    let bytes = match copy_file_contents(ops, source, &temp, attrs).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // 内容没写完整：删掉自己的临时文件，目标不动。
            remove_tree(ops, &temp).await;
            return Err(err);
        }
    };
    if let Some(expected) = attrs.size {
        if bytes != expected {
            remove_tree(ops, &temp).await;
            return Err(SftpError::Remote(format!(
                "复制后大小对不上（源 {expected} 字节，副本 {bytes} 字节）：{target}"
            )));
        }
    }
    if let Err(err) = options.check_cancel() {
        remove_tree(ops, &temp).await;
        return Err(err);
    }
    commit_temp(ops, &temp, target).await?;
    Ok(bytes)
}

/// 把文件内容写到 `dest`（用它当临时名还是临时目录里的名字由调用方决定），核对大小、
/// 保留权限和时间；失败时不删 `dest`（调用方决定怎么清理）。
async fn copy_file_contents(
    ops: &dyn CopyDataOps,
    source: &str,
    dest: &str,
    attrs: &CopyAttrs,
) -> Result<u64, SftpError> {
    let read = ops.open_read(source).await?;
    let write = match ops.open_write_truncate(dest).await {
        Ok(handle) => handle,
        Err(err) => {
            let _ = ops.close_handle(&read).await;
            return Err(err);
        }
    };
    let copied = ops.copy_data(&read, &write).await;
    let actual = if copied.is_ok() {
        ops.fstat_size(&write).await.ok()
    } else {
        None
    };
    let closed_read = ops.close_handle(&read).await;
    let closed_write = ops.close_handle(&write).await;
    copied?;
    closed_read?;
    closed_write?;
    let bytes = actual.unwrap_or(0);
    if let Some(expected) = attrs.size {
        if bytes != expected {
            return Err(SftpError::Remote(format!(
                "复制后大小对不上（源 {expected} 字节，副本 {bytes} 字节）：{dest}"
            )));
        }
    }
    // 像 `cp -a` 一样保留权限和修改时间（尽力而为：设不上不算复制失败）。
    if let Some(mode) = attrs.permissions {
        let _ = ops.set_permissions(dest, mode & 0o7777).await;
    }
    if let Some(mtime) = attrs.mtime {
        let _ = ops.set_mtime(dest, mtime).await;
    }
    Ok(bytes)
}

/// 提交临时文件：有 `posix-rename` 用它，没有走 R-6c R02 的三步替换（`SftpChannel::rename`
/// 内部决定）。失败时按 R-6c 的规则决定能不能删临时文件。
async fn commit_temp(ops: &dyn CopyDataOps, temp: &str, target: &str) -> Result<(), SftpError> {
    match ops.rename(temp, target, true).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if may_remove_temp(&err) {
                remove_tree(ops, temp).await;
            }
            Err(err)
        }
    }
}

/// 覆盖改名失败时临时文件还能不能删：原文件被挪到备用名 / 结果不确定时，两份完整内容
/// 都不能动（R-6c R02）。
fn may_remove_temp(err: &SftpError) -> bool {
    !matches!(
        err,
        SftpError::ReplaceFailed {
            state: crate::sftp::error::ReplaceState::OriginalAside { .. }
                | crate::sftp::error::ReplaceState::Uncertain,
            ..
        }
    )
}

/// `copy-data` 复制的统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyDataStats {
    pub files: u64,
    pub bytes: u64,
    pub skipped_links: Vec<String>,
    /// 合并时按用户选择跳过的同名文件数。
    pub skipped_files: u64,
}

/// 数一棵树时的预算（C09）：条目数和层数都有上限，超过就当"无法确认"。
#[derive(Debug, Clone, Copy)]
struct TreeBudget {
    max_entries: u64,
    max_depth: usize,
}

impl TreeBudget {
    const DEFAULT: Self = Self {
        max_entries: MAX_VERIFY_ENTRIES,
        max_depth: MAX_TREE_DEPTH,
    };
}

/// 数一棵树：文件数、字节数。符号链接（包括指向目录的）按链接本身算一个条目、
/// 字节数是链接本身的大小，**不跟进去**：`cp -a` 复制的就是链接本身，跟进去会被循环链接
/// 困住、或者数到树外面去（C09）。
async fn tree_totals(
    ops: &dyn CopyDataOps,
    root: &str,
    cancel: &AtomicBool,
    budget: TreeBudget,
) -> Result<(u64, u64), SftpError> {
    let attrs = ops.lstat(root).await?;
    if !attrs.is_dir {
        return Ok((1, attrs.size.unwrap_or(0)));
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut seen = 0u64;
    let mut pending = vec![(root.to_owned(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(SftpError::Cancelled);
        }
        if depth > budget.max_depth {
            return Err(SftpError::Remote(format!(
                "文件夹嵌套超过 {} 层，无法确认副本完整：{dir}",
                budget.max_depth
            )));
        }
        for entry in ops.read_dir(&dir).await? {
            if cancel.load(Ordering::SeqCst) {
                return Err(SftpError::Cancelled);
            }
            seen += 1;
            if seen > budget.max_entries {
                return Err(SftpError::Remote(format!(
                    "条目超过 {} 个，无法确认副本完整：{root}",
                    budget.max_entries
                )));
            }
            if entry.is_dir && !entry.is_symlink {
                pending.push((entry.path.clone(), depth + 1));
            } else {
                files += 1;
                bytes += entry.size.unwrap_or(0);
            }
        }
    }
    Ok((files, bytes))
}

/// 清理 `cp` 的临时文件 / 临时文件夹：能用 `rm -rf` 就用（大目录快几个数量级），
/// 不行再走 SFTP 逐项删。
async fn remove_cp_temp(runner: &dyn CpRunner, ops: &dyn CopyDataOps, temp: &str) {
    match runner.remove_tree(temp).await {
        Ok(true) => {}
        _ => remove_tree(ops, temp).await,
    }
}

/// 远端写锁：服务器端复制写一个目标之前先拿锁（M3-5b 第 2 项）。
fn lock_target(
    server: &str,
    path: &str,
) -> Result<crate::sftp::transfer::lock::TargetGuard, SftpError> {
    TargetLocks::remote().try_acquire_remote(server, path)
}

/// 递归删掉一棵树（失败只记日志；清理自己的临时文件时用）。
async fn remove_tree(ops: &dyn CopyDataOps, path: &str) {
    let attrs = match ops.lstat(path).await {
        Ok(attrs) => attrs,
        Err(_) => return,
    };
    if attrs.is_dir {
        if let Ok(entries) = ops.read_dir(path).await {
            for entry in entries {
                if entry.is_dir && !entry.is_symlink {
                    Box::pin(remove_tree(ops, &entry.path)).await;
                } else {
                    let _ = ops.remove_file(&entry.path).await;
                }
            }
        }
        let _ = ops.remove_dir(path).await;
    } else {
        let _ = ops.remove_file(path).await;
    }
}

#[cfg(test)]
mod tests;
