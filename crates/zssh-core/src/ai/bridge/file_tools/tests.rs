//! 文件工具经工具桥的行为：作用范围、确认（只读不确认、写要确认且带差异和危险标记）、
//! 备份失败不写、审计不含内容。用内存文件系统，不联网。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::super::{
    ApprovalDecision, ApprovalRequest, Approver, AuditEntry, AuditLog, Bridge, BridgeHost,
    ExecDecision, ExecMode, ExecModeProvider,
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

    fn remote_fs<'a>(
        &'a self,
        _target_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RemoteFs>, String>> {
        self.opened.fetch_add(1, Ordering::SeqCst);
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
