//! Bounded, replaceable iLink transport.
//!
//! `IlinkHttpTransport` is the production network boundary.  It is deliberately
//! small and blocking so a host can run it in its own worker.  `LoopbackIlinkTransport`
//! implements the same contract without credentials or external side effects.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::{RequestContext, RequestControlError};
use crate::secret_store::{SecretStore, SecretStoreError};
use crate::{SecretMaterial, SecretRef};

pub const PRODUCTION_ILINK_ENDPOINT: &str = "https://ilinkai.weixin.qq.com/";
pub const MAX_ILINK_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_ILINK_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_ILINK_MESSAGES: usize = 128;
pub const MAX_ILINK_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_ILINK_ID_BYTES: usize = 512;
pub const MAX_QR_CONTENT_BYTES: usize = 64 * 1024;
pub const MAX_LONG_POLL_TIMEOUT_MS: u64 = 120_000;
/// The default budget is long enough for the maximum server-side long poll.
/// Callers can lower it with `with_request_timeout` or `RequestContext`.
pub const DEFAULT_ILINK_REQUEST_TIMEOUT: Duration = Duration::from_secs(125);

const QR_PATH: &str = "ilink/bot/get_bot_qrcode";
const QR_STATUS_PATH: &str = "ilink/bot/get_qrcode_status";
const UPDATES_PATH: &str = "ilink/bot/getupdates";
const SEND_MESSAGE_PATH: &str = "ilink/bot/sendmessage";
const APP_ID: &str = "bot";
const APP_CLIENT_VERSION: &str = "131330";

/// Transport-level operations shared by HTTP and deterministic loopback.
pub trait IlinkTransport: Send {
    fn fetch_qr(&mut self, context: &RequestContext) -> Result<QrChallenge, IlinkError>;

    fn poll_qr_status(
        &mut self,
        qrcode: &str,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<QrPoll, IlinkError>;

    fn get_updates(
        &mut self,
        cursor: &str,
        context: &RequestContext,
    ) -> Result<PollBatch, IlinkError>;

    fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<SendResult, IlinkError>;

    fn set_auth_token(&mut self, token: SecretMaterial) -> Result<(), IlinkError>;

    fn clear_auth_token(&mut self);

    fn is_authenticated(&self) -> bool;

    fn is_loopback(&self) -> bool {
        false
    }

    /// Creates an independent transport for a background poll worker.
    ///
    /// The default is deliberately unsupported: custom transports must opt in
    /// rather than accidentally sharing mutable credentials or state across
    /// the Host request loop and a worker thread.
    fn fork_for_worker(&self) -> Result<Box<dyn IlinkTransport>, IlinkError> {
        Err(IlinkError::Unavailable("background transport"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QrChallenge {
    pub qrcode: String,
    pub image_content: String,
}

impl QrChallenge {
    pub fn new(
        qrcode: impl Into<String>,
        image_content: impl Into<String>,
    ) -> Result<Self, IlinkError> {
        let challenge = Self {
            qrcode: qrcode.into(),
            image_content: image_content.into(),
        };
        validate_id("qrcode", &challenge.qrcode)?;
        if challenge.image_content.is_empty()
            || challenge.image_content.len() > MAX_QR_CONTENT_BYTES
            || challenge.image_content.chars().any(char::is_control)
        {
            return Err(IlinkError::InvalidInput("qrcode image content"));
        }
        Ok(challenge)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QrStatus {
    Waiting,
    Scanned,
    Confirmed,
    Expired,
    ScannedButRedirected,
    NeedVerifyCode,
    VerifyCodeBlocked,
    BoundRedirect,
}

#[derive(Debug)]
pub struct QrPoll {
    pub status: QrStatus,
    pub auth_token: Option<SecretMaterial>,
}

impl QrPoll {
    pub fn new(status: QrStatus) -> Self {
        Self {
            status,
            auth_token: None,
        }
    }

    pub fn confirmed(token: SecretMaterial) -> Self {
        Self {
            status: QrStatus::Confirmed,
            auth_token: Some(token),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PollBatch {
    pub cursor: String,
    pub messages: Vec<IlinkMessage>,
    pub has_more: bool,
    pub long_poll_timeout: Option<Duration>,
}

impl PollBatch {
    pub fn new(cursor: impl Into<String>, messages: Vec<IlinkMessage>) -> Result<Self, IlinkError> {
        if messages.len() > MAX_ILINK_MESSAGES {
            return Err(IlinkError::TooManyMessages {
                count: messages.len(),
                maximum: MAX_ILINK_MESSAGES,
            });
        }
        let batch = Self {
            cursor: cursor.into(),
            messages,
            has_more: false,
            long_poll_timeout: None,
        };
        validate_cursor(&batch.cursor)?;
        for message in &batch.messages {
            message.validate()?;
        }
        Ok(batch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IlinkMessage {
    pub message_id: String,
    pub from_user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_state: Option<u32>,
    #[serde(default)]
    pub item_list: Vec<IlinkMessageItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IlinkMessageItem {
    #[serde(rename = "type")]
    pub item_type: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_item: Option<IlinkTextItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_item: Option<IlinkVoiceItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IlinkTextItem {
    pub text: String,
}

/// A bounded reference to audio hosted by the iLink CDN. It contains no
/// downloaded media bytes; the Voice plugin owns decoding and playback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IlinkCdnMedia {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypt_query_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aes_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypt_type: Option<u32>,
}

/// iLink's voice item metadata, including an optional server transcription.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IlinkVoiceItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<IlinkCdnMedia>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encode_type: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playtime: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl IlinkMessage {
    pub fn text(
        message_id: impl Into<String>,
        from_user_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, IlinkError> {
        let message = Self {
            message_id: message_id.into(),
            from_user_id: from_user_id.into(),
            to_user_id: None,
            client_id: None,
            create_time_ms: None,
            session_id: None,
            group_id: None,
            message_type: Some(1),
            message_state: None,
            item_list: vec![IlinkMessageItem {
                item_type: 1,
                text_item: Some(IlinkTextItem { text: text.into() }),
                is_completed: Some(true),
                msg_id: None,
                voice_item: None,
            }],
            context_token: None,
        };
        message.validate()?;
        Ok(message)
    }

    /// Constructs the stable iLink Bot text-reply shape. The context token is
    /// intentionally required because it is the routing capability for the
    /// originating Weixin conversation.
    pub fn reply_text(
        message_id: impl Into<String>,
        to_user_id: impl Into<String>,
        context_token: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, IlinkError> {
        let message_id = message_id.into();
        let message = Self {
            message_id: message_id.clone(),
            from_user_id: String::new(),
            to_user_id: Some(to_user_id.into()),
            client_id: Some(message_id),
            create_time_ms: None,
            session_id: None,
            group_id: None,
            message_type: Some(2),
            message_state: Some(2),
            item_list: vec![IlinkMessageItem {
                item_type: 1,
                text_item: Some(IlinkTextItem { text: text.into() }),
                is_completed: Some(true),
                msg_id: None,
                voice_item: None,
            }],
            context_token: Some(context_token.into()),
        };
        message.validate()?;
        Ok(message)
    }

    pub fn validate(&self) -> Result<(), IlinkError> {
        validate_id("message_id", &self.message_id)?;
        if self.from_user_id.is_empty() {
            if self.message_type != Some(2) {
                return Err(IlinkError::InvalidInput("from_user_id"));
            }
        } else {
            validate_id("from_user_id", &self.from_user_id)?;
        }
        for (field, value) in [
            ("to_user_id", self.to_user_id.as_deref()),
            ("client_id", self.client_id.as_deref()),
            ("session_id", self.session_id.as_deref()),
            ("group_id", self.group_id.as_deref()),
            ("context_token", self.context_token.as_deref()),
        ] {
            if let Some(value) = value {
                validate_id(field, value)?;
            }
        }
        if self.item_list.len() > MAX_ILINK_MESSAGES {
            return Err(IlinkError::TooManyMessages {
                count: self.item_list.len(),
                maximum: MAX_ILINK_MESSAGES,
            });
        }
        for item in &self.item_list {
            if let Some(text) = item.text_item.as_ref().map(|item| &item.text) {
                if text.len() > MAX_ILINK_TEXT_BYTES
                    || text.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                    })
                {
                    return Err(IlinkError::InvalidInput("message text"));
                }
            }
            if let Some(msg_id) = &item.msg_id {
                validate_id("item msg_id", msg_id)?;
            }
        }
        if self.item_list.is_empty() && self.context_token.is_none() {
            return Err(IlinkError::InvalidInput("message content"));
        }
        if self.message_type == Some(2) {
            if self.to_user_id.is_none() {
                return Err(IlinkError::InvalidInput("to_user_id"));
            }
            if self.context_token.is_none() {
                return Err(IlinkError::InvalidInput("context_token"));
            }
            if self.message_state != Some(2) {
                return Err(IlinkError::InvalidInput("message_state"));
            }
        }
        for item in &self.item_list {
            if item.voice_item.is_some() && item.item_type != 3 {
                return Err(IlinkError::InvalidInput("voice item type"));
            }
            if item.item_type == 3 && item.voice_item.is_none() {
                return Err(IlinkError::InvalidInput("voice item"));
            }
            if let Some(voice) = &item.voice_item {
                if voice
                    .encode_type
                    .is_some_and(|value| !(1..=8).contains(&value))
                    || voice
                        .bits_per_sample
                        .is_some_and(|value| value == 0 || value > 64)
                    || voice
                        .sample_rate
                        .is_some_and(|value| value == 0 || value > 384_000)
                    || voice
                        .playtime
                        .is_some_and(|value| value > 7 * 24 * 60 * 60 * 1000)
                {
                    return Err(IlinkError::InvalidInput("voice metadata"));
                }
                if let Some(text) = &voice.text {
                    if text.len() > MAX_ILINK_TEXT_BYTES
                        || text.chars().any(|character| {
                            character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                        })
                    {
                        return Err(IlinkError::InvalidInput("voice transcript"));
                    }
                }
                if let Some(media) = &voice.media {
                    for (field, value) in [
                        ("voice media query", media.encrypt_query_param.as_deref()),
                        ("voice media key", media.aes_key.as_deref()),
                    ] {
                        if let Some(value) = value {
                            validate_id(field, value)?;
                        }
                    }
                    if media.encrypt_type.is_some_and(|value| value > 1) {
                        return Err(IlinkError::InvalidInput("voice media encrypt_type"));
                    }
                }
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for IlinkMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireMessage {
            #[serde(default)]
            message_id: Option<WireIdentifier>,
            from_user_id: String,
            #[serde(default)]
            to_user_id: Option<String>,
            #[serde(default)]
            client_id: Option<String>,
            #[serde(default)]
            seq: Option<u64>,
            #[serde(default)]
            create_time_ms: Option<u64>,
            #[serde(default)]
            session_id: Option<String>,
            #[serde(default)]
            group_id: Option<String>,
            #[serde(default)]
            message_type: Option<u32>,
            #[serde(default)]
            message_state: Option<u32>,
            #[serde(default)]
            item_list: Vec<IlinkMessageItem>,
            #[serde(default)]
            context_token: Option<String>,
        }

        let wire = WireMessage::deserialize(deserializer)?;
        let client_id = wire.client_id;
        let message_id = wire
            .message_id
            .map(WireIdentifier::into_string)
            .or_else(|| client_id.clone())
            .or_else(|| wire.seq.map(|value| format!("seq-{value}")))
            .ok_or_else(|| D::Error::custom("missing iLink message id"))?;
        let message = Self {
            message_id,
            from_user_id: wire.from_user_id,
            to_user_id: wire.to_user_id,
            client_id,
            create_time_ms: wire.create_time_ms,
            session_id: wire.session_id,
            group_id: wire.group_id,
            message_type: wire.message_type,
            message_state: wire.message_state,
            item_list: wire.item_list,
            context_token: wire.context_token,
        };
        message.validate().map_err(serde::de::Error::custom)?;
        Ok(message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendResult {
    pub status_code: u16,
}

#[derive(Clone, Debug)]
pub struct IlinkHttpConfig {
    endpoint: String,
    account_alias: String,
    token_ref: Option<SecretRef>,
    request_timeout: Duration,
    max_request_bytes: usize,
    max_response_bytes: usize,
    allow_loopback: bool,
}

impl IlinkHttpConfig {
    pub fn production(account_alias: impl Into<String>) -> Result<Self, IlinkError> {
        Self::build(
            PRODUCTION_ILINK_ENDPOINT.to_owned(),
            account_alias.into(),
            false,
        )
    }

    pub fn for_loopback(
        endpoint: impl Into<String>,
        account_alias: impl Into<String>,
    ) -> Result<Self, IlinkError> {
        Self::build(endpoint.into(), account_alias.into(), true)
    }

    pub fn with_token_ref(mut self, token_ref: SecretRef) -> Self {
        self.token_ref = Some(token_ref);
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self, IlinkError> {
        if timeout.is_zero() {
            return Err(IlinkError::InvalidConfig("request_timeout"));
        }
        self.request_timeout = timeout;
        Ok(self)
    }

    pub fn with_response_limit(mut self, bytes: usize) -> Result<Self, IlinkError> {
        if bytes == 0 || bytes > MAX_ILINK_RESPONSE_BYTES {
            return Err(IlinkError::InvalidConfig("max_response_bytes"));
        }
        self.max_response_bytes = bytes;
        Ok(self)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn account_alias(&self) -> &str {
        &self.account_alias
    }

    fn build(
        endpoint: String,
        account_alias: String,
        allow_loopback: bool,
    ) -> Result<Self, IlinkError> {
        if endpoint.is_empty() || endpoint.len() > 2048 || endpoint.chars().any(char::is_whitespace)
        {
            return Err(IlinkError::InvalidConfig("endpoint"));
        }
        let parsed =
            reqwest::Url::parse(&endpoint).map_err(|_| IlinkError::InvalidConfig("endpoint"))?;
        if parsed.path() == "/" && !endpoint.ends_with('/') {
            return Err(IlinkError::InvalidConfig("endpoint"));
        }
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(IlinkError::InvalidConfig("endpoint"));
        }
        if allow_loopback
            && !parsed.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
            })
        {
            return Err(IlinkError::InvalidConfig("loopback endpoint"));
        }
        if !allow_loopback && endpoint != PRODUCTION_ILINK_ENDPOINT {
            return Err(IlinkError::InvalidConfig("production endpoint"));
        }
        if account_alias.is_empty() || account_alias.len() > MAX_ILINK_ID_BYTES {
            return Err(IlinkError::InvalidConfig("account_alias"));
        }
        Ok(Self {
            endpoint,
            account_alias,
            token_ref: None,
            request_timeout: DEFAULT_ILINK_REQUEST_TIMEOUT,
            max_request_bytes: MAX_ILINK_REQUEST_BYTES,
            max_response_bytes: MAX_ILINK_RESPONSE_BYTES,
            allow_loopback,
        })
    }

    fn apply_server_endpoint(&mut self, endpoint: &str) -> Result<(), IlinkError> {
        if endpoint.is_empty() || endpoint.len() > 2048 || endpoint.chars().any(char::is_whitespace)
        {
            return Err(IlinkError::InvalidResponse("invalid iLink base URL"));
        }
        let parsed = reqwest::Url::parse(endpoint)
            .map_err(|_| IlinkError::InvalidResponse("invalid iLink base URL"))?;
        let host = parsed
            .host_str()
            .ok_or(IlinkError::InvalidResponse("iLink base URL has no host"))?;
        let is_loopback =
            host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1";
        if (self.allow_loopback && !is_loopback)
            || (!self.allow_loopback && parsed.scheme() != "https")
        {
            return Err(IlinkError::InvalidResponse("invalid iLink base URL"));
        }
        if !self.allow_loopback && is_loopback {
            return Err(IlinkError::InvalidResponse("invalid iLink base URL"));
        }
        self.endpoint = endpoint.strip_suffix('/').unwrap_or(endpoint).to_owned() + "/";
        Ok(())
    }
}

pub struct IlinkHttpTransport {
    client: Client,
    config: IlinkHttpConfig,
    token: Option<SecretMaterial>,
}

impl fmt::Debug for IlinkHttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IlinkHttpTransport")
            .field("endpoint", &self.config.endpoint)
            .field("account_alias", &"<redacted>")
            .field("authenticated", &self.token.is_some())
            .field("request_timeout", &self.config.request_timeout)
            .field("max_response_bytes", &self.config.max_response_bytes)
            .finish()
    }
}

impl IlinkHttpTransport {
    pub fn new<S: SecretStore>(config: IlinkHttpConfig, store: &S) -> Result<Self, IlinkError> {
        let token = match config.token_ref.as_ref() {
            Some(reference) => store.get(reference)?,
            None => None,
        };
        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .map_err(|_| IlinkError::Transport("client initialization"))?;
        Ok(Self {
            client,
            config,
            token,
        })
    }

    pub fn production<S: SecretStore>(
        account_alias: impl Into<String>,
        token_ref: Option<SecretRef>,
        store: &S,
    ) -> Result<Self, IlinkError> {
        let config = IlinkHttpConfig::production(account_alias)?;
        let config = token_ref.map_or(config.clone(), |reference| config.with_token_ref(reference));
        Self::new(config, store)
    }

    fn request<TRequest, TResponse>(
        &self,
        method: Method,
        path: &str,
        body: Option<&TRequest>,
        query: &[(&str, &str)],
        authenticated: bool,
        context: &RequestContext,
    ) -> Result<(TResponse, u16), IlinkError>
    where
        TRequest: Serialize,
        TResponse: DeserializeOwned,
    {
        context.check()?;
        let mut endpoint = reqwest::Url::parse(&self.config.endpoint)
            .map_err(|_| IlinkError::InvalidConfig("endpoint"))?
            .join(path)
            .map_err(|_| IlinkError::InvalidConfig("endpoint path"))?;
        for (key, value) in query {
            endpoint.query_pairs_mut().append_pair(key, value);
        }
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| IlinkError::Serialization)?
            .unwrap_or_default();
        if body.len() > self.config.max_request_bytes {
            return Err(IlinkError::RequestTooLarge {
                size: body.len(),
                maximum: self.config.max_request_bytes,
            });
        }
        if authenticated && self.token.is_none() {
            return Err(IlinkError::NotAuthenticated);
        }
        let mut request = self
            .client
            .request(method.clone(), endpoint)
            .header("iLink-App-Id", APP_ID)
            .header("iLink-App-ClientVersion", APP_CLIENT_VERSION);
        if method == Method::POST {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .header("AuthorizationType", "ilink_bot_token")
                .header(
                    HeaderName::from_static("x-wechat-uin"),
                    random_wechat_uin()?,
                );
        }
        if let Some(token) = self.token.as_ref().filter(|_| authenticated) {
            request = request.header(AUTHORIZATION, authorization_header(token)?);
        }
        let request = request.body(body);
        let timeout = context
            .remaining()
            .map_or(self.config.request_timeout, |remaining| {
                remaining.min(self.config.request_timeout)
            });
        if timeout.is_zero() {
            return Err(IlinkError::TimedOut);
        }
        let mut response = request.timeout(timeout).send().map_err(|error| {
            if error.is_timeout() {
                IlinkError::TimedOut
            } else {
                IlinkError::Transport("request")
            }
        })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(&mut response, self.config.max_response_bytes)?;
        if !(200..=299).contains(&status) {
            return Err(IlinkError::HttpStatus { status });
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| IlinkError::InvalidJson)?;
        for field in ["errcode", "ret"] {
            if let Some(code) = value
                .get(field)
                .and_then(Value::as_i64)
                .filter(|code| *code != 0)
            {
                return Err(if code == -14 {
                    IlinkError::SessionExpired
                } else {
                    IlinkError::Api { code }
                });
            }
        }
        let decoded = serde_json::from_value(value).map_err(|_| IlinkError::InvalidJson)?;
        context.check()?;
        Ok((decoded, status))
    }

    fn qr_request(&self, context: &RequestContext) -> Result<QrWireResponse, IlinkError> {
        self.request::<(), _>(
            Method::GET,
            QR_PATH,
            None,
            &[("bot_type", "3")],
            false,
            context,
        )
        .map(|(value, _)| value)
    }
}

impl IlinkTransport for Box<dyn IlinkTransport> {
    fn fetch_qr(&mut self, context: &RequestContext) -> Result<QrChallenge, IlinkError> {
        (**self).fetch_qr(context)
    }

    fn poll_qr_status(
        &mut self,
        qrcode: &str,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<QrPoll, IlinkError> {
        (**self).poll_qr_status(qrcode, verify_code, context)
    }

    fn get_updates(
        &mut self,
        cursor: &str,
        context: &RequestContext,
    ) -> Result<PollBatch, IlinkError> {
        (**self).get_updates(cursor, context)
    }

    fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<SendResult, IlinkError> {
        (**self).send_message(message, context)
    }

    fn set_auth_token(&mut self, token: SecretMaterial) -> Result<(), IlinkError> {
        (**self).set_auth_token(token)
    }

    fn clear_auth_token(&mut self) {
        (**self).clear_auth_token();
    }

    fn is_authenticated(&self) -> bool {
        (**self).is_authenticated()
    }

    fn is_loopback(&self) -> bool {
        (**self).is_loopback()
    }
}

impl IlinkTransport for IlinkHttpTransport {
    fn fetch_qr(&mut self, context: &RequestContext) -> Result<QrChallenge, IlinkError> {
        let response = self.qr_request(context)?;
        QrChallenge::new(response.qrcode, response.qrcode_img_content)
    }

    fn poll_qr_status(
        &mut self,
        qrcode: &str,
        verify_code: Option<&str>,
        context: &RequestContext,
    ) -> Result<QrPoll, IlinkError> {
        validate_id("qrcode", qrcode)?;
        if let Some(verify_code) = verify_code {
            validate_id("verify_code", verify_code)?;
        }
        let mut query = vec![("qrcode", qrcode)];
        if let Some(verify_code) = verify_code {
            query.push(("verify_code", verify_code));
        }
        let response: QrStatusWireResponse = self
            .request::<(), _>(Method::GET, QR_STATUS_PATH, None, &query, false, context)
            .map(|(value, _)| value)?;
        let status = response.status.parse()?;
        if let Some(base_url) = response.baseurl.as_deref() {
            self.config.apply_server_endpoint(base_url)?;
        } else if status == QrStatus::ScannedButRedirected {
            if let Some(host) = response.redirect_host.as_deref() {
                self.config
                    .apply_server_endpoint(&format!("https://{host}"))?;
            }
        }
        let token = response
            .ilink_bot_token
            .or(response.bot_token)
            .map(SecretMaterial::from_text)
            .transpose()
            .map_err(|error| IlinkError::Secret(SecretStoreError::Secret(error)))?;
        Ok(QrPoll {
            status,
            auth_token: token,
        })
    }

    fn get_updates(
        &mut self,
        cursor: &str,
        context: &RequestContext,
    ) -> Result<PollBatch, IlinkError> {
        validate_cursor(cursor)?;
        let request = UpdatesWireRequest {
            get_updates_buf: cursor.to_owned(),
            base_info: BaseInfo::default(),
        };
        let (response, _) = self.request::<_, UpdatesWireResponse>(
            Method::POST,
            UPDATES_PATH,
            Some(&request),
            &[],
            true,
            context,
        )?;
        response.into_batch(cursor)
    }

    fn send_message(
        &mut self,
        message: &IlinkMessage,
        context: &RequestContext,
    ) -> Result<SendResult, IlinkError> {
        message.validate()?;
        let request = SendWireRequest {
            msg: message.clone(),
            base_info: BaseInfo::default(),
        };
        let (_, status) = self.request::<_, SendWireResponse>(
            Method::POST,
            SEND_MESSAGE_PATH,
            Some(&request),
            &[],
            true,
            context,
        )?;
        Ok(SendResult {
            status_code: status,
        })
    }

    fn set_auth_token(&mut self, token: SecretMaterial) -> Result<(), IlinkError> {
        authorization_header(&token)?;
        self.token = Some(token);
        Ok(())
    }

    fn clear_auth_token(&mut self) {
        self.token = None;
    }

    fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    fn is_loopback(&self) -> bool {
        self.config.allow_loopback
    }

    fn fork_for_worker(&self) -> Result<Box<dyn IlinkTransport>, IlinkError> {
        let token = self
            .token
            .as_ref()
            .map(|token| SecretMaterial::from_bytes(token.as_bytes().to_vec()))
            .transpose()
            .map_err(|error| IlinkError::Secret(SecretStoreError::Secret(error)))?;
        Ok(Box::new(Self {
            client: self.client.clone(),
            config: self.config.clone(),
            token,
        }))
    }
}

/// A deterministic transport for tests and local demonstrations. It never
/// claims to have a real Weixin login.
#[derive(Debug, Default)]
pub struct LoopbackIlinkTransport {
    qr: Option<QrChallenge>,
    qr_statuses: std::collections::VecDeque<Result<QrPoll, IlinkError>>,
    polls: std::collections::VecDeque<Result<PollBatch, IlinkError>>,
    sent: Vec<IlinkMessage>,
    authenticated: bool,
}

impl Clone for LoopbackIlinkTransport {
    fn clone(&self) -> Self {
        Self {
            qr: self.qr.clone(),
            // QR state is only consumed by the foreground login flow. A
            // worker clone starts after authentication and must not duplicate
            // pending login transitions.
            qr_statuses: std::collections::VecDeque::new(),
            polls: self.polls.clone(),
            sent: Vec::new(),
            authenticated: self.authenticated,
        }
    }
}

impl LoopbackIlinkTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn queue_qr(&mut self, challenge: QrChallenge) {
        self.qr = Some(challenge);
    }

    pub fn queue_qr_status(&mut self, status: Result<QrStatus, IlinkError>) {
        self.qr_statuses.push_back(status.map(QrPoll::new));
    }

    pub fn queue_login_poll(&mut self, poll: Result<QrPoll, IlinkError>) {
        self.qr_statuses.push_back(poll);
    }

    pub fn queue_poll(&mut self, batch: Result<PollBatch, IlinkError>) {
        self.polls.push_back(batch);
    }

    pub fn sent_messages(&self) -> &[IlinkMessage] {
        &self.sent
    }
}

impl IlinkTransport for LoopbackIlinkTransport {
    fn fetch_qr(&mut self, _context: &RequestContext) -> Result<QrChallenge, IlinkError> {
        _context.check()?;
        self.qr
            .clone()
            .ok_or(IlinkError::Unavailable("loopback QR"))
    }

    fn poll_qr_status(
        &mut self,
        _qrcode: &str,
        _verify_code: Option<&str>,
        _context: &RequestContext,
    ) -> Result<QrPoll, IlinkError> {
        _context.check()?;
        self.qr_statuses
            .pop_front()
            .unwrap_or_else(|| Ok(QrPoll::new(QrStatus::Waiting)))
    }

    fn get_updates(
        &mut self,
        _cursor: &str,
        _context: &RequestContext,
    ) -> Result<PollBatch, IlinkError> {
        _context.check()?;
        self.polls
            .pop_front()
            .unwrap_or_else(|| PollBatch::new("", Vec::new()))
    }

    fn send_message(
        &mut self,
        message: &IlinkMessage,
        _context: &RequestContext,
    ) -> Result<SendResult, IlinkError> {
        _context.check()?;
        if !self.authenticated {
            return Err(IlinkError::NotAuthenticated);
        }
        message.validate()?;
        self.sent.push(message.clone());
        Ok(SendResult { status_code: 200 })
    }

    fn set_auth_token(&mut self, _token: SecretMaterial) -> Result<(), IlinkError> {
        self.authenticated = true;
        Ok(())
    }

    fn clear_auth_token(&mut self) {
        self.authenticated = false;
    }

    fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    fn is_loopback(&self) -> bool {
        true
    }

    fn fork_for_worker(&self) -> Result<Box<dyn IlinkTransport>, IlinkError> {
        Ok(Box::new(self.clone()))
    }
}

#[derive(Clone, Debug, Serialize)]
struct BaseInfo {
    channel_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    bot_agent: String,
}

impl Default for BaseInfo {
    fn default() -> Self {
        Self {
            channel_version: env!("CARGO_PKG_VERSION").to_owned(),
            bot_agent: String::new(),
        }
    }
}

#[derive(Deserialize)]
struct QrWireResponse {
    qrcode: String,
    qrcode_img_content: String,
}

#[derive(Deserialize)]
struct QrStatusWireResponse {
    #[serde(default)]
    status: String,
    #[serde(default)]
    bot_token: Option<String>,
    #[serde(default)]
    ilink_bot_token: Option<String>,
    #[serde(default)]
    baseurl: Option<String>,
    #[serde(default)]
    redirect_host: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireIdentifier {
    Text(String),
    Number(serde_json::Number),
}

impl WireIdentifier {
    fn into_string(self) -> String {
        match self {
            Self::Text(value) => value,
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Serialize)]
struct UpdatesWireRequest {
    get_updates_buf: String,
    base_info: BaseInfo,
}

#[derive(Deserialize)]
struct UpdatesWireResponse {
    #[serde(default)]
    #[serde(alias = "next_key")]
    get_updates_buf: Option<String>,
    #[serde(default)]
    #[serde(alias = "messages", alias = "Msgs")]
    msgs: Vec<IlinkMessage>,
    #[serde(default)]
    updates: Vec<IlinkUpdate>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    longpolling_timeout_ms: Option<u64>,
}

impl UpdatesWireResponse {
    fn into_batch(mut self, fallback_cursor: &str) -> Result<PollBatch, IlinkError> {
        if self.msgs.len().saturating_add(self.updates.len()) > MAX_ILINK_MESSAGES {
            return Err(IlinkError::TooManyMessages {
                count: self.msgs.len().saturating_add(self.updates.len()),
                maximum: MAX_ILINK_MESSAGES,
            });
        }
        for update in self.updates {
            if let Some(message) = update.into_message()? {
                self.msgs.push(message);
            }
        }
        if self
            .longpolling_timeout_ms
            .is_some_and(|timeout| timeout > MAX_LONG_POLL_TIMEOUT_MS)
        {
            return Err(IlinkError::InvalidResponse(
                "long-poll timeout exceeds bound",
            ));
        }
        let mut batch = PollBatch::new(
            self.get_updates_buf
                .unwrap_or_else(|| fallback_cursor.to_owned()),
            self.msgs,
        )?;
        batch.has_more = self.has_more;
        batch.long_poll_timeout = self.longpolling_timeout_ms.map(Duration::from_millis);
        Ok(batch)
    }
}

#[derive(Deserialize)]
struct IlinkUpdate {
    #[serde(default)]
    update_id: Option<i64>,
    #[serde(default)]
    update_type: Option<String>,
    #[serde(default)]
    message: Option<IlinkUpdateMessage>,
}

impl IlinkUpdate {
    fn into_message(self) -> Result<Option<IlinkMessage>, IlinkError> {
        if self
            .update_type
            .as_deref()
            .is_some_and(|kind| kind != "message")
        {
            return Ok(None);
        }
        let Some(message) = self.message else {
            return Ok(None);
        };
        let Some(from) = message.from else {
            return Ok(None);
        };
        let Some(text) = message.text.filter(|text| !text.is_empty()) else {
            return Ok(None);
        };
        let message_id = message
            .message_id
            .map(WireIdentifier::into_string)
            .unwrap_or_else(|| {
                format!(
                    "update-{}",
                    self.update_id
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_owned())
                )
            });
        let mut converted = IlinkMessage::text(message_id, from.user_id, text)?;
        converted.to_user_id = message.chat_id.clone();
        converted.group_id = matches!(message.chat_type.as_deref(), Some("group" | "chatroom"))
            .then_some(message.chat_id)
            .flatten();
        converted.create_time_ms = message.timestamp.map(|timestamp| {
            if timestamp < 10_000_000_000 {
                timestamp.saturating_mul(1000)
            } else {
                timestamp
            }
        });
        converted.validate()?;
        Ok(Some(converted))
    }
}

#[derive(Deserialize)]
struct IlinkUpdateMessage {
    #[serde(default)]
    message_id: Option<WireIdentifier>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    chat_type: Option<String>,
    #[serde(default)]
    from: Option<IlinkUpdateSender>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Deserialize)]
struct IlinkUpdateSender {
    user_id: String,
}

#[derive(Serialize)]
struct SendWireRequest {
    msg: IlinkMessage,
    base_info: BaseInfo,
}

#[derive(Deserialize)]
struct SendWireResponse {}

fn read_bounded(response: &mut Response, maximum: usize) -> Result<Vec<u8>, IlinkError> {
    let mut bytes = Vec::new();
    response
        .take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| IlinkError::Transport("response body"))?;
    if bytes.len() > maximum {
        return Err(IlinkError::ResponseTooLarge { maximum });
    }
    Ok(bytes)
}

fn random_wechat_uin() -> Result<HeaderValue, IlinkError> {
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes).map_err(|_| IlinkError::Transport("random Weixin UIN"))?;
    let number = u32::from_be_bytes(bytes).to_string();
    let encoded = base64::engine::general_purpose::STANDARD.encode(number);
    HeaderValue::from_bytes(encoded.as_bytes())
        .map_err(|_| IlinkError::InvalidInput("X-WECHAT-UIN"))
}

fn authorization_header(token: &SecretMaterial) -> Result<HeaderValue, IlinkError> {
    let mut value = b"Bearer ".to_vec();
    value.extend_from_slice(token.as_bytes());
    HeaderValue::from_bytes(&value).map_err(|_| IlinkError::InvalidInput("auth token"))
}

fn validate_id(field: &'static str, value: &str) -> Result<(), IlinkError> {
    if value.is_empty() || value.len() > MAX_ILINK_ID_BYTES || value.chars().any(char::is_control) {
        return Err(IlinkError::InvalidInput(field));
    }
    Ok(())
}

fn validate_cursor(value: &str) -> Result<(), IlinkError> {
    if value.len() > MAX_ILINK_ID_BYTES || value.chars().any(char::is_control) {
        return Err(IlinkError::InvalidInput("cursor"));
    }
    Ok(())
}

impl std::str::FromStr for QrStatus {
    type Err = IlinkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "wait" | "waiting" => Ok(Self::Waiting),
            "scaned" | "scanned" => Ok(Self::Scanned),
            "confirmed" => Ok(Self::Confirmed),
            "expired" => Ok(Self::Expired),
            "scaned_but_redirect" | "scanned_but_redirect" => Ok(Self::ScannedButRedirected),
            "need_verifycode" | "need_verify_code" => Ok(Self::NeedVerifyCode),
            "verify_code_blocked" => Ok(Self::VerifyCodeBlocked),
            "binded_redirect" | "bound_redirect" => Ok(Self::BoundRedirect),
            _ => Err(IlinkError::InvalidResponse("unknown QR status")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IlinkError {
    InvalidConfig(&'static str),
    InvalidInput(&'static str),
    InvalidResponse(&'static str),
    Serialization,
    InvalidJson,
    RequestTooLarge { size: usize, maximum: usize },
    ResponseTooLarge { maximum: usize },
    TooManyMessages { count: usize, maximum: usize },
    HttpStatus { status: u16 },
    Api { code: i64 },
    SessionExpired,
    Transport(&'static str),
    Secret(SecretStoreError),
    NotAuthenticated,
    Unavailable(&'static str),
    Cancelled,
    TimedOut,
}

impl fmt::Display for IlinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(field) => write!(formatter, "invalid iLink configuration: {field}"),
            Self::InvalidInput(field) => write!(formatter, "invalid iLink input: {field}"),
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid iLink response: {message}")
            }
            Self::Serialization => formatter.write_str("iLink request serialization failed"),
            Self::InvalidJson => formatter.write_str("iLink response JSON is invalid"),
            Self::RequestTooLarge { size, maximum } => {
                write!(
                    formatter,
                    "iLink request is {size} bytes, maximum is {maximum}"
                )
            }
            Self::ResponseTooLarge { maximum } => {
                write!(formatter, "iLink response exceeds {maximum} bytes")
            }
            Self::TooManyMessages { count, maximum } => {
                write!(
                    formatter,
                    "iLink returned {count} messages, maximum is {maximum}"
                )
            }
            Self::HttpStatus { status } => write!(formatter, "iLink HTTP status {status}"),
            Self::Api { code } => write!(formatter, "iLink API returned error code {code}"),
            Self::SessionExpired => formatter.write_str("iLink session expired; login is required"),
            Self::Transport(operation) => {
                write!(formatter, "iLink transport failed during {operation}")
            }
            Self::Secret(error) => error.fmt(formatter),
            Self::NotAuthenticated => formatter.write_str("iLink transport is not authenticated"),
            Self::Unavailable(operation) => {
                write!(formatter, "iLink operation unavailable: {operation}")
            }
            Self::Cancelled => formatter.write_str("iLink operation was cancelled"),
            Self::TimedOut => formatter.write_str("iLink operation timed out"),
        }
    }
}

impl std::error::Error for IlinkError {}

impl From<SecretStoreError> for IlinkError {
    fn from(error: SecretStoreError) -> Self {
        Self::Secret(error)
    }
}

impl From<RequestControlError> for IlinkError {
    fn from(error: RequestControlError) -> Self {
        match error {
            RequestControlError::Cancelled => Self::Cancelled,
            RequestControlError::TimedOut => Self::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_and_poll_bounds_are_enforced() {
        let message = IlinkMessage::text("message", "peer", "hello").expect("message");
        assert!(PollBatch::new("cursor", vec![message]).is_ok());
        let oversized = "x".repeat(MAX_ILINK_TEXT_BYTES + 1);
        assert!(IlinkMessage::text("message", "peer", oversized).is_err());
    }

    #[test]
    fn production_endpoint_cannot_be_replaced_accidentally() {
        assert!(IlinkHttpConfig::production("account").is_ok());
        assert!(IlinkHttpConfig::for_loopback("https://example.com/", "account").is_err());
        assert!(IlinkHttpConfig::for_loopback("http://127.0.0.1:8787/", "account").is_ok());
        assert!(
            IlinkHttpConfig::production("account")
                .expect("config")
                .with_token_ref(SecretRef::new("host:weixin/token").expect("reference"))
                .endpoint()
                .starts_with("https://ilinkai.weixin.qq.com")
        );
    }

    #[test]
    fn loopback_requires_explicit_authentication_for_send() {
        let mut transport = LoopbackIlinkTransport::new();
        let message = IlinkMessage::text("message", "peer", "hello").expect("message");
        let context = RequestContext::new();
        assert_eq!(
            transport.send_message(&message, &context),
            Err(IlinkError::NotAuthenticated)
        );
    }

    #[test]
    fn reply_shape_requires_context_and_supports_voice_metadata() {
        let reply =
            IlinkMessage::reply_text("client-1", "peer-1", "context-1", "hello").expect("reply");
        assert_eq!(reply.message_type, Some(2));
        assert_eq!(reply.message_state, Some(2));
        assert!(reply.from_user_id.is_empty());
        assert!(
            IlinkMessage {
                item_list: vec![IlinkMessageItem {
                    item_type: 3,
                    text_item: None,
                    is_completed: Some(true),
                    msg_id: None,
                    voice_item: Some(IlinkVoiceItem {
                        media: Some(IlinkCdnMedia {
                            encrypt_query_param: Some("query".to_owned()),
                            aes_key: Some("key".to_owned()),
                            encrypt_type: Some(1),
                        }),
                        encode_type: Some(6),
                        bits_per_sample: Some(16),
                        sample_rate: Some(24_000),
                        playtime: Some(1_000),
                        text: Some("transcript".to_owned()),
                    }),
                }],
                ..reply
            }
            .validate()
            .is_ok()
        );
        let invalid = IlinkMessage::reply_text("client-2", "peer-1", "", "hello")
            .expect_err("empty context must be rejected");
        assert_eq!(invalid, IlinkError::InvalidInput("context_token"));
    }

    #[test]
    fn update_shape_is_normalized_and_long_poll_timeout_is_bounded() {
        let response: UpdatesWireResponse = serde_json::from_str(
            r#"{
                "get_updates_buf":"next",
                "longpolling_timeout_ms":1000,
                "updates":[{
                    "update_id":7,
                    "update_type":"message",
                    "message":{
                        "chat_id":"room",
                        "chat_type":"group",
                        "from":{"user_id":"peer"},
                        "text":"hello",
                        "timestamp":10
                    }
                }]
            }"#,
        )
        .expect("updates JSON");
        let batch = response.into_batch("").expect("batch");
        assert_eq!(batch.messages[0].message_id, "update-7");
        assert_eq!(batch.messages[0].group_id.as_deref(), Some("room"));
        assert_eq!(batch.messages[0].create_time_ms, Some(10_000));
        assert_eq!(batch.long_poll_timeout, Some(Duration::from_millis(1000)));

        let response: UpdatesWireResponse =
            serde_json::from_str(r#"{"longpolling_timeout_ms":120001}"#).expect("timeout JSON");
        assert!(matches!(
            response.into_batch(""),
            Err(IlinkError::InvalidResponse(_))
        ));
    }
}
