use std::fmt;

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::VoiceContractError;
use crate::identifiers::{RequestId, StreamId};

pub const MAX_AUDIO_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_STREAM_CHUNKS: usize = 4096;
pub const MAX_SAMPLE_RATE_HZ: u32 = 192_000;
pub const MAX_CHANNELS: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    PcmS16Le,
    PcmF32Le,
    Opus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AudioFormat {
    pub codec: AudioCodec,
    pub sample_rate_hz: u32,
    pub channels: u8,
}

impl AudioFormat {
    pub fn new(
        codec: AudioCodec,
        sample_rate_hz: u32,
        channels: u8,
    ) -> Result<Self, VoiceContractError> {
        let format = Self {
            codec,
            sample_rate_hz,
            channels,
        };
        format.validate()?;
        Ok(format)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        if !(8_000..=MAX_SAMPLE_RATE_HZ).contains(&self.sample_rate_hz) {
            return Err(VoiceContractError::InvalidValue {
                field: "sample_rate_hz",
                message: "must be between 8000 and 192000",
            });
        }
        if !(1..=MAX_CHANNELS).contains(&self.channels) {
            return Err(VoiceContractError::InvalidValue {
                field: "channels",
                message: "must be between 1 and 8",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AudioFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireFormat {
            codec: AudioCodec,
            sample_rate_hz: u32,
            channels: u8,
        }

        let wire = WireFormat::deserialize(deserializer)?;
        Self::new(wire.codec, wire.sample_rate_hz, wire.channels).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AudioChunk {
    pub stream_id: StreamId,
    pub sequence: u64,
    pub format: AudioFormat,
    pub data: Vec<u8>,
    pub end_of_stream: bool,
}

impl AudioChunk {
    pub fn new(
        stream_id: StreamId,
        sequence: u64,
        format: AudioFormat,
        data: Vec<u8>,
        end_of_stream: bool,
    ) -> Result<Self, VoiceContractError> {
        let chunk = Self {
            stream_id,
            sequence,
            format,
            data,
            end_of_stream,
        };
        chunk.validate()?;
        Ok(chunk)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        self.format.validate()?;
        if self.data.len() > MAX_AUDIO_CHUNK_BYTES {
            return Err(VoiceContractError::AudioChunkTooLarge {
                size: self.data.len(),
                maximum: MAX_AUDIO_CHUNK_BYTES,
            });
        }
        if self.data.is_empty() && !self.end_of_stream {
            return Err(VoiceContractError::EmptyAudioChunk);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AudioChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireChunk {
            stream_id: StreamId,
            sequence: u64,
            format: AudioFormat,
            data: BoundedAudioData,
            end_of_stream: bool,
        }

        let wire = WireChunk::deserialize(deserializer)?;
        Self::new(
            wire.stream_id,
            wire.sequence,
            wire.format,
            wire.data.0,
            wire.end_of_stream,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynthesizedAudioChunk {
    pub request_id: RequestId,
    pub stream_id: StreamId,
    pub sequence: u64,
    pub format: AudioFormat,
    pub data: Vec<u8>,
    pub end_of_stream: bool,
}

impl SynthesizedAudioChunk {
    pub fn new(
        request_id: RequestId,
        stream_id: StreamId,
        sequence: u64,
        format: AudioFormat,
        data: Vec<u8>,
        end_of_stream: bool,
    ) -> Result<Self, VoiceContractError> {
        let chunk = Self {
            request_id,
            stream_id,
            sequence,
            format,
            data,
            end_of_stream,
        };
        chunk.validate()?;
        Ok(chunk)
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        self.format.validate()?;
        if self.data.len() > MAX_AUDIO_CHUNK_BYTES {
            return Err(VoiceContractError::AudioChunkTooLarge {
                size: self.data.len(),
                maximum: MAX_AUDIO_CHUNK_BYTES,
            });
        }
        if self.data.is_empty() && !self.end_of_stream {
            return Err(VoiceContractError::EmptyAudioChunk);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SynthesizedAudioChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireChunk {
            request_id: RequestId,
            stream_id: StreamId,
            sequence: u64,
            format: AudioFormat,
            data: BoundedAudioData,
            end_of_stream: bool,
        }

        let wire = WireChunk::deserialize(deserializer)?;
        Self::new(
            wire.request_id,
            wire.stream_id,
            wire.sequence,
            wire.format,
            wire.data.0,
            wire.end_of_stream,
        )
        .map_err(D::Error::custom)
    }
}

struct BoundedAudioData(Vec<u8>);

impl<'de> Deserialize<'de> for BoundedAudioData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AudioDataVisitor;

        impl<'de> Visitor<'de> for AudioDataVisitor {
            type Value = BoundedAudioData;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "at most {MAX_AUDIO_CHUNK_BYTES} audio bytes")
            }

            fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_byte_buf(value.to_vec())
            }

            fn visit_borrowed_bytes<E>(self, value: &'de [u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_bytes(value)
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAX_AUDIO_CHUNK_BYTES {
                    return Err(E::custom(VoiceContractError::AudioChunkTooLarge {
                        size: value.len(),
                        maximum: MAX_AUDIO_CHUNK_BYTES,
                    }));
                }
                Ok(BoundedAudioData(value))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                if sequence
                    .size_hint()
                    .is_some_and(|size| size > MAX_AUDIO_CHUNK_BYTES)
                {
                    return Err(A::Error::custom(VoiceContractError::AudioChunkTooLarge {
                        size: sequence.size_hint().unwrap_or(MAX_AUDIO_CHUNK_BYTES + 1),
                        maximum: MAX_AUDIO_CHUNK_BYTES,
                    }));
                }
                let mut data = Vec::with_capacity(
                    sequence.size_hint().unwrap_or(0).min(MAX_AUDIO_CHUNK_BYTES),
                );
                while let Some(byte) = sequence.next_element()? {
                    if data.len() == MAX_AUDIO_CHUNK_BYTES {
                        return Err(A::Error::custom(VoiceContractError::AudioChunkTooLarge {
                            size: MAX_AUDIO_CHUNK_BYTES + 1,
                            maximum: MAX_AUDIO_CHUNK_BYTES,
                        }));
                    }
                    data.push(byte);
                }
                Ok(BoundedAudioData(data))
            }
        }

        deserializer.deserialize_bytes(AudioDataVisitor)
    }
}
