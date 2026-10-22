//! 内置编辑器的后端（M3-5 第 2 节）。
//!
//! 远端文件走完整的一套编辑流程：`eN/content/<文件名>` 放下载下来的内容、`eN/state/meta.json` 放恢复记录、
//! `EditSession` 做冲突检测、临时文件 + 改名、属主处理（N05）、实际上传字节的摘要（N02）、同一路径的写锁（R01）。
//! 界面按 Ctrl+S 把文本交给后端立刻保存，所以冲突对话框、属主询问、上传失败提示、断线后重试都是同一套代码、
//! 同一组事件。
//!
//! 文本一律按 UTF-8 处理（见 `zssh_core::sftp::edit::text`）：不是 UTF-8、含 NUL、换行混用、
//! 超过大小上限的不打开；BOM 和换行风格记在条目里，保存时原样还原。
//!
//! 未保存的内容每隔几秒存一份草稿（`eN/draft/<文件名>`），恢复记录把草稿算作「没上传的修改」。
//! 本地文件同样可以用内置编辑器打开：保存是「先写同目录的临时文件，再原子替换」，保存前检查原文件
//! 有没有被别的程序改过。本地文件是符号链接时：打开时解析出真实目标，读 / 写都落在真实目标上
//! （链接本身不动），保存前核对链接没有被换成指向别处。

use serde::Serialize;
use zssh_core::sftp::edit::recovery::{self, EditMeta};
use zssh_core::sftp::edit::text::{self, TextFormat};
use zssh_core::sftp::edit::{
    download, sanitize_local_name, EditBackend, EditOutcome, EditSession, OpenError, MAX_EDIT_BYTES,
};

use super::{
    emit_edit, lock_std, remove_dir_quiet, remove_unless_unsent, report_outcome, Arc, AtomicBool,
    EditEntry, EditHost, EditRegistry, EditSink, Extra, Mutex, Ordering, Path, PathBuf,
    SessionView, StdMutex,
};

/// 本地文件「链接」的解析：生产里是文件系统；Windows 上普通用户建不了符号链接，
/// 测试里可以换成假的链接表来测「保存写真实目标、链接被改了就拒绝」这条逻辑。
pub(super) trait LocalLinks: Send + Sync {
    /// 这个路径是符号链接的话返回真实目标；普通文件返回 `None`。
    fn target_of(&self, path: &Path) -> std::io::Result<Option<PathBuf>>;
}

/// 生产实现：`symlink_metadata` 判断 + `canonicalize` 解析真实目标。
pub(super) struct SystemLinks;

impl LocalLinks for SystemLinks {
    fn target_of(&self, path: &Path) -> std::io::Result<Option<PathBuf>> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.file_type().is_symlink() {
            return Ok(None);
        }
        Ok(Some(std::fs::canonicalize(path)?))
    }
}

/// 打开 / 保存时实际要读写的路径：链接写真实目标（`link_target`），普通文件写自己。
fn local_target(
    links: &dyn LocalLinks,
    path: &Path,
) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    match links.target_of(path)? {
        Some(target) => Ok((target.clone(), Some(target))),
        None => Ok((path.to_path_buf(), None)),
    }
}

/// 内置编辑器条目独有的状态。
pub(super) struct BuiltinState {
    /// 打开时文件的样子（BOM、换行）；保存时还原，放弃本地修改重新拉远端后会更新。
    pub format: StdMutex<TextFormat>,
    /// `content/` 里的文件名（草稿用同一个名字）。
    pub file_name: String,
    /// 冲突 / 属主询问挂起的那次保存的内容版本。对话框里处理成功（上传或放弃）时按它删草稿：
    /// 不能全删（处理期间又打的字在更晚的草稿里），也不能不删（重启后旧草稿会被当成没保存的修改）。
    pub pending_version: StdMutex<Option<u64>>,
    /// 「放弃本地修改」之后 `content/` 已经换成服务器上的内容，但编辑器还没成功重新载入：
    /// 这时编辑器里还是旧文本，保存或存草稿都会把旧文本当成新基准上的修改写回去，必须拒绝。
    pub needs_reload: AtomicBool,
}

impl BuiltinState {
    pub(super) fn new(format: TextFormat, file_name: String) -> Self {
        Self {
            format: StdMutex::new(format),
            file_name,
            pending_version: StdMutex::new(None),
            needs_reload: AtomicBool::new(false),
        }
    }

    pub(super) fn take_pending_version(&self) -> Option<u64> {
        lock_std(&self.pending_version).take()
    }
}

/// 放弃修改后还没成功重新载入时，保存 / 存草稿给的错误。
fn needs_reload_error() -> EditorError {
    EditorError::new(
        "needs_reload",
        "已经放弃本地修改并换回了服务器上的内容，但编辑器没能重新载入它；编辑器里的文字已经过时，\
         不能保存。请关掉这个标签后重新打开文件",
    )
}

/// 当前会话连到的服务器和打开编辑器时不是同一台。
fn server_changed_error() -> EditorError {
    EditorError::new(
        "server_changed",
        "当前的 SFTP 会话连到的服务器和打开这个文件时不一致（主机的地址或用户名改过？），没有保存。\
         编辑内容已经保留；确认后请关掉这个标签，到正确的服务器上重新打开文件",
    )
}

/// 内置编辑器打开的**本地**文件。
pub(super) struct LocalEditorEntry {
    pub dir: PathBuf,
    /// 用户打开的路径（符号链接时是链接路径本身，标签上显示它）。
    pub path: PathBuf,
    pub file_name: String,
    /// 符号链接的真实目标（打开时解析出来的）；保存写这里，链接本身不动。
    pub link_target: StdMutex<Option<PathBuf>>,
    pub format: StdMutex<TextFormat>,
    /// 保存互斥：连按两次 Ctrl+S 不会同时写。
    pub gate: Mutex<()>,
}

/// 打开内置编辑器的结果（也是界面开新标签用的全部信息）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditorOpenDto {
    pub edit_id: u64,
    /// `remote` 或 `local`。
    pub side: &'static str,
    pub text: String,
    pub bom: bool,
    /// `LF` 或 `CRLF`。
    pub eol: &'static str,
    pub file_name: String,
    /// 远端路径，或本地路径。
    pub path: String,
    /// 主机库里的主机 id；本地文件是 0。
    pub host_id: i64,
    pub host_label: String,
    pub size: u64,
    /// 本地文件是符号链接时，打开时解析出来的真实目标（界面悬停显示「链接 → 目标」）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

/// 放弃本地修改后重新读回的内容。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditorTextDto {
    pub text: String,
    pub bom: bool,
    pub eol: &'static str,
}

/// 保存的结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditorSaveDto {
    /// `uploaded`（已保存 / 已上传）、`conflict`（远端或本地文件在打开后被改过，没有写）、
    /// `owner_prompt`（保存后属主会改变，等用户选）、`failed`。
    pub state: &'static str,
    pub message: Option<String>,
    /// 「另存为」实际写到的新路径（本地文件用：界面要把文档路径换过去）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 内置编辑器的错误：`code` 是稳定的代码，界面据此显示提示。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EditorError {
    pub code: &'static str,
    pub message: String,
}

impl EditorError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn eol_label(format: &TextFormat) -> &'static str {
    format.eol.label()
}

fn open_error(err: OpenError) -> EditorError {
    match err {
        OpenError::TooLarge { size, limit } => EditorError::new(
            "too_large",
            format!("文件有 {size} 字节，超过内置编辑器的上限（{limit} 字节）。请下载后再看"),
        ),
        OpenError::NotAFile => EditorError::new("not_a_file", "这不是普通文件，不能编辑"),
        OpenError::Remote(err) => EditorError::new("remote", err.to_string()),
    }
}

fn refusal(refusal: text::Refusal) -> EditorError {
    EditorError::new(refusal.code(), refusal.message())
}

impl EditRegistry {
    /// 同一台主机上同一个远端路径的编辑（同一个文件只允许打开一次）。
    pub(super) fn find_by_path(&self, host_id: i64, path: &str) -> Option<Arc<EditEntry>> {
        lock_std(&self.entries).values().find_map(|entry| {
            let session = entry.session.try_lock().ok()?;
            (entry.host_id == host_id && session.remote_path == path).then(|| Arc::clone(entry))
        })
    }

    fn get_local(&self, edit_id: u64) -> Option<Arc<LocalEditorEntry>> {
        lock_std(&self.locals).get(&edit_id).cloned()
    }

    fn local_by_path(&self, path: &Path) -> Option<u64> {
        lock_std(&self.locals).iter().find_map(|(id, entry)| {
            let same_file = zssh_core::pathnorm::same_path(&entry.path, path)
                || lock_std(&entry.link_target)
                    .as_ref()
                    .is_some_and(|target| zssh_core::pathnorm::same_path(target, path));
            same_file.then_some(*id)
        })
    }
}

// ---------------------------------------------------------------------------
// 远端文件
// ---------------------------------------------------------------------------

/// 用内置编辑器打开一个远端文件：下载 → 解码 → 写进 `eN/content/` → 写恢复记录 → 登记。
pub async fn open_builtin<B>(
    registry: &Arc<EditRegistry>,
    backend: Arc<B>,
    sink: &dyn EditSink,
    sftp_id: &str,
    host: EditHost,
    remote_path: &str,
) -> Result<EditorOpenDto, EditorError>
where
    B: EditBackend + 'static,
{
    if registry.find_by_path(host.host_id, remote_path).is_some() {
        return Err(EditorError::new(
            "already_open",
            "这个文件已经在内置编辑器里打开了",
        ));
    }

    let (data, base) = download(backend.as_ref(), remote_path, MAX_EDIT_BYTES)
        .await
        .map_err(open_error)?;
    let decoded = text::decode(&data).map_err(refusal)?;

    let edit_id = registry.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    let dir = registry.root.join(format!("e{edit_id}"));
    std::fs::create_dir_all(recovery::content_dir(&dir))
        .map_err(|err| EditorError::new("local_write", format!("建不了临时目录：{err}")))?;
    let file_name = sanitize_local_name(remote_path.rsplit('/').next().unwrap_or("file"));
    let content = recovery::content_dir(&dir).join(&file_name);
    if let Err(err) = std::fs::write(&content, &data) {
        remove_dir_quiet(&dir);
        return Err(EditorError::new(
            "local_write",
            format!("写不了临时文件：{err}"),
        ));
    }

    let record = recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: remote_path.to_owned(),
            host_id: host.host_id,
            host_label: host.host_label.clone(),
            file_name: file_name.clone(),
            link_target: None,
            base_size: base.size,
            base_mtime: base.mtime,
            clean_sha256: recovery::sha256_hex(&data),
            server_identity: Some(host.server_identity.clone()),
            local: false,
        },
    );

    let entry = Arc::new(EditEntry {
        sftp_id: sftp_id.to_owned(),
        server_identity: host.server_identity.clone(),
        dir: dir.clone(),
        session: Mutex::new(EditSession::new(remote_path.to_owned(), content, base)),
        dirty: AtomicBool::new(false),
        host_id: host.host_id,
        builtin: Some(BuiltinState::new(decoded.format, file_name.clone())),
    });
    registry.insert(edit_id, Arc::clone(&entry));

    if let Err(err) = record {
        // 编辑照常进行，但要让用户知道：这次的恢复记录没写成，程序崩溃后可能找不回这次修改。
        emit_edit(
            sink,
            &entry,
            edit_id,
            remote_path,
            "warning",
            Extra {
                message: Some(format!(
                    "没能写下恢复记录（{err}）：编辑可以照常进行，但如果程序在保存之前崩溃，崩溃后可能无法恢复这次修改。建议改完马上保存。"
                )),
                ..Extra::default()
            },
        );
    }

    Ok(EditorOpenDto {
        edit_id,
        side: "remote",
        eol: eol_label(&decoded.format),
        bom: decoded.format.bom,
        text: decoded.text,
        file_name,
        path: remote_path.to_owned(),
        link_target: None,
        host_id: host.host_id,
        host_label: host.host_label,
        size: data.len() as u64,
    })
}

fn builtin_entry(registry: &EditRegistry, edit_id: u64) -> Result<Arc<EditEntry>, EditorError> {
    registry
        .get(edit_id)
        .filter(|entry| entry.builtin.is_some())
        .ok_or_else(|| EditorError::new("closed", "这个编辑已经关闭"))
}

/// 保存（Ctrl+S）：把文本编码回文件内容，写进 `content/`，然后走编辑保存的那一整套
/// （对远端 → 冲突检测 → 临时文件 + 改名 → 属主处理 → 恢复记录）。
///
/// `version` 是这次保存的内容版本（编辑器文档里的单调递增计数）：保存成功后删掉版本不晚于它的草稿，
/// 保存期间用户又打了字（版本更晚的草稿）就留着，下次还能恢复出来。
pub async fn save_builtin<B: EditBackend>(
    registry: &Arc<EditRegistry>,
    backend: &B,
    sink: &dyn EditSink,
    edit_id: u64,
    text: &str,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    let entry = builtin_entry(registry, edit_id)?;
    let builtin = entry.builtin.as_ref().expect("builtin_entry 已经检查过");
    // 写到哪台服务器只认打开时记下的身份，不认前端挑的会话：主机记录的地址 / 用户名改过之后，
    // 同一个 host_id 可能已经连到另一台服务器（R-9 Q01）。
    if let Some(current) = backend.server_identity() {
        if current != entry.server_identity {
            return Err(server_changed_error());
        }
    }

    // 和「处理冲突」「放弃修改」互斥：同一时间只有一个在动这个编辑。
    let mut session = entry.session.lock().await;
    // 放在会话锁里检查：放弃修改是拿着同一把锁换掉 content/ 的。
    if builtin.needs_reload.load(Ordering::SeqCst) {
        return Err(needs_reload_error());
    }
    let format = *lock_std(&builtin.format);
    let bytes = text::encode(text, &format);
    let content = recovery::content_path(&entry.dir, &builtin.file_name);
    zssh_core::sftp::local::write_atomic(&content, &bytes)
        .map_err(|err| EditorError::new("local_write", format!("写不了临时文件：{err}")))?;
    let remote_path = session.remote_path.clone();
    entry.dirty.store(true, Ordering::SeqCst);
    emit_edit(
        sink,
        &entry,
        edit_id,
        &remote_path,
        "uploading",
        Extra::default(),
    );
    let outcome = session.on_local_saved(backend).await;
    let view = SessionView::of(&session);
    // 挂起等用户在对话框里处理的保存：记下版本，处理成功时按它删草稿（R-9 T03）。
    // 其它结果说明没有挂起的处理了，旧记录作废。
    *lock_std(&builtin.pending_version) = match outcome {
        EditOutcome::Conflict { .. } | EditOutcome::OwnerWouldChange(_) => Some(version),
        _ => None,
    };
    drop(session);

    // 保存成功：删掉版本不晚于这次保存的草稿；保存期间又打了新字（草稿版本更晚）就留着。
    if matches!(outcome, EditOutcome::Uploaded { .. }) {
        recovery::remove_draft_up_to(&entry.dir, &builtin.file_name, version);
    }

    let dto = match &outcome {
        EditOutcome::Uploaded { .. } => EditorSaveDto {
            state: "uploaded",
            message: None,
            path: None,
        },
        EditOutcome::Conflict { remote } => EditorSaveDto {
            state: "conflict",
            message: Some(if remote.is_some() {
                "服务器上的文件在你打开之后被改过，没有保存".to_owned()
            } else {
                "服务器上的文件已经被删掉了，没有保存".to_owned()
            }),
            path: None,
        },
        EditOutcome::OwnerWouldChange(change) => EditorSaveDto {
            state: "owner_prompt",
            message: Some(format!("保存后属主会从 {} 变成 {}", change.from, change.to)),
            path: None,
        },
        EditOutcome::Failed(reason) => EditorSaveDto {
            state: "failed",
            message: Some(reason.clone()),
            path: None,
        },
        EditOutcome::Discarded => EditorSaveDto {
            state: "uploaded",
            message: None,
            path: None,
        },
    };
    report_outcome(sink, &entry, edit_id, view, outcome);
    Ok(dto)
}

/// 存草稿：每隔几秒把没保存的内容存一份。草稿和已对齐的内容一样（改回去了）就删掉草稿。
pub fn draft_for(
    registry: &EditRegistry,
    edit_id: u64,
    text: &str,
    version: u64,
) -> Result<(), EditorError> {
    let (dir, file_name, format) =
        if let Some(entry) = registry.get(edit_id).filter(|e| e.builtin.is_some()) {
            let builtin = entry.builtin.as_ref().expect("已经检查过");
            // 编辑器里还是放弃之前的旧文本：存成草稿会在重启后把它当成没保存的修改。
            if builtin.needs_reload.load(Ordering::SeqCst) {
                return Err(needs_reload_error());
            }
            (
                entry.dir.clone(),
                builtin.file_name.clone(),
                *lock_std(&builtin.format),
            )
        } else if let Some(local) = registry.get_local(edit_id) {
            (
                local.dir.clone(),
                local.file_name.clone(),
                *lock_std(&local.format),
            )
        } else {
            return Err(EditorError::new("closed", "这个编辑已经关闭"));
        };
    let bytes = text::encode(text, &format);
    let synced = recovery::read_meta(&dir).map(|meta| meta.clean_sha256);
    if synced.as_deref() == Some(recovery::sha256_hex(&bytes).as_str()) {
        recovery::remove_draft(&dir, &file_name);
        return Ok(());
    }
    recovery::write_draft(&dir, &file_name, &bytes, version)
        .map_err(|err| EditorError::new("local_write", format!("存草稿失败：{err}")))
}

/// 放弃本地修改（冲突对话框里选了「放弃我的修改」）后，`content/` 已经换回远端的内容：重新读出来。
pub fn reload_builtin(registry: &EditRegistry, edit_id: u64) -> Result<EditorTextDto, EditorError> {
    let entry = builtin_entry(registry, edit_id)?;
    let builtin = entry.builtin.as_ref().expect("builtin_entry 已经检查过");
    let data = std::fs::read(recovery::content_path(&entry.dir, &builtin.file_name))
        .map_err(|err| EditorError::new("local_read", format!("读不了临时文件：{err}")))?;
    // 解不出来就保持 needs_reload：编辑器里的旧文本不能再保存（R-9 T02）。
    let decoded = text::decode(&data).map_err(refusal)?;
    *lock_std(&builtin.format) = decoded.format;
    // 内容已经换回远端的了，旧的草稿不再是「没保存的修改」。
    recovery::remove_draft(&entry.dir, &builtin.file_name);
    builtin.needs_reload.store(false, Ordering::SeqCst);
    Ok(EditorTextDto {
        eol: eol_label(&decoded.format),
        bom: decoded.format.bom,
        text: decoded.text,
    })
}

/// 放弃本地修改后重新读回内容：远端走 [`reload_builtin`]，本地文件读磁盘上现在的内容。
pub fn reload_editor(registry: &EditRegistry, edit_id: u64) -> Result<EditorTextDto, EditorError> {
    if registry
        .get(edit_id)
        .is_some_and(|entry| entry.builtin.is_some())
    {
        reload_builtin(registry, edit_id)
    } else {
        reload_local(registry, edit_id)
    }
}

/// 放弃本地文件的修改：以磁盘上现在的内容为准（内容和编码 / 换行一起换回来），并把它当成新的
/// 「已对齐」基准——之后再保存不会又报冲突。符号链接读的是它现在指向的真实目标。
pub fn reload_local(registry: &EditRegistry, edit_id: u64) -> Result<EditorTextDto, EditorError> {
    let local = registry
        .get_local(edit_id)
        .ok_or_else(|| EditorError::new("closed", "这个编辑已经关闭"))?;
    let long = zssh_core::sftp::local::long_path(&local.path);
    let (target, current_link) = local_target(&SystemLinks, &long).map_err(|err| {
        EditorError::new(
            "local_read",
            format!("读不了 {}：{err}", local.path.display()),
        )
    })?;
    let target_long = zssh_core::sftp::local::long_path(&target);
    let data = std::fs::read(&target_long).map_err(|err| {
        EditorError::new(
            "local_read",
            format!("读不了 {}：{err}", local.path.display()),
        )
    })?;
    let decoded = text::decode(&data).map_err(refusal)?;
    *lock_std(&local.format) = decoded.format;
    *lock_std(&local.link_target) = current_link.clone();
    if let Some(mut meta) = recovery::read_meta(&local.dir) {
        meta.clean_sha256 = recovery::sha256_hex(&data);
        meta.base_size = data.len() as u64;
        meta.base_mtime = mtime_secs(&target_long);
        meta.link_target = current_link
            .as_ref()
            .map(|target| target.display().to_string());
        let _ = recovery::write_meta(&local.dir, &meta);
    }
    let snapshot = recovery::content_path(&local.dir, &local.file_name);
    let _ = zssh_core::sftp::local::write_atomic(&snapshot, &data);
    // 内容已经换回磁盘上的了，旧的草稿不再是「没保存的修改」。
    recovery::remove_draft(&local.dir, &local.file_name);
    Ok(EditorTextDto {
        eol: eol_label(&decoded.format),
        bom: decoded.format.bom,
        text: decoded.text,
    })
}

/// 关闭编辑器标签。`discard = true` 是用户明确选了「不保存」：连草稿一起删；
/// 否则只在确认没有未上传的修改时才删目录（有草稿 / 保存失败留下的内容就留着，下次启动会提示）。
pub fn close_editor(registry: &EditRegistry, edit_id: u64, discard: bool) {
    if registry
        .get(edit_id)
        .is_some_and(|entry| entry.builtin.is_some())
    {
        if let Some(entry) = registry.remove(edit_id) {
            if discard {
                remove_dir_quiet(&entry.dir);
            } else {
                remove_unless_unsent(&entry.dir);
            }
        }
        return;
    }
    let removed = lock_std(&registry.locals).remove(&edit_id);
    if let Some(local) = removed {
        if discard {
            remove_dir_quiet(&local.dir);
        } else {
            remove_unless_unsent(&local.dir);
        }
    }
}

// ---------------------------------------------------------------------------
// 本地文件
// ---------------------------------------------------------------------------

/// 用内置编辑器打开一个本地文件。符号链接打开的是它指向的真实目标（保存也写目标，链接本身不动）。
pub fn open_local(registry: &Arc<EditRegistry>, path: &Path) -> Result<EditorOpenDto, EditorError> {
    open_local_with(registry, path, &SystemLinks)
}

/// [`open_local`] 的实现；`links` 让测试在不能创建符号链接的机器上也能测链接逻辑。
pub(super) fn open_local_with(
    registry: &Arc<EditRegistry>,
    path: &Path,
    links: &dyn LocalLinks,
) -> Result<EditorOpenDto, EditorError> {
    let long = zssh_core::sftp::local::long_path(path);
    let (target, link_target) = local_target(links, &long).map_err(|err| {
        EditorError::new("local_read", format!("打不开 {}：{err}", path.display()))
    })?;
    let target_long = zssh_core::sftp::local::long_path(&target);
    let meta = std::fs::metadata(&target_long).map_err(|err| {
        EditorError::new("local_read", format!("打不开 {}：{err}", path.display()))
    })?;
    if !meta.is_file() {
        return Err(EditorError::new(
            "not_a_file",
            "只能编辑文件，不能编辑文件夹",
        ));
    }
    if meta.len() > MAX_EDIT_BYTES {
        return Err(open_error(OpenError::TooLarge {
            size: meta.len(),
            limit: MAX_EDIT_BYTES,
        }));
    }
    if registry.local_by_path(path).is_some()
        || link_target
            .as_ref()
            .is_some_and(|target| registry.local_by_path(target).is_some())
    {
        return Err(EditorError::new(
            "already_open",
            "这个文件已经在内置编辑器里打开了",
        ));
    }
    let data = std::fs::read(&target_long).map_err(|err| {
        EditorError::new("local_read", format!("读不了 {}：{err}", path.display()))
    })?;
    let decoded = text::decode(&data).map_err(refusal)?;

    let edit_id = registry.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    let dir = registry.root.join(format!("e{edit_id}"));
    let file_name = sanitize_local_name(
        &path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned()),
    );
    std::fs::create_dir_all(recovery::content_dir(&dir))
        .map_err(|err| EditorError::new("local_write", format!("建不了临时目录：{err}")))?;
    let snapshot = recovery::content_dir(&dir).join(&file_name);
    std::fs::write(&snapshot, &data)
        .map_err(|err| EditorError::new("local_write", format!("写不了临时文件：{err}")))?;
    // 恢复记录写不成不拦着打开（草稿功能会受影响，下次保存时再提示不了就算了）。
    let _ = recovery::write_meta(
        &dir,
        &EditMeta {
            remote_path: path.display().to_string(),
            host_id: 0,
            host_label: "本机".to_owned(),
            file_name: file_name.clone(),
            link_target: link_target
                .as_ref()
                .map(|target| target.display().to_string()),
            base_size: data.len() as u64,
            base_mtime: mtime_secs(&target_long),
            clean_sha256: recovery::sha256_hex(&data),
            server_identity: None,
            local: true,
        },
    );
    lock_std(&registry.locals).insert(
        edit_id,
        Arc::new(LocalEditorEntry {
            dir,
            path: path.to_path_buf(),
            file_name: file_name.clone(),
            link_target: StdMutex::new(link_target.clone()),
            format: StdMutex::new(decoded.format),
            gate: Mutex::new(()),
        }),
    );
    Ok(EditorOpenDto {
        edit_id,
        side: "local",
        eol: eol_label(&decoded.format),
        bom: decoded.format.bom,
        text: decoded.text,
        file_name,
        path: path.display().to_string(),
        link_target: link_target
            .as_ref()
            .map(|target| target.display().to_string()),
        host_id: 0,
        host_label: "本机".to_owned(),
        size: data.len() as u64,
    })
}

fn mtime_secs(path: &Path) -> Option<u32> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u32::try_from(duration.as_secs()).ok())
}

/// 保存本地文件：先检查原文件有没有被别的程序改过（`force` 是用户看过冲突后选了覆盖），
/// 然后先写同目录的临时文件、再原子替换。`version` 同 [`save_builtin`]，用来删旧草稿。
///
/// 打开时是符号链接的话：保存前核对链接没有被换成指向别处（或已经不是链接了），写的是真实目标，
/// 链接本身不动。
pub async fn save_local(
    registry: &Arc<EditRegistry>,
    edit_id: u64,
    text: &str,
    force: bool,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    save_local_with(registry, edit_id, text, force, version, &SystemLinks).await
}

/// [`save_local`] 的实现；`links` 让测试在不能创建符号链接的机器上也能测链接逻辑。
pub(super) async fn save_local_with(
    registry: &Arc<EditRegistry>,
    edit_id: u64,
    text: &str,
    force: bool,
    version: u64,
    links: &dyn LocalLinks,
) -> Result<EditorSaveDto, EditorError> {
    let local = registry
        .get_local(edit_id)
        .ok_or_else(|| EditorError::new("closed", "这个编辑已经关闭"))?;
    let _gate = local.gate.lock().await;
    let format = *lock_std(&local.format);
    let bytes = text::encode(text, &format);
    let long = zssh_core::sftp::local::long_path(&local.path);
    let Some(mut meta) = recovery::read_meta(&local.dir) else {
        return Err(EditorError::new(
            "local_read",
            "这个编辑的记录丢了，不能保存；请把内容复制出来，关掉标签重新打开",
        ));
    };
    let (target, current_link) = match local_target(links, &long) {
        Ok(found) => found,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EditorSaveDto {
                state: "conflict",
                message: Some("这个文件已经被删除或移走了，没有保存".to_owned()),
                path: None,
            })
        }
        Err(err) => {
            return Ok(EditorSaveDto {
                state: "failed",
                message: Some(format!("读不了原文件，没有保存：{err}")),
                path: None,
            })
        }
    };
    // 打开时是链接：它现在必须还指向同一个真实目标，否则可能把内容写到用户没想到的文件上。
    let recorded_link = lock_std(&local.link_target).clone();
    if let Some(recorded) = recorded_link {
        let still_same = current_link
            .as_ref()
            .is_some_and(|now| zssh_core::pathnorm::same_path(&recorded, now));
        if !still_same {
            return Ok(EditorSaveDto {
                state: "failed",
                message: Some(
                    "这个文件的符号链接已经被改成指向别处（或者已经不是链接了），没有保存"
                        .to_owned(),
                ),
                path: None,
            });
        }
    }
    let target_long = zssh_core::sftp::local::long_path(&target);

    if !force {
        match std::fs::read(&target_long) {
            Ok(current) if recovery::sha256_hex(&current) == meta.clean_sha256 => {}
            Ok(_) => {
                return Ok(EditorSaveDto {
                    state: "conflict",
                    message: Some("这个文件在你打开之后被别的程序改过，没有保存".to_owned()),
                    path: None,
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(EditorSaveDto {
                    state: "conflict",
                    message: Some("这个文件已经被删除或移走了，没有保存".to_owned()),
                    path: None,
                })
            }
            Err(err) => {
                return Ok(EditorSaveDto {
                    state: "failed",
                    message: Some(format!("读不了原文件，没有保存：{err}")),
                    path: None,
                })
            }
        }
    }

    if let Err(err) = zssh_core::sftp::local::write_atomic(&target_long, &bytes) {
        return Ok(EditorSaveDto {
            state: "failed",
            message: Some(format!("保存失败，原文件没有改动：{err}")),
            path: None,
        });
    }
    *lock_std(&local.link_target) = current_link.clone();

    // 保存成功：当前内容成了新的「已对齐」基准。
    let sha = recovery::sha256_hex(&bytes);
    let snapshot = recovery::content_path(&local.dir, &local.file_name);
    let _ = zssh_core::sftp::local::write_atomic(&snapshot, &bytes);
    meta.clean_sha256 = sha.clone();
    meta.base_size = bytes.len() as u64;
    meta.base_mtime = mtime_secs(&target_long);
    meta.link_target = current_link
        .as_ref()
        .map(|target| target.display().to_string());
    let _ = recovery::write_meta(&local.dir, &meta);
    recovery::remove_draft_up_to(&local.dir, &local.file_name, version);
    Ok(EditorSaveDto {
        state: "uploaded",
        message: None,
        path: None,
    })
}

/// 「另存为」的新名字：不能为空、不能带路径分隔符、不能是 `.` / `..`。
/// 另存为的新名字：必须原样是一个合法的 Windows 文件名（`C:evil.txt` 这种带盘符的写法会让
/// `join` 丢掉所在目录，R-10 M1）。
fn validate_local_save_as_name(name: &str) -> Result<String, String> {
    zssh_core::sftp::local::check_child_name(name).map_err(|err| err.to_string())
}

/// 本地文件保存冲突时选「另存为」：把当前内容写到**同目录**下的新文件，之后这个编辑会话改跟踪
/// 新文件（界面同步文档路径和标签标题）。不会覆盖已经存在的文件。
pub async fn save_local_as(
    registry: &Arc<EditRegistry>,
    edit_id: u64,
    text: &str,
    name: &str,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    let local = registry
        .get_local(edit_id)
        .ok_or_else(|| EditorError::new("closed", "这个编辑已经关闭"))?;
    let _gate = local.gate.lock().await;
    let name = validate_local_save_as_name(name)
        .map_err(|message| EditorError::new("bad_name", message))?;
    let parent = local
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| EditorError::new("local_write", "不知道这个文件在哪个文件夹里"))?;
    let target = parent.join(&name);
    // 名字已经查过，再按拼出来的路径核对一次：结果必须还在原文件所在的目录里。
    zssh_core::sftp::local::ensure_inside(&parent, &target)
        .map_err(|err| EditorError::new("bad_name", err.to_string()))?;
    if std::fs::symlink_metadata(&target).is_ok() {
        return Ok(EditorSaveDto {
            state: "failed",
            message: Some(format!("「{name}」已经存在，换个名字")),
            path: None,
        });
    }
    let file_name = sanitize_local_name(&name);
    let format = *lock_std(&local.format);
    let bytes = text::encode(text, &format);
    if let Err(err) = zssh_core::sftp::local::write_atomic(&target, &bytes) {
        return Ok(EditorSaveDto {
            state: "failed",
            message: Some(format!("另存为失败，原文件没有改动：{err}")),
            path: None,
        });
    }
    let sha = recovery::sha256_hex(&bytes);
    let snapshot = recovery::content_path(&local.dir, &file_name);
    let _ = zssh_core::sftp::local::write_atomic(&snapshot, &bytes);
    if let Some(mut meta) = recovery::read_meta(&local.dir) {
        meta.remote_path = target.display().to_string();
        meta.file_name = file_name.clone();
        meta.clean_sha256 = sha;
        meta.link_target = None;
        meta.base_size = bytes.len() as u64;
        meta.base_mtime = mtime_secs(&target);
        let _ = recovery::write_meta(&local.dir, &meta);
    }
    recovery::remove_draft_up_to(&local.dir, &file_name, version);
    // 换掉登记：之后这个编辑的保存写新文件（旧文件不再被碰）。
    lock_std(&registry.locals).insert(
        edit_id,
        Arc::new(LocalEditorEntry {
            dir: local.dir.clone(),
            path: target.clone(),
            file_name,
            // 另存为写出的是普通文件，不再是链接。
            link_target: StdMutex::new(None),
            format: StdMutex::new(format),
            gate: Mutex::new(()),
        }),
    );
    Ok(EditorSaveDto {
        state: "uploaded",
        message: None,
        path: Some(target.display().to_string()),
    })
}

// ---------------------------------------------------------------------------
// Tauri 命令（只做转接：取标签、读设置、发事件；逻辑都在上面的函数里，有测试）
// ---------------------------------------------------------------------------

use tauri::{AppHandle, Manager, State};

use super::{pick_sftp_id_for_edit, AppSink, AppTabSource, EditTabSource, PickError};
use crate::commands::sftp::tab_of;
use crate::state::AppState;

/// 用内置编辑器打开远端文件。
#[tauri::command]
pub async fn editor_open(
    app: AppHandle,
    state: State<'_, AppState>,
    sftp_id: String,
    remote_path: String,
) -> Result<EditorOpenDto, EditorError> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    let tab = tab_of(&app, &sftp_id)
        .await
        .map_err(|message| EditorError::new("remote", message))?;
    let host_label = state
        .store
        .host_get(tab.host_id)
        .map(|host| host.name)
        .unwrap_or_default();
    open_builtin(
        &registry,
        Arc::clone(&tab.browse),
        &AppSink(app.clone()),
        &sftp_id,
        EditHost {
            host_id: tab.host_id,
            host_label,
            server_identity: tab.browse.server_key().to_owned(),
        },
        &remote_path,
    )
    .await
}

/// 保存。`sftp_id` 是这台主机**现在**可用的 SFTP 会话（原来的会话可能已经关了或者重连过）。
#[tauri::command]
pub async fn editor_save(
    app: AppHandle,
    sftp_id: String,
    edit_id: u64,
    text: String,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    let entry = builtin_entry(&registry, edit_id)?;
    // 前端按 hostId 挑的会话只当首选：和冲突处理一样，按打开时的主机 + 服务器身份找（R-9 Q01）。
    let sftp_id = pick_sftp_id_for_edit(
        &AppTabSource(&app).candidates(),
        entry.host_id,
        &entry.server_identity,
        &sftp_id,
    )
    .map_err(|err| match err {
        PickError::NotFound => EditorError::new(
            "no_session",
            "这台主机没有打开的 SFTP 会话。编辑内容已经保留，打开这台主机的 SFTP 后可以再保存",
        ),
        PickError::ServerChanged => server_changed_error(),
    })?;
    let tab = tab_of(&app, &sftp_id).await.map_err(|message| {
        EditorError::new(
            "no_session",
            format!("{message}。编辑内容已经保留，打开这台主机的 SFTP 后可以再保存"),
        )
    })?;
    // 挑选和取标签之间标签可能刚换过：save_builtin 还会按服务器身份再核对一次。
    if tab.host_id != entry.host_id {
        return Err(server_changed_error());
    }
    save_builtin(
        &registry,
        tab.browse.as_ref(),
        &AppSink(app.clone()),
        edit_id,
        &text,
        version,
    )
    .await
}

/// 存草稿（远端和本地文件的编辑器都用它）。`version` 是这份草稿对应的内容版本。
#[tauri::command]
pub fn editor_draft(
    app: AppHandle,
    edit_id: u64,
    text: String,
    version: u64,
) -> Result<(), EditorError> {
    let registry = app.state::<Arc<EditRegistry>>();
    draft_for(&registry, edit_id, &text, version)
}

/// 放弃本地修改后把内容重新读回来（远端和本地文件都走它）。
#[tauri::command]
pub fn editor_reload(app: AppHandle, edit_id: u64) -> Result<EditorTextDto, EditorError> {
    let registry = app.state::<Arc<EditRegistry>>();
    reload_editor(&registry, edit_id)
}

/// 关编辑器标签。`discard` 是用户选了「不保存」。
#[tauri::command]
pub fn editor_close(app: AppHandle, edit_id: u64, discard: bool) {
    let registry = app.state::<Arc<EditRegistry>>();
    close_editor(&registry, edit_id, discard);
}

/// 用内置编辑器打开本地文件。
#[tauri::command]
pub fn local_editor_open(app: AppHandle, path: String) -> Result<EditorOpenDto, EditorError> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    open_local(&registry, Path::new(&path))
}

/// 保存本地文件（`force`：用户看过「文件被改过」的提示后选了覆盖）。
#[tauri::command]
pub async fn local_editor_save(
    app: AppHandle,
    edit_id: u64,
    text: String,
    force: bool,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    save_local(&registry, edit_id, &text, force, version).await
}

/// 本地文件保存冲突时「另存为」：写到同目录的新文件，之后保存跟踪新文件。
#[tauri::command]
pub async fn local_editor_save_as(
    app: AppHandle,
    edit_id: u64,
    text: String,
    name: String,
    version: u64,
) -> Result<EditorSaveDto, EditorError> {
    let registry = Arc::clone(&app.state::<Arc<EditRegistry>>());
    save_local_as(&registry, edit_id, &text, &name, version).await
}
