//! 集成测试共用的东西：测试服务器信息、临时 known_hosts。
//!
//! 每个测试二进制都会编译这个模块，用不到的辅助函数在这里统一放宽 dead_code。

#![allow(dead_code)]

use std::collections::VecDeque;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use zssh_core::ssh::{
    AuthConfig, BoxFuture, ConnectParams, Connection, HostKeyDecision, HostKeyInfo, Interactor,
    KnownHosts, NonInteractive, PromptAnswers, PromptKind, PromptRequest,
};

/// 连测试服务器要用的东西。字段只在建 `ConnectParams` 时用，不打印。
pub struct TestServer {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl TestServer {
    /// 返回的错误信息只提变量名，不带里面的值。
    pub fn from_env() -> Result<Self, String> {
        let host = require("ZSSH_TEST_HOST")?;
        install_scrubbing_panic_hook(&host);
        Ok(Self {
            host,
            port: match std::env::var("ZSSH_TEST_PORT") {
                Ok(raw) if !raw.trim().is_empty() => raw
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "ZSSH_TEST_PORT 不是合法端口".to_owned())?,
                _ => 22,
            },
            username: require("ZSSH_TEST_USER")?,
            password: require("ZSSH_TEST_PASSWORD")?,
        })
    }

    /// 密码认证 + 指定 known_hosts 的连接参数。
    pub fn params(&self, known_hosts: &TempKnownHosts) -> ConnectParams {
        ConnectParams {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth: AuthConfig::password(Some(self.password.clone())),
            known_hosts: known_hosts.known_hosts(),
            legacy_algorithms: false,
        }
    }

    /// 用默认的无人值守交互（主机密钥"仅本次"）连一次。
    pub async fn connect(&self, known_hosts: &TempKnownHosts) -> Result<Connection, String> {
        Connection::connect(self.params(known_hosts), Arc::new(NonInteractive))
            .await
            .map_err(|err| self.sanitize(&err.to_string()))
    }

    /// 用指定的交互实现连一次。
    pub async fn connect_with(
        &self,
        known_hosts: &TempKnownHosts,
        interactor: Arc<dyn Interactor>,
    ) -> Result<Connection, String> {
        Connection::connect(self.params(known_hosts), interactor)
            .await
            .map_err(|err| self.sanitize(&err.to_string()))
    }

    /// 用手写的连接参数连一次（走转发器、复用凭据等），错误同样先过 `sanitize`。
    pub async fn connect_params(
        &self,
        params: ConnectParams,
        interactor: Arc<dyn Interactor>,
    ) -> Result<Connection, String> {
        Connection::connect(params, interactor)
            .await
            .map_err(|err| self.sanitize(&err.to_string()))
    }

    /// 联网测试里 `.expect(..)` 的替代：失败时 panic 的内容先过 `sanitize`。
    /// 连接错误的文本里带服务器地址，直接 `.expect` 会把它原样打进测试输出。
    pub fn expect_ok<T, E: std::fmt::Debug>(&self, result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(err) => panic!("{context}：{}", self.sanitize(&format!("{err:?}"))),
        }
    }

    /// 把文本里的测试服务器地址换成"测试服务器"：测试输出不能带真实地址。
    ///
    /// 报错、断言消息、shell 输出（有些服务器的提示符里带主机名）都可能包含它，
    /// 凡是往 stdout 或 panic 里放的东西都要先过这里。
    pub fn sanitize(&self, text: &str) -> String {
        text.replace(&self.host, "测试服务器")
    }

    /// 指纹回调之类的地方拿到的主机名：是测试服务器就换成标签。
    pub fn host_label(&self, host: &str) -> String {
        if host == self.host {
            "测试服务器".to_owned()
        } else {
            host.to_owned()
        }
    }
}

static SCRUB_HOSTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 把已登记的服务器地址换成"测试服务器"。
pub fn scrub(text: &str) -> String {
    let hosts = SCRUB_HOSTS.lock().map(|g| g.clone()).unwrap_or_default();
    hosts.iter().fold(text.to_owned(), |acc, host| {
        acc.replace(host.as_str(), "测试服务器")
    })
}

/// panic 信息的统一出口：`.expect(..)`、`assert!` 带出来的连接错误里有服务器地址，
/// 在这里兜底换掉，不用每个测试各自记得调 `sanitize`。
pub fn format_panic(message: &str, location: &str) -> String {
    scrub(&format!(
        "thread panicked at {location}:
{message}"
    ))
}

/// 登记服务器地址并换掉 panic 钩子（只换一次）。`TestServer::from_env` 会调它，
/// 所以任何拿到服务器信息的联网测试，panic 输出都是干净的。
pub fn install_scrubbing_panic_hook(host: &str) {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    if let Ok(mut hosts) = SCRUB_HOSTS.lock() {
        if !host.is_empty() && !hosts.iter().any(|h| h == host) {
            hosts.push(host.to_owned());
        }
    }
    INSTALLED.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let payload = info.payload();
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "（非文本 panic）".to_owned());
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_default();
            eprintln!("{}", format_panic(&message, &location));
        }));
    });
}

/// 门禁里的联网测试只留冒烟组（连接、开 shell、SFTP 列目录、传小文件）；
/// 其余联网测试要设 `ZSSH_LIVE_FULL=1`，由 `scripts/live-tests.ps1` 跑。
pub fn full_live(name: &str) -> bool {
    let on = matches!(std::env::var("ZSSH_LIVE_FULL").as_deref(), Ok("1"));
    if !on {
        println!("跳过 {name}：完整联网测试，用 scripts/live-tests.ps1 跑（ZSSH_LIVE_FULL=1）");
    }
    on
}

/// 探测一个 TCP 端口能不能连上（SOCKS5 测试用来区分"代理没开"和"代理坏了"）。
pub async fn probe_tcp(host: &str, port: u16, timeout: std::time::Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

/// 在一个 shell 上跑 `echo ZSSH_<marker>_$((40+2))`，读到标记或者超时为止。
///
/// 回显里只有字面的 `$((40+2))`，所以只有命令真的执行了才会看到 `_42_`。
/// 读完标记就停，**不关 shell**（复用连接的测试要继续用同一个 shell）。
/// 等终端安静下来：把已经在路上的输出（比如上一条命令的提示符，可能比
/// 标记晚到一小会儿）读掉；连续 `quiet` 时间没有新数据就返回。
///
/// 只用在“断言探测没有往终端里写东西”**之前**：先把属于上一条命令的提示符收干净，
/// 后面的检查才只可能看到探测写进来的东西。机器忙时提示符会晚到，固定等一小会儿
/// 会偶发误判（load 高时 600 毫秒不够）。
pub async fn settle_terminal(
    shell: &mut zssh_core::ssh::ShellSession,
    quiet: std::time::Duration,
    max: std::time::Duration,
) {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(quiet.min(remaining), shell.next_event()).await {
            Ok(Some(zssh_core::ssh::ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
            }
            Ok(Some(_)) | Ok(None) => return,
            // 连续 quiet 时间没数据：安静了。
            Err(_) => return,
        }
    }
}

pub async fn run_marker(
    shell: &mut zssh_core::ssh::ShellSession,
    marker: &str,
    limit: std::time::Duration,
) -> String {
    let command = format!("echo ZSSH_{marker}_$((40+2))\r");
    if shell.write(command.as_bytes()).await.is_err() {
        return String::new();
    }
    let expect = format!("ZSSH_{marker}_42");

    let mut output = Vec::new();
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, shell.next_event()).await {
            Ok(Some(zssh_core::ssh::ShellEvent::Data(bytes))) => {
                shell.ack(bytes.len() as u32);
                output.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&output).contains(&expect) {
                    break;
                }
            }
            Ok(Some(_)) | Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// 临时目录里的一份 known_hosts，测试结束自己删。
pub struct TempKnownHosts {
    path: PathBuf,
    /// 放在最后：目录守卫最后 drop。
    _dir: TestDir,
}

impl TempKnownHosts {
    pub fn new(tag: &str) -> Self {
        let dir = TestDir::new(&format!("known-hosts-{tag}"));
        let path = dir.join("known_hosts");
        Self { path, _dir: dir }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn known_hosts(&self) -> KnownHosts {
        KnownHosts::new(&self.path)
    }

    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    pub fn write(&self, content: &str) {
        std::fs::write(&self.path, content).expect("写临时 known_hosts");
    }
}

pub fn require(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("缺少环境变量 {name}")),
    }
}

/// 可选的私钥路径 / 口令（`ZSSH_TEST_KEY_PATH` 等），没设置就跳过相关测试。
pub fn optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// 预设回答的模拟交互（认证和指纹测试用）。
///
/// 记录问了什么、问了几次；指纹问题返回固定决定，提问按队列里的回答返回，
/// 队列空或者轮到 `None` 就当用户取消。
pub struct MockInteractor {
    host_key_answer: HostKeyDecision,
    /// 被问了几次主机指纹
    pub host_key_asks: AtomicUsize,
    /// 每次提问按顺序取一个回答
    answers: Mutex<VecDeque<Option<String>>>,
    /// 收到的问题
    prompts: Mutex<Vec<PromptRequest>>,
    /// 记录看到的指纹信息
    seen_host_keys: Mutex<Vec<HostKeyInfo>>,
}

impl MockInteractor {
    pub fn new(host_key_answer: HostKeyDecision, answers: Vec<Option<&str>>) -> Self {
        Self {
            host_key_answer,
            host_key_asks: AtomicUsize::new(0),
            answers: Mutex::new(
                answers
                    .into_iter()
                    .map(|answer| answer.map(str::to_owned))
                    .collect(),
            ),
            prompts: Mutex::new(Vec::new()),
            seen_host_keys: Mutex::new(Vec::new()),
        }
    }

    pub fn asks(&self) -> usize {
        self.host_key_asks.load(Ordering::Relaxed)
    }

    /// 收到过哪些种类的问题
    pub fn asked_kinds(&self) -> Vec<PromptKind> {
        self.prompts
            .lock()
            .expect("锁")
            .iter()
            .map(|request| request.kind)
            .collect()
    }

    pub fn host_key_infos(&self) -> Vec<HostKeyInfo> {
        self.seen_host_keys.lock().expect("锁").clone()
    }
}

impl Interactor for MockInteractor {
    fn confirm_host_key(&self, info: HostKeyInfo) -> BoxFuture<'_, HostKeyDecision> {
        Box::pin(async move {
            self.host_key_asks.fetch_add(1, Ordering::Relaxed);
            self.seen_host_keys.lock().expect("锁").push(info);
            self.host_key_answer
        })
    }

    fn prompt(&self, request: PromptRequest) -> BoxFuture<'_, Option<PromptAnswers>> {
        Box::pin(async move {
            self.prompts.lock().expect("锁").push(request);
            match self.answers.lock().expect("锁").pop_front() {
                Some(Some(answer)) => Some(PromptAnswers {
                    answers: vec![answer],
                    remember: true,
                }),
                Some(None) | None => None,
            }
        })
    }
}

/// 让同一个测试二进制里的联网测试排队跑。
///
/// 它们都要对着同一台测试服务器灌流量，并行跑会互相抢带宽，让"暂停""恢复"这些
/// 带时限的断言变得不稳定。
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// 测试**准备阶段**开 SFTP 通道：偶发"SFTP 请求超时"（链路或服务器起 sftp-server 慢，
/// 默认等 10 秒）会让门禁无故变红，所以失败时等一下重试一次。只用在准备阶段，
/// 断言本身不重试；要断言"开通道的行为"的用例（通道数 +1）直接调 `SftpChannel::open`。
pub async fn open_sftp(
    connection: &Arc<Connection>,
) -> Result<zssh_core::sftp::SftpChannel, zssh_core::sftp::SftpError> {
    match zssh_core::sftp::SftpChannel::open(connection).await {
        Ok(channel) => Ok(channel),
        Err(first) => {
            println!("准备阶段开 SFTP 通道失败（{first}），2 秒后重试一次");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            zssh_core::sftp::SftpChannel::open(connection).await
        }
    }
}

// ---------- 远端 / 本地临时目录的守卫（M3-2 第 0 节） ----------
//
// M3-1 验收发现：测试中途失败（断言不过、panic）时，`teardown()` 根本走不到，
// 服务器 /tmp 里留了一堆 zssh-sftp-live-* 目录。所以清理不能只写在"成功路径"上：
//
// 1. 守卫对象：`Drop` 里兜底删（另起线程 + 自己的 runtime + 现连一条新连接——
//    测试自己的连接这时可能已经死了，比如掐线测试）；
// 2. 测试开始时顺手清掉同前缀、超过 1 小时的旧目录（上一次运行崩掉留下的）。
//
// 两条都做：守卫管"本次一定删"，清扫管"上次没删掉的"。

/// 远端临时目录的前缀（只碰这个前缀，别的不动）。
pub const REMOTE_TEMP_PREFIX: &str = "zssh-sftp-live-";
/// 旧目录多大算"上次留下的"。
pub const STALE_DIR_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// 服务器 /tmp 里同前缀的目录清单（只列名字和修改时间，不删）。
pub async fn list_remote_temp_dirs(
    channel: &zssh_core::sftp::SftpChannel,
) -> Result<Vec<(String, Option<u32>)>, zssh_core::sftp::SftpError> {
    let entries = channel.read_dir("/tmp").await?;
    Ok(entries
        .into_iter()
        .filter(|entry| entry.name.starts_with(REMOTE_TEMP_PREFIX) && entry.kind.is_dir())
        .map(|entry| (entry.name, entry.mtime))
        .collect())
}

/// 清掉 /tmp 里同前缀、修改时间早于 `min_age` 的目录。返回删了几个（名字打出来）。
///
/// 只删 [`REMOTE_TEMP_PREFIX`] 开头的目录，别的什么都不碰。SFTP 删不动（比如里面有
/// 非 UTF-8 名字）就用 shell 的 `rm -rf` 兜底。
pub async fn sweep_stale_remote_dirs(
    connection: &Arc<Connection>,
    channel: &zssh_core::sftp::SftpChannel,
    min_age: std::time::Duration,
) -> usize {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Ok(candidates) = list_remote_temp_dirs(channel).await else {
        return 0; // /tmp 列不出来就算了（极少见），别为清扫把测试搞挂
    };
    let mut removed = 0;
    for (name, mtime) in candidates {
        let stale =
            mtime.is_some_and(|mtime| now.saturating_sub(mtime as u64) >= min_age.as_secs());
        if !stale {
            continue;
        }
        let path = format!("/tmp/{name}");
        if delete_remote_dir(connection, channel, &path).await {
            removed += 1;
            println!("清扫：删掉上次遗留的 {path}");
        } else {
            println!("清扫：{path} 没删掉，留给下次");
        }
    }
    removed
}

/// 删一个远端目录：先走自己的递归删除，删不动用 shell 的 `rm -rf` 兜底
/// （非 UTF-8 名字 SFTP 拿不到原始字节，shell 可以）。返回最后还在不在。
pub async fn delete_remote_dir(
    connection: &Arc<Connection>,
    channel: &zssh_core::sftp::SftpChannel,
    path: &str,
) -> bool {
    let deleter = zssh_core::sftp::RecursiveDelete::new();
    let _ = deleter.run(channel, path, |_| {}).await;
    if channel.lstat(path).await.is_ok() {
        let _ = connection.exec(&format!("rm -rf -- '{path}'")).await;
    }
    channel.lstat(path).await.is_err()
}

/// 兜底清理（收尾）用的一条连接的信息：`Drop` 里现连一条，别用测试自己的连接。
#[derive(Clone)]
struct RemoteSweepSpec {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl RemoteSweepSpec {
    fn from_server(server: &TestServer) -> Self {
        Self {
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            password: server.password.clone(),
        }
    }
}

/// 兜底清理的时限：超了就打警告放行（不能把整个测试进程卡死在网络抖动上）。
const SWEEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// 远端临时目录（守卫）：**任何退出路径**都会删掉。
///
/// - 正常路径：测试自己调 [`RemoteTempDir::cleanup`]（用现有通道，快）；
/// - 失败 / panic：`Drop` 里另起线程、现连一条新连接删（测试自己的连接可能已经死了）。
pub struct RemoteTempDir {
    path: String,
    spec: RemoteSweepSpec,
    done: std::cell::Cell<bool>,
}

impl RemoteTempDir {
    /// 建临时目录（顺手清掉上次运行留下的同前缀旧目录）。
    pub async fn create(
        server: &TestServer,
        connection: &Arc<Connection>,
        channel: &zssh_core::sftp::SftpChannel,
        tag: &str,
    ) -> Self {
        sweep_stale_remote_dirs(connection, channel, STALE_DIR_AGE).await;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = format!("/tmp/{REMOTE_TEMP_PREFIX}{tag}-{nanos:x}");
        channel.create_dir(&path).await.expect("建远端临时目录");
        Self {
            path,
            spec: RemoteSweepSpec::from_server(server),
            done: std::cell::Cell::new(false),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 正常路径的清理：用测试手上的通道删（比兜底的现连一条快得多）。
    pub async fn cleanup(
        &self,
        connection: &Arc<Connection>,
        channel: &zssh_core::sftp::SftpChannel,
    ) {
        if self.done.get() {
            return;
        }
        if delete_remote_dir(connection, channel, &self.path).await {
            self.done.set(true);
        } else {
            println!("清理 {} 失败，交给 Drop 兜底", self.path);
        }
    }

    /// 已经用别的办法删掉了（比如非 UTF-8 名字的目录只能用 shell 删）：告诉守卫别再动。
    pub fn mark_done(&self) {
        self.done.set(true);
    }
}

impl std::fmt::Display for RemoteTempDir {
    /// 让 `format!("{dir}/a")` 直接可用（测试里到处拼路径）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path)
    }
}

impl Drop for RemoteTempDir {
    fn drop(&mut self) {
        if self.done.get() {
            return;
        }
        let path = self.path.clone();
        let spec = self.spec.clone();
        println!("守卫：兜底清理 {path}（测试没有走到正常收尾）");
        let path_for_log = path.clone();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            // 自己的 runtime + 自己的一条新连接：这时测试的 runtime 可能正在关闭，
            // 它上面那条连接随时会死，靠不住。
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                println!("守卫：建 runtime 失败，{path} 没删掉");
                return;
            };
            runtime.block_on(async move {
                let known_hosts = TempKnownHosts::new("sftp-guard");
                let params = ConnectParams {
                    host: spec.host,
                    port: spec.port,
                    username: spec.username,
                    auth: AuthConfig::password(Some(spec.password)),
                    known_hosts: known_hosts.known_hosts(),
                    legacy_algorithms: false,
                };
                let Ok(connection) = Connection::connect(params, Arc::new(NonInteractive)).await
                else {
                    println!("守卫：连不上测试服务器，{path} 没删掉");
                    return;
                };
                let connection = Arc::new(connection);
                let Ok(channel) = zssh_core::sftp::SftpChannel::open(&connection).await else {
                    println!("守卫：开 SFTP 失败，{path} 没删掉");
                    return;
                };
                if delete_remote_dir(&connection, &channel, &path).await {
                    println!("守卫：{path} 已删掉");
                } else {
                    println!("守卫：{path} 没删掉（SFTP 和 rm -rf 都失败了）");
                }
                channel.close();
                let _ = connection.disconnect().await;
            });
            let _ = tx.send(());
        });
        if rx.recv_timeout(SWEEP_TIMEOUT).is_err() {
            println!(
                "守卫：清理 {path_for_log} 超过 {} 秒还没完成",
                SWEEP_TIMEOUT.as_secs()
            );
        }
    }
}

/// 本地临时目录（守卫）：`Drop` 里整个删掉，失败 / panic 也一样。
pub struct LocalTempDir {
    path: std::path::PathBuf,
    done: std::cell::Cell<bool>,
    /// 兜底守卫：放在最后，`Drop::drop` 跑完之后它再删一遍（目录已经没了就什么也不做）。
    _guard: TestDir,
}

impl LocalTempDir {
    pub fn create(tag: &str) -> Self {
        let guard = TestDir::new(&format!("sftp-{tag}"));
        let path = guard.to_path_buf();
        Self {
            path,
            done: std::cell::Cell::new(false),
            _guard: guard,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> std::path::PathBuf {
        self.path.join(name)
    }

    pub fn mark_done(&self) {
        self.done.set(true);
    }
}

impl Drop for LocalTempDir {
    fn drop(&mut self) {
        if self.done.get() {
            return;
        }
        // 长路径过一遍（测试里可能造出超过 260 字符的路径）。
        let path = zssh_core::sftp::local::long_path(&self.path);
        if let Err(err) = std::fs::remove_dir_all(&path) {
            println!("守卫：本地临时目录 {} 没删掉（{err}）", self.path.display());
        }
    }
}

/// 本机的 ssh-keygen 在不在（私钥相关测试要用它现生成钥匙）。
pub fn has_ssh_keygen() -> bool {
    std::process::Command::new("ssh-keygen")
        .arg("-?")
        .output()
        .map(|_| true)
        .unwrap_or(false)
}

// ---------- M3-4：AI 文件工具的联网测试共用 ----------

/// 给 `BridgeHost::remote_fs` 用：在连接上开一条自己的 SFTP 通道（和 `src-tauri` 一样，用完即关）。
pub fn remote_fs_on(
    connection: &Arc<Connection>,
) -> zssh_core::ssh::BoxFuture<'_, Result<Arc<dyn zssh_core::ai::files::RemoteFs>, String>> {
    Box::pin(async move {
        let channel = zssh_core::sftp::SftpChannel::open(connection)
            .await
            .map_err(|err| err.to_string())?;
        let fs: Arc<dyn zssh_core::ai::files::RemoteFs> =
            Arc::new(zssh_core::ai::files::SftpRemoteFs::new(Arc::new(channel)));
        Ok(fs)
    })
}

/// 三个智能体的"读、改一行、写回"端到端共用的场景：服务器专用临时目录里一个三行文件。
pub struct FileEditScenario {
    pub dir: RemoteTempDir,
    pub path: String,
    channel: zssh_core::sftp::SftpChannel,
    connection: Arc<Connection>,
}

pub const FILE_ORIGINAL: &str = "alpha\nbravo\ncharlie\n";
pub const FILE_EDITED: &str = "alpha\nBRAVO-EDITED\ncharlie\n";

impl FileEditScenario {
    pub async fn setup(server: &TestServer, connection: &Arc<Connection>) -> Self {
        let channel = open_sftp(connection).await.expect("开测试通道");
        let dir = RemoteTempDir::create(server, connection, &channel, "agentfile").await;
        let path = format!("{}/notes.txt", dir.path());
        zssh_core::sftp::fs::write_bytes(&channel, &path, FILE_ORIGINAL.as_bytes(), u64::MAX)
            .await
            .expect("准备文件");
        Self {
            dir,
            path,
            channel,
            connection: Arc::clone(connection),
        }
    }

    /// 给智能体的话：读、只改第二行、写回（要求用文件工具，不要用命令）。
    pub fn prompt(&self) -> String {
        format!(
            "请用 read_file 工具读取测试服务器上的文件 {path}，把第二行 bravo 改成 BRAVO-EDITED，\
             其余内容原样不动，再用 write_file 工具把完整新内容写回同一个路径。\
             不要用 run_command。完成后只回复 DONE。",
            path = self.path
        )
    }

    /// 核对：文件只有第二行变了；备份是原内容；没有临时文件残留。返回备份路径。
    pub async fn verify(&self) -> String {
        let now = zssh_core::sftp::fs::read_bytes(&self.channel, &self.path, 1 << 20)
            .await
            .expect("读回文件");
        assert_eq!(String::from_utf8_lossy(&now), FILE_EDITED, "文件内容不对");
        let names: Vec<String> = self
            .channel
            .read_dir(self.dir.path())
            .await
            .expect("列临时目录")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            names.iter().all(|n| !n.contains("zssh-tmp")),
            "有临时文件残留：{names:?}"
        );
        let backup = names
            .iter()
            .find(|n| n.starts_with("notes.txt.zssh-bak-"))
            .unwrap_or_else(|| panic!("没有生成备份：{names:?}"));
        let backup_path = format!("{}/{backup}", self.dir.path());
        let old = zssh_core::sftp::fs::read_bytes(&self.channel, &backup_path, 1 << 20)
            .await
            .expect("读备份");
        assert_eq!(
            String::from_utf8_lossy(&old),
            FILE_ORIGINAL,
            "备份应该是原内容"
        );
        backup_path
    }

    pub async fn cleanup(&self) {
        self.dir.cleanup(&self.connection, &self.channel).await;
        assert!(
            self.channel.stat(self.dir.path()).await.is_err(),
            "临时目录要删干净"
        );
    }
}

/// 临时目录守卫：`zssh-` 前缀，Drop 时整棵删掉（含 panic / 提前 return）。
/// 可以直接当 `&Path` 用；要先于它释放的句柄/子进程放在持有者里更靠前的字段。
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
