//! Dependency-light HTTP/1.1 carrier for the bounded Gateway facade.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use yunxi_web_contract::{
    ClientRequest, ClientResponse, EVENTS_HOST_METHOD, EVENTS_MUX_METHOD, EventChannel,
    MAX_FRAME_BYTES, RpcError, RpcId, RpcMessage, RpcResult,
};

use crate::assets::{EmbeddedWebAsset, embedded_web_asset};
use crate::sse::{MAX_SSE_EVENTS, MAX_SSE_RESPONSE_BYTES, encode_events, error_event};
use crate::{
    AGENT_PRESET_LIST_METHOD, COMMANDS_LIST_METHOD, CREDENTIALS_DESCRIBE_METHOD,
    DYNAMIC_CORDIS_INVENTORY_METHOD, DYNAMIC_CORDIS_SYNC_INSPECT_METHOD, GatewayBackend,
    HEALTH_STATUS_METHOD, HOST_DESCRIBE_METHOD, LLM_PROVIDERS_METHOD, PLUGIN_INVENTORY_LIST_METHOD,
    SESSION_CREATE_METHOD, SESSION_HISTORY_METHOD, SESSION_LIST_METHOD, SESSION_MODELS_METHOD,
    SESSION_PROMPT_METHOD, SETTINGS_DESCRIBE_METHOD, SETTINGS_MUTATE_METHOD,
    SETTINGS_REPLACE_METHOD, SETTINGS_UPDATE_METHOD, SKILL_LIST_METHOD, SUBAGENT_LIST_METHOD,
    WORKSPACE_LIST_METHOD,
};

pub const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_HTTP_BODY_BYTES: usize = MAX_FRAME_BYTES;
pub const MAX_HTTP_RESPONSE_BYTES: usize = MAX_SSE_RESPONSE_BYTES;
pub use crate::assets::MAX_WEB_ASSET_BYTES;
pub const DEFAULT_HTTP_READ_TIMEOUT: Duration = Duration::from_secs(10);

const SSE_STATIC_OVERHEAD_BYTES: usize = 32;
const HTTP_VERSION: &str = "HTTP/1.1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    pub fn new(
        method: impl Into<String>,
        path: impl Into<String>,
        headers: impl IntoIterator<Item = (String, String)>,
        body: Vec<u8>,
    ) -> Result<Self, HttpParseError> {
        let method = method.into();
        let path = path.into();
        if method.is_empty()
            || method.chars().any(|character| {
                !character.is_ascii() || character.is_control() || character.is_whitespace()
            })
        {
            return Err(HttpParseError::Malformed(
                "HTTP method is invalid".to_string(),
            ));
        }
        if !path.starts_with('/') {
            return Err(HttpParseError::Malformed(
                "HTTP path must be origin-form".to_string(),
            ));
        }
        if body.len() > MAX_HTTP_BODY_BYTES {
            return Err(HttpParseError::TooLarge {
                kind: "request body",
                maximum: MAX_HTTP_BODY_BYTES,
            });
        }
        let mut normalized = BTreeMap::new();
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            if name.is_empty()
                || name.chars().any(|character| {
                    !character.is_ascii() || character.is_control() || character.is_whitespace()
                })
            {
                return Err(HttpParseError::Malformed(
                    "HTTP header name is invalid".to_string(),
                ));
            }
            if value
                .chars()
                .any(|character| character.is_control() && character != '\t')
            {
                return Err(HttpParseError::Malformed(
                    "HTTP header value is invalid".to_string(),
                ));
            }
            if normalized.insert(name, value.trim().to_string()).is_some() {
                return Err(HttpParseError::Malformed(
                    "duplicate HTTP header".to_string(),
                ));
            }
        }
        Ok(Self {
            method,
            path,
            headers: normalized,
            body,
        })
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(
            format!("{HTTP_VERSION} {} {}\r\n", self.status, self.reason).as_bytes(),
        );
        for (name, value) in &self.headers {
            output.extend_from_slice(name.as_bytes());
            output.extend_from_slice(b": ");
            output.extend_from_slice(value.as_bytes());
            output.extend_from_slice(b"\r\n");
        }
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&self.body);
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpParseError {
    Incomplete,
    TooLarge { kind: &'static str, maximum: usize },
    Malformed(String),
}

impl fmt::Display for HttpParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => formatter.write_str("HTTP request is incomplete"),
            Self::TooLarge { kind, maximum } => write!(formatter, "{kind} exceeds {maximum} bytes"),
            Self::Malformed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for HttpParseError {}

#[derive(Clone, Default)]
pub struct ShutdownToken(Arc<AtomicBool>);

impl ShutdownToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_shutdown(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_shutdown(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl fmt::Debug for ShutdownToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShutdownToken")
            .field("shutdown", &self.is_shutdown())
            .finish()
    }
}

pub struct HttpCarrier<B> {
    backend: B,
    read_timeout: Duration,
}

impl<B> HttpCarrier<B>
where
    B: GatewayBackend,
    B::Error: fmt::Display,
{
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            read_timeout: DEFAULT_HTTP_READ_TIMEOUT,
        }
    }

    pub fn with_read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = read_timeout;
        self
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    pub fn handle_bytes(&mut self, bytes: &[u8]) -> HttpResponse {
        match parse_request(bytes) {
            Ok(request) => self.handle_request(request),
            Err(error) => parse_error_response(error),
        }
    }

    pub fn handle_request(&mut self, request: HttpRequest) -> HttpResponse {
        let path = request
            .path()
            .split_once('?')
            .map_or(request.path(), |(path, _)| path);
        match request.method() {
            "GET" if path == "/" => web_asset_response("/index.html"),
            "GET" if embedded_web_asset(path).is_some() => web_asset_response(path),
            "GET" if path == "/plugins/events" => static_plugin_events_response(),
            "GET" if api_path_matches(path, EVENTS_MUX_METHOD) => {
                self.handle_events(EventChannel::Mux)
            }
            "GET" if api_path_matches(path, EVENTS_HOST_METHOD) => {
                self.handle_events(EventChannel::Host)
            }
            "POST" => {
                if api_path_matches(path, "respond") {
                    return self.handle_response(request);
                }
                let Some(method) = unary_method(path) else {
                    return text_response(404, "not found");
                };
                self.handle_unary(request, method)
            }
            _ => text_response(404, "not found"),
        }
    }

    pub fn serve_connection(&mut self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(self.read_timeout))?;
        let response = match read_request(&mut stream) {
            Ok(request) => self.handle_request(request),
            Err(ReadRequestError::Parse(error)) => parse_error_response(error),
            Err(ReadRequestError::Io(error)) => return Err(error),
        };
        stream.write_all(&response.to_bytes())?;
        stream.flush()?;
        let _ignored = stream.shutdown(Shutdown::Both);
        Ok(())
    }

    pub fn serve_until(
        &mut self,
        listener: TcpListener,
        shutdown: &ShutdownToken,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        while !shutdown.is_shutdown() {
            match listener.accept() {
                Ok((stream, _peer)) => self.serve_connection(stream)?,
                Err(error) if is_would_block(&error) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn bind_loopback(port: u16) -> io::Result<TcpListener> {
        TcpListener::bind(("127.0.0.1", port))
    }

    fn handle_response(&mut self, request: HttpRequest) -> HttpResponse {
        if !has_json_content_type(&request) {
            return text_response(415, "content type must be application/json");
        }
        let value = match serde_json::from_slice::<Value>(request.body()) {
            Ok(value) => value,
            Err(_) => return text_response(400, "body is not JSON"),
        };
        let response = match serde_json::from_value::<ClientResponse>(value) {
            Ok(response) => response,
            Err(_) => {
                return response_from_value(json!({
                    "accepted": false,
                    "reason": "bad-response",
                }));
            }
        };
        self.backend.refresh();
        match self.backend.respond(&response) {
            Ok(receipt) => response_from_value(receipt),
            Err(error) => text_response(500, &format!("gateway response failure: {error}")),
        }
    }

    fn handle_unary(&mut self, request: HttpRequest, method: &str) -> HttpResponse {
        if !has_json_content_type(&request) {
            return text_response(415, "content type must be application/json");
        }
        let value = match serde_json::from_slice::<Value>(request.body()) {
            Ok(value) => value,
            Err(_) => return text_response(400, "body is not JSON"),
        };
        let client_request = match serde_json::from_value::<ClientRequest>(value.clone()) {
            Ok(request) => request,
            Err(error) => {
                return response_from_message(bad_request_response(
                    salvaged_rpc_id(&value),
                    error.to_string(),
                ));
            }
        };
        if client_request.method() != method {
            return response_from_message(bad_request_response(
                client_request.rpc_id().clone(),
                format!(
                    "method `{}` does not match path `{method}`",
                    client_request.method()
                ),
            ));
        }
        self.backend.refresh();
        match self.backend.dispatch(&client_request) {
            Ok(response) => response_from_message(RpcMessage::ServerResponse(response)),
            Err(error) => text_response(500, &format!("gateway handler failure: {error}")),
        }
    }

    fn handle_events(&mut self, channel: EventChannel) -> HttpResponse {
        self.backend.refresh();
        let raw_budget = MAX_SSE_RESPONSE_BYTES
            .saturating_sub(SSE_STATIC_OVERHEAD_BYTES)
            .saturating_sub(MAX_SSE_EVENTS * 8);
        let body = match self
            .backend
            .take_events(channel, MAX_SSE_EVENTS, raw_budget)
        {
            Ok(events) => encode_events(&events),
            Err(error) => error_event(channel, error.to_string()),
        };
        let body = match body {
            Ok(body) => body,
            Err(error) => return text_response(500, &format!("SSE carrier failure: {error}")),
        };
        response(
            200,
            "OK",
            vec![
                ("Content-Type".to_string(), "text/event-stream".to_string()),
                ("Cache-Control".to_string(), "no-cache".to_string()),
                ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
            ],
            body,
        )
    }
}

fn is_would_block(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(10035)
}

fn web_asset_response(path: &str) -> HttpResponse {
    let EmbeddedWebAsset {
        content_type,
        cache_control,
        body,
    } = embedded_web_asset(path).expect("Web asset path is checked before dispatch");
    response(
        200,
        "OK",
        vec![
            ("Content-Type".to_string(), content_type.to_string()),
            ("Cache-Control".to_string(), cache_control.to_string()),
            ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        ],
        body.to_vec(),
    )
}

fn static_plugin_events_response() -> HttpResponse {
    response(
        200,
        "OK",
        vec![
            ("Content-Type".to_string(), "text/event-stream".to_string()),
            ("Cache-Control".to_string(), "no-cache".to_string()),
            ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        ],
        b": static YunXi Web build\n\n".to_vec(),
    )
}

fn unary_method(path: &str) -> Option<&'static str> {
    match path.strip_prefix("/api/")? {
        AGENT_PRESET_LIST_METHOD => Some(AGENT_PRESET_LIST_METHOD),
        COMMANDS_LIST_METHOD => Some(COMMANDS_LIST_METHOD),
        CREDENTIALS_DESCRIBE_METHOD => Some(CREDENTIALS_DESCRIBE_METHOD),
        DYNAMIC_CORDIS_INVENTORY_METHOD => Some(DYNAMIC_CORDIS_INVENTORY_METHOD),
        DYNAMIC_CORDIS_SYNC_INSPECT_METHOD => Some(DYNAMIC_CORDIS_SYNC_INSPECT_METHOD),
        HEALTH_STATUS_METHOD => Some(HEALTH_STATUS_METHOD),
        HOST_DESCRIBE_METHOD => Some(HOST_DESCRIBE_METHOD),
        LLM_PROVIDERS_METHOD => Some(LLM_PROVIDERS_METHOD),
        PLUGIN_INVENTORY_LIST_METHOD => Some(PLUGIN_INVENTORY_LIST_METHOD),
        SESSION_CREATE_METHOD => Some(SESSION_CREATE_METHOD),
        SESSION_HISTORY_METHOD => Some(SESSION_HISTORY_METHOD),
        SESSION_LIST_METHOD => Some(SESSION_LIST_METHOD),
        SESSION_MODELS_METHOD => Some(SESSION_MODELS_METHOD),
        SESSION_PROMPT_METHOD => Some(SESSION_PROMPT_METHOD),
        SETTINGS_DESCRIBE_METHOD => Some(SETTINGS_DESCRIBE_METHOD),
        SETTINGS_MUTATE_METHOD => Some(SETTINGS_MUTATE_METHOD),
        SETTINGS_REPLACE_METHOD => Some(SETTINGS_REPLACE_METHOD),
        SETTINGS_UPDATE_METHOD => Some(SETTINGS_UPDATE_METHOD),
        SKILL_LIST_METHOD => Some(SKILL_LIST_METHOD),
        SUBAGENT_LIST_METHOD => Some(SUBAGENT_LIST_METHOD),
        WORKSPACE_LIST_METHOD => Some(WORKSPACE_LIST_METHOD),
        _ => None,
    }
}

fn has_json_content_type(request: &HttpRequest) -> bool {
    request
        .header("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
}

fn api_path_matches(path: &str, method: &str) -> bool {
    path.strip_prefix("/api/") == Some(method)
}

fn parse_request(bytes: &[u8]) -> Result<HttpRequest, HttpParseError> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        if bytes.len() > MAX_HTTP_HEADER_BYTES {
            return Err(HttpParseError::TooLarge {
                kind: "HTTP headers",
                maximum: MAX_HTTP_HEADER_BYTES,
            });
        }
        return Err(HttpParseError::Incomplete);
    };
    let header_bytes = &bytes[..header_end];
    if header_bytes.len() > MAX_HTTP_HEADER_BYTES {
        return Err(HttpParseError::TooLarge {
            kind: "HTTP headers",
            maximum: MAX_HTTP_HEADER_BYTES,
        });
    }
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| HttpParseError::Malformed("HTTP headers are not UTF-8".to_string()))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| HttpParseError::Malformed("HTTP request line is missing".to_string()))?;
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || parts[2] != HTTP_VERSION {
        return Err(HttpParseError::Malformed(
            "HTTP request line is invalid".to_string(),
        ));
    }
    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpParseError::Malformed(
                "HTTP header line is invalid".to_string(),
            ));
        };
        if name.trim() != name || name.is_empty() {
            return Err(HttpParseError::Malformed(
                "HTTP header name is invalid".to_string(),
            ));
        }
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(HttpParseError::Malformed(
                "chunked transfer encoding is not supported".to_string(),
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(HttpParseError::Malformed(
                    "duplicate content-length header".to_string(),
                ));
            }
            let parsed = value.parse::<usize>().map_err(|_| {
                HttpParseError::Malformed("content-length header is invalid".to_string())
            })?;
            if parsed > MAX_HTTP_BODY_BYTES {
                return Err(HttpParseError::TooLarge {
                    kind: "request body",
                    maximum: MAX_HTTP_BODY_BYTES,
                });
            }
            content_length = Some(parsed);
        }
        headers.push((name.to_string(), value));
    }
    let body_start = header_end + 4;
    let body_length = content_length.unwrap_or(0);
    let body_end = body_start
        .checked_add(body_length)
        .ok_or(HttpParseError::TooLarge {
            kind: "request body",
            maximum: MAX_HTTP_BODY_BYTES,
        })?;
    if bytes.len() < body_end {
        return Err(HttpParseError::Incomplete);
    }
    if bytes.len() != body_end {
        return Err(HttpParseError::Malformed(
            "pipelined HTTP requests are not supported".to_string(),
        ));
    }
    HttpRequest::new(
        parts[0],
        parts[1].split_once('?').map_or(parts[1], |(path, _)| path),
        headers,
        bytes[body_start..body_end].to_vec(),
    )
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, ReadRequestError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match parse_request(&bytes) {
            Ok(request) => return Ok(request),
            Err(HttpParseError::Incomplete) => {}
            Err(error) => return Err(ReadRequestError::Parse(error)),
        }
        if bytes.len() > MAX_HTTP_HEADER_BYTES + MAX_HTTP_BODY_BYTES + 4 {
            return Err(ReadRequestError::Parse(HttpParseError::TooLarge {
                kind: "HTTP request",
                maximum: MAX_HTTP_HEADER_BYTES + MAX_HTTP_BODY_BYTES + 4,
            }));
        }
        let read = stream.read(&mut buffer).map_err(ReadRequestError::Io)?;
        if read == 0 {
            return Err(ReadRequestError::Parse(HttpParseError::Incomplete));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

enum ReadRequestError {
    Parse(HttpParseError),
    Io(io::Error),
}

fn parse_error_response(error: HttpParseError) -> HttpResponse {
    match error {
        HttpParseError::TooLarge { .. } => text_response(413, "request too large"),
        HttpParseError::Incomplete | HttpParseError::Malformed(_) => {
            text_response(400, "bad request")
        }
    }
}

fn salvaged_rpc_id(value: &Value) -> RpcId {
    value
        .get("rpcId")
        .and_then(Value::as_str)
        .and_then(|value| RpcId::new(value).ok())
        .unwrap_or_else(|| RpcId::new("invalid-request").expect("static rpc id"))
}

fn bad_request_response(rpc_id: RpcId, message: String) -> RpcMessage {
    let error = RpcError::new(
        "bad-request",
        "invalid client-request message",
        json!({ "message": message }),
    )
    .expect("bounded bad request error");
    RpcMessage::server_response(rpc_id, RpcResult::failure(error))
        .expect("bounded bad request response")
}

fn response_from_message(message: RpcMessage) -> HttpResponse {
    match message.encode() {
        Ok(body) => response(
            200,
            "OK",
            vec![("Content-Type".to_string(), "application/json".to_string())],
            body,
        ),
        Err(error) => text_response(500, &format!("response encoding failed: {error}")),
    }
}

fn response_from_value(value: Value) -> HttpResponse {
    let body = match serde_json::to_vec(&value) {
        Ok(body) if body.len() <= MAX_HTTP_RESPONSE_BYTES => body,
        Ok(body) => {
            return text_response(
                500,
                &format!(
                    "JSON response is {} bytes; maximum is {}",
                    body.len(),
                    MAX_HTTP_RESPONSE_BYTES
                ),
            );
        }
        Err(error) => return text_response(500, &format!("response encoding failed: {error}")),
    };
    response(
        200,
        "OK",
        vec![("Content-Type".to_string(), "application/json".to_string())],
        body,
    )
}

fn text_response(status: u16, text: &str) -> HttpResponse {
    let (status, reason) = status_reason(status);
    let body = text.as_bytes().to_vec();
    response(
        status,
        reason,
        vec![(
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body,
    )
}

fn response(
    status: u16,
    reason: &'static str,
    mut headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> HttpResponse {
    headers.push(("Content-Length".to_string(), body.len().to_string()));
    headers.push(("Connection".to_string(), "close".to_string()));
    HttpResponse {
        status,
        reason,
        headers,
        body,
    }
}

fn status_reason(status: u16) -> (u16, &'static str) {
    match status {
        200 => (200, "OK"),
        400 => (400, "Bad Request"),
        404 => (404, "Not Found"),
        413 => (413, "Payload Too Large"),
        415 => (415, "Unsupported Media Type"),
        500 => (500, "Internal Server Error"),
        _ => (500, "Internal Server Error"),
    }
}
