//! Model plugin request loop and per-request API failure containment.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, ChatRequest, ChatResult, GrantKind, GrantRequirement,
    HostMessage, InvocationCodecError, InvocationResponse, MODEL_CHAT_COMPLETE_OPERATION,
    PluginMessage, ProtocolError, capabilities, connect_plugin_with_grants,
};

use crate::{ApiError, OpenAiChatClient, ProviderConfig, ProviderConfigError};

pub const MODEL_PLUGIN_ID: &str = "yunxi.model.openai-compatible";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run_model_plugin_from_env() -> Result<(), ModelPluginError> {
    let config = ProviderConfig::from_env()?;
    run_model_plugin(config)
}

pub fn run_model_plugin(config: ProviderConfig) -> Result<(), ModelPluginError> {
    let client = OpenAiChatClient::new(config)?;
    let model_chat =
        CapabilityDescriptor::new(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
    let mut session = connect_plugin_with_grants(
        MODEL_PLUGIN_ID,
        "OpenAI-compatible chat model",
        env!("CARGO_PKG_VERSION"),
        vec![model_chat],
        vec![
            GrantRequirement::required(GrantKind::Network),
            GrantRequirement::required(GrantKind::ProviderCredential),
        ],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::MODEL_CHAT
                    || request.capability().version() != capabilities::MODEL_CHAT_VERSION
                    || request.operation() != MODEL_CHAT_COMPLETE_OPERATION
                {
                    session.send(&PluginMessage::InvocationFailed {
                        request_id,
                        code: "unsupported_operation".to_string(),
                        message: format!(
                            "plugin does not support {}@{}:{}",
                            request.capability().id(),
                            request.capability().version(),
                            request.operation()
                        ),
                        retryable: false,
                    })?;
                    continue;
                }

                let chat = match request.decode_payload::<ChatRequest>() {
                    Ok(chat) => chat,
                    Err(error) => {
                        session.send(&PluginMessage::InvocationFailed {
                            request_id,
                            code: "invalid_request".to_string(),
                            message: error.to_string(),
                            retryable: false,
                        })?;
                        continue;
                    }
                };
                match client.complete_with_tools(chat.messages(), chat.tools()) {
                    Ok(completion) => {
                        let result = ChatResult::new(
                            completion.content(),
                            completion.finish_reason().map(str::to_string),
                        )
                        .with_tool_calls(completion.tool_calls().to_vec());
                        let response = InvocationResponse::encode(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => session.send(&PluginMessage::InvocationFailed {
                        request_id,
                        code: error.code().to_string(),
                        message: error.to_string(),
                        retryable: error.retryable(),
                    })?,
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(ModelPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

#[derive(Debug)]
pub enum ModelPluginError {
    Config(ProviderConfigError),
    Api(ApiError),
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for ModelPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "model plugin configuration failed: {error}"),
            Self::Api(error) => write!(formatter, "model plugin HTTP client failed: {error}"),
            Self::Capability(error) => {
                write!(
                    formatter,
                    "model plugin capability declaration failed: {error}"
                )
            }
            Self::Invocation(error) => write!(formatter, "model plugin invocation failed: {error}"),
            Self::Protocol(error) => write!(formatter, "model plugin protocol failed: {error}"),
            Self::UnexpectedHostMessage(message) => {
                write!(
                    formatter,
                    "model plugin received invalid host message: {message}"
                )
            }
        }
    }
}

impl Error for ModelPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Api(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<ProviderConfigError> for ModelPluginError {
    fn from(error: ProviderConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<ApiError> for ModelPluginError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<CapabilityError> for ModelPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for ModelPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for ModelPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
