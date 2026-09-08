use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yunxi_composition::{CompositionEntry, ConfigLayer, Profile};
use yunxi_web_contract::{EventChannel, RpcId, RpcMessage, RpcResult, parse_event_message};
use yunxi_web_gateway::{
    AGENT_PRESET_LIST_METHOD, COMMANDS_LIST_METHOD, CREDENTIALS_DESCRIBE_METHOD,
    DYNAMIC_CORDIS_INVENTORY_METHOD, DYNAMIC_CORDIS_SYNC_INSPECT_METHOD, Gateway,
    GatewayProjection, GatewaySessionSummary, GatewayStatus, HOST_DESCRIBE_METHOD, HttpCarrier,
    HttpResponse, LLM_PROVIDERS_METHOD, MAILBOX_GET_METHOD, MAILBOX_LIST_METHOD,
    MAILBOX_MARK_READ_METHOD, MAX_HTTP_BODY_BYTES, MAX_HTTP_HEADER_BYTES, MEMORY_LIST_METHOD,
    MEMORY_SHOW_METHOD, MEMORY_STATUS_METHOD, PERSONA_LIST_METHOD, PERSONA_PROFILE_METHOD,
    PERSONA_STATUS_METHOD, PLUGIN_INVENTORY_LIST_METHOD, RELATIONSHIP_LIST_METHOD,
    RELATIONSHIP_STATUS_METHOD, SESSION_CANCEL_METHOD, SESSION_CREATE_METHOD,
    SESSION_HISTORY_METHOD, SESSION_LIST_METHOD, SESSION_MODELS_METHOD, SESSION_PROMPT_METHOD,
    SETTINGS_DESCRIBE_METHOD, SETTINGS_MUTATE_METHOD, SETTINGS_REPLACE_METHOD,
    SETTINGS_UPDATE_METHOD, SKILL_LIST_METHOD, SUBAGENT_LIST_METHOD, ShutdownToken,
    VOICE_CANCEL_METHOD, VOICE_CHAT_METHOD, VOICE_DEVICES_METHOD, VOICE_DOCTOR_METHOD,
    VOICE_PLAYBACK_METHOD, VOICE_SAVE_METHOD, VOICE_SPEAK_METHOD, VOICE_TALK_METHOD,
    VOICE_TRANSCRIBE_METHOD, WEIXIN_SERVE_START_METHOD, WEIXIN_SERVE_STATUS_METHOD,
    WEIXIN_SERVE_STOP_METHOD, WORKSPACE_LIST_METHOD,
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

fn client_request(id: &str, method: &str, payload: Value) -> Vec<u8> {
    RpcMessage::client_request(RpcId::new(id).expect("rpc id"), method, payload)
        .expect("client request")
        .encode()
        .expect("client request frame")
}

fn client_response(id: &str, value: Value) -> Vec<u8> {
    RpcMessage::client_response(RpcId::new(id).expect("rpc id"), RpcResult::success(value))
        .expect("client response")
        .encode()
        .expect("client response frame")
}

fn raw_request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn post(path: &str, body: Vec<u8>) -> Vec<u8> {
    raw_request("POST", path, &[("Content-Type", "application/json")], &body)
}

fn temporary_event_journal_path(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "yunxi-web-gateway-http-{name}-{}-{stamp}.jsonl",
        std::process::id()
    ))
}

fn rpc_response(response: &HttpResponse) -> yunxi_web_contract::ServerResponse {
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let RpcMessage::ServerResponse(response) = RpcMessage::decode(response.body()).expect("RPC")
    else {
        panic!("HTTP unary response must be a server-response")
    };
    response
}

fn failure_code(response: &HttpResponse) -> String {
    let response = rpc_response(response);
    response
        .result()
        .error()
        .expect("RPC failure")
        .code()
        .to_string()
}

fn sse_messages(response: &HttpResponse) -> Vec<RpcMessage> {
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let body = std::str::from_utf8(response.body()).expect("SSE is UTF-8");
    assert!(body.starts_with(": connected\n\n"));
    assert!(body.ends_with(": end\n\n"));
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| RpcMessage::decode(line.as_bytes()).expect("SSE RPC frame"))
        .collect()
}

fn sse_records(response: &HttpResponse) -> Vec<(Option<u64>, RpcMessage)> {
    assert_eq!(response.status(), 200);
    let body = std::str::from_utf8(response.body()).expect("SSE is UTF-8");
    let mut records = Vec::new();
    for chunk in body.split("\n\n") {
        let id = chunk
            .lines()
            .find_map(|line| line.strip_prefix("id: "))
            .map(|value| value.parse::<u64>().expect("numeric SSE id"));
        let Some(data) = chunk.lines().find_map(|line| line.strip_prefix("data: ")) else {
            continue;
        };
        records.push((
            id,
            RpcMessage::decode(data.as_bytes()).expect("SSE RPC frame"),
        ));
    }
    records
}

#[test]
fn unary_http_routes_return_dsh_json_envelopes() {
    let mut carrier = HttpCarrier::new(gateway());
    let cases = [
        (AGENT_PRESET_LIST_METHOD, json!({})),
        (COMMANDS_LIST_METHOD, json!({ "args": {} })),
        (
            CREDENTIALS_DESCRIBE_METHOD,
            json!({ "refs": ["DEEPSEEK_API_KEY"] }),
        ),
        (DYNAMIC_CORDIS_INVENTORY_METHOD, json!({ "args": {} })),
        (
            DYNAMIC_CORDIS_SYNC_INSPECT_METHOD,
            json!({ "args": { "providers": [] } }),
        ),
        ("health.status", json!({})),
        (HOST_DESCRIBE_METHOD, json!({})),
        (LLM_PROVIDERS_METHOD, json!({})),
        (PLUGIN_INVENTORY_LIST_METHOD, json!({})),
        (SESSION_LIST_METHOD, json!({})),
        (SESSION_MODELS_METHOD, json!({ "sessionId": "session-1" })),
        (SETTINGS_DESCRIBE_METHOD, json!({})),
        (SKILL_LIST_METHOD, json!({ "sessionId": "session-1" })),
        (
            SUBAGENT_LIST_METHOD,
            json!({ "parentSessionId": "session-1" }),
        ),
        (WORKSPACE_LIST_METHOD, json!({})),
    ];

    for (index, (method, payload)) in cases.into_iter().enumerate() {
        let response = carrier.handle_bytes(&post(
            &format!("/api/{method}"),
            client_request(&format!("rpc-{index}"), method, payload),
        ));
        let response = rpc_response(&response);
        assert_eq!(response.rpc_id().as_str(), format!("rpc-{index}"));
        assert!(response.result().is_success());
    }
}

#[test]
fn management_http_routes_are_registered_with_the_carrier() {
    let mut carrier = HttpCarrier::new(gateway());
    let cases = [
        (MEMORY_STATUS_METHOD, json!({})),
        (MEMORY_LIST_METHOD, json!({})),
        (MEMORY_SHOW_METHOD, json!({ "id": "memory-1" })),
        (PERSONA_STATUS_METHOD, json!({})),
        (PERSONA_LIST_METHOD, json!({})),
        (PERSONA_PROFILE_METHOD, json!({})),
        (RELATIONSHIP_STATUS_METHOD, json!({})),
        (RELATIONSHIP_LIST_METHOD, json!({})),
        (MAILBOX_LIST_METHOD, json!({})),
        (MAILBOX_GET_METHOD, json!({ "itemId": "mailbox-1" })),
        (
            MAILBOX_MARK_READ_METHOD,
            json!({ "itemId": "mailbox-1", "read": true }),
        ),
        (VOICE_DOCTOR_METHOD, json!({})),
        (VOICE_DEVICES_METHOD, json!({})),
        (VOICE_TRANSCRIBE_METHOD, json!({})),
        (VOICE_SPEAK_METHOD, json!({})),
        (VOICE_CHAT_METHOD, json!({})),
        (VOICE_TALK_METHOD, json!({})),
        (VOICE_PLAYBACK_METHOD, json!({})),
        (VOICE_SAVE_METHOD, json!({})),
        (VOICE_CANCEL_METHOD, json!({})),
        (WEIXIN_SERVE_START_METHOD, json!({})),
        (WEIXIN_SERVE_STATUS_METHOD, json!({})),
        (WEIXIN_SERVE_STOP_METHOD, json!({})),
    ];

    for (index, (method, payload)) in cases.into_iter().enumerate() {
        let response = carrier.handle_bytes(&post(
            &format!("/api/{method}"),
            client_request(&format!("rpc-management-{index}"), method, payload),
        ));
        let response = rpc_response(&response);
        assert_eq!(
            response
                .result()
                .error()
                .expect("read-only Gateway fixture failure")
                .code(),
            "method-not-supported",
            "management method was not routed through the RPC envelope: {method}"
        );
    }
}

#[test]
fn session_http_routes_are_registered_with_the_carrier() {
    let mut carrier = HttpCarrier::new(gateway());
    let cases = [
        (SESSION_CREATE_METHOD, json!({})),
        (SESSION_CANCEL_METHOD, json!({ "sessionId": "session-1" })),
        (SESSION_HISTORY_METHOD, json!({ "sessionId": "session-1" })),
        (
            SESSION_PROMPT_METHOD,
            json!({
                "sessionId": "session-1",
                "mode": "queue",
                "content": [{ "type": "text", "text": "hello" }]
            }),
        ),
    ];
    for (index, (method, payload)) in cases.into_iter().enumerate() {
        let response = carrier.handle_bytes(&post(
            &format!("/api/{method}"),
            client_request(&format!("rpc-session-{index}"), method, payload),
        ));
        let response = rpc_response(&response);
        assert_eq!(
            response
                .result()
                .error()
                .expect("Gateway fixture failure")
                .code(),
            "method-not-supported"
        );
    }
}

#[test]
fn settings_write_http_routes_are_registered_with_the_carrier() {
    let mut carrier = HttpCarrier::new(gateway());
    let cases = [
        (
            SETTINGS_UPDATE_METHOD,
            json!({
                "ns": "yunxi-capabilities",
                "patch": { "context": false },
                "expectedRevision": 0,
            }),
        ),
        (
            SETTINGS_REPLACE_METHOD,
            json!({
                "ns": "yunxi-capabilities",
                "section": { "context": false },
                "expectedRevision": 0,
            }),
        ),
        (
            SETTINGS_MUTATE_METHOD,
            json!({
                "ns": "yunxi-capabilities",
                "ops": [{ "op": "set", "path": ["context"], "value": false }],
                "expectedRevision": 0,
            }),
        ),
    ];
    for (index, (method, payload)) in cases.into_iter().enumerate() {
        let response = carrier.handle_bytes(&post(
            &format!("/api/{method}"),
            client_request(&format!("rpc-settings-{index}"), method, payload),
        ));
        let response = rpc_response(&response);
        assert_eq!(
            response
                .result()
                .error()
                .expect("read-only Gateway fixture failure")
                .code(),
            "method-not-supported"
        );
    }
}

#[test]
fn respond_route_returns_a_bounded_receipt() {
    let mut carrier = HttpCarrier::new(gateway());
    let response = carrier.handle_bytes(&post(
        "/api/respond",
        client_response(
            "approval-rpc-1",
            json!({
                "sessionId": "session-1",
                "approvalId": "approval-1",
                "outcome": "allowed-once"
            }),
        ),
    ));
    assert_eq!(response.status(), 200);
    assert_eq!(
        serde_json::from_slice::<Value>(response.body()).expect("receipt"),
        json!({ "accepted": false, "reason": "not-pending" })
    );

    let malformed = carrier.handle_bytes(&post(
        "/api/respond",
        serde_json::to_vec(&json!({ "type": "client-response" })).expect("JSON"),
    ));
    assert_eq!(malformed.status(), 200);
    assert_eq!(
        serde_json::from_slice::<Value>(malformed.body()).expect("bad receipt"),
        json!({ "accepted": false, "reason": "bad-response" })
    );
}

#[test]
fn unary_http_routes_preserve_projection_shapes() {
    let mut carrier = HttpCarrier::new(gateway());

    let health = carrier.handle_bytes(&post(
        "/api/health.status?view=full",
        client_request("rpc-health", "health.status", json!({})),
    ));
    let health = rpc_response(&health);
    let RpcResult::Success(value) = health.result() else {
        panic!("health should succeed")
    };
    assert_eq!(value["status"], "ok");
    assert_eq!(value["provider"], "deepseek");

    let inventory = carrier.handle_bytes(&post(
        "/api/pluginInventory/list",
        client_request("rpc-inventory", PLUGIN_INVENTORY_LIST_METHOD, json!({})),
    ));
    let inventory = rpc_response(&inventory);
    let RpcResult::Success(value) = inventory.result() else {
        panic!("inventory should succeed")
    };
    assert_eq!(value["entries"][0]["entryId"], "model");
    assert_eq!(value["entries"][1]["enabled"], false);

    let sessions = carrier.handle_bytes(&post(
        "/api/session.list",
        client_request("rpc-sessions", SESSION_LIST_METHOD, json!({})),
    ));
    let sessions = rpc_response(&sessions);
    let RpcResult::Success(value) = sessions.result() else {
        panic!("sessions should succeed")
    };
    assert_eq!(value["items"][0]["sessionId"], "session-1");
    assert_eq!(value["items"][0]["cwd"], "D:/workspace");
}

#[test]
fn http_routes_reject_unknown_methods_and_media_types() {
    let mut carrier = HttpCarrier::new(gateway());
    let request = client_request("rpc-unknown", "health.status", json!({}));

    let unknown = carrier.handle_bytes(&post("/api/not-enabled", request.clone()));
    assert_eq!(unknown.status(), 404);

    let wrong_verb = carrier.handle_bytes(&raw_request("GET", "/api/health.status", &[], &[]));
    assert_eq!(wrong_verb.status(), 404);

    let wrong_content_type = carrier.handle_bytes(&raw_request(
        "POST",
        "/api/health.status",
        &[("Content-Type", "text/plain")],
        &request,
    ));
    assert_eq!(wrong_content_type.status(), 415);

    let invalid_json = carrier.handle_bytes(&raw_request(
        "POST",
        "/api/health.status",
        &[("Content-Type", "application/json")],
        b"not-json",
    ));
    assert_eq!(invalid_json.status(), 400);
}

#[test]
fn malformed_rpc_envelopes_use_structured_bad_request_responses() {
    let mut carrier = HttpCarrier::new(gateway());
    let valid_id = carrier.handle_bytes(&post(
        "/api/health.status",
        serde_json::to_vec(&json!({
            "type": "client-request",
            "rpcId": "rpc-salvage",
            "method": "health.status"
        }))
        .expect("JSON"),
    ));
    assert_eq!(failure_code(&valid_id), "bad-request");
    let RpcMessage::ServerResponse(response) =
        RpcMessage::decode(valid_id.body()).expect("response")
    else {
        panic!("expected server response")
    };
    assert_eq!(response.rpc_id().as_str(), "rpc-salvage");

    let invalid_id = carrier.handle_bytes(&post(
        "/api/health.status",
        serde_json::to_vec(&json!({
            "type": "client-request",
            "rpcId": "rpc id",
            "method": "health.status"
        }))
        .expect("JSON"),
    ));
    let RpcMessage::ServerResponse(response) =
        RpcMessage::decode(invalid_id.body()).expect("response")
    else {
        panic!("expected server response")
    };
    assert_eq!(response.rpc_id().as_str(), "invalid-request");
    assert_eq!(
        response.result().error().expect("failure").code(),
        "bad-request"
    );
}

#[test]
fn path_and_envelope_method_mismatch_is_a_structured_failure() {
    let mut carrier = HttpCarrier::new(gateway());
    let response = carrier.handle_bytes(&post(
        "/api/health.status",
        client_request("rpc-mismatch", SESSION_LIST_METHOD, json!({})),
    ));
    assert_eq!(failure_code(&response), "bad-request");
    let RpcMessage::ServerResponse(response) = RpcMessage::decode(response.body()).expect("RPC")
    else {
        panic!("expected server response")
    };
    assert!(
        response.result().error().expect("failure").details()["message"]
            .as_str()
            .expect("failure details message")
            .contains("does not match path")
    );
}

#[test]
fn request_framing_and_size_limits_are_enforced() {
    let mut carrier = HttpCarrier::new(gateway());

    let oversized_headers = vec![b'x'; MAX_HTTP_HEADER_BYTES + 1];
    assert_eq!(carrier.handle_bytes(&oversized_headers).status(), 413);

    let oversized_body_header = format!(
        "POST /api/health.status HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        MAX_HTTP_BODY_BYTES + 1
    );
    assert_eq!(
        carrier
            .handle_bytes(oversized_body_header.as_bytes())
            .status(),
        413
    );

    let chunked = b"POST /api/health.status HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert_eq!(carrier.handle_bytes(chunked).status(), 400);

    let duplicate_length = b"POST /api/health.status HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(carrier.handle_bytes(duplicate_length).status(), 400);

    let pipelined = [
        b"GET /api/health.status HTTP/1.1\r\nHost: localhost\r\n\r\n".as_slice(),
        b"GET /api/health.status HTTP/1.1\r\nHost: localhost\r\n\r\n".as_slice(),
    ]
    .concat();
    assert_eq!(carrier.handle_bytes(&pipelined).status(), 400);
}

#[test]
fn sse_routes_keep_mux_and_host_channels_isolated() {
    let mut carrier = HttpCarrier::new(gateway());
    carrier
        .backend_mut()
        .publish_event(EventChannel::Mux, json!({ "type": "session/created" }))
        .expect("mux event");
    carrier
        .backend_mut()
        .publish_event(EventChannel::Host, json!({ "type": "host/status" }))
        .expect("host event");

    let mux = carrier.handle_bytes(&raw_request("GET", "/api/events.mux", &[], &[]));
    let mux_messages = sse_messages(&mux);
    assert_eq!(mux_messages.len(), 1);
    let (channel, _, payload) = parse_event_message(&mux_messages[0]).expect("mux event");
    assert_eq!(channel, EventChannel::Mux);
    assert_eq!(payload["type"], "session/created");

    let host = carrier.handle_bytes(&raw_request("GET", "/api/events.host", &[], &[]));
    let host_messages = sse_messages(&host);
    assert_eq!(host_messages.len(), 1);
    let (channel, _, payload) = parse_event_message(&host_messages[0]).expect("host event");
    assert_eq!(channel, EventChannel::Host);
    assert_eq!(payload["type"], "host/status");
}

#[test]
fn sse_reconnects_replay_from_last_event_id_or_after_seq() {
    let mut carrier = HttpCarrier::new(gateway());
    carrier
        .backend_mut()
        .publish_event(EventChannel::Host, json!({ "type": "host/one" }))
        .expect("first event");

    let first = carrier.handle_bytes(&raw_request("GET", "/api/events.host", &[], &[]));
    let first_records = sse_records(&first);
    assert_eq!(first_records.len(), 1);
    assert_eq!(first_records[0].0, Some(1));

    carrier
        .backend_mut()
        .publish_event(EventChannel::Host, json!({ "type": "host/two" }))
        .expect("second event");
    let second = carrier.handle_bytes(&raw_request(
        "GET",
        "/api/events.host?afterSeq=1",
        &[("Last-Event-ID", "1")],
        &[],
    ));
    let second_records = sse_records(&second);
    assert_eq!(second_records.len(), 1);
    assert_eq!(second_records[0].0, Some(2));
    let (_, message) = &second_records[0];
    assert_eq!(
        parse_event_message(message).expect("event").2["type"],
        "host/two"
    );

    let replay = carrier.handle_bytes(&raw_request("GET", "/api/events.host?afterSeq=0", &[], &[]));
    let replay_records = sse_records(&replay);
    assert_eq!(replay_records.len(), 2);
    assert_eq!(replay_records[0].0, Some(1));
    assert_eq!(replay_records[1].0, Some(2));
}

#[test]
fn sse_reports_a_structured_replay_gap_after_window_eviction() {
    let mut carrier = HttpCarrier::new(gateway());
    for index in 0..(yunxi_web_gateway::MAX_PENDING_EVENTS + 2) {
        carrier
            .backend_mut()
            .publish_event(EventChannel::Host, json!({ "index": index }))
            .expect("event is queued and immediately polled");
        let response = carrier.handle_bytes(&raw_request("GET", "/api/events.host", &[], &[]));
        assert_eq!(sse_records(&response).len(), 1);
    }

    let response =
        carrier.handle_bytes(&raw_request("GET", "/api/events.host?afterSeq=0", &[], &[]));
    let records = sse_records(&response);
    assert_eq!(records.len(), yunxi_web_gateway::MAX_PENDING_EVENTS);
    let (id, message) = records.first().expect("replay gap event");
    assert_eq!(*id, Some(2));
    let (_, _, payload) = parse_event_message(message).expect("replay gap event envelope");
    assert_eq!(payload["type"], "stream/error");
    assert_eq!(payload["error"]["code"], "internal");
    assert!(
        payload["error"]["message"]
            .as_str()
            .expect("gap message")
            .contains("afterSeq=0")
    );
    assert_eq!(response.header("x-yunxi-replay-gap"), Some("true"));
    assert_eq!(response.header("x-yunxi-event-after"), Some("0"));
    assert_eq!(response.header("x-yunxi-event-oldest"), Some("3"));
    assert_eq!(response.header("x-yunxi-event-latest"), Some("258"));
    assert_eq!(response.header("x-yunxi-event-has-more"), Some("true"));
    let ids = records
        .iter()
        .map(|(id, _)| id.expect("every replay record has a monotonic id"))
        .collect::<Vec<_>>();
    assert!(ids.windows(2).all(|window| window[0] < window[1]));
}

#[test]
fn sse_long_poll_is_bounded_and_rejects_invalid_wait_values() {
    let mut carrier = HttpCarrier::new(gateway());
    let invalid = carrier.handle_bytes(&raw_request(
        "GET",
        "/api/events.host?waitMs=not-a-number",
        &[],
        &[],
    ));
    assert_eq!(invalid.status(), 400);

    let started = std::time::Instant::now();
    let idle = carrier.handle_bytes(&raw_request(
        "GET",
        "/api/events.host?waitMs=999999",
        &[],
        &[],
    ));
    assert_eq!(idle.status(), 200);
    assert_eq!(idle.header("x-yunxi-event-has-more"), Some("false"));
    assert!(started.elapsed() < Duration::from_millis(500));

    let duplicate = carrier.handle_bytes(&raw_request(
        "GET",
        "/api/events.host?waitMs=1&waitMs=2",
        &[],
        &[],
    ));
    assert_eq!(duplicate.status(), 400);
}

#[test]
fn sse_rejects_invalid_cursors_without_touching_backend_events() {
    let mut carrier = HttpCarrier::new(gateway());
    carrier
        .backend_mut()
        .publish_event(EventChannel::Mux, json!({ "type": "mux/event" }))
        .expect("event");
    let invalid =
        carrier.handle_bytes(&raw_request("GET", "/api/events.mux?afterSeq=-1", &[], &[]));
    assert_eq!(invalid.status(), 400);

    let ahead = carrier.handle_bytes(&raw_request("GET", "/api/events.mux?afterSeq=1", &[], &[]));
    assert_eq!(ahead.status(), 400);
    assert!(String::from_utf8_lossy(ahead.body()).contains("ahead"));

    let valid = carrier.handle_bytes(&raw_request("GET", "/api/events.mux", &[], &[]));
    assert_eq!(sse_records(&valid).len(), 1);
}

#[test]
fn sse_rejects_conflicting_cursor_forms_without_consuming_events() {
    let mut carrier = HttpCarrier::new(gateway());
    carrier
        .backend_mut()
        .publish_event(EventChannel::Host, json!({ "type": "host/event" }))
        .expect("event");

    let conflicting = carrier.handle_bytes(&raw_request(
        "GET",
        "/api/events.host?afterSeq=0",
        &[("Last-Event-ID", "1")],
        &[],
    ));
    assert_eq!(conflicting.status(), 400);
    assert!(String::from_utf8_lossy(conflicting.body()).contains("same event cursor"));

    let recovered =
        carrier.handle_bytes(&raw_request("GET", "/api/events.host?afterSeq=0", &[], &[]));
    let records = sse_records(&recovered);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, Some(1));
}

#[test]
fn http_response_wire_bytes_are_self_delimiting() {
    let mut carrier = HttpCarrier::new(gateway());
    let response = carrier.handle_bytes(&post(
        "/api/health.status",
        client_request("rpc-wire", "health.status", json!({})),
    ));
    let wire = response.to_bytes();
    let separator = wire
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP header separator");
    let headers = std::str::from_utf8(&wire[..separator]).expect("HTTP headers");
    assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(headers.contains("Content-Type: application/json\r\n"));
    assert!(headers.contains(&format!("Content-Length: {}\r\n", response.body().len())));
    assert_eq!(&wire[separator + 4..], response.body());
}

#[test]
fn tcp_connection_carrier_returns_a_complete_http_response() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("connection");
        let mut carrier = HttpCarrier::new(gateway());
        carrier.serve_connection(stream).expect("serve connection");
    });

    let mut client = TcpStream::connect(address).expect("connect");
    let request = post(
        "/api/health.status",
        client_request("rpc-tcp", "health.status", json!({})),
    );
    client.write_all(&request).expect("write request");
    client.shutdown(Shutdown::Write).expect("finish request");
    let mut response = Vec::new();
    client.read_to_end(&mut response).expect("read response");
    server.join().expect("server thread");

    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(response.windows(4).any(|window| window == b"\r\n\r\n"));
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response separator");
    assert!(RpcMessage::decode(&response[separator + 4..]).is_ok());
}

#[test]
fn accepted_nonblocking_connections_wait_for_a_request() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(10035) =>
                {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("accept connection: {error}"),
            }
        };
        let mut carrier = HttpCarrier::new(gateway());
        carrier.serve_connection(stream).expect("serve connection");
    });

    let mut client = TcpStream::connect(address).expect("connect");
    thread::sleep(Duration::from_millis(25));
    let request = post(
        "/api/health.status",
        client_request("rpc-delayed", "health.status", json!({})),
    );
    client.write_all(&request).expect("write request");
    client.shutdown(Shutdown::Write).expect("finish request");
    let mut response = Vec::new();
    client.read_to_end(&mut response).expect("read response");
    server.join().expect("server thread");

    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(response.windows(4).any(|window| window == b"\r\n\r\n"));
}

#[test]
fn concurrent_carrier_serves_sse_and_unary_connections_independently() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let shutdown = ShutdownToken::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        HttpCarrier::new(gateway())
            .with_read_timeout(Duration::from_millis(250))
            .serve_until_concurrent(listener, &server_shutdown)
            .expect("serve concurrent connections");
    });

    let event_client = thread::spawn(move || {
        let mut client = TcpStream::connect(address).expect("event connection");
        client
            .write_all(&raw_request("GET", "/api/events.host?waitMs=40", &[], &[]))
            .expect("write event request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish event request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("read event response");
        response
    });
    let unary_client = thread::spawn(move || {
        let mut client = TcpStream::connect(address).expect("unary connection");
        client
            .write_all(&post(
                "/api/health.status",
                client_request("rpc-concurrent", "health.status", json!({})),
            ))
            .expect("write unary request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish unary request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("read unary response");
        response
    });

    let event_response = event_client.join().expect("event client thread");
    let unary_response = unary_client.join().expect("unary client thread");
    shutdown.request_shutdown();
    server.join().expect("server thread");

    assert!(event_response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(
        event_response
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
    );
    assert!(unary_response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let separator = unary_response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("unary response separator");
    assert!(RpcMessage::decode(&unary_response[separator + 4..]).is_ok());
}

#[test]
fn concurrent_initial_sse_subscribers_take_disjoint_live_batches() {
    let mut gateway = gateway();
    gateway
        .publish_event(EventChannel::Host, json!({ "type": "host/one" }))
        .expect("first event");
    gateway
        .publish_event(EventChannel::Host, json!({ "type": "host/two" }))
        .expect("second event");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let shutdown = ShutdownToken::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        HttpCarrier::new(gateway)
            .serve_until_concurrent(listener, &server_shutdown)
            .expect("serve concurrent connections");
    });

    let barrier = Arc::new(Barrier::new(3));
    let clients = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut client = TcpStream::connect(address).expect("connect");
                barrier.wait();
                client
                    .write_all(&raw_request("GET", "/api/events.host", &[], &[]))
                    .expect("write event request");
                client.shutdown(Shutdown::Write).expect("finish request");
                let mut response = Vec::new();
                client.read_to_end(&mut response).expect("read response");
                response
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let responses = clients
        .into_iter()
        .map(|client| client.join().expect("join client"))
        .collect::<Vec<_>>();
    shutdown.request_shutdown();
    server.join().expect("join server");

    let mut ids = responses
        .iter()
        .flat_map(|response| sse_ids_from_wire(response))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn tcp_sse_reconnect_replays_from_a_persistent_carrier_journal() {
    let mut gateway = gateway();
    gateway
        .publish_event(EventChannel::Host, json!({ "type": "host/one" }))
        .expect("event");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let shutdown = ShutdownToken::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        HttpCarrier::new(gateway)
            .serve_until_concurrent(listener, &server_shutdown)
            .expect("serve HTTP/SSE");
    });

    let mut first_client = TcpStream::connect(address).expect("first connection");
    first_client
        .write_all(&raw_request(
            "GET",
            "/api/events.host?afterSeq=0",
            &[("Last-Event-ID", "0")],
            &[],
        ))
        .expect("write first SSE request");
    first_client
        .shutdown(Shutdown::Write)
        .expect("finish first request");
    let mut first_response = Vec::new();
    first_client
        .read_to_end(&mut first_response)
        .expect("read first SSE response");
    assert!(first_response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(sse_ids_from_wire(&first_response), vec![1]);

    let mut reconnect = TcpStream::connect(address).expect("reconnect");
    reconnect
        .write_all(&raw_request(
            "GET",
            "/api/events.host?afterSeq=1",
            &[("Last-Event-ID", "1")],
            &[],
        ))
        .expect("write reconnect request");
    reconnect
        .shutdown(Shutdown::Write)
        .expect("finish reconnect request");
    let mut reconnect_response = Vec::new();
    reconnect
        .read_to_end(&mut reconnect_response)
        .expect("read reconnect response");
    assert!(reconnect_response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(sse_ids_from_wire(&reconnect_response).is_empty());
    let reconnect_text = String::from_utf8_lossy(&reconnect_response);
    assert!(reconnect_text.contains("X-Yunxi-Event-After: 1\r\n"));
    assert!(reconnect_text.contains("X-Yunxi-Event-Oldest: 1\r\n"));
    assert!(reconnect_text.contains("X-Yunxi-Event-Latest: 1\r\n"));

    shutdown.request_shutdown();
    server.join().expect("join server");
}

#[test]
fn sse_cursor_replays_after_carrier_restart_with_durable_journal() {
    let path = temporary_event_journal_path("restart");
    let mut first = HttpCarrier::new(gateway())
        .with_event_journal_path(&path)
        .expect("attach durable journal");
    first
        .backend_mut()
        .publish_event(EventChannel::Host, json!({ "type": "host/restarted" }))
        .expect("event");
    let first_response =
        first.handle_bytes(&raw_request("GET", "/api/events.host?afterSeq=0", &[], &[]));
    assert_eq!(sse_ids_from_wire(&first_response.to_bytes()), vec![1]);
    drop(first);

    let mut restarted = HttpCarrier::new(gateway())
        .with_event_journal_path(&path)
        .expect("reload durable journal");
    let replay =
        restarted.handle_bytes(&raw_request("GET", "/api/events.host?afterSeq=0", &[], &[]));
    assert_eq!(sse_ids_from_wire(&replay.to_bytes()), vec![1]);
    assert_eq!(restarted.event_journal_path(), Some(path.as_path()));
    let _ = std::fs::remove_file(path);
}

#[test]
fn concurrent_server_cancels_a_connection_with_an_incomplete_request() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let shutdown = ShutdownToken::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        HttpCarrier::new(gateway())
            .serve_until_concurrent(listener, &server_shutdown)
            .expect("serve HTTP");
    });

    let mut client = TcpStream::connect(address).expect("connect");
    client
        .write_all(b"GET /api/events.host HTTP/1.1\r\nHost: localhost\r\n")
        .expect("write incomplete request");
    thread::sleep(Duration::from_millis(35));
    let started = std::time::Instant::now();
    shutdown.request_shutdown();
    server.join().expect("join cancelled server");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn slow_sse_consumer_does_not_block_an_independent_unary_request() {
    let mut gateway = gateway();
    gateway
        .publish_event(
            EventChannel::Host,
            json!({ "type": "host/large", "data": "x".repeat(240_000) }),
        )
        .expect("large event");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let shutdown = ShutdownToken::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        HttpCarrier::new(gateway)
            .serve_until_concurrent(listener, &server_shutdown)
            .expect("serve HTTP/SSE");
    });

    let mut slow_client = TcpStream::connect(address).expect("slow connection");
    slow_client
        .write_all(&raw_request("GET", "/api/events.host", &[], &[]))
        .expect("write slow SSE request");
    slow_client
        .shutdown(Shutdown::Write)
        .expect("finish slow SSE request");
    thread::sleep(Duration::from_millis(30));

    let started = std::time::Instant::now();
    let mut unary_client = TcpStream::connect(address).expect("unary connection");
    unary_client
        .write_all(&post(
            "/api/health.status",
            client_request("rpc-after-slow", "health.status", json!({})),
        ))
        .expect("write unary request");
    unary_client
        .shutdown(Shutdown::Write)
        .expect("finish unary request");
    let mut unary_response = Vec::new();
    unary_client
        .read_to_end(&mut unary_response)
        .expect("read unary response");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(unary_response.starts_with(b"HTTP/1.1 200 OK\r\n"));

    shutdown.request_shutdown();
    server.join().expect("join server");
}

fn sse_ids_from_wire(response: &[u8]) -> Vec<u64> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response separator");
    String::from_utf8_lossy(&response[separator + 4..])
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .map(|id| id.parse::<u64>().expect("numeric SSE id"))
        .collect()
}

#[test]
fn root_serves_the_embedded_web_app_assets() {
    let mut carrier = HttpCarrier::new(gateway());
    let cases = [
        ("/", "text/html", "no-cache", b"__DSH_BOOT__".as_slice()),
        (
            "/index.html",
            "text/html",
            "no-cache",
            b"YunXi Next".as_slice(),
        ),
        (
            "/manifest.webmanifest",
            "application/manifest+json",
            "no-cache",
            b"YunXi Next".as_slice(),
        ),
        (
            "/assets/index-ClqxG24t.js",
            "application/javascript",
            "public, max-age=31536000, immutable",
            b"createRoot".as_slice(),
        ),
        (
            "/assets/fonts/KaTeX_Main-Regular-B22Nviop.woff2",
            "font/woff2",
            "public, max-age=31536000, immutable",
            b"wOF2".as_slice(),
        ),
        (
            "/plugins/@deepseek-ai/dsh-client-runtime/client.js?rev=aba836a0c42d",
            "application/javascript",
            "public, max-age=31536000, immutable",
            b"@deepseek-ai/dsh-client-runtime".as_slice(),
        ),
    ];

    for (path, content_type, cache_control, marker) in cases {
        let response = carrier.handle_bytes(&raw_request("GET", path, &[], &[]));
        assert_eq!(response.status(), 200, "asset path: {path}");
        assert!(
            response
                .header("content-type")
                .is_some_and(|value| value.starts_with(content_type)),
            "asset content type: {path}"
        );
        assert_eq!(
            response.header("cache-control"),
            Some(cache_control),
            "asset cache policy: {path}"
        );
        assert!(
            response
                .body()
                .windows(marker.len())
                .any(|window| window == marker),
            "asset marker: {path}"
        );
    }

    for path in [
        "/app.css",
        "/not-a-web-route",
        "/assets/../index.html",
        "/plugins/%2e%2e/index.html",
        "/assets\\index-ClqxG24t.js",
    ] {
        assert_eq!(
            carrier
                .handle_bytes(&raw_request("GET", path, &[], &[]))
                .status(),
            404,
            "unknown or unsafe asset path: {path}"
        );
    }
}

#[test]
fn static_client_hmr_endpoint_is_a_bounded_empty_sse_response() {
    let mut carrier = HttpCarrier::new(gateway());
    let response = carrier.handle_bytes(&raw_request("GET", "/plugins/events", &[], &[]));
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    assert_eq!(response.header("cache-control"), Some("no-cache"));
    assert_eq!(response.body(), b": static YunXi Web build\n\n");
}
