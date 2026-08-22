//! Errors for dsh-compatible JSON contracts.

use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebContractError {
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    InvalidField {
        field: &'static str,
    },
    JsonTooLarge {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    FrameTooLarge {
        length: usize,
        maximum: usize,
    },
    Json {
        message: String,
    },
    InvalidMessage {
        message: String,
    },
    InvalidEventChannel {
        method: String,
    },
}

impl fmt::Display for WebContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::InvalidField { field } => {
                write!(
                    formatter,
                    "{field} contains unsupported whitespace or characters"
                )
            }
            Self::JsonTooLarge {
                field,
                length,
                maximum,
            } => write!(
                formatter,
                "{field} JSON is {length} bytes; maximum is {maximum}"
            ),
            Self::FrameTooLarge { length, maximum } => {
                write!(
                    formatter,
                    "JSON frame is {length} bytes; maximum is {maximum}"
                )
            }
            Self::Json { message } => write!(formatter, "invalid JSON: {message}"),
            Self::InvalidMessage { message } => write!(formatter, "invalid RPC message: {message}"),
            Self::InvalidEventChannel { method } => {
                write!(formatter, "unsupported event channel method `{method}`")
            }
        }
    }
}

impl Error for WebContractError {}
