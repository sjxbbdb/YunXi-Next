//! Plugin handshake and dispatch for companion decisions and management.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    COMPANION_DECIDE_OPERATION, COMPANION_MANAGEMENT_CHECK_OPERATION,
    COMPANION_MANAGEMENT_CLEAR_OPERATION, COMPANION_MANAGEMENT_HISTORY_OPERATION,
    COMPANION_MANAGEMENT_SET_ENABLED_OPERATION, COMPANION_MANAGEMENT_STATUS_OPERATION,
    CapabilityDescriptor, CapabilityError, CompanionCheckRequest, CompanionClearRequest,
    CompanionDecisionRequest, CompanionHistoryRequest, CompanionSetEnabledRequest,
    CompanionStatusRequest, GrantKind, GrantRequirement, HostMessage, InvocationCodecError,
    InvocationResponse, ManagementRequestError, PluginMessage, ProtocolError, capabilities,
    connect_plugin_with_grants,
};

use crate::{
    CompanionManagementError, check_with_grant, clear_with_grant, decide, enabled,
    history_with_grant, set_enabled_with_grant, status_with_grant,
};

pub const COMPANION_PLUGIN_ID: &str = "yunxi.companion";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_companion_plugin() -> Result<(), CompanionPluginError> {
    let decision_capability = CapabilityDescriptor::new(
        capabilities::COMPANION_DECIDE,
        capabilities::COMPANION_DECIDE_VERSION,
    )?;
    let management_capability = CapabilityDescriptor::new(
        capabilities::COMPANION_MANAGEMENT,
        capabilities::COMPANION_MANAGEMENT_VERSION,
    )?;
    let mut session = connect_plugin_with_grants(
        COMPANION_PLUGIN_ID,
        "Deterministic companion policy",
        env!("CARGO_PKG_VERSION"),
        vec![decision_capability, management_capability],
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
                let response = match (
                    request.capability().id().as_str(),
                    request.capability().version(),
                    request.operation(),
                ) {
                    (
                        capabilities::COMPANION_DECIDE,
                        capabilities::COMPANION_DECIDE_VERSION,
                        COMPANION_DECIDE_OPERATION,
                    ) => request
                        .decode_payload::<CompanionDecisionRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            if !enabled() {
                                return Err(CompanionPluginError::Disabled);
                            }
                            InvocationResponse::encode(request_id, &decide(&payload))
                                .map_err(CompanionPluginError::Invocation)
                        }),
                    (
                        capabilities::COMPANION_MANAGEMENT,
                        capabilities::COMPANION_MANAGEMENT_VERSION,
                        COMPANION_MANAGEMENT_STATUS_OPERATION,
                    ) => request
                        .decode_payload::<CompanionStatusRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            InvocationResponse::encode(
                                request_id,
                                &status_with_grant(payload.grant()),
                            )
                            .map_err(CompanionPluginError::Invocation)
                        }),
                    (
                        capabilities::COMPANION_MANAGEMENT,
                        capabilities::COMPANION_MANAGEMENT_VERSION,
                        COMPANION_MANAGEMENT_CHECK_OPERATION,
                    ) => request
                        .decode_payload::<CompanionCheckRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(CompanionPluginError::ManagementRequest)?;
                            check_with_grant(&payload).map_err(CompanionPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(CompanionPluginError::Invocation)
                        }),
                    (
                        capabilities::COMPANION_MANAGEMENT,
                        capabilities::COMPANION_MANAGEMENT_VERSION,
                        COMPANION_MANAGEMENT_HISTORY_OPERATION,
                    ) => request
                        .decode_payload::<CompanionHistoryRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(CompanionPluginError::ManagementRequest)?;
                            InvocationResponse::encode(request_id, &history_with_grant(&payload))
                                .map_err(CompanionPluginError::Invocation)
                        }),
                    (
                        capabilities::COMPANION_MANAGEMENT,
                        capabilities::COMPANION_MANAGEMENT_VERSION,
                        COMPANION_MANAGEMENT_CLEAR_OPERATION,
                    ) => request
                        .decode_payload::<CompanionClearRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            clear_with_grant(&payload).map_err(CompanionPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(CompanionPluginError::Invocation)
                        }),
                    (
                        capabilities::COMPANION_MANAGEMENT,
                        capabilities::COMPANION_MANAGEMENT_VERSION,
                        COMPANION_MANAGEMENT_SET_ENABLED_OPERATION,
                    ) => request
                        .decode_payload::<CompanionSetEnabledRequest>()
                        .map_err(CompanionPluginError::Invocation)
                        .and_then(|payload| {
                            set_enabled_with_grant(&payload)
                                .map_err(CompanionPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(CompanionPluginError::Invocation)
                        }),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "companion plugin does not support the requested operation".to_string(),
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
    Disabled,
    Management(CompanionManagementError),
    ManagementRequest(ManagementRequestError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl CompanionPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Management(CompanionManagementError::WriteNotGranted) => "write_not_granted",
            Self::Management(_) => "companion_management_error",
            Self::ManagementRequest(_) | Self::Invocation(_) => "invalid_request",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for CompanionPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Disabled => formatter.write_str("companion plugin is disabled by the user"),
            Self::Management(error) => error.fmt(formatter),
            Self::ManagementRequest(error) => error.fmt(formatter),
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
            Self::Management(error) => Some(error),
            Self::ManagementRequest(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Disabled | Self::UnexpectedHostMessage(_) => None,
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
