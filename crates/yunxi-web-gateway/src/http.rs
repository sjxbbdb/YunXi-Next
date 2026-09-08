//! Dependency-light HTTP/1.1 carrier for the bounded Gateway facade.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use yunxi_web_contract::{
    ClientRequest, ClientResponse, EVENTS_HOST_METHOD, EVENTS_MUX_METHOD, EventChannel,
    MAX_FRAME_BYTES, RpcError, RpcId, RpcMessage, RpcResult,
};

use crate::assets::{EmbeddedWebAsset, embedded_web_asset};
use crate::events::{EventJournal, EventJournalError};
use crate::sse::{MAX_SSE_EVENTS, MAX_SSE_RESPONSE_BYTES, encode_page, error_event};
use crate::{
    AGENT_PRESET_LIST_METHOD, COMMANDS_LIST_METHOD, CREDENTIALS_DESCRIBE_METHOD,
    DYNAMIC_CORDIS_INVENTORY_METHOD, DYNAMIC_CORDIS_SYNC_INSPECT_METHOD, GatewayBackend,
    HEALTH_STATUS_METHOD, HOST_DESCRIBE_METHOD, LLM_PROVIDERS_METHOD, MAILBOX_GET_METHOD,
    MAILBOX_LIST_METHOD, MAILBOX_MARK_READ_METHOD, MEMORY_LIST_METHOD, MEMORY_SHOW_METHOD,
    MEMORY_STATUS_METHOD, PERSONA_LIST_METHOD, PERSONA_PROFILE_METHOD, PERSONA_STATUS_METHOD,
    PLUGIN_INVENTORY_LIST_METHOD, RELATIONSHIP_LIST_METHOD, RELATIONSHIP_STATUS_METHOD,
    SESSION_ATTACHMENT_METHOD, SESSION_CANCEL_METHOD, SESSION_CREATE_METHOD, SESSION_FORK_METHOD,
    SESSION_HISTORY_METHOD, SESSION_LIST_METHOD, SESSION_MODELS_METHOD, SESSION_PROMPT_METHOD,
    SESSION_RENAME_METHOD, SESSION_SEARCH_METHOD, SESSION_SELECT_MODEL_METHOD,
    SESSION_UPDATE_QUEUE_METHOD, SETTINGS_DESCRIBE_METHOD, SETTINGS_MUTATE_METHOD,
    SETTINGS_REPLACE_METHOD, SETTINGS_UPDATE_METHOD, SKILL_LIST_METHOD, SUBAGENT_HISTORY_METHOD,
    SUBAGENT_INTERRUPT_METHOD, SUBAGENT_LIST_METHOD, SUBAGENT_PROMPT_METHOD, VOICE_CANCEL_METHOD,
    VOICE_CHAT_METHOD, VOICE_DEVICES_METHOD, VOICE_DOCTOR_METHOD, VOICE_PLAYBACK_METHOD,
    VOICE_SAVE_METHOD, VOICE_SPEAK_METHOD, VOICE_TALK_METHOD, VOICE_TRANSCRIBE_METHOD,
    WEIXIN_CONTROL_METHOD, WEIXIN_DOCTOR_METHOD, WEIXIN_LOGIN_METHOD, WEIXIN_LOGOUT_METHOD,
    WEIXIN_PAIR_METHOD, WEIXIN_POLL_LOGIN_METHOD, WEIXIN_QUEUED_METHOD, WEIXIN_REPLY_METHOD,
    WEIXIN_SEND_METHOD, WEIXIN_SERVE_METHOD, WEIXIN_SERVE_START_METHOD, WEIXIN_SERVE_STATUS_METHOD,
    WEIXIN_SERVE_STOP_METHOD, WEIXIN_SESSION_METHOD, WEIXIN_STATUS_METHOD, WORKSPACE_LIST_METHOD,
};

pub const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_HTTP_BODY_BYTES: usize = MAX_FRAME_BYTES;
pub const MAX_HTTP_RESPONSE_BYTES: usize = MAX_SSE_RESPONSE_BYTES;
pub use crate::assets::MAX_WEB_ASSET_BYTES;
pub const DEFAULT_HTTP_READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_HTTP_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_HTTP_CONNECTION_WORKERS: usize = 32;
pub const MAX_SSE_LONG_POLL: Duration = Duration::from_millis(40);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

const SSE_STATIC_OVERHEAD_BYTES: usize = 32;
const SSE_GAP_RESERVE_BYTES: usize = 1024;
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
    event_journal: EventJournal,
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
            event_journal: EventJournal::new(),
            read_timeout: DEFAULT_HTTP_READ_TIMEOUT,
        }
    }

    /// Attach an optional durable JSONL replay journal to this carrier.
    pub fn with_event_journal_path(
        mut self,
        path: impl AsRef<Path>,
    ) -> Result<Self, EventJournalError> {
        self.attach_event_journal_path(path)?;
        Ok(self)
    }

    /// Load and attach a durable journal, replacing this carrier's in-memory window.
    pub fn attach_event_journal_path(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), EventJournalError> {
        self.event_journal.attach_path(path)
    }

    pub fn event_journal_path(&self) -> Option<&Path> {
        self.event_journal.path()
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
                self.handle_events(EventChannel::Mux, &request)
            }
            "GET" if api_path_matches(path, EVENTS_HOST_METHOD) => {
                self.handle_events(EventChannel::Host, &request)
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
        stream.set_write_timeout(Some(MAX_HTTP_WRITE_TIMEOUT))?;
        let response = match read_request(&mut stream) {
            Ok(request) => self.handle_request(request),
            Err(ReadRequestError::Cancelled) => return Ok(()),
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

    /// Serve independent HTTP connections concurrently while keeping backend
    /// dispatch serialized. Long-running work must be detached by the backend
    /// before returning from `dispatch`; this lets event and cancellation
    /// requests proceed without making the backend itself concurrent.
    pub fn serve_until_concurrent(
        self,
        listener: TcpListener,
        shutdown: &ShutdownToken,
    ) -> io::Result<()>
    where
        B: Send + 'static,
    {
        listener.set_nonblocking(true)?;
        let carrier = Arc::new(SharedHttpCarrier {
            backend: Mutex::new(self.backend),
            event_journal: Mutex::new(self.event_journal),
            read_timeout: self.read_timeout,
            shutdown: shutdown.clone(),
        });
        let mut workers = Vec::<thread::JoinHandle<io::Result<()>>>::new();
        while !shutdown.is_shutdown() {
            reap_finished_workers(&mut workers);
            match listener.accept() {
                Ok((mut stream, _peer)) if workers.len() >= MAX_HTTP_CONNECTION_WORKERS => {
                    let response = text_response(503, "too many active HTTP connections");
                    let _ = stream.write_all(&response.to_bytes());
                    let _ = stream.shutdown(Shutdown::Both);
                }
                Ok((stream, _peer)) => {
                    let carrier = Arc::clone(&carrier);
                    workers.push(thread::spawn(move || {
                        serve_shared_connection(&carrier, stream)
                    }));
                }
                Err(error) if is_would_block(&error) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
        for worker in workers {
            let _ = worker.join();
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

    fn handle_events(&mut self, channel: EventChannel, request: &HttpRequest) -> HttpResponse {
        let after_sequence = match event_cursor(request) {
            Ok(cursor) => cursor,
            Err(message) => return text_response(400, &message),
        };
        if let Some(cursor) = after_sequence
            && cursor > self.event_journal.latest_sequence(channel)
        {
            return text_response(400, "event cursor is ahead of the latest retained event");
        }
        let raw_budget = MAX_SSE_RESPONSE_BYTES
            .saturating_sub(SSE_STATIC_OVERHEAD_BYTES)
            .saturating_sub(MAX_SSE_EVENTS * 32)
            .saturating_sub(SSE_GAP_RESERVE_BYTES);
        let wait = match query_u64(request.path(), "waitMs") {
            Ok(Some(value)) => Duration::from_millis(value).min(MAX_SSE_LONG_POLL),
            Ok(None) => Duration::ZERO,
            Err(message) => return text_response(400, &message),
        };
        let deadline = Instant::now() + wait;
        let page = loop {
            let previous_latest = self.event_journal.latest_sequence(channel);
            self.backend.refresh();
            let events = match self
                .backend
                .take_events(channel, MAX_SSE_EVENTS, raw_budget)
            {
                Ok(events) => events,
                Err(error) => return sse_response(error_event(channel, error.to_string()), None),
            };
            if let Err(error) = self.event_journal.ingest(channel, events) {
                return text_response(500, &format!("SSE event journal failure: {error}"));
            }
            let cursor = after_sequence.unwrap_or(previous_latest);
            let page = self
                .event_journal
                .page_after(channel, cursor, MAX_SSE_EVENTS, raw_budget);
            if !page.events.is_empty()
                || page.replay_gap.is_some()
                || wait.is_zero()
                || Instant::now() >= deadline
            {
                break page;
            }
            thread::sleep(Duration::from_millis(2));
        };
        sse_response(encode_page(channel, &page), Some(&page))
    }
}

fn serve_shared_connection<B>(
    carrier: &Arc<SharedHttpCarrier<B>>,
    mut stream: TcpStream,
) -> io::Result<()>
where
    B: GatewayBackend,
    B::Error: fmt::Display,
{
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(carrier.read_timeout.min(SHUTDOWN_POLL_INTERVAL)))?;
    stream.set_write_timeout(Some(MAX_HTTP_WRITE_TIMEOUT))?;
    let response =
        match read_request_until(&mut stream, Some(&carrier.shutdown), carrier.read_timeout) {
            Ok(request) => carrier.handle_request(request),
            Err(ReadRequestError::Cancelled) => return Ok(()),
            Err(ReadRequestError::Parse(error)) => parse_error_response(error),
            Err(ReadRequestError::Io(error)) => return Err(error),
        };
    stream.write_all(&response.to_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

struct SharedHttpCarrier<B> {
    backend: Mutex<B>,
    event_journal: Mutex<EventJournal>,
    read_timeout: Duration,
    shutdown: ShutdownToken,
}

impl<B> SharedHttpCarrier<B>
where
    B: GatewayBackend,
    B::Error: fmt::Display,
{
    fn handle_request(&self, request: HttpRequest) -> HttpResponse {
        let path = request
            .path()
            .split_once('?')
            .map_or(request.path(), |(path, _)| path);
        match request.method() {
            "GET" if path == "/" => web_asset_response("/index.html"),
            "GET" if embedded_web_asset(path).is_some() => web_asset_response(path),
            "GET" if path == "/plugins/events" => static_plugin_events_response(),
            "GET" if api_path_matches(path, EVENTS_MUX_METHOD) => {
                self.handle_events(EventChannel::Mux, &request)
            }
            "GET" if api_path_matches(path, EVENTS_HOST_METHOD) => {
                self.handle_events(EventChannel::Host, &request)
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

    fn handle_response(&self, request: HttpRequest) -> HttpResponse {
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
        let mut backend = lock_unpoisoned(&self.backend);
        backend.refresh();
        match backend.respond(&response) {
            Ok(receipt) => response_from_value(receipt),
            Err(error) => text_response(500, &format!("gateway response failure: {error}")),
        }
    }

    fn handle_unary(&self, request: HttpRequest, method: &str) -> HttpResponse {
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
        let mut backend = lock_unpoisoned(&self.backend);
        backend.refresh();
        match backend.dispatch(&client_request) {
            Ok(response) => response_from_message(RpcMessage::ServerResponse(response)),
            Err(error) => text_response(500, &format!("gateway handler failure: {error}")),
        }
    }

    fn handle_events(&self, channel: EventChannel, request: &HttpRequest) -> HttpResponse {
        let after_sequence = match event_cursor(request) {
            Ok(cursor) => cursor,
            Err(message) => return text_response(400, &message),
        };
        if let Some(cursor) = after_sequence
            && cursor > lock_unpoisoned(&self.event_journal).latest_sequence(channel)
        {
            return text_response(400, "event cursor is ahead of the latest retained event");
        }
        let raw_budget = MAX_SSE_RESPONSE_BYTES
            .saturating_sub(SSE_STATIC_OVERHEAD_BYTES)
            .saturating_sub(MAX_SSE_EVENTS * 32)
            .saturating_sub(SSE_GAP_RESERVE_BYTES);
        let wait = match query_u64(request.path(), "waitMs") {
            Ok(Some(value)) => Duration::from_millis(value).min(MAX_SSE_LONG_POLL),
            Ok(None) => Duration::ZERO,
            Err(message) => return text_response(400, &message),
        };
        let deadline = Instant::now() + wait;
        let page = loop {
            // Serialize the cursor snapshot, backend drain, and journal
            // ingest. Without this, two initial subscribers can both observe
            // the same previous cursor and replay each other's batch.
            let mut journal = lock_unpoisoned(&self.event_journal);
            let previous_latest = journal.latest_sequence(channel);
            let events = {
                let mut backend = lock_unpoisoned(&self.backend);
                backend.refresh();
                match backend.take_events(channel, MAX_SSE_EVENTS, raw_budget) {
                    Ok(events) => events,
                    Err(error) => {
                        return sse_response(error_event(channel, error.to_string()), None);
                    }
                }
            };
            if let Err(error) = journal.ingest(channel, events) {
                return text_response(500, &format!("SSE event journal failure: {error}"));
            }
            let cursor = after_sequence.unwrap_or(previous_latest);
            let page = journal.page_after(channel, cursor, MAX_SSE_EVENTS, raw_budget);
            if !page.events.is_empty()
                || page.replay_gap.is_some()
                || wait.is_zero()
                || Instant::now() >= deadline
            {
                break page;
            }
            thread::sleep(Duration::from_millis(2));
        };
        sse_response(encode_page(channel, &page), Some(&page))
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn sse_response(
    body: Result<Vec<u8>, crate::GatewayError>,
    page: Option<&crate::events::ReplayPage>,
) -> HttpResponse {
    let body = match body {
        Ok(body) => body,
        Err(error) => return text_response(500, &format!("SSE carrier failure: {error}")),
    };
    let mut headers = vec![
        ("Content-Type".to_string(), "text/event-stream".to_string()),
        ("Cache-Control".to_string(), "no-cache".to_string()),
        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        ("X-Accel-Buffering".to_string(), "no".to_string()),
    ];
    if let Some(page) = page {
        headers.push((
            "X-Yunxi-Event-After".to_string(),
            page.after_sequence.to_string(),
        ));
        headers.push((
            "X-Yunxi-Event-Oldest".to_string(),
            page.oldest_sequence.to_string(),
        ));
        headers.push((
            "X-Yunxi-Event-Latest".to_string(),
            page.latest_sequence.to_string(),
        ));
        if page.replay_gap.is_some() {
            headers.push(("X-Yunxi-Replay-Gap".to_string(), "true".to_string()));
        }
        headers.push((
            "X-Yunxi-Event-Has-More".to_string(),
            page.has_more.to_string(),
        ));
    }
    response(200, "OK", headers, body)
}

fn reap_finished_workers(workers: &mut Vec<thread::JoinHandle<io::Result<()>>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
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
        MAILBOX_GET_METHOD => Some(MAILBOX_GET_METHOD),
        MAILBOX_LIST_METHOD => Some(MAILBOX_LIST_METHOD),
        MAILBOX_MARK_READ_METHOD => Some(MAILBOX_MARK_READ_METHOD),
        MEMORY_LIST_METHOD => Some(MEMORY_LIST_METHOD),
        MEMORY_SHOW_METHOD => Some(MEMORY_SHOW_METHOD),
        MEMORY_STATUS_METHOD => Some(MEMORY_STATUS_METHOD),
        PERSONA_LIST_METHOD => Some(PERSONA_LIST_METHOD),
        PERSONA_PROFILE_METHOD => Some(PERSONA_PROFILE_METHOD),
        PERSONA_STATUS_METHOD => Some(PERSONA_STATUS_METHOD),
        PLUGIN_INVENTORY_LIST_METHOD => Some(PLUGIN_INVENTORY_LIST_METHOD),
        RELATIONSHIP_LIST_METHOD => Some(RELATIONSHIP_LIST_METHOD),
        RELATIONSHIP_STATUS_METHOD => Some(RELATIONSHIP_STATUS_METHOD),
        SESSION_CREATE_METHOD => Some(SESSION_CREATE_METHOD),
        SESSION_CANCEL_METHOD => Some(SESSION_CANCEL_METHOD),
        SESSION_HISTORY_METHOD => Some(SESSION_HISTORY_METHOD),
        SESSION_LIST_METHOD => Some(SESSION_LIST_METHOD),
        SESSION_MODELS_METHOD => Some(SESSION_MODELS_METHOD),
        SESSION_PROMPT_METHOD => Some(SESSION_PROMPT_METHOD),
        SESSION_SEARCH_METHOD => Some(SESSION_SEARCH_METHOD),
        SESSION_RENAME_METHOD => Some(SESSION_RENAME_METHOD),
        SESSION_FORK_METHOD => Some(SESSION_FORK_METHOD),
        SESSION_SELECT_MODEL_METHOD => Some(SESSION_SELECT_MODEL_METHOD),
        SESSION_UPDATE_QUEUE_METHOD => Some(SESSION_UPDATE_QUEUE_METHOD),
        SESSION_ATTACHMENT_METHOD => Some(SESSION_ATTACHMENT_METHOD),
        SETTINGS_DESCRIBE_METHOD => Some(SETTINGS_DESCRIBE_METHOD),
        SETTINGS_MUTATE_METHOD => Some(SETTINGS_MUTATE_METHOD),
        SETTINGS_REPLACE_METHOD => Some(SETTINGS_REPLACE_METHOD),
        SETTINGS_UPDATE_METHOD => Some(SETTINGS_UPDATE_METHOD),
        SKILL_LIST_METHOD => Some(SKILL_LIST_METHOD),
        SUBAGENT_HISTORY_METHOD => Some(SUBAGENT_HISTORY_METHOD),
        SUBAGENT_LIST_METHOD => Some(SUBAGENT_LIST_METHOD),
        SUBAGENT_PROMPT_METHOD => Some(SUBAGENT_PROMPT_METHOD),
        SUBAGENT_INTERRUPT_METHOD => Some(SUBAGENT_INTERRUPT_METHOD),
        VOICE_CANCEL_METHOD => Some(VOICE_CANCEL_METHOD),
        VOICE_CHAT_METHOD => Some(VOICE_CHAT_METHOD),
        VOICE_DEVICES_METHOD => Some(VOICE_DEVICES_METHOD),
        VOICE_DOCTOR_METHOD => Some(VOICE_DOCTOR_METHOD),
        VOICE_PLAYBACK_METHOD => Some(VOICE_PLAYBACK_METHOD),
        VOICE_SAVE_METHOD => Some(VOICE_SAVE_METHOD),
        VOICE_SPEAK_METHOD => Some(VOICE_SPEAK_METHOD),
        VOICE_TALK_METHOD => Some(VOICE_TALK_METHOD),
        VOICE_TRANSCRIBE_METHOD => Some(VOICE_TRANSCRIBE_METHOD),
        WEIXIN_CONTROL_METHOD => Some(WEIXIN_CONTROL_METHOD),
        WEIXIN_DOCTOR_METHOD => Some(WEIXIN_DOCTOR_METHOD),
        WEIXIN_LOGIN_METHOD => Some(WEIXIN_LOGIN_METHOD),
        WEIXIN_LOGOUT_METHOD => Some(WEIXIN_LOGOUT_METHOD),
        WEIXIN_PAIR_METHOD => Some(WEIXIN_PAIR_METHOD),
        WEIXIN_POLL_LOGIN_METHOD => Some(WEIXIN_POLL_LOGIN_METHOD),
        WEIXIN_QUEUED_METHOD => Some(WEIXIN_QUEUED_METHOD),
        WEIXIN_REPLY_METHOD => Some(WEIXIN_REPLY_METHOD),
        WEIXIN_SEND_METHOD => Some(WEIXIN_SEND_METHOD),
        WEIXIN_SERVE_METHOD => Some(WEIXIN_SERVE_METHOD),
        WEIXIN_SERVE_START_METHOD => Some(WEIXIN_SERVE_START_METHOD),
        WEIXIN_SERVE_STATUS_METHOD => Some(WEIXIN_SERVE_STATUS_METHOD),
        WEIXIN_SERVE_STOP_METHOD => Some(WEIXIN_SERVE_STOP_METHOD),
        WEIXIN_SESSION_METHOD => Some(WEIXIN_SESSION_METHOD),
        WEIXIN_STATUS_METHOD => Some(WEIXIN_STATUS_METHOD),
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

fn event_cursor(request: &HttpRequest) -> Result<Option<u64>, String> {
    let header_cursor = request
        .header("last-event-id")
        .filter(|value| !value.is_empty())
        .map(parse_event_cursor)
        .transpose()?;
    let query_cursor = query_u64(request.path(), "afterSeq")?;
    match (header_cursor, query_cursor) {
        (Some(header), Some(query)) if header != query => {
            Err("Last-Event-ID and afterSeq must identify the same event cursor".to_string())
        }
        (Some(header), _) => Ok(Some(header)),
        (_, Some(query)) => Ok(Some(query)),
        (None, None) => Ok(None),
    }
}

fn query_u64(path: &str, wanted_name: &str) -> Result<Option<u64>, String> {
    let Some((_, query)) = path.split_once('?') else {
        return Ok(None);
    };
    let mut value = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let Some((name, candidate)) = pair.split_once('=') else {
            return Err("query parameters must use name=value form".to_string());
        };
        if name != wanted_name {
            continue;
        }
        if value.is_some() {
            return Err(format!("{wanted_name} must appear at most once"));
        }
        value = Some(parse_event_cursor(candidate)?);
    }
    Ok(value)
}

fn parse_event_cursor(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| "event cursor must be a non-negative integer".to_string())
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
        parts[1],
        headers,
        bytes[body_start..body_end].to_vec(),
    )
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, ReadRequestError> {
    read_request_until(stream, None, Duration::MAX)
}

fn read_request_until(
    stream: &mut TcpStream,
    shutdown: Option<&ShutdownToken>,
    read_timeout: Duration,
) -> Result<HttpRequest, ReadRequestError> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = (read_timeout != Duration::MAX).then(|| Instant::now() + read_timeout);
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
        let read = match stream.read(&mut buffer) {
            Ok(read) => read,
            Err(error)
                if error.kind() == io::ErrorKind::TimedOut
                    || error.kind() == io::ErrorKind::WouldBlock =>
            {
                if shutdown.is_some_and(ShutdownToken::is_shutdown) {
                    return Err(ReadRequestError::Cancelled);
                }
                if shutdown.is_none() || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(ReadRequestError::Io(error));
                }
                continue;
            }
            Err(error) => return Err(ReadRequestError::Io(error)),
        };
        if read == 0 {
            return Err(ReadRequestError::Parse(HttpParseError::Incomplete));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

enum ReadRequestError {
    Parse(HttpParseError),
    Io(io::Error),
    Cancelled,
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
    let body = if text.len() <= MAX_HTTP_RESPONSE_BYTES {
        text.as_bytes().to_vec()
    } else {
        b"response too large".to_vec()
    };
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
        503 => (503, "Service Unavailable"),
        500 => (500, "Internal Server Error"),
        _ => (500, "Internal Server Error"),
    }
}
