//! Bounded JSON-RPC client for one MCP stdio or HTTP Server.

use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yunxi_protocol::{
    ActionGrant, MCP_PROTOCOL_VERSION, McpToolCallResult, McpToolDescriptor, McpToolListResult,
    NetworkGrant, SecretGrant,
};

use crate::McpConfig;
use crate::config::McpTransportKind;
use crate::http::HttpTransport;

pub const MAX_MCP_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_MCP_RESULT_BYTES: usize = 1024 * 1024;
const INITIALIZE_METHOD: &str = "initialize";
const INITIALIZED_NOTIFICATION: &str = "notifications/initialized";
const TOOLS_LIST_METHOD: &str = "tools/list";
const TOOLS_CALL_METHOD: &str = "tools/call";
const CANCELLED_NOTIFICATION: &str = "notifications/cancelled";

#[derive(Debug)]
pub struct McpClient {
    backend: McpBackend,
    next_request_id: u64,
    server_name: String,
    tools: Vec<McpToolDescriptor>,
}

#[derive(Debug)]
enum McpBackend {
    Stdio {
        child: Child,
        stdin: ChildStdin,
        events: Receiver<ReaderEvent>,
        redactions: Vec<String>,
    },
    Http(HttpTransport),
}

impl McpClient {
    pub fn start(config: &McpConfig, timeout: Duration) -> Result<Self, McpClientError> {
        let network_grant = match config.transport_kind() {
            McpTransportKind::Stdio => NetworkGrant::none(),
            // This compatibility constructor is intended for direct client
            // users. The plugin path uses `start_with_authority` below.
            McpTransportKind::Http => NetworkGrant::unrestricted(),
        };
        Self::start_with_authority(config, timeout, network_grant, SecretGrant::empty())
    }

    pub fn start_with_authority(
        config: &McpConfig,
        timeout: Duration,
        network_grant: NetworkGrant,
        secret_grant: SecretGrant,
    ) -> Result<Self, McpClientError> {
        let backend = match config.transport_kind() {
            McpTransportKind::Stdio => spawn_stdio(config)?,
            McpTransportKind::Http => McpBackend::Http(HttpTransport::new(
                config,
                timeout,
                network_grant,
                secret_grant,
            )?),
        };
        let mut client = Self {
            backend,
            next_request_id: 1,
            server_name: config.server_name().to_string(),
            tools: Vec::new(),
        };

        let initialize = client.request(
            INITIALIZE_METHOD,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "yunxi-next-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
            timeout,
            None,
            None,
        )?;
        validate_initialize_result(&initialize)?;
        client.send_notification(INITIALIZED_NOTIFICATION, json!({}))?;
        client.list_tools(timeout)?;
        Ok(client)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tools(&self) -> &[McpToolDescriptor] {
        &self.tools
    }

    pub fn list_tools(&mut self, timeout: Duration) -> Result<McpToolListResult, McpClientError> {
        let value = self.request(TOOLS_LIST_METHOD, json!({}), timeout, None, None)?;
        let object = value
            .as_object()
            .ok_or_else(|| McpClientError::InvalidResponse {
                method: TOOLS_LIST_METHOD,
                message: "tools/list result must be an object".to_string(),
            })?;
        let raw_tools =
            object
                .get("tools")
                .cloned()
                .ok_or_else(|| McpClientError::InvalidResponse {
                    method: TOOLS_LIST_METHOD,
                    message: "tools/list result has no tools array".to_string(),
                })?;
        let tools =
            serde_json::from_value::<Vec<McpToolDescriptor>>(raw_tools).map_err(|error| {
                McpClientError::InvalidResponse {
                    method: TOOLS_LIST_METHOD,
                    message: format!("invalid tool descriptor: {error}"),
                }
            })?;
        let truncated = object
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null());
        let result = McpToolListResult::new(self.server_name.clone(), tools, truncated).map_err(
            |error| McpClientError::InvalidResponse {
                method: TOOLS_LIST_METHOD,
                message: error.to_string(),
            },
        )?;
        self.tools = result.tools().to_vec();
        Ok(result)
    }

    pub fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<McpToolCallResult, McpClientError> {
        self.call_tool_with_authority(tool_name, arguments, None, None, timeout)
    }

    pub fn call_tool_with_grant(
        &mut self,
        tool_name: &str,
        arguments: Value,
        grant: &ActionGrant,
        timeout: Duration,
    ) -> Result<McpToolCallResult, McpClientError> {
        self.call_tool_with_authority(
            tool_name,
            arguments,
            Some(grant.network_grant()),
            Some(grant.secret_grant()),
            timeout,
        )
    }

    fn call_tool_with_authority(
        &mut self,
        tool_name: &str,
        arguments: Value,
        network_grant: Option<&NetworkGrant>,
        secret_grant: Option<&SecretGrant>,
        timeout: Duration,
    ) -> Result<McpToolCallResult, McpClientError> {
        if !self.tools.iter().any(|tool| tool.name() == tool_name) {
            return Err(McpClientError::UnknownTool {
                name: tool_name.to_string(),
            });
        }
        let value = self.request(
            TOOLS_CALL_METHOD,
            json!({"name": tool_name, "arguments": arguments}),
            timeout,
            network_grant,
            secret_grant,
        )?;
        let is_error = value
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        McpToolCallResult::new(
            self.server_name.clone(),
            tool_name.to_string(),
            value,
            is_error,
        )
        .map_err(|error| McpClientError::InvalidResponse {
            method: TOOLS_CALL_METHOD,
            message: error.to_string(),
        })
    }

    fn request(
        &mut self,
        method: &'static str,
        params: Value,
        timeout: Duration,
        network_grant: Option<&NetworkGrant>,
        secret_grant: Option<&SecretGrant>,
    ) -> Result<Value, McpClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).unwrap_or(1);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: request_id,
            method,
            params,
        };
        let result = if matches!(&self.backend, McpBackend::Http(_)) {
            let request =
                serde_json::to_value(&request).map_err(|error| McpClientError::Encode {
                    message: error.to_string(),
                })?;
            let response = {
                let McpBackend::Http(transport) = &mut self.backend else {
                    unreachable!("backend kind changed while building request")
                };
                transport.request(&request, method, timeout, network_grant, secret_grant)
            };
            response.and_then(|value| self.validate_response(value, request_id, method))
        } else {
            self.write_message(&request)?;
            self.receive_response(request_id, method, timeout)
        };
        match result {
            Err(error @ McpClientError::Timeout { .. }) => {
                let _ignored = self.cancel_request_with_authority(
                    request_id,
                    "request timed out",
                    network_grant,
                    secret_grant,
                );
                Err(error)
            }
            result => result,
        }
    }

    fn send_notification(
        &mut self,
        method: &'static str,
        params: Value,
    ) -> Result<(), McpClientError> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };
        if matches!(&self.backend, McpBackend::Http(_)) {
            let payload =
                serde_json::to_value(&notification).map_err(|error| McpClientError::Encode {
                    message: error.to_string(),
                })?;
            let McpBackend::Http(transport) = &mut self.backend else {
                unreachable!("backend kind changed while sending notification")
            };
            transport.notification(&payload, method)
        } else {
            self.write_message(&notification)
        }
    }

    pub fn cancel_request(
        &mut self,
        request_id: u64,
        reason: impl Into<String>,
    ) -> Result<(), McpClientError> {
        self.cancel_request_with_authority(request_id, reason, None, None)
    }

    fn cancel_request_with_authority(
        &mut self,
        request_id: u64,
        reason: impl Into<String>,
        network_grant: Option<&NetworkGrant>,
        secret_grant: Option<&SecretGrant>,
    ) -> Result<(), McpClientError> {
        if request_id == 0 {
            return Err(McpClientError::InvalidCancellation {
                message: "request id must be greater than zero".to_string(),
            });
        }
        let reason = reason.into();
        if reason.len() > 1024 || reason.chars().any(char::is_control) {
            return Err(McpClientError::InvalidCancellation {
                message: "cancellation reason is invalid or too long".to_string(),
            });
        }
        if matches!(&self.backend, McpBackend::Http(_)) {
            let McpBackend::Http(transport) = &mut self.backend else {
                unreachable!("backend kind changed while cancelling request")
            };
            transport.cancel(request_id, &reason, network_grant, secret_grant)
        } else {
            self.send_notification(
                CANCELLED_NOTIFICATION,
                json!({"requestId": request_id, "reason": reason}),
            )
        }
    }

    fn write_message<T: Serialize>(&mut self, message: &T) -> Result<(), McpClientError> {
        let bytes = serde_json::to_vec(message).map_err(|error| McpClientError::Encode {
            message: error.to_string(),
        })?;
        if bytes.len() + 1 > MAX_MCP_FRAME_BYTES {
            return Err(McpClientError::FrameTooLarge {
                size: bytes.len() + 1,
                maximum: MAX_MCP_FRAME_BYTES,
            });
        }
        let McpBackend::Stdio { stdin, .. } = &mut self.backend else {
            return Err(McpClientError::HttpConfiguration {
                message: "stdio message was sent to an HTTP transport".to_string(),
            });
        };
        stdin.write_all(&bytes).map_err(McpClientError::Io)?;
        stdin.write_all(b"\n").map_err(McpClientError::Io)?;
        stdin.flush().map_err(McpClientError::Io)
    }

    fn receive_response(
        &mut self,
        expected_id: u64,
        method: &'static str,
        timeout: Duration,
    ) -> Result<Value, McpClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(McpClientError::Timeout { method, timeout });
            }
            let event =
                self.backend_events()
                    .recv_timeout(remaining)
                    .map_err(|error| match error {
                        RecvTimeoutError::Timeout => McpClientError::Timeout { method, timeout },
                        RecvTimeoutError::Disconnected => McpClientError::ChildStopped,
                    })?;
            let frame = match event {
                ReaderEvent::Frame(frame) => frame,
                ReaderEvent::Error(message) => return Err(McpClientError::Read { message }),
                ReaderEvent::Eof => return Err(McpClientError::ChildStopped),
            };
            let value = serde_json::from_slice::<Value>(&frame).map_err(|error| {
                McpClientError::MalformedJson {
                    message: error.to_string(),
                }
            })?;
            let object = value
                .as_object()
                .ok_or_else(|| McpClientError::MalformedJson {
                    message: "JSON-RPC message must be an object".to_string(),
                })?;
            if object.contains_key("method") && !object.contains_key("id") {
                continue;
            }
            return self.validate_response(value, expected_id, method);
        }
    }

    fn validate_response(
        &self,
        value: Value,
        expected_id: u64,
        method: &'static str,
    ) -> Result<Value, McpClientError> {
        let response = serde_json::from_value::<JsonRpcResponse>(value).map_err(|error| {
            McpClientError::MalformedJson {
                message: error.to_string(),
            }
        })?;
        if response.jsonrpc != "2.0" {
            return Err(McpClientError::InvalidResponse {
                method,
                message: "JSON-RPC version must be 2.0".to_string(),
            });
        }
        if response.id != Some(json!(expected_id)) {
            return Err(McpClientError::UnexpectedResponseId {
                expected: expected_id,
                received: response.id,
            });
        }
        if let Some(error) = response.error {
            return Err(McpClientError::Remote {
                code: error.code,
                message: self.redact_text(&error.message),
            });
        }
        let Some(result) = response.result else {
            return Err(McpClientError::InvalidResponse {
                method,
                message: "JSON-RPC response has neither result nor error".to_string(),
            });
        };
        let size = serde_json::to_vec(&result)
            .map_err(|error| McpClientError::Encode {
                message: error.to_string(),
            })?
            .len();
        if size > MAX_MCP_RESULT_BYTES {
            return Err(McpClientError::FrameTooLarge {
                size,
                maximum: MAX_MCP_RESULT_BYTES,
            });
        }
        Ok(self.redact_value(result))
    }

    fn redact_text(&self, value: &str) -> String {
        match &self.backend {
            McpBackend::Stdio { redactions, .. } => redact_text(value, redactions),
            McpBackend::Http(transport) => transport.redact_text(value),
        }
    }

    fn redact_value(&self, value: Value) -> Value {
        match value {
            Value::String(value) => Value::String(self.redact_text(&value)),
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|value| self.redact_value(value))
                    .collect(),
            ),
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, self.redact_value(value)))
                    .collect(),
            ),
            value => value,
        }
    }

    fn backend_events(&mut self) -> &mut Receiver<ReaderEvent> {
        match &mut self.backend {
            McpBackend::Stdio { events, .. } => events,
            McpBackend::Http(_) => unreachable!("HTTP responses do not use reader events"),
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let McpBackend::Stdio { child, .. } = &mut self.backend {
            terminate_child(child);
        }
    }
}

fn spawn_stdio(config: &McpConfig) -> Result<McpBackend, McpClientError> {
    let mut command = Command::new(config.command());
    command
        .args(config.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .envs(config.environment());
    let mut child = command.spawn().map_err(|error| McpClientError::Spawn {
        command: config.command().display().to_string(),
        message: error.to_string(),
    })?;
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            terminate_child(&mut child);
            return Err(McpClientError::Spawn {
                command: config.command().display().to_string(),
                message: "MCP child stdin was not piped".to_string(),
            });
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_child(&mut child);
            return Err(McpClientError::Spawn {
                command: config.command().display().to_string(),
                message: "MCP child stdout was not piped".to_string(),
            });
        }
    };
    Ok(McpBackend::Stdio {
        child,
        stdin,
        events: spawn_reader(stdout),
        redactions: config
            .environment()
            .iter()
            .filter(|(key, value)| is_sensitive_environment_key(key) && !value.is_empty())
            .map(|(_, value)| value.clone())
            .collect(),
    })
}

fn is_sensitive_environment_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "AUTH"]
        .iter()
        .any(|marker| key.contains(marker))
}

fn redact_text(value: &str, redactions: &[String]) -> String {
    redactions
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(value.to_string(), |value, secret| {
            value.replace(secret, "<redacted>")
        })
}

fn terminate_child(child: &mut Child) {
    let _ignored = child.kill();
    let _ignored = child.wait();
}

fn validate_initialize_result(value: &Value) -> Result<(), McpClientError> {
    let object = value
        .as_object()
        .ok_or_else(|| McpClientError::InvalidResponse {
            method: INITIALIZE_METHOD,
            message: "initialize result must be an object".to_string(),
        })?;
    let protocol = object
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| McpClientError::InvalidResponse {
            method: INITIALIZE_METHOD,
            message: "initialize result has no protocolVersion".to_string(),
        })?;
    if protocol.len() > 64 {
        return Err(McpClientError::InvalidResponse {
            method: INITIALIZE_METHOD,
            message: "initialize protocolVersion is too long".to_string(),
        });
    }
    Ok(())
}

fn spawn_reader(stdout: ChildStdout) -> Receiver<ReaderEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || read_stdout(stdout, sender));
    receiver
}

fn read_stdout(stdout: ChildStdout, sender: Sender<ReaderEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_frame(&mut reader) {
            Ok(Some(frame)) => {
                if sender.send(ReaderEvent::Frame(frame)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ignored = sender.send(ReaderEvent::Eof);
                return;
            }
            Err(error) => {
                let _ignored = sender.send(ReaderEvent::Error(error.to_string()));
                return;
            }
        }
    }
}

fn read_frame(reader: &mut BufReader<ChildStdout>) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available_len = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if frame.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MCP Server closed stdout before a JSON-RPC newline",
                ));
            }
            if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
                let size = frame.len() + position + 1;
                if size > MAX_MCP_FRAME_BYTES {
                    reader.consume(position + 1);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "MCP JSON-RPC frame exceeds the maximum size",
                    ));
                }
                frame.extend_from_slice(&available[..position]);
                reader.consume(position + 1);
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return Ok(Some(frame));
            }
            available.len()
        };
        if frame.len() + available_len >= MAX_MCP_FRAME_BYTES {
            let remaining = MAX_MCP_FRAME_BYTES.saturating_sub(frame.len());
            reader.consume(remaining.min(available_len));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP JSON-RPC frame exceeds the maximum size",
            ));
        }
        let available = reader.fill_buf()?;
        frame.extend_from_slice(&available[..available_len]);
        reader.consume(available_len);
    }
}
#[derive(Debug)]
enum ReaderEvent {
    Frame(Vec<u8>),
    Error(String),
    Eof,
}

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug)]
pub enum McpClientError {
    Spawn {
        command: String,
        message: String,
    },
    Io(io::Error),
    HttpTransport(reqwest::Error),
    HttpBodyRead(io::Error),
    HttpStatus {
        status: u16,
    },
    HttpResponseTooLarge {
        maximum: usize,
    },
    HttpConfiguration {
        message: String,
    },
    NetworkDenied {
        endpoint: String,
    },
    SecretDenied {
        reference: String,
    },
    InvalidCancellation {
        message: String,
    },
    Read {
        message: String,
    },
    Encode {
        message: String,
    },
    MalformedJson {
        message: String,
    },
    InvalidResponse {
        method: &'static str,
        message: String,
    },
    UnexpectedResponseId {
        expected: u64,
        received: Option<Value>,
    },
    Remote {
        code: i64,
        message: String,
    },
    UnknownTool {
        name: String,
    },
    Timeout {
        method: &'static str,
        timeout: Duration,
    },
    ChildStopped,
    FrameTooLarge {
        size: usize,
        maximum: usize,
    },
}

impl McpClientError {
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Spawn { .. }
                | Self::Io(_)
                | Self::HttpTransport(_)
                | Self::HttpBodyRead(_)
                | Self::Read { .. }
                | Self::Encode { .. }
                | Self::MalformedJson { .. }
                | Self::InvalidResponse { .. }
                | Self::UnexpectedResponseId { .. }
                | Self::Timeout { .. }
                | Self::ChildStopped
                | Self::FrameTooLarge { .. }
                | Self::HttpResponseTooLarge { .. }
        )
    }
}

impl fmt::Display for McpClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { command, message } => {
                write!(
                    formatter,
                    "failed to start MCP command `{command}`: {message}"
                )
            }
            Self::Io(error) => write!(formatter, "MCP stdio I/O failed: {error}"),
            Self::HttpTransport(error) => write!(formatter, "MCP HTTP transport failed: {error}"),
            Self::HttpBodyRead(error) => {
                write!(formatter, "MCP HTTP response read failed: {error}")
            }
            Self::HttpStatus { status } => {
                write!(formatter, "MCP HTTP endpoint returned status {status}")
            }
            Self::HttpResponseTooLarge { maximum } => {
                write!(formatter, "MCP HTTP response exceeded {maximum} bytes")
            }
            Self::HttpConfiguration { message } => {
                write!(formatter, "MCP HTTP configuration is invalid: {message}")
            }
            Self::NetworkDenied { endpoint } => {
                write!(formatter, "MCP network grant does not allow `{endpoint}`")
            }
            Self::SecretDenied { reference } => {
                write!(formatter, "MCP secret grant does not allow `{reference}`")
            }
            Self::InvalidCancellation { message } => {
                write!(formatter, "MCP cancellation is invalid: {message}")
            }
            Self::Read { message } => write!(formatter, "MCP stdio reader failed: {message}"),
            Self::Encode { message } => {
                write!(formatter, "MCP JSON-RPC encoding failed: {message}")
            }
            Self::MalformedJson { message } => {
                write!(
                    formatter,
                    "MCP Server returned malformed JSON-RPC: {message}"
                )
            }
            Self::InvalidResponse { method, message } => {
                write!(
                    formatter,
                    "MCP method `{method}` returned an invalid response: {message}"
                )
            }
            Self::UnexpectedResponseId { expected, received } => write!(
                formatter,
                "MCP JSON-RPC response id did not match request {expected}: {received:?}"
            ),
            Self::Remote { code, message } => {
                write!(
                    formatter,
                    "MCP Server rejected the request ({code}): {message}"
                )
            }
            Self::UnknownTool { name } => write!(formatter, "MCP tool `{name}` was not discovered"),
            Self::Timeout { method, timeout } => write!(
                formatter,
                "MCP method `{method}` timed out after {} ms",
                timeout.as_millis()
            ),
            Self::ChildStopped => formatter.write_str("MCP Server stopped before responding"),
            Self::FrameTooLarge { size, maximum } => write!(
                formatter,
                "MCP JSON-RPC frame is {size} bytes; maximum is {maximum}"
            ),
        }
    }
}

impl Error for McpClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) | Self::HttpBodyRead(error) => Some(error),
            Self::HttpTransport(error) => Some(error),
            _ => None,
        }
    }
}
