//! 文件工具经工具桥的行为：作用范围、确认（只读不确认、写要确认且带差异和危险标记）、
//! 备份失败不写、审计不含内容。用内存文件系统，不联网。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::super::{
    ApprovalDecision, ApprovalRequest, Approver, AuditEntry, AuditLog, Bridge, BridgeHost,
    ExecDecision, ExecMode, ExecModeProvider, ExecTargetReport,
};
use crate::ai::files::testfs::MemFs;
use crate::ai::files::RemoteFs;
use crate::ai::protocol::{
    RequestEnvelope, METHOD_LIST_DIR, METHOD_READ_FILE, METHOD_STAT, METHOD_WRITE_FILE,
};
use crate::ssh::{BoxFuture, ExecCapture};

struct Mode(Mutex<ExecMode>);

impl ExecModeProvider for Mode {
    fn mode(&self) -> ExecMode {
        *self.0.lock().unwrap()
    }
}

/// 宿主：所有目标共用一份内存文件系统；目标 id 不是 `s1` 的一律没有文件系统。
struct FsHost {
    fs: Arc<MemFs>,
    opened: AtomicUsize,
    /// 每次开文件系统用的 ID（绑定了会话就是会话 ID）和收到的执行位置回报。
    opened_for: Mutex<Vec<String>>,
    reports: Mutex<Vec<ExecTargetReport>>,
}

impl BridgeHost for FsHost {
    fn exec<'a>(
        &'a self,
        _session_id: &'a str,
        _command: &'a str,
        _wait: Duration,
        _max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async { Err("本测试不执行命令".to_owned()) })
    }

    fn read_terminal<'a>(
        &'a self,
        _session_id: &'a str,
        _max_lines: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn report_target(&self, report: ExecTargetReport) {
        self.reports.lock().unwrap().push(report);
    }

    fn remote_fs<'a>(
        &'a self,
        target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        self.opened_for.lock().unwrap().push(target_id.to_owned());
        let fs: Arc<dyn RemoteFs> = self.fs.clone();
        Box::pin(async move { Ok(fs) })
    }
}

#[derive(Clone, Copy)]
enum Answer {
    Allow,
    Deny,
    Never,
}

struct Approvals {
    answer: Answer,
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl Approver for Approvals {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        self.requests.lock().unwrap().push(request);
        let answer = self.answer;
        Box::pin(async move {
            match answer {
                Answer::Allow => ApprovalDecision::Allowed,
                Answer::Deny => ApprovalDecision::Denied,
                Answer::Never => std::future::pending().await,
            }
        })
    }
}

#[derive(Default)]
struct Audit(Mutex<Vec<AuditEntry>>);

impl AuditLog for Audit {
    fn record(&self, entry: AuditEntry) {
        self.0.lock().unwrap().push(entry);
    }
}

struct Rig {
    bridge: Arc<Bridge>,
    fs: Arc<MemFs>,
    approvals: Arc<Approvals>,
    audit: Arc<Audit>,
    mode: Arc<Mode>,
    host: Arc<FsHost>,
}

fn rig(mode: ExecMode, answer: Answer) -> Rig {
    let fs = Arc::new(MemFs::new());
    let host = Arc::new(FsHost {
        fs: fs.clone(),
        opened: AtomicUsize::new(0),
        opened_for: Mutex::new(Vec::new()),
        reports: Mutex::new(Vec::new()),
    });
    let approvals = Arc::new(Approvals {
        answer,
        requests: Mutex::new(Vec::new()),
    });
    let audit = Arc::new(Audit::default());
    let mode = Arc::new(Mode(Mutex::new(mode)));
    let bridge = Arc::new(
        Bridge::new(host.clone(), audit.clone(), approvals.clone(), mode.clone())
            .with_approval_wait(Duration::from_millis(300)),
    );
    Rig {
        bridge,
        fs,
        approvals,
        audit,
        mode,
        host,
    }
}

fn request(capability: &str, method: &str, params: Value) -> RequestEnvelope {
    RequestEnvelope {
        id: 1,
        method: method.to_owned(),
        capability: capability.to_owned(),
        params,
    }
}

async fn call(rig: &Rig, capability: &str, method: &str, params: Value) -> Result<Value, String> {
    rig.bridge
        .dispatch(&request(capability, method, params))
        .await
}

/// 一个带目标 `s1` 的对话。
fn scope_with_target(rig: &Rig) -> String {
    let handle = rig.bridge.create_scope();
    handle.add_target("s1", "web-1", "root", "connected", true, false);
    handle.capability().to_owned()
}

#[tokio::test]
async fn read_only_tools_never_ask_for_confirmation_even_in_manual_mode() {
    let rig = rig(ExecMode::Manual, Answer::Deny);
    rig.fs.put("/home/u/a.txt", b"hello\n", 0o644);
    let cap = scope_with_target(&rig);

    let listing = call(&rig, &cap, METHOD_LIST_DIR, json!({ "path": "~" }))
        .await
        .unwrap();
    assert_eq!(listing["entries"][0]["name"], "a.txt");
    let stat = call(&rig, &cap, METHOD_STAT, json!({ "path": "a.txt" }))
        .await
        .unwrap();
    assert_eq!(stat["exists"], true);
    let read = call(&rig, &cap, METHOD_READ_FILE, json!({ "path": "a.txt" }))
        .await
        .unwrap();
    assert_eq!(read["content"], "hello\n");

    assert!(
        rig.approvals.requests.lock().unwrap().is_empty(),
        "只读工具不能弹确认"
    );
    // 三次都进了审计，带工具名和路径，没有文件内容。
    let entries = rig.audit.0.lock().unwrap().clone();
    let tools: Vec<&str> = entries.iter().map(|e| e.tool.as_str()).collect();
    assert_eq!(tools, vec!["list_dir", "stat", "read_file"]);
    assert_eq!(entries[2].path.as_deref(), Some("/home/u/a.txt"));
    assert_eq!(entries[2].bytes, Some(6));
    assert!(entries.iter().all(|e| e.decision == ExecDecision::Auto));
    let dump = format!("{entries:?}");
    assert!(!dump.contains("hello"), "审计不能带文件内容：{dump}");
}

#[tokio::test]
async fn targets_outside_the_scope_are_rejected_by_every_file_tool() {
    let rig = rig(ExecMode::Auto, Answer::Allow);
    rig.fs.put("/home/u/a.txt", b"x", 0o644);
    let cap = scope_with_target(&rig);
    for (method, params) in [
        (METHOD_LIST_DIR, json!({ "target_id": "s2", "path": "~" })),
        (METHOD_STAT, json!({ "target_id": "s2", "path": "a.txt" })),
        (
            METHOD_READ_FILE,
            json!({ "target_id": "s2", "path": "a.txt" }),
        ),
        (
            METHOD_WRITE_FILE,
            json!({ "target_id": "s2", "path": "a.txt", "content": "y" }),
        ),
    ] {
        let err = call(&rig, &cap, method, params).await.unwrap_err();
        assert!(err.contains("作用范围"), "{method}：{err}");
    }
    assert_eq!(
        rig.host.opened.load(Ordering::SeqCst),
        0,
        "范围外的目标连文件系统都不能碰"
    );
    assert_eq!(rig.fs.get("/home/u/a.txt").unwrap(), b"x");
    assert!(rig.audit.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn another_conversations_target_is_not_reachable() {
    let rig = rig(ExecMode::Auto, Answer::Allow);
    let _first = scope_with_target(&rig);
    // 第二个对话没有任何目标。
    let second = rig.bridge.create_scope();
    let err = call(
        &rig,
        second.capability(),
        METHOD_LIST_DIR,
        json!({ "target_id": "s1", "path": "~" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("作用范围"), "{err}");
    let err = call(&rig, "bogus", METHOD_STAT, json!({ "path": "~" }))
        .await
        .unwrap_err();
    assert!(err.contains("能力密钥"), "{err}");
}

#[tokio::test]
async fn manual_write_asks_with_diff_and_flags_system_paths() {
    let rig = rig(ExecMode::Manual, Answer::Deny);
    rig.fs.put_dir("/etc");
    rig.fs.put("/etc/hosts", b"127.0.0.1 localhost\n", 0o644);
    let cap = scope_with_target(&rig);

    let err = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "/etc/hosts", "content": "127.0.0.1 localhost\n10.9.9.9 evil\n" }),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("用户拒绝") && err.contains("没有改动"),
        "{err}"
    );

    // 拒绝：文件没变、没有备份、没有临时文件、没有任何写操作。
    assert_eq!(rig.fs.get("/etc/hosts").unwrap(), b"127.0.0.1 localhost\n");
    assert!(
        rig.fs.log().is_empty(),
        "拒绝后不能碰服务器：{:?}",
        rig.fs.log()
    );
    assert_eq!(rig.fs.names_in("/etc").len(), 1);

    let requests = rig.approvals.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let ask = &requests[0];
    assert_eq!(ask.command, "write_file /etc/hosts");
    assert_eq!(ask.danger, crate::ai::safety::DangerLevel::Dangerous);
    assert!(ask.reasons.iter().any(|r| r.contains("/etc")));
    let file = ask.file.as_ref().expect("确认卡片要带文件细节");
    assert_eq!(file.path, "/etc/hosts");
    assert!(!file.is_new);
    assert_eq!((file.diff.added, file.diff.removed), (1, 0));
    assert!(file.diff.lines.iter().any(|l| l.text == "10.9.9.9 evil"));

    // 拒绝也记审计：denied、标红、没有字节数和备份。
    let entries = rig.audit.0.lock().unwrap().clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tool, "write_file");
    assert_eq!(entries[0].decision, ExecDecision::Denied);
    assert_eq!(entries[0].danger, crate::ai::safety::DangerLevel::Dangerous);
    assert_eq!(entries[0].bytes, None);
    assert_eq!(entries[0].backup_path, None);
}

#[tokio::test]
async fn manual_write_allowed_backs_up_writes_and_audits_without_content() {
    let rig = rig(ExecMode::Manual, Answer::Allow);
    rig.fs
        .put("/home/u/app.conf", b"port=80\nmode=old\n", 0o640);
    let cap = scope_with_target(&rig);
    let secret = "mode=new-SECRET-VALUE\n";

    let result = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "app.conf", "content": format!("port=80\n{secret}") }),
    )
    .await
    .unwrap();

    assert_eq!(result["created"], false);
    assert_eq!(result["dangerous"], false);
    let backup = result["backup_path"]
        .as_str()
        .expect("要告诉模型备份在哪")
        .to_owned();
    assert!(backup.starts_with("/home/u/app.conf.zssh-bak-"), "{backup}");
    assert_eq!(rig.fs.get(&backup).unwrap(), b"port=80\nmode=old\n");
    assert_eq!(
        rig.fs.get("/home/u/app.conf").unwrap(),
        format!("port=80\n{secret}").as_bytes()
    );

    let entries = rig.audit.0.lock().unwrap().clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].decision, ExecDecision::Allowed);
    assert_eq!(entries[0].backup_path.as_deref(), Some(backup.as_str()));
    assert_eq!(
        entries[0].bytes,
        Some(format!("port=80\n{secret}").len() as i64)
    );
    assert_eq!(entries[0].path.as_deref(), Some("/home/u/app.conf"));
    let dump = format!("{entries:?}");
    assert!(!dump.contains("SECRET-VALUE"), "审计不能带文件内容：{dump}");
    assert!(!dump.contains("mode=old"), "审计不能带文件内容：{dump}");
}

#[tokio::test]
async fn auto_mode_writes_without_asking_but_still_flags_and_backs_up() {
    let rig = rig(ExecMode::Auto, Answer::Deny);
    rig.fs.put_dir("/etc");
    rig.fs.put("/etc/motd", b"hi\n", 0o644);
    let cap = scope_with_target(&rig);

    let result = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "/etc/motd", "content": "hello\n" }),
    )
    .await
    .unwrap();
    assert!(
        rig.approvals.requests.lock().unwrap().is_empty(),
        "自动模式不弹确认"
    );
    assert_eq!(result["dangerous"], true);
    assert!(result["danger_reasons"][0]
        .as_str()
        .unwrap()
        .contains("/etc"));
    assert!(result["backup_path"].is_string());
    assert_eq!(rig.fs.get("/etc/motd").unwrap(), b"hello\n");
    let entries = rig.audit.0.lock().unwrap().clone();
    assert_eq!(entries[0].decision, ExecDecision::Auto);
    assert_eq!(entries[0].danger, crate::ai::safety::DangerLevel::Dangerous);
}

#[tokio::test]
async fn backup_failure_through_the_bridge_leaves_the_file_alone() {
    let rig = rig(ExecMode::Auto, Answer::Allow);
    rig.fs.put("/home/u/c.txt", b"keep me\n", 0o644);
    *rig.fs.fail_write_containing.lock().unwrap() = Some("zssh-bak".to_owned());
    let cap = scope_with_target(&rig);

    let err = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "c.txt", "content": "overwritten\n" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("备份失败"), "{err}");
    assert_eq!(rig.fs.get("/home/u/c.txt").unwrap(), b"keep me\n");
    let entries = rig.audit.0.lock().unwrap().clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].exit_code, Some(1), "失败也记审计");
    assert_eq!(entries[0].bytes, None);
}

#[tokio::test]
async fn creating_a_new_file_shows_the_whole_text_and_create_only_refuses_existing() {
    let rig = rig(ExecMode::Manual, Answer::Allow);
    rig.fs.put("/home/u/exists.txt", b"x\n", 0o644);
    let cap = scope_with_target(&rig);

    let result = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "fresh.txt", "content": "a\nb\n", "create_only": true }),
    )
    .await
    .unwrap();
    assert_eq!(result["created"], true);
    assert!(result.get("backup_path").is_none());
    let ask = rig.approvals.requests.lock().unwrap()[0].clone();
    let file = ask.file.unwrap();
    assert!(file.is_new);
    assert_eq!(file.diff.added, 2);

    let err = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "exists.txt", "content": "y\n", "create_only": true }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("create_only"), "{err}");
    assert_eq!(
        rig.approvals.requests.lock().unwrap().len(),
        1,
        "被前置检查拒绝的写入不该打扰用户"
    );
}

#[tokio::test]
async fn an_unanswered_confirmation_times_out_as_a_refusal() {
    let rig = rig(ExecMode::Manual, Answer::Never);
    rig.fs.put("/home/u/t.txt", b"1\n", 0o644);
    let cap = scope_with_target(&rig);
    let err = call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "t.txt", "content": "2\n" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("超时"), "{err}");
    assert_eq!(rig.fs.get("/home/u/t.txt").unwrap(), b"1\n");
    assert_eq!(
        rig.audit.0.lock().unwrap()[0].decision,
        ExecDecision::Timeout
    );
    *rig.mode.0.lock().unwrap() = ExecMode::Auto;
}

// ---------------------------------------------------------------------------
// AUDIT-2 A02：等连接的时候目标被移出对话，读文件请求不能继续读、也不能把内容交回去。
// 走真实的回环协议（只监听 127.0.0.1、系统分配端口），宿主在"等连接"阶段暂停。
// ---------------------------------------------------------------------------

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use crate::ai::files::{FsEntry, FsStat, RenameFailure};

/// 数读取次数的文件系统：包一层内存文件系统，所有读操作都计数。
///
/// `hold_first_read` 置位时，第一次读会停在半路（通知 `read_entered`，等 `read_release`），
/// 用来模拟"读的过程中目标被移出"。
struct CountingFs {
    inner: Arc<MemFs>,
    reads: AtomicUsize,
    hold_first_read: AtomicBool,
    read_entered: Notify,
    read_release: Notify,
}

impl CountingFs {
    fn new(inner: Arc<MemFs>) -> CountingFs {
        CountingFs {
            inner,
            reads: AtomicUsize::new(0),
            hold_first_read: AtomicBool::new(false),
            read_entered: Notify::new(),
            read_release: Notify::new(),
        }
    }

    async fn count_read(&self) {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.hold_first_read.swap(false, Ordering::SeqCst) {
            self.read_entered.notify_one();
            self.read_release.notified().await;
        }
    }
}

impl RemoteFs for CountingFs {
    fn home<'a>(&'a self) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.home().await
        })
    }
    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<FsStat>, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.stat(path).await
        })
    }
    fn realpath<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.realpath(path).await
        })
    }
    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FsEntry>, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.list(path).await
        })
    }
    fn read_range<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: usize,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.read_range(path, offset, len).await
        })
    }
    fn read_all<'a>(&'a self, path: &'a str, limit: u64) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            self.count_read().await;
            self.inner.read_all(path, limit).await
        })
    }
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        data: &'a [u8],
        mode: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.inner.write_file(path, data, mode)
    }
    fn rename_over<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
    ) -> BoxFuture<'a, Result<(), RenameFailure>> {
        self.inner.rename_over(from, to)
    }
    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.inner.remove_file(path)
    }
    fn set_owner<'a>(
        &'a self,
        path: &'a str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.inner.set_owner(path, uid, gid)
    }
}

/// 宿主：`remote_fs` 先通知"已进入连接等待"，再等测试放行。
struct GatedHost {
    fs: Arc<CountingFs>,
    entered: Notify,
    release: Notify,
}

impl BridgeHost for GatedHost {
    fn exec<'a>(
        &'a self,
        _session_id: &'a str,
        _command: &'a str,
        _wait: Duration,
        _max_stream_bytes: usize,
    ) -> BoxFuture<'a, Result<ExecCapture, String>> {
        Box::pin(async { Err("本测试不执行命令".to_owned()) })
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
        _target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            let fs: Arc<dyn RemoteFs> = self.fs.clone();
            Ok(fs)
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum WhileConnecting {
    Nothing,
    RemoveTarget,
    RemoveAndAddBackSameId,
    /// 连接已经好了，读到一半（宿主之后的第一次读停住）时目标被移出。
    RemoveWhileReading,
}

/// 发一条读类请求，在连接等待阶段按 `what` 动一下作用范围，再放行连接。返回（响应, 实际读取次数）。
async fn gated_read(method: &str, params: Value, what: WhileConnecting) -> (Value, usize) {
    let mem = Arc::new(MemFs::new());
    mem.put("/home/u/a.txt", b"audit-witness", 0o644);
    let host = Arc::new(GatedHost {
        fs: Arc::new(CountingFs::new(mem)),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let approvals = Arc::new(Approvals {
        answer: Answer::Deny,
        requests: Mutex::new(Vec::new()),
    });
    let bridge = Arc::new(Bridge::new(
        host.clone(),
        Arc::new(Audit::default()),
        approvals,
        Arc::new(Mode(Mutex::new(ExecMode::Manual))),
    ));
    if what == WhileConnecting::RemoveWhileReading {
        host.fs.hold_first_read.store(true, Ordering::SeqCst);
    }
    let scope = bridge.create_scope();
    scope.add_target("s1", "web-1", "root", "connected", true, false);

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(Arc::clone(&bridge).serve(listener));
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    for value in [
        json!({ "token": bridge.connection_token() }),
        json!({
            "id": 1,
            "method": method,
            "params": params,
            "capability": scope.capability(),
        }),
    ] {
        let mut line = serde_json::to_string(&value).unwrap();
        line.push('\n');
        writer.write_all(line.as_bytes()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), host.entered.notified())
        .await
        .expect("请求应该已经进入连接等待");
    match what {
        WhileConnecting::Nothing => {}
        WhileConnecting::RemoveTarget => scope.remove_target("s1"),
        WhileConnecting::RemoveAndAddBackSameId => {
            scope.remove_target("s1");
            scope.add_target("s1", "web-1", "root", "connected", true, false);
        }
        WhileConnecting::RemoveWhileReading => {
            host.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), host.fs.read_entered.notified())
                .await
                .expect("请求应该已经读到一半");
            scope.remove_target("s1");
            host.fs.read_release.notify_one();
        }
    }
    if what != WhileConnecting::RemoveWhileReading {
        host.release.notify_one();
    }
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("应该有响应")
        .unwrap();
    server.abort();
    let response: Value = serde_json::from_str(line.trim()).unwrap();
    (response, host.fs.reads.load(Ordering::SeqCst))
}

fn read_tool_calls() -> Vec<(&'static str, Value)> {
    vec![
        (
            METHOD_READ_FILE,
            json!({ "target_id": "s1", "path": "/home/u/a.txt" }),
        ),
        (
            METHOD_LIST_DIR,
            json!({ "target_id": "s1", "path": "/home/u" }),
        ),
        (
            METHOD_STAT,
            json!({ "target_id": "s1", "path": "/home/u/a.txt" }),
        ),
    ]
}

#[tokio::test]
async fn read_tools_still_read_when_nothing_changes_while_connecting() {
    for (method, params) in read_tool_calls() {
        let (response, reads) = gated_read(method, params, WhileConnecting::Nothing).await;
        assert!(response.get("error").is_none(), "{method}：{response}");
        assert!(reads > 0, "{method}：对照组应该真的读了");
        if method == METHOD_READ_FILE {
            assert_eq!(response["result"]["content"], "audit-witness");
        }
    }
}

#[tokio::test]
async fn read_tools_do_not_read_after_the_target_is_removed_while_connecting() {
    for (method, params) in read_tool_calls() {
        let (response, reads) = gated_read(method, params, WhileConnecting::RemoveTarget).await;
        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{method}：移出后该被拒绝：{response}"));
        assert!(message.contains("移出"), "{method}：{message}");
        assert_eq!(reads, 0, "{method}：移出之后不能再读任何东西");
        assert!(!response.to_string().contains("audit-witness"));
    }
}

#[tokio::test]
async fn an_old_request_stays_invalid_when_the_same_target_id_is_added_back() {
    for (method, params) in read_tool_calls() {
        let (response, reads) =
            gated_read(method, params, WhileConnecting::RemoveAndAddBackSameId).await;
        assert!(
            response.get("error").is_some(),
            "{method}：移出又加回同一个 ID，旧请求也不能变回有效：{response}"
        );
        assert_eq!(reads, 0, "{method}");
        assert!(!response.to_string().contains("audit-witness"));
    }
}

#[tokio::test]
async fn read_tools_do_not_return_content_when_the_target_is_removed_mid_read() {
    for (method, params) in read_tool_calls() {
        let (response, reads) =
            gated_read(method, params, WhileConnecting::RemoveWhileReading).await;
        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{method}：读的过程中移出，结果也不能交回去：{response}"));
        assert!(message.contains("移出"), "{method}：{message}");
        assert!(
            reads > 0,
            "{method}：这条用例是读到一半才移出，读应当已经开始"
        );
        assert!(!response.to_string().contains("audit-witness"));
    }
}

#[tokio::test]
async fn file_tools_open_the_bound_session_and_report_where_they_run() {
    // 逻辑目标 host-7 绑定了会话 sess-9：文件通道开在绑定的会话上，开通道之前就把
    // 实际执行位置回报给界面；写文件的确认请求带同一份位置。
    let rig = rig(ExecMode::Manual, Answer::Allow);
    rig.fs.put("/home/u/a.txt", b"hello\n", 0o644);
    let handle = rig.bridge.create_scope();
    handle.add_target_summary(crate::ai::protocol::TargetSummary {
        id: "host-7".to_owned(),
        host_label: "web-1".to_owned(),
        username: "root".to_owned(),
        status: "connected".to_owned(),
        current: true,
        pinned: false,
        session_id: Some("sess-9".to_owned()),
    });
    let cap = handle.capability().to_owned();

    call(&rig, &cap, METHOD_LIST_DIR, json!({ "path": "~" }))
        .await
        .unwrap();
    call(&rig, &cap, METHOD_STAT, json!({ "path": "a.txt" }))
        .await
        .unwrap();
    call(&rig, &cap, METHOD_READ_FILE, json!({ "path": "a.txt" }))
        .await
        .unwrap();
    call(
        &rig,
        &cap,
        METHOD_WRITE_FILE,
        json!({ "path": "b.txt", "content": "x" }),
    )
    .await
    .unwrap();

    assert_eq!(
        *rig.host.opened_for.lock().unwrap(),
        vec!["sess-9"; 4],
        "四个文件工具都开在绑定的会话上"
    );
    let reports = rig.host.reports.lock().unwrap().clone();
    let seen: Vec<(&str, &str)> = reports
        .iter()
        .map(|r| (r.tool.as_str(), r.key.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("list_dir", "~"),
            ("stat", "a.txt"),
            ("read_file", "a.txt"),
            ("write_file", "b.txt"),
        ]
    );
    assert!(reports.iter().all(|r| r.target_id == "host-7"
        && r.session_id.as_deref() == Some("sess-9")
        && r.host_label == "web-1"));
    let approvals = rig.approvals.requests.lock().unwrap().clone();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].target_id, "host-7");
    assert_eq!(approvals[0].session_id.as_deref(), Some("sess-9"));
    // 每次调用一个编号，写文件的确认请求带的就是它回报的那个。
    let mut ids: Vec<&str> = reports.iter().map(|r| r.exec_id.as_str()).collect();
    assert!(ids.iter().all(|id| !id.is_empty()));
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4, "四次调用四个不同的编号");
    assert_eq!(approvals[0].exec_id, reports[3].exec_id);
}
