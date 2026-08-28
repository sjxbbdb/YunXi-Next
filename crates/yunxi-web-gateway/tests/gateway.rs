use serde_json::{Value, json};
use yunxi_composition::{CompositionEntry, ConfigLayer, Profile};
use yunxi_web_contract::{
    ClientRequest, EVENTS_HOST_METHOD, EVENTS_MUX_METHOD, EventChannel, RpcId, RpcMessage,
    RpcResult, parse_event_message,
};
use yunxi_web_gateway::{
    AGENT_PRESET_LIST_METHOD, COMMANDS_LIST_METHOD, CREDENTIALS_DESCRIBE_METHOD,
    DYNAMIC_CORDIS_INVENTORY_METHOD, DYNAMIC_CORDIS_SYNC_INSPECT_METHOD, Gateway,
    GatewayProjection, GatewaySessionSummary, GatewayStatus, HEALTH_STATUS_METHOD,
    HOST_DESCRIBE_METHOD, LLM_PROVIDERS_METHOD, MAX_PENDING_EVENTS, PLUGIN_INVENTORY_LIST_METHOD,
    SESSION_LIST_METHOD, SESSION_MODELS_METHOD, SETTINGS_DESCRIBE_METHOD, SKILL_LIST_METHOD,
    SUBAGENT_LIST_METHOD, WORKSPACE_LIST_METHOD,
};

fn gateway() -> Gateway {
    let mut layer = ConfigLayer::new("base").expect("layer");
    layer
        .insert(vec![
            CompositionEntry::new("model", "yunxi.model.openai").expect("entry"),
            CompositionEntry::new("shell", "yunxi.tool.shell")
                .expect("entry")
                .with_enabled(false),
        ])
        .expect("entries");
    let mut profile = Profile::new("web").expect("profile");
    profile.add_bundle(layer).expect("bundle");
    let composition = profile.compose().expect("composition");
    let projection = GatewayProjection::new(
        GatewayStatus::new("running", "running", true, 1, 1, 0)
            .with_model("deepseek", "deepseek-chat"),
        composition.inventory(),
    )
    .with_sessions([
        GatewaySessionSummary::new("session-1", 42, false, false).with_cwd("D:/workspace")
    ])
    .with_host_paths("D:/workspace", "C:/Users/test");
    Gateway::new(projection)
}

#[test]
fn dsh_bootstrap_projections_have_exact_required_fields() {
    let mut gateway = gateway();

    let host = gateway
        .dispatch(&request("rpc-host", HOST_DESCRIBE_METHOD, json!({})))
        .expect("host response");
    let RpcResult::Success(host) = host.result() else {
        panic!("host description should succeed")
    };
    assert_eq!(host["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(host["cwd"], "D:/workspace");
    assert_eq!(host["home"], "C:/Users/test");
    assert_eq!(host["provider"], "deepseek");
    assert_eq!(host["model"], "deepseek-chat");
    assert_eq!(host["attachedSessions"], 0);
    assert_eq!(host["canOpenPath"], false);

    let workspaces = gateway
        .dispatch(&request("rpc-workspaces", WORKSPACE_LIST_METHOD, json!({})))
        .expect("workspace response");
    let RpcResult::Success(workspaces) = workspaces.result() else {
        panic!("workspace list should succeed")
    };
    assert_eq!(workspaces["items"][0]["workspaceId"], "yunxi-default");
    assert_eq!(workspaces["items"][0]["path"], "D:/workspace");
    assert_eq!(workspaces["items"][0]["title"], "workspace");
    assert_eq!(workspaces["items"][0]["sessionIds"][0], "session-1");
    assert_eq!(
        workspaces["items"][0]["updatedAt"],
        "1970-01-01T00:00:00.042Z"
    );
    assert_eq!(workspaces["archivedSessionIds"], json!([]));

    let settings = gateway
        .dispatch(&request(
            "rpc-settings",
            SETTINGS_DESCRIBE_METHOD,
            json!({}),
        ))
        .expect("settings response");
    let settings = settings.result().value().expect("settings value");
    assert_eq!(settings["writable"], false);
    assert_eq!(settings["hasDocument"], false);
    assert_eq!(settings["namespaces"][0]["ns"], "ui-onboarding");
    assert_eq!(
        settings["namespaces"][0]["value"]["welcomeNoticeVersion"],
        "2026-08-13.1"
    );
}

#[test]
fn optional_dsh_panels_receive_read_only_compatibility_values() {
    let mut gateway = gateway();
    let cases = [
        (
            AGENT_PRESET_LIST_METHOD,
            json!({}),
            json!({ "presets": [], "authorable": false, "hasDocument": false }),
        ),
        (COMMANDS_LIST_METHOD, json!({ "args": {} }), json!([])),
        (
            DYNAMIC_CORDIS_INVENTORY_METHOD,
            json!({ "args": {} }),
            json!([]),
        ),
        (
            DYNAMIC_CORDIS_SYNC_INSPECT_METHOD,
            json!({ "args": { "providers": [] } }),
            Value::Null,
        ),
        (
            SKILL_LIST_METHOD,
            json!({ "sessionId": "session-1" }),
            json!({ "skills": [] }),
        ),
        (
            SUBAGENT_LIST_METHOD,
            json!({ "parentSessionId": "session-1" }),
            json!({ "entries": [], "parentAvailable": true }),
        ),
    ];
    for (index, (method, payload, expected)) in cases.into_iter().enumerate() {
        let response = gateway
            .dispatch(&request(&format!("compat-{index}"), method, payload))
            .expect("compatibility response");
        assert_eq!(
            response.result().value(),
            Some(&expected),
            "method {method}"
        );
    }

    let credentials = gateway
        .dispatch(&request(
            "credentials",
            CREDENTIALS_DESCRIBE_METHOD,
            json!({ "refs": ["DEEPSEEK_API_KEY", "OTHER_API_KEY"] }),
        ))
        .expect("credential metadata");
    let credentials = credentials.result().value().expect("credential value");
    assert_eq!(
        credentials["credentials"]["DEEPSEEK_API_KEY"],
        json!({ "configured": true, "source": "host", "writable": false })
    );
    assert_eq!(
        credentials["credentials"]["OTHER_API_KEY"],
        json!({ "configured": false, "writable": false })
    );

    let providers = gateway
        .dispatch(&request("providers", LLM_PROVIDERS_METHOD, json!({})))
        .expect("provider list");
    assert_eq!(
        providers.result().value().expect("provider value")["providers"][0]["provider"],
        "deepseek"
    );

    let models = gateway
        .dispatch(&request(
            "models",
            SESSION_MODELS_METHOD,
            json!({ "sessionId": "session-1" }),
        ))
        .expect("session models");
    let models = models.result().value().expect("models value");
    assert_eq!(models["current"]["model"], "deepseek-chat");
    assert_eq!(models["routable"], true);
}

fn request(id: &str, method: &str, payload: serde_json::Value) -> ClientRequest {
    ClientRequest::new(RpcId::new(id).expect("rpc id"), method, payload).expect("request")
}

#[test]
fn health_status_is_a_bounded_server_response() {
    let mut gateway = gateway();
    let response = gateway
        .dispatch(&request("rpc-health", HEALTH_STATUS_METHOD, json!({})))
        .expect("response");
    assert_eq!(response.rpc_id().as_str(), "rpc-health");
    let RpcResult::Success(value) = response.result() else {
        panic!("health status should succeed")
    };
    assert_eq!(value["status"], "ok");
    assert_eq!(value["provider"], "deepseek");
    assert_eq!(value["model"], "deepseek-chat");
}

#[test]
fn inventory_and_sessions_keep_dsh_response_shapes() {
    let mut gateway = gateway();
    let inventory = gateway
        .dispatch(&request(
            "rpc-inventory",
            PLUGIN_INVENTORY_LIST_METHOD,
            json!({}),
        ))
        .expect("inventory response");
    let RpcResult::Success(value) = inventory.result() else {
        panic!("inventory should succeed")
    };
    assert_eq!(value["entries"][0]["entryId"], "model");
    assert_eq!(value["entries"][1]["enabled"], false);

    let sessions = gateway
        .dispatch(&request("rpc-sessions", SESSION_LIST_METHOD, json!({})))
        .expect("session response");
    let RpcResult::Success(value) = sessions.result() else {
        panic!("session list should succeed")
    };
    assert_eq!(value["items"][0]["sessionId"], "session-1");
    assert_eq!(value["items"][0]["cwd"], "D:/workspace");
    assert!(value["items"][0].get("parentSessionId").is_none());
}

#[test]
fn unsupported_and_invalid_methods_are_structured_failures() {
    let mut gateway = gateway();
    for (id, method, payload, expected_code) in [
        (
            "rpc-unknown",
            "session.prompt",
            json!({}),
            "method-not-supported",
        ),
        (
            "rpc-stream",
            EVENTS_MUX_METHOD,
            json!({}),
            "stream-only-method",
        ),
        ("rpc-bad", SESSION_LIST_METHOD, json!([]), "invalid-payload"),
    ] {
        let response = gateway
            .dispatch(&request(id, method, payload))
            .expect("structured failure response");
        assert_eq!(
            response.result().error().expect("error").code(),
            expected_code
        );
    }
}

#[test]
fn event_channels_are_isolated_and_bounded() {
    let mut gateway = gateway();
    gateway
        .publish_event(EventChannel::Mux, json!({ "type": "session/created" }))
        .expect("mux event");
    gateway
        .publish_event(EventChannel::Host, json!({ "type": "host/status" }))
        .expect("host event");

    let mux = gateway.drain_events(EventChannel::Mux);
    assert_eq!(mux.len(), 1);
    assert_eq!(
        parse_event_message(&mux[0]).expect("mux frame").0.method(),
        EVENTS_MUX_METHOD
    );
    assert!(gateway.drain_events(EventChannel::Mux).is_empty());
    let host = gateway.drain_events(EventChannel::Host);
    assert_eq!(host.len(), 1);
    assert_eq!(
        parse_event_message(&host[0])
            .expect("host frame")
            .0
            .method(),
        EVENTS_HOST_METHOD
    );

    for index in 0..MAX_PENDING_EVENTS {
        gateway
            .publish_event(EventChannel::Host, json!({ "index": index }))
            .expect("within event limit");
    }
    let error = gateway
        .publish_event(EventChannel::Host, json!({ "overflow": true }))
        .expect_err("event queue must be bounded");
    assert!(error.to_string().contains("event channel"));
}

#[test]
fn only_client_requests_enter_unary_dispatch() {
    let mut gateway = gateway();
    let message = RpcMessage::server_request(
        RpcId::new("server-request").expect("rpc id"),
        "host.event",
        json!({}),
    )
    .expect("server request");
    let error = gateway
        .dispatch_message(message)
        .expect_err("server request is not a unary client call");
    assert!(error.to_string().contains("client-request"));
}
