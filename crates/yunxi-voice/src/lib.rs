//! Stable, bounded contracts and replaceable provider boundaries for isolated
//! YunXi voice plugins.
//!
//! The crate contains no concrete audio device, codec, network, or async
//! runtime integration.  It does provide synchronous object-safe boundaries
//! that a real plugin can implement, plus deterministic Mock/Loopback and
//! text-fallback providers for local verification.
#![forbid(unsafe_code)]

mod audio;
mod error;
mod fixture;
mod identifiers;
mod message;
mod plugin;
mod provider;
mod state;
mod stream;
mod transcript;

pub use audio::{
    AudioChunk, AudioCodec, AudioFormat, MAX_AUDIO_CHUNK_BYTES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ,
    MAX_STREAM_CHUNKS, SynthesizedAudioChunk,
};
pub use error::VoiceContractError;
pub use fixture::{SynthesisFixture, TranscriptionFixture, synthesize_fixture, transcribe_fixture};
pub use identifiers::{RequestId, StreamId};
pub use message::{
    CapabilityDescriptor, MAX_SYNTHESIS_TEXT_BYTES, SYNTHESIZE_CAPABILITY, SynthesisEvent,
    SynthesisRequest, TRANSCRIBE_CAPABILITY, TranscribeEvent, TranscribeRequest,
    VOICE_CAPABILITY_VERSION, VoiceCapability,
};
pub use plugin::{
    CANCEL_OPERATION, CancelRequest, CancelResult, SYNTHESIZE_OPERATION, TRANSCRIBE_OPERATION,
    VOICE_FIXTURE_PLUGIN_ID, VoiceFixtureError, run_voice_fixture,
};
pub use provider::{
    AudioChunkSink, AudioChunkSource, Device, LoopbackProvider, MockDevice, MockSynthesizer,
    MockTranscriber, ProviderDescriptor, ProviderGate, ProviderOutcome, QueueAudioSink,
    QueueAudioSource, SharedTranscriptSink, SynthesizedAudioSink, SynthesizedAudioSource,
    Synthesizer, TextFallback, TextFallbackOutput, TextFallbackProvider, Transcriber,
    TranscriptSink, VOICE_PROVIDER_API_VERSION, VecSynthesizedAudioSink, VecSynthesizedAudioSource,
    VecTranscriptSink, cancellation_handle, request_source,
};
pub use state::{
    BackpressureState, CancellationState, MAX_BUFFER_CAPACITY_BYTES, MAX_CANCELLATION_REASON_BYTES,
    StreamStatus,
};
pub use stream::{
    AudioChunkIterator, AudioChunkQueue, CancellationToken, OperationContext, VoiceProviderError,
};
pub use transcript::{MAX_TRANSCRIPT_TEXT_BYTES, TranscriptEvent, TranscriptKind};
