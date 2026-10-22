use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Instant};

use crate::ai::terminal::SharedTerminalRing;
use crate::error::ZsshError;
use crate::ssh::flow::FlowGate;

/// 请求的终端类型，M1 固定用这个。
pub const TERM: &str = "xterm-256color";

/// 通道本地缓冲的上限（设计文档 4.1 第一级）：读任务把通道数据堆到这里，
/// 堆满才停止读 russh。调大会多占内存，调小会让刷屏更容易卡住整条连接。
pub const DEFAULT_BUFFER_LIMIT: usize = 8 * 1024 * 1024;

/// 缓冲降到上限的一半就恢复读，避免在满和不满之间反复横跳。
fn resume_at(limit: usize) -> usize {
    limit / 2
}

/// 事件队列长度。攒批之后一条事件最多 64KB，真正限制在途数据的是
/// [`FlowGate`] 的高水位，这里留够就行。
const EVENT_BUFFER: usize = 64;

/// 攒批的两个阈值（设计文档 5.1），放在一起：
///
/// - 攒够 [`BATCH_MAX_BYTES`] 立刻发，不等定时器，避免大块输出被拆得太碎；
/// - 从第一个字节进来算起最多等 [`BATCH_MAX_DELAY`]，16ms 约等于一帧（60Hz），
///   再长就会让交互输出看起来一顿一顿的。
///
/// 两个值都是"先到者为准"。
const BATCH_MAX_BYTES: usize = 64 * 1024;
const BATCH_MAX_DELAY: Duration = Duration::from_millis(16);

/// shell 通道产生的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// 远端输出（包含 stderr），已经攒过批。
    Data(Vec<u8>),
    /// 远端进程退出，`None` 表示只收到了信号没有退出码。
    Exit(Option<u32>),
    /// 通道已经关闭，不会再有事件。
    Closed,
}

/// 开终端时等服务器回复 PTY / shell 请求的超时（每一步各算一次）。
pub const OPEN_REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// PTY 被服务器拒绝时放在终端输出最前面的提示（学 OpenSSH：不判失败，以无终端模式继续）。
/// 典型场景是 git 托管账号、`authorized_keys` 里写了 `no-pty` 的账号——服务器多半会回一句
/// 说明再断开，用户得能看到这句，所以提示只是前置、不拦后面的输出。
const PTY_DENIED_NOTICE: &str =
    "\r\n\x1b[33m[Z-SSH] 服务器拒绝分配终端（PTY），以无终端模式继续\x1b[0m\r\n";

/// 请求 PTY 和 shell，两步都等服务器的回复；返回等回复期间先到的输出（原顺序）。
///
/// `data_limit` 是等回复期间允许累计的数据字节数（和正常缓冲同一个值）：russh 收到数据
/// 就自动放宽窗口，不等这里处理，不设上限的话服务器一直发内存就一直涨。
async fn request_pty_and_shell(
    write_half: &ChannelWriteHalf<Msg>,
    read_half: &mut ChannelReadHalf,
    cols: u16,
    rows: u16,
    reply_timeout: Duration,
    data_limit: usize,
) -> Result<Vec<Item>, ZsshError> {
    // 有的服务器回复之前就先发数据，这些一个字节都不能丢。
    let mut pending = Pending::default();

    write_half
        .request_pty(true, TERM, u32::from(cols), u32::from(rows), 0, 0, &[])
        .await
        .map_err(|err| ZsshError::Shell(format!("请求 PTY 失败：{err}")))?;
    let pty_accepted =
        wait_reply(read_half, "PTY", reply_timeout, &mut pending, data_limit).await?;

    write_half
        .request_shell(true)
        .await
        .map_err(|err| ZsshError::Shell(format!("请求 shell 失败：{err}")))?;
    if !wait_reply(read_half, "shell", reply_timeout, &mut pending, data_limit).await? {
        return Err(ZsshError::Shell("服务器拒绝打开 shell".into()));
    }

    let mut items = pending.items;
    if !pty_accepted {
        items.insert(0, Item::Data(PTY_DENIED_NOTICE.as_bytes().to_vec()));
    }
    Ok(items)
}

/// 等回复期间最多暂存多少条消息（数据块和退出事件合计）。
///
/// 字节上限只管数据；退出事件不带数据却也要存一条，服务器反复发退出事件同样能把内存
/// 撑大，所以条数也要卡。正常服务器在回复前顶多发几条，碰不到这个数。
const PENDING_MAX_ITEMS: usize = 4096;

/// 等回复期间先到的输出，连同累计的数据字节数（用来卡上限）。
#[derive(Default)]
struct Pending {
    items: Vec<Item>,
    data_bytes: usize,
}

impl Pending {
    /// 暂存一条；超过条数上限就判定打开失败。
    fn push(&mut self, item: Item, what: &str) -> Result<(), ZsshError> {
        if self.items.len() >= PENDING_MAX_ITEMS {
            return Err(ZsshError::Shell(format!(
                "服务器在回复前发送了过多消息（等待 {what} 请求的回复时，超过 {PENDING_MAX_ITEMS} 条）"
            )));
        }
        self.items.push(item);
        Ok(())
    }
}

/// 在通道上等一个请求的回复：`Ok(true)` = Success，`Ok(false)` = Failure。
///
/// 等的过程中到的数据和退出事件存进 `pending`；通道关了、EOF 了、没了都算打开失败；
/// 累计数据超过 `data_limit` 也算打开失败。
async fn wait_reply(
    read_half: &mut ChannelReadHalf,
    what: &str,
    reply_timeout: Duration,
    pending: &mut Pending,
    data_limit: usize,
) -> Result<bool, ZsshError> {
    let wait = async {
        loop {
            match read_half.wait().await {
                Some(ChannelMsg::Success) => return Ok(true),
                Some(ChannelMsg::Failure) => return Ok(false),
                // stderr 走扩展数据，和 read_channel 一样按普通输出处理。
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    pending.data_bytes = pending.data_bytes.saturating_add(data.len());
                    if pending.data_bytes > data_limit {
                        return Err(ZsshError::Shell(format!(
                            "服务器在回复前发送了过多数据（等待 {what} 请求的回复时，超过 {data_limit} 字节）"
                        )));
                    }
                    pending.push(Item::Data(data.to_vec()), what)?;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    pending.push(Item::Exit(Some(exit_status)), what)?;
                }
                Some(ChannelMsg::ExitSignal { .. }) => pending.push(Item::Exit(None), what)?,
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => {
                    return Err(ZsshError::Shell(format!(
                        "服务器在打开终端时关闭了连接（等待 {what} 请求的回复时）"
                    )));
                }
                // 窗口调整等无关消息照常忽略。
                Some(_) => {}
            }
        }
    };
    match timeout(reply_timeout, wait).await {
        Ok(result) => result,
        Err(_) => Err(ZsshError::Shell(format!(
            "等待服务器回复 {what} 请求超时（{} 秒）",
            reply_timeout.as_secs_f32().ceil() as u64
        ))),
    }
}

/// 关通道时最多等多久（连接已经坏了的话 `close()` 可能一直发不出去）。
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// 还没交给 [`ShellSession`] 的通道写半边：丢弃即关闭。
///
/// 开终端要等服务器回复，最长 20 秒；这期间用户关掉窗格，外层会直接丢弃正在等的 future，
/// 走不到任何"失败后关通道"的代码。russh 的 `Channel` / `ChannelWriteHalf` 被丢弃时不会通知
/// 服务器，复用的连接上这条通道会一直占着一个会话名额（MaxSessions）。所以由守卫在
/// `Drop` 里兜底：用 `tokio::spawn` 异步发 `close()`（`Drop` 里不能 await）。
/// 打开成功时用 [`CloseOnDrop::disarm`] 取出写半边，守卫就不再管了。
struct CloseOnDrop {
    write_half: Option<ChannelWriteHalf<Msg>>,
}

impl CloseOnDrop {
    fn new(write_half: ChannelWriteHalf<Msg>) -> Self {
        Self {
            write_half: Some(write_half),
        }
    }

    /// 只有 disarm / close_now 会取走写半边，而它们都消耗 self，所以这里实际不会是 None。
    fn write_half(&self) -> Result<&ChannelWriteHalf<Msg>, ZsshError> {
        self.write_half
            .as_ref()
            .ok_or_else(|| ZsshError::Shell("通道写半边已被取走".into()))
    }

    /// 解除守卫，把写半边交出去。
    fn disarm(mut self) -> Result<ChannelWriteHalf<Msg>, ZsshError> {
        self.write_half
            .take()
            .ok_or_else(|| ZsshError::Shell("通道写半边已被取走".into()))
    }

    /// 正常的失败路径：直接等着把通道关掉（有超时），不依赖 Drop。
    async fn close_now(mut self) {
        if let Some(write_half) = self.write_half.take() {
            let _ = timeout(CLOSE_TIMEOUT, write_half.close()).await;
        }
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let Some(write_half) = self.write_half.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let _ = timeout(CLOSE_TIMEOUT, write_half.close()).await;
                });
            }
            // 没有运行时（运行时已经在关闭）：发不出异步消息，连接此时也基本在随之关闭。
            Err(_) => tracing::debug!("开终端被取消时没有 tokio 运行时，未能发送通道关闭"),
        }
    }
}

/// 一个已打开的 shell 通道。
///
/// 需要一边等事件一边写的时候用 [`ShellSession::split`] 把读写两半拆开，这样两个
/// 任务各自持有半边，不用给 shell 加锁。
pub struct ShellSession {
    writer: ShellWriter,
    events: ShellEventStream,
}

impl ShellSession {
    /// 在一个已经打开的通道上请求 PTY 和 shell。
    ///
    /// `buffer_limit` 是通道本地缓冲的上限（设计文档 4.1 第一级）。压测要能快速
    /// 把缓冲顶满（不然得先灌 8MB），所以做成参数；生产路径用
    /// [`DEFAULT_BUFFER_LIMIT`]。`ring` 给 `Some` 时，转发路径上会旁路复制一份
    /// 最近输出（工具桥 `read_terminal` 用）。
    ///
    /// 两个请求都带 want_reply 并**等服务器的回复**（`CHANNEL_SUCCESS` / `CHANNEL_FAILURE`）：
    /// russh 的 `request_*` 只管把请求发出去，不等回复，不等的话服务器拒绝了界面也会
    /// 显示"已连接"（终端审查 T05）。
    pub(crate) async fn open_with_buffer_limit(
        channel: Channel<Msg>,
        cols: u16,
        rows: u16,
        buffer_limit: usize,
        ring: Option<SharedTerminalRing>,
    ) -> Result<Self, ZsshError> {
        Self::open_with_reply_timeout(channel, cols, rows, buffer_limit, ring, OPEN_REPLY_TIMEOUT)
            .await
    }

    /// 同 [`ShellSession::open_with_buffer_limit`]，等每个回复的超时可调（测试用，不用真等 20 秒）。
    pub(crate) async fn open_with_reply_timeout(
        channel: Channel<Msg>,
        cols: u16,
        rows: u16,
        buffer_limit: usize,
        ring: Option<SharedTerminalRing>,
        reply_timeout: Duration,
    ) -> Result<Self, ZsshError> {
        // 先拆开：读半边用来等回复，写半边放进守卫。不管后面是失败、超时还是外层直接把
        // 这个 future 丢掉（用户关窗格），没交给 ShellSession 的通道都会被关掉。
        let (mut read_half, write_half) = channel.split();
        let guard = CloseOnDrop::new(write_half);
        let pending = match request_pty_and_shell(
            guard.write_half()?,
            &mut read_half,
            cols,
            rows,
            reply_timeout,
            buffer_limit.max(1),
        )
        .await
        {
            Ok(pending) => pending,
            Err(err) => {
                // 通道已经开出来了：不关的话，复用的连接上会一直占着一个会话名额
                // （服务器的 MaxSessions 有上限）。
                guard.close_now().await;
                return Err(err);
            }
        };
        let write_half = guard.disarm()?;
        let (events_tx, events) = mpsc::channel(EVENT_BUFFER);
        let gate = Arc::new(FlowGate::new());
        let buffer = Arc::new(SessionBuffer::new(buffer_limit));
        // 等回复期间先到的数据（以及 PTY 被拒的提示）排在读任务读到的数据前面。
        for item in pending {
            buffer.push_item(item);
        }

        // 两级缓冲（设计文档 4.1）：读任务只管把 russh 读进本地缓冲，转发任务把
        // 缓冲里的数据攒批发给调用方、受流控门控制。一个终端不 ack 只会顶满它
        // 自己的缓冲，不会卡住整条连接的事件循环——除非缓冲也满了。
        let reader = tokio::spawn(read_channel(read_half, Arc::clone(&buffer)));
        let forwarder = tokio::spawn(forward_events(
            Arc::clone(&buffer),
            events_tx,
            Arc::clone(&gate),
            ring,
        ));

        Ok(Self {
            writer: ShellWriter {
                write_half,
                gate: Arc::clone(&gate),
            },
            events: ShellEventStream {
                events,
                reader,
                forwarder,
                gate,
                buffer,
            },
        })
    }

    /// 把按键字节写给远端。
    pub async fn write(&mut self, data: &[u8]) -> Result<(), ZsshError> {
        self.writer.write(data).await
    }

    /// 通知远端终端尺寸变了。
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<(), ZsshError> {
        self.writer.resize(cols, rows).await
    }

    /// 关闭通道，远端进程会收到 EOF。
    pub async fn close(&mut self) -> Result<(), ZsshError> {
        self.writer.close().await
    }

    /// 把前端的确认转给流控门。
    pub fn ack(&self, bytes: u32) {
        self.writer.ack(bytes);
    }

    /// 流控门，压测和排查时用来读未确认字节数。
    pub fn flow(&self) -> &FlowGate {
        self.writer.flow()
    }

    /// 拿一份流控门的句柄。
    ///
    /// 设计文档 5.2 的硬规则：ack 和关闭不能排在会阻塞的命令队列后面。调用方
    /// 拿着这个句柄就能绕过会话任务，直接 [`FlowGate::on_ack`] / [`FlowGate::close`]。
    pub fn flow_handle(&self) -> Arc<FlowGate> {
        self.writer.flow_handle()
    }

    /// 本地缓冲里还没转发的字节数。
    pub fn buffered_bytes(&self) -> usize {
        self.events.buffered_bytes()
    }

    /// 本地缓冲的句柄（压测用：看它满没满）。
    pub fn buffer_handle(&self) -> Arc<SessionBuffer> {
        self.events.buffer_handle()
    }

    /// 取下一个事件，通道结束时返回 `None`。
    pub async fn next_event(&mut self) -> Option<ShellEvent> {
        self.events.next().await
    }

    /// 拆成写半边和事件流，分别交给不同任务。
    pub fn split(self) -> (ShellWriter, ShellEventStream) {
        (self.writer, self.events)
    }
}

/// shell 的写半边。
pub struct ShellWriter {
    write_half: ChannelWriteHalf<Msg>,
    gate: Arc<FlowGate>,
}

/// 写、改窗口、关闭都只要 `&self`（russh 的写半边本身就是共享引用可用的）：会话任务据此把
/// "正在进行的写入"和关闭信号、读事件放在同一个 `select!` 里——远端不再补充窗口时写入会
/// 一直等，关闭必须能抢占它（审计 C16）。
impl ShellWriter {
    pub async fn write(&self, data: &[u8]) -> Result<(), ZsshError> {
        self.write_half
            .data_bytes(data.to_vec())
            .await
            .map_err(|err| ZsshError::Shell(format!("写入 shell 失败：{err}")))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), ZsshError> {
        self.write_half
            .window_change(u32::from(cols), u32::from(rows), 0, 0)
            .await
            .map_err(|err| ZsshError::Shell(format!("调整窗口大小失败：{err}")))
    }

    pub async fn close(&self) -> Result<(), ZsshError> {
        self.write_half
            .eof()
            .await
            .map_err(|err| ZsshError::Shell(format!("发送 EOF 失败：{err}")))?;
        self.write_half
            .close()
            .await
            .map_err(|err| ZsshError::Shell(format!("关闭 shell 失败：{err}")))
    }

    /// 把前端的确认转给流控门。
    pub fn ack(&self, bytes: u32) {
        self.gate.on_ack(bytes as usize);
    }

    pub fn flow(&self) -> &FlowGate {
        &self.gate
    }

    /// 拿一份流控门的句柄，见 [`ShellSession::flow_handle`]。
    pub fn flow_handle(&self) -> Arc<FlowGate> {
        Arc::clone(&self.gate)
    }
}

/// shell 的输出事件流。
pub struct ShellEventStream {
    events: mpsc::Receiver<ShellEvent>,
    reader: JoinHandle<()>,
    forwarder: JoinHandle<()>,
    gate: Arc<FlowGate>,
    buffer: Arc<SessionBuffer>,
}

impl ShellEventStream {
    pub async fn next(&mut self) -> Option<ShellEvent> {
        self.events.recv().await
    }

    /// 本地缓冲里还没转发的字节数（压测用）。
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.buffered_bytes()
    }

    /// 本地缓冲的句柄（压测用：看它满没满）。
    pub fn buffer_handle(&self) -> Arc<SessionBuffer> {
        Arc::clone(&self.buffer)
    }
}

impl Drop for ShellEventStream {
    fn drop(&mut self) {
        // 丢弃事件流说明调用方不再读输出：先关门让转发任务退出、丢弃缓冲让读任务
        // 立刻返回并收工，再掐掉两个任务兜底，保证不留挂住的任务。
        self.gate.close();
        self.buffer.discard();
        self.reader.abort();
        self.forwarder.abort();
    }
}

/// 本地缓冲里的一个条目。数据和退出事件必须在同一个队列里保序。
enum Item {
    Data(Vec<u8>),
    /// `None` = 收到了退出信号但没有退出码
    Exit(Option<u32>),
}

#[derive(Default)]
struct BufferState {
    items: VecDeque<Item>,
    /// 只统计 [`Item::Data`] 的字节数
    bytes: usize,
    reader_done: bool,
    discarded: bool,
    /// 读任务现在是不是卡在"缓冲满了、等腾地方"上（压测排查用）。
    /// false = 正在从 russh 读（或者等着数据到），true = 停读了。
    reader_stopped: bool,
}

/// 通道自己的本地缓冲（设计文档 4.1 的第一级）。
///
/// - 读任务只受它限制：没满就一直从 russh 读，别的通道不受影响
/// - 转发任务从这儿取数据，受 [`FlowGate`] 限制：未确认超过高水位就停发
/// - 只有它满了，读任务才停止读 russh，这时才会卡住整条连接——正常刷屏摸不到
pub struct SessionBuffer {
    state: Mutex<BufferState>,
    limit: usize,
    /// 一个通知走天下：推数据、腾空间、读结束、丢弃都 notify，等待方醒来自己
    /// 重查条件。条件都很便宜，没必要拆成两个。
    notify: Notify,
}

impl SessionBuffer {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(BufferState::default()),
            limit: limit.max(1),
            notify: Notify::new(),
        }
    }

    /// 缓冲上限。
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// 现在堆了多少字节。
    pub fn buffered_bytes(&self) -> usize {
        self.lock().bytes
    }

    /// 缓冲是不是满了（读任务在这时候会停下来）。
    pub fn is_full(&self) -> bool {
        self.buffered_bytes() >= self.limit
    }

    /// 读任务是不是已经结束了（对端关了通道或者缓冲被丢弃）。
    pub fn is_reader_done(&self) -> bool {
        self.reader_done()
    }

    /// 读任务是不是停读了（本地缓冲满了）。压测时用来看"卡在哪儿"。
    pub fn is_reader_stopped(&self) -> bool {
        self.lock().reader_stopped
    }

    fn lock(&self) -> MutexGuard<'_, BufferState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_item(&self, item: Item) {
        {
            let mut state = self.lock();
            if state.discarded {
                return;
            }
            if let Item::Data(data) = &item {
                state.bytes = state.bytes.saturating_add(data.len());
            }
            state.items.push_back(item);
        }
        self.notify.notify_waiters();
    }

    /// 立刻取一个条目，没有就返回 `None`，不等待。
    fn pop_now(&self) -> Option<Item> {
        let item = {
            let mut state = self.lock();
            let item = state.items.pop_front()?;
            if let Item::Data(data) = &item {
                state.bytes = state.bytes.saturating_sub(data.len());
            }
            item
        };
        // 腾出了空间，唤醒等空间的读任务。
        self.notify.notify_waiters();
        Some(item)
    }

    /// 读任务每次读之前问：还能不能读。返回 `false` 表示缓冲被丢弃，该收工了。
    ///
    /// 停了之后要降到上限的一半才恢复，见 [`resume_at`]。
    async fn wait_space(&self) -> bool {
        loop {
            // 先把通知注册上再看状态，否则"读完状态到开始等"之间会丢掉唤醒。
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();

            {
                let mut state = self.lock();
                if state.discarded {
                    return false;
                }
                let threshold = if state.bytes >= self.limit {
                    resume_at(self.limit)
                } else {
                    self.limit
                };
                if state.bytes < threshold {
                    state.reader_stopped = false;
                    return true;
                }
                state.reader_stopped = true;
            }

            notified.await;
        }
    }

    /// 转发任务等新数据：有数据、读结束或者被丢弃时返回。
    async fn wait_data(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();

            {
                let state = self.lock();
                if !state.items.is_empty() || state.reader_done || state.discarded {
                    return;
                }
            }

            notified.await;
        }
    }

    fn finish_reader(&self) {
        self.lock().reader_done = true;
        self.notify.notify_waiters();
    }

    fn reader_done(&self) -> bool {
        self.lock().reader_done
    }

    fn is_empty(&self) -> bool {
        self.lock().items.is_empty()
    }

    fn discarded(&self) -> bool {
        self.lock().discarded
    }

    /// 丢弃缓冲：读任务和转发任务都会醒过来收工。
    fn discard(&self) {
        {
            let mut state = self.lock();
            state.discarded = true;
            state.items.clear();
            state.bytes = 0;
        }
        self.notify.notify_waiters();
    }
}

/// 读通道的循环：只管把 russh 的数据搬进本地缓冲（设计文档 4.1 的第一级）。
///
/// 每次读之前先问缓冲还有没有空间；缓冲满了就停在这里，russh 的有界队列被塞满，
/// 整条连接的事件循环才会停下来（设计文档 5.2）。
async fn read_channel(mut read_half: ChannelReadHalf, buffer: Arc<SessionBuffer>) {
    loop {
        if !buffer.wait_space().await {
            return;
        }

        match read_half.wait().await {
            // stderr 走扩展数据，终端里按普通输出显示。
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                buffer.push_item(Item::Data(data.to_vec()));
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                buffer.push_item(Item::Exit(Some(exit_status)));
            }
            Some(ChannelMsg::ExitSignal { .. }) => {
                buffer.push_item(Item::Exit(None));
            }
            Some(ChannelMsg::Close) => break,
            Some(_) => {}
            None => break,
        }
    }

    buffer.finish_reader();
}

/// 转发循环：从本地缓冲取数据，攒批后发给调用方；发送受流控门控制。
///
/// 攒批规则见 [`BATCH_MAX_BYTES`] 和 [`BATCH_MAX_DELAY`]；Exit / Closed 之前都先把
/// 缓冲里剩下的数据发掉，保证事件顺序和输出顺序一致。
///
/// `ring` 给 `Some` 时，每个数据块在进批次之前先旁路拷一份进去。锁只在这几行
/// 代码里短暂持有，不参与 ack / 关闭路径，不会把转发或流控卡住（设计文档 5.2）。
async fn forward_events(
    buffer: Arc<SessionBuffer>,
    events: mpsc::Sender<ShellEvent>,
    gate: Arc<FlowGate>,
    ring: Option<SharedTerminalRing>,
) {
    let mut batch: Vec<u8> = Vec::new();
    // 缓冲里第一个字节进来后开始算的截止时间；None 表示手里没有没发完的批次。
    let mut flush_at: Option<Instant> = None;
    // 取到的退出事件，等前面的数据发完再发它
    let mut exited: Option<Option<u32>> = None;

    'outer: loop {
        // 1. 把缓冲里现成的条目都搬出来：数据进批次，退出事件记下来
        while exited.is_none() {
            match buffer.pop_now() {
                Some(Item::Data(chunk)) => {
                    if let Some(ring) = &ring {
                        let mut ring = ring.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        ring.push(&chunk);
                    }
                    if flush_at.is_none() {
                        flush_at = Some(Instant::now() + BATCH_MAX_DELAY);
                    }
                    batch.extend_from_slice(&chunk);
                    if batch.len() >= BATCH_MAX_BYTES {
                        break;
                    }
                }
                Some(Item::Exit(status)) => {
                    exited = Some(status);
                    break;
                }
                None => break,
            }
        }

        // 2. 该发就发。发送前先问流控门：暂停时停在这里，缓冲继续涨，涨满了
        //    读任务才会停——这就是设计文档 4.1 的两级缓冲。
        let due = matches!(flush_at, Some(at) if at <= Instant::now());
        if !batch.is_empty()
            && (due || batch.len() >= BATCH_MAX_BYTES || exited.is_some() || buffer.reader_done())
            && !send_data(&events, &gate, &mut batch, &mut flush_at).await
        {
            break 'outer;
        }

        // 3. 退出事件紧跟它前面的数据
        if let Some(status) = exited.take() {
            if events.send(ShellEvent::Exit(status)).await.is_err() {
                break 'outer;
            }
        }

        // 4. 读结束、缓冲清空、手里也没有没发完的东西：收工
        if buffer.discarded()
            || (buffer.reader_done() && buffer.is_empty() && batch.is_empty() && exited.is_none())
        {
            break;
        }

        // 5. 等下一批：手里有没发完的批次就等到截止时间，否则等缓冲来数据
        match flush_at {
            Some(at) => {
                let _ = timeout_at(at, buffer.wait_data()).await;
            }
            None => buffer.wait_data().await,
        }
    }

    // 收尾：把手里剩的尽量发掉（门已经关了就算了），然后报关闭。
    let _ = send_data(&events, &gate, &mut batch, &mut flush_at).await;
    let _ = events.send(ShellEvent::Closed).await;
}

/// 把批次发给调用方并计入未确认字节。返回 `false` 表示流控门已关或者接收端没了。
async fn send_data(
    events: &mpsc::Sender<ShellEvent>,
    gate: &FlowGate,
    batch: &mut Vec<u8>,
    flush_at: &mut Option<Instant>,
) -> bool {
    if batch.is_empty() {
        return true;
    }
    if !gate.wait_ready().await {
        return false;
    }

    let bytes = std::mem::take(batch);
    // 先记账再发：ack 有可能在发送返回之前就回来，反过来记会让计数对不上。
    gate.on_sent(bytes.len());
    // 换一个新缓冲，下次攒批不用自己长。
    batch.reserve(BATCH_MAX_BYTES);
    *flush_at = None;

    events.send(ShellEvent::Data(bytes)).await.is_ok()
}

#[cfg(test)]
mod open_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::terminal::TerminalRing;

    #[tokio::test]
    async fn forward_events_tees_output_into_the_ring() {
        let buffer = Arc::new(SessionBuffer::new(1024 * 1024));
        buffer.push_item(Item::Data(b"first line\n".to_vec()));
        buffer.push_item(Item::Data(b"second line\n".to_vec()));
        buffer.finish_reader();

        let ring = Arc::new(Mutex::new(TerminalRing::new()));
        let (tx, mut rx) = mpsc::channel(EVENT_BUFFER);
        let gate = Arc::new(FlowGate::new());
        let forwarder = tokio::spawn(forward_events(
            Arc::clone(&buffer),
            tx,
            Arc::clone(&gate),
            Some(Arc::clone(&ring)),
        ));

        let mut delivered = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                ShellEvent::Data(data) => delivered.extend_from_slice(&data),
                ShellEvent::Exit(_) => {}
                ShellEvent::Closed => break,
            }
        }
        forwarder.await.expect("转发任务不该 panic");

        assert_eq!(delivered, b"first line\nsecond line\n");
        let lines = ring.lock().unwrap().recent_lines(10);
        assert_eq!(lines, vec!["first line", "second line"]);
    }

    #[tokio::test]
    async fn forward_events_without_a_ring_still_delivers() {
        let buffer = Arc::new(SessionBuffer::new(1024 * 1024));
        buffer.push_item(Item::Data(b"no ring\n".to_vec()));
        buffer.finish_reader();

        let (tx, mut rx) = mpsc::channel(EVENT_BUFFER);
        let gate = Arc::new(FlowGate::new());
        let forwarder = tokio::spawn(forward_events(Arc::clone(&buffer), tx, gate, None));

        let mut delivered = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                ShellEvent::Data(data) => delivered.extend_from_slice(&data),
                ShellEvent::Exit(_) => {}
                ShellEvent::Closed => break,
            }
        }
        forwarder.await.expect("转发任务不该 panic");
        assert_eq!(delivered, b"no ring\n");
    }
}
