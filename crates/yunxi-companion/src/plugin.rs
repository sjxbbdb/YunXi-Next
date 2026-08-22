//! Plugin handshake and dispatch for companion response decisions.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    COMPANION_DECIDE_OPERATION, CapabilityDescriptor, CapabilityError, CompanionDecisionRequest,
    HostMessage, InvocationCodecError, InvocationResponse, PluginMessage, ProtocolError,
    capabilities, connect_plugin,
};

use crate::decide;

pub const COMPANION_PLUGIN_ID: &str = "yunxi.companion";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_companion_plugin() -> Result<(), CompanionPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::COMPANION_DECIDE,
        capabilities::COMPANION_DECIDE_VERSION,
    )?;
    let mut session = connect_plugin(
        COMPANION_PLUGIN_ID,
        "Deterministic companion policy",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::COMPANION_DECIDE
                    || request.capability().version() != capabilities::COMPANION_DECIDE_VERSION
                    || request.operation() != COMPANION_DECIDE_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "companion plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let payload = match request.decode_payload::<CompanionDecisionRequest>() {
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
                let response = InvocationResponse::encode(request_id, &decide(&payload))?;
                session.send(&PluginMessage::InvocationCompleted { response })?;
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(CompanionPluginError::UnexpectedHostMessage(
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
pub enum CompanionPluginError {
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for CompanionPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for CompanionPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for CompanionPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for CompanionPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for CompanionPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
