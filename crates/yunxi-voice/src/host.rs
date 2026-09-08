//! Host-facing voice runtime facade.
//!
//! This module keeps provider selection and failure policy at the Host-facing
//! boundary. Providers remain replaceable trait objects, while a panic in an
//! in-process adapter quarantines only that provider. Process-backed providers
//! still provide the stronger crash and hard-timeout isolation described by
//! [`ProcessSidecarTransport`](crate::ProcessSidecarTransport).

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use crate::plugin::{CancelRequest, CancelResult};
use crate::provider::{
    AudioChunkSource, ProviderDescriptor, ProviderOutcome, SynthesizedAudioSink,
    SynthesizedAudioSource, TextFallback, TranscriptSink,
};
use crate::sidecar::{
    AudioOutputRequest, ChatProvider, ChatRequest, ChatSink, DoctorReport, EnumeratedDevices,
    OutputResult, ProviderFeatures, TalkRequest, TalkSink, VoiceProvider, VoiceRouter,
};
use crate::stream::{OperationContext, VoiceProviderError};
use crate::{SpeakRequest, TranscribeRequest};

pub const DEFAULT_HOST_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_HOST_OPERATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Host-owned limits for one voice operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoiceHostConfig {
    operation_timeout: Duration,
}

impl Default for VoiceHostConfig {
    fn default() -> Self {
        Self {
            operation_timeout: DEFAULT_HOST_OPERATION_TIMEOUT,
        }
    }
}

impl VoiceHostConfig {
    pub fn new(operation_timeout: Duration) -> Result<Self, VoiceProviderError> {
        if operation_timeout.is_zero() || operation_timeout > MAX_HOST_OPERATION_TIMEOUT {
            return Err(VoiceProviderError::invalid_provider("invalid_timeout"));
        }
        Ok(Self { operation_timeout })
    }

    pub fn operation_timeout(self) -> Duration {
        self.operation_timeout
    }
}

/// A provider wrapper that converts an adapter panic into a stable failure and
/// quarantines that adapter until the Host replaces it.
pub struct PanicIsolatedProvider {
    inner: Box<dyn VoiceProvider>,
    descriptor: ProviderDescriptor,
    features: ProviderFeatures,
    quarantined: bool,
}

impl fmt::Debug for PanicIsolatedProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PanicIsolatedProvider")
            .field("provider_id", &self.descriptor.id)
            .field("quarantined", &self.quarantined)
            .finish()
    }
}

impl PanicIsolatedProvider {
    pub fn new<P>(provider: P) -> Self
    where
        P: VoiceProvider + 'static,
    {
        Self::from_box(Box::new(provider))
    }

    pub fn from_box(provider: Box<dyn VoiceProvider>) -> Self {
        let metadata = catch_unwind(AssertUnwindSafe(|| {
            (provider.descriptor().clone(), provider.features())
        }));
        match metadata {
            Ok((descriptor, features)) => Self {
                inner: provider,
                descriptor,
                features,
                quarantined: false,
            },
            Err(_) => Self {
                inner: provider,
                descriptor: quarantined_descriptor(),
                features: disabled_features(),
                quarantined: true,
            },
        }
    }

    pub fn is_quarantined(&self) -> bool {
        self.quarantined
    }

    fn panicked() -> VoiceProviderError {
        VoiceProviderError::provider_failure("provider_panicked", false)
    }

    fn call<R>(
        &mut self,
        call: impl FnOnce(&mut dyn VoiceProvider) -> Result<R, VoiceProviderError>,
    ) -> Result<R, VoiceProviderError> {
        if self.quarantined {
            return Err(Self::panicked());
        }
        match catch_unwind(AssertUnwindSafe(|| call(self.inner.as_mut()))) {
            Ok(result) => result,
            Err(_) => {
                self.quarantined = true;
                Err(Self::panicked())
            }
        }
    }
}

impl VoiceProvider for PanicIsolatedProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn features(&self) -> ProviderFeatures {
        self.features
    }

    fn restart(&mut self) {
        if self.quarantined {
            return;
        }
        if catch_unwind(AssertUnwindSafe(|| self.inner.restart())).is_err() {
            self.quarantined = true;
        }
    }

    fn doctor(&mut self, context: &OperationContext) -> Result<DoctorReport, VoiceProviderError> {
        self.call(|provider| provider.doctor(context))
    }

    fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        self.call(|provider| provider.enumerate_devices(context))
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.call(|provider| provider.transcribe(request, source, sink, context))
    }

    fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.call(|provider| provider.speak(request, sink, context))
    }

    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.call(|provider| provider.chat(request, sink, context))
    }

    fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.call(|provider| provider.talk(request, sink, context))
    }

    fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.call(|provider| provider.playback(request, source, context))
    }

    fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        self.call(|provider| provider.save(request, source, context))
    }

    fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        self.call(|provider| provider.cancel(request))
    }
}

/// Dynamic Host-facing facade for all voice operations.
pub struct VoiceHostRuntime<F, C> {
    router: VoiceRouter<PanicIsolatedProvider, F, C>,
    config: VoiceHostConfig,
}

impl<F, C> fmt::Debug for VoiceHostRuntime<F, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VoiceHostRuntime")
            .field("provider", &self.router.provider())
            .field("generation", &self.router.generation())
            .field("operation_timeout", &self.config.operation_timeout)
            .finish()
    }
}

impl<F, C> VoiceHostRuntime<F, C> {
    pub fn new<P>(provider: P, fallback: F, chat: C, config: VoiceHostConfig) -> Self
    where
        P: VoiceProvider + 'static,
    {
        Self {
            router: VoiceRouter::new(PanicIsolatedProvider::new(provider), fallback, chat),
            config,
        }
    }

    pub fn config(&self) -> VoiceHostConfig {
        self.config
    }

    /// Creates a caller-owned context with the Host's default deadline.
    pub fn operation_context(&self) -> OperationContext {
        OperationContext::with_timeout(self.config.operation_timeout)
    }

    /// Applies the Host deadline while preserving the caller's cancellation
    /// token. Use the returned context for one provider call.
    pub fn bounded_context(&self, context: &OperationContext) -> OperationContext {
        context.bounded_by(self.config.operation_timeout)
    }

    pub fn replace_provider<P>(&mut self, provider: P)
    where
        P: VoiceProvider + 'static,
    {
        self.router
            .replace_provider(PanicIsolatedProvider::new(provider));
    }

    pub fn enable(&self) {
        self.router.enable();
    }

    pub fn disable(&self) {
        self.router.disable();
    }

    pub fn is_enabled(&self) -> bool {
        self.router.is_enabled()
    }

    pub fn generation(&self) -> u64 {
        self.router.generation()
    }

    /// Restarts provider-owned state after an out-of-band Host cancellation.
    ///
    /// The protocol cancellation message only carries the numeric invocation
    /// id, so it cannot be decoded as a voice `CancelRequest`. Resetting the
    /// provider here still guarantees that a process sidecar is torn down and
    /// that no stale audio operation can survive the cancellation boundary.
    pub fn restart(&mut self) {
        self.router.restart();
    }

    pub fn provider_descriptor(&self) -> &ProviderDescriptor {
        self.router.provider().descriptor()
    }

    pub fn provider_features(&self) -> ProviderFeatures {
        self.router.provider().features()
    }

    pub fn provider_is_quarantined(&self) -> bool {
        self.router.provider().is_quarantined()
    }
}

impl<F, C> VoiceHostRuntime<F, C>
where
    F: TextFallback,
    C: ChatProvider,
{
    pub fn doctor(&mut self, context: &OperationContext) -> DoctorReport {
        let context = self.bounded_context(context);
        self.router.doctor(&context)
    }

    pub fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.enumerate_devices(&context)
    }

    pub fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.transcribe(request, source, sink, &context)
    }

    pub fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.speak(request, sink, &context)
    }

    pub fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.chat(request, sink, &context)
    }

    pub fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.talk(request, sink, &context)
    }

    pub fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.playback(request, source, &context)
    }

    pub fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        let context = self.bounded_context(context);
        self.router.save(request, source, &context)
    }

    /// Acknowledge cancellation and reset provider-owned state. To stop a
    /// currently running call, cancel its shared `OperationContext` first.
    pub fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        self.router.cancel(request)
    }
}

fn disabled_features() -> ProviderFeatures {
    ProviderFeatures {
        doctor: false,
        device_enumeration: false,
        transcribe: false,
        speak: false,
        chat: false,
        talk: false,
        playback: false,
        save: false,
    }
}

fn quarantined_descriptor() -> ProviderDescriptor {
    ProviderDescriptor::new(
        "quarantined.provider",
        "Quarantined voice provider",
        false,
        false,
        false,
    )
    .expect("static quarantined provider descriptor")
}
