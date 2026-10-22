//! 本地自动备份：DPAPI 加密的快照，最多保留 N 份（默认 20）。
//!
//! 触发时机由调用方决定：合并云端数据之前、恢复之前、应用版本升级后第一次启动。
//! 恢复 = 先给当前状态做一份备份，再把选中的备份在一个数据库事务里写回去。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::apply::{apply_snapshot_guarded, ApplyReport, LocalGuard};
use super::error::SyncError;
use super::local_extras::{ExtrasRestored, LocalExtras};
use super::package::{
    SyncSnapshot, Tombstone, ENTITY_GROUP, ENTITY_HOST, ENTITY_IDENTITY, ENTITY_PROXY,
};
use super::sealer::SecretSealer;
use super::snapshot::snapshot_local;
use crate::store::settings::SYNC_LAST_BACKUP_VERSION_KEY;

/// 备份文件的明文外层（整体再 DPAPI 加密）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEnvelope {
    pub schema_version: u32,
    pub created_at: i64,
    #[serde(default)]
    pub app_version: Option<String>,
    pub snapshot: SyncSnapshot,
    /// 只在本机的主机记忆和转发规则（同步包里没有；旧备份没有这个字段，按空处理）。
    #[serde(default)]
    pub local: LocalExtras,
}

pub const BACKUP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// 文件名里的时间（`YYYYMMDD-HHMMSS`，UTC）。
    pub stamp: String,
    /// 同一秒内的序号（1 开始）。
    pub sequence: u64,
}

/// 备份列表里的一行：文件信息 + 解开后的时间、应用版本、记录数。
/// 解不开的备份（比如换过 Windows 用户）`readable` 为 false，其余字段为空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupDetail {
    #[serde(flatten)]
    pub info: BackupInfo,
    pub readable: bool,
    pub created_at: Option<i64>,
    pub app_version: Option<String>,
    pub record_count: Option<usize>,
    pub host_count: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct RestoreReport {
    /// 恢复前自动做的那份备份。
    pub safety_backup: BackupInfo,
    pub applied: ApplyReport,
    /// 跟着恢复回来的主机记忆和转发规则。
    pub extras: ExtrasRestored,
}

pub struct BackupStore<'a> {
    dir: PathBuf,
    sealer: &'a dyn SecretSealer,
}

impl<'a> BackupStore<'a> {
    pub fn new(data_dir: &Path, sealer: &'a dyn SecretSealer) -> Self {
        Self {
            dir: data_dir.join("backups"),
            sealer,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 文件名必须严格是 `zssh-backup-YYYYMMDD-HHMMSS[-N].bin`，别的文件一律不碰。
    fn parse_name(name: &str) -> Option<(String, u64)> {
        let rest = name.strip_prefix("zssh-backup-")?.strip_suffix(".bin")?;
        let (stamp, sequence) = match rest.split_once('-') {
            Some((date, after)) => {
                let (time, sequence) = match after.split_once('-') {
                    Some((time, sequence)) => (time, Some(sequence)),
                    None => (after, None),
                };
                let stamp = if date.len() == 8
                    && time.len() == 6
                    && date.chars().all(|c| c.is_ascii_digit())
                    && time.chars().all(|c| c.is_ascii_digit())
                {
                    format!("{date}-{time}")
                } else {
                    return None;
                };
                let sequence = match sequence {
                    None => 1,
                    Some(raw) if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) => {
                        raw.parse().ok()?
                    }
                    Some(_) => return None,
                };
                (stamp, sequence)
            }
            None => return None,
        };
        Some((stamp, sequence))
    }

    /// 写一份备份，然后按 `keep` 清理旧备份。返回新备份的信息。
    pub fn create(
        &self,
        snapshot: &SyncSnapshot,
        local: &LocalExtras,
        now: i64,
        keep: usize,
        app_version: Option<&str>,
    ) -> Result<BackupInfo, SyncError> {
        std::fs::create_dir_all(&self.dir)?;
        let envelope = BackupEnvelope {
            schema_version: BACKUP_SCHEMA_VERSION,
            created_at: now,
            app_version: app_version.map(str::to_owned),
            snapshot: snapshot.clone(),
            local: local.clone(),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&envelope)
                .map_err(|err| SyncError::Internal(format!("备份序列化失败：{err}")))?,
        );
        let sealed = self.sealer.seal(&plaintext)?;
        let stamp = crate::ai::files::compact_timestamp(now);
        let mut sequence = 1u64;
        let path = loop {
            let name = if sequence == 1 {
                format!("zssh-backup-{stamp}.bin")
            } else {
                format!("zssh-backup-{stamp}-{sequence}.bin")
            };
            let candidate = self.dir.join(&name);
            if !candidate.exists() {
                break candidate;
            }
            sequence += 1;
            if sequence > 1000 {
                return Err(SyncError::Internal("同一秒内的备份太多了".to_owned()));
            }
        };
        let temp = self.dir.join(format!(".{stamp}-{sequence}.tmp"));
        std::fs::write(&temp, &sealed)?;
        if let Err(err) = crate::sftp::local::move_no_replace(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            return Err(SyncError::Io(err));
        }
        self.prune(keep)?;
        let size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or_default();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        Ok(BackupInfo {
            path: path.display().to_string(),
            name,
            size,
            stamp,
            sequence,
        })
    }

    /// 按文件名排序（时间 → 序号），删掉超出 `keep` 的旧备份。
    pub fn prune(&self, keep: usize) -> Result<usize, SyncError> {
        let keep = keep.max(1);
        if !self.dir.exists() {
            return Ok(0);
        }
        let mut backups: Vec<(String, u64, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((stamp, sequence)) = Self::parse_name(&name) {
                backups.push((stamp, sequence, entry.path()));
            }
        }
        backups.sort();
        let excess = backups.len().saturating_sub(keep);
        let mut removed = 0usize;
        for (_, _, path) in backups.into_iter().take(excess) {
            std::fs::remove_file(path)?;
            removed += 1;
        }
        Ok(removed)
    }

    pub fn list(&self) -> Result<Vec<BackupInfo>, SyncError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut backups = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((stamp, sequence)) = Self::parse_name(&name) else {
                continue;
            };
            backups.push(BackupInfo {
                path: entry.path().display().to_string(),
                size: entry.metadata().map(|m| m.len()).unwrap_or_default(),
                name,
                stamp,
                sequence,
            });
        }
        backups.sort_by(|a, b| (&b.stamp, b.sequence).cmp(&(&a.stamp, a.sequence)));
        Ok(backups)
    }

    pub fn list_details(&self) -> Result<Vec<BackupDetail>, SyncError> {
        let mut details = Vec::new();
        for info in self.list()? {
            let loaded = self.load(Path::new(&info.path));
            details.push(match loaded {
                Ok(envelope) => BackupDetail {
                    readable: true,
                    created_at: Some(envelope.created_at),
                    app_version: envelope.app_version.clone(),
                    record_count: Some(envelope.snapshot.record_count()),
                    host_count: Some(envelope.snapshot.hosts.len()),
                    info,
                },
                Err(_) => BackupDetail {
                    readable: false,
                    created_at: None,
                    app_version: None,
                    record_count: None,
                    host_count: None,
                    info,
                },
            });
        }
        Ok(details)
    }

    /// 只认备份目录里、文件名符合备份命名的文件（界面传来的路径不可信）。
    pub fn check_path(&self, path: &Path) -> Result<PathBuf, SyncError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| SyncError::InvalidData("备份路径不合法".to_owned()))?;
        if Self::parse_name(name).is_none() {
            return Err(SyncError::InvalidData("不是 Z-SSH 的备份文件".to_owned()));
        }
        let candidate = self.dir.join(name);
        let same_dir = match (path.parent(), self.dir.canonicalize()) {
            (Some(parent), Ok(dir)) => parent.canonicalize().map(|p| p == dir).unwrap_or(false),
            _ => false,
        };
        if !same_dir || !candidate.is_file() {
            return Err(SyncError::InvalidData("备份文件不在备份目录里".to_owned()));
        }
        Ok(candidate)
    }

    pub fn load(&self, path: &Path) -> Result<BackupEnvelope, SyncError> {
        let bytes = std::fs::read(path)?;
        let plaintext = self.sealer.open(&bytes)?;
        let envelope: BackupEnvelope = serde_json::from_slice(&plaintext)
            .map_err(|err| SyncError::Format(format!("备份文件解析失败：{err}")))?;
        if envelope.schema_version > BACKUP_SCHEMA_VERSION {
            return Err(SyncError::FormatTooNew {
                found: envelope.schema_version,
                supported: BACKUP_SCHEMA_VERSION,
            });
        }
        Ok(envelope)
    }

    /// 恢复之前先给当前状态自动备份，再把选中的备份写回（一个事务）。
    pub fn restore(
        &self,
        db: &crate::store::Database,
        device_id: &str,
        known_hosts_path: Option<&Path>,
        backup_path: &Path,
        now: i64,
    ) -> Result<RestoreReport, SyncError> {
        self.restore_with_known_deletions(db, device_id, known_hosts_path, backup_path, &[], now)
    }

    /// 同 [`Self::restore`]，`known_deletions` 是本机墓碑表以外也知道的删除（同步基线里的墓碑）。
    pub fn restore_with_known_deletions(
        &self,
        db: &crate::store::Database,
        device_id: &str,
        known_hosts_path: Option<&Path>,
        backup_path: &Path,
        known_deletions: &[Tombstone],
        now: i64,
    ) -> Result<RestoreReport, SyncError> {
        let envelope = self.load(backup_path)?;
        let current = snapshot_local(db, self.sealer, device_id, known_hosts_path, now)?;
        let keep = db.sync_backup_keep()?;
        let safety_backup = self.create(
            &current,
            &LocalExtras::read(db)?,
            now,
            keep,
            envelope.app_version.as_deref(),
        )?;
        // 安全备份和写入之间用户又改了本地的话，备份就不是“恢复前的状态”了，
        // 所以带上同样的检查。
        // 备份里的记录如果在备份之后被删过（本机有它的有效墓碑），原样写回会在下一次同步
        // 被墓碑再删一遍，云端和别的设备的墓碑也清不掉。像冲突页“恢复被删的记录”一样给它们
        // 换新 UID（引用跟着改），恢复才是一次新建，不会被撤销（R7-02）。
        let mut snapshot = envelope.snapshot.clone();
        let deletions: Vec<Tombstone> = current
            .tombstones
            .iter()
            .chain(known_deletions)
            .cloned()
            .collect();
        let renamed = renew_deleted_uids(&mut snapshot, &deletions);
        let applied = apply_snapshot_guarded(
            db,
            &snapshot,
            self.sealer,
            known_hosts_path,
            &[],
            now,
            Some(LocalGuard {
                expected: &current,
                device_id,
            }),
        )?;
        // 主机回来了，它的记忆和转发规则也回来（复审 F4）
        let extras = envelope.local.restore(db, &renamed)?;
        Ok(RestoreReport {
            safety_backup,
            applied,
            extras,
        })
    }

    /// 应用版本升级后第一次启动：备份一次并把版本记下来。
    pub fn backup_after_upgrade_if_needed(
        &self,
        db: &crate::store::Database,
        device_id: &str,
        known_hosts_path: Option<&Path>,
        app_version: &str,
        now: i64,
    ) -> Result<Option<BackupInfo>, SyncError> {
        let last = db
            .setting_get(SYNC_LAST_BACKUP_VERSION_KEY)
            .map_err(SyncError::from)?;
        let last_version = last
            .as_deref()
            .and_then(|raw| serde_json::from_str::<String>(raw).ok());
        if last_version.as_deref() == Some(app_version) {
            return Ok(None);
        }
        let snapshot = snapshot_local(db, self.sealer, device_id, known_hosts_path, now)?;
        let value = serde_json::to_string(app_version)
            .map_err(|err| SyncError::Internal(format!("版本号序列化失败：{err}")))?;
        // 全新安装第一次启动时库是空的，备份一份空库没有意义，还白占一个备份名额。
        // 版本照样记下：这个版本的“升级”已经过去了，之后同一版本里有了数据也不用补备份。
        if snapshot.record_count() == 0 {
            db.setting_set(SYNC_LAST_BACKUP_VERSION_KEY, &value)
                .map_err(SyncError::from)?;
            return Ok(None);
        }
        let keep = db.sync_backup_keep()?;
        let info = self.create(
            &snapshot,
            &LocalExtras::read(db)?,
            now,
            keep,
            Some(app_version),
        )?;
        db.setting_set(SYNC_LAST_BACKUP_VERSION_KEY, &value)
            .map_err(SyncError::from)?;
        Ok(Some(info))
    }
}

/// 把快照里“有有效墓碑的记录”换成新 UID，并改掉所有指向它们的引用。返回换了几条。
pub(crate) fn renew_deleted_uids(
    snapshot: &mut SyncSnapshot,
    tombstones: &[Tombstone],
) -> std::collections::BTreeMap<String, String> {
    use std::collections::{BTreeMap, BTreeSet};

    let dead: BTreeSet<(&str, &str)> = tombstones
        .iter()
        .map(|t| (t.entity.as_str(), t.key.as_str()))
        .collect();
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    let mut plan = |entity: &str, uid: &str| {
        if dead.contains(&(entity, uid)) {
            renamed
                .entry(uid.to_owned())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
    };
    snapshot
        .groups
        .iter()
        .for_each(|r| plan(ENTITY_GROUP, &r.uid));
    snapshot
        .hosts
        .iter()
        .for_each(|r| plan(ENTITY_HOST, &r.uid));
    snapshot
        .identities
        .iter()
        .for_each(|r| plan(ENTITY_IDENTITY, &r.uid));
    snapshot
        .proxies
        .iter()
        .for_each(|r| plan(ENTITY_PROXY, &r.uid));
    if renamed.is_empty() {
        return renamed;
    }

    let swap = |uid: &mut String| {
        if let Some(new) = renamed.get(uid.as_str()) {
            *uid = new.clone();
        }
    };
    let swap_opt = |uid: &mut Option<String>| {
        if let Some(value) = uid {
            if let Some(new) = renamed.get(value.as_str()) {
                *value = new.clone();
            }
        }
    };
    for group in &mut snapshot.groups {
        swap(&mut group.uid);
        swap_opt(&mut group.parent_uid);
    }
    for host in &mut snapshot.hosts {
        swap(&mut host.uid);
        swap_opt(&mut host.group_uid);
        swap_opt(&mut host.identity_uid);
        swap_opt(&mut host.proxy_uid);
        swap_opt(&mut host.jump_host_uid);
    }
    for identity in &mut snapshot.identities {
        swap(&mut identity.uid);
    }
    for proxy in &mut snapshot.proxies {
        swap(&mut proxy.uid);
    }
    if let Some(serde_json::Value::String(uid)) = snapshot
        .settings
        .get_mut(crate::store::settings::DEFAULT_PROXY_KEY)
    {
        swap(uid);
    }
    renamed
}

#[cfg(test)]
mod tests;
