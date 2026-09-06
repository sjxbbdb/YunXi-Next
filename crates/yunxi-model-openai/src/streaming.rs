//! Bounded Server-Sent Events support for OpenAI-compatible providers.
//!
//! Providers are allowed to differ in minor wire details, but the host still
//! needs one predictable stream boundary. This module parses only the
//! `data:` SSE field, ignores harmless metadata fields, and never lets a
//! provider grow an unbounded response or tool argument buffer.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::StatusCode;
use reqwest::blocking::Response;
use serde::Deserialize;
use yunxi_protocol::ToolCall;

use crate::client::redact_provider_message;
use crate::{ApiError, ChatCompletion};

pub const MAX_STREAM_EVENTS: usize = 8_192;
pub const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_DELTA_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_STREAM_TOOL_CALLS: usize = 64;

/// Limits applied before a stream is handed to the Agent spine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamOptions {
    max_events: usize,
    max_line_bytes: usize,
    max_delta_bytes: usize,
    max_response_bytes: u64,
}

impl StreamOptions {
    pub fn new(
        max_events: usize,
        max_line_bytes: usize,
        max_delta_bytes: usize,
        max_response_bytes: u64,
    ) -> Result<Self, StreamOptionsError> {
        if max_events == 0 || max_events > MAX_STREAM_EVENTS {
            return Err(StreamOptionsError::OutOfRange {
                field: "max_events",
                value: max_events as u64,
                maximum: MAX_STREAM_EVENTS as u64,
            });
        }
        if max_line_bytes == 0 || max_line_bytes > MAX_STREAM_LINE_BYTES {
            return Err(StreamOptionsError::OutOfRange {
                field: "max_line_bytes",
                value: max_line_bytes as u64,
                maximum: MAX_STREAM_LINE_BYTES as u64,
            });
        }
        if max_delta_bytes == 0 || max_delta_bytes > MAX_STREAM_DELTA_BYTES {
            return Err(StreamOptionsError::OutOfRange {
                field: "max_delta_bytes",
                value: max_delta_bytes as u64,
                maximum: MAX_STREAM_DELTA_BYTES as u64,
            });
        }
        if max_response_bytes == 0 || max_response_bytes > MAX_STREAM_RESPONSE_BYTES {
            return Err(StreamOptionsError::OutOfRange {
                field: "max_response_bytes",
                value: max_response_bytes,
                maximum: MAX_STREAM_RESPONSE_BYTES,
            });
        }
        Ok(Self {
            max_events,
            max_line_bytes,
            max_delta_bytes,
            max_response_bytes,
        })
    }

    pub const fn max_events(self) -> usize {
        self.max_events
    }

    pub const fn max_line_bytes(self) -> usize {
        self.max_line_bytes
    }

    pub const fn max_delta_bytes(self) -> usize {
        self.max_delta_bytes
    }

    pub const fn max_response_bytes(self) -> u64 {
        self.max_response_bytes
    }
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            max_events: MAX_STREAM_EVENTS,
            max_line_bytes: MAX_STREAM_LINE_BYTES,
            max_delta_bytes: MAX_STREAM_DELTA_BYTES,
            max_response_bytes: MAX_STREAM_RESPONSE_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatStreamEvent {
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Finished {
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamObserverError {
    message: String,
}

impl StreamObserverError {
    pub fn new(message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 1_024 {
            let mut end = 1_024;
            while !message.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            message.truncate(end);
        }
        if message.trim().is_empty() {
            message = "stream observer stopped".to_string();
        }
        Self { message }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for StreamObserverError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StreamObserverError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamOptionsError {
    OutOfRange {
        field: &'static str,
        value: u64,
        maximum: u64,
    },
}

impl std::fmt::Display for StreamOptionsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange {
                field,
                value,
                maximum,
            } => write!(formatter, "{field} value {value} is outside 1..={maximum}"),
        }
    }
}

impl std::error::Error for StreamOptionsError {}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCallDelta>,
}

#[derive(Deserialize)]
struct StreamToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: StreamFunctionDelta,
}

#[derive(Default, Deserialize)]
struct StreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
}

pub(crate) fn consume_response<F, C>(
    mut response: Response,
    secret: &str,
    options: StreamOptions,
    is_cancelled: C,
    mut observer: F,
) -> Result<ChatCompletion, ApiError>
where
    F: FnMut(ChatStreamEvent) -> Result<(), StreamObserverError>,
    C: Fn() -> bool,
{
    let status = response.status();
    if !status.is_success() {
        return Err(parse_http_error(
            status,
            &mut response,
            secret,
            options.max_response_bytes,
        ));
    }

    let mut reader = BufReader::new(response);
    let mut line = Vec::new();
    let mut data = String::new();
    let mut bytes_read = 0_u64;
    let mut event_count = 0_usize;
    let mut content = String::new();
    let mut finish_reason = None;
    let mut tool_calls = BTreeMap::<usize, PartialToolCall>::new();
    let mut saw_payload = false;

    loop {
        if is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| ApiError::InvalidResponse(format!("stream read failed: {error}")))?;
        if read == 0 {
            if !data.is_empty() {
                process_data(
                    &data,
                    &options,
                    &is_cancelled,
                    &mut observer,
                    &mut event_count,
                    &mut content,
                    &mut finish_reason,
                    &mut tool_calls,
                    &mut saw_payload,
                )?;
            }
            break;
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if bytes_read > options.max_response_bytes {
            return Err(ApiError::ResponseTooLarge {
                limit: options.max_response_bytes,
            });
        }
        if line.len() > options.max_line_bytes {
            return Err(ApiError::InvalidResponse(format!(
                "stream line exceeded {} bytes",
                options.max_line_bytes
            )));
        }

        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        if line.is_empty() {
            if !data.is_empty() {
                process_data(
                    &data,
                    &options,
                    &is_cancelled,
                    &mut observer,
                    &mut event_count,
                    &mut content,
                    &mut finish_reason,
                    &mut tool_calls,
                    &mut saw_payload,
                )?;
                data.clear();
            }
            continue;
        }

        if line.starts_with(b":") {
            continue;
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            let value = if value.first() == Some(&b' ') {
                &value[1..]
            } else {
                value
            };
            let value = std::str::from_utf8(value).map_err(|error| {
                ApiError::InvalidResponse(format!("stream data was not UTF-8: {error}"))
            })?;
            if data.len().saturating_add(value.len()).saturating_add(1) > options.max_delta_bytes {
                return Err(ApiError::InvalidResponse(format!(
                    "stream event exceeded {} bytes",
                    options.max_delta_bytes
                )));
            }
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value);
        }
    }

    if !saw_payload {
        return Err(ApiError::InvalidResponse(
            "stream contained no data events".to_string(),
        ));
    }
    if tool_calls.len() > MAX_STREAM_TOOL_CALLS {
        return Err(ApiError::InvalidResponse(format!(
            "stream contained more than {MAX_STREAM_TOOL_CALLS} tool calls"
        )));
    }

    let mut completed_tools = Vec::with_capacity(tool_calls.len());
    for (index, partial) in tool_calls {
        let id = if partial.id.trim().is_empty() {
            format!("stream-call-{index}")
        } else {
            partial.id
        };
        if partial.name.trim().is_empty() {
            return Err(ApiError::InvalidResponse(format!(
                "stream tool call {index} did not provide a function name"
            )));
        }
        let arguments = if partial.arguments.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&partial.arguments).map_err(|error| {
                ApiError::InvalidResponse(format!(
                    "stream tool call {id} arguments are invalid JSON: {error}"
                ))
            })?
        };
        let call = ToolCall::new(id, partial.name, arguments)
            .map_err(|error| ApiError::InvalidResponse(error.to_string()))?;
        completed_tools.push(call);
    }

    if content.is_empty() && completed_tools.is_empty() {
        return Err(ApiError::InvalidResponse(
            "stream contained no content or tool calls".to_string(),
        ));
    }
    Ok(ChatCompletion::from_parts(
        content,
        finish_reason,
        completed_tools,
    ))
}

#[allow(clippy::too_many_arguments)]
fn process_data<F, C>(
    data: &str,
    options: &StreamOptions,
    is_cancelled: &C,
    observer: &mut F,
    event_count: &mut usize,
    content: &mut String,
    finish_reason: &mut Option<String>,
    tool_calls: &mut BTreeMap<usize, PartialToolCall>,
    saw_payload: &mut bool,
) -> Result<(), ApiError>
where
    F: FnMut(ChatStreamEvent) -> Result<(), StreamObserverError>,
    C: Fn() -> bool,
{
    if is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    if data == "[DONE]" {
        return Ok(());
    }
    *saw_payload = true;
    let chunk = serde_json::from_str::<StreamChunk>(data)
        .map_err(|error| ApiError::InvalidResponse(format!("invalid stream JSON: {error}")))?;
    if chunk.choices.is_empty() {
        return Err(ApiError::InvalidResponse(
            "stream JSON contained no choices".to_string(),
        ));
    }

    for choice in chunk.choices {
        if let Some(delta) = choice.delta.content {
            if delta.len() > options.max_delta_bytes
                || content.len().saturating_add(delta.len()) > options.max_response_bytes as usize
            {
                return Err(ApiError::ResponseTooLarge {
                    limit: options.max_response_bytes,
                });
            }
            if !delta.is_empty() {
                content.push_str(&delta);
                emit(
                    observer,
                    event_count,
                    options.max_events,
                    ChatStreamEvent::TextDelta { text: delta },
                )?;
            }
        }
        for delta in choice.delta.tool_calls {
            let StreamToolCallDelta {
                index,
                id,
                function,
            } = delta;
            let function_name = function.name;
            let arguments = function.arguments.unwrap_or_default();
            let entry = tool_calls.entry(index).or_default();
            if let Some(id) = id {
                if id.len() > options.max_delta_bytes {
                    return Err(ApiError::InvalidResponse(
                        "stream tool call id is too long".to_string(),
                    ));
                }
                entry.id = id;
            }
            if let Some(name) = function_name {
                if name.len() > options.max_delta_bytes {
                    return Err(ApiError::InvalidResponse(
                        "stream tool function name is too long".to_string(),
                    ));
                }
                entry.name.push_str(&name);
            }
            if entry.arguments.len().saturating_add(arguments.len()) > options.max_delta_bytes {
                return Err(ApiError::InvalidResponse(
                    "stream tool arguments are too long".to_string(),
                ));
            }
            entry.arguments.push_str(&arguments);
            emit(
                observer,
                event_count,
                options.max_events,
                ChatStreamEvent::ToolCallDelta {
                    index,
                    id: Some(entry.id.clone()).filter(|value| !value.is_empty()),
                    name: Some(entry.name.clone()).filter(|value| !value.is_empty()),
                    arguments,
                },
            )?;
        }
        if choice.finish_reason.is_some() {
            *finish_reason = choice.finish_reason.clone();
            emit(
                observer,
                event_count,
                options.max_events,
                ChatStreamEvent::Finished {
                    reason: choice.finish_reason,
                },
            )?;
        }
    }
    Ok(())
}

fn emit<F>(
    observer: &mut F,
    event_count: &mut usize,
    maximum: usize,
    event: ChatStreamEvent,
) -> Result<(), ApiError>
where
    F: FnMut(ChatStreamEvent) -> Result<(), StreamObserverError>,
{
    *event_count = event_count.saturating_add(1);
    if *event_count > maximum {
        return Err(ApiError::StreamLimitExceeded { limit: maximum });
    }
    observer(event).map_err(|error| ApiError::StreamObserver(error.to_string()))
}

fn parse_http_error(
    status: StatusCode,
    response: &mut Response,
    secret: &str,
    limit: u64,
) -> ApiError {
    let mut body = Vec::new();
    let read_result = response
        .take(limit.saturating_add(1))
        .read_to_end(&mut body);
    if read_result.is_err() {
        return ApiError::Http {
            status: status.as_u16(),
            message: "provider returned an unreadable error response".to_string(),
            retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
        };
    }
    if body.len() as u64 > limit {
        return ApiError::ResponseTooLarge { limit };
    }
    let message = serde_json::from_slice::<ErrorEnvelope>(&body)
        .ok()
        .and_then(|envelope| envelope.error)
        .and_then(|error| error.message)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
    ApiError::Http {
        status: status.as_u16(),
        message: redact_provider_message(
            if message.is_empty() {
                "empty error response".to_string()
            } else {
                message
            },
            secret,
        ),
        retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
    }
}

/// Convenience cancellation source for hosts that already use an atomic
/// cancellation flag. The callback form remains the primary API so the Agent
/// spine does not need to depend on this crate.
pub fn atomic_cancellation(flag: &AtomicBool) -> impl Fn() -> bool + '_ {
    move || flag.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::{OpenAiChatClient, ProviderConfig};

    fn fixture_client(listener: &TcpListener) -> OpenAiChatClient {
        let address = listener.local_addr().expect("fixture address");
        let config = ProviderConfig::new(
            "fixture",
            "fixture-model",
            format!("http://{address}"),
            "test-key",
        )
        .expect("fixture config")
        .with_timeout(Duration::from_secs(2));
        OpenAiChatClient::new(config).expect("fixture client")
    }

    #[test]
    fn parses_text_and_tool_deltas_with_backpressure_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let client = fixture_client(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            let mut reader = BufReader::new(stream.try_clone().expect("clone fixture stream"));
            let mut content_length = 0_usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("read request header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().expect("content length");
                }
            }
            let mut request_body = vec![0_u8; content_length];
            reader
                .read_exact(&mut request_body)
                .expect("read request body");
            let request_body = String::from_utf8(request_body).expect("UTF-8 request body");
            assert!(request_body.contains("\"stream\":true"));
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\",\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"file.read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write fixture");
        });
        let mut events = Vec::new();
        let completion = client
            .stream_with_tools_cancelable(
                &[yunxi_protocol::ChatMessage::user("hello")],
                None,
                StreamOptions::default(),
                || false,
                |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .expect("stream completion");
        assert_eq!(completion.content(), "Hello");
        assert_eq!(completion.tool_calls().len(), 1);
        assert_eq!(completion.tool_calls()[0].name().as_str(), "file.read");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ChatStreamEvent::TextDelta { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ChatStreamEvent::ToolCallDelta { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::Finished {
                reason: Some(reason)
            } if reason == "tool_calls"
        )));
        server.join().expect("join fixture");
    }

    #[test]
    fn cancellation_is_reported_before_consuming_the_next_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let client = fixture_client(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            let mut reader = BufReader::new(stream.try_clone().expect("clone fixture stream"));
            let mut content_length = 0_usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("read request header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().expect("content length");
                }
            }
            let mut request_body = vec![0_u8; content_length];
            reader
                .read_exact(&mut request_body)
                .expect("read request body");
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write fixture");
        });
        let flag = AtomicBool::new(true);
        let error = client
            .stream_with_tools_cancelable(
                &[yunxi_protocol::ChatMessage::user("hello")],
                None,
                StreamOptions::default(),
                atomic_cancellation(&flag),
                |_| Ok(()),
            )
            .expect_err("cancelled stream");
        assert!(matches!(error, ApiError::Cancelled));
        server.join().expect("join fixture");
    }

    #[test]
    fn options_reject_unbounded_values() {
        assert!(StreamOptions::new(0, 1, 1, 1).is_err());
        assert!(
            StreamOptions::new(
                MAX_STREAM_EVENTS + 1,
                MAX_STREAM_LINE_BYTES,
                MAX_STREAM_DELTA_BYTES,
                MAX_STREAM_RESPONSE_BYTES,
            )
            .is_err()
        );
    }

    #[test]
    fn observer_error_stops_a_stream_without_becoming_a_protocol_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let client = fixture_client(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture");
            let mut reader = BufReader::new(stream.try_clone().expect("clone fixture stream"));
            let mut content_length = 0_usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("read request header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().expect("content length");
                }
            }
            let mut request_body = vec![0_u8; content_length];
            reader
                .read_exact(&mut request_body)
                .expect("read request body");
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write fixture");
        });
        let error = client
            .stream_with_tools(&[yunxi_protocol::ChatMessage::user("hello")], None, |_| {
                Err(StreamObserverError::new("consumer is full"))
            })
            .expect_err("observer must stop the stream");
        assert!(
            matches!(error, ApiError::StreamObserver(message) if message == "consumer is full")
        );
        server.join().expect("join fixture");
    }
}
