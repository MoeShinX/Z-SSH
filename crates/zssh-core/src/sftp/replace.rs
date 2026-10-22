//! 服务器没有 `posix-rename@openssh.com` 时的"覆盖改名"（R-6c R02）。
//!
//! 普通的 SFTP rename 目标存在会失败，所以覆盖只能先把目标让开。旧实现是"先删目标、再改名"，
//! 改名失败时服务器上旧文件、新文件都没了。这里改成三步，**任何一步失败，服务器上都至少有一份
//! 完整的旧文件或新文件**：
//!
//! 1. 把原文件改名成同目录的备用名；
//! 2. 把新文件改名成目标——失败就把备用文件改回原名；
//! 3. 删掉备用文件（删不掉不算失败，只是多留一个备用文件）。
//!
//! 失败时要说清楚现在是什么状态（[`ReplaceState`]）：提交前失败原文件没动、原文件已经挪开但
//! 没能恢复（现在在备用名）、或者结果不确定（连接断了）。调用方据此决定能不能删临时文件。

use crate::sftp::error::{ReplaceState, SftpError};
use crate::ssh::BoxFuture;

/// 三步里用到的服务器操作（真实实现包一条 SFTP 通道，测试用内存实现）。
pub trait RenameOps: Send + Sync {
    /// 路径存在吗（不跟随链接）。
    fn exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, SftpError>>;
    /// 普通改名：**目标已存在必须失败**（和 `SSH_FXP_RENAME` 一样）。
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn remove<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
}

/// 备用名：`<目标>.zssh-old-<标识>`，标识每次覆盖都不同。
pub fn backup_path(target: &str, token: &str) -> String {
    format!("{target}.zssh-old-{token}")
}

fn failed(state: ReplaceState, message: String) -> SftpError {
    SftpError::ReplaceFailed { state, message }
}

fn lost_connection(err: &SftpError) -> bool {
    matches!(err, SftpError::Disconnected(_)) || err.is_disconnect()
}

/// 用 `from` 覆盖 `to`（`to` 不存在也行）。成功时 `from` 没了、`to` 是新内容、备用文件已删。
pub async fn replace_with_backup(
    ops: &dyn RenameOps,
    from: &str,
    to: &str,
    token: &str,
) -> Result<(), SftpError> {
    let exists = match ops.exists(to).await {
        Ok(exists) => exists,
        Err(err) => {
            let state = if lost_connection(&err) {
                ReplaceState::Uncertain
            } else {
                ReplaceState::Untouched
            };
            return Err(failed(state, format!("检查 {to} 是否存在时失败：{err}")));
        }
    };
    if !exists {
        // 目标不存在：一步改名就行，不需要备用文件。
        return ops.rename(from, to).await.map_err(|err| {
            let state = if lost_connection(&err) {
                ReplaceState::Uncertain
            } else {
                ReplaceState::Untouched
            };
            failed(state, format!("改名失败：{err}"))
        });
    }

    let backup = backup_path(to, token);

    // 1. 原文件让开。
    if let Err(err) = ops.rename(to, &backup).await {
        return Err(if lost_connection(&err) {
            failed(
                ReplaceState::Uncertain,
                format!("把原文件改名成备用名时连接出了问题：{err}（原文件可能还在 {to}，也可能已经在 {backup}）"),
            )
        } else {
            failed(
                ReplaceState::Untouched,
                format!("把原文件挪到备用名 {backup} 时失败，原文件没动：{err}"),
            )
        });
    }

    // 2. 新文件到位；失败就把原文件改回来。
    if let Err(err) = ops.rename(from, to).await {
        if lost_connection(&err) {
            return Err(failed(
                ReplaceState::Uncertain,
                format!(
                    "把新内容改名成 {to} 时连接出了问题：{err}（原文件在备用名 {backup}，新内容在 {from} 或者已经到了 {to}）"
                ),
            ));
        }
        return Err(match ops.rename(&backup, to).await {
            Ok(()) => failed(
                ReplaceState::Untouched,
                format!("把新内容改名成 {to} 失败，原文件已经恢复：{err}"),
            ),
            Err(restore) => failed(
                ReplaceState::OriginalAside {
                    backup: backup.clone(),
                },
                format!(
                    "把新内容改名成 {to} 失败：{err}；把原文件从 {backup} 改回来也失败了：{restore}"
                ),
            ),
        });
    }

    // 3. 备用文件用完了。删不掉只是多留一个文件，内容已经替换成功。
    if let Err(err) = ops.remove(&backup).await {
        tracing::warn!(%err, %backup, "覆盖改名成功，但没能删掉备用文件");
    }
    Ok(())
}
