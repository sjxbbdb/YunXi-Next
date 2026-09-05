use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::WeixinContractError;
use crate::identifiers::{IdempotencyKey, MessageId, ParticipantId, RequestId, SessionId};
use crate::media::{MAX_MEDIA_ITEMS, MAX_MEDIA_METADATA_BYTES, MediaMetadata};
use crate::state::DeliveryStatus;

pub const WEIXIN_CAPABILITY_VERSION: u16 = 1;
pub const INBOUND_CAPABILITY: &str = "weixin.inbound";
pub const OUTBOUND_CAPABILITY: &str = "weixin.outbound";
pub const INBOUND_CONTRACT: &str = "weixin.inbound@1";
pub const OUTBOUND_CONTRACT: &str = "weixin.outbound@1";
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeixinCapability {
    #[serde(rename = "weixin.inbound")]
    Inbound,
    #[serde(rename = "weixin.outbound")]
    Outbound,
}

impl WeixinCapability {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inbound => INBOUND_CAPABILITY,
            Self::Outbound => OUTBOUND_CAPABILITY,
        }
    }

    pub const fn contract(self) -> &'static str {
        match self {
            Self::Inbound => INBOUND_CONTRACT,
            Self::Outbound => OUTBOUND_CONTRACT,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    pub capability: WeixinCapability,
    pub version: u16,
}

impl CapabilityDescriptor {
    pub fn new(capability: WeixinCapability) -> Self {
        Self {
            capability,
            version: WEIXIN_CAPABILITY_VERSION,
        }
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        if self.version != WEIXIN_CAPABILITY_VERSION {
            return Err(WeixinContractError::UnsupportedCapabilityVersion {
                version: self.version,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CapabilityDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            capability: WeixinCapability,
            version: u16,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        let descriptor = Self {
            capability: wire.capability,
            version: wire.version,
        };
        descriptor.validate().map_err(D::Error::custom)?;
        Ok(descriptor)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MessageContent {
    pub text: Option<String>,
    pub media: Vec<MediaMetadata>,
}

impl MessageContent {
    pub fn new(
        text: Option<String>,
        media: Vec<MediaMetadata>,
    ) -> Result<Self, WeixinContractError> {
        let content = Self { text, media };
        content.validate()?;
        Ok(content)
    }

    pub fn text(text: impl Into<String>) -> Result<Self, WeixinContractError> {
        Self::new(Some(text.into()), Vec::new())
    }

    pub fn media(media: Vec<MediaMetadata>) -> Result<Self, WeixinContractError> {
        Self::new(None, media)
    }

    pub fn text_with_media(
        text: impl Into<String>,
        media: Vec<MediaMetadata>,
    ) -> Result<Self, WeixinContractError> {
        Self::new(Some(text.into()), media)
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        if self.text.is_none() && self.media.is_empty() {
            return Err(WeixinContractError::InvalidValue {
                field: "content",
                message: "must contain text or media",
            });
        }
        if let Some(text) = &self.text {
            crate::error::validate_text("message text", text, MAX_MESSAGE_TEXT_BYTES, false)?;
        }
        if self.media.len() > MAX_MEDIA_ITEMS {
            return Err(WeixinContractError::TooManyMedia {
                count: self.media.len(),
                maximum: MAX_MEDIA_ITEMS,
            });
        }
        let metadata_size = self
            .media
            .iter()
            .map(MediaMetadata::bounded_size_bytes)
            .sum::<usize>();
        if metadata_size > MAX_MEDIA_METADATA_BYTES {
            return Err(WeixinContractError::MediaMetadataTooLarge {
                size: metadata_size,
                maximum: MAX_MEDIA_METADATA_BYTES,
            });
        }
        for media in &self.media {
            media.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMessageContent {
            text: Option<String>,
            media: Vec<MediaMetadata>,
        }

        let wire = WireMessageContent::deserialize(deserializer)?;
        Self::new(wire.text, wire.media).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MessageEnvelope {
    pub message_id: MessageId,
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub idempotency_key: IdempotencyKey,
    pub direction: Direction,
    pub sender_id: ParticipantId,
    pub recipient_id: ParticipantId,
    pub sent_at_ms: u64,
    pub content: MessageContent,
    pub delivery: DeliveryStatus,
}

impl MessageEnvelope {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        message_id: MessageId,
        session_id: SessionId,
        request_id: RequestId,
        idempotency_key: IdempotencyKey,
        direction: Direction,
        sender_id: ParticipantId,
        recipient_id: ParticipantId,
        sent_at_ms: u64,
        content: MessageContent,
        delivery: DeliveryStatus,
    ) -> Result<Self, WeixinContractError> {
        let envelope = Self {
            message_id,
            session_id,
            request_id,
            idempotency_key,
            direction,
            sender_id,
            recipient_id,
            sent_at_ms,
            content,
            delivery,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        self.content.validate()?;
        self.delivery.validate()
    }
}

impl<'de> Deserialize<'de> for MessageEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMessageEnvelope {
            message_id: MessageId,
            session_id: SessionId,
            request_id: RequestId,
            idempotency_key: IdempotencyKey,
            direction: Direction,
            sender_id: ParticipantId,
            recipient_id: ParticipantId,
            sent_at_ms: u64,
            content: MessageContent,
            delivery: DeliveryStatus,
        }

        let wire = WireMessageEnvelope::deserialize(deserializer)?;
        Self::new(
            wire.message_id,
            wire.session_id,
            wire.request_id,
            wire.idempotency_key,
            wire.direction,
            wire.sender_id,
            wire.recipient_id,
            wire.sent_at_ms,
            wire.content,
            wire.delivery,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct InboundMessage(MessageEnvelope);

impl InboundMessage {
    pub fn new(envelope: MessageEnvelope) -> Result<Self, WeixinContractError> {
        envelope.validate()?;
        if envelope.direction != Direction::Inbound {
            return Err(WeixinContractError::InvalidDirection {
                expected: Direction::Inbound.as_str(),
                actual: envelope.direction.as_str(),
            });
        }
        Ok(Self(envelope))
    }

    pub fn envelope(&self) -> &MessageEnvelope {
        &self.0
    }

    pub fn into_envelope(self) -> MessageEnvelope {
        self.0
    }
}

impl<'de> Deserialize<'de> for InboundMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(MessageEnvelope::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OutboundMessage(MessageEnvelope);

impl OutboundMessage {
    pub fn new(envelope: MessageEnvelope) -> Result<Self, WeixinContractError> {
        envelope.validate()?;
        if envelope.direction != Direction::Outbound {
            return Err(WeixinContractError::InvalidDirection {
                expected: Direction::Outbound.as_str(),
                actual: envelope.direction.as_str(),
            });
        }
        Ok(Self(envelope))
    }

    pub fn envelope(&self) -> &MessageEnvelope {
        &self.0
    }

    pub fn into_envelope(self) -> MessageEnvelope {
        self.0
    }
}

impl<'de> Deserialize<'de> for OutboundMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(MessageEnvelope::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
