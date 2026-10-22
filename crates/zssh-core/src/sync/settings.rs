//! 设置的白名单：只有明确标成 [`SyncMode::Synced`] 的键才进同步包。
//!
//! 用白名单而不是黑名单：绝大多数设置是本机相关的（路径、布局、窗口状态），
//! 新加的设置默认不同步，免得黑名单漏一项就把别人的电脑改坏。
//!
//! [`settings_are_all_classified`] 会扫描代码里的设置键：有没归类的键测试就失败，
//! 以后加设置时不会忘了想“这台电脑相关吗”。
//!
//! 对方来的包不可信：进合并和写库之前，键要在白名单里，取值要过 [`validate_value`]
//! 的类型 / 枚举 / 范围检查（R-7 F07）。

use std::collections::BTreeMap;

use serde_json::Value;

/// 一个设置键在同步里的归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// 进同步包（跨电脑有意义）。
    Synced,
    /// 只留在本机（路径、布局、窗口状态、本机缓存之类）。
    LocalOnly,
}

/// 所有设置键和它们的归类。新增设置必须在这里明确选一边。
pub const SETTINGS_SYNC_CLASS: &[(&str, SyncMode)] = &[
    // 外观 / 终端：换一台电脑也希望一样
    // 0.1.5 起：主题模式（系统 / 浅色 / 深色）+ 浅色、深色各一个皮肤
    ("ui_theme_mode", SyncMode::Synced),
    ("ui_skin_light", SyncMode::Synced),
    ("ui_skin_dark", SyncMode::Synced),
    ("terminal_theme", SyncMode::Synced),
    // 0.1.4 及以前的界面主题和"终端背景跟随界面"：新版本不再用，但和旧版本互相同步时照样收发
    ("ui_theme", SyncMode::Synced),
    ("terminal_bg_follow_ui", SyncMode::Synced),
    ("terminal_font_family", SyncMode::Synced),
    ("terminal_font_size", SyncMode::Synced),
    ("terminal_line_height", SyncMode::Synced),
    ("terminal_scrollback", SyncMode::Synced),
    ("terminal_cursor_style", SyncMode::Synced),
    ("terminal_cursor_blink", SyncMode::Synced),
    ("terminal_copy_on_select", SyncMode::Synced),
    ("terminal_ime_english", SyncMode::Synced),
    ("terminal_right_click_paste", SyncMode::Synced),
    ("terminal_confirm_multiline_paste", SyncMode::Synced),
    // 默认代理、默认智能体：跨电脑有意义
    ("default_proxy_id", SyncMode::Synced),
    ("ai_default_agent", SyncMode::Synced),
    // AI 执行模式是**权限**设置（手动审批 / 自动执行）：只能在本机改，
    // 不能让另一台电脑通过同步把这台电脑提升成"自动执行"。
    ("ai_exec_mode", SyncMode::LocalOnly),
    // SFTP 传输参数和写文件备份份数
    ("sftp_concurrency", SyncMode::Synced),
    ("sftp_preserve_mtime", SyncMode::Synced),
    ("sftp_preserve_permissions", SyncMode::Synced),
    ("ai_write_backup_keep", SyncMode::Synced),
    // 连接参数、状态栏、界面开关：本机偏好，不同步
    ("pool_idle_timeout_secs", SyncMode::LocalOnly),
    ("keepalive_interval_secs", SyncMode::LocalOnly),
    // 快捷命令（终端底部的命令栏）：换电脑也想用同一套；栏开不开、当前看哪一组是本机偏好
    ("quick_commands", SyncMode::Synced),
    ("terminal_highlight_enabled", SyncMode::Synced),
    ("terminal_highlight_rules", SyncMode::Synced),
    // 会话日志：目录是本机路径，开关跟着目录走，都不同步
    ("session_log_enabled", SyncMode::LocalOnly),
    ("session_log_dir", SyncMode::LocalOnly),
    // 关闭主窗口时缩到托盘还是退出（UI-4）：每台电脑的使用习惯不同（常驻托盘还是用完就退），
    // 同步过去会让另一台电脑的 × 突然变了行为；「托盘提示出过了」是本机状态。都不同步。
    ("close_action", SyncMode::LocalOnly),
    ("tray_hint_shown", SyncMode::LocalOnly),
    // 关闭 Z-SFTP 文件窗口时断开还是合并回主窗口（UI-5）：同样是每台电脑的使用习惯，不同步。
    ("file_window_close", SyncMode::LocalOnly),
    ("quick_commands_bar", SyncMode::LocalOnly),
    ("quick_commands_group", SyncMode::LocalOnly),
    // 软件自身联网的代理：各台电脑的网络环境不一样
    ("app_network_proxy", SyncMode::LocalOnly),
    ("stats_show", SyncMode::LocalOnly),
    ("stats_refresh_secs", SyncMode::LocalOnly),
    ("sftp_show_hidden", SyncMode::LocalOnly),
    // 本机路径：每台电脑装的位置不一样
    ("ai_opencode_path", SyncMode::LocalOnly),
    ("ai_codex_path", SyncMode::LocalOnly),
    ("ai_claude_path", SyncMode::LocalOnly),
    // 布局：屏幕大小不一样，同步过去只会把对方界面挤变形
    ("layout.ai_panel_width", SyncMode::LocalOnly),
    ("layout.aside_panel_width", SyncMode::LocalOnly),
    ("layout.sftp_panel_width", SyncMode::LocalOnly),
    ("layout.sftp_split_ratio", SyncMode::LocalOnly),
    ("layout.host_tree_width", SyncMode::LocalOnly),
    ("layout.ai_input_height", SyncMode::LocalOnly),
    ("layout.quick_bar_height", SyncMode::LocalOnly),
    ("layout.vault_expanded", SyncMode::LocalOnly),
    ("layout.vault_sidebar_collapsed", SyncMode::LocalOnly),
    // 主机页卡片按最近打开排：每台电脑用的不一样
    ("layout.recent_hosts", SyncMode::LocalOnly),
    ("layout.overview_width", SyncMode::LocalOnly),
    // 同步引擎自己的本机状态
    ("sync_device_id", SyncMode::LocalOnly),
    ("sync_backup_keep", SyncMode::LocalOnly),
    ("sync_last_backup_version", SyncMode::LocalOnly),
    // 同步页的连接信息、主密码、自动同步、上次结果：每台电脑自己连自己的
    ("sync_storage", SyncMode::LocalOnly),
    ("sync_concurrency", SyncMode::LocalOnly),
    ("sync_master_password", SyncMode::LocalOnly),
    ("sync_auto", SyncMode::LocalOnly),
    ("sync_last_result", SyncMode::LocalOnly),
    ("sync_ready", SyncMode::LocalOnly),
    // OneDrive 的访问令牌 + 刷新令牌（DPAPI 加密）：绝不能进同步包 / 离线备份
    ("sync_onedrive_tokens", SyncMode::LocalOnly),
    // 因为值本版本不接受、没写进本机的同步键（R-7b）：只记本机的状态，不能同步
    ("sync_pending_settings", SyncMode::LocalOnly),
    // 自动更新：开关、上次检查时间、跳过的版本，都是这台电脑自己的事
    ("update_auto_check", SyncMode::LocalOnly),
    ("update_last_check", SyncMode::LocalOnly),
    ("update_skipped_version", SyncMode::LocalOnly),
];

/// 0.1.4 及以前的界面主题 id（旧版本同步过来的值照样收）。
pub const UI_THEME_IDS: &[&str] = &[
    "snow",
    "mist",
    "pure-black",
    "midnight",
    "deep-blue",
    "graphite",
    "obsidian",
];

pub const THEME_MODES: &[&str] = &["system", "light", "dark"];

/// 浅色、深色皮肤 id（`ui/src/state/skins.ts`）。有测试核对两边一致。
pub const LIGHT_SKIN_IDS: &[&str] = &[
    "paper",
    "solarized-light",
    "ayu-light",
    "github-light",
    "catppuccin-latte",
    "rose-pine-dawn",
    "everforest-light",
];
pub const DARK_SKIN_IDS: &[&str] = &[
    "graphite",
    "tokyo-night",
    "dracula",
    "catppuccin-mocha",
    "nord",
    "gruvbox-dark",
    "rose-pine",
    "kanagawa",
    "everforest-dark",
];

/// 终端配色：`skin`（跟随皮肤）、任一皮肤的配色，以及 0.1.4 及以前的配色 id
/// （旧版本同步过来时照样收，界面换成最接近的）。
pub const TERMINAL_THEME_IDS: &[&str] = &[
    "skin",
    "paper",
    "solarized-light",
    "ayu-light",
    "github-light",
    "catppuccin-latte",
    "rose-pine-dawn",
    "everforest-light",
    "graphite",
    "tokyo-night",
    "dracula",
    "catppuccin-mocha",
    "nord",
    "gruvbox-dark",
    "rose-pine",
    "kanagawa",
    "everforest-dark",
    "system-catppuccin-dark",
    "system-gruvbox-material-dark",
    "system-nord-midnight-dark",
    "system-tokyo-night-dark",
    "system-monokai-pro-spectrum-dark",
    "system-github-light",
    "system-catppuccin-light",
    "system-tokyo-night-light",
];

const CURSOR_STYLES: &[&str] = &["block", "underline", "bar"];
const AGENT_IDS: &[&str] = &["opencode", "codex", "claude"];

/// 一个同步键允许的取值。范围取设置页能调出来的范围，对方的包不能超出。
enum Rule {
    Bool,
    OneOf(&'static [&'static str]),
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    /// 非空字符串，不超过 200 个字符，不含控制字符。
    FontFamily,
    /// `null`（不使用默认代理）或代理的 uid（非空字符串，不超过 128 个字符，不含控制字符）。
    ProxyUid,
    /// 快捷命令列表：JSON 数组序列化成的字符串，见 [`check_quick_commands`]。
    QuickCommands,
    /// 终端高亮规则：JSON 数组序列化成的字符串，见 [`check_highlight_rules`]。
    HighlightRules,
}

fn rule_for(key: &str) -> Option<Rule> {
    Some(match key {
        "ui_theme" => Rule::OneOf(UI_THEME_IDS),
        "ui_theme_mode" => Rule::OneOf(THEME_MODES),
        "ui_skin_light" => Rule::OneOf(LIGHT_SKIN_IDS),
        "ui_skin_dark" => Rule::OneOf(DARK_SKIN_IDS),
        "terminal_theme" => Rule::OneOf(TERMINAL_THEME_IDS),
        "terminal_bg_follow_ui"
        | "terminal_cursor_blink"
        | "terminal_copy_on_select"
        | "terminal_ime_english"
        | "terminal_right_click_paste"
        | "terminal_confirm_multiline_paste"
        | "terminal_highlight_enabled"
        | "sftp_preserve_mtime"
        | "sftp_preserve_permissions" => Rule::Bool,
        "terminal_font_family" => Rule::FontFamily,
        "terminal_font_size" => Rule::Int { min: 10, max: 24 },
        "terminal_line_height" => Rule::Float { min: 1.0, max: 1.6 },
        "terminal_scrollback" => Rule::Int {
            min: 1000,
            max: 100_000,
        },
        "terminal_cursor_style" => Rule::OneOf(CURSOR_STYLES),
        "default_proxy_id" => Rule::ProxyUid,
        "ai_default_agent" => Rule::OneOf(AGENT_IDS),
        "sftp_concurrency" => Rule::Int { min: 1, max: 8 },
        "ai_write_backup_keep" => Rule::Int { min: 1, max: 20 },
        "quick_commands" => Rule::QuickCommands,
        "terminal_highlight_rules" => Rule::HighlightRules,
        _ => return None,
    })
}

/// 快捷命令最多这么多条、整个列表序列化后最多这么多字节。
pub const QUICK_COMMANDS_MAX: usize = 500;
pub const QUICK_COMMANDS_MAX_BYTES: usize = 256 * 1024;

/// 快捷命令列表（设置里存的是 JSON 数组序列化成的字符串）：每条要有 id、name、command，
/// group 可选，enter 是布尔。命令正文允许换行和 Tab（多行命令），别的控制字符不收——
/// 它会原样发进别人的终端，ESC 之类的控制序列能改终端状态。
fn check_quick_commands(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.len() > QUICK_COMMANDS_MAX_BYTES {
        return false;
    }
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let field = |item: &Value, key: &str, max: usize, multiline: bool| -> bool {
        match item.get(key) {
            Some(Value::String(text)) => {
                text.chars().count() <= max
                    && !text
                        .chars()
                        .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
            }
            _ => false,
        }
    };
    items.len() <= QUICK_COMMANDS_MAX
        && items.iter().all(|item| {
            item.is_object()
                && field(item, "id", 64, false)
                && field(item, "name", 100, false)
                && field(item, "command", 8000, true)
                && (item.get("group").is_none() || field(item, "group", 50, false))
                && item.get("enter").is_none_or(Value::is_boolean)
        })
}

/// 高亮规则最多这么多条（和界面 `MAX_RULES` 一致）。
pub const HIGHLIGHT_RULES_MAX: usize = 50;

/// 高亮规则：每条有 id、pattern（不超过 500 字、不含控制字符），color 是 `#RRGGBB`，
/// regex / caseSensitive / background 是布尔（可省）。正则写得对不对由界面判断：
/// 编译不了的规则界面会跳过，不会因此出事。
fn check_highlight_rules(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.len() > 64 * 1024 {
        return false;
    }
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let plain = |item: &Value, key: &str, max: usize| {
        item.get(key)
            .and_then(Value::as_str)
            .is_some_and(|text| is_plain_text(text, max))
    };
    let color = |item: &Value| {
        item.get("color")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.len() == 7
                    && text.starts_with('#')
                    && text[1..].chars().all(|c| c.is_ascii_hexdigit())
            })
    };
    let flag = |item: &Value, key: &str| item.get(key).is_none_or(Value::is_boolean);
    items.len() <= HIGHLIGHT_RULES_MAX
        && items.iter().all(|item| {
            item.is_object()
                && plain(item, "id", 64)
                && plain(item, "pattern", 500)
                && color(item)
                && flag(item, "regex")
                && flag(item, "caseSensitive")
                && flag(item, "background")
                && flag(item, "enabled")
        })
}

fn is_plain_text(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(char::is_control)
}

/// 检查一个**允许同步**的键的取值。返回 `Err(原因)` 时这个键要被忽略、保留本机原值。
///
/// 不在白名单的键、白名单里但没有取值规则的键也返回 `Err`（后者说明有人加了同步键
/// 却忘了写规则，宁可不收；测试 `every_synced_key_has_a_value_rule` 会拦住）。
/// 原因里不带取值本身，免得把对方塞的内容写进日志。
pub fn validate_value(key: &str, value: &serde_json::Value) -> Result<(), String> {
    if !is_synced(key) {
        return Err("不在同步白名单里".to_owned());
    }
    let Some(rule) = rule_for(key) else {
        return Err("没有取值规则".to_owned());
    };
    let ok = match rule {
        Rule::Bool => value.is_boolean(),
        Rule::OneOf(allowed) => value.as_str().is_some_and(|text| allowed.contains(&text)),
        // 必须是 JSON 整数：`14.0` 写进库以后后端按整数解析会失败。
        Rule::Int { min, max } => {
            (value.is_i64() || value.is_u64())
                && value.as_i64().is_some_and(|n| (min..=max).contains(&n))
        }
        Rule::Float { min, max } => value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= min && n <= max),
        Rule::FontFamily => value.as_str().is_some_and(|text| is_plain_text(text, 200)),
        Rule::ProxyUid => {
            value.is_null() || value.as_str().is_some_and(|text| is_plain_text(text, 128))
        }
        Rule::QuickCommands => check_quick_commands(value),
        Rule::HighlightRules => check_highlight_rules(value),
    };
    if ok {
        return Ok(());
    }
    Err(match rule {
        Rule::Bool => "要是 true / false".to_owned(),
        Rule::OneOf(allowed) => format!("要是 {} 之一", allowed.join(" / ")),
        Rule::Int { min, max } => format!("要是 {min}～{max} 的整数"),
        Rule::Float { min, max } => format!("要是 {min}～{max} 的数字"),
        Rule::FontFamily => "要是 1～200 个字符、不含控制字符的字符串".to_owned(),
        Rule::ProxyUid => "要是 null 或代理 uid 字符串".to_owned(),
        Rule::QuickCommands => format!(
            "要是不超过 {QUICK_COMMANDS_MAX} 条的快捷命令列表（字段齐全、命令里没有换行和 Tab 以外的控制字符）"
        ),
        Rule::HighlightRules => {
            format!("要是不超过 {HIGHLIGHT_RULES_MAX} 条的高亮规则列表（颜色是 #RRGGBB）")
        }
    })
}

/// 被忽略的设置键和原因（不含取值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedSetting {
    pub key: String,
    pub reason: String,
}

/// 不认识的键，单个值序列化后最多这么多字节。
pub const UNKNOWN_VALUE_MAX_BYTES: usize = 4096;
/// 不认识的键，同步包 / 基线里最多留这么多个。
pub const UNKNOWN_KEYS_MAX: usize = 200;
/// 本机记下“哪些同步键的值因为本版本不接受而没写进本机，当时本机的值是什么”。
/// 只在本机（见 [`SETTINGS_SYNC_CLASS`]），格式是 JSON 对象：键 → 当时本机的值。
pub const SYNC_PENDING_SETTINGS_KEY: &str = "sync_pending_settings";

/// 按规则整理一份从别处来的设置（对方的包、基线）：
///
/// - 只在本机的键：去掉；
/// - 不认识的键：留着，但单个值超过 [`UNKNOWN_VALUE_MAX_BYTES`] 的去掉，总数超过
///   [`UNKNOWN_KEYS_MAX`] 的（按键名排序，后面的）去掉；
/// - 同步键：原样留着，取值检查留到决定“能不能写进本机”的时候做。
///
/// 返回整理后的设置和被去掉的键（调用方负责记日志；原因里不带取值）。
pub fn sanitize_settings(
    settings: &BTreeMap<String, Value>,
    policy: &SettingsPolicy,
) -> (BTreeMap<String, Value>, Vec<RejectedSetting>) {
    let mut kept = BTreeMap::new();
    let mut rejected = Vec::new();
    for (key, value) in settings {
        match policy.class(key) {
            KeyClass::LocalOnly => rejected.push(RejectedSetting {
                key: key.clone(),
                reason: "只在本机的设置，不参与同步".to_owned(),
            }),
            KeyClass::Synced => {
                kept.insert(key.clone(), value.clone());
            }
            KeyClass::Unknown => {
                kept.insert(key.clone(), value.clone());
            }
        }
    }
    rejected.extend(limit_unknown(&mut kept, policy));
    (kept, rejected)
}

/// 限制不认识的键的大小和个数，免得一个坏包无限撑大同步包。返回被去掉的键。
pub fn limit_unknown(
    settings: &mut BTreeMap<String, Value>,
    policy: &SettingsPolicy,
) -> Vec<RejectedSetting> {
    let mut rejected = Vec::new();
    let mut kept = 0usize;
    let unknown: Vec<String> = settings
        .keys()
        .filter(|key| policy.class(key) == KeyClass::Unknown)
        .cloned()
        .collect();
    for key in unknown {
        let size = settings
            .get(&key)
            .map(|value| value.to_string().len())
            .unwrap_or_default();
        let reason = if size > UNKNOWN_VALUE_MAX_BYTES {
            Some(format!(
                "不认识的设置，值超过 {UNKNOWN_VALUE_MAX_BYTES} 字节"
            ))
        } else if kept >= UNKNOWN_KEYS_MAX {
            Some(format!("不认识的设置超过 {UNKNOWN_KEYS_MAX} 个"))
        } else {
            None
        };
        match reason {
            Some(reason) => {
                settings.remove(&key);
                rejected.push(RejectedSetting { key, reason });
            }
            None => kept += 1,
        }
    }
    rejected
}

/// 合并前整理好的三份设置。
pub struct PreparedSettings {
    pub base: BTreeMap<String, Value>,
    pub local: BTreeMap<String, Value>,
    pub remote: BTreeMap<String, Value>,
    pub ignored: Vec<RejectedSetting>,
}

/// 合并设置之前的整理（R-7 F07 / R-7b）。
///
/// 三类键的处理：
/// 1. 本版本标成只在本机的键：三份里都去掉，不参与合并、不进上传的包；
/// 2. 本版本不认识的键：原样带过合并（保留在上传的包和基线里），受大小 / 个数限制；
///    写库那一层永远不把它写进本机；
/// 3. 同步键：照常合并，对方的值本版本不接受时**也留在合并结果里**（写库那一层才拒绝写进本机）。
///    但本机对这个键“没有意见”的时候，要把本机那一份拿掉，让对方的值留下来，而不是用本机的
///    旧值覆盖云端。“没有意见”指对方这个键有值，并且满足下面任意一条：
///    - 本机的值就是上次“因为值不合格没写进本机”时记下的那个值（`pending`）：说明这个值
///      一直是本机原来的，用户没动过；
///    - 对方的值本版本不接受，而且没有基线：没法证明本机的值是用户改过的，不能拿它去覆盖
///      一个本版本看不懂的值。
///
///    其余情况（用户真的改过本机的值）按正常的三方合并规则走。
pub fn prepare_for_merge(
    base: &BTreeMap<String, Value>,
    local: &BTreeMap<String, Value>,
    remote: &BTreeMap<String, Value>,
    pending: &BTreeMap<String, Value>,
    policy: &SettingsPolicy,
) -> PreparedSettings {
    let (base, _) = sanitize_settings(base, policy);
    let (mut local, _) = sanitize_settings(local, policy);
    let (remote, ignored) = sanitize_settings(remote, policy);

    let keys: Vec<String> = local.keys().cloned().collect();
    for key in keys {
        if policy.class(&key) != KeyClass::Synced {
            continue;
        }
        let Some(remote_value) = remote.get(&key) else {
            continue;
        };
        let declined_before = pending
            .get(&key)
            .is_some_and(|recorded| Some(recorded) == local.get(&key));
        let unprovable = policy.validate(&key, remote_value).is_err() && !base.contains_key(&key);
        if declined_before || unprovable {
            local.remove(&key);
        }
    }
    PreparedSettings {
        base,
        local,
        remote,
        ignored,
    }
}

/// 合并并且（需要的话）写完本机之后，算出要记下的“没写进本机的同步键”：目标里这个键的值
/// 本版本不接受，并且本机有这个键、值和目标不一样。键 → 本机当时的值。
///
/// 下次合并时本机的值还是它，就说明用户没动过（见 [`prepare_for_merge`]）。每次都整体重算，
/// 所以值变得合格了、或者用户改了本机的值，记录自然就没了。
pub fn compute_pending(
    target: &BTreeMap<String, Value>,
    local: &BTreeMap<String, Value>,
    policy: &SettingsPolicy,
) -> BTreeMap<String, Value> {
    let mut pending = BTreeMap::new();
    for (key, target_value) in target {
        if policy.class(key) != KeyClass::Synced || policy.validate(key, target_value).is_ok() {
            continue;
        }
        if let Some(local_value) = local.get(key) {
            if local_value != target_value {
                pending.insert(key.clone(), local_value.clone());
            }
        }
    }
    pending
}

/// 合并结果和本机比，**写库时**有没有要改的内容。
///
/// 合并结果里有一些键本机永远不会写（不认识的键、值本版本不接受的同步键），它们和本机
/// 的差别不算“要改本机”，否则每次同步都会白白做一次备份和写库。
pub fn same_data_for_local(
    target: &super::package::SyncSnapshot,
    local: &super::package::SyncSnapshot,
    policy: &SettingsPolicy,
) -> bool {
    let unwritable: std::collections::BTreeSet<&String> = target
        .settings
        .iter()
        .filter(|(key, value)| policy.validate(key, value).is_err())
        .map(|(key, _)| key)
        .collect();
    let mut target = target.clone();
    let mut local = local.clone();
    target.settings.retain(|key, _| !unwritable.contains(key));
    local.settings.retain(|key, _| !unwritable.contains(key));
    target.same_data(&local)
}

/// 一个设置键在**某个版本**眼里的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    /// 白名单里标成同步：可以写进本机，值要过取值检查。
    Synced,
    /// 白名单里标成只在本机：合并时丢掉，不写本机，不进上传的包。
    LocalOnly,
    /// 这个版本根本不认识（可能是更新版本新加的键）：原样带过合并，永远不写进本机。
    Unknown,
}

/// 这个版本怎么看待同步里的设置：键的类别和取值检查。
///
/// 单独做成一个值，是为了在测试里同时模拟“新版本”和“旧版本”两台电脑；
/// 正式代码一律用 [`SettingsPolicy::CURRENT`]。
#[derive(Clone, Copy)]
pub struct SettingsPolicy {
    pub class_of: fn(&str) -> KeyClass,
    /// 只对 [`KeyClass::Synced`] 的键调用。
    pub check_value: fn(&str, &Value) -> Result<(), String>,
}

fn current_class(key: &str) -> KeyClass {
    match sync_mode(key) {
        Some(SyncMode::Synced) => KeyClass::Synced,
        Some(SyncMode::LocalOnly) => KeyClass::LocalOnly,
        None => KeyClass::Unknown,
    }
}

impl SettingsPolicy {
    /// 本版本的规则（上面的白名单表和取值规则）。
    pub const CURRENT: SettingsPolicy = SettingsPolicy {
        class_of: current_class,
        check_value: validate_value,
    };

    pub fn class(&self, key: &str) -> KeyClass {
        (self.class_of)(key)
    }

    pub fn is_synced(&self, key: &str) -> bool {
        self.class(key) == KeyClass::Synced
    }

    /// 能不能把这个值写进本机：键是同步键，并且值过了取值检查。
    pub fn validate(&self, key: &str, value: &Value) -> Result<(), String> {
        if !self.is_synced(key) {
            return Err("不在同步白名单里".to_owned());
        }
        (self.check_value)(key, value)
    }
}

pub fn sync_mode(key: &str) -> Option<SyncMode> {
    SETTINGS_SYNC_CLASS
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, mode)| *mode)
}

/// 这个键进不进同步包。**没归类的一律不同步**（新设置默认不同步）。
pub fn is_synced(key: &str) -> bool {
    sync_mode(key) == Some(SyncMode::Synced)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn read(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("读 {} 失败：{err}", path.display()))
    }

    /// `ui/src/stores/settings.ts` 里 `SETTING_DEFAULTS` 的键。
    fn ui_setting_keys() -> BTreeSet<String> {
        let source = read("ui/src/stores/settings.ts");
        let mut keys = BTreeSet::new();
        let mut inside = false;
        for line in source.lines() {
            if line.contains("export const SETTING_DEFAULTS") {
                inside = true;
                continue;
            }
            if inside {
                if line.trim_start().starts_with("} as const;") {
                    break;
                }
                let trimmed = line.trim();
                let name = if let Some(rest) = trimmed.strip_prefix('"') {
                    rest.split_once('"').map(|(name, _)| name)
                } else {
                    trimmed.split_once(':').map(|(name, _)| name)
                };
                if let Some(name) = name {
                    let name = name.trim();
                    if !name.is_empty()
                        && name.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'
                        })
                    {
                        keys.insert(name.to_owned());
                    }
                }
            }
        }
        keys
    }

    /// Rust 生产代码里的设置键：`setting_get("…")`、`setting_set("…")` 和
    /// `…_KEY: &str = "…"` 常量（含命令层的 `KEY` 常量）。
    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    fn rust_setting_keys() -> BTreeSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let mut files = Vec::new();
        rust_files(
            &root.join("crates").join("zssh-core").join("src"),
            &mut files,
        );
        rust_files(&root.join("src-tauri").join("src"), &mut files);
        let plausible = |key: &str| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        };
        let mut keys = BTreeSet::new();
        for path in files {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("读 {} 失败：{err}", path.display()));
            for line in source.lines() {
                for marker in ["setting_get(\"", "setting_set(\""] {
                    if let Some(start) = line.find(marker) {
                        let rest = &line[start + marker.len()..];
                        if let Some(end) = rest.find('"') {
                            let key = &rest[..end];
                            if plausible(key) {
                                keys.insert(key.to_owned());
                            }
                        }
                    }
                }
                if line.contains("KEY: &str = \"") || line.contains("SETTING: &str = \"") {
                    if let Some(start) = line.find("= \"") {
                        let rest = &line[start + 3..];
                        if let Some(end) = rest.find('"') {
                            let key = &rest[..end];
                            if plausible(key) {
                                keys.insert(key.to_owned());
                            }
                        }
                    }
                }
            }
        }
        keys
    }

    #[test]
    fn settings_are_all_classified() {
        let mut code_keys = ui_setting_keys();
        code_keys.extend(rust_setting_keys());
        assert!(
            !code_keys.is_empty(),
            "扫描不到设置键，说明扫描逻辑坏了，不是代码真的没有设置"
        );
        for expected in [
            "ui_theme",
            "layout.ai_panel_width",
            "sftp_concurrency",
            "stats_refresh_secs",
            "ai_write_backup_keep",
            "sync_last_backup_version",
        ] {
            assert!(
                code_keys.contains(expected),
                "扫描漏了 {expected}：{code_keys:?}"
            );
        }

        let classified: BTreeSet<&str> = SETTINGS_SYNC_CLASS.iter().map(|(key, _)| *key).collect();
        let missing: Vec<&String> = code_keys
            .iter()
            .filter(|key| !classified.contains(key.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "这些设置键没有明确归类（同步 / 不同步）：{missing:?}"
        );

        let duplicates: Vec<&str> = {
            let mut seen = BTreeSet::new();
            SETTINGS_SYNC_CLASS
                .iter()
                .filter(|(key, _)| !seen.insert(*key))
                .map(|(key, _)| *key)
                .collect()
        };
        assert!(duplicates.is_empty(), "设置键重复归类：{duplicates:?}");
    }

    fn accepts(key: &str, value: Value) {
        assert_eq!(
            validate_value(key, &value),
            Ok(()),
            "{key} = {value} 应该合格"
        );
    }

    fn rejects(key: &str, value: Value) {
        assert!(
            validate_value(key, &value).is_err(),
            "{key} = {value} 应该被拒绝"
        );
    }

    #[test]
    fn every_synced_key_has_a_value_rule() {
        for (key, mode) in SETTINGS_SYNC_CLASS {
            if *mode == SyncMode::Synced {
                assert!(rule_for(key).is_some(), "同步键 {key} 没有取值规则");
            } else {
                assert!(rule_for(key).is_none(), "不同步的键 {key} 不该有取值规则");
            }
        }
    }

    #[test]
    fn enum_rules_only_take_listed_strings() {
        accepts("ui_theme", json!("graphite"));
        rejects("ui_theme", json!("nord"));
        rejects("ui_theme", json!(1));
        accepts("terminal_theme", json!("system-github-light"));
        rejects("terminal_theme", json!("system-nord"));
        accepts("terminal_cursor_style", json!("underline"));
        rejects("terminal_cursor_style", json!("Block"));
        accepts("ai_default_agent", json!("claude"));
        rejects("ai_default_agent", json!("gpt"));
        rejects("ai_default_agent", json!(null));
    }

    #[test]
    fn boolean_rules_reject_strings_and_numbers() {
        for key in [
            "terminal_bg_follow_ui",
            "terminal_cursor_blink",
            "terminal_copy_on_select",
            "terminal_ime_english",
            "terminal_right_click_paste",
            "terminal_confirm_multiline_paste",
            "terminal_highlight_enabled",
            "sftp_preserve_mtime",
            "sftp_preserve_permissions",
        ] {
            accepts(key, json!(true));
            accepts(key, json!(false));
            rejects(key, json!("true"));
            rejects(key, json!(1));
            rejects(key, json!(null));
        }
    }

    #[test]
    fn number_rules_check_type_and_range_including_the_edges() {
        accepts("terminal_font_size", json!(10));
        accepts("terminal_font_size", json!(24));
        rejects("terminal_font_size", json!(9));
        rejects("terminal_font_size", json!(25));
        rejects("terminal_font_size", json!("14"));
        rejects("terminal_font_size", json!(14.5));
        rejects("terminal_font_size", json!(14.0));
        accepts("terminal_scrollback", json!(1000));
        accepts("terminal_scrollback", json!(100_000));
        rejects("terminal_scrollback", json!(999));
        rejects("terminal_scrollback", json!(100_001));
        rejects("terminal_scrollback", json!(u64::MAX));
        accepts("sftp_concurrency", json!(1));
        accepts("sftp_concurrency", json!(8));
        rejects("sftp_concurrency", json!(0));
        rejects("sftp_concurrency", json!(9));
        rejects("sftp_concurrency", json!(-1));
        accepts("ai_write_backup_keep", json!(1));
        accepts("ai_write_backup_keep", json!(20));
        rejects("ai_write_backup_keep", json!(0));
        rejects("ai_write_backup_keep", json!(21));
        accepts("terminal_line_height", json!(1));
        accepts("terminal_line_height", json!(1.25));
        accepts("terminal_line_height", json!(1.6));
        rejects("terminal_line_height", json!(0.99));
        rejects("terminal_line_height", json!(1.61));
        rejects("terminal_line_height", json!("1.2"));
    }

    #[test]
    fn string_rules_reject_empty_long_and_control_characters() {
        accepts("terminal_font_family", json!("JetBrains Mono, monospace"));
        rejects("terminal_font_family", json!(""));
        rejects("terminal_font_family", json!("   "));
        rejects("terminal_font_family", json!("a".repeat(201)));
        rejects("terminal_font_family", json!("Mono\u{1b}[31m"));
        rejects("terminal_font_family", json!(12));
        accepts("default_proxy_id", json!(null));
        accepts("default_proxy_id", json!("p1"));
        rejects("default_proxy_id", json!(""));
        rejects("default_proxy_id", json!(5));
        rejects("default_proxy_id", json!("x".repeat(129)));
    }

    #[test]
    fn local_only_and_unknown_keys_never_validate() {
        for key in [
            "ai_exec_mode",
            "ai_codex_path",
            "ai_opencode_path",
            "ai_claude_path",
            "sync_device_id",
            "sync_last_backup_version",
            "sync_master_password",
            "layout.ai_panel_width",
            "totally_unknown_key",
            "sftp_external_editor",
        ] {
            let reason = validate_value(key, &json!("auto")).expect_err(key);
            assert!(reason.contains("白名单"), "{key}：{reason}");
        }
    }

    fn map(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    const CUR: &SettingsPolicy = &SettingsPolicy::CURRENT;

    #[test]
    fn sanitize_drops_local_only_keeps_unknown_and_synced_and_never_logs_values() {
        let settings = map(&[
            ("ui_theme", json!("graphite")),
            ("terminal_font_size", json!("SECRET-LOOKING")),
            ("ai_exec_mode", json!("auto")),
            ("future_key", json!({"a": 1})),
        ]);
        let (kept, rejected) = sanitize_settings(&settings, CUR);
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["future_key", "terminal_font_size", "ui_theme"],
            "同步键（包括值不合格的）和不认识的键都留着，只去掉本机键"
        );
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].key, "ai_exec_mode");
        assert!(rejected.iter().all(|item| !item.reason.contains("auto")));
    }

    #[test]
    fn unknown_keys_are_limited_by_size_and_count() {
        let mut settings = BTreeMap::new();
        for index in 0..205 {
            settings.insert(format!("future_{index:03}"), json!(index));
        }
        settings.insert(
            "future_big".to_owned(),
            json!("x".repeat(UNKNOWN_VALUE_MAX_BYTES)),
        );
        settings.insert("ui_theme".to_owned(), json!("graphite"));
        let (kept, rejected) = sanitize_settings(&settings, CUR);
        let unknown = kept.keys().filter(|key| key.starts_with("future_")).count();
        assert_eq!(unknown, UNKNOWN_KEYS_MAX);
        assert!(kept.contains_key("ui_theme"), "同步键不受个数限制");
        assert!(!kept.contains_key("future_big"), "值超过限制的丢掉");
        assert!(kept.contains_key("future_199") && !kept.contains_key("future_200"));
        let dropped: Vec<&str> = rejected.iter().map(|item| item.key.as_str()).collect();
        assert!(dropped.contains(&"future_big"));
        assert!(dropped.contains(&"future_204"));
        assert_eq!(dropped.len(), 1 + 5);

        // 刚好等于上限的值不算超。
        let mut edge = BTreeMap::new();
        edge.insert(
            "future_edge".to_owned(),
            json!("x".repeat(UNKNOWN_VALUE_MAX_BYTES - 2)),
        );
        assert!(sanitize_settings(&edge, CUR).0.contains_key("future_edge"));
    }

    #[test]
    fn prepare_keeps_remote_value_when_local_has_no_opinion() {
        let remote = map(&[("ui_theme", json!("aurora"))]);
        let base = map(&[("ui_theme", json!("midnight"))]);
        let local = map(&[("ui_theme", json!("midnight"))]);

        // 没有记录、有基线：本机的值就是基线的值，正常合并，对方的值本来就会赢。
        let prepared = prepare_for_merge(&base, &local, &remote, &BTreeMap::new(), CUR);
        assert_eq!(prepared.local, local);

        // 记下的值和本机现在的值一样：本机没有意见。
        let pending = map(&[("ui_theme", json!("midnight"))]);
        let base_aurora = map(&[("ui_theme", json!("aurora"))]);
        let prepared = prepare_for_merge(&base_aurora, &local, &remote, &pending, CUR);
        assert!(prepared.local.is_empty());

        // 用户改过本机的值（和记下的不一样）：有意见，按正常规则走。
        let changed = map(&[("ui_theme", json!("obsidian"))]);
        let prepared = prepare_for_merge(&base_aurora, &changed, &remote, &pending, CUR);
        assert_eq!(prepared.local, changed);

        // 没有基线，对方的值本版本不接受：证明不了本机的值是用户改的，没有意见。
        let prepared = prepare_for_merge(&BTreeMap::new(), &local, &remote, &BTreeMap::new(), CUR);
        assert!(prepared.local.is_empty());
        // 没有基线，但对方的值本版本接受：照常合并（两边都新增，按设备 ID 平局）。
        let valid_remote = map(&[("ui_theme", json!("snow"))]);
        let prepared = prepare_for_merge(
            &BTreeMap::new(),
            &local,
            &valid_remote,
            &BTreeMap::new(),
            CUR,
        );
        assert_eq!(prepared.local, local);

        // 对方没有这个键：本机的值留着（缺一项不当成删除）。
        let prepared = prepare_for_merge(&base_aurora, &local, &BTreeMap::new(), &pending, CUR);
        assert_eq!(prepared.local, local);
    }

    #[test]
    fn pending_records_only_declined_keys_where_local_differs() {
        let target = map(&[
            ("ui_theme", json!("aurora")),
            ("terminal_font_size", json!(14)),
            ("terminal_cursor_style", json!("triangle")),
            ("future_key", json!(1)),
        ]);
        let local = map(&[
            ("ui_theme", json!("midnight")),
            ("terminal_font_size", json!(14)),
            ("terminal_cursor_style", json!("triangle")),
        ]);
        let pending = compute_pending(&target, &local, CUR);
        assert_eq!(
            pending,
            map(&[("ui_theme", json!("midnight"))]),
            "只记值不合格并且本机值不一样的同步键（不认识的键本机本来就没有）"
        );
        assert!(compute_pending(&target, &BTreeMap::new(), CUR).is_empty());
    }

    #[test]
    fn same_data_for_local_ignores_what_the_machine_never_writes() {
        use crate::sync::package::SyncSnapshot;
        let mut target = SyncSnapshot::empty("cloud", 1);
        target.settings = map(&[
            ("ui_theme", json!("aurora")),
            ("future_key", json!(1)),
            ("terminal_font_size", json!(16)),
        ]);
        let mut local = SyncSnapshot::empty("dev", 1);
        local.settings = map(&[
            ("ui_theme", json!("midnight")),
            ("terminal_font_size", json!(16)),
        ]);
        assert!(same_data_for_local(&target, &local, CUR));

        // 真正要写的差别还是算差别。
        local
            .settings
            .insert("terminal_font_size".to_owned(), json!(18));
        assert!(!same_data_for_local(&target, &local, CUR));
        // 本机有同步键、合并结果里没有：写库会删掉它，也是差别。
        local
            .settings
            .insert("terminal_font_size".to_owned(), json!(16));
        local
            .settings
            .insert("terminal_scrollback".to_owned(), json!(5000));
        assert!(!same_data_for_local(&target, &local, CUR));
    }

    /// 取出 TypeScript 源码里每行 `id: "…"` 的值。
    fn ids_in(source: &str) -> BTreeSet<String> {
        source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("id: \""))
            .filter_map(|rest| rest.split_once('"').map(|(id, _)| id.to_owned()))
            .collect()
    }

    #[test]
    fn skin_id_lists_match_the_ui() {
        let ui = ids_in(&read("ui/src/state/skins.ts"));
        let rust: BTreeSet<String> = LIGHT_SKIN_IDS
            .iter()
            .chain(DARK_SKIN_IDS)
            .map(|id| id.to_string())
            .collect();
        assert_eq!(rust, ui, "皮肤 id 和 skins.ts 对不上，同步校验会拒绝合法值");
        for id in &ui {
            assert!(
                TERMINAL_THEME_IDS.contains(&id.as_str()),
                "终端配色要能选皮肤 {id}"
            );
        }
        accepts("ui_theme_mode", json!("system"));
        rejects("ui_theme_mode", json!("auto"));
        accepts("ui_skin_dark", json!("dracula"));
        rejects("ui_skin_dark", json!("paper"));
        accepts("terminal_theme", json!("skin"));
        accepts("terminal_theme", json!("system-catppuccin-dark"));
    }

    #[test]
    fn machine_specific_settings_are_not_synced() {
        for key in [
            "layout.ai_panel_width",
            "layout.aside_panel_width",
            "layout.sftp_panel_width",
            "layout.sftp_split_ratio",
            "layout.host_tree_width",
            "layout.ai_input_height",
            "layout.quick_bar_height",
            "ai_opencode_path",
            "ai_codex_path",
            "ai_claude_path",
            "ai_exec_mode",
            "sync_device_id",
            "sync_backup_keep",
            "sync_last_backup_version",
            "sync_storage",
            "sync_master_password",
            "sync_onedrive_tokens",
            // UI-4：关闭主窗口的行为是本机习惯，托盘提示状态是本机状态
            "close_action",
            "tray_hint_shown",
            "file_window_close",
        ] {
            assert_eq!(sync_mode(key), Some(SyncMode::LocalOnly), "{key}");
            assert!(!is_synced(key), "{key} 不能同步");
        }
        for key in [
            "ui_theme",
            "terminal_theme",
            "terminal_font_family",
            "default_proxy_id",
            "ai_default_agent",
            "sftp_concurrency",
            "sftp_preserve_mtime",
            "sftp_preserve_permissions",
            "ai_write_backup_keep",
        ] {
            assert!(is_synced(key), "{key} 应该同步");
        }
        // 没归类的新键默认不同步。
        assert!(!is_synced("some_future_setting"));
    }

    /// R-8 0.2：外部编辑器整个删掉了，旧数据库或旧同步包里还带着这两个键时安静忽略
    /// （不在白名单里就不会进同步包，也不会被当成错误）。
    #[test]
    fn removed_external_editor_settings_are_silently_ignored() {
        for key in ["sftp_external_editor", "sftp_double_click_editor"] {
            assert_eq!(sync_mode(key), None, "{key} 已经删掉，不该再归类");
            assert!(!is_synced(key), "{key} 不能进同步包");
        }
    }

    #[test]
    fn quick_commands_are_checked_item_by_item() {
        let list = |items: Value| json!(items.to_string());
        accepts(
            "quick_commands",
            list(json!([
                {"id": "a1", "name": "端口查看", "command": "ss -tlnp", "group": "常用", "enter": true},
                {"id": "a2", "name": "两行", "command": "apt update\napt -y upgrade", "enter": false}
            ])),
        );
        accepts("quick_commands", list(json!([])));
        rejects("quick_commands", json!([]));
        rejects("quick_commands", json!("not json"));
        rejects("quick_commands", list(json!([{"id": "a", "name": "x"}])));
        rejects(
            "quick_commands",
            list(json!([{"id": "a", "name": "x", "command": "echo \u{1b}[2J"}])),
        );
        rejects(
            "quick_commands",
            list(json!([{"id": "a", "name": "x", "command": "ls", "enter": "yes"}])),
        );
        let too_many: Vec<Value> = (0..=QUICK_COMMANDS_MAX)
            .map(|i| json!({"id": format!("c{i}"), "name": "n", "command": "ls"}))
            .collect();
        rejects("quick_commands", list(Value::Array(too_many)));
    }

    #[test]
    fn highlight_rules_are_checked_item_by_item() {
        let list = |items: Value| json!(items.to_string());
        accepts(
            "terminal_highlight_rules",
            list(json!([
                {"id": "h1", "pattern": "error|fail", "regex": true, "caseSensitive": false,
                 "color": "#ef4444", "background": false},
                {"id": "h2", "pattern": "WARN", "color": "#f59e0b"}
            ])),
        );
        accepts("terminal_highlight_rules", list(json!([])));
        rejects("terminal_highlight_rules", json!([]));
        rejects(
            "terminal_highlight_rules",
            list(json!([{"id": "h", "pattern": "x", "color": "red"}])),
        );
        rejects(
            "terminal_highlight_rules",
            list(json!([{"id": "h", "pattern": "", "color": "#ffffff"}])),
        );
        rejects(
            "terminal_highlight_rules",
            list(json!([{"id": "h", "pattern": "x", "color": "#ffffff", "regex": "yes"}])),
        );
        // 单条开关：布尔值可以，别的不行；没有这个字段（旧版本）也可以
        accepts(
            "terminal_highlight_rules",
            list(json!([{"id": "h", "pattern": "x", "color": "#ffffff", "enabled": false}])),
        );
        rejects(
            "terminal_highlight_rules",
            list(json!([{"id": "h", "pattern": "x", "color": "#ffffff", "enabled": "no"}])),
        );
    }
}
