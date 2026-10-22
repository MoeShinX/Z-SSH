use super::*;

const NOW: i64 = 1_760_000_000;

fn snap(device: &str) -> SyncSnapshot {
    SyncSnapshot::empty(device, NOW)
}

fn group(uid: &str, name: &str, updated: i64, device: &str) -> GroupRecord {
    GroupRecord {
        uid: uid.to_owned(),
        name: name.to_owned(),
        parent_uid: None,
        sort: 0,
        updated_at: updated,
        device_id: device.to_owned(),
    }
}

fn host(uid: &str, name: &str, updated: i64, device: &str) -> HostRecord {
    HostRecord {
        uid: uid.to_owned(),
        group_uid: None,
        name: name.to_owned(),
        host: format!("{name}.example.internal"),
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
        updated_at: updated,
        device_id: device.to_owned(),
    }
}

fn identity(uid: &str, name: &str, updated: i64, device: &str) -> IdentityRecord {
    IdentityRecord {
        uid: uid.to_owned(),
        name: name.to_owned(),
        kind: "password".to_owned(),
        key_path: None,
        secret: Some(format!("secret-{uid}")),
        updated_at: updated,
        device_id: device.to_owned(),
    }
}

fn proxy(uid: &str, name: &str, updated: i64, device: &str) -> ProxyRecord {
    ProxyRecord {
        uid: uid.to_owned(),
        name: name.to_owned(),
        kind: "socks5".to_owned(),
        host: "proxy.example.internal".to_owned(),
        port: 1080,
        username: None,
        secret: None,
        updated_at: updated,
        device_id: device.to_owned(),
    }
}

fn base_with_host() -> SyncSnapshot {
    let mut base = snap("cloud");
    base.hosts.push(host("h1", "web-1", 10, "cloud"));
    base
}

#[test]
fn only_one_side_changed_uses_that_side() {
    let base = base_with_host();
    let mut local = base.clone();
    local.device_id = "dev-a".to_owned();
    local.hosts[0].name = "web-1-local".to_owned();
    local.hosts[0].updated_at = 20;
    local.hosts[0].device_id = "dev-a".to_owned();
    let remote = base.clone();

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(outcome.snapshot.hosts[0].name, "web-1-local");
    assert!(outcome.conflicts.is_empty(), "单边修改不算冲突");

    let local = base.clone();
    let mut remote = base.clone();
    remote.hosts[0].name = "web-1-remote".to_owned();
    remote.hosts[0].updated_at = 20;
    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(outcome.snapshot.hosts[0].name, "web-1-remote");
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn both_changed_to_the_same_content_is_not_a_conflict() {
    let base = base_with_host();
    let mut local = base.clone();
    local.hosts[0].name = "same".to_owned();
    local.hosts[0].updated_at = 20;
    local.hosts[0].device_id = "dev-a".to_owned();
    let mut remote = base.clone();
    remote.hosts[0].name = "same".to_owned();
    remote.hosts[0].updated_at = 21;
    remote.hosts[0].device_id = "dev-b".to_owned();

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(outcome.snapshot.hosts[0].name, "same");
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn both_changed_differently_keeps_newer_and_stores_the_other_copy() {
    let base = base_with_host();
    let mut local = base.clone();
    local.hosts[0].name = "local-version".to_owned();
    local.hosts[0].updated_at = 20;
    local.hosts[0].device_id = "dev-a".to_owned();
    let mut remote = base.clone();
    remote.hosts[0].name = "remote-version".to_owned();
    remote.hosts[0].updated_at = 30;
    remote.hosts[0].device_id = "dev-b".to_owned();

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(outcome.snapshot.hosts[0].name, "remote-version");
    assert_eq!(outcome.conflicts.len(), 1);
    let conflict = &outcome.conflicts[0];
    assert_eq!(conflict.entity, "host");
    assert_eq!(conflict.key, "h1");
    assert!(conflict.kept.contains("remote-version"));
    assert!(conflict.other.contains("local-version"), "另一份要保留");
}

#[test]
fn clock_skew_is_broken_by_device_id_deterministically() {
    let base = base_with_host();
    let mut local = base.clone();
    local.device_id = "dev-a".to_owned();
    local.hosts[0].name = "from-a".to_owned();
    local.hosts[0].updated_at = 20;
    local.hosts[0].device_id = "dev-a".to_owned();
    let mut remote = base.clone();
    remote.device_id = "dev-b".to_owned();
    remote.hosts[0].name = "from-b".to_owned();
    remote.hosts[0].updated_at = 21; // 只差 1 秒，算时钟不准
    remote.hosts[0].device_id = "dev-b".to_owned();

    let ab = merge(Some(&base), &local, &remote, NOW).expect("A 合并");
    let ba = merge(Some(&base), &remote, &local, NOW).expect("B 合并");
    assert_eq!(ab.snapshot.hosts[0].name, "from-a", "设备 ID 小的赢");
    assert_eq!(ba.snapshot.hosts[0].name, "from-a", "换一边合并结果一样");
    assert_eq!(ab.snapshot.hosts, ba.snapshot.hosts);
    assert_eq!(ab.conflicts.len(), 1);
}

#[test]
fn deletion_wins_over_unchanged_copy_and_writes_a_tombstone() {
    let base = base_with_host();
    let mut local = base.clone();
    local.hosts.clear(); // 本地删了
    let remote = base.clone();

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert!(outcome.snapshot.hosts.is_empty(), "删除不能复活");
    assert_eq!(outcome.tombstones.len(), 1);
    assert_eq!(outcome.tombstones[0].entity, "host");
    assert_eq!(outcome.tombstones[0].key, "h1");
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn deletion_beats_an_edit_and_keeps_the_edit_in_conflicts() {
    let base = base_with_host();
    let mut local = base.clone();
    local.hosts.clear();
    let mut remote = base.clone();
    remote.hosts[0].name = "edited-while-deleted".to_owned();
    remote.hosts[0].updated_at = 30;

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert!(outcome.snapshot.hosts.is_empty());
    assert_eq!(outcome.conflicts.len(), 1);
    assert!(outcome.conflicts[0].other.contains("edited-while-deleted"));
    assert!(outcome.conflicts[0].kept.is_empty(), "冲突里 kept 是删除");
}

#[test]
fn expired_tombstone_does_not_resurrect_a_long_offline_device() {
    // B 很久没同步，基线里还有这条；云端的墓碑已经过期被清掉了。
    let base = base_with_host();
    let local = base.clone(); // B 本地一直没动
    let mut remote = base.clone();
    remote.hosts.clear();
    remote.tombstones.push(Tombstone {
        entity: "host".to_owned(),
        key: "h1".to_owned(),
        deleted_at: NOW - 91 * 24 * 3600,
        device_id: "dev-a".to_owned(),
    });

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert!(outcome.snapshot.hosts.is_empty(), "过期墓碑也不能复活");
    assert_eq!(outcome.snapshot.tombstones.len(), 1);
    assert_eq!(outcome.snapshot.tombstones[0].key, "h1");
}

#[test]
fn deleted_identity_nulls_host_references_without_losing_the_host() {
    let mut base = snap("cloud");
    base.identities
        .push(identity("i1", "部署密码", 10, "cloud"));
    let mut h = host("h1", "web-1", 10, "cloud");
    h.identity_uid = Some("i1".to_owned());
    base.hosts.push(h);

    let local = base.clone();
    let mut remote = base.clone();
    remote.identities.clear(); // 另一台电脑删了凭据

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(
        outcome.snapshot.hosts.len(),
        1,
        "主机不能因为引用消失就没了"
    );
    assert_eq!(outcome.snapshot.hosts[0].identity_uid, None);
    assert!(
        outcome
            .nulled_references
            .iter()
            .any(|note| note.contains("凭据") && note.contains("web-1")),
        "{:?}",
        outcome.nulled_references
    );
}

#[test]
fn group_parent_cycles_are_broken_deterministically() {
    let mut local = snap("dev-a");
    local.groups.push(group("g1", "甲", 10, "dev-a"));
    local.groups.push(group("g2", "乙", 10, "dev-a"));
    local.groups[0].parent_uid = Some("g2".to_owned());
    local.groups[1].parent_uid = Some("g1".to_owned());
    let remote = SyncSnapshot::empty("dev-b", NOW);

    let outcome = merge(None, &local, &remote, NOW).expect("合并");
    for group in &outcome.snapshot.groups {
        assert_eq!(group.parent_uid, None, "成环的父链接要断开");
    }
    assert!(
        outcome
            .nulled_references
            .iter()
            .any(|note| note.contains("成环")),
        "{:?}",
        outcome.nulled_references
    );
}

#[test]
fn first_sync_unions_both_sides_and_keeps_same_name_hosts() {
    let mut local = snap("dev-a");
    local.hosts.push(host("h1", "shared-name", 10, "dev-a"));
    local.groups.push(group("g1", "生产", 10, "dev-a"));
    let mut remote = snap("dev-b");
    remote.hosts.push(host("h2", "shared-name", 20, "dev-b"));
    remote.groups.push(group("g2", "生产", 20, "dev-b"));

    let outcome = merge(None, &local, &remote, NOW).expect("首次同步");
    assert_eq!(outcome.snapshot.hosts.len(), 2, "同名主机不去重");
    assert_eq!(outcome.snapshot.groups.len(), 2);
    assert!(outcome.conflicts.is_empty(), "不同 uid 的新增不是冲突");
    assert!(outcome.tombstones.is_empty());
}

#[test]
fn settings_merge_uses_device_id_for_conflicts_and_never_drops_keys() {
    let mut base = snap("cloud");
    base.settings
        .insert("ui_theme".to_owned(), serde_json::json!("midnight"));
    base.settings
        .insert("terminal_font_size".to_owned(), serde_json::json!(14));
    let mut local = base.clone();
    local.device_id = "dev-a".to_owned();
    local
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("graphite"));
    let mut remote = base.clone();
    remote.device_id = "dev-b".to_owned();
    remote
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("obsidian"));
    remote.settings.remove("terminal_font_size"); // 缺一项不算删除

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(
        outcome.snapshot.settings["ui_theme"],
        serde_json::json!("graphite")
    );
    assert_eq!(
        outcome.snapshot.settings["terminal_font_size"],
        serde_json::json!(14),
        "设置缺失不能被当成删除"
    );
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].entity, "setting");
}

/// R-7 F07：合并层自己也只认白名单里的键，不依赖后面写库那一层再挡一次。
#[test]
fn settings_merge_drops_keys_outside_the_whitelist() {
    let base = snap("cloud");
    let mut local = snap("dev-a");
    local
        .settings
        .insert("ai_exec_mode".to_owned(), serde_json::json!("manual"));
    let mut remote = snap("dev-b");
    for (key, value) in [
        ("ai_exec_mode", serde_json::json!("auto")),
        ("ai_codex_path", serde_json::json!("/evil/codex")),
        ("sync_device_id", serde_json::json!("dev-evil")),
        ("sync_last_backup_version", serde_json::json!("9")),
        ("totally_unknown_key", serde_json::json!(1)),
        ("ui_theme", serde_json::json!("graphite")),
    ] {
        remote.settings.insert(key.to_owned(), value);
    }

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    let keys: Vec<&str> = outcome
        .snapshot
        .settings
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["totally_unknown_key", "ui_theme"],
        "只在本机的键丢掉；不认识的键原样带过（可能是更新版本的设置）"
    );
    let mut ignored: Vec<&str> = outcome
        .ignored_settings
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    ignored.sort_unstable();
    assert_eq!(
        ignored,
        vec![
            "ai_codex_path",
            "ai_exec_mode",
            "sync_device_id",
            "sync_last_backup_version"
        ],
        "被忽略的本机键都要报出来（工具会记日志）"
    );
    assert!(outcome.conflicts.is_empty(), "被忽略的键不该产生冲突记录");
}

/// 对方的值本版本不接受：它留在合并结果里（本机没改过，就不能拿本机的旧值覆盖云端），
/// 写库那一层才拒绝把它写进本机。
#[test]
fn settings_merge_keeps_a_remote_value_this_version_cannot_apply_when_local_is_unchanged() {
    let mut base = snap("cloud");
    base.settings
        .insert("terminal_font_size".to_owned(), serde_json::json!(14));
    let local = base.clone();
    let mut remote = base.clone();
    remote
        .settings
        .insert("terminal_font_size".to_owned(), serde_json::json!(32));
    remote
        .settings
        .insert("terminal_cursor_style".to_owned(), serde_json::json!("bar"));

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    let settings = &outcome.snapshot.settings;
    assert_eq!(settings["terminal_font_size"], serde_json::json!(32));
    assert_eq!(settings["terminal_cursor_style"], serde_json::json!("bar"));
    assert!(outcome.conflicts.is_empty());
}

#[test]
fn settings_merge_with_a_pending_record_does_not_count_the_old_local_value_as_a_change() {
    use crate::sync::merge::merge_with;
    use crate::sync::settings::SettingsPolicy;

    // 基线和云端是 aurora（本版本不接受），本机还是 midnight。
    let mut base = snap("cloud");
    base.settings
        .insert("ui_theme".to_owned(), serde_json::json!("aurora"));
    let remote = base.clone();
    let mut local = snap("dev-a");
    local
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("midnight"));

    // 没有记录：本机的 midnight 看起来像用户改的，会覆盖云端（这正是记录要避免的）。
    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(
        outcome.snapshot.settings["ui_theme"],
        serde_json::json!("midnight")
    );

    // 有记录（上次因为不接受而没写进本机，当时本机的值是 midnight）：云端的值留下。
    let pending =
        std::collections::BTreeMap::from([("ui_theme".to_owned(), serde_json::json!("midnight"))]);
    let outcome = merge_with(
        Some(&base),
        &local,
        &remote,
        NOW,
        &SettingsPolicy::CURRENT,
        &pending,
    )
    .expect("合并");
    assert_eq!(
        outcome.snapshot.settings["ui_theme"],
        serde_json::json!("aurora")
    );

    // 用户把本机改成了别的值：不再等于记录，按正常规则上传。
    local
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("obsidian"));
    let outcome = merge_with(
        Some(&base),
        &local,
        &remote,
        NOW,
        &SettingsPolicy::CURRENT,
        &pending,
    )
    .expect("合并");
    assert_eq!(
        outcome.snapshot.settings["ui_theme"],
        serde_json::json!("obsidian")
    );
}

#[test]
fn settings_merge_does_not_resurrect_base_value_when_the_remote_value_is_invalid() {
    // 本机没有这一项，基线里有（比如以前同步过、本机后来清掉了）。对方给的值本版本不接受：
    // 合并结果留对方的值，不能退回基线的旧值（写库那一层不会把对方的值写进本机）。
    let mut base = snap("cloud");
    base.settings
        .insert("terminal_font_size".to_owned(), serde_json::json!(14));
    let mut local = base.clone();
    local.device_id = "dev-a".to_owned();
    local.settings.remove("terminal_font_size");
    let mut remote = base.clone();
    remote.device_id = "dev-b".to_owned();
    remote
        .settings
        .insert("terminal_font_size".to_owned(), serde_json::json!(999));

    let outcome = merge(Some(&base), &local, &remote, NOW).expect("合并");
    assert_eq!(
        outcome.snapshot.settings["terminal_font_size"],
        serde_json::json!(999)
    );
}

#[test]
fn merging_is_symmetric_and_repeatable() {
    let base = base_with_host();
    let mut local = base.clone();
    local.device_id = "dev-a".to_owned();
    local.hosts[0].name = "a".to_owned();
    local.hosts[0].updated_at = 20;
    local.hosts[0].device_id = "dev-a".to_owned();
    local
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("graphite"));
    let mut remote = base.clone();
    remote.device_id = "dev-b".to_owned();
    remote.hosts[0].name = "b".to_owned();
    remote.hosts[0].updated_at = 21;
    remote.hosts[0].device_id = "dev-b".to_owned();
    remote
        .settings
        .insert("ui_theme".to_owned(), serde_json::json!("obsidian"));

    let ab = merge(Some(&base), &local, &remote, NOW).expect("A");
    let ba = merge(Some(&base), &remote, &local, NOW).expect("B");
    assert!(
        ab.snapshot.same_data(&ba.snapshot),
        "同样的三份输入结果必须一样"
    );
    assert_eq!(ab.snapshot.hosts, ba.snapshot.hosts);
}

// ---------- 随机用例：两台电脑各自增删改，A 先同步、B 再同步 ----------

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn random_mutate(snapshot: &mut SyncSnapshot, rng: &mut Lcg, device: &str, tag: &str) {
    // 分组
    if rng.bool() && !snapshot.groups.is_empty() {
        snapshot.groups.remove(rng.below(snapshot.groups.len()));
    } else {
        let uid = format!("{tag}-g{}", rng.next() % 6);
        let name = format!("group-{}", rng.next() % 6);
        snapshot
            .groups
            .push(group(&uid, &name, (rng.next() % 100) as i64 + 1, device));
    }
    // 主机
    if rng.bool() && !snapshot.hosts.is_empty() {
        let index = rng.below(snapshot.hosts.len());
        snapshot.hosts[index].name = format!("host-{}", rng.next() % 8);
        snapshot.hosts[index].updated_at = (rng.next() % 100) as i64 + 1;
        snapshot.hosts[index].device_id = device.to_owned();
    } else {
        let uid = format!("{tag}-h{}", rng.next() % 6);
        snapshot.hosts.push(host(
            &uid,
            &format!("host-{}", rng.next() % 8),
            (rng.next() % 100) as i64 + 1,
            device,
        ));
    }
    // 凭据
    if rng.bool() && !snapshot.identities.is_empty() {
        snapshot
            .identities
            .remove(rng.below(snapshot.identities.len()));
    } else {
        let uid = format!("{tag}-i{}", rng.next() % 4);
        snapshot.identities.push(identity(
            &uid,
            &format!("identity-{}", rng.next() % 4),
            (rng.next() % 100) as i64 + 1,
            device,
        ));
    }
    // 代理
    if rng.bool() {
        let uid = format!("{tag}-p{}", rng.next() % 3);
        snapshot.proxies.push(proxy(
            &uid,
            &format!("proxy-{}", rng.next() % 3),
            (rng.next() % 100) as i64 + 1,
            device,
        ));
    }
    // 设置
    snapshot.settings.insert(
        "ui_theme".to_owned(),
        serde_json::json!(["midnight", "graphite", "obsidian"][(rng.next() % 3) as usize]),
    );
    snapshot.settings.insert(
        "terminal_font_size".to_owned(),
        serde_json::json!(rng.next() % 5 + 10),
    );
}

#[test]
fn random_two_computers_converge_after_a_then_b() {
    let mut rng = Lcg(0x5eed_1234_abcd_ef01);
    for round in 0..40 {
        let mut base = snap("cloud");
        base.groups.push(group("g1", "生产", 10, "cloud"));
        base.hosts.push(host("h1", "web-1", 10, "cloud"));
        base.identities.push(identity("i1", "部署", 10, "cloud"));
        base.proxies.push(proxy("p1", "代理", 10, "cloud"));
        base.settings
            .insert("ui_theme".to_owned(), serde_json::json!("midnight"));

        let mut a = base.clone();
        a.device_id = "dev-a".to_owned();
        let mut b = base.clone();
        b.device_id = "dev-b".to_owned();
        random_mutate(&mut a, &mut rng, "dev-a", "a");
        random_mutate(&mut b, &mut rng, "dev-b", "b");

        // A 先同步：本地 A + 云端 B（基线是共同的 base）。
        let merged_a = merge(Some(&base), &a, &b, NOW).expect("A 合并").snapshot;
        // B 再同步：本地 B + 云端 A 刚上传的结果。
        let merged_b = merge(Some(&base), &b, &merged_a, NOW)
            .expect("B 合并")
            .snapshot;

        assert!(
            merged_a.same_data(&merged_b),
            "第 {round} 轮两台电脑不一致\nA: {:?}\nB: {:?}",
            merged_a,
            merged_b
        );
        // 再把 A 同步一次也不应该变化（收敛）。
        let again = merge(Some(&merged_a), &merged_a, &merged_b, NOW)
            .expect("再合并")
            .snapshot;
        assert!(
            again.same_data(&merged_a),
            "第 {round} 轮没有收敛
merged_a: {merged_a:?}
merged_b: {merged_b:?}
again: {again:?}"
        );
    }
}
