//! Bounded progress frames for a model invocation.
//!
//! These frames are an additive extension to the protocol-v2 invocation
//! envelope. A plugin that does not stream can continue to send only the
//! terminal invocation response. The host validates every progress frame
//! before forwarding it to an Agent event sink.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::StreamProtocolError;

pub const MAX_MODEL_STREAM_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_MODEL_STREAM_NAME_BYTES: usize = 256;
pub const MAX_MODEL_STREAM_ID_BYTES: usize = 256;
pub const MAX_MODEL_STREAM_FINISH_REASON_BYTES: usize = 128;
pub const MAX_MODEL_STREAM_TOOL_CALL_INDEX: usize = 1024;

/// A single bounded delta emitted while a model request is running.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        arguments: String,
    },
    Finished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl ModelStreamEvent {
    pub fn text_delta(text: impl Into<String>) -> Result<Self, StreamProtocolError> {
        let event = Self::TextDelta { text: text.into() };
        event.validate()?;
        Ok(event)
    }

    pub fn tool_call_delta(
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: impl Into<String>,
    ) -> Result<Self, StreamProtocolError> {
        let event = Self::ToolCallDelta {
            index,
            id,
            name,
            arguments: arguments.into(),
        };
        event.validate()?;
        Ok(event)
    }

    pub fn finished(reason: Option<String>) -> Result<Self, StreamProtocolError> {
        let event = Self::Finished { reason };
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), StreamProtocolError> {
        match self {
            Self::TextDelta { text } => {
                validate_text("model stream text delta", text, MAX_MODEL_STREAM_TEXT_BYTES)
            }
            Self::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                if *index > MAX_MODEL_STREAM_TOOL_CALL_INDEX {
                    return Err(StreamProtocolError::FieldTooLong {
                        field: "model stream tool call index",
                        length: *index,
                        maximum: MAX_MODEL_STREAM_TOOL_CALL_INDEX,
                    });
                }
                if let Some(id) = id {
                    validate_text("model stream tool call id", id, MAX_MODEL_STREAM_ID_BYTES)?;
                }
                if let Some(name) = name {
                    validate_text(
                        "model stream tool call name",
                        name,
                        MAX_MODEL_STREAM_NAME_BYTES,
                    )?;
                }
                validate_text(
                    "model stream tool call arguments",
                    arguments,
                    MAX_MODEL_STREAM_TEXT_BYTES,
                )
            }
            Self::Finished { reason } => {
                if let Some(reason) = reason {
                    validate_text(
                        "model stream finish reason",
                        reason,
                        MAX_MODEL_STREAM_FINISH_REASON_BYTES,
                    )?;
                }
                Ok(())
            }
        }
    }
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), StreamProtocolError> {
    if value.len() > maximum {
        return Err(StreamProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if value.contains('\0') {
        return Err(StreamProtocolError::NulCharacter { field });
    }
    Ok(())
}

impl fmt::Display for ModelStreamEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextDelta { .. } => formatter.write_str("text_delta"),
            Self::ToolCallDelta { .. } => formatter.write_str("tool_call_delta"),
            Self::Finished { .. } => formatter.write_str("finished"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_events_round_trip_and_validate() {
        let events = [
            ModelStreamEvent::text_delta("hello").expect("text delta"),
            ModelStreamEvent::tool_call_delta(
                MAX_MODEL_STREAM_TOOL_CALL_INDEX,
                Some("call-1".to_string()),
                Some("search".to_string()),
                "{\"q\":\"rust\"}",
            )
            .expect("tool delta"),
            ModelStreamEvent::finished(Some("stop".to_string())).expect("finished"),
        ];

        for event in events {
            event.validate().expect("event validates before encoding");
            let encoded = serde_json::to_string(&event).expect("encode event");
            let decoded: ModelStreamEvent = serde_json::from_str(&encoded).expect("decode event");
            assert_eq!(decoded, event);
            decoded.validate().expect("decoded event validates");
        }
    }

    #[test]
    fn length_limits_are_enforced_at_and_above_the_boundary() {
        ModelStreamEvent::text_delta("x".repeat(MAX_MODEL_STREAM_TEXT_BYTES))
            .expect("text at maximum is valid");
        assert!(matches!(
            ModelStreamEvent::text_delta("x".repeat(MAX_MODEL_STREAM_TEXT_BYTES + 1)),
            Err(StreamProtocolError::FieldTooLong { .. })
        ));
        assert!(matches!(
            ModelStreamEvent::tool_call_delta(
                MAX_MODEL_STREAM_TOOL_CALL_INDEX + 1,
                None,
                None,
                "{}"
            ),
            Err(StreamProtocolError::FieldTooLong { .. })
        ));
        assert!(matches!(
            ModelStreamEvent::tool_call_delta(
                0,
                Some("x".repeat(MAX_MODEL_STREAM_ID_BYTES + 1)),
                None,
                "{}"
            ),
            Err(StreamProtocolError::FieldTooLong { .. })
        ));
        assert!(matches!(
            ModelStreamEvent::tool_call_delta(
                0,
                None,
                Some("x".repeat(MAX_MODEL_STREAM_NAME_BYTES + 1)),
                "{}"
            ),
            Err(StreamProtocolError::FieldTooLong { .. })
        ));
        assert!(matches!(
            ModelStreamEvent::finished(Some("x".repeat(MAX_MODEL_STREAM_FINISH_REASON_BYTES + 1))),
            Err(StreamProtocolError::FieldTooLong { .. })
        ));
    }

    #[test]
    fn nul_characters_are_rejected_in_every_text_field() {
        let cases = [
            ModelStreamEvent::text_delta("bad\0text"),
            ModelStreamEvent::tool_call_delta(0, Some("bad\0id".to_string()), None, "{}"),
            ModelStreamEvent::tool_call_delta(0, None, Some("bad\0name".to_string()), "{}"),
            ModelStreamEvent::tool_call_delta(0, None, None, "bad\0args"),
            ModelStreamEvent::finished(Some("bad\0reason".to_string())),
        ];

        for result in cases {
            assert!(matches!(
                result,
                Err(StreamProtocolError::NulCharacter { .. })
            ));
        }
    }
}
