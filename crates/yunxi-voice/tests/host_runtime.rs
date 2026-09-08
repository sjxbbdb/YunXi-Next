use std::time::Duration;

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{CapabilityDescriptor, GrantKind, capabilities};
use yunxi_voice::{
    AudioOutputPayload, AudioOutputRequest, CANCEL_OPERATION, ChatEvent, ChatRequest,
    DeviceDirection, DeviceEnumerationRequest, DeviceGrant, DeviceId, DoctorRequest, OutputResult,
    OutputSelection, PLAYBACK_OPERATION, RequestId, SAVE_OPERATION, SPEAK_OPERATION,
    SYNTHESIZE_OPERATION, StreamStatus, TRANSCRIBE_OPERATION, TalkEvent, TalkRequest,
    TranscribeEvent, VOICE_FIXTURE_PLUGIN_ID, synthesize_fixture, transcribe_fixture,
};

fn isolated_plugin_command() -> PluginCommand {
    let mut command =
        PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-voice-fixture")).clear_environment();
    for name in [
        "PATH",
        "Path",
        "PATHEXT",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command = command.env(name, value);
        }
    }
    command
}

#[test]
fn voice_fixture_is_routed_through_the_process_host() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    let command = isolated_plugin_command();

    host.launch(
        PluginLaunch::new(plugin_id.clone(), command)
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_expected_capabilities([transcribe.clone(), synthesize]),
    )
    .expect("voice fixture launch");

    let fixture = transcribe_fixture().expect("transcription fixture");
    let events: Vec<TranscribeEvent> = host
        .invoke(&transcribe, TRANSCRIBE_OPERATION, &fixture.request)
        .expect("transcription route");
    assert!(events.iter().any(|event| {
        matches!(event, TranscribeEvent::Transcript(transcript) if transcript.text == "fixture final")
    }));

    host.disable(&plugin_id).expect("disable voice fixture");
    assert!(
        host.catalog()
            .providers(capabilities::VOICE_TRANSCRIBE, 1)
            .is_empty()
    );
}

#[test]
fn voice_fixture_process_host_exposes_every_bounded_operation() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    host.launch(
        PluginLaunch::new(plugin_id, isolated_plugin_command())
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_expected_capabilities([transcribe.clone(), synthesize.clone()]),
    )
    .expect("voice fixture launch");

    let doctor: yunxi_voice::DoctorReport = host
        .invoke(
            &transcribe,
            yunxi_voice::DOCTOR_OPERATION,
            &DoctorRequest::new(RequestId::new("doctor").expect("request id")),
        )
        .expect("doctor route");
    assert_eq!(doctor.status, yunxi_voice::DoctorStatus::Ready);

    let devices: yunxi_voice::EnumeratedDevices = host
        .invoke(
            &transcribe,
            yunxi_voice::DEVICE_ENUMERATION_OPERATION,
            &DeviceEnumerationRequest {
                request_id: RequestId::new("devices").expect("request id"),
            },
        )
        .expect("devices route");
    assert_eq!(devices.devices.len(), 1);
    let devices_alias: yunxi_voice::EnumeratedDevices = host
        .invoke(
            &transcribe,
            yunxi_voice::DEVICES_OPERATION,
            &DeviceEnumerationRequest {
                request_id: RequestId::new("devices-alias").expect("request id"),
            },
        )
        .expect("devices alias route");
    assert_eq!(devices_alias.devices.len(), 1);

    let chat_request = ChatRequest::new(
        RequestId::new("chat-request").expect("request id"),
        "conversation",
        "hello",
    )
    .expect("chat request");
    let chat: Vec<ChatEvent> = host
        .invoke(&transcribe, yunxi_voice::CHAT_OPERATION, &chat_request)
        .expect("chat route");
    assert_eq!(chat.len(), 1);

    let transcription = transcribe_fixture().expect("transcription fixture");
    let talk_request = TalkRequest::new(
        RequestId::new("talk-request").expect("request id"),
        transcription.request,
        yunxi_voice::AudioFormat::new(yunxi_voice::AudioCodec::PcmS16Le, 16_000, 1)
            .expect("output format"),
        StreamStatus::new(),
    )
    .expect("talk request")
    .with_input_device_grant(
        DeviceGrant::new(
            "input-grant",
            DeviceId::new("loopback-default").expect("device id"),
            DeviceDirection::Duplex,
        )
        .expect("input grant"),
    );
    let talk: Vec<TalkEvent> = host
        .invoke(&transcribe, yunxi_voice::TALK_OPERATION, &talk_request)
        .expect("talk route");
    assert!(
        talk.iter()
            .any(|event| matches!(event, TalkEvent::Audio(_)))
    );

    let synthesis = synthesize_fixture().expect("synthesis fixture");
    let speech: Vec<yunxi_voice::SynthesisEvent> = host
        .invoke(&synthesize, SYNTHESIZE_OPERATION, &synthesis.request)
        .expect("synthesis route");
    assert!(
        speech
            .iter()
            .any(|event| matches!(event, yunxi_voice::SynthesisEvent::Audio(_)))
    );
    let speech_alias: Vec<yunxi_voice::SynthesisEvent> = host
        .invoke(&synthesize, SPEAK_OPERATION, &synthesis.request)
        .expect("speak alias route");
    assert_eq!(speech_alias.len(), speech.len());

    let device = DeviceId::new("loopback-default").expect("device id");
    let playback_request = AudioOutputRequest::new(
        synthesis.request.request_id.clone(),
        synthesis.request.stream_id.clone(),
        synthesis.request.format,
        OutputSelection::Playback(device.clone()),
        StreamStatus::new(),
    )
    .expect("playback request")
    .with_device_grant(
        DeviceGrant::new("output-grant", device, DeviceDirection::Output).expect("output grant"),
    );
    let playback: OutputResult = host
        .invoke(
            &synthesize,
            PLAYBACK_OPERATION,
            &AudioOutputPayload {
                request: playback_request,
                chunks: synthesis
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        yunxi_voice::SynthesisEvent::Audio(chunk) => Some(chunk.clone()),
                        yunxi_voice::SynthesisEvent::Status(_) => None,
                    })
                    .collect(),
            },
        )
        .expect("playback route");
    assert!(playback.chunks > 0);

    let save_request = AudioOutputRequest::new(
        synthesis.request.request_id,
        synthesis.request.stream_id,
        synthesis.request.format,
        OutputSelection::Save(
            yunxi_voice::SaveDestinationId::new("fixture-save").expect("destination"),
        ),
        StreamStatus::new(),
    )
    .expect("save request");
    let saved: OutputResult = host
        .invoke(
            &synthesize,
            SAVE_OPERATION,
            &AudioOutputPayload {
                request: save_request,
                chunks: synthesis
                    .events
                    .into_iter()
                    .filter_map(|event| match event {
                        yunxi_voice::SynthesisEvent::Audio(chunk) => Some(chunk),
                        yunxi_voice::SynthesisEvent::Status(_) => None,
                    })
                    .collect(),
            },
        )
        .expect("save route");
    assert!(saved.chunks > 0);

    let cancel = yunxi_voice::CancelRequest::new(
        yunxi_voice::StreamId::new("cancel-stream").expect("stream id"),
        "test cancellation",
    )
    .expect("cancel request");
    let _: yunxi_voice::CancelResult = host
        .invoke(&transcribe, CANCEL_OPERATION, &cancel)
        .expect("transcribe cancel route");
    let _: yunxi_voice::CancelResult = host
        .invoke(&synthesize, CANCEL_OPERATION, &cancel)
        .expect("synthesize cancel route");
}

#[test]
fn voice_plugin_rejects_bad_payload_without_losing_its_route() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    host.launch(
        PluginLaunch::new(plugin_id, isolated_plugin_command())
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_expected_capabilities([transcribe.clone(), synthesize]),
    )
    .expect("voice fixture launch");

    let error = host
        .invoke::<_, serde_json::Value>(
            &transcribe,
            TRANSCRIBE_OPERATION,
            &serde_json::json!({"chunks": []}),
        )
        .expect_err("bad payload must be rejected");
    assert!(
        matches!(error, PluginCallError::Rejected { ref code, .. } if code == "invalid_request")
    );
    assert_eq!(host.connection_count(), 1);
}

#[test]
fn configured_sidecar_is_selected_through_the_voice_plugin_boundary() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    let command = isolated_plugin_command().env(
        "YUNXI_VOICE_SIDECAR_PROGRAM",
        env!("CARGO_BIN_EXE_yunxi-voice-sidecar-fixture"),
    );

    host.launch(
        PluginLaunch::new(plugin_id, command)
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_expected_capabilities([transcribe.clone(), synthesize])
            .with_required_grants([GrantKind::Device]),
    )
    .expect("voice sidecar plugin launch");

    let fixture = transcribe_fixture().expect("transcription fixture");
    let events: Vec<TranscribeEvent> = host
        .invoke(&transcribe, TRANSCRIBE_OPERATION, &fixture.request)
        .expect("sidecar transcription route");
    assert!(events.iter().any(|event| {
        matches!(event, TranscribeEvent::Transcript(transcript) if transcript.text == "sidecar final")
    }));

    let doctor: yunxi_voice::DoctorReport = host
        .invoke(
            &transcribe,
            yunxi_voice::DOCTOR_OPERATION,
            &DoctorRequest::new(RequestId::new("sidecar-doctor").expect("request id")),
        )
        .expect("sidecar doctor route");
    assert_eq!(doctor.status, yunxi_voice::DoctorStatus::Ready);

    let devices: yunxi_voice::EnumeratedDevices = host
        .invoke(
            &transcribe,
            yunxi_voice::DEVICE_ENUMERATION_OPERATION,
            &DeviceEnumerationRequest {
                request_id: RequestId::new("sidecar-devices").expect("request id"),
            },
        )
        .expect("sidecar devices route");
    assert_eq!(devices.devices[0].id.as_str(), "sidecar-default");

    let chat: Vec<ChatEvent> = host
        .invoke(
            &transcribe,
            yunxi_voice::CHAT_OPERATION,
            &ChatRequest::new(
                RequestId::new("sidecar-chat").expect("request id"),
                "conversation",
                "hello",
            )
            .expect("chat request"),
        )
        .expect("sidecar chat route");
    assert_eq!(chat[0].text, "sidecar chat");
}

#[test]
fn unavailable_configured_sidecar_falls_back_without_losing_the_route() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    let command = isolated_plugin_command().env(
        "YUNXI_VOICE_SIDECAR_PROGRAM",
        "missing-yunxi-voice-sidecar-for-fallback-test.exe",
    );

    host.launch(
        PluginLaunch::new(plugin_id, command)
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_expected_capabilities([transcribe.clone(), synthesize])
            .with_required_grants([GrantKind::Device]),
    )
    .expect("voice fallback plugin launch");

    let fixture = transcribe_fixture().expect("transcription fixture");
    let events: Vec<TranscribeEvent> = host
        .invoke(&transcribe, TRANSCRIBE_OPERATION, &fixture.request)
        .expect("fallback transcription route");
    assert!(events.iter().any(|event| {
        matches!(event, TranscribeEvent::Transcript(transcript) if transcript.text == "fixture final")
    }));
    assert_eq!(host.connection_count(), 1);
}
