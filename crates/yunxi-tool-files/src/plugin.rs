//! Plugin handshake and dispatch for read-only file tools.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, FileReadRequest, FileSearchRequest, GrantKind,
    GrantRequirement, HostMessage, InvocationCodecError, InvocationResponse, PluginMessage,
    ProtocolError, TOOL_FILES_READ_OPERATION, TOOL_FILES_SEARCH_OPERATION, capabilities,
    connect_plugin_with_grants,
};

use crate::{FileToolError, read_file, search_files};

pub const FILES_PLUGIN_ID: &str = "yunxi.tool.files";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_file_tool_plugin() -> Result<(), FileToolPluginError> {
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_FILES, capabilities::TOOL_FILES_VERSION)?;
    let mut session = connect_plugin_with_grants(
        FILES_PLUGIN_ID,
        "Read-only workspace file tools",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        vec![GrantRequirement::required(GrantKind::WorkspaceRead)],
        CONNECT_TIMEOUT,
    )?;
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_FILES
                    || request.capability().version() != capabilities::TOOL_FILES_VERSION
                    || !matches!(
                        request.operation(),
                        TOOL_FILES_SEARCH_OPERATION | TOOL_FILES_READ_OPERATION
                    )
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "file tool plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                let result = match request.operation() {
                    TOOL_FILES_SEARCH_OPERATION => request
                        .decode_payload::<FileSearchRequest>()
                        .map_err(FileToolPluginError::Invocation)
                        .and_then(|payload| {
                            search_files(&payload).map_err(FileToolPluginError::Execution)
                        })
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(|error| {
                                FileToolPluginError::Protocol(ProtocolError::Encode(error))
                            })
                        }),
                    TOOL_FILES_READ_OPERATION => request
                        .decode_payload::<FileReadRequest>()
                        .map_err(FileToolPluginError::Invocation)
                        .and_then(|payload| {
                            read_file(&payload).map_err(FileToolPluginError::Execution)
                        })
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(|error| {
                                FileToolPluginError::Protocol(ProtocolError::Encode(error))
                            })
                        }),
                    _ => unreachable!("operation checked above"),
                };
                match result {
                    Ok(result) => {
                        let response = InvocationResponse::encode(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?
                    }
                }
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(FileToolPluginError::UnexpectedHostMessage(
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
pub enum FileToolPluginError {
    Capability(CapabilityError),
    Execution(FileToolError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl FileToolPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Execution(error) => error.code(),
            Self::Invocation(_) => "invalid_request",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for FileToolPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Execution(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for FileToolPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Execution(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for FileToolPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for FileToolPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for FileToolPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
