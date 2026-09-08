use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, process};

use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_plugin_host::{PluginCallError, PluginLaunch, ProcessPluginHost};
use yunxi_protocol::{CapabilityDescriptor, GrantKind, capabilities};
use yunxi_weixin::{
    ACK_OPERATION, ChannelMessage, DESCRIBE_OPERATION, EmptyRequest, INBOUND_OPERATION,
    LOGIN_OPERATION, MessageMutationRequest, OUTBOUND_OPERATION, POLL_LOGIN_OPERATION,
    SERVE_START_OPERATION, SERVE_STATUS_OPERATION, SERVE_STOP_OPERATION, STATUS_OPERATION,
    WEIXIN_MASTER_KEY_HEX_ENV, WEIXIN_MODE_ENV, WEIXIN_PLUGIN_ID, WEIXIN_SECRET_STORE_ENV,
    WeixinPluginResponse, WeixinPollLoginRequest, WeixinServeRequest, inbound_fixture,
    outbound_fixture,
};

#[test]
fn weixin_fixture_uses_the_same_isolated_host_path() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(WEIXIN_PLUGIN_ID).expect("weixin plugin id");
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .expect("channel capability");
    host.launch(
        PluginLaunch::new(
            plugin_id.clone(),
            PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-weixin-plugin-fixture")),
        )
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
        .with_required_grants([GrantKind::Network, GrantKind::Secret])
        .with_expected_capabilities([capability.clone()]),
    )
    .expect("weixin fixture launch");

    let inbound = inbound_fixture().expect("inbound fixture").message;
    let accepted: WeixinPluginResponse = host
        .invoke(&capability, INBOUND_OPERATION, &inbound)
        .expect("inbound route");
    assert!(matches!(
        accepted,
        WeixinPluginResponse::Accepted {
            duplicate: false,
            ..
        }
    ));

    let duplicate: WeixinPluginResponse = host
        .invoke(&capability, INBOUND_OPERATION, &inbound)
        .expect("duplicate route");
    assert!(matches!(
        duplicate,
        WeixinPluginResponse::Accepted {
            duplicate: true,
            ..
        }
    ));

    let ack: WeixinPluginResponse = host
        .invoke(
            &capability,
            ACK_OPERATION,
            &MessageMutationRequest {
                idempotency_key: inbound.envelope().idempotency_key.clone(),
                reason: None,
            },
        )
        .expect("ack route");
    assert!(matches!(ack, WeixinPluginResponse::Mutated { .. }));

    host.disable(&plugin_id).expect("disable weixin fixture");
    assert!(
        host.catalog()
            .providers(
                capabilities::CHANNEL_WEIXIN,
                capabilities::CHANNEL_WEIXIN_VERSION
            )
            .is_empty()
    );
}

#[test]
fn weixin_fixture_keeps_bad_payloads_and_wrong_routes_local() {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(WEIXIN_PLUGIN_ID).expect("weixin plugin id");
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .expect("channel capability");
    host.launch(
        PluginLaunch::new(
            plugin_id,
            PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-weixin-plugin-fixture")),
        )
        .with_handshake_timeout(Duration::from_secs(2))
        .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
        .with_required_grants([GrantKind::Network, GrantKind::Secret])
        .with_expected_capabilities([capability.clone()]),
    )
    .expect("weixin fixture launch");

    let error = host
        .invoke::<_, WeixinPluginResponse>(
            &capability,
            INBOUND_OPERATION,
            &serde_json::json!({"direction": "outbound", "message": {}}),
        )
        .expect_err("wrong payload must be rejected");
    assert!(
        matches!(error, PluginCallError::Rejected { ref code, .. } if code == "invalid_request")
    );

    let outbound = outbound_fixture().expect("outbound fixture").message;
    let accepted: WeixinPluginResponse = host
        .invoke(&capability, OUTBOUND_OPERATION, &outbound)
        .expect("outbound route remains available");
    assert!(matches!(
        accepted,
        WeixinPluginResponse::Accepted {
            message: ChannelMessage::Outbound(_),
            ..
        }
    ));
}

#[test]
fn configured_process_enters_the_real_ilink_host_boundary_without_claiming_login() {
    let store_path = temporary_store("production");
    let (mut host, plugin_id, capability) =
        launch_configured_process(&store_path, &"01".repeat(yunxi_weixin::MASTER_KEY_BYTES));

    let description: WeixinPluginResponse = host
        .invoke(&capability, DESCRIBE_OPERATION, &EmptyRequest {})
        .expect("production description");
    assert!(matches!(
        description,
        WeixinPluginResponse::Description {
            ref mode,
            real_weixin: false,
            ..
        } if mode == "production"
    ));

    let status: WeixinPluginResponse = host
        .invoke(&capability, STATUS_OPERATION, &EmptyRequest {})
        .expect("production status");
    assert!(matches!(
        status,
        WeixinPluginResponse::Runtime {
            ref operation,
            ref mode,
            ref report,
        } if operation == STATUS_OPERATION
            && mode == "production"
            && report["productionReady"] == false
            && report["state"] == "logged_out"
    ));

    host.disable(&plugin_id)
        .expect("disable configured process");
    let _ = fs::remove_file(store_path);
}

#[test]
fn malformed_production_configuration_falls_back_inside_the_plugin_boundary() {
    let store_path = temporary_store("fallback");
    let (mut host, plugin_id, capability) =
        launch_configured_process(&store_path, "not-a-master-key");

    let description: WeixinPluginResponse = host
        .invoke(&capability, DESCRIBE_OPERATION, &EmptyRequest {})
        .expect("fallback description");
    assert!(matches!(
        description,
        WeixinPluginResponse::Description {
            ref mode,
            real_weixin: false,
            ..
        } if mode == "production-unavailable"
    ));

    let login: WeixinPluginResponse = host
        .invoke(&capability, LOGIN_OPERATION, &EmptyRequest {})
        .expect("fallback login");
    assert!(matches!(
        login,
        WeixinPluginResponse::Runtime { ref report, .. }
            if report["state"] == "awaiting_qr" && report["loopback"] == true
    ));
    let confirmed: WeixinPluginResponse = host
        .invoke(
            &capability,
            POLL_LOGIN_OPERATION,
            &WeixinPollLoginRequest { verify_code: None },
        )
        .expect("fallback login confirmation");
    assert!(matches!(
        confirmed,
        WeixinPluginResponse::Runtime { ref report, .. }
            if report["state"] == "loopback"
                && report["credential_stored"] == true
                && report["loopback"] == true
    ));

    host.disable(&plugin_id).expect("disable fallback process");
    let _ = fs::remove_file(store_path);
}

#[test]
fn configured_process_exposes_a_nonblocking_idempotent_poll_lifecycle() {
    let store_path = temporary_store("poll-lifecycle");
    let (mut host, plugin_id, capability) =
        launch_configured_process(&store_path, "not-a-master-key");

    host.invoke::<_, WeixinPluginResponse>(&capability, LOGIN_OPERATION, &EmptyRequest {})
        .expect("start loopback login");
    host.invoke::<_, WeixinPluginResponse>(
        &capability,
        POLL_LOGIN_OPERATION,
        &WeixinPollLoginRequest { verify_code: None },
    )
    .expect("confirm loopback login");

    let request = WeixinServeRequest {
        max_polls: Some(1),
        max_messages_per_poll: Some(8),
        require_approval: false,
    };
    let started: WeixinPluginResponse = host
        .invoke(&capability, SERVE_START_OPERATION, &request)
        .expect("start polling without holding the Host request");
    let started_report = runtime_report(started, SERVE_START_OPERATION);
    assert!(matches!(
        started_report["state"].as_str(),
        Some("running" | "completed")
    ));
    assert!(started_report.get("report").is_some());
    assert!(started_report.get("worker").is_some());
    assert!(!started_report.to_string().contains("loopback-token"));

    let repeated: WeixinPluginResponse = host
        .invoke(&capability, SERVE_START_OPERATION, &request)
        .expect("repeat start is idempotent");
    assert_eq!(
        runtime_report(repeated, SERVE_START_OPERATION)["generation"],
        started_report["generation"]
    );

    let status: WeixinPluginResponse = host
        .invoke(&capability, SERVE_STATUS_OPERATION, &EmptyRequest {})
        .expect("status remains responsive while polling");
    assert!(
        runtime_report(status, SERVE_STATUS_OPERATION)
            .get("messages")
            .is_some()
    );

    let stopped: WeixinPluginResponse = host
        .invoke(&capability, SERVE_STOP_OPERATION, &EmptyRequest {})
        .expect("stop only requests cancellation and is bounded");
    assert_eq!(
        runtime_report(stopped, SERVE_STOP_OPERATION)["state"],
        "stopped"
    );
    let repeated_stop: WeixinPluginResponse = host
        .invoke(&capability, SERVE_STOP_OPERATION, &EmptyRequest {})
        .expect("repeated stop is idempotent");
    assert_eq!(
        runtime_report(repeated_stop, SERVE_STOP_OPERATION)["state"],
        "stopped"
    );

    host.disable(&plugin_id)
        .expect("disable configured process");
    let _ = fs::remove_file(store_path);
}

fn runtime_report(response: WeixinPluginResponse, operation: &str) -> serde_json::Value {
    match response {
        WeixinPluginResponse::Runtime {
            operation: actual,
            report,
            ..
        } if actual == operation => report,
        unexpected => panic!("unexpected response: {unexpected:?}"),
    }
}

fn launch_configured_process(
    store_path: &std::path::Path,
    master_key: &str,
) -> (ProcessPluginHost, PluginId, CapabilityDescriptor) {
    let mut host = ProcessPluginHost::new();
    let plugin_id = PluginId::new(WEIXIN_PLUGIN_ID).expect("weixin plugin id");
    let capability = CapabilityDescriptor::new(
        capabilities::CHANNEL_WEIXIN,
        capabilities::CHANNEL_WEIXIN_VERSION,
    )
    .expect("channel capability");
    let command = PluginCommand::new(env!("CARGO_BIN_EXE_yunxi-weixin-plugin"))
        .env(WEIXIN_MODE_ENV, "production")
        .env(WEIXIN_MASTER_KEY_HEX_ENV, master_key)
        .env(WEIXIN_SECRET_STORE_ENV, store_path);
    host.launch(
        PluginLaunch::new(plugin_id.clone(), command)
            .with_handshake_timeout(Duration::from_secs(2))
            .with_io_timeouts(Some(Duration::from_secs(2)), Some(Duration::from_secs(2)))
            .with_required_grants([GrantKind::Network, GrantKind::Secret])
            .with_expected_capabilities([capability.clone()]),
    )
    .expect("configured Weixin process launch");
    (host, plugin_id, capability)
}

fn temporary_store(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "yunxi-weixin-plugin-{label}-{}-{}.bin",
        process::id(),
        nonce
    ))
}
