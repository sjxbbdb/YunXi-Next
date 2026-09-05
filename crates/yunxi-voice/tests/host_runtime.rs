use std::time::Duration;

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{CapabilityDescriptor, capabilities};
use yunxi_voice::{
    TRANSCRIBE_OPERATION, TranscribeEvent, VOICE_FIXTURE_PLUGIN_ID, transcribe_fixture,
};

#[test]
fn voice_fixture_is_routed_through_the_process_host() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    let command = PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-voice-fixture"));

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
fn voice_plugin_rejects_bad_payload_without_losing_its_route() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).expect("voice plugin id");
    let transcribe = CapabilityDescriptor::new(capabilities::VOICE_TRANSCRIBE, 1)
        .expect("transcribe capability");
    let synthesize = CapabilityDescriptor::new(capabilities::VOICE_SYNTHESIZE, 1)
        .expect("synthesize capability");
    host.launch(
        PluginLaunch::new(
            plugin_id,
            PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-voice-fixture")),
        )
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
