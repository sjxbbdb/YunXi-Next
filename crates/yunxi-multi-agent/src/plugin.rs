//! Plugin handshake and typed dispatch for multi-agent coordination.

use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use yunxi_protocol::{
    AgentInterruptRequest, AgentListRequest, AgentSpawnRequest, AgentTurnCompleteRequest,
    AgentTurnFailRequest, AgentTurnStartRequest, CapabilityDescriptor, CapabilityError, GrantKind,
    GrantRequirement, HostMessage, InvocationCodecError, InvocationResponse, PluginMessage,
    ProtocolError, TOOL_MULTI_AGENT_INTERRUPT_OPERATION, TOOL_MULTI_AGENT_LIST_OPERATION,
    TOOL_MULTI_AGENT_SPAWN_OPERATION, TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION,
    TOOL_MULTI_AGENT_TURN_FAIL_OPERATION, TOOL_MULTI_AGENT_TURN_START_OPERATION, capabilities,
    connect_plugin_with_grants,
};

use crate::{CoordinatorStore, MultiAgentStoreError};

pub const MULTI_AGENT_PLUGIN_ID: &str = "yunxi.multi-agent";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_multi_agent_plugin() -> Result<(), MultiAgentPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::TOOL_MULTI_AGENT,
        capabilities::TOOL_MULTI_AGENT_VERSION,
    )?;
    let mut session = connect_plugin_with_grants(
        MULTI_AGENT_PLUGIN_ID,
        "Isolated multi-agent coordinator",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        vec![
            GrantRequirement::required(GrantKind::Approval),
            GrantRequirement::required(GrantKind::WorkspaceRead),
            GrantRequirement::required(GrantKind::WorkspaceWrite),
            GrantRequirement::required(GrantKind::AgentDelegation),
        ],
        CONNECT_TIMEOUT,
    )?;
    let instance_id = coordinator_instance_id();

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_MULTI_AGENT
                    || request.capability().version() != capabilities::TOOL_MULTI_AGENT_VERSION
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_capability",
                        "multi-agent plugin does not support the requested capability".to_string(),
                    )?;
                    continue;
                }

                let response = match request.operation() {
                    TOOL_MULTI_AGENT_SPAWN_OPERATION => request
                        .decode_payload::<AgentSpawnRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.spawn(&payload))
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    TOOL_MULTI_AGENT_LIST_OPERATION => request
                        .decode_payload::<AgentListRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.list())
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    TOOL_MULTI_AGENT_TURN_START_OPERATION => request
                        .decode_payload::<AgentTurnStartRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.start_turn(&payload))
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION => request
                        .decode_payload::<AgentTurnCompleteRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.complete_turn(&payload))
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    TOOL_MULTI_AGENT_TURN_FAIL_OPERATION => request
                        .decode_payload::<AgentTurnFailRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.fail_turn(&payload))
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    TOOL_MULTI_AGENT_INTERRUPT_OPERATION => request
                        .decode_payload::<AgentInterruptRequest>()
                        .map_err(MultiAgentPluginError::Invocation)
                        .and_then(|payload| {
                            CoordinatorStore::from_grant(payload.grant(), &instance_id)
                                .and_then(|store| store.interrupt(&payload))
                                .map_err(MultiAgentPluginError::Store)
                        })
                        .and_then(|result| encode(request_id, &result)),
                    _ => {
                        send_failure(
                            &mut session,
                            request_id,
                            "unsupported_operation",
                            "multi-agent plugin does not support the requested operation"
                                .to_string(),
                        )?;
                        continue;
                    }
                };

                match response {
                    Ok(response) => {
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?;
                    }
                }
            }
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(MultiAgentPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn encode<T: serde::Serialize>(
    request_id: u64,
    result: &T,
) -> Result<InvocationResponse, MultiAgentPluginError> {
    InvocationResponse::encode(request_id, result).map_err(MultiAgentPluginError::Invocation)
}

fn coordinator_instance_id() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("coordinator-{}-{stamp}", std::process::id())
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
pub enum MultiAgentPluginError {
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Store(MultiAgentStoreError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl MultiAgentPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invocation(_) => "invalid_request",
            Self::Store(error) => error.code(),
            Self::Capability(_) | Self::Protocol(_) | Self::UnexpectedHostMessage(_) => {
                "plugin_error"
            }
        }
    }
}

impl fmt::Display for MultiAgentPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Store(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for MultiAgentPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for MultiAgentPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for MultiAgentPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for MultiAgentPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
