//! 工具桥里的文件工具（M3-4）：`list_dir`、`read_file`、`write_file`、`stat`。
//!
//! 和 `run_command` 走同一套：能力密钥找到对话 → 目标必须在目标列表里 → 执行模式 →
//! 审计。差别：
//!
//! - 只读工具（`list_dir` / `read_file` / `stat`）手动模式下**不弹确认**，直接执行；
//! - `write_file` 手动模式要确认（卡片里带差异和危险标记），自动模式直接写；两种模式都
//!   会在服务器上先备份原文件，备份失败就不写；
//! - 审计只记工具名、主机、路径、字节数、危险标记、备份路径，**不记文件内容**。

use std::sync::Arc;

use serde_json::Value;

use super::{
    now_millis, random_hex, ApprovalRequest, AuditEntry, Bridge, ExecDecision, ExecGate,
    FileApproval, RequestTicket, Scope,
};
use crate::ai::files::{
    execute_write_checked, prepare_write, tool_list_dir, tool_read_file, tool_stat, RemoteFs,
};
use crate::ai::protocol::{
    PathParams, ReadFileParams, WriteFileParams, METHOD_LIST_DIR, METHOD_READ_FILE, METHOD_STAT,
    METHOD_WRITE_FILE,
};
use crate::ai::safety::DangerLevel;

/// 文件工具的目标：目标摘要、文件系统、请求票据、执行用的会话。
type FileTarget = (
    crate::ai::protocol::TargetSummary,
    Arc<dyn RemoteFs>,
    RequestTicket,
    Option<String>,
);

/// 一次文件工具调用要写进审计的内容（不含文件内容）。
struct FileAudit {
    tool: &'static str,
    path: String,
    danger: DangerLevel,
    decision: ExecDecision,
    ok: bool,
    bytes: Option<i64>,
    backup_path: Option<String>,
    duration_ms: i64,
}

impl Bridge {
    /// 四个文件工具的入口（`dispatch` 按方法名转进来）。
    pub(super) async fn file_tool(
        &self,
        scope: &Arc<Scope>,
        method: &str,
        params: Value,
        call_id: &str,
    ) -> Result<Value, String> {
        match method {
            METHOD_LIST_DIR => {
                let params: PathParams = parse(params)?;
                let (summary, fs, ticket, _) = self
                    .file_target(
                        scope,
                        params.target_id.as_deref(),
                        "list_dir",
                        &params.path,
                        call_id,
                    )
                    .await?;
                let started = std::time::Instant::now();
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "list_dir",
                    &params.path,
                    started,
                )?;
                let outcome = tool_list_dir(fs.as_ref(), &params.path).await;
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "list_dir",
                    &params.path,
                    started,
                )?;
                self.read_audit(
                    scope,
                    &summary.host_label,
                    "list_dir",
                    outcome.as_ref().map(|(_, path)| path.clone()).ok(),
                    &params.path,
                    outcome
                        .as_ref()
                        .ok()
                        .map(|(listing, _)| listing.entries.len() as i64),
                    outcome.is_ok(),
                    started,
                );
                let (listing, _) = outcome?;
                to_value(&listing)
            }
            METHOD_STAT => {
                let params: PathParams = parse(params)?;
                let (summary, fs, ticket, _) = self
                    .file_target(
                        scope,
                        params.target_id.as_deref(),
                        "stat",
                        &params.path,
                        call_id,
                    )
                    .await?;
                let started = std::time::Instant::now();
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "stat",
                    &params.path,
                    started,
                )?;
                let outcome = tool_stat(fs.as_ref(), &params.path).await;
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "stat",
                    &params.path,
                    started,
                )?;
                self.read_audit(
                    scope,
                    &summary.host_label,
                    "stat",
                    outcome.as_ref().map(|(_, path)| path.clone()).ok(),
                    &params.path,
                    None,
                    outcome.is_ok(),
                    started,
                );
                let (result, _) = outcome?;
                to_value(&result)
            }
            METHOD_READ_FILE => {
                let params: ReadFileParams = parse(params)?;
                let (summary, fs, ticket, _) = self
                    .file_target(
                        scope,
                        params.target_id.as_deref(),
                        "read_file",
                        &params.path,
                        call_id,
                    )
                    .await?;
                let started = std::time::Instant::now();
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "read_file",
                    &params.path,
                    started,
                )?;
                let outcome =
                    tool_read_file(fs.as_ref(), &params.path, params.offset, params.max_bytes)
                        .await;
                // 读完之后、把内容交给智能体之前再核对一次：读的过程中目标被移出，内容也不能返回
                self.ensure_still_valid(
                    scope,
                    &ticket,
                    &summary.host_label,
                    "read_file",
                    &params.path,
                    started,
                )?;
                self.read_audit(
                    scope,
                    &summary.host_label,
                    "read_file",
                    outcome.as_ref().map(|r| r.path.clone()).ok(),
                    &params.path,
                    outcome.as_ref().ok().map(|r| r.returned_bytes as i64),
                    outcome.is_ok(),
                    started,
                );
                to_value(&outcome?)
            }
            METHOD_WRITE_FILE => {
                let params: WriteFileParams = parse(params)?;
                self.write_file(scope, params, call_id).await
            }
            other => Err(format!("未知方法：{other}")),
        }
    }

    /// 解析目标并拿到它的文件系统（开 SFTP 通道）。目标不在目标列表里直接拒绝。
    ///
    /// 解析出的实际执行位置在开通道之前就回报给界面（`tool` / `key` 用来对到卡片上，AI-SCOPE-2）；
    /// 通道开在执行用的会话上（界面绑定的优先），第四项是这个会话 ID。
    async fn file_target(
        &self,
        scope: &Arc<Scope>,
        target_id: Option<&str>,
        tool: &str,
        key: &str,
        call_id: &str,
    ) -> Result<FileTarget, String> {
        let resolved = self
            .resolve_exec_target(scope, target_id, tool, key, call_id)
            .await?;
        // 票据是解析会话之前记下的（连接可能要等好一会儿）。
        let fs = self.host.remote_fs(&resolved.exec_id).await?;
        Ok((resolved.summary, fs, resolved.ticket, resolved.session_id))
    }

    /// 只读工具的有效性核对：目标被移出对话（哪怕又被加回来）、用户点了停止、作用范围销毁，
    /// 这条请求都不能再读 / 再返回内容。和写文件、执行命令用的是同一张 [`RequestTicket`]。
    /// 失效时记一条失败审计（不含内容），返回给模型的原因和写工具一致。
    fn ensure_still_valid(
        &self,
        scope: &Arc<Scope>,
        ticket: &RequestTicket,
        host_label: &str,
        tool: &'static str,
        requested: &str,
        started: std::time::Instant,
    ) -> Result<(), String> {
        match scope.ticket_refusal(ticket) {
            None => Ok(()),
            Some(reason) => {
                self.read_audit(
                    scope, host_label, tool, None, requested, None, false, started,
                );
                Err(reason.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn read_audit(
        &self,
        scope: &Arc<Scope>,
        host_label: &str,
        tool: &'static str,
        resolved: Option<String>,
        requested: &str,
        bytes: Option<i64>,
        ok: bool,
        started: std::time::Instant,
    ) {
        self.record_file_audit(
            scope,
            host_label,
            FileAudit {
                tool,
                path: resolved.unwrap_or_else(|| requested.to_owned()),
                danger: DangerLevel::Safe,
                decision: ExecDecision::Auto,
                ok,
                bytes,
                backup_path: None,
                duration_ms: started.elapsed().as_millis() as i64,
            },
        );
    }

    async fn write_file(
        &self,
        scope: &Arc<Scope>,
        params: WriteFileParams,
        call_id: &str,
    ) -> Result<Value, String> {
        // 之后目标被移出 / 用户点了停止，批准了也不能再写（R-9 F11、F12）。
        let (summary, fs, ticket, session_id) = self
            .file_target(
                scope,
                params.target_id.as_deref(),
                "write_file",
                &params.path,
                call_id,
            )
            .await?;
        let started = std::time::Instant::now();
        // 准备：读现有内容、算差异和危险标记。这一步不改服务器上的任何东西。
        let plan = prepare_write(
            fs.as_ref(),
            &params.path,
            &params.content,
            params.create_only,
        )
        .await?;
        let danger = if plan.danger.dangerous {
            DangerLevel::Dangerous
        } else {
            DangerLevel::Safe
        };

        // 手动模式要用户在确认卡片上看差异再点执行；自动模式直接写，危险只做标记。
        let (decision, refusal) = self
            .decide(
                scope,
                &ticket,
                ApprovalRequest {
                    scope: scope.id.clone(),
                    exec_id: call_id.to_owned(),
                    target_id: summary.id.clone(),
                    session_id: session_id.clone(),
                    host_label: summary.host_label.clone(),
                    command: format!("write_file {}", plan.target),
                    danger,
                    reasons: plan.danger.reasons.clone(),
                    file: Some(FileApproval {
                        path: plan.target.clone(),
                        is_new: plan.is_new(),
                        new_bytes: plan.new_content.len(),
                        diff: plan.diff.clone(),
                    }),
                },
            )
            .await;
        if let Some(message) = refusal {
            self.record_file_audit(
                scope,
                &summary.host_label,
                FileAudit {
                    tool: "write_file",
                    path: plan.target.clone(),
                    danger,
                    decision,
                    ok: false,
                    bytes: None,
                    backup_path: None,
                    duration_ms: started.elapsed().as_millis() as i64,
                },
            );
            return Err(format!("{message}，文件没有改动"));
        }

        // 批准之后的读取、备份可能要等一阵：每个还没改动文件的位置都再核对一次，点了停止 /
        // 移出了目标就停下（R-9 F12）。替换一旦开始就做完，不在半路丢下。
        let gate = ExecGate::new(scope, &ticket);
        let checkpoint = || gate.check();
        let result = execute_write_checked(
            fs.as_ref(),
            &plan,
            params.create_only,
            now_millis() / 1000,
            &random_hex(4),
            self.host.write_backup_keep(),
            &checkpoint,
        )
        .await;
        self.record_file_audit(
            scope,
            &summary.host_label,
            FileAudit {
                tool: "write_file",
                path: plan.target.clone(),
                danger,
                decision,
                ok: result.is_ok(),
                bytes: result.as_ref().ok().map(|r| r.bytes_written as i64),
                backup_path: result.as_ref().ok().and_then(|r| r.backup_path.clone()),
                duration_ms: started.elapsed().as_millis() as i64,
            },
        );
        to_value(&result?)
    }

    fn record_file_audit(&self, scope: &Arc<Scope>, host_label: &str, audit: FileAudit) {
        self.audit.record(AuditEntry {
            at: now_millis(),
            scope: scope.id.clone(),
            host_label: host_label.to_owned(),
            // 命令栏记"工具 + 路径"，方便在审计列表里一眼看懂；文件内容不进审计。
            command: format!("{} {}", audit.tool, audit.path),
            danger: audit.danger,
            decision: audit.decision,
            exit_code: Some(if audit.ok { 0 } else { 1 }),
            duration_ms: Some(audit.duration_ms),
            location: None,
            fallback_reason: None,
            tool: audit.tool.to_owned(),
            path: Some(audit.path),
            bytes: audit.bytes,
            backup_path: audit.backup_path,
        });
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, String> {
    serde_json::from_value(params).map_err(|err| format!("参数不正确：{err}"))
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests;
