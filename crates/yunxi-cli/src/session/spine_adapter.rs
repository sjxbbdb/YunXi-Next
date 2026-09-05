//! Compatibility adapters between the Agent spine and the CLI process host.
//!
//! The production session uses the richer broker in `spine_runtime`; these
//! generic adapters remain reusable seams for external-compatible plugins and
//! are covered by focused integration tests.
#![allow(dead_code)]

use std::cell::{Ref, RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use yunxi_agent_spine::{
    ContextAssembler, ContextAssemblyRequest, ContextError, ConversationContextAssembler,
    ModelError, ModelProvider, ModelRequest, ToolBroker, ToolError, ToolRequest,
};
use yunxi_kernel::KernelSnapshot;
use yunxi_plugin_host::{CapabilityCatalog, PluginCallError, ProcessPluginHost};
use yunxi_protocol::{
    CONTEXT_COMPOSE_OPERATION, CapabilityDescriptor, ChatMessage, ChatRequest,
    ContextComposeRequest, ContextComposeResult, MODEL_CHAT_COMPLETE_OPERATION, ToolCatalog,
    ToolResultOutcome,
};

/// A cloneable handle for the synchronous host used by the CLI.
///
/// The handle does not make host calls concurrent.  It only allows the spine
/// model, context, and tool components to share one `ProcessPluginHost` while
/// retaining the spine's `&mut self` trait contracts.
#[derive(Clone)]
pub struct ProcessPluginHostHandle {
    host: Rc<RefCell<ProcessPluginHost>>,
}

impl ProcessPluginHostHandle {
    pub fn new(host: ProcessPluginHost) -> Self {
        Self {
            host: Rc::new(RefCell::new(host)),
        }
    }

    pub fn from_shared(host: Rc<RefCell<ProcessPluginHost>>) -> Self {
        Self { host }
    }

    pub fn borrow_mut(&self) -> RefMut<'_, ProcessPluginHost> {
        self.host.borrow_mut()
    }

    /// Execute one bounded host call and release the interior borrow before
    /// returning to the caller.  Keeping this boundary here prevents a
    /// plugin-call borrow from leaking across session state transitions.
    pub fn invoke<Request, Response>(
        &self,
        capability: &CapabilityDescriptor,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, PluginCallError>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.host
            .borrow_mut()
            .invoke(capability, operation, payload)
    }

    pub fn snapshot(&self) -> KernelSnapshot {
        self.host.borrow_mut().snapshot()
    }

    pub fn catalog(&self) -> Ref<'_, CapabilityCatalog> {
        Ref::map(self.host.borrow(), ProcessPluginHost::catalog)
    }

    pub fn connection_count(&self) -> usize {
        self.host.borrow().connection_count()
    }
}

/// A host-backed spine model provider.
pub struct ProcessPluginModelProvider {
    host: ProcessPluginHostHandle,
    capability: CapabilityDescriptor,
    operation: String,
}

impl ProcessPluginModelProvider {
    pub fn new(host: ProcessPluginHostHandle, capability: CapabilityDescriptor) -> Self {
        Self {
            host,
            capability,
            operation: MODEL_CHAT_COMPLETE_OPERATION.to_string(),
        }
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = operation.into();
        self
    }
}

impl ModelProvider for ProcessPluginModelProvider {
    fn complete(
        &mut self,
        request: &ModelRequest,
        _cancellation: &yunxi_agent_spine::CancellationToken,
    ) -> Result<yunxi_protocol::ChatResult, ModelError> {
        self.host
            .invoke(&self.capability, &self.operation, request.chat())
            .map_err(|error| component_error("model_plugin_call_failed", error))
    }
}

/// Controls whether an optional context plugin failure ends the spine turn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextFailureMode {
    /// Keep the baseline conversation when the host route is unavailable.
    #[default]
    BestEffort,
    /// Surface the host failure as a spine context error.
    FailTurn,
}

/// A context assembler that preserves the spine conversation assembly and can
/// optionally prepend the existing `context.compose` plugin result.
#[derive(Clone)]
pub struct ProcessPluginContextAssembler {
    base: ConversationContextAssembler,
    host: Option<ProcessPluginHostHandle>,
    capability: Option<CapabilityDescriptor>,
    cwd: Option<PathBuf>,
    failure_mode: ContextFailureMode,
}

impl ProcessPluginContextAssembler {
    pub fn new() -> Self {
        Self {
            base: ConversationContextAssembler,
            host: None,
            capability: None,
            cwd: None,
            failure_mode: ContextFailureMode::BestEffort,
        }
    }

    pub fn with_context_plugin(
        mut self,
        host: ProcessPluginHostHandle,
        capability: CapabilityDescriptor,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        self.host = Some(host);
        self.capability = Some(capability);
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_failure_mode(mut self, failure_mode: ContextFailureMode) -> Self {
        self.failure_mode = failure_mode;
        self
    }

    fn compose_with_instructions(
        base: &ChatRequest,
        instructions: &str,
    ) -> Result<ChatRequest, ContextError> {
        if instructions.trim().is_empty() {
            return Ok(base.clone());
        }
        let mut messages = Vec::with_capacity(base.messages().len() + 1);
        messages.push(ChatMessage::system(instructions));
        messages.extend_from_slice(base.messages());
        let mut request = ChatRequest::new(messages);
        if let Some(tools) = base.tools().cloned() {
            request = request.with_tools(tools);
        }
        Ok(request)
    }
}

impl Default for ProcessPluginContextAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextAssembler for ProcessPluginContextAssembler {
    fn assemble(
        &mut self,
        request: ContextAssemblyRequest<'_>,
    ) -> Result<ChatRequest, ContextError> {
        let base = self.base.assemble(request)?;
        let (Some(host), Some(capability), Some(cwd)) = (
            self.host.as_ref(),
            self.capability.as_ref(),
            self.cwd.as_deref(),
        ) else {
            return Ok(base);
        };

        let result = host.invoke::<_, ContextComposeResult>(
            capability,
            CONTEXT_COMPOSE_OPERATION,
            &ContextComposeRequest::new(cwd),
        );
        match result {
            Ok(context) => Self::compose_with_instructions(&base, context.instructions()),
            Err(_error) if self.failure_mode == ContextFailureMode::BestEffort => Ok(base),
            Err(error) => Err(component_error("context_plugin_call_failed", error)),
        }
    }
}

/// The invocation produced only after the caller has applied its approval and
/// grant policy.  The adapter does not construct grants or hold continuation
/// state; that remains owned by the existing CLI session.
pub struct ApprovedPluginInvocation {
    capability: CapabilityDescriptor,
    operation: String,
    payload: Value,
}

impl ApprovedPluginInvocation {
    pub fn new(
        capability: CapabilityDescriptor,
        operation: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self {
            capability,
            operation: operation.into(),
            payload,
        }
    }

    pub fn capability(&self) -> &CapabilityDescriptor {
        &self.capability
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

/// Encodes a spine tool request into an already-approved host invocation.
///
/// Implementations should return a structured `ToolError` when approval is
/// pending, denied, or the call cannot be converted to the typed protocol
/// request required by a built-in plugin.
pub trait ApprovedToolInvocationEncoder {
    fn encode(&mut self, request: &ToolRequest<'_>) -> Result<ApprovedPluginInvocation, ToolError>;
}

/// A fail-closed encoder useful while a caller has not connected its approval
/// continuation yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyUnapprovedToolCalls;

impl ApprovedToolInvocationEncoder for DenyUnapprovedToolCalls {
    fn encode(&mut self, request: &ToolRequest<'_>) -> Result<ApprovedPluginInvocation, ToolError> {
        Err(ToolError::new(
            "approval_required",
            format!(
                "approval is required before executing {}",
                request.call().name()
            ),
            false,
        ))
    }
}

/// A host-backed tool broker with an explicit approval/encoding boundary.
pub struct ProcessPluginToolBroker<E> {
    host: ProcessPluginHostHandle,
    catalog: ToolCatalog,
    encoder: E,
}

impl<E> ProcessPluginToolBroker<E> {
    pub fn new(host: ProcessPluginHostHandle, catalog: ToolCatalog, encoder: E) -> Self {
        Self {
            host,
            catalog,
            encoder,
        }
    }

    pub fn encoder_mut(&mut self) -> &mut E {
        &mut self.encoder
    }
}

impl<E> ToolBroker for ProcessPluginToolBroker<E>
where
    E: ApprovedToolInvocationEncoder,
{
    fn catalog(&self) -> Result<ToolCatalog, ToolError> {
        Ok(self.catalog.clone())
    }

    fn execute(
        &mut self,
        request: ToolRequest<'_>,
        _cancellation: &yunxi_agent_spine::CancellationToken,
    ) -> Result<ToolResultOutcome, ToolError> {
        let invocation = self.encoder.encode(&request)?;
        let output = self.host.invoke::<_, Value>(
            invocation.capability(),
            invocation.operation(),
            invocation.payload(),
        );
        let output = output.map_err(|error| component_error("tool_plugin_call_failed", error))?;
        ToolResultOutcome::completed(output)
            .map_err(|error| ToolError::new("invalid_tool_result", error.to_string(), false))
    }
}

fn component_error(code: &str, error: PluginCallError) -> yunxi_agent_spine::ComponentError {
    match error {
        PluginCallError::Rejected {
            code,
            message,
            retryable,
            ..
        } => yunxi_agent_spine::ComponentError::new(code, message, retryable),
        other => yunxi_agent_spine::ComponentError::new(code, other.to_string(), false),
    }
}
