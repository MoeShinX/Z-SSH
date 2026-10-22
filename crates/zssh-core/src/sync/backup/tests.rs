use super::*;
use crate::store::Database;
use crate::sync::apply::apply_snapshot;
use crate::sync::package::{HostRecord, SyncSnapshot};
use crate::sync::sealer::TestSealer;

fn temp_dir(name: &str) -> crate::test_util::TestDir {
    crate::test_util::TestDir::new(&format!("sync-backup-{name}"))
}

fn snapshot_with_host(name: &str) -> SyncSnapshot {
    let mut snapshot = SyncSnapshot::empty("dev-a", 1_700_000_000);
    snapshot.hosts.push(HostRecord {
        uid: "h1".to_owned(),
        group_uid: None,
        name: name.to_owned(),
        host: "web1.example.internal".to_owned(),
        port: 22,
        username: "root".to_owned(),
        auth_method: "password".to_owned(),
        identity_uid: None,
        proxy_uid: None,
        jump_host_uid: None,
        proxy_mode: "default".to_owned(),
        tags: Vec::new(),
        sort: 0,
        legacy_algorithms: false,
        created_at: 1,
        updated_at: 1,
        device_id: "dev-a".to_owned(),
    });
    snapshot
}

#[test]
fn backups_are_dpapi_sealed_and_listed_newest_first() {
    let dir = temp_dir("list");
    let sealer = TestSealer(7);
    let store = BackupStore::new(&dir, &sealer);
    let info = store
        .create(
            &snapshot_with_host("web-1"),
            &Default::default(),
            1_760_000_000,
            20,
            Some("0.1.0"),
        )
        .expect("备份");
    let bytes = std::fs::read(&info.path).expect("读备份");
    assert_eq!(&bytes[..2], b"T\x07", "必须是本机封装后的密文");
    assert!(
        !String::from_utf8_lossy(&bytes).contains("web-1"),
        "备份里不能出现明文"
    );

    let loaded = store.load(Path::new(&info.path)).expect("读回");
    assert_eq!(loaded.snapshot.hosts[0].name, "web-1");
    assert_eq!(loaded.app_version.as_deref(), Some("0.1.0"));

    let more = store
        .create(
            &snapshot_with_host("web-2"),
            &Default::default(),
            1_760_000_100,
            20,
            None,
        )
        .expect("第二份");
    let list = store.list().expect("列表");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].path, more.path, "最新的在前");
    assert_eq!(list[1].path, info.path);
}

#[test]
fn retention_keeps_twenty_and_never_touches_other_files() {
    let dir = temp_dir("retention");
    let sealer = TestSealer(1);
    let store = BackupStore::new(&dir, &sealer);
    std::fs::write(dir.join("notes.txt"), b"keep me").expect("别的文件");
    std::fs::write(dir.join("zssh-backup-not-a-time.bin"), b"keep me too").expect("像备份的文件");
    std::fs::create_dir_all(dir.join("zssh-backup-20200101-000000.bin")).expect("目录");

    for index in 0..25i64 {
        store
            .create(
                &snapshot_with_host(&format!("web-{index}")),
                &Default::default(),
                1_700_000_000 + index,
                20,
                None,
            )
            .expect("备份");
    }
    let list = store.list().expect("列表");
    assert_eq!(list.len(), 20, "最多保留 20 份");
    assert!(
        !list.iter().any(|info| info.stamp == "20231114-221320"),
        "最早的 5 份应该被删"
    );
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join("zssh-backup-not-a-time.bin").exists());
    assert!(dir.join("zssh-backup-20200101-000000.bin").is_dir());
}

#[test]
fn restore_backs_up_current_state_before_overwriting() {
    let dir = temp_dir("restore");
    let sealer = TestSealer(9);
    let store = BackupStore::new(&dir, &sealer);
    let db = Database::open_in_memory().expect("内存库");

    // 当前状态：一台叫 current 的主机。
    let mut current = snapshot_with_host("current");
    current.hosts[0].uid = "h-current".to_owned();
    current.hosts[0].device_id = "dev-a".to_owned();
    apply_snapshot(&db, &current, &sealer, None, &[], 1).expect("铺当前状态");

    // 备份里是另一台叫 restored 的主机。
    let mut restored = snapshot_with_host("restored");
    restored.hosts[0].uid = "h-restored".to_owned();
    let info = store
        .create(&restored, &Default::default(), 1_750_000_000, 20, None)
        .expect("备份");

    let report = store
        .restore(&db, "dev-a", None, Path::new(&info.path), 1_760_000_000)
        .expect("恢复");
    let hosts = db.host_list().expect("列主机");
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].name, "restored");

    // 恢复前自动备份的那份里还是 current。
    let safety = store
        .load(Path::new(&report.safety_backup.path))
        .expect("读安全备份");
    assert_eq!(safety.snapshot.hosts[0].name, "current");
    assert!(store.list().unwrap().len() >= 2);
}

#[test]
fn corrupt_backup_is_reported_not_silently_ignored() {
    let dir = temp_dir("corrupt");
    let sealer = TestSealer(3);
    let store = BackupStore::new(&dir, &sealer);
    let path = dir.join("zssh-backup-20260101-000000.bin");
    std::fs::write(&path, b"garbage").expect("写坏文件");
    assert!(store.load(&path).is_err());
}

#[test]
fn upgrade_backup_runs_once_per_version() {
    let dir = temp_dir("upgrade");
    let sealer = TestSealer(5);
    let store = BackupStore::new(&dir, &sealer);
    let db = Database::open_in_memory().expect("内存库");
    add_group(&db, "生产");

    let first = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_000)
        .expect("首次");
    assert!(first.is_some());
    let again = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_100)
        .expect("同版本");
    assert!(again.is_none(), "同一个版本只备份一次");
    let upgraded = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.2.0", 1_760_000_200)
        .expect("升级");
    assert!(upgraded.is_some(), "版本变了要再备份");
    assert_eq!(store.list().unwrap().len(), 2);
}

fn add_group(db: &Database, name: &str) {
    db.group_save(&crate::model::GroupDraft {
        id: None,
        name: name.to_owned(),
        parent_id: None,
        sort: 0,
    })
    .expect("建分组");
}

#[test]
fn an_empty_library_skips_the_upgrade_backup_but_remembers_the_version() {
    let dir = temp_dir("upgrade-empty");
    let sealer = TestSealer(5);
    let store = BackupStore::new(&dir, &sealer);
    let db = Database::open_in_memory().expect("内存库");

    // 全新安装第一次启动：库是空的，不备份、不建 backups 目录。
    let first = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_000)
        .expect("空库");
    assert!(first.is_none());
    assert!(store.list().unwrap().is_empty(), "不能占一个备份名额");
    assert!(!dir.join("backups").exists());

    // 版本已经记下：之后同一版本里有了数据，不算“升级”，不补备份。
    add_group(&db, "生产");
    let same = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_100)
        .expect("同版本");
    assert!(same.is_none());
    assert!(store.list().unwrap().is_empty());

    // 真正升级到新版本时，有数据了，要备份。
    let upgraded = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.2.0", 1_760_000_200)
        .expect("升级");
    assert!(upgraded.is_some());
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn any_kind_of_record_makes_the_library_worth_backing_up() {
    use crate::model::{
        AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind, ProxyMode,
    };
    type AddRecord = Box<dyn Fn(&Database)>;
    let cases: Vec<(&str, AddRecord)> = vec![
        ("分组", Box::new(|db| add_group(db, "g"))),
        (
            "主机",
            Box::new(|db| {
                db.host_save(&HostDraft {
                    id: None,
                    group_id: None,
                    name: "h".to_owned(),
                    host: "h.example.internal".to_owned(),
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
            }),
        ),
        (
            "凭据",
            Box::new(|db| {
                db.identity_save(&IdentityDraft {
                    id: None,
                    name: "i".to_owned(),
                    kind: IdentityKind::Agent,
                    key_path: None,
                    secret: None,
                    passphrase: None,
                })
                .expect("建凭据");
            }),
        ),
        (
            "代理",
            Box::new(|db| {
                db.proxy_save(&ProxyDraft {
                    id: None,
                    name: "p".to_owned(),
                    kind: ProxyKind::Socks5,
                    host: "127.0.0.1".to_owned(),
                    port: 1080,
                    username: None,
                    secret: None,
                })
                .expect("建代理");
            }),
        ),
    ];
    for (label, add) in cases {
        let dir = temp_dir("upgrade-kind");
        let sealer = TestSealer(5);
        let store = BackupStore::new(&dir, &sealer);
        let db = Database::open_in_memory().expect("内存库");
        add(&db);
        let info = store
            .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_000)
            .expect("备份");
        assert!(info.is_some(), "只有{label}也要备份");
    }
}

#[test]
fn settings_alone_do_not_count_as_data() {
    let dir = temp_dir("upgrade-settings");
    let sealer = TestSealer(5);
    let store = BackupStore::new(&dir, &sealer);
    let db = Database::open_in_memory().expect("内存库");
    db.setting_set("ui_theme", "\"dark\"").expect("设置");
    let info = store
        .backup_after_upgrade_if_needed(&db, "dev-a", None, "0.1.0", 1_760_000_000)
        .expect("只有设置");
    assert!(info.is_none(), "没有分组 / 主机 / 凭据 / 代理就不备份");
}
