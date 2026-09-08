//! Shared execution boundary for isolated child agents.
//!
//! A child receives a model process of its own, while file and patch calls are
//! routed through the already-supervised parent host. The model process never
//! receives the child's workspace grants during its handshake: those grants
//! authorize tools, not the model provider.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use yunxi_agent_spine::{
    Agent, AgentConfig, AgentError, CancellationToken as SpineCancellationToken,
    ConversationContextAssembler, EventSink, EventSinkError, ToolBroker, ToolError, ToolRequest,
};
use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_model_openai::MODEL_PLUGIN_ID;
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    ActionGrant, AgentStreamEvent, AgentTranscriptEntry, AgentTranscriptRole, CapabilityDescriptor,
    ChatMessage, FileReadRequest, FileReadResult, FileSearchRequest, FileSearchResult, GrantKind,
    ModelStreamEvent, PatchApplyRequest, PatchApplyResult, TOOL_FILES_READ_OPERATION,
    TOOL_FILES_SEARCH_OPERATION, TOOL_PATCH_APPLY_OPERATION, ToolCatalog, ToolResultOutcome,
    WorkspaceGrant,
};

use super::spine_adapter::{ProcessPluginHostHandle, ProcessPluginModelProvider};
use super::{HANDSHAKE_TIMEOUT, WRITE_TIMEOUT, tool_loop};
use crate::args::SandboxMode;
use yunxi_multi_agent::{ChildToolCatalog, ChildToolKind};

const CHILD_SYSTEM_PROMPT: &str = "You are an isolated YunXi child agent. Complete only the delegated task. Use only the tools supplied in this turn. Never claim actions you did not perform.";
const CHILD_PATCH_OUTPUT_BYTES: usize = 64 * 1024;
const CHILD_ERROR_MESSAGE_BYTES: usize = 3000;

/// Immutable dependencies captured when a child job is prepared.
#[derive(Clone)]
pub(crate) struct ChildAgentRuntime {
    host: ProcessPluginHostHandle,
    cwd: PathBuf,
    files_capability: Option<CapabilityDescriptor>,
    patch_capability: Option<CapabilityDescriptor>,
    sandbox: SandboxMode,
    child_model_command: PluginCommand,
    child_model_required_grants: Vec<GrantKind>,
    child_model_response_timeout: Duration,
    model_capability: CapabilityDescriptor,
}

impl ChildAgentRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        host: ProcessPluginHostHandle,
        cwd: PathBuf,
        files_capability: Option<CapabilityDescriptor>,
        patch_capability: Option<CapabilityDescriptor>,
        sandbox: SandboxMode,
        child_model_command: PluginCommand,
        child_model_required_grants: Vec<GrantKind>,
        child_model_response_timeout: Duration,
        model_capability: CapabilityDescriptor,
    ) -> Self {
        Self {
            host,
            cwd,
            files_capability,
            patch_capability,
            sandbox,
            child_model_command,
            child_model_required_grants,
            child_model_response_timeout,
            model_capability,
        }
    }

    /// Runs one child turn with a fresh model process and a bounded tool set.
    ///
    /// `is_cancelled` is a pull-based bridge for callers whose cancellation
    /// token belongs to another runtime (the Web worker is one example). It is
    /// owned by an `Arc` so both the model and tool boundaries can observe it
    /// without creating a polling thread.
    pub(crate) fn run<IsCancelled, OnEvent>(
        &self,
        transcript: &[AgentTranscriptEntry],
        fallback_message: &str,
        model: &str,
        tool_grants: &[GrantKind],
        is_cancelled: IsCancelled,
        on_event: OnEvent,
    ) -> Result<String, ChildAgentFailure>
    where
        IsCancelled: Fn() -> bool + Send + Sync + 'static,
        OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
    {
        let cancellation_probe: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(is_cancelled);
        if cancellation_probe() {
            return Err(ChildAgentFailure::cancelled());
        }

        yunxi_multi_agent::ChildWorkerSpec::new(model, tool_grants.iter().copied())
            .map_err(|error| ChildAgentFailure::new("child_invalid_spec", error.to_string()))?;
        let child_catalog = ChildToolCatalog::from_grants(tool_grants)
            .map_err(|error| ChildAgentFailure::new("child_grant_invalid", error.to_string()))?;
        validate_child_grants(
            &child_catalog,
            self.files_capability.is_some(),
            self.patch_capability.is_some(),
            self.sandbox,
        )?;

        let child_tools = ChildToolBroker::new(
            self.host.clone(),
            self.cwd.clone(),
            self.files_capability.clone(),
            self.patch_capability.clone(),
            self.sandbox,
            child_catalog,
            cancellation_probe.clone(),
        );

        let model_id = PluginId::new(MODEL_PLUGIN_ID).map_err(|error| {
            ChildAgentFailure::new("child_model_unavailable", error.to_string())
        })?;
        let model_command = self
            .child_model_command
            .clone()
            .env("YUNXI_AGENT_MODEL", model.to_string());
        let launch = PluginLaunch::new(model_id, model_command)
            .with_display_name("Isolated child chat model")
            .with_handshake_timeout(HANDSHAKE_TIMEOUT)
            .with_io_timeouts(Some(self.child_model_response_timeout), Some(WRITE_TIMEOUT))
            // These are the model provider's own requirements. Child tool
            // grants stay in ChildToolBroker and never enter this handshake.
            .with_required_grants(self.child_model_required_grants.iter().copied())
            .with_expected_capabilities([self.model_capability.clone()]);

        let mut process_host = ProcessPluginHost::new();
        if let Err(error) = process_host.launch(launch) {
            process_host.shutdown();
            return Err(ChildAgentFailure::new(
                "child_model_unavailable",
                error.to_string(),
            ));
        }
        let model_host = ProcessPluginHostHandle::new(process_host);
        let model_provider =
            ProcessPluginModelProvider::new(model_host.clone(), self.model_capability.clone())
                .with_cancellation_probe(cancellation_probe.clone());
        let config =
            match AgentConfig::default().with_turn_timeout(self.child_model_response_timeout) {
                Ok(config) => config,
                Err(error) => {
                    model_host.borrow_mut().shutdown();
                    return Err(ChildAgentFailure::from_agent_error(error, false));
                }
            };
        let mut agent = match Agent::new(
            "yunxi-child",
            model_provider,
            ConversationContextAssembler,
            child_tools,
            config,
        ) {
            Ok(agent) => agent,
            Err(error) => {
                model_host.borrow_mut().shutdown();
                return Err(ChildAgentFailure::from_agent_error(error, false));
            }
        };

        let (seed, current_message) = child_messages(transcript, fallback_message);
        if let Err(error) = agent.reset_conversation(seed) {
            model_host.borrow_mut().shutdown();
            return Err(ChildAgentFailure::from_agent_error(error, false));
        }

        let spine_cancellation = SpineCancellationToken::new();
        if cancellation_probe() {
            spine_cancellation.cancel("child agent turn was cancelled");
        }
        let mut forwarder = ChildEventForwarder::new(
            on_event,
            spine_cancellation.clone(),
            cancellation_probe.clone(),
        );
        let result = agent.run_turn_streaming(
            ChatMessage::user(current_message),
            &spine_cancellation,
            &mut forwarder,
        );
        let externally_cancelled = cancellation_probe();
        model_host.borrow_mut().shutdown();

        match result {
            Ok(_result) if externally_cancelled || spine_cancellation.is_cancelled() => {
                Err(ChildAgentFailure::cancelled())
            }
            Ok(result) => {
                if result.content().trim().is_empty() {
                    Err(ChildAgentFailure::new(
                        "child_empty_response",
                        "isolated child agent returned an empty response",
                    ))
                } else {
                    Ok(result.content().to_string())
                }
            }
            Err(error) => Err(ChildAgentFailure::from_agent_error(
                error,
                externally_cancelled,
            )),
        }
    }
}

/// A bounded error safe to persist in the multi-agent graph.
pub(crate) struct ChildAgentFailure {
    code: String,
    message: String,
}

impl ChildAgentFailure {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = normalize_failure_code(code.into());
        let message = bounded_message(message.into());
        Self { code, message }
    }

    pub(crate) fn cancelled() -> Self {
        Self::new("cancelled", "child agent turn was cancelled")
    }

    fn from_agent_error(error: AgentError, externally_cancelled: bool) -> Self {
        if externally_cancelled || error.is_cancelled() {
            return Self::cancelled();
        }
        // Keep the established multi-agent error vocabulary at the parent
        // boundary while retaining the provider's detail in the message.
        let code = match &error {
            AgentError::Model(_) => "child_model_failed",
            _ => error.code(),
        };
        Self::new(code, error.message())
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

fn child_messages(
    transcript: &[AgentTranscriptEntry],
    fallback_message: &str,
) -> (Vec<ChatMessage>, String) {
    let (history, current) = match transcript.last() {
        Some(entry) if entry.role() == AgentTranscriptRole::User => (
            &transcript[..transcript.len().saturating_sub(1)],
            entry.content(),
        ),
        _ => (transcript, fallback_message),
    };
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(ChatMessage::system(CHILD_SYSTEM_PROMPT));
    messages.extend(history.iter().map(|entry| match entry.role() {
        AgentTranscriptRole::User => ChatMessage::user(entry.content()),
        AgentTranscriptRole::Assistant => ChatMessage::assistant(entry.content()),
    }));
    (messages, current.to_string())
}

fn validate_child_grants(
    catalog: &ChildToolCatalog,
    files_available: bool,
    patch_available: bool,
    sandbox: SandboxMode,
) -> Result<(), ChildAgentFailure> {
    let read = catalog.allows(ChildToolKind::WorkspaceRead);
    let write = catalog.allows(ChildToolKind::WorkspacePatch);
    if write && !read {
        return Err(ChildAgentFailure::new(
            "child_grant_invalid",
            "workspace_write requires workspace_read",
        ));
    }
    if read && !files_available && !patch_available {
        return Err(ChildAgentFailure::new(
            "child_tool_unavailable",
            "workspace_read was granted but no file or patch capability is available",
        ));
    }
    if write && (sandbox == SandboxMode::ReadOnly || !patch_available) {
        return Err(ChildAgentFailure::new(
            "child_write_unavailable",
            "workspace_write is unavailable in the current sandbox or plugin set",
        ));
    }
    Ok(())
}

struct ChildToolBroker {
    host: ProcessPluginHostHandle,
    cwd: PathBuf,
    files_capability: Option<CapabilityDescriptor>,
    patch_capability: Option<CapabilityDescriptor>,
    sandbox: SandboxMode,
    catalog: ChildToolCatalog,
    cancellation_probe: Arc<dyn Fn() -> bool + Send + Sync>,
    next_ticket: u64,
}

impl ChildToolBroker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        host: ProcessPluginHostHandle,
        cwd: PathBuf,
        files_capability: Option<CapabilityDescriptor>,
        patch_capability: Option<CapabilityDescriptor>,
        sandbox: SandboxMode,
        catalog: ChildToolCatalog,
        cancellation_probe: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            host,
            cwd,
            files_capability,
            patch_capability,
            sandbox,
            catalog,
            cancellation_probe,
            next_ticket: 1,
        }
    }

    fn allows(&self, tool: ChildToolKind) -> bool {
        self.catalog.allows(tool)
    }

    fn observe_cancellation(&self, cancellation: &SpineCancellationToken) -> bool {
        if (self.cancellation_probe)() {
            cancellation.cancel("child agent turn was cancelled");
        }
        cancellation.is_cancelled()
    }

    fn invoke<Request, Response>(
        &self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
        cancellation: &SpineCancellationToken,
    ) -> Result<Response, ToolError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        if self.observe_cancellation(cancellation) {
            return Err(cancelled_tool_error());
        }
        let probe = self.cancellation_probe.clone();
        let token = cancellation.clone();
        let result = self.host.invoke_streaming(
            capability,
            operation,
            payload,
            move || {
                if probe() {
                    token.cancel("child agent turn was cancelled");
                }
                token.is_cancelled()
            },
            |_| Ok(()),
        );
        if self.observe_cancellation(cancellation) {
            return Err(cancelled_tool_error());
        }
        result.map_err(map_plugin_error)
    }

    fn completed<T: Serialize>(
        &self,
        value: T,
        invalid_message: &'static str,
    ) -> Result<ToolResultOutcome, ToolError> {
        let value = serde_json::to_value(value)
            .map_err(|error| ToolError::new("invalid_tool_result", error.to_string(), false))?;
        ToolResultOutcome::completed(value)
            .map_err(|_error| ToolError::new("invalid_tool_result", invalid_message, false))
    }
}

impl ToolBroker for ChildToolBroker {
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        Ok(child_tool_catalog(
            &self.catalog,
            self.files_capability.is_some(),
            self.patch_capability.is_some(),
            self.sandbox,
        ))
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        cancellation: &SpineCancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        if self.observe_cancellation(cancellation) {
            return Err(cancelled_tool_error());
        }
        let action = tool_loop::decode_call_with_skills(request.call(), false, &[], &[])
            .map_err(|error| ToolError::new("invalid_tool_call", error.to_string(), false))?;
        match action {
            tool_loop::ToolAction::FileSearch { query, path } => {
                if !self.allows(ChildToolKind::WorkspaceSearch) || self.files_capability.is_none() {
                    return Err(child_tool_denied("workspace read is not granted"));
                }
                let capability = self
                    .files_capability
                    .as_ref()
                    .expect("file capability checked above");
                let payload = FileSearchRequest::new(
                    WorkspaceGrant::read_only(self.cwd.clone()),
                    self.cwd.join(path),
                    query,
                );
                let result = self.invoke::<_, FileSearchResult>(
                    capability,
                    TOOL_FILES_SEARCH_OPERATION,
                    &payload,
                    cancellation,
                )?;
                self.completed(result, "child file search returned an invalid result")
            }
            tool_loop::ToolAction::FileRead { path } => {
                if !self.allows(ChildToolKind::WorkspaceRead) || self.files_capability.is_none() {
                    return Err(child_tool_denied("workspace read is not granted"));
                }
                let capability = self
                    .files_capability
                    .as_ref()
                    .expect("file capability checked above");
                let payload = FileReadRequest::new(
                    WorkspaceGrant::read_only(self.cwd.clone()),
                    self.cwd.join(path),
                );
                let result = self.invoke::<_, FileReadResult>(
                    capability,
                    TOOL_FILES_READ_OPERATION,
                    &payload,
                    cancellation,
                )?;
                self.completed(result, "child file read returned an invalid result")
            }
            tool_loop::ToolAction::Patch {
                patch,
                timeout_millis,
            } => {
                if !self.allows(ChildToolKind::WorkspacePatch)
                    || self.patch_capability.is_none()
                    || self.sandbox == SandboxMode::ReadOnly
                {
                    return Err(child_tool_denied("workspace write is not granted"));
                }
                let capability = self
                    .patch_capability
                    .as_ref()
                    .expect("patch capability checked above");
                let ticket = format!("child-tool-{}-{}", std::process::id(), self.next_ticket);
                self.next_ticket = self.next_ticket.saturating_add(1);
                let grant = ActionGrant::approved(
                    WorkspaceGrant::read_write(self.cwd.clone()).with_workspace_write(),
                    self.cwd.clone(),
                    ticket,
                )
                .with_write(true)
                .with_limits(timeout_millis, CHILD_PATCH_OUTPUT_BYTES);
                let payload = PatchApplyRequest::new(grant, patch);
                let result = self.invoke::<_, PatchApplyResult>(
                    capability,
                    TOOL_PATCH_APPLY_OPERATION,
                    &payload,
                    cancellation,
                )?;
                self.completed(result, "child patch tool returned an invalid result")
            }
            _ => Err(child_tool_denied(
                "the isolated child broker exposes only file and patch tools",
            )),
        }
    }
}

fn child_tool_catalog(
    catalog: &ChildToolCatalog,
    files_available: bool,
    patch_available: bool,
    sandbox: SandboxMode,
) -> ToolCatalog {
    let files_enabled = catalog.allows(ChildToolKind::WorkspaceRead) && files_available;
    let patch_enabled = catalog.allows(ChildToolKind::WorkspacePatch)
        && patch_available
        && sandbox != SandboxMode::ReadOnly;
    tool_loop::catalog_with_skills(false, patch_enabled, files_enabled, false, &[], &[])
        .unwrap_or_else(|| ToolCatalog::new(Vec::new()).expect("empty child tool catalog"))
}

struct ChildEventForwarder<OnEvent> {
    on_event: OnEvent,
    cancellation: SpineCancellationToken,
    cancellation_probe: Arc<dyn Fn() -> bool + Send + Sync>,
    next_tool_index: usize,
}

impl<OnEvent> ChildEventForwarder<OnEvent> {
    fn new(
        on_event: OnEvent,
        cancellation: SpineCancellationToken,
        cancellation_probe: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            on_event,
            cancellation,
            cancellation_probe,
            next_tool_index: 0,
        }
    }

    fn check_cancelled(&self) -> Result<(), EventSinkError> {
        if (self.cancellation_probe)() {
            self.cancellation.cancel("child agent turn was cancelled");
        }
        if self.cancellation.is_cancelled() {
            Err(EventSinkError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl<OnEvent> EventSink for ChildEventForwarder<OnEvent>
where
    OnEvent: FnMut(ModelStreamEvent) -> Result<(), String>,
{
    fn emit(&mut self, event: AgentStreamEvent) -> Result<(), EventSinkError> {
        self.check_cancelled()?;
        match event {
            AgentStreamEvent::TextDelta { delta, .. } => {
                let event = ModelStreamEvent::text_delta(delta)
                    .map_err(|error| EventSinkError::InvalidEvent(error.to_string()))?;
                (self.on_event)(event).map_err(|_| EventSinkError::Closed)
            }
            AgentStreamEvent::ToolStart {
                call_id, tool_name, ..
            } => {
                let index = self.next_tool_index;
                self.next_tool_index = self.next_tool_index.saturating_add(1);
                let event = ModelStreamEvent::tool_call_delta(
                    index,
                    Some(call_id.to_string()),
                    Some(tool_name.to_string()),
                    "{}",
                )
                .map_err(|error| EventSinkError::InvalidEvent(error.to_string()))?;
                (self.on_event)(event).map_err(|_| EventSinkError::Closed)
            }
            AgentStreamEvent::ToolProgress { .. }
            | AgentStreamEvent::ToolResult { .. }
            | AgentStreamEvent::TurnState { .. }
            | AgentStreamEvent::TurnError { .. }
            | AgentStreamEvent::TurnDone { .. } => Ok(()),
        }
    }

    fn emit_terminal(&mut self, _event: AgentStreamEvent) -> Result<(), EventSinkError> {
        // The outer Web/CLI job owns terminal lifecycle events. Avoid emitting
        // a duplicate terminal model event after the child result is known.
        Ok(())
    }
}

fn child_tool_denied(message: impl Into<String>) -> ToolError {
    ToolError::new("child_tool_denied", message, false)
}

fn cancelled_tool_error() -> ToolError {
    ToolError::new("cancelled", "child agent turn was cancelled", false)
}

fn map_plugin_error(error: PluginCallError) -> ToolError {
    match error {
        PluginCallError::Rejected {
            code,
            message,
            retryable,
            ..
        } => ToolError::new(code, message, retryable),
        PluginCallError::Cancelled { message, .. } => ToolError::new("cancelled", message, false),
        other => ToolError::new(
            "child_tool_failed",
            bounded_message(other.to_string()),
            false,
        ),
    }
}

fn normalize_failure_code(value: String) -> String {
    let mut code = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(64)
        .collect::<String>();
    if code.is_empty() {
        code = "child_agent_failed".to_string();
    }
    code
}

fn bounded_message(value: String) -> String {
    let mut bounded = String::new();
    for character in value.replace('\0', " ").chars() {
        if bounded.len() + character.len_utf8() > CHILD_ERROR_MESSAGE_BYTES {
            break;
        }
        bounded.push(character);
    }
    if bounded.trim().is_empty() {
        "isolated child agent turn failed".to_string()
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_catalog_is_the_intersection_of_grants_and_available_plugins() {
        let read_grants =
            ChildToolCatalog::from_grants(&[GrantKind::WorkspaceRead]).expect("read grants");
        let read_only = child_tool_catalog(&read_grants, true, true, SandboxMode::ReadOnly);
        let names = read_only
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                tool_loop::FILE_SEARCH_TOOL_NAME,
                tool_loop::FILE_READ_TOOL_NAME
            ]
        );

        let read_write_grants =
            ChildToolCatalog::from_grants(&[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite])
                .expect("read/write grants");
        let read_write =
            child_tool_catalog(&read_write_grants, true, true, SandboxMode::WorkspaceWrite);
        assert!(
            read_write
                .tools()
                .iter()
                .any(|tool| tool.name().as_str() == tool_loop::PATCH_TOOL_NAME)
        );

        let unavailable =
            child_tool_catalog(&read_write_grants, false, true, SandboxMode::WorkspaceWrite);
        assert_eq!(
            unavailable
                .tools()
                .iter()
                .map(|tool| tool.name().as_str())
                .collect::<Vec<_>>(),
            vec![tool_loop::PATCH_TOOL_NAME]
        );
    }

    #[test]
    fn child_grants_fail_closed_for_unknown_or_escalated_capabilities() {
        assert!(ChildToolCatalog::from_grants(&[GrantKind::Device]).is_err());
        assert!(ChildToolCatalog::from_grants(&[GrantKind::WorkspaceWrite]).is_err());
        let read = ChildToolCatalog::from_grants(&[GrantKind::WorkspaceRead]).expect("read grants");
        let unavailable = validate_child_grants(&read, false, false, SandboxMode::WorkspaceWrite);
        assert!(unavailable.is_err());
    }
}
