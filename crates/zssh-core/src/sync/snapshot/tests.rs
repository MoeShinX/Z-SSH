//! 本地快照：引用换 uid、DPAPI 解密、设置白名单、known_hosts、墓碑。

use super::*;
use crate::model::{
    AuthMethod, GroupDraft, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind,
    ProxyMode,
};
use crate::store::Database;
use crate::sync::package::Tombstone;

fn raw_uid(db: &Database, table: &str, id: i64) -> String {
    db.with_conn(|conn| {
        let uid: String = conn.query_row(
            &format!("SELECT uid FROM {table} WHERE id = ?1"),
            [id],
            |row| row.get(0),
        )?;
        Ok(uid)
    })
    .expect("读 uid")
}

#[cfg(windows)]
#[test]
fn snapshot_uses_uids_for_every_reference_and_includes_secrets() {
    let db = Database::open_in_memory().expect("内存库");
    let root = db
        .group_save(&GroupDraft {
            id: None,
            name: "生产".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .expect("根分组");
    let child = db
        .group_save(&GroupDraft {
            id: None,
            name: "华东".to_owned(),
            parent_id: Some(root.id),
            sort: 1,
        })
        .expect("子分组");
    let identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "部署密码".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("identity-secret-xyz".to_owned()),
            passphrase: None,
        })
        .expect("凭据");
    let proxy = db
        .proxy_save(&ProxyDraft {
            id: None,
            name: "公司代理".to_owned(),
            kind: ProxyKind::Socks5,
            host: "proxy.example.internal".to_owned(),
            port: 1080,
            username: Some("proxy-user".to_owned()),
            secret: Some("proxy-secret-xyz".to_owned()),
        })
        .expect("代理");
    db.set_default_proxy_id(Some(proxy.id)).expect("默认代理");

    let mut draft = HostDraft {
        id: None,
        group_id: Some(child.id),
        name: "web-1".to_owned(),
        host: "web1.example.internal".to_owned(),
        port: 2222,
        username: "deploy".to_owned(),
        auth_method: AuthMethod::Password,
        identity_id: Some(identity.id),
        proxy_id: Some(proxy.id),
        jump_host_id: None,
        proxy_mode: ProxyMode::Custom,
        tags: vec!["web".to_owned()],
        sort: 3,
        legacy_algorithms: false,
    };
    // 跳板机的 id 比目标大：快照要先收齐 uid 再解析引用。
    let jump = db.host_save(&draft).expect("跳板机");
    draft.name = "目标".to_owned();
    draft.jump_host_id = Some(jump.id);
    let target = db.host_save(&draft).expect("目标主机");

    db.setting_set("ui_theme", "\"nord\"").expect("主题");
    db.setting_set("layout.ai_panel_width", "520")
        .expect("布局");
    db.setting_set("ai_opencode_path", "\"C:\\\\tools\\\\opencode.exe\"")
        .expect("本机路径");

    let known_hosts_dir = crate::test_util::TestDir::new("sync-snapshot");
    let known_hosts = known_hosts_dir.join("known_hosts");
    std::fs::write(
        &known_hosts,
        "web1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa\n",
    )
    .expect("写 known_hosts");

    let now = 1_760_000_000i64;
    db.with_conn(|conn| {
        record_tombstones(
            conn,
            &[
                Tombstone {
                    entity: "host".to_owned(),
                    key: "deleted-host-uid".to_owned(),
                    deleted_at: now - 10,
                    device_id: "dev-a".to_owned(),
                },
                Tombstone {
                    entity: "host".to_owned(),
                    key: "expired-host-uid".to_owned(),
                    deleted_at: now - 91 * 24 * 3600,
                    device_id: "dev-a".to_owned(),
                },
            ],
        )
        .map_err(|err| crate::error::ZsshError::Internal(err.to_string()))?;
        Ok(())
    })
    .expect("写墓碑");

    let snapshot = snapshot_local(
        &db,
        &crate::sync::sealer::DpapiSealer,
        "dev-a",
        Some(&known_hosts),
        now,
    )
    .expect("快照");

    assert_eq!(snapshot.device_id, "dev-a");
    assert_eq!(snapshot.groups.len(), 2);
    let child_record = snapshot
        .groups
        .iter()
        .find(|group| group.uid == raw_uid(&db, "groups", child.id))
        .expect("子分组");
    assert_eq!(
        child_record.parent_uid.as_deref(),
        Some(raw_uid(&db, "groups", root.id).as_str()),
        "父分组引用要用 uid"
    );

    let target_record = snapshot
        .hosts
        .iter()
        .find(|host| host.uid == raw_uid(&db, "hosts", target.id))
        .expect("目标主机");
    assert_eq!(
        target_record.group_uid.as_deref(),
        Some(raw_uid(&db, "groups", child.id).as_str())
    );
    assert_eq!(
        target_record.identity_uid.as_deref(),
        Some(raw_uid(&db, "identities", identity.id).as_str())
    );
    assert_eq!(
        target_record.proxy_uid.as_deref(),
        Some(raw_uid(&db, "proxies", proxy.id).as_str())
    );
    assert_eq!(
        target_record.jump_host_uid.as_deref(),
        Some(raw_uid(&db, "hosts", jump.id).as_str()),
        "跳板机引用要用 uid"
    );
    assert!(uuid::Uuid::parse_str(&target_record.uid).is_ok());
    assert_eq!(target_record.tags, vec!["web".to_owned()]);

    assert_eq!(
        snapshot.identities[0].secret.as_deref(),
        Some("identity-secret-xyz"),
        "DPAPI 密文要解开成明文放进（加密后的）同步包"
    );
    assert_eq!(
        snapshot.proxies[0].secret.as_deref(),
        Some("proxy-secret-xyz")
    );

    assert_eq!(
        snapshot.settings.get("ui_theme"),
        Some(&serde_json::json!("nord"))
    );
    assert_eq!(
        snapshot.settings.get("default_proxy_id"),
        Some(&serde_json::json!(raw_uid(&db, "proxies", proxy.id))),
        "默认代理要换成 uid，不能出现本机自增 id"
    );
    assert!(
        !snapshot.settings.contains_key("layout.ai_panel_width"),
        "布局设置不能同步"
    );
    assert!(
        !snapshot.settings.contains_key("ai_opencode_path"),
        "智能体路径不能同步"
    );
    assert!(
        !snapshot.settings.contains_key("sync_device_id"),
        "设备 ID 不能同步"
    );

    assert_eq!(snapshot.known_hosts.len(), 1);
    assert_eq!(snapshot.known_hosts[0].port, 22);
    assert_eq!(snapshot.known_hosts[0].key_type, "ssh-ed25519");

    assert_eq!(snapshot.tombstones.len(), 1, "过期墓碑不传播");
    assert_eq!(snapshot.tombstones[0].key, "deleted-host-uid");
}

#[test]
fn missing_known_hosts_is_not_an_error() {
    let db = Database::open_in_memory().expect("内存库");
    let dir = crate::test_util::TestDir::new("sync-snapshot");
    let path = dir.join("missing");
    let snapshot = snapshot_local(
        &db,
        &crate::sync::sealer::DpapiSealer,
        "dev-a",
        Some(&path),
        1,
    )
    .expect("没有文件也能导出");
    assert!(snapshot.known_hosts.is_empty());
    assert_eq!(snapshot.record_count(), 0);
}
