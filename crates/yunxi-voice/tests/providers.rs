use std::collections::VecDeque;
use std::time::Duration;

use yunxi_voice::{
    AudioChunk, AudioChunkIterator, AudioChunkQueue, AudioChunkSource, AudioCodec, AudioFormat,
    ChatEvent, ChatEventKind, ChatRequest, Device, DeviceAvailability, DeviceDirection,
    DeviceGrant, DeviceId, DoctorReport, DoctorStatus, EnumeratedDevices, ExternalSidecarProvider,
    LoopbackProvider, LoopbackVoiceProvider, MockChatProvider, MockSynthesizer, MockTranscriber,
    OperationContext, OutputSelection, ProviderDescriptor, ProviderFeatures, ProviderGate,
    ProviderOutcome, RequestId, ScriptedSidecarTransport, SidecarResponse, StreamId, StreamStatus,
    SynthesisRequest, SynthesizedAudioChunk, Synthesizer, TalkRequest, TextFallback,
    TextFallbackProvider, TranscribeEvent, TranscribeRequest, Transcriber, VecChatSink,
    VecSynthesizedAudioSink, VecSynthesizedAudioSource, VecTalkSink, VecTranscriptSink,
    VoiceProvider, VoiceProviderError, VoiceRouter,
};

fn format() -> AudioFormat {
    AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format")
}

fn chunk(sequence: u64, end_of_stream: bool) -> AudioChunk {
    AudioChunk::new(
        StreamId::new("provider-stream").expect("stream id"),
        sequence,
        format(),
        vec![sequence as u8; 4],
        end_of_stream,
    )
    .expect("chunk")
}

fn request() -> TranscribeRequest {
    let stream_id = StreamId::new("provider-stream").expect("stream id");
    TranscribeRequest::new(
        RequestId::new("provider-request").expect("request id"),
        stream_id.clone(),
        format(),
        vec![
            AudioChunk::new(stream_id.clone(), 0, format(), vec![1; 4], false).expect("chunk"),
            AudioChunk::new(stream_id, 1, format(), vec![2; 4], true).expect("chunk"),
        ],
        true,
        StreamStatus::new(),
    )
    .expect("request")
}

fn synthesis_request() -> SynthesisRequest {
    SynthesisRequest::new(
        RequestId::new("provider-request").expect("request id"),
        StreamId::new("provider-stream").expect("stream id"),
        "hello",
        format(),
        StreamStatus::new(),
    )
    .expect("synthesis request")
}

fn output_request(selection: OutputSelection) -> yunxi_voice::AudioOutputRequest {
    yunxi_voice::AudioOutputRequest::new(
        RequestId::new("provider-request").expect("request id"),
        StreamId::new("provider-stream").expect("stream id"),
        format(),
        selection,
        StreamStatus::new(),
    )
    .expect("output request")
}

struct CountingAudioSource {
    chunks: VecDeque<AudioChunk>,
    pulls: usize,
}

impl AudioChunkSource for CountingAudioSource {
    fn next(
        &mut self,
        _context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError> {
        let chunk = self.chunks.pop_front();
        if chunk.is_some() {
            self.pulls += 1;
        }
        Ok(chunk)
    }
}

#[test]
fn queue_is_byte_bounded_and_reports_timeout_and_cancellation() {
    let queue = AudioChunkQueue::new(4).expect("queue");
    queue.try_push(chunk(0, true)).expect("first chunk fits");
    assert!(matches!(
        queue.try_push(chunk(1, true)),
        Err(VoiceProviderError::Backpressure {
            buffered: 4,
            capacity: 4
        })
    ));

    let timed_out = OperationContext::with_timeout(Duration::from_millis(5));
    assert!(matches!(
        queue.push(chunk(1, true), &timed_out),
        Err(VoiceProviderError::TimedOut)
    ));

    let cancelled = OperationContext::new();
    cancelled.cancel();
    assert!(matches!(
        queue.pop(&cancelled),
        Err(VoiceProviderError::Cancelled)
    ));

    assert_eq!(
        queue
            .pop(&OperationContext::new())
            .expect("pop")
            .unwrap()
            .sequence,
        0
    );
    queue.close();
    assert!(
        queue
            .pop(&OperationContext::new())
            .expect("closed pop")
            .is_none()
    );
    assert!(matches!(
        queue.try_push(chunk(2, true)),
        Err(VoiceProviderError::QueueClosed)
    ));
}

#[test]
fn iterator_revalidates_mutated_untrusted_chunks() {
    let mut request = request();
    request.chunks[1].sequence = 7;
    assert!(matches!(
        AudioChunkIterator::new(&request),
        Err(yunxi_voice::VoiceContractError::InvalidSequence {
            expected: 1,
            actual: 7
        })
    ));
}

#[test]
fn mock_providers_stream_without_hardware_and_can_be_disabled() {
    let request = request();
    let queue = AudioChunkQueue::new(16).expect("queue");
    queue.try_push(request.chunks[0].clone()).expect("chunk");
    queue.try_push(request.chunks[1].clone()).expect("chunk");
    queue.close();

    let mut transcriber = MockTranscriber::new("partial", "final").expect("transcriber");
    let mut source = queue.clone();
    let mut transcript = VecTranscriptSink::new();
    assert_eq!(
        transcriber
            .transcribe(
                &request,
                &mut source,
                &mut transcript,
                &OperationContext::new(),
            )
            .expect("transcription"),
        ProviderOutcome::Completed
    );
    assert_eq!(transcript.events().len(), 2);

    let mut synthesizer = MockSynthesizer::new().expect("synthesizer");
    let mut audio = VecSynthesizedAudioSink::new();
    assert_eq!(
        synthesizer
            .synthesize(&synthesis_request(), &mut audio, &OperationContext::new(),)
            .expect("synthesis"),
        ProviderOutcome::Completed
    );
    assert_eq!(audio.chunks().len(), 2);
    assert_eq!(audio.chunks()[0].data, vec![0; 8]);

    let gate = synthesizer.gate();
    gate.disable();
    let mut disabled_audio = VecSynthesizedAudioSink::new();
    assert!(matches!(
        synthesizer.synthesize(
            &synthesis_request(),
            &mut disabled_audio,
            &OperationContext::new(),
        ),
        Err(VoiceProviderError::Disabled)
    ));
    assert!(disabled_audio.chunks().is_empty());
}

#[test]
fn loopback_device_and_text_fallback_have_replaceable_boundaries() {
    let mut loopback =
        LoopbackProvider::new(vec![chunk(0, true)], "partial", "final").expect("loopback");
    let queue = AudioChunkQueue::new(8).expect("queue");
    let mut queue_sink = queue.clone();
    assert_eq!(
        loopback
            .capture(format(), &mut queue_sink, &OperationContext::new())
            .expect("capture"),
        ProviderOutcome::Completed
    );
    queue.close();

    let mut source = queue;
    let mut transcript = VecTranscriptSink::new();
    assert_eq!(
        loopback
            .transcribe(
                &request(),
                &mut source,
                &mut transcript,
                &OperationContext::new(),
            )
            .expect("loopback transcribe"),
        ProviderOutcome::Completed
    );

    let mut audio = VecSynthesizedAudioSink::new();
    loopback
        .synthesize(&synthesis_request(), &mut audio, &OperationContext::new())
        .expect("loopback synthesize");
    let mut audio_source = VecSynthesizedAudioSource::new(audio.into_chunks());
    loopback
        .playback(format(), &mut audio_source, &OperationContext::new())
        .expect("loopback playback");
    assert_eq!(loopback.device().played_chunks(), 2);

    let mut fallback = TextFallbackProvider::new("typed fallback").expect("fallback");
    let mut fallback_events = VecTranscriptSink::new();
    assert_eq!(
        fallback
            .transcribe_text(&request(), &mut fallback_events, &OperationContext::new(),)
            .expect("fallback transcription"),
        ProviderOutcome::TextFallback
    );
    let text = fallback
        .synthesize_text(&synthesis_request(), &OperationContext::new())
        .expect("fallback synthesis");
    assert_eq!(text.text, "hello");
}

#[test]
fn diagnostics_do_not_include_audio_payloads_or_provider_secrets() {
    let queue = AudioChunkQueue::new(4).expect("queue");
    queue.try_push(chunk(0, true)).expect("chunk");
    let debug = format!("{queue:?}");
    assert!(!debug.contains("00000000"));
    assert!(!debug.contains("provider-secret"));

    let gate = ProviderGate::new(false);
    assert_eq!(
        gate.check().expect_err("disabled gate"),
        VoiceProviderError::Disabled
    );
}

#[test]
fn external_sidecar_contract_covers_doctor_devices_and_chat() {
    let report = DoctorReport::ready(ProviderFeatures::audio_and_text(), 1).expect("report");
    let format = format();
    let devices = EnumeratedDevices::new(vec![
        yunxi_voice::DeviceInfo::new(
            DeviceId::new("microphone").expect("device id"),
            "External microphone",
            DeviceDirection::Input,
            DeviceAvailability::Available,
            vec![format],
        )
        .expect("device"),
    ])
    .expect("devices");
    let transport = ScriptedSidecarTransport::new([
        Ok(SidecarResponse::Doctor(report)),
        Ok(SidecarResponse::Devices(devices)),
        Ok(SidecarResponse::Chat(vec![
            ChatEvent::new(ChatEventKind::Final, "external reply").expect("event"),
        ])),
    ]);
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", true, true, true)
            .expect("descriptor"),
        ProviderFeatures::audio_and_text(),
        transport,
    )
    .expect("provider");

    assert_eq!(
        provider
            .doctor(&OperationContext::new())
            .expect("doctor")
            .status,
        DoctorStatus::Ready
    );
    assert_eq!(
        provider
            .enumerate_devices(&OperationContext::new())
            .expect("devices")
            .devices
            .len(),
        1
    );
    let request = ChatRequest::new(
        RequestId::new("request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut sink = VecChatSink::new();
    provider
        .chat(&request, &mut sink, &OperationContext::new())
        .expect("chat");
    assert_eq!(sink.events()[0].text, "external reply");
}

#[test]
fn external_sidecar_dispatches_all_operations_with_frames_and_grants() {
    let format = format();
    let device_id = DeviceId::new("speaker").expect("device id");
    let grant = DeviceGrant::new("grant-1", device_id.clone(), DeviceDirection::Output)
        .expect("device grant");
    let speech_chunk = SynthesizedAudioChunk::new(
        RequestId::new("provider-request").expect("request id"),
        StreamId::new("provider-stream").expect("stream id"),
        0,
        format,
        vec![1, 2, 3, 4],
        true,
    )
    .expect("speech chunk");
    let talk_chunk = SynthesizedAudioChunk::new(
        RequestId::new("talk-request").expect("request id"),
        StreamId::new("provider-stream").expect("stream id"),
        0,
        format,
        vec![1, 2, 3, 4],
        true,
    )
    .expect("talk chunk");
    let report = DoctorReport::ready(ProviderFeatures::audio_and_text(), 1).expect("report");
    let devices = EnumeratedDevices::new(vec![
        yunxi_voice::DeviceInfo::new(
            device_id.clone(),
            "External speaker",
            DeviceDirection::Output,
            DeviceAvailability::Available,
            vec![format],
        )
        .expect("device"),
    ])
    .expect("devices");
    let transport = ScriptedSidecarTransport::new([
        Ok(SidecarResponse::Doctor(report)),
        Ok(SidecarResponse::Devices(devices)),
        Ok(SidecarResponse::Transcripts(vec![
            TranscribeEvent::Transcript(
                yunxi_voice::TranscriptEvent::final_text(
                    StreamId::new("provider-stream").expect("stream id"),
                    0,
                    "sidecar transcript",
                )
                .expect("transcript"),
            ),
        ])),
        Ok(SidecarResponse::Speech(vec![
            yunxi_voice::SynthesisEvent::Audio(speech_chunk.clone()),
        ])),
        Ok(SidecarResponse::Chat(vec![
            ChatEvent::new(ChatEventKind::Final, "sidecar chat").expect("chat"),
        ])),
        Ok(SidecarResponse::Talk(vec![yunxi_voice::TalkEvent::Audio(
            talk_chunk,
        )])),
        Ok(SidecarResponse::Playback(yunxi_voice::OutputResult {
            chunks: 1,
            bytes: 4,
        })),
        Ok(SidecarResponse::Saved(yunxi_voice::OutputResult {
            chunks: 1,
            bytes: 4,
        })),
    ]);
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", true, true, true)
            .expect("descriptor"),
        ProviderFeatures::audio_and_text(),
        transport,
    )
    .expect("provider");

    assert_eq!(
        provider
            .doctor(&OperationContext::new())
            .expect("doctor")
            .status,
        DoctorStatus::Ready
    );
    assert_eq!(
        provider
            .enumerate_devices(&OperationContext::new())
            .expect("devices")
            .devices
            .len(),
        1
    );

    let transcription = request();
    let mut input = AudioChunkIterator::new(&transcription).expect("input");
    let mut transcripts = VecTranscriptSink::new();
    assert_eq!(
        provider
            .transcribe(
                &transcription,
                &mut input,
                &mut transcripts,
                &OperationContext::new(),
            )
            .expect("transcribe"),
        ProviderOutcome::Completed
    );

    let mut audio = VecSynthesizedAudioSink::new();
    assert_eq!(
        provider
            .speak(&synthesis_request(), &mut audio, &OperationContext::new())
            .expect("speak"),
        ProviderOutcome::Completed
    );

    let chat_request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut chat = VecChatSink::new();
    provider
        .chat(&chat_request, &mut chat, &OperationContext::new())
        .expect("chat");

    let talk_request = TalkRequest::new(
        RequestId::new("talk-request").expect("request id"),
        request(),
        format,
        StreamStatus::new(),
    )
    .expect("talk request")
    .with_input_device_grant(
        DeviceGrant::new("input-grant", device_id.clone(), DeviceDirection::Input)
            .expect("input grant"),
    );
    let mut talk = VecTalkSink::new();
    provider
        .talk(&talk_request, &mut talk, &OperationContext::new())
        .expect("talk");

    let playback_request = output_request(OutputSelection::Playback(device_id));
    let playback_request = playback_request.with_device_grant(grant);
    let mut playback_source = VecSynthesizedAudioSource::new([speech_chunk.clone()]);
    provider
        .playback(
            &playback_request,
            &mut playback_source,
            &OperationContext::new(),
        )
        .expect("playback");

    let save_request = output_request(OutputSelection::Save(
        yunxi_voice::SaveDestinationId::new("fixture").expect("save destination"),
    ));
    let mut save_source = VecSynthesizedAudioSource::new([speech_chunk]);
    assert_eq!(
        provider
            .save(&save_request, &mut save_source, &OperationContext::new())
            .expect("save"),
        yunxi_voice::OutputResult {
            chunks: 1,
            bytes: 4,
        }
    );
    assert_eq!(provider.transport_mut().request_count(), 8);
}

#[test]
fn sidecar_response_audio_is_bound_to_the_active_request_and_resets_transport() {
    let wrong_chunk = SynthesizedAudioChunk::new(
        RequestId::new("other-request").expect("request id"),
        StreamId::new("other-stream").expect("stream id"),
        0,
        format(),
        vec![1, 2, 3, 4],
        true,
    )
    .expect("wrong chunk");
    let transport = ScriptedSidecarTransport::new([Ok(SidecarResponse::Speech(vec![
        yunxi_voice::SynthesisEvent::Audio(wrong_chunk),
    ]))]);
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", false, false, true)
            .expect("descriptor"),
        ProviderFeatures {
            doctor: false,
            device_enumeration: false,
            transcribe: false,
            speak: true,
            chat: false,
            talk: false,
            playback: false,
            save: false,
        },
        transport,
    )
    .expect("provider");
    let mut sink = VecSynthesizedAudioSink::new();
    assert!(matches!(
        provider.speak(&synthesis_request(), &mut sink, &OperationContext::new()),
        Err(VoiceProviderError::Contract(
            yunxi_voice::VoiceContractError::MixedStream
        ))
    ));
    assert!(sink.chunks().is_empty());
    assert_eq!(provider.transport_mut().reset_count(), 1);
}

#[test]
fn sidecar_input_budget_is_enforced_before_more_chunks_are_read() {
    let stream_id = StreamId::new("provider-stream").expect("stream id");
    let chunks = (0..6)
        .map(|sequence| {
            AudioChunk::new(
                stream_id.clone(),
                sequence,
                format(),
                vec![0; yunxi_voice::MAX_AUDIO_CHUNK_BYTES],
                sequence == 5,
            )
            .expect("bounded input chunk")
        })
        .collect();
    let mut source = CountingAudioSource { chunks, pulls: 0 };
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", false, true, false)
            .expect("descriptor"),
        ProviderFeatures {
            doctor: false,
            device_enumeration: false,
            transcribe: true,
            speak: false,
            chat: false,
            talk: false,
            playback: false,
            save: false,
        },
        ScriptedSidecarTransport::new(std::iter::empty::<
            Result<SidecarResponse, VoiceProviderError>,
        >()),
    )
    .expect("provider");
    let mut sink = VecTranscriptSink::new();
    assert!(matches!(
        provider.transcribe(
            &request(),
            &mut source,
            &mut sink,
            &OperationContext::new(),
        ),
        Err(VoiceProviderError::ProviderFailure { code, retryable: false })
            if code == "payload_too_large"
    ));
    assert_eq!(source.pulls, 5);
    assert_eq!(provider.transport_mut().request_count(), 0);
}

#[test]
fn external_output_requires_matching_device_grant_and_selection() {
    let device_id = DeviceId::new("speaker").expect("device id");
    let transport =
        ScriptedSidecarTransport::new([Ok(SidecarResponse::Playback(yunxi_voice::OutputResult {
            chunks: 0,
            bytes: 0,
        }))]);
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", true, true, true)
            .expect("descriptor"),
        ProviderFeatures::audio_and_text(),
        transport,
    )
    .expect("provider");
    let request = output_request(OutputSelection::Playback(device_id.clone()));
    let mut source = VecSynthesizedAudioSource::new([]);
    assert!(matches!(
        provider.playback(&request, &mut source, &OperationContext::new()),
        Err(VoiceProviderError::ProviderFailure { code, retryable: false }) if code == "device_grant_required"
    ));
    assert_eq!(provider.transport_mut().request_count(), 0);

    let wrong_grant = DeviceGrant::new(
        "grant-1",
        DeviceId::new("other-speaker").expect("device id"),
        DeviceDirection::Output,
    )
    .expect("wrong grant");
    let request = request.with_device_grant(wrong_grant);
    let mut source = VecSynthesizedAudioSource::new([]);
    assert!(matches!(
        provider.playback(&request, &mut source, &OperationContext::new()),
        Err(VoiceProviderError::ProviderFailure { code, retryable: false }) if code == "device_grant_mismatch"
    ));
    assert_eq!(provider.transport_mut().request_count(), 0);
}

#[test]
fn feature_bits_and_context_stop_external_work_before_transport() {
    let transport = ScriptedSidecarTransport::new([Ok(SidecarResponse::Chat(vec![
        ChatEvent::new(ChatEventKind::Final, "should not be used").expect("chat"),
    ]))]);
    let mut provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", false, false, false)
            .expect("descriptor"),
        ProviderFeatures {
            doctor: true,
            device_enumeration: false,
            transcribe: false,
            speak: false,
            chat: false,
            talk: false,
            playback: false,
            save: false,
        },
        transport,
    )
    .expect("provider");
    let request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut sink = VecChatSink::new();
    assert!(matches!(
        provider.chat(&request, &mut sink, &OperationContext::new()),
        Err(VoiceProviderError::ProviderFailure { code, retryable: false }) if code == "unsupported_operation"
    ));
    assert!(sink.events().is_empty());
    assert_eq!(provider.transport_mut().request_count(), 0);

    let cancelled = OperationContext::new();
    cancelled.cancel();
    assert!(matches!(
        provider.doctor(&cancelled),
        Err(VoiceProviderError::Cancelled)
    ));
    assert_eq!(provider.transport_mut().request_count(), 0);
}

#[test]
fn unavailable_external_audio_falls_back_while_chat_stays_available() {
    let transport = ScriptedSidecarTransport::new(std::iter::empty::<
        Result<SidecarResponse, VoiceProviderError>,
    >());
    let provider = ExternalSidecarProvider::new(
        ProviderDescriptor::new("external.voice", "External voice", true, true, true)
            .expect("descriptor"),
        ProviderFeatures::audio_and_text(),
        transport,
    )
    .expect("provider");
    let fallback = TextFallbackProvider::new("typed fallback").expect("fallback");
    let chat = MockChatProvider::new("chat remains available").expect("chat");
    let mut router = VoiceRouter::new(provider, fallback, chat);
    let request = request();
    let mut source = AudioChunkIterator::new(&request).expect("source");
    let mut transcript = VecTranscriptSink::new();
    assert_eq!(
        router
            .transcribe(
                &request,
                &mut source,
                &mut transcript,
                &OperationContext::new(),
            )
            .expect("fallback transcription"),
        ProviderOutcome::TextFallback
    );

    let chat_request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut chat_events = VecChatSink::new();
    router
        .chat(&chat_request, &mut chat_events, &OperationContext::new())
        .expect("chat");
    assert_eq!(chat_events.events()[0].text, "chat remains available");
}

#[test]
fn router_disables_restarts_and_falls_back_without_blocking_text_chat() {
    let provider =
        LoopbackVoiceProvider::new(vec![chunk(0, true)], "partial", "final", "voice chat")
            .expect("loopback provider");
    let fallback = TextFallbackProvider::new("typed fallback").expect("fallback");
    let chat = MockChatProvider::new("text chat remains available").expect("chat");
    let mut router = VoiceRouter::new(provider, fallback, chat);
    router.disable();

    let request = request();
    let mut source = AudioChunkIterator::new(&request).expect("source");
    let mut transcripts = VecTranscriptSink::new();
    assert_eq!(
        router
            .transcribe(
                &request,
                &mut source,
                &mut transcripts,
                &OperationContext::new()
            )
            .expect("text fallback"),
        ProviderOutcome::TextFallback
    );

    let chat_request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let mut chat_events = VecChatSink::new();
    router
        .chat(&chat_request, &mut chat_events, &OperationContext::new())
        .expect("text chat");
    assert_eq!(chat_events.events()[0].text, "text chat remains available");

    router.restart();
    assert_eq!(router.generation(), 1);
    assert_eq!(
        router.doctor(&OperationContext::new()).status,
        DoctorStatus::Ready
    );
}
