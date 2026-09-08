//! Versioned, bounded events emitted while an Agent turn is executing.
//!
//! The stream is an additive extension to the v2 host/plugin protocol.  It is
//! intentionally separate from [`crate::HostMessage`] and the v1 tool
//! protocol so legacy plugins can continue to use the existing request/
//! response contract without understanding streaming events.

use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ChatResult, ToolCallBatch, ToolResult};

/// Version of the Agent turn event extension.
pub const STREAM_PROTOCOL_VERSION: u32 = 1;

/// Maximum number of events a single turn may publish.
pub const MAX_STREAM_EVENTS_PER_TURN: usize = 4096;
/// Maximum size of a turn id in bytes.
pub const MAX_STREAM_TURN_ID_BYTES: usize = 128;
/// Maximum size of a text delta or final model text in bytes.
pub const MAX_STREAM_TEXT_BYTES: usize = 64 * 1024;
/// Maximum size of a stream error code in bytes.
pub const MAX_STREAM_ERROR_CODE_BYTES: usize = 128;
/// Maximum size of a stream error message in bytes.
pub const MAX_STREAM_ERROR_MESSAGE_BYTES: usize = 4096;
/// Maximum size of a model finish reason in bytes.
pub const MAX_STREAM_FINISH_REASON_BYTES: usize = 128;
/// Maximum serialized size of one stream event.
pub const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;

/// Lifecycle states represented on the stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamTurnState {
    Queued,
    Created,
    ContextBuilding,
    ModelCalling,
    ToolCalling,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl StreamTurnState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
}

/// A bounded, structured failure that is safe to put on the event stream.
///
/// The stream layer does not log component errors.  Hosts that do not want to
/// expose implementation details should use [`Self::redacted`] when mapping
/// an internal error to an event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StreamError {
    code: String,
    message: String,
    retryable: bool,
}

impl StreamError {
    /// Creates a validated stream error.
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Result<Self, StreamProtocolError> {
        let error = Self {
            code: code.into(),
            message: message.into(),
            retryable,
        };
        error.validate()?;
        Ok(error)
    }

    /// Creates a generic error without copying a component's message.
    ///
    /// This is the preferred conversion for diagnostics because user prompts,
    /// file paths, command output, and provider payloads never cross this
    /// boundary.
    pub fn redacted(code: impl Into<String>, retryable: bool) -> Self {
        let code = normalize_code(code.into());
        Self {
            code,
            message: "turn operation failed".to_string(),
            retryable,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    pub fn validate(&self) -> Result<(), StreamProtocolError> {
        validate_code(&self.code)?;
        validate_text(
            "stream error message",
            &self.message,
            MAX_STREAM_ERROR_MESSAGE_BYTES,
            false,
        )
    }
}

impl<'de> Deserialize<'de> for StreamError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireError {
            code: String,
            message: String,
            retryable: bool,
        }

        let wire = WireError::deserialize(deserializer)?;
        let error = Self {
            code: wire.code,
            message: wire.message,
            retryable: wire.retryable,
        };
        error.validate().map_err(D::Error::custom)?;
        Ok(error)
    }
}

/// Events emitted by the Agent spine in sequence order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        delta: String,
    },
    ToolStart {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        call_id: crate::ToolCallId,
        tool_name: crate::ToolName,
    },
    ToolProgress {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        call_id: crate::ToolCallId,
        tool_name: crate::ToolName,
        progress: String,
    },
    ToolResult {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        result: ToolResult,
    },
    TurnState {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        state: StreamTurnState,
    },
    TurnError {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        error: StreamError,
    },
    TurnDone {
        protocol_version: u32,
        sequence: u64,
        turn_id: String,
        round: u16,
        response: Option<ChatResult>,
    },
}

/// Alias used by Agent-facing code and external adapters.
pub type AgentStreamEvent = StreamEvent;

/// Additive JSONL envelope for a dedicated stream channel.
///
/// It is intentionally separate from `HostMessage`/`PluginMessage`: old v2
/// peers can continue decoding their existing exhaustive enums, while a host
/// and a streaming-capable plugin can opt into this envelope on a second
/// JSONL channel (or carry it in an explicitly negotiated invocation).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEnvelope {
    Event {
        protocol_version: u32,
        event: StreamEvent,
    },
    Cancel {
        protocol_version: u32,
        turn_id: String,
        reason: String,
    },
}

pub type AgentStreamEnvelope = StreamEnvelope;

impl StreamEnvelope {
    pub fn event(event: StreamEvent) -> Result<Self, StreamProtocolError> {
        let envelope = Self::Event {
            protocol_version: STREAM_PROTOCOL_VERSION,
            event,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn cancel(
        turn_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, StreamProtocolError> {
        let envelope = Self::Cancel {
            protocol_version: STREAM_PROTOCOL_VERSION,
            turn_id: turn_id.into(),
            reason: reason.into(),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::Event {
                protocol_version, ..
            }
            | Self::Cancel {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn stream_event(&self) -> Option<&StreamEvent> {
        match self {
            Self::Event { event, .. } => Some(event),
            Self::Cancel { .. } => None,
        }
    }

    pub fn cancel_turn_id(&self) -> Option<&str> {
        match self {
            Self::Event { .. } => None,
            Self::Cancel { turn_id, .. } => Some(turn_id),
        }
    }

    pub fn cancel_reason(&self) -> Option<&str> {
        match self {
            Self::Event { .. } => None,
            Self::Cancel { reason, .. } => Some(reason),
        }
    }

    pub fn validate(&self) -> Result<(), StreamProtocolError> {
        if self.protocol_version() != STREAM_PROTOCOL_VERSION {
            return Err(StreamProtocolError::UnsupportedVersion {
                version: self.protocol_version(),
                expected: STREAM_PROTOCOL_VERSION,
            });
        }
        match self {
            Self::Event { event, .. } => event.validate()?,
            Self::Cancel {
                turn_id, reason, ..
            } => {
                validate_text("turn id", turn_id, MAX_STREAM_TURN_ID_BYTES, false)?;
                validate_text(
                    "cancellation reason",
                    reason,
                    MAX_STREAM_ERROR_MESSAGE_BYTES,
                    true,
                )?;
            }
        }
        let size = serde_json::to_vec(self)
            .map_err(|error| StreamProtocolError::Serialization(error.to_string()))?
            .len();
        if size > MAX_STREAM_EVENT_BYTES {
            return Err(StreamProtocolError::EventTooLarge {
                size,
                maximum: MAX_STREAM_EVENT_BYTES,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for StreamEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum WireEnvelope {
            Event {
                protocol_version: u32,
                event: StreamEvent,
            },
            Cancel {
                protocol_version: u32,
                turn_id: String,
                reason: String,
            },
        }

        let envelope = match WireEnvelope::deserialize(deserializer)? {
            WireEnvelope::Event {
                protocol_version,
                event,
            } => Self::Event {
                protocol_version,
                event,
            },
            WireEnvelope::Cancel {
                protocol_version,
                turn_id,
                reason,
            } => Self::Cancel {
                protocol_version,
                turn_id,
                reason,
            },
        };
        envelope.validate().map_err(D::Error::custom)?;
        Ok(envelope)
    }
}

impl StreamEvent {
    pub fn text_delta(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        delta: impl Into<String>,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::TextDelta {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            delta: delta.into(),
        };
        event.validate()?;
        Ok(event)
    }

    pub fn tool_start(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        call_id: crate::ToolCallId,
        tool_name: crate::ToolName,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::ToolStart {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            call_id,
            tool_name,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn tool_progress(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        call_id: crate::ToolCallId,
        tool_name: crate::ToolName,
        progress: impl Into<String>,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::ToolProgress {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            call_id,
            tool_name,
            progress: progress.into(),
        };
        event.validate()?;
        Ok(event)
    }

    pub fn tool_result(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        result: ToolResult,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::ToolResult {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            result,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn turn_state(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        state: StreamTurnState,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::TurnState {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            state,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn turn_error(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        error: StreamError,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::TurnError {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            error,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn turn_done(
        sequence: u64,
        turn_id: impl Into<String>,
        round: u16,
        response: Option<ChatResult>,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::TurnDone {
            protocol_version: STREAM_PROTOCOL_VERSION,
            sequence,
            turn_id: turn_id.into(),
            round,
            response,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::TextDelta {
                protocol_version, ..
            }
            | Self::ToolStart {
                protocol_version, ..
            }
            | Self::ToolProgress {
                protocol_version, ..
            }
            | Self::ToolResult {
                protocol_version, ..
            }
            | Self::TurnState {
                protocol_version, ..
            }
            | Self::TurnError {
                protocol_version, ..
            }
            | Self::TurnDone {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::TextDelta { sequence, .. }
            | Self::ToolStart { sequence, .. }
            | Self::ToolProgress { sequence, .. }
            | Self::ToolResult { sequence, .. }
            | Self::TurnState { sequence, .. }
            | Self::TurnError { sequence, .. }
            | Self::TurnDone { sequence, .. } => *sequence,
        }
    }

    pub fn turn_id(&self) -> &str {
        match self {
            Self::TextDelta { turn_id, .. }
            | Self::ToolStart { turn_id, .. }
            | Self::ToolProgress { turn_id, .. }
            | Self::ToolResult { turn_id, .. }
            | Self::TurnState { turn_id, .. }
            | Self::TurnError { turn_id, .. }
            | Self::TurnDone { turn_id, .. } => turn_id,
        }
    }

    pub fn round(&self) -> u16 {
        match self {
            Self::TextDelta { round, .. }
            | Self::ToolStart { round, .. }
            | Self::ToolProgress { round, .. }
            | Self::ToolResult { round, .. }
            | Self::TurnState { round, .. }
            | Self::TurnError { round, .. }
            | Self::TurnDone { round, .. } => *round,
        }
    }

    /// Returns whether losing this event is safe under a drop policy.
    pub fn is_droppable(&self) -> bool {
        matches!(self, Self::TextDelta { .. } | Self::ToolProgress { .. })
    }

    /// Terminal events are never silently classified as progress.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::TurnError { .. } | Self::TurnDone { .. })
            || matches!(self, Self::TurnState { state, .. } if state.is_terminal())
    }

    pub fn validate(&self) -> Result<(), StreamProtocolError> {
        if self.protocol_version() != STREAM_PROTOCOL_VERSION {
            return Err(StreamProtocolError::UnsupportedVersion {
                version: self.protocol_version(),
                expected: STREAM_PROTOCOL_VERSION,
            });
        }
        if self.sequence() == 0 {
            return Err(StreamProtocolError::ZeroSequence);
        }
        validate_text("turn id", self.turn_id(), MAX_STREAM_TURN_ID_BYTES, false)?;

        match self {
            Self::TextDelta { round, delta, .. } => {
                validate_round(*round, true)?;
                validate_text("text delta", delta, MAX_STREAM_TEXT_BYTES, true)?;
            }
            Self::ToolStart {
                round,
                call_id,
                tool_name,
                ..
            } => {
                validate_round(*round, true)?;
                validate_tool_identifiers(call_id, tool_name)?;
            }
            Self::ToolProgress {
                round,
                call_id,
                tool_name,
                progress,
                ..
            } => {
                validate_round(*round, true)?;
                validate_tool_identifiers(call_id, tool_name)?;
                validate_text("tool progress", progress, MAX_STREAM_TEXT_BYTES, true)?;
            }
            Self::ToolResult { round, result, .. } => {
                validate_round(*round, true)?;
                result
                    .validate()
                    .map_err(|error| StreamProtocolError::InvalidToolResult(error.to_string()))?;
            }
            Self::TurnState { round, .. } => validate_round(*round, false)?,
            Self::TurnError { round, error, .. } => {
                validate_round(*round, false)?;
                error.validate()?;
            }
            Self::TurnDone {
                round, response, ..
            } => {
                validate_round(*round, false)?;
                if let Some(response) = response {
                    validate_chat_result(response)?;
                }
            }
        }

        let size = serde_json::to_vec(self)
            .map_err(|error| StreamProtocolError::Serialization(error.to_string()))?
            .len();
        if size > MAX_STREAM_EVENT_BYTES {
            return Err(StreamProtocolError::EventTooLarge {
                size,
                maximum: MAX_STREAM_EVENT_BYTES,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum WireEvent {
            TextDelta {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                delta: String,
            },
            ToolStart {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                call_id: crate::ToolCallId,
                tool_name: crate::ToolName,
            },
            ToolProgress {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                call_id: crate::ToolCallId,
                tool_name: crate::ToolName,
                progress: String,
            },
            ToolResult {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                result: ToolResult,
            },
            TurnState {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                state: StreamTurnState,
            },
            TurnError {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                error: StreamError,
            },
            TurnDone {
                protocol_version: u32,
                sequence: u64,
                turn_id: String,
                round: u16,
                response: Option<ChatResult>,
            },
        }

        let event = match WireEvent::deserialize(deserializer)? {
            WireEvent::TextDelta {
                protocol_version,
                sequence,
                turn_id,
                round,
                delta,
            } => Self::TextDelta {
                protocol_version,
                sequence,
                turn_id,
                round,
                delta,
            },
            WireEvent::ToolStart {
                protocol_version,
                sequence,
                turn_id,
                round,
                call_id,
                tool_name,
            } => Self::ToolStart {
                protocol_version,
                sequence,
                turn_id,
                round,
                call_id,
                tool_name,
            },
            WireEvent::ToolProgress {
                protocol_version,
                sequence,
                turn_id,
                round,
                call_id,
                tool_name,
                progress,
            } => Self::ToolProgress {
                protocol_version,
                sequence,
                turn_id,
                round,
                call_id,
                tool_name,
                progress,
            },
            WireEvent::ToolResult {
                protocol_version,
                sequence,
                turn_id,
                round,
                result,
            } => Self::ToolResult {
                protocol_version,
                sequence,
                turn_id,
                round,
                result,
            },
            WireEvent::TurnState {
                protocol_version,
                sequence,
                turn_id,
                round,
                state,
            } => Self::TurnState {
                protocol_version,
                sequence,
                turn_id,
                round,
                state,
            },
            WireEvent::TurnError {
                protocol_version,
                sequence,
                turn_id,
                round,
                error,
            } => Self::TurnError {
                protocol_version,
                sequence,
                turn_id,
                round,
                error,
            },
            WireEvent::TurnDone {
                protocol_version,
                sequence,
                turn_id,
                round,
                response,
            } => Self::TurnDone {
                protocol_version,
                sequence,
                turn_id,
                round,
                response,
            },
        };
        event.validate().map_err(D::Error::custom)?;
        Ok(event)
    }
}

/// Validation failures for stream input and event construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamProtocolError {
    UnsupportedVersion {
        version: u32,
        expected: u32,
    },
    ZeroSequence,
    InvalidRound {
        round: u16,
    },
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    NulCharacter {
        field: &'static str,
    },
    InvalidErrorCode {
        code: String,
    },
    InvalidToolResult(String),
    InvalidChatResult(String),
    EventTooLarge {
        size: usize,
        maximum: usize,
    },
    Serialization(String),
}

impl fmt::Display for StreamProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { version, expected } => write!(
                formatter,
                "stream protocol version {version} is unsupported; expected {expected}"
            ),
            Self::ZeroSequence => {
                formatter.write_str("stream event sequence must be greater than zero")
            }
            Self::InvalidRound { round } => {
                write!(formatter, "stream event round {round} is invalid")
            }
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::NulCharacter { field } => write!(formatter, "{field} contains a NUL character"),
            Self::InvalidErrorCode { code } => {
                write!(formatter, "invalid stream error code `{code}`")
            }
            Self::InvalidToolResult(message) => {
                write!(formatter, "invalid stream tool result: {message}")
            }
            Self::InvalidChatResult(message) => {
                write!(formatter, "invalid stream chat result: {message}")
            }
            Self::EventTooLarge { size, maximum } => {
                write!(
                    formatter,
                    "stream event is {size} bytes; maximum is {maximum}"
                )
            }
            Self::Serialization(message) => {
                write!(formatter, "stream event serialization failed: {message}")
            }
        }
    }
}

impl Error for StreamProtocolError {}

fn validate_round(round: u16, required: bool) -> Result<(), StreamProtocolError> {
    if (required && round == 0) || round > crate::MAX_TOOL_ROUNDS {
        return Err(StreamProtocolError::InvalidRound { round });
    }
    Ok(())
}

fn validate_tool_identifiers(
    call_id: &crate::ToolCallId,
    tool_name: &crate::ToolName,
) -> Result<(), StreamProtocolError> {
    if call_id.as_str().is_empty() || tool_name.as_str().is_empty() {
        return Err(StreamProtocolError::EmptyField {
            field: "tool identifier",
        });
    }
    Ok(())
}

fn validate_chat_result(response: &ChatResult) -> Result<(), StreamProtocolError> {
    validate_text(
        "model response",
        response.content(),
        MAX_STREAM_TEXT_BYTES,
        response.tool_calls().is_empty(),
    )?;
    if let Some(reason) = response.finish_reason() {
        validate_text(
            "finish reason",
            reason,
            MAX_STREAM_FINISH_REASON_BYTES,
            true,
        )?;
    }
    if !response.tool_calls().is_empty() {
        ToolCallBatch::new(1, response.tool_calls().to_vec())
            .map_err(|error| StreamProtocolError::InvalidChatResult(error.to_string()))?;
    }
    Ok(())
}

fn validate_code(code: &str) -> Result<(), StreamProtocolError> {
    if code.is_empty() {
        return Err(StreamProtocolError::EmptyField {
            field: "stream error code",
        });
    }
    if code.len() > MAX_STREAM_ERROR_CODE_BYTES {
        return Err(StreamProtocolError::FieldTooLong {
            field: "stream error code",
            length: code.len(),
            maximum: MAX_STREAM_ERROR_CODE_BYTES,
        });
    }
    if !code.chars().enumerate().all(|(index, character)| {
        (index == 0 && character.is_ascii_lowercase())
            || (index > 0
                && (character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '-' | '_')))
    }) {
        return Err(StreamProtocolError::InvalidErrorCode {
            code: code.to_string(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    require_non_empty: bool,
) -> Result<(), StreamProtocolError> {
    if require_non_empty && value.trim().is_empty() {
        return Err(StreamProtocolError::EmptyField { field });
    }
    if value.contains('\0') {
        return Err(StreamProtocolError::NulCharacter { field });
    }
    if value.len() > maximum {
        return Err(StreamProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn normalize_code(value: String) -> String {
    let code = truncate(value, MAX_STREAM_ERROR_CODE_BYTES);
    if validate_code(&code).is_ok() {
        code
    } else {
        "stream_error".to_string()
    }
}

fn truncate(value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids() -> (crate::ToolCallId, crate::ToolName) {
        (
            crate::ToolCallId::new("call-1").expect("call id"),
            crate::ToolName::new("shell.execute").expect("tool name"),
        )
    }

    fn result() -> crate::ToolResult {
        let (call_id, tool_name) = ids();
        crate::ToolResult::new(
            1,
            call_id,
            tool_name,
            crate::ToolResultOutcome::completed(json!({"ok": true})).expect("outcome"),
        )
        .expect("result")
    }

    #[test]
    fn every_event_round_trips_with_explicit_type_and_bounds() {
        let (call_id, tool_name) = ids();
        let events = [
            StreamEvent::text_delta(1, "turn-1", 1, "hello").expect("text"),
            StreamEvent::tool_start(2, "turn-1", 1, call_id.clone(), tool_name.clone())
                .expect("start"),
            StreamEvent::tool_progress(3, "turn-1", 1, call_id, tool_name, "working")
                .expect("progress"),
            StreamEvent::tool_result(4, "turn-1", 1, result()).expect("result"),
            StreamEvent::turn_state(5, "turn-1", 1, StreamTurnState::ModelCalling).expect("state"),
            StreamEvent::turn_error(
                6,
                "turn-1",
                1,
                StreamError::redacted("provider_failed", true),
            )
            .expect("error"),
            StreamEvent::turn_done(
                7,
                "turn-1",
                1,
                Some(ChatResult::new("done", Some("stop".to_string()))),
            )
            .expect("done"),
        ];

        for event in events {
            let json = serde_json::to_string(&event).expect("serialize event");
            assert!(json.contains("\"protocol_version\":1"));
            let decoded = serde_json::from_str::<StreamEvent>(&json).expect("decode event");
            assert_eq!(decoded, event);
            decoded.validate().expect("validated event");
        }
    }

    #[test]
    fn malformed_or_oversized_wire_events_are_rejected() {
        let bad_version = r#"{"type":"text_delta","protocol_version":2,"sequence":1,"turn_id":"turn-1","round":1,"delta":"x"}"#;
        assert!(
            serde_json::from_str::<StreamEvent>(bad_version)
                .expect_err("version must fail")
                .to_string()
                .contains("unsupported")
        );

        let zero_sequence = r#"{"type":"turn_state","protocol_version":1,"sequence":0,"turn_id":"turn-1","round":0,"state":"created"}"#;
        assert!(serde_json::from_str::<StreamEvent>(zero_sequence).is_err());

        let oversized =
            StreamEvent::text_delta(1, "turn-1", 1, "x".repeat(MAX_STREAM_TEXT_BYTES + 1))
                .expect_err("oversized delta must fail");
        assert!(matches!(
            oversized,
            StreamProtocolError::FieldTooLong { .. }
        ));
    }

    #[test]
    fn redacted_errors_do_not_copy_sensitive_component_text() {
        let error = StreamError::redacted("model_failed", true);
        assert_eq!(error.message(), "turn operation failed");
        assert!(!error.message().contains("model_failed"));
    }

    #[test]
    fn stream_envelope_round_trips_events_and_cancellation() {
        let event = StreamEvent::text_delta(1, "turn-1", 1, "hello").expect("event");
        let envelopes = [
            StreamEnvelope::event(event).expect("event envelope"),
            StreamEnvelope::cancel("turn-1", "user cancelled").expect("cancel envelope"),
        ];
        for envelope in envelopes {
            let json = serde_json::to_string(&envelope).expect("serialize envelope");
            let decoded = serde_json::from_str::<StreamEnvelope>(&json).expect("decode envelope");
            assert_eq!(decoded, envelope);
            decoded.validate().expect("validate envelope");
        }
    }

    #[test]
    fn stream_envelope_rejects_unknown_versions_and_unbounded_cancel_reason() {
        let bad_version =
            r#"{"type":"cancel","protocol_version":2,"turn_id":"turn-1","reason":"stop"}"#;
        assert!(serde_json::from_str::<StreamEnvelope>(bad_version).is_err());
        let error =
            StreamEnvelope::cancel("turn-1", "x".repeat(MAX_STREAM_ERROR_MESSAGE_BYTES + 1))
                .expect_err("oversized reason");
        assert!(matches!(error, StreamProtocolError::FieldTooLong { .. }));
    }
}
