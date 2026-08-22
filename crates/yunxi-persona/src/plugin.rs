//! Plugin-side handshake and request dispatch for persona context compilation.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, HostMessage, InvocationCodecError, InvocationResponse,
    PERSONA_CONTEXT_COMPILE_OPERATION, PersonaContextRequest, PluginMessage, ProtocolError,
    capabilities, connect_plugin,
};

use crate::{PersonaCompileError, compile_context};

pub const PERSONA_PLUGIN_ID: &str = "yunxi.persona";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_persona_plugin() -> Result<(), PersonaPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::PERSONA_CONTEXT,
        capabilities::PERSONA_CONTEXT_VERSION,
    )?;
    let mut session = connect_plugin(
        PERSONA_PLUGIN_ID,
        "Persona context compiler",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::PERSONA_CONTEXT
                    || request.capability().version() != capabilities::PERSONA_CONTEXT_VERSION
                    || request.operation() != PERSONA_CONTEXT_COMPILE_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "persona plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let payload = match request.decode_payload::<PersonaContextRequest>() {
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
                match compile_context(&payload) {
                    Ok(result) => {
                        let response = InvocationResponse::encode(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => send_failure(
                        &mut session,
                        request_id,
                        "persona_compile_error",
                        error.to_string(),
                    )?,
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(PersonaPluginError::UnexpectedHostMessage(
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
pub enum PersonaPluginError {
    Capability(CapabilityError),
    Compile(PersonaCompileError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for PersonaPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Compile(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for PersonaPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Compile(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for PersonaPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<PersonaCompileError> for PersonaPluginError {
    fn from(error: PersonaCompileError) -> Self {
        Self::Compile(error)
    }
}

impl From<InvocationCodecError> for PersonaPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for PersonaPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
