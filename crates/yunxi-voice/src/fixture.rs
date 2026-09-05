use crate::audio::{AudioChunk, AudioCodec, AudioFormat, SynthesizedAudioChunk};
use crate::error::VoiceContractError;
use crate::identifiers::{RequestId, StreamId};
use crate::message::{SynthesisEvent, SynthesisRequest, TranscribeEvent, TranscribeRequest};
use crate::state::StreamStatus;
use crate::transcript::TranscriptEvent;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionFixture {
    pub request: TranscribeRequest,
    pub events: Vec<TranscribeEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SynthesisFixture {
    pub request: SynthesisRequest,
    pub events: Vec<SynthesisEvent>,
}

pub fn transcribe_fixture() -> Result<TranscriptionFixture, VoiceContractError> {
    let stream_id = StreamId::new("fixture-transcribe")?;
    let request_id = RequestId::new("fixture-transcribe-request")?;
    let format = AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1)?;
    let chunks = vec![
        AudioChunk::new(stream_id.clone(), 0, format, vec![0; 32], false)?,
        AudioChunk::new(stream_id.clone(), 1, format, vec![1; 32], true)?,
    ];
    let request = TranscribeRequest::new(
        request_id,
        stream_id.clone(),
        format,
        chunks,
        true,
        StreamStatus::new(),
    )?;
    let events = vec![
        TranscribeEvent::Transcript(TranscriptEvent::partial(stream_id.clone(), 0, "你好")?),
        TranscribeEvent::Transcript(TranscriptEvent::final_text(stream_id, 1, "你好，云熙")?),
    ];
    Ok(TranscriptionFixture { request, events })
}

pub fn synthesize_fixture() -> Result<SynthesisFixture, VoiceContractError> {
    let stream_id = StreamId::new("fixture-synthesize")?;
    let request_id = RequestId::new("fixture-synthesize-request")?;
    let format = AudioFormat::new(AudioCodec::PcmS16Le, 24_000, 1)?;
    let request = SynthesisRequest::new(
        request_id.clone(),
        stream_id.clone(),
        "你好，云熙",
        format,
        StreamStatus::new(),
    )?;
    let events = vec![
        SynthesisEvent::Audio(SynthesizedAudioChunk::new(
            request_id.clone(),
            stream_id.clone(),
            0,
            format,
            vec![2; 32],
            false,
        )?),
        SynthesisEvent::Audio(SynthesizedAudioChunk::new(
            request_id,
            stream_id,
            1,
            format,
            vec![3; 16],
            true,
        )?),
    ];
    Ok(SynthesisFixture { request, events })
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn transcription_fixture_round_trips() {
        let fixture = transcribe_fixture().expect("valid transcription fixture");
        assert_eq!(fixture.request, round_trip(&fixture.request));
        assert_eq!(fixture.events, round_trip(&fixture.events));
        assert!(matches!(
            fixture.events[0],
            TranscribeEvent::Transcript(TranscriptEvent {
                kind: crate::TranscriptKind::Partial,
                ..
            })
        ));
        assert!(matches!(
            fixture.events[1],
            TranscribeEvent::Transcript(TranscriptEvent {
                kind: crate::TranscriptKind::Final,
                ..
            })
        ));
    }

    #[test]
    fn synthesis_fixture_round_trips() {
        let fixture = synthesize_fixture().expect("valid synthesis fixture");
        assert_eq!(fixture.request, round_trip(&fixture.request));
        assert_eq!(fixture.events, round_trip(&fixture.events));
    }

    #[test]
    fn serde_rejects_oversized_audio_before_constructing_a_chunk() {
        let fixture = transcribe_fixture().expect("valid transcription fixture");
        let mut value = serde_json::to_value(&fixture.request.chunks[0]).expect("serialize chunk");
        value["data"] = serde_json::to_value(vec![0_u8; crate::audio::MAX_AUDIO_CHUNK_BYTES + 1])
            .expect("serialize oversized bytes");

        let error = serde_json::from_value::<AudioChunk>(value).expect_err("oversized data fails");
        assert!(error.to_string().contains("maximum is 65536"));
    }

    #[test]
    fn serde_rejects_invalid_audio_format_and_unknown_fields() {
        let invalid_rate = serde_json::json!({
            "codec": "pcm_s16_le",
            "sample_rate_hz": 1000,
            "channels": 1
        });
        assert!(serde_json::from_value::<AudioFormat>(invalid_rate).is_err());

        let unknown = serde_json::json!({
            "codec": "pcm_s16_le",
            "sample_rate_hz": 16000,
            "channels": 1,
            "vendor_option": true
        });
        assert!(serde_json::from_value::<AudioFormat>(unknown).is_err());
    }

    #[test]
    fn request_validation_rejects_sequence_gaps() {
        let mut fixture = transcribe_fixture().expect("valid transcription fixture");
        fixture.request.chunks[1].sequence = 7;
        assert_eq!(
            fixture.request.validate(),
            Err(VoiceContractError::InvalidSequence {
                expected: 1,
                actual: 7,
            })
        );
    }

    #[test]
    fn serde_rejects_oversized_transcript() {
        let invalid = serde_json::json!({
            "stream_id": "voice-stream",
            "sequence": 0,
            "kind": "final",
            "text": "x".repeat(crate::transcript::MAX_TRANSCRIPT_TEXT_BYTES + 1)
        });
        assert!(serde_json::from_value::<TranscriptEvent>(invalid).is_err());
    }

    #[test]
    fn cancellation_and_backpressure_transitions_are_validated() {
        let mut status = StreamStatus::with_capacity(64).expect("bounded status");
        assert!(status.mark_cancelled().is_err());
        status
            .request_cancel("user stopped capture")
            .expect("request cancellation");
        status.mark_cancelled().expect("finish cancellation");
        assert_eq!(round_trip(&status), status);

        let mut flow = StreamStatus::with_capacity(64).expect("bounded status");
        flow.set_buffered_bytes(64).expect("at capacity");
        flow.pause().expect("pause producer");
        flow.start_draining().expect("drain buffer");
        flow.set_buffered_bytes(0).expect("drained");
        assert_eq!(flow.backpressure, crate::BackpressureState::Ready);
        assert!(flow.set_buffered_bytes(65).is_err());
    }

    #[test]
    fn serde_rejects_invalid_status_and_capability_version() {
        let invalid_status = serde_json::json!({
            "cancellation": "active",
            "cancellation_reason": "not allowed while active",
            "backpressure": "ready",
            "buffered_bytes": 0,
            "capacity_bytes": 64
        });
        assert!(serde_json::from_value::<StreamStatus>(invalid_status).is_err());

        let invalid_version = serde_json::json!({
            "capability": "voice.transcribe",
            "version": 2
        });
        assert!(serde_json::from_value::<crate::CapabilityDescriptor>(invalid_version).is_err());

        let descriptor = crate::CapabilityDescriptor::new(crate::VoiceCapability::Transcribe);
        let encoded = serde_json::to_value(descriptor).expect("descriptor serializes");
        assert_eq!(encoded["capability"], "voice.transcribe");
        assert_eq!(encoded["version"], 1);
    }
}
