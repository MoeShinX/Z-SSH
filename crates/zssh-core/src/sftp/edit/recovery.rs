//! 编辑的崩溃恢复（F05）：恢复目录、恢复记录、草稿，以及上次没处理完的修改。
//!
//! 每个编辑的临时目录里除了下载下来的文件，还放一个 `meta.json`：记着它对应哪台主机的哪个
//! 远端文件、打开时远端的样子、以及"最近一次确认和远端一致"的内容摘要。程序启动时扫描上次
//! 留下的目录：摘要和现在的内容一致 = 没有未上传的修改，直接清掉；不一致（或者认不出）= 可能
//! 有用户的修改，**留下来**交给用户处理（重新上传 / 另存到本地 / 放弃）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{EditBackend, EditOutcome, EditSession, RemoteSnapshot, Resolution};

/// 一个编辑目录 `eN/` 里**用户的文件**和**内部状态**分开放，互不重叠：
/// - `eN/content/<远端文件名>`：下载下来、交给编辑器的文件；
/// - `eN/state/meta.json`：恢复记录。
///
/// 远端文件叫 `meta.json`、`meta.json.tmp`、`META.JSON` 都不会和内部文件撞上，以后再加内部文件也不会。
pub const CONTENT_DIR: &str = "content";
pub const STATE_DIR: &str = "state";
/// 恢复记录的文件名（在 `state/` 里）。
pub const META_FILE: &str = "meta.json";
/// 内置编辑器的**草稿**：用户还没保存的内容，每隔几秒存一份。放在 `eN/draft/<文件名>`，和
/// `content/`（最近一次和远端对齐的内容）分开——草稿不能被当成「已保存」去触发上传。
pub const DRAFT_DIR: &str = "draft";

pub fn content_dir(dir: &Path) -> PathBuf {
    dir.join(CONTENT_DIR)
}

pub fn state_dir(dir: &Path) -> PathBuf {
    dir.join(STATE_DIR)
}

pub fn draft_dir(dir: &Path) -> PathBuf {
    dir.join(DRAFT_DIR)
}

/// 草稿文件的路径（文件名同样只取最后一段）。
pub fn draft_path(dir: &Path, file_name: &str) -> PathBuf {
    let name = Path::new(file_name)
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "file".into());
    draft_dir(dir).join(name)
}

/// 草稿版本的路径：草稿旁边的 `<文件名>.version`，记着这份草稿是哪个内容版本写的。
/// 保存版本 N 成功后只删版本 ≤ N 的草稿（更晚的草稿是保存期间新输入的内容，还没传上去）。
pub fn draft_version_path(dir: &Path, file_name: &str) -> PathBuf {
    let name = Path::new(file_name)
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "file".into());
    draft_dir(dir).join(format!("{}.version", name.to_string_lossy()))
}

/// 读草稿的版本号；没有（旧版本程序留下的草稿）就是 `None`。
pub fn read_draft_version(dir: &Path, file_name: &str) -> Option<u64> {
    std::fs::read_to_string(draft_version_path(dir, file_name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// 写草稿（先写临时文件再改名，崩在中间不会留下半截草稿）。`version` 是这份草稿对应的内容版本，
/// 保存成功后据此决定删不删（见 [`remove_draft_up_to`]）。
pub fn write_draft(dir: &Path, file_name: &str, bytes: &[u8], version: u64) -> std::io::Result<()> {
    let target = draft_path(dir, file_name);
    std::fs::create_dir_all(draft_dir(dir))?;
    let tmp = draft_dir(dir).join(format!(".draft-{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    crate::sftp::local::replace_file(&tmp, &target)?;
    // 内容先落盘、版本号后落盘：正常退出时两者一致；崩在中间时内容仍然算未保存的修改。
    let version_tmp = draft_dir(dir).join(format!(".draft-version-{}.tmp", std::process::id()));
    std::fs::write(&version_tmp, version.to_string())?;
    crate::sftp::local::replace_file(&version_tmp, &draft_version_path(dir, file_name))
}

/// 删草稿（内容和版本号一起删；没有也不算错）。
pub fn remove_draft(dir: &Path, file_name: &str) {
    let _ = std::fs::remove_file(draft_path(dir, file_name));
    let _ = std::fs::remove_file(draft_version_path(dir, file_name));
}

/// 保存版本 `saved_version` 成功后调用：草稿版本不晚于它才删，更晚的草稿保留。
///
/// 旧版本程序留下的草稿没有版本号，按 0（比任何一次新保存都旧）处理——升级后第一次保存就该清掉它，
/// 否则它会被恢复列表当成“最新未保存内容”，反过来覆盖掉新保存的版本。
/// 返回有没有删。
pub fn remove_draft_up_to(dir: &Path, file_name: &str, saved_version: u64) -> bool {
    if !draft_path(dir, file_name).exists() {
        // 内容不在：版本号文件如果单独留着也没有意义，顺手清掉。
        let _ = std::fs::remove_file(draft_version_path(dir, file_name));
        return false;
    }
    let draft_version = read_draft_version(dir, file_name).unwrap_or(0);
    if draft_version <= saved_version {
        remove_draft(dir, file_name);
        true
    } else {
        false
    }
}

/// 记录里的文件名对应的本地路径（只取最后一段，记录被改坏了也跑不出 `content/`）。
pub fn content_path(dir: &Path, file_name: &str) -> PathBuf {
    let name = Path::new(file_name)
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "file".into());
    content_dir(dir).join(name)
}

/// 一个编辑的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditMeta {
    pub remote_path: String,
    pub host_id: i64,
    /// 界面上显示的主机名。
    pub host_label: String,
    /// 临时目录里的本地文件名。
    pub file_name: String,
    /// 打开（或者上一次上传成功）时远端的大小。
    pub base_size: u64,
    pub base_mtime: Option<u32>,
    /// 最近一次确认"本地内容 = 远端内容"时本地内容的 SHA-256（十六进制）。
    pub clean_sha256: String,
    /// 编辑的是**本机**的文件（内置编辑器打开了本地面板里的文件）：`remote_path` 里放的是本地路径，
    /// 恢复时是「写回原文件」而不是「重新上传」。旧版本的记录没有这一项，当作 `false`。
    #[serde(default)]
    pub local: bool,
    /// 本机文件是符号链接时，打开时解析出来的真实目标（保存 / 恢复写这里，链接本身不动）。
    /// 旧版本的记录没有这一项，当作「不是链接」（写 `remote_path`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// 打开时连到的服务器身份（`用户名@主机:端口`，远端文件才有）。崩溃后"重新上传"要核对它：
    /// 只有 host_id 不够，主机记录的地址 / 用户名可能已经改过（R-10 M17）。旧版本的记录没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_identity: Option<String>,
}

impl EditMeta {
    pub fn base(&self) -> RemoteSnapshot {
        RemoteSnapshot {
            size: self.base_size,
            mtime: self.base_mtime,
        }
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 写元数据（先写临时文件再改名，崩在中间不会留下半截 JSON）。
pub fn write_meta(dir: &Path, meta: &EditMeta) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    let state = state_dir(dir);
    std::fs::create_dir_all(&state)?;
    let tmp = state.join(format!("{META_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    let final_path = state.join(META_FILE);
    crate::sftp::local::replace_file(&tmp, &final_path)
}

pub fn read_meta(dir: &Path) -> Option<EditMeta> {
    let raw = std::fs::read(state_dir(dir).join(META_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// 一个编辑目录里有没有没上传的修改：三种答案，**只有第一种可以自动删除**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsent {
    /// 确认没有修改：本地内容和最近一次对齐远端的内容一致（或者文件已经不在了）。
    None,
    /// 有未上传的修改。
    Yes,
    /// 不知道：恢复记录缺失 / 损坏，或者读不了本地文件（编辑器正占着、权限、临时 I/O 错误）。
    /// 读不到只说明现在读不了，不说明没改过——保留，进恢复列表。
    Unknown,
}

/// 按恢复记录检查本地文件：`content/` 里的内容和草稿**任何一个**和「已对齐远端的内容」不同，都算有
/// 没上传的修改；读不了就是不知道。
pub fn unsent_state_of(dir: &Path, meta: &EditMeta) -> Unsent {
    let content = match std::fs::read(content_path(dir, &meta.file_name)) {
        Ok(data) if sha256_hex(&data) == meta.clean_sha256 => Unsent::None,
        Ok(_) => Unsent::Yes,
        // 文件不在了：没有东西可丢。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Unsent::None,
        Err(_) => Unsent::Unknown,
    };
    let draft = draft_state(dir, meta);
    match (content, draft) {
        (Unsent::Yes, _) | (_, Unsent::Yes) => Unsent::Yes,
        (Unsent::Unknown, _) | (_, Unsent::Unknown) => Unsent::Unknown,
        _ => Unsent::None,
    }
}

/// 草稿的状态：没有草稿、或者草稿和已对齐的内容一样 = `None`；不一样 = `Yes`；读不了 = `Unknown`。
pub fn draft_state(dir: &Path, meta: &EditMeta) -> Unsent {
    match std::fs::read(draft_path(dir, &meta.file_name)) {
        Ok(data) if sha256_hex(&data) == meta.clean_sha256 => Unsent::None,
        Ok(_) => Unsent::Yes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Unsent::None,
        Err(_) => Unsent::Unknown,
    }
}

/// 检查一个编辑目录：记录读不出来就是"不知道"。
pub fn unsent_state(dir: &Path) -> Unsent {
    match read_meta(dir) {
        Some(meta) => unsent_state_of(dir, &meta),
        None => Unsent::Unknown,
    }
}

/// 这个目录可以自动删掉吗：**只有**确认没有未上传的修改（`Unsent::None`）才可以。
///
/// 有修改（Yes）和读不了 / 认不出（Unknown）都必须保留——"读不到"只说明现在判断不了，
/// 不说明没有修改（R-8 第 5 节）。
pub fn safe_to_delete(dir: &Path) -> bool {
    unsent_state(dir) == Unsent::None
}

/// 本地文件现在的内容和"已确认一致"的摘要不同吗？（"确定有"才返回 `true`；读不到不算。）
///
/// 只用于查询 / 提示；**自动清理不能用它**（读不到会被当成没有），清理一律用
/// [`unsent_state`] 的三态 + [`safe_to_delete`]。
pub fn has_unsent_changes(dir: &Path, meta: &EditMeta) -> bool {
    unsent_state_of(dir, meta) == Unsent::Yes
}

/// 上传成功（或者选了放弃修改、重新拉了远端）之后：把**实际对齐远端的那份内容**记成"已确认一致"。
///
/// `synced_sha256` 是上传（或拉回）的那些字节的摘要，**不去重新读本地文件**——上传期间用户可能
/// 又保存了一次，本地文件已经不是传上去的那份了；这时记录和本地不一致，仍然算"有未上传的修改"。
pub fn mark_clean(
    dir: &Path,
    remote_path: &str,
    base: RemoteSnapshot,
    synced_sha256: &str,
) -> std::io::Result<()> {
    let Some(mut meta) = read_meta(dir) else {
        return Ok(());
    };
    meta.clean_sha256 = synced_sha256.to_owned();
    meta.remote_path = remote_path.to_owned();
    meta.base_size = base.size;
    meta.base_mtime = base.mtime;
    write_meta(dir, &meta)
}

/// 启动时发现的、上次没处理完的编辑。
#[derive(Debug, Clone)]
pub struct Leftover {
    /// 目录名（`e3`），界面和命令里用它指这一项。
    pub id: String,
    pub dir: PathBuf,
    /// 里面那个文件（用户可能改过的内容）。
    pub local_path: PathBuf,
    /// 元数据；`None` = 认不出（元数据缺失或损坏），只能另存到本地或放弃。
    pub meta: Option<EditMeta>,
    /// 没法确认这份文件有没有改过（读不了文件、或者认不出）。因为保险才留下的。
    pub uncertain: bool,
    /// `local_path` 指的是内置编辑器的草稿（没保存的内容），不是 `content/` 里的文件。
    pub from_draft: bool,
}

/// 一个目录里"用户的文件"的枚举结果。
///
/// 只有 [`UserFiles::Empty`]（确认是空的）可以自动删目录；读不了（`Unknown`）必须保留，
/// "读不到"不等于"是空的"（R-8 第 5 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserFiles {
    Found(PathBuf),
    Empty,
    Unknown,
}

/// 扫描上次留下的临时目录：没有未上传修改的清掉，其余的返回。
pub fn scan(root: &Path) -> Vec<Leftover> {
    let mut leftovers = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return leftovers;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        match read_meta(&dir) {
            Some(meta) => match unsent_state_of(&dir, &meta) {
                // 确认没有修改：清掉。
                Unsent::None => {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                state => {
                    // 有草稿（没保存的内容）就以草稿为准：它才是用户最后想要的。
                    let from_draft = draft_state(&dir, &meta) == Unsent::Yes;
                    leftovers.push(Leftover {
                        id,
                        local_path: if from_draft {
                            draft_path(&dir, &meta.file_name)
                        } else {
                            content_path(&dir, &meta.file_name)
                        },
                        dir,
                        meta: Some(meta),
                        uncertain: state == Unsent::Unknown,
                        from_draft,
                    });
                }
            },
            None => {
                // 认不出：里面有文件就留着；确认空才清掉；读不了也留着（并且列出来）。
                match first_user_file(&dir) {
                    UserFiles::Found(local_path) => leftovers.push(Leftover {
                        id,
                        dir,
                        local_path,
                        meta: None,
                        uncertain: true,
                        from_draft: false,
                    }),
                    UserFiles::Empty => {
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                    UserFiles::Unknown => {
                        // 连里面有没有文件都确认不了：保留目录，列出来让用户能"放弃"。
                        leftovers.push(Leftover {
                            local_path: dir.clone(),
                            id,
                            dir,
                            meta: None,
                            uncertain: true,
                            from_draft: false,
                        });
                    }
                }
            }
        }
    }
    leftovers.sort_by(|a, b| a.id.cmp(&b.id));
    leftovers
}

/// 找一个目录里用户可能改过的文件。
///
/// 依次看草稿目录、`content/`、旧布局（文件直接放在 `eN/` 里，跳过内部文件）。任何一个
/// 位置"读不了"（不是 NotFound 的错误，比如权限 / IO）整体就是 [`UserFiles::Unknown`]；
/// 只有确认三个位置都没有文件才是 [`UserFiles::Empty`]。
fn first_user_file(dir: &Path) -> UserFiles {
    let first_in = |folder: &Path, skip_internal: bool| -> UserFiles {
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            // 目录不存在 = 这个位置确认没有文件。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return UserFiles::Empty,
            // 权限 / IO 错误：读不了，不知道里面有没有用户的文件。
            Err(_) => return UserFiles::Unknown,
        };
        // 读不了 = 不确定：迭代中途出错、某一项的属性读不出来，都不能当成"这里没有文件"，
        // 否则调用方会把这个恢复目录整个删掉（R-9 N03）。
        for entry in entries {
            let Ok(entry) = entry else {
                return UserFiles::Unknown;
            };
            let path = entry.path();
            let is_file = match entry.file_type() {
                // 符号链接看它指向什么（和原来的 `Path::is_file` 一样跟随链接）。
                Ok(kind) if kind.is_symlink() => match std::fs::metadata(&path) {
                    Ok(meta) => meta.is_file(),
                    Err(_) => return UserFiles::Unknown,
                },
                Ok(kind) => kind.is_file(),
                Err(_) => return UserFiles::Unknown,
            };
            if !is_file {
                continue;
            }
            if skip_internal
                && path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy().to_ascii_lowercase();
                    name == META_FILE || name == "meta.json.tmp"
                })
            {
                continue;
            }
            return UserFiles::Found(path);
        }
        UserFiles::Empty
    };
    // 新布局：草稿或 content/ 里的文件；旧布局（文件直接放在 eN/ 里）也保留，按"认不出"处理。
    match first_in(&draft_dir(dir), false) {
        UserFiles::Found(path) => return UserFiles::Found(path),
        UserFiles::Unknown => return UserFiles::Unknown,
        UserFiles::Empty => {}
    }
    match first_in(&content_dir(dir), false) {
        UserFiles::Found(path) => return UserFiles::Found(path),
        UserFiles::Unknown => return UserFiles::Unknown,
        UserFiles::Empty => {}
    }
    first_in(dir, true)
}

/// 目录名 `e<数字>` 里最大的数字（新建的编辑要从它后面编号，不能和留下的撞）。
pub fn max_edit_number(root: &Path) -> u64 {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .strip_prefix('e')
                        .and_then(|digits| digits.parse::<u64>().ok())
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// 把留下的修改重新传上去。`overwrite = false` 时先对一遍远端：远端在这期间被改过（或删了）
/// 就返回 `Conflict`，什么都不传；`overwrite = true` 是用户看过冲突之后选了"覆盖"。
/// 认不出来源的（没有元数据）不能重传。
pub async fn reupload<B: EditBackend>(
    leftover: &Leftover,
    backend: &B,
    overwrite: bool,
) -> Result<EditOutcome, String> {
    let meta = leftover
        .meta
        .as_ref()
        .ok_or_else(|| "这份修改认不出对应的服务器文件，只能另存到本地或放弃".to_owned())?;
    let mut session = EditSession::new(
        meta.remote_path.clone(),
        leftover.local_path.clone(),
        meta.base(),
    );
    let outcome = if overwrite {
        session.resolve(backend, Resolution::Overwrite).await
    } else {
        session.on_local_saved(backend).await
    };
    // 传上去的是哪份内容就记哪份；上传期间文件又变了，记录和本地不一致，仍然算有未上传的修改。
    if let (EditOutcome::Uploaded { .. }, Some(sha)) = (&outcome, session.synced_sha256()) {
        if leftover.from_draft {
            promote_draft(&leftover.dir, &meta.file_name, sha);
        }
        let _ = mark_clean(&leftover.dir, &session.remote_path, session.base(), sha);
    }
    Ok(outcome)
}

/// 草稿已经传上去 / 写回去了：把它升成「已对齐的内容」（写进 `content/`），再删掉草稿。
/// 只有草稿现在的摘要正好是刚传的那份才动（之后又变了就留着，仍算没上传）。
fn promote_draft(dir: &Path, file_name: &str, synced_sha256: &str) {
    let draft = draft_path(dir, file_name);
    let Ok(data) = std::fs::read(&draft) else {
        return;
    };
    if sha256_hex(&data) != synced_sha256 {
        return;
    }
    let target = content_path(dir, file_name);
    let tmp = content_dir(dir).join(format!(".promote-{}.tmp", std::process::id()));
    if std::fs::create_dir_all(content_dir(dir)).is_ok()
        && std::fs::write(&tmp, &data).is_ok()
        && crate::sftp::local::replace_file(&tmp, &target).is_ok()
    {
        let _ = std::fs::remove_file(&draft);
    }
}

/// 写回本地文件的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRestore {
    /// 写回去了。
    Written { size: u64 },
    /// 本地文件在这期间被别的程序改过（或者不见了），没有写。
    Conflict,
}

/// 内置编辑器打开的是**本地**文件时的恢复：把留下的内容写回原文件。
///
/// 打开时是符号链接的话，记录里带着真实目标（`link_target`）：写的是真实目标，链接本身不动。
///
/// `force = false` 时先看原文件现在是不是还和打开 / 上次保存时一样（摘要对得上），不一样返回
/// [`LocalRestore::Conflict`]、什么都不写；`force = true` 是用户看过冲突之后选了覆盖。写入先写同目录的
/// 临时文件，再原子替换。
pub fn restore_local(leftover: &Leftover, force: bool) -> Result<LocalRestore, String> {
    let meta = leftover
        .meta
        .as_ref()
        .filter(|meta| meta.local)
        .ok_or_else(|| "这不是本地文件的编辑记录".to_owned())?;
    let target: PathBuf = meta
        .link_target
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&meta.remote_path));
    let target = target.as_path();
    let data = std::fs::read(&leftover.local_path).map_err(|err| {
        format!(
            "读不了留下的内容（{}）：{err}",
            leftover.local_path.display()
        )
    })?;
    if !force {
        match std::fs::read(crate::sftp::local::long_path(target)) {
            Ok(current) if sha256_hex(&current) == meta.clean_sha256 => {}
            Ok(_) => return Ok(LocalRestore::Conflict),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LocalRestore::Conflict)
            }
            Err(err) => return Err(format!("读不了原文件 {}：{err}", target.display())),
        }
    }
    crate::sftp::local::write_atomic(target, &data)
        .map_err(|err| format!("写回 {} 失败：{err}", target.display()))?;
    let sha = sha256_hex(&data);
    if leftover.from_draft {
        promote_draft(&leftover.dir, &meta.file_name, &sha);
    }
    let size = data.len() as u64;
    let snapshot = std::fs::metadata(crate::sftp::local::long_path(target)).ok();
    let mut updated = meta.clone();
    updated.clean_sha256 = sha;
    updated.base_size = size;
    updated.base_mtime = snapshot
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| u32::try_from(duration.as_secs()).ok());
    let _ = write_meta(&leftover.dir, &updated);
    Ok(LocalRestore::Written { size })
}

/// 把留下的文件另存到 `dest_dir`（重名就在名字后面加 `(2)`、`(3)`…），返回最终路径。
pub fn save_copy(leftover: &Leftover, dest_dir: &Path) -> std::io::Result<PathBuf> {
    let name = leftover
        .local_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recovered".to_owned());
    std::fs::create_dir_all(dest_dir)?;
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_owned(), name[dot..].to_owned()),
        _ => (name.clone(), String::new()),
    };
    for attempt in 1..1000 {
        let candidate = if attempt == 1 {
            dest_dir.join(&name)
        } else {
            dest_dir.join(format!("{stem} ({attempt}){ext}"))
        };
        // `create_new`：绝不覆盖用户已有的文件。
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write;
                let data = std::fs::read(&leftover.local_path)?;
                file.write_all(&data)?;
                file.sync_all()?;
                return Ok(candidate);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("同名文件太多，换个文件夹"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_for(name: &str, content: &[u8]) -> EditMeta {
        EditMeta {
            remote_path: format!("/srv/{name}"),
            host_id: 7,
            host_label: "测试主机".to_owned(),
            file_name: name.to_owned(),
            link_target: None,
            base_size: content.len() as u64,
            base_mtime: Some(1_700_000_000),
            clean_sha256: sha256_hex(content),
            server_identity: None,
            local: false,
        }
    }

    /// 造一个编辑目录：下载下来的内容 + 元数据。
    fn open_edit(root: &Path, id: &str, name: &str, downloaded: &[u8]) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(content_dir(&dir)).unwrap();
        std::fs::write(content_path(&dir, name), downloaded).unwrap();
        write_meta(&dir, &meta_for(name, downloaded)).unwrap();
        dir
    }

    #[test]
    fn unsent_changes_survive_a_restart_and_are_reported() {
        let root = crate::test_util::TestDir::new("recovery-dirty");
        let dir = open_edit(&root, "e1", "app.conf", b"before");
        // 用户在编辑器里改了、保存了，程序在上传完成之前崩了。
        std::fs::write(content_path(&dir, "app.conf"), b"AFTER THE USER'S EDIT").unwrap();

        // 重启：扫描。
        let left = scan(&root);
        assert_eq!(left.len(), 1, "有未上传的修改必须留下来");
        assert_eq!(left[0].id, "e1");
        assert_eq!(
            std::fs::read(&left[0].local_path).unwrap(),
            b"AFTER THE USER'S EDIT"
        );
        let meta = left[0].meta.as_ref().expect("元数据在");
        assert_eq!(meta.remote_path, "/srv/app.conf");
        assert_eq!(meta.host_id, 7);
        assert!(dir.exists(), "目录不能被删");
    }

    #[test]
    fn untouched_and_already_uploaded_edits_are_cleaned_up() {
        let root = crate::test_util::TestDir::new("recovery-clean");
        open_edit(&root, "e1", "a.txt", b"same");
        // 改过但已经上传成功（mark_clean 之后）。
        let dir2 = open_edit(&root, "e2", "b.txt", b"old");
        std::fs::write(content_path(&dir2, "b.txt"), b"new").unwrap();
        mark_clean(
            &dir2,
            "/srv/b.txt",
            RemoteSnapshot {
                size: 3,
                mtime: Some(1_700_000_100),
            },
            &sha256_hex(b"new"),
        )
        .unwrap();

        assert!(scan(&root).is_empty());
        assert!(!root.join("e1").exists());
        assert!(!root.join("e2").exists());
    }

    #[test]
    fn a_directory_we_cannot_identify_is_kept_if_it_has_a_file() {
        let root = crate::test_util::TestDir::new("recovery-unknown");
        let dir = root.join("e5");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"maybe important").unwrap();
        std::fs::write(dir.join(META_FILE), b"{ this is not json").unwrap();
        let empty = root.join("e6");
        std::fs::create_dir_all(&empty).unwrap();

        let left = scan(&root);
        assert_eq!(left.len(), 1);
        assert!(left[0].meta.is_none());
        assert_eq!(
            std::fs::read(&left[0].local_path).unwrap(),
            b"maybe important"
        );
        assert!(!empty.exists(), "空目录可以清掉");
    }

    #[test]
    fn new_edits_are_numbered_after_the_leftovers() {
        let root = crate::test_util::TestDir::new("recovery-number");
        open_edit(&root, "e3", "a", b"x");
        open_edit(&root, "e12", "b", b"y");
        assert_eq!(max_edit_number(&root), 12);
    }

    #[test]
    fn save_copy_never_overwrites_an_existing_file() {
        let root = crate::test_util::TestDir::new("recovery-save");
        let dir = open_edit(&root, "e1", "app.conf", b"before");
        std::fs::write(content_path(&dir, "app.conf"), b"EDITED").unwrap();
        let left = scan(&root);
        let dest = root.join("saved");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("app.conf"), b"USERS OWN FILE").unwrap();

        let saved = save_copy(&left[0], &dest).unwrap();
        assert_eq!(saved, dest.join("app (2).conf"));
        assert_eq!(std::fs::read(&saved).unwrap(), b"EDITED");
        assert_eq!(
            std::fs::read(dest.join("app.conf")).unwrap(),
            b"USERS OWN FILE"
        );
    }

    #[test]
    fn mark_clean_records_the_given_digest_and_never_rereads_the_file() {
        let root = crate::test_util::TestDir::new("recovery-markclean");
        let dir = open_edit(&root, "e1", "a.txt", b"old");
        // 传上去的是 A，之后本地又变成了 B。
        std::fs::write(content_path(&dir, "a.txt"), b"B, saved later").unwrap();
        mark_clean(
            &dir,
            "/srv/a.txt",
            RemoteSnapshot {
                size: 1,
                mtime: Some(5),
            },
            &sha256_hex(b"A"),
        )
        .unwrap();
        let meta = read_meta(&dir).unwrap();
        assert_eq!(meta.clean_sha256, sha256_hex(b"A"));
        assert!(has_unsent_changes(&dir, &meta), "B 还没上传");
        assert_eq!(scan(&root).len(), 1, "重启后 B 必须留着");
    }

    #[test]
    fn draft_versions_decide_which_draft_is_deleted() {
        let root = crate::test_util::TestDir::new("recovery-draft-version");
        let dir = root.join("e1");
        write_draft(&dir, "a.txt", b"draft A", 1).unwrap();
        assert_eq!(read_draft_version(&dir, "a.txt"), Some(1));

        // 保存版本 0（比草稿旧）：草稿版本 1 更新，必须留着。
        assert!(!remove_draft_up_to(&dir, "a.txt", 0));
        assert!(draft_path(&dir, "a.txt").exists());

        // 保存版本 1：草稿版本不晚于它，删掉（内容和版本号一起）。
        assert!(remove_draft_up_to(&dir, "a.txt", 1));
        assert!(!draft_path(&dir, "a.txt").exists());
        assert!(!draft_version_path(&dir, "a.txt").exists());
    }

    #[test]
    fn a_draft_without_a_version_file_is_treated_as_the_oldest() {
        let root = crate::test_util::TestDir::new("recovery-draft-noversion");
        let dir = root.join("e1");
        std::fs::create_dir_all(draft_dir(&dir)).unwrap();
        // 旧版本程序留下的草稿：只有内容，没有版本号。
        std::fs::write(draft_path(&dir, "a.txt"), b"old draft").unwrap();
        assert_eq!(read_draft_version(&dir, "a.txt"), None);
        assert!(remove_draft_up_to(&dir, "a.txt", 0));
        assert!(!draft_path(&dir, "a.txt").exists());
    }

    /// R-8 第 5 节：枚举目录要分清"确认是空的"和"读不了"。
    #[test]
    fn directory_enumeration_tells_empty_apart_from_unreadable() {
        let root = crate::test_util::TestDir::new("recovery-userfiles");
        // 空目录：确认没有文件。
        let empty = root.join("e1");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(first_user_file(&empty), UserFiles::Empty);

        // 有文件：找到它。
        let with_file = open_edit(&root, "e2", "a.txt", b"x");
        assert_eq!(
            first_user_file(&with_file),
            UserFiles::Found(content_path(&with_file, "a.txt"))
        );

        // 读不了（这里用普通文件路径：三个候选位置的 read_dir 都会失败）：
        // 不能当成"确认是空的"。
        let not_a_dir = root.join("not-a-dir");
        std::fs::write(&not_a_dir, b"x").unwrap();
        assert_eq!(
            first_user_file(&not_a_dir),
            UserFiles::Unknown,
            "读不了目录不能当成空目录"
        );
    }

    /// R-8 第 5 节：启动扫描遇到"读不了"的目录要保留（并列出），不能当空目录删掉。
    #[cfg(windows)]
    #[test]
    fn scan_keeps_a_directory_it_cannot_enumerate() {
        let root = crate::test_util::TestDir::new("recovery-unreadable");
        let dir = root.join("e9");
        std::fs::create_dir_all(&dir).unwrap();

        // 只拒绝「列出目录内容」（RD），读属性仍然可用；真的生效才继续，造不出来就跳过。
        let deny = std::process::Command::new("icacls")
            .arg(&dir)
            .arg("/deny")
            .arg("*S-1-1-0:(RD)")
            .output();
        let restore = || {
            let _ = std::process::Command::new("icacls")
                .arg(&dir)
                .arg("/remove:d")
                .arg("*S-1-1-0")
                .output();
        };
        let constructed = deny.is_ok() && dir.is_dir() && std::fs::read_dir(&dir).is_err();
        if !constructed {
            restore();
            eprintln!("跳过：这个环境造不出「读不了目录」（icacls deny RD 没生效）");
            return;
        }

        let left = scan(&root);
        restore();
        assert_eq!(left.len(), 1, "读不了的目录要保留并列出来：{left:?}");
        assert_eq!(left[0].id, "e9");
        assert!(left[0].uncertain);
        assert!(dir.exists(), "读不了的目录不能被删掉");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
