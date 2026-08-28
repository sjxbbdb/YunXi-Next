//! Black-box process and protocol isolation tests for the capability host.

use std::env;
use std::io::Write;
use std::net::TcpStream;
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use yunxi_kernel::{KernelState, PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CapabilityDescriptor, GrantKind, GrantRequirement, HostMessage,
    InvocationResponse, PluginMessage, connect_plugin, connect_plugin_with_grants,
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
    if mode == "malformed" {
        let address = env::var(CONNECT_ADDRESS_ENV)
            .expect("fixture connection address")
            .parse::<std::net::SocketAddr>()
            .expect("fixture socket address");
        let mut stream = TcpStream::connect(address).expect("connect malformed fixture");
        stream
            .write_all(b"{malformed fixture frame\n")
            .expect("write malformed fixture frame");
        return;
    }

    let grants = match mode.as_str() {
        "manifest" => vec![GrantRequirement::required(GrantKind::Network)],
        "missing-grant" => vec![GrantRequirement::required(GrantKind::WorkspaceRead)],
        _ => Vec::new(),
    };
    let mut session = if grants.is_empty() {
        connect_plugin(
            id,
            "Host integration fixture",
            "1.0.0",
            vec![capability],
            Duration::from_secs(2),
        )
    } else {
        connect_plugin_with_grants(
            id,
            "Host integration fixture",
            "1.0.0",
            vec![capability],
            grants,
            Duration::from_secs(2),
        )
    }
    .expect("fixture handshake");

    if mode == "crash" {
        process::exit(42);
    }
    if mode == "hang" {
        thread::sleep(Duration::from_secs(60));
        return;
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
fn required_manifest_grants_are_recorded_and_accepted() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.manifest");
    let capability = CapabilityDescriptor::new("fixture.manifest", 1).expect("capability");

    host.launch(
        fixture_launch(id.clone(), "manifest", capability.id().as_str())
            .with_required_grants([GrantKind::Network]),
    )
    .expect("manifest plugin launch");

    let record = host.catalog().plugin(&id).expect("manifest catalog record");
    let manifest = record.manifest().expect("manifest is retained in catalog");
    assert!(manifest.declares_required_grant(GrantKind::Network));
    assert_eq!(manifest.plugin_id(), id.as_str());
    assert_eq!(manifest.capabilities(), &[capability]);
}

#[test]
fn missing_required_manifest_grant_is_rejected_before_route_registration() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.missing-grant");
    let capability = CapabilityDescriptor::new("fixture.missing-grant", 1).expect("capability");

    let error = host
        .launch(
            fixture_launch(id.clone(), "missing-grant", capability.id().as_str())
                .with_required_grants([GrantKind::Network]),
        )
        .expect_err("missing required grant must fail launch");
    assert!(error.to_string().contains("required grant `network`"));
    assert!(host.catalog().plugin(&id).is_none());
    assert!(
        host.catalog()
            .providers("fixture.missing-grant", 1)
            .is_empty()
    );
    assert_eq!(host.snapshot().failed_plugin_count(), 1);
    assert_eq!(host.kernel_state(), KernelState::Running);
}

#[test]
fn malformed_plugin_frame_isolated_from_a_healthy_sibling() {
    let mut host = ProcessPluginHost::new();
    let healthy_id = plugin_id("yunxi.test.healthy-frame");
    let malformed_id = plugin_id("yunxi.test.malformed-frame");
    let healthy_capability =
        CapabilityDescriptor::new("fixture.healthy-frame", 1).expect("healthy capability");
    let malformed_capability =
        CapabilityDescriptor::new("fixture.malformed-frame", 1).expect("malformed capability");

    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy plugin");
    let error = host
        .launch(fixture_launch(
            malformed_id,
            "malformed",
            malformed_capability.id().as_str(),
        ))
        .expect_err("malformed frame must fail launch");
    assert!(
        error
            .to_string()
            .contains("failed to decode protocol message")
    );
    let reply = host
        .invoke::<_, String>(&healthy_capability, "run", &"frame-safe".to_string())
        .expect("healthy sibling still serves");
    assert_eq!(reply, "frame-safe");
    assert_eq!(host.kernel_state(), KernelState::Running);
}

#[test]
fn timed_out_plugin_is_removed_while_a_healthy_sibling_keeps_serving() {
    let mut host = ProcessPluginHost::new();
    let healthy_id = plugin_id("yunxi.test.healthy-timeout");
    let hanging_id = plugin_id("yunxi.test.hanging");
    let healthy_capability =
        CapabilityDescriptor::new("fixture.healthy-timeout", 1).expect("healthy capability");
    let hanging_capability = CapabilityDescriptor::new("fixture.hanging", 1).expect("capability");

    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy plugin");
    host.launch(fixture_launch(
        hanging_id,
        "hang",
        hanging_capability.id().as_str(),
    ))
    .expect("launch hanging plugin");

    let error = host
        .invoke::<_, String>(&hanging_capability, "run", &"wait".to_string())
        .expect_err("hanging plugin must time out");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    let reply = host
        .invoke::<_, String>(&healthy_capability, "run", &"timeout-safe".to_string())
        .expect("healthy sibling still serves");
    assert_eq!(reply, "timeout-safe");
    assert_eq!(host.connection_count(), 1);
    assert_eq!(host.kernel_state(), KernelState::Running);
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
