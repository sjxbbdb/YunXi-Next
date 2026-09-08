//! Plugin-side handshake and request dispatch for context composition.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CONTEXT_COMPOSE_OPERATION, CapabilityDescriptor, CapabilityError, ContextComposeRequest,
    HostMessage, InvocationCodecError, InvocationResponse, PluginMessage, ProtocolError,
    capabilities, connect_plugin,
};

use crate::{ContextComposeError, compose_context};

pub const CONTEXT_PLUGIN_ID: &str = "yunxi.context";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_context_plugin() -> Result<(), ContextPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::CONTEXT_COMPOSE,
        capabilities::CONTEXT_COMPOSE_VERSION,
    )?;
    let mut session = connect_plugin(
        CONTEXT_PLUGIN_ID,
        "Project instruction context",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::CONTEXT_COMPOSE
                    || request.capability().version() != capabilities::CONTEXT_COMPOSE_VERSION
                    || request.operation() != CONTEXT_COMPOSE_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "context plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let payload = match request.decode_payload::<ContextComposeRequest>() {
                    Ok(payload) => payload,
                    Err(error) => {
                        send_failure(
                            &mut session,
                            request_id,
                            "invalid_request",
                            error.to_string(),
                        )?;
                        continue;
                    }
                };
                match compose_context(payload.cwd()) {
                    Ok(result) => {
                        let response = InvocationResponse::encode(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, "context_error", error.to_string())?
                    }
                }
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(ContextPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn send_failure(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    code: &str,
    message: String,
) -> Result<(), ProtocolError> {
    session.send(&PluginMessage::InvocationFailed {
        request_id,
        code: code.to_string(),
        message,
        retryable: false,
    })
}

#[derive(Debug)]
pub enum ContextPluginError {
    Capability(CapabilityError),
    Compose(ContextComposeError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for ContextPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Compose(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for ContextPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Compose(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for ContextPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<ContextComposeError> for ContextPluginError {
    fn from(error: ContextComposeError) -> Self {
        Self::Compose(error)
    }
}

impl From<InvocationCodecError> for ContextPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for ContextPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
