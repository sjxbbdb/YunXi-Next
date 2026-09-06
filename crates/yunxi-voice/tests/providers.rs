use std::time::Duration;

use yunxi_voice::{
    AudioChunk, AudioChunkIterator, AudioChunkQueue, AudioCodec, AudioFormat, Device,
    LoopbackProvider, MockSynthesizer, MockTranscriber, OperationContext, ProviderGate,
    ProviderOutcome, RequestId, StreamId, StreamStatus, SynthesisRequest, Synthesizer,
    TextFallback, TextFallbackProvider, TranscribeRequest, Transcriber, VecSynthesizedAudioSink,
    VecSynthesizedAudioSource, VecTranscriptSink, VoiceProviderError,
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
