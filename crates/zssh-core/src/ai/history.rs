//! AI 面板的历史会话（R11）：关掉面板、重启 Z-SSH 之后还能回看和续接。
//!
//! 存在 `<数据目录>\ai-history\<智能体>\<会话 id>.json`，一个会话一个文件。没有单独的
//! 索引文件：索引和会话文件两处写，任何一处写坏都会对不上；启动 / 打开列表时扫一遍
//! 目录（每种智能体最多 50 个文件）足够快，也不会有"索引说有、文件没了"的情况。
//!
//! 存什么、不存什么：
//! - 存：标题（首条用户消息前 40 字）、创建 / 更新时间、目标主机的**显示名**、用户消息、
//!   AI 回复、工具调用名和参数摘要（每个字符串字段最多 2 KB）、执行结果摘要（每条最多
//!   4 KB）、智能体自己的会话标识（续接用）；
//! - 不存：密码、令牌、能力密钥、桥端口、主机地址 / 用户名。工具参数里名字像凭据的键
//!   整个去掉；调用方把这个对话的能力密钥和连接令牌传进来，正文里出现的一律替换掉
//!   （模型偶尔会把系统提示里的密钥复述出来）。
//!
//! 上限：每种智能体最多保留最近 50 个会话（超出删最旧的）；单个文件最多 2 MB（超出先
//! 丢最早的消息）。写文件先写临时文件再改名，写到一半断电不会留下半个文件；读到坏
//! 文件跳过并记日志，不让整个列表打不开。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 每种智能体最多保留的会话数。
pub const MAX_SESSIONS_PER_AGENT: usize = 50;
/// 单个会话文件的上限。
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
/// 每条工具执行结果最多保留的字节数。
pub const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024;
/// 工具参数里每个字符串字段最多保留的字节数（参数摘要）。
pub const MAX_TOOL_INPUT_FIELD_BYTES: usize = 2 * 1024;
/// 标题取首条用户消息的前多少个字。
pub const TITLE_CHARS: usize = 40;
/// 主机显示名的上限（防止异常长的名字撑大列表）。
const HOST_LABEL_CHARS: usize = 100;
/// 认得的智能体（目录名就是它们，别的一律拒绝，防止拼出奇怪的路径）。
pub const AGENTS: [&str; 3] = ["opencode", "codex", "claude"];
/// 文件格式版本。
const FORMAT_VERSION: u32 = 1;
/// 被截断的内容末尾加的标记。
const TRUNCATED_MARK: &str = "…（已截断）";
/// 正文里出现的密钥替换成这个。
const REDACTED: &str = "[已隐藏]";
/// 工具参数里名字含这些词的键整个去掉（不区分大小写）。
const SECRET_KEY_WORDS: [&str; 9] = [
    "capability",
    "token",
    "password",
    "passphrase",
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "credential",
];

/// 一条消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryMessage {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Tool {
        tool: String,
        /// 参数摘要（凭据类的键已去掉、长字符串已截断）。
        #[serde(default)]
        input: Value,
        /// 执行结果摘要（最多 4 KB）。
        #[serde(default)]
        output: String,
        /// 卡片状态（done / error / rejected / stopped / running）。
        #[serde(default)]
        status: String,
    },
}

/// 一个会话文件的完整内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub version: u32,
    pub id: String,
    pub agent: String,
    pub title: String,
    /// 毫秒时间戳。
    pub created_at: u64,
    pub updated_at: u64,
    /// 目标主机的显示名（主机库里的名字；不是地址）。
    #[serde(default)]
    pub host_label: Option<String>,
    /// 智能体自己的会话标识（opencode 的 session id、codex 的 thread id、Claude Code
    /// 的 session id），续接用；没有就只能回看。
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<HistoryMessage>,
}

/// 列表里的一项（不带消息正文）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistorySummary {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub host_label: Option<String>,
    pub message_count: usize,
    /// 有智能体的会话标识（可以尝试续接）。
    pub resumable: bool,
}

/// 前端交来的一次保存（时间、标题、会话标识由这里定）。
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryDraft {
    pub id: String,
    #[serde(default)]
    pub host_label: Option<String>,
    #[serde(default)]
    pub messages: Vec<HistoryMessage>,
}

/// 列表扫描时只解析摘要需要的字段（消息不展开，省内存）。
#[derive(Deserialize)]
struct SummaryOnly {
    id: String,
    agent: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    updated_at: u64,
    #[serde(default)]
    host_label: Option<String>,
    #[serde(default)]
    agent_session_id: Option<String>,
    #[serde(default)]
    messages: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("不认识的智能体：{0}")]
    InvalidAgent(String),
    #[error("会话 id 不合法")]
    InvalidId,
    #[error("历史会话不存在")]
    NotFound,
    #[error("历史会话文件损坏：{0}")]
    Corrupt(String),
    #[error("读写历史会话失败：{0}")]
    Io(String),
}

/// 历史会话的存取。
pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    /// `data_dir` 是应用数据目录（支持 `ZSSH_DATA_DIR` 覆盖，由调用方解析好）。
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("ai-history"),
        }
    }

    /// 历史目录（`<数据目录>\ai-history`）。
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn agent_dir(&self, agent: &str) -> Result<PathBuf, HistoryError> {
        if !AGENTS.contains(&agent) {
            return Err(HistoryError::InvalidAgent(agent.to_string()));
        }
        Ok(self.root.join(agent))
    }

    fn file_of(&self, agent: &str, id: &str) -> Result<PathBuf, HistoryError> {
        if !valid_id(id) {
            return Err(HistoryError::InvalidId);
        }
        Ok(self.agent_dir(agent)?.join(format!("{id}.json")))
    }

    /// 保存（新建或覆盖）一个会话。没有任何用户消息时不存，返回 `Ok(None)`。
    ///
    /// `agent_session_id` 是这次拿到的智能体会话标识；None 时沿用文件里原来的。
    /// `secrets` 是要从正文里抹掉的字符串（这个对话的能力密钥、连接令牌）。
    pub fn save(
        &self,
        agent: &str,
        draft: HistoryDraft,
        agent_session_id: Option<String>,
        secrets: &[&str],
        now_ms: u64,
    ) -> Result<Option<HistorySummary>, HistoryError> {
        let path = self.file_of(agent, &draft.id)?;
        let messages: Vec<HistoryMessage> = draft
            .messages
            .into_iter()
            .map(|m| sanitize_message(m, secrets))
            .collect();
        let Some(title) = title_of(&messages) else {
            return Ok(None);
        };
        // 原来的文件读不出来（坏了）就当新的：创建时间从现在算，不因为坏文件存不了。
        let previous = read_record(&path).ok();
        let record = HistoryRecord {
            version: FORMAT_VERSION,
            id: draft.id,
            agent: agent.to_string(),
            title,
            created_at: previous.as_ref().map_or(now_ms, |p| p.created_at),
            updated_at: now_ms,
            host_label: draft
                .host_label
                .map(|h| h.trim().chars().take(HOST_LABEL_CHARS).collect::<String>())
                .filter(|h| !h.is_empty()),
            agent_session_id: agent_session_id
                .filter(|s| !s.trim().is_empty())
                .or_else(|| previous.and_then(|p| p.agent_session_id)),
            messages,
        };
        let (bytes, message_count) = encode_within_limit(record.clone())?;
        let dir = self.agent_dir(agent)?;
        std::fs::create_dir_all(&dir).map_err(|e| HistoryError::Io(e.to_string()))?;
        write_atomically(&dir, &path, &bytes)?;
        self.prune(&dir, &path);
        Ok(Some(HistorySummary {
            id: record.id,
            agent: record.agent,
            title: record.title,
            created_at: record.created_at,
            updated_at: record.updated_at,
            host_label: record.host_label,
            message_count,
            resumable: record.agent_session_id.is_some(),
        }))
    }

    /// 某个智能体的历史列表，按更新时间从新到旧。坏文件跳过并记日志。
    pub fn list(&self, agent: &str) -> Result<Vec<HistorySummary>, HistoryError> {
        let dir = self.agent_dir(agent)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(HistoryError::Io(err.to_string())),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = history_file_id(&path) else {
                continue;
            };
            match read_summary(&path) {
                Ok(summary) if summary.id == id && summary.agent == agent => out.push(summary),
                Ok(_) => {
                    tracing::warn!(file = %id, "历史会话文件的 id / 智能体和文件名对不上，跳过")
                }
                Err(err) => tracing::warn!(file = %id, error = %err, "历史会话文件读不了，跳过"),
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        Ok(out)
    }

    /// 读一个会话（回看 / 续接）。
    pub fn load(&self, agent: &str, id: &str) -> Result<HistoryRecord, HistoryError> {
        let path = self.file_of(agent, id)?;
        let mut record = read_record(&path)?;
        if record.agent != agent || record.id != id {
            return Err(HistoryError::Corrupt(
                "id / 智能体和文件名对不上".to_string(),
            ));
        }
        // 续接时这个标识会拼进智能体的命令行参数（例如 claude --resume）：样子不对就当没有，
        // 按"上下文没带过去"的新会话继续。
        if record
            .agent_session_id
            .as_deref()
            .is_some_and(|session| !valid_agent_session_id(session))
        {
            tracing::warn!(agent, "历史会话里的智能体会话标识格式不对，按新会话处理");
            record.agent_session_id = None;
        }
        Ok(record)
    }

    /// 删一个会话（不存在也算成功）。
    pub fn delete(&self, agent: &str, id: &str) -> Result<(), HistoryError> {
        let path = self.file_of(agent, id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(HistoryError::Io(err.to_string())),
        }
    }

    /// 清空所有智能体的历史，返回删掉了几个会话。只删我们自己的文件，目录里别的东西不碰。
    pub fn clear(&self) -> Result<usize, HistoryError> {
        let mut removed = 0;
        for agent in AGENTS {
            let dir = self.root.join(agent);
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(HistoryError::Io(err.to_string())),
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let ours = history_file_id(&path).is_some() || is_temp_file(&path);
                if ours && std::fs::remove_file(&path).is_ok() && !is_temp_file(&path) {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// 超出 50 个时按修改时间删最旧的（刚写的那个不删）。
    fn prune(&self, dir: &Path, keep: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| history_file_id(p).is_some())
            .map(|p| {
                let modified = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (modified, p)
            })
            .collect();
        if files.len() <= MAX_SESSIONS_PER_AGENT {
            return;
        }
        files.sort_by_key(|(modified, _)| *modified);
        let excess = files.len() - MAX_SESSIONS_PER_AGENT;
        for (_, path) in files.into_iter().filter(|(_, p)| p != keep).take(excess) {
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(error = %err, "删除超出上限的历史会话失败");
            }
        }
    }
}

/// 会话 id：只允许字母、数字、`-`、`_`，1～64 个字符（拼进文件名，不能带路径）。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 智能体自己的会话标识（opencode 的 ses_…、codex 的 thread id、Claude 的 UUID）：
/// 只允许字母、数字、`-`、`_`，1～128 个字符；开头必须是字母或数字，免得被当成命令行开关。
pub fn valid_agent_session_id(id: &str) -> bool {
    id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<id>.json` → `Some(id)`；临时文件和别的文件 → None。
fn history_file_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_suffix(".json")?;
    valid_id(id).then(|| id.to_string())
}

fn is_temp_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.') && n.contains(".json.tmp-"))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, HistoryError> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(HistoryError::NotFound)
        }
        Err(err) => return Err(HistoryError::Io(err.to_string())),
    };
    // 正常写出来的文件不会超过 2 MB；大得离谱的是坏文件，不整个读进内存。
    if meta.len() > (MAX_FILE_BYTES as u64) * 2 {
        return Err(HistoryError::Corrupt("文件超过上限".to_string()));
    }
    std::fs::read(path).map_err(|e| HistoryError::Io(e.to_string()))
}

fn read_record(path: &Path) -> Result<HistoryRecord, HistoryError> {
    let bytes = read_bytes(path)?;
    serde_json::from_slice(&bytes).map_err(|e| HistoryError::Corrupt(e.to_string()))
}

fn read_summary(path: &Path) -> Result<HistorySummary, HistoryError> {
    let bytes = read_bytes(path)?;
    let raw: SummaryOnly =
        serde_json::from_slice(&bytes).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
    Ok(HistorySummary {
        id: raw.id,
        agent: raw.agent,
        title: raw.title,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        host_label: raw.host_label,
        message_count: raw.messages.len(),
        resumable: raw.agent_session_id.is_some(),
    })
}

/// 先写同目录下的临时文件再改名（Windows 上 `rename` 会覆盖已有文件）。
fn write_atomically(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), HistoryError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(HistoryError::InvalidId)?;
    let temp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    if let Err(err) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(HistoryError::Io(err.to_string()));
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(HistoryError::Io(err.to_string()));
    }
    Ok(())
}

/// 序列化，超过 2 MB 就从最早的消息开始丢；只剩一条还超就截断它的正文。
/// 返回写进文件的字节和最终保留的消息条数。
fn encode_within_limit(mut record: HistoryRecord) -> Result<(Vec<u8>, usize), HistoryError> {
    let encode = |r: &HistoryRecord| -> Result<(Vec<u8>, usize), HistoryError> {
        serde_json::to_vec(r)
            .map(|bytes| (bytes, r.messages.len()))
            .map_err(|e| HistoryError::Io(e.to_string()))
    };
    let (bytes, count) = encode(&record)?;
    if bytes.len() <= MAX_FILE_BYTES {
        return Ok((bytes, count));
    }
    // 每条消息单独算一次大小，一次性算出要丢几条（不反复整篇序列化）。
    let sizes: Vec<usize> = record
        .messages
        .iter()
        .map(|m| serde_json::to_vec(m).map(|b| b.len() + 1).unwrap_or(0))
        .collect();
    let mut over = bytes.len() - MAX_FILE_BYTES;
    let mut drop = 0;
    while over > 0 && drop + 1 < sizes.len() {
        over = over.saturating_sub(sizes[drop]);
        drop += 1;
    }
    record.messages.drain(..drop);
    let (bytes, count) = encode(&record)?;
    if bytes.len() <= MAX_FILE_BYTES {
        return Ok((bytes, count));
    }
    // 只剩一条（或估算有误差）还超：把剩下第一条的正文截到放得下。
    let over = bytes.len() - MAX_FILE_BYTES;
    if let Some(first) = record.messages.first_mut() {
        let text = match first {
            HistoryMessage::User { text } | HistoryMessage::Assistant { text } => text,
            HistoryMessage::Tool { output, .. } => output,
        };
        // JSON 转义会让一个字符占好几个字节，多留些余量。
        let keep = text.len().saturating_sub(over * 6 + 64);
        *text = truncate_bytes(text, keep);
    }
    let (bytes, count) = encode(&record)?;
    if bytes.len() <= MAX_FILE_BYTES {
        return Ok((bytes, count));
    }
    record.messages.clear();
    encode(&record)
}

/// 标题：首条用户消息，空白合并成一个空格，取前 40 个字。
fn title_of(messages: &[HistoryMessage]) -> Option<String> {
    messages.iter().find_map(|m| match m {
        HistoryMessage::User { text } => {
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!flat.is_empty()).then(|| flat.chars().take(TITLE_CHARS).collect())
        }
        _ => None,
    })
}

fn sanitize_message(message: HistoryMessage, secrets: &[&str]) -> HistoryMessage {
    match message {
        HistoryMessage::User { text } => HistoryMessage::User {
            text: redact(&text, secrets),
        },
        HistoryMessage::Assistant { text } => HistoryMessage::Assistant {
            text: redact(&text, secrets),
        },
        HistoryMessage::Tool {
            tool,
            input,
            output,
            status,
        } => HistoryMessage::Tool {
            tool: truncate_bytes(&tool, 200),
            input: sanitize_input(input, secrets),
            output: truncate_bytes(&redact(&output, secrets), MAX_TOOL_OUTPUT_BYTES),
            status: truncate_bytes(&status, 32),
        },
    }
}

/// 工具参数摘要：凭据类的键去掉，每个字符串截到 2 KB，密钥替换掉。
fn sanitize_input(value: Value, secrets: &[&str]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| !is_secret_key(key))
                .map(|(key, v)| (key, sanitize_input(v, secrets)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| sanitize_input(v, secrets))
                .collect(),
        ),
        Value::String(text) => Value::String(truncate_bytes(
            &redact(&text, secrets),
            MAX_TOOL_INPUT_FIELD_BYTES,
        )),
        other => other,
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_WORDS.iter().any(|word| lower.contains(word))
}

fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        // 太短的串（比如空串）替换起来会误伤正文，密钥和令牌都远比这长。
        if secret.len() >= 8 && out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// 截到最多 `max` 字节（不切断 UTF-8 字符），截了就加标记。
fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let budget = max.saturating_sub(TRUNCATED_MARK.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_MARK}", &text[..end])
}

/// 现在的毫秒时间戳。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (crate::test_util::TestDir, HistoryStore) {
        let dir = crate::test_util::TestDir::new("ai history");
        let store = HistoryStore::new(&dir);
        (dir, store)
    }

    fn draft(id: &str, messages: Vec<HistoryMessage>) -> HistoryDraft {
        HistoryDraft {
            id: id.to_string(),
            host_label: Some("web-1".to_string()),
            messages,
        }
    }

    fn user(text: &str) -> HistoryMessage {
        HistoryMessage::User {
            text: text.to_string(),
        }
    }

    fn assistant(text: &str) -> HistoryMessage {
        HistoryMessage::Assistant {
            text: text.to_string(),
        }
    }

    #[test]
    fn save_then_list_and_load_round_trip() {
        let (dir, store) = store();
        let summary = store
            .save(
                "opencode",
                draft("s1", vec![user("看看磁盘"), assistant("好的")]),
                Some("ses_1".into()),
                &[],
                1000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(summary.title, "看看磁盘");
        assert!(summary.resumable);
        assert!(dir
            .join("ai-history")
            .join("opencode")
            .join("s1.json")
            .is_file());

        let list = store.list("opencode").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].message_count, 2);
        assert_eq!(list[0].host_label.as_deref(), Some("web-1"));
        // 按智能体分开：别的智能体看不到。
        assert!(store.list("codex").unwrap().is_empty());

        let record = store.load("opencode", "s1").unwrap();
        assert_eq!(record.agent_session_id.as_deref(), Some("ses_1"));
        assert_eq!(record.messages.len(), 2);
        assert_eq!(record.created_at, 1000);
    }

    #[test]
    fn resave_keeps_created_at_and_previous_session_id() {
        let (_dir, store) = store();
        store
            .save(
                "codex",
                draft("s1", vec![user("a")]),
                Some("t-1".into()),
                &[],
                1000,
            )
            .unwrap();
        store
            .save(
                "codex",
                draft("s1", vec![user("a"), assistant("b")]),
                None,
                &[],
                2000,
            )
            .unwrap();
        let record = store.load("codex", "s1").unwrap();
        assert_eq!(record.created_at, 1000);
        assert_eq!(record.updated_at, 2000);
        assert_eq!(record.agent_session_id.as_deref(), Some("t-1"));
        // 续接失败换了新会话：新的标识覆盖旧的。
        store
            .save(
                "codex",
                draft("s1", vec![user("a")]),
                Some("t-2".into()),
                &[],
                3000,
            )
            .unwrap();
        assert_eq!(
            store
                .load("codex", "s1")
                .unwrap()
                .agent_session_id
                .as_deref(),
            Some("t-2")
        );
    }

    #[test]
    fn title_is_first_user_message_cut_to_40_chars() {
        let long = "检查".repeat(30);
        let messages = vec![
            assistant("先回复"),
            user(&format!("  {long}\n第二行 ")),
            user("x"),
        ];
        let title = title_of(&messages).unwrap();
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.starts_with("检查检查"));
        assert_eq!(title_of(&[assistant("没有用户消息")]), None);
    }

    #[test]
    fn no_user_message_means_nothing_saved() {
        let (dir, store) = store();
        let saved = store
            .save("opencode", draft("s1", vec![assistant("hi")]), None, &[], 1)
            .unwrap();
        assert!(saved.is_none());
        assert!(!dir
            .join("ai-history")
            .join("opencode")
            .join("s1.json")
            .exists());
    }

    #[test]
    fn secrets_are_never_written() {
        let (dir, store) = store();
        let capability = "0123456789abcdef0123456789abcdef";
        let token = "conn-token-very-secret";
        let tool = HistoryMessage::Tool {
            tool: "zssh_run_command".into(),
            input: json!({
                "capability": capability,
                "command": "uptime",
                "nested": {"password": "hunter2", "api_key": "k", "path": "/etc"},
                "Authorization": "Bearer x"
            }),
            output: format!("ok {token}"),
            status: "done".into(),
        };
        let messages = vec![user("go"), assistant(&format!("密钥是 {capability}")), tool];
        store
            .save(
                "claude",
                draft("s1", messages),
                None,
                &[capability, token],
                1,
            )
            .unwrap();
        let text =
            std::fs::read_to_string(dir.join("ai-history").join("claude").join("s1.json")).unwrap();
        for leaked in [
            capability,
            token,
            "hunter2",
            "\"capability\"",
            "Authorization",
            "api_key",
        ] {
            assert!(
                !text.contains(leaked),
                "历史文件里不能出现 {leaked}：{text}"
            );
        }
        assert!(text.contains("uptime"));
        assert!(text.contains("/etc"));
        assert!(text.contains(REDACTED));
    }

    #[test]
    fn tool_output_is_cut_to_4kb_and_input_fields_to_2kb() {
        let (_dir, store) = store();
        let tool = HistoryMessage::Tool {
            tool: "zssh_write_file".into(),
            input: json!({ "path": "/tmp/a", "content": "中".repeat(5000) }),
            output: "输出".repeat(5000),
            status: "done".into(),
        };
        store
            .save(
                "opencode",
                draft("s1", vec![user("写"), tool]),
                None,
                &[],
                1,
            )
            .unwrap();
        let record = store.load("opencode", "s1").unwrap();
        let HistoryMessage::Tool { input, output, .. } = &record.messages[1] else {
            panic!("第二条应该是工具");
        };
        assert!(output.len() <= MAX_TOOL_OUTPUT_BYTES, "{}", output.len());
        assert!(output.ends_with(TRUNCATED_MARK));
        let content = input["content"].as_str().unwrap();
        assert!(content.len() <= MAX_TOOL_INPUT_FIELD_BYTES);
        assert_eq!(input["path"], "/tmp/a");
    }

    #[test]
    fn oversized_session_drops_oldest_messages_first() {
        let (dir, store) = store();
        // 每条约 300 KB，十条约 3 MB：超出 2 MB，要从最早的开始丢。
        let chunk = "x".repeat(300 * 1024);
        let mut messages = vec![user("最早的一条")];
        for i in 0..10 {
            messages.push(assistant(&format!("{i}:{chunk}")));
        }
        store
            .save("opencode", draft("big", messages), None, &[], 1)
            .unwrap();
        let path = dir.join("ai-history").join("opencode").join("big.json");
        let size = std::fs::metadata(&path).unwrap().len() as usize;
        assert!(size <= MAX_FILE_BYTES, "文件 {size} 字节超过上限");
        let record = store.load("opencode", "big").unwrap();
        // 最新的那条还在，最早的被丢了；标题仍是原来的首条用户消息。
        let last = match record.messages.last().unwrap() {
            HistoryMessage::Assistant { text } => text.clone(),
            _ => panic!(),
        };
        assert!(last.starts_with("9:"));
        assert!(!record
            .messages
            .iter()
            .any(|m| matches!(m, HistoryMessage::User { .. })));
        assert_eq!(record.title, "最早的一条");
    }

    #[test]
    fn single_huge_message_is_truncated_to_fit() {
        let (dir, store) = store();
        let huge = "y".repeat(3 * 1024 * 1024);
        store
            .save("opencode", draft("one", vec![user(&huge)]), None, &[], 1)
            .unwrap();
        let size = std::fs::metadata(dir.join("ai-history").join("opencode").join("one.json"))
            .unwrap()
            .len() as usize;
        assert!(size <= MAX_FILE_BYTES);
    }

    #[test]
    fn keeps_only_the_newest_50_per_agent() {
        let (dir, store) = store();
        let agent_dir = dir.join("ai-history").join("codex");
        for i in 0..(MAX_SESSIONS_PER_AGENT + 3) {
            store
                .save(
                    "codex",
                    draft(&format!("s{i:03}"), vec![user("hi")]),
                    None,
                    &[],
                    i as u64,
                )
                .unwrap();
            // 修改时间要分得开（文件系统时间精度有限）。
            let path = agent_dir.join(format!("s{i:03}.json"));
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i as u64);
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(when).unwrap();
        }
        let list = store.list("codex").unwrap();
        assert_eq!(list.len(), MAX_SESSIONS_PER_AGENT);
        // 最旧的三个被删了。
        for i in 0..3 {
            assert!(
                !agent_dir.join(format!("s{i:03}.json")).exists(),
                "s{i:03} 该被删"
            );
        }
        assert!(agent_dir.join("s003.json").exists());
        // 别的智能体不受影响。
        store
            .save("opencode", draft("o1", vec![user("hi")]), None, &[], 1)
            .unwrap();
        assert_eq!(store.list("opencode").unwrap().len(), 1);
    }

    #[test]
    fn broken_files_are_skipped_not_fatal() {
        let (dir, store) = store();
        store
            .save("opencode", draft("good", vec![user("ok")]), None, &[], 5)
            .unwrap();
        let agent_dir = dir.join("ai-history").join("opencode");
        std::fs::write(agent_dir.join("bad.json"), "{ 坏的").unwrap();
        std::fs::write(agent_dir.join("empty.json"), "").unwrap();
        // 文件名和内容里的 id 对不上（被人改名了）：跳过。
        std::fs::copy(agent_dir.join("good.json"), agent_dir.join("renamed.json")).unwrap();
        std::fs::write(agent_dir.join("notes.txt"), "不是我们的").unwrap();
        let list = store.list("opencode").unwrap();
        assert_eq!(
            list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["good"]
        );
        assert!(matches!(
            store.load("opencode", "bad"),
            Err(HistoryError::Corrupt(_))
        ));
        assert!(matches!(
            store.load("opencode", "nope"),
            Err(HistoryError::NotFound)
        ));
        // 坏文件可以被新的保存覆盖。
        store
            .save("opencode", draft("bad", vec![user("修好")]), None, &[], 6)
            .unwrap();
        assert_eq!(store.load("opencode", "bad").unwrap().title, "修好");
    }

    #[test]
    fn rejects_unknown_agents_and_path_like_ids() {
        let (_dir, store) = store();
        for id in ["../x", "a/b", "a\\b", "", "a.json", &"x".repeat(65)] {
            assert!(
                matches!(
                    store.save("opencode", draft(id, vec![user("hi")]), None, &[], 1),
                    Err(HistoryError::InvalidId)
                ),
                "id {id:?} 应被拒绝"
            );
        }
        assert!(matches!(
            store.list("../etc"),
            Err(HistoryError::InvalidAgent(_))
        ));
        assert!(matches!(
            store.load("other", "s1"),
            Err(HistoryError::InvalidAgent(_))
        ));
    }

    #[test]
    fn delete_and_clear_only_touch_our_files() {
        let (dir, store) = store();
        store
            .save("opencode", draft("a", vec![user("1")]), None, &[], 1)
            .unwrap();
        store
            .save("codex", draft("b", vec![user("2")]), None, &[], 1)
            .unwrap();
        store
            .save("claude", draft("c", vec![user("3")]), None, &[], 1)
            .unwrap();
        store.delete("opencode", "a").unwrap();
        store.delete("opencode", "a").unwrap();
        assert!(store.list("opencode").unwrap().is_empty());
        let other = dir.join("ai-history").join("codex").join("keep.txt");
        std::fs::write(&other, "x").unwrap();
        assert_eq!(store.clear().unwrap(), 2);
        assert!(store.list("codex").unwrap().is_empty());
        assert!(store.list("claude").unwrap().is_empty());
        assert!(other.exists(), "别的文件不碰");
    }

    #[test]
    fn no_temp_files_left_after_save() {
        let (dir, store) = store();
        store
            .save("opencode", draft("a", vec![user("1")]), None, &[], 1)
            .unwrap();
        store
            .save("opencode", draft("a", vec![user("2")]), None, &[], 2)
            .unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.join("ai-history").join("opencode"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.json".to_string()]);
    }

    #[test]
    fn host_label_is_trimmed_and_capped() {
        let (_dir, store) = store();
        let mut d = draft("a", vec![user("1")]);
        d.host_label = Some(format!("  {}  ", "h".repeat(300)));
        let summary = store.save("opencode", d, None, &[], 1).unwrap().unwrap();
        assert_eq!(summary.host_label.unwrap().len(), HOST_LABEL_CHARS);
        let mut d = draft("b", vec![user("1")]);
        d.host_label = Some("   ".into());
        let summary = store.save("opencode", d, None, &[], 1).unwrap().unwrap();
        assert_eq!(summary.host_label, None);
    }

    #[test]
    fn agent_session_ids_only_take_plain_identifiers() {
        for ok in ["ses_01JABC", "0b7c1e2a-55f4-4c1d-9a8e-1f2b3c4d5e6f", "t-1"] {
            assert!(valid_agent_session_id(ok), "{ok}");
        }
        for bad in [
            "",
            "--dangerously-skip-permissions",
            "-x",
            "_x",
            "a b",
            "x;rm -rf /",
            "a/b",
            "é",
        ] {
            assert!(!valid_agent_session_id(bad), "{bad:?}");
        }
        assert!(!valid_agent_session_id(&"a".repeat(129)));
    }
}
