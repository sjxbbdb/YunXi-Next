use std::time::Duration;

use yunxi_voice::{
    AudioChunk, AudioChunkSource, AudioCodec, AudioFormat, AudioOutputRequest, CancelRequest,
    ChatRequest, ChatSink, DeviceId, DoctorStatus, ExternalSidecarProvider, LoopbackVoiceProvider,
    MockChatProvider, OperationContext, OutputSelection, ProviderDescriptor, ProviderFeatures,
    ProviderOutcome, RequestId, ScriptedSidecarTransport, SpeakRequest, StreamId, StreamStatus,
    SynthesizedAudioChunk, SynthesizedAudioSink, TalkRequest, TalkSink, TextFallbackProvider,
    TranscribeRequest, VecChatSink, VecSynthesizedAudioSink, VecSynthesizedAudioSource,
    VecTalkSink, VecTranscriptSink, VoiceHostConfig, VoiceHostRuntime, VoiceProvider,
    VoiceProviderError, request_source,
};

fn format() -> AudioFormat {
    AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format")
}

fn input_request() -> TranscribeRequest {
    let stream_id = StreamId::new("host-stream").expect("stream id");
    TranscribeRequest::new(
        RequestId::new("host-request").expect("request id"),
        stream_id.clone(),
        format(),
        vec![
            AudioChunk::new(stream_id.clone(), 0, format(), vec![1; 4], false).expect("chunk"),
            AudioChunk::new(stream_id, 1, format(), vec![2; 4], true).expect("chunk"),
        ],
        true,
        StreamStatus::new(),
    )
    .expect("transcribe request")
}

fn speech_chunk(request_id: &str, stream_id: &str) -> SynthesizedAudioChunk {
    SynthesizedAudioChunk::new(
        RequestId::new(request_id).expect("request id"),
        StreamId::new(stream_id).expect("stream id"),
        0,
        format(),
        vec![3; 4],
        true,
    )
    .expect("speech chunk")
}

fn runtime() -> VoiceHostRuntime<TextFallbackProvider, MockChatProvider> {
    let provider = LoopbackVoiceProvider::new(
        vec![
            AudioChunk::new(
                StreamId::new("host-stream").expect("stream id"),
                0,
                format(),
                vec![1; 4],
                true,
            )
            .expect("loopback chunk"),
        ],
        "partial",
        "final",
        "chat reply",
    )
    .expect("loopback provider");
    VoiceHostRuntime::new(
        provider,
        TextFallbackProvider::new("typed fallback").expect("fallback"),
        MockChatProvider::new("independent chat").expect("chat"),
        VoiceHostConfig::new(Duration::from_secs(1)).expect("config"),
    )
}

#[test]
fn host_facade_covers_all_voice_operations_and_hot_swap() {
    let mut runtime = runtime();
    let context = runtime.operation_context();
    assert_eq!(runtime.doctor(&context).status, DoctorStatus::Ready);
    assert_eq!(
        runtime
            .enumerate_devices(&context)
            .expect("devices")
            .devices
            .len(),
        1
    );

    let request = input_request();
    let mut source = request_source(&request).expect("source");
    let mut transcripts = VecTranscriptSink::new();
    assert_eq!(
        runtime
            .transcribe(&request, &mut source, &mut transcripts, &context)
            .expect("transcribe"),
        ProviderOutcome::Completed
    );
    assert_eq!(transcripts.events().len(), 2);

    let speak = SpeakRequest::new(
        request.request_id.clone(),
        request.stream_id.clone(),
        "hello",
        format(),
        StreamStatus::new(),
    )
    .expect("speak request");
    let mut audio = VecSynthesizedAudioSink::new();
    runtime.speak(&speak, &mut audio, &context).expect("speak");
    assert!(!audio.chunks().is_empty());

    let chat_request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut chat = VecChatSink::new();
    runtime
        .chat(&chat_request, &mut chat, &context)
        .expect("chat");
    assert_eq!(chat.events()[0].text, "chat reply");

    let talk_request = TalkRequest::new(
        RequestId::new("talk-request").expect("request id"),
        request.clone(),
        format(),
        StreamStatus::new(),
    )
    .expect("talk request");
    let mut talk = VecTalkSink::new();
    runtime
        .talk(&talk_request, &mut talk, &context)
        .expect("talk");
    assert!(
        talk.events()
            .iter()
            .any(|event| matches!(event, yunxi_voice::TalkEvent::Audio(_)))
    );

    let playback_request = AudioOutputRequest::new(
        RequestId::new("playback-request").expect("request id"),
        StreamId::new("playback-stream").expect("stream id"),
        format(),
        OutputSelection::Playback(DeviceId::new("loopback-default").expect("device id")),
        StreamStatus::new(),
    )
    .expect("playback request");
    let mut playback =
        VecSynthesizedAudioSource::new([speech_chunk("playback-request", "playback-stream")]);
    assert_eq!(
        runtime
            .playback(&playback_request, &mut playback, &context)
            .expect("playback"),
        ProviderOutcome::Completed
    );

    let save_request = AudioOutputRequest::new(
        RequestId::new("save-request").expect("request id"),
        StreamId::new("save-stream").expect("stream id"),
        format(),
        OutputSelection::Save(yunxi_voice::SaveDestinationId::new("memory").expect("destination")),
        StreamStatus::new(),
    )
    .expect("save request");
    let mut saved = VecSynthesizedAudioSource::new([speech_chunk("save-request", "save-stream")]);
    let result = runtime
        .save(&save_request, &mut saved, &context)
        .expect("save");
    assert_eq!((result.chunks, result.bytes), (1, 4));

    let cancel = CancelRequest::new(request.stream_id.clone(), "user stopped").expect("cancel");
    let result = runtime.cancel(&cancel).expect("cancel result");
    assert_eq!(
        result.status.cancellation,
        yunxi_voice::CancellationState::Cancelled
    );

    let generation = runtime.generation();
    runtime.replace_provider(
        LoopbackVoiceProvider::new(vec![], "partial", "final", "replacement").expect("replacement"),
    );
    assert!(runtime.generation() > generation);
    assert!(!runtime.provider_is_quarantined());
}

struct PanickingProvider {
    descriptor: ProviderDescriptor,
}

impl PanickingProvider {
    fn new() -> Self {
        Self {
            descriptor: ProviderDescriptor::new(
                "panic.voice",
                "Panicking provider",
                true,
                true,
                true,
            )
            .expect("descriptor"),
        }
    }
}

impl VoiceProvider for PanickingProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn features(&self) -> ProviderFeatures {
        ProviderFeatures::audio_and_text()
    }

    fn doctor(
        &mut self,
        _: &OperationContext,
    ) -> Result<yunxi_voice::DoctorReport, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn enumerate_devices(
        &mut self,
        _: &OperationContext,
    ) -> Result<yunxi_voice::EnumeratedDevices, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn transcribe(
        &mut self,
        _: &TranscribeRequest,
        _: &mut dyn AudioChunkSource,
        _: &mut dyn yunxi_voice::TranscriptSink,
        _: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn speak(
        &mut self,
        _: &SpeakRequest,
        _: &mut dyn SynthesizedAudioSink,
        _: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn chat(
        &mut self,
        _: &ChatRequest,
        _: &mut dyn ChatSink,
        _: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn talk(
        &mut self,
        _: &TalkRequest,
        _: &mut dyn TalkSink,
        _: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn playback(
        &mut self,
        _: &AudioOutputRequest,
        _: &mut dyn yunxi_voice::SynthesizedAudioSource,
        _: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        panic!("provider test panic")
    }

    fn save(
        &mut self,
        _: &AudioOutputRequest,
        _: &mut dyn yunxi_voice::SynthesizedAudioSource,
        _: &OperationContext,
    ) -> Result<yunxi_voice::OutputResult, VoiceProviderError> {
        panic!("provider test panic")
    }
}

#[test]
fn panic_isolation_quarantines_only_the_provider_and_deadlines_are_shared() {
    let mut runtime = VoiceHostRuntime::new(
        PanickingProvider::new(),
        TextFallbackProvider::new("fallback").expect("fallback"),
        MockChatProvider::new("chat survives").expect("chat"),
        VoiceHostConfig::new(Duration::from_millis(20)).expect("config"),
    );
    let context = runtime.operation_context();
    let report = runtime.doctor(&context);
    assert_eq!(report.status, DoctorStatus::Unavailable);
    assert!(runtime.provider_is_quarantined());
    assert!(matches!(
        runtime.enumerate_devices(&context),
        Err(VoiceProviderError::ProviderFailure { code, .. }) if code == "provider_panicked"
    ));

    let mut chat = VecChatSink::new();
    let request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    runtime
        .chat(&request, &mut chat, &context)
        .expect("chat survives");
    assert_eq!(chat.events()[0].text, "chat survives");

    let parent = OperationContext::new();
    let bounded = runtime.bounded_context(&parent);
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(bounded.check(), Err(VoiceProviderError::TimedOut)));
    parent.cancel();
    assert!(matches!(
        bounded.check(),
        Err(VoiceProviderError::Cancelled)
    ));

    runtime.replace_provider(
        LoopbackVoiceProvider::new(vec![], "partial", "final", "recovered").expect("replacement"),
    );
    assert!(!runtime.provider_is_quarantined());
    let recovered_context = runtime.operation_context();
    assert_eq!(
        runtime.doctor(&recovered_context).status,
        DoctorStatus::Ready
    );
}

#[test]
fn external_cancel_acknowledges_and_resets_sidecar_state() {
    let descriptor = ProviderDescriptor::new("external.voice", "External voice", true, true, true)
        .expect("descriptor");
    let mut provider = ExternalSidecarProvider::new(
        descriptor,
        ProviderFeatures::audio_and_text(),
        ScriptedSidecarTransport::new(std::iter::empty::<
            Result<yunxi_voice::SidecarResponse, VoiceProviderError>,
        >()),
    )
    .expect("provider");
    let request = CancelRequest::new(
        StreamId::new("cancel-stream").expect("stream id"),
        "user stopped",
    )
    .expect("cancel request");
    let result = provider.cancel(&request).expect("cancel result");
    assert_eq!(
        result.status.cancellation,
        yunxi_voice::CancellationState::Cancelled
    );
    assert_eq!(provider.transport_mut().request_count(), 0);
    assert_eq!(provider.transport_mut().reset_count(), 1);
}
