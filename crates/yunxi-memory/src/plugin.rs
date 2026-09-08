//! Plugin-side handshake and dispatch for memory recall, writes, and review.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationResponse, MEMORY_MANAGEMENT_CLEAR_OPERATION,
    MEMORY_MANAGEMENT_LIST_OPERATION, MEMORY_MANAGEMENT_MUTATE_OPERATION,
    MEMORY_MANAGEMENT_QUERY_OPERATION, MEMORY_MANAGEMENT_SET_ENABLED_OPERATION,
    MEMORY_MANAGEMENT_SHOW_OPERATION, MEMORY_MANAGEMENT_STATUS_OPERATION, MEMORY_RECALL_OPERATION,
    MEMORY_WRITE_EXTRACT_OPERATION, MEMORY_WRITE_REVIEW_OPERATION, ManagementRequestError,
    MemoryClearRequest, MemoryListRequest, MemoryMutationRequest, MemoryQueryRequest,
    MemoryRecallRequest, MemoryReviewRequest, MemorySetEnabledRequest, MemoryShowRequest,
    MemoryStatusRequest, MemoryWriteRequest, PluginMessage, ProtocolError, capabilities,
    connect_plugin_with_grants,
};

use crate::{
    MemoryManagementError, MemoryRecallError, MemoryWriteError, clear_with_grant,
    extract_and_store, list_with_grant, mutate_with_grant, query_with_grant, recall, review_memory,
    set_enabled_with_grant, show_with_grant, status_with_grant,
};

pub const MEMORY_PLUGIN_ID: &str = "yunxi.memory";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_memory_plugin() -> Result<(), MemoryPluginError> {
    let recall_capability = CapabilityDescriptor::new(
        capabilities::MEMORY_RECALL,
        capabilities::MEMORY_RECALL_VERSION,
    )?;
    let write_capability = CapabilityDescriptor::new(
        capabilities::MEMORY_WRITE,
        capabilities::MEMORY_WRITE_VERSION,
    )?;
    let management_capability = CapabilityDescriptor::new(
        capabilities::MEMORY_MANAGEMENT,
        capabilities::MEMORY_MANAGEMENT_VERSION,
    )?;
    let mut session = connect_plugin_with_grants(
        MEMORY_PLUGIN_ID,
        "Long-term memory",
        env!("CARGO_PKG_VERSION"),
        vec![recall_capability, write_capability, management_capability],
        vec![
            GrantRequirement::required(GrantKind::WorkspaceRead),
            GrantRequirement::required(GrantKind::WorkspaceWrite),
        ],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                let capability = request.capability().id().as_str();
                let response = match (
                    capability,
                    request.capability().version(),
                    request.operation(),
                ) {
                    (
                        capabilities::MEMORY_RECALL,
                        capabilities::MEMORY_RECALL_VERSION,
                        MEMORY_RECALL_OPERATION,
                    ) => request
                        .decode_payload::<MemoryRecallRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| recall(&payload).map_err(MemoryPluginError::Recall))
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_WRITE,
                        capabilities::MEMORY_WRITE_VERSION,
                        MEMORY_WRITE_EXTRACT_OPERATION,
                    ) => request
                        .decode_payload::<MemoryWriteRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            extract_and_store(&payload).map_err(MemoryPluginError::Write)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_STATUS_OPERATION,
                    ) => request
                        .decode_payload::<MemoryStatusRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            status_with_grant(payload.grant())
                                .map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_QUERY_OPERATION,
                    ) => request
                        .decode_payload::<MemoryQueryRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(MemoryPluginError::ManagementRequest)?;
                            query_with_grant(&payload).map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_MUTATE_OPERATION,
                    ) => request
                        .decode_payload::<MemoryMutationRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(MemoryPluginError::ManagementRequest)?;
                            mutate_with_grant(&payload).map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_CLEAR_OPERATION,
                    ) => request
                        .decode_payload::<MemoryClearRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            clear_with_grant(&payload).map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_SET_ENABLED_OPERATION,
                    ) => request
                        .decode_payload::<MemorySetEnabledRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            set_enabled_with_grant(payload.grant(), payload.enabled())
                                .map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_LIST_OPERATION,
                    ) => request
                        .decode_payload::<MemoryListRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(MemoryPluginError::ManagementRequest)?;
                            list_with_grant(payload.grant(), payload.limit())
                                .map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_MANAGEMENT,
                        capabilities::MEMORY_MANAGEMENT_VERSION,
                        MEMORY_MANAGEMENT_SHOW_OPERATION,
                    ) => request
                        .decode_payload::<MemoryShowRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(MemoryPluginError::ManagementRequest)?;
                            show_with_grant(payload.grant(), payload.id())
                                .map_err(MemoryPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    (
                        capabilities::MEMORY_WRITE,
                        capabilities::MEMORY_WRITE_VERSION,
                        MEMORY_WRITE_REVIEW_OPERATION,
                    ) => request
                        .decode_payload::<MemoryReviewRequest>()
                        .map_err(MemoryPluginError::Invocation)
                        .and_then(|payload| {
                            review_memory(&payload).map_err(MemoryPluginError::Write)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(MemoryPluginError::Invocation)
                        }),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "memory plugin does not support the requested operation".to_string(),
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
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(MemoryPluginError::UnexpectedHostMessage(
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
pub enum MemoryPluginError {
    Capability(CapabilityError),
    Recall(MemoryRecallError),
    Write(MemoryWriteError),
    Management(MemoryManagementError),
    ManagementRequest(ManagementRequestError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl MemoryPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invocation(_) => "invalid_request",
            Self::Recall(_) => "memory_recall_error",
            Self::Write(MemoryWriteError::WriteNotGranted) => "write_not_granted",
            Self::Write(_) => "memory_write_error",
            Self::Management(MemoryManagementError::WriteNotGranted) => "write_not_granted",
            Self::Management(_) => "memory_management_error",
            Self::ManagementRequest(_) => "invalid_request",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for MemoryPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Recall(error) => error.fmt(formatter),
            Self::Write(error) => error.fmt(formatter),
            Self::Management(error) => error.fmt(formatter),
            Self::ManagementRequest(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for MemoryPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Recall(error) => Some(error),
            Self::Write(error) => Some(error),
            Self::Management(error) => Some(error),
            Self::ManagementRequest(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for MemoryPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<MemoryRecallError> for MemoryPluginError {
    fn from(error: MemoryRecallError) -> Self {
        Self::Recall(error)
    }
}

impl From<InvocationCodecError> for MemoryPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for MemoryPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
