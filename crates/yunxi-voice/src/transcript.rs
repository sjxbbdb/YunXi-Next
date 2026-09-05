use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{VoiceContractError, validate_text};
use crate::identifiers::StreamId;

pub const MAX_TRANSCRIPT_TEXT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    Partial,
    Final,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscriptEvent {
    pub stream_id: StreamId,
    pub sequence: u64,
    pub kind: TranscriptKind,
    pub text: String,
}

impl TranscriptEvent {
    pub fn partial(
        stream_id: StreamId,
        sequence: u64,
        text: impl Into<String>,
    ) -> Result<Self, VoiceContractError> {
        Self::new(stream_id, sequence, TranscriptKind::Partial, text)
    }

    pub fn final_text(
        stream_id: StreamId,
        sequence: u64,
        text: impl Into<String>,
    ) -> Result<Self, VoiceContractError> {
        Self::new(stream_id, sequence, TranscriptKind::Final, text)
    }

    pub fn new(
        stream_id: StreamId,
        sequence: u64,
        kind: TranscriptKind,
        text: impl Into<String>,
    ) -> Result<Self, VoiceContractError> {
        let event = Self {
            stream_id,
            sequence,
            kind,
            text: text.into(),
        };
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        validate_text(
            "transcript text",
            &self.text,
            MAX_TRANSCRIPT_TEXT_BYTES,
            false,
        )
    }
}

impl<'de> Deserialize<'de> for TranscriptEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireTranscript {
            stream_id: StreamId,
            sequence: u64,
            kind: TranscriptKind,
            text: String,
        }

        let wire = WireTranscript::deserialize(deserializer)?;
        Self::new(wire.stream_id, wire.sequence, wire.kind, wire.text).map_err(D::Error::custom)
    }
}
