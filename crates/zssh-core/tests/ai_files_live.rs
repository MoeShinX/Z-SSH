//! 文件工具实测（M3-4）：真实 SSH 服务器 + 工具桥（TCP 行协议，和 zssh-mcp 转发的一样）。
//!
//! 四个工具直接经过工具桥调一遍；`write_file` 覆盖时生成了备份且内容正确；手动模式下拒绝时
//! 文件没有被改；写系统路径时在确认那一步拒绝，**不会真的写进去**；作用范围外的目标被拒；
//! 超过 64KB 的文件分段读回来和 SHA-256 对得上；二进制文件只报"这是二进制"。
//!
//! 服务器上只用专用临时目录（`RemoteTempDir`：正常路径清理，失败 / panic 时 Drop 兜底删），
//! 系统路径那一条只在确认卡片上拒绝，不碰服务器。输出里不出现地址：打印前一律过 `sanitize`。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use zssh_core::ai::bridge::{
    ApprovalDecision, ApprovalRequest, Approver, Bridge, BridgeHost, ExecMode, ExecModeProvider,
};
use zssh_core::ai::files::{RemoteFs, SftpRemoteFs};
use zssh_core::ai::protocol::{RequestEnvelope, ResponseEnvelope, TokenLine};
use zssh_core::ai::safety::DangerLevel;
use zssh_core::sftp::SftpChannel;
use zssh_core::ssh::{BoxFuture, Connection, ExecCapture};
use zssh_core::Database;

use common::{serial, RemoteTempDir, TempKnownHosts, TestServer};

const TARGET: &str = "files-1";

/// 宿主：每次工具调用在连接上开一条自己的 SFTP 通道（和 `src-tauri` 里一样）。
struct LiveFilesHost {
    connection: Arc<Connection>,
    opened: AtomicUsize,
}

impl BridgeHost for LiveFilesHost {
    fn exec<'a>(
        &'a self,
        _session_id: &'a str,
        _command: &'a str,
        _wait: Duration,
        _max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async { Err("文件工具测试不执行命令".to_owned()) })
    }

    fn read_terminal<'a>(
        &'a self,
        _session_id: &'a str,
        _max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remote_fs<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        let known = target_id == TARGET;
        Box::pin(async move {
            if !known {
                return Err("测试宿主只认识一个目标".to_owned());
            }
            self.opened.fetch_add(1, Ordering::SeqCst);
            let channel = SftpChannel::open(&self.connection)
                .await
                .map_err(|err| err.to_string())?;
            let fs: Arc<dyn RemoteFs> = Arc::new(SftpRemoteFs::new(Arc::new(channel)));
            Ok(fs)
        })
    }
}

struct ScriptedApprover {
    allow: AtomicBool,
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl Approver for ScriptedApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.requests.lock().unwrap().push(request);
        let allow = self.allow.load(Ordering::SeqCst);
        Box::pin(async move {
            if allow {
                ApprovalDecision::Allowed
            } else {
                ApprovalDecision::Denied
            }
        })
    }
}

struct SwitchableMode(AtomicBool);

impl ExecModeProvider for SwitchableMode {
    fn mode(&self) -> ExecMode {
        if self.0.load(Ordering::SeqCst) {
            ExecMode::Auto
        } else {
            ExecMode::Manual
        }
    }
}

/// 工具桥的行协议客户端（zssh-mcp 做的就是这件事）。
struct BridgeLine {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    capability: String,
    next_id: u64,
}

impl BridgeLine {
    async fn connect(port: u16, token: &str, capability: &str) -> BridgeLine {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("连工具桥");
        let (read, mut write) = stream.into_split();
        let mut hello = serde_json::to_string(&TokenLine {
            token: token.to_owned(),
        })
        .unwrap();
        hello.push('\n');
        write.write_all(hello.as_bytes()).await.expect("握手");
        BridgeLine {
            reader: BufReader::new(read),
            writer: write,
            capability: capability.to_owned(),
            next_id: 1,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&RequestEnvelope {
            id,
            method: method.to_owned(),
            capability: self.capability.clone(),
            params,
        })
        .unwrap();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("发请求");
        let mut reply = String::new();
        self.reader.read_line(&mut reply).await.expect("读响应");
        let response: ResponseEnvelope = serde_json::from_str(reply.trim()).expect("响应格式");
        match response.error {
            Some(error) => Err(error.message),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 用测试自己的通道读远端文件（独立于被测的工具路径）。
async fn remote_bytes(channel: &SftpChannel, path: &str) -> Option<Vec<u8>> {
    zssh_core::sftp::fs::read_bytes(channel, path, 16 * 1024 * 1024)
        .await
        .ok()
}

async fn names_in(channel: &SftpChannel, dir: &str) -> Vec<String> {
    channel
        .read_dir(dir)
        .await
        .map(|entries| entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn file_tools_through_the_bridge_on_a_real_server() {
    if !common::full_live("file_tools_through_the_bridge_on_a_real_server") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 file_tools_through_the_bridge_on_a_real_server：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("ai-files");
    let connection = Arc::new(
        server
            .connect(&known_hosts)
            .await
            .expect("连接测试服务器失败"),
    );
    let channel = common::open_sftp(&connection).await.expect("开测试通道");
    let dir = RemoteTempDir::create(&server, &connection, &channel, "aifiles").await;
    let base = dir.path().to_owned();

    // 桥：手动 / 自动可切，审批可脚本化，审计写内存库。
    let database = Arc::new(Database::open_in_memory().expect("审计库"));
    let approver = Arc::new(ScriptedApprover {
        allow: AtomicBool::new(true),
        requests: Mutex::new(Vec::new()),
    });
    let mode = Arc::new(SwitchableMode(AtomicBool::new(false)));
    let host = Arc::new(LiveFilesHost {
        connection: Arc::clone(&connection),
        opened: AtomicUsize::new(0),
    });
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        database.clone(),
        approver.clone(),
        mode.clone(),
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("绑端口");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Arc::clone(&bridge).serve(listener));
    let scope = bridge.create_scope();
    scope.add_target(TARGET, "测试服务器", "root", "connected", true, false);
    let mut line = BridgeLine::connect(port, bridge.connection_token(), scope.capability()).await;

    // ---- 1. 新建文件（手动模式，确认通过）：卡片上是全文，不留备份 ----
    let hello = format!("{base}/hello.txt");
    let created = line
        .call(
            "write_file",
            json!({ "path": hello, "content": "line one\nline two\nline three\n", "create_only": true }),
        )
        .await
        .unwrap_or_else(|e| panic!("新建失败：{}", server.sanitize(&e)));
    assert_eq!(created["created"], true);
    assert!(created.get("backup_path").is_none());
    assert_eq!(
        remote_bytes(&channel, &hello).await.unwrap(),
        b"line one\nline two\nline three\n"
    );
    {
        let asked = approver.requests.lock().unwrap().clone();
        let file = asked[0].file.clone().expect("确认带文件细节");
        assert!(file.is_new);
        assert_eq!(file.diff.added, 3);
        assert!(asked[0].danger == DangerLevel::Safe);
    }
    println!("新建文件：通过（手动模式确认 1 次，全文 3 行）");

    // ---- 2. stat / list_dir / read_file ----
    let stat = line.call("stat", json!({ "path": hello })).await.unwrap();
    assert_eq!(
        (stat["exists"].as_bool(), stat["type"].as_str()),
        (Some(true), Some("file"))
    );
    assert_eq!(stat["size"], 29);
    let missing = line
        .call("stat", json!({ "path": format!("{base}/nope.txt") }))
        .await
        .unwrap();
    assert_eq!(missing["exists"], false);
    let listing = line
        .call("list_dir", json!({ "path": base }))
        .await
        .unwrap();
    assert_eq!(listing["entries"][0]["name"], "hello.txt");
    assert_eq!(listing["truncated"], false);
    let read = line
        .call("read_file", json!({ "path": hello }))
        .await
        .unwrap();
    assert_eq!(read["content"], "line one\nline two\nline three\n");
    assert_eq!(
        approver.requests.lock().unwrap().len(),
        1,
        "只读工具不能弹确认（手动模式）"
    );
    println!("stat / list_dir / read_file：通过（手动模式没有弹确认）");

    // ---- 3. 覆盖（改一行）：生成备份，备份是原内容，新内容正确，没有临时文件 ----
    let edited = line
        .call(
            "write_file",
            json!({ "path": hello, "content": "line one\nline TWO changed\nline three\n" }),
        )
        .await
        .unwrap_or_else(|e| panic!("覆盖失败：{}", server.sanitize(&e)));
    assert_eq!(edited["created"], false);
    let backup = edited["backup_path"].as_str().expect("要有备份").to_owned();
    assert!(
        backup.starts_with(&format!("{hello}.zssh-bak-")),
        "{backup}"
    );
    assert_eq!(
        remote_bytes(&channel, &backup).await.unwrap(),
        b"line one\nline two\nline three\n",
        "备份是原内容"
    );
    assert_eq!(
        remote_bytes(&channel, &hello).await.unwrap(),
        b"line one\nline TWO changed\nline three\n"
    );
    let names = names_in(&channel, &base).await;
    assert!(
        names.iter().all(|n| !n.contains("zssh-tmp")),
        "临时文件要清掉：{names:?}"
    );
    {
        let asked = approver.requests.lock().unwrap().clone();
        let diff = &asked[1].file.as_ref().unwrap().diff;
        assert_eq!((diff.added, diff.removed), (1, 1));
    }
    println!("覆盖：通过（备份 {} 字节，临时文件已清）", 29);

    // ---- 3b. 备份保留：同一个文件只留最近 5 份，别的文件（哪怕名字像备份）不碰 ----
    let keep_path = format!("{base}/keep.txt");
    zssh_core::sftp::fs::write_bytes(&channel, &keep_path, b"v0\n", u64::MAX)
        .await
        .expect("准备保留份数测试文件");
    let foreign = format!("{base}/other.txt.zssh-bak-20200101-000000");
    zssh_core::sftp::fs::write_bytes(&channel, &foreign, b"keep me\n", u64::MAX)
        .await
        .expect("准备别的文件");
    let before_owner = zssh_core::sftp::fs::stat(&channel, &keep_path, true)
        .await
        .expect("stat 原文件");
    for index in 0..7 {
        let written = line
            .call(
                "write_file",
                json!({ "path": keep_path, "content": format!("v{}\n", index + 1) }),
            )
            .await
            .unwrap_or_else(|e| panic!("保留份数写入失败：{}", server.sanitize(&e)));
        assert!(
            written.get("owner_change").is_none(),
            "同一用户写自己的文件不该改属主：{written}"
        );
    }
    let names = names_in(&channel, &base).await;
    let kept: Vec<_> = names
        .iter()
        .filter(|name| name.starts_with("keep.txt.zssh-bak-"))
        .collect();
    assert_eq!(kept.len(), 5, "每个文件只留最近 5 份：{names:?}");
    assert!(
        names
            .iter()
            .any(|name| name == "other.txt.zssh-bak-20200101-000000"),
        "别的文件的备份不能删：{names:?}"
    );
    let after_owner = zssh_core::sftp::fs::stat(&channel, &keep_path, true)
        .await
        .expect("stat 写后");
    assert_eq!(
        (after_owner.uid, after_owner.gid),
        (before_owner.uid, before_owner.gid),
        "写完之后属主应该和原文件一样"
    );
    println!("备份保留：通过（7 次覆盖只留 5 份，其他文件未动，属主未变）");

    // ---- 4. 手动模式拒绝：文件没变、没有新备份 ----
    approver.allow.store(false, Ordering::SeqCst);
    let before = names_in(&channel, &base).await.len();
    let denied = line
        .call(
            "write_file",
            json!({ "path": hello, "content": "SHOULD NOT LAND\n" }),
        )
        .await
        .unwrap_err();
    assert!(
        denied.contains("用户拒绝") && denied.contains("没有改动"),
        "{denied}"
    );
    assert_eq!(
        remote_bytes(&channel, &hello).await.unwrap(),
        b"line one\nline TWO changed\nline three\n"
    );
    assert_eq!(
        names_in(&channel, &base).await.len(),
        before,
        "拒绝后不能多出任何文件"
    );
    println!("手动拒绝：通过（文件未改、没有新备份）");

    // ---- 5. 系统路径：在确认那一步拒绝，不碰服务器 ----
    let system_path = format!("/etc/zssh-m34-never-written-{}.conf", std::process::id());
    let blocked = line
        .call(
            "write_file",
            json!({ "path": system_path, "content": "# must never be written\n" }),
        )
        .await;
    // 路径所在目录 /etc 存在，所以会走到确认；确认被拒绝。
    assert!(blocked.is_err());
    let asked = approver.requests.lock().unwrap().last().cloned().unwrap();
    assert!(asked.danger == DangerLevel::Dangerous, "系统路径要标红");
    assert!(
        asked.reasons.iter().any(|r| r.contains("/etc")),
        "{:?}",
        asked.reasons
    );
    let etc_check = channel.stat(&system_path).await;
    assert!(etc_check.is_err(), "系统路径上不能出现这个文件");
    println!(
        "系统路径：在确认卡片上拒绝，原因 {:?}，文件没有被创建",
        asked.reasons
    );

    // ---- 6. 范围外的目标 ----
    let outside = line
        .call(
            "read_file",
            json!({ "target_id": "not-mine", "path": hello }),
        )
        .await
        .unwrap_err();
    assert!(outside.contains("作用范围"), "{outside}");

    // ---- 7. 大文件分段读：64KB 一段，拼回来和 SHA-256 对上 ----
    approver.allow.store(true, Ordering::SeqCst);
    let big_path = format!("{base}/big.txt");
    let big: String = (0..30_000)
        .map(|i| format!("第 {i} 行 ✓ some text\n"))
        .collect();
    zssh_core::sftp::fs::write_bytes(&channel, &big_path, big.as_bytes(), u64::MAX)
        .await
        .expect("准备大文件");
    assert!(big.len() > 2 * 64 * 1024);
    let mut got = String::new();
    let mut offset = 0u64;
    let mut segments = 0;
    loop {
        let part = line
            .call("read_file", json!({ "path": big_path, "offset": offset }))
            .await
            .unwrap();
        assert!(part["returned_bytes"].as_u64().unwrap() <= 64 * 1024);
        got.push_str(part["content"].as_str().unwrap());
        segments += 1;
        match part["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert!(segments >= 3);
    assert_eq!(
        sha(got.as_bytes()),
        sha(big.as_bytes()),
        "分段读回来要和原文件一致"
    );
    println!(
        "分段读：通过（{} 字节分 {segments} 段，SHA-256 一致）",
        big.len()
    );

    // ---- 8. 二进制文件 ----
    let bin_path = format!("{base}/blob.bin");
    zssh_core::sftp::fs::write_bytes(
        &channel,
        &bin_path,
        &[0x7f, b'E', b'L', b'F', 0, 1, 2, 3],
        u64::MAX,
    )
    .await
    .expect("准备二进制");
    let bin = line
        .call("read_file", json!({ "path": bin_path }))
        .await
        .unwrap();
    assert_eq!(bin["binary"], true);
    assert_eq!(bin["content"], "");

    // ---- 9. 自动模式：不弹确认，照样备份 ----
    mode.0.store(true, Ordering::SeqCst);
    let asked_before = approver.requests.lock().unwrap().len();
    let auto = line
        .call(
            "write_file",
            json!({ "path": hello, "content": "auto mode content\n" }),
        )
        .await
        .unwrap();
    assert_eq!(
        approver.requests.lock().unwrap().len(),
        asked_before,
        "自动模式不弹确认"
    );
    assert!(auto["backup_path"].is_string());
    assert_eq!(
        remote_bytes(&channel, &hello).await.unwrap(),
        b"auto mode content\n"
    );

    // ---- 审计：有工具名、路径、字节数、备份路径，没有文件内容 ----
    let rows = database
        .ai_audit_list(Some(scope.scope_id()), 100)
        .expect("读审计");
    let writes: Vec<_> = rows.iter().filter(|r| r.tool == "write_file").collect();
    assert!(writes
        .iter()
        .any(|r| r.backup_path.is_some() && r.bytes.is_some()));
    assert!(writes.iter().any(|r| r.decision == "denied"));
    for tool in ["stat", "list_dir", "read_file"] {
        assert!(rows.iter().any(|r| r.tool == tool), "审计里要有 {tool}");
    }
    let dump = format!("{rows:?}");
    for secret in [
        "SHOULD NOT LAND",
        "line TWO changed",
        "auto mode content",
        "must never be written",
    ] {
        assert!(!dump.contains(secret), "审计里不能有文件内容：{secret}");
    }
    println!("审计：{} 条，不含文件内容", rows.len());

    // ---- 收尾：删临时目录，核对没有残留 ----
    dir.cleanup(&connection, &channel).await;
    assert!(channel.stat(&base).await.is_err(), "临时目录要删干净");
    scope.destroy();
    let _ = host.opened.load(Ordering::SeqCst);
}
