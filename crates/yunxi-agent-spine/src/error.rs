use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::cancellation::CancellationError;
use crate::session::SessionError;
use crate::state::AgentState;

pub const MAX_ERROR_CODE_BYTES: usize = 128;
pub const MAX_ERROR_MESSAGE_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentError {
    code: String,
    message: String,
    retryable: bool,
}

impl ComponentError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: normalize_code(code.into()),
            message: bounded_message(message.into()),
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
}

impl fmt::Display for ComponentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for ComponentError {}

pub type ContextError = ComponentError;
pub type ModelError = ComponentError;
pub type ToolError = ComponentError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    Rounds,
    ModelCalls,
    ToolCalls,
}

impl fmt::Display for BudgetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Rounds => "rounds",
            Self::ModelCalls => "model_calls",
            Self::ToolCalls => "tool_calls",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentError {
    InvalidInput(ComponentError),
    InvalidState {
        operation: String,
        state: AgentState,
    },
    Session(SessionError),
    Context(ComponentError),
    Model(ComponentError),
    Tool(ComponentError),
    BudgetExceeded {
        kind: BudgetKind,
        limit: u64,
        used: u64,
    },
    Cancelled(CancellationError),
    Protocol(ComponentError),
}

impl AgentError {
    pub fn invalid_input(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidInput(ComponentError::new(code, message, false))
    }

    pub fn protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Protocol(ComponentError::new(code, message, false))
    }

    pub fn code(&self) -> &str {
        match self {
            Self::InvalidInput(error)
            | Self::Context(error)
            | Self::Model(error)
            | Self::Tool(error)
            | Self::Protocol(error) => error.code(),
            Self::InvalidState { .. } => "invalid_state",
            Self::Session(error) => error.code(),
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::Cancelled(error) => error.code(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::InvalidInput(error)
            | Self::Context(error)
            | Self::Model(error)
            | Self::Tool(error)
            | Self::Protocol(error) => error.message().to_string(),
            Self::InvalidState { operation, state } => {
                format!("cannot {operation} while agent is {state}")
            }
            Self::Session(error) => error.to_string(),
            Self::BudgetExceeded { kind, limit, used } => {
                format!("{kind} budget exhausted: used {used}, limit {limit}")
            }
            Self::Cancelled(error) => error.reason().to_string(),
        }
    }

    pub const fn retryable(&self) -> bool {
        match self {
            Self::InvalidInput(_)
            | Self::InvalidState { .. }
            | Self::BudgetExceeded { .. }
            | Self::Cancelled(_)
            | Self::Protocol(_) => false,
            Self::Session(_) => false,
            Self::Context(error) | Self::Model(error) | Self::Tool(error) => error.retryable(),
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }

    pub fn cancellation_reason(&self) -> Option<&str> {
        match self {
            Self::Cancelled(error) => Some(error.reason()),
            _ => None,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

impl Error for AgentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Session(error) => Some(error),
            Self::Cancelled(error) => Some(error),
            Self::InvalidInput(error)
            | Self::Context(error)
            | Self::Model(error)
            | Self::Tool(error)
            | Self::Protocol(error) => Some(error),
            Self::InvalidState { .. } | Self::BudgetExceeded { .. } => None,
        }
    }
}

impl From<SessionError> for AgentError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<CancellationError> for AgentError {
    fn from(error: CancellationError) -> Self {
        Self::Cancelled(error)
    }
}

fn normalize_code(value: String) -> String {
    let value = truncate(value, MAX_ERROR_CODE_BYTES);
    if !value.is_empty()
        && value.chars().enumerate().all(|(index, character)| {
            (index == 0 && character.is_ascii_lowercase())
                || (index > 0
                    && (character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '.' | '-' | '_')))
        })
    {
        value
    } else {
        "component_error".to_string()
    }
}

fn bounded_message(value: String) -> String {
    let value = truncate(value, MAX_ERROR_MESSAGE_BYTES);
    if value.is_empty() {
        "component operation failed".to_string()
    } else {
        value
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
