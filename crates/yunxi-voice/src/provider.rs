//! Replaceable device, transcription, synthesis, and text-fallback boundaries.
//!
//! These traits are intentionally synchronous and object-safe.  A real
//! plugin can adapt a platform SDK, an async service, or a subprocess behind
//! them without moving device handles or provider credentials into the Host.
//! The implementations in this module are deterministic test doubles only.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::audio::{AudioChunk, AudioFormat, SynthesizedAudioChunk};
use crate::message::{SynthesisRequest, TranscribeRequest};
use crate::stream::{
    AudioChunkIterator, AudioChunkQueue, CancellationToken, OperationContext, VoiceProviderError,
};
use crate::transcript::TranscriptEvent;

pub const VOICE_PROVIDER_API_VERSION: u16 = 1;
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_PROVIDER_NAME_BYTES: usize = 256;

/// Static metadata used during provider negotiation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    pub api_version: u16,
    pub supports_device: bool,
    pub supports_transcribe: bool,
    pub supports_synthesize: bool,
}

impl ProviderDescriptor {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        supports_device: bool,
        supports_transcribe: bool,
        supports_synthesize: bool,
    ) -> Result<Self, VoiceProviderError> {
        let id = id.into();
        let name = name.into();
        if id.is_empty()
            || id.len() > MAX_PROVIDER_ID_BYTES
            || !id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
            })
        {
            return Err(VoiceProviderError::invalid_provider("invalid_id"));
        }
        if name.is_empty()
            || name.len() > MAX_PROVIDER_NAME_BYTES
            || name.chars().any(char::is_control)
        {
            return Err(VoiceProviderError::invalid_provider("invalid_name"));
        }
        Ok(Self {
            id,
            name,
            api_version: VOICE_PROVIDER_API_VERSION,
            supports_device,
            supports_transcribe,
            supports_synthesize,
        })
    }
}

/// Host-controlled enable/disable gate for an individual provider.
///
/// Disabling a gate does not destroy session state.  It makes subsequent
/// provider calls fail before any device or provider work is started.
#[derive(Clone, Debug)]
pub struct ProviderGate {
    enabled: Arc<AtomicBool>,
}

impl ProviderGate {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), VoiceProviderError> {
        if self.is_enabled() {
            Ok(())
        } else {
            Err(VoiceProviderError::Disabled)
        }
    }
}

/// A source of captured audio.  Implementations must obey the supplied
/// cancellation/deadline context before blocking or doing provider work.
pub trait AudioChunkSource {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError>;
}

/// A bounded destination for captured audio.
pub trait AudioChunkSink {
    fn push(
        &mut self,
        chunk: AudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError>;
}

/// A source of synthesized audio for a device playback implementation.
pub trait SynthesizedAudioSource {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<SynthesizedAudioChunk>, VoiceProviderError>;
}

/// A destination for synthesized audio chunks.
pub trait SynthesizedAudioSink {
    fn push(
        &mut self,
        chunk: SynthesizedAudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError>;
}

/// A destination for streaming transcript events.
pub trait TranscriptSink {
    fn push(
        &mut self,
        event: TranscriptEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError>;
}

/// Result returned by a provider after it has drained or emitted its stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderOutcome {
    Completed,
    TextFallback,
}

/// Text returned when synthesis cannot produce audio and the Host elects to
/// show a text fallback instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextFallbackOutput {
    pub text: String,
}

/// Replaceable microphone/speaker boundary.
pub trait Device: Send {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn capture(
        &mut self,
        format: AudioFormat,
        sink: &mut dyn AudioChunkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;

    fn playback(
        &mut self,
        format: AudioFormat,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
}

/// Replaceable speech-to-text boundary.
pub trait Transcriber: Send {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
}

/// Replaceable text-to-speech boundary.
pub trait Synthesizer: Send {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn synthesize(
        &mut self,
        request: &SynthesisRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
}

/// Optional text-only fallback boundary for devices/providers that are
/// unavailable or intentionally disabled.
pub trait TextFallback: Send {
    fn transcribe_text(
        &mut self,
        request: &TranscribeRequest,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;

    fn synthesize_text(
        &mut self,
        request: &SynthesisRequest,
        context: &OperationContext,
    ) -> Result<TextFallbackOutput, VoiceProviderError>;
}

impl AudioChunkSource for AudioChunkQueue {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError> {
        self.pop(context)
    }
}

impl AudioChunkSink for AudioChunkQueue {
    fn push(
        &mut self,
        chunk: AudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        AudioChunkQueue::push(self, chunk, context)
    }
}

impl<'a> AudioChunkSource for AudioChunkIterator<'a> {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError> {
        context.check()?;
        Iterator::next(self)
            .transpose()
            .map(|chunk| chunk.cloned())
            .map_err(Into::into)
    }
}

/// An in-memory transcript sink useful for adapters and tests.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct VecTranscriptSink {
    events: Vec<TranscriptEvent>,
}

impl fmt::Debug for VecTranscriptSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VecTranscriptSink")
            .field("event_count", &self.events.len())
            .finish()
    }
}

impl VecTranscriptSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    pub fn into_events(self) -> Vec<TranscriptEvent> {
        self.events
    }
}

impl TranscriptSink for VecTranscriptSink {
    fn push(
        &mut self,
        event: TranscriptEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        context.check()?;
        event.validate()?;
        self.events.push(event);
        Ok(())
    }
}

/// An in-memory synthesized-audio sink.  Its Debug output contains only
/// counts and byte totals, never audio bytes.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct VecSynthesizedAudioSink {
    chunks: Vec<SynthesizedAudioChunk>,
}

impl fmt::Debug for VecSynthesizedAudioSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self
            .chunks
            .iter()
            .map(|chunk| chunk.data.len())
            .sum::<usize>();
        formatter
            .debug_struct("VecSynthesizedAudioSink")
            .field("chunk_count", &self.chunks.len())
            .field("byte_count", &bytes)
            .finish()
    }
}

impl VecSynthesizedAudioSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chunks(&self) -> &[SynthesizedAudioChunk] {
        &self.chunks
    }

    pub fn into_chunks(self) -> Vec<SynthesizedAudioChunk> {
        self.chunks
    }
}

impl SynthesizedAudioSink for VecSynthesizedAudioSink {
    fn push(
        &mut self,
        chunk: SynthesizedAudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        context.check()?;
        chunk.validate()?;
        self.chunks.push(chunk);
        Ok(())
    }
}

/// A synthesized-audio source that owns its chunks and exposes no blocking
/// behavior.  It is useful for device adapters and deterministic tests.
#[derive(Default)]
pub struct VecSynthesizedAudioSource {
    chunks: VecDeque<SynthesizedAudioChunk>,
}

impl fmt::Debug for VecSynthesizedAudioSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self
            .chunks
            .iter()
            .map(|chunk| chunk.data.len())
            .sum::<usize>();
        formatter
            .debug_struct("VecSynthesizedAudioSource")
            .field("chunk_count", &self.chunks.len())
            .field("byte_count", &bytes)
            .finish()
    }
}

impl VecSynthesizedAudioSource {
    pub fn new(chunks: impl IntoIterator<Item = SynthesizedAudioChunk>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
        }
    }
}

impl SynthesizedAudioSource for VecSynthesizedAudioSource {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<SynthesizedAudioChunk>, VoiceProviderError> {
        context.check()?;
        Ok(self.chunks.pop_front())
    }
}

/// Deterministic microphone/speaker stand-in.  It never opens a device and
/// records playback metadata only, not audio bytes.
pub struct MockDevice {
    descriptor: ProviderDescriptor,
    gate: ProviderGate,
    capture_chunks: Vec<AudioChunk>,
    played_chunks: usize,
    played_bytes: usize,
}

impl fmt::Debug for MockDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockDevice")
            .field("provider_id", &self.descriptor.id)
            .field("capture_chunk_count", &self.capture_chunks.len())
            .field("played_chunks", &self.played_chunks)
            .field("played_bytes", &self.played_bytes)
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl MockDevice {
    pub fn new(chunks: Vec<AudioChunk>) -> Result<Self, VoiceProviderError> {
        let descriptor =
            ProviderDescriptor::new("mock.device", "Mock voice device", true, false, false)?;
        for chunk in &chunks {
            chunk.validate()?;
        }
        Ok(Self {
            descriptor,
            gate: ProviderGate::new(true),
            capture_chunks: chunks,
            played_chunks: 0,
            played_bytes: 0,
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }

    pub fn played_chunks(&self) -> usize {
        self.played_chunks
    }

    pub fn played_bytes(&self) -> usize {
        self.played_bytes
    }
}

impl Device for MockDevice {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &mut self,
        format: AudioFormat,
        sink: &mut dyn AudioChunkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        format.validate()?;
        for chunk in self.capture_chunks.iter().cloned() {
            context.check()?;
            if chunk.format != format {
                return Err(VoiceProviderError::Contract(
                    crate::VoiceContractError::MixedFormat,
                ));
            }
            sink.push(chunk, context)?;
        }
        Ok(ProviderOutcome::Completed)
    }

    fn playback(
        &mut self,
        format: AudioFormat,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        format.validate()?;
        while let Some(chunk) = source.next(context)? {
            context.check()?;
            if chunk.format != format {
                return Err(VoiceProviderError::Contract(
                    crate::VoiceContractError::MixedFormat,
                ));
            }
            self.played_chunks += 1;
            self.played_bytes = self.played_bytes.saturating_add(chunk.data.len());
        }
        Ok(ProviderOutcome::Completed)
    }
}

/// Deterministic speech-to-text stand-in.  The input is drained to exercise
/// backpressure and cancellation, while output text is fixed and synthetic.
pub struct MockTranscriber {
    descriptor: ProviderDescriptor,
    gate: ProviderGate,
    partial_text: String,
    final_text: String,
}

impl fmt::Debug for MockTranscriber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockTranscriber")
            .field("provider_id", &self.descriptor.id)
            .field("partial_text_bytes", &self.partial_text.len())
            .field("final_text_bytes", &self.final_text.len())
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl MockTranscriber {
    pub fn new(
        partial_text: impl Into<String>,
        final_text: impl Into<String>,
    ) -> Result<Self, VoiceProviderError> {
        let descriptor =
            ProviderDescriptor::new("mock.transcriber", "Mock transcriber", false, true, false)?;
        let partial_text = partial_text.into();
        let final_text = final_text.into();
        let stream_id = crate::StreamId::new("mock-stream")?;
        crate::TranscriptEvent::partial(stream_id.clone(), 0, &partial_text)?;
        crate::TranscriptEvent::final_text(stream_id, 1, &final_text)?;
        Ok(Self {
            descriptor,
            gate: ProviderGate::new(true),
            partial_text,
            final_text,
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }
}

impl Transcriber for MockTranscriber {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        let mut count = 0;
        while source.next(context)?.is_some() {
            count += 1;
            if count > crate::MAX_STREAM_CHUNKS {
                return Err(VoiceProviderError::Contract(
                    crate::VoiceContractError::TooManyChunks {
                        count,
                        maximum: crate::MAX_STREAM_CHUNKS,
                    },
                ));
            }
        }
        sink.push(
            crate::TranscriptEvent::partial(request.stream_id.clone(), 0, &self.partial_text)?,
            context,
        )?;
        if request.input_complete {
            sink.push(
                crate::TranscriptEvent::final_text(request.stream_id.clone(), 1, &self.final_text)?,
                context,
            )?;
        }
        Ok(ProviderOutcome::Completed)
    }
}

/// Deterministic text-to-speech stand-in.  The emitted bytes are markers only
/// and are never claimed to be playable audio.
pub struct MockSynthesizer {
    descriptor: ProviderDescriptor,
    gate: ProviderGate,
}

impl fmt::Debug for MockSynthesizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockSynthesizer")
            .field("provider_id", &self.descriptor.id)
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl MockSynthesizer {
    pub fn new() -> Result<Self, VoiceProviderError> {
        Ok(Self {
            descriptor: ProviderDescriptor::new(
                "mock.synthesizer",
                "Mock synthesizer",
                false,
                false,
                true,
            )?,
            gate: ProviderGate::new(true),
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }
}

impl Synthesizer for MockSynthesizer {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn synthesize(
        &mut self,
        request: &SynthesisRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        context.check()?;
        sink.push(
            SynthesizedAudioChunk::new(
                request.request_id.clone(),
                request.stream_id.clone(),
                0,
                request.format,
                vec![0; 8],
                false,
            )?,
            context,
        )?;
        sink.push(
            SynthesizedAudioChunk::new(
                request.request_id.clone(),
                request.stream_id.clone(),
                1,
                request.format,
                vec![1; 8],
                true,
            )?,
            context,
        )?;
        Ok(ProviderOutcome::Completed)
    }
}

/// A complete deterministic provider bundle for end-to-end local tests.
pub struct LoopbackProvider {
    device: MockDevice,
    transcriber: MockTranscriber,
    synthesizer: MockSynthesizer,
}

impl LoopbackProvider {
    pub fn new(
        chunks: Vec<AudioChunk>,
        partial_text: impl Into<String>,
        final_text: impl Into<String>,
    ) -> Result<Self, VoiceProviderError> {
        Ok(Self {
            device: MockDevice::new(chunks)?,
            transcriber: MockTranscriber::new(partial_text, final_text)?,
            synthesizer: MockSynthesizer::new()?,
        })
    }

    pub fn device(&self) -> &MockDevice {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut MockDevice {
        &mut self.device
    }

    pub fn transcriber_mut(&mut self) -> &mut MockTranscriber {
        &mut self.transcriber
    }

    pub fn synthesizer_mut(&mut self) -> &mut MockSynthesizer {
        &mut self.synthesizer
    }
}

impl Device for LoopbackProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        self.device.descriptor()
    }

    fn capture(
        &mut self,
        format: AudioFormat,
        sink: &mut dyn AudioChunkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.device.capture(format, sink, context)
    }

    fn playback(
        &mut self,
        format: AudioFormat,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.device.playback(format, source, context)
    }
}

impl Transcriber for LoopbackProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        self.transcriber.descriptor()
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.transcriber.transcribe(request, source, sink, context)
    }
}

impl Synthesizer for LoopbackProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        self.synthesizer.descriptor()
    }

    fn synthesize(
        &mut self,
        request: &SynthesisRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.synthesizer.synthesize(request, sink, context)
    }
}

/// Text-only fallback implementation.  It does not access audio devices.
pub struct TextFallbackProvider {
    descriptor: ProviderDescriptor,
    gate: ProviderGate,
    transcription: String,
}

impl fmt::Debug for TextFallbackProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextFallbackProvider")
            .field("provider_id", &self.descriptor.id)
            .field("transcription_bytes", &self.transcription.len())
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl TextFallbackProvider {
    pub fn new(transcription: impl Into<String>) -> Result<Self, VoiceProviderError> {
        let transcription = transcription.into();
        let stream_id = crate::StreamId::new("fallback-stream")?;
        crate::TranscriptEvent::final_text(stream_id, 0, &transcription)?;
        Ok(Self {
            descriptor: ProviderDescriptor::new(
                "text.fallback",
                "Text fallback",
                false,
                true,
                true,
            )?,
            gate: ProviderGate::new(true),
            transcription,
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }
}

impl TextFallback for TextFallbackProvider {
    fn transcribe_text(
        &mut self,
        request: &TranscribeRequest,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        sink.push(
            crate::TranscriptEvent::final_text(request.stream_id.clone(), 0, &self.transcription)?,
            context,
        )?;
        Ok(ProviderOutcome::TextFallback)
    }

    fn synthesize_text(
        &mut self,
        request: &SynthesisRequest,
        context: &OperationContext,
    ) -> Result<TextFallbackOutput, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        context.check()?;
        Ok(TextFallbackOutput {
            text: request.text.clone(),
        })
    }
}

/// A small helper that turns a request's validated slice into a source for a
/// streaming transcriber.
pub fn request_source(
    request: &TranscribeRequest,
) -> Result<AudioChunkIterator<'_>, VoiceProviderError> {
    Ok(AudioChunkIterator::new(request)?)
}

/// A cloneable cancellation handle convenient for callers that run a provider
/// on a worker thread.
pub fn cancellation_handle(context: &OperationContext) -> CancellationToken {
    context.cancellation_token()
}

/// A sink that forwards audio to a queue while retaining no audio itself.
pub struct QueueAudioSink {
    queue: AudioChunkQueue,
}

impl QueueAudioSink {
    pub fn new(queue: AudioChunkQueue) -> Self {
        Self { queue }
    }

    pub fn queue(&self) -> &AudioChunkQueue {
        &self.queue
    }
}

impl AudioChunkSink for QueueAudioSink {
    fn push(
        &mut self,
        chunk: AudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        self.queue.push(chunk, context)
    }
}

/// A simple source wrapper for a queue, useful when the queue is shared with a
/// capture worker.
pub struct QueueAudioSource {
    queue: AudioChunkQueue,
}

impl QueueAudioSource {
    pub fn new(queue: AudioChunkQueue) -> Self {
        Self { queue }
    }
}

impl AudioChunkSource for QueueAudioSource {
    fn next(
        &mut self,
        context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError> {
        self.queue.pop(context)
    }
}

// Keep a mutex-backed holder available for adapters that need to share a
// collected sink between a worker and a Host thread without exposing bytes in
// diagnostics.
pub struct SharedTranscriptSink {
    inner: Arc<Mutex<VecTranscriptSink>>,
}

impl SharedTranscriptSink {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecTranscriptSink::new())),
        }
    }

    pub fn snapshot(&self) -> Result<Vec<TranscriptEvent>, VoiceProviderError> {
        self.inner
            .lock()
            .map(|sink| sink.events().to_vec())
            .map_err(|_| VoiceProviderError::QueuePoisoned)
    }
}

impl Default for SharedTranscriptSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptSink for SharedTranscriptSink {
    fn push(
        &mut self,
        event: TranscriptEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        let mut sink = self
            .inner
            .lock()
            .map_err(|_| VoiceProviderError::QueuePoisoned)?;
        sink.push(event, context)
    }
}
