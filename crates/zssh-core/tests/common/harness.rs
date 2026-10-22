//! 同步集成测试共用的夹具：两个数据目录模拟两台电脑、种子数据、完整内容比较、
//! 对任意存储跑的完整场景。WebDAV / S3 / GitHub 的集成测试都用它。

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Mutex;

use zssh_core::model::{
    AuthMethod, GroupDraft, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind,
    ProxyMode,
};
use zssh_core::ssh::BoxFuture;
use zssh_core::store::Database;
use zssh_core::sync::engine::{SyncEngine, SyncStatus};
use zssh_core::sync::guard::DeletionDecision;
use zssh_core::sync::sealer::DpapiSealer;
use zssh_core::sync::snapshot::snapshot_local;
use zssh_core::sync::storage::{StorageError, StoredObject, SyncStorage};
use zssh_core::sync::SyncSnapshot;

pub const PASSWORD: &str = "cloud-integration-master-password";

pub fn temp_dir(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("zssh-sync-cloud-{name}-"))
        .tempdir()
        .expect("建目录")
}

pub struct Machine {
    pub db: Database,
    pub dir: PathBuf,
    pub device: &'static str,
    /// 目录守卫：放在最后，数据库先关、目录最后删（panic / 提前 return 也一样）。
    _guard: tempfile::TempDir,
}

impl Machine {
    pub fn new(name: &str, device: &'static str) -> Self {
        let guard = temp_dir(name);
        let dir = guard.path().to_path_buf();
        Self {
            db: Database::open(&dir.join("zssh.db")).expect("打开库"),
            dir,
            device,
            _guard: guard,
        }
    }

    pub fn engine(&self) -> SyncEngine<'_> {
        SyncEngine::new(
            &self.db,
            &DpapiSealer,
            self.device,
            &self.dir,
            Some(self.dir.join("known_hosts")),
            Some("0.1.0".to_owned()),
        )
    }

    pub fn content(&self) -> SyncSnapshot {
        snapshot_local(
            &self.db,
            &DpapiSealer,
            "compare",
            Some(&self.dir.join("known_hosts")),
            1_000,
        )
        .expect("读完整同步内容")
    }

    pub fn add_host(&self, name: &str) {
        self.db.host_save(&host_draft(None, name)).expect("建主机");
    }

    pub fn host_id(&self, name: &str) -> i64 {
        self.db
            .host_list()
            .unwrap()
            .into_iter()
            .find(|host| host.name == name)
            .unwrap_or_else(|| panic!("没有主机 {name}"))
            .id
    }

    pub fn rename_host(&self, from: &str, to: &str) {
        let id = self.host_id(from);
        self.db.host_save(&host_draft(Some(id), to)).expect("改名");
    }

    pub fn delete_host(&self, name: &str) {
        self.db.host_delete(self.host_id(name)).expect("删主机");
    }

    pub fn host_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .db
            .host_list()
            .unwrap()
            .into_iter()
            .map(|host| host.name)
            .collect();
        names.sort();
        names
    }
}

pub fn host_draft(id: Option<i64>, name: &str) -> HostDraft {
    HostDraft {
        id,
        group_id: None,
        name: name.to_owned(),
        host: format!("{name}.example.internal"),
        port: 22,
        username: "deploy".to_owned(),
        auth_method: AuthMethod::Password,
        identity_id: None,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: ProxyMode::Default,
        tags: vec!["it".to_owned()],
        sort: 0,
        legacy_algorithms: false,
    }
}

/// 两边完整内容必须一致。只有本机字段（本地 ID、设备 ID、快照时间）可以不同。
pub fn assert_same_content(a: &Machine, b: &Machine, label: &str) {
    let (left, right) = (a.content(), b.content());
    assert_eq!(left.groups, right.groups, "{label}：分组");
    assert_eq!(left.hosts, right.hosts, "{label}：主机");
    assert_eq!(left.identities, right.identities, "{label}：凭据");
    assert_eq!(left.proxies, right.proxies, "{label}：代理");
    assert_eq!(left.settings, right.settings, "{label}：设置");
    assert_eq!(left.known_hosts, right.known_hosts, "{label}：known_hosts");
    assert_eq!(left.tombstones, right.tombstones, "{label}：墓碑");
    assert!(left.same_data(&right), "{label}：整体");
}

pub fn seed(machine: &Machine) {
    let group = machine
        .db
        .group_save(&GroupDraft {
            id: None,
            name: "生产".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .expect("分组");
    machine
        .db
        .identity_save(&IdentityDraft {
            id: None,
            name: "部署凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("pw-from-a".to_owned()),
            passphrase: None,
        })
        .expect("凭据");
    machine
        .db
        .identity_save(&IdentityDraft {
            id: None,
            name: "私钥凭据".to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some("-----BEGIN FAKE KEY-----\nabc\n-----END FAKE KEY-----".to_owned()),
            passphrase: Some("key-passphrase".to_owned()),
        })
        .expect("私钥凭据");
    let proxy = machine
        .db
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
    machine.db.set_default_proxy_id(Some(proxy.id)).unwrap();
    machine.db.setting_set("ui_theme", "\"obsidian\"").unwrap();
    machine.db.setting_set("terminal_font_size", "15").unwrap();
    for index in 1..=4 {
        let mut draft = host_draft(None, &format!("host-{index}"));
        draft.group_id = Some(group.id);
        machine.db.host_save(&draft).expect("建主机");
    }
    std::fs::write(
        machine.dir.join("known_hosts"),
        "host-1.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIaaa\n",
    )
    .expect("known_hosts");
}

pub async fn sync_ok(machine: &Machine, storage: &dyn SyncStorage, label: &str) {
    let report = machine
        .engine()
        .sync(storage, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .unwrap_or_else(|err| panic!("{label}：{err}"));
    assert_eq!(report.status, SyncStatus::Completed, "{label}");
}

/// 一个后端的完整场景。`inject` 在“下一次写之前让另一台电脑抢先写”。
pub async fn full_scenario(
    sa: &dyn SyncStorage,
    sb: &dyn SyncStorage,
    inject: &dyn Fn(Vec<u8>),
    cloud_object: &dyn Fn() -> Option<Vec<u8>>,
) {
    let a = Machine::new("full-a", "dev-a");
    let b = Machine::new("full-b", "dev-b");
    std::fs::write(
        b.dir.join("known_hosts"),
        "web-b.example.internal ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIbbb\n",
    )
    .unwrap();
    seed(&a);

    // --- 1. 两台电脑收敛（比较完整内容）---
    sync_ok(&a, sa, "A 首次").await;
    let cloud = cloud_object().expect("云端有同步包");
    assert!(
        !String::from_utf8_lossy(&cloud).contains("pw-from-a"),
        "云端只能是密文"
    );
    sync_ok(&b, sb, "B 首次").await;
    sync_ok(&a, sa, "A 第二次").await;
    assert_same_content(&a, &b, "首次同步之后");
    let content = b.content();
    assert!(content
        .identities
        .iter()
        .any(|i| i.secret.as_deref() == Some("pw-from-a")));
    assert!(content
        .proxies
        .iter()
        .any(|p| p.secret.as_deref() == Some("proxy-pw")));
    assert_eq!(content.known_hosts.len(), 2);

    a.add_host("a-new");
    b.rename_host("host-2", "b-renamed");
    b.delete_host("host-3");
    sync_ok(&a, sa, "A").await;
    sync_ok(&b, sb, "B").await;
    sync_ok(&a, sa, "A 收尾").await;
    assert_same_content(&a, &b, "增删改一轮之后");
    assert!(!a.host_names().contains(&"host-3".to_owned()));

    // --- 2. 版本冲突：写之前另一台电脑抢先写，重新读、重新合并、再写 ---
    b.add_host("external-host");
    let external = b.engine().export_package(PASSWORD).expect("导出");
    a.add_host("a-local-during-conflict");
    inject(external);
    let report = a
        .engine()
        .sync(sa, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("版本冲突后重试");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(report.retries >= 1, "要重试过：{}", report.retries);
    assert!(a.host_names().contains(&"external-host".to_owned()));
    sync_ok(&b, sb, "B").await;
    sync_ok(&a, sa, "A").await;
    assert_same_content(&a, &b, "版本冲突重试之后");
    assert!(b
        .host_names()
        .contains(&"a-local-during-conflict".to_owned()));

    // --- 3. 大量删除需要确认 ---
    for index in 0..12 {
        a.add_host(&format!("bulk-{index:02}"));
    }
    sync_ok(&a, sa, "A 上传批量主机").await;
    sync_ok(&b, sb, "B 取回批量主机").await;
    for index in 0..12 {
        a.delete_host(&format!("bulk-{index:02}"));
    }
    sync_ok(&a, sa, "A 上传删除").await;
    let before = b.host_names();
    let report = b.engine().sync(sb, PASSWORD, None).await.expect("B 同步");
    assert_eq!(report.status, SyncStatus::NeedsConfirmation);
    assert!(report.deletion.deleted_hosts >= 12, "{:?}", report.deletion);
    assert_eq!(b.host_names(), before, "确认之前本地一条都不能删");
    assert_eq!(report.deletion.samples.len(), 5, "对话框要列出前几条的名字");
    assert!(report
        .deletion
        .samples
        .iter()
        .all(|s| s.entity == "host" && s.name.starts_with("bulk-")));
    sync_ok(&b, sb, "B 确认继续").await;
    assert!(!b.host_names().iter().any(|n| n.starts_with("bulk-")));
    sync_ok(&a, sa, "A").await;
    assert_same_content(&a, &b, "大量删除确认之后");

    // --- 4. 冲突列表：恢复被删除的记录 ---
    a.rename_host("host-4", "host-4-edited");
    b.delete_host("host-4");
    sync_ok(&b, sb, "B 删除上云").await;
    sync_ok(&a, sa, "A 的改动遇到墓碑").await;
    assert!(
        !a.host_names().contains(&"host-4-edited".to_owned()),
        "删除赢"
    );
    let engine_a = a.engine();
    let conflicts = engine_a.list_conflicts().expect("冲突列表");
    let conflict = conflicts
        .iter()
        .find(|c| c.deletion && c.name == "host-4-edited")
        .expect("删除类冲突");
    assert!(conflict.can_use_other);
    engine_a.use_conflict_other(conflict.id).expect("恢复");
    assert!(a.host_names().contains(&"host-4-edited".to_owned()));
    sync_ok(&a, sa, "A 上传恢复的记录").await;
    sync_ok(&b, sb, "B").await;
    sync_ok(&a, sa, "A").await;
    assert_same_content(&a, &b, "恢复之后");
    assert!(b.host_names().contains(&"host-4-edited".to_owned()));
}

/// 读之后、写之前，另一台电脑改了云端：回调在 `read` 返回后立刻执行。
pub struct AfterRead<'a> {
    pub inner: &'a dyn SyncStorage,
    pub hook: Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
}

impl<'b> SyncStorage for AfterRead<'b> {
    fn read<'a>(&'a self) -> BoxFuture<'a, Result<Option<StoredObject>, StorageError>> {
        Box::pin(async move {
            let result = self.inner.read().await;
            if let Some(hook) = self.hook.lock().unwrap().take() {
                hook();
            }
            result
        })
    }
    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, StorageError>> {
        self.inner.write(bytes, expected_version)
    }
    fn delete<'a>(
        &'a self,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        self.inner.delete(expected_version)
    }
}

/// 不支持并发保护的服务器：同步照常工作；读和“写前再读”之间的改动能被发现。
pub async fn read_before_write_scenario(
    set_external: &(dyn Fn(Vec<u8>) + Sync),
    sa: &dyn SyncStorage,
    sb: &dyn SyncStorage,
) {
    let a = Machine::new("rbw-a", "dev-a");
    let b = Machine::new("rbw-b", "dev-b");
    seed(&a);
    sync_ok(&a, sa, "A 首次").await;
    sync_ok(&b, sb, "B 首次").await;
    assert_same_content(&a, &b, "首次同步之后");

    b.add_host("external-host");
    let external = b.engine().export_package(PASSWORD).unwrap();
    a.add_host("a-local");
    let hooked = AfterRead {
        inner: sa,
        hook: Mutex::new(Some(Box::new(move || set_external(external)))),
    };
    let report = a
        .engine()
        .sync(&hooked, PASSWORD, Some(DeletionDecision::Continue))
        .await
        .expect("写前再读发现变化后重试");
    assert_eq!(report.status, SyncStatus::Completed);
    assert!(
        report.retries >= 1,
        "写前再读发现云端变了，要重试：{}",
        report.retries
    );
    assert!(a.host_names().contains(&"external-host".to_owned()));
    assert!(a.host_names().contains(&"a-local".to_owned()));
    sync_ok(&b, sb, "B").await;
    sync_ok(&a, sa, "A").await;
    assert_same_content(&a, &b, "降级模式下重试之后");
}
