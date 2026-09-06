//! Stable, bounded contracts for isolated YunXi Weixin plugins.
//!
//! The crate intentionally contains no Weixin SDK, network, credentials,
//! device, media bytes, or async runtime integration.
#![forbid(unsafe_code)]

mod adapter;
mod control;
mod error;
mod fixture;
mod identifiers;
mod media;
mod message;
mod plugin;
mod state;

pub use adapter::{
    HttpRequest, HttpResponse, HttpTransport, MAX_ENDPOINT_BYTES, MAX_REPLAY_ENTRIES,
    MAX_SECRET_BYTES, MAX_SECRET_REF_BYTES, MAX_WEBHOOK_BODY_BYTES, MockTransport, OutboundResult,
    SecretError, SecretMaterial, SecretRef, SecretResolver, SignatureAlgorithm, SignatureError,
    SignatureInput, SignatureVerifier, StaticSecretResolver, TransportError, WebhookAck,
    WebhookAdapter, WebhookAdapterState, WebhookConfig, WebhookRequest, WeixinAdapterError,
    WeixinSha1Verifier,
};
pub use control::{CancellationToken, RequestContext, RequestControlError};
pub use error::WeixinContractError;
pub use fixture::{InboundFixture, OutboundFixture, inbound_fixture, outbound_fixture};
pub use identifiers::{
    IdempotencyKey, MAX_MEDIA_ID_BYTES, MediaId, MessageId, ParticipantId, RequestId, SessionId,
};
pub use media::{
    MAX_DECLARED_MEDIA_BYTES, MAX_DIMENSION, MAX_FILE_NAME_BYTES, MAX_MEDIA_DURATION_MS,
    MAX_MEDIA_ITEMS, MAX_MEDIA_METADATA_BYTES, MAX_MIME_TYPE_BYTES, MediaKind, MediaMetadata,
};
pub use message::{
    CapabilityDescriptor, Direction, INBOUND_CAPABILITY, INBOUND_CONTRACT, InboundMessage,
    MAX_MESSAGE_TEXT_BYTES, MessageContent, MessageEnvelope, OUTBOUND_CAPABILITY,
    OUTBOUND_CONTRACT, OutboundMessage, WEIXIN_CAPABILITY_VERSION, WeixinCapability,
};
pub use plugin::{
    ACK_OPERATION, CANCEL_OPERATION, CHANNEL_CONTRACT, ChannelMessage, DESCRIBE_OPERATION,
    EmptyRequest, FAIL_OPERATION, FIXTURE_OPERATION, FixtureDirection, FixtureRequest,
    INBOUND_OPERATION, MessageMutationRequest, OUTBOUND_OPERATION, WEIXIN_PLUGIN_ID,
    WeixinPluginError, WeixinPluginResponse, run_weixin_plugin,
};
pub use state::{
    AckState, BackpressureState, CancellationState, DeliveryState, DeliveryStatus,
    MAX_BUFFER_CAPACITY_BYTES, MAX_CANCELLATION_REASON_BYTES, MAX_RETRY_ATTEMPTS,
    MAX_RETRY_ERROR_BYTES, RetryState,
};
