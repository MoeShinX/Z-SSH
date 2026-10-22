//! 智能体的发现（通用，AI-2b 重写；R-2 第 2 节补齐常见安装方式）。
//!
//! 规则（2026-10-16 用户决定）：
//! - **默认自动**：按下面的顺序找第一个 `--version` 能跑通的候选
//! - **手动指定只是兜底**：给了设置路径就只用它，坏了报错、**不回退到自动**
//! - 找到 `.cmd` / `.bat` / `.ps1` / scoop 的 `.shim` 时先拆出里面引用的真实 exe，
//!   直接启动 exe；winget Links 里的符号链接也解析到真实 exe
//!
//! 候选顺序：数据目录 `tools\<智能体>\` → PATH → 常见安装位置。常见安装位置覆盖
//! 包管理器（npm / pnpm / yarn / bun，**问命令要真实全局目录**）、Node 版本管理器
//! （nvm-windows / fnm / volta）、winget、scoop、chocolatey 和各家的官方安装包。
//!
//! 模块做成通用的：AI-3（codex）、AI-4（Claude Code）各提供自己的 [`AgentSpec`]。
//! opencode 自己那份在 [`OPENCODE`]。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;

/// 版本探测的超时（冷启动慢的机器上给足）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// 问包管理器要全局目录时的超时（问不到就跳过，不能让发现卡住）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// 一个智能体的发现规则。
pub struct AgentSpec {
    /// 命令名（不带扩展名），例如 `opencode`。
    pub command: &'static str,
    /// 一键下载装的位置：`<数据目录>\tools\<子目录>\`（为空表示不提供下载）。
    pub tools_subdir: &'static str,
    /// 官方安装脚本 / 独立安装包的位置：`(环境变量名, 相对子路径)`。
    ///
    /// 子路径为空表示**环境变量本身就是那个目录**（例如 codex 的
    /// `$CODEX_INSTALL_DIR`）。环境变量没设就跳过这一条。
    pub extra_bins: &'static [(&'static str, &'static str)],
    /// 最低可用版本；低于它时提示"版本过低，建议下载新版"。
    pub minimum_version: Option<(u64, u64, u64)>,
}

/// opencode 的发现规则。
pub const OPENCODE: AgentSpec = AgentSpec {
    command: "opencode",
    tools_subdir: "opencode",
    // 官方安装脚本装到 `~\.opencode\bin`（设计 2.5）。
    extra_bins: &[("USERPROFILE", ".opencode\\bin")],
    // 1.17.x 在共享数据目录上有 SQLite 外键 bug，1.19.0 没有（见 AI-2 报告）。
    minimum_version: Some((1, 19, 0)),
};

/// Claude Code 的发现规则（AI-4）。
///
/// 主要来源是 npm 全局目录（`claude.ps1` / `claude.cmd`，shim 里直接引用
/// `node_modules\@anthropic-ai\claude-code\bin\claude.exe`）和官方安装脚本的位置
/// `%USERPROFILE%\.local\bin`（设计 2.5）。最低版本 2.0.41 的依据见报告 AI-4 第 1 节。
pub const CLAUDE: AgentSpec = AgentSpec {
    command: "claude",
    tools_subdir: "claude",
    extra_bins: &[("USERPROFILE", ".local\\bin")],
    minimum_version: Some(crate::ai::agent::claude::config::MIN_VERSION),
};

/// codex 的发现规则（AI-3）。
///
/// 主要来源是 npm 全局目录（`codex.ps1` / `codex.cmd` / 裸 shim），要拆到真正的
/// `codex.exe`（见 [`resolve_launch_path`]）；独立安装包（官方
/// `releases.openai.com/codex/install.ps1`）装到 `$CODEX_INSTALL_DIR`，没设就是
/// `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin`。最低版本 0.144.0 的依据见报告 AI-3
/// 第 1 节（0.144.0 实测通过）。
pub const CODEX: AgentSpec = AgentSpec {
    command: "codex",
    tools_subdir: "codex",
    extra_bins: &[
        ("CODEX_INSTALL_DIR", ""),
        ("LOCALAPPDATA", "Programs\\OpenAI\\Codex\\bin"),
    ],
    minimum_version: Some(crate::ai::agent::codex::startup::MIN_VERSION),
};

/// 候选是从哪儿找到的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    /// 设置里手动指定的路径。
    Setting,
    /// 数据目录下 tools\<智能体>\（一键下载装在这）。
    ToolsDir,
    /// 系统 PATH。
    Path,
    /// 其他常见安装位置（npm/pnpm/volta/scoop 全局、官方脚本目录）。
    KnownLocation,
}

/// 被跳过的旧安装（版本低于最低要求，但后面找到了可用的）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedOlder {
    /// `--version` 输出。
    pub version: String,
    /// 这个旧版本是从哪儿找到的（界面据此说"PATH 上还有一个旧版本 x.y.z"）。
    pub source: DiscoverySource,
}

/// 找到的安装。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentInstall {
    /// 可执行文件（或 `.cmd` 包装脚本）的路径。
    pub path: String,
    /// `--version` 输出。
    pub version: String,
    pub source: DiscoverySource,
    /// 是不是没能拆出真实 exe 的 `.cmd` / `.bat`（标准库会经 cmd.exe 启动它）。
    pub needs_shell: bool,
    /// 自动发现的结果（手动指定时为 false）。
    pub auto: bool,
    /// 版本低于最低要求（仅在 [`AgentSpec::minimum_version`] 设了时可能为 true）。
    pub too_old: bool,
    /// 前面找到过、但版本低于最低要求所以被跳过的那个安装（R-2 遗留 3 / M3-1 第 0 节）：
    /// 有它就说明"能用的是后面那个"，界面据此提示旧版本的存在。没有就是 `None`。
    pub skipped_older: Option<SkippedOlder>,
}

/// 一键下载的安装目录：`<数据目录>\tools\<智能体>\`。
pub fn tools_dir(data_dir: &Path, spec: &AgentSpec) -> PathBuf {
    data_dir.join("tools").join(spec.tools_subdir)
}

/// 找可用的智能体。
///
/// - `setting` 有值（去掉空白后非空）：**只用**它，路径不存在 / 探不通都返回 `Err`，
///   绝不回退到自动（用户明确指定了就按指定的来）。
/// - `setting` 为空：按"自动发现为主"的顺序找，找到第一个可用的返回 `Ok(Some(_))`；
///   哪儿都没有返回 `Ok(None)`。
pub async fn discover(
    spec: &AgentSpec,
    setting: Option<&str>,
    data_dir: &Path,
) -> Result<Option<AgentInstall>, String> {
    let env = EnvView::from_process().await;
    discover_with_env(spec, setting, data_dir, &env).await
}

/// 同 [`discover`]，但发现环境由调用方给（单测用假的 PATH / 目录表）。
///
/// 顺序规则：**版本太旧的候选不拦路**——PATH 上有个旧版本时继续往后找，找到满足最低
/// 版本的就用它，并把旧版本记进 [`AgentInstall::skipped_older`]；所有候选都太旧才回退到
/// 第一个旧版本（界面标"版本过低"）。
pub async fn discover_with_env(
    spec: &AgentSpec,
    setting: Option<&str>,
    data_dir: &Path,
    env: &EnvView,
) -> Result<Option<AgentInstall>, String> {
    if let Some(raw) = setting {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if !path.is_file() {
                return Err(format!("手动指定的路径不存在：{}", spec.command));
            }
            return match probe(spec, &path, DiscoverySource::Setting, false).await {
                Ok(install) => Ok(Some(install)),
                Err(err) => Err(format!("手动指定的 {} 不能用：{err}", spec.command)),
            };
        }
    }

    let mut older: Option<AgentInstall> = None;
    for candidate in auto_candidates(spec, data_dir, env) {
        let source = classify_source(&candidate.path, data_dir, spec, candidate.source);
        let Ok(install) = probe(spec, &candidate.path, source, true).await else {
            continue;
        };
        if !install.too_old {
            let skipped_older = older.map(|old| SkippedOlder {
                version: old.version,
                source: old.source,
            });
            return Ok(Some(AgentInstall {
                skipped_older,
                ..install
            }));
        }
        // 太旧：记下第一个（界面提示用的就是它），继续找后面的候选。
        if older.is_none() {
            older = Some(install);
        }
    }
    Ok(older)
}

/// 只探测一个指定路径（设置页「检测」按钮，以及手动模式）。
pub async fn probe_path(spec: &AgentSpec, path: &Path) -> Result<AgentInstall, String> {
    if !path.is_file() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    probe(spec, path, DiscoverySource::Setting, false)
        .await
        .map_err(|e| e.to_string())
}

/// 发现用的环境视图（R-2 第 2 节）。
///
/// 为什么要抽这一层：路径里带空格和中文、装在偏僻位置的情况必须能单测，而单测
/// 不能改真实的环境变量（用例并行跑会互相干扰），也不能真的去问包管理器。
/// 生产用 [`EnvView::from_process`]，单测用 [`EnvView::fake`] 造一份假环境。
pub struct EnvView {
    /// 环境变量（名字统一大写；Windows 上环境变量名不区分大小写）。
    vars: Vec<(String, String)>,
    /// 问包管理器要来的全局目录（`npm config get prefix` 这类命令的结果）。
    manager_dirs: Vec<PathBuf>,
}

impl EnvView {
    /// 真实环境：快照进程环境变量 + 问一次包管理器（进程内缓存，问不到就算了）。
    pub async fn from_process() -> Self {
        Self {
            vars: std::env::vars()
                .map(|(name, value)| (name.to_ascii_uppercase(), value))
                .collect(),
            manager_dirs: package_manager_dirs().await.clone(),
        }
    }

    /// 单测用：环境变量（含 `PATH`）和包管理器目录都由调用方给。
    pub fn fake(vars: &[(&str, &str)], manager_dirs: &[&str]) -> Self {
        Self {
            vars: vars
                .iter()
                .map(|(name, value)| (name.to_ascii_uppercase(), (*value).to_string()))
                .collect(),
            manager_dirs: manager_dirs.iter().map(PathBuf::from).collect(),
        }
    }

    /// 取一个环境变量（名字大小写不敏感；空字符串当没设）。
    pub fn var(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_uppercase();
        self.vars
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.trim().is_empty())
    }

    /// 某个环境变量指向的目录，再拼一层子路径（子路径为空就用它本身）。
    fn dir_of(&self, name: &str, sub: &str) -> Option<PathBuf> {
        let base = PathBuf::from(self.var(name)?);
        Some(if sub.is_empty() { base } else { base.join(sub) })
    }

    /// `%LOCALAPPDATA%` / `%APPDATA%` / `%USERPROFILE%` 下的一个子目录。
    fn local_dir(&self, name: &str, sub: &str) -> Option<PathBuf> {
        self.dir_of(name, sub)
    }
}

/// 问包管理器要全局目录（npm / pnpm / yarn / bun），**进程内只问一次**。
///
/// 为什么不只猜默认路径：`npm config get prefix` 会读用户自己的 `.npmrc`，
/// 有人把 prefix 改到别的盘（R-2 第 2 节）；换用 yarn / bun 装的话默认路径也
/// 各不相同。问不到的（没装这个工具）就跳过，默认路径在 [`known_dirs`] 里兜底。
async fn package_manager_dirs() -> &'static Vec<PathBuf> {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    if let Some(dirs) = DIRS.get() {
        return dirs;
    }
    let mut found = Vec::new();
    for (program, args) in [
        ("npm", &["config", "get", "prefix"][..]),
        ("pnpm", &["bin", "-g"][..]),
        ("yarn", &["global", "bin"][..]),
        ("bun", &["pm", "bin", "-g"][..]),
    ] {
        if let Some(dir) = query_dir(program, args).await {
            if !found.contains(&dir) {
                found.push(dir);
            }
        }
    }
    let _ = DIRS.set(found);
    DIRS.get().expect("刚设过")
}

/// 跑一条命令问一个目录；输出取最后一行非空文本。
///
/// 只认 stdout：pnpm 这类工具在"全局目录不在 PATH 里"时会把警告写到 stderr
/// （实测 `pnpm bin -g` 就是），拿 stderr 当目录就错了。
async fn query_dir(program: &str, args: &[&str]) -> Option<PathBuf> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    hide_console(&mut cmd);
    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(QUERY_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = crate::ai::agent::text::text_lossy(&output.stdout);
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let dir = PathBuf::from(line);
    dir.is_dir().then_some(dir)
}

/// 候选实际算哪一类来源：路径落在数据目录的 tools 下就是"一键下载装的"，不管它是从 PATH、
/// 常见位置还是别处找到的，也不管写的是 8.3 短路径还是长路径（规范化之后再比较）。
fn classify_source(
    path: &Path,
    data_dir: &Path,
    spec: &AgentSpec,
    declared: DiscoverySource,
) -> DiscoverySource {
    let tools = tools_dir(data_dir, spec);
    if crate::pathnorm::is_within(&tools, path) {
        DiscoverySource::ToolsDir
    } else {
        declared
    }
}

/// 一个待探测的候选。
struct Candidate {
    path: PathBuf,
    source: DiscoverySource,
}

/// "自动发现为主"的候选顺序：tools 目录 → PATH → 常见安装位置。
///
/// `ZSSH_AGENT_HIDE_LOCAL`（截图脚本用）置位时跳过 PATH 和常见位置，只找数据目录
/// 下的 tools——这样就能在"本机装了 opencode"的机器上拍出"没找到 opencode"的画面。
fn auto_candidates(spec: &AgentSpec, data_dir: &Path, env: &EnvView) -> Vec<Candidate> {
    let mut out = Vec::new();

    // 1. 数据目录下的 tools\<智能体>\（一键下载装的位置）。
    let tools = tools_dir(data_dir, spec);
    for ext in ["exe", "cmd", "bat", "com"] {
        out.push(Candidate {
            path: tools.join(format!("{}.{}", spec.command, ext)),
            source: DiscoverySource::ToolsDir,
        });
    }

    // 2. PATH：同一目录里对每个名字，先 .exe/.cmd/.bat/.com，再裸 shim。
    if hide_local() {
        return out;
    }
    if let Some(paths) = env.var("PATH") {
        for dir in std::env::split_paths(std::ffi::OsStr::new(paths)) {
            for path in runnable_names(spec, &dir) {
                out.push(Candidate {
                    path,
                    source: DiscoverySource::Path,
                });
            }
        }
    }

    // 3. 其他常见位置。
    for dir in known_dirs(spec, env) {
        for path in runnable_names(spec, &dir) {
            out.push(Candidate {
                path,
                source: DiscoverySource::KnownLocation,
            });
        }
    }
    // 3b. winget 的包目录（`Packages\<包>\...`）：包名带版本和哈希，只能扫。
    for path in winget_package_exes(spec, env) {
        out.push(Candidate {
            path,
            source: DiscoverySource::KnownLocation,
        });
    }

    out
}

/// 截图脚本用的开关：跳过本机 PATH 和常见安装位置。
fn hide_local() -> bool {
    std::env::var_os("ZSSH_AGENT_HIDE_LOCAL").is_some()
}

/// 一个目录里，按扩展名优先级给出候选文件名。
///
/// 同一目录里同时有无扩展名的 shim（npm 的 POSIX 版）和 `.cmd` 时，**先用 `.cmd`**
/// ——裸 shim 在 Windows 上 `cmd /c` 跑不了。
fn runnable_names(spec: &AgentSpec, dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for ext in ["exe", "cmd", "bat", "com"] {
        out.push(dir.join(format!("{}.{}", spec.command, ext)));
    }
    out.push(dir.join(spec.command));
    out
}

/// 其他常见安装位置（按顺序）。
///
/// 三类：
/// 1. **包管理器问出来的真实全局目录**（`npm config get prefix` / `pnpm bin -g` /
///    `yarn global bin` / `bun pm bin -g`）——问不到时下面还有默认路径兜底；
/// 2. 各家默认目录（含 Node 版本管理器、scoop、chocolatey、winget）；
/// 3. 每个智能体自己的官方安装位置（[`AgentSpec::extra_bins`]）。
fn known_dirs(spec: &AgentSpec, env: &EnvView) -> Vec<PathBuf> {
    let mut out = env.manager_dirs.clone();

    // npm 全局（默认 `%APPDATA%\npm`，prefix 改过的话上面那条已经覆盖）。
    if let Some(dir) = env.local_dir("APPDATA", "npm") {
        out.push(dir);
    }
    // pnpm：默认全局目录和受管理的 bin 目录。
    if let Some(dir) = env.local_dir("PNPM_HOME", "") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "pnpm") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "pnpm\\bin") {
        out.push(dir);
    }
    // yarn / bun 的全局目录。
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "Yarn\\bin") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("BUN_INSTALL", "bin") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("USERPROFILE", ".bun\\bin") {
        out.push(dir);
    }

    // Node 版本管理器：nvm-windows 的当前版本（`NVM_SYMLINK` 指到它）、fnm 的别名
    // 目录、volta 的 bin。
    if let Some(dir) = env.local_dir("NVM_SYMLINK", "") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("NVM_HOME", "") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("FNM_DIR", "aliases\\default") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("APPDATA", "fnm\\aliases\\default") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "fnm\\aliases\\default") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("VOLTA_HOME", "bin") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "Volta\\bin") {
        out.push(dir);
    }

    // scoop（`SCOOP` 指到非默认目录时优先）和 chocolatey。
    if let Some(dir) = env.local_dir("SCOOP", "shims") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("USERPROFILE", "scoop\\shims") {
        out.push(dir);
    }
    if let Some(dir) = env.local_dir("ChocolateyInstall", "bin") {
        out.push(dir);
    }
    out.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin"));

    // winget：`Links` 里是指向真实 exe 的符号链接（`Packages\<包>\` 由
    // [`winget_package_exes`] 扫）。
    if let Some(dir) = env.local_dir("LOCALAPPDATA", "Microsoft\\WinGet\\Links") {
        out.push(dir);
    }

    // 官方安装脚本 / 独立安装包（每个智能体自己的位置）。
    for (var, sub) in spec.extra_bins {
        if let Some(dir) = env.local_dir(var, sub) {
            out.push(dir);
        }
    }

    out.retain(|dir| !dir.as_os_str().is_empty());
    out
}

/// winget 装的包在 `%LOCALAPPDATA%\Microsoft\WinGet\Packages\<包>\` 下面，
/// 包名带版本和哈希（例如 `Anthropic.Claude_1.0.0_x64__8wekyb3d8bbwe` 这种样子），
/// 猜不出来，只能逐个包目录里找同名 exe（深度受限）。
fn winget_package_exes(spec: &AgentSpec, env: &EnvView) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(root) = env.local_dir("LOCALAPPDATA", "Microsoft\\WinGet\\Packages") else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return out;
    };
    let mut packages: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    packages.sort();
    let name = format!("{}.exe", spec.command);
    for package in packages {
        if let Some(found) = find_named_file(&package, &name, 4) {
            out.push(found);
        }
    }
    out
}

/// 探测一个候选：先按需要拆包装脚本，再跑一次 `--version`。
async fn probe(
    spec: &AgentSpec,
    path: &Path,
    source: DiscoverySource,
    auto: bool,
) -> Result<AgentInstall, ProbeError> {
    // `.cmd` / `.bat` / `.ps1` 先拆出里面引用的真实 exe，直接启动 exe。
    let launch = resolve_launch_path(path);
    if is_powershell_shim(&launch) {
        // `.ps1` 不能当可执行文件启动（AI-3 第 5 节），拆不出真实 exe 就是不能用。
        return Err(ProbeError::Reject(
            "不能直接启动 .ps1 包装脚本，也没有从里面拆出真实可执行文件（换 .cmd 或 exe 的路径）"
                .to_string(),
        ));
    }
    let version = probe_version(&launch).await?;
    let too_old = spec
        .minimum_version
        .map(|min| version_is_older(&version, min))
        .unwrap_or(false);
    Ok(AgentInstall {
        path: launch.display().to_string(),
        version,
        source,
        needs_shell: needs_shell(&launch),
        auto,
        too_old,
        skipped_older: None,
    })
}

/// 包装脚本 / 链接里的真实可执行文件；拆不出来就用原路径（`.ps1` 由调用方拒绝）。
///
/// 三种拆法（R-2 第 2 节补齐后两种）：
/// 1. shim 里直接引用 exe（opencode 的 npm shim：`"%~dp0\node_modules\...\opencode.exe"`）——
///    参考 Netcatty `electron/bridges/ai/shellUtils.cjs` 的 `resolveWindowsShimToNativeExe`；
/// 2. shim 转发给 `node` + 包里的 `.js`（codex 的 npm shim 就是这样，`.ps1` / `.cmd` /
///    裸 shim 三种都指到 `node_modules\@openai\codex\bin\codex.js`）：顺着包目录找同名的
///    `codex.exe`（AI-3 第 5 节）；
/// 3. scoop 的 `.shim`（`<名字>.exe` 旁边的文本文件，里面 `path = 真 exe`）。
///
/// 最后再把符号链接解析到目标（winget 的 `Links\` 是指向真实 exe 的 reparse point，
/// bun 之类的全局 bin 也可能是链接）。
pub fn resolve_launch_path(path: &Path) -> PathBuf {
    let wrapper = if is_wrapper(path) {
        resolve_shim_exe(path)
            .or_else(|| resolve_node_shim_exe(path))
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        resolve_scoop_shim(path).unwrap_or_else(|| path.to_path_buf())
    };
    resolve_link_target(&wrapper)
}

/// scoop 的 shim：`<名字>.exe` 旁边放一个 `<名字>.shim`，里面写着真实 exe 的路径。
///
/// `.shim` 的格式（scoop 生成的 INI 风格）：
///
/// ```text
/// path = C:\Program Files\Some App\some-app.exe
/// args = --flag
/// ```
fn resolve_scoop_shim(path: &Path) -> Option<PathBuf> {
    let is_exe = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return None;
    }
    let shim = path.with_extension("shim");
    if !shim.is_file() {
        return None;
    }
    let text = crate::ai::agent::text::read_text_lossy(&shim)?;
    let target = parse_shim_path(&text)?;
    let target = PathBuf::from(target);
    target.is_file().then_some(target)
}

/// 从 `.shim` 的内容里取 `path = ...`（大小写不敏感，取第一个非空值）。
pub fn parse_shim_path(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = crate::ai::agent::text::strip_bom(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("path") {
            continue;
        }
        let value = value.trim().trim_matches('"').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// 把符号链接 / reparse point 解析到真实文件；解析不动（或就是它自己）就返回原路径。
///
/// `canonicalize` 在 Windows 上会给出 `\\?\C:\...` 这种 verbatim 前缀，去掉它，
/// 否则界面和日志里会多出四个字符（`\\?\UNC\server\share` 要还原成 `\\server\share`）。
fn resolve_link_target(path: &Path) -> PathBuf {
    let Ok(target) = std::fs::canonicalize(path) else {
        return path.to_path_buf();
    };
    if target == path {
        return path.to_path_buf();
    }
    simplify_verbatim(target)
}

/// 去掉 Windows 的 verbatim 前缀。
fn simplify_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

fn resolve_shim_exe(path: &Path) -> Option<PathBuf> {
    let contents = crate::ai::agent::text::read_text_lossy(path)?;
    let dir = path.parent()?;
    for relative in extract_dp0_exe_refs(&contents) {
        // `%~dp0` 后面常跟 `\..\node_modules\...`：把 / 和 \ 都当分隔符拼。
        let normalized = relative.replace('/', "\\");
        let candidate = dir.join(normalized.trim_start_matches('\\'));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 包装脚本里指向真实 exe 的两处 `%~dp0` 变体。
///
/// - 老式 npm shim 直接写 `"%~dp0\node_modules\...\opencode.exe"`；
/// - 新式 shim 先 `SET dp0=%~dp0` 再用 `"%dp0%\node_modules\...`（这里跟着处理
///   `%dp0%`，两者都指向脚本所在目录）。
const DP0_NEEDLES: [&str; 2] = ["%~dp0", "%dp0%"];

/// 从包装脚本内容里抽出指向真实 exe 的引用（大小写不敏感）。
pub fn extract_dp0_exe_refs(contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    for needle in DP0_NEEDLES {
        extract_refs_for(contents, needle, &mut out);
    }
    out
}

fn extract_refs_for(contents: &str, needle: &str, out: &mut Vec<String>) {
    let bytes = contents.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        // 按字节比较（needle 是 ASCII）。不能用 `contents[i..].is_char_boundary(...)`
        // 那种写法：i 每次加 1，会落到多字节字符（脚本里的中文注释）中间，切 `str`
        // 直接 panic；字节全等的匹配天然落在字符边界上。
        if bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes) {
            // 从 needle 之后走到下一个引号，中间必须在同一行内。
            let rest = &contents[i + needle_bytes.len()..];
            let end = rest.find('"').unwrap_or(rest.len());
            let frag = &rest[..end];
            if !frag.contains('\n') && frag.to_ascii_lowercase().ends_with(".exe") {
                out.push(frag.to_string());
            }
            i += needle_bytes.len() + end;
        } else {
            i += 1;
        }
    }
}
pub fn needs_shell(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd") | Some("bat")
    )
}

/// 需要拆包装的脚本（`.cmd` / `.bat` 能经 `cmd /c` 跑，`.ps1` 不能直接启动）。
fn is_wrapper(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd") | Some("bat") | Some("ps1")
    )
}

/// 是不是 PowerShell 脚本（拆不出真实 exe 就不能用）。
fn is_powershell_shim(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ps1")
    )
}

/// shim 转发给 `node <脚本>` 时，从内容里抽出 `node_modules\...\x.js` 的引用。
///
/// 三种 shim 写法都指向同一个脚本：
/// - `.cmd`：`"%dp0%\node_modules\@openai\codex\bin\codex.js"`；
/// - `.ps1`：`"$basedir/node_modules/@openai/codex/bin/codex.js"`；
/// - 裸 shim（POSIX）：`... "$basedir/node_modules/@openai/codex/bin/codex.js"`。
///
/// 只按 `node_modules` 这个锚点截到下一个引号 / 空白，前后缀怎么写都不影响。
pub fn extract_node_script_ref(contents: &str) -> Option<String> {
    let lower = contents.to_ascii_lowercase();
    let at = lower.find("node_modules")?;
    let rest = &contents[at..];
    let end = rest
        .find(['"', '\'', '\n', '\r', ' '])
        .unwrap_or(rest.len());
    let frag = rest[..end].trim();
    if frag.to_ascii_lowercase().ends_with(".js") {
        Some(frag.to_string())
    } else {
        None
    }
}

/// 顺着 node 包装脚本找到包里的真实 exe：包根下按名字找 `<命令>.exe`。
fn resolve_node_shim_exe(path: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    let script = extract_node_script_ref(&contents)?;
    let script_path = dir.join(script.replace('/', "\\").trim_start_matches('\\'));
    if !script_path.is_file() {
        return None;
    }
    // `...\@openai\codex\bin\codex.js`：`bin` 的上一级就是包根。
    let package_root = script_path.parent()?.parent()?;
    let exe_name = format!("{}.exe", path.file_stem()?.to_str()?.to_ascii_lowercase());
    find_named_file(package_root, &exe_name, 8)
}

/// 在目录树里找同名文件（深度受限、按名字排序保证结果稳定）。
///
/// 读不动的子目录**跳过继续找**：winget 的包目录和用户自己的安装目录里什么都有
/// （没权限的、正在被别的程序占着的），一个读不动不能把整次查找废掉。
fn find_named_file(root: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = queue.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = read
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for entry in &entries {
            let file_name = entry
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if file_name == name && entry.is_file() {
                return Some(entry.clone());
            }
        }
        if depth >= max_depth {
            continue;
        }
        for entry in entries {
            if entry.is_dir() {
                queue.push((entry, depth + 1));
            }
        }
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("启动失败：{0}")]
    Spawn(String),
    #[error("版本探测超时")]
    Timeout,
    #[error("退出码异常：{0}")]
    Exit(String),
    #[error("拿不到版本号")]
    NoVersion,
    /// 路径存在但不能用（例如拆不出真实 exe 的 `.ps1`）。
    #[error("{0}")]
    Reject(String),
}

/// 跑一次 `--version`，返回版本号文本。
pub async fn probe_version(path: &Path) -> Result<String, ProbeError> {
    let mut cmd = build_command(path);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    hide_console(&mut cmd);

    let child = cmd.spawn().map_err(|e| ProbeError::Spawn(e.to_string()))?;
    let output = match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(ProbeError::Spawn(err.to_string())),
        Err(_) => return Err(ProbeError::Timeout),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let detail = first_meaningful_line(&stderr)
            .or_else(|| first_meaningful_line(&stdout))
            .unwrap_or_else(|| format!("退出码 {}", output.status.code().unwrap_or(-1)));
        return Err(ProbeError::Exit(detail));
    }

    first_meaningful_line(&stdout)
        .or_else(|| first_meaningful_line(&stderr))
        .ok_or(ProbeError::NoVersion)
}

/// 按路径组装命令。`.cmd` / `.bat`（没能拆出真实 exe 的包装脚本）也直接交给标准库启动。
///
/// 为什么不再自己拼 `cmd /c <脚本> 参数…`（K05）：那样程序名是 `cmd.exe`，标准库按
/// 普通 exe 的规则给参数加引号，而 cmd.exe 不认这套规则，参数里的 `&` `|` `%VAR%`
/// 会被 cmd 当成命令语法解释。标准库发现程序本身是批处理脚本时会改用 cmd 专用的转义
/// （Rust 1.77 起，CVE-2024-24576 的修复），转义不了的参数直接拒绝启动，而不是冒险。
pub fn build_command(path: &Path) -> tokio::process::Command {
    tokio::process::Command::new(path)
}

/// 不给子进程弹控制台窗口。
pub fn hide_console(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// 版本号是否低于最低要求。
///
/// 版本文本可能带前缀噪声（`opencode 1.19.0`），取里面第一个 `x.y.z` 比。
pub fn version_is_older(version: &str, minimum: (u64, u64, u64)) -> bool {
    match parse_version(version) {
        Some(found) => found < minimum,
        // 解析不出当不旧：宁可少提示，别把能用的版本拦下来。
        None => false,
    }
}

/// 从文本里抽出第一个 `x.y.z`。
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let token = &text[start..i];
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() >= 2 {
                let major = parts[0].parse().ok()?;
                let minor = parts[1].parse().ok()?;
                let patch = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
                return Some((major, minor, patch));
            }
        } else {
            i += 1;
        }
    }
    None
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个假安装布局用的根目录。
    ///
    /// 路径里**故意带空格和中文**（R-2 第 2 节：别人的用户名可能就是这样，
    /// `C:\Users\张 三\...`），所有布局用例都建在它下面。
    fn layout_root(name: &str) -> crate::test_util::TestDir {
        crate::test_util::TestDir::new(&format!("zssh 发现测试 {name}"))
    }

    /// 比较规范化之后的路径：程序解析出来的是长路径，测试里的目录可能是 8.3 短路径。
    fn assert_same_path(actual: &Path, expected: &Path) {
        assert!(
            crate::pathnorm::same_path(actual, expected),
            "路径不是同一个位置：
  实际 {}
  期望 {}",
            actual.display(),
            expected.display()
        );
    }

    /// 这个目录的 8.3 短路径写法（GitHub 的 Windows 机器上 `%TEMP%` 就是这样）。
    /// 卷上没有 8.3 短名字时退回原路径并打印原因。
    fn short_form(dir: &Path) -> PathBuf {
        #[cfg(windows)]
        if let Some(short) = crate::pathnorm::short_path(dir) {
            return short;
        }
        eprintln!("注意：这个卷上没有 8.3 短名字，用原路径代替短路径写法");
        dir.to_path_buf()
    }

    /// npm 风格的全局安装：`<dir>\<命令>.cmd` 里 `%~dp0` 指向包里的真 exe。
    fn npm_layout(dir: &Path, command: &str) -> PathBuf {
        let bin = dir
            .join("node_modules")
            .join(format!("{command}-package"))
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(format!("{command}.exe"));
        fs::write(&exe, b"fake exe").unwrap();
        fs::write(
            dir.join(format!("{command}.cmd")),
            format!(
                "@ECHO off\r\n\"%~dp0\\node_modules\\{command}-package\\bin\\{command}.exe\" %*\r\n"
            ),
        )
        .unwrap();
        exe
    }

    /// 一个直接放的 exe（scoop / chocolatey / 官方安装包那种布局）。
    fn plain_exe(dir: &Path, command: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let exe = dir.join(format!("{command}.exe"));
        fs::write(&exe, b"fake exe").unwrap();
        exe
    }

    /// 自动发现的候选路径（字符串形式，方便断言）。
    fn candidates(spec: &AgentSpec, data_dir: &Path, env: &EnvView) -> Vec<String> {
        auto_candidates(spec, data_dir, env)
            .iter()
            .map(|candidate| candidate.path.display().to_string())
            .collect()
    }

    /// 把测试里的 `(&str, String)` 变量表转成 [`EnvView::fake`] 要的形状。
    fn env_pairs<'a>(vars: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
        vars.iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect()
    }

    #[test]
    fn tools_dir_layout() {
        let dir = tools_dir(Path::new("C:\\data"), &OPENCODE);
        assert_eq!(dir, PathBuf::from("C:\\data\\tools\\opencode"));
    }

    #[test]
    fn shell_wrappers_are_detected() {
        assert!(needs_shell(Path::new("C:\\npm\\opencode.cmd")));
        assert!(needs_shell(Path::new("C:\\npm\\opencode.BAT")));
        assert!(!needs_shell(Path::new("C:\\bin\\opencode.exe")));
    }

    /// 包管理器：npm / pnpm / yarn / bun 的全局目录都要能找到。
    ///
    /// npm 的真实全局目录**问命令**（`npm config get prefix`）拿到，所以这里既
    /// 验证"问到的目录"进候选，也验证各家默认目录兜底。`%APPDATA%\npm` 那种是
    /// 常见情况，pnpm / yarn / bun 的默认位置各家不同。
    #[test]
    fn package_manager_global_dirs_are_all_candidates() {
        let root = layout_root("managers");
        let spec = &OPENCODE;
        let appdata = root.join("AppData\\Roaming");
        let local = root.join("AppData\\Local");
        let home = root.join("Users\\张 三");

        // ① 问命令要来的全局目录（npm 改过 prefix 的情况）：npm 是 shim 布局。
        let queried = root.join("D 盘\\自定义 npm 前缀");
        npm_layout(&queried, "opencode");
        // ② npm 的默认位置。
        npm_layout(&appdata.join("npm"), "opencode");
        // ③ pnpm（含受管理的 bin）、yarn、bun、`PNPM_HOME`：直接放 exe。
        let direct_dirs = [
            local.join("pnpm"),
            local.join("pnpm\\bin"),
            local.join("Yarn\\bin"),
            home.join(".bun\\bin"),
            root.join("pnpm 主目录"),
        ];
        let mut expected = vec![
            queried.join("opencode.cmd"),
            appdata.join("npm").join("opencode.cmd"),
        ];
        for dir in &direct_dirs {
            expected.push(plain_exe(dir, "opencode"));
        }

        let vars = [
            ("APPDATA", appdata.display().to_string()),
            ("LOCALAPPDATA", local.display().to_string()),
            ("USERPROFILE", home.display().to_string()),
            ("PNPM_HOME", root.join("pnpm 主目录").display().to_string()),
        ];
        let queried_text = queried.display().to_string();
        let env = EnvView::fake(&env_pairs(&vars), &[queried_text.as_str()]);

        let found = candidates(spec, &root.join("数据 目录"), &env);
        for path in &expected {
            let text = path.display().to_string();
            assert!(found.contains(&text), "候选里没有 {text}");
        }
    }

    /// Node 版本管理器：nvm-windows（`NVM_SYMLINK`）/ fnm（别名目录）/ volta。
    #[test]
    fn node_version_manager_dirs_are_candidates() {
        let root = layout_root("node-managers");
        let spec = &CLAUDE;
        let appdata = root.join("AppData\\Roaming");

        let nvm = root.join("nvm 软链接");
        let fnm = appdata.join("fnm\\aliases\\default");
        npm_layout(&nvm, "claude");
        npm_layout(&fnm, "claude");
        let volta = root.join("volta 家目录\\bin");
        let volta_exe = plain_exe(&volta, "claude");

        let vars = [
            ("NVM_SYMLINK", nvm.display().to_string()),
            ("APPDATA", appdata.display().to_string()),
            (
                "VOLTA_HOME",
                root.join("volta 家目录").display().to_string(),
            ),
            (
                "USERPROFILE",
                root.join("Users\\张 三").display().to_string(),
            ),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let found = candidates(spec, &root.join("数据 目录"), &env);
        for path in [nvm.join("claude.cmd"), fnm.join("claude.cmd"), volta_exe] {
            let text = path.display().to_string();
            assert!(found.contains(&text), "候选里没有 {text}");
        }
    }

    /// winget（Links + Packages）/ scoop（SCOOP 指到别处 + `.shim`）/ chocolatey。
    #[test]
    fn winget_scoop_and_chocolatey_layouts_are_candidates() {
        let guard = layout_root("windows-managers-a-rather-long-name");
        let root = short_form(&guard);
        let spec = &CODEX;
        let local = root.join("AppData\\Local");

        let links = local.join("Microsoft\\WinGet\\Links");
        let link_exe = plain_exe(&links, "codex");
        let package_dir =
            local.join("Microsoft\\WinGet\\Packages\\OpenAI.Codex_1.0.0_x64__abc123\\bin");
        let package_exe = plain_exe(&package_dir, "codex");

        let scoop_shims = root.join("SCOOP 自定义\\shims");
        let real_dir = root.join("SCOOP 自定义\\apps\\codex\\current\\bin");
        let real_exe = plain_exe(&real_dir, "codex");
        fs::create_dir_all(&scoop_shims).unwrap();
        fs::write(
            scoop_shims.join("codex.shim"),
            format!("path = {}\r\nargs = --flag\r\n", real_exe.display()),
        )
        .unwrap();
        fs::write(scoop_shims.join("codex.exe"), b"shim launcher").unwrap();

        let choco = root.join("ProgramData\\chocolatey\\bin");
        let choco_exe = plain_exe(&choco, "codex");

        let vars = [
            ("LOCALAPPDATA", local.display().to_string()),
            ("SCOOP", root.join("SCOOP 自定义").display().to_string()),
            (
                "ChocolateyInstall",
                root.join("ProgramData\\chocolatey").display().to_string(),
            ),
            (
                "USERPROFILE",
                root.join("Users\\张 三").display().to_string(),
            ),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let found = candidates(spec, &root.join("数据 目录"), &env);
        for exe in [&link_exe, &package_exe, &choco_exe] {
            let text = exe.display().to_string();
            assert!(found.contains(&text), "候选里没有 {text}");
        }
        // scoop：先看到的是 shim 启动器，解析后要落到真实 exe。
        assert!(found
            .iter()
            .any(|path| path == &scoop_shims.join("codex.exe").display().to_string()));
        assert_same_path(
            &resolve_launch_path(&scoop_shims.join("codex.exe")),
            &real_exe,
        );
    }

    /// 官方安装脚本 / 独立安装包：opencode / Claude Code / codex 三家各自的位置。
    #[test]
    fn official_installer_dirs_are_candidates() {
        let root = layout_root("official");
        let local = root.join("AppData\\Local");
        let home = root.join("Users\\张 三");

        let opencode_exe = plain_exe(&home.join(".opencode\\bin"), "opencode");
        let claude_exe = plain_exe(&home.join(".local\\bin"), "claude");
        let codex_default = plain_exe(&local.join("Programs\\OpenAI\\Codex\\bin"), "codex");
        let codex_custom_dir = root.join("另装 的 Codex 目录");
        let codex_custom = plain_exe(&codex_custom_dir, "codex");

        let vars = [
            ("USERPROFILE", home.display().to_string()),
            ("LOCALAPPDATA", local.display().to_string()),
            ("CODEX_INSTALL_DIR", codex_custom_dir.display().to_string()),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let data_dir = root.join("数据 目录");

        for (spec, exe) in [
            (&OPENCODE, &opencode_exe),
            (&CLAUDE, &claude_exe),
            (&CODEX, &codex_default),
            (&CODEX, &codex_custom),
        ] {
            let text = exe.display().to_string();
            let found = candidates(spec, &data_dir, &env);
            assert!(found.contains(&text), "候选里没有 {text}");
        }
    }

    /// K05：`.cmd` 包装脚本的参数里有 cmd 的元字符时，原样传给脚本，不能被当成命令执行。
    #[cfg(windows)]
    #[tokio::test]
    async fn batch_wrapper_arguments_are_not_interpreted_by_cmd() {
        let root = layout_root("batch-args");
        let dir = root.join("带 空格 的目录");
        fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("agent.cmd");
        // 和 npm 的包装脚本一样用 %* 原样转交参数（保留引号）；被注入的话会多出一行
        // INJECTED。脚本自己怎么用参数（比如 %~1 去掉引号再拼命令）不归启动方管。
        fs::write(&shim, "@ECHO off\r\necho ARG=[%*]\r\n").unwrap();
        // 引号先把 MSVC 式转义（\"）的引号区间提前闭合，后面的 & 就落到引号外面了
        let payload = "x\" & echo INJECTED & rem \"";
        let mut cmd = build_command(&shim);
        cmd.arg(payload)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // 标准库对转义不了的参数会拒绝启动：那也算安全
        if let Ok(child) = cmd.spawn() {
            let output = child.wait_with_output().await.unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                !stdout.lines().any(|line| line.trim() == "INJECTED"),
                "参数里的 & 被 cmd 当成了命令分隔符：{stdout}"
            );
            assert!(stdout.contains("ARG=["), "脚本应该照常运行：{stdout}");
        }
    }

    /// 探测：`.cmd` 包装脚本（真实可运行）→ 版本和"版本过低"的判定。
    #[tokio::test]
    async fn probe_reads_version_from_a_fake_install() {
        let root = layout_root("probe");
        let dir = root.join("npm 全局 目录");
        fs::create_dir_all(&dir).unwrap();

        let shim = dir.join("opencode.cmd");
        fs::write(&shim, "@ECHO off\r\necho opencode 1.19.1\r\n").unwrap();
        let install = probe(&OPENCODE, &shim, DiscoverySource::Path, true)
            .await
            .expect("要探到版本");
        assert_eq!(install.version, "opencode 1.19.1");
        assert!(!install.too_old);
        assert!(install.needs_shell, ".cmd 要经 cmd.exe 启动");
        assert!(install.auto);

        // 老版本：标版本过低（不拦，只提示）。
        fs::write(&shim, "@ECHO off\r\necho 1.17.3\r\n").unwrap();
        let install = probe(&OPENCODE, &shim, DiscoverySource::Path, true)
            .await
            .expect("要探到版本");
        assert!(install.too_old, "1.17.3 低于 1.19.0");

        // 报错退出：探不通（发现流程里跳过它继续找下一个候选）。
        fs::write(&shim, "@ECHO off\r\nexit /b 3\r\n").unwrap();
        assert!(probe(&OPENCODE, &shim, DiscoverySource::Path, true)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn probe_missing_path_reports_not_found() {
        let err = probe_path(&OPENCODE, Path::new("C:\\不存在的目录\\opencode.exe")).await;
        assert!(err.unwrap_err().contains("不存在"));
    }

    #[tokio::test]
    async fn manual_mode_with_bad_path_errors_and_does_not_fall_back() {
        let tmp = crate::test_util::TestDir::new("discovery-test-bad");
        let err = discover(&OPENCODE, Some("C:\\不存在的\\opencode.exe"), &tmp)
            .await
            .expect_err("手动指定坏了要报出来");
        assert!(err.contains("手动指定"));
    }

    #[tokio::test]
    async fn auto_mode_with_blank_setting_does_not_error() {
        // 设置里是空白：跳过设置、走自动。找到找不到都行（本机 PATH 上可能有 opencode）。
        let tmp = crate::test_util::TestDir::new("discovery-test-none");
        let found = discover(&OPENCODE, Some("   "), &tmp)
            .await
            .expect("空白设置不算错误");
        drop(found);
    }

    /// 造一个能跑 `--version` 的假安装：`.cmd` 里 echo 一个版本号。
    fn versioned_cmd(dir: &Path, command: &str, version: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{command}.cmd"));
        fs::write(&path, format!("@ECHO off\r\necho {version}\r\n")).unwrap();
        path
    }

    /// R-2 遗留 3 / M3-1 第 0 节：PATH 上的旧版本**不能拦住**后面满足最低版本的安装。
    #[tokio::test]
    async fn too_old_on_path_does_not_block_a_newer_install() {
        let root = layout_root("old-path");
        let old_dir = root.join("PATH 上的 旧版本");
        versioned_cmd(&old_dir, "opencode", "1.17.3"); // 低于 1.19.0
                                                       // 后面那个位置上的可用版本：pnpm 的默认全局目录里就是直接放 .cmd 的布局。
        let new_dir = root.join("AppData\\Local\\pnpm");
        versioned_cmd(&new_dir, "opencode", "1.19.1");

        let vars = [
            ("PATH", old_dir.display().to_string()),
            (
                "LOCALAPPDATA",
                root.join("AppData\\Local").display().to_string(),
            ),
            (
                "USERPROFILE",
                root.join("Users\\张 三").display().to_string(),
            ),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let data_dir = root.join("数据 目录");

        let install = discover_with_env(&OPENCODE, None, &data_dir, &env)
            .await
            .expect("发现不该报错")
            .expect("要找到可用的那个");
        assert!(!install.too_old, "1.19.1 满足最低版本");
        assert!(
            install.version.contains("1.19.1"),
            "用的应该是新版本：{}",
            install.version
        );
        // PATH 上那个旧版本要记下来（设置页据此提示）。
        let skipped = install.skipped_older.expect("要记下被跳过的旧版本");
        assert!(skipped.version.contains("1.17.3"), "{}", skipped.version);
        assert_eq!(skipped.source, DiscoverySource::Path);
    }

    /// R-6b：PATH 里写的是数据目录 tools 的 8.3 短路径，也要认出它在 tools 下（界面显示"已下载"）。
    #[tokio::test]
    async fn a_path_entry_inside_the_tools_dir_is_reported_as_downloaded_even_in_short_form() {
        let guard = layout_root("tools-short-a-rather-long-name");
        let data_dir = crate::pathnorm::normalize(&guard);
        let tools = tools_dir(&data_dir, &OPENCODE);
        versioned_cmd(&tools, "opencode", "1.19.1");
        let short_tools = short_form(&tools);

        let vars = [("PATH", short_tools.display().to_string())];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        // 候选里 tools 自己那份排在最前，所以让 tools 里没有 .cmd 以外的位置也行：
        // 直接用 `probe` 之前的分类函数验证。
        assert_eq!(
            classify_source(&tools, &data_dir, &OPENCODE, DiscoverySource::Path),
            DiscoverySource::ToolsDir
        );
        assert_eq!(
            classify_source(
                &short_tools.join("opencode.cmd"),
                &data_dir,
                &OPENCODE,
                DiscoverySource::Path
            ),
            DiscoverySource::ToolsDir
        );
        assert_eq!(
            classify_source(
                &guard.join("elsewhere").join("opencode.cmd"),
                &data_dir,
                &OPENCODE,
                DiscoverySource::Path
            ),
            DiscoverySource::Path
        );
        drop(env);
    }

    /// 所有候选都太旧时才标"版本过低"（返回第一个旧版本，没有 skipped_older）。
    #[tokio::test]
    async fn all_candidates_too_old_falls_back_to_the_old_one() {
        let root = layout_root("all-old");
        let old_dir = root.join("PATH 上的 旧版本");
        versioned_cmd(&old_dir, "opencode", "1.17.3");
        let also_old = root.join("AppData\\Local\\pnpm");
        versioned_cmd(&also_old, "opencode", "1.18.0");

        let vars = [
            ("PATH", old_dir.display().to_string()),
            (
                "LOCALAPPDATA",
                root.join("AppData\\Local").display().to_string(),
            ),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let install = discover_with_env(&OPENCODE, None, &root.join("数据 目录"), &env)
            .await
            .expect("发现不该报错")
            .expect("都太旧时要把第一个返回去，给界面标「版本过低」");
        assert!(install.too_old);
        assert!(install.version.contains("1.17.3"));
        assert!(install.skipped_older.is_none(), "没有可用的就别提旧版本");
    }

    /// 候选都没有：返回 None（不是错误）。
    #[tokio::test]
    async fn nothing_found_is_none() {
        let root = layout_root("none");
        let vars = [
            ("PATH", root.join("空的 PATH").display().to_string()),
            (
                "LOCALAPPDATA",
                root.join("AppData\\Local").display().to_string(),
            ),
        ];
        let env = EnvView::fake(&env_pairs(&vars), &[]);
        let found = discover_with_env(&CODEX, None, &root.join("数据 目录"), &env)
            .await
            .expect("发现不该报错");
        assert!(found.is_none());
    }

    /// 拆 `.cmd`：写一个假 shim 指到假 exe，确认解析到 exe。
    #[test]
    fn resolves_shim_to_exe_on_disk() {
        let guard = crate::test_util::TestDir::new("discovery-shim-a-rather-long-name");
        let tmp = short_form(&guard);
        let bin = tmp.join("node_modules").join("opencode-ai").join("bin");
        fs::create_dir_all(&bin).unwrap();
        let real_exe = bin.join("opencode.exe");
        fs::write(&real_exe, b"x").unwrap();
        let shim = tmp.join("opencode.cmd");
        fs::write(
            &shim,
            "@ECHO off\r\n\"%~dp0\\node_modules\\opencode-ai\\bin\\opencode.exe\" %*\r\n",
        )
        .unwrap();

        let resolved = resolve_launch_path(&shim);
        assert_same_path(&resolved, &real_exe);
    }

    /// codex 的 npm 布局（AI-3 第 5 节）：shim 转发 node + 包里的 .js，
    /// 要顺着包目录找到 `codex.exe`。
    #[test]
    fn resolves_codex_npm_shims_to_the_vendored_exe() {
        let guard = crate::test_util::TestDir::new("discovery-codex-shim-a-rather-long-name");
        let tmp = short_form(&guard);
        let real_exe = tmp
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join("codex-win32-x64")
            .join("vendor")
            .join("x86_64-pc-windows-msvc")
            .join("bin")
            .join("codex.exe");
        fs::create_dir_all(real_exe.parent().unwrap()).unwrap();
        fs::write(&real_exe, b"x").unwrap();
        let bin = tmp
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("codex.js"), b"// wrapper").unwrap();

        // .cmd 写法（%dp0%）。
        let cmd = tmp.join("codex.cmd");
        fs::write(
            &cmd,
            "@ECHO off\r\nGOTO start\r\n:start\r\n\"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
        )
        .unwrap();
        assert_same_path(&resolve_launch_path(&cmd), &real_exe);

        // .ps1 写法（$basedir，正斜杠）。
        let ps1 = tmp.join("codex.ps1");
        fs::write(
            &ps1,
            "#!/usr/bin/env pwsh\n$basedir=Split-Path $MyInvocation.MyCommand.Definition -Parent\n& \"$basedir/node_modules/@openai/codex/bin/codex.js\" $args\n",
        )
        .unwrap();
        assert_same_path(&resolve_launch_path(&ps1), &real_exe);

        // 拆不出真实 exe 的 .ps1 不能当可执行文件用。
        let broken = tmp.join("broken.ps1");
        fs::write(&broken, "Write-Host 'hello'\n").unwrap();
        assert!(is_powershell_shim(&resolve_launch_path(&broken)));
    }

    #[test]
    fn extracts_node_script_refs_from_all_shim_styles() {
        let cmd = "\"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\"   %*";
        assert_eq!(
            extract_node_script_ref(cmd).as_deref(),
            Some("node_modules\\@openai\\codex\\bin\\codex.js")
        );
        let ps1 = "& \"$basedir/node_modules/@openai/codex/bin/codex.js\" $args";
        assert_eq!(
            extract_node_script_ref(ps1).as_deref(),
            Some("node_modules/@openai/codex/bin/codex.js")
        );
        assert!(extract_node_script_ref("no reference here").is_none());
    }
}
