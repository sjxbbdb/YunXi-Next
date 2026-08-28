use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use yunxi_protocol::{ActionGrant, NetworkGrant, SecretGrant, WorkspaceGrant};
use yunxi_tool_mcp::{McpClient, McpClientError, McpConfig};

#[derive(Clone, Debug)]
struct RecordedRequest {
    method: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn spawn_fixture(
    mode: &'static str,
    expected_requests: usize,
) -> (String, Arc<Mutex<Vec<RecordedRequest>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP fixture");
    let address = listener.local_addr().expect("fixture address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("accept HTTP request");
            let request = read_request(&mut stream).expect("read HTTP request");
            let method = request
                .body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            recorded
                .lock()
                .expect("record requests")
                .push(request.clone());

            if mode == "timeout" && method == "tools/call" {
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(500));
                    drop(stream);
                });
                continue;
            }
            if mode == "malformed" && method == "tools/list" {
                write_response(&mut stream, 200, "application/json", b"not-json");
                continue;
            }
            if mode == "oversized" && method == "tools/list" {
                let body = vec![b'x'; 1024 * 1024 + 1];
                write_response(&mut stream, 200, "application/json", &body);
                continue;
            }
            if mode == "status" && method == "tools/list" {
                write_response(&mut stream, 401, "application/json", b"{}");
                continue;
            }
            if mode == "remote-secret" && method == "tools/list" {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request.body.get("id").cloned().unwrap_or(Value::Null),
                    "error": {"code": -32001, "message": "fixture-secret-value"}
                });
                let body = serde_json::to_vec(&response).expect("encode secret error");
                write_response(&mut stream, 200, "application/json", &body);
                continue;
            }
            if method == "notifications/initialized" || method == "notifications/cancelled" {
                write_response(&mut stream, 204, "", &[]);
                continue;
            }

            let id = request.body.get("id").cloned().unwrap_or(Value::Null);
            let response = match method.as_str() {
                "initialize" => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "http-fixture", "version": "1.0.0"}
                    }
                }),
                "tools/list" => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Return the provided arguments.",
                            "inputSchema": {"type": "object", "additionalProperties": true}
                        }]
                    }
                }),
                "tools/call" => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": "http fixture"}],
                        "isError": false
                    }
                }),
                _ => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                }),
            };
            let body = serde_json::to_vec(&response).expect("encode HTTP response");
            if mode == "session" && method == "initialize" {
                write_response_with_session(
                    &mut stream,
                    200,
                    "application/json",
                    &body,
                    Some("fixture-session"),
                );
            } else if mode == "sse" && method == "tools/list" {
                let event = format!(
                    "event: message\ndata: {}\n\n",
                    String::from_utf8_lossy(&body)
                );
                write_response(&mut stream, 200, "text/event-stream", event.as_bytes());
            } else {
                write_response(&mut stream, 200, "application/json", &body);
            }
        }
    });
    (format!("http://{address}/mcp"), requests, handle)
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<RecordedRequest> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let method = first_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut headers = BTreeMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().expect("content length");
            }
            headers.insert(name, value);
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).expect("JSON-RPC request body")
    };
    Ok(RecordedRequest {
        method,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    write_response_with_session(stream, status, content_type, body, None);
}

fn write_response_with_session(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    session_id: Option<&str>,
) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
        body.len(),
        session_id
            .map(|session_id| format!("Mcp-Session-Id: {session_id}\r\n"))
            .unwrap_or_default()
    )
    .expect("write HTTP headers");
    stream.write_all(body).expect("write HTTP body");
    stream.flush().expect("flush HTTP response");
}

fn network(endpoint: &str) -> NetworkGrant {
    NetworkGrant::for_url(endpoint).expect("network scope")
}

#[test]
fn http_transport_discovers_calls_and_supports_sse() {
    let (endpoint, requests, server) = spawn_fixture("sse", 4);
    let config = McpConfig::http(
        &endpoint,
        "fixture",
        BTreeMap::from([("X-Fixture".to_string(), "yes".to_string())]),
    )
    .expect("HTTP config");
    let mut client = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        network(&endpoint),
        SecretGrant::empty(),
    )
    .expect("start HTTP MCP client");
    let result = client
        .call_tool("echo", json!({"text": "hello"}), Duration::from_secs(2))
        .expect("call HTTP tool");
    assert!(!result.is_error());
    assert_eq!(result.tool_name(), "echo");
    drop(client);
    server.join().expect("HTTP fixture server");
    let requests = requests.lock().expect("read recorded requests");
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().any(|request| request.method == "POST"));
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("x-fixture")
            .is_some_and(|value| value == "yes")
    }));
}

#[test]
fn http_transport_reuses_server_session_id_after_initialize() {
    let (endpoint, requests, server) = spawn_fixture("session", 4);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("HTTP config");
    let mut client = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        network(&endpoint),
        SecretGrant::empty(),
    )
    .expect("start HTTP MCP client");
    client
        .call_tool("echo", json!({}), Duration::from_secs(2))
        .expect("call HTTP tool");
    drop(client);
    server.join().expect("HTTP fixture server");
    let requests = requests.lock().expect("read recorded requests");
    assert_eq!(requests.len(), 4);
    assert!(!requests[0].headers.contains_key("mcp-session-id"));
    assert!(requests[1..].iter().all(|request| {
        request
            .headers
            .get("mcp-session-id")
            .is_some_and(|value| value == "fixture-session")
    }));
}

#[test]
fn http_secret_headers_require_a_per_call_secret_grant_and_never_cross_the_wire_protocol() {
    let (endpoint, requests, server) = spawn_fixture("normal", 4);
    let config = McpConfig::http_with_secrets(
        &endpoint,
        "fixture",
        BTreeMap::from([(
            "Authorization".to_string(),
            "secret://fixture/token".to_string(),
        )]),
        BTreeMap::from([(
            "fixture/token".to_string(),
            "fixture-secret-value".to_string(),
        )]),
    )
    .expect("HTTP config");
    let scope = network(&endpoint);
    let secret = SecretGrant::one("fixture/token").expect("secret grant");
    let mut client = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        scope.clone(),
        secret.clone(),
    )
    .expect("start HTTP MCP client");
    let action = ActionGrant::approved(WorkspaceGrant::read_only("C:\\workspace"), ".", "ticket")
        .with_network_grant(scope)
        .with_secret_grant(secret);
    let result = client
        .call_tool_with_grant("echo", json!({}), &action, Duration::from_secs(2))
        .expect("call authorized HTTP tool");
    assert!(!result.is_error());
    drop(client);
    server.join().expect("HTTP fixture server");
    let requests = requests.lock().expect("read recorded requests");
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .is_some_and(|value| value == "fixture-secret-value")
    }));
    let wire = serde_json::to_string(&action).expect("serialize action grant");
    assert!(!wire.contains("fixture-secret-value"));
    assert!(wire.contains("fixture/token"));
}

#[test]
fn http_scope_and_secret_denials_happen_before_a_request_is_sent() {
    let (endpoint, requests, server) = spawn_fixture("normal", 0);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("HTTP config");
    let error = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        NetworkGrant::for_url("https://other.example.test").expect("other scope"),
        SecretGrant::empty(),
    )
    .expect_err("network scope must deny endpoint");
    assert!(matches!(error, McpClientError::NetworkDenied { .. }));
    server.join().expect("HTTP fixture server");
    assert!(requests.lock().expect("read requests").is_empty());

    let (endpoint, requests, server) = spawn_fixture("normal", 0);
    let config = McpConfig::http_with_secrets(
        &endpoint,
        "fixture",
        BTreeMap::from([(
            "Authorization".to_string(),
            "secret://fixture/token".to_string(),
        )]),
        BTreeMap::from([(
            "fixture/token".to_string(),
            "fixture-secret-value".to_string(),
        )]),
    )
    .expect("HTTP config");
    let error = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        network(&endpoint),
        SecretGrant::empty(),
    )
    .expect_err("missing Secret grant must deny HTTP request");
    assert!(matches!(error, McpClientError::SecretDenied { .. }));
    server.join().expect("secret denial fixture");
    assert!(requests.lock().expect("read requests").is_empty());
}

#[test]
fn http_malformed_and_oversized_responses_are_fatal_boundaries() {
    let (endpoint, _requests, server) = spawn_fixture("malformed", 3);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("config");
    let error = McpClient::start(&config, Duration::from_secs(2))
        .expect_err("malformed HTTP response must fail");
    assert!(matches!(error, McpClientError::MalformedJson { .. }));
    assert!(error.is_fatal());
    server.join().expect("malformed fixture");

    let (endpoint, _requests, server) = spawn_fixture("oversized", 3);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("config");
    let error = McpClient::start(&config, Duration::from_secs(2))
        .expect_err("oversized HTTP response must fail");
    assert!(matches!(error, McpClientError::HttpResponseTooLarge { .. }));
    assert!(error.is_fatal());
    server.join().expect("oversized fixture");
}

#[test]
fn http_remote_error_diagnostics_redact_configured_secret_values() {
    let (endpoint, _requests, server) = spawn_fixture("remote-secret", 3);
    let config = McpConfig::http_with_secrets(
        &endpoint,
        "fixture",
        BTreeMap::from([(
            "Authorization".to_string(),
            "secret://fixture/token".to_string(),
        )]),
        BTreeMap::from([(
            "fixture/token".to_string(),
            "fixture-secret-value".to_string(),
        )]),
    )
    .expect("HTTP config");
    let error = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        network(&endpoint),
        SecretGrant::one("fixture/token").expect("secret grant"),
    )
    .expect_err("fixture remote error");
    let message = error.to_string();
    assert!(!message.contains("fixture-secret-value"));
    assert!(message.contains("<redacted>"));
    server.join().expect("secret fixture server");
}

#[test]
fn http_status_errors_keep_the_status_code_without_becoming_protocol_fatal() {
    let (endpoint, _requests, server) = spawn_fixture("status", 3);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("config");
    let error = McpClient::start(&config, Duration::from_secs(2))
        .expect_err("HTTP status error must fail startup");
    assert!(matches!(error, McpClientError::HttpStatus { status: 401 }));
    assert!(!error.is_fatal());
    server.join().expect("status fixture");
}

#[test]
fn http_timeout_sends_mcp_cancellation_notification() {
    let (endpoint, requests, server) = spawn_fixture("timeout", 5);
    let config = McpConfig::http(&endpoint, "fixture", BTreeMap::new()).expect("config");
    let mut client = McpClient::start_with_authority(
        &config,
        Duration::from_secs(2),
        network(&endpoint),
        SecretGrant::empty(),
    )
    .expect("start HTTP MCP client");
    let error = client
        .call_tool("echo", json!({}), Duration::from_millis(100))
        .expect_err("slow HTTP call must time out");
    assert!(matches!(error, McpClientError::Timeout { .. }));
    drop(client);
    server.join().expect("timeout fixture server");
    let requests = requests.lock().expect("read recorded requests");
    assert!(requests.iter().any(|request| {
        request.body.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
    }));
}
