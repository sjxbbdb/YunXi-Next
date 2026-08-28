//! Process-level coverage for the Web command and its shared Host boundary.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yunxi_web_contract::{EventChannel, RpcId, RpcMessage, RpcResult, parse_event_message};

#[test]
fn web_command_serves_chat_history_events_and_approval() {
    let workspace = unique_temp_dir("yunxi-web-host");
    fs::create_dir_all(&workspace).expect("create Web workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");
    let model_server = thread::spawn(move || serve_model_requests(model_listener));

    let mut web = WebChild::spawn_chat(&workspace, model_address);
    let address = web.wait_for_address();

    let created = rpc_call(address, "create-1", "session.create", json!({}));
    let session_id = created["sessionId"]
        .as_str()
        .expect("created session id")
        .to_string();
    let listed = rpc_call(address, "list-1", "session.list", json!({}));
    let created_summary = listed["items"]
        .as_array()
        .expect("session list items")
        .iter()
        .find(|summary| summary["sessionId"] == session_id)
        .expect("created session summary");
    assert_eq!(created_summary["running"], false);

    let first = rpc_call(
        address,
        "prompt-1",
        "session.prompt",
        json!({
            "sessionId": session_id,
            "mode": "queue",
            "content": [{ "type": "text", "text": "hello from web" }]
        }),
    );
    assert_eq!(first, json!({ "accepted": true }));

    let first_events = get_events(address, "/api/events.mux");
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/subscribed"
            && event.payload["sessionId"] == session_id
            && event.payload["lastSeq"] == -1
    }));
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "user/message"
            && event.payload["event"]["data"]["content"][0]["text"] == "hello from web"
    }));
    let first_host_events = get_events_for_channel(address, "/api/events.host", EventChannel::Host);
    let running_states = first_host_events
        .iter()
        .filter(|event| event.payload["type"] == "host/session-status")
        .map(|event| event.payload["running"].clone())
        .collect::<Vec<_>>();
    assert_eq!(running_states, vec![json!(true), json!(false)]);
    assert!(first_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "web fixture reply"
    }));

    let second = rpc_call(
        address,
        "prompt-2",
        "session.prompt",
        json!({
            "sessionId": session_id,
            "mode": "queue",
            "content": [{ "type": "text", "text": "run the approved command" }]
        }),
    );
    assert_eq!(second, json!({ "accepted": true }));

    let approval_events = get_events(address, "/api/events.mux");
    let approval = approval_events
        .iter()
        .find(|event| event.payload["type"] == "approval/requested")
        .expect("approval event");
    let approval_rpc_id = approval.rpc_id.clone();
    let approval_id = approval.payload["approvalId"]
        .as_str()
        .expect("approval id")
        .to_string();
    assert_eq!(
        approval.payload["sessionId"].as_str(),
        Some(session_id.as_str())
    );
    assert_eq!(approval.payload["toolName"].as_str(), Some("shell.execute"));

    let response = RpcMessage::client_response(
        RpcId::new(approval_rpc_id).expect("approval response id"),
        RpcResult::success(json!({
            "sessionId": session_id,
            "approvalId": approval_id,
            "outcome": "allowed-once"
        })),
    )
    .expect("approval response");
    let receipt = post_json(
        address,
        "/api/respond",
        response.encode().expect("encode response"),
    );
    assert_eq!(receipt, json!({ "accepted": true }));

    let resolved_events = get_events(address, "/api/events.mux");
    assert!(
        resolved_events
            .iter()
            .any(|event| event.payload["type"] == "approval/resolved")
    );
    assert!(resolved_events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "web approval complete"
    }));

    let history = rpc_call(
        address,
        "history-1",
        "session.history",
        json!({ "sessionId": session_id, "maxMessages": 16 }),
    );
    let history_events = history["events"].as_array().expect("history events");
    assert_eq!(history_events.len(), 12);
    assert_eq!(history_events[0]["event"]["type"], "turn/start");
    assert_eq!(history_events[1]["event"]["type"], "user/message");
    assert_eq!(
        history_events[3]["event"]["data"]["message"]["content"][0]["text"],
        "web fixture reply"
    );
    assert_eq!(history_events[5]["event"]["type"], "turn/end");
    assert_eq!(history_events[7]["event"]["type"], "user/message");
    assert_eq!(
        history_events[9]["event"]["data"]["message"]["content"][0]["text"],
        "web approval complete"
    );
    assert_eq!(history_events[11]["event"]["type"], "turn/end");
    assert_eq!(history["hasMore"], false);

    web.stop();
    model_server.join().expect("join model fixture");
    remove_workspace(&workspace);
}

#[test]
fn capability_settings_persist_and_apply_only_after_host_restart() {
    let workspace = unique_temp_dir("yunxi-web-settings");
    fs::create_dir_all(&workspace).expect("create settings workspace");

    let model_listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
    model_listener
        .set_nonblocking(true)
        .expect("make model fixture nonblocking");
    let model_address = model_listener.local_addr().expect("model fixture address");

    let mut web = WebChild::spawn_settings(&workspace, model_address);
    let address = web.wait_for_address();
    let initial_health = rpc_call(address, "health-initial", "health.status", json!({}));
    let initial_capabilities = initial_health["capabilities"]
        .as_u64()
        .expect("initial capability count");

    let described = rpc_call(address, "settings-describe", "settings.describe", json!({}));
    assert_eq!(described["writable"], true);
    let capabilities = settings_namespace(&described);
    assert_eq!(capabilities["revision"], 0);
    assert_eq!(capabilities["value"]["context"], true);
    assert_eq!(capabilities["applies"], "restart");
    let encoded_description = described.to_string();
    assert!(!encoded_description.contains("fixture-secret"));
    assert!(!encoded_description.contains(&workspace.to_string_lossy().to_string()));

    let updated = rpc_call(
        address,
        "settings-update",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "context": false },
            "expectedRevision": 0,
        }),
    );
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["value"]["context"], false);
    assert_eq!(updated["user"]["context"], false);

    let settings_path = workspace.join("next-home").join("settings.json");
    let persisted: Value = serde_json::from_slice(
        &fs::read(&settings_path).expect("read persisted capability settings"),
    )
    .expect("decode persisted capability settings");
    assert_eq!(persisted["version"], 1);
    assert_eq!(persisted["revision"], 1);
    assert_eq!(persisted["capabilities"]["context"], false);
    assert!(!persisted.to_string().contains("fixture-secret"));

    let host_events = get_events_for_channel(address, "/api/events.host", EventChannel::Host);
    assert!(host_events.iter().any(|event| {
        event.payload["type"] == "host/remote-event"
            && event.payload["event"] == "settings/document-updated"
            && event.payload["args"] == json!(["yunxi-capabilities", 1])
    }));

    let RpcResult::Failure(conflict) = rpc_result(
        address,
        "settings-conflict",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "shell": true },
            "expectedRevision": 0,
        }),
    ) else {
        panic!("stale settings write must fail");
    };
    assert_eq!(conflict.code(), "settings-conflict");
    assert_eq!(conflict.details()["currentRevision"], 1);

    let RpcResult::Failure(unknown_field) = rpc_result(
        address,
        "settings-unknown",
        "settings.update",
        json!({
            "ns": "yunxi-capabilities",
            "patch": { "unknown": true },
            "expectedRevision": 1,
        }),
    ) else {
        panic!("unknown capability field must fail");
    };
    assert_eq!(unknown_field.code(), "settings-rejected");

    let RpcResult::Failure(invalid_path) = rpc_result(
        address,
        "settings-path",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "set", "path": ["context", "nested"], "value": true }],
            "expectedRevision": 1,
        }),
    ) else {
        panic!("nested capability path must fail");
    };
    assert_eq!(invalid_path.code(), "invalid-payload");

    let mutated = rpc_call(
        address,
        "settings-mutate",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "set", "path": ["files"], "value": true }],
            "expectedRevision": 1,
        }),
    );
    assert_eq!(mutated["revision"], 2);
    assert_eq!(mutated["user"]["files"], true);

    let replaced = rpc_call(
        address,
        "settings-replace",
        "settings.replace",
        json!({
            "ns": "yunxi-capabilities",
            "section": { "context": false, "storage": true },
            "expectedRevision": 2,
        }),
    );
    assert_eq!(replaced["revision"], 3);
    assert!(replaced["user"].get("files").is_none());
    assert_eq!(replaced["user"]["storage"], true);

    let unset = rpc_call(
        address,
        "settings-unset",
        "settings.mutate",
        json!({
            "ns": "yunxi-capabilities",
            "ops": [{ "op": "unset", "path": ["storage"] }],
            "expectedRevision": 3,
        }),
    );
    assert_eq!(unset["revision"], 4);
    assert_eq!(unset["user"], json!({ "context": false }));

    let live_health = rpc_call(address, "health-live", "health.status", json!({}));
    assert_eq!(live_health["capabilities"], initial_capabilities);
    let live_inventory = rpc_call(address, "inventory-live", "pluginInventory/list", json!({}));
    assert_eq!(
        inventory_entry(&live_inventory, "yunxi.context")["enabled"],
        true
    );
    web.stop();

    let model_server = thread::spawn(move || {
        let (stream, body) = accept_request(&model_listener);
        assert!(
            !body.contains("YunXi Next Development Instructions"),
            "disabled Context route leaked into model request: {body}"
        );
        write_response(
            stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"settings restart applied"},"finish_reason":"stop"}]}"#,
        );
    });

    let mut restarted = WebChild::spawn_settings(&workspace, model_address);
    let restarted_address = restarted.wait_for_address();
    let restarted_settings = rpc_call(
        restarted_address,
        "settings-restarted",
        "settings.describe",
        json!({}),
    );
    assert_eq!(settings_namespace(&restarted_settings)["revision"], 4);
    assert_eq!(
        settings_namespace(&restarted_settings)["user"],
        json!({ "context": false })
    );

    let restarted_inventory = rpc_call(
        restarted_address,
        "inventory-restarted",
        "pluginInventory/list",
        json!({}),
    );
    let context = inventory_entry(&restarted_inventory, "yunxi.context");
    assert_eq!(context["enabled"], false);
    assert_eq!(context["fiberPhase"], Value::Null);
    let restarted_health = rpc_call(
        restarted_address,
        "health-restarted",
        "health.status",
        json!({}),
    );
    assert_eq!(restarted_health["capabilities"], initial_capabilities - 1);

    let created = rpc_call(
        restarted_address,
        "restart-session",
        "session.create",
        json!({}),
    );
    let session_id = created["sessionId"].as_str().expect("session id");
    assert_eq!(
        rpc_call(
            restarted_address,
            "restart-prompt",
            "session.prompt",
            json!({
                "sessionId": session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": "verify restarted settings" }],
            }),
        ),
        json!({ "accepted": true })
    );
    let events = get_events(restarted_address, "/api/events.mux");
    assert!(events.iter().any(|event| {
        event.payload["type"] == "session/event"
            && event.payload["event"]["type"] == "assistant/message"
            && event.payload["event"]["data"]["message"]["content"][0]["text"]
                == "settings restart applied"
    }));

    restarted.stop();
    model_server.join().expect("join restarted model fixture");
    remove_workspace(&workspace);
}

struct WebChild {
    child: Child,
    stdout: BufReader<std::process::ChildStdout>,
}

impl WebChild {
    fn spawn_chat(workspace: &std::path::Path, model_address: SocketAddr) -> Self {
        let mut command = Self::command(workspace, model_address);
        command
            .env("YUNXI_NEXT_CONTEXT_ENABLED", "false")
            .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
            .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
            .env("YUNXI_NEXT_SHELL_ENABLED", "true");
        Self::start(command)
    }

    fn spawn_settings(workspace: &std::path::Path, model_address: SocketAddr) -> Self {
        let mut command = Self::command(workspace, model_address);
        command
            .env_remove("YUNXI_NEXT_CONTEXT_ENABLED")
            .env("YUNXI_NEXT_PERSONA_ENABLED", "false")
            .env("YUNXI_NEXT_MEMORY_ENABLED", "false")
            .env("YUNXI_NEXT_COMPANION_ENABLED", "false")
            .env("YUNXI_NEXT_STORAGE_ENABLED", "true")
            .env("YUNXI_NEXT_MAILBOX_ENABLED", "false")
            .env("YUNXI_NEXT_SCHEDULER_ENABLED", "false")
            .env("YUNXI_NEXT_SHELL_ENABLED", "false")
            .env("YUNXI_NEXT_PATCH_ENABLED", "false")
            .env("YUNXI_NEXT_FILES_ENABLED", "false")
            .env("YUNXI_NEXT_MCP_ENABLED", "false")
            .env("YUNXI_NEXT_SKILLS_ENABLED", "false");
        Self::start(command)
    }

    fn command(workspace: &std::path::Path, model_address: SocketAddr) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yunxi-next"));
        command
            .arg("web")
            .args(["--bind", "127.0.0.1:0"])
            .current_dir(workspace)
            .env("YUNXI_NEXT_HOME", workspace.join("next-home"))
            .env("YUNXI_PROVIDER_PROFILE", "fixture")
            .env(
                "YUNXI_PROVIDER_BASE_URL",
                format!("http://{model_address}/v1"),
            )
            .env("YUNXI_PROVIDER_API_KEY", "fixture-secret")
            .env("YUNXI_AGENT_MODEL", "fixture-model")
            .env("YUNXI_PROVIDER_TIMEOUT_MILLIS", "3000")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        command
    }

    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch Web command");
        let stdout = BufReader::new(child.stdout.take().expect("Web stdout"));
        Self { child, stdout }
    }

    fn wait_for_address(&mut self) -> SocketAddr {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read Web startup line");
        line.trim()
            .strip_prefix("YunXi Next Web listening on http://")
            .expect("Web startup address")
            .parse()
            .expect("Web socket address")
    }

    fn stop(&mut self) {
        if self.child.try_wait().expect("poll Web command").is_none() {
            drop(self.child.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if self.child.try_wait().expect("poll Web shutdown").is_some() {
                    return;
                }
                if Instant::now() >= deadline {
                    let _ignored = self.child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        let _ignored = self.child.wait();
    }
}

impl Drop for WebChild {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_model_requests(listener: TcpListener) {
    let (first_stream, first_body) = accept_request(&listener);
    assert!(first_body.contains("hello from web"));
    write_response(
        first_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"web fixture reply"},"finish_reason":"stop"}]}"#,
    );

    let (second_stream, second_body) = accept_request(&listener);
    assert!(second_body.contains("shell.execute"));
    write_response(
        second_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"web-call-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo web-action\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    );

    let (third_stream, third_body) = accept_request(&listener);
    assert!(third_body.contains("\"role\":\"tool\""));
    assert!(third_body.contains("web-action"));
    write_response(
        third_stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"web approval complete"},"finish_reason":"stop"}]}"#,
    );
}

fn rpc_call(address: SocketAddr, id: &str, method: &str, payload: Value) -> Value {
    match rpc_result(address, id, method, payload) {
        RpcResult::Success(value) => value,
        RpcResult::Failure(error) => panic!("Web RPC failed: {error:?}"),
    }
}

fn rpc_result(address: SocketAddr, id: &str, method: &str, payload: Value) -> RpcResult<Value> {
    let request = RpcMessage::client_request(RpcId::new(id).expect("RPC id"), method, payload)
        .expect("client request")
        .encode()
        .expect("encode request");
    let body = post_json(address, &format!("/api/{method}"), request);
    let message = serde_json::from_value(body).expect("server response");
    let RpcMessage::ServerResponse(response) = message else {
        panic!("unary Web response must be a server response");
    };
    response.result().clone()
}

fn settings_namespace(document: &Value) -> &Value {
    document["namespaces"]
        .as_array()
        .expect("settings namespaces")
        .iter()
        .find(|namespace| namespace["ns"] == "yunxi-capabilities")
        .expect("capability settings namespace")
}

fn inventory_entry<'a>(inventory: &'a Value, entry_id: &str) -> &'a Value {
    inventory["entries"]
        .as_array()
        .expect("plugin inventory entries")
        .iter()
        .find(|entry| entry["entryId"] == entry_id)
        .expect("plugin inventory entry")
}

struct EventFrame {
    rpc_id: String,
    payload: Value,
}

fn get_events(address: SocketAddr, path: &str) -> Vec<EventFrame> {
    get_events_for_channel(address, path, EventChannel::Mux)
}

fn get_events_for_channel(
    address: SocketAddr,
    path: &str,
    expected_channel: EventChannel,
) -> Vec<EventFrame> {
    let body = request(address, "GET", path, &[]);
    let text = String::from_utf8(body).expect("SSE UTF-8");
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| {
            let message = RpcMessage::decode(line.as_bytes()).expect("SSE event message");
            let (channel, rpc_id, payload) = parse_event_message(&message).expect("event frame");
            assert_eq!(channel, expected_channel);
            EventFrame {
                rpc_id: rpc_id.as_str().to_string(),
                payload: payload.clone(),
            }
        })
        .collect()
}

fn post_json(address: SocketAddr, path: &str, body: Vec<u8>) -> Value {
    serde_json::from_slice(&request(address, "POST", path, &body)).expect("HTTP JSON")
}

fn request(address: SocketAddr, method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).expect("connect Web command");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request headers");
    stream.write_all(body).expect("write HTTP request body");
    stream
        .shutdown(Shutdown::Write)
        .expect("finish HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HTTP response");
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response separator");
    let headers = String::from_utf8_lossy(&response[..separator]);
    assert!(
        headers.starts_with("HTTP/1.1 200 OK"),
        "HTTP response: {headers}"
    );
    response[separator + 4..].to_vec()
}

fn accept_request(listener: &TcpListener) -> (TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "model fixture received no request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("model fixture accept failed: {error}"),
        }
    };
    stream
        .set_nonblocking(false)
        .expect("make accepted model stream blocking");
    let mut reader = BufReader::new(stream.try_clone().expect("clone model stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read model request line");
    assert!(request_line.starts_with("POST /v1/chat/completions "));

    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read model header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>().expect("content length"));
            }
        }
    }
    let mut body = vec![0; content_length.expect("model content length")];
    reader
        .read_exact(&mut body)
        .expect("read model request body");
    (
        stream,
        String::from_utf8(body).expect("model request UTF-8"),
    )
}

fn write_response(mut stream: TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write model response");
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
}

fn remove_workspace(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error)
                if error.kind() != std::io::ErrorKind::NotFound && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("remove Web workspace: {error}"),
        }
    }
}
