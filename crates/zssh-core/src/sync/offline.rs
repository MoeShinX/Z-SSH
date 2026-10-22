//! 离线备份：把主机、分组、凭据（含密码和私钥）、代理、可同步的设置和 known_hosts 导出成
//! 一个用密码加密的文件，换电脑或不想用云同步时手动搬。
//!
//! 文件格式就是同步包（[`super::crypto`]：Argon2id + XChaCha20-Poly1305），不另造一套。
//! 导入有两种：
//!
//! - **合并**：两边都留；同一条记录（同一个 uid）两边都有时按修改时间取新的，另一份进冲突页。
//! - **替换**：本机变成和文件一模一样，文件里没有的本机记录会被删掉。
//!
//! 两种都会先自动做一份本地备份（和恢复备份一样），导错了可以在备份页恢复回去。
//! 端口转发规则只存本机，不在文件里。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::apply::{apply_snapshot_guarded_with, ApplyReport, LocalGuard};
use super::backup::{renew_deleted_uids, BackupInfo};
use super::crypto::{seal_with, KdfParams};
use super::engine::SyncEngine;
use super::error::SyncError;
use super::merge::{merge_with, null_missing_references};
use super::package::SyncSnapshot;
use super::settings::sanitize_settings;
use super::snapshot::snapshot_local_with;

/// 导出密码最短几个字符：文件可能被拷到 U 盘、网盘，弱密码等于明文。
pub const MIN_PASSWORD_CHARS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineImportMode {
    Merge,
    Replace,
}

#[derive(Debug, Clone, Serialize)]
pub struct OfflineImportReport {
    /// 导入前自动做的那份本地备份。
    pub safety_backup: BackupInfo,
    pub applied: ApplyReport,
    /// 合并时两边都改过、进了冲突页的条数。
    pub conflicts: usize,
    pub nulled_references: Vec<String>,
}

fn random_bytes<const N: usize>() -> Result<[u8; N], SyncError> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out)
        .map_err(|err| SyncError::Internal(format!("系统随机数不可用：{err}")))?;
    Ok(out)
}

impl SyncEngine<'_> {
    /// 导出本机数据，返回加密后的文件内容。
    pub fn export_offline(&self, password: &str) -> Result<Vec<u8>, SyncError> {
        self.export_offline_with(password, KdfParams::default())
    }

    pub(crate) fn export_offline_with(
        &self,
        password: &str,
        params: KdfParams,
    ) -> Result<Vec<u8>, SyncError> {
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(SyncError::InvalidData(format!(
                "备份密码至少 {MIN_PASSWORD_CHARS} 个字符"
            )));
        }
        let now = Self::now();
        let mut snapshot = snapshot_local_with(
            self.db,
            self.sealer,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            now,
            &self.policy,
        )?;
        // 墓碑是"这台电脑删过什么"，带到别的电脑上只会误删那边的东西
        snapshot.tombstones.clear();
        snapshot.sort_canonical();
        let plaintext = zeroize::Zeroizing::new(snapshot.to_json()?);
        seal_with(
            &plaintext,
            password,
            params,
            &random_bytes::<{ super::crypto::SALT_BYTES }>()?,
            &random_bytes::<{ super::crypto::NONCE_BYTES }>()?,
        )
    }

    /// 导入离线备份文件。密码错、文件坏都在写本机之前报错，本机一个字节不动。
    pub fn import_offline(
        &self,
        bytes: &[u8],
        password: &str,
        mode: OfflineImportMode,
    ) -> Result<OfflineImportReport, SyncError> {
        let now = Self::now();
        let mut incoming = SyncSnapshot::open(bytes, password)?;
        incoming.tombstones.clear();

        let current = snapshot_local_with(
            self.db,
            self.sealer,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            now,
            &self.policy,
        )?;
        // 文件里有、但本机删过的记录（有墓碑）：原样写回会在下一次云同步被墓碑再删掉。
        // 像恢复备份一样换新 uid，导入就是一次新建。
        let mut deletions = current.tombstones.clone();
        if let Some(baseline) = self.load_baseline()? {
            deletions.extend(baseline.tombstones);
        }
        renew_deleted_uids(&mut incoming, &deletions);

        let (mut target, conflicts, nulled_references) = match mode {
            OfflineImportMode::Replace => {
                let mut target = incoming;
                target.settings = sanitize_settings(&target.settings, &self.policy).0;
                let nulled = null_missing_references(&mut target);
                // 本机的墓碑照旧保留，云同步的删除保护不受影响
                target.tombstones = current.tombstones.clone();
                (target, Vec::new(), nulled)
            }
            OfflineImportMode::Merge => {
                let outcome = merge_with(
                    None,
                    &current,
                    &incoming,
                    now,
                    &self.policy,
                    &BTreeMap::new(),
                )?;
                (
                    outcome.snapshot,
                    outcome.conflicts,
                    outcome.nulled_references,
                )
            }
        };
        target.sort_canonical();

        let keep = self.db.sync_backup_keep().map_err(SyncError::from)?;
        let safety_backup = self.backup_store().create(
            &current,
            &super::local_extras::LocalExtras::read(self.db)?,
            now,
            keep,
            self.app_version.as_deref(),
        )?;
        let applied = apply_snapshot_guarded_with(
            self.db,
            &target,
            self.sealer,
            self.known_hosts_path.as_deref(),
            &conflicts,
            now,
            Some(LocalGuard {
                expected: &current,
                device_id: &self.device_id,
            }),
            &self.policy,
        )?;
        Ok(OfflineImportReport {
            safety_backup,
            applied,
            conflicts: conflicts.len(),
            nulled_references,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, HostDraft, IdentityDraft, IdentityKind, ProxyMode};
    use crate::store::Database;
    use crate::sync::sealer::{DpapiSealer, SecretSealer, TestSealer};

    const PASSWORD: &str = "offline-test-password";

    /// 测试用最低的 KDF 参数：debug 构建下默认参数解一次要好几秒。
    fn fast() -> KdfParams {
        KdfParams {
            memory_kib: 8 * 1024,
            iterations: 1,
            ..KdfParams::default()
        }
    }

    fn host(name: &str, identity_id: Option<i64>) -> HostDraft {
        HostDraft {
            id: None,
            group_id: None,
            name: name.to_owned(),
            host: format!("{name}.example.invalid"),
            port: 22,
            username: "tester".to_owned(),
            auth_method: AuthMethod::Password,
            identity_id,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
        }
    }

    fn names(db: &Database) -> Vec<String> {
        let mut names: Vec<String> = db
            .host_list()
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        names.sort();
        names
    }

    struct Machine {
        db: Database,
        dir: crate::test_util::TestDir,
    }

    impl Machine {
        fn new(tag: &str) -> Self {
            let dir = crate::test_util::TestDir::new(&format!("offline-{tag}"));
            Self {
                db: Database::open_in_memory().unwrap(),
                dir,
            }
        }

        fn engine<'a>(&'a self, sealer: &'a dyn SecretSealer, device: &str) -> SyncEngine<'a> {
            SyncEngine::new(&self.db, sealer, device, &*self.dir, None, None)
        }
    }

    /// 导出方：库里的凭据是真的 DPAPI 密文，所以导出用 DpapiSealer。
    fn source_file() -> (Machine, Vec<u8>) {
        let sealer = DpapiSealer;
        let source = Machine::new("source");
        let identity = source
            .db
            .identity_save(&IdentityDraft {
                id: None,
                name: "部署密码".to_owned(),
                kind: IdentityKind::Password,
                key_path: None,
                secret: Some("not-a-real-password".to_owned()),
                passphrase: None,
            })
            .unwrap();
        source.db.host_save(&host("web", None)).unwrap();
        source.db.host_save(&host("db", Some(identity.id))).unwrap();
        let bytes = source
            .engine(&sealer, "dev-a")
            .export_offline_with(PASSWORD, fast())
            .unwrap();
        (source, bytes)
    }

    #[test]
    fn short_password_is_refused() {
        let sealer = TestSealer(1);
        let machine = Machine::new("short");
        let err = machine
            .engine(&sealer, "dev-a")
            .export_offline_with("1234567", fast())
            .unwrap_err();
        assert!(err.to_string().contains("至少"), "{err}");
    }

    #[test]
    fn wrong_password_changes_nothing() {
        let sealer = DpapiSealer;
        let (_source, bytes) = source_file();
        let target = Machine::new("wrong");
        target.db.host_save(&host("local", None)).unwrap();
        let err = target
            .engine(&sealer, "dev-b")
            .import_offline(&bytes, "not-the-password", OfflineImportMode::Replace)
            .unwrap_err();
        assert!(matches!(err, SyncError::WrongPassword), "{err}");
        assert_eq!(names(&target.db), vec!["local"]);
    }

    #[test]
    fn merge_keeps_both_sides_and_brings_secrets_along() {
        let (_source, bytes) = source_file();
        // 另一台电脑的"DPAPI"不同：凭据要在那边用那边的密钥重新加密
        let sealer_b = TestSealer(2);
        let target = Machine::new("merge");
        target.db.host_save(&host("local", None)).unwrap();
        let report = target
            .engine(&sealer_b, "dev-b")
            .import_offline(&bytes, PASSWORD, OfflineImportMode::Merge)
            .unwrap();
        assert_eq!(names(&target.db), vec!["db", "local", "web"]);
        assert_eq!(report.applied.hosts.inserted, 2);
        assert!(report.safety_backup.name.starts_with("zssh-backup-"));

        let identity = &target.db.identity_list().unwrap()[0];
        assert!(identity.has_secret);
        let db_host = target
            .db
            .host_list()
            .unwrap()
            .into_iter()
            .find(|h| h.name == "db")
            .unwrap();
        assert_eq!(
            db_host.identity_id,
            Some(identity.id),
            "引用跟着换成本机 id"
        );
    }

    #[test]
    fn replace_makes_this_machine_match_the_file() {
        let sealer = DpapiSealer;
        let (_source, bytes) = source_file();
        let target = Machine::new("replace");
        target.db.host_save(&host("local", None)).unwrap();
        target
            .engine(&sealer, "dev-b")
            .import_offline(&bytes, PASSWORD, OfflineImportMode::Replace)
            .unwrap();
        assert_eq!(names(&target.db), vec!["db", "web"]);
    }

    /// 复审 F4：替换导入删掉的本机主机，连同它的长期记忆和转发规则，恢复导入前的自动备份后都回来。
    #[test]
    fn restoring_the_safety_backup_brings_back_memory_and_forwards() {
        use crate::store::forwards::{ForwardKind, PortForwardDraft};

        let sealer = DpapiSealer;
        let (_source, bytes) = source_file();
        let target = Machine::new("restore-extras");
        let local_id = target.db.host_save(&host("local", None)).unwrap().id;
        target.db.host_memory_set(local_id, "服务：nginx").unwrap();
        target
            .db
            .forward_save(&PortForwardDraft {
                id: None,
                host_id: local_id,
                name: "数据库".to_owned(),
                kind: ForwardKind::Local,
                bind_host: "127.0.0.1".to_owned(),
                bind_port: 15432,
                dest_host: "localhost".to_owned(),
                dest_port: 5432,
                auto_start: true,
            })
            .unwrap();

        let engine = target.engine(&sealer, "dev-b");
        let report = engine
            .import_offline(&bytes, PASSWORD, OfflineImportMode::Replace)
            .unwrap();
        assert!(
            target.db.forward_list().unwrap().is_empty(),
            "替换导入删掉了主机和它的规则"
        );

        let restored = engine
            .restore_backup(std::path::Path::new(&report.safety_backup.path))
            .unwrap();
        assert_eq!(
            (restored.extras.host_memory, restored.extras.forwards),
            (1, 1)
        );
        let local = target
            .db
            .host_list()
            .unwrap()
            .into_iter()
            .find(|h| h.name == "local")
            .expect("主机回来了");
        assert_eq!(
            target
                .db
                .host_memory_get(local.id)
                .unwrap()
                .unwrap()
                .content,
            "服务：nginx"
        );
        let forwards = target.db.forward_list().unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(
            (forwards[0].host_id, forwards[0].bind_port),
            (local.id, 15432)
        );
    }
}
