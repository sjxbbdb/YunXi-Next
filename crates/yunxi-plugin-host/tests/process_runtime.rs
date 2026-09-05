//! Black-box process and protocol isolation tests for the capability host.

use std::env;
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yunxi_kernel::{KernelState, PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CapabilityDescriptor, GrantKind, GrantRequirement, HostMessage,
    InvocationResponse, PluginMessage, connect_plugin, connect_plugin_with_grants,
};

const TEST_MODE_ENV: &str = "YUNXI_HOST_TEST_PLUGIN_MODE";
const TEST_ID_ENV: &str = "YUNXI_HOST_TEST_PLUGIN_ID";
const TEST_CAPABILITY_ENV: &str = "YUNXI_HOST_TEST_CAPABILITY";
const TEST_STATE_ENV: &str = "YUNXI_HOST_TEST_PLUGIN_STATE";

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
    if mode == "toggle" {
        let healthy = env::var_os(TEST_STATE_ENV)
            .and_then(|path| fs::read_to_string(path).ok())
            .is_some_and(|state| state.trim() == "healthy");
        if !healthy {
            process::exit(42);
        }
    }
    if mode == "hang" {
        thread::sleep(Duration::from_secs(60));
        return;
    }
    loop {
        match session.receive().expect("receive host message") {
            HostMessage::Invoke { request } => {
                if mode == "drop-on-invoke" {
                    let healthy = env::var_os(TEST_STATE_ENV)
                        .and_then(|path| fs::read_to_string(path).ok())
                        .is_some_and(|state| state.trim() == "healthy");
                    if !healthy {
                        process::exit(43);
                    }
                }
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
fn launch_capability_contract_rejects_unexpected_declarations() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.capability-contract");
    let announced = CapabilityDescriptor::new("fixture.unexpected", 1).expect("capability");
    let expected = CapabilityDescriptor::new("fixture.expected", 1).expect("capability");

    let error = host
        .launch(
            fixture_launch(id.clone(), "healthy", announced.id().as_str())
                .with_expected_capabilities([expected]),
        )
        .expect_err("unexpected capability must fail the launch contract");

    assert!(error.to_string().contains("outside its launch contract"));
    assert!(host.catalog().plugin(&id).is_none());
    assert!(
        host.catalog()
            .providers(announced.id().as_str(), 1)
            .is_empty()
    );
    assert_eq!(host.snapshot().failed_plugin_count(), 1);
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

#[test]
fn fast_crashes_are_restarted_three_times_then_disabled() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.retry-exhaustion");
    let capability = CapabilityDescriptor::new("fixture.retry-exhaustion", 1).expect("capability");

    host.launch(fixture_launch(
        id.clone(),
        "crash",
        capability.id().as_str(),
    ))
    .expect("launch crashing plugin");

    let retry = wait_for_retry_exhaustion(&mut host, &id);
    assert!(!retry.enabled());
    assert!(retry.exhausted());
    assert_eq!(retry.automatic_restarts(), 3);
    assert_eq!(retry.max_automatic_restarts(), 3);
    assert_eq!(
        host.plugin(&id)
            .expect("failed plugin snapshot")
            .generation(),
        4,
        "one initial generation plus three automatic restarts"
    );
    assert!(
        host.catalog()
            .providers(capability.id().as_str(), 1)
            .is_empty()
    );

    // Repeated refreshes must not create another generation after exhaustion.
    for _ in 0..5 {
        host.refresh();
    }
    assert_eq!(
        host.plugin(&id)
            .expect("still registered plugin")
            .generation(),
        4
    );
}

#[test]
fn manual_enable_resets_an_exhausted_cycle_and_reconnects() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.manual-enable");
    let capability = CapabilityDescriptor::new("fixture.manual-enable", 1).expect("capability");
    let state_path = test_state_path("manual-enable");
    fs::write(&state_path, "crash").expect("write initial fixture state");

    host.launch(toggle_fixture_launch(
        id.clone(),
        capability.id().as_str(),
        &state_path,
    ))
    .expect("launch toggle plugin");
    let exhausted = wait_for_retry_exhaustion(&mut host, &id);
    assert_eq!(exhausted.automatic_restarts(), 3);

    fs::write(&state_path, "healthy").expect("make fixture healthy");
    host.enable(&id).expect("manual enable after exhaustion");

    let retry = host
        .retry_snapshot(&id)
        .expect("retry state after manual enable");
    assert!(retry.enabled());
    assert_eq!(retry.automatic_restarts(), 0);
    let reply = host
        .invoke::<_, String>(&capability, "run", &"manual-enable-ok".to_string())
        .expect("re-enabled plugin serves");
    assert_eq!(reply, "manual-enable-ok");
    assert!(fs::remove_file(&state_path).is_ok());
}

#[test]
fn manual_restart_rehandshakes_without_affecting_a_sibling() {
    let mut host = ProcessPluginHost::new();
    let first_id = plugin_id("yunxi.test.manual-restart");
    let sibling_id = plugin_id("yunxi.test.manual-restart-sibling");
    let first_capability =
        CapabilityDescriptor::new("fixture.manual-restart", 1).expect("capability");
    let sibling_capability =
        CapabilityDescriptor::new("fixture.manual-restart-sibling", 1).expect("capability");

    host.launch(fixture_launch(
        first_id.clone(),
        "healthy",
        first_capability.id().as_str(),
    ))
    .expect("launch restartable plugin");
    host.launch(fixture_launch(
        sibling_id,
        "healthy",
        sibling_capability.id().as_str(),
    ))
    .expect("launch healthy sibling");
    let before = host
        .plugin(&first_id)
        .expect("plugin before restart")
        .generation();

    host.restart(&first_id).expect("manual restart");
    let after = host
        .plugin(&first_id)
        .expect("plugin after restart")
        .generation();
    assert_eq!(after, before + 1);
    assert_eq!(
        host.retry_snapshot(&first_id)
            .expect("retry state after restart")
            .automatic_restarts(),
        0
    );
    assert_eq!(
        host.invoke::<_, String>(&first_capability, "run", &"restarted".to_string())
            .expect("restarted plugin serves"),
        "restarted"
    );
    assert_eq!(
        host.invoke::<_, String>(
            &sibling_capability,
            "run",
            &"sibling-still-alive".to_string(),
        )
        .expect("sibling remains available"),
        "sibling-still-alive"
    );
}

#[test]
fn transport_failure_is_recovered_on_the_next_refresh() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.transport-recovery");
    let capability =
        CapabilityDescriptor::new("fixture.transport-recovery", 1).expect("capability");
    let state_path = test_state_path("transport-recovery");
    fs::write(&state_path, "drop").expect("write initial fixture state");

    host.launch(drop_on_invoke_fixture_launch(
        id.clone(),
        capability.id().as_str(),
        &state_path,
    ))
    .expect("launch transport fixture");
    let error = host
        .invoke::<_, String>(&capability, "run", &"disconnect".to_string())
        .expect_err("transport failure must be reported");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(
        host.catalog()
            .providers(capability.id().as_str(), 1)
            .is_empty()
    );

    fs::write(&state_path, "healthy").expect("make replacement healthy");
    host.refresh();
    assert_eq!(
        host.retry_snapshot(&id)
            .expect("retry state after transport recovery")
            .automatic_restarts(),
        1
    );
    assert_eq!(
        host.invoke::<_, String>(&capability, "run", &"reconnected".to_string())
            .expect("replacement serves after re-handshake"),
        "reconnected"
    );
    assert!(fs::remove_file(&state_path).is_ok());
}

#[test]
fn explicit_disable_removes_routes_until_enable() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.explicit-disable");
    let capability = CapabilityDescriptor::new("fixture.explicit-disable", 1).expect("capability");

    host.launch(fixture_launch(
        id.clone(),
        "healthy",
        capability.id().as_str(),
    ))
    .expect("launch disable fixture");
    let before = host
        .plugin(&id)
        .expect("plugin before disable")
        .generation();

    host.disable(&id).expect("disable plugin");
    assert!(
        host.catalog()
            .providers(capability.id().as_str(), 1)
            .is_empty()
    );
    host.refresh();
    assert!(
        !host
            .retry_snapshot(&id)
            .expect("disabled retry state")
            .enabled()
    );
    assert_eq!(
        host.plugin(&id)
            .expect("disabled plugin remains registered")
            .generation(),
        before
    );

    host.enable(&id).expect("enable plugin");
    assert_eq!(
        host.invoke::<_, String>(&capability, "run", &"enabled-again".to_string())
            .expect("enabled plugin serves"),
        "enabled-again"
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

fn toggle_fixture_launch(
    id: PluginId,
    capability: &str,
    state_path: &std::path::Path,
) -> PluginLaunch {
    let executable = env::current_exe().expect("resolve integration test executable");
    let command = PluginCommand::new(executable)
        .args(["--exact", "plugin_subprocess_entrypoint", "--nocapture"])
        .env(TEST_MODE_ENV, "toggle")
        .env(TEST_ID_ENV, id.as_str())
        .env(TEST_CAPABILITY_ENV, capability)
        .env(TEST_STATE_ENV, state_path.as_os_str());
    PluginLaunch::new(id, command)
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
}

fn drop_on_invoke_fixture_launch(
    id: PluginId,
    capability: &str,
    state_path: &std::path::Path,
) -> PluginLaunch {
    let executable = env::current_exe().expect("resolve integration test executable");
    let command = PluginCommand::new(executable)
        .args(["--exact", "plugin_subprocess_entrypoint", "--nocapture"])
        .env(TEST_MODE_ENV, "drop-on-invoke")
        .env(TEST_ID_ENV, id.as_str())
        .env(TEST_CAPABILITY_ENV, capability)
        .env(TEST_STATE_ENV, state_path.as_os_str());
    PluginLaunch::new(id, command)
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
}

fn wait_for_retry_exhaustion(
    host: &mut ProcessPluginHost,
    id: &PluginId,
) -> yunxi_plugin_host::RetrySnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let retry = host
            .retry_snapshot(id)
            .expect("registered plugin retry state");
        if retry.exhausted() {
            return retry;
        }
        assert!(
            Instant::now() < deadline,
            "plugin did not exhaust its retry budget: {retry:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn test_state_path(label: &str) -> std::path::PathBuf {
    env::temp_dir().join(format!(
        "yunxi-plugin-host-{label}-{}-{}.state",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn plugin_id(value: &str) -> PluginId {
    PluginId::new(value).expect("valid fixture plugin id")
}
