//! 导入事务：引用换回本地 id、DPAPI 重新加密、失败回滚、known_hosts 只追加。

use super::*;
use crate::store::Database;
use crate::sync::merge::Conflict;
use crate::sync::package::{
    GroupRecord, HostRecord, IdentityRecord, ProxyRecord, SyncSnapshot, Tombstone,
};

/// 模拟“这台电脑的 DPAPI”：可逆的假密文，带一个前缀，能看出导入时重新封装过。
struct FakeSealer(u8);

impl SecretSealer for FakeSealer {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SyncError> {
        let mut out = vec![b'F', self.0];
        out.extend(plaintext.iter().map(|byte| byte ^ self.0));
        Ok(out)
    }

    fn open(&self, blob: &[u8]) -> Result<Vec<u8>, SyncError> {
        if blob.len() < 2 || blob[0] != b'F' || blob[1] != self.0 {
            return Err(SyncError::InvalidData("不是这层封装".to_owned()));
        }
        Ok(blob[2..].iter().map(|byte| byte ^ self.0).collect())
    }
}

fn sample(device: &str) -> SyncSnapshot {
    let mut snapshot = SyncSnapshot::empty(device, 1_760_000_000);
    snapshot.groups.push(GroupRecord {
        uid: "g1".to_owned(),
        name: "生产".to_owned(),
        parent_uid: None,
        sort: 0,
        updated_at: 10,
        device_id: device.to_owned(),
    });
    snapshot.identities.push(IdentityRecord {
        uid: "i1".to_owned(),
        name: "部署密码".to_owned(),
        kind: "password".to_owned(),
        key_path: None,
        secret: Some("identity-secret-xyz".to_owned()),
        updated_at: 10,
        device_id: device.to_owned(),
    });
    snapshot.proxies.push(ProxyRecord {
        uid: "p1".to_owned(),
        name: "代理".to_owned(),
        kind: "socks5".to_owned(),
        host: "proxy.example.internal".to_owned(),
        port: 1080,
        username: None,
        secret: Some("proxy-secret-xyz".to_owned()),
        updated_at: 10,
        device_id: device.to_owned(),
    });
    snapshot.hosts.push(HostRecord {
        uid: "h1".to_owned(),
        group_uid: Some("g1".to_owned()),
        name: "web-1".to_owned(),
        host: "web1.example.internal".to_owned(),
        port: 22,
        username: "deploy".to_owned(),
        auth_method: "password".to_owned(),
        identity_uid: Some("i1".to_owned()),
        proxy_uid: Some("p1".to_owned()),
        jump_host_uid: None,
        proxy_mode: "custom".to_owned(),
        tags: vec!["web".to_owned()],
        sort: 0,
        legacy_algorithms: false,
        created_at: 1,
        updated_at: 10,
        device_id: device.to_owned(),
    });
    snapshot.settings.insert(
        crate::store::settings::DEFAULT_PROXY_KEY.to_owned(),
        serde_json::json!("p1"),
    );
    snapshot
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("graphite"));
    snapshot.tombstones.push(Tombstone {
        entity: "host".to_owned(),
        key: "deleted-uid".to_owned(),
        deleted_at: 1_759_000_000,
        device_id: "dev-a".to_owned(),
    });
    snapshot
}

fn raw_secret(db: &Database, table: &str, id: i64) -> Vec<u8> {
    db.with_conn(|conn| {
        let blob: Vec<u8> = conn.query_row(
            &format!("SELECT secret_blob FROM {table} WHERE id = ?1"),
            [id],
            |row| row.get(0),
        )?;
        Ok(blob)
    })
    .expect("读密文")
}

#[test]
fn applying_a_snapshot_remaps_references_and_reencrypts_secrets() {
    let db = Database::open_in_memory().expect("内存库");
    db.setting_set("layout.ai_panel_width", "520")
        .expect("本地布局");
    let sealer = FakeSealer(0x5A);
    let known_hosts_dir = crate::test_util::TestDir::new("sync-apply");
    let known_hosts = known_hosts_dir.join("known_hosts");
    std::fs::write(
        &known_hosts,
        "web1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa\n",
    )
    .expect("写 known_hosts");
    let snapshot = sample("dev-a");
    let conflicts = vec![Conflict {
        entity: "host".to_owned(),
        key: "h1".to_owned(),
        kept: "{}".to_owned(),
        other: "{}".to_owned(),
        message: "测试冲突".to_owned(),
    }];

    let report =
        apply_snapshot(&db, &snapshot, &sealer, Some(&known_hosts), &conflicts, 42).expect("导入");
    assert_eq!(report.groups.inserted, 1);
    assert_eq!(report.identities.inserted, 1);
    assert_eq!(report.proxies.inserted, 1);
    assert_eq!(report.hosts.inserted, 1);
    assert_eq!(report.conflicts_stored, 1);
    assert_eq!(report.known_hosts_added, 0, "已经有的主机密钥不重复追加");

    let host = db.host_list().expect("列主机").remove(0);
    assert_eq!(host.name, "web-1");
    assert_eq!(
        host.group_id,
        Some(db.group_list().unwrap()[0].id),
        "分组换成本地 id"
    );
    assert_eq!(
        host.identity_id,
        Some(db.identity_list().unwrap()[0].id),
        "凭据换成本地 id"
    );
    assert_eq!(host.proxy_id, Some(db.proxy_list().unwrap()[0].id));
    assert_eq!(db.default_proxy_id().unwrap(), host.proxy_id);
    assert_eq!(
        db.setting_get("ui_theme").unwrap().as_deref(),
        Some("\"graphite\""),
        "同步的主题写进来了"
    );
    assert_eq!(
        db.setting_get("layout.ai_panel_width").unwrap().as_deref(),
        Some("520"),
        "本机布局设置不能被同步覆盖"
    );

    let identity_id = db.identity_list().unwrap()[0].id;
    let blob = raw_secret(&db, "identities", identity_id);
    assert_ne!(blob, b"identity-secret-xyz", "落库必须是密文");
    assert_eq!(
        sealer.open(&blob).expect("本机封装能解开"),
        b"identity-secret-xyz",
        "导入时用本机的封装重新加密"
    );
    let proxy_id = db.proxy_list().unwrap()[0].id;
    let blob = raw_secret(&db, "proxies", proxy_id);
    assert_eq!(sealer.open(&blob).unwrap(), b"proxy-secret-xyz");

    let tombstones = crate::sync::snapshot::load_tombstones(&db, 1_760_000_000).expect("墓碑");
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].key, "deleted-uid");

    let conflicts: i64 = db
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT count(*) FROM sync_conflicts", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(conflicts, 1);
}

#[test]
fn applying_twice_updates_instead_of_duplicating() {
    let db = Database::open_in_memory().expect("内存库");
    let sealer = FakeSealer(1);
    let snapshot = sample("dev-a");
    apply_snapshot(&db, &snapshot, &sealer, None, &[], 1).expect("第一次");
    let mut changed = snapshot.clone();
    changed.hosts[0].name = "web-1-renamed".to_owned();
    changed.hosts[0].updated_at = 20;
    let report = apply_snapshot(&db, &changed, &sealer, None, &[], 2).expect("第二次");
    assert_eq!(report.groups.inserted, 0);
    assert_eq!(report.hosts.inserted, 0);
    assert_eq!(report.hosts.updated, 1);
    assert_eq!(db.host_list().unwrap().len(), 1);
    assert_eq!(db.host_list().unwrap()[0].name, "web-1-renamed");
    assert_eq!(db.group_list().unwrap().len(), 1);
    assert_eq!(db.identity_list().unwrap().len(), 1);
}

#[test]
fn removing_a_record_deletes_it_and_releases_references() {
    let db = Database::open_in_memory().expect("内存库");
    let sealer = FakeSealer(2);
    let snapshot = sample("dev-a");
    apply_snapshot(&db, &snapshot, &sealer, None, &[], 1).expect("第一次");

    let mut without_identity = snapshot.clone();
    without_identity.identities.clear();
    // 合并阶段会把引用置空；这里直接模拟已经置空的结果。
    without_identity.hosts[0].identity_uid = None;
    let report = apply_snapshot(&db, &without_identity, &sealer, None, &[], 2).expect("删除");
    assert_eq!(report.identities.deleted, 1);
    assert_eq!(db.identity_list().unwrap().len(), 0);
    assert_eq!(db.host_list().unwrap()[0].identity_id, None);
}

#[test]
fn a_bad_record_rolls_back_the_whole_import() {
    let db = Database::open_in_memory().expect("内存库");
    let sealer = FakeSealer(3);
    let mut snapshot = sample("dev-a");
    // auth_method 有 CHECK 约束；写到这里会失败，前面写进去的分组/凭据也要回滚。
    snapshot.hosts[0].auth_method = "nonsense".to_owned();
    let err = apply_snapshot(&db, &snapshot, &sealer, None, &[], 1).expect_err("应该失败");
    assert!(
        err.to_string().contains("CHECK") || err.to_string().contains("约束"),
        "{err}"
    );
    assert_eq!(db.group_list().unwrap().len(), 0, "分组要回滚");
    assert_eq!(db.identity_list().unwrap().len(), 0, "凭据要回滚");
    assert_eq!(db.proxy_list().unwrap().len(), 0, "代理要回滚");
    assert_eq!(db.host_list().unwrap().len(), 0);
}

#[test]
fn empty_secret_clears_the_stored_blob() {
    let db = Database::open_in_memory().expect("内存库");
    let sealer = FakeSealer(4);
    let mut snapshot = sample("dev-a");
    apply_snapshot(&db, &snapshot, &sealer, None, &[], 1).expect("写入");
    snapshot.identities[0].secret = None;
    snapshot.identities[0].updated_at = 20;
    apply_snapshot(&db, &snapshot, &sealer, None, &[], 2).expect("清掉");
    let id = db.identity_list().unwrap()[0].id;
    let blob: Option<Vec<u8>> = db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT secret_blob FROM identities WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(blob.unwrap_or_default().is_empty());
}

/// F06：AI 执行模式是权限设置，另一台电脑不能通过同步把本机从"手动审批"改成"自动执行"。
#[test]
fn a_snapshot_carrying_the_ai_exec_mode_does_not_change_this_machine() {
    let db = Database::open_in_memory().expect("内存库");
    db.set_ai_exec_mode("manual").expect("本机设成手动");
    let mut snapshot = sample("dev-a");
    snapshot.settings.insert(
        "ai_exec_mode".to_owned(),
        serde_json::Value::String("auto".to_owned()),
    );
    apply_snapshot(&db, &snapshot, &FakeSealer(1), None, &[], 1).expect("导入");
    assert_eq!(
        db.ai_exec_mode().unwrap(),
        "manual",
        "同步包把本机的执行模式改成了自动"
    );
}

#[test]
fn this_machines_ai_exec_mode_is_never_exported() {
    let db = Database::open_in_memory().expect("内存库");
    db.set_ai_exec_mode("auto").expect("本机设成自动");
    let snapshot =
        crate::sync::snapshot::snapshot_local(&db, &FakeSealer(1), "dev-a", None, 1).expect("导出");
    assert!(
        !snapshot.settings.contains_key("ai_exec_mode"),
        "执行模式进了同步包"
    );
}

/// R-7 F07：写库这一层也检查类型和取值；不合格的键保留本机原值，不影响同一个包里的其他键。
#[test]
fn applying_skips_invalid_setting_values_and_keeps_the_local_ones() {
    let db = Database::open_in_memory().expect("内存库");
    db.setting_set("terminal_font_size", "16").unwrap();
    db.setting_set("ui_theme", "\"graphite\"").unwrap();
    db.setting_set("ai_exec_mode", "\"manual\"").unwrap();
    let mut snapshot = SyncSnapshot::empty("dev-a", 1_760_000_000);
    for (key, value) in [
        ("terminal_font_size", serde_json::json!("14")),
        ("ui_theme", serde_json::json!("no-such-theme")),
        ("terminal_scrollback", serde_json::json!(5)),
        ("ai_exec_mode", serde_json::json!("auto")),
        ("terminal_cursor_style", serde_json::json!("bar")),
    ] {
        snapshot.settings.insert(key.to_owned(), value);
    }

    let report = apply_snapshot(&db, &snapshot, &FakeSealer(0x5A), None, &[], 42).expect("导入");

    assert_eq!(report.settings_written, 1, "只有合格的那一个键算写入");
    let get = |key: &str| db.setting_get(key).unwrap();
    assert_eq!(get("terminal_font_size").as_deref(), Some("16"));
    assert_eq!(get("ui_theme").as_deref(), Some("\"graphite\""));
    assert_eq!(get("ai_exec_mode").as_deref(), Some("\"manual\""));
    assert_eq!(get("terminal_scrollback"), None);
    assert_eq!(get("terminal_cursor_style").as_deref(), Some("\"bar\""));
}
