//! Plugin handshake and request dispatch for persistent sessions.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, HostMessage, InvocationCodecError, InvocationResponse,
    PluginMessage, ProtocolError, STORAGE_SESSIONS_APPEND_OPERATION,
    STORAGE_SESSIONS_LIST_OPERATION, STORAGE_SESSIONS_LOAD_OPERATION,
    STORAGE_SESSIONS_MUTATE_OPERATION, SessionAppendRequest, SessionListRequest,
    SessionLoadRequest, SessionMutationRequest, capabilities, connect_plugin,
};

use crate::{SessionStore, StorageError};

pub const STORAGE_PLUGIN_ID: &str = "yunxi.storage";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_storage_plugin() -> Result<(), StoragePluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::STORAGE_SESSIONS,
        capabilities::STORAGE_SESSIONS_VERSION,
    )?;
    let mut session = connect_plugin(
        STORAGE_PLUGIN_ID,
        "Persistent conversation sessions",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::STORAGE_SESSIONS
                    || request.capability().version() != capabilities::STORAGE_SESSIONS_VERSION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_capability",
                        "storage plugin does not support the requested capability".to_string(),
                    )?;
                    continue;
                }
                let response = match request.operation() {
                    STORAGE_SESSIONS_APPEND_OPERATION => request
                        .decode_payload::<SessionAppendRequest>()
                        .map_err(StoragePluginError::Invocation)
                        .and_then(|payload| {
                            SessionStore::from_grant(payload.grant())
                                .and_then(|store| store.append(&payload))
                                .map_err(StoragePluginError::Storage)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(StoragePluginError::Invocation)
                        }),
                    STORAGE_SESSIONS_LOAD_OPERATION => request
                        .decode_payload::<SessionLoadRequest>()
                        .map_err(StoragePluginError::Invocation)
                        .and_then(|payload| {
                            SessionStore::from_grant(payload.grant())
                                .and_then(|store| store.load(&payload))
                                .map_err(StoragePluginError::Storage)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(StoragePluginError::Invocation)
                        }),
                    STORAGE_SESSIONS_LIST_OPERATION => request
                        .decode_payload::<SessionListRequest>()
                        .map_err(StoragePluginError::Invocation)
                        .and_then(|payload| {
                            SessionStore::from_grant(payload.grant())
                                .and_then(|store| store.list(&payload))
                                .map_err(StoragePluginError::Storage)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(StoragePluginError::Invocation)
                        }),
                    STORAGE_SESSIONS_MUTATE_OPERATION => request
                        .decode_payload::<SessionMutationRequest>()
                        .map_err(StoragePluginError::Invocation)
                        .and_then(|payload| {
                            SessionStore::from_grant(payload.grant())
                                .and_then(|store| store.mutate(&payload))
                                .map_err(StoragePluginError::Storage)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(StoragePluginError::Invocation)
                        }),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "storage plugin does not support the requested operation".to_string(),
                        )?;
                        continue;
                    }
                };
                match response {
                    Ok(response) => {
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?
                    }
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(StoragePluginError::UnexpectedHostMessage(
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
pub enum StoragePluginError {
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Storage(StorageError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl StoragePluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invocation(_) => "invalid_request",
            Self::Storage(StorageError::WriteNotGranted) => "write_not_granted",
            Self::Storage(StorageError::NotFound(_)) => "session_not_found",
            Self::Storage(StorageError::LegacyReadOnly(_)) => "legacy_read_only",
            Self::Storage(_) => "storage_error",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for StoragePluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for StoragePluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for StoragePluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<ProtocolError> for StoragePluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
