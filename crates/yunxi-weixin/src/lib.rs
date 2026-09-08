//! Stable, bounded contracts for isolated YunXi Weixin plugins.
//!
//! The crate keeps the network and credential boundaries replaceable and
//! bounded; production integration is exposed as a blocking library facade.
#![forbid(unsafe_code)]

mod adapter;
mod bridge;
mod control;
mod error;
mod fixture;
mod identifiers;
mod ilink;
mod media;
mod message;
mod plugin;
mod poll_worker;
mod process_plugin;
mod runtime;
mod secret_store;
mod state;

pub use adapter::{
    HttpRequest, HttpResponse, HttpTransport, MAX_ENDPOINT_BYTES, MAX_REPLAY_ENTRIES,
    MAX_SECRET_BYTES, MAX_SECRET_REF_BYTES, MAX_WEBHOOK_BODY_BYTES, MockTransport, OutboundResult,
    SecretError, SecretMaterial, SecretRef, SecretResolver, SignatureAlgorithm, SignatureError,
    SignatureInput, SignatureVerifier, StaticSecretResolver, TransportError, WebhookAck,
    WebhookAdapter, WebhookAdapterState, WebhookConfig, WebhookRequest, WeixinAdapterError,
    WeixinSha1Verifier,
};
pub use bridge::{
    AgentBridge, AgentBridgeError, AgentBridgeSnapshot, AgentBridgeState, AgentWorkItem,
    MAX_AGENT_ATTEMPT_ID_BYTES, MAX_AGENT_ATTEMPTS, MAX_AGENT_ERROR_BYTES,
};
pub use control::{CancellationToken, RequestContext, RequestControlError};
pub use error::WeixinContractError;
pub use fixture::{InboundFixture, OutboundFixture, inbound_fixture, outbound_fixture};
pub use identifiers::{
    IdempotencyKey, MAX_MEDIA_ID_BYTES, MediaId, MessageId, ParticipantId, RequestId, SessionId,
};
pub use ilink::{
    DEFAULT_ILINK_REQUEST_TIMEOUT, IlinkCdnMedia, IlinkError, IlinkHttpConfig, IlinkHttpTransport,
    IlinkMessage, IlinkMessageItem, IlinkTextItem, IlinkTransport, IlinkVoiceItem,
    LoopbackIlinkTransport, MAX_ILINK_ID_BYTES, MAX_ILINK_MESSAGES, MAX_ILINK_REQUEST_BYTES,
    MAX_ILINK_RESPONSE_BYTES, MAX_ILINK_TEXT_BYTES, MAX_LONG_POLL_TIMEOUT_MS, MAX_QR_CONTENT_BYTES,
    PRODUCTION_ILINK_ENDPOINT, PollBatch, QrChallenge, QrPoll, QrStatus, SendResult,
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
pub use poll_worker::{
    LongPollCompletion, LongPollError, LongPollOptions, LongPollSnapshot, LongPollState,
    LongPollWorker, PolledBatch,
};
pub use process_plugin::{
    DOCTOR_OPERATION as RUNTIME_DOCTOR_OPERATION, LOGIN_OPERATION, LOGOUT_OPERATION,
    PAIR_OPERATION, POLL_LOGIN_OPERATION, QUEUED_MESSAGES_OPERATION, REMOTE_CONTROL_OPERATION,
    REPLY_TEXT_OPERATION, SEND_MESSAGE_OPERATION, SERVE_OPERATION, SERVE_START_OPERATION,
    SERVE_STATUS_OPERATION, SERVE_STOP_OPERATION, SESSION_OPERATION, STATUS_OPERATION,
    WEIXIN_ACCOUNT_ENV, WEIXIN_MASTER_KEY_ENV, WEIXIN_MASTER_KEY_HEX_ENV, WEIXIN_MODE_ENV,
    WEIXIN_SECRET_STORE_ENV, WEIXIN_TOKEN_REF_ENV, WeixinPairRequest, WeixinPollLoginRequest,
    WeixinProcessPluginError, WeixinQueuedMessagesRequest, WeixinRemoteControlRequest,
    WeixinReplyTextRequest, WeixinSendMessageRequest, WeixinServeLifecycleSnapshot,
    WeixinServeLifecycleState, WeixinServeRequest, WeixinServeSnapshot, WeixinSessionRequest,
    run_weixin_plugin_from_env, weixin_production_requested,
};
pub use runtime::{
    ControlResult, DoctorCheck, DoctorReport, LoginOptions, LoginReport, LoginState, LogoutReport,
    PairAction, PairReport, PairRequest, PairState, QueuedMessageSnapshot, RemoteCommand,
    RemoteControlState, ServeOptions, ServeReport, SessionBinding, SessionCommand, SessionReport,
    StatusReport, WeixinControlPlane, WeixinRuntimeError,
};
pub use secret_store::{
    FileSecretStore, MASTER_KEY_BYTES, MAX_SECRET_ENTRIES, MAX_SECRET_FILE_BYTES,
    MAX_TOTAL_SECRET_BYTES, MemorySecretStore, SecretStore, SecretStoreError,
};
pub use state::{
    AckState, BackpressureState, CancellationState, DeliveryState, DeliveryStatus,
    MAX_BUFFER_CAPACITY_BYTES, MAX_CANCELLATION_REASON_BYTES, MAX_RETRY_ATTEMPTS,
    MAX_RETRY_ERROR_BYTES, RetryState,
};
