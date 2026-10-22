use super::*;
use crate::model::{AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyMode};
use crate::store::Database;
use crate::sync::engine::SyncEngine;
use crate::sync::sealer::TestSealer;
use crate::sync::storage::MemoryStorage;

const PASSWORD: &str = "conflict-test-password";

fn temp_dir(name: &str) -> crate::test_util::TestDir {
    crate::test_util::TestDir::new(&format!("sync-conflicts-{name}"))
}

fn draft(id: Option<i64>, name: &str, host: &str) -> HostDraft {
    HostDraft {
        id,
        group_id: None,
        name: name.to_owned(),
        host: host.to_owned(),
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
    }
}

fn host_names(db: &Database) -> Vec<String> {
    let mut names: Vec<String> = db
        .host_list()
        .unwrap()
        .into_iter()
        .map(|h| h.name)
        .collect();
    names.sort();
    names
}

struct Pair {
    db_a: Database,
    db_b: Database,
    // 目录守卫放在数据库之后：先关库再删目录。
    dir_a: crate::test_util::TestDir,
    dir_b: crate::test_util::TestDir,
    storage: MemoryStorage,
}

impl Pair {
    fn engine_a(&self) -> SyncEngine<'_> {
        // 这里的 TestSealer 要活得比引擎久，所以用泄漏的静态引用。
        let sealer: &'static TestSealer = Box::leak(Box::new(TestSealer(1)));
        SyncEngine::new(&self.db_a, sealer, "dev-a", &self.dir_a, None, None)
    }
    fn engine_b(&self) -> SyncEngine<'_> {
        let sealer: &'static TestSealer = Box::leak(Box::new(TestSealer(2)));
        SyncEngine::new(&self.db_b, sealer, "dev-b", &self.dir_b, None, None)
    }
}

async fn paired(name: &str) -> Pair {
    let pair = Pair {
        db_a: Database::open_in_memory().expect("A 库"),
        db_b: Database::open_in_memory().expect("B 库"),
        dir_a: temp_dir(&format!("{name}-a")),
        dir_b: temp_dir(&format!("{name}-b")),
        storage: MemoryStorage::new(),
    };
    pair.db_a
        .host_save(&draft(None, "web-1", "web-1.example.internal"))
        .expect("建主机");
    pair.db_a
        .host_save(&draft(None, "keep", "keep.example.internal"))
        .expect("建主机");
    pair.engine_a()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("A 首次");
    pair.engine_b()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("B 首次");
    pair
}

/// B 删了 web-1，A 改了它：A 先同步让云端有 A 的改动，B 再同步 → 删除赢。
async fn make_delete_conflict(pair: &Pair) {
    let host = pair
        .db_a
        .host_list()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "web-1")
        .expect("web-1");
    pair.db_a
        .host_save(&draft(
            Some(host.id),
            "web-1-edited",
            "web-1.example.internal",
        ))
        .expect("A 改");
    let b_host = pair
        .db_b
        .host_list()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "web-1")
        .expect("B 上的 web-1");
    pair.db_b.host_delete(b_host.id).expect("B 删");
    pair.engine_b()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("B 同步（删除上云）");
    pair.engine_a()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("A 同步（改动遇到墓碑）");
}

#[tokio::test]
async fn a_deleted_record_can_be_restored_from_the_conflict_list() {
    let pair = paired("restore").await;
    make_delete_conflict(&pair).await;
    assert_eq!(
        host_names(&pair.db_a),
        vec!["keep"],
        "删除赢：A 上的 web-1 没了"
    );

    let engine_a = pair.engine_a();
    let conflicts = engine_a.list_conflicts().expect("列表");
    let conflict = conflicts
        .iter()
        .find(|c| c.entity == "host")
        .expect("主机冲突");
    assert!(conflict.deletion && conflict.can_use_other);
    assert_eq!(conflict.name, "web-1-edited", "显示的是被删记录最后的内容");
    assert_eq!(conflict.kept_summary, "已删除");
    assert!(conflict.other_summary.contains("web-1.example.internal"));
    assert!(conflict.other_is_local, "被删的那份是本机的改动");

    engine_a.use_conflict_other(conflict.id).expect("恢复");
    assert_eq!(host_names(&pair.db_a), vec!["keep", "web-1-edited"]);
    assert!(
        engine_a
            .list_conflicts()
            .unwrap()
            .iter()
            .all(|c| c.id != conflict.id),
        "处理过的冲突要从列表里消失"
    );

    // 之后正常参与同步：两台电脑都有恢复出来的这条，不会被原来的墓碑再删掉。
    engine_a
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("A");
    pair.engine_b()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("B");
    engine_a
        .sync(&pair.storage, PASSWORD, None)
        .await
        .expect("A 再同步");
    assert_eq!(host_names(&pair.db_a), vec!["keep", "web-1-edited"]);
    assert_eq!(host_names(&pair.db_b), vec!["keep", "web-1-edited"]);
}

#[tokio::test]
async fn using_the_other_version_of_a_two_sided_edit_overwrites_and_syncs() {
    let pair = paired("other").await;
    let a_host = pair
        .db_a
        .host_list()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "web-1")
        .unwrap();
    let b_host = pair
        .db_b
        .host_list()
        .unwrap()
        .into_iter()
        .find(|h| h.name == "web-1")
        .unwrap();
    pair.db_a
        .host_save(&draft(
            Some(a_host.id),
            "edited-by-a",
            "web-1.example.internal",
        ))
        .unwrap();
    pair.db_b
        .host_save(&draft(
            Some(b_host.id),
            "edited-by-b",
            "web-1.example.internal",
        ))
        .unwrap();
    pair.engine_a()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .unwrap();
    let report = pair
        .engine_b()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .unwrap();
    assert!(!report.conflicts.is_empty(), "两边都改了，要有冲突");

    let engine_b = pair.engine_b();
    let conflicts = engine_b.list_conflicts().unwrap();
    let conflict = conflicts.iter().find(|c| c.entity == "host").expect("冲突");
    assert!(!conflict.deletion);
    let kept_name = host_names(&pair.db_b)
        .into_iter()
        .find(|n| n.starts_with("edited"))
        .unwrap();
    let other_name = if kept_name == "edited-by-a" {
        "edited-by-b"
    } else {
        "edited-by-a"
    };
    assert!(conflict.other_summary.contains("web-1.example.internal"));

    engine_b.use_conflict_other(conflict.id).unwrap();
    assert!(host_names(&pair.db_b).contains(&other_name.to_owned()));
    engine_b.sync(&pair.storage, PASSWORD, None).await.unwrap();
    pair.engine_a()
        .sync(&pair.storage, PASSWORD, None)
        .await
        .unwrap();
    assert_eq!(host_names(&pair.db_a), host_names(&pair.db_b));
    assert!(host_names(&pair.db_a).contains(&other_name.to_owned()));
}

#[tokio::test]
async fn dismissing_removes_only_that_conflict() {
    let pair = paired("dismiss").await;
    make_delete_conflict(&pair).await;
    let engine_a = pair.engine_a();
    let before = engine_a.list_conflicts().unwrap();
    assert!(!before.is_empty());
    engine_a.dismiss_conflict(before[0].id).unwrap();
    assert_eq!(engine_a.list_conflicts().unwrap().len(), before.len() - 1);
    assert_eq!(
        host_names(&pair.db_a),
        vec!["keep"],
        "只是清掉记录，不改数据"
    );
}

#[tokio::test]
async fn conflicts_are_sealed_at_rest_and_expire_after_ninety_days() {
    let pair = paired("expire").await;
    // 凭据冲突：里面有密码，入库后不能是明文。
    let identity = pair
        .db_a
        .identity_save(&IdentityDraft {
            id: None,
            name: "cred".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: None,
            passphrase: None,
        })
        .unwrap();
    let _ = identity;
    make_delete_conflict(&pair).await;
    let raw: Vec<(String, String)> = pair
        .db_a
        .with_conn_as(|conn| -> Result<_, SyncError> {
            let mut statement = conn.prepare("SELECT kept, other FROM sync_conflicts")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .unwrap();
    assert!(!raw.is_empty());
    for (_, other) in &raw {
        assert!(other.starts_with("enc1:"), "冲突内容要封装后入库：{other}");
        assert!(!other.contains("web-1"));
    }

    // 把冲突的时间拨到 91 天前，再列一次就被清掉。
    let now = SyncEngine::now();
    pair.db_a
        .with_conn_as(|conn| -> Result<_, SyncError> {
            conn.execute(
                "UPDATE sync_conflicts SET at = ?1",
                [now - CONFLICT_RETENTION_SECS - 10],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(pair.engine_a().list_conflicts().unwrap().is_empty());
}

#[tokio::test]
async fn a_deleted_known_host_fingerprint_cannot_be_restored() {
    let db = Database::open_in_memory().unwrap();
    let sealer = TestSealer(1);
    let other = serde_json::json!({
        "host": "h.example.internal", "port": 22, "key_type": "ssh-ed25519",
        "key": "AAAA", "marker": null, "updated_at": 1, "device_id": "dev-x"
    })
    .to_string();
    db.with_conn_as(|conn| -> Result<_, SyncError> {
        store_conflict(conn, &sealer, 100, "known_host", "k", "", &other)
    })
    .unwrap();
    let views = list(&db, &sealer, "dev-a", 200).unwrap();
    assert_eq!(views.len(), 1);
    assert!(views[0].deletion && !views[0].can_use_other);
    let err = use_other(&db, &sealer, "dev-a", None, views[0].id, 200).unwrap_err();
    assert!(err.to_string().contains("不能恢复"), "{err}");
}

#[tokio::test]
async fn a_known_host_conflict_cannot_switch_to_the_other_key() {
    // 两边对同一台主机记了不同的公钥。“改用另一份”以前会提示成功、清掉冲突，
    // 但落盘只追加缺的记录，文件里还是原来的公钥（C15）。现在必须明确拒绝。
    let dir = temp_dir("conflict-known-host");
    let path = dir.join("known_hosts");
    let local_line = "h.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa\n";
    std::fs::write(&path, local_line).unwrap();
    let db = Database::open_in_memory().unwrap();
    let sealer = TestSealer(1);
    let record = |key: &str, device: &str| {
        serde_json::json!({
            "host": "h.example.internal", "port": 22, "key_type": "ssh-ed25519",
            "key": key, "marker": null, "updated_at": 1, "device_id": device
        })
        .to_string()
    };
    let key = crate::sync::package::known_host_key("h.example.internal", 22, "ssh-ed25519");
    db.with_conn_as(|conn| -> Result<_, SyncError> {
        store_conflict(
            conn,
            &sealer,
            100,
            "known_host",
            &key,
            &record("AAAAC3NzaC1lZDI1NTE5AAAAIaaa", "dev-a"),
            &record("AAAAC3NzaC1lZDI1NTE5AAAAIbbb", "dev-x"),
        )
    })
    .unwrap();

    let views = list(&db, &sealer, "dev-a", 200).unwrap();
    assert_eq!(views.len(), 1);
    assert!(!views[0].deletion);
    assert!(!views[0].can_use_other, "界面上不能给出这个按钮");
    assert!(views[0].message.contains("指纹"), "{}", views[0].message);

    let err = use_other(&db, &sealer, "dev-a", Some(&path), views[0].id, 200).unwrap_err();
    assert!(err.to_string().contains("不能"), "{err}");
    assert_eq!(
        list(&db, &sealer, "dev-a", 200).unwrap().len(),
        1,
        "没生效就不能清掉冲突"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), local_line);
}
