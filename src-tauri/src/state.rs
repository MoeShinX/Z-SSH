use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use zssh_core::ai::bridge::terminal_write::{deliver_ai_write, AiWriteSlot, SESSION_GONE};
use zssh_core::ai::pty::TerminalState;
use zssh_core::ai::terminal::SharedTerminalRing;
use zssh_core::ssh::connection::ExecGuard;
use zssh_core::ssh::{
    Connection, ConnectionGuard, ConnectionPool, FlowGate, KnownHosts, PoolConfig,
};
use zssh_core::Database;

use crate::interact::PromptRegistry;

/// 界面发给某个会话的指令。
///
/// 会话的读写两半分别被不同的任务持有，指令用 mpsc 送进去，避免共享锁。
///
/// ack 不在这里：设计文档 5.2 的硬规则——ack 和关闭不能排在任何可能阻塞的操作
/// 后面。会话任务卡在写通道时，排在它后面的指令永远不会被处理。
#[derive(Debug)]
pub enum SessionCommand {
    Write(Vec<u8>),
    /// AI 命令的整行包装脚本（AI-2d 第 2 节）：会话任务在写之前先让显示过滤器
    /// 记住这条命令，包装脚本的回显和标记行就不会出现在用户眼前。
    WriteAiPayload {
        /// 整行原文（清输入前缀 + 包装脚本）。
        payload: Vec<u8>,
        marker: String,
        /// 用户要看到的命令原文（合成回显用）。
        command: String,
        /// 写之前要先认领：排队期间用户点了停止 / 移出了目标，这条就不写（审计 C04）。
        slot: AiWriteSlot,
    },
    /// AI 命令超时返回（命令还在终端里跑，结束标记以后才出现）：显示过滤器退出
    /// 完整过滤（AI-2e：不能压着用户之后的输入），只留轻量过滤丢迟到的那一行。
    AiCommandTimedOut {
        marker: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Close,
}

/// 会话对连接的一份引用：池里复用的连接，或者快速连接自己建的那条。
#[derive(Clone)]
pub enum SessionConnection {
    /// 池里的连接：最后一个引用释放后，池子空闲一会儿再断开
    Pooled(Arc<ConnectionGuard>),
    /// 快速连接不进池：最后一个引用没了直接断开
    Direct(Arc<Connection>),
    /// 只给单测用：`reset_sessions` 只碰流控门和命令队列，不碰连接，所以测会话
    /// 回收不需要真的连一台服务器。生产构建里没有这个变体。
    #[cfg(test)]
    Test,
}

impl SessionConnection {
    pub fn connection(&self) -> &Arc<Connection> {
        match self {
            Self::Pooled(guard) => guard.connection(),
            Self::Direct(connection) => connection,
            #[cfg(test)]
            Self::Test => unreachable!("测试占位的连接没有底层连接"),
        }
    }

    /// 这条连接是这一次新建的，还是池里复用的（快速连接每次都是新建的）。
    pub fn is_fresh(&self) -> bool {
        match self {
            Self::Pooled(guard) => guard.is_fresh(),
            Self::Direct(_) => true,
            #[cfg(test)]
            Self::Test => true,
        }
    }
}

/// 已经连上的会话。
pub struct OpenSession {
    pub commands: mpsc::Sender<SessionCommand>,
    /// 流控门句柄。`term_ack` 和 `session_close` 直接操作它，绕开命令队列。
    pub gate: Arc<FlowGate>,
    pub connection: SessionConnection,
    /// 主机库里的主机 ID；快速连接是 `None`（不能进池、也不能"记住密码"）
    pub host_id: Option<i64>,
    /// 界面上的标签：主机名，快速连接是连接串
    pub label: String,
    /// 登录用户名（工具桥的 `list_sessions` 要显示）。
    pub username: String,
    /// 终端输出环形缓冲（工具桥的 `read_terminal` 读它）。
    pub ring: SharedTerminalRing,
    /// 终端执行用的状态（AI-2c）：备用屏幕跟踪 + "AI 正在执行" 标志 + shell 类型。
    pub terminal: Arc<TerminalState>,
    /// 关闭信号：绕开命令队列直接叫醒会话任务（审计 C16）。远端不再补充 SSH 窗口时写入会
    /// 一直等，队列里排在后面的 Close 永远轮不到；会话任务把它和正在进行的写入一起 select。
    pub closing: Arc<CancelSignal>,
}

impl OpenSession {
    /// 叫会话任务收工：先置关闭信号（不排队，写入卡住也能被抢占），再往队列里放一个 Close
    /// 兜底。队列满时不等——关闭信号已经够了，等空位反而可能卡在写不出去的会话上（C16）。
    pub fn close_task(&self) {
        self.closing.cancel();
        let _ = self.commands.try_send(SessionCommand::Close);
    }
}

/// 会话表里的条目：连接中（已经在后台连了）或者已经连上。
pub enum SessionEntry {
    Connecting { cancel: Arc<CancelSignal> },
    Open(OpenSession),
}

/// 会话的句柄（写、改窗口大小、关）。
#[derive(Clone)]
pub struct SessionHandle {
    pub commands: mpsc::Sender<SessionCommand>,
    pub gate: Arc<FlowGate>,
}

/// 取消信号：连接过程中关闭会话，用它中断后台任务。
#[derive(Default)]
pub struct CancelSignal {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelSignal {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 给 `zssh-core` 里只认原子布尔值的取消检查用（服务器端复制、计算大小等）。
    pub fn atomic(&self) -> &AtomicBool {
        &self.cancelled
    }

    /// 等到被取消（已经取消了就立刻返回）。
    pub async fn cancelled(&self) {
        loop {
            // 先注册通知再看状态，否则"读完状态到开始等"之间会丢掉唤醒。
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// 应用全局状态：会话表和主机库。
///
/// 会话表用 `tokio::sync::Mutex`（命令是 async 的，锁要能跨 await 持有）；
/// 数据库自己带锁，见 `zssh_core::store::Database`。
pub struct AppState {
    sessions: Mutex<HashMap<String, SessionEntry>>,
    next_id: AtomicU64,
    /// 主机库、凭据、代理。
    pub store: Database,
    /// 还没被回答的认证 / 指纹提问。会话关闭时用它取消这个会话挂着的提问。
    pub prompts: Arc<PromptRegistry>,
    /// known_hosts：自己的可写文件 + 系统那份只读
    pub known_hosts: KnownHosts,
    /// 已认证连接的池子（设计文档 4.1）：同一台主机的第二个 shell 不再认证。
    pub pool: ConnectionPool,
}

impl AppState {
    pub fn new(store: Database, known_hosts: KnownHosts, pool_config: PoolConfig) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            store,
            prompts: Arc::new(PromptRegistry::default()),
            known_hosts,
            pool: ConnectionPool::new(pool_config),
        }
    }

    /// 会话 ID 从 1 开始，方便和日志对照。
    pub fn next_session_id(&self) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("sess-{id}")
    }

    /// 登记一个"连接中"的会话（`session_open` 立即返回之前调用）。
    pub async fn insert_connecting(&self, session_id: String, cancel: Arc<CancelSignal>) {
        self.sessions
            .lock()
            .await
            .insert(session_id, SessionEntry::Connecting { cancel });
    }

    /// 连接成功：把"连接中"换成已连上。
    ///
    /// 返回 `false` 表示这个会话在连接期间被取消了（条目已经不在），调用方负责
    /// 把刚建好的 shell 和连接引用收掉。
    pub async fn promote_to_open(&self, session_id: &str, session: OpenSession) -> bool {
        let mut sessions = self.sessions.lock().await;
        match sessions.get(session_id) {
            Some(SessionEntry::Connecting { .. }) => {
                sessions.insert(session_id.to_owned(), SessionEntry::Open(session));
                true
            }
            _ => false,
        }
    }

    /// 取走"连接中"的条目（连接失败收尾用）。返回有没有取到。
    pub async fn take_connecting(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        if matches!(
            sessions.get(session_id),
            Some(SessionEntry::Connecting { .. })
        ) {
            sessions.remove(session_id);
            true
        } else {
            false
        }
    }

    /// 摘掉一个会话（关闭时先摘，新的命令立刻拿不到它）。
    pub async fn remove(&self, session_id: &str) -> Option<SessionEntry> {
        self.sessions.lock().await.remove(session_id)
    }

    /// 已连上的会话的句柄。
    pub async fn handle(&self, session_id: &str) -> Option<SessionHandle> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some(SessionHandle {
                commands: session.commands.clone(),
                gate: Arc::clone(&session.gate),
            }),
            _ => None,
        }
    }

    /// 复制会话要的东西：连接引用、主机 ID、标签、用户名。
    pub async fn duplicate_source(
        &self,
        session_id: &str,
    ) -> Option<(SessionConnection, Option<i64>, String, String)> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some((
                session.connection.clone(),
                session.host_id,
                session.label.clone(),
                session.username.clone(),
            )),
            _ => None,
        }
    }

    /// 工具桥用：会话背后的底层连接（`run_command` 在它上面开 exec 通道）。
    pub async fn exec_connection(&self, session_id: &str) -> Option<Arc<Connection>> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some(Arc::clone(session.connection.connection())),
            _ => None,
        }
    }

    /// 取终端当前目录用：这个会话背后的连接，以及它是这条连接上**第几个**打开的终端
    /// （从 0 数，按会话 ID 的数字顺序）和这条连接上现在一共几个终端。
    ///
    /// 同一条池连接上的多个终端在服务器上是同一个 sshd 的兄弟进程，只能靠打开顺序对号
    /// （见 `zssh_core::ssh::cwd`）。
    pub async fn shell_position(
        &self,
        session_id: &str,
    ) -> Option<(Arc<Connection>, usize, usize)> {
        let sessions = self.sessions.lock().await;
        let SessionEntry::Open(target) = sessions.get(session_id)? else {
            return None;
        };
        let connection = Arc::clone(target.connection.connection());
        let mut ids: Vec<u64> = sessions
            .iter()
            .filter_map(|(id, entry)| match entry {
                SessionEntry::Open(open)
                    if Arc::ptr_eq(open.connection.connection(), &connection) =>
                {
                    id.strip_prefix("sess-")?.parse::<u64>().ok()
                }
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        let own = session_id.strip_prefix("sess-")?.parse::<u64>().ok()?;
        let ordinal = ids.iter().position(|id| *id == own)?;
        Some((connection, ordinal, ids.len()))
    }

    /// 工具桥用：会话的终端环形缓冲（`read_terminal` 读它）。
    pub async fn terminal_ring(&self, session_id: &str) -> Option<SharedTerminalRing> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some(Arc::clone(&session.ring)),
            _ => None,
        }
    }

    /// AI-2c：会话的终端执行状态（判断能不能就地执行）。
    pub async fn terminal_exec(&self, session_id: &str) -> Option<Arc<TerminalState>> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some(Arc::clone(&session.terminal)),
            _ => None,
        }
    }

    /// AI-2c：按主机 ID 找一个已经连上的会话（给 @ 主机复用连接用）。
    pub async fn session_for_host(&self, host_id: i64) -> Option<String> {
        self.sessions
            .lock()
            .await
            .iter()
            .find_map(|(id, entry)| match entry {
                SessionEntry::Open(session) if session.host_id == Some(host_id) => Some(id.clone()),
                _ => None,
            })
    }

    /// 会话表里有没有这个会话（连着的或者还在连接中的都算）。AI 判断界面绑定的会话还在不在用。
    pub async fn has_session(&self, session_id: &str) -> bool {
        self.sessions.lock().await.contains_key(session_id)
    }

    /// AI-2c：向一个会话的终端写入字节（发 Ctrl+C 之类）。
    pub async fn write_raw(&self, session_id: &str, data: Vec<u8>) -> bool {
        // 先放全局锁再等队列空位（C16）。
        match self.command_sender(session_id).await {
            Some(sender) => sender.send(SessionCommand::Write(data)).await.is_ok(),
            None => false,
        }
    }

    /// AI-2e：告诉会话任务这条 AI 命令超时返回了（显示过滤器退出、标记转待处理）。
    pub async fn ai_command_timed_out(&self, session_id: &str, marker: String) -> bool {
        match self.command_sender(session_id).await {
            Some(sender) => sender
                .send(SessionCommand::AiCommandTimedOut { marker })
                .await
                .is_ok(),
            None => false,
        }
    }

    /// AI-2d：把 AI 命令的包装脚本写进终端，并让显示过滤器记住它（合成回显用）。
    ///
    /// 有效性检查跟着消息一起进队列，会话任务真正写之前再核对（审计 C04）：排队 / 等前面的
    /// 写入期间请求失效，这条就不写，返回原因。`Ok` 表示已经写进终端。
    pub async fn write_ai_payload(
        &self,
        session_id: &str,
        payload: Vec<u8>,
        marker: String,
        command: String,
        guard: Arc<dyn ExecGuard>,
    ) -> Result<(), String> {
        let Some(sender) = self.command_sender(session_id).await else {
            return Err(SESSION_GONE.to_owned());
        };
        deliver_ai_write(&sender, guard, |slot| SessionCommand::WriteAiPayload {
            payload,
            marker,
            command,
            slot,
        })
        .await
    }

    /// 已连上的会话的命令队列（克隆一份）。
    ///
    /// 往队列里送东西要在全局会话锁**外面**做：队列有界，会话任务卡在写入上时会一直等空位，
    /// 拿着全局锁等就把所有会话的操作都卡住了（审计 C16）。
    async fn command_sender(&self, session_id: &str) -> Option<mpsc::Sender<SessionCommand>> {
        match self.sessions.lock().await.get(session_id) {
            Some(SessionEntry::Open(session)) => Some(session.commands.clone()),
            _ => None,
        }
    }

    /// 服务器状态栏（M1-10）用：会话背后的底层连接（含连接键）。
    ///
    /// 同一条池连接上的所有会话（duplicate 出来的分屏）拿到**同一个**键和
    /// 同一个 `Arc<Connection>`，采集任务天然去重。会话还没连上时没有连接，
    /// 状态栏也就采不了。
    pub async fn stats_connection(&self, session_id: &str) -> Option<(String, Arc<Connection>)> {
        let (connection, _, _, _) = self.duplicate_source(session_id).await?;
        let inner = connection.connection();
        let key = connection_key(inner);
        Some((key, Arc::clone(inner)))
    }

    /// 前端换了一页（刷新 / 热更新 / 崩溃后重开）：把上一页留下的会话全部收掉。
    ///
    /// 新的前端实例不可能认领这些会话，所以连同**连接中的**（等于取消）和挂着的
    /// 提问一起关掉是安全的。连接不在这里断：池子按正常的空闲规则回收。
    ///
    /// 关闭对象是**进入本函数时**会话表的快照：本函数还在关旧会话的时候，前端
    /// 新开的会话不在快照里，不会被误关。返回关掉了几个。
    pub async fn reset_sessions(&self) -> usize {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut closed = 0;
        for session_id in ids {
            // 快照之后可能已经被正常关掉了，取不到就跳过(不计入)。
            let Some(entry) = self.sessions.lock().await.remove(&session_id) else {
                continue;
            };
            match entry {
                SessionEntry::Connecting { cancel } => {
                    // 连接中：取消后台任务，它醒来自己收尾
                    cancel.cancel();
                    self.prompts.cancel_session(&session_id);
                }
                SessionEntry::Open(session) => {
                    // 和 session_close 一样：先直接关流控门，读任务才醒得过来
                    session.gate.close();
                    self.prompts.cancel_session(&session_id);
                    session.close_task();
                }
            }
            closed += 1;
        }
        closed
    }
}

/// 服务器状态栏的连接键：底层 `Arc<Connection>` 的指针地址（同一台主机
/// 分屏出来的所有会话共享同一条连接，键相同）。
pub fn connection_key(connection: &Arc<Connection>) -> String {
    format!("conn-{:p}", Arc::as_ptr(connection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 直接建一个测试用的 AppState（内存库 + 默认池）。
    /// 同时返回 known_hosts 目录的守卫：调用方持有到测试结束，Drop 时整个目录删掉。
    fn test_state() -> (AppState, tempfile::TempDir) {
        let store = Database::open_in_memory().expect("内存库");
        let known_hosts_dir = tempfile::Builder::new()
            .prefix("zssh-test-known-hosts-")
            .tempdir()
            .expect("建临时目录");
        let known_hosts = KnownHosts::new(known_hosts_dir.path().join("known_hosts"));
        (
            AppState::new(store, known_hosts, PoolConfig::default()),
            known_hosts_dir,
        )
    }

    /// 造一个"已连上"的会话（连接是测试占位，`reset_sessions` 不碰它）。
    async fn open_entry() -> SessionEntry {
        let (commands, _rx) = mpsc::channel(8);
        SessionEntry::Open(OpenSession {
            commands,
            gate: Arc::new(FlowGate::new()),
            connection: SessionConnection::Test,
            host_id: None,
            label: "测试".to_owned(),
            username: "root".to_owned(),
            ring: zssh_core::ai::terminal::TerminalRing::shared(),
            terminal: Arc::new(TerminalState::new()),
            closing: Arc::new(CancelSignal::default()),
        })
    }

    /// 放一个命令队列已经满了的会话（会话任务卡在写入上、不再取队列）。
    async fn insert_stuck_session(
        state: &AppState,
        session_id: &str,
    ) -> (mpsc::Receiver<SessionCommand>, Arc<CancelSignal>) {
        let (commands, rx) = mpsc::channel(1);
        commands
            .try_send(SessionCommand::Write(b"x".to_vec()))
            .expect("占满队列");
        let closing = Arc::new(CancelSignal::default());
        state.sessions.lock().await.insert(
            session_id.to_owned(),
            SessionEntry::Open(OpenSession {
                commands,
                gate: Arc::new(FlowGate::new()),
                connection: SessionConnection::Test,
                host_id: Some(7),
                label: "测试".to_owned(),
                username: "root".to_owned(),
                ring: zssh_core::ai::terminal::TerminalRing::shared(),
                terminal: Arc::new(TerminalState::new()),
                closing: Arc::clone(&closing),
            }),
        );
        (rx, closing)
    }

    #[tokio::test]
    async fn a_full_session_queue_does_not_block_the_global_session_table() {
        // C16：AI 往一个卡住的会话发 Ctrl+C / 超时通知时在等队列空位，这时别的会话操作
        // （查会话、开新会话、关会话）不能跟着卡住。以前是拿着全局会话锁等的。
        let (state, _known_hosts_dir) = test_state();
        let state = Arc::new(state);
        let (_rx, _closing) = insert_stuck_session(&state, "sess-1").await;
        let raw = {
            let state = Arc::clone(&state);
            tokio::spawn(async move { state.write_raw("sess-1", vec![0x03]).await })
        };
        let timed_out = {
            let state = Arc::clone(&state);
            tokio::spawn(async move { state.ai_command_timed_out("sess-1", "m".into()).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!raw.is_finished(), "队列满，发送方要等");
        assert!(!timed_out.is_finished(), "队列满，发送方要等");
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(state.handle("sess-1").await.is_some());
            assert_eq!(state.session_for_host(7).await.as_deref(), Some("sess-1"));
            state
                .insert_connecting("sess-2".to_owned(), Arc::new(CancelSignal::default()))
                .await;
            assert!(state.remove("sess-2").await.is_some());
        })
        .await
        .expect("全局会话表不能被一个满队列卡住");
        raw.abort();
        timed_out.abort();
    }

    #[tokio::test]
    async fn has_session_counts_connecting_and_open_sessions() {
        // AI 判断界面绑定的会话还在不在：连接中的也算在（要等它，不能另开一个）。
        let (state, _known_hosts_dir) = test_state();
        assert!(!state.has_session("sess-1").await);
        state
            .insert_connecting("sess-1".to_owned(), Arc::new(CancelSignal::default()))
            .await;
        assert!(state.has_session("sess-1").await);
        assert!(state.remove("sess-1").await.is_some());
        assert!(!state.has_session("sess-1").await);
    }

    #[tokio::test]
    async fn closing_a_stuck_session_signals_the_task_without_waiting_for_queue_space() {
        // C16：会话任务卡在写入上、队列满：关闭不能排在队列后面等，要直接置关闭信号。
        let (state, _known_hosts_dir) = test_state();
        let (_rx, closing) = insert_stuck_session(&state, "sess-1").await;
        let closed = tokio::time::timeout(Duration::from_secs(2), state.reset_sessions())
            .await
            .expect("关闭不能等队列空位");
        assert_eq!(closed, 1);
        assert!(closing.is_cancelled(), "关闭信号要置上");
    }

    #[tokio::test]
    async fn reset_closes_both_connecting_and_open_and_is_idempotent() {
        let (state, _known_hosts_dir) = test_state();

        // 一个连接中的 + 一个已连上的
        let cancel = Arc::new(CancelSignal::default());
        state
            .insert_connecting("sess-1".to_owned(), Arc::clone(&cancel))
            .await;
        let gate = match open_entry().await {
            SessionEntry::Open(session) => {
                state
                    .sessions
                    .lock()
                    .await
                    .insert("sess-2".to_owned(), SessionEntry::Open(session));
                state
                    .handle("sess-2")
                    .await
                    .expect("刚插入的会话要能拿到句柄")
                    .gate
            }
            SessionEntry::Connecting { .. } => unreachable!(),
        };

        assert_eq!(state.reset_sessions().await, 2, "两个会话都要关掉");
        assert!(cancel.is_cancelled(), "连接中的会话要被取消");
        assert!(gate.is_closed(), "已连上的会话的流控门要被关掉");
        assert!(state.handle("sess-2").await.is_none(), "会话表要清空");

        // 再调一次：空的，不报错（StrictMode 下挂载两次）
        assert_eq!(state.reset_sessions().await, 0, "重复调用是空的，也不报错");
    }

    #[tokio::test]
    async fn reset_only_closes_sessions_that_existed_when_it_started() {
        // reset 的快照语义：reset 之后新开的会话不该被同一次 reset 碰到。
        // 单线程下没法真的并发，这里验证语义本身——reset 只处理调用那一刻表里的条目。
        let (state, _known_hosts_dir) = test_state();
        state
            .insert_connecting("old".to_owned(), Arc::new(CancelSignal::default()))
            .await;

        let closed = state.reset_sessions().await;
        assert_eq!(closed, 1);

        // reset 之后新开的会话还在
        let new_cancel = Arc::new(CancelSignal::default());
        state
            .insert_connecting("new".to_owned(), Arc::clone(&new_cancel))
            .await;
        assert!(!new_cancel.is_cancelled(), "reset 之后开的会话不该被关");
        assert!(state.handle("new").await.is_none(), "还是连接中，没有句柄");
    }

    /// 取终端目录：会话不存在 / 还在连接中，都取不到——命令返回 `None`，前端安静地留在家目录。
    #[tokio::test]
    async fn shell_position_is_none_for_unknown_or_connecting_sessions() {
        let (state, _known_hosts_dir) = test_state();
        assert!(state.shell_position("sess-404").await.is_none());
        state
            .insert_connecting("sess-1".to_owned(), Arc::new(CancelSignal::default()))
            .await;
        assert!(state.shell_position("sess-1").await.is_none());
    }
}
