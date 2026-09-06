//! Minimal replaceable HTTP/Webhook boundary for the Weixin channel.
//!
//! This module intentionally stops at the adapter boundary.  A production
//! plugin supplies an HTTPS transport and a Host-backed secret resolver; the
//! crate itself never logs or serializes secret material and never claims to
//! perform Weixin login.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::control::{RequestContext, RequestControlError};
use crate::{IdempotencyKey, InboundMessage, OutboundMessage, WeixinContractError};

pub const MAX_SECRET_REF_BYTES: usize = 256;
pub const MAX_SECRET_BYTES: usize = 4096;
pub const MAX_ENDPOINT_BYTES: usize = 2048;
pub const MAX_WEBHOOK_BODY_BYTES: usize = 256 * 1024;
pub const MAX_REPLAY_ENTRIES: usize = 8192;

/// A reference understood by the Host, never the secret value itself.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Result<Self, WeixinAdapterError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SECRET_REF_BYTES {
            return Err(WeixinAdapterError::InvalidConfig("secret_ref"));
        }
        if !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, ':' | '.' | '_' | '-' | '/')
        }) {
            return Err(WeixinAdapterError::InvalidConfig("secret_ref"));
        }
        if !value.starts_with("env:")
            && !value.starts_with("host:")
            && !value.starts_with("keychain:")
        {
            return Err(WeixinAdapterError::InvalidConfig("secret_ref"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef(<redacted>)")
    }
}

impl Serialize for SecretRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Secret bytes are scoped to one operation and zeroed on drop.  The type has
/// no serde or public Debug implementation that could accidentally disclose it.
pub struct SecretMaterial(Vec<u8>);

impl SecretMaterial {
    pub fn from_bytes(value: impl Into<Vec<u8>>) -> Result<Self, SecretError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretError::Empty);
        }
        if value.len() > MAX_SECRET_BYTES {
            return Err(SecretError::TooLarge);
        }
        Ok(Self(value))
    }

    pub fn from_text(value: impl AsRef<str>) -> Result<Self, SecretError> {
        Self::from_bytes(value.as_ref().as_bytes().to_vec())
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretMaterial(<redacted>)")
    }
}

impl Drop for SecretMaterial {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
    }
}

/// Host implementation point for resolving a reference without putting the
/// resulting secret in a plugin message or adapter configuration.
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, SecretError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretError {
    Empty,
    TooLarge,
    Unavailable,
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("secret resolver returned an empty value"),
            Self::TooLarge => formatter.write_str("secret resolver value exceeds its bound"),
            Self::Unavailable => formatter.write_str("secret reference is unavailable"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Small resolver intended for tests and local loopback only.
pub struct StaticSecretResolver {
    reference: SecretRef,
    value: Vec<u8>,
}

impl fmt::Debug for StaticSecretResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticSecretResolver")
            .field("reference", &self.reference)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl StaticSecretResolver {
    pub fn new(reference: SecretRef, value: impl Into<Vec<u8>>) -> Result<Self, SecretError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretError::Empty);
        }
        if value.len() > MAX_SECRET_BYTES {
            return Err(SecretError::TooLarge);
        }
        Ok(Self { reference, value })
    }
}

impl Drop for StaticSecretResolver {
    fn drop(&mut self) {
        for byte in &mut self.value {
            *byte = 0;
        }
    }
}

impl SecretResolver for StaticSecretResolver {
    fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, SecretError> {
        if reference != &self.reference {
            return Err(SecretError::Unavailable);
        }
        SecretMaterial::from_bytes(self.value.clone())
    }
}

/// Signature input passed to a replaceable verifier.
#[derive(Clone, Copy)]
pub struct SignatureInput<'a> {
    pub timestamp_secs: u64,
    pub nonce: &'a str,
    pub body: &'a [u8],
    pub signature: &'a str,
}

/// Pluggable webhook signature verifier.  Implementations must not include
/// secret material in errors or diagnostics.
pub trait SignatureVerifier: Send + Sync {
    fn verify(
        &self,
        secret: &SecretMaterial,
        input: SignatureInput<'_>,
    ) -> Result<(), SignatureError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    WeixinSha1,
}

/// Standard token/timestamp/nonce SHA-1 verifier used by the classic Weixin
/// webhook handshake.  It is a protocol compatibility primitive, not a claim
/// that the adapter implements encrypted Weixin message login.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeixinSha1Verifier;

impl WeixinSha1Verifier {
    pub fn signature_for_bytes(secret: &[u8], timestamp_secs: u64, nonce: &str) -> String {
        let mut parts = [
            secret.to_vec(),
            timestamp_secs.to_string().into_bytes(),
            nonce.as_bytes().to_vec(),
        ];
        parts.sort();
        let mut canonical = Vec::new();
        for part in parts {
            canonical.extend_from_slice(&part);
        }
        hex_lower(&sha1(&canonical))
    }

    pub fn signature_for(secret: &SecretMaterial, timestamp_secs: u64, nonce: &str) -> String {
        Self::signature_for_bytes(secret.as_bytes(), timestamp_secs, nonce)
    }
}

impl SignatureVerifier for WeixinSha1Verifier {
    fn verify(
        &self,
        secret: &SecretMaterial,
        input: SignatureInput<'_>,
    ) -> Result<(), SignatureError> {
        if input.nonce.is_empty() || input.nonce.len() > 256 {
            return Err(SignatureError::InvalidInput);
        }
        if input.signature.len() != 40 {
            return Err(SignatureError::InvalidSignature);
        }
        let expected = Self::signature_for(secret, input.timestamp_secs, input.nonce);
        if constant_time_equal(expected.as_bytes(), input.signature.as_bytes()) {
            Ok(())
        } else {
            Err(SignatureError::InvalidSignature)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureError {
    InvalidInput,
    InvalidSignature,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("webhook signature input is invalid"),
            Self::InvalidSignature => formatter.write_str("webhook signature is invalid"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// Minimal adapter configuration.  `secret_ref` is persisted; secret bytes
/// are resolved only for inbound verification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    pub endpoint: String,
    pub secret_ref: SecretRef,
    pub signature_algorithm: SignatureAlgorithm,
    pub max_body_bytes: usize,
    pub clock_skew_secs: u64,
    pub max_replay_entries: usize,
    pub enabled: bool,
}

impl WebhookConfig {
    pub fn new(
        endpoint: impl Into<String>,
        secret_ref: SecretRef,
    ) -> Result<Self, WeixinAdapterError> {
        let config = Self {
            endpoint: endpoint.into(),
            secret_ref,
            signature_algorithm: SignatureAlgorithm::WeixinSha1,
            max_body_bytes: MAX_WEBHOOK_BODY_BYTES,
            clock_skew_secs: 300,
            max_replay_entries: MAX_REPLAY_ENTRIES,
            enabled: true,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), WeixinAdapterError> {
        if self.endpoint.is_empty()
            || self.endpoint.len() > MAX_ENDPOINT_BYTES
            || self.endpoint.chars().any(char::is_whitespace)
            || !(self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://"))
            || self.endpoint.contains('@')
        {
            return Err(WeixinAdapterError::InvalidConfig("endpoint"));
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > MAX_WEBHOOK_BODY_BYTES {
            return Err(WeixinAdapterError::InvalidConfig("max_body_bytes"));
        }
        if self.clock_skew_secs == 0 || self.clock_skew_secs > 86_400 {
            return Err(WeixinAdapterError::InvalidConfig("clock_skew_secs"));
        }
        if self.max_replay_entries == 0 || self.max_replay_entries > MAX_REPLAY_ENTRIES {
            return Err(WeixinAdapterError::InvalidConfig("max_replay_entries"));
        }
        Ok(())
    }
}

/// A bounded, body-owning webhook request.  Debug output omits body and
/// signature contents.
pub struct WebhookRequest {
    timestamp_secs: u64,
    nonce: String,
    signature: String,
    body: Vec<u8>,
}

impl fmt::Debug for WebhookRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookRequest")
            .field("timestamp_secs", &self.timestamp_secs)
            .field("nonce_bytes", &self.nonce.len())
            .field("signature", &"<redacted>")
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl WebhookRequest {
    pub fn new(
        timestamp_secs: u64,
        nonce: impl Into<String>,
        signature: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, WeixinAdapterError> {
        let request = Self {
            timestamp_secs,
            nonce: nonce.into(),
            signature: signature.into(),
            body: body.into(),
        };
        if request.nonce.is_empty()
            || request.nonce.len() > 256
            || request.signature.is_empty()
            || request.signature.len() > 128
            || request.body.len() > MAX_WEBHOOK_BODY_BYTES
        {
            return Err(WeixinAdapterError::InvalidRequest("webhook request"));
        }
        Ok(request)
    }

    pub fn timestamp_secs(&self) -> u64 {
        self.timestamp_secs
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookAck {
    pub accepted: bool,
    pub duplicate: bool,
    pub idempotency_key: Option<IdempotencyKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundResult {
    pub status: u16,
    pub duplicate: bool,
}

/// A deliberately small HTTP request representation.  A real transport can
/// map it to reqwest, hyper, or a Host-owned network worker.
#[derive(Clone)]
pub struct HttpRequest {
    endpoint: String,
    body: Vec<u8>,
    idempotency_key: String,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("endpoint", &"<redacted>")
            .field("body_bytes", &self.body.len())
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

impl HttpRequest {
    fn new(endpoint: String, body: Vec<u8>, idempotency_key: String) -> Self {
        Self {
            endpoint,
            body,
            idempotency_key,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Result<Self, WeixinAdapterError> {
        let body = body.into();
        if body.len() > MAX_WEBHOOK_BODY_BYTES {
            return Err(WeixinAdapterError::BodyTooLarge {
                size: body.len(),
                maximum: MAX_WEBHOOK_BODY_BYTES,
            });
        }
        Ok(Self { status, body })
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    pub code: String,
    pub retryable: bool,
}

impl TransportError {
    pub fn new(code: impl Into<String>, retryable: bool) -> Self {
        let code = code.into();
        let code = if code.is_empty()
            || !code.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            }) {
            "transport_error".to_owned()
        } else {
            code.chars().take(64).collect()
        };
        Self { code, retryable }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HTTP transport failed ({})", self.code)
    }
}

impl std::error::Error for TransportError {}

pub trait HttpTransport: Send {
    fn send(
        &mut self,
        request: &HttpRequest,
        context: &RequestContext,
    ) -> Result<HttpResponse, TransportError>;
}

/// Deterministic transport double.  It records only request metadata and
/// body length, never body bytes or secrets.
pub struct MockTransport {
    responses: VecDeque<Result<HttpResponse, TransportError>>,
    calls: usize,
    last_body_bytes: usize,
    last_idempotency_key: Option<String>,
}

impl fmt::Debug for MockTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockTransport")
            .field("queued_responses", &self.responses.len())
            .field("calls", &self.calls)
            .field("last_body_bytes", &self.last_body_bytes)
            .field("last_idempotency_key", &self.last_idempotency_key)
            .finish()
    }
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            responses: VecDeque::new(),
            calls: 0,
            last_body_bytes: 0,
            last_idempotency_key: None,
        }
    }

    pub fn queue_response(&mut self, response: HttpResponse) {
        self.responses.push_back(Ok(response));
    }

    pub fn queue_error(&mut self, error: TransportError) {
        self.responses.push_back(Err(error));
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn last_body_bytes(&self) -> usize {
        self.last_body_bytes
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport for MockTransport {
    fn send(
        &mut self,
        request: &HttpRequest,
        context: &RequestContext,
    ) -> Result<HttpResponse, TransportError> {
        if let Err(error) = context.check() {
            return Err(TransportError::new(
                match error {
                    RequestControlError::Cancelled => "cancelled",
                    RequestControlError::TimedOut => "timed_out",
                },
                false,
            ));
        }
        self.calls += 1;
        self.last_body_bytes = request.body.len();
        self.last_idempotency_key = Some(request.idempotency_key.clone());
        self.responses.pop_front().unwrap_or_else(|| {
            HttpResponse::new(200, Vec::new()).map_err(|_| TransportError::new("mock_error", false))
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookAdapterState {
    pub enabled: bool,
    pub inbound_messages: usize,
    pub replay_entries: usize,
}

/// Minimal stateful adapter with replaceable verifier, resolver, and HTTP
/// transport.  It is deliberately generic so the kernel never depends on a
/// concrete HTTP client.
pub struct WebhookAdapter<T, R> {
    config: WebhookConfig,
    transport: T,
    resolver: R,
    verifier: Box<dyn SignatureVerifier>,
    replay: BTreeMap<(u64, String), ()>,
    inbound: BTreeMap<String, InboundMessage>,
    outbound: BTreeMap<String, (Vec<u8>, OutboundResult)>,
}

impl<T, R> fmt::Debug for WebhookAdapter<T, R>
where
    T: fmt::Debug,
    R: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookAdapter")
            .field("config", &self.config)
            .field("transport", &self.transport)
            .field("resolver", &self.resolver)
            .field("inbound_messages", &self.inbound.len())
            .field("outbound_messages", &self.outbound.len())
            .field("replay_entries", &self.replay.len())
            .finish()
    }
}

impl<T, R> WebhookAdapter<T, R>
where
    T: HttpTransport,
    R: SecretResolver,
{
    pub fn new(
        config: WebhookConfig,
        transport: T,
        resolver: R,
    ) -> Result<Self, WeixinAdapterError> {
        Self::with_verifier(config, transport, resolver, Box::new(WeixinSha1Verifier))
    }

    pub fn with_verifier(
        config: WebhookConfig,
        transport: T,
        resolver: R,
        verifier: Box<dyn SignatureVerifier>,
    ) -> Result<Self, WeixinAdapterError> {
        config.validate()?;
        Ok(Self {
            config,
            transport,
            resolver,
            verifier,
            replay: BTreeMap::new(),
            inbound: BTreeMap::new(),
            outbound: BTreeMap::new(),
        })
    }

    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn enable(&mut self) {
        self.config.enabled = true;
    }

    pub fn disable(&mut self) {
        self.config.enabled = false;
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn state(&self) -> WebhookAdapterState {
        WebhookAdapterState {
            enabled: self.config.enabled,
            inbound_messages: self.inbound.len(),
            replay_entries: self.replay.len(),
        }
    }

    pub fn handle_inbound_at(
        &mut self,
        request: WebhookRequest,
        now_secs: u64,
        context: &RequestContext,
    ) -> Result<WebhookAck, WeixinAdapterError> {
        self.ensure_enabled()?;
        context.check()?;
        if request.body.len() > self.config.max_body_bytes {
            return Err(WeixinAdapterError::BodyTooLarge {
                size: request.body.len(),
                maximum: self.config.max_body_bytes,
            });
        }
        self.validate_timestamp(request.timestamp_secs, now_secs)?;
        let secret = self
            .resolver
            .resolve(&self.config.secret_ref)
            .map_err(WeixinAdapterError::Secret)?;
        self.verifier
            .verify(
                &secret,
                SignatureInput {
                    timestamp_secs: request.timestamp_secs,
                    nonce: &request.nonce,
                    body: &request.body,
                    signature: &request.signature,
                },
            )
            .map_err(WeixinAdapterError::Signature)?;
        context.check()?;
        self.prune_replay(now_secs);
        let replay_key = (request.timestamp_secs, request.nonce.clone());
        if self.replay.contains_key(&replay_key) {
            return Err(WeixinAdapterError::ReplayDetected);
        }
        if self.replay.len() >= self.config.max_replay_entries {
            return Err(WeixinAdapterError::ReplayCacheFull);
        }
        let message: InboundMessage = serde_json::from_slice(&request.body)
            .map_err(|_| WeixinAdapterError::InvalidPayload)?;
        let key = message.envelope().idempotency_key.as_str().to_owned();
        if let Some(existing) = self.inbound.get(&key) {
            if existing == &message {
                self.replay.insert(replay_key, ());
                return Ok(WebhookAck {
                    accepted: true,
                    duplicate: true,
                    idempotency_key: Some(message.envelope().idempotency_key.clone()),
                });
            }
            return Err(WeixinAdapterError::IdempotencyConflict);
        }
        self.replay.insert(replay_key, ());
        if self.inbound.len() >= self.config.max_replay_entries {
            return Err(WeixinAdapterError::CapacityExceeded);
        }
        self.inbound.insert(key, message.clone());
        Ok(WebhookAck {
            accepted: true,
            duplicate: false,
            idempotency_key: Some(message.envelope().idempotency_key.clone()),
        })
    }

    pub fn send_outbound(
        &mut self,
        message: OutboundMessage,
        context: &RequestContext,
    ) -> Result<OutboundResult, WeixinAdapterError> {
        self.ensure_enabled()?;
        context.check()?;
        let key = message.envelope().idempotency_key.as_str().to_owned();
        let body = serde_json::to_vec(&message).map_err(|_| WeixinAdapterError::Serialization)?;
        if body.len() > self.config.max_body_bytes {
            return Err(WeixinAdapterError::BodyTooLarge {
                size: body.len(),
                maximum: self.config.max_body_bytes,
            });
        }
        if let Some((existing_body, result)) = self.outbound.get(&key) {
            if existing_body == &body {
                return Ok(OutboundResult {
                    status: result.status,
                    duplicate: true,
                });
            }
            return Err(WeixinAdapterError::IdempotencyConflict);
        }
        let request = HttpRequest::new(self.config.endpoint.clone(), body.clone(), key.clone());
        let response = self
            .transport
            .send(&request, context)
            .map_err(WeixinAdapterError::Transport)?;
        context.check()?;
        if !(200..=299).contains(&response.status) {
            return Err(WeixinAdapterError::RemoteRejected(response.status));
        }
        let result = OutboundResult {
            status: response.status,
            duplicate: false,
        };
        self.outbound.insert(key, (body, result.clone()));
        Ok(result)
    }

    fn ensure_enabled(&self) -> Result<(), WeixinAdapterError> {
        if self.config.enabled {
            Ok(())
        } else {
            Err(WeixinAdapterError::Disabled)
        }
    }

    fn validate_timestamp(&self, timestamp: u64, now: u64) -> Result<(), WeixinAdapterError> {
        let lower = now.saturating_sub(self.config.clock_skew_secs);
        let upper = now.saturating_add(self.config.clock_skew_secs);
        if timestamp < lower {
            return Err(WeixinAdapterError::StaleRequest);
        }
        if timestamp > upper {
            return Err(WeixinAdapterError::FutureRequest);
        }
        Ok(())
    }

    fn prune_replay(&mut self, now: u64) {
        let lower = now.saturating_sub(self.config.clock_skew_secs);
        self.replay.retain(|(timestamp, _), _| *timestamp >= lower);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WeixinAdapterError {
    Disabled,
    InvalidConfig(&'static str),
    InvalidRequest(&'static str),
    InvalidPayload,
    BodyTooLarge { size: usize, maximum: usize },
    StaleRequest,
    FutureRequest,
    ReplayDetected,
    ReplayCacheFull,
    CapacityExceeded,
    IdempotencyConflict,
    Signature(SignatureError),
    Secret(SecretError),
    Transport(TransportError),
    RemoteRejected(u16),
    Serialization,
    Cancelled,
    TimedOut,
    Contract(WeixinContractError),
}

impl fmt::Display for WeixinAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("weixin adapter is disabled"),
            Self::InvalidConfig(field) => {
                write!(formatter, "invalid adapter configuration: {field}")
            }
            Self::InvalidRequest(field) => write!(formatter, "invalid adapter request: {field}"),
            Self::InvalidPayload => formatter.write_str("webhook payload is invalid"),
            Self::BodyTooLarge { size, maximum } => {
                write!(
                    formatter,
                    "webhook body is {size} bytes, maximum is {maximum}"
                )
            }
            Self::StaleRequest => formatter.write_str("webhook timestamp is stale"),
            Self::FutureRequest => formatter.write_str("webhook timestamp is in the future"),
            Self::ReplayDetected => formatter.write_str("webhook request is a replay"),
            Self::ReplayCacheFull => formatter.write_str("webhook replay cache is full"),
            Self::CapacityExceeded => formatter.write_str("webhook message capacity is full"),
            Self::IdempotencyConflict => {
                formatter.write_str("idempotency key is bound to another message")
            }
            Self::Signature(error) => error.fmt(formatter),
            Self::Secret(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
            Self::RemoteRejected(status) => {
                write!(formatter, "remote endpoint rejected with HTTP {status}")
            }
            Self::Serialization => formatter.write_str("weixin message serialization failed"),
            Self::Cancelled => formatter.write_str("weixin operation was cancelled"),
            Self::TimedOut => formatter.write_str("weixin operation timed out"),
            Self::Contract(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WeixinAdapterError {}

impl From<RequestControlError> for WeixinAdapterError {
    fn from(error: RequestControlError) -> Self {
        match error {
            RequestControlError::Cancelled => Self::Cancelled,
            RequestControlError::TimedOut => Self::TimedOut,
        }
    }
}

impl From<WeixinContractError> for WeixinAdapterError {
    fn from(error: WeixinContractError) -> Self {
        Self::Contract(error)
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut message = Vec::with_capacity(input.len() + 72);
    message.extend_from_slice(input);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = [
        0x6745_2301_u32,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    for block in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) =
            (state[0], state[1], state[2], state[3], state[4]);
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temporary = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temporary;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut output = [0_u8; 20];
    for (index, word) in state.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}
