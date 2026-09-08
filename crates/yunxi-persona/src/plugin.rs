//! Plugin-side handshake and request dispatch for persona context compilation.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationResponse, ManagementRequestError,
    PERSONA_CONTEXT_COMPILE_OPERATION, PERSONA_MANAGEMENT_IMPORT_OPERATION,
    PERSONA_MANAGEMENT_LIST_OPERATION, PERSONA_MANAGEMENT_PROFILE_OPERATION,
    PERSONA_MANAGEMENT_RESET_OPERATION, PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION,
    PERSONA_MANAGEMENT_SET_ENABLED_OPERATION, PERSONA_MANAGEMENT_STATUS_OPERATION,
    PersonaContextRequest, PersonaImportRequest, PersonaListRequest, PersonaProfileRequest,
    PersonaResetRequest, PersonaSetActiveRequest, PersonaSetEnabledRequest, PersonaStatusRequest,
    PluginMessage, ProtocolError, capabilities, connect_plugin_with_grants,
};

use crate::{
    PersonaCompileError, PersonaManagementError, compile_context, import_profile_json_with_grant,
    list_profiles_with_grant, profile_with_grant, reset_with_grant, set_active_profile_with_grant,
    set_enabled_with_grant, status_with_grant,
};

pub const PERSONA_PLUGIN_ID: &str = "yunxi.persona";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_persona_plugin() -> Result<(), PersonaPluginError> {
    let context_capability = CapabilityDescriptor::new(
        capabilities::PERSONA_CONTEXT,
        capabilities::PERSONA_CONTEXT_VERSION,
    )?;
    let management_capability = CapabilityDescriptor::new(
        capabilities::PERSONA_MANAGEMENT,
        capabilities::PERSONA_MANAGEMENT_VERSION,
    )?;
    let mut session = connect_plugin_with_grants(
        PERSONA_PLUGIN_ID,
        "Persona context compiler",
        env!("CARGO_PKG_VERSION"),
        vec![context_capability, management_capability],
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
                        capabilities::PERSONA_CONTEXT,
                        capabilities::PERSONA_CONTEXT_VERSION,
                        PERSONA_CONTEXT_COMPILE_OPERATION,
                    ) => request
                        .decode_payload::<PersonaContextRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            compile_context(&payload).map_err(PersonaPluginError::Compile)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_STATUS_OPERATION,
                    ) => request
                        .decode_payload::<PersonaStatusRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            InvocationResponse::encode(
                                request_id,
                                &status_with_grant(payload.grant()),
                            )
                            .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_LIST_OPERATION,
                    ) => request
                        .decode_payload::<PersonaListRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            InvocationResponse::encode(
                                request_id,
                                &list_profiles_with_grant(payload.grant()),
                            )
                            .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_PROFILE_OPERATION,
                    ) => request
                        .decode_payload::<PersonaProfileRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(PersonaPluginError::ManagementRequest)?;
                            InvocationResponse::encode(
                                request_id,
                                &profile_with_grant(payload.grant(), payload.id()),
                            )
                            .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_IMPORT_OPERATION,
                    ) => request
                        .decode_payload::<PersonaImportRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(PersonaPluginError::ManagementRequest)?;
                            import_profile_json_with_grant(payload.grant(), payload.profile_json())
                                .map_err(PersonaPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION,
                    ) => request
                        .decode_payload::<PersonaSetActiveRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            payload
                                .validate()
                                .map_err(PersonaPluginError::ManagementRequest)?;
                            set_active_profile_with_grant(payload.grant(), payload.id())
                                .map_err(PersonaPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_SET_ENABLED_OPERATION,
                    ) => request
                        .decode_payload::<PersonaSetEnabledRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            set_enabled_with_grant(payload.grant(), payload.enabled())
                                .map_err(PersonaPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(PersonaPluginError::Invocation)
                        }),
                    (
                        capabilities::PERSONA_MANAGEMENT,
                        capabilities::PERSONA_MANAGEMENT_VERSION,
                        PERSONA_MANAGEMENT_RESET_OPERATION,
                    ) => request
                        .decode_payload::<PersonaResetRequest>()
                        .map_err(PersonaPluginError::Invocation)
                        .and_then(|payload| {
                            reset_with_grant(payload.grant())
                                .map_err(PersonaPluginError::Management)
                        })
                        .and_then(|result| {
                            InvocationResponse::encode(request_id, &result)
                                .map_err(PersonaPluginError::Invocation)
                        }),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "persona plugin does not support the requested operation".to_string(),
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
    Management(PersonaManagementError),
    ManagementRequest(ManagementRequestError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl PersonaPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invocation(_) | Self::ManagementRequest(_) => "invalid_request",
            Self::Compile(_) => "persona_compile_error",
            Self::Management(PersonaManagementError::WriteNotGranted) => "write_not_granted",
            Self::Management(_) => "persona_management_error",
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for PersonaPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Compile(error) => error.fmt(formatter),
            Self::Management(error) => error.fmt(formatter),
            Self::ManagementRequest(error) => error.fmt(formatter),
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
            Self::Management(error) => Some(error),
            Self::ManagementRequest(error) => Some(error),
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
