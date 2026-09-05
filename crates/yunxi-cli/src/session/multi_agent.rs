//! Host-owned child model isolation and multi-agent tool execution.

use serde::Serialize;
use yunxi_kernel::PluginId;
use yunxi_model_openai::MODEL_PLUGIN_ID;
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CapabilityDescriptor, ChatMessage, ChatRequest, ChatResult, MODEL_CHAT_COMPLETE_OPERATION,
    ToolResultOutcome,
};

use super::{ChatSession, HANDSHAKE_TIMEOUT, WRITE_TIMEOUT, call_lost_route, tool_loop};

impl ChatSession {
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

        let reply = match self.run_isolated_child_model(started.transcript()) {
            Ok(reply) => reply,
            Err(error) => {
                self.report_child_agent_failure(capability, grant, agent_id, &error);
                return failed_tool_outcome(error.code, error.message);
            }
        };
        let complete =
            match yunxi_protocol::AgentTurnCompleteRequest::new(grant.clone(), agent_id, &reply) {
                Ok(request) => request,
                Err(error) => {
                    let failure = ChildTurnError::new("child_reply_invalid", error.to_string());
                    self.report_child_agent_failure(capability, grant, agent_id, &failure);
                    return failed_tool_outcome(failure.code, failure.message);
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

    fn run_isolated_child_model(
        &self,
        transcript: &[yunxi_protocol::AgentTranscriptEntry],
    ) -> Result<String, ChildTurnError> {
        let mut messages = vec![ChatMessage::system(
            "You are an isolated YunXi child agent. Complete only the delegated task. You have no inherited tools, workspace access, or channel authority. Return a concrete result and do not claim actions you could not perform.",
        )];
        messages.extend(transcript.iter().map(|entry| match entry.role() {
            yunxi_protocol::AgentTranscriptRole::User => ChatMessage::user(entry.content()),
            yunxi_protocol::AgentTranscriptRole::Assistant => {
                ChatMessage::assistant(entry.content())
            }
        }));

        let id = PluginId::new(MODEL_PLUGIN_ID)
            .map_err(|error| ChildTurnError::new("child_model_unavailable", error.to_string()))?;
        let mut child_host = ProcessPluginHost::new();
        let launch = PluginLaunch::new(id, self.child_model_command.clone())
            .with_display_name("Isolated child chat model")
            .with_handshake_timeout(HANDSHAKE_TIMEOUT)
            .with_io_timeouts(Some(self.child_model_response_timeout), Some(WRITE_TIMEOUT))
            .with_required_grants(self.child_model_required_grants.iter().copied());
        if let Err(error) = child_host.launch(launch) {
            child_host.shutdown();
            return Err(ChildTurnError::new(
                "child_model_unavailable",
                error.to_string(),
            ));
        }
        let result = child_host.invoke::<_, ChatResult>(
            &self.model_capability,
            MODEL_CHAT_COMPLETE_OPERATION,
            &ChatRequest::new(messages),
        );
        child_host.shutdown();
        let result =
            result.map_err(|error| ChildTurnError::new("child_model_failed", error.to_string()))?;
        if !result.tool_calls().is_empty() {
            return Err(ChildTurnError::new(
                "child_tools_not_allowed",
                "isolated child model returned tool calls without receiving a tool catalog",
            ));
        }
        if result.content().trim().is_empty() {
            return Err(ChildTurnError::new(
                "child_empty_response",
                "isolated child model returned an empty response",
            ));
        }
        Ok(result.content().to_string())
    }

    fn report_child_agent_failure(
        &mut self,
        capability: &CapabilityDescriptor,
        grant: &yunxi_protocol::AgentDelegationGrant,
        agent_id: &str,
        failure: &ChildTurnError,
    ) {
        let request = match yunxi_protocol::AgentTurnFailRequest::new(
            grant.clone(),
            agent_id,
            failure.code,
            &failure.message,
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
        )
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

struct ChildTurnError {
    code: &'static str,
    message: String,
}

impl ChildTurnError {
    fn new(code: &'static str, message: impl AsRef<str>) -> Self {
        let mut bounded = String::new();
        for character in message.as_ref().replace('\0', " ").chars() {
            if bounded.len() + character.len_utf8() > 3000 {
                break;
            }
            bounded.push(character);
        }
        if bounded.trim().is_empty() {
            bounded = "isolated child model turn failed".to_string();
        }
        Self {
            code,
            message: bounded,
        }
    }
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
