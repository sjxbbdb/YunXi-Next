use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoiceContractError {
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
    AudioChunkTooLarge {
        size: usize,
        maximum: usize,
    },
    EmptyAudioChunk,
    InvalidSequence {
        expected: u64,
        actual: u64,
    },
    TooManyChunks {
        count: usize,
        maximum: usize,
    },
    MixedStream,
    MixedFormat,
    BufferExceedsCapacity {
        buffered: usize,
        capacity: usize,
    },
    InvalidStateTransition {
        from: &'static str,
        to: &'static str,
    },
    UnsupportedCapabilityVersion {
        version: u16,
    },
}

impl fmt::Display for VoiceContractError {
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
            Self::AudioChunkTooLarge { size, maximum } => {
                write!(
                    formatter,
                    "audio chunk is {size} bytes, maximum is {maximum}"
                )
            }
            Self::EmptyAudioChunk => {
                formatter.write_str("audio chunk must contain data unless it ends the stream")
            }
            Self::InvalidSequence { expected, actual } => {
                write!(
                    formatter,
                    "expected audio sequence {expected}, received {actual}"
                )
            }
            Self::TooManyChunks { count, maximum } => {
                write!(
                    formatter,
                    "stream contains {count} chunks, maximum is {maximum}"
                )
            }
            Self::MixedStream => {
                formatter.write_str("all chunks in a request must use the same stream id")
            }
            Self::MixedFormat => {
                formatter.write_str("all chunks in a request must use the same audio format")
            }
            Self::BufferExceedsCapacity { buffered, capacity } => {
                write!(
                    formatter,
                    "buffered bytes {buffered} exceed capacity {capacity}"
                )
            }
            Self::InvalidStateTransition { from, to } => {
                write!(
                    formatter,
                    "invalid stream state transition from {from} to {to}"
                )
            }
            Self::UnsupportedCapabilityVersion { version } => {
                write!(formatter, "unsupported voice capability version {version}")
            }
        }
    }
}

impl Error for VoiceContractError {}

pub(crate) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), VoiceContractError> {
    if !allow_empty && value.is_empty() {
        return Err(VoiceContractError::Empty { field });
    }
    if value.len() > maximum {
        return Err(VoiceContractError::TooLong {
            field,
            size: value.len(),
            maximum,
        });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(VoiceContractError::InvalidText { field });
    }
    Ok(())
}

pub(crate) fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), VoiceContractError> {
    if value.is_empty() {
        return Err(VoiceContractError::Empty { field });
    }
    if value.len() > maximum {
        return Err(VoiceContractError::TooLong {
            field,
            size: value.len(),
            maximum,
        });
    }
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
    }) {
        return Err(VoiceContractError::InvalidToken { field });
    }
    Ok(())
}
