//! Model plugin request loop and per-request API failure containment.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{CHAT_CAPABILITY, HostMessage, PluginMessage, ProtocolError, connect_plugin};

use crate::{ApiError, OpenAiChatClient, ProviderConfig, ProviderConfigError};

pub const MODEL_PLUGIN_ID: &str = "yunxi.model.openai-compatible";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run_model_plugin_from_env() -> Result<(), ModelPluginError> {
    let config = ProviderConfig::from_env()?;
    run_model_plugin(config)
}

pub fn run_model_plugin(config: ProviderConfig) -> Result<(), ModelPluginError> {
    let client = OpenAiChatClient::new(config)?;
    let mut session = connect_plugin(
        MODEL_PLUGIN_ID,
        client.config().provider(),
        client.config().model(),
        vec![CHAT_CAPABILITY.to_string()],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Chat {
                request_id,
                messages,
            } => match client.complete(&messages) {
                Ok(completion) => session.send(&PluginMessage::ChatCompleted {
                    request_id,
                    content: completion.content().to_string(),
                    finish_reason: completion.finish_reason().map(str::to_string),
                })?,
                Err(error) => session.send(&PluginMessage::RequestFailed {
                    request_id,
                    code: error.code().to_string(),
                    message: error.to_string(),
                    retryable: error.retryable(),
                })?,
            },
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
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for ModelPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "model plugin configuration failed: {error}"),
            Self::Api(error) => write!(formatter, "model plugin HTTP client failed: {error}"),
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

impl From<ProtocolError> for ModelPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
