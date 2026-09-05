use crate::error::WeixinContractError;
use crate::identifiers::{IdempotencyKey, MediaId, MessageId, ParticipantId, RequestId, SessionId};
use crate::media::{MediaKind, MediaMetadata};
use crate::message::{Direction, InboundMessage, MessageContent, MessageEnvelope, OutboundMessage};
use crate::state::DeliveryStatus;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundFixture {
    pub message: InboundMessage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundFixture {
    pub message: OutboundMessage,
}

pub fn inbound_fixture() -> Result<InboundFixture, WeixinContractError> {
    let media = MediaMetadata::new(
        MediaId::new("fixture-image")?,
        MediaKind::Image,
        "image/png",
        Some("greeting.png".to_owned()),
        2048,
        None,
        Some(640),
        Some(480),
    )?;
    let content = MessageContent::text_with_media("你好，云熙", vec![media])?;
    let envelope = MessageEnvelope::new(
        MessageId::new("fixture-inbound-message")?,
        SessionId::new("fixture-session")?,
        RequestId::new("fixture-inbound-request")?,
        IdempotencyKey::new("fixture-inbound-key")?,
        Direction::Inbound,
        ParticipantId::new("user-001")?,
        ParticipantId::new("bot-001")?,
        1_757_000_000_000,
        content,
        DeliveryStatus::new_inbound(),
    )?;
    Ok(InboundFixture {
        message: InboundMessage::new(envelope)?,
    })
}

pub fn outbound_fixture() -> Result<OutboundFixture, WeixinContractError> {
    let content = MessageContent::text("收到，我会处理这条消息")?;
    let envelope = MessageEnvelope::new(
        MessageId::new("fixture-outbound-message")?,
        SessionId::new("fixture-session")?,
        RequestId::new("fixture-outbound-request")?,
        IdempotencyKey::new("fixture-outbound-key")?,
        Direction::Outbound,
        ParticipantId::new("bot-001")?,
        ParticipantId::new("user-001")?,
        1_757_000_000_100,
        content,
        DeliveryStatus::new_outbound(),
    )?;
    Ok(OutboundFixture {
        message: OutboundMessage::new(envelope)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AckState, CapabilityDescriptor, Direction, INBOUND_CONTRACT, MAX_MESSAGE_TEXT_BYTES,
        MessageEnvelope, OutboundMessage, WeixinCapability,
    };
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_vec(value).expect("fixture serializes");
        serde_json::from_slice(&encoded).expect("fixture validates after decoding")
    }

    #[test]
    fn fixtures_round_trip_and_keep_typed_directions() {
        let inbound = inbound_fixture().expect("valid inbound fixture");
        let outbound = outbound_fixture().expect("valid outbound fixture");
        assert_eq!(inbound.message, round_trip(&inbound.message));
        assert_eq!(outbound.message, round_trip(&outbound.message));
        assert_eq!(inbound.message.envelope().direction, Direction::Inbound);
        assert_eq!(outbound.message.envelope().direction, Direction::Outbound);
    }

    #[test]
    fn descriptor_serializes_the_versioned_contract() {
        let descriptor = CapabilityDescriptor::new(WeixinCapability::Inbound);
        let encoded = serde_json::to_value(descriptor).expect("descriptor serializes");
        assert_eq!(encoded["capability"], "weixin.inbound");
        assert_eq!(encoded["version"], 1);
        assert_eq!(WeixinCapability::Inbound.contract(), INBOUND_CONTRACT);
    }

    #[test]
    fn serde_rejects_unknown_fields_and_oversized_text() {
        let fixture = inbound_fixture().expect("valid inbound fixture");
        let mut value = serde_json::to_value(fixture.message).expect("message serializes");
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<MessageEnvelope>(value).is_err());

        let invalid = serde_json::json!({
            "text": "x".repeat(MAX_MESSAGE_TEXT_BYTES + 1),
            "media": []
        });
        assert!(serde_json::from_value::<MessageContent>(invalid).is_err());
    }

    #[test]
    fn serde_rejects_invalid_media_metadata() {
        let invalid = serde_json::json!({
            "media_id": "media-1",
            "kind": "image",
            "mime_type": "image/png",
            "file_name": "..\\secret.png",
            "byte_length": 4,
            "duration_ms": null,
            "width": 100,
            "height": null
        });
        assert!(serde_json::from_value::<MediaMetadata>(invalid).is_err());
    }

    #[test]
    fn typed_messages_reject_a_mismatched_direction() {
        let fixture = outbound_fixture().expect("valid outbound fixture");
        let error = InboundMessage::new(fixture.message.into_envelope())
            .expect_err("outbound cannot enter inbound contract");
        assert!(error.to_string().contains("expected inbound"));
        let _ = OutboundMessage::new;
    }

    #[test]
    fn delivery_status_models_ack_retry_cancel_and_backpressure() {
        let mut status = DeliveryStatus::new_outbound();
        status.mark_in_flight().expect("first attempt starts");
        status
            .mark_failed("temporary provider failure")
            .expect("failure is recorded");
        status.schedule_retry().expect("retry remains in budget");
        status.mark_in_flight().expect("retry starts");
        status.mark_accepted().expect("provider accepts message");
        status.mark_delivered().expect("delivery completes");
        assert!(status.acknowledge().is_ok());
        assert_eq!(status.acknowledgement, AckState::Acknowledged);

        let mut flow =
            DeliveryStatus::new_inbound_with_capacity(64).expect("small bounded capacity");
        flow.set_buffered_bytes(64)
            .expect("buffer stays within capacity");
        flow.pause().expect("pause producer");
        flow.start_draining().expect("drain buffer");
        flow.set_buffered_bytes(0).expect("buffer drains");
        flow.request_cancel("host shutdown")
            .expect("cancel requested");
        flow.mark_cancelled().expect("cancellation completes");
        assert!(flow.set_buffered_bytes(65).is_err());
        assert!(flow.validate().is_ok());
    }

    #[test]
    fn serde_rejects_invalid_delivery_status_and_capability_version() {
        let invalid_status = serde_json::json!({
            "state": "queued",
            "acknowledgement": "rejected",
            "acknowledgement_reason": null,
            "retry": {"attempt": 0, "max_attempts": 8, "last_error": null},
            "cancellation": "active",
            "cancellation_reason": null,
            "backpressure": "ready",
            "buffered_bytes": 0,
            "capacity_bytes": 64
        });
        assert!(serde_json::from_value::<DeliveryStatus>(invalid_status).is_err());

        let invalid_version = serde_json::json!({
            "capability": "weixin.inbound",
            "version": 2
        });
        assert!(serde_json::from_value::<CapabilityDescriptor>(invalid_version).is_err());
    }
}
