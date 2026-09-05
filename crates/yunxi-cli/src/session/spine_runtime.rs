//! The default CLI adapter for the Rust Agent spine.
//!
//! The spine owns turn state, context assembly, tool approval, and bounded
//! looping. This module only translates the existing CLI capability contracts
//! into spine traits; optional capabilities still execute in isolated plugin
//! processes through the shared Host handle.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use yunxi_agent_spine::{
    Agent, AgentConfig, AgentError, AgentTurnOutcome, CancellationToken, ToolApprovalPolicy,
    ToolBroker, ToolDecision, ToolError, ToolRequest,
};
use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_model_openai::MODEL_PLUGIN_ID;
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    ActionGrant, AgentBudget, CapabilityDescriptor, ChatMessage, GrantKind, McpToolCallRequest,
    NetworkGrant, SecretGrant, ToolApprovalDecision, ToolApprovalRequest, ToolApprovalState,
    ToolCatalog, ToolResultOutcome, WorkspaceGrant,
};

use super::spine_adapter::{
    ProcessPluginContextAssembler, ProcessPluginHostHandle, ProcessPluginModelProvider,
};
use super::tool_loop::{self, ToolAction};
use super::{ChatFailure, HANDSHAKE_TIMEOUT, WRITE_TIMEOUT, call_lost_route};

/// Inputs captured once while the process host is being assembled.
pub(crate) struct SpineRuntimeConfig {
    pub host: ProcessPluginHostHandle,
    pub model_capability: CapabilityDescriptor,
    pub cwd: PathBuf,
    pub shell_capability: Option<CapabilityDescriptor>,
    pub patch_capability: Option<CapabilityDescriptor>,
    pub files_capability: Option<CapabilityDescriptor>,
    pub mcp_capability: Option<CapabilityDescriptor>,
    pub mcp_tools: Vec<tool_loop::McpToolBinding>,
    pub mcp_network_grant: Option<NetworkGrant>,
    pub mcp_secret_grant: SecretGrant,
    pub skill_tools: Vec<tool_loop::SkillToolBinding>,
    pub multi_agent_capability: Option<CapabilityDescriptor>,
    pub agent_session_id: String,
    pub agent_budget: AgentBudget,
    pub child_model_command: PluginCommand,
    pub child_model_required_grants: Vec<GrantKind>,
    pub child_model_response_timeout: Duration,
}

type SpineAgent = Agent<ProcessPluginModelProvider, ProcessPluginContextAssembler, SpineToolBroker>;

/// Owns one approval-aware spine instance and its small outer-session bridge.
pub(crate) struct SpineController {
    agent: SpineAgent,
    notices: Rc<RefCell<Vec<String>>>,
    prompt: Option<String>,
    next_approval_ticket: u64,
}

impl SpineController {
    pub(crate) fn new(config: SpineRuntimeConfig) -> Result<Self, AgentError> {
        let notices = Rc::new(RefCell::new(Vec::new()));
        let broker = SpineToolBroker::new(config, notices.clone());
        let model =
            ProcessPluginModelProvider::new(broker.host.clone(), broker.model_capability.clone());
        let agent = Agent::new(
            "yunxi-root",
            model,
            ProcessPluginContextAssembler::new(),
            broker,
            AgentConfig::default(),
        )?;
        Ok(Self {
            agent,
            notices,
            prompt: None,
            next_approval_ticket: 1,
        })
    }

    pub(crate) fn set_agent_session_id(&mut self, session_id: impl Into<String>) {
        self.agent
            .tools_mut()
            .set_agent_session_id(session_id.into());
    }

    pub(crate) fn start(
        &mut self,
        user_message: ChatMessage,
        seed: Vec<ChatMessage>,
    ) -> Result<AgentTurnOutcome, AgentError> {
        self.agent.reset_conversation(seed)?;
        self.prompt = Some(user_message.content().to_string());
        self.agent
            .run_turn_with_approval(user_message, &CancellationToken::new())
    }

    pub(crate) fn resolve(&mut self, approved: bool) -> Result<AgentTurnOutcome, AgentError> {
        self.resolve_with_denial_reason(approved, "the user denied this tool call")
    }

    pub(crate) fn resolve_with_denial_reason(
        &mut self,
        approved: bool,
        denial_reason: &str,
    ) -> Result<AgentTurnOutcome, AgentError> {
        let request =
            self.agent
                .pending_approval()
                .cloned()
                .ok_or_else(|| AgentError::InvalidState {
                    operation: "resolve a pending tool".to_string(),
                    state: self.agent.state(),
                })?;
        let state = if approved {
            let ticket = format!(
                "spine-tool-{}-{}",
                std::process::id(),
                self.next_approval_ticket
            );
            self.next_approval_ticket = self.next_approval_ticket.saturating_add(1);
            ToolApprovalState::approved(ticket)
                .map_err(|error| AgentError::protocol("invalid_approval", error.to_string()))?
        } else {
            ToolApprovalState::denied(denial_reason)
                .map_err(|error| AgentError::protocol("invalid_approval", error.to_string()))?
        };
        let decision = ToolApprovalDecision::new(
            request.round(),
            request.call_id().clone(),
            request.tool_name().clone(),
            state,
        )
        .map_err(|error| AgentError::protocol("invalid_approval", error.to_string()))?;
        self.agent
            .approve_pending_tool(decision, &CancellationToken::new())
    }

    pub(crate) fn cancel(&mut self, reason: &str) -> Result<(), AgentError> {
        self.agent.cancel_pending_turn(reason)
    }

    pub(crate) fn reset(&mut self) -> Result<(), AgentError> {
        if self.agent.pending_approval().is_some() {
            self.cancel("session was reset")?;
        }
        self.prompt = None;
        self.agent.reset_conversation(Vec::new())
    }

    pub(crate) fn pending(&self) -> Option<&ToolApprovalRequest> {
        self.agent.pending_approval()
    }

    pub(crate) fn take_prompt(&mut self) -> Option<String> {
        self.prompt.take()
    }

    pub(crate) fn drain_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.notices.borrow_mut())
    }
}

/// Convert a spine failure into the CLI's established error vocabulary.
pub(crate) fn chat_failure_from_agent(error: AgentError) -> ChatFailure {
    match error {
        AgentError::Model(error) | AgentError::Context(error) => ChatFailure::Request {
            code: error.code().to_string(),
            message: error.message().to_string(),
            retryable: error.retryable(),
        },
        AgentError::Tool(error) => ChatFailure::ToolLoop(error.to_string()),
        AgentError::Protocol(error) => ChatFailure::ProtocolViolation(error.to_string()),
        AgentError::BudgetExceeded { .. } => ChatFailure::ToolLoop(error.to_string()),
        AgentError::Cancelled(error) => ChatFailure::Unavailable(error.to_string()),
        AgentError::InvalidInput(error) => ChatFailure::ProtocolViolation(error.to_string()),
        AgentError::InvalidState { .. } | AgentError::Session(_) => {
            ChatFailure::Unavailable(error.to_string())
        }
    }
}

/// A process-host-backed tool broker with a conservative, explicit policy.
struct SpineToolBroker {
    host: ProcessPluginHostHandle,
    model_capability: CapabilityDescriptor,
    cwd: PathBuf,
    shell_capability: Option<CapabilityDescriptor>,
    patch_capability: Option<CapabilityDescriptor>,
    files_capability: Option<CapabilityDescriptor>,
    mcp_capability: Option<CapabilityDescriptor>,
    mcp_tools: Vec<tool_loop::McpToolBinding>,
    mcp_network_grant: Option<NetworkGrant>,
    mcp_secret_grant: SecretGrant,
    skill_tools: Vec<tool_loop::SkillToolBinding>,
    multi_agent_capability: Option<CapabilityDescriptor>,
    agent_session_id: String,
    agent_budget: AgentBudget,
    child_model_command: PluginCommand,
    child_model_required_grants: Vec<GrantKind>,
    child_model_response_timeout: Duration,
    notices: Rc<RefCell<Vec<String>>>,
    next_action_ticket: u64,
}

impl SpineToolBroker {
    fn new(config: SpineRuntimeConfig, notices: Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            host: config.host,
            model_capability: config.model_capability,
            cwd: config.cwd,
            shell_capability: config.shell_capability,
            patch_capability: config.patch_capability,
            files_capability: config.files_capability,
            mcp_capability: config.mcp_capability,
            mcp_tools: config.mcp_tools,
            mcp_network_grant: config.mcp_network_grant,
            mcp_secret_grant: config.mcp_secret_grant,
            skill_tools: config.skill_tools,
            multi_agent_capability: config.multi_agent_capability,
            agent_session_id: config.agent_session_id,
            agent_budget: config.agent_budget,
            child_model_command: config.child_model_command,
            child_model_required_grants: config.child_model_required_grants,
            child_model_response_timeout: config.child_model_response_timeout,
            notices,
            next_action_ticket: 1,
        }
    }

    fn set_agent_session_id(&mut self, session_id: String) {
        self.agent_session_id = session_id;
    }

    fn decode(&self, request: &ToolRequest<'_>) -> Result<ToolAction, String> {
        tool_loop::decode_call_with_skills(
            request.call(),
            self.multi_agent_capability.is_some(),
            &self.mcp_tools,
            &self.skill_tools,
        )
        .map_err(|error| error.to_string())
    }

    fn approval_request(
        &self,
        request: &ToolRequest<'_>,
        action: &ToolAction,
    ) -> Result<ToolApprovalRequest, ToolError> {
        ToolApprovalRequest::new(
            request.round(),
            request.call().id().clone(),
            request.call().name().clone(),
            action.summary(),
            requested_grants(action, self),
        )
        .map_err(|error| ToolError::new("invalid_approval_request", error.to_string(), false))
    }

    fn execute_action(&mut self, action: &ToolAction, ticket: &str) -> ToolResultOutcome {
        match action {
            ToolAction::Skill { binding, .. } => completed_rejection(
                "skill_tool_unavailable",
                format!(
                    "Skill `{}` declared tool `{}` as metadata only; execution is not enabled",
                    binding.skill_id(),
                    binding.remote_name()
                ),
            ),
            ToolAction::Shell {
                command,
                timeout_millis,
            } => self.execute_shell(command, *timeout_millis, ticket),
            ToolAction::Patch {
                patch,
                timeout_millis,
            } => self.execute_patch(patch, *timeout_millis, ticket),
            ToolAction::FileSearch { query, path } => self.execute_file_search(query, path),
            ToolAction::FileRead { path } => self.execute_file_read(path),
            ToolAction::Mcp { binding, arguments } => self.execute_mcp(binding, arguments, ticket),
            ToolAction::AgentSpawn {
                task,
                name,
                parent_id,
            } => self.execute_agent_spawn(task, name.as_deref(), parent_id.as_deref(), ticket),
            ToolAction::AgentList => self.execute_agent_list(ticket),
            ToolAction::AgentMessage { agent_id, message } => {
                self.execute_agent_message(agent_id, message, ticket)
            }
            ToolAction::AgentInterrupt {
                agent_id,
                recursive,
            } => self.execute_agent_interrupt(agent_id, *recursive, ticket),
        }
    }

    fn execute_shell(&mut self, command: &str, timeout: u64, ticket: &str) -> ToolResultOutcome {
        let Some(capability) = self.shell_capability.clone() else {
            return failed_outcome(
                "tool_unavailable",
                "shell capability is disabled or unavailable",
                false,
            );
        };
        let grant = ActionGrant::approved(WorkspaceGrant::read_only(&self.cwd), &self.cwd, ticket)
            .with_limits(timeout, 64 * 1024);
        match self.host.invoke::<_, yunxi_protocol::ShellExecuteResult>(
            &capability,
            yunxi_protocol::TOOL_SHELL_EXECUTE_OPERATION,
            &yunxi_protocol::ShellExecuteRequest::new(grant, command),
        ) {
            Ok(result) => serialized_outcome(result, "shell returned an invalid result"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::SHELL_TOOL_NAME,
                error,
                "tool_unavailable",
                |broker| broker.shell_capability = None,
            ),
        }
    }

    fn execute_patch(&mut self, patch: &str, timeout: u64, ticket: &str) -> ToolResultOutcome {
        let Some(capability) = self.patch_capability.clone() else {
            return failed_outcome(
                "tool_unavailable",
                "patch capability is disabled or unavailable",
                false,
            );
        };
        let grant = ActionGrant::approved(
            WorkspaceGrant::read_write(&self.cwd).with_workspace_write(),
            &self.cwd,
            ticket,
        )
        .with_write(true)
        .with_limits(timeout, 64 * 1024);
        match self.host.invoke::<_, yunxi_protocol::PatchApplyResult>(
            &capability,
            yunxi_protocol::TOOL_PATCH_APPLY_OPERATION,
            &yunxi_protocol::PatchApplyRequest::new(grant, patch),
        ) {
            Ok(result) => serialized_outcome(result, "patch returned an invalid result"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::PATCH_TOOL_NAME,
                error,
                "tool_unavailable",
                |broker| broker.patch_capability = None,
            ),
        }
    }

    fn execute_file_search(&mut self, query: &str, path: &str) -> ToolResultOutcome {
        let Some(capability) = self.files_capability.clone() else {
            return failed_outcome(
                "file_tool_unavailable",
                "file capability is disabled or unavailable",
                false,
            );
        };
        let request = yunxi_protocol::FileSearchRequest::new(
            WorkspaceGrant::read_only(&self.cwd),
            self.cwd.join(path),
            query,
        );
        match self.host.invoke::<_, yunxi_protocol::FileSearchResult>(
            &capability,
            yunxi_protocol::TOOL_FILES_SEARCH_OPERATION,
            &request,
        ) {
            Ok(result) => serialized_outcome(result, "file search returned an invalid result"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::FILE_SEARCH_TOOL_NAME,
                error,
                "file_tool_unavailable",
                |broker| broker.files_capability = None,
            ),
        }
    }

    fn execute_file_read(&mut self, path: &str) -> ToolResultOutcome {
        let Some(capability) = self.files_capability.clone() else {
            return failed_outcome(
                "file_tool_unavailable",
                "file capability is disabled or unavailable",
                false,
            );
        };
        let request =
            yunxi_protocol::FileReadRequest::new(WorkspaceGrant::read_only(&self.cwd), path);
        match self.host.invoke::<_, yunxi_protocol::FileReadResult>(
            &capability,
            yunxi_protocol::TOOL_FILES_READ_OPERATION,
            &request,
        ) {
            Ok(result) => serialized_outcome(result, "file read returned an invalid result"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::FILE_READ_TOOL_NAME,
                error,
                "file_tool_unavailable",
                |broker| broker.files_capability = None,
            ),
        }
    }

    fn execute_mcp(
        &mut self,
        binding: &tool_loop::McpToolBinding,
        arguments: &Value,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.mcp_capability.clone() else {
            return failed_outcome(
                "mcp_unavailable",
                "MCP capability is disabled or unavailable",
                false,
            );
        };
        let grant = ActionGrant::approved(WorkspaceGrant::read_only(&self.cwd), &self.cwd, ticket)
            .with_limits(tool_loop::DEFAULT_MODEL_TOOL_TIMEOUT_MILLIS, 1024 * 1024);
        let grant = match self.mcp_network_grant.clone() {
            Some(network) => grant.with_network_grant(network),
            None => grant,
        }
        .with_secret_grant(self.mcp_secret_grant.clone());
        let request = match McpToolCallRequest::new(
            grant,
            binding.server_name(),
            binding.remote_name(),
            arguments.clone(),
        ) {
            Ok(request) => request,
            Err(error) => return completed_rejection("invalid_mcp_request", error.to_string()),
        };
        match self.host.invoke::<_, yunxi_protocol::McpToolCallResult>(
            &capability,
            yunxi_protocol::TOOL_MCP_CALL_OPERATION,
            &request,
        ) {
            Ok(result) => serialized_outcome(result, "MCP returned an invalid result"),
            Err(error) => self.plugin_error_outcome(
                binding.model_name().as_str(),
                error,
                "mcp_unavailable",
                |broker| {
                    broker.mcp_capability = None;
                    broker.mcp_tools.clear();
                },
            ),
        }
    }

    fn plugin_error_outcome<F>(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
        fallback_code: &str,
        disable: F,
    ) -> ToolResultOutcome
    where
        F: FnOnce(&mut Self),
    {
        let (code, message, retryable) = match error {
            PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            } => (code, message, retryable),
            other => {
                if call_lost_route(&other) {
                    disable(self);
                }
                (fallback_code.to_string(), other.to_string(), false)
            }
        };
        self.notice(format!(
            "model tool `{tool_name}` failed: {code}: {message}"
        ));
        failed_outcome(&code, message, retryable)
    }

    fn notice(&self, message: String) {
        self.notices.borrow_mut().push(message);
    }

    fn next_ticket(&mut self) -> String {
        let ticket = format!(
            "model-tool-{}-{}",
            std::process::id(),
            self.next_action_ticket
        );
        self.next_action_ticket = self.next_action_ticket.saturating_add(1);
        ticket
    }

    fn execute_agent_spawn(
        &mut self,
        task: &str,
        name: Option<&str>,
        parent_id: Option<&str>,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is disabled or unavailable",
                false,
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_outcome("invalid_agent_grant", error.to_string(), false),
        };
        let mut request = match yunxi_protocol::AgentSpawnRequest::new(grant.clone(), task) {
            Ok(request) => request,
            Err(error) => return failed_outcome("invalid_agent_request", error.to_string(), false),
        };
        if let Some(name) = name {
            request = match request.with_name(name) {
                Ok(request) => request,
                Err(error) => {
                    return failed_outcome("invalid_agent_request", error.to_string(), false);
                }
            };
        }
        if let Some(parent_id) = parent_id {
            request = match request.with_parent(parent_id) {
                Ok(request) => request,
                Err(error) => {
                    return failed_outcome("invalid_agent_request", error.to_string(), false);
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
                return self.plugin_error_outcome(
                    tool_loop::AGENT_SPAWN_TOOL_NAME,
                    error,
                    "multi_agent_unavailable",
                    |broker| broker.multi_agent_capability = None,
                );
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

    fn execute_agent_message(
        &mut self,
        agent_id: &str,
        message: &str,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is disabled or unavailable",
                false,
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_outcome("invalid_agent_grant", error.to_string(), false),
        };
        self.execute_child_agent_turn(
            &capability,
            &grant,
            agent_id,
            message,
            tool_loop::AGENT_MESSAGE_TOOL_NAME,
        )
    }

    fn execute_agent_list(&mut self, ticket: &str) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is disabled or unavailable",
                false,
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_outcome("invalid_agent_grant", error.to_string(), false),
        };
        match self.host.invoke::<_, yunxi_protocol::AgentListResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_LIST_OPERATION,
            &yunxi_protocol::AgentListRequest::new(grant),
        ) {
            Ok(result) => serialized_outcome(result, "multi-agent list returned invalid data"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::AGENT_LIST_TOOL_NAME,
                error,
                "multi_agent_unavailable",
                |broker| broker.multi_agent_capability = None,
            ),
        }
    }

    fn execute_agent_interrupt(
        &mut self,
        agent_id: &str,
        recursive: bool,
        ticket: &str,
    ) -> ToolResultOutcome {
        let Some(capability) = self.multi_agent_capability.clone() else {
            return failed_outcome(
                "multi_agent_unavailable",
                "multi-agent capability is disabled or unavailable",
                false,
            );
        };
        let grant = match self.agent_delegation_grant(ticket) {
            Ok(grant) => grant,
            Err(error) => return failed_outcome("invalid_agent_grant", error.to_string(), false),
        };
        let request = match yunxi_protocol::AgentInterruptRequest::new(grant, agent_id, recursive) {
            Ok(request) => request,
            Err(error) => return failed_outcome("invalid_agent_request", error.to_string(), false),
        };
        match self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
            &request,
        ) {
            Ok(result) => serialized_outcome(result, "multi-agent interrupt returned invalid data"),
            Err(error) => self.plugin_error_outcome(
                tool_loop::AGENT_INTERRUPT_TOOL_NAME,
                error,
                "multi_agent_unavailable",
                |broker| broker.multi_agent_capability = None,
            ),
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
                    return failed_outcome("invalid_agent_request", error.to_string(), false);
                }
            };
        let started = match self.host.invoke::<_, yunxi_protocol::AgentTurnStartResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_START_OPERATION,
            &start,
        ) {
            Ok(result) => result,
            Err(error) => {
                return self.plugin_error_outcome(
                    tool_name,
                    error,
                    "multi_agent_unavailable",
                    |broker| broker.multi_agent_capability = None,
                );
            }
        };
        let reply = match self.run_isolated_child_model(started.transcript()) {
            Ok(reply) => reply,
            Err(error) => {
                self.report_child_agent_failure(capability, grant, agent_id, &error);
                return failed_outcome(error.code, error.message, false);
            }
        };
        let complete =
            match yunxi_protocol::AgentTurnCompleteRequest::new(grant.clone(), agent_id, &reply) {
                Ok(request) => request,
                Err(error) => {
                    let failure = ChildTurnError::new("child_reply_invalid", error.to_string());
                    self.report_child_agent_failure(capability, grant, agent_id, &failure);
                    return failed_outcome(failure.code, failure.message, false);
                }
            };
        match self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
            &complete,
        ) {
            Ok(result) => serialized_outcome(
                serde_json::json!({
                    "agent": result.agents().first(),
                    "reply": reply,
                    "events": result.events(),
                }),
                "multi-agent completion returned invalid data",
            ),
            Err(error) => {
                self.plugin_error_outcome(tool_name, error, "multi_agent_unavailable", |broker| {
                    broker.multi_agent_capability = None
                })
            }
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
            .with_required_grants(self.child_model_required_grants.iter().copied())
            .with_expected_capabilities([self.model_capability.clone()]);
        if let Err(error) = child_host.launch(launch) {
            child_host.shutdown();
            return Err(ChildTurnError::new(
                "child_model_unavailable",
                error.to_string(),
            ));
        }
        let result = child_host.invoke::<_, yunxi_protocol::ChatResult>(
            &self.model_capability,
            yunxi_protocol::MODEL_CHAT_COMPLETE_OPERATION,
            &yunxi_protocol::ChatRequest::new(messages),
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
                self.notice(format!(
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
            self.notice(format!(
                "failed to persist child agent `{agent_id}` failure: {error}"
            ));
        }
    }

    fn agent_delegation_grant(
        &self,
        ticket: &str,
    ) -> Result<yunxi_protocol::AgentDelegationGrant, yunxi_protocol::AgentProtocolError> {
        yunxi_protocol::AgentDelegationGrant::new(
            WorkspaceGrant::read_write(&self.cwd),
            &self.agent_session_id,
            ticket,
            self.agent_budget,
        )
    }
}

impl ToolBroker for SpineToolBroker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        Ok(tool_loop::catalog_with_skills(
            self.shell_capability.is_some(),
            self.patch_capability.is_some(),
            self.files_capability.is_some(),
            self.multi_agent_capability.is_some(),
            &self.mcp_tools,
            &self.skill_tools,
        )
        .unwrap_or_else(|| ToolCatalog::new(Vec::new()).expect("empty tool catalog is valid")))
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        let action = self
            .decode(&request)
            .map_err(|error| ToolError::new("invalid_tool_call", error, false))?;
        if requires_approval(&action) {
            return Err(ToolError::new(
                "approval_required",
                format!(
                    "approval is required before executing {}",
                    request.call().name()
                ),
                false,
            ));
        }
        let ticket = self.next_ticket();
        Ok(self.execute_action(&action, &ticket))
    }

    fn execute_approved(
        &mut self,
        request: ToolRequest<'_>,
        approval: &ToolApprovalDecision,
        _cancellation: &CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        let action = self
            .decode(&request)
            .map_err(|error| ToolError::new("invalid_tool_call", error, false))?;
        let ticket = match approval.state() {
            ToolApprovalState::Approved { ticket } => ticket.as_str(),
            ToolApprovalState::Denied { .. } => {
                return Err(ToolError::new(
                    "approval_required",
                    "a denied tool call cannot be executed",
                    false,
                ));
            }
        };
        Ok(self.execute_action(&action, ticket))
    }

    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        let action = match self.decode(request) {
            Ok(action) => action,
            Err(message) => {
                return Ok(ToolDecision::Reject {
                    code: "invalid_arguments".to_string(),
                    message,
                });
            }
        };
        if matches!(action, ToolAction::Skill { .. }) {
            return Ok(ToolDecision::Reject {
                code: "skill_tool_unavailable".to_string(),
                message: "skill tools are metadata-only declarations".to_string(),
            });
        }
        if !requires_approval(&action) {
            return Ok(ToolDecision::Execute);
        }
        self.approval_request(request, &action)
            .map(ToolDecision::RequestApproval)
    }
}

impl ToolApprovalPolicy for SpineToolBroker {
    fn decide(&mut self, request: &ToolRequest<'_>) -> Result<ToolDecision, ToolError> {
        <Self as ToolBroker>::decide(self, request)
    }
}

fn requires_approval(action: &ToolAction) -> bool {
    matches!(
        action,
        ToolAction::Shell { .. }
            | ToolAction::Patch { .. }
            | ToolAction::Mcp { .. }
            | ToolAction::AgentSpawn { .. }
            | ToolAction::AgentMessage { .. }
    )
}

fn requested_grants(action: &ToolAction, broker: &SpineToolBroker) -> Vec<GrantKind> {
    match action {
        ToolAction::Shell { .. } => vec![GrantKind::Approval, GrantKind::WorkspaceRead],
        ToolAction::Patch { .. } => vec![
            GrantKind::Approval,
            GrantKind::WorkspaceRead,
            GrantKind::WorkspaceWrite,
        ],
        ToolAction::Mcp { .. } => {
            let mut grants = vec![GrantKind::Approval];
            if broker.mcp_network_grant.is_some() {
                grants.push(GrantKind::Network);
            }
            if !broker.mcp_secret_grant.is_empty() {
                grants.push(GrantKind::Secret);
            }
            grants
        }
        ToolAction::AgentSpawn { .. } | ToolAction::AgentMessage { .. } => vec![
            GrantKind::Approval,
            GrantKind::WorkspaceRead,
            GrantKind::WorkspaceWrite,
            GrantKind::AgentDelegation,
        ],
        ToolAction::FileSearch { .. }
        | ToolAction::FileRead { .. }
        | ToolAction::AgentList
        | ToolAction::AgentInterrupt { .. }
        | ToolAction::Skill { .. } => Vec::new(),
    }
}

fn serialized_outcome<T: Serialize>(value: T, invalid_message: &str) -> ToolResultOutcome {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| ToolResultOutcome::completed(value).ok())
        .unwrap_or_else(|| completed_rejection("invalid_tool_result", invalid_message))
}

fn completed_rejection(code: &str, message: impl Into<String>) -> ToolResultOutcome {
    ToolResultOutcome::rejected(code, message.into())
        .unwrap_or_else(|_| ToolResultOutcome::cancelled("tool result was invalid").unwrap())
}

fn failed_outcome(code: &str, message: impl Into<String>, retryable: bool) -> ToolResultOutcome {
    ToolResultOutcome::failed(code, message.into(), retryable)
        .unwrap_or_else(|_| ToolResultOutcome::cancelled("tool failure was invalid").unwrap())
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
