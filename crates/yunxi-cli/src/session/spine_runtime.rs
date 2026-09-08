//! The default CLI adapter for the Rust Agent spine.
//!
//! The spine owns turn state, context assembly, tool approval, and bounded
//! looping. This module only translates the existing CLI capability contracts
//! into spine traits; optional capabilities still execute in isolated plugin
//! processes through the shared Host handle.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use yunxi_agent_spine::{
    Agent, AgentConfig, AgentError, AgentTurnOutcome, CancellationToken, EventSink,
    ToolApprovalPolicy, ToolBroker, ToolDecision, ToolError, ToolRequest,
};
use yunxi_kernel::PluginCommand;
use yunxi_plugin_host::PluginCallError;
use yunxi_protocol::{
    ActionGrant, AgentBudget, CapabilityDescriptor, ChatMessage, GrantKind, McpToolCallRequest,
    NetworkGrant, SecretGrant, SkillActionRequest, ToolApprovalDecision, ToolApprovalRequest,
    ToolApprovalState, ToolCatalog, ToolResultOutcome, WorkspaceGrant,
};
use yunxi_tool_skills::{SkillActionError, SkillActionExecutor};

use super::child_agent::{ChildAgentFailure, ChildAgentRuntime};
use super::spine_adapter::{
    ProcessPluginContextAssembler, ProcessPluginHostHandle, ProcessPluginModelProvider,
};
use super::tool_loop::{self, ToolAction};
use super::{ChatFailure, call_lost_route};
use crate::args::{ApprovalMode, SandboxMode};

/// Inputs captured once while the process host is being assembled.
pub(crate) struct SpineRuntimeConfig {
    pub host: ProcessPluginHostHandle,
    pub model_capability: CapabilityDescriptor,
    pub model: String,
    pub cwd: PathBuf,
    pub shell_capability: Option<CapabilityDescriptor>,
    pub patch_capability: Option<CapabilityDescriptor>,
    pub files_capability: Option<CapabilityDescriptor>,
    pub mcp_capability: Option<CapabilityDescriptor>,
    pub mcp_tools: Vec<tool_loop::McpToolBinding>,
    pub mcp_network_grant: Option<NetworkGrant>,
    pub mcp_secret_grant: SecretGrant,
    pub skill_tools: Vec<tool_loop::SkillToolBinding>,
    pub skill_action_executor: Option<SkillActionExecutor>,
    pub multi_agent_capability: Option<CapabilityDescriptor>,
    pub agent_session_id: String,
    pub agent_budget: AgentBudget,
    pub child_model_command: PluginCommand,
    pub child_model_required_grants: Vec<GrantKind>,
    pub child_model_response_timeout: Duration,
    pub approval: ApprovalMode,
    pub sandbox: SandboxMode,
}

type SpineAgent = Agent<ProcessPluginModelProvider, ProcessPluginContextAssembler, SpineToolBroker>;

/// Owns one approval-aware spine instance and its small outer-session bridge.
pub(crate) struct SpineController {
    agent: SpineAgent,
    notices: Arc<Mutex<Vec<String>>>,
    prompt: Option<String>,
    next_approval_ticket: u64,
}

impl SpineController {
    pub(crate) fn new(config: SpineRuntimeConfig) -> Result<Self, AgentError> {
        let notices = Arc::new(Mutex::new(Vec::new()));
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

    pub(crate) fn start_streaming<S: EventSink>(
        &mut self,
        user_message: ChatMessage,
        seed: Vec<ChatMessage>,
        cancellation: &CancellationToken,
        sink: &mut S,
    ) -> Result<AgentTurnOutcome, AgentError> {
        self.agent.reset_conversation(seed)?;
        self.prompt = Some(user_message.content().to_string());
        self.agent
            .run_turn_with_approval_streaming(user_message, cancellation, sink)
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

    pub(crate) fn resolve_streaming<S: EventSink>(
        &mut self,
        approved: bool,
        cancellation: &CancellationToken,
        sink: &mut S,
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
            ToolApprovalState::denied("the user denied this tool call")
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
            .approve_pending_tool_streaming(decision, cancellation, sink)
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
        std::mem::take(&mut *lock_or_recover(&self.notices))
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
    model: String,
    cwd: PathBuf,
    shell_capability: Option<CapabilityDescriptor>,
    patch_capability: Option<CapabilityDescriptor>,
    files_capability: Option<CapabilityDescriptor>,
    mcp_capability: Option<CapabilityDescriptor>,
    mcp_tools: Vec<tool_loop::McpToolBinding>,
    mcp_network_grant: Option<NetworkGrant>,
    mcp_secret_grant: SecretGrant,
    skill_tools: Vec<tool_loop::SkillToolBinding>,
    skill_action_executor: Option<SkillActionExecutor>,
    multi_agent_capability: Option<CapabilityDescriptor>,
    agent_session_id: String,
    agent_budget: AgentBudget,
    child_model_command: PluginCommand,
    child_model_required_grants: Vec<GrantKind>,
    child_model_response_timeout: Duration,
    notices: Arc<Mutex<Vec<String>>>,
    next_action_ticket: u64,
    approval: ApprovalMode,
    sandbox: SandboxMode,
}

impl SpineToolBroker {
    fn new(config: SpineRuntimeConfig, notices: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            host: config.host,
            model_capability: config.model_capability,
            model: config.model,
            cwd: config.cwd,
            shell_capability: config.shell_capability,
            patch_capability: config.patch_capability,
            files_capability: config.files_capability,
            mcp_capability: config.mcp_capability,
            mcp_tools: config.mcp_tools,
            mcp_network_grant: config.mcp_network_grant,
            mcp_secret_grant: config.mcp_secret_grant,
            skill_tools: config.skill_tools,
            skill_action_executor: config.skill_action_executor,
            multi_agent_capability: config.multi_agent_capability,
            agent_session_id: config.agent_session_id,
            agent_budget: config.agent_budget,
            child_model_command: config.child_model_command,
            child_model_required_grants: config.child_model_required_grants,
            child_model_response_timeout: config.child_model_response_timeout,
            approval: config.approval,
            sandbox: config.sandbox,
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

    fn execute_action(
        &mut self,
        action: &ToolAction,
        ticket: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
        if cancellation.is_cancelled() {
            return cancelled_outcome(cancellation_reason(cancellation));
        }
        match action {
            ToolAction::Skill { binding, arguments } => {
                self.execute_skill(binding, arguments, ticket, cancellation)
            }
            ToolAction::Shell {
                command,
                timeout_millis,
            } => self.execute_shell(command, *timeout_millis, ticket, cancellation),
            ToolAction::Patch {
                patch,
                timeout_millis,
            } => self.execute_patch(patch, *timeout_millis, ticket, cancellation),
            ToolAction::FileSearch { query, path } => {
                self.execute_file_search(query, path, cancellation)
            }
            ToolAction::FileRead { path } => self.execute_file_read(path, cancellation),
            ToolAction::Mcp { binding, arguments } => {
                self.execute_mcp(binding, arguments, ticket, cancellation)
            }
            ToolAction::AgentSpawn {
                task,
                name,
                parent_id,
                requested_grants,
            } => self.execute_agent_spawn(
                task,
                name.as_deref(),
                parent_id.as_deref(),
                requested_grants,
                ticket,
                cancellation,
            ),
            ToolAction::AgentList => self.execute_agent_list(ticket, cancellation),
            ToolAction::AgentMessage { agent_id, message } => {
                self.execute_agent_message(agent_id, message, ticket, cancellation)
            }
            ToolAction::AgentInterrupt {
                agent_id,
                recursive,
            } => self.execute_agent_interrupt(agent_id, *recursive, ticket, cancellation),
        }
    }

    fn invoke_cancellable<Request, Response>(
        &self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
        cancellation: &CancellationToken,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.host.invoke_streaming(
            capability,
            operation,
            payload,
            || cancellation.is_cancelled(),
            |_| Ok(()),
        )
    }

    fn execute_skill(
        &mut self,
        binding: &tool_loop::SkillToolBinding,
        arguments: &Value,
        ticket: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
        if !binding.executable() {
            return completed_rejection(
                "skill_tool_unavailable",
                format!(
                    "Skill `{}` declared tool `{}` as metadata only",
                    binding.skill_id(),
                    binding.remote_name()
                ),
            );
        }
        let Some(executor) = self.skill_action_executor.clone() else {
            return completed_rejection(
                "skill_action_disabled",
                "executable Skill actions are not enabled",
            );
        };
        if binding.requires_workspace_write() && self.sandbox == SandboxMode::ReadOnly {
            return completed_rejection(
                "sandbox_read_only",
                "Skill action requires workspace write access while --sandbox read-only is active",
            );
        }
        let request = match SkillActionRequest::new(
            binding.skill_id(),
            binding.remote_name(),
            arguments.clone(),
        ) {
            Ok(request) => request,
            Err(error) => {
                return completed_rejection("invalid_skill_action_request", error.to_string());
            }
        };
        let workspace = if binding.requires_workspace_write() {
            WorkspaceGrant::read_write(&self.cwd).with_workspace_write()
        } else {
            WorkspaceGrant::read_only(&self.cwd)
        };
        let mut grant = ActionGrant::approved(workspace, &self.cwd, ticket).with_limits(
            yunxi_protocol::MAX_SKILL_ACTION_TIMEOUT_MILLIS,
            yunxi_protocol::MAX_SKILL_ACTION_OUTPUT_BYTES,
        );
        if binding.requires_workspace_write() {
            grant = grant.with_write(true);
        }
        match executor.execute_with_cancellation(&request, &grant, || cancellation.is_cancelled()) {
            Ok(execution) => {
                serialized_outcome(execution, "Skill action returned an invalid bounded result")
            }
            Err(error) => {
                self.notice(format!(
                    "model Skill tool `{}` failed: {error}",
                    binding.model_name()
                ));
                skill_action_error_outcome(error)
            }
        }
    }

    fn execute_shell(
        &mut self,
        command: &str,
        timeout: u64,
        ticket: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
        let Some(capability) = self.shell_capability.clone() else {
            return failed_outcome(
                "tool_unavailable",
                "shell capability is disabled or unavailable",
                false,
            );
        };
        let grant = ActionGrant::approved(WorkspaceGrant::read_only(&self.cwd), &self.cwd, ticket)
            .with_limits(timeout, 64 * 1024);
        match self.invoke_cancellable::<_, yunxi_protocol::ShellExecuteResult>(
            &capability,
            yunxi_protocol::TOOL_SHELL_EXECUTE_OPERATION,
            &yunxi_protocol::ShellExecuteRequest::new(grant, command),
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "shell returned an invalid result"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::SHELL_TOOL_NAME,
                error,
                "tool_unavailable",
                cancellation,
                |broker| broker.shell_capability = None,
            ),
        }
    }

    fn execute_patch(
        &mut self,
        patch: &str,
        timeout: u64,
        ticket: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
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
        match self.invoke_cancellable::<_, yunxi_protocol::PatchApplyResult>(
            &capability,
            yunxi_protocol::TOOL_PATCH_APPLY_OPERATION,
            &yunxi_protocol::PatchApplyRequest::new(grant, patch),
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "patch returned an invalid result"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::PATCH_TOOL_NAME,
                error,
                "tool_unavailable",
                cancellation,
                |broker| broker.patch_capability = None,
            ),
        }
    }

    fn execute_file_search(
        &mut self,
        query: &str,
        path: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
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
        match self.invoke_cancellable::<_, yunxi_protocol::FileSearchResult>(
            &capability,
            yunxi_protocol::TOOL_FILES_SEARCH_OPERATION,
            &request,
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "file search returned an invalid result"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::FILE_SEARCH_TOOL_NAME,
                error,
                "file_tool_unavailable",
                cancellation,
                |broker| broker.files_capability = None,
            ),
        }
    }

    fn execute_file_read(
        &mut self,
        path: &str,
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
        let Some(capability) = self.files_capability.clone() else {
            return failed_outcome(
                "file_tool_unavailable",
                "file capability is disabled or unavailable",
                false,
            );
        };
        let request =
            yunxi_protocol::FileReadRequest::new(WorkspaceGrant::read_only(&self.cwd), path);
        match self.invoke_cancellable::<_, yunxi_protocol::FileReadResult>(
            &capability,
            yunxi_protocol::TOOL_FILES_READ_OPERATION,
            &request,
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "file read returned an invalid result"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::FILE_READ_TOOL_NAME,
                error,
                "file_tool_unavailable",
                cancellation,
                |broker| broker.files_capability = None,
            ),
        }
    }

    fn execute_mcp(
        &mut self,
        binding: &tool_loop::McpToolBinding,
        arguments: &Value,
        ticket: &str,
        cancellation: &CancellationToken,
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
        match self.invoke_cancellable::<_, yunxi_protocol::McpToolCallResult>(
            &capability,
            yunxi_protocol::TOOL_MCP_CALL_OPERATION,
            &request,
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "MCP returned an invalid result"),
            Err(error) => self.cancellable_plugin_error_outcome(
                binding.model_name().as_str(),
                error,
                "mcp_unavailable",
                cancellation,
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

    fn cancellable_plugin_error_outcome<F>(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
        fallback_code: &str,
        cancellation: &CancellationToken,
        disable: F,
    ) -> ToolResultOutcome
    where
        F: FnOnce(&mut Self),
    {
        if cancellation.is_cancelled() {
            if call_lost_route(&error) {
                disable(self);
            }
            let reason = cancellation_reason(cancellation);
            self.notice(format!("model tool `{tool_name}` cancelled: {reason}"));
            cancelled_outcome(reason)
        } else {
            self.plugin_error_outcome(tool_name, error, fallback_code, disable)
        }
    }

    fn notice(&self, message: String) {
        lock_or_recover(&self.notices).push(message);
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
        requested_grants: &[GrantKind],
        ticket: &str,
        cancellation: &CancellationToken,
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
        request = match request.with_requested_child_grants(requested_grants.iter().copied()) {
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
        let spawned = match self.invoke_cancellable::<_, yunxi_protocol::AgentSpawnResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_SPAWN_OPERATION,
            &request,
            cancellation,
        ) {
            Ok(result) => result,
            Err(error) => {
                return self.cancellable_plugin_error_outcome(
                    tool_loop::AGENT_SPAWN_TOOL_NAME,
                    error,
                    "multi_agent_unavailable",
                    cancellation,
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
            cancellation,
        )
    }

    fn execute_agent_message(
        &mut self,
        agent_id: &str,
        message: &str,
        ticket: &str,
        cancellation: &CancellationToken,
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
            cancellation,
        )
    }

    fn execute_agent_list(
        &mut self,
        ticket: &str,
        cancellation: &CancellationToken,
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
        match self.invoke_cancellable::<_, yunxi_protocol::AgentListResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_LIST_OPERATION,
            &yunxi_protocol::AgentListRequest::new(grant),
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "multi-agent list returned invalid data"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::AGENT_LIST_TOOL_NAME,
                error,
                "multi_agent_unavailable",
                cancellation,
                |broker| broker.multi_agent_capability = None,
            ),
        }
    }

    fn execute_agent_interrupt(
        &mut self,
        agent_id: &str,
        recursive: bool,
        ticket: &str,
        cancellation: &CancellationToken,
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
        match self.invoke_cancellable::<_, yunxi_protocol::AgentMutationResult>(
            &capability,
            yunxi_protocol::TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
            &request,
            cancellation,
        ) {
            Ok(result) => serialized_outcome(result, "multi-agent interrupt returned invalid data"),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_loop::AGENT_INTERRUPT_TOOL_NAME,
                error,
                "multi_agent_unavailable",
                cancellation,
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
        cancellation: &CancellationToken,
    ) -> ToolResultOutcome {
        let start =
            match yunxi_protocol::AgentTurnStartRequest::new(grant.clone(), agent_id, message) {
                Ok(request) => request,
                Err(error) => {
                    return failed_outcome("invalid_agent_request", error.to_string(), false);
                }
            };
        let started = match self.invoke_cancellable::<_, yunxi_protocol::AgentTurnStartResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_START_OPERATION,
            &start,
            cancellation,
        ) {
            Ok(result) => result,
            Err(error) => {
                return self.cancellable_plugin_error_outcome(
                    tool_name,
                    error,
                    "multi_agent_unavailable",
                    cancellation,
                    |broker| broker.multi_agent_capability = None,
                );
            }
        };
        let child_grants = started.agent().child_grants().to_vec();
        let runtime = self.child_agent_runtime();
        let model = self.model.clone();
        let child_cancellation = cancellation.clone();
        let reply = match runtime.run(
            started.transcript(),
            message,
            &model,
            &child_grants,
            move || child_cancellation.is_cancelled(),
            |_| Ok(()),
        ) {
            Ok(reply) => reply,
            Err(error) => {
                if cancellation.is_cancelled() || error.code() == "cancelled" {
                    self.interrupt_cancelled_child(capability, grant, agent_id);
                    return cancelled_outcome(cancellation_reason(cancellation));
                }
                self.report_child_agent_failure(capability, grant, agent_id, &error);
                return failed_outcome(error.code(), error.message(), false);
            }
        };
        let complete =
            match yunxi_protocol::AgentTurnCompleteRequest::new(grant.clone(), agent_id, &reply) {
                Ok(request) => request,
                Err(error) => {
                    let failure = ChildAgentFailure::new("child_reply_invalid", error.to_string());
                    self.report_child_agent_failure(capability, grant, agent_id, &failure);
                    return failed_outcome(failure.code(), failure.message(), false);
                }
            };
        match self.invoke_cancellable::<_, yunxi_protocol::AgentMutationResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
            &complete,
            cancellation,
        ) {
            Ok(result) => serialized_outcome(
                serde_json::json!({
                    "agent": result.agents().first(),
                    "reply": reply,
                    "events": result.events(),
                }),
                "multi-agent completion returned invalid data",
            ),
            Err(error) => self.cancellable_plugin_error_outcome(
                tool_name,
                error,
                "multi_agent_unavailable",
                cancellation,
                |broker| broker.multi_agent_capability = None,
            ),
        }
    }

    fn interrupt_cancelled_child(
        &mut self,
        capability: &CapabilityDescriptor,
        grant: &yunxi_protocol::AgentDelegationGrant,
        agent_id: &str,
    ) {
        let request =
            match yunxi_protocol::AgentInterruptRequest::new(grant.clone(), agent_id, true) {
                Ok(request) => request,
                Err(error) => {
                    self.notice(format!(
                        "failed to encode cancellation for child agent `{agent_id}`: {error}"
                    ));
                    return;
                }
            };
        if let Err(error) = self.host.invoke::<_, yunxi_protocol::AgentMutationResult>(
            capability,
            yunxi_protocol::TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
            &request,
        ) {
            if call_lost_route(&error) {
                self.multi_agent_capability = None;
            }
            self.notice(format!(
                "failed to persist cancellation for child agent `{agent_id}`: {error}"
            ));
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
        .and_then(|grant| grant.with_allowed_child_grants(self.available_child_grants()))
    }

    fn available_child_grants(&self) -> Vec<GrantKind> {
        let patch_available =
            self.patch_capability.is_some() && self.sandbox != SandboxMode::ReadOnly;
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
        cancellation: &CancellationToken,
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
        Ok(self.execute_action(&action, &ticket, cancellation))
    }

    fn execute_approved(
        &mut self,
        request: ToolRequest<'_>,
        approval: &ToolApprovalDecision,
        cancellation: &CancellationToken,
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
        Ok(self.execute_action(&action, ticket, cancellation))
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
        if matches!(
            action,
            ToolAction::Skill { ref binding, .. }
                if !binding.executable() || self.skill_action_executor.is_none()
        ) {
            return Ok(ToolDecision::Reject {
                code: "skill_tool_unavailable".to_string(),
                message: "Skill tool is metadata-only or executable actions are disabled"
                    .to_string(),
            });
        }
        if self.sandbox == SandboxMode::ReadOnly
            && (matches!(action, ToolAction::Patch { .. })
                || matches!(
                    action,
                    ToolAction::Skill { ref binding, .. }
                        if binding.requires_workspace_write()
                ))
        {
            return Ok(ToolDecision::Reject {
                code: "sandbox_read_only".to_string(),
                message: "workspace-write execution is disabled by --sandbox read-only".to_string(),
            });
        }
        if !requires_approval(&action) {
            return Ok(ToolDecision::Execute);
        }
        if self.approval == ApprovalMode::Never {
            return Ok(ToolDecision::Reject {
                code: "approval_disabled".to_string(),
                message: "model action rejected because --approval never is active".to_string(),
            });
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
    match action {
        ToolAction::Shell { .. }
        | ToolAction::Patch { .. }
        | ToolAction::Mcp { .. }
        | ToolAction::AgentSpawn { .. }
        | ToolAction::AgentMessage { .. } => true,
        ToolAction::Skill { binding, .. } => binding.executable(),
        ToolAction::FileSearch { .. }
        | ToolAction::FileRead { .. }
        | ToolAction::AgentList
        | ToolAction::AgentInterrupt { .. } => false,
    }
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
        ToolAction::AgentSpawn {
            requested_grants, ..
        } => {
            let mut grants = vec![GrantKind::Approval, GrantKind::AgentDelegation];
            for grant in requested_grants.iter().copied() {
                if !grants.contains(&grant) {
                    grants.push(grant);
                }
            }
            grants
        }
        ToolAction::AgentMessage { .. } => {
            vec![GrantKind::Approval, GrantKind::AgentDelegation]
        }
        ToolAction::Skill { binding, .. } => {
            let mut grants = vec![GrantKind::Approval, GrantKind::WorkspaceRead];
            if binding.requires_workspace_write() {
                grants.push(GrantKind::WorkspaceWrite);
            }
            grants
        }
        ToolAction::FileSearch { .. }
        | ToolAction::FileRead { .. }
        | ToolAction::AgentList
        | ToolAction::AgentInterrupt { .. } => Vec::new(),
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

fn cancelled_outcome(reason: impl Into<String>) -> ToolResultOutcome {
    ToolResultOutcome::cancelled(reason.into())
        .unwrap_or_else(|_| ToolResultOutcome::cancelled("tool call was cancelled").unwrap())
}

fn cancellation_reason(cancellation: &CancellationToken) -> String {
    cancellation
        .reason()
        .unwrap_or_else(|| "tool call was cancelled".to_string())
}

fn skill_action_error_outcome(error: SkillActionError) -> ToolResultOutcome {
    match error {
        error @ (SkillActionError::ActionsDisabled
        | SkillActionError::SkillNotFound { .. }
        | SkillActionError::ActionNotDeclared { .. }
        | SkillActionError::WorkspaceWriteRequired
        | SkillActionError::ForbiddenGrant
        | SkillActionError::Grant(_)
        | SkillActionError::Request(_)) => {
            completed_rejection("skill_action_rejected", error.to_string())
        }
        other => failed_outcome("skill_action_failed", other.to_string(), false),
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
