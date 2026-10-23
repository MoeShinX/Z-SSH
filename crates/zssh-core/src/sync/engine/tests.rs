use std::io::Write;
use std::sync::{Arc, Mutex};

use super::*;
use crate::model::{AuthMethod, HostDraft, ProxyMode};
use crate::store::Database;
use crate::sync::sealer::TestSealer;
use crate::sync::storage::MemoryStorage;

const PASSWORD: &str = "test-master-password";

fn temp_dir(name: &str) -> crate::test_util::TestDir {
    crate::test_util::TestDir::new(&format!("sync-engine-{name}"))
}

fn add_host(db: &Database, name: &str) {
    db.host_save(&HostDraft {
        id: None,
        group_id: None,
        name: name.to_owned(),
        host: format!("{name}.example.internal"),
        port: 22,
        username: "root".to_owned(),
        auth_method: AuthMethod::Password,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: ProxyMode::Default,
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("建主机");
}

/// 把主机的修改时间钉死成一个固定的值。
///
/// 合并时“谁改得晚谁赢”，只有两边相差不超过 `CLOCK_SKEW_SECS`（2 秒）才按设备 ID 平局。
/// 修改时间是真实时钟（秒），测试里两次编辑之间隔着一次带 Argon2 的同步，机器忙的时候
/// 会超过 2 秒，结果就从“dev-a 赢”变成“后改的 B 赢”——这就是这个测试偶发失败的原因。
/// 所以要测平局规则的地方，两边的修改时间由测试自己定，不看真实时钟。
fn pin_host_time(db: &Database, at: i64) {
    db.with_conn_as(|conn| -> Result<(), crate::error::ZsshError> {
        conn.execute("UPDATE hosts SET updated_at = ?1", [at])?;
        Ok(())
    })
    .expect("钉住修改时间");
}

#[tokio::test]
async fn first_sync_uploads_then_the_second_computer_downloads() {
    let dir_a = temp_dir("first-a");
    let dir_b = temp_dir("first-b");
    let db_a = Database::open_in_memory().expect("A 库");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = TestSealer(1);
    let sealer_b = TestSealer(2);
    let storage = MemoryStorage::new();
    add_host(&db_a, "web-1");

    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    let report = engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 首次同步");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(storage.bytes().is_some(), "首次同步要把本地包传上去");

    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    let report = engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 首次同步");
    assert_eq!(report.status, SyncStatus::Completed);
    assert_eq!(db_b.host_list().expect("B 主机").len(), 1);
    assert_eq!(db_b.host_list().unwrap()[0].name, "web-1");
    assert!(engine_b.load_baseline().expect("B 基线").is_some());
}

#[tokio::test]
async fn two_computers_edit_and_converge_with_a_conflict() {
    let dir_a = temp_dir("edit-a");
    let dir_b = temp_dir("edit-b");
    let db_a = Database::open_in_memory().expect("A 库");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = TestSealer(1);
    let sealer_b = TestSealer(2);
    let storage = MemoryStorage::new();
    add_host(&db_a, "web-1");

    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 首次");
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 首次");

    // 两边各自改同一个名字，修改时间钉成同一个值（见 pin_host_time）：设备 ID 决定谁赢。
    let mut host = db_a.host_list().unwrap()[0].clone();
    host.name = "from-a".to_owned();
    db_a.host_save(&HostDraft {
        id: Some(host.id),
        group_id: None,
        name: host.name.clone(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: host.auth_method,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: host.proxy_mode,
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("A 改");
    let edit_time = SyncEngine::now() + 3600;
    pin_host_time(&db_a, edit_time);
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 改完同步");

    let mut host = db_b.host_list().unwrap()[0].clone();
    host.name = "from-b".to_owned();
    db_b.host_save(&HostDraft {
        id: Some(host.id),
        group_id: None,
        name: host.name.clone(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: host.auth_method,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: host.proxy_mode,
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("B 改");
    pin_host_time(&db_b, edit_time);
    let report = engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 同步");
    assert_eq!(report.conflicts.len(), 1, "两边都改要记冲突");
    let a_name = db_a.host_list().unwrap()[0].name.clone();

    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 再同步");
    assert_eq!(
        db_a.host_list().unwrap()[0].name,
        db_b.host_list().unwrap()[0].name,
        "最后两台电脑一致"
    );
    assert_eq!(db_a.host_list().unwrap()[0].name, a_name);
    assert_eq!(a_name, "from-a", "相同时间按设备 ID 小的 dev-a");
}

#[tokio::test]
async fn version_conflict_is_retried_with_reread_and_merge() {
    let dir = temp_dir("conflict");
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    add_host(&db, "web-1");
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine.sync(&storage, PASSWORD, None).await.expect("首次");

    storage.inject_write_conflicts(1);
    let report = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("冲突之后重试");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(report.retries >= 1, "应该记下重试次数");
    assert!(storage.current_version().is_some());

    storage.inject_write_timeouts(1);
    let report = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("写超时之后重试");
    assert!(report.retries >= 1);

    storage.inject_read_timeouts(1);
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("读超时之后重试");
}

#[tokio::test]
async fn half_written_upload_fails_and_does_not_update_the_baseline() {
    let dir = temp_dir("partial");
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    add_host(&db, "before");
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine.sync(&storage, PASSWORD, None).await.expect("首次");
    let baseline_before = engine.load_baseline().expect("基线").expect("有基线");

    let host = db.host_list().unwrap()[0].clone();
    db.host_save(&HostDraft {
        id: Some(host.id),
        group_id: None,
        name: "after".to_owned(),
        host: host.host,
        port: host.port,
        username: host.username,
        auth_method: host.auth_method,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: host.proxy_mode,
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("改主机");

    storage.inject_partial_writes(1);
    let err = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("写到一半要失败");
    assert!(err.to_string().contains("存储"), "{err}");
    assert_eq!(
        engine.load_baseline().unwrap().unwrap().hosts[0].name,
        baseline_before.hosts[0].name,
        "上传失败不能更新基线"
    );
    assert_eq!(db.host_list().unwrap()[0].name, "after", "本地改动还在");
}

#[tokio::test]
async fn too_new_package_is_rejected_and_not_overwritten() {
    let dir = temp_dir("toonew");
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    add_host(&db, "web-1");
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine.sync(&storage, PASSWORD, None).await.expect("首次");

    // 把云端包的版本号改成未来版本。
    let bytes = storage.bytes().expect("云端有包");
    let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    envelope["version"] = serde_json::json!(2);
    let too_new = serde_json::to_vec(&envelope).unwrap();
    storage.put_external(too_new.clone());

    let err = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect_err("版本太新要拒绝");
    assert!(matches!(err, SyncError::FormatTooNew { .. }), "{err}");
    assert_eq!(
        storage.bytes().as_deref(),
        Some(too_new.as_slice()),
        "不能覆盖"
    );
}

async fn guard_setup(
    dir_name: &str,
) -> (
    Database,
    crate::test_util::TestDir,
    TestSealer,
    MemoryStorage,
) {
    let dir = temp_dir(dir_name);
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    for name in ["web-1", "web-2", "web-3"] {
        add_host(&db, name);
    }
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine.sync(&storage, PASSWORD, None).await.expect("铺基线");

    // 云端版本删掉 2/3 主机。
    let mut cloud =
        crate::sync::snapshot::snapshot_local(&db, &sealer, "dev-b", None, SyncEngine::now())
            .expect("云端快照");
    cloud.hosts.truncate(1);
    storage.put_external(cloud.seal(PASSWORD).expect("加密"));
    (db, dir, sealer, storage)
}

#[tokio::test]
async fn mass_delete_needs_confirmation_then_continue_applies() {
    let (db, dir, sealer, storage) = guard_setup("guard-continue").await;
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    let cloud_before = storage.bytes().unwrap();

    let report = engine.sync(&storage, PASSWORD, None).await.expect("先问");
    assert_eq!(report.status, SyncStatus::NeedsConfirmation);
    assert!(report.summary.contains("删除"));
    assert_eq!(db.host_list().unwrap().len(), 3, "确认之前不能删本地");
    assert_eq!(storage.bytes().unwrap(), cloud_before, "确认之前不能改云端");

    let report = engine
        .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("用户选继续");
    assert_eq!(report.status, SyncStatus::Completed);
    assert_eq!(db.host_list().unwrap().len(), 1);
}

#[tokio::test]
async fn mass_delete_cancel_changes_nothing() {
    let (db, dir, sealer, storage) = guard_setup("guard-cancel").await;
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    let cloud_before = storage.bytes().unwrap();
    let hosts_before = db.host_list().unwrap().len();
    let report = engine
        .sync(&storage, PASSWORD, Some(DeletionDecision::Cancel))
        .await
        .expect("取消");
    assert_eq!(report.status, SyncStatus::Cancelled);
    assert_eq!(db.host_list().unwrap().len(), hosts_before);
    assert_eq!(storage.bytes().unwrap(), cloud_before);
}

#[tokio::test]
async fn mass_delete_use_cloud_overwrites_local() {
    let (db, dir, sealer, storage) = guard_setup("guard-cloud").await;
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    let report = engine
        .sync(&storage, PASSWORD, Some(DeletionDecision::UseCloud))
        .await
        .expect("用云端覆盖");
    assert_eq!(report.status, SyncStatus::Completed);
    assert_eq!(db.host_list().unwrap().len(), 1);
}

#[test]
fn export_and_import_do_not_duplicate_records() {
    let dir_a = temp_dir("import-a");
    let dir_b = temp_dir("import-b");
    let db_a = Database::open_in_memory().expect("A 库");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = TestSealer(1);
    let sealer_b = TestSealer(2);
    add_host(&db_a, "web-1");
    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    let package = engine_a.export_package(PASSWORD).expect("导出");

    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    let report = engine_b
        .import_package(&package, PASSWORD, None)
        .expect("导入");
    assert_eq!(report.status, SyncStatus::Completed);
    assert_eq!(db_b.host_list().unwrap().len(), 1);
    engine_b
        .import_package(&package, PASSWORD, None)
        .expect("再导入一次");
    assert_eq!(db_b.host_list().unwrap().len(), 1, "导入不能重复记录");
    assert!(engine_b.load_baseline().unwrap().is_some());

    // 反向导回去也不重复。
    let package_b = engine_b.export_package(PASSWORD).expect("B 导出");
    engine_a
        .import_package(&package_b, PASSWORD, None)
        .expect("A 导入");
    assert_eq!(db_a.host_list().unwrap().len(), 1);
}

// ---------- R-7 F07：设置只收白名单里的键，并检查类型和取值 ----------

/// 一个对方做得出来的、用正确主密码加密的包：里面塞了本机才有的设置、没人认识的键，
/// 以及取值不合法的同步键，另外夹着几个合格的键。
fn hostile_settings_package() -> Vec<u8> {
    let mut snapshot = SyncSnapshot::empty("dev-evil", 1_760_000_000);
    let put = |snapshot: &mut SyncSnapshot, key: &str, value: serde_json::Value| {
        snapshot.settings.insert(key.to_owned(), value);
    };
    // 只在本机的设置（权限、本机路径、同步引擎内部状态）和一个不认识的键。
    put(&mut snapshot, "ai_exec_mode", serde_json::json!("auto"));
    put(
        &mut snapshot,
        "ai_codex_path",
        serde_json::json!("/evil/codex"),
    );
    put(
        &mut snapshot,
        "ai_opencode_path",
        serde_json::json!("/evil/opencode"),
    );
    put(
        &mut snapshot,
        "ai_claude_path",
        serde_json::json!("/evil/claude"),
    );
    put(
        &mut snapshot,
        "sync_device_id",
        serde_json::json!("dev-evil"),
    );
    put(
        &mut snapshot,
        "sync_last_backup_version",
        serde_json::json!("999"),
    );
    put(
        &mut snapshot,
        "sync_storage",
        serde_json::json!("{\"kind\":\"evil\"}"),
    );
    put(&mut snapshot, "totally_unknown_key", serde_json::json!("x"));
    // 允许同步、但取值不合法的键：每一种错法各一个。
    put(
        &mut snapshot,
        "ui_theme",
        serde_json::json!("no-such-theme"),
    );
    put(&mut snapshot, "terminal_theme", serde_json::json!(42));
    put(&mut snapshot, "terminal_font_size", serde_json::json!("14"));
    put(&mut snapshot, "terminal_scrollback", serde_json::json!(5));
    put(
        &mut snapshot,
        "terminal_line_height",
        serde_json::json!(9.9),
    );
    put(
        &mut snapshot,
        "terminal_cursor_style",
        serde_json::json!("triangle"),
    );
    put(
        &mut snapshot,
        "terminal_cursor_blink",
        serde_json::json!("yes"),
    );
    put(&mut snapshot, "ai_default_agent", serde_json::json!(7));
    put(&mut snapshot, "sftp_concurrency", serde_json::json!(99));
    put(&mut snapshot, "ai_write_backup_keep", serde_json::json!(-3));
    // 合格的键：必须正常生效，不能被同一个包里的坏键连累。
    put(
        &mut snapshot,
        "terminal_font_family",
        serde_json::json!("Fira Code, monospace"),
    );
    put(
        &mut snapshot,
        "sftp_preserve_mtime",
        serde_json::json!(true),
    );
    put(
        &mut snapshot,
        "terminal_copy_on_select",
        serde_json::json!(true),
    );
    snapshot.seal(PASSWORD).expect("封包")
}

fn setting(db: &Database, key: &str) -> Option<String> {
    db.setting_get(key).expect("读设置")
}

#[test]
fn importing_a_package_ignores_local_only_unknown_and_invalid_settings() {
    let dir = temp_dir("f07-import");
    let db = Database::open_in_memory().expect("本机库");
    let sealer = TestSealer(2);
    // 本机现有的值：导入以后一个都不能变。
    let local: [(&str, &str); 14] = [
        ("ai_exec_mode", "\"manual\""),
        ("ai_codex_path", "\"/local/codex\""),
        ("ai_opencode_path", "\"\""),
        ("ai_claude_path", "\"\""),
        ("sync_last_backup_version", "\"7\""),
        ("ui_theme", "\"graphite\""),
        ("terminal_theme", "\"system-nord-midnight-dark\""),
        ("terminal_font_size", "16"),
        ("terminal_scrollback", "20000"),
        ("terminal_line_height", "1.2"),
        ("terminal_cursor_style", "\"underline\""),
        ("terminal_cursor_blink", "false"),
        ("sftp_concurrency", "4"),
        ("ai_write_backup_keep", "8"),
    ];
    for (key, value) in local {
        db.setting_set(key, value).expect("写本机设置");
    }
    db.setting_set("ai_default_agent", "\"codex\"").unwrap();
    let device_before = db.sync_device_id().expect("本机设备 ID");
    let engine = SyncEngine::new(&db, &sealer, "dev-b", &dir, None, None);

    let report = engine
        .import_package(&hostile_settings_package(), PASSWORD, None)
        .expect("导入");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");

    for (key, value) in local {
        assert_eq!(
            setting(&db, key).as_deref(),
            Some(value),
            "{key} 不能被同步包改掉"
        );
    }
    assert_eq!(
        setting(&db, "ai_default_agent").as_deref(),
        Some("\"codex\"")
    );
    assert_eq!(
        db.sync_device_id().unwrap(),
        device_before,
        "同步设备 ID 不能变"
    );
    assert_eq!(
        setting(&db, "sync_storage"),
        None,
        "同步连接信息不能被写进来"
    );
    assert_eq!(
        setting(&db, "totally_unknown_key"),
        None,
        "不认识的键不能写进来"
    );

    // 合格的键正常生效。
    assert_eq!(
        setting(&db, "terminal_font_family").as_deref(),
        Some("\"Fira Code, monospace\"")
    );
    assert_eq!(setting(&db, "sftp_preserve_mtime").as_deref(), Some("true"));
    assert_eq!(
        setting(&db, "terminal_copy_on_select").as_deref(),
        Some("true")
    );

    // 基线（下一次同步的共同祖先）：本机键不能进；不认识的键原样留着（以后升级了才能用上）；
    // 对方给的、本版本不接受的值也留着（本机没改过就不能拿本机的旧值去覆盖它），但上面已经
    // 核对过它们没有写进本机。
    let baseline = engine.load_baseline().expect("读基线").expect("有基线");
    for key in baseline.settings.keys() {
        assert_ne!(
            crate::sync::settings::SettingsPolicy::CURRENT.class(key),
            crate::sync::settings::KeyClass::LocalOnly,
            "基线里不该有只在本机的键：{key}"
        );
    }
    assert!(!baseline.settings.contains_key("ai_exec_mode"));
    assert!(!baseline.settings.contains_key("sync_device_id"));
    assert!(baseline.settings.contains_key("totally_unknown_key"));
    assert_eq!(
        baseline.settings.get("ui_theme"),
        Some(&serde_json::json!("no-such-theme")),
        "对方的值留在基线里，本机的值不动"
    );
}

#[test]
fn importing_invalid_settings_into_an_empty_database_writes_nothing_for_them() {
    let dir = temp_dir("f07-import-empty");
    let db = Database::open_in_memory().expect("本机库");
    let sealer = TestSealer(2);
    let engine = SyncEngine::new(&db, &sealer, "dev-b", &dir, None, None);
    engine
        .import_package(&hostile_settings_package(), PASSWORD, None)
        .expect("导入");
    for key in [
        "ai_exec_mode",
        "ai_codex_path",
        "sync_last_backup_version",
        "totally_unknown_key",
        "ui_theme",
        "terminal_theme",
        "terminal_font_size",
        "terminal_scrollback",
        "terminal_line_height",
        "terminal_cursor_style",
        "terminal_cursor_blink",
        "ai_default_agent",
        "sftp_concurrency",
        "ai_write_backup_keep",
    ] {
        assert_eq!(
            setting(&db, key),
            None,
            "{key} 本机没有，包里的值不合格，不能写进来"
        );
    }
    assert_eq!(
        setting(&db, "terminal_font_family").as_deref(),
        Some("\"Fira Code, monospace\"")
    );
}

#[tokio::test]
async fn sync_with_a_hostile_cloud_package_keeps_local_settings() {
    let dir = temp_dir("f07-sync");
    let db = Database::open_in_memory().expect("本机库");
    let sealer = TestSealer(2);
    db.setting_set("ai_exec_mode", "\"manual\"").unwrap();
    db.setting_set("ui_theme", "\"graphite\"").unwrap();
    let storage = MemoryStorage::with_bytes(hostile_settings_package());
    let engine = SyncEngine::new(&db, &sealer, "dev-b", &dir, None, None);

    let report = engine.sync(&storage, PASSWORD, None).await.expect("同步");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");
    assert_eq!(setting(&db, "ai_exec_mode").as_deref(), Some("\"manual\""));
    assert_eq!(setting(&db, "ui_theme").as_deref(), Some("\"graphite\""));
    assert_eq!(setting(&db, "totally_unknown_key"), None);
    assert_eq!(setting(&db, "sftp_preserve_mtime").as_deref(), Some("true"));

    // 本机传回云端的包里不能带着只在本机的键；不认识的键原样留着。
    let uploaded = storage.bytes().expect("云端有包");
    let uploaded = SyncSnapshot::open(&uploaded, PASSWORD).expect("解开");
    for key in uploaded.settings.keys() {
        assert_ne!(
            crate::sync::settings::SettingsPolicy::CURRENT.class(key),
            crate::sync::settings::KeyClass::LocalOnly,
            "上传的包里不该有只在本机的键：{key}"
        );
    }
    assert!(uploaded.settings.contains_key("totally_unknown_key"));
}

// ---------- R-7b：新旧版本的电脑一起同步时，设置不能被旧版本删掉或改回去 ----------
//
// 用两套规则模拟两个版本：`SettingsPolicy::CURRENT` 是“旧版本”（v0.1）；`NEWER` 是“新版本”
// （v0.2）：多认识一个同步键、多认识一个主题 id、还把原来只在本机的 `sftp_show_hidden` 改成了同步。

use crate::sync::package::SyncSnapshot as Snapshot;
use crate::sync::settings::{validate_value, KeyClass, SettingsPolicy};
use serde_json::Value;

/// 旧版本完全不认识的新同步键。
const NEWER_ONLY_FLAG: &str = "terminal_ligatures";
/// 旧版本的主题列表里没有的新主题。
const NEW_THEME: &str = "aurora";

fn newer_class(key: &str) -> KeyClass {
    match key {
        NEWER_ONLY_FLAG | "sftp_show_hidden" => KeyClass::Synced,
        other => (SettingsPolicy::CURRENT.class_of)(other),
    }
}

fn newer_check(key: &str, value: &Value) -> Result<(), String> {
    match key {
        NEWER_ONLY_FLAG | "sftp_show_hidden" if value.is_boolean() => Ok(()),
        NEWER_ONLY_FLAG | "sftp_show_hidden" => Err("要是 true / false".to_owned()),
        "ui_theme" if value == NEW_THEME => Ok(()),
        _ => validate_value(key, value),
    }
}

const OLD: SettingsPolicy = SettingsPolicy::CURRENT;
const NEWER: SettingsPolicy = SettingsPolicy {
    class_of: newer_class,
    check_value: newer_check,
};

struct Machine {
    db: Database,
    dir: crate::test_util::TestDir,
    sealer: TestSealer,
    device: &'static str,
}

impl Machine {
    fn new(name: &str, device: &'static str, seal: u8) -> Self {
        Self {
            db: Database::open_in_memory().expect("库"),
            dir: temp_dir(name),
            sealer: TestSealer(seal),
            device,
        }
    }

    fn engine(&self, policy: SettingsPolicy) -> SyncEngine<'_> {
        SyncEngine::new(&self.db, &self.sealer, self.device, &self.dir, None, None)
            .with_policy(policy)
    }

    async fn sync(&self, policy: SettingsPolicy, storage: &MemoryStorage) -> SyncReport {
        let report = self
            .engine(policy)
            .sync(storage, PASSWORD, None)
            .await
            .expect("同步");
        assert_eq!(report.status, SyncStatus::Completed, "{report:?}");
        report
    }

    fn get(&self, key: &str) -> Option<String> {
        self.db.setting_get(key).expect("读设置")
    }

    fn set(&self, key: &str, raw: &str) {
        self.db.setting_set(key, raw).expect("写设置");
    }
}

fn cloud(storage: &MemoryStorage) -> Snapshot {
    Snapshot::open(&storage.bytes().expect("云端有包"), PASSWORD).expect("解开云端包")
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

#[tokio::test]
async fn setting_unknown_to_an_older_version_survives_it_and_reaches_other_new_machines() {
    let storage = MemoryStorage::new();
    let a = Machine::new("r7b-1-a", "dev-a", 1); // 新版本
    let b = Machine::new("r7b-1-b", "dev-b", 2); // 旧版本
    let c = Machine::new("r7b-1-c", "dev-c", 3); // 另一台新版本
    a.set(NEWER_ONLY_FLAG, "true");

    a.sync(NEWER, &storage).await;
    b.sync(OLD, &storage).await;
    assert_eq!(
        cloud(&storage).settings.get(NEWER_ONLY_FLAG),
        Some(&serde_json::json!(true)),
        "旧版本不认识这个键，也要把它原样留在上传的包里"
    );
    assert_eq!(
        b.get(NEWER_ONLY_FLAG),
        None,
        "旧版本永远不把不认识的键写进本机"
    );
    assert_eq!(
        b.engine(OLD)
            .load_baseline()
            .unwrap()
            .unwrap()
            .settings
            .get(NEWER_ONLY_FLAG),
        Some(&serde_json::json!(true)),
        "基线里也要保留"
    );

    // 旧版本同步之后、A 再次同步之前，另一台新版本电脑也要能拿到这个设置。
    c.sync(NEWER, &storage).await;
    assert_eq!(
        c.get(NEWER_ONLY_FLAG).as_deref(),
        Some("true"),
        "C 应该拿到新设置"
    );

    for round in 1..=3 {
        a.sync(NEWER, &storage).await;
        assert_eq!(
            a.get(NEWER_ONLY_FLAG).as_deref(),
            Some("true"),
            "第 {round} 轮：A 本机的值不变"
        );
        b.sync(OLD, &storage).await;
        assert_eq!(
            b.get(NEWER_ONLY_FLAG),
            None,
            "第 {round} 轮：旧版本本机没有这个键"
        );
        assert_eq!(
            cloud(&storage).settings.get(NEWER_ONLY_FLAG),
            Some(&serde_json::json!(true)),
            "第 {round} 轮：云端的值不变"
        );
    }
}

/// A 新版本先用 `midnight` 同步过，B 旧版本拿到了；随后 A 把主题改成旧版本不认识的 `aurora`。
async fn theme_scenario_prologue(storage: &MemoryStorage) -> (Machine, Machine) {
    let a = Machine::new("r7b-2-a", "dev-a", 1);
    let b = Machine::new("r7b-2-b", "dev-b", 2);
    a.set("ui_theme", &quoted("midnight"));
    a.sync(NEWER, storage).await;
    b.sync(OLD, storage).await;
    assert_eq!(b.get("ui_theme").as_deref(), Some("\"midnight\""));
    a.set("ui_theme", &quoted(NEW_THEME));
    a.sync(NEWER, storage).await;
    (a, b)
}

#[tokio::test]
async fn new_value_of_a_known_key_is_not_reverted_by_an_older_version() {
    let storage = MemoryStorage::new();
    let (a, b) = theme_scenario_prologue(&storage).await;

    for round in 1..=3 {
        b.sync(OLD, &storage).await;
        assert_eq!(
            b.get("ui_theme").as_deref(),
            Some("\"midnight\""),
            "第 {round} 轮：旧版本本机不能被改成它不认识的主题"
        );
        assert_eq!(
            cloud(&storage).settings.get("ui_theme"),
            Some(&serde_json::json!(NEW_THEME)),
            "第 {round} 轮：旧版本没改过主题，不能用自己的值覆盖云端"
        );
        a.sync(NEWER, &storage).await;
        assert_eq!(
            a.get("ui_theme"),
            Some(quoted(NEW_THEME)),
            "第 {round} 轮：A 的主题不变"
        );
    }
}

#[tokio::test]
async fn older_version_first_sync_does_not_overwrite_a_value_it_cannot_apply() {
    let storage = MemoryStorage::new();
    let a = Machine::new("r7b-2b-a", "dev-a", 1);
    let b = Machine::new("r7b-2b-b", "dev-b", 2);
    a.set("ui_theme", &quoted(NEW_THEME));
    a.sync(NEWER, &storage).await;
    // B 还没有同步过（没有基线），本机主题和 A 的不一样。
    b.set("ui_theme", &quoted("graphite"));

    for round in 1..=3 {
        b.sync(OLD, &storage).await;
        assert_eq!(
            b.get("ui_theme").as_deref(),
            Some("\"graphite\""),
            "第 {round} 轮"
        );
        assert_eq!(
            cloud(&storage).settings.get("ui_theme"),
            Some(&serde_json::json!(NEW_THEME)),
            "第 {round} 轮：云端的主题不变"
        );
        a.sync(NEWER, &storage).await;
        assert_eq!(a.get("ui_theme"), Some(quoted(NEW_THEME)), "第 {round} 轮");
    }
}

#[tokio::test]
async fn user_change_on_the_older_version_still_syncs_normally() {
    let storage = MemoryStorage::new();
    let (a, b) = theme_scenario_prologue(&storage).await;
    b.sync(OLD, &storage).await; // 先同步一轮：B 拒绝 aurora，记下“没写进本机”

    // B 的用户主动改了主题。
    b.set("ui_theme", &quoted("obsidian"));
    b.sync(OLD, &storage).await;
    assert_eq!(
        cloud(&storage).settings.get("ui_theme"),
        Some(&serde_json::json!("obsidian")),
        "用户真的改了，就按正常规则上传"
    );
    a.sync(NEWER, &storage).await;
    assert_eq!(
        a.get("ui_theme").as_deref(),
        Some("\"obsidian\""),
        "A 收到 B 的新主题"
    );

    for round in 1..=2 {
        b.sync(OLD, &storage).await;
        a.sync(NEWER, &storage).await;
        assert_eq!(
            b.get("ui_theme").as_deref(),
            Some("\"obsidian\""),
            "第 {round} 轮"
        );
        assert_eq!(
            a.get("ui_theme").as_deref(),
            Some("\"obsidian\""),
            "第 {round} 轮"
        );
    }
}

#[tokio::test]
async fn older_version_that_upgrades_picks_up_the_value_it_had_to_decline() {
    let storage = MemoryStorage::new();
    let (a, b) = theme_scenario_prologue(&storage).await;
    b.sync(OLD, &storage).await;
    assert_eq!(b.get("ui_theme").as_deref(), Some("\"midnight\""));

    // B 升级到新版本：同一份数据目录，换成新规则。
    b.sync(NEWER, &storage).await;
    assert_eq!(
        b.get("ui_theme"),
        Some(quoted(NEW_THEME)),
        "升级后应该拿到 aurora"
    );
    a.sync(NEWER, &storage).await;
    assert_eq!(a.get("ui_theme"), Some(quoted(NEW_THEME)));
    assert_eq!(
        cloud(&storage).settings.get("ui_theme"),
        Some(&serde_json::json!(NEW_THEME))
    );
}

#[tokio::test]
async fn key_local_only_in_the_older_version_is_restored_by_the_newer_machine() {
    // `sftp_show_hidden` 在旧版本里是“只在本机”，在新版本里改成了同步。
    let storage = MemoryStorage::new();
    let a = Machine::new("r7b-5-a", "dev-a", 1);
    let b = Machine::new("r7b-5-b", "dev-b", 2);
    a.set("sftp_show_hidden", "true");
    a.sync(NEWER, &storage).await;
    b.sync(OLD, &storage).await;
    assert_eq!(b.get("sftp_show_hidden"), None, "旧版本不把本机键写进本机");
    for round in 1..=3 {
        a.sync(NEWER, &storage).await;
        assert_eq!(
            a.get("sftp_show_hidden").as_deref(),
            Some("true"),
            "第 {round} 轮"
        );
        b.sync(OLD, &storage).await;
        assert_eq!(b.get("sftp_show_hidden"), None, "第 {round} 轮");
    }
    assert_eq!(
        cloud(&storage).settings.get("sftp_show_hidden"),
        None,
        "旧版本把本机键从上传的包里去掉（审计要求，不放宽）"
    );
}

#[test]
fn oversized_or_too_many_unknown_settings_are_dropped_and_the_rest_still_import() {
    let machine = Machine::new("r7b-4", "dev-b", 2);
    let mut snapshot = Snapshot::empty("dev-evil", 1_760_000_000);
    for index in 0..250 {
        snapshot
            .settings
            .insert(format!("future_key_{index:03}"), serde_json::json!(index));
    }
    snapshot
        .settings
        .insert("future_big".to_owned(), serde_json::json!("x".repeat(5000)));
    snapshot
        .settings
        .insert("ai_exec_mode".to_owned(), serde_json::json!("auto"));
    snapshot
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("graphite"));
    let package = snapshot.seal(PASSWORD).expect("封包");

    let report = machine
        .engine(OLD)
        .import_package(&package, PASSWORD, None)
        .expect("导入");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");

    let baseline = machine.engine(OLD).load_baseline().unwrap().unwrap();
    let unknown: Vec<&String> = baseline
        .settings
        .keys()
        .filter(|key| key.starts_with("future_"))
        .collect();
    assert_eq!(unknown.len(), 200, "不认识的键最多留 200 个");
    assert!(baseline.settings.contains_key("future_key_199"));
    assert!(!baseline.settings.contains_key("future_key_200"));
    assert!(
        !baseline.settings.contains_key("future_big"),
        "超过 4 KB 的值丢掉"
    );
    assert!(
        !baseline.settings.contains_key("ai_exec_mode"),
        "本机键不进基线"
    );
    assert_eq!(
        machine.get("ui_theme").as_deref(),
        Some("\"graphite\""),
        "其余正常导入"
    );
    assert_eq!(machine.get("ai_exec_mode"), None);
    assert_eq!(
        machine.get("future_key_000"),
        None,
        "不认识的键永远不写进本机"
    );
}

// ---------- 日志里不能出现密码 / 私钥 / 同步包明文 ----------

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(self.0.clone())
    }
}

#[cfg(windows)]
#[test]
fn logs_never_contain_passwords_keys_or_package_plaintext() {
    let dir_a = temp_dir("logs-a");
    let dir_b = temp_dir("logs-b");
    let db_a = Database::open_in_memory().expect("A 库");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = crate::sync::sealer::DpapiSealer;
    let sealer_b = TestSealer(2);
    add_host(&db_a, "PLAINTEXT-HOST-MARKER");
    db_a.identity_save(&crate::model::IdentityDraft {
        id: None,
        name: "测试私钥".to_owned(),
        kind: crate::model::IdentityKind::KeyInline,
        key_path: None,
        secret: Some("PRIVATE-KEY-MARKER".to_owned()),
        passphrase: Some("KEY-PASSPHRASE-MARKER".to_owned()),
    })
    .expect("凭据");

    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    let captured = Arc::new(Mutex::new(Vec::new()));
    let make_subscriber = || {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(Capture(captured.clone()))
            .finish()
    };

    // 用全局订阅者而不是线程局部的：同一个测试进程里别的用例可能先触发过这些
    // callsite（当时没有订阅者），线程局部订阅者收不到已经被缓存的 callsite。
    let installed = tracing::subscriber::set_global_default(make_subscriber()).is_ok();
    let run = || {
        let package = engine_a
            .export_package("MASTER-PASSWORD-MARKER")
            .expect("导出");
        let report = engine_b
            .import_package(&package, "MASTER-PASSWORD-MARKER", None)
            .expect("导入");
        assert_eq!(report.status, SyncStatus::Completed, "{:?}", report);
        assert_eq!(db_b.host_list().expect("B 主机").len(), 1);
        let err = engine_b
            .import_package(&package, "WRONG-PASSWORD-MARKER", None)
            .expect_err("错误密码");
        assert!(matches!(err, SyncError::WrongPassword), "{err}");
    };
    if installed {
        tracing::callsite::rebuild_interest_cache();
        run();
    } else {
        tracing::subscriber::with_default(make_subscriber(), || {
            tracing::callsite::rebuild_interest_cache();
            run();
        });
    }

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(
        logs.contains("同步包已导入"),
        "导入流程自己的日志没抓到：{logs:?}"
    );
    for secret in [
        "MASTER-PASSWORD-MARKER",
        "WRONG-PASSWORD-MARKER",
        "PRIVATE-KEY-MARKER",
        "KEY-PASSPHRASE-MARKER",
        "PLAINTEXT-HOST-MARKER",
    ] {
        assert!(!logs.contains(secret), "日志里出现了 {secret}：\n{logs}");
    }
}

/// 写云端之前先执行一段回调，模拟“用户在同步进行到一半时改了本地”。
struct HookedStorage {
    inner: MemoryStorage,
    before_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl SyncStorage for HookedStorage {
    fn read<'a>(
        &'a self,
    ) -> crate::ssh::BoxFuture<'a, Result<Option<crate::sync::storage::StoredObject>, StorageError>>
    {
        self.inner.read()
    }
    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> crate::ssh::BoxFuture<'a, Result<String, StorageError>> {
        let hook = self.before_write.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        self.inner.write(bytes, expected_version)
    }
    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> crate::ssh::BoxFuture<'a, Result<(), StorageError>> {
        self.inner.delete(expected_version)
    }
}

fn rename_host(db: &Database, from: &str, to: &str) {
    let host = db
        .host_list()
        .unwrap()
        .into_iter()
        .find(|host| host.name == from)
        .expect("要改的主机");
    db.host_save(&HostDraft {
        id: Some(host.id),
        group_id: None,
        name: to.to_owned(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: host.auth_method,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: host.proxy_mode,
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("改主机");
}

fn sorted_names(db: &Database) -> Vec<String> {
    let mut names: Vec<String> = db
        .host_list()
        .unwrap()
        .into_iter()
        .map(|host| host.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn local_changes_made_during_a_sync_are_kept_and_synced_next_round() {
    let dir_a = temp_dir("race-a");
    let dir_b = temp_dir("race-b");
    let db_a = std::sync::Arc::new(Database::open_in_memory().expect("A 库"));
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = TestSealer(1);
    let sealer_b = TestSealer(2);
    add_host(&db_a, "web-1");
    add_host(&db_a, "web-2");

    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    let storage = HookedStorage {
        inner: MemoryStorage::new(),
        before_write: Mutex::new(None),
    };
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 首次");
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 首次");

    // B 先改了云端，让 A 下一次同步需要写本地。
    add_host(&db_b, "from-b");
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 推送");

    // A 同步到一半（写云端时）：用户新建一台主机、改了另一台。
    let hook_db = db_a.clone();
    *storage.before_write.lock().unwrap() = Some(Box::new(move || {
        add_host(&hook_db, "created-during-sync");
        rename_host(&hook_db, "web-2", "edited-during-sync");
    }));
    let report = engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 同步");
    assert_eq!(report.status, SyncStatus::LocalChanged);
    assert!(report.applied.is_none(), "本地一个字也不能改");
    assert_eq!(
        sorted_names(&db_a),
        vec!["created-during-sync", "edited-during-sync", "web-1"],
        "新建的和修改的都还在，云端的 from-b 还没写进来"
    );

    // 基线没有存新的：再同步一轮，用户的改动按“相对旧基线的改动”合进去。
    let report = engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 再同步");
    assert_eq!(report.status, SyncStatus::Completed);
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 再同步");
    let expected = vec![
        "created-during-sync",
        "edited-during-sync",
        "from-b",
        "web-1",
    ];
    assert_eq!(sorted_names(&db_a), expected);
    assert_eq!(sorted_names(&db_b), expected);
}

#[test]
fn import_package_also_refuses_when_local_changed_in_between() {
    // apply 层直接验证：期望的本地状态和库里不一致时，库和 known_hosts 都不动。
    use crate::sync::apply::{apply_snapshot_guarded, LocalGuard};
    use crate::sync::snapshot::snapshot_local;
    let db = Database::open_in_memory().expect("库");
    add_host(&db, "web-1");
    let sealer = TestSealer(1);
    let expected = snapshot_local(&db, &sealer, "dev", None, 10).expect("快照");
    add_host(&db, "late");
    let empty = crate::sync::SyncSnapshot::empty("dev", 10);
    let err = apply_snapshot_guarded(
        &db,
        &empty,
        &sealer,
        None,
        &[],
        10,
        Some(LocalGuard {
            expected: &expected,
            device_id: "dev",
        }),
    )
    .expect_err("本地变了要拒绝");
    assert!(matches!(err, SyncError::LocalChanged));
    assert_eq!(sorted_names(&db), vec!["late", "web-1"]);
}

fn rename_only_host(db: &Database, name: &str) {
    let host = db.host_list().unwrap()[0].clone();
    db.host_save(&HostDraft {
        id: Some(host.id),
        group_id: host.group_id,
        name: name.to_owned(),
        host: host.host.clone(),
        port: host.port,
        username: host.username.clone(),
        auth_method: host.auth_method,
        identity_id: host.identity_id,
        proxy_id: host.proxy_id,
        jump_host_id: host.jump_host_id,
        proxy_mode: host.proxy_mode,
        tags: host.tags.clone(),
        sort: host.sort,
        legacy_algorithms: host.legacy_algorithms,
    })
    .expect("改名");
}

/// R-10 H4：恢复本地备份之后，下一次同步要把恢复的内容推上云端，而不是被云端覆盖回去。
#[tokio::test]
async fn a_restored_backup_survives_the_next_sync() {
    let dir = temp_dir("restore-survives");
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    add_host(&db, "web-1");
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("首次同步");

    // 备份"web-1"这个状态。
    let snapshot = engine.local(SyncEngine::now()).expect("本地快照");
    let backup = engine
        .backup_store()
        .create(&snapshot, &Default::default(), SyncEngine::now(), 20, None)
        .expect("建备份");

    // 之后改了名并同步上云：云端和基线都是 renamed。
    rename_only_host(&db, "renamed");
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("改名后同步");

    // 用户发现改错了，恢复备份；然后自动同步照常跑。
    engine
        .restore_backup(std::path::Path::new(&backup.path))
        .expect("恢复");
    assert_eq!(db.host_list().unwrap()[0].name, "web-1");
    let report = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("恢复后同步");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");
    assert_eq!(
        db.host_list().unwrap()[0].name,
        "web-1",
        "恢复的内容不能被云端覆盖回去"
    );

    // 另一台电脑拿到的也是恢复后的内容。
    let dir_b = temp_dir("restore-survives-b");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_b = TestSealer(2);
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 同步");
    assert_eq!(db_b.host_list().unwrap()[0].name, "web-1");
}

/// R7-02：恢复的备份里有一台之后被删（并已同步）的主机，恢复后再同步，它不能又被墓碑删掉；
/// 另一台电脑同步后也要看到它，引用（所属分组）保持一致。
#[tokio::test]
async fn restoring_a_backup_with_a_deleted_host_survives_the_next_sync() {
    let dir = temp_dir("restore-deleted");
    let db = Database::open_in_memory().expect("库");
    let sealer = TestSealer(1);
    let storage = MemoryStorage::new();
    for name in ["web-a", "web-b", "web-c"] {
        add_host(&db, name);
    }
    let engine = SyncEngine::new(&db, &sealer, "dev-a", &dir, None, None);
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("首次同步");

    let snapshot = engine.local(SyncEngine::now()).expect("本地快照");
    let backup = engine
        .backup_store()
        .create(&snapshot, &Default::default(), SyncEngine::now(), 20, None)
        .expect("建备份");

    // 删掉 web-a 并同步：云端、基线、本机都有它的墓碑。
    let doomed = db
        .host_list()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "web-a")
        .expect("web-a");
    db.host_delete(doomed.id).expect("删除");
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("删除后同步");
    assert_eq!(db.host_list().unwrap().len(), 2);

    // 恢复备份：web-a 回来了；再同步，它不能被墓碑删掉。
    engine
        .restore_backup(std::path::Path::new(&backup.path))
        .expect("恢复");
    let names = |db: &Database| {
        let mut names: Vec<String> = db
            .host_list()
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(&db), ["web-a", "web-b", "web-c"]);
    let report = engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("恢复后同步");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");
    assert_eq!(
        names(&db),
        ["web-a", "web-b", "web-c"],
        "恢复的主机不能再被同步删掉"
    );

    // 再同步一次也稳定。
    engine.sync(&storage, PASSWORD, None).await.expect("再同步");
    assert_eq!(names(&db), ["web-a", "web-b", "web-c"]);

    // 另一台电脑同步后拿到的是恢复后的内容，而且不会把恢复撤销。
    let dir_b = temp_dir("restore-deleted-b");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_b = TestSealer(2);
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 同步");
    assert_eq!(names(&db_b), ["web-a", "web-b", "web-c"]);
    engine
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 再同步");
    assert_eq!(names(&db), ["web-a", "web-b", "web-c"]);
}

/// 按顺序规则（sort > 0 在前按 sort，sort == 0 在后按名字）排出某一层的主机名。
fn layer_names(db: &Database, group_id: Option<i64>) -> Vec<String> {
    let mut hosts: Vec<_> = db
        .host_list()
        .unwrap()
        .into_iter()
        .filter(|host| host.group_id == group_id)
        .collect();
    hosts.sort_by(|a, b| {
        (a.sort == 0, a.sort, a.name.clone()).cmp(&(b.sort == 0, b.sort, b.name.clone()))
    });
    hosts.into_iter().map(|host| host.name).collect()
}

fn root_group_names(db: &Database) -> Vec<String> {
    let mut groups: Vec<_> = db
        .group_list()
        .unwrap()
        .into_iter()
        .filter(|group| group.parent_id.is_none())
        .collect();
    groups.sort_by(|a, b| {
        (a.sort == 0, a.sort, a.name.clone()).cmp(&(b.sort == 0, b.sort, b.name.clone()))
    });
    groups.into_iter().map(|group| group.name).collect()
}

#[tokio::test]
async fn manual_host_and_group_order_syncs_to_the_other_computer() {
    use crate::model::GroupDraft;

    let dir_a = temp_dir("order-a");
    let dir_b = temp_dir("order-b");
    let db_a = Database::open_in_memory().expect("A 库");
    let db_b = Database::open_in_memory().expect("B 库");
    let sealer_a = TestSealer(1);
    let sealer_b = TestSealer(2);
    let storage = MemoryStorage::new();

    let group_id = |db: &Database, name: &str| {
        db.group_save(&GroupDraft {
            id: None,
            name: name.to_owned(),
            parent_id: None,
            sort: 0,
        })
        .expect("建分组")
        .id
    };
    let alpha = group_id(&db_a, "alpha");
    let beta = group_id(&db_a, "beta");
    let mut ids = Vec::new();
    for name in ["h1", "h2", "h3"] {
        let draft = HostDraft {
            id: None,
            group_id: Some(alpha),
            name: name.to_owned(),
            host: format!("{name}.example.internal"),
            port: 22,
            username: "root".to_owned(),
            auth_method: AuthMethod::Password,
            identity_id: None,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
        };
        ids.push(db_a.host_save(&draft).expect("建主机").id);
    }

    let engine_a = SyncEngine::new(&db_a, &sealer_a, "dev-a", &dir_a, None, None);
    let engine_b = SyncEngine::new(&db_b, &sealer_b, "dev-b", &dir_b, None, None);
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 首次");
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 首次");
    let b_alpha = db_b
        .group_list()
        .unwrap()
        .into_iter()
        .find(|group| group.name == "alpha")
        .expect("B 收到分组")
        .id;
    assert_eq!(
        layer_names(&db_b, Some(b_alpha)),
        ["h1", "h2", "h3"],
        "没调过时按名字"
    );

    // A 调整顺序：主机 h3 h1 h2，分组 beta alpha。修改时间钉在未来，免得和同一秒里的建库平局。
    db_a.host_reorder(&[ids[2], ids[0], ids[1]])
        .expect("重排主机");
    db_a.group_reorder(&[beta, alpha]).expect("重排分组");
    let edit_time = SyncEngine::now() + 3600;
    db_a.with_conn_as(|conn| -> Result<(), crate::error::ZsshError> {
        conn.execute(
            "UPDATE hosts SET updated_at = ?1 WHERE sort != 0",
            [edit_time],
        )?;
        conn.execute(
            "UPDATE groups SET updated_at = ?1 WHERE sort != 0",
            [edit_time],
        )?;
        Ok(())
    })
    .expect("钉住修改时间");
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 重排后同步");
    let report = engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 同步");
    assert_eq!(report.status, SyncStatus::Completed, "{report:?}");

    assert_eq!(
        layer_names(&db_b, Some(b_alpha)),
        ["h3", "h1", "h2"],
        "另一台电脑的主机顺序跟着变"
    );
    assert_eq!(root_group_names(&db_b), ["beta", "alpha"]);
    assert_eq!(
        layer_names(&db_a, Some(alpha)),
        layer_names(&db_b, Some(b_alpha)),
        "两台电脑一致"
    );

    // 再同步一轮也稳定：顺序不会被撤销。
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 再同步");
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 再同步");
    assert_eq!(layer_names(&db_a, Some(alpha)), ["h3", "h1", "h2"]);
    assert_eq!(layer_names(&db_b, Some(b_alpha)), ["h3", "h1", "h2"]);
}
