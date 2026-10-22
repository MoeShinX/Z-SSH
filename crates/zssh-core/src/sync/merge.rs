//! 三方合并：本地当前、云端当前、上次同步的基线。
//!
//! 规则（任务卡第 4 节）：
//!
//! - “改没改”用**内容比较**（和基线比），不看修改时间；
//! - 只有一边改了 → 用改了的那边；两边改成一样的 → 不算冲突；
//! - 两边都改了同一条 → 用修改时间较新的那份，另一份记进冲突；时间相同或相差
//!   不超过 [`CLOCK_SKEW_SECS`] 时用设备 ID 做确定性的平局规则（同一份输入在
//!   两台电脑上合并结果一致）；
//! - 删除用墓碑，保留 90 天；墓碑过期后靠“基线里有、本地没有、云端也没有”仍然
//!   判成删除，不能让很久没同步的记录复活；
//! - 引用被删掉的凭据 / 代理 / 分组 / 跳板机 → 引用置空并写说明，导入不失败；
//! - 首次同步（没有基线）两边都当新增，同名主机不去重（uid 不同就是两条）。

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use super::error::SyncError;
use super::package::{
    GroupRecord, HostRecord, IdentityRecord, KnownHostRecord, ProxyRecord, SyncSnapshot, Tombstone,
    ENTITY_GROUP, ENTITY_HOST, ENTITY_IDENTITY, ENTITY_KNOWN_HOST, ENTITY_PROXY,
};
use super::settings::{limit_unknown, prepare_for_merge, RejectedSetting, SettingsPolicy};

/// 两台电脑的时钟相差在这个范围内就算“很接近”，用设备 ID 平局。
pub const CLOCK_SKEW_SECS: i64 = 2;

/// 两边都改了同一条记录时留下的冲突：`kept` 是这次采用的，`other` 是另一份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Conflict {
    pub entity: String,
    pub key: String,
    /// 采用的那份（JSON 文本；删除冲突时是空串）。
    pub kept: String,
    /// 没采用的那份（JSON 文本）。
    pub other: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct MergeOutcome {
    pub snapshot: SyncSnapshot,
    pub conflicts: Vec<Conflict>,
    /// 引用被置空的说明（给界面列出来）。
    pub nulled_references: Vec<String>,
    /// 这次合并里所有仍然有效的墓碑（导入时写回本地库）。
    pub tombstones: Vec<Tombstone>,
    /// 合并时被忽略的设置键和原因（本机键、超出大小 / 个数限制的不认识的键）。
    pub ignored_settings: Vec<RejectedSetting>,
}

/// 记录级合并要的少量访问器。
trait MergeRecord: Clone + Serialize {
    fn key(&self) -> String;
    fn updated_at(&self) -> i64;
    fn device_id(&self) -> &str;
    fn same_content(&self, other: &Self) -> bool;
}

impl MergeRecord for GroupRecord {
    fn key(&self) -> String {
        self.uid.clone()
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn same_content(&self, other: &Self) -> bool {
        GroupRecord::same_content(self, other)
    }
}

impl MergeRecord for HostRecord {
    fn key(&self) -> String {
        self.uid.clone()
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn same_content(&self, other: &Self) -> bool {
        HostRecord::same_content(self, other)
    }
}

impl MergeRecord for IdentityRecord {
    fn key(&self) -> String {
        self.uid.clone()
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn same_content(&self, other: &Self) -> bool {
        IdentityRecord::same_content(self, other)
    }
}

impl MergeRecord for ProxyRecord {
    fn key(&self) -> String {
        self.uid.clone()
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn same_content(&self, other: &Self) -> bool {
        ProxyRecord::same_content(self, other)
    }
}

impl MergeRecord for KnownHostRecord {
    fn key(&self) -> String {
        KnownHostRecord::key(self)
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn same_content(&self, other: &Self) -> bool {
        KnownHostRecord::same_content(self, other)
    }
}

fn index<R: MergeRecord>(records: &[R]) -> BTreeMap<String, R> {
    records
        .iter()
        .map(|record| (record.key(), record.clone()))
        .collect()
}

fn json_of<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// 两台电脑合并同一条记录：时间新的优先，时间接近（时钟不准）时设备 ID 小的赢。
///
/// 交换 local / remote 再合并必须得到同一个赢家，所以这里只比较记录自身携带的
/// 设备 ID（快照导出的记录都会写上），不比较“谁在本地”。
fn pick_winner<'a, R: MergeRecord>(
    local: &'a R,
    remote: &'a R,
    local_snapshot_device: &str,
    remote_snapshot_device: &str,
) -> (&'a R, &'a R) {
    let local_time = local.updated_at();
    let remote_time = remote.updated_at();
    if local_time.abs_diff(remote_time) > CLOCK_SKEW_SECS as u64 {
        return if local_time > remote_time {
            (local, remote)
        } else {
            (remote, local)
        };
    }
    let local_device = if local.device_id().is_empty() {
        local_snapshot_device
    } else {
        local.device_id()
    };
    let remote_device = if remote.device_id().is_empty() {
        remote_snapshot_device
    } else {
        remote.device_id()
    };
    match local_device.cmp(remote_device) {
        std::cmp::Ordering::Less => (local, remote),
        std::cmp::Ordering::Greater => (remote, local),
        // 同一台设备（理论上同一条记录不会出现两份）：内容按规范 JSON 再比一次。
        std::cmp::Ordering::Equal => {
            if json_of(local) <= json_of(remote) {
                (local, remote)
            } else {
                (remote, local)
            }
        }
    }
}

fn conflict(
    entity: &str,
    key: &str,
    kept: String,
    other: String,
    message: impl Into<String>,
) -> Conflict {
    Conflict {
        entity: entity.to_owned(),
        key: key.to_owned(),
        kept,
        other,
        message: message.into(),
    }
}

/// 收集输入里的有效墓碑，同一个键取更晚的删除时间；时间相同取设备 ID 小的。
fn collect_tombstones(
    snapshots: &[&SyncSnapshot],
    now: i64,
) -> BTreeMap<(String, String), Tombstone> {
    let mut active: BTreeMap<(String, String), Tombstone> = BTreeMap::new();
    for snapshot in snapshots {
        for tombstone in &snapshot.tombstones {
            if tombstone.is_expired(now) {
                continue;
            }
            let key = (tombstone.entity.clone(), tombstone.key.clone());
            match active.get(&key) {
                Some(existing)
                    if existing.deleted_at > tombstone.deleted_at
                        || (existing.deleted_at == tombstone.deleted_at
                            && existing.device_id <= tombstone.device_id) => {}
                _ => {
                    active.insert(key, tombstone.clone());
                }
            }
        }
    }
    active
}

fn deletion_tombstone<R: MergeRecord>(
    entity: &str,
    key: &str,
    base: Option<&R>,
    local: &SyncSnapshot,
    remote: &SyncSnapshot,
    deleter_device: &str,
) -> Tombstone {
    // 删除时间必须由三份输入确定（同一份输入在两台电脑上算出来一样），所以用
    // 基线的修改时间和两边快照的导出时间取最大，而不是各机器自己的 now。
    let deleted_at = base
        .map(|record| record.updated_at())
        .unwrap_or_default()
        .max(local.created_at)
        .max(remote.created_at);
    Tombstone {
        entity: entity.to_owned(),
        key: key.to_owned(),
        deleted_at,
        device_id: deleter_device.to_owned(),
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_records<R: MergeRecord>(
    entity: &'static str,
    base: &BTreeMap<String, R>,
    local: &BTreeMap<String, R>,
    remote: &BTreeMap<String, R>,
    local_snapshot: &SyncSnapshot,
    remote_snapshot: &SyncSnapshot,
    active: &BTreeMap<(String, String), Tombstone>,
    tombstones_out: &mut BTreeMap<(String, String), Tombstone>,
    conflicts: &mut Vec<Conflict>,
) -> Vec<R> {
    let keys: BTreeSet<String> = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .cloned()
        .collect();
    let mut merged = Vec::new();

    for key in keys {
        let base_record = base.get(&key);
        let local_record = local.get(&key);
        let remote_record = remote.get(&key);

        // 墓碑优先：删掉的记录不能被另一个设备的旧副本带回来。
        if let Some(tombstone) = active.get(&(entity.to_owned(), key.clone())) {
            if let Some(other) = local_record.or(remote_record) {
                if base_record
                    .map(|base| !base.same_content(other))
                    .unwrap_or(true)
                {
                    conflicts.push(conflict(
                        entity,
                        &key,
                        String::new(),
                        json_of(other),
                        "这条记录一边已删除、另一边有改动；按墓碑保留删除（防止复活），另一份留在冲突里",
                    ));
                }
            }
            tombstones_out.insert(
                (tombstone.entity.clone(), tombstone.key.clone()),
                tombstone.clone(),
            );
            continue;
        }

        match (base_record, local_record, remote_record) {
            (Some(base_record), Some(local_record), Some(remote_record)) => {
                let local_changed = !local_record.same_content(base_record);
                let remote_changed = !remote_record.same_content(base_record);
                match (local_changed, remote_changed) {
                    (false, false) => merged.push(base_record.clone()),
                    (true, false) => merged.push(local_record.clone()),
                    (false, true) => merged.push(remote_record.clone()),
                    (true, true) => {
                        if local_record.same_content(remote_record) {
                            let (winner, _) = pick_winner(
                                local_record,
                                remote_record,
                                &local_snapshot.device_id,
                                &remote_snapshot.device_id,
                            );
                            merged.push(winner.clone());
                        } else {
                            let (winner, loser) = pick_winner(
                                local_record,
                                remote_record,
                                &local_snapshot.device_id,
                                &remote_snapshot.device_id,
                            );
                            conflicts.push(conflict(
                                entity,
                                &key,
                                json_of(winner),
                                json_of(loser),
                                "两边都改了同一条记录，采用修改时间较新的那份（时间接近时按设备 ID 决定）",
                            ));
                            merged.push(winner.clone());
                        }
                    }
                }
            }
            (Some(base_record), None, Some(remote_record)) => {
                // 本地删了，云端还在：只要不是两边都删，就按删除处理。
                let deleted = deletion_tombstone(
                    entity,
                    &key,
                    Some(base_record),
                    local_snapshot,
                    remote_snapshot,
                    &local_snapshot.device_id,
                );
                if !remote_record.same_content(base_record) {
                    conflicts.push(conflict(
                        entity,
                        &key,
                        String::new(),
                        json_of(remote_record),
                        "本地删除了这条记录，云端在这之后又改过；按删除处理（防止复活），改动留在冲突里",
                    ));
                }
                tombstones_out.insert((entity.to_owned(), key.clone()), deleted);
            }
            (Some(base_record), Some(local_record), None) => {
                // 云端删了，本地还在。
                let deleted = deletion_tombstone(
                    entity,
                    &key,
                    Some(base_record),
                    local_snapshot,
                    remote_snapshot,
                    &remote_snapshot.device_id,
                );
                if !local_record.same_content(base_record) {
                    conflicts.push(conflict(
                        entity,
                        &key,
                        String::new(),
                        json_of(local_record),
                        "云端删除了这条记录，本地在这之后又改过；按删除处理（防止复活），改动留在冲突里",
                    ));
                }
                tombstones_out.insert((entity.to_owned(), key.clone()), deleted);
            }
            (Some(base_record), None, None) => {
                // 两边都删了：墓碑时间取确定值，设备 ID 取较小的一个。
                let deleter = local_snapshot
                    .device_id
                    .as_str()
                    .min(remote_snapshot.device_id.as_str());
                let deleted = deletion_tombstone(
                    entity,
                    &key,
                    Some(base_record),
                    local_snapshot,
                    remote_snapshot,
                    deleter,
                );
                tombstones_out.insert((entity.to_owned(), key.clone()), deleted);
            }
            (None, Some(local_record), Some(remote_record)) => {
                // 首次同步 / 两边都新增：同名不去重（uid 不同就是两条）。
                if local_record.same_content(remote_record) {
                    let (winner, _) = pick_winner(
                        local_record,
                        remote_record,
                        &local_snapshot.device_id,
                        &remote_snapshot.device_id,
                    );
                    merged.push(winner.clone());
                } else {
                    let (winner, loser) = pick_winner(
                        local_record,
                        remote_record,
                        &local_snapshot.device_id,
                        &remote_snapshot.device_id,
                    );
                    conflicts.push(conflict(
                        entity,
                        &key,
                        json_of(winner),
                        json_of(loser),
                        "两边新增了同一条记录但内容不同，采用修改时间较新的那份",
                    ));
                    merged.push(winner.clone());
                }
            }
            (None, Some(record), None) | (None, None, Some(record)) => {
                merged.push(record.clone());
            }
            (None, None, None) => {}
        }
    }
    merged
}

/// 设置项合并：没有时间戳，冲突时直接用设备 ID 决定；缺一项不当成删除。
fn merge_settings(
    base: &BTreeMap<String, Value>,
    local: &BTreeMap<String, Value>,
    remote: &BTreeMap<String, Value>,
    local_device: &str,
    remote_device: &str,
    conflicts: &mut Vec<Conflict>,
) -> BTreeMap<String, Value> {
    let keys: BTreeSet<&String> = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .collect();
    let mut merged = BTreeMap::new();
    for key in keys {
        let base_value = base.get(key);
        let local_value = local.get(key);
        let remote_value = remote.get(key);

        let chosen = match (base_value, local_value, remote_value) {
            (_, Some(left), Some(right)) if left == right => left.clone(),
            (Some(base_value), Some(left), Some(right)) => {
                if left == base_value {
                    right.clone()
                } else if right == base_value {
                    left.clone()
                } else {
                    let winner = if local_device <= remote_device {
                        left
                    } else {
                        right
                    };
                    let loser = if local_device <= remote_device {
                        right
                    } else {
                        left
                    };
                    conflicts.push(conflict(
                        "setting",
                        key,
                        json_of(winner),
                        json_of(loser),
                        "两台电脑都改了这个设置，按设备 ID 采用其中一份（设置没有修改时间）",
                    ));
                    winner.clone()
                }
            }
            (None, Some(left), Some(right)) => {
                let winner = if local_device <= remote_device {
                    left
                } else {
                    right
                };
                let loser = if local_device <= remote_device {
                    right
                } else {
                    left
                };
                conflicts.push(conflict(
                    "setting",
                    key,
                    json_of(winner),
                    json_of(loser),
                    "两台电脑都设置了这项但值不同，按设备 ID 采用其中一份",
                ));
                winner.clone()
            }
            (Some(base_value), Some(left), None) => {
                if left == base_value {
                    base_value.clone()
                } else {
                    left.clone()
                }
            }
            (Some(base_value), None, Some(right)) => {
                if right == base_value {
                    base_value.clone()
                } else {
                    right.clone()
                }
            }
            (Some(base_value), None, None) => base_value.clone(),
            (None, Some(value), None) | (None, None, Some(value)) => value.clone(),
            (None, None, None) => continue,
        };
        merged.insert(key.clone(), chosen);
    }
    merged
}

/// 合并后把指向不存在记录的引用置空，返回给用户看的说明。
pub fn null_missing_references(snapshot: &mut SyncSnapshot) -> Vec<String> {
    let mut notes = Vec::new();
    let group_uids: BTreeSet<String> = snapshot.groups.iter().map(|g| g.uid.clone()).collect();
    let identity_uids: BTreeSet<String> = snapshot
        .identities
        .iter()
        .map(|identity| identity.uid.clone())
        .collect();
    let proxy_uids: BTreeSet<String> = snapshot
        .proxies
        .iter()
        .map(|proxy| proxy.uid.clone())
        .collect();
    let host_uids: BTreeSet<String> = snapshot.hosts.iter().map(|host| host.uid.clone()).collect();

    for group in &mut snapshot.groups {
        if let Some(parent) = group.parent_uid.clone() {
            if parent == group.uid || !group_uids.contains(&parent) {
                notes.push(format!(
                    "分组「{}」的父分组已不存在，已挂回根目录",
                    group.name
                ));
                group.parent_uid = None;
            }
        }
    }
    // 合并可能把两个分组的父链接成环；按 uid 顺序断开，保证两台电脑结果一样。
    let parents: BTreeMap<String, Option<String>> = snapshot
        .groups
        .iter()
        .map(|group| (group.uid.clone(), group.parent_uid.clone()))
        .collect();
    let mut broken = BTreeSet::new();
    for group in &snapshot.groups {
        let mut seen = BTreeSet::new();
        let mut current = Some(group.uid.clone());
        while let Some(uid) = current {
            if !seen.insert(uid.clone()) {
                // 只有“从这个分组出发绕回了自己”才断它；链尾进入别人的环时不碰链上的分组。
                if group.uid == uid {
                    broken.insert(group.uid.clone());
                }
                break;
            }
            current = parents.get(&uid).cloned().flatten();
        }
    }
    for group in &mut snapshot.groups {
        if broken.contains(&group.uid) && group.parent_uid.is_some() {
            notes.push(format!(
                "分组「{}」的父分组关系成环，已挂回根目录",
                group.name
            ));
            group.parent_uid = None;
        }
    }

    for host in &mut snapshot.hosts {
        if let Some(group) = host.group_uid.clone() {
            if !group_uids.contains(&group) {
                notes.push(format!("主机「{}」的分组已不存在，已移出分组", host.name));
                host.group_uid = None;
            }
        }
        if let Some(identity) = host.identity_uid.clone() {
            if !identity_uids.contains(&identity) {
                notes.push(format!(
                    "主机「{}」引用的凭据已不存在，引用已置空",
                    host.name
                ));
                host.identity_uid = None;
            }
        }
        if let Some(proxy) = host.proxy_uid.clone() {
            if !proxy_uids.contains(&proxy) {
                notes.push(format!(
                    "主机「{}」引用的代理已不存在，引用已置空",
                    host.name
                ));
                host.proxy_uid = None;
            }
        }
        if let Some(jump) = host.jump_host_uid.clone() {
            if jump == host.uid || !host_uids.contains(&jump) {
                notes.push(format!("主机「{}」的跳板机已不存在，引用已置空", host.name));
                host.jump_host_uid = None;
            }
        }
    }

    if let Some(value) = snapshot
        .settings
        .get_mut(crate::store::settings::DEFAULT_PROXY_KEY)
    {
        if let Some(uid) = value.as_str() {
            if !proxy_uids.contains(uid) {
                notes.push("默认代理已不存在，已改为不使用默认代理".to_owned());
                *value = Value::Null;
            }
        }
    }
    notes
}

/// 合并三份数据。`base` 为 `None` 表示首次同步（两边都当新增）。
pub fn merge(
    base: Option<&SyncSnapshot>,
    local: &SyncSnapshot,
    remote: &SyncSnapshot,
    now: i64,
) -> Result<MergeOutcome, SyncError> {
    merge_with(
        base,
        local,
        remote,
        now,
        &SettingsPolicy::CURRENT,
        &BTreeMap::new(),
    )
}

/// 同 [`merge`]，设置按给定的规则处理。`pending` 是本机记下的“上次因为值不合格没写进本机的
/// 设置”（见 `settings::compute_pending`）。
pub fn merge_with(
    base: Option<&SyncSnapshot>,
    local: &SyncSnapshot,
    remote: &SyncSnapshot,
    now: i64,
    policy: &SettingsPolicy,
    pending: &BTreeMap<String, Value>,
) -> Result<MergeOutcome, SyncError> {
    let empty = SyncSnapshot::empty("", 0);
    let base = base.unwrap_or(&empty);
    let tombstones = collect_tombstones(&[base, local, remote], now);
    let mut tombstones_out = BTreeMap::new();
    let mut conflicts = Vec::new();

    let mut snapshot = SyncSnapshot::empty(
        local.device_id.clone(),
        local.created_at.max(remote.created_at),
    );
    snapshot.groups = merge_records(
        ENTITY_GROUP,
        &index(&base.groups),
        &index(&local.groups),
        &index(&remote.groups),
        local,
        remote,
        &tombstones,
        &mut tombstones_out,
        &mut conflicts,
    );
    snapshot.hosts = merge_records(
        ENTITY_HOST,
        &index(&base.hosts),
        &index(&local.hosts),
        &index(&remote.hosts),
        local,
        remote,
        &tombstones,
        &mut tombstones_out,
        &mut conflicts,
    );
    snapshot.identities = merge_records(
        ENTITY_IDENTITY,
        &index(&base.identities),
        &index(&local.identities),
        &index(&remote.identities),
        local,
        remote,
        &tombstones,
        &mut tombstones_out,
        &mut conflicts,
    );
    snapshot.proxies = merge_records(
        ENTITY_PROXY,
        &index(&base.proxies),
        &index(&local.proxies),
        &index(&remote.proxies),
        local,
        remote,
        &tombstones,
        &mut tombstones_out,
        &mut conflicts,
    );
    snapshot.known_hosts = merge_records(
        ENTITY_KNOWN_HOST,
        &index(&base.known_hosts),
        &index(&local.known_hosts),
        &index(&remote.known_hosts),
        local,
        remote,
        &tombstones,
        &mut tombstones_out,
        &mut conflicts,
    );
    // 设置先按三类键整理（本机键去掉、不认识的键带过、本版本不接受的值不覆盖云端），
    // 规则见 `prepare_for_merge`。
    let prepared = prepare_for_merge(
        &base.settings,
        &local.settings,
        &remote.settings,
        pending,
        policy,
    );
    snapshot.settings = merge_settings(
        &prepared.base,
        &prepared.local,
        &prepared.remote,
        &local.device_id,
        &remote.device_id,
        &mut conflicts,
    );

    // 这条记录三条链里都已经没有时（上次就删掉了），也要把仍有用的墓碑继续带着，
    // 否则下一次同步会把它当成“从没出现过”，墓碑就失效了。
    for (key, tombstone) in &tombstones {
        tombstones_out
            .entry(key.clone())
            .or_insert_with(|| tombstone.clone());
    }

    // 被引用记录删除后：引用置空、主机保留，导入不能失败。
    let nulled_references = null_missing_references(&mut snapshot);
    snapshot.tombstones = tombstones_out.values().cloned().collect();
    snapshot.sort_canonical();
    conflicts.sort_by(|a, b| {
        (a.entity.as_str(), a.key.as_str()).cmp(&(b.entity.as_str(), b.key.as_str()))
    });

    let mut ignored_settings = prepared.ignored;
    ignored_settings.extend(limit_unknown(&mut snapshot.settings, policy));
    for item in &ignored_settings {
        tracing::warn!(key = %item.key, reason = %item.reason, "同步包里的设置已忽略");
    }

    Ok(MergeOutcome {
        snapshot,
        conflicts,
        nulled_references,
        tombstones: tombstones_out.into_values().collect(),
        ignored_settings,
    })
}

#[cfg(test)]
mod tests;
