//! Process-isolated Host plugin for the versioned Weixin channel contract.
//!
//! This is a deterministic loopback adapter. It proves the production
//! boundary (manifest, grants, typed messages, idempotency, mutation, and
//! shutdown) without pretending to implement Weixin login or network I/O.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationRequest, InvocationResponse, PluginManifest, PluginMessage,
    PluginRiskLevel, PluginRuntimeMetadata, ProtocolError, capabilities,
    connect_plugin_with_manifest,
};

use crate::{
    IdempotencyKey, InboundMessage, OutboundMessage, WeixinContractError, inbound_fixture,
    outbound_fixture,
};

pub const WEIXIN_PLUGIN_ID: &str = "yunxi.channel.weixin";
pub const INBOUND_OPERATION: &str = "inbound";
pub const OUTBOUND_OPERATION: &str = "outbound";
pub const FIXTURE_OPERATION: &str = "fixture";
pub const ACK_OPERATION: &str = "ack";
pub const CANCEL_OPERATION: &str = "cancel";
pub const FAIL_OPERATION: &str = "fail";
pub const DESCRIBE_OPERATION: &str = "describe";
pub const CHANNEL_CONTRACT: &str = "channel.weixin@1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STORED_MESSAGES: usize = 1024;

/// A typed message crossing the channel plugin boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "direction", content = "message", rename_all = "snake_case")]
pub enum ChannelMessage {
    Inbound(InboundMessage),
    Outbound(OutboundMessage),
}

impl ChannelMessage {
    fn idempotency_key(&self) -> &str {
        match self {
            Self::Inbound(message) => message.envelope().idempotency_key.as_str(),
            Self::Outbound(message) => message.envelope().idempotency_key.as_str(),
        }
    }

    fn same_message(&self, other: &Self) -> bool {
        let (left, right) = match (self, other) {
            (Self::Inbound(left), Self::Inbound(right)) => (left.envelope(), right.envelope()),
            (Self::Outbound(left), Self::Outbound(right)) => (left.envelope(), right.envelope()),
            _ => return false,
        };
        left.message_id == right.message_id
            && left.session_id == right.session_id
            && left.request_id == right.request_id
            && left.idempotency_key == right.idempotency_key
            && left.direction == right.direction
            && left.sender_id == right.sender_id
            && left.recipient_id == right.recipient_id
            && left.sent_at_ms == right.sent_at_ms
            && left.content == right.content
    }

    fn mutate<F>(&mut self, operation: F) -> Result<(), WeixinContractError>
    where
        F: FnOnce(&mut crate::DeliveryStatus) -> Result<(), WeixinContractError>,
    {
        match self {
            Self::Inbound(message) => {
                let mut envelope = message.clone().into_envelope();
                operation(&mut envelope.delivery)?;
                *message = InboundMessage::new(envelope)?;
            }
            Self::Outbound(message) => {
                let mut envelope = message.clone().into_envelope();
                operation(&mut envelope.delivery)?;
                *message = OutboundMessage::new(envelope)?;
            }
        }
        Ok(())
    }
}

/// Direction used by the deterministic `fixture` operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRequest {
    pub direction: FixtureDirection,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageMutationRequest {
    pub idempotency_key: IdempotencyKey,
    pub reason: Option<String>,
}

impl MessageMutationRequest {
    fn reason(&self, operation: &'static str) -> Result<&str, WeixinPluginError> {
        self.reason.as_deref().ok_or_else(|| {
            WeixinPluginError::InvalidRequest(format!("{operation} requires a non-empty reason"))
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WeixinPluginResponse {
    Accepted {
        idempotency_key: String,
        duplicate: bool,
        message: ChannelMessage,
    },
    Mutated {
        operation: String,
        idempotency_key: String,
        message: ChannelMessage,
    },
    Description {
        plugin: String,
        capability: String,
        contract: String,
        mode: String,
        real_weixin: bool,
    },
    Runtime {
        operation: String,
        mode: String,
        report: serde_json::Value,
    },
}

#[derive(Default)]
pub(crate) struct ChannelRuntime {
    messages: BTreeMap<String, ChannelMessage>,
}

impl ChannelRuntime {
    fn register(
        &mut self,
        message: ChannelMessage,
    ) -> Result<WeixinPluginResponse, WeixinPluginError> {
        let key = message.idempotency_key().to_owned();
        if let Some(existing) = self.messages.get(&key) {
            if existing.same_message(&message) {
                return Ok(WeixinPluginResponse::Accepted {
                    idempotency_key: key,
                    duplicate: true,
                    message: existing.clone(),
                });
            }
            return Err(WeixinPluginError::IdempotencyConflict(key));
        }
        if self.messages.len() >= MAX_STORED_MESSAGES {
            return Err(WeixinPluginError::CapacityExceeded {
                maximum: MAX_STORED_MESSAGES,
            });
        }
        self.messages.insert(key.clone(), message.clone());
        Ok(WeixinPluginResponse::Accepted {
            idempotency_key: key,
            duplicate: false,
            message,
        })
    }

    fn mutate(
        &mut self,
        request: &MessageMutationRequest,
        operation: &'static str,
        action: impl FnOnce(&mut crate::DeliveryStatus) -> Result<(), WeixinContractError>,
    ) -> Result<WeixinPluginResponse, WeixinPluginError> {
        let key = request.idempotency_key.as_str();
        let message = self
            .messages
            .get_mut(key)
            .ok_or_else(|| WeixinPluginError::UnknownMessage(key.to_owned()))?;
        message.mutate(action)?;
        Ok(WeixinPluginResponse::Mutated {
            operation: operation.to_owned(),
            idempotency_key: key.to_owned(),
            message: message.clone(),
        })
    }
}

/// Run the Weixin channel fixture as a protocol plugin process.
pub fn run_weixin_plugin() -> Result<(), WeixinPluginError> {
    run_weixin_loopback_plugin("loopback-fixture", "weixin-channel")
}

fn run_weixin_loopback_plugin(
    mode: &'static str,
    adapter: &'static str,
) -> Result<(), WeixinPluginError> {
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .map_err(WeixinPluginError::Capability)?;
    let manifest = PluginManifest::new(
        WEIXIN_PLUGIN_ID,
        "YunXi Weixin channel fixture",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
    )
    .with_grants(vec![
        GrantRequirement::required(GrantKind::Network),
        GrantRequirement::required(GrantKind::Secret),
    ])
    .with_runtime_metadata(PluginRuntimeMetadata::new(
        adapter,
        PluginRiskLevel::External,
    ));
    let mut session = connect_plugin_with_manifest(manifest, CONNECT_TIMEOUT)?;
    let mut runtime = ChannelRuntime::default();

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                match dispatch_with_mode(&mut runtime, &request, mode, false) {
                    Ok(response) => {
                        let response = InvocationResponse::encode(request_id, &response)?;
                        session.send(&PluginMessage::InvocationCompleted { response })?;
                    }
                    Err(error) => {
                        send_failure(&mut session, request_id, error.code(), error.to_string())?;
                    }
                }
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(WeixinPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

#[cfg(test)]
fn dispatch(
    runtime: &mut ChannelRuntime,
    request: &InvocationRequest,
) -> Result<WeixinPluginResponse, WeixinPluginError> {
    dispatch_with_mode(runtime, request, "loopback-fixture", false)
}

pub(crate) fn dispatch_with_mode(
    runtime: &mut ChannelRuntime,
    request: &InvocationRequest,
    mode: &str,
    real_weixin: bool,
) -> Result<WeixinPluginResponse, WeixinPluginError> {
    if request.capability().id().as_str() != capabilities::CHANNEL_WEIXIN
        || request.capability().version() != capabilities::CHANNEL_WEIXIN_VERSION
    {
        return Err(WeixinPluginError::UnsupportedOperation);
    }

    match request.operation() {
        INBOUND_OPERATION => {
            let message = request
                .decode_payload::<InboundMessage>()
                .map_err(WeixinPluginError::Invocation)?;
            runtime.register(ChannelMessage::Inbound(message))
        }
        OUTBOUND_OPERATION => {
            let message = request
                .decode_payload::<OutboundMessage>()
                .map_err(WeixinPluginError::Invocation)?;
            runtime.register(ChannelMessage::Outbound(message))
        }
        FIXTURE_OPERATION => {
            let request = request
                .decode_payload::<FixtureRequest>()
                .map_err(WeixinPluginError::Invocation)?;
            let message = match request.direction {
                FixtureDirection::Inbound => ChannelMessage::Inbound(inbound_fixture()?.message),
                FixtureDirection::Outbound => ChannelMessage::Outbound(outbound_fixture()?.message),
            };
            runtime.register(message)
        }
        ACK_OPERATION => {
            let request = request
                .decode_payload::<MessageMutationRequest>()
                .map_err(WeixinPluginError::Invocation)?;
            runtime.mutate(&request, ACK_OPERATION, |delivery| delivery.acknowledge())
        }
        CANCEL_OPERATION => {
            let request = request
                .decode_payload::<MessageMutationRequest>()
                .map_err(WeixinPluginError::Invocation)?;
            let reason = request.reason(CANCEL_OPERATION)?.to_owned();
            runtime.mutate(&request, CANCEL_OPERATION, move |delivery| {
                delivery.request_cancel(reason)?;
                delivery.mark_cancelled()
            })
        }
        FAIL_OPERATION => {
            let request = request
                .decode_payload::<MessageMutationRequest>()
                .map_err(WeixinPluginError::Invocation)?;
            let reason = request.reason(FAIL_OPERATION)?.to_owned();
            runtime.mutate(&request, FAIL_OPERATION, move |delivery| {
                delivery.mark_failed(reason)
            })
        }
        DESCRIBE_OPERATION => {
            request
                .decode_payload::<EmptyRequest>()
                .map_err(WeixinPluginError::Invocation)?;
            Ok(WeixinPluginResponse::Description {
                plugin: WEIXIN_PLUGIN_ID.to_owned(),
                capability: capabilities::CHANNEL_WEIXIN.to_owned(),
                contract: CHANNEL_CONTRACT.to_owned(),
                mode: mode.to_owned(),
                real_weixin,
            })
        }
        _ => Err(WeixinPluginError::UnsupportedOperation),
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
        code: code.to_owned(),
        message,
        retryable: false,
    })
}

#[derive(Debug)]
pub enum WeixinPluginError {
    CapacityExceeded { maximum: usize },
    Capability(CapabilityError),
    Contract(WeixinContractError),
    IdempotencyConflict(String),
    InvalidRequest(String),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnsupportedOperation,
    UnexpectedHostMessage(String),
    UnknownMessage(String),
}

impl WeixinPluginError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) | Self::Invocation(_) => "invalid_request",
            Self::IdempotencyConflict(_) => "idempotency_conflict",
            Self::UnknownMessage(_) => "unknown_message",
            Self::CapacityExceeded { .. } => "capacity_exceeded",
            Self::Contract(_) => "contract_error",
            Self::Capability(_) => "plugin_error",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::Protocol(_) | Self::UnexpectedHostMessage(_) => "plugin_error",
        }
    }
}

impl fmt::Display for WeixinPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { maximum } => {
                write!(formatter, "channel message capacity {maximum} was reached")
            }
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Contract(error) => error.fmt(formatter),
            Self::IdempotencyConflict(key) => {
                write!(
                    formatter,
                    "idempotency key `{key}` is bound to another message"
                )
            }
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnsupportedOperation => {
                formatter.write_str("weixin plugin does not support this operation")
            }
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
            Self::UnknownMessage(key) => {
                write!(
                    formatter,
                    "no message is stored for idempotency key `{key}`"
                )
            }
        }
    }
}

impl Error for WeixinPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Contract(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl From<CapabilityError> for WeixinPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<WeixinContractError> for WeixinPluginError {
    fn from(error: WeixinContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<InvocationCodecError> for WeixinPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for WeixinPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdempotencyKey, inbound_fixture, outbound_fixture};

    fn request<T: Serialize>(operation: &str, payload: &T) -> InvocationRequest {
        InvocationRequest::encode(
            1,
            CapabilityDescriptor::new(
                capabilities::CHANNEL_WEIXIN,
                capabilities::CHANNEL_WEIXIN_VERSION,
            )
            .expect("capability"),
            operation,
            payload,
        )
        .expect("request")
    }

    #[test]
    fn idempotent_registration_and_mutation_are_typed() {
        let mut runtime = ChannelRuntime::default();
        let inbound = inbound_fixture().expect("inbound fixture").message;
        let first = dispatch(&mut runtime, &request(INBOUND_OPERATION, &inbound))
            .expect("register inbound");
        assert!(matches!(
            first,
            WeixinPluginResponse::Accepted {
                duplicate: false,
                ..
            }
        ));
        let duplicate = dispatch(&mut runtime, &request(INBOUND_OPERATION, &inbound))
            .expect("duplicate inbound");
        assert!(matches!(
            duplicate,
            WeixinPluginResponse::Accepted {
                duplicate: true,
                ..
            }
        ));

        let key = IdempotencyKey::new("fixture-inbound-key").expect("key");
        let ack = dispatch(
            &mut runtime,
            &request(
                ACK_OPERATION,
                &MessageMutationRequest {
                    idempotency_key: key,
                    reason: None,
                },
            ),
        )
        .expect("ack");
        assert!(matches!(ack, WeixinPluginResponse::Mutated { .. }));
    }

    #[test]
    fn wrong_direction_and_unknown_operation_fail_closed() {
        let outbound = outbound_fixture().expect("outbound fixture").message;
        let error = dispatch(
            &mut ChannelRuntime::default(),
            &request(INBOUND_OPERATION, &outbound),
        )
        .expect_err("wrong direction must fail");
        assert!(matches!(error, WeixinPluginError::Invocation(_)));

        let error = dispatch(
            &mut ChannelRuntime::default(),
            &request("unknown", &EmptyRequest {}),
        )
        .expect_err("unknown operation must fail");
        assert!(matches!(error, WeixinPluginError::UnsupportedOperation));
    }
}
