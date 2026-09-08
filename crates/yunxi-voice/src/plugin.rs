//! Isolated JSONL plugin for the voice capability contracts.
//!
//! The process selects a configured bounded sidecar when one is available and
//! otherwise preserves a deterministic loopback path. The only network
//! operation is the existing local loopback protocol connection used by plugin
//! processes.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationRequest, InvocationResponse, PluginManifest, PluginMessage,
    PluginRiskLevel, PluginRuntimeMetadata, ProtocolError, capabilities,
    connect_plugin_with_manifest,
};

use crate::sidecar::{
    CHAT_OPERATION, ChatRequest, DEVICE_ENUMERATION_OPERATION, DOCTOR_OPERATION, OutputResult,
    PLAYBACK_OPERATION, SAVE_OPERATION, SPEAK_OPERATION, TALK_OPERATION,
};
use crate::{
    AudioOutputRequest, CancellationState, DeviceEnumerationRequest, DoctorRequest,
    ExternalSidecarProvider, LoopbackVoiceProvider, MockChatProvider, ProcessSidecarConfig,
    ProcessSidecarTransport, ProviderDescriptor, ProviderFeatures, ProviderOutcome, SidecarRequest,
    SynthesisEvent, SynthesisRequest, SynthesizedAudioChunk, TalkRequest, TextFallbackProvider,
    TranscribeEvent, TranscribeRequest, VecChatSink, VecSynthesizedAudioSink,
    VecSynthesizedAudioSource, VecTalkSink, VecTranscriptSink, VoiceContractError, VoiceHostConfig,
    VoiceHostRuntime, VoiceProvider, VoiceProviderError, request_source, transcribe_fixture,
};

#[cfg(test)]
use crate::TranscriptEvent;

pub const VOICE_FIXTURE_PLUGIN_ID: &str = "yunxi.voice.fixture";
pub const TRANSCRIBE_OPERATION: &str = "transcribe";
pub const SYNTHESIZE_OPERATION: &str = "synthesize";
pub const CANCEL_OPERATION: &str = "cancel";
pub const DEVICES_OPERATION: &str = "devices";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_PARTIAL_TEXT: &str = "fixture partial";
const FIXTURE_FINAL_TEXT: &str = "fixture final";

type ProcessVoiceHost = VoiceHostRuntime<TextFallbackProvider, MockChatProvider>;

/// Payload used by the process Host for `playback` and `save`.
///
/// The request carries the destination and grant; chunks are deliberately
/// explicit so the Host can validate and bound the complete output before it
/// reaches a device or a sidecar.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOutputPayload {
    pub request: AudioOutputRequest,
    pub chunks: Vec<SynthesizedAudioChunk>,
}

impl fmt::Debug for AudioOutputPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioOutputPayload")
            .field("request", &self.request)
            .field("chunk_count", &self.chunks.len())
            .field(
                "byte_count",
                &self
                    .chunks
                    .iter()
                    .map(|chunk| chunk.data.len())
                    .sum::<usize>(),
            )
            .finish()
    }
}

impl AudioOutputPayload {
    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        self.request.validate()?;
        if self.chunks.len() > crate::MAX_STREAM_CHUNKS {
            return Err(VoiceProviderError::provider_failure(
                "too_many_chunks",
                false,
            ));
        }
        let mut bytes = 0usize;
        for chunk in &self.chunks {
            chunk.validate()?;
            bytes = bytes.saturating_add(chunk.data.len());
            if bytes > crate::MAX_SIDECAR_PAYLOAD_BYTES {
                return Err(VoiceProviderError::provider_failure(
                    "payload_too_large",
                    false,
                ));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AudioOutputPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireAudioOutputPayload {
            request: AudioOutputRequest,
            chunks: Vec<SynthesizedAudioChunk>,
        }

        let wire = WireAudioOutputPayload::deserialize(deserializer)?;
        let payload = Self {
            request: wire.request,
            chunks: wire.chunks,
        };
        payload.validate().map_err(D::Error::custom)?;
        Ok(payload)
    }
}

/// Starts the production-selecting voice plugin entry point.
///
/// An explicit `YUNXI_VOICE_SIDECAR_PROGRAM` selects the bounded sidecar
/// provider. With no sidecar configuration this deliberately keeps the
/// deterministic fixture, so a machine without audio hardware remains usable.
pub fn run_voice_plugin_from_env() -> Result<(), VoiceFixtureError> {
    let config = match ProcessSidecarConfig::from_environment() {
        Ok(Some(config)) => config,
        Ok(None) | Err(_) => return run_voice_fixture(),
    };
    let timeout = config.timeout_value();
    let provider = match config.into_provider(
        ProviderDescriptor::new(
            "process.voice",
            "Configured voice sidecar",
            true,
            true,
            true,
        )?,
        ProviderFeatures::audio_and_text(),
    ) {
        Ok(provider) => provider,
        Err(_) => return run_voice_fixture_with_device_declaration(),
    };
    run_provider_plugin(provider, timeout)
}

fn run_voice_fixture_with_device_declaration() -> Result<(), VoiceFixtureError> {
    run_voice_fixture_with_manifest(
        vec![GrantRequirement::required(GrantKind::Device)],
        "voice-sidecar-fallback",
        PluginRiskLevel::External,
    )
}

pub fn run_voice_fixture() -> Result<(), VoiceFixtureError> {
    run_voice_fixture_with_manifest(Vec::new(), "voice-fixture", PluginRiskLevel::Safe)
}

fn run_voice_fixture_with_manifest(
    grants: Vec<GrantRequirement>,
    adapter: &str,
    risk: PluginRiskLevel,
) -> Result<(), VoiceFixtureError> {
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
    .with_grants(grants)
    .with_runtime_metadata(PluginRuntimeMetadata::new(adapter, risk));
    let mut session = connect_plugin_with_manifest(manifest, CONNECT_TIMEOUT)?;
    let mut runtime = loopback_runtime(Duration::from_secs(30))?;
    run_runtime_plugin(&mut session, &mut runtime)
}

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

fn run_provider_plugin(
    provider: ExternalSidecarProvider<ProcessSidecarTransport>,
    timeout: Duration,
) -> Result<(), VoiceFixtureError> {
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
        "YunXi configured voice provider",
        env!("CARGO_PKG_VERSION"),
        vec![transcribe, synthesize],
    )
    .with_grants(vec![GrantRequirement::required(GrantKind::Device)])
    .with_runtime_metadata(PluginRuntimeMetadata::new(
        "voice-sidecar",
        PluginRiskLevel::External,
    ));
    let mut session = connect_plugin_with_manifest(manifest, CONNECT_TIMEOUT)?;
    let mut runtime = host_runtime(provider, timeout)?;
    run_runtime_plugin(&mut session, &mut runtime)
}

fn host_runtime<P>(provider: P, timeout: Duration) -> Result<ProcessVoiceHost, VoiceFixtureError>
where
    P: VoiceProvider + 'static,
{
    let fallback = TextFallbackProvider::new(FIXTURE_FINAL_TEXT)?;
    let chat = MockChatProvider::new("fixture chat")?;
    let config = VoiceHostConfig::new(timeout)?;
    Ok(VoiceHostRuntime::new(provider, fallback, chat, config))
}

fn loopback_runtime(timeout: Duration) -> Result<ProcessVoiceHost, VoiceFixtureError> {
    let fixture = transcribe_fixture()?;
    let provider = LoopbackVoiceProvider::new(
        fixture.request.chunks,
        FIXTURE_PARTIAL_TEXT,
        FIXTURE_FINAL_TEXT,
        "fixture chat",
    )?;
    host_runtime(provider, timeout)
}

fn run_runtime_plugin(
    session: &mut yunxi_protocol::PluginSession,
    runtime: &mut ProcessVoiceHost,
) -> Result<(), VoiceFixtureError> {
    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => handle_runtime_request(session, &request, runtime)?,
            HostMessage::Cancel { .. } => runtime.restart(),
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(VoiceFixtureError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn handle_runtime_request(
    session: &mut yunxi_protocol::PluginSession,
    request: &InvocationRequest,
    runtime: &mut ProcessVoiceHost,
) -> Result<(), VoiceFixtureError> {
    let request_id = request.request_id();
    if !supports(request) {
        send_failure(
            session,
            request_id,
            "unsupported_operation",
            "voice provider does not support the requested capability or operation".to_string(),
        )?;
        return Ok(());
    }

    let operation = request.operation();
    match operation {
        DOCTOR_OPERATION => {
            let payload = match request.decode_payload::<DoctorRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let _ = payload;
            let context = runtime.operation_context();
            send_result(session, request_id, runtime.doctor(&context))?;
        }
        DEVICE_ENUMERATION_OPERATION | DEVICES_OPERATION => {
            let payload = match request.decode_payload::<DeviceEnumerationRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let _ = payload;
            let context = runtime.operation_context();
            match runtime.enumerate_devices(&context) {
                Ok(result) => send_result(session, request_id, result)?,
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        TRANSCRIBE_OPERATION => {
            let payload = match request.decode_payload::<TranscribeRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let context = runtime.operation_context();
            let mut source = request_source(&payload)?;
            let mut sink = VecTranscriptSink::new();
            match runtime.transcribe(&payload, &mut source, &mut sink, &context) {
                Ok(_) => {
                    let mut events = vec![TranscribeEvent::Status(payload.status.clone())];
                    events.extend(
                        sink.into_events()
                            .into_iter()
                            .map(TranscribeEvent::Transcript),
                    );
                    send_result(session, request_id, events)?;
                }
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        SYNTHESIZE_OPERATION | SPEAK_OPERATION => {
            let payload = match request.decode_payload::<SynthesisRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let context = runtime.operation_context();
            let mut sink = VecSynthesizedAudioSink::new();
            match runtime.speak(&payload, &mut sink, &context) {
                Ok(_) => {
                    let mut events = vec![SynthesisEvent::Status(payload.status.clone())];
                    events.extend(sink.into_chunks().into_iter().map(SynthesisEvent::Audio));
                    send_result(session, request_id, events)?;
                }
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        CHAT_OPERATION => {
            let payload = match request.decode_payload::<ChatRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let context = runtime.operation_context();
            let mut sink = VecChatSink::new();
            match runtime.chat(&payload, &mut sink, &context) {
                Ok(_) => send_result(session, request_id, sink.events())?,
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        TALK_OPERATION => {
            let payload = match request.decode_payload::<TalkRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let context = runtime.operation_context();
            let mut sink = VecTalkSink::new();
            match runtime.talk(&payload, &mut sink, &context) {
                Ok(_) => send_result(session, request_id, sink.events())?,
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        PLAYBACK_OPERATION | SAVE_OPERATION => {
            let payload = match request.decode_payload::<AudioOutputPayload>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            let sidecar_request = if operation == PLAYBACK_OPERATION {
                SidecarRequest::Playback {
                    request: payload.request.clone(),
                    chunks: payload.chunks.clone(),
                }
            } else {
                SidecarRequest::Save {
                    request: payload.request.clone(),
                    chunks: payload.chunks.clone(),
                }
            };
            if let Err(error) = sidecar_request.validate() {
                return invalid_request(session, request_id, error);
            }
            let summary = OutputResult {
                chunks: payload.chunks.len(),
                bytes: payload.chunks.iter().map(|chunk| chunk.data.len()).sum(),
            };
            let mut source = VecSynthesizedAudioSource::new(payload.chunks);
            let context = runtime.operation_context();
            if operation == PLAYBACK_OPERATION {
                match runtime.playback(&payload.request, &mut source, &context) {
                    Ok(ProviderOutcome::Completed) => send_result(session, request_id, summary)?,
                    Ok(ProviderOutcome::TextFallback) => send_failure(
                        session,
                        request_id,
                        "audio_output_unavailable",
                        "audio playback was not performed".to_owned(),
                    )?,
                    Err(error) => send_provider_failure(session, request_id, &error)?,
                }
            } else {
                match runtime.save(&payload.request, &mut source, &context) {
                    Ok(result) => send_result(session, request_id, result)?,
                    Err(error) => send_provider_failure(session, request_id, &error)?,
                }
            }
        }
        CANCEL_OPERATION => {
            let payload = match request.decode_payload::<CancelRequest>() {
                Ok(payload) => payload,
                Err(error) => return invalid_request(session, request_id, error),
            };
            match runtime.cancel(&payload) {
                Ok(result) => send_result(session, request_id, result)?,
                Err(error) => send_provider_failure(session, request_id, &error)?,
            }
        }
        _ => unreachable!("operation was checked by supports"),
    }
    Ok(())
}

fn invalid_request<E: fmt::Display>(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    error: E,
) -> Result<(), VoiceFixtureError> {
    send_failure(session, request_id, "invalid_request", error.to_string())?;
    Ok(())
}

fn supports(request: &InvocationRequest) -> bool {
    let capability = request.capability();
    let id = capability.id().as_str();
    let version = capability.version();
    let version_ok = version == crate::VOICE_CAPABILITY_VERSION as u32;
    version_ok
        && match request.operation() {
            TRANSCRIBE_OPERATION
            | DOCTOR_OPERATION
            | DEVICE_ENUMERATION_OPERATION
            | DEVICES_OPERATION
            | CHAT_OPERATION
            | TALK_OPERATION => id == capabilities::VOICE_TRANSCRIBE,
            SYNTHESIZE_OPERATION | SPEAK_OPERATION | PLAYBACK_OPERATION | SAVE_OPERATION => {
                id == capabilities::VOICE_SYNTHESIZE
            }
            CANCEL_OPERATION => {
                id == capabilities::VOICE_TRANSCRIBE || id == capabilities::VOICE_SYNTHESIZE
            }
            _ => false,
        }
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

fn send_provider_failure(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    error: &VoiceProviderError,
) -> Result<(), ProtocolError> {
    session.send(&PluginMessage::InvocationFailed {
        request_id,
        code: "voice_provider_error".to_string(),
        message: error.to_string(),
        retryable: error.is_retryable(),
    })
}

#[derive(Debug)]
pub enum VoiceFixtureError {
    Capability(CapabilityError),
    Contract(VoiceContractError),
    Invocation(InvocationCodecError),
    Provider(VoiceProviderError),
    Protocol(ProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for VoiceFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Contract(error) => error.fmt(formatter),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Provider(error) => error.fmt(formatter),
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
            Self::Provider(error) => Some(error),
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

impl From<VoiceProviderError> for VoiceFixtureError {
    fn from(error: VoiceProviderError) -> Self {
        Self::Provider(error)
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
