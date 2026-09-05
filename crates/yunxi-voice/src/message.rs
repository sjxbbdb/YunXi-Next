use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::audio::{AudioChunk, AudioFormat, MAX_STREAM_CHUNKS, SynthesizedAudioChunk};
use crate::error::{VoiceContractError, validate_text};
use crate::identifiers::{RequestId, StreamId};
use crate::state::StreamStatus;
use crate::transcript::TranscriptEvent;

pub const VOICE_CAPABILITY_VERSION: u16 = 1;
pub const TRANSCRIBE_CAPABILITY: &str = "voice.transcribe";
pub const SYNTHESIZE_CAPABILITY: &str = "voice.synthesize";
pub const MAX_SYNTHESIS_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceCapability {
    #[serde(rename = "voice.transcribe")]
    Transcribe,
    #[serde(rename = "voice.synthesize")]
    Synthesize,
}

impl VoiceCapability {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Transcribe => TRANSCRIBE_CAPABILITY,
            Self::Synthesize => SYNTHESIZE_CAPABILITY,
        }
    }

    pub const fn version(self) -> u16 {
        VOICE_CAPABILITY_VERSION
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    pub capability: VoiceCapability,
    pub version: u16,
}

impl CapabilityDescriptor {
    pub fn new(capability: VoiceCapability) -> Self {
        Self {
            capability,
            version: capability.version(),
        }
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        if self.version != VOICE_CAPABILITY_VERSION {
            return Err(VoiceContractError::UnsupportedCapabilityVersion {
                version: self.version,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CapabilityDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            capability: VoiceCapability,
            version: u16,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        let descriptor = Self {
            capability: wire.capability,
            version: wire.version,
        };
        descriptor.validate().map_err(D::Error::custom)?;
        Ok(descriptor)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscribeRequest {
    pub request_id: RequestId,
    pub stream_id: StreamId,
    pub format: AudioFormat,
    pub chunks: Vec<AudioChunk>,
    pub input_complete: bool,
    pub status: StreamStatus,
}

impl TranscribeRequest {
    pub fn new(
        request_id: RequestId,
        stream_id: StreamId,
        format: AudioFormat,
        chunks: Vec<AudioChunk>,
        input_complete: bool,
        status: StreamStatus,
    ) -> Result<Self, VoiceContractError> {
        let request = Self {
            request_id,
            stream_id,
            format,
            chunks,
            input_complete,
            status,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        self.format.validate()?;
        self.status.validate()?;
        if self.chunks.is_empty() {
            return Err(VoiceContractError::Empty { field: "chunks" });
        }
        for chunk in &self.chunks {
            chunk.validate()?;
        }
        validate_chunks(&self.stream_id, &self.format, &self.chunks, |chunk| {
            (&chunk.stream_id, &chunk.format, chunk.sequence)
        })?;
        if self.input_complete && self.chunks.last().is_some_and(|chunk| !chunk.end_of_stream) {
            return Err(VoiceContractError::InvalidValue {
                field: "input_complete",
                message: "complete input must end with an end_of_stream chunk",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for TranscribeRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            request_id: RequestId,
            stream_id: StreamId,
            format: AudioFormat,
            chunks: Vec<AudioChunk>,
            input_complete: bool,
            status: StreamStatus,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(
            wire.request_id,
            wire.stream_id,
            wire.format,
            wire.chunks,
            wire.input_complete,
            wire.status,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynthesisRequest {
    pub request_id: RequestId,
    pub stream_id: StreamId,
    pub text: String,
    pub format: AudioFormat,
    pub status: StreamStatus,
}

impl SynthesisRequest {
    pub fn new(
        request_id: RequestId,
        stream_id: StreamId,
        text: impl Into<String>,
        format: AudioFormat,
        status: StreamStatus,
    ) -> Result<Self, VoiceContractError> {
        let request = Self {
            request_id,
            stream_id,
            text: text.into(),
            format,
            status,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        validate_text(
            "synthesis text",
            &self.text,
            MAX_SYNTHESIS_TEXT_BYTES,
            false,
        )?;
        self.format.validate()?;
        self.status.validate()
    }
}

impl<'de> Deserialize<'de> for SynthesisRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            request_id: RequestId,
            stream_id: StreamId,
            text: String,
            format: AudioFormat,
            status: StreamStatus,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(
            wire.request_id,
            wire.stream_id,
            wire.text,
            wire.format,
            wire.status,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscribeEvent {
    Transcript(TranscriptEvent),
    Status(StreamStatus),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SynthesisEvent {
    Audio(SynthesizedAudioChunk),
    Status(StreamStatus),
}

fn validate_chunks<T, F>(
    stream_id: &StreamId,
    format: &AudioFormat,
    chunks: &[T],
    fields: F,
) -> Result<(), VoiceContractError>
where
    F: Fn(&T) -> (&StreamId, &AudioFormat, u64),
{
    if chunks.len() > MAX_STREAM_CHUNKS {
        return Err(VoiceContractError::TooManyChunks {
            count: chunks.len(),
            maximum: MAX_STREAM_CHUNKS,
        });
    }
    for (index, chunk) in chunks.iter().enumerate() {
        let (chunk_stream, chunk_format, sequence) = fields(chunk);
        if chunk_stream != stream_id {
            return Err(VoiceContractError::MixedStream);
        }
        if chunk_format != format {
            return Err(VoiceContractError::MixedFormat);
        }
        let expected = index as u64;
        if sequence != expected {
            return Err(VoiceContractError::InvalidSequence {
                expected,
                actual: sequence,
            });
        }
    }
    Ok(())
}
