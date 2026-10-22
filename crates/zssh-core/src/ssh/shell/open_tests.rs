//! 开终端时等服务器回复 PTY / shell 请求（终端审查 T05）。
//!
//! 进程内假 SSH 服务器：只监听 127.0.0.1、端口由系统分配，主机密钥现场生成。服务器对
//! PTY / shell 请求的反应可配置（接受 / 拒绝 / 不回 / 直接关通道），还能在回复之前先发数据。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::keys::{Algorithm, PrivateKey, PublicKeyOrCertificate};
use russh::server::{self, Auth, Msg as ServerMsg, Session};
use russh::{client, Channel, ChannelId};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::*;

/// 假服务器对一个请求的反应。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reply {
    Success,
    Failure,
    /// 收到了但不回复。
    Silent,
    /// 不回复，直接关通道。
    Close,
}

#[derive(Clone)]
struct Script {
    pty: Reply,
    shell: Reply,
    /// 回复 PTY 请求之前先发的数据（stdout）。
    before_pty_reply: Vec<u8>,
    /// 回复 shell 请求之前先发的数据（stderr，走扩展数据）。
    before_shell_reply_stderr: Vec<u8>,
    /// shell 请求成功之后发的数据。
    after_shell: Vec<u8>,
    /// 前几个 PTY 请求按 [`Reply::Silent`] 处理，之后才按 `pty` 的设置回复
    /// （同一条连接上第一个通道不回复、第二个通道正常的场景用）。
    silent_pty_requests: usize,
}

impl Script {
    fn new(pty: Reply, shell: Reply) -> Self {
        Self {
            pty,
            shell,
            before_pty_reply: Vec::new(),
            before_shell_reply_stderr: Vec::new(),
            after_shell: Vec::new(),
            silent_pty_requests: 0,
        }
    }
}

struct FakeServer {
    script: Script,
    /// 客户端关了通道就往这里报一声。
    closed: mpsc::UnboundedSender<()>,
}

fn respond(reply: Reply, channel: ChannelId, session: &mut Session) -> Result<(), russh::Error> {
    match reply {
        Reply::Success => session.channel_success(channel),
        Reply::Failure => session.channel_failure(channel),
        Reply::Silent => Ok(()),
        Reply::Close => session.close(channel),
    }
}

impl server::Handler for FakeServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<ServerMsg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.script.before_pty_reply.is_empty() {
            session.data(channel, self.script.before_pty_reply.clone())?;
        }
        if self.script.silent_pty_requests > 0 {
            self.script.silent_pty_requests -= 1;
            return respond(Reply::Silent, channel, session);
        }
        respond(self.script.pty, channel, session)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.script.before_shell_reply_stderr.is_empty() {
            session.extended_data(channel, 1, self.script.before_shell_reply_stderr.clone())?;
        }
        respond(self.script.shell, channel, session)?;
        if self.script.shell == Reply::Success && !self.script.after_shell.is_empty() {
            session.data(channel, self.script.after_shell.clone())?;
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = self.closed.send(());
        Ok(())
    }
}

struct AcceptAnyHostKey;

impl client::Handler for AcceptAnyHostKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 起假服务器，返回地址和"客户端关通道"的通知。只接一个连接。
async fn spawn_server(script: Script) -> (SocketAddr, mpsc::UnboundedReceiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本机端口");
    let addr = listener.local_addr().expect("取端口");
    let config = Arc::new(server::Config {
        inactivity_timeout: None,
        auth_rejection_time: Duration::from_millis(1),
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("生成主机密钥")],
        ..Default::default()
    });
    let (closed, closed_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let handler = FakeServer { script, closed };
        if let Ok(running) = server::run_stream(config, socket, handler).await {
            let _ = running.await;
        }
    });
    (addr, closed_rx)
}

/// 连上假服务器并开一个会话通道。`Handle` 要一直拿着，丢了连接就断。
async fn connect_and_open_channel(
    addr: SocketAddr,
) -> (client::Handle<AcceptAnyHostKey>, Channel<client::Msg>) {
    let mut handle = client::connect(Arc::new(client::Config::default()), addr, AcceptAnyHostKey)
        .await
        .expect("连接假服务器");
    let auth = handle.authenticate_none("tester").await.expect("认证请求");
    assert!(auth.success(), "假服务器应当接受 none 认证");
    let channel = handle.channel_open_session().await.expect("开会话通道");
    (handle, channel)
}

/// 收输出，直到攒够 `want` 字节、通道关闭或者 5 秒超时。
async fn collect_output(shell: &mut ShellSession, want: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while out.len() < want {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, shell.next_event()).await {
            Ok(Some(ShellEvent::Data(data))) => out.extend_from_slice(&data),
            Ok(Some(ShellEvent::Exit(_))) => {}
            Ok(Some(ShellEvent::Closed)) | Ok(None) | Err(_) => break,
        }
    }
    out
}

fn notice_bytes() -> Vec<u8> {
    PTY_DENIED_NOTICE.as_bytes().to_vec()
}

async fn open(
    channel: Channel<client::Msg>,
    reply_timeout: Duration,
) -> Result<ShellSession, ZsshError> {
    open_with_limit(channel, reply_timeout, DEFAULT_BUFFER_LIMIT).await
}

async fn open_with_limit(
    channel: Channel<client::Msg>,
    reply_timeout: Duration,
    buffer_limit: usize,
) -> Result<ShellSession, ZsshError> {
    ShellSession::open_with_reply_timeout(channel, 80, 24, buffer_limit, None, reply_timeout).await
}

fn error_text(result: Result<ShellSession, ZsshError>) -> String {
    match result {
        Ok(_) => panic!("预期打开失败，却成功了"),
        Err(err) => err.to_string(),
    }
}

#[tokio::test]
async fn opens_when_pty_and_shell_are_accepted() {
    let mut script = Script::new(Reply::Success, Reply::Success);
    script.after_shell = b"hello\r\n".to_vec();
    let (addr, _closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let mut shell = open(channel, Duration::from_secs(5))
        .await
        .expect("正常打开");
    let out = collect_output(&mut shell, 7).await;
    assert_eq!(out, b"hello\r\n", "PTY 被接受时不应出现提示");
}

#[tokio::test]
async fn pty_rejected_continues_without_pty_and_shows_notice_first() {
    let mut script = Script::new(Reply::Failure, Reply::Success);
    script.before_pty_reply = b"early\r\n".to_vec();
    script.after_shell = b"motd\r\n".to_vec();
    let (addr, _closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let mut shell = open(channel, Duration::from_secs(5))
        .await
        .expect("PTY 被拒不该判失败");
    let mut expected = notice_bytes();
    expected.extend_from_slice(b"early\r\nmotd\r\n");
    let out = collect_output(&mut shell, expected.len()).await;
    // 提示在最前面，服务器的数据一个字节不少、顺序不变。
    assert_eq!(
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&expected)
    );
    assert!(String::from_utf8_lossy(&out).contains("服务器拒绝分配终端（PTY）"));
}

#[tokio::test]
async fn shell_rejected_is_an_error_and_closes_the_channel() {
    let (addr, mut closed) = spawn_server(Script::new(Reply::Success, Reply::Failure)).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let text = error_text(open(channel, Duration::from_secs(5)).await);
    assert!(text.contains("服务器拒绝打开 shell"), "实际：{text}");
    // 打开失败后通道要关掉，不能占着连接上的会话名额。
    let seen = timeout(Duration::from_secs(5), closed.recv()).await;
    assert!(matches!(seen, Ok(Some(()))), "服务器应当看到通道被关闭");
}

#[tokio::test]
async fn silent_server_times_out_on_pty_reply() {
    let (addr, _closed) = spawn_server(Script::new(Reply::Silent, Reply::Success)).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let started = Instant::now();
    let text = error_text(open(channel, Duration::from_millis(300)).await);
    assert!(
        text.contains("PTY") && text.contains("超时"),
        "实际：{text}"
    );
    assert!(started.elapsed() < Duration::from_secs(5), "不该真等 20 秒");
}

#[tokio::test]
async fn silent_server_times_out_on_shell_reply() {
    let (addr, _closed) = spawn_server(Script::new(Reply::Success, Reply::Silent)).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let started = Instant::now();
    let text = error_text(open(channel, Duration::from_millis(300)).await);
    assert!(
        text.contains("shell") && text.contains("超时"),
        "实际：{text}"
    );
    assert!(started.elapsed() < Duration::from_secs(5), "不该真等 20 秒");
}

#[tokio::test]
async fn data_before_replies_is_kept_in_order() {
    let mut script = Script::new(Reply::Success, Reply::Success);
    script.before_pty_reply = b"AAA".to_vec();
    script.before_shell_reply_stderr = b"BBB".to_vec();
    script.after_shell = b"CCC".to_vec();
    let (addr, _closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let mut shell = open(channel, Duration::from_secs(5))
        .await
        .expect("正常打开");
    let out = collect_output(&mut shell, 9).await;
    assert_eq!(out, b"AAABBBCCC");
}

#[tokio::test]
async fn channel_closed_while_waiting_for_pty_reply() {
    let (addr, _closed) = spawn_server(Script::new(Reply::Close, Reply::Success)).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let text = error_text(open(channel, Duration::from_secs(5)).await);
    assert!(
        text.contains("服务器在打开终端时关闭了连接"),
        "实际：{text}"
    );
}

#[tokio::test]
async fn channel_closed_while_waiting_for_shell_reply() {
    let (addr, _closed) = spawn_server(Script::new(Reply::Success, Reply::Close)).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let text = error_text(open(channel, Duration::from_secs(5)).await);
    assert!(
        text.contains("服务器在打开终端时关闭了连接"),
        "实际：{text}"
    );
}

// ---- TERM-3：取消时关通道 ----

/// 等回复期间把打开任务中止（相当于用户关掉窗格，外层丢弃了正在等的 future），
/// 服务器要在短时间内看到这个通道被关。
async fn assert_cancel_closes_channel(script: Script) {
    let (addr, mut closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let task = tokio::spawn(open(channel, Duration::from_secs(30)));
    // 留够时间让请求发出去、打开任务停在等回复上。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "服务器不回复，打开应当还在等");
    task.abort();

    let seen = timeout(Duration::from_secs(2), closed.recv()).await;
    assert!(
        matches!(seen, Ok(Some(()))),
        "取消后服务器应当在 2 秒内看到通道被关闭"
    );
}

#[tokio::test]
async fn cancel_while_waiting_for_pty_reply_closes_the_channel() {
    assert_cancel_closes_channel(Script::new(Reply::Silent, Reply::Success)).await;
}

#[tokio::test]
async fn cancel_while_waiting_for_shell_reply_closes_the_channel() {
    assert_cancel_closes_channel(Script::new(Reply::Success, Reply::Silent)).await;
}

#[tokio::test]
async fn cancelled_channel_is_closed_and_the_connection_stays_usable() {
    // 第一个通道的 PTY 请求服务器不回复，第二个通道正常。
    let mut script = Script::new(Reply::Success, Reply::Success);
    script.silent_pty_requests = 1;
    script.after_shell = b"second
"
    .to_vec();
    let (addr, mut closed) = spawn_server(script).await;
    let (handle, first) = connect_and_open_channel(addr).await;

    let task = tokio::spawn(open(first, Duration::from_secs(30)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    task.abort();
    let seen = timeout(Duration::from_secs(2), closed.recv()).await;
    assert!(
        matches!(seen, Ok(Some(()))),
        "被取消的通道应当在 2 秒内被关闭"
    );

    // 同一条连接上再开一个通道，照常能用。
    let second = handle.channel_open_session().await.expect("连接应仍可用");
    let mut shell = open(second, Duration::from_secs(5))
        .await
        .expect("第二个通道正常打开");
    let out = collect_output(&mut shell, 8).await;
    assert_eq!(
        out,
        b"second
"
    );
    // 成功打开后守卫已解除：不能再有多余的关闭。
    assert!(
        timeout(Duration::from_millis(300), closed.recv())
            .await
            .is_err(),
        "成功打开的通道不该被守卫关掉"
    );
    shell.write(b"x").await.expect("打开后的通道还能写");
}

// ---- TERM-3：等回复期间的输出上限 ----

#[tokio::test]
async fn too_much_data_before_the_reply_fails_and_closes_the_channel() {
    let mut script = Script::new(Reply::Success, Reply::Success);
    script.before_pty_reply = vec![b'x'; 4096];
    let (addr, mut closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let text = error_text(open_with_limit(channel, Duration::from_secs(5), 1024).await);
    assert!(
        text.contains("服务器在回复前发送了过多数据"),
        "实际：{text}"
    );
    let seen = timeout(Duration::from_secs(5), closed.recv()).await;
    assert!(matches!(seen, Ok(Some(()))), "超限后通道应当被关闭");
}

#[tokio::test]
async fn the_cap_counts_data_across_both_replies_and_stderr() {
    // 两段各 700 字节、单段都不到上限，合计超过 1024。
    let mut script = Script::new(Reply::Success, Reply::Success);
    script.before_pty_reply = vec![b'a'; 700];
    script.before_shell_reply_stderr = vec![b'b'; 700];
    let (addr, _closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let text = error_text(open_with_limit(channel, Duration::from_secs(5), 1024).await);
    assert!(
        text.contains("服务器在回复前发送了过多数据"),
        "实际：{text}"
    );
}

#[tokio::test]
async fn data_exactly_at_the_cap_is_kept_in_full_and_in_order() {
    let mut script = Script::new(Reply::Success, Reply::Success);
    let mut early = Vec::new();
    // 1024 字节：PTY 回复前 600 + shell 回复前（stderr）424，刚好等于上限。
    early.extend((0..600u32).map(|i| (i % 251) as u8));
    script.before_pty_reply = early.clone();
    let tail: Vec<u8> = (0..424u32).map(|i| (i % 241) as u8).collect();
    script.before_shell_reply_stderr = tail.clone();
    script.after_shell = b"END".to_vec();
    let (addr, _closed) = spawn_server(script).await;
    let (_handle, channel) = connect_and_open_channel(addr).await;

    let mut shell = open_with_limit(channel, Duration::from_secs(5), 1024)
        .await
        .expect("刚好不超过上限应当成功");
    let mut expected = early;
    expected.extend_from_slice(&tail);
    expected.extend_from_slice(b"END");
    let out = collect_output(&mut shell, expected.len()).await;
    assert_eq!(out, expected, "数据一个字节不丢、顺序不变");
}
