use std::process::{Child, Command};
use std::time::Duration;

use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, HostMessage, InvocationRequest, InvocationResponse,
    PluginAcceptor, PluginMessage, capabilities,
};
use yunxi_voice::{
    CANCEL_OPERATION, CancelRequest, CancelResult, SYNTHESIZE_OPERATION, SynthesisEvent,
    TRANSCRIBE_OPERATION, TranscribeEvent, VOICE_FIXTURE_PLUGIN_ID, synthesize_fixture,
    transcribe_fixture,
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn fixture_process_completes_handshake_and_dispatch_boundaries() {
    let acceptor = PluginAcceptor::bind().expect("bind loopback acceptor");
    let address = acceptor.address().expect("read loopback address");
    let token = acceptor.connection_token().to_string();
    let child = Command::new(env!("CARGO_BIN_EXE_yunxi-voice-fixture"))
        .env(CONNECT_ADDRESS_ENV, address.to_string())
        .env(CONNECT_TOKEN_ENV, token)
        .spawn()
        .expect("start voice fixture");
    let mut child = ChildGuard(child);

    let mut session = acceptor
        .accept(VOICE_FIXTURE_PLUGIN_ID, Duration::from_secs(2))
        .expect("fixture handshake");
    session
        .set_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
        .expect("set fixture test timeouts");
    assert_eq!(session.info().plugin_id(), VOICE_FIXTURE_PLUGIN_ID);
    assert!(session.info().supports(capabilities::VOICE_TRANSCRIBE, 1));
    assert!(session.info().supports(capabilities::VOICE_SYNTHESIZE, 1));
    let runtime = session
        .info()
        .manifest()
        .expect("fixture announces a manifest")
        .runtime_metadata()
        .expect("fixture announces runtime metadata");
    assert_eq!(runtime.host_group(), "voice-fixture");
    assert!(runtime.default_enabled());

    let transcription = transcribe_fixture().expect("transcription fixture");
    let transcribe_request = InvocationRequest::encode(
        1,
        yunxi_protocol::CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
            .expect("transcribe capability"),
        TRANSCRIBE_OPERATION,
        &transcription.request,
    )
    .expect("encode transcribe invocation");
    session
        .send(&HostMessage::Invoke {
            request: transcribe_request,
        })
        .expect("send transcribe invocation");
    let response = receive_completed(&mut session);
    let events: Vec<TranscribeEvent> = response.decode_payload().expect("decode transcripts");
    assert!(events.iter().any(|event| {
        matches!(event, TranscribeEvent::Transcript(transcript) if transcript.text == "fixture final")
    }));

    let synthesis = synthesize_fixture().expect("synthesis fixture");
    let synthesize_request = InvocationRequest::encode(
        2,
        yunxi_protocol::CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
            .expect("synthesize capability"),
        SYNTHESIZE_OPERATION,
        &synthesis.request,
    )
    .expect("encode synthesize invocation");
    session
        .send(&HostMessage::Invoke {
            request: synthesize_request,
        })
        .expect("send synthesize invocation");
    let response = receive_completed(&mut session);
    let events: Vec<SynthesisEvent> = response.decode_payload().expect("decode audio");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SynthesisEvent::Audio(_)))
            .count(),
        2
    );

    let cancel = CancelRequest::new(
        synthesis.request.stream_id.clone(),
        "fixture test cancellation",
    )
    .expect("cancel request");
    let cancel_request = InvocationRequest::encode(
        3,
        yunxi_protocol::CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
            .expect("synthesize capability"),
        CANCEL_OPERATION,
        &cancel,
    )
    .expect("encode cancel invocation");
    session
        .send(&HostMessage::Invoke {
            request: cancel_request,
        })
        .expect("send cancel invocation");
    let response = receive_completed(&mut session);
    let result: CancelResult = response.decode_payload().expect("decode cancellation");
    assert_eq!(
        result.status.cancellation,
        yunxi_voice::CancellationState::Cancelled
    );

    let invalid_request = InvocationRequest::encode(
        4,
        yunxi_protocol::CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
            .expect("transcribe capability"),
        TRANSCRIBE_OPERATION,
        &serde_json::json!({"chunks": []}),
    )
    .expect("encode invalid invocation");
    session
        .send(&HostMessage::Invoke {
            request: invalid_request,
        })
        .expect("send invalid invocation");
    match session.receive().expect("receive invalid request failure") {
        PluginMessage::InvocationFailed {
            request_id,
            code,
            retryable,
            ..
        } => {
            assert_eq!(request_id, 4);
            assert_eq!(code, "invalid_request");
            assert!(!retryable);
        }
        message => panic!("expected invalid request failure, got {message:?}"),
    }

    let unsupported_request = InvocationRequest::encode(
        5,
        yunxi_protocol::CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
            .expect("transcribe capability"),
        "unknown",
        &serde_json::json!({}),
    )
    .expect("encode unsupported invocation");
    session
        .send(&HostMessage::Invoke {
            request: unsupported_request,
        })
        .expect("send unsupported invocation");
    match session
        .receive()
        .expect("receive unsupported operation failure")
    {
        PluginMessage::InvocationFailed {
            request_id,
            code,
            retryable,
            ..
        } => {
            assert_eq!(request_id, 5);
            assert_eq!(code, "unsupported_operation");
            assert!(!retryable);
        }
        message => panic!("expected unsupported operation failure, got {message:?}"),
    }

    session
        .send(&HostMessage::Shutdown)
        .expect("shutdown fixture");
    let status = child.0.wait().expect("wait for fixture");
    assert!(status.success());
}

fn receive_completed(session: &mut yunxi_protocol::HostPluginSession) -> InvocationResponse {
    match session.receive().expect("receive fixture response") {
        PluginMessage::InvocationCompleted { response } => response,
        message => panic!("expected completed invocation, got {message:?}"),
    }
}
