//! Black-box process and protocol isolation tests for the capability host.

use std::env;
use std::fs;
use std::io;
use std::io::Write;
use std::net::TcpStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yunxi_kernel::{KernelState, PluginCommand, PluginId};
use yunxi_plugin_host::{
    PluginCallError, PluginLaunch, PluginResourcePolicy, ProcessPluginHost, SharedProcessPluginHost,
};
use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CapabilityDescriptor, GrantKind, GrantRequirement, HostMessage,
    InvocationResponse, ModelStreamEvent, PluginMessage, ProtocolError, connect_plugin,
    connect_plugin_with_grants,
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
    let mut cancellation_handled = false;
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
                if mode == "cancel-aware-stream" && !cancellation_handled {
                    session
                        .set_timeouts(Some(Duration::from_millis(25)), None)
                        .expect("set cancellation polling timeout");
                    loop {
                        match session.receive() {
                            Ok(HostMessage::Cancel { request_id })
                                if request_id == request.request_id() =>
                            {
                                cancellation_handled = true;
                                session
                                    .send(&PluginMessage::InvocationFailed {
                                        request_id,
                                        code: "cancelled".to_string(),
                                        message: "fixture acknowledged cancellation".to_string(),
                                        retryable: false,
                                    })
                                    .expect("send cancellation acknowledgement");
                                break;
                            }
                            Ok(HostMessage::Cancel { .. }) => {}
                            Ok(message) => panic!("unexpected control frame: {message:?}"),
                            Err(ProtocolError::Io(error))
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                                ) => {}
                            Err(error) => panic!("cancellation polling failed: {error}"),
                        }
                    }
                    session
                        .set_timeouts(None, None)
                        .expect("restore fixture timeouts");
                    continue;
                }
                if mode == "slow-stream" {
                    thread::sleep(Duration::from_secs(60));
                }
                if mode == "stream-then-stall" {
                    session
                        .send(&PluginMessage::InvocationProgress {
                            request_id: request.request_id(),
                            event: ModelStreamEvent::text_delta("started")
                                .expect("started progress"),
                        })
                        .expect("send started progress");
                    thread::sleep(Duration::from_secs(60));
                }
                if mode == "progress-then-stall" {
                    thread::sleep(Duration::from_millis(80));
                    session
                        .send(&PluginMessage::InvocationProgress {
                            request_id: request.request_id(),
                            event: ModelStreamEvent::text_delta("ignored").expect("progress event"),
                        })
                        .expect("send progress event");
                    thread::sleep(Duration::from_secs(60));
                }
                if mode == "stream" {
                    for event in [
                        ModelStreamEvent::text_delta("hello ").expect("first progress"),
                        ModelStreamEvent::text_delta("world").expect("second progress"),
                    ] {
                        session
                            .send(&PluginMessage::InvocationProgress {
                                request_id: request.request_id(),
                                event,
                            })
                            .expect("send fixture progress");
                    }
                }
                if mode == "flood-stream" {
                    for _ in 0..1_000 {
                        session
                            .send(&PluginMessage::InvocationProgress {
                                request_id: request.request_id(),
                                event: ModelStreamEvent::text_delta("x")
                                    .expect("bounded flood progress"),
                            })
                            .expect("send flood progress");
                        thread::sleep(Duration::from_millis(2));
                    }
                }
                if mode == "flood-stream-limit" {
                    for _ in 0..=yunxi_protocol::MAX_STREAM_EVENTS_PER_TURN {
                        session
                            .send(&PluginMessage::InvocationProgress {
                                request_id: request.request_id(),
                                event: ModelStreamEvent::text_delta("x")
                                    .expect("bounded limit progress"),
                            })
                            .expect("send limit progress");
                    }
                }
                let response_payload = if mode == "oversized-response" {
                    "x".repeat(12 * 1024)
                } else {
                    payload
                };
                let response = InvocationResponse::encode(request.request_id(), &response_payload)
                    .expect("encode fixture response");
                session
                    .send(&PluginMessage::InvocationCompleted { response })
                    .expect("send fixture response");
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return,
            HostMessage::Welcome { .. } => panic!("unexpected second welcome"),
        }
    }
}

#[test]
fn streaming_invocation_preserves_progress_order_and_unary_invoke_ignores_it() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.streaming");
    let capability = CapabilityDescriptor::new("fixture.streaming", 1).expect("capability");
    host.launch(fixture_launch(id, "stream", capability.id().as_str()))
        .expect("launch streaming fixture");

    let mut progress = Vec::new();
    let streamed = host
        .invoke_streaming::<_, String, _, _>(
            &capability,
            "run",
            &"streamed".to_string(),
            || false,
            |event| {
                progress.push(event);
                Ok(())
            },
        )
        .expect("streaming invocation");
    assert_eq!(streamed, "streamed");
    assert_eq!(
        progress,
        vec![
            ModelStreamEvent::text_delta("hello ").expect("first progress"),
            ModelStreamEvent::text_delta("world").expect("second progress"),
        ]
    );

    let unary = host
        .invoke::<_, String>(&capability, "run", &"unary".to_string())
        .expect("unary invocation after progress");
    assert_eq!(unary, "unary");
}

#[test]
fn streaming_callback_panic_is_contained_and_connection_is_failed() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.callback-panic");
    let capability = CapabilityDescriptor::new("fixture.callback-panic", 1).expect("capability");
    host.launch(fixture_launch(id, "stream", capability.id().as_str()))
        .expect("launch callback fixture");

    let result = catch_unwind(AssertUnwindSafe(|| {
        host.invoke_streaming::<_, String, _, _>(
            &capability,
            "run",
            &"panic-in-callback".to_string(),
            || false,
            |_| -> Result<(), String> { panic!("callback payload must not escape") },
        )
    }));
    let error = result
        .expect("host must contain callback panic")
        .expect_err("callback panic must fail the invocation");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(
        host.catalog()
            .providers(capability.id().as_str(), 1)
            .is_empty()
    );
}

#[test]
fn cancelling_stream_isolates_one_generation_and_keeps_sibling_healthy() {
    let mut host = ProcessPluginHost::new();
    let cancelled_id = plugin_id("yunxi.test.cancelled-stream");
    let healthy_id = plugin_id("yunxi.test.healthy-after-cancel");
    let cancelled_capability =
        CapabilityDescriptor::new("fixture.cancelled-stream", 1).expect("capability");
    let healthy_capability =
        CapabilityDescriptor::new("fixture.healthy-after-cancel", 1).expect("capability");
    host.launch(fixture_launch(
        cancelled_id,
        "slow-stream",
        cancelled_capability.id().as_str(),
    ))
    .expect("launch cancellable fixture");
    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy sibling");

    let cancelled = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancelled);
    let cancellation_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        trigger.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let error = host
        .invoke_streaming::<_, String, _, _>(
            &cancelled_capability,
            "run",
            &"cancel-me".to_string(),
            || cancelled.load(Ordering::Acquire),
            |_| Ok(()),
        )
        .expect_err("cancelled invocation must fail");
    let elapsed = started.elapsed();
    cancellation_thread.join().expect("cancellation thread");

    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(
        elapsed < Duration::from_secs(2),
        "cancellation took too long: {elapsed:?}"
    );
    assert!(
        host.catalog()
            .providers(cancelled_capability.id().as_str(), 1)
            .is_empty(),
        "cancelled generation must be removed from routing"
    );
    assert_eq!(host.connection_count(), 1);
    assert_eq!(
        host.invoke::<_, String>(
            &healthy_capability,
            "run",
            &"sibling-still-healthy".to_string(),
        )
        .expect("healthy sibling remains available"),
        "sibling-still-healthy"
    );
}

#[test]
fn shared_host_does_not_hold_the_global_lock_during_plugin_io() {
    let mut host = ProcessPluginHost::new();
    let blocked_id = plugin_id("yunxi.test.concurrent-blocked");
    let healthy_id = plugin_id("yunxi.test.concurrent-healthy");
    let blocked_capability =
        CapabilityDescriptor::new("fixture.concurrent-blocked", 1).expect("capability");
    let healthy_capability =
        CapabilityDescriptor::new("fixture.concurrent-healthy", 1).expect("capability");
    host.launch(fixture_launch(
        blocked_id,
        "stream-then-stall",
        blocked_capability.id().as_str(),
    ))
    .expect("launch blocked fixture");
    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy fixture");

    let host = SharedProcessPluginHost::new(host);
    let worker_host = host.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (progress_tx, progress_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        worker_host.invoke_streaming::<_, String, _, _>(
            &blocked_capability,
            "run",
            &"blocked".to_string(),
            || worker_cancelled.load(Ordering::Acquire),
            |event| {
                let _ignored = progress_tx.try_send(event);
                Ok(())
            },
        )
    });
    progress_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("blocked invocation reached plugin I/O");

    let started = Instant::now();
    let response = host
        .invoke::<_, String>(
            &healthy_capability,
            "run",
            &"sibling-served-concurrently".to_string(),
        )
        .expect("healthy sibling must remain callable");
    assert_eq!(response, "sibling-served-concurrently");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "healthy sibling waited on unrelated plugin I/O: {:?}",
        started.elapsed()
    );

    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        worker.join().expect("blocked invocation worker"),
        Err(PluginCallError::Unavailable { .. })
    ));
}

#[test]
fn streaming_cancellation_is_checked_between_continuous_progress_frames() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.flood-cancel");
    let capability = CapabilityDescriptor::new("fixture.flood-cancel", 1).expect("capability");
    host.launch(fixture_launch(id, "flood-stream", capability.id().as_str()))
        .expect("launch flood fixture");
    let cancelled = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancelled);
    let cancellation_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        trigger.store(true, Ordering::Release);
    });

    let started = Instant::now();
    let error = host
        .invoke_streaming::<_, String, _, _>(
            &capability,
            "run",
            &"cancel-flood".to_string(),
            || cancelled.load(Ordering::Acquire),
            |_| Ok(()),
        )
        .expect_err("continuous progress must still be cancellable");
    cancellation_thread.join().expect("cancellation thread");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn cooperative_stream_cancellation_reuses_the_plugin_connection() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.cooperative-cancel");
    let capability =
        CapabilityDescriptor::new("fixture.cooperative-cancel", 1).expect("capability");
    host.launch(fixture_launch(
        id,
        "cancel-aware-stream",
        capability.id().as_str(),
    ))
    .expect("launch cooperative fixture");

    let cancelled = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancelled);
    let cancellation_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        trigger.store(true, Ordering::Release);
    });
    let error = host
        .invoke_streaming::<_, String, _, _>(
            &capability,
            "run",
            &"cancel-me".to_string(),
            || cancelled.load(Ordering::Acquire),
            |_| Ok(()),
        )
        .expect_err("cooperative cancellation must return a cancellation error");
    cancellation_thread.join().expect("cancellation thread");

    assert!(matches!(error, PluginCallError::Cancelled { .. }));
    assert_eq!(host.connection_count(), 1);
    assert_eq!(
        host.invoke::<_, String>(&capability, "run", &"after-cancel".to_string())
            .expect("connection remains reusable after cancellation"),
        "after-cancel"
    );
}

#[test]
fn streaming_poll_preserves_the_configured_idle_timeout() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.stream-timeout");
    let capability = CapabilityDescriptor::new("fixture.stream-timeout", 1).expect("capability");
    let launch = fixture_launch(id, "slow-stream", capability.id().as_str()).with_io_timeouts(
        Some(Duration::from_millis(120)),
        Some(Duration::from_secs(2)),
    );
    host.launch(launch).expect("launch timeout fixture");

    let started = Instant::now();
    let error = host
        .invoke_streaming::<_, String, _, _>(
            &capability,
            "run",
            &"time-out".to_string(),
            || false,
            |_| Ok(()),
        )
        .expect_err("idle stream must time out");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn host_resource_policy_converts_invocation_timeout_to_a_bounded_failure() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.resource-duration");
    let capability = CapabilityDescriptor::new("fixture.resource-duration", 1).expect("capability");
    let policy = PluginResourcePolicy::new(64 * 1024, 64 * 1024, Duration::from_millis(120))
        .expect("bounded policy");
    host.launch(fixture_launch(id, "hang", capability.id().as_str()).with_resource_policy(policy))
        .expect("launch duration fixture");

    let started = Instant::now();
    let error = host
        .invoke::<_, String>(&capability, "run", &"bounded".to_string())
        .expect_err("duration policy must terminate a hanging invocation");
    assert!(matches!(error, PluginCallError::ResourceLimit { .. }));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn unary_invocation_deadline_is_not_reset_by_progress_frames() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.progress-deadline");
    let capability = CapabilityDescriptor::new("fixture.progress-deadline", 1).expect("capability");
    let policy = PluginResourcePolicy::new(64 * 1024, 64 * 1024, Duration::from_millis(120))
        .expect("bounded policy");
    host.launch(
        fixture_launch(id, "progress-then-stall", capability.id().as_str())
            .with_resource_policy(policy),
    )
    .expect("launch progress fixture");

    let started = Instant::now();
    let error = host
        .invoke::<_, String>(&capability, "run", &"bounded".to_string())
        .expect_err("progress must not extend the invocation deadline");
    assert!(matches!(error, PluginCallError::ResourceLimit { .. }));
    assert!(started.elapsed() < Duration::from_millis(180));
}

#[test]
fn accumulated_plugin_output_is_bounded() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.output-budget");
    let capability = CapabilityDescriptor::new("fixture.output-budget", 1).expect("capability");
    let policy = PluginResourcePolicy::new(64 * 1024, 64 * 1024, Duration::from_secs(1))
        .expect("bounded policy")
        .with_max_output_bytes(128)
        .expect("bounded output policy");
    host.launch(
        fixture_launch(id, "oversized-response", capability.id().as_str())
            .with_resource_policy(policy),
    )
    .expect("launch output fixture");

    let error = host
        .invoke::<_, String>(&capability, "run", &"output".to_string())
        .expect_err("total output must be bounded");
    assert!(matches!(error, PluginCallError::ResourceLimit { .. }));
    assert!(error.to_string().contains("invocation output bytes"));
}

#[test]
fn host_resource_policy_rejects_oversized_invocation_before_socket_write() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.resource-payload");
    let capability = CapabilityDescriptor::new("fixture.resource-payload", 1).expect("capability");
    let policy =
        PluginResourcePolicy::new(64 * 1024, 8, Duration::from_secs(1)).expect("bounded policy");
    host.launch(
        fixture_launch(id, "healthy", capability.id().as_str()).with_resource_policy(policy),
    )
    .expect("launch payload fixture");

    let error = host
        .invoke::<_, String>(&capability, "run", &"payload-too-large".to_string())
        .expect_err("oversized invocation must be rejected by the host");
    assert!(matches!(error, PluginCallError::ResourceLimit { .. }));
}

#[test]
fn oversized_plugin_frame_isolated_from_a_healthy_sibling() {
    let mut host = ProcessPluginHost::new();
    let healthy_id = plugin_id("yunxi.test.healthy-frame-limit");
    let oversized_id = plugin_id("yunxi.test.oversized-response");
    let healthy_capability =
        CapabilityDescriptor::new("fixture.healthy-frame-limit", 1).expect("capability");
    let oversized_capability =
        CapabilityDescriptor::new("fixture.oversized-response", 1).expect("capability");
    let policy = PluginResourcePolicy::new(8 * 1024, 64 * 1024, Duration::from_secs(1))
        .expect("bounded frame policy");

    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy sibling");
    host.launch(
        fixture_launch(
            oversized_id,
            "oversized-response",
            oversized_capability.id().as_str(),
        )
        .with_resource_policy(policy),
    )
    .expect("launch oversized-frame fixture");

    let error = host
        .invoke::<_, String>(&oversized_capability, "run", &"too-large".to_string())
        .expect_err("oversized response frame must be rejected");
    assert!(matches!(error, PluginCallError::Unavailable { .. }));
    assert!(error.to_string().contains("frame exceeded 8192 bytes"));
    assert!(
        host.catalog()
            .providers(oversized_capability.id().as_str(), 1)
            .is_empty()
    );
    assert_eq!(
        host.invoke::<_, String>(
            &healthy_capability,
            "run",
            &"healthy-after-frame-limit".to_string(),
        )
        .expect("healthy sibling remains available"),
        "healthy-after-frame-limit"
    );
}

#[test]
fn streaming_progress_limit_isolated_from_a_healthy_sibling() {
    let mut host = ProcessPluginHost::new();
    let noisy_id = plugin_id("yunxi.test.progress-limit");
    let healthy_id = plugin_id("yunxi.test.healthy-after-progress-limit");
    let noisy_capability =
        CapabilityDescriptor::new("fixture.progress-limit", 1).expect("progress capability");
    let healthy_capability = CapabilityDescriptor::new("fixture.healthy-after-progress-limit", 1)
        .expect("healthy capability");

    host.launch(fixture_launch(
        noisy_id,
        "flood-stream-limit",
        noisy_capability.id().as_str(),
    ))
    .expect("launch progress flood fixture");
    host.launch(fixture_launch(
        healthy_id,
        "healthy",
        healthy_capability.id().as_str(),
    ))
    .expect("launch healthy sibling");

    let started = Instant::now();
    let error = host
        .invoke_streaming::<_, String, _, _>(
            &noisy_capability,
            "run",
            &"too-many-progress-frames".to_string(),
            || false,
            |_| Ok(()),
        )
        .expect_err("progress frame budget must be enforced");
    assert!(matches!(error, PluginCallError::ProtocolViolation { .. }));
    assert!(error.to_string().contains("progress frame count"));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        host.catalog()
            .providers(noisy_capability.id().as_str(), 1)
            .is_empty()
    );
    assert_eq!(
        host.invoke::<_, String>(
            &healthy_capability,
            "run",
            &"healthy-after-progress-limit".to_string(),
        )
        .expect("healthy sibling remains available"),
        "healthy-after-progress-limit"
    );
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

#[test]
fn disabling_a_plugin_revokes_its_host_secret_until_reenabled() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.secret-revocation");
    let capability =
        CapabilityDescriptor::new("fixture.secret-revocation", 1).expect("secret capability");
    host.launch(fixture_launch(
        id.clone(),
        "healthy",
        capability.id().as_str(),
    ))
    .expect("launch secret fixture");

    let broker = host.secret_broker();
    broker
        .put_provider_credential(b"host-only-credential")
        .expect("store host credential");
    let reference = broker
        .issue_provider_credential(&id)
        .expect("issue plugin secret reference");

    host.disable(&id).expect("disable plugin");
    assert!(broker.with_secret(reference, &id, |_| ()).is_err());

    host.enable(&id).expect("re-enable plugin");
    let fresh_reference = broker
        .issue_provider_credential(&id)
        .expect("issue reference after re-enable");
    assert!(
        broker
            .with_secret(fresh_reference, &id, |bytes| bytes
                == b"host-only-credential")
            .expect("resolve reference after re-enable")
    );
}

#[test]
fn duplicate_launch_cannot_reenable_a_disabled_plugin_secret() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.duplicate-secret");
    let capability = CapabilityDescriptor::new("fixture.duplicate-secret", 1).expect("capability");
    host.launch(fixture_launch(
        id.clone(),
        "healthy",
        capability.id().as_str(),
    ))
    .expect("launch original fixture");
    let broker = host.secret_broker();
    broker
        .put_provider_credential(b"duplicate-launch-secret")
        .expect("store secret");
    let reference = broker
        .issue_provider_credential(&id)
        .expect("issue secret reference");

    host.disable(&id).expect("disable original fixture");
    let duplicate = host.launch(fixture_launch(
        id.clone(),
        "healthy",
        capability.id().as_str(),
    ));
    assert!(matches!(
        duplicate,
        Err(yunxi_plugin_host::PluginHostError::Kernel(
            yunxi_kernel::KernelError::DuplicatePlugin { .. }
        ))
    ));
    assert!(broker.with_secret(reference, &id, |_| ()).is_err());
}

#[test]
fn crashed_plugin_revokes_secret_references_before_recovery() {
    let mut host = ProcessPluginHost::new();
    let id = plugin_id("yunxi.test.crash-secret");
    let capability = CapabilityDescriptor::new("fixture.crash-secret", 1).expect("capability");
    host.launch(fixture_launch(
        id.clone(),
        "crash",
        capability.id().as_str(),
    ))
    .expect("launch crash fixture");
    let broker = host.secret_broker();
    broker
        .put_provider_credential(b"crash-revocation-secret")
        .expect("store secret");
    let reference = broker
        .issue_provider_credential(&id)
        .expect("issue secret reference");

    let retry = wait_for_retry_exhaustion(&mut host, &id);
    assert!(retry.exhausted());
    assert!(broker.with_secret(reference, &id, |_| ()).is_err());
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
