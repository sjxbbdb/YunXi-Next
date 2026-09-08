//! Plugin handshake and dispatch for bounded shell execution.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    ActionGrantError, CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement,
    HostMessage, InvocationCodecError, InvocationResponse, PluginMessage, ProtocolError,
    ShellExecuteRequest, TOOL_SHELL_EXECUTE_OPERATION, capabilities, connect_plugin_with_grants,
};

use crate::{ShellError, execute};

pub const SHELL_PLUGIN_ID: &str = "yunxi.tool.shell";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_shell_plugin() -> Result<(), ShellPluginError> {
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_SHELL, capabilities::TOOL_SHELL_VERSION)?;
    let mut session = connect_plugin_with_grants(
        SHELL_PLUGIN_ID,
        "Host-approved shell execution",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        vec![
            GrantRequirement::required(GrantKind::Approval),
            GrantRequirement::required(GrantKind::WorkspaceRead),
            GrantRequirement::optional(GrantKind::WorkspaceWrite),
            GrantRequirement::optional(GrantKind::Network),
        ],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_SHELL
                    || request.capability().version() != capabilities::TOOL_SHELL_VERSION
                    || request.operation() != TOOL_SHELL_EXECUTE_OPERATION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "shell plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let response = request
                    .decode_payload::<ShellExecuteRequest>()
                    .map_err(ShellPluginError::Invocation)
                    .and_then(|payload| execute(&payload).map_err(ShellPluginError::Execution))
                    .and_then(|result| {
                        InvocationResponse::encode(request_id, &result)
                            .map_err(ShellPluginError::Invocation)
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
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(ShellPluginError::UnexpectedHostMessage(
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
pub enum ShellPluginError {
    Capability(CapabilityError),
    Grant(ActionGrantError),
    Invocation(InvocationCodecError),
    Execution(ShellError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl ShellPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Grant(_) => "grant_denied",
            Self::Invocation(_) => "invalid_request",
            Self::Execution(error) => error.code(),
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for ShellPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Grant(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Execution(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for ShellPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Grant(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Execution(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for ShellPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for ShellPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for ShellPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
