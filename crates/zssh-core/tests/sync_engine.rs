//! 集成测试（S-1）：两个独立的数据目录模拟两台电脑，通过内存存储反复同步。
//!
//! 不连任何云存储。两边用同一个 Windows 用户的 DPAPI（测试机上只有这一个），
//! “换一台电脑的 DPAPI”这一层由 `sync::apply` 的单元测试用替身验证。
//! 每一轮比较**完整的同步内容**（`snapshot_local` 解开凭据后的全部字段），
//! 不只是主机名。

use std::path::Path;

mod common;

use zssh_core::model::{
    AuthMethod, GroupDraft, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind,
    ProxyMode,
};
use zssh_core::store::Database;
use zssh_core::sync::engine::{SyncEngine, SyncStatus};
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::snapshot::snapshot_local;
use zssh_core::sync::storage::MemoryStorage;
use zssh_core::sync::SyncSnapshot;

const PASSWORD: &str = "integration-master-password";

fn temp_dir(name: &str) -> common::TestDir {
    common::TestDir::new(&format!("sync-integration-{name}"))
}

fn open_db(dir: &Path) -> Database {
    Database::open(&dir.join("zssh.db")).expect("打开库")
}

fn add_host(db: &Database, name: &str, group: Option<i64>, identity: Option<i64>) {
    db.host_save(&HostDraft {
        id: None,
        group_id: group,
        name: name.to_owned(),
        host: format!("{name}.example.internal"),
        port: 22,
        username: "deploy".to_owned(),
        auth_method: AuthMethod::Password,
        identity_id: identity,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: ProxyMode::Default,
        tags: vec!["it".to_owned()],
        sort: 0,
        legacy_algorithms: false,
    })
    .expect("建主机");
}

fn content(db: &Database, dir: &Path, device: &str) -> SyncSnapshot {
    snapshot_local(
        db,
        &DpapiSealer,
        device,
        Some(&dir.join("known_hosts")),
        1_000,
    )
    .expect("读完整同步内容")
}

/// 两边完整内容必须一致：分组、主机全部字段、凭据（解开后的密码 / 私钥）、代理、
/// 白名单设置、known_hosts、墓碑。只有本机字段（本地 ID、设备 ID、快照时间）可以不同。
fn assert_same_content(db_a: &Database, dir_a: &Path, db_b: &Database, dir_b: &Path, label: &str) {
    let a = content(db_a, dir_a, "compare");
    let b = content(db_b, dir_b, "compare");
    assert_eq!(a.groups, b.groups, "{label}：分组");
    assert_eq!(a.hosts, b.hosts, "{label}：主机");
    assert_eq!(a.identities, b.identities, "{label}：凭据");
    assert_eq!(a.proxies, b.proxies, "{label}：代理");
    assert_eq!(a.settings, b.settings, "{label}：设置");
    assert_eq!(a.known_hosts, b.known_hosts, "{label}：known_hosts");
    assert_eq!(a.tombstones, b.tombstones, "{label}：墓碑");
    assert!(a.same_data(&b), "{label}：整体");
}

#[tokio::test]
async fn two_data_directories_converge_through_repeated_syncs() {
    let dir_a = temp_dir("a");
    let dir_b = temp_dir("b");
    let db_a = open_db(&dir_a);
    let db_b = open_db(&dir_b);
    let sealer_a = DpapiSealer;
    let sealer_b = DpapiSealer;
    let storage = MemoryStorage::new();
    let known_a = dir_a.join("known_hosts");
    let known_b = dir_b.join("known_hosts");
    std::fs::write(
        &known_a,
        "web-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa\n",
    )
    .expect("A known_hosts");
    std::fs::write(
        &known_b,
        "web-2.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIbbb\n",
    )
    .expect("B known_hosts");

    let group = db_a
        .group_save(&GroupDraft {
            id: None,
            name: "生产".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .expect("分组");
    let identity = db_a
        .identity_save(&IdentityDraft {
            id: None,
            name: "部署凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("pw-from-a".to_owned()),
            passphrase: None,
        })
        .expect("凭据");
    db_a.identity_save(&IdentityDraft {
        id: None,
        name: "私钥凭据".to_owned(),
        kind: IdentityKind::KeyInline,
        key_path: None,
        secret: Some(
            "-----BEGIN FAKE KEY-----
abc
-----END FAKE KEY-----"
                .to_owned(),
        ),
        passphrase: Some("key-passphrase".to_owned()),
    })
    .expect("私钥凭据");
    let proxy = db_a
        .proxy_save(&ProxyDraft {
            id: None,
            name: "出口代理".to_owned(),
            kind: ProxyKind::Socks5,
            host: "proxy.example.internal".to_owned(),
            port: 1080,
            username: Some("proxy-user".to_owned()),
            secret: Some("proxy-pw".to_owned()),
        })
        .expect("代理");
    db_a.set_default_proxy_id(Some(proxy.id)).expect("默认代理");
    db_a.setting_set("ui_theme", "\"obsidian\"").expect("主题");
    db_a.setting_set("terminal_font_size", "15").expect("字号");
    for index in 1..=12 {
        add_host(
            &db_a,
            &format!("host-{index:02}"),
            Some(group.id),
            Some(identity.id),
        );
    }

    let engine_a = SyncEngine::new(
        &db_a,
        &sealer_a,
        "dev-a",
        &dir_a,
        Some(known_a.clone()),
        Some("0.1.0".to_owned()),
    );
    let engine_b = SyncEngine::new(
        &db_b,
        &sealer_b,
        "dev-b",
        &dir_b,
        Some(known_b.clone()),
        Some("0.1.0".to_owned()),
    );

    let report = engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 首次同步");
    assert_eq!(report.status, SyncStatus::Completed);
    engine_b
        .sync(&storage, PASSWORD, None)
        .await
        .expect("B 首次同步");
    // B 带上来了自己的 known_hosts，A 再同步一次才拿得到。
    engine_a
        .sync(&storage, PASSWORD, None)
        .await
        .expect("A 第二次同步");
    assert_same_content(&db_a, &dir_a, &db_b, &dir_b, "首次同步之后");
    let b_content = content(&db_b, &dir_b, "compare");
    assert!(
        b_content
            .identities
            .iter()
            .any(|i| i.secret.as_deref() == Some("pw-from-a")),
        "B 要能解出 A 的密码"
    );
    assert!(
        b_content
            .identities
            .iter()
            .any(|i| i.secret.as_deref().is_some_and(|s| s.contains("FAKE KEY"))),
        "B 要能解出 A 的私钥"
    );
    assert!(b_content
        .proxies
        .iter()
        .any(|p| p.secret.as_deref() == Some("proxy-pw")));
    assert_eq!(
        b_content.settings.get("ui_theme"),
        Some(&serde_json::json!("obsidian"))
    );
    assert_eq!(db_b.group_list().unwrap().len(), 1);
    assert_eq!(db_b.identity_list().unwrap().len(), 2);
    assert!(
        db_b.host_list().unwrap()[0].group_id.is_some(),
        "分组引用要按 uid 还原成本地 id"
    );
    assert!(
        std::fs::read_to_string(&known_b)
            .unwrap()
            .contains("web-1.example.internal"),
        "known_hosts 要同步过去"
    );

    // 反复各自增删改：每轮 A 同步、B 同步、A 再同步，最后必须一致。
    for round in 0..5usize {
        add_host(&db_a, &format!("a-new-{round}"), None, None);
        let b_hosts = db_b.host_list().unwrap();
        if let Some(victim) = b_hosts.iter().find(|host| host.name.starts_with("host-")) {
            if round % 3 == 0 {
                db_b.host_delete(victim.id).expect("B 删主机");
            } else {
                db_b.host_save(&HostDraft {
                    id: Some(victim.id),
                    group_id: None,
                    name: format!("b-edited-{round}"),
                    host: victim.host.clone(),
                    port: victim.port,
                    username: victim.username.clone(),
                    auth_method: victim.auth_method,
                    identity_id: None,
                    proxy_id: None,
                    jump_host_id: None,
                    proxy_mode: victim.proxy_mode,
                    tags: Vec::new(),
                    sort: 0,
                    legacy_algorithms: false,
                })
                .expect("B 改主机");
            }
        }
        if round == 2 {
            let identity = db_b
                .identity_list()
                .unwrap()
                .into_iter()
                .find(|i| i.name == "部署凭据")
                .expect("B 上的凭据");
            db_b.identity_save(&IdentityDraft {
                id: Some(identity.id),
                name: identity.name.clone(),
                kind: IdentityKind::Password,
                key_path: None,
                secret: Some("pw-changed-on-b".to_owned()),
                passphrase: None,
            })
            .expect("B 改密码");
        }
        if round == 3 {
            db_a.setting_set("terminal_font_size", "18")
                .expect("A 改字号");
        }
        if round == 4 {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&known_b)
                .and_then(|mut file| {
                    std::io::Write::write_all(
                        &mut file,
                        b"late.example.internal ssh-rsa AAAAB3NzaC1yc2Eaaaaa\n",
                    )
                })
                .expect("B 追加 known_hosts");
        }

        engine_a
            .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
            .await
            .expect("A 同步");
        engine_b
            .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
            .await
            .expect("B 同步");
        engine_a
            .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
            .await
            .expect("A 收尾同步");

        assert_same_content(&db_a, &dir_a, &db_b, &dir_b, &format!("第 {round} 轮之后"));
    }
    assert!(
        std::fs::read_to_string(&known_a)
            .unwrap()
            .contains("late.example.internal"),
        "B 后来追加的 known_hosts 要传给 A"
    );
    assert!(dir_a.join("sync-baseline.bin").exists());
    assert!(dir_b.join("sync-baseline.bin").exists());
    assert!(dir_a.join("backups").is_dir(), "A 应该有本地备份");
    assert!(dir_b.join("backups").is_dir(), "B 应该有本地备份");
    assert_ne!(
        std::fs::read(dir_a.join("sync-baseline.bin")).unwrap(),
        std::fs::read(dir_b.join("sync-baseline.bin")).unwrap(),
        "两边用各自的封装，基线密文不会一样"
    );
}

/// 第二台电脑第一次同步：本地什么都没有，合并云端之前不备份空库；
/// 之后本地有了记录再被云端改动，才会备份。
#[tokio::test]
async fn empty_local_library_skips_the_pre_merge_backup() {
    let dir_a = temp_dir("empty-a");
    let dir_b = temp_dir("empty-b");
    let db_a = open_db(&dir_a);
    let db_b = open_db(&dir_b);
    let sealer = DpapiSealer;
    let storage = MemoryStorage::new();
    add_host(&db_a, "web-1", None, None);
    let engine_a = SyncEngine::new(
        &db_a,
        &sealer,
        "dev-a",
        &dir_a,
        None,
        Some("0.1.0".to_owned()),
    );
    let engine_b = SyncEngine::new(
        &db_b,
        &sealer,
        "dev-b",
        &dir_b,
        None,
        Some("0.1.0".to_owned()),
    );

    engine_a
        .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("A 上传");
    let report = engine_b
        .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("B 第一次同步");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(report.applied.is_some(), "云端的主机应该合并进了 B");
    assert!(report.backup.is_none(), "本地是空库，不该备份");
    assert!(
        engine_b.list_backups().expect("列备份").is_empty(),
        "B 不该有任何备份"
    );

    // 本地有记录之后，云端再改动会照常备份。
    add_host(&db_a, "web-2", None, None);
    engine_a
        .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("A 再上传");
    let report = engine_b
        .sync(&storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("B 再同步");
    assert!(report.backup.is_some(), "本地有记录时要备份");
}
