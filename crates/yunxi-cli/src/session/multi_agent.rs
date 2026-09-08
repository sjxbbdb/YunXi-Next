//! Host-owned child model isolation and multi-agent tool execution.

use serde::Serialize;
use yunxi_multi_agent::{CancellationToken, ChildTurn, ChildWorkerError};
use yunxi_plugin_host::PluginCallError;
use yunxi_protocol::{
    AgentDelegationGrant, AgentTurnStartRequest, CapabilityDescriptor, GrantKind, ModelStreamEvent,
    ToolResultOutcome,
};

use super::child_agent::{ChildAgentFailure, ChildAgentRuntime};
use super::{ChatSession, call_lost_route, tool_loop};

/// The immutable portion of a Web child turn.  It deliberately contains no
/// mutable parent session, so a child can continue while WebHost services
/// another session or an interrupt request.
#[derive(Clone)]
pub(crate) struct WebAgentTask {
    runtime: ChildAgentRuntime,
}

impl WebAgentTask {
    /// Runs one child turn without owning coordinator state.  The runtime
    /// persists the turn lifecycle; this adapter only owns the isolated model
    /// process and forwards bounded stream events.
    pub(crate) fn run<OnEvent>(
        self,
        turn: ChildTurn,
        cancellation: &CancellationToken,
        on_event: OnEvent,
    ) -> Result<String, ChildWorkerError>
    where
        OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
    {
        if cancellation.is_cancelled() {
            return Err(child_worker_error(
                "cancelled",
                "child agent turn was cancelled",
            ));
        }

        let cancellation = cancellation.clone();
        self.runtime
            .run(
                &turn.transcript,
                &turn.message,
                &turn.model,
                &turn.tool_grants,
                move || cancellation.is_cancelled(),
                on_event,
            )
            .map_err(|error| child_worker_error(error.code(), error.message()))
    }
}

impl ChatSession {
    pub(crate) fn multi_agent_web_available(&self) -> bool {
        self.multi_agent_capability.is_some()
    }

    pub(crate) fn web_agent_model(&self) -> &str {
        &self.model
    }

    /// Creates the immutable execution adapter used when a persisted Web
    /// child turn is reattached after a host restart.  The adapter carries
    /// the same host, workspace, sandbox, and plugin capabilities as the
    /// parent session; the coordinator remains responsible for restoring the
    /// child transcript and grant policy.
    pub(crate) fn web_agent_task(&self) -> WebAgentTask {
        WebAgentTask {
            runtime: self.child_agent_runtime(),
        }
    }

    pub(super) fn prepare_agent_session(&mut self) {
        if self.multi_agent_capability.is_none() {
            return;
        }
        if let Some(session_id) = &self.active_session_id {
            self.agent_session_id = session_id.clone();
            return;
        }
        if self.storage_capability.is_some() {
            match self.create_web_session() {
                Ok(session) => self.agent_session_id = session.id().to_string(),
                Err(error) => self.push_notice(format!(
                    "multi-agent state could not bind to a persisted chat session: {error}"
                )),
            }
        }
    }

    pub(super) fn execute_agent_spawn(
        &mut self,
        task: &str,
        name: Option<&str>,
        parent_id: Option<&str>,
        requested_grants: &[GrantKind],
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_tool_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is no longer available",
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_tool_outcome("invalid_agent_grant", error.to_string()),
        };
        let mut request = match yunxi_protocol::AgentSpawnRequest::new(grant.clone(), task) {
            Ok(request) => request,
            Err(error) => return failed_tool_outcome("invalid_agent_request", error.to_string()),
        };
        request = match request.with_requested_child_grants(requested_grants.iter().copied()) {
            Ok(request) => request,
            Err(error) => return failed_tool_outcome("invalid_agent_request", error.to_string()),
        };
        if let Some(name) = name {
            request = match request.with_name(name) {
                Ok(request) => request,
                Err(error) => {
                    return failed_tool_outcome("invalid_agent_request", error.to_string());
                }
            };
        }
        if let Some(parent_id) = parent_id {
            request = match request.with_parent(parent_id) {
                Ok(request) => request,
                Err(error) => {
                    return failed_tool_outcome("invalid_agent_request", error.to_string());
                }
            };
        }
        let spawned = match self.host.invoke::<_, yunxi_protocol::AgentSpawnResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_SPAWN_OPERATION,
            &request,
        ) {
            Ok(result) => result,
            Err(error) => {
                return self
                    .multi_agent_tool_error_outcome(tool_loop::AGENT_SPAWN_TOOL_NAME, error);
            }
        };
        self.execute_child_agent_turn(
            &capability,
            &grant,
            spawned.agent().id(),
            task,
            tool_loop::AGENT_SPAWN_TOOL_NAME,
        )
    }

    pub(super) fn execute_agent_message(
        &mut self,
        agent_id: &str,
        message: &str,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_tool_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is no longer available",
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_tool_outcome("invalid_agent_grant", error.to_string()),
        };
        self.execute_child_agent_turn(
            &capability,
            &grant,
            agent_id,
            message,
            tool_loop::AGENT_MESSAGE_TOOL_NAME,
        )
    }

    pub(super) fn execute_agent_list(&mut self, ticket: &str) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_tool_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is no longer available",
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_tool_outcome("invalid_agent_grant", error.to_string()),
        };
        match self.host.invoke::<_, yunxi_protocol::AgentListResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_LIST_OPERATION,
            &yunxi_protocol::AgentListRequest::new(grant),
        ) {
            Ok(result) => completed_tool_outcome(result, "multi-agent list returned invalid data"),
            Err(error) => {
                self.multi_agent_tool_error_outcome(tool_loop::AGENT_LIST_TOOL_NAME, error)
            }
        }
    }

    pub(crate) fn web_agent_graph(
        &mut self,
        root_session_id: &str,
    ) -> Result<yunxi_protocol::AgentListResult, String> {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return Err("multi-agent capability is disabled or unavailable".to_string());
        };
        let grant = self
            .web_agent_grant(root_session_id)
            .map_err(|error| error.to_string())?;
        let result = self.host.invoke::<_, yunxi_protocol::AgentListResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_LIST_OPERATION,
            &yunxi_protocol::AgentListRequest::new(grant),
        );
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                if call_lost_route(&error) {
                    self.multi_agent_capability = None;
                }
                Err(error.to_string())
            }
        }
    }

    pub(crate) fn prepare_web_agent_runtime_with_model(
        &self,
        root_session_id: &str,
        agent_id: &str,
        message: &str,
        model: Option<&str>,
    ) -> Result<(AgentDelegationGrant, WebAgentTask), String> {
        if let Some(model) = model {
            yunxi_multi_agent::ChildWorkerSpec::new(model, [])
                .map_err(|error| error.to_string())?;
        }
        let grant = self
            .web_agent_write_grant(root_session_id)
            .map_err(|error| error.to_string())?;
        AgentTurnStartRequest::new(grant.clone(), agent_id, message)
            .map_err(|error| error.to_string())?;
        Ok((
            grant,
            WebAgentTask {
                runtime: self.child_agent_runtime(),
            },
        ))
    }

    pub(crate) fn web_agent_interrupt(
        &mut self,
        root_session_id: &str,
        agent_id: &str,
    ) -> Result<(), String> {
        let capability = self
            .multi_agent_capability
            .clone()
            .ok_or_else(|| "multi-agent capability is disabled or unavailable".to_string())?;
        let grant = self
            .web_agent_write_grant(root_session_id)
            .map_err(|error| error.to_string())?;
        let request = yunxi_protocol::AgentInterruptRequest::new(grant, agent_id, false)
            .map_err(|error| error.to_string())?;
        self.host
            .invoke::<_, yunxi_protocol::AgentMutationResult>(
                &capability,
                yunxi_protocol::TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
                &request,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn web_agent_inspect(
        &mut self,
        root_session_id: &str,
        agent_id: &str,
    ) -> Result<yunxi_protocol::AgentInspectResult, String> {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return Err("multi-agent capability is disabled or unavailable".to_string());
        };
        let grant = self
            .web_agent_grant(root_session_id)
            .map_err(|error| error.to_string())?;
        let request = yunxi_protocol::AgentInspectRequest::new(grant, agent_id)
            .map_err(|error| error.to_string())?;
        let result = self.host.invoke::<_, yunxi_protocol::AgentInspectResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_INSPECT_OPERATION,
            &request,
        );
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                if call_lost_route(&error) {
                    self.multi_agent_capability = None;
                }
                Err(error.to_string())
            }
        }
    }

    pub(super) fn execute_agent_interrupt(
        &mut self,
        agent_id: &str,
        recursive: bool,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_tool_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is no longer available",
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_tool_outcome("invalid_agent_grant", error.to_string()),
        };
        let request = match yunxi_protocol::AgentInterruptRequest::new(grant, agent_id, recursive) {
            Ok(request) => request,
            Err(error) => return failed_tool_outcome("invalid_agent_request", error.to_string()),
        };
        match self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
            &request,
        ) {
            Ok(result) => {
                completed_tool_outcome(result, "multi-agent interrupt returned invalid data")
            }
            Err(error) => {
                self.multi_agent_tool_error_outcome(tool_loop::AGENT_INTERRUPT_TOOL_NAME, error)
            }
        }
    }

    fn execute_child_agent_turn(
        &mut self,
        capability: &CapabilityDescriptor,
        grant: &yunxi_protocol::AgentDelegationGrant,
        agent_id: &str,
        message: &str,
        tool_name: &str,
    ) -> ToolResultOutcome {
        let start =
            match yunxi_protocol::AgentTurnStartRequest::new(grant.clone(), agent_id, message) {
                Ok(request) => request,
                Err(error) => {
                    return failed_tool_outcome("invalid_agent_request", error.to_string());
                }
            };
        let started = match self.host.invoke::<_, yunxi_protocol::AgentTurnStartResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_START_OPERATION,
            &start,
        ) {
            Ok(result) => result,
            Err(error) => return self.multi_agent_tool_error_outcome(tool_name, error),
        };

        let child_grants = started.agent().child_grants().to_vec();
        let runtime = self.child_agent_runtime();
        let model = self.model.clone();
        let reply = match runtime.run(
            started.transcript(),
            message,
            &model,
            &child_grants,
            || false,
            |_| Ok(()),
        ) {
            Ok(reply) => reply,
            Err(error) => {
                self.report_child_agent_failure(capability, grant, agent_id, &error);
                return failed_tool_outcome(error.code(), error.message());
            }
        };
        let complete =
            match yunxi_protocol::AgentTurnCompleteRequest::new(grant.clone(), agent_id, &reply) {
                Ok(request) => request,
                Err(error) => {
                    let failure = ChildAgentFailure::new("child_reply_invalid", error.to_string());
                    self.report_child_agent_failure(capability, grant, agent_id, &failure);
                    return failed_tool_outcome(failure.code(), failure.message());
                }
            };
        match self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
            &complete,
        ) {
            Ok(result) => completed_tool_outcome(
                serde_json::json!({
                    "agent": result.agents().first(),
                    "reply": reply,
                    "events": result.events(),
                }),
                "multi-agent completion returned invalid data",
            ),
            Err(error) => self.multi_agent_tool_error_outcome(tool_name, error),
        }
    }

    fn report_child_agent_failure(
        &mut self,
        capability: &CapabilityDescriptor,
        grant: &yunxi_protocol::AgentDelegationGrant,
        agent_id: &str,
        failure: &ChildAgentFailure,
    ) {
        let request = match yunxi_protocol::AgentTurnFailRequest::new(
            grant.clone(),
            agent_id,
            failure.code(),
            failure.message(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.push_notice(format!(
                    "failed to encode child agent failure for `{agent_id}`: {error}"
                ));
                return;
            }
        };
        if let Err(error) = self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_FAIL_OPERATION,
            &request,
        ) {
            if call_lost_route(&error) {
                self.multi_agent_capability = None;
            }
            self.push_notice(format!(
                "failed to persist child agent `{agent_id}` failure: {error}"
            ));
        }
    }

    fn agent_delegation_grant(
        &self,
        ticket: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        yunxi_protocol::AgentDelegationGrant::new(
            yunxi_protocol::WorkspaceGrant::read_write(&self.cwd),
            &self.agent_session_id,
            ticket,
            self.agent_budget,
        )?
        .with_allowed_child_grants(self.available_child_grants())
    }

    fn available_child_grants(&self) -> Vec<GrantKind> {
        let patch_available =
            self.patch_capability.is_some() && self.sandbox != crate::args::SandboxMode::ReadOnly;
        let read_available = self.files_capability.is_some() || patch_available;
        let mut grants = Vec::new();
        if read_available {
            grants.push(GrantKind::WorkspaceRead);
        }
        if patch_available {
            grants.push(GrantKind::WorkspaceWrite);
        }
        grants
    }

    fn child_agent_runtime(&self) -> ChildAgentRuntime {
        ChildAgentRuntime::new(
            self.host.clone(),
            self.cwd.clone(),
            self.files_capability.clone(),
            self.patch_capability.clone(),
            self.sandbox,
            self.child_model_command.clone(),
            self.child_model_required_grants.clone(),
            self.child_model_response_timeout,
            self.model_capability.clone(),
        )
    }

    fn web_agent_grant(
        &self,
        root_session_id: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        yunxi_protocol::AgentDelegationGrant::new(
            yunxi_protocol::WorkspaceGrant::read_only(&self.cwd),
            root_session_id,
            format!("web-read-{}", std::process::id()),
            self.agent_budget,
        )?
        .with_allowed_child_grants(
            self.available_child_grants()
                .into_iter()
                .filter(|grant| *grant == GrantKind::WorkspaceRead)
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) fn web_agent_read_grant(
        &self,
        root_session_id: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        self.web_agent_grant(root_session_id)
    }

    pub(crate) fn web_agent_recovery_grant(
        &self,
        root_session_id: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        self.web_agent_write_grant(root_session_id)
    }

    fn web_agent_write_grant(
        &self,
        root_session_id: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        yunxi_protocol::AgentDelegationGrant::new(
            yunxi_protocol::WorkspaceGrant::read_write(&self.cwd),
            root_session_id,
            format!("web-write-{}", std::process::id()),
            self.agent_budget,
        )?
        .with_allowed_child_grants(self.available_child_grants())
    }

    fn multi_agent_tool_error_outcome(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
    ) -> ToolResultOutcome {
        let (code, message, retryable) = match error {
            PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            } => (code, message, retryable),
            other => {
                if call_lost_route(&other) {
                    self.multi_agent_capability = None;
                }
                (
                    "multi_agent_unavailable".to_string(),
                    other.to_string(),
                    false,
                )
            }
        };
        self.push_notice(format!(
            "model multi-agent tool `{tool_name}` failed: {code}: {message}"
        ));
        ToolResultOutcome::failed(code, message, retryable)
            .unwrap_or_else(|_| ToolResultOutcome::cancelled("agent result was invalid").unwrap())
    }
}

fn child_worker_error(code: impl Into<String>, message: impl Into<String>) -> ChildWorkerError {
    ChildWorkerError::new(code, message).unwrap_or_else(|_| {
        ChildWorkerError::new("child_worker_failed", "child worker failed")
            .expect("bounded fallback")
    })
}

fn failed_tool_outcome(code: impl Into<String>, message: impl Into<String>) -> ToolResultOutcome {
    ToolResultOutcome::failed(code, message, false)
        .unwrap_or_else(|_| ToolResultOutcome::cancelled("tool failure was invalid").unwrap())
}

fn completed_tool_outcome<T>(value: T, invalid_message: &str) -> ToolResultOutcome
where
    T: Serialize,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| ToolResultOutcome::completed(value).ok())
        .unwrap_or_else(|| {
            ToolResultOutcome::rejected("invalid_tool_result", invalid_message)
                .expect("bounded invalid tool result")
        })
}

pub(super) fn new_agent_session_id() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("session-{}-{stamp}", std::process::id())
}
