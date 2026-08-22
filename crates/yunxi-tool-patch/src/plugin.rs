//! Plugin handshake and dispatch for transactional patch application.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    ActionGrantError, CapabilityDescriptor, CapabilityError, HostMessage, InvocationCodecError,
    InvocationResponse, PatchApplyRequest, PluginMessage, ProtocolError,
    TOOL_PATCH_APPLY_OPERATION, capabilities, connect_plugin,
};

use crate::{PatchError, apply_patch};

pub const PATCH_PLUGIN_ID: &str = "yunxi.tool.patch";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_patch_plugin() -> Result<(), PatchPluginError> {
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_PATCH, capabilities::TOOL_PATCH_VERSION)?;
    let mut session = connect_plugin(
        PATCH_PLUGIN_ID,
        "Host-approved patch application",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_PATCH
                    || request.capability().version() != capabilities::TOOL_PATCH_VERSION
                    || request.operation() != TOOL_PATCH_APPLY_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "patch plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let response = request
                    .decode_payload::<PatchApplyRequest>()
                    .map_err(PatchPluginError::Invocation)
                    .and_then(|payload| apply_patch(&payload).map_err(PatchPluginError::Apply))
                    .and_then(|result| {
                        InvocationResponse::encode(request_id, &result)
                            .map_err(PatchPluginError::Invocation)
                    });
                match response {
                    Ok(response) => {
                        session.send(&PluginMessage::InvocationCompleted { response })?
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?
                    }
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(PatchPluginError::UnexpectedHostMessage(
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
pub enum PatchPluginError {
    Capability(CapabilityError),
    Grant(ActionGrantError),
    Invocation(InvocationCodecError),
    Apply(PatchError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl PatchPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Grant(_) => "grant_denied",
            Self::Invocation(_) => "invalid_request",
            Self::Apply(error) => error.code(),
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for PatchPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Grant(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Apply(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for PatchPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Grant(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Apply(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for PatchPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for PatchPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for PatchPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
