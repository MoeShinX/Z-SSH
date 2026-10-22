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
}

impl Script {
    fn new(pty: Reply, shell: Reply) -> Self {
        Self {
            pty,
            shell,
            before_pty_reply: Vec::new(),
            before_shell_reply_stderr: Vec::new(),
            after_shell: Vec::new(),
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
    ShellSession::open_with_reply_timeout(
        channel,
        80,
        24,
        DEFAULT_BUFFER_LIMIT,
        None,
        reply_timeout,
    )
    .await
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
