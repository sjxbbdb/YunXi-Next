use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WeixinContractError {
    Empty {
        field: &'static str,
    },
    TooLong {
        field: &'static str,
        size: usize,
        maximum: usize,
    },
    InvalidToken {
        field: &'static str,
    },
    InvalidText {
        field: &'static str,
    },
    InvalidValue {
        field: &'static str,
        message: &'static str,
    },
    InvalidDirection {
        expected: &'static str,
        actual: &'static str,
    },
    TooManyMedia {
        count: usize,
        maximum: usize,
    },
    MediaMetadataTooLarge {
        size: usize,
        maximum: usize,
    },
    BufferExceedsCapacity {
        buffered: usize,
        capacity: usize,
    },
    InvalidStateTransition {
        from: &'static str,
        to: &'static str,
    },
    RetryExhausted,
    UnsupportedCapabilityVersion {
        version: u16,
    },
}

impl fmt::Display for WeixinContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field} must not be empty"),
            Self::TooLong {
                field,
                size,
                maximum,
            } => write!(formatter, "{field} is {size} bytes, maximum is {maximum}"),
            Self::InvalidToken { field } => write!(formatter, "{field} contains an invalid token"),
            Self::InvalidText { field } => {
                write!(formatter, "{field} contains a disallowed control character")
            }
            Self::InvalidValue { field, message } => {
                write!(formatter, "{field} is invalid: {message}")
            }
            Self::InvalidDirection { expected, actual } => {
                write!(
                    formatter,
                    "expected {expected} direction, received {actual}"
                )
            }
            Self::TooManyMedia { count, maximum } => {
                write!(
                    formatter,
                    "message contains {count} media items, maximum is {maximum}"
                )
            }
            Self::MediaMetadataTooLarge { size, maximum } => write!(
                formatter,
                "media metadata is {size} bytes, maximum is {maximum}"
            ),
            Self::BufferExceedsCapacity { buffered, capacity } => write!(
                formatter,
                "buffered bytes {buffered} exceed capacity {capacity}"
            ),
            Self::InvalidStateTransition { from, to } => {
                write!(
                    formatter,
                    "invalid delivery state transition from {from} to {to}"
                )
            }
            Self::RetryExhausted => formatter.write_str("delivery retry budget is exhausted"),
            Self::UnsupportedCapabilityVersion { version } => {
                write!(formatter, "unsupported Weixin capability version {version}")
            }
        }
    }
}

impl Error for WeixinContractError {}

pub(crate) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), WeixinContractError> {
    if !allow_empty && value.is_empty() {
        return Err(WeixinContractError::Empty { field });
    }
    if value.len() > maximum {
        return Err(WeixinContractError::TooLong {
            field,
            size: value.len(),
            maximum,
        });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(WeixinContractError::InvalidText { field });
    }
    Ok(())
}

pub(crate) fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), WeixinContractError> {
    if value.is_empty() {
        return Err(WeixinContractError::Empty { field });
    }
    if value.len() > maximum {
        return Err(WeixinContractError::TooLong {
            field,
            size: value.len(),
            maximum,
        });
    }
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':' | '@')
    }) {
        return Err(WeixinContractError::InvalidToken { field });
    }
    Ok(())
}
