//! Stateful post-response orchestration and REPL management calls.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use yunxi_protocol::{
    ActionGrant, COMPANION_MAILBOX_ENQUEUE_OPERATION, COMPANION_MAILBOX_GET_OPERATION,
    COMPANION_MAILBOX_LIST_OPERATION, COMPANION_MAILBOX_MARK_READ_OPERATION,
    MEMORY_WRITE_EXTRACT_OPERATION, MEMORY_WRITE_REVIEW_OPERATION, MailboxEnqueueRequest,
    MailboxGetRequest, MailboxGetResult, MailboxItemKind, MailboxListRequest, MailboxListResult,
    MailboxMarkReadRequest, MailboxMutationResult, MemoryReviewAction, MemoryReviewRequest,
    MemoryReviewResult, MemoryWriteRequest, MemoryWriteResult, MemoryWriteStatus,
    PatchApplyRequest, PatchApplyResult, PatchChangeKind, ProactiveSchedulerRequest,
    ProactiveSchedulerResult, SCHEDULER_PROACTIVE_EVALUATE_OPERATION,
    STORAGE_SESSIONS_APPEND_OPERATION, STORAGE_SESSIONS_LIST_OPERATION,
    STORAGE_SESSIONS_LOAD_OPERATION, SessionAppendRequest, SessionListRequest, SessionListResult,
    SessionLoadRequest, SessionLoadResult, ShellExecuteRequest, ShellExecuteResult,
    TOOL_PATCH_APPLY_OPERATION, TOOL_SHELL_EXECUTE_OPERATION, WorkspaceGrant,
};

use crate::management::{ManagementCommand, ManagementResult};

use super::{ChatSession, call_lost_route};

impl ChatSession {
    pub(super) fn persist_turn(&mut self, prompt: &str, reply: &str) {
        let Some(capability) = self.storage_capability.clone() else {
            return;
        };
        let mut request =
            SessionAppendRequest::new(WorkspaceGrant::read_write(&self.cwd), prompt, reply)
                .with_provider(&self.provider)
                .with_model(&self.model);
        if let Some(session_id) = &self.active_session_id {
            request = request.with_session_id(session_id);
        }
        match self
            .host
            .invoke(&capability, STORAGE_SESSIONS_APPEND_OPERATION, &request)
        {
            Ok(session) => {
                let session: yunxi_protocol::SessionSnapshot = session;
                self.active_session_id = Some(session.id().to_string());
            }
            Err(error) => {
                if call_lost_route(&error) {
                    self.storage_capability = None;
                }
                self.push_notice(format!("session storage degraded: {error}"));
            }
        }
    }

    pub(super) fn extract_turn_memories(&mut self, prompt: &str, reply: &str) {
        let Some(capability) = self.memory_write_capability.clone() else {
            return;
        };
        let mut request = MemoryWriteRequest::new(memory_write_grant(&self.cwd), prompt)
            .with_assistant_response(reply);
        if let Some(session_id) = &self.active_session_id {
            request = request.with_source_session_id(session_id);
        }
        match self.host.invoke::<_, MemoryWriteResult>(
            &capability,
            MEMORY_WRITE_EXTRACT_OPERATION,
            &request,
        ) {
            Ok(result) => {
                for warning in result.warnings() {
                    self.push_notice(format!("memory write warning: {warning}"));
                }
                for record in result.records() {
                    match record.status() {
                        MemoryWriteStatus::Pending => self.push_notice(format!(
                            "memory `{}` is pending review; use `/memory approve {}` or `/memory reject {}`",
                            record.content(),
                            record.id().unwrap_or("unknown"),
                            record.id().unwrap_or("unknown")
                        )),
                        MemoryWriteStatus::Discarded => self.push_notice(format!(
                            "memory was discarded by privacy policy: {}",
                            record.reason()
                        )),
                        MemoryWriteStatus::Active | MemoryWriteStatus::Rejected => {}
                    }
                }
            }
            Err(error) => {
                if call_lost_route(&error) {
                    self.memory_write_capability = None;
                    self.memory_capability = None;
                }
                self.push_notice(format!("memory write capability degraded: {error}"));
            }
        }
    }

    pub(super) fn evaluate_proactive(&mut self, prompt: &str) {
        let Some(capability) = self.scheduler_capability.clone() else {
            return;
        };
        let request = proactive_request(prompt, self.proactive_in_session);
        match self.host.invoke::<_, ProactiveSchedulerResult>(
            &capability,
            SCHEDULER_PROACTIVE_EVALUATE_OPERATION,
            &request,
        ) {
            Ok(result) => {
                for plan in result.plans() {
                    let message = plan.message().to_string();
                    if self.enqueue_proactive_message(plan.reason(), &message) {
                        self.proactive_in_session = self.proactive_in_session.saturating_add(1);
                        self.push_notice(format!("companion message queued: {message}"));
                    } else if self.mailbox_capability.is_none() {
                        self.proactive_in_session = self.proactive_in_session.saturating_add(1);
                        self.push_notice(format!("companion: {message}"));
                    }
                }
            }
            Err(error) => {
                if call_lost_route(&error) {
                    self.scheduler_capability = None;
                }
                self.push_notice(format!("scheduler capability degraded: {error}"));
            }
        }
    }

    pub(super) fn manage_command(
        &mut self,
        command: ManagementCommand,
    ) -> Result<ManagementResult, String> {
        match command {
            ManagementCommand::ListSessions => self.list_sessions(),
            ManagementCommand::ResumeSession(id) => self.resume_session(&id),
            ManagementCommand::NewSession => {
                self.active_session_id = None;
                self.first_turn = true;
                self.proactive_in_session = 0;
                Ok(ManagementResult::replace_history(
                    vec!["Started a new session.".to_string()],
                    Vec::new(),
                ))
            }
            ManagementCommand::ReviewMemory { id, approve } => self.review_memory(&id, approve),
            ManagementCommand::ListMailbox => self.list_mailbox(),
            ManagementCommand::ReadMailbox(id) => self.read_mailbox(&id),
            ManagementCommand::RequestShell(command) => self.queue_shell(command),
            ManagementCommand::RequestPatch(path) => self.queue_patch(path),
            ManagementCommand::ApproveAction => self.approve_action(),
            ManagementCommand::DenyAction => {
                if self.pending_action.take().is_some() {
                    Ok(ManagementResult::lines(vec![
                        "Pending action denied.".to_string(),
                    ]))
                } else {
                    Err("there is no pending action".to_string())
                }
            }
        }
    }

    fn queue_shell(&mut self, command: String) -> Result<ManagementResult, String> {
        if self.shell_capability.is_none() {
            return Err("shell capability is disabled or unavailable".to_string());
        }
        let command = command.trim().to_string();
        if command.is_empty() {
            return Err("usage: /shell <command>".to_string());
        }
        self.pending_action = Some(super::PendingAction::Shell {
            command: command.clone(),
        });
        Ok(ManagementResult::lines(vec![
            "Approval required for shell action.".to_string(),
            format!("command: {command}"),
            format!("workspace: {}", self.cwd.display()),
            "Use /approve to run it or /deny to discard it.".to_string(),
        ]))
    }

    fn queue_patch(&mut self, value: String) -> Result<ManagementResult, String> {
        if self.patch_capability.is_none() {
            return Err("patch capability is disabled or unavailable".to_string());
        }
        let value = value.trim();
        if value.is_empty() {
            return Err("usage: /patch <patch-file>".to_string());
        }
        let path = resolve_patch_file(&self.cwd, value)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("patch file {} is unavailable: {error}", path.display()))?;
        if metadata.len() > 2 * 1024 * 1024 {
            return Err("patch file exceeds the 2 MiB host limit".to_string());
        }
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("patch file {} is not UTF-8: {error}", path.display()))?;
        self.pending_action = Some(super::PendingAction::Patch {
            path: path.clone(),
            content,
        });
        Ok(ManagementResult::lines(vec![
            "Approval required for patch action.".to_string(),
            format!("patch file: {}", path.display()),
            format!("workspace: {}", self.cwd.display()),
            "Use /approve to apply it or /deny to discard it.".to_string(),
        ]))
    }

    fn approve_action(&mut self) -> Result<ManagementResult, String> {
        let action = self
            .pending_action
            .take()
            .ok_or_else(|| "there is no pending action".to_string())?;
        let ticket = format!("action-{}-{}", process::id(), self.next_action_ticket);
        self.next_action_ticket = self.next_action_ticket.saturating_add(1);
        let grant = ActionGrant::approved(
            yunxi_protocol::WorkspaceGrant::read_write(&self.cwd).with_workspace_write(),
            &self.cwd,
            ticket,
        )
        .with_write(true)
        .with_network(action_network_allowed());

        match action {
            super::PendingAction::Shell { command } => {
                let capability = self
                    .shell_capability
                    .clone()
                    .ok_or_else(|| "shell capability is disabled or unavailable".to_string())?;
                let request = ShellExecuteRequest::new(grant, command);
                let result = self
                    .host
                    .invoke::<_, ShellExecuteResult>(
                        &capability,
                        TOOL_SHELL_EXECUTE_OPERATION,
                        &request,
                    )
                    .map_err(|error| {
                        if call_lost_route(&error) {
                            self.shell_capability = None;
                        }
                        error.to_string()
                    })?;
                Ok(ManagementResult::lines(format_shell_result(&result)))
            }
            super::PendingAction::Patch { path, content } => {
                let capability = self
                    .patch_capability
                    .clone()
                    .ok_or_else(|| "patch capability is disabled or unavailable".to_string())?;
                let request = PatchApplyRequest::new(grant, content);
                let result = self
                    .host
                    .invoke::<_, PatchApplyResult>(
                        &capability,
                        TOOL_PATCH_APPLY_OPERATION,
                        &request,
                    )
                    .map_err(|error| {
                        if call_lost_route(&error) {
                            self.patch_capability = None;
                        }
                        error.to_string()
                    })?;
                Ok(ManagementResult::lines(format_patch_result(&path, &result)))
            }
        }
    }

    fn enqueue_proactive_message(&mut self, reason: &str, message: &str) -> bool {
        let Some(capability) = self.mailbox_capability.clone() else {
            return false;
        };
        let session = self.active_session_id.as_deref().unwrap_or("ephemeral");
        let idempotency_key = format!(
            "{session}:{}:{}",
            self.proactive_in_session,
            compact_for_key(reason, 120)
        );
        let request = MailboxEnqueueRequest::new(
            WorkspaceGrant::read_write(&self.cwd),
            MailboxItemKind::ProactiveMessage,
            "YunXi follow-up",
            message,
            reason,
            idempotency_key,
        );
        match self.host.invoke::<_, MailboxMutationResult>(
            &capability,
            COMPANION_MAILBOX_ENQUEUE_OPERATION,
            &request,
        ) {
            Ok(result) => result.item().is_some(),
            Err(error) => {
                if call_lost_route(&error) {
                    self.mailbox_capability = None;
                }
                self.push_notice(format!("mailbox capability degraded: {error}"));
                false
            }
        }
    }

    fn list_sessions(&mut self) -> Result<ManagementResult, String> {
        let capability = self
            .storage_capability
            .clone()
            .ok_or_else(|| "session storage capability is disabled or unavailable".to_string())?;
        let request = SessionListRequest::new(WorkspaceGrant::read_only(&self.cwd));
        let result = self
            .host
            .invoke::<_, SessionListResult>(&capability, STORAGE_SESSIONS_LIST_OPERATION, &request)
            .map_err(|error| {
                if call_lost_route(&error) {
                    self.storage_capability = None;
                }
                error.to_string()
            })?;
        for warning in result.warnings() {
            self.push_notice(format!("session data warning: {warning}"));
        }
        let mut lines = result
            .sessions()
            .iter()
            .map(|session| {
                format!(
                    "{}{}{} | {} messages | {}{}",
                    session.id(),
                    if session.pinned() { " [pinned]" } else { "" },
                    if session.archived() {
                        " [archived]"
                    } else {
                        ""
                    },
                    session.message_count(),
                    session.title(),
                    if session.legacy() {
                        " [legacy read-only]"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push("No saved sessions.".to_string());
        } else if result.truncated() {
            lines.push("Session list was truncated.".to_string());
        }
        Ok(ManagementResult::lines(lines))
    }

    fn resume_session(&mut self, id: &str) -> Result<ManagementResult, String> {
        let capability = self
            .storage_capability
            .clone()
            .ok_or_else(|| "session storage capability is disabled or unavailable".to_string())?;
        let request = SessionLoadRequest::new(WorkspaceGrant::read_only(&self.cwd), id);
        let result = self
            .host
            .invoke::<_, SessionLoadResult>(&capability, STORAGE_SESSIONS_LOAD_OPERATION, &request)
            .map_err(|error| {
                if call_lost_route(&error) {
                    self.storage_capability = None;
                }
                error.to_string()
            })?;
        for warning in result.warnings() {
            self.push_notice(format!("session data warning: {warning}"));
        }
        let session = result
            .into_session()
            .ok_or_else(|| format!("session `{id}` was not found"))?;
        let messages = session.messages().to_vec();
        self.active_session_id = Some(session.id().to_string());
        self.first_turn = true;
        self.proactive_in_session = 0;
        Ok(ManagementResult::replace_history(
            vec![format!(
                "Resumed session `{}` with {} messages{}.",
                session.id(),
                messages.len(),
                if session.legacy() {
                    "; the next turn will import it into YunXi Next"
                } else {
                    ""
                }
            )],
            messages,
        ))
    }

    fn review_memory(&mut self, id: &str, approve: bool) -> Result<ManagementResult, String> {
        let capability = self
            .memory_write_capability
            .clone()
            .ok_or_else(|| "memory write capability is disabled or unavailable".to_string())?;
        let action = if approve {
            MemoryReviewAction::Approve
        } else {
            MemoryReviewAction::Reject
        };
        let request = MemoryReviewRequest::new(memory_write_grant(&self.cwd), id, action);
        let result = self
            .host
            .invoke::<_, MemoryReviewResult>(&capability, MEMORY_WRITE_REVIEW_OPERATION, &request)
            .map_err(|error| {
                if call_lost_route(&error) {
                    self.memory_write_capability = None;
                    self.memory_capability = None;
                }
                error.to_string()
            })?;
        for warning in result.warnings() {
            self.push_notice(format!("memory data warning: {warning}"));
        }
        let record = result
            .record()
            .ok_or_else(|| format!("memory `{id}` was not found"))?;
        Ok(ManagementResult::lines(vec![format!(
            "Memory `{}` is now {:?}: {}",
            record.id().unwrap_or(id),
            record.status(),
            record.content()
        )]))
    }

    fn list_mailbox(&mut self) -> Result<ManagementResult, String> {
        let capability = self
            .mailbox_capability
            .clone()
            .ok_or_else(|| "mailbox capability is disabled or unavailable".to_string())?;
        let request = MailboxListRequest::new(WorkspaceGrant::read_only(&self.cwd));
        let result = self
            .host
            .invoke::<_, MailboxListResult>(&capability, COMPANION_MAILBOX_LIST_OPERATION, &request)
            .map_err(|error| {
                if call_lost_route(&error) {
                    self.mailbox_capability = None;
                }
                error.to_string()
            })?;
        for warning in result.warnings() {
            self.push_notice(format!("mailbox data warning: {warning}"));
        }
        let mut lines = result
            .items()
            .iter()
            .map(|item| {
                format!(
                    "{}{} | {} | {}",
                    item.id(),
                    if item.read() { " [read]" } else { " [unread]" },
                    item.subject(),
                    item.reason()
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push("Mailbox is empty.".to_string());
        } else {
            lines.insert(0, format!("Unread: {}", result.unread_count()));
        }
        Ok(ManagementResult::lines(lines))
    }

    fn read_mailbox(&mut self, id: &str) -> Result<ManagementResult, String> {
        let capability = self
            .mailbox_capability
            .clone()
            .ok_or_else(|| "mailbox capability is disabled or unavailable".to_string())?;
        let request = MailboxGetRequest::new(WorkspaceGrant::read_only(&self.cwd), id);
        let result = self
            .host
            .invoke::<_, MailboxGetResult>(&capability, COMPANION_MAILBOX_GET_OPERATION, &request)
            .map_err(|error| {
                if call_lost_route(&error) {
                    self.mailbox_capability = None;
                }
                error.to_string()
            })?;
        for warning in result.warnings() {
            self.push_notice(format!("mailbox data warning: {warning}"));
        }
        let entry = result
            .entry()
            .ok_or_else(|| format!("mailbox item `{id}` was not found"))?;
        let subject = entry.summary().subject().to_string();
        let content = entry.content().to_string();
        let mark = MailboxMarkReadRequest::new(WorkspaceGrant::read_write(&self.cwd), id, true);
        if let Err(error) = self.host.invoke::<_, MailboxMutationResult>(
            &capability,
            COMPANION_MAILBOX_MARK_READ_OPERATION,
            &mark,
        ) {
            self.push_notice(format!("mailbox read state was not saved: {error}"));
        }
        Ok(ManagementResult::lines(vec![
            format!("{subject} [{id}]"),
            content,
        ]))
    }
}

fn proactive_request(prompt: &str, proactive_in_session: u32) -> ProactiveSchedulerRequest {
    let now_minute = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| ((duration.as_secs() / 60) % 1440) as u16)
        .unwrap_or_default();
    let normalized = prompt
        .trim()
        .strip_prefix("companion check:")
        .map(str::trim)
        .unwrap_or_else(|| prompt.trim());
    let lower = normalized.to_ascii_lowercase();
    let mut request = ProactiveSchedulerRequest::new(now_minute)
        .with_counts(proactive_in_session, proactive_in_session)
        .allow_tool_requests(
            env::var("YUNXI_NEXT_COMPANION_TOOL_REQUESTS")
                .ok()
                .is_some_and(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                }),
        );
    if lower.contains("reminder due") || normalized.contains("提醒到期") {
        request = request.with_reminder_due(true);
    }
    if let Some(task) = normalized
        .strip_prefix("unfinished task:")
        .or_else(|| normalized.strip_prefix("未完成任务："))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.with_unfinished_task(task);
    }
    if let Some(topic) = normalized
        .strip_prefix("continue topic:")
        .or_else(|| normalized.strip_prefix("继续话题："))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.with_topic_continuation(topic);
    }
    if lower.contains("periodic summary") || normalized.contains("阶段总结") {
        request = request.with_periodic_summary_due(true);
    }
    if let Some(milestone) = normalized
        .strip_prefix("relationship milestone:")
        .or_else(|| normalized.strip_prefix("关系里程碑："))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.with_relationship_milestone(milestone);
    }
    if let Some(tool) = normalized
        .strip_prefix("tool request:")
        .or_else(|| normalized.strip_prefix("工具请求："))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.with_tool_request(tool);
    }
    if lower.contains("long idle") || normalized.contains("长时间空闲") {
        request = request.with_idle_minutes(120);
    }
    request
}

fn compact_for_key(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn resolve_patch_file(workspace: &Path, value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Err("patch file must be relative to the current workspace".to_string());
    }
    let resolved = fs::canonicalize(workspace.join(path))
        .map_err(|error| format!("patch file cannot be resolved: {error}"))?;
    let root = fs::canonicalize(workspace)
        .map_err(|error| format!("workspace cannot be resolved: {error}"))?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err("patch file must be a regular file inside the current workspace".to_string());
    }
    Ok(resolved)
}

fn action_network_allowed() -> bool {
    matches!(
        env::var("YUNXI_NEXT_ACTION_ALLOW_NETWORK")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn format_shell_result(result: &ShellExecuteResult) -> Vec<String> {
    let mut lines = vec![format!(
        "shell exit: {}{}{}",
        result
            .exit_code()
            .map_or_else(|| "unknown".to_string(), |code| code.to_string()),
        if result.timed_out() {
            " [timed out]"
        } else {
            ""
        },
        if result.output_truncated() {
            " [output truncated]"
        } else {
            ""
        }
    )];
    append_output(&mut lines, "stdout", result.stdout());
    append_output(&mut lines, "stderr", result.stderr());
    lines
}

fn append_output(lines: &mut Vec<String>, label: &str, content: &str) {
    if content.is_empty() {
        return;
    }
    lines.push(format!("{label}:"));
    lines.extend(content.lines().map(ToString::to_string));
}

fn format_patch_result(path: &Path, result: &PatchApplyResult) -> Vec<String> {
    let mut lines = vec![format!("patch applied: {}", path.display())];
    for change in result.changes() {
        let kind = match change.kind() {
            PatchChangeKind::Added => "added",
            PatchChangeKind::Updated => "updated",
            PatchChangeKind::Deleted => "deleted",
        };
        lines.push(format!(
            "{kind}: {} (+{} -{})",
            change.path(),
            change.added_lines(),
            change.removed_lines()
        ));
    }
    lines
}

fn memory_write_grant(cwd: &Path) -> WorkspaceGrant {
    WorkspaceGrant::read_write(cwd).with_state_root(next_state_root())
}

fn next_state_root() -> PathBuf {
    if let Some(path) = env::var_os("YUNXI_NEXT_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = env::var_os("USERPROFILE") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    if let Some(path) = env::var_os("HOME") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    PathBuf::from(".yunxi-next")
}
