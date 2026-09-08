//! YunXi plugin handshake and typed dispatch for the MCP bridge.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use yunxi_protocol::{
    ActionGrantError, CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement,
    HostMessage, InvocationCodecError, InvocationResponse, McpServerState, McpStatusRequest,
    McpStatusResult, McpToolCallRequest, McpToolCancelRequest, McpToolCancelResult,
    McpToolListRequest, NetworkGrant, PluginMessage, ProtocolError, SecretGrant,
    TOOL_MCP_CALL_OPERATION, TOOL_MCP_CANCEL_OPERATION, TOOL_MCP_LIST_OPERATION,
    TOOL_MCP_STATUS_OPERATION, capabilities, connect_plugin_with_grants,
};

use crate::{McpClient, McpClientError, McpConfig, McpTransportKind};

pub const MCP_PLUGIN_ID: &str = "yunxi.tool.mcp";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run_mcp_plugin() -> Result<(), McpPluginError> {
    let config = McpConfig::from_env()?;
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_MCP, capabilities::TOOL_MCP_VERSION)?;
    let mut grants = vec![
        GrantRequirement::required(GrantKind::Approval),
        GrantRequirement::optional(GrantKind::WorkspaceRead),
        GrantRequirement::optional(GrantKind::WorkspaceWrite),
        GrantRequirement::optional(GrantKind::Secret),
    ];
    if config.transport_kind() == McpTransportKind::Http {
        grants.push(GrantRequirement::required(GrantKind::Network));
    } else {
        grants.push(GrantRequirement::optional(GrantKind::Network));
    }
    let mut session = connect_plugin_with_grants(
        MCP_PLUGIN_ID,
        "MCP tool bridge",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        grants,
        CONNECT_TIMEOUT,
    )?;
    let mut client = None;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_MCP
                    || request.capability().version() != capabilities::TOOL_MCP_VERSION
                    || !matches!(
                        request.operation(),
                        TOOL_MCP_LIST_OPERATION
                            | TOOL_MCP_CALL_OPERATION
                            | TOOL_MCP_CANCEL_OPERATION
                            | TOOL_MCP_STATUS_OPERATION
                    )
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "MCP bridge does not support the requested operation".to_string(),
                    )?;
                    continue;
                }

                match request.operation() {
                    TOOL_MCP_LIST_OPERATION => {
                        let list_request = request
                            .decode_payload::<McpToolListRequest>()
                            .map_err(McpPluginError::Invocation)?;
                        list_request
                            .validate()
                            .map_err(|error| McpPluginError::ProtocolPayload(error.to_string()))?;
                        let client = ensure_client(
                            &mut client,
                            &config,
                            list_request.network_grant(),
                            list_request.secret_grant(),
                        )?;
                        let result = if list_request.refresh() {
                            client.list_tools(DISCOVERY_TIMEOUT)
                        } else {
                            yunxi_protocol::McpToolListResult::new(
                                client.server_name(),
                                client.tools().to_vec(),
                                false,
                            )
                            .map_err(|error| {
                                McpClientError::InvalidResponse {
                                    method: "tools/list",
                                    message: error.to_string(),
                                }
                            })
                        };
                        let response = match result {
                            Ok(result) => encode_response(request_id, &result),
                            Err(error) => Err(McpPluginError::Client(error)),
                        };
                        match response {
                            Ok(response) => {
                                session.send(&PluginMessage::InvocationCompleted { response })?
                            }
                            Err(error) if error.is_fatal_client_error() => return Err(error),
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    error.code(),
                                    error.to_string(),
                                )?;
                            }
                        }
                    }
                    TOOL_MCP_CALL_OPERATION => {
                        let call = request
                            .decode_payload::<McpToolCallRequest>()
                            .map_err(McpPluginError::Invocation)?;
                        let client = ensure_client(&mut client, &config, None, None)?;
                        if call.server_name() != client.server_name() {
                            send_failure(
                                &mut session,
                                request_id,
                                "unknown_server",
                                format!("MCP server `{}` is not registered", call.server_name()),
                            )?;
                            continue;
                        }
                        let timeout = Duration::from_millis(call.grant().timeout_millis());
                        let result = match client.call_tool_with_grant(
                            call.tool_name(),
                            call.arguments().clone(),
                            call.grant(),
                            timeout,
                        ) {
                            Ok(result) => encode_response(request_id, &result),
                            Err(error) => Err(McpPluginError::Client(error)),
                        };
                        match result {
                            Ok(response) => {
                                session.send(&PluginMessage::InvocationCompleted { response })?
                            }
                            Err(error) if error.is_fatal_client_error() => return Err(error),
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    error.code(),
                                    error.to_string(),
                                )?;
                            }
                        }
                    }
                    TOOL_MCP_CANCEL_OPERATION => {
                        let cancel = request
                            .decode_payload::<McpToolCancelRequest>()
                            .map_err(McpPluginError::Invocation)?;
                        let result: Result<McpToolCancelResult, McpPluginError> =
                            if let Some(client) = client.as_mut() {
                                if cancel.server_name() != client.server_name() {
                                    send_failure(
                                        &mut session,
                                        request_id,
                                        "unknown_server",
                                        format!(
                                            "MCP server `{}` is not registered",
                                            cancel.server_name()
                                        ),
                                    )?;
                                    continue;
                                }
                                client
                                    .cancel_request(cancel.request_id(), cancel.reason())
                                    .map_err(McpPluginError::Client)
                                    .and_then(|()| {
                                        McpToolCancelResult::new(
                                            client.server_name(),
                                            cancel.request_id(),
                                            true,
                                            true,
                                        )
                                        .map_err(|error| {
                                            McpPluginError::ProtocolPayload(error.to_string())
                                        })
                                    })
                            } else {
                                McpToolCancelResult::new(
                                    cancel.server_name(),
                                    cancel.request_id(),
                                    false,
                                    true,
                                )
                                .map_err(|error| McpPluginError::ProtocolPayload(error.to_string()))
                            };
                        match result {
                            Ok(result) => {
                                let response = encode_response(request_id, &result)?;
                                session.send(&PluginMessage::InvocationCompleted { response })?;
                            }
                            Err(McpPluginError::Client(error)) if error.is_fatal() => {
                                return Err(McpPluginError::Client(error));
                            }
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    error.code(),
                                    error.to_string(),
                                )?;
                            }
                        }
                    }
                    TOOL_MCP_STATUS_OPERATION => {
                        let status = request
                            .decode_payload::<McpStatusRequest>()
                            .map_err(McpPluginError::Invocation)?;
                        let client = ensure_client(&mut client, &config, None, None)?;
                        if status.server_name() != client.server_name() {
                            send_failure(
                                &mut session,
                                request_id,
                                "unknown_server",
                                format!("MCP server `{}` is not registered", status.server_name()),
                            )?;
                            continue;
                        }
                        let result = McpStatusResult::new(
                            client.server_name(),
                            McpServerState::Ready,
                            client.tools().len(),
                            None,
                        )
                        .map_err(|error| {
                            McpPluginError::Client(McpClientError::InvalidResponse {
                                method: "status",
                                message: error.to_string(),
                            })
                        })?;
                        let response = encode_response(request_id, &result)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    _ => unreachable!("operation checked above"),
                }
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(McpPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn ensure_client<'a>(
    client: &'a mut Option<McpClient>,
    config: &McpConfig,
    network_grant: Option<&NetworkGrant>,
    secret_grant: Option<&SecretGrant>,
) -> Result<&'a mut McpClient, McpPluginError> {
    if client.is_none() {
        *client = Some(McpClient::start_with_authority(
            config,
            DISCOVERY_TIMEOUT,
            network_grant.cloned().unwrap_or_default(),
            secret_grant.cloned().unwrap_or_default(),
        )?);
    }
    client
        .as_mut()
        .ok_or(McpPluginError::Client(McpClientError::ChildStopped))
}

fn encode_response<T: serde::Serialize>(
    request_id: u64,
    result: &T,
) -> Result<InvocationResponse, McpPluginError> {
    InvocationResponse::encode(request_id, result).map_err(McpPluginError::Invocation)
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
pub enum McpPluginError {
    Config(crate::McpConfigError),
    Client(McpClientError),
    Capability(CapabilityError),
    Grant(ActionGrantError),
    Invocation(InvocationCodecError),
    ProtocolPayload(String),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl McpPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::Config(_) | Self::Capability(_) | Self::Protocol(_) => "plugin_error",
            Self::Grant(_) => "grant_denied",
            Self::Invocation(_) | Self::ProtocolPayload(_) => "invalid_request",
            Self::Client(McpClientError::Remote { .. }) => "mcp_remote_error",
            Self::Client(McpClientError::UnknownTool { .. }) => "unknown_tool",
            Self::Client(McpClientError::NetworkDenied { .. }) => "network_denied",
            Self::Client(McpClientError::SecretDenied { .. }) => "secret_denied",
            Self::Client(
                McpClientError::HttpStatus { .. } | McpClientError::HttpConfiguration { .. },
            ) => "mcp_http_error",
            Self::Client(_) => "mcp_protocol_error",
            Self::UnexpectedHostMessage(_) => "plugin_error",
        }
    }

    fn is_fatal_client_error(&self) -> bool {
        matches!(self, Self::Client(error) if error.is_fatal())
    }
}

impl fmt::Display for McpPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Client(error) => error.fmt(formatter),
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Grant(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::ProtocolPayload(message) => formatter.write_str(message),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for McpPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Client(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Grant(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::ProtocolPayload(_) => None,
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<crate::McpConfigError> for McpPluginError {
    fn from(error: crate::McpConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<McpClientError> for McpPluginError {
    fn from(error: McpClientError) -> Self {
        Self::Client(error)
    }
}

impl From<CapabilityError> for McpPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<ActionGrantError> for McpPluginError {
    fn from(error: ActionGrantError) -> Self {
        Self::Grant(error)
    }
}

impl From<InvocationCodecError> for McpPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<String> for McpPluginError {
    fn from(error: String) -> Self {
        Self::ProtocolPayload(error)
    }
}

impl From<ProtocolError> for McpPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
