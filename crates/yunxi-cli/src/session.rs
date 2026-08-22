//! Host-side lifecycle and request handling for the isolated model plugin.

use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

use yunxi_kernel::{KernelError, PluginCommand, PluginId, PluginIdError, PluginSpec, YunxiKernel};
use yunxi_model_openai::{MODEL_PLUGIN_ID, ProviderConfig, ProviderConfigError};
use yunxi_plugin_host::{CapabilityCatalog, CatalogError};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, CapabilityDescriptor, CapabilityError, ChatMessage,
    ChatRequest, ChatResult, HostMessage, HostPluginSession, InvocationRequest,
    MODEL_CHAT_COMPLETE_OPERATION, PluginAcceptor, PluginMessage, ProtocolError, capabilities,
};

use crate::INTERNAL_MODEL_PLUGIN_ARGUMENT;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT_MARGIN: Duration = Duration::from_secs(5);

pub(crate) trait ChatBackend {
    fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure>;
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    fn status(&mut self) -> BackendStatus;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendStatus {
    pub kernel: String,
    pub plugin: String,
    pub protocol_ready: bool,
    pub plugins: usize,
    pub capabilities: usize,
}

pub(crate) struct ChatSession {
    kernel: YunxiKernel,
    catalog: CapabilityCatalog,
    plugin_id: PluginId,
    chat_capability: CapabilityDescriptor,
    connection: Option<HostPluginSession>,
    provider: String,
    model: String,
    next_request_id: u64,
}

impl ChatSession {
    pub(crate) fn launch(plugin_path: Option<&Path>) -> Result<Self, SessionError> {
        // Validate the inherited provider configuration before starting a child whose stderr is
        // intentionally detached. The key remains out of protocol messages and diagnostics.
        let config = ProviderConfig::from_env()?;
        let response_timeout = config
            .timeout()
            .checked_add(RESPONSE_TIMEOUT_MARGIN)
            .unwrap_or(config.timeout());
        let provider = config.provider().to_string();
        let model = config.model().to_string();
        drop(config);

        let acceptor = PluginAcceptor::bind()?;
        let mut command = match plugin_path {
            Some(path) => PluginCommand::new(path),
            None => PluginCommand::new(env::current_exe()?).arg(INTERNAL_MODEL_PLUGIN_ARGUMENT),
        };
        command = command
            .env(CONNECT_ADDRESS_ENV, acceptor.address()?.to_string())
            .env(CONNECT_TOKEN_ENV, acceptor.connection_token());

        let plugin_id = PluginId::new(MODEL_PLUGIN_ID)?;
        let spec = PluginSpec::new(plugin_id.clone(), command)
            .with_display_name("OpenAI-compatible chat model");
        let mut kernel = YunxiKernel::new();
        kernel.register(spec)?;
        kernel.start(&plugin_id)?;

        let connection = match acceptor.accept(MODEL_PLUGIN_ID, HANDSHAKE_TIMEOUT) {
            Ok(connection) => connection,
            Err(error) => {
                kernel.refresh();
                kernel.shutdown();
                return Err(SessionError::Protocol(error));
            }
        };
        let mut catalog = CapabilityCatalog::new();
        catalog.register_connection(connection.info())?;
        let chat_capability =
            CapabilityDescriptor::new(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
        let chat_provider =
            catalog.resolve_unique(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
        if chat_provider.id() != &plugin_id {
            kernel.shutdown();
            return Err(SessionError::Catalog(CatalogError::MissingCapability {
                capability: capabilities::MODEL_CHAT.to_string(),
                version: capabilities::MODEL_CHAT_VERSION,
            }));
        }
        connection.set_timeouts(Some(response_timeout), Some(WRITE_TIMEOUT))?;

        Ok(Self {
            kernel,
            catalog,
            plugin_id,
            chat_capability,
            connection: Some(connection),
            provider,
            model,
            next_request_id: 1,
        })
    }

    fn mark_unavailable(&mut self) {
        self.connection = None;
        self.catalog.unregister(&self.plugin_id);
        let _ignored = self.kernel.stop(&self.plugin_id);
        self.kernel.refresh();
    }
}

impl ChatBackend for ChatSession {
    fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).unwrap_or(1);

        let Some(connection) = self.connection.as_mut() else {
            return Err(ChatFailure::Unavailable(
                "model plugin connection is not available".to_string(),
            ));
        };
        let request = InvocationRequest::encode(
            request_id,
            self.chat_capability.clone(),
            MODEL_CHAT_COMPLETE_OPERATION,
            &ChatRequest::new(messages.to_vec()),
        )
        .map_err(|error| ChatFailure::ProtocolViolation(error.to_string()))?;
        if let Err(error) = connection.send(&HostMessage::Invoke { request }) {
            let message = error.to_string();
            self.mark_unavailable();
            return Err(ChatFailure::Unavailable(message));
        }

        let Some(connection) = self.connection.as_mut() else {
            return Err(ChatFailure::Unavailable(
                "model plugin connection closed after request send".to_string(),
            ));
        };
        let response = match connection.receive() {
            Ok(response) => response,
            Err(error) => {
                let message = error.to_string();
                self.mark_unavailable();
                return Err(ChatFailure::Unavailable(message));
            }
        };

        match response {
            PluginMessage::InvocationCompleted { response }
                if response.request_id() == request_id =>
            {
                match response.decode_payload::<ChatResult>() {
                    Ok(result) => Ok(result.content().to_string()),
                    Err(error) => {
                        self.mark_unavailable();
                        Err(ChatFailure::ProtocolViolation(error.to_string()))
                    }
                }
            }
            PluginMessage::InvocationFailed {
                request_id: response_id,
                code,
                message,
                retryable,
            } if response_id == request_id => Err(ChatFailure::Request {
                code,
                message,
                retryable,
            }),
            message => {
                self.mark_unavailable();
                Err(ChatFailure::ProtocolViolation(format!(
                    "expected response for request {request_id}, received {message:?}"
                )))
            }
        }
    }

    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn status(&mut self) -> BackendStatus {
        self.kernel.refresh();
        let plugin = self
            .kernel
            .plugin(&self.plugin_id)
            .map(|snapshot| snapshot.state().to_string())
            .unwrap_or_else(|| "not registered".to_string());
        BackendStatus {
            kernel: self.kernel.state().to_string(),
            plugin,
            protocol_ready: self.connection.is_some(),
            plugins: self.catalog.plugin_count(),
            capabilities: self.catalog.capability_count(),
        }
    }
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_mut() {
            let _ignored = connection.send(&HostMessage::Shutdown);
        }
        self.connection = None;
        self.kernel.shutdown();
    }
}

#[derive(Debug)]
pub(crate) enum SessionError {
    Config(ProviderConfigError),
    Executable(io::Error),
    PluginId(PluginIdError),
    Capability(CapabilityError),
    Catalog(CatalogError),
    Kernel(KernelError),
    Protocol(ProtocolError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "provider configuration failed: {error}"),
            Self::Executable(error) => {
                write!(formatter, "failed to locate the YunXi executable: {error}")
            }
            Self::PluginId(error) => write!(formatter, "model plugin id is invalid: {error}"),
            Self::Capability(error) => {
                write!(formatter, "capability declaration is invalid: {error}")
            }
            Self::Catalog(error) => write!(formatter, "plugin catalog failed: {error}"),
            Self::Kernel(error) => write!(formatter, "kernel operation failed: {error}"),
            Self::Protocol(error) => write!(formatter, "model plugin startup failed: {error}"),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Executable(error) => Some(error),
            Self::PluginId(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Catalog(error) => Some(error),
            Self::Kernel(error) => Some(error),
            Self::Protocol(error) => Some(error),
        }
    }
}

impl From<ProviderConfigError> for SessionError {
    fn from(error: ProviderConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Executable(error)
    }
}

impl From<PluginIdError> for SessionError {
    fn from(error: PluginIdError) -> Self {
        Self::PluginId(error)
    }
}

impl From<CapabilityError> for SessionError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<CatalogError> for SessionError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<KernelError> for SessionError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}

impl From<ProtocolError> for SessionError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChatFailure {
    Request {
        code: String,
        message: String,
        retryable: bool,
    },
    Unavailable(String),
    ProtocolViolation(String),
}

impl fmt::Display for ChatFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request {
                code,
                message,
                retryable,
            } => {
                write!(formatter, "model request failed ({code}): {message}")?;
                if *retryable {
                    formatter.write_str(" [retryable]")?;
                }
                Ok(())
            }
            Self::Unavailable(message) => write!(formatter, "model plugin unavailable: {message}"),
            Self::ProtocolViolation(message) => {
                write!(formatter, "model plugin protocol violation: {message}")
            }
        }
    }
}

impl Error for ChatFailure {}
