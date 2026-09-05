use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    ChatMessage, ChatResult, ChatRole, ToolApprovalDecision, ToolApprovalRequest, ToolCallBatch,
    ToolResult,
};

pub const DEFAULT_MAX_SESSION_EVENTS: usize = 512;
pub const DEFAULT_MAX_SESSION_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_SESSION_EVENT_BYTES: usize = 1024 * 1024;
pub const MAX_SESSION_TURN_ID_BYTES: usize = 128;
pub const MAX_SESSION_TEXT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionLimits {
    max_events: usize,
    max_bytes: usize,
    max_event_bytes: usize,
}

impl SessionLimits {
    pub const fn new(max_events: usize, max_bytes: usize, max_event_bytes: usize) -> Self {
        Self {
            max_events,
            max_bytes,
            max_event_bytes,
        }
    }

    pub const fn max_events(&self) -> usize {
        self.max_events
    }

    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub const fn max_event_bytes(&self) -> usize {
        self.max_event_bytes
    }

    pub fn validate(&self) -> Result<(), SessionError> {
        if self.max_events == 0 {
            return Err(SessionError::InvalidLimit {
                field: "max_events",
                value: self.max_events,
            });
        }
        if self.max_bytes == 0 {
            return Err(SessionError::InvalidLimit {
                field: "max_bytes",
                value: self.max_bytes,
            });
        }
        if self.max_event_bytes == 0 || self.max_event_bytes > self.max_bytes {
            return Err(SessionError::InvalidLimit {
                field: "max_event_bytes",
                value: self.max_event_bytes,
            });
        }
        Ok(())
    }
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_SESSION_EVENTS,
            DEFAULT_MAX_SESSION_BYTES,
            DEFAULT_MAX_SESSION_EVENT_BYTES,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    TurnStarted,
    ContextBuilt,
    ModelRequested,
    ModelResponded,
    ToolCallsRequested,
    ToolApprovalRequested,
    ToolApprovalResolved,
    ToolResultRecorded,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted {
        turn_id: String,
        message: ChatMessage,
    },
    ContextBuilt {
        turn_id: String,
        message_count: usize,
        tool_count: usize,
    },
    ModelRequested {
        turn_id: String,
        round: u16,
        message_count: usize,
        tool_count: usize,
    },
    ModelResponded {
        turn_id: String,
        round: u16,
        response: ChatResult,
    },
    ToolCallsRequested {
        turn_id: String,
        batch: ToolCallBatch,
    },
    ToolApprovalRequested {
        turn_id: String,
        request: ToolApprovalRequest,
    },
    ToolApprovalResolved {
        turn_id: String,
        decision: ToolApprovalDecision,
    },
    ToolResultRecorded {
        turn_id: String,
        result: ToolResult,
    },
    TurnCompleted {
        turn_id: String,
        content: String,
    },
    TurnFailed {
        turn_id: String,
        code: String,
        message: String,
        retryable: bool,
    },
    TurnCancelled {
        turn_id: String,
        reason: String,
    },
}

impl SessionEvent {
    pub fn kind(&self) -> SessionEventKind {
        match self {
            Self::TurnStarted { .. } => SessionEventKind::TurnStarted,
            Self::ContextBuilt { .. } => SessionEventKind::ContextBuilt,
            Self::ModelRequested { .. } => SessionEventKind::ModelRequested,
            Self::ModelResponded { .. } => SessionEventKind::ModelResponded,
            Self::ToolCallsRequested { .. } => SessionEventKind::ToolCallsRequested,
            Self::ToolApprovalRequested { .. } => SessionEventKind::ToolApprovalRequested,
            Self::ToolApprovalResolved { .. } => SessionEventKind::ToolApprovalResolved,
            Self::ToolResultRecorded { .. } => SessionEventKind::ToolResultRecorded,
            Self::TurnCompleted { .. } => SessionEventKind::TurnCompleted,
            Self::TurnFailed { .. } => SessionEventKind::TurnFailed,
            Self::TurnCancelled { .. } => SessionEventKind::TurnCancelled,
        }
    }

    pub fn turn_id(&self) -> &str {
        match self {
            Self::TurnStarted { turn_id, .. }
            | Self::ContextBuilt { turn_id, .. }
            | Self::ModelRequested { turn_id, .. }
            | Self::ModelResponded { turn_id, .. }
            | Self::ToolCallsRequested { turn_id, .. }
            | Self::ToolApprovalRequested { turn_id, .. }
            | Self::ToolApprovalResolved { turn_id, .. }
            | Self::ToolResultRecorded { turn_id, .. }
            | Self::TurnCompleted { turn_id, .. }
            | Self::TurnFailed { turn_id, .. }
            | Self::TurnCancelled { turn_id, .. } => turn_id,
        }
    }

    pub fn validate(&self) -> Result<(), SessionError> {
        validate_turn_id(self.turn_id())?;
        match self {
            Self::TurnStarted { message, .. } => {
                if message.role() != ChatRole::User {
                    return Err(SessionError::InvalidEvent {
                        message: "turn start message must have the user role".to_string(),
                    });
                }
                validate_text("user message", message.content(), MAX_SESSION_TEXT_BYTES)?;
            }
            Self::ContextBuilt { .. } | Self::ModelRequested { .. } => {}
            Self::ModelResponded {
                round, response, ..
            } => {
                validate_round(*round)?;
                if response.tool_calls().is_empty() {
                    validate_text("model response", response.content(), MAX_SESSION_TEXT_BYTES)?;
                } else {
                    if response.content().len() > MAX_SESSION_TEXT_BYTES
                        || response.content().contains('\0')
                    {
                        return Err(SessionError::InvalidEvent {
                            message: "model response exceeds the session text bound".to_string(),
                        });
                    }
                    ToolCallBatch::new(*round, response.tool_calls().to_vec()).map_err(
                        |error| SessionError::InvalidEvent {
                            message: error.to_string(),
                        },
                    )?;
                }
            }
            Self::ToolCallsRequested { batch, .. } => {
                batch
                    .validate()
                    .map_err(|error| SessionError::InvalidEvent {
                        message: error.to_string(),
                    })?
            }
            Self::ToolApprovalRequested { request, .. } => {
                request
                    .validate()
                    .map_err(|error| SessionError::InvalidEvent {
                        message: error.to_string(),
                    })?;
            }
            Self::ToolApprovalResolved { decision, .. } => {
                decision
                    .validate()
                    .map_err(|error| SessionError::InvalidEvent {
                        message: error.to_string(),
                    })?;
            }
            Self::ToolResultRecorded { result, .. } => {
                result
                    .validate()
                    .map_err(|error| SessionError::InvalidEvent {
                        message: error.to_string(),
                    })?;
            }
            Self::TurnCompleted { content, .. } => {
                validate_text("turn response", content, MAX_SESSION_TEXT_BYTES)?;
                if content.trim().is_empty() {
                    return Err(SessionError::InvalidEvent {
                        message: "completed turn response cannot be empty".to_string(),
                    });
                }
            }
            Self::TurnFailed { code, message, .. } => {
                validate_text("failure code", code, 128)?;
                validate_text("failure message", message, MAX_SESSION_TEXT_BYTES)?;
            }
            Self::TurnCancelled { reason, .. } => {
                validate_text("cancellation reason", reason, MAX_SESSION_TEXT_BYTES)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    sequence: u64,
    event: SessionEvent,
}

impl SessionRecord {
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn event(&self) -> &SessionEvent {
        &self.event
    }
}

#[derive(Clone, Debug)]
pub struct SessionLog {
    limits: SessionLimits,
    seed: Vec<ChatMessage>,
    records: Vec<SessionRecord>,
    bytes_used: usize,
    next_sequence: u64,
}

impl SessionLog {
    pub fn new() -> Self {
        Self::with_limits(SessionLimits::default()).expect("default session limits are valid")
    }

    pub fn with_limits(limits: SessionLimits) -> Result<Self, SessionError> {
        limits.validate()?;
        Ok(Self {
            limits,
            seed: Vec::new(),
            records: Vec::new(),
            bytes_used: 0,
            next_sequence: 1,
        })
    }

    /// Replace the conversation supplied by an outer session adapter.
    ///
    /// The seed is deliberately kept separate from the event log. A Web or
    /// CLI adapter may own the durable transcript and rehydrate the spine
    /// before a turn; copying that transcript into synthetic lifecycle events
    /// would distort the event stream and consume the bounded event budget.
    pub fn reset_with_messages(&mut self, messages: Vec<ChatMessage>) -> Result<(), SessionError> {
        validate_seed(&messages, self.limits.max_bytes())?;
        self.seed = messages;
        self.records.clear();
        self.bytes_used = 0;
        self.next_sequence = 1;
        Ok(())
    }

    pub fn seed(&self) -> &[ChatMessage] {
        &self.seed
    }

    pub fn append(&mut self, event: SessionEvent) -> Result<u64, SessionError> {
        event.validate()?;
        let encoded_size = serde_json::to_vec(&event)
            .map_err(|error| SessionError::Serialization(error.to_string()))?
            .len();
        if encoded_size > self.limits.max_event_bytes() {
            return Err(SessionError::EventTooLarge {
                size: encoded_size,
                maximum: self.limits.max_event_bytes(),
            });
        }
        if self.records.len() >= self.limits.max_events() {
            return Err(SessionError::EventLimitReached {
                maximum: self.limits.max_events(),
            });
        }
        let new_total =
            self.bytes_used
                .checked_add(encoded_size)
                .ok_or(SessionError::ByteLimitReached {
                    used: self.bytes_used,
                    incoming: encoded_size,
                    maximum: self.limits.max_bytes(),
                })?;
        if new_total > self.limits.max_bytes() {
            return Err(SessionError::ByteLimitReached {
                used: self.bytes_used,
                incoming: encoded_size,
                maximum: self.limits.max_bytes(),
            });
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SessionError::SequenceExhausted)?;
        self.records.push(SessionRecord { sequence, event });
        self.bytes_used = new_total;
        Ok(sequence)
    }

    pub fn limits(&self) -> SessionLimits {
        self.limits
    }

    pub fn records(&self) -> &[SessionRecord] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub const fn bytes_used(&self) -> usize {
        self.bytes_used
    }

    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn conversation(&self) -> Vec<ChatMessage> {
        let mut messages = self.seed.clone();
        for record in &self.records {
            match record.event() {
                SessionEvent::TurnStarted { message, .. } => messages.push(message.clone()),
                SessionEvent::ModelResponded { response, .. } => {
                    if response.tool_calls().is_empty() {
                        messages.push(ChatMessage::assistant(response.content()));
                    } else {
                        messages.push(ChatMessage::assistant_tool_calls(
                            response.tool_calls().to_vec(),
                        ));
                    }
                }
                SessionEvent::ToolResultRecorded { result, .. } => {
                    let content = serde_json::to_string(result.outcome()).unwrap_or_else(|error| {
                        format!("{{\"status\":\"failed\",\"message\":\"{error}\"}}")
                    });
                    messages.push(ChatMessage::tool_result(
                        result.call_id(),
                        result.tool_name(),
                        content,
                    ));
                }
                SessionEvent::ContextBuilt { .. }
                | SessionEvent::ModelRequested { .. }
                | SessionEvent::ToolCallsRequested { .. }
                | SessionEvent::ToolApprovalRequested { .. }
                | SessionEvent::ToolApprovalResolved { .. }
                | SessionEvent::TurnCompleted { .. }
                | SessionEvent::TurnFailed { .. }
                | SessionEvent::TurnCancelled { .. } => {}
            }
        }
        messages
    }
}

impl Default for SessionLog {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    InvalidLimit {
        field: &'static str,
        value: usize,
    },
    InvalidEvent {
        message: String,
    },
    InvalidSeed {
        message: String,
    },
    Serialization(String),
    EventTooLarge {
        size: usize,
        maximum: usize,
    },
    EventLimitReached {
        maximum: usize,
    },
    ByteLimitReached {
        used: usize,
        incoming: usize,
        maximum: usize,
    },
    SequenceExhausted,
}

impl SessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidLimit { .. } => "invalid_session_limit",
            Self::InvalidEvent { .. } => "invalid_session_event",
            Self::InvalidSeed { .. } => "invalid_session_seed",
            Self::Serialization(_) => "session_serialization_error",
            Self::EventTooLarge { .. } => "session_event_too_large",
            Self::EventLimitReached { .. } => "session_event_limit",
            Self::ByteLimitReached { .. } => "session_byte_limit",
            Self::SequenceExhausted => "session_sequence_exhausted",
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit { field, value } => {
                write!(formatter, "session limit {field} has invalid value {value}")
            }
            Self::InvalidEvent { message } => write!(formatter, "invalid session event: {message}"),
            Self::InvalidSeed { message } => write!(formatter, "invalid session seed: {message}"),
            Self::Serialization(message) => {
                write!(formatter, "session event serialization failed: {message}")
            }
            Self::EventTooLarge { size, maximum } => {
                write!(
                    formatter,
                    "session event is {size} bytes; maximum is {maximum}"
                )
            }
            Self::EventLimitReached { maximum } => {
                write!(
                    formatter,
                    "session event limit of {maximum} has been reached"
                )
            }
            Self::ByteLimitReached {
                used,
                incoming,
                maximum,
            } => write!(
                formatter,
                "session byte limit exceeded: {used} + {incoming} > {maximum}"
            ),
            Self::SequenceExhausted => formatter.write_str("session event sequence is exhausted"),
        }
    }
}

impl Error for SessionError {}

fn validate_turn_id(value: &str) -> Result<(), SessionError> {
    validate_text("turn id", value, MAX_SESSION_TURN_ID_BYTES)
}

fn validate_seed(messages: &[ChatMessage], maximum_bytes: usize) -> Result<(), SessionError> {
    let mut used = 0usize;
    for message in messages {
        if message.content().contains('\0') {
            return Err(SessionError::InvalidSeed {
                message: "conversation seed contains a NUL character".to_string(),
            });
        }
        if message.content().len() > MAX_SESSION_TEXT_BYTES {
            return Err(SessionError::InvalidSeed {
                message: "conversation seed message exceeds the text bound".to_string(),
            });
        }
        let encoded = serde_json::to_vec(message)
            .map_err(|error| SessionError::Serialization(error.to_string()))?;
        used = used
            .checked_add(encoded.len())
            .ok_or(SessionError::ByteLimitReached {
                used,
                incoming: encoded.len(),
                maximum: maximum_bytes,
            })?;
        if used > maximum_bytes {
            return Err(SessionError::ByteLimitReached {
                used: used.saturating_sub(encoded.len()),
                incoming: encoded.len(),
                maximum: maximum_bytes,
            });
        }
    }
    Ok(())
}

fn validate_round(round: u16) -> Result<(), SessionError> {
    if round == 0 {
        Err(SessionError::InvalidEvent {
            message: "round must be greater than zero".to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), SessionError> {
    if value.trim().is_empty() {
        return Err(SessionError::InvalidEvent {
            message: format!("{field} cannot be empty"),
        });
    }
    if value.contains('\0') {
        return Err(SessionError::InvalidEvent {
            message: format!("{field} contains a NUL character"),
        });
    }
    if value.len() > maximum {
        return Err(SessionError::InvalidEvent {
            message: format!("{field} exceeds {maximum} bytes"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_is_bounded_and_sequence_is_monotonic() {
        let mut log = SessionLog::with_limits(SessionLimits::new(1, 1024, 1024)).expect("limits");
        assert_eq!(
            log.append(SessionEvent::TurnStarted {
                turn_id: "turn-1".to_string(),
                message: ChatMessage::user("hello"),
            })
            .expect("append"),
            1
        );
        assert_eq!(log.next_sequence(), 2);
        let error = log
            .append(SessionEvent::TurnCompleted {
                turn_id: "turn-1".to_string(),
                content: "done".to_string(),
            })
            .expect_err("bounded log");
        assert!(matches!(error, SessionError::EventLimitReached { .. }));
    }

    #[test]
    fn reset_with_messages_keeps_seed_outside_the_lifecycle_event_budget() {
        let mut log = SessionLog::with_limits(SessionLimits::new(1, 1024, 1024)).expect("limits");
        log.reset_with_messages(vec![ChatMessage::user("from web")])
            .expect("seed");
        assert_eq!(log.conversation(), vec![ChatMessage::user("from web")]);
        log.append(SessionEvent::TurnStarted {
            turn_id: "turn-1".to_string(),
            message: ChatMessage::user("next"),
        })
        .expect("append");
        assert_eq!(
            log.conversation(),
            vec![ChatMessage::user("from web"), ChatMessage::user("next")]
        );
        log.reset_with_messages(vec![ChatMessage::user("replacement")])
            .expect("replace seed");
        assert_eq!(log.len(), 0);
        assert_eq!(log.conversation(), vec![ChatMessage::user("replacement")]);
    }
}
