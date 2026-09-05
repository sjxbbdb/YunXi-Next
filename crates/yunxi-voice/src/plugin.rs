//! Isolated JSONL loopback fixture for the voice capability contracts.
//!
//! This module is deliberately a test double. It has no device, codec, SDK,
//! network, or async runtime integration. The only network operation is the
//! existing local loopback protocol connection used by plugin processes.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantRequirement, HostMessage, InvocationCodecError,
    InvocationRequest, InvocationResponse, PluginManifest, PluginMessage, PluginRiskLevel,
    PluginRuntimeMetadata, ProtocolError, capabilities, connect_plugin_with_manifest,
};

use crate::{
    CancellationState, SynthesisEvent, SynthesisRequest, TranscribeEvent, TranscribeRequest,
    TranscriptEvent, VoiceContractError,
};

pub const VOICE_FIXTURE_PLUGIN_ID: &str = "yunxi.voice.fixture";
pub const TRANSCRIBE_OPERATION: &str = "transcribe";
pub const SYNTHESIZE_OPERATION: &str = "synthesize";
pub const CANCEL_OPERATION: &str = "cancel";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_PARTIAL_TEXT: &str = "fixture partial";
const FIXTURE_FINAL_TEXT: &str = "fixture final";

/// A cancellation request accepted by either voice capability.
///
/// The status must be `requested` or `cancelled`; an active status is not a
/// valid cancellation command. Reusing [`StreamStatus`](crate::StreamStatus)
/// keeps cancellation state transitions identical to the voice contracts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CancelRequest {
    pub stream_id: crate::StreamId,
    pub status: crate::StreamStatus,
}

impl CancelRequest {
    pub fn new(
        stream_id: crate::StreamId,
        reason: impl Into<String>,
    ) -> Result<Self, VoiceContractError> {
        let mut status = crate::StreamStatus::new();
        status.request_cancel(reason)?;
        let request = Self { stream_id, status };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        self.status.validate()?;
        if self.status.cancellation == CancellationState::Active {
            return Err(VoiceContractError::InvalidValue {
                field: "cancellation",
                message: "cancel request must be requested or cancelled",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CancelRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCancelRequest {
            stream_id: crate::StreamId,
            status: crate::StreamStatus,
        }

        let wire = WireCancelRequest::deserialize(deserializer)?;
        let request = Self {
            stream_id: wire.stream_id,
            status: wire.status,
        };
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

/// Deterministic cancellation response for fixture assertions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelResult {
    pub stream_id: crate::StreamId,
    pub status: crate::StreamStatus,
}

pub fn run_voice_fixture() -> Result<(), VoiceFixtureError> {
    let transcribe = CapabilityDescriptor::new(
        capabilities::VOICE_TRANSCRIBE,
        crate::VOICE_CAPABILITY_VERSION as u32,
    )?;
    let synthesize = CapabilityDescriptor::new(
        capabilities::VOICE_SYNTHESIZE,
        crate::VOICE_CAPABILITY_VERSION as u32,
    )?;
    let manifest = PluginManifest::new(
        VOICE_FIXTURE_PLUGIN_ID,
        "YunXi voice contract fixture",
        env!("CARGO_PKG_VERSION"),
        vec![transcribe, synthesize],
    )
    .with_grants(Vec::<GrantRequirement>::new())
    .with_runtime_metadata(PluginRuntimeMetadata::new(
        "voice-fixture",
        PluginRiskLevel::Safe,
    ));
    let mut session = connect_plugin_with_manifest(manifest, CONNECT_TIMEOUT)?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => handle_request(&mut session, &request)?,
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(VoiceFixtureError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn handle_request(
    session: &mut yunxi_protocol::PluginSession,
    request: &InvocationRequest,
) -> Result<(), VoiceFixtureError> {
    let request_id = request.request_id();
    if !supports(request) {
        send_failure(
            session,
            request_id,
            "unsupported_operation",
            "voice fixture does not support the requested capability or operation".to_string(),
        )?;
        return Ok(());
    }

    match request.operation() {
        TRANSCRIBE_OPERATION => {
            let payload = match request.decode_payload::<TranscribeRequest>() {
                Ok(payload) => payload,
                Err(error) => {
                    send_failure(session, request_id, "invalid_request", error.to_string())?;
                    return Ok(());
                }
            };
            let events = transcribe(&payload).map_err(VoiceFixtureError::Contract);
            send_result(session, request_id, events?)?;
        }
        SYNTHESIZE_OPERATION => {
            let payload = match request.decode_payload::<SynthesisRequest>() {
                Ok(payload) => payload,
                Err(error) => {
                    send_failure(session, request_id, "invalid_request", error.to_string())?;
                    return Ok(());
                }
            };
            let events = synthesize(&payload).map_err(VoiceFixtureError::Contract);
            send_result(session, request_id, events?)?;
        }
        CANCEL_OPERATION => {
            let payload = match request.decode_payload::<CancelRequest>() {
                Ok(payload) => payload,
                Err(error) => {
                    send_failure(session, request_id, "invalid_request", error.to_string())?;
                    return Ok(());
                }
            };
            send_result(
                session,
                request_id,
                cancel(&payload).map_err(VoiceFixtureError::Contract)?,
            )?;
        }
        _ => unreachable!("operation was checked by supports"),
    }
    Ok(())
}

fn supports(request: &InvocationRequest) -> bool {
    let capability = request.capability();
    let id = capability.id().as_str();
    let version = capability.version();
    let version_ok = version == crate::VOICE_CAPABILITY_VERSION as u32;
    version_ok
        && match request.operation() {
            TRANSCRIBE_OPERATION => id == capabilities::VOICE_TRANSCRIBE,
            SYNTHESIZE_OPERATION => id == capabilities::VOICE_SYNTHESIZE,
            CANCEL_OPERATION => {
                id == capabilities::VOICE_TRANSCRIBE || id == capabilities::VOICE_SYNTHESIZE
            }
            _ => false,
        }
}

fn transcribe(request: &TranscribeRequest) -> Result<Vec<TranscribeEvent>, VoiceContractError> {
    if request.status.cancellation != CancellationState::Active {
        let mut status = request.status.clone();
        if status.cancellation == CancellationState::Requested {
            status.mark_cancelled()?;
        }
        return Ok(vec![TranscribeEvent::Status(status)]);
    }

    let mut events = vec![TranscribeEvent::Status(request.status.clone())];
    events.push(TranscribeEvent::Transcript(TranscriptEvent::partial(
        request.stream_id.clone(),
        0,
        FIXTURE_PARTIAL_TEXT,
    )?));
    if request.input_complete {
        events.push(TranscribeEvent::Transcript(TranscriptEvent::final_text(
            request.stream_id.clone(),
            1,
            FIXTURE_FINAL_TEXT,
        )?));
    }
    Ok(events)
}

fn synthesize(request: &SynthesisRequest) -> Result<Vec<SynthesisEvent>, VoiceContractError> {
    if request.status.cancellation != CancellationState::Active {
        let mut status = request.status.clone();
        if status.cancellation == CancellationState::Requested {
            status.mark_cancelled()?;
        }
        return Ok(vec![SynthesisEvent::Status(status)]);
    }

    Ok(vec![
        SynthesisEvent::Status(request.status.clone()),
        SynthesisEvent::Audio(crate::SynthesizedAudioChunk::new(
            request.request_id.clone(),
            request.stream_id.clone(),
            0,
            request.format,
            vec![0; 8],
            false,
        )?),
        SynthesisEvent::Audio(crate::SynthesizedAudioChunk::new(
            request.request_id.clone(),
            request.stream_id.clone(),
            1,
            request.format,
            vec![1; 8],
            true,
        )?),
    ])
}

fn cancel(request: &CancelRequest) -> Result<CancelResult, VoiceContractError> {
    request.validate()?;
    let mut status = request.status.clone();
    if status.cancellation == CancellationState::Requested {
        status.mark_cancelled()?;
    }
    Ok(CancelResult {
        stream_id: request.stream_id.clone(),
        status,
    })
}

fn send_result<T: Serialize>(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    result: T,
) -> Result<(), VoiceFixtureError> {
    let response = InvocationResponse::encode(request_id, &result)?;
    session.send(&PluginMessage::InvocationCompleted { response })?;
    Ok(())
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
pub enum VoiceFixtureError {
    Capability(CapabilityError),
    Contract(VoiceContractError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for VoiceFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Contract(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for VoiceFixtureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Capability(error) => Some(error),
            Self::Contract(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<CapabilityError> for VoiceFixtureError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<VoiceContractError> for VoiceFixtureError {
    fn from(error: VoiceContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<InvocationCodecError> for VoiceFixtureError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for VoiceFixtureError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioChunk, AudioCodec, AudioFormat, RequestId, StreamId, StreamStatus};

    fn transcribe_request() -> TranscribeRequest {
        let stream_id = StreamId::new("fixture-stream").expect("stream id");
        let format = AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format");
        TranscribeRequest::new(
            RequestId::new("fixture-request").expect("request id"),
            stream_id.clone(),
            format,
            vec![AudioChunk::new(stream_id, 0, format, vec![7; 4], true).expect("chunk")],
            true,
            StreamStatus::new(),
        )
        .expect("request")
    }

    fn invocation(
        capability: &str,
        operation: &str,
        payload: &impl Serialize,
    ) -> InvocationRequest {
        InvocationRequest::encode(
            1,
            yunxi_protocol::CapabilityDescriptor::new(capability, 1).expect("capability"),
            operation,
            payload,
        )
        .expect("invocation")
    }

    #[test]
    fn transcribe_response_is_deterministic_and_has_final_text() {
        let request = transcribe_request();
        let result = transcribe(&request).expect("fixture response");
        assert_eq!(result.len(), 3);
        assert!(matches!(result[0], TranscribeEvent::Status(_)));
        assert!(matches!(
            result[2],
            TranscribeEvent::Transcript(TranscriptEvent {
                kind: crate::TranscriptKind::Final,
                ..
            })
        ));
    }

    #[test]
    fn synthesize_response_contains_only_fixture_audio() {
        let request = SynthesisRequest::new(
            RequestId::new("fixture-request").expect("request id"),
            StreamId::new("fixture-stream").expect("stream id"),
            "hello",
            AudioFormat::new(AudioCodec::PcmS16Le, 24_000, 1).expect("format"),
            StreamStatus::new(),
        )
        .expect("request");
        let result = synthesize(&request).expect("fixture response");
        assert_eq!(result.len(), 3);
        assert!(matches!(result[1], SynthesisEvent::Audio(_)));
        assert!(matches!(result[2], SynthesisEvent::Audio(_)));
    }

    #[test]
    fn cancellation_transitions_to_cancelled_without_audio_work() {
        let cancel_request = CancelRequest::new(
            StreamId::new("fixture-stream").expect("stream id"),
            "user stopped",
        )
        .expect("cancel request");
        let result = cancel(&cancel_request).expect("cancel response");
        assert_eq!(result.status.cancellation, CancellationState::Cancelled);
        assert!(
            transcribe(&TranscribeRequest {
                status: result.status.clone(),
                ..transcribe_request()
            })
            .expect("cancelled transcribe response")
            .iter()
            .all(|event| matches!(event, TranscribeEvent::Status(_)))
        );
    }

    #[test]
    fn invalid_cancel_status_and_unknown_operations_fail_closed() {
        let invalid = CancelRequest {
            stream_id: StreamId::new("fixture-stream").expect("stream id"),
            status: StreamStatus::new(),
        };
        assert!(invalid.validate().is_err());

        let request = invocation(
            capabilities::VOICE_TRANSCRIBE,
            "unknown",
            &serde_json::json!({}),
        );
        assert!(!supports(&request));
    }

    #[test]
    fn invalid_payload_is_rejected_by_contract_deserializer() {
        let request = invocation(
            capabilities::VOICE_TRANSCRIBE,
            TRANSCRIBE_OPERATION,
            &serde_json::json!({"chunks": []}),
        );
        assert!(request.decode_payload::<TranscribeRequest>().is_err());
    }
}
