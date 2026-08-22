//! Black-box process and protocol isolation tests for the capability host.

use std::env;
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use yunxi_kernel::{KernelState, PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CapabilityDescriptor, HostMessage, InvocationResponse, PluginMessage, connect_plugin,
};

const TEST_MODE_ENV: &str = "YUNXI_HOST_TEST_PLUGIN_MODE";
const TEST_ID_ENV: &str = "YUNXI_HOST_TEST_PLUGIN_ID";
const TEST_CAPABILITY_ENV: &str = "YUNXI_HOST_TEST_CAPABILITY";

#[test]
fn plugin_subprocess_entrypoint() {
    let Ok(mode) = env::var(TEST_MODE_ENV) else {
        return;
    };
    let id = env::var(TEST_ID_ENV).expect("fixture plugin id");
    let capability_id = env::var(TEST_CAPABILITY_ENV).expect("fixture capability id");
    let capability = CapabilityDescriptor::new(&capability_id, 1).expect("fixture capability");
    let mut session = connect_plugin(
        id,
        "Host integration fixture",
        "1.0.0",
        vec![capability],
        Duration::from_secs(2),
    )
    .expect("fixture handshake");

    if mode == "crash" {
        process::exit(42);
    }
    loop {
        match session.receive().expect("receive host message") {
            HostMessage::Invoke { request } => {
                let payload = request
                    .decode_payload::<String>()
                    .expect("decode fixture payload");
                let response = InvocationResponse::encode(request.request_id(), &payload)
                    .expect("encode fixture response");
                session
                    .send(&PluginMessage::InvocationCompleted { response })
                    .expect("send fixture response");
            }
            HostMessage::Shutdown => return,
            HostMessage::Welcome { .. } => panic!("unexpected second welcome"),
        }
    }
}

#[test]
fn a_crashed_provider_is_removed_while_its_sibling_keeps_serving() {
    let mut host = ProcessPluginHost::new();
    let healthy_id = plugin_id("yunxi.test.healthy");
    let crashing_id = plugin_id("yunxi.test.crashing");
    let healthy_capability = CapabilityDescriptor::new("fixture.echo", 1).expect("capability");
    let crashing_capability = CapabilityDescriptor::new("fixture.crash", 1).expect("capability");

    host.launch(fixture_launch(
        healthy_id.clone(),
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy plugin");
    host.launch(fixture_launch(
        crashing_id,
        "crash",
        crashing_capability.id().as_str(),
    ))
    .expect("launch crashing plugin");

    let error = host
        .invoke::<_, String>(&crashing_capability, "run", &"crash".to_string())
        .expect_err("crashed plugin call must fail");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(host.catalog().providers("fixture.crash", 1).is_empty());

    let reply = host
        .invoke::<_, String>(&healthy_capability, "run", &"still healthy".to_string())
        .expect("healthy sibling still serves");
    assert_eq!(reply, "still healthy");
    assert_eq!(host.kernel_state(), KernelState::Running);
    assert_eq!(host.connection_count(), 1);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = host.snapshot();
        if snapshot.failed_plugin_count() == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "crash was not observed by kernel"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        host.catalog()
            .providers(healthy_capability.id().as_str(), 1)
            .len(),
        1
    );
}

fn fixture_launch(id: PluginId, mode: &str, capability: &str) -> PluginLaunch {
    let executable = env::current_exe().expect("integration test executable");
    let command = PluginCommand::new(executable)
        .args(["--exact", "plugin_subprocess_entrypoint", "--nocapture"])
        .env(TEST_MODE_ENV, mode)
        .env(TEST_ID_ENV, id.as_str())
        .env(TEST_CAPABILITY_ENV, capability);
    PluginLaunch::new(id, command)
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
}

fn plugin_id(value: &str) -> PluginId {
    PluginId::new(value).expect("valid fixture plugin id")
}
