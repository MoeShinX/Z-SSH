//! 自动更新的本机逻辑（不依赖 Tauri，方便测试）：版本比较、“跳过这个版本”的记忆、
//! 一天最多自动检查一次、重启前要拦下用户的几种情况。
//!
//! 下载、验签、安装由 Tauri 官方 updater 插件做；这里只放“什么时候查、要不要提示、
//! 能不能现在重启”的判断。设置只存在本机（`SETTINGS_SYNC_CLASS` 里是 LocalOnly）。

use serde::Serialize;

use crate::error::ZsshError;
use crate::store::Database;

/// 启动后是否自动检查更新（缺省 = 开）。
pub const AUTO_CHECK_KEY: &str = "update_auto_check";
/// 上次检查更新的时间（Unix 秒）。手动检查也算，自动检查据此限流。
pub const LAST_CHECK_KEY: &str = "update_last_check";
/// 用户选了“跳过这个版本”的版本号。比它更新的版本照常提示。
pub const SKIPPED_VERSION_KEY: &str = "update_skipped_version";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdatePrefs {
    pub auto_check: bool,
    pub last_check_at: Option<i64>,
    pub skipped_version: Option<String>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_check_at: None,
            skipped_version: None,
        }
    }
}

pub fn load_prefs(db: &Database) -> Result<UpdatePrefs, ZsshError> {
    // 读不了 = 不确定：当成缺省值，不崩。
    let auto_check = db
        .setting_get(AUTO_CHECK_KEY)?
        .and_then(|raw| serde_json::from_str::<bool>(&raw).ok())
        .unwrap_or(true);
    let last_check_at = db
        .setting_get(LAST_CHECK_KEY)?
        .and_then(|raw| raw.trim().parse::<i64>().ok());
    let skipped_version = db
        .setting_get(SKIPPED_VERSION_KEY)?
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty());
    Ok(UpdatePrefs {
        auto_check,
        last_check_at,
        skipped_version,
    })
}

pub fn set_auto_check(db: &Database, enabled: bool) -> Result<(), ZsshError> {
    db.setting_set(AUTO_CHECK_KEY, if enabled { "true" } else { "false" })
}

pub fn record_check(db: &Database, now: i64) -> Result<(), ZsshError> {
    db.setting_set(LAST_CHECK_KEY, &now.to_string())
}

pub fn skip_version(db: &Database, version: &str) -> Result<(), ZsshError> {
    db.setting_set(SKIPPED_VERSION_KEY, version.trim().trim_start_matches('v'))
}

/// 启动时要不要自动检查：开关打开就每次启动都查（一次请求、只在状态栏提示，不打扰）；
/// 关掉就只能在"关于"页手动检查。
pub fn should_auto_check(prefs: &UpdatePrefs) -> bool {
    prefs.auto_check
}

type ParsedVersion = (u64, u64, u64, Option<String>);

/// 拆成 (主, 次, 修订, 预发布标识)；不是 `x.y.z` 就返回 `None`。
fn parse_version(raw: &str) -> Option<ParsedVersion> {
    let raw = raw.trim().trim_start_matches('v');
    let raw = raw.split('+').next().unwrap_or(raw);
    let (core, pre) = match raw.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_owned())),
        None => (raw, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

/// 语义化版本比较：`candidate` 是否比 `current` 新。预发布版本比同号的正式版旧。
/// 任何一边解析不了都当成“不更新”（不确定时不打扰用户）。
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let (Some(cur), Some(cand)) = (parse_version(current), parse_version(candidate)) else {
        return false;
    };
    match (cur.0, cur.1, cur.2).cmp(&(cand.0, cand.1, cand.2)) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => match (&cur.3, &cand.3) {
            (Some(_), None) => true,
            (Some(a), Some(b)) => compare_prerelease(a, b) == std::cmp::Ordering::Less,
            _ => false,
        },
    }
}

fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// 发现的新版本要不要提示：比当前新，并且用户没有对它选过“跳过”。
/// 手动点“检查更新”不受“跳过”影响（用户主动问了，就如实告诉他）。
pub fn should_announce(prefs: &UpdatePrefs, current: &str, candidate: &str, manual: bool) -> bool {
    if !is_newer(current, candidate) {
        return false;
    }
    manual || prefs.skipped_version.as_deref() != Some(candidate.trim().trim_start_matches('v'))
}

/// 重启安装之前，这些事情正在进行，直接重启会把它们打断。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RestartBusy {
    /// 正在进行的 SFTP 传输个数。
    pub transfers: usize,
    /// 正在终端里执行的 AI 命令个数。
    pub ai_commands: usize,
    /// 编辑器里还没保存的修改个数。
    pub unsaved_edits: usize,
}

impl RestartBusy {
    pub fn is_idle(&self) -> bool {
        self.transfers == 0 && self.ai_commands == 0 && self.unsaved_edits == 0
    }

    /// 给用户看的说明；空闲时是 `None`。
    pub fn describe(&self) -> Option<String> {
        if self.is_idle() {
            return None;
        }
        let mut parts = Vec::new();
        if self.transfers > 0 {
            parts.push(format!(
                "{} 个文件传输还没完成（重启会取消）",
                self.transfers
            ));
        }
        if self.ai_commands > 0 {
            parts.push(format!(
                "{} 条 AI 命令正在终端里执行（重启会中断）",
                self.ai_commands
            ));
        }
        if self.unsaved_edits > 0 {
            parts.push(format!(
                "{} 个文件在编辑器里改了还没保存（重启后会丢失）",
                self.unsaved_edits
            ));
        }
        Some(parts.join("；"))
    }
}

/// 是不是免安装版：安装版的目录里有 NSIS 安装时写出的 `uninstall.exe`（tauri-bundler 的
/// 模板 `WriteUninstaller "$INSTDIR\uninstall.exe"`），免安装 zip 里没有。
///
/// 免安装版不能走自动更新：更新包是安装程序，装完是另一份安装版，解压出来的那份不会变。
pub fn is_portable_layout(exe_dir: &std::path::Path) -> bool {
    !exe_dir.join("uninstall.exe").is_file()
}

/// 由更新地址推出这个版本的发布页，免安装版提示用户手动下载时打开。
///
/// 只认 `https://github.com/<owner>/<repo>/releases/...` 形式的地址；版本号只允许
/// semver 里出现的字符，免得拼出别的地址。推不出来返回 `None`。
pub fn release_page_url(endpoint: &str, version: &str) -> Option<String> {
    let rest = endpoint.strip_prefix("https://github.com/")?;
    let cut = rest.find("/releases/")?;
    let repo = &rest[..cut];
    let mut parts = repo.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let safe = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if parts.next().is_some() || !safe(owner) || !safe(name) {
        return None;
    }
    let version = version.trim().trim_start_matches('v');
    if version.is_empty()
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return None;
    }
    Some(format!(
        "https://github.com/{owner}/{name}/releases/tag/v{version}"
    ))
}

/// 下载进度推送的节流：按时间间隔推，避免每个网络块（几 KB）都推一次刷爆界面事件。
///
/// 两种情况不受间隔限制：第一块（界面要尽快从"0"变成有数字）和下载满了总大小
/// （否则最后一段落在间隔里，界面会停在 9x% 直到状态变成"已下载好"）。
#[derive(Debug, Clone)]
pub struct ProgressThrottle {
    interval: std::time::Duration,
    last: Option<std::time::Instant>,
}

impl ProgressThrottle {
    pub fn new(interval: std::time::Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// 这一块进度要不要推给界面。返回 `true` 时已经记下这次推送的时间。
    pub fn should_emit(
        &mut self,
        now: std::time::Instant,
        downloaded: u64,
        total: Option<u64>,
    ) -> bool {
        let finished = total.is_some_and(|total| total > 0 && downloaded >= total);
        let due = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if finished || due {
            self.last = Some(now);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Database, crate::test_util::TestDir) {
        let dir = crate::test_util::TestDir::new("updater");
        let db = Database::open(&dir.join("zssh.db")).expect("open");
        (db, dir)
    }

    #[test]
    fn progress_is_throttled_by_time_but_first_and_last_always_go_out() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let mut throttle = ProgressThrottle::new(Duration::from_millis(100));
        let total = Some(1000);
        assert!(throttle.should_emit(ms(0), 10, total), "第一块要推");
        // 间隔内的一大串小块都不推（模拟每 1 毫秒一块）
        let pushed = (1..100)
            .filter(|n| throttle.should_emit(ms(*n), 10 + n, total))
            .count();
        assert_eq!(pushed, 0, "100 毫秒内不该再推");
        assert!(throttle.should_emit(ms(100), 500, total), "到了间隔要推");
        assert!(!throttle.should_emit(ms(120), 900, total));
        assert!(
            throttle.should_emit(ms(130), 1000, total),
            "满了总大小，哪怕在间隔内也要推，界面才能到 100%"
        );
        // 不知道总大小：只按时间
        let mut unknown = ProgressThrottle::new(Duration::from_millis(100));
        assert!(unknown.should_emit(ms(0), 1, None));
        assert!(!unknown.should_emit(ms(50), 2, None));
        assert!(unknown.should_emit(ms(150), 3, None));
        // 总大小为 0（服务器报错的长度）不当成"满了"，免得每块都推
        let mut zero = ProgressThrottle::new(Duration::from_millis(100));
        assert!(zero.should_emit(ms(0), 1, Some(0)));
        assert!(!zero.should_emit(ms(10), 2, Some(0)));
    }

    #[test]
    fn newer_versions_are_detected_by_semver_not_by_string() {
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.9.0", "0.10.0"), "数字比较，不是字符串比较");
        assert!(is_newer("1.2.3", "2.0.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.1.0", "v0.1.1"), "标签里的 v 要认");
        // 预发布比同号的正式版旧
        assert!(is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0-beta.1", "0.2.0-beta.2"));
        assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.10"));
        assert!(is_newer("0.1.0", "0.2.0-rc.1"));
        // 解析不了：不更新
        assert!(!is_newer("0.1.0", "latest"));
        assert!(!is_newer("garbage", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2"));
    }

    #[test]
    fn auto_check_runs_on_every_start_unless_turned_off() {
        let mut prefs = UpdatePrefs::default();
        assert!(should_auto_check(&prefs), "默认打开");
        prefs.last_check_at = Some(1_000);
        assert!(should_auto_check(&prefs), "刚查过也照样查，不再一天一次");
        prefs.auto_check = false;
        assert!(!should_auto_check(&prefs), "关了就只能手动检查");
    }

    #[test]
    fn prefs_round_trip_and_default_to_on() {
        let (db, _dir) = temp_db();
        let prefs = load_prefs(&db).unwrap();
        assert_eq!(prefs, UpdatePrefs::default());
        assert!(prefs.auto_check);

        set_auto_check(&db, false).unwrap();
        record_check(&db, 1_700_000_000).unwrap();
        skip_version(&db, "v0.3.0").unwrap();
        let prefs = load_prefs(&db).unwrap();
        assert!(!prefs.auto_check);
        assert_eq!(prefs.last_check_at, Some(1_700_000_000));
        assert_eq!(prefs.skipped_version.as_deref(), Some("0.3.0"));

        // 存坏了的值：当缺省，不崩
        db.setting_set(AUTO_CHECK_KEY, "maybe").unwrap();
        db.setting_set(LAST_CHECK_KEY, "yesterday").unwrap();
        let prefs = load_prefs(&db).unwrap();
        assert!(prefs.auto_check);
        assert_eq!(prefs.last_check_at, None);
        // 库先关、目录后删（解构出来的 `_dir` 比 `db` 先 drop，所以这里显式先放库）。
        drop(db);
    }

    #[test]
    fn skipping_a_version_hides_only_that_version_and_only_for_auto_checks() {
        let prefs = UpdatePrefs {
            skipped_version: Some("0.3.0".to_owned()),
            ..UpdatePrefs::default()
        };
        assert!(!should_announce(&prefs, "0.2.0", "0.3.0", false));
        assert!(
            !should_announce(&prefs, "0.2.0", "v0.3.0", false),
            "带 v 的写法也算同一个版本"
        );
        assert!(
            should_announce(&prefs, "0.2.0", "0.3.1", false),
            "更新的版本照常提示"
        );
        assert!(
            should_announce(&prefs, "0.2.0", "0.3.0", true),
            "手动检查不受跳过影响"
        );
        assert!(
            !should_announce(&prefs, "0.3.0", "0.3.0", true),
            "没有新版本就是没有"
        );
    }

    #[test]
    fn restart_is_blocked_by_transfers_ai_commands_and_unsaved_edits() {
        assert!(RestartBusy::default().is_idle());
        assert_eq!(RestartBusy::default().describe(), None);
        for busy in [
            RestartBusy {
                transfers: 1,
                ..Default::default()
            },
            RestartBusy {
                ai_commands: 2,
                ..Default::default()
            },
            RestartBusy {
                unsaved_edits: 3,
                ..Default::default()
            },
        ] {
            assert!(!busy.is_idle());
            assert!(busy.describe().is_some());
        }
        let all = RestartBusy {
            transfers: 1,
            ai_commands: 2,
            unsaved_edits: 3,
        }
        .describe()
        .unwrap();
        assert!(all.contains("传输") && all.contains("AI 命令") && all.contains("还没保存"));
    }

    #[test]
    fn portable_layout_is_told_apart_by_the_uninstaller() {
        let dir = crate::test_util::TestDir::new("updater-portable");
        assert!(is_portable_layout(&dir), "没有 uninstall.exe 就是免安装版");
        std::fs::write(dir.join("uninstall.exe"), b"MZ").unwrap();
        assert!(!is_portable_layout(&dir), "安装版目录里有 uninstall.exe");
    }

    #[test]
    fn release_page_comes_from_the_github_endpoint() {
        let endpoint = "https://github.com/MoeShinX/Z-SSH/releases/latest/download/latest.json";
        assert_eq!(
            release_page_url(endpoint, "0.1.2").as_deref(),
            Some("https://github.com/MoeShinX/Z-SSH/releases/tag/v0.1.2")
        );
        assert_eq!(
            release_page_url(endpoint, "v0.2.0-beta.1").as_deref(),
            Some("https://github.com/MoeShinX/Z-SSH/releases/tag/v0.2.0-beta.1")
        );
        assert_eq!(
            release_page_url("http://github.com/a/b/releases/x", "1.0.0"),
            None
        );
        assert_eq!(
            release_page_url("https://example.com/a/b/releases/x", "1.0.0"),
            None
        );
        assert_eq!(release_page_url(endpoint, "1.0.0/../../evil"), None);
        assert_eq!(release_page_url(endpoint, "1.0.0 & calc"), None);
        assert_eq!(
            release_page_url("https://github.com/a/b/c/releases/x", "1.0.0"),
            None
        );
    }
}
