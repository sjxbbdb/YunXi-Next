//! Errors raised while building or carrying a Web Gateway response.

use std::error::Error;
use std::fmt;

use yunxi_web_contract::{EventChannel, WebContractError};

use crate::events::EventJournalError;

#[derive(Debug)]
pub enum GatewayError {
    Contract(WebContractError),
    Json(serde_json::Error),
    EventJournal(EventJournalError),
    UnexpectedMessage {
        message: String,
    },
    EventQueueFull {
        channel: EventChannel,
        capacity: usize,
    },
    EventSequenceExhausted {
        channel: EventChannel,
    },
    ResponseTooLarge {
        kind: &'static str,
        length: usize,
        maximum: usize,
    },
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => {
                write!(formatter, "Web contract rejected the response: {error}")
            }
            Self::Json(error) => write!(
                formatter,
                "Gateway projection is not JSON serializable: {error}"
            ),
            Self::EventJournal(error) => write!(formatter, "event journal failure: {error}"),
            Self::UnexpectedMessage { message } => {
                write!(
                    formatter,
                    "Gateway expected a client-request message: {message}"
                )
            }
            Self::EventQueueFull { channel, capacity } => write!(
                formatter,
                "event channel `{}` reached its pending frame limit of {capacity}",
                channel.method()
            ),
            Self::EventSequenceExhausted { channel } => write!(
                formatter,
                "event channel `{}` exhausted its sequence space",
                channel.method()
            ),
            Self::ResponseTooLarge {
                kind,
                length,
                maximum,
            } => write!(
                formatter,
                "{kind} response is {length} bytes; maximum is {maximum}"
            ),
        }
    }
}

impl Error for GatewayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::EventJournal(error) => Some(error),
            Self::UnexpectedMessage { .. }
            | Self::EventQueueFull { .. }
            | Self::EventSequenceExhausted { .. }
            | Self::ResponseTooLarge { .. } => None,
        }
    }
}

impl From<WebContractError> for GatewayError {
    fn from(error: WebContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<serde_json::Error> for GatewayError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<EventJournalError> for GatewayError {
    fn from(error: EventJournalError) -> Self {
        Self::EventJournal(error)
    }
}
