use std::fmt;
use std::fs;
use std::path::Path;

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::VoiceContractError;
use crate::identifiers::{RequestId, StreamId};

pub const MAX_AUDIO_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_STREAM_CHUNKS: usize = 4096;
pub const MAX_SAMPLE_RATE_HZ: u32 = 192_000;
pub const MAX_CHANNELS: u8 = 8;
/// File adapters use the same aggregate bound as one sidecar payload.  This
/// keeps a local file path from becoming an unbounded memory input.
pub const MAX_AUDIO_FILE_BYTES: usize = 256 * 1024;

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

    pub const fn bytes_per_sample(self) -> Option<usize> {
        match self.codec {
            AudioCodec::PcmS16Le => Some(2),
            AudioCodec::PcmF32Le => Some(4),
            AudioCodec::Opus => None,
        }
    }

    pub const fn is_linear_pcm(self) -> bool {
        self.bytes_per_sample().is_some()
    }
}

/// PCM bytes together with the format needed to interpret them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PcmAudio {
    pub format: AudioFormat,
    pub data: Vec<u8>,
}

impl PcmAudio {
    pub fn new(format: AudioFormat, data: Vec<u8>) -> Result<Self, AudioFileError> {
        format.validate().map_err(AudioFileError::Contract)?;
        let Some(bytes_per_sample) = format.bytes_per_sample() else {
            return Err(AudioFileError::UnsupportedCodec);
        };
        let frame_bytes = bytes_per_sample * usize::from(format.channels);
        if data.len() > MAX_AUDIO_FILE_BYTES {
            return Err(AudioFileError::TooLarge {
                size: data.len(),
                maximum: MAX_AUDIO_FILE_BYTES,
            });
        }
        if data.len() % frame_bytes != 0 {
            return Err(AudioFileError::InvalidWav);
        }
        Ok(Self { format, data })
    }
}

/// Errors from the standard-library audio file adapter.  File paths and OS
/// error text are deliberately not retained so they cannot leak into plugin
/// diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioFileError {
    Io,
    InvalidWav,
    UnsupportedCodec,
    TooLarge { size: usize, maximum: usize },
    Contract(VoiceContractError),
}

impl fmt::Display for AudioFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io => formatter.write_str("audio file IO failed"),
            Self::InvalidWav => formatter.write_str("invalid WAV/PCM data"),
            Self::UnsupportedCodec => formatter.write_str("audio codec is not linear PCM"),
            Self::TooLarge { size, maximum } => {
                write!(
                    formatter,
                    "audio file is {size} bytes, maximum is {maximum}"
                )
            }
            Self::Contract(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AudioFileError {}

/// Decode a little-endian RIFF/WAVE file containing PCM S16 or IEEE float32.
/// Only the `fmt ` and `data` chunks are required; metadata chunks are skipped.
pub fn decode_wav(bytes: &[u8]) -> Result<PcmAudio, AudioFileError> {
    if bytes.len() > MAX_AUDIO_FILE_BYTES || bytes.len() < 12 {
        return Err(if bytes.len() > MAX_AUDIO_FILE_BYTES {
            AudioFileError::TooLarge {
                size: bytes.len(),
                maximum: MAX_AUDIO_FILE_BYTES,
            }
        } else {
            AudioFileError::InvalidWav
        });
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioFileError::InvalidWav);
    }
    let declared_size = read_u32(&bytes[4..8]) as usize;
    if declared_size < 4 || declared_size.saturating_add(8) > bytes.len() {
        return Err(AudioFileError::InvalidWav);
    }

    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_size = read_u32(&bytes[offset + 4..offset + 8]) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(chunk_size)
            .ok_or(AudioFileError::InvalidWav)?;
        if end > bytes.len() {
            return Err(AudioFileError::InvalidWav);
        }
        match &bytes[offset..offset + 4] {
            b"fmt " => {
                if chunk_size < 16 || format.is_some() {
                    return Err(AudioFileError::InvalidWav);
                }
                let audio_format = read_u16(&bytes[start..start + 2]);
                let channels = read_u16(&bytes[start + 2..start + 4]);
                let sample_rate = read_u32(&bytes[start + 4..start + 8]);
                let byte_rate = read_u32(&bytes[start + 8..start + 12]);
                let block_align = read_u16(&bytes[start + 12..start + 14]);
                let bits = read_u16(&bytes[start + 14..start + 16]);
                let codec = match (audio_format, bits) {
                    (1, 16) => AudioCodec::PcmS16Le,
                    (3, 32) => AudioCodec::PcmF32Le,
                    _ => return Err(AudioFileError::UnsupportedCodec),
                };
                let channels = u8::try_from(channels).map_err(|_| AudioFileError::InvalidWav)?;
                let decoded = AudioFormat::new(codec, sample_rate, channels)
                    .map_err(AudioFileError::Contract)?;
                let expected_block = decoded.bytes_per_sample().unwrap() * usize::from(channels);
                let expected_rate = sample_rate
                    .checked_mul(u32::try_from(expected_block).unwrap_or(u32::MAX))
                    .ok_or(AudioFileError::InvalidWav)?;
                if block_align != expected_block as u16 || byte_rate != expected_rate {
                    return Err(AudioFileError::InvalidWav);
                }
                format = Some(decoded);
            }
            b"data" => {
                if data.is_some() {
                    return Err(AudioFileError::InvalidWav);
                }
                data = Some(bytes[start..end].to_vec());
            }
            _ => {}
        }
        offset = end
            .checked_add(chunk_size % 2)
            .ok_or(AudioFileError::InvalidWav)?;
    }
    let format = format.ok_or(AudioFileError::InvalidWav)?;
    let data = data.ok_or(AudioFileError::InvalidWav)?;
    PcmAudio::new(format, data)
}

pub fn read_wav_file(path: &Path) -> Result<PcmAudio, AudioFileError> {
    let bytes = fs::read(path).map_err(|_| AudioFileError::Io)?;
    decode_wav(&bytes)
}

pub fn read_pcm_file(path: &Path, format: AudioFormat) -> Result<PcmAudio, AudioFileError> {
    let bytes = fs::read(path).map_err(|_| AudioFileError::Io)?;
    PcmAudio::new(format, bytes)
}

pub fn encode_wav(audio: &PcmAudio) -> Result<Vec<u8>, AudioFileError> {
    audio.format.validate().map_err(AudioFileError::Contract)?;
    let Some(bytes_per_sample) = audio.format.bytes_per_sample() else {
        return Err(AudioFileError::UnsupportedCodec);
    };
    if audio.data.len() > MAX_AUDIO_FILE_BYTES {
        return Err(AudioFileError::TooLarge {
            size: audio.data.len(),
            maximum: MAX_AUDIO_FILE_BYTES,
        });
    }
    let frame_bytes = bytes_per_sample * usize::from(audio.format.channels);
    if audio.data.len() % frame_bytes != 0 {
        return Err(AudioFileError::InvalidWav);
    }
    let data_len = u32::try_from(audio.data.len()).map_err(|_| AudioFileError::TooLarge {
        size: audio.data.len(),
        maximum: u32::MAX as usize,
    })?;
    let riff_size = 36u32
        .checked_add(data_len)
        .ok_or(AudioFileError::InvalidWav)?;
    let byte_rate = audio
        .format
        .sample_rate_hz
        .checked_mul(u32::try_from(frame_bytes).map_err(|_| AudioFileError::InvalidWav)?)
        .ok_or(AudioFileError::InvalidWav)?;
    let bits = u16::try_from(bytes_per_sample * 8).unwrap();
    let block_align = u16::try_from(frame_bytes).map_err(|_| AudioFileError::InvalidWav)?;
    let audio_format = match audio.format.codec {
        AudioCodec::PcmS16Le => 1u16,
        AudioCodec::PcmF32Le => 3u16,
        AudioCodec::Opus => return Err(AudioFileError::UnsupportedCodec),
    };
    let mut output = Vec::with_capacity(44 + audio.data.len());
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&riff_size.to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16u32.to_le_bytes());
    output.extend_from_slice(&audio_format.to_le_bytes());
    output.extend_from_slice(&(u16::from(audio.format.channels)).to_le_bytes());
    output.extend_from_slice(&audio.format.sample_rate_hz.to_le_bytes());
    output.extend_from_slice(&byte_rate.to_le_bytes());
    output.extend_from_slice(&block_align.to_le_bytes());
    output.extend_from_slice(&bits.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_len.to_le_bytes());
    output.extend_from_slice(&audio.data);
    Ok(output)
}

pub fn write_wav_file(path: &Path, audio: &PcmAudio) -> Result<(), AudioFileError> {
    let bytes = encode_wav(audio)?;
    fs::write(path, bytes).map_err(|_| AudioFileError::Io)
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
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
