//! Bounded Server-Sent Events support for OpenAI-compatible providers.
//!
//! Providers are allowed to differ in minor wire details, but the host still
//! needs one predictable stream boundary. This module parses only the
//! `data:` SSE field, ignores harmless metadata fields, and never lets a
//! provider grow an unbounded response or tool argument buffer.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::StatusCode;
use reqwest::blocking::Response;
use serde::Deserialize;
use yunxi_protocol::ToolCall;

use crate::client::{redact_provider_message, retryable_http_status};
use crate::{ApiError, ChatCompletion};

pub const MAX_STREAM_EVENTS: usize = 8_192;
pub const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_DELTA_BYTES: usize = 1024 * 1024;
pub const MAX_STREAM_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_STREAM_TOOL_CALLS: usize = 64;
const MAX_JSON_CHOICES: usize = 64;
const MAX_JSON_TOOL_CALL_ID_BYTES: usize = 128;
const MAX_JSON_TOOL_NAME_BYTES: usize = 128;
const MAX_JSON_TOOL_TYPE_BYTES: usize = 32;
const MAX_JSON_FINISH_REASON_BYTES: usize = 128;

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
struct JsonCompletionResponse {
    #[serde(default)]
    choices: Vec<JsonChoice>,
}

#[derive(Deserialize)]
struct JsonChoice {
    message: JsonAssistantMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct JsonAssistantMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<JsonToolCall>>,
}

#[derive(Deserialize)]
struct JsonToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: JsonFunctionCall,
}

#[derive(Deserialize)]
struct JsonFunctionCall {
    name: String,
    arguments: String,
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

    if is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    let json_content_type = response_is_json(&response);
    let mut reader = BufReader::new(response);
    let first_byte = reader
        .fill_buf()
        .map_err(|_| {
            ApiError::InvalidResponse("stream response could not be inspected".to_string())
        })
        .map(first_non_whitespace)?;
    if json_content_type || first_byte == Some(b'{') {
        return consume_json_response(&mut reader, secret, options, is_cancelled, observer);
    }
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
        let read =
            read_sse_line(&mut reader, &mut line, options.max_line_bytes).map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData {
                    ApiError::InvalidResponse(format!(
                        "stream line exceeded {} bytes",
                        options.max_line_bytes
                    ))
                } else {
                    ApiError::InvalidResponse(format!("stream read failed: {error}"))
                }
            })?;
        if read == 0 {
            if !data.is_empty() {
                process_data(
                    &data,
                    secret,
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
                    secret,
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
                invalid_stream_response(
                    secret,
                    format!("stream tool call {id} arguments are invalid JSON: {error}"),
                )
            })?
        };
        let call = ToolCall::new(id, partial.name, arguments)
            .map_err(|error| invalid_stream_response(secret, error.to_string()))?;
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

fn response_is_json(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("application/json") || value.ends_with("+json")
        })
}

fn first_non_whitespace(bytes: &[u8]) -> Option<u8> {
    bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

fn read_sse_line<R>(reader: &mut R, line: &mut Vec<u8>, maximum: usize) -> io::Result<usize>
where
    R: BufRead,
{
    line.clear();
    let mut total = 0_usize;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(total);
        }
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if total.saturating_add(take) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSE line exceeded its configured limit",
            ));
        }
        line.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        total = total.saturating_add(take);
        if line.last() == Some(&b'\n') {
            return Ok(total);
        }
    }
}

fn consume_json_response<F, C, R>(
    reader: &mut R,
    secret: &str,
    options: StreamOptions,
    is_cancelled: C,
    mut observer: F,
) -> Result<ChatCompletion, ApiError>
where
    F: FnMut(ChatStreamEvent) -> Result<(), StreamObserverError>,
    C: Fn() -> bool,
    R: Read,
{
    let body = read_json_body(reader, options.max_response_bytes, &is_cancelled)?;
    if is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    let parsed = serde_json::from_slice::<JsonCompletionResponse>(&body).map_err(|error| {
        invalid_json_response(
            secret,
            format!("ordinary JSON completion is invalid: {error}"),
        )
    })?;
    if parsed.choices.is_empty() {
        return Err(invalid_json_response(
            secret,
            "ordinary JSON completion contained no choices",
        ));
    }
    if parsed.choices.len() > MAX_JSON_CHOICES {
        return Err(invalid_json_response(
            secret,
            format!("ordinary JSON completion contained more than {MAX_JSON_CHOICES} choices"),
        ));
    }

    let choice = parsed.choices.into_iter().next().expect("checked above");
    let content = choice.message.content.unwrap_or_default();
    validate_json_field(
        "content",
        &content,
        options.max_response_bytes as usize,
        secret,
    )?;
    let finish_reason = choice.finish_reason;
    if let Some(reason) = finish_reason.as_deref() {
        validate_json_field(
            "finish_reason",
            reason,
            MAX_JSON_FINISH_REASON_BYTES,
            secret,
        )?;
    }

    let json_tools = choice.message.tool_calls.unwrap_or_default();
    if json_tools.len() > MAX_STREAM_TOOL_CALLS {
        return Err(invalid_json_response(
            secret,
            format!(
                "ordinary JSON completion contained more than {MAX_STREAM_TOOL_CALLS} tool calls"
            ),
        ));
    }
    let mut tool_calls = Vec::with_capacity(json_tools.len());
    for tool in json_tools {
        validate_json_field("tool type", &tool.kind, MAX_JSON_TOOL_TYPE_BYTES, secret)?;
        if tool.kind != "function" {
            return Err(invalid_json_response(
                secret,
                "ordinary JSON completion contained an unsupported tool type",
            ));
        }
        validate_json_field(
            "tool call id",
            &tool.id,
            MAX_JSON_TOOL_CALL_ID_BYTES,
            secret,
        )?;
        validate_json_field(
            "tool function name",
            &tool.function.name,
            MAX_JSON_TOOL_NAME_BYTES,
            secret,
        )?;
        validate_json_field(
            "tool arguments",
            &tool.function.arguments,
            options.max_delta_bytes,
            secret,
        )?;
        let arguments = serde_json::from_str(&tool.function.arguments).map_err(|_| {
            invalid_json_response(secret, "ordinary JSON tool arguments are invalid JSON")
        })?;
        let call = ToolCall::new(tool.id, tool.function.name, arguments).map_err(|_| {
            invalid_json_response(secret, "ordinary JSON tool call fields are invalid")
        })?;
        tool_calls.push(call);
    }

    if content.is_empty() && tool_calls.is_empty() {
        return Err(invalid_json_response(
            secret,
            "ordinary JSON completion contained no content or tool calls",
        ));
    }

    let mut event_count = 0;
    emit_text_chunks(
        &content,
        options.max_delta_bytes,
        &is_cancelled,
        &mut observer,
        &mut event_count,
        options.max_events,
    )?;
    for (index, tool_call) in tool_calls.iter().enumerate() {
        if is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        emit(
            &mut observer,
            &mut event_count,
            options.max_events,
            ChatStreamEvent::ToolCallDelta {
                index,
                id: Some(tool_call.id().to_string()),
                name: Some(tool_call.name().to_string()),
                arguments: serde_json::to_string(tool_call.arguments()).map_err(|_| {
                    invalid_json_response(secret, "ordinary JSON tool arguments are invalid")
                })?,
            },
        )?;
    }
    if is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    emit(
        &mut observer,
        &mut event_count,
        options.max_events,
        ChatStreamEvent::Finished {
            reason: finish_reason.clone(),
        },
    )?;

    Ok(ChatCompletion::from_parts(
        content,
        finish_reason,
        tool_calls,
    ))
}

fn read_json_body<R, C>(reader: &mut R, limit: u64, is_cancelled: &C) -> Result<Vec<u8>, ApiError>
where
    R: Read,
    C: Fn() -> bool,
{
    let mut body = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        if is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        let read = reader.read(&mut chunk).map_err(|_| {
            ApiError::InvalidResponse("ordinary JSON response could not be read".to_string())
        })?;
        if read == 0 {
            break;
        }
        if (body.len() as u64).saturating_add(read as u64) > limit {
            return Err(ApiError::ResponseTooLarge { limit });
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(body)
}

fn validate_json_field(
    field: &str,
    value: &str,
    maximum: usize,
    secret: &str,
) -> Result<(), ApiError> {
    if value.len() > maximum || value.contains('\0') {
        return Err(invalid_json_response(
            secret,
            format!("ordinary JSON {field} field exceeded its limit"),
        ));
    }
    Ok(())
}

fn emit_text_chunks<F>(
    text: &str,
    maximum_bytes: usize,
    is_cancelled: &impl Fn() -> bool,
    observer: &mut F,
    event_count: &mut usize,
    maximum_events: usize,
) -> Result<(), ApiError>
where
    F: FnMut(ChatStreamEvent) -> Result<(), StreamObserverError>,
{
    if text.is_empty() {
        return Ok(());
    }
    let mut start = 0;
    while start < text.len() {
        if is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        let mut end = (start + maximum_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            return Err(ApiError::InvalidResponse(
                "ordinary JSON content could not be split safely".to_string(),
            ));
        }
        emit(
            observer,
            event_count,
            maximum_events,
            ChatStreamEvent::TextDelta {
                text: text[start..end].to_string(),
            },
        )?;
        start = end;
    }
    Ok(())
}

fn invalid_json_response(secret: &str, message: impl Into<String>) -> ApiError {
    ApiError::InvalidResponse(redact_provider_message(message, secret))
}

fn invalid_stream_response(secret: &str, message: impl Into<String>) -> ApiError {
    ApiError::InvalidResponse(redact_provider_message(message, secret))
}

#[allow(clippy::too_many_arguments)]
fn process_data<F, C>(
    data: &str,
    secret: &str,
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
    let chunk = serde_json::from_str::<StreamChunk>(data).map_err(|error| {
        invalid_stream_response(secret, format!("invalid stream JSON: {error}"))
    })?;
    if chunk.choices.is_empty() {
        return Err(invalid_stream_response(
            secret,
            "stream JSON contained no choices",
        ));
    }

    for choice in chunk.choices {
        if is_cancelled() {
            return Err(ApiError::Cancelled);
        }
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
            if is_cancelled() {
                return Err(ApiError::Cancelled);
            }
            let StreamToolCallDelta {
                index,
                id,
                function,
            } = delta;
            let function_name = function.name;
            let arguments = function.arguments.unwrap_or_default();
            if !tool_calls.contains_key(&index) && tool_calls.len() >= MAX_STREAM_TOOL_CALLS {
                return Err(ApiError::InvalidResponse(format!(
                    "stream contained more than {MAX_STREAM_TOOL_CALLS} tool calls"
                )));
            }
            let entry = tool_calls.entry(index).or_default();
            let event_id = id.clone();
            let event_name = function_name.clone();
            if let Some(id) = id {
                if id.len() > options.max_delta_bytes {
                    return Err(ApiError::InvalidResponse(
                        "stream tool call id is too long".to_string(),
                    ));
                }
                entry.id = id;
            }
            if let Some(name) = function_name {
                if name.len() > options.max_delta_bytes
                    || entry.name.len().saturating_add(name.len()) > options.max_delta_bytes
                {
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
                    id: event_id,
                    name: event_name,
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
            retryable: retryable_http_status(status),
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
        retryable: retryable_http_status(status),
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
    use std::io::{BufRead, BufReader, Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
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
    fn parses_ordinary_json_completion_as_bounded_stream_events() {
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
            assert!(
                String::from_utf8(request_body)
                    .expect("UTF-8 request body")
                    .contains("\"stream\":true")
            );
            let body = concat!(
                "{\"choices\":[{\"message\":{\"content\":\"兼容回答\",",
                "\"tool_calls\":[{\"id\":\"call-json\",\"type\":\"function\",",
                "\"function\":{\"name\":\"file.read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},",
                "\"finish_reason\":\"tool_calls\"}]}"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
            .expect("JSON fallback completion");

        assert_eq!(completion.content(), "兼容回答");
        assert_eq!(completion.finish_reason(), Some("tool_calls"));
        assert_eq!(completion.tool_calls().len(), 1);
        assert_eq!(completion.tool_calls()[0].name().as_str(), "file.read");
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::TextDelta { text } if text == "兼容回答"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::ToolCallDelta {
                index: 0,
                id: Some(id),
                name: Some(name),
                arguments,
            } if id == "call-json" && name == "file.read" && arguments.contains("README.md")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::Finished {
                reason: Some(reason)
            } if reason == "tool_calls"
        )));
        server.join().expect("join fixture");
    }

    #[test]
    fn sse_content_type_keeps_incremental_event_path() {
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
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"one\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\" two\"},\"finish_reason\":\"stop\"}]}\n\n",
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
            .stream_with_tools(
                &[yunxi_protocol::ChatMessage::user("hello")],
                None,
                |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .expect("SSE completion");

        assert_eq!(completion.content(), "one two");
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    ChatStreamEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .concat(),
            "one two"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::Finished {
                reason: Some(reason)
            } if reason == "stop"
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
        let flag = AtomicBool::new(false);
        let error = client
            .stream_with_tools_cancelable(
                &[yunxi_protocol::ChatMessage::user("hello")],
                None,
                StreamOptions::default(),
                atomic_cancellation(&flag),
                |_| {
                    flag.store(true, Ordering::Release);
                    Ok(())
                },
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
    fn sse_line_reader_rejects_an_oversized_line_before_appending_it() {
        let mut reader = BufReader::new(Cursor::new(vec![b'x'; 128]));
        let mut line = Vec::new();

        let error = read_sse_line(&mut reader, &mut line, 16).expect_err("line is too large");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(line.is_empty());
    }

    #[test]
    fn tool_call_observer_receives_raw_deltas_while_completion_accumulates_them() {
        let options = StreamOptions::default();
        let mut events = Vec::new();
        let mut event_count = 0;
        let mut content = String::new();
        let mut finish_reason = None;
        let mut tool_calls = BTreeMap::new();
        let mut saw_payload = false;

        process_data(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"fo","arguments":"{\"x\":"}}]}}]}"#,
            "test-key",
            &options,
            &|| false,
            &mut |event| {
                events.push(event);
                Ok(())
            },
            &mut event_count,
            &mut content,
            &mut finish_reason,
            &mut tool_calls,
            &mut saw_payload,
        )
        .expect("first tool delta");
        process_data(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"o","arguments":"1}"}}]}}]}"#,
            "test-key",
            &options,
            &|| false,
            &mut |event| {
                events.push(event);
                Ok(())
            },
            &mut event_count,
            &mut content,
            &mut finish_reason,
            &mut tool_calls,
            &mut saw_payload,
        )
        .expect("second tool delta");

        assert!(matches!(
            &events[0],
            ChatStreamEvent::ToolCallDelta {
                id: Some(id),
                name: Some(name),
                arguments,
                ..
            } if id == "call-1" && name == "fo" && arguments == "{\"x\":"
        ));
        assert!(matches!(
            &events[1],
            ChatStreamEvent::ToolCallDelta {
                id: None,
                name: Some(name),
                arguments,
                ..
            } if name == "o" && arguments == "1}"
        ));
        assert_eq!(tool_calls[&0].name, "foo");
        assert_eq!(tool_calls[&0].arguments, r#"{"x":1}"#);
    }

    #[test]
    fn cancellation_is_checked_before_starting_the_http_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let client = fixture_client(&listener);
        let error = client
            .stream_with_tools_cancelable(
                &[yunxi_protocol::ChatMessage::user("hello")],
                None,
                StreamOptions::default(),
                || true,
                |_| Ok(()),
            )
            .expect_err("cancelled request");

        assert!(matches!(error, ApiError::Cancelled));
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
