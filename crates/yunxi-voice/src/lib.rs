//! Stable, bounded contracts and replaceable provider boundaries for isolated
//! YunXi voice plugins.
//!
//! The crate contains no unstable OS audio, codec SDK, network, or async
//! runtime integration. It does provide synchronous object-safe boundaries
//! that a real plugin can implement, a standard-library WAV/PCM file adapter,
//! and deterministic Mock/Loopback and text-fallback providers for local
//! verification.
#![forbid(unsafe_code)]

mod audio;
mod error;
mod fixture;
mod host;
mod identifiers;
mod local;
mod message;
mod plugin;
mod process;
mod provider;
mod sidecar;
mod state;
mod stream;
mod transcript;

pub use audio::{
    AudioChunk, AudioCodec, AudioFileError, AudioFormat, MAX_AUDIO_CHUNK_BYTES,
    MAX_AUDIO_FILE_BYTES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, MAX_STREAM_CHUNKS, PcmAudio,
    SynthesizedAudioChunk, decode_wav, encode_wav, read_pcm_file, read_wav_file, write_wav_file,
};
pub use error::VoiceContractError;
pub use fixture::{SynthesisFixture, TranscriptionFixture, synthesize_fixture, transcribe_fixture};
pub use host::{
    DEFAULT_HOST_OPERATION_TIMEOUT, MAX_HOST_OPERATION_TIMEOUT, PanicIsolatedProvider,
    VoiceHostConfig, VoiceHostRuntime,
};
pub use identifiers::{RequestId, StreamId};
pub use local::{
    LOCAL_INPUT_DEVICE_ID, LOCAL_INPUT_PCM_ENV, LOCAL_INPUT_WAV_ENV, LOCAL_OUTPUT_DEVICE_ID,
    LOCAL_OUTPUT_PCM_ENV, LOCAL_OUTPUT_WAV_ENV, LocalAudioConfig, LocalFileDevice,
};
pub use message::{
    CapabilityDescriptor, MAX_SYNTHESIS_TEXT_BYTES, SYNTHESIZE_CAPABILITY, SynthesisEvent,
    SynthesisRequest, TRANSCRIBE_CAPABILITY, TranscribeEvent, TranscribeRequest,
    VOICE_CAPABILITY_VERSION, VoiceCapability,
};
pub use plugin::{
    AudioOutputPayload, CANCEL_OPERATION, CancelRequest, CancelResult, DEVICES_OPERATION,
    SYNTHESIZE_OPERATION, TRANSCRIBE_OPERATION, VOICE_FIXTURE_PLUGIN_ID, VoiceFixtureError,
    run_voice_fixture, run_voice_plugin_from_env,
};
pub use process::{
    DEFAULT_SIDECAR_MAX_FRAME_BYTES, DEFAULT_SIDECAR_TIMEOUT, MAX_SIDECAR_FRAME_BYTES,
    MAX_SIDECAR_TIMEOUT, ProcessSidecarConfig, ProcessSidecarTransport,
};
pub use provider::{
    AudioChunkSink, AudioChunkSource, Device, LoopbackProvider, MockDevice, MockSynthesizer,
    MockTranscriber, ProviderDescriptor, ProviderGate, ProviderOutcome, QueueAudioSink,
    QueueAudioSource, SharedTranscriptSink, SynthesizedAudioSink, SynthesizedAudioSource,
    Synthesizer, TextFallback, TextFallbackOutput, TextFallbackProvider, Transcriber,
    TranscriptSink, VOICE_PROVIDER_API_VERSION, VecSynthesizedAudioSink, VecSynthesizedAudioSource,
    VecTranscriptSink, cancellation_handle, request_source,
};
pub use sidecar::{
    AUDIO_OUTPUT_OPERATION, AudioOutputRequest, CHAT_OPERATION, ChatEvent, ChatEventKind,
    ChatProvider, ChatRequest, ChatSink, DEVICE_ENUMERATION_OPERATION, DOCTOR_OPERATION,
    DeviceAvailability, DeviceDirection, DeviceEnumerationRequest, DeviceGrant, DeviceId,
    DeviceInfo, DoctorReport, DoctorRequest, DoctorStatus, EnumeratedDevices,
    ExternalSidecarProvider, LoopbackVoiceProvider, MAX_CHAT_TEXT_BYTES, MAX_DEVICE_NAME_BYTES,
    MAX_DEVICES, MAX_SIDECAR_ERROR_CODE_BYTES, MAX_SIDECAR_PAYLOAD_BYTES, MockChatProvider,
    OutputResult, OutputSelection, PLAYBACK_OPERATION, ProviderFeatures, SAVE_OPERATION,
    SIDECAR_PROTOCOL_VERSION, SPEAK_OPERATION, SaveDestinationId, ScriptedSidecarTransport,
    SidecarError, SidecarRequest, SidecarRequestFrame, SidecarResponse, SidecarResponseFrame,
    SidecarTransport, SpeakRequest, TALK_OPERATION, TalkEvent, TalkRequest, TalkSink, VecChatSink,
    VecTalkSink, VoiceProvider, VoiceRouter, VoiceRuntimeError,
};
pub use state::{
    BackpressureState, CancellationState, MAX_BUFFER_CAPACITY_BYTES, MAX_CANCELLATION_REASON_BYTES,
    StreamStatus,
};
pub use stream::{
    AudioChunkIterator, AudioChunkQueue, CancellationToken, OperationContext, VoiceProviderError,
};
pub use transcript::{MAX_TRANSCRIPT_TEXT_BYTES, TranscriptEvent, TranscriptKind};
