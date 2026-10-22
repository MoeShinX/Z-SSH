//! 同步引擎：本地快照 ↔ 加密同步包 ↔ 云端存储。
//!
//! - 基线（上次同步成功时的快照）用本机 DPAPI 加密存在数据目录；
//! - 每次合并 / 导入 / 恢复之前自动做一份本地备份；
//! - 版本冲突时重新读取、重新合并、再写，最多 [`MAX_ATTEMPTS`] 次；
//! - 主密码只用于同步包，不落盘；日志只记数量，不记内容。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::apply::{apply_snapshot_guarded_with, ApplyReport, LocalGuard};
use super::backup::{BackupInfo, BackupStore, RestoreReport};
use super::error::SyncError;
use super::guard::{deletion_plan, DeletionDecision, DeletionPlan};
use super::merge::{merge_with, null_missing_references, Conflict};
use super::package::SyncSnapshot;
use super::sealer::SecretSealer;
use super::settings::{
    compute_pending, same_data_for_local, sanitize_settings, SettingsPolicy,
    SYNC_PENDING_SETTINGS_KEY,
};
use super::snapshot::snapshot_local_with;
use super::storage::{StorageError, StoredObject, SyncStorage};

/// 版本冲突 / 超时之后最多重来几次。
pub const MAX_ATTEMPTS: usize = 4;
const MAX_READ_RETRIES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    Completed,
    /// 云端已经更新，但合并和写本地之间用户又改了本地：本地没动、基线没存，
    /// 调用方应稍后重新同步。
    LocalChanged,
    NeedsConfirmation,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub status: SyncStatus,
    pub summary: String,
    pub conflicts: Vec<Conflict>,
    pub nulled_references: Vec<String>,
    pub deletion: DeletionPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<ApplyReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_version: Option<String>,
    pub retries: usize,
}

/// 合并 + 防误删之后的决定。
enum MergeDecision {
    NeedsConfirmation(DeletionPlan),
    Cancelled(DeletionPlan),
    Apply {
        target: Box<SyncSnapshot>,
        conflicts: Vec<Conflict>,
        nulled_references: Vec<String>,
        deletion: DeletionPlan,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct BaselineFile {
    schema_version: u32,
    snapshot: SyncSnapshot,
}

const BASELINE_VERSION: u32 = 1;

pub struct SyncEngine<'a> {
    pub db: &'a crate::store::Database,
    pub sealer: &'a dyn SecretSealer,
    pub device_id: String,
    pub data_dir: PathBuf,
    pub known_hosts_path: Option<PathBuf>,
    pub app_version: Option<String>,
    /// 设置的同步规则。正式代码恒为 [`SettingsPolicy::CURRENT`]，测试用它模拟别的版本。
    pub policy: SettingsPolicy,
}

impl<'a> SyncEngine<'a> {
    pub fn new(
        db: &'a crate::store::Database,
        sealer: &'a dyn SecretSealer,
        device_id: impl Into<String>,
        data_dir: impl Into<PathBuf>,
        known_hosts_path: Option<PathBuf>,
        app_version: Option<String>,
    ) -> Self {
        Self {
            db,
            sealer,
            device_id: device_id.into(),
            data_dir: data_dir.into(),
            known_hosts_path,
            app_version,
            policy: SettingsPolicy::CURRENT,
        }
    }

    /// 换一套设置规则（测试里模拟新旧版本的电脑）。
    pub fn with_policy(mut self, policy: SettingsPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub(crate) fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }

    pub(crate) fn backup_store(&self) -> BackupStore<'_> {
        BackupStore::new(&self.data_dir, self.sealer)
    }

    fn local(&self, now: i64) -> Result<SyncSnapshot, SyncError> {
        snapshot_local_with(
            self.db,
            self.sealer,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            now,
            &self.policy,
        )
    }

    fn baseline_path(&self) -> PathBuf {
        self.data_dir.join("sync-baseline.bin")
    }

    /// 读上次同步成功时的基线。文件不存在返回 `None`；文件坏了要报错，
    /// 不能悄悄当成“首次同步”（那会让删掉的记录复活）。
    pub fn load_baseline(&self) -> Result<Option<SyncSnapshot>, SyncError> {
        let path = self.baseline_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(SyncError::Io(err)),
        };
        let plaintext = self.sealer.open(&bytes)?;
        let file: BaselineFile = serde_json::from_slice(&plaintext)
            .map_err(|err| SyncError::Format(format!("同步基线损坏：{err}")))?;
        if file.schema_version > BASELINE_VERSION {
            return Err(SyncError::FormatTooNew {
                found: file.schema_version,
                supported: BASELINE_VERSION,
            });
        }
        Ok(Some(file.snapshot))
    }

    pub fn save_baseline(&self, snapshot: &SyncSnapshot) -> Result<(), SyncError> {
        std::fs::create_dir_all(&self.data_dir)?;
        let file = BaselineFile {
            schema_version: BASELINE_VERSION,
            snapshot: snapshot.clone(),
        };
        let plaintext = zeroize::Zeroizing::new(
            serde_json::to_vec(&file)
                .map_err(|err| SyncError::Internal(format!("基线序列化失败：{err}")))?,
        );
        let sealed = self.sealer.seal(&plaintext)?;
        let path = self.baseline_path();
        let temp = self.data_dir.join(".sync-baseline.tmp");
        std::fs::write(&temp, sealed)?;
        // 原子替换：不再"先删旧基线再改名"（改名失败就没有基线了）。
        crate::sftp::local::replace_file(&temp, &path)?;
        Ok(())
    }

    /// 导出：本机数据 → 加密同步包（不连任何存储）。
    pub fn export_package(&self, master_password: &str) -> Result<Vec<u8>, SyncError> {
        let now = Self::now();
        let snapshot = self.local(now)?;
        snapshot.seal(master_password)
    }

    /// 导入：解密一个同步包，和本地三方合并后写回（一个事务），不上传。
    pub fn import_package(
        &self,
        package: &[u8],
        master_password: &str,
        decision: Option<DeletionDecision>,
    ) -> Result<SyncReport, SyncError> {
        let now = Self::now();
        let remote = SyncSnapshot::open(package, master_password)?;
        let local = self.local(now)?;
        let base = self.load_baseline()?;
        let plan = self.merge_with_decision(base.as_ref(), &local, &remote, decision, now)?;
        let (target, conflicts, nulled_references, deletion) = match plan {
            MergeDecision::NeedsConfirmation(deletion) => {
                return Ok(SyncReport {
                    status: SyncStatus::NeedsConfirmation,
                    summary: deletion.summary(),
                    conflicts: Vec::new(),
                    nulled_references: Vec::new(),
                    deletion,
                    backup: None,
                    applied: None,
                    cloud_version: None,
                    retries: 0,
                })
            }
            MergeDecision::Cancelled(deletion) => {
                return Ok(SyncReport {
                    status: SyncStatus::Cancelled,
                    summary: "已取消这次同步，本地和同步包都没有改动".to_owned(),
                    conflicts: Vec::new(),
                    nulled_references: Vec::new(),
                    deletion,
                    backup: None,
                    applied: None,
                    cloud_version: None,
                    retries: 0,
                })
            }
            MergeDecision::Apply {
                target,
                conflicts,
                nulled_references,
                deletion,
            } => (*target, conflicts, nulled_references, deletion),
        };

        let mut backup = None;
        let mut applied = None;
        if !same_data_for_local(&target, &local, &self.policy) {
            backup = self.create_backup(&local, now)?;
            match self.apply_guarded(&target, &conflicts, &local, now) {
                Ok(report) => applied = Some(report),
                Err(SyncError::LocalChanged) => {
                    return Ok(SyncReport {
                        status: SyncStatus::LocalChanged,
                        summary: SyncError::LocalChanged.to_string(),
                        conflicts: Vec::new(),
                        nulled_references: Vec::new(),
                        deletion,
                        backup,
                        applied: None,
                        cloud_version: None,
                        retries: 0,
                    })
                }
                Err(err) => return Err(err),
            }
        }
        self.record_pending(&target, &local)?;
        self.save_baseline(&target)?;
        tracing::info!(
            groups = target.groups.len(),
            hosts = target.hosts.len(),
            identities = target.identities.len(),
            proxies = target.proxies.len(),
            conflicts = conflicts.len(),
            "同步包已导入"
        );
        Ok(SyncReport {
            status: SyncStatus::Completed,
            summary: format!("导入完成：{} 条记录", target.record_count()),
            conflicts,
            nulled_references,
            deletion,
            backup,
            applied,
            cloud_version: None,
            retries: 0,
        })
    }

    /// 和云端同步一次。`decision` 只在防误删触发时使用（`None` = 需要上层确认）。
    pub async fn sync(
        &self,
        storage: &dyn SyncStorage,
        master_password: &str,
        decision: Option<DeletionDecision>,
    ) -> Result<SyncReport, SyncError> {
        let now = Self::now();
        let base = self.load_baseline()?;
        let local = self.local(now)?;
        let mut remote = read_cloud(storage).await?;
        let mut retries = 0usize;
        let mut backup: Option<BackupInfo> = None;
        let mut applied: Option<ApplyReport> = None;

        loop {
            let (remote_snapshot, remote_version) = match &remote {
                Some(object) => {
                    let snapshot = SyncSnapshot::open(&object.bytes, master_password)?;
                    (Some(snapshot), Some(object.version.clone()))
                }
                None => (None, None),
            };

            let plan = match &remote_snapshot {
                Some(remote_snapshot) => {
                    self.merge_with_decision(base.as_ref(), &local, remote_snapshot, decision, now)?
                }
                None => MergeDecision::Apply {
                    target: Box::new(local.clone()),
                    conflicts: Vec::new(),
                    nulled_references: Vec::new(),
                    deletion: DeletionPlan::default(),
                },
            };

            let (target, conflicts, nulled_references, deletion) = match plan {
                MergeDecision::NeedsConfirmation(deletion) => {
                    return Ok(SyncReport {
                        status: SyncStatus::NeedsConfirmation,
                        summary: deletion.summary(),
                        conflicts: Vec::new(),
                        nulled_references: Vec::new(),
                        deletion,
                        backup,
                        applied,
                        cloud_version: remote_version,
                        retries,
                    })
                }
                MergeDecision::Cancelled(deletion) => {
                    return Ok(SyncReport {
                        status: SyncStatus::Cancelled,
                        summary: "已取消这次同步，本地和云端都没有改动".to_owned(),
                        conflicts: Vec::new(),
                        nulled_references: Vec::new(),
                        deletion,
                        backup,
                        applied,
                        cloud_version: remote_version,
                        retries,
                    })
                }
                MergeDecision::Apply {
                    target,
                    conflicts,
                    nulled_references,
                    deletion,
                } => (*target, conflicts, nulled_references, deletion),
            };

            let needs_apply = !same_data_for_local(&target, &local, &self.policy);
            if needs_apply && backup.is_none() {
                // 在改本地之前备份一次“同步前”的状态。上传冲突重试时本地还没动，
                // 不需要再备份。
                backup = self.create_backup(&local, now)?;
            }

            let bytes = target.seal(master_password)?;
            let expected = remote.as_ref().map(|object| object.version.as_str());
            match storage.write(&bytes, expected).await {
                Ok(version) => {
                    // 先上传再改本地：上传失败（版本冲突 / 超时）时本地保持原样，
                    // 重试会从同一份本地状态重新合并，不会出现“云端没写成、本地
                    // 已经删了”的半截状态。
                    if needs_apply {
                        match self.apply_guarded(&target, &conflicts, &local, now) {
                            Ok(report) => applied = Some(report),
                            Err(SyncError::LocalChanged) => {
                                // 云端已经是新版本；本地和基线都不动，下次同步时
                                // 用户的新改动按“相对旧基线的改动”合进去。
                                return Ok(SyncReport {
                                    status: SyncStatus::LocalChanged,
                                    summary: SyncError::LocalChanged.to_string(),
                                    conflicts: Vec::new(),
                                    nulled_references,
                                    deletion,
                                    backup,
                                    applied: None,
                                    cloud_version: Some(version),
                                    retries,
                                });
                            }
                            Err(err) => return Err(err),
                        }
                    }
                    // 只有上传成功才更新基线；上传失败时下次同步还能把本地改动
                    // 当成“相对旧基线的改动”重新合进去。
                    self.record_pending(&target, &local)?;
                    self.save_baseline(&target)?;
                    tracing::info!(
                        groups = target.groups.len(),
                        hosts = target.hosts.len(),
                        identities = target.identities.len(),
                        proxies = target.proxies.len(),
                        conflicts = conflicts.len(),
                        retries,
                        "同步完成"
                    );
                    return Ok(SyncReport {
                        status: SyncStatus::Completed,
                        summary: format!(
                            "同步完成：{} 条记录，上传版本 {version}",
                            target.record_count()
                        ),
                        conflicts,
                        nulled_references,
                        deletion,
                        backup,
                        applied,
                        cloud_version: Some(version),
                        retries,
                    });
                }
                Err(
                    err @ (StorageError::Conflict
                    | StorageError::Timeout
                    | StorageError::Unavailable(_)),
                ) if retries + 1 < MAX_ATTEMPTS => {
                    retries += 1;
                    tracing::debug!(?err, retries, "云端写入冲突 / 超时，重新读取后重试");
                    remote = read_cloud(storage).await?;
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    /// 从本地备份恢复。恢复前先自动备份当前状态；恢复在一个事务里。
    pub fn restore_backup(&self, backup_path: &Path) -> Result<RestoreReport, SyncError> {
        let now = Self::now();
        let store = self.backup_store();
        let backup_path = store.check_path(backup_path)?;
        // 基线里的删除正常都已写进本机墓碑表；一起带上，两边万一不一致也不会漏。
        let baseline_deletions = self
            .load_baseline()?
            .map(|baseline| baseline.tombstones)
            .unwrap_or_default();
        let report = store.restore_with_known_deletions(
            self.db,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            &backup_path,
            &baseline_deletions,
            now,
        )?;
        // **不**重置同步基线：基线要保持"上次和云端对齐时的样子"，恢复出来的差异才会被当成本地
        // 改动推上云端。以前把恢复后的状态存成基线，下一次三方合并就看成"本地没改、云端改了"，
        // 自动同步约 30 秒后把恢复的内容又覆盖回去（R-10 H4）。恢复删掉的记录照常走删除保护。
        Ok(report)
    }

    /// 应用版本升级后第一次启动时自动备份（同一个版本只做一次）。
    pub fn backup_after_upgrade(&self, app_version: &str) -> Result<Option<BackupInfo>, SyncError> {
        let now = Self::now();
        self.backup_store().backup_after_upgrade_if_needed(
            self.db,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            app_version,
            now,
        )
    }

    /// 冲突列表（已清掉超过 90 天的）。
    pub fn list_conflicts(&self) -> Result<Vec<super::conflicts::ConflictView>, SyncError> {
        super::conflicts::list(self.db, self.sealer, &self.device_id, Self::now())
    }

    /// 改用另一份 / 恢复被删的记录。结果写回本地，下次同步时推上去。
    pub fn use_conflict_other(&self, id: i64) -> Result<(), SyncError> {
        super::conflicts::use_other(
            self.db,
            self.sealer,
            &self.device_id,
            self.known_hosts_path.as_deref(),
            id,
            Self::now(),
        )
    }

    /// “我知道了”：清掉一条冲突。
    pub fn dismiss_conflict(&self, id: i64) -> Result<(), SyncError> {
        super::conflicts::dismiss(self.db, id)
    }

    /// 本地备份列表（时间、应用版本、记录数）。
    pub fn list_backups(&self) -> Result<Vec<super::backup::BackupDetail>, SyncError> {
        self.backup_store().list_details()
    }

    /// 重建同步基线：删掉损坏的基线文件，下次同步按首次同步处理（两边的数据都保留）。
    pub fn reset_baseline(&self) -> Result<(), SyncError> {
        match std::fs::remove_file(self.baseline_path()) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(SyncError::Io(err)),
        }
    }

    /// 读本机记下的“因为值本版本不接受、没写进本机的同步键”。读不出来就报错（不能当成
    /// “没有记录”，那样会用本机的旧值覆盖云端）；内容坏了才当成没有，并记一条日志。
    fn load_pending(&self) -> Result<BTreeMap<String, serde_json::Value>, SyncError> {
        match self
            .db
            .setting_get(SYNC_PENDING_SETTINGS_KEY)
            .map_err(SyncError::from)?
        {
            None => Ok(BTreeMap::new()),
            Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_else(|err| {
                tracing::warn!(%err, "同步待定设置记录格式不对，按没有处理");
                BTreeMap::new()
            })),
        }
    }

    /// 同步或导入成功之后，重新算一遍并保存“没写进本机的同步键”。
    fn record_pending(&self, target: &SyncSnapshot, local: &SyncSnapshot) -> Result<(), SyncError> {
        let fresh = compute_pending(&target.settings, &local.settings, &self.policy);
        if fresh == self.load_pending()? {
            return Ok(());
        }
        let raw = serde_json::to_string(&fresh)
            .map_err(|err| SyncError::InvalidData(format!("待定设置记录序列化失败：{err}")))?;
        self.db
            .setting_set(SYNC_PENDING_SETTINGS_KEY, &raw)
            .map_err(SyncError::from)
    }

    fn apply_guarded(
        &self,
        target: &SyncSnapshot,
        conflicts: &[Conflict],
        local: &SyncSnapshot,
        now: i64,
    ) -> Result<ApplyReport, SyncError> {
        apply_snapshot_guarded_with(
            self.db,
            target,
            self.sealer,
            self.known_hosts_path.as_deref(),
            conflicts,
            now,
            Some(LocalGuard {
                expected: local,
                device_id: &self.device_id,
            }),
            &self.policy,
        )
    }

    /// 本地一条记录都没有时不备份（第二台电脑第一次同步就是这样）：备份一份空库没有意义，
    /// 还会白占一个保留名额把真正有用的旧备份挤掉。
    fn create_backup(
        &self,
        snapshot: &SyncSnapshot,
        now: i64,
    ) -> Result<Option<BackupInfo>, SyncError> {
        if snapshot.record_count() == 0 {
            return Ok(None);
        }
        let keep = self.db.sync_backup_keep().map_err(SyncError::from)?;
        self.backup_store()
            .create(
                snapshot,
                &super::local_extras::LocalExtras::read(self.db)?,
                now,
                keep,
                self.app_version.as_deref(),
            )
            .map(Some)
    }

    fn merge_with_decision(
        &self,
        base: Option<&SyncSnapshot>,
        local: &SyncSnapshot,
        remote: &SyncSnapshot,
        decision: Option<DeletionDecision>,
        now: i64,
    ) -> Result<MergeDecision, SyncError> {
        let pending = self.load_pending()?;
        let outcome = merge_with(base, local, remote, now, &self.policy, &pending)?;
        let plan = deletion_plan(local, &outcome.snapshot);
        if plan.needs_confirmation() {
            match decision {
                None => return Ok(MergeDecision::NeedsConfirmation(plan)),
                Some(DeletionDecision::Cancel) => return Ok(MergeDecision::Cancelled(plan)),
                Some(DeletionDecision::UseCloud) => {
                    let mut cloud = remote.clone();
                    cloud.settings = sanitize_settings(&cloud.settings, &self.policy).0;
                    let nulled_references = null_missing_references(&mut cloud);
                    cloud.sort_canonical();
                    return Ok(MergeDecision::Apply {
                        target: Box::new(cloud),
                        conflicts: Vec::new(),
                        nulled_references,
                        deletion: plan,
                    });
                }
                Some(DeletionDecision::Continue) => {}
            }
        } else if decision == Some(DeletionDecision::UseCloud) {
            // 没触发防误删但调用方明确选了“用云端覆盖”，按明确的选择办。
            let mut cloud = remote.clone();
            cloud.settings = sanitize_settings(&cloud.settings, &self.policy).0;
            let nulled_references = null_missing_references(&mut cloud);
            cloud.sort_canonical();
            return Ok(MergeDecision::Apply {
                target: Box::new(cloud),
                conflicts: Vec::new(),
                nulled_references,
                deletion: plan,
            });
        } else if decision == Some(DeletionDecision::Cancel) {
            return Ok(MergeDecision::Cancelled(plan));
        }

        Ok(MergeDecision::Apply {
            target: Box::new(outcome.snapshot),
            conflicts: outcome.conflicts,
            nulled_references: outcome.nulled_references,
            deletion: plan,
        })
    }
}

async fn read_cloud(storage: &dyn SyncStorage) -> Result<Option<StoredObject>, SyncError> {
    let mut attempts = 0usize;
    loop {
        match storage.read().await {
            Ok(object) => return Ok(object),
            Err(StorageError::Timeout | StorageError::Unavailable(_))
                if attempts < MAX_READ_RETRIES =>
            {
                attempts += 1;
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests;
