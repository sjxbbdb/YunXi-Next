//! Standard-library local audio adapter.
//!
//! This adapter intentionally models explicit WAV/PCM files, rather than
//! pretending that a platform microphone API is available everywhere. It is
//! useful for real sidecar integration and CI: the bytes are read and written
//! on the local machine, while STT/TTS remains replaceable in a sidecar.

use std::fs;
use std::path::{Path, PathBuf};

use crate::audio::{
    AudioChunk, AudioFormat, MAX_AUDIO_CHUNK_BYTES, MAX_AUDIO_FILE_BYTES, PcmAudio, read_pcm_file,
    read_wav_file, write_wav_file,
};
use crate::identifiers::StreamId;
use crate::provider::{
    AudioChunkSink, Device, ProviderDescriptor, ProviderOutcome, SynthesizedAudioSource,
};
use crate::sidecar::{
    DeviceAvailability, DeviceDirection, DeviceGrant, DeviceId, DeviceInfo, EnumeratedDevices,
};
use crate::stream::{OperationContext, VoiceProviderError};

pub const LOCAL_INPUT_WAV_ENV: &str = "YUNXI_VOICE_INPUT_WAV";
pub const LOCAL_OUTPUT_WAV_ENV: &str = "YUNXI_VOICE_OUTPUT_WAV";
pub const LOCAL_INPUT_PCM_ENV: &str = "YUNXI_VOICE_INPUT_PCM";
pub const LOCAL_OUTPUT_PCM_ENV: &str = "YUNXI_VOICE_OUTPUT_PCM";
pub const LOCAL_INPUT_DEVICE_ID: &str = "local.file.input";
pub const LOCAL_OUTPUT_DEVICE_ID: &str = "local.file.output";

const LOCAL_PATH_BYTES: usize = 4096;
const LOCAL_FORMAT: AudioFormat = AudioFormat {
    codec: crate::audio::AudioCodec::PcmS16Le,
    sample_rate_hz: 16_000,
    channels: 1,
};

/// Explicit local file endpoints. No path is inherited by a sidecar; the
/// sidecar receives bounded PCM chunks through the normal request contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAudioConfig {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

impl LocalAudioConfig {
    pub fn new(
        input: Option<PathBuf>,
        output: Option<PathBuf>,
    ) -> Result<Self, VoiceProviderError> {
        let config = Self { input, output };
        config.validate()?;
        Ok(config)
    }

    pub fn from_environment() -> Result<Option<Self>, VoiceProviderError> {
        let input = first_path(LOCAL_INPUT_WAV_ENV, LOCAL_INPUT_PCM_ENV)?;
        let output = first_path(LOCAL_OUTPUT_WAV_ENV, LOCAL_OUTPUT_PCM_ENV)?;
        if input.is_none() && output.is_none() {
            return Ok(None);
        }
        Self::new(input, output).map(Some)
    }

    pub fn input_path(&self) -> Option<&Path> {
        self.input.as_deref()
    }

    pub fn output_path(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    /// Reports configured endpoint readiness without opening an audio device.
    pub fn enumerate_devices(&self) -> Result<EnumeratedDevices, VoiceProviderError> {
        let mut devices = Vec::new();
        if let Some(path) = &self.input {
            devices.push(DeviceInfo::new(
                DeviceId::new(LOCAL_INPUT_DEVICE_ID).map_err(VoiceProviderError::Contract)?,
                "Configured WAV/PCM input",
                DeviceDirection::Input,
                input_availability(path),
                vec![LOCAL_FORMAT],
            )?);
        }
        if let Some(path) = &self.output {
            devices.push(DeviceInfo::new(
                DeviceId::new(LOCAL_OUTPUT_DEVICE_ID).map_err(VoiceProviderError::Contract)?,
                "Configured WAV output",
                DeviceDirection::Output,
                output_availability(path),
                vec![LOCAL_FORMAT],
            )?);
        }
        EnumeratedDevices::new(devices)
    }

    fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.input.is_none() && self.output.is_none() {
            return Err(VoiceProviderError::invalid_provider("local_config_empty"));
        }
        for path in [&self.input, &self.output].into_iter().flatten() {
            if path.as_os_str().is_empty() || path.to_string_lossy().len() > LOCAL_PATH_BYTES {
                return Err(VoiceProviderError::invalid_provider("local_config_invalid"));
            }
        }
        Ok(())
    }
}

/// A Host-gated file-backed `Device`. It is a data-path adapter, not a claim
/// that an OS microphone or speaker is available.
pub struct LocalFileDevice {
    descriptor: ProviderDescriptor,
    config: LocalAudioConfig,
}

impl std::fmt::Debug for LocalFileDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalFileDevice")
            .field("input_configured", &self.config.input.is_some())
            .field("output_configured", &self.config.output.is_some())
            .finish()
    }
}

impl LocalFileDevice {
    pub fn new(config: LocalAudioConfig) -> Result<Self, VoiceProviderError> {
        Ok(Self {
            descriptor: ProviderDescriptor::new(
                "local.file-audio",
                "Local WAV/PCM file adapter",
                true,
                false,
                false,
            )?,
            config,
        })
    }

    pub fn config(&self) -> &LocalAudioConfig {
        &self.config
    }

    /// Capture is explicitly Host-authorized. The grant must name the local
    /// input endpoint and allow input before any file bytes are read.
    pub fn capture_with_grant(
        &mut self,
        format: AudioFormat,
        grant: &DeviceGrant,
        sink: &mut dyn AudioChunkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.check_grant(grant, LOCAL_INPUT_DEVICE_ID, DeviceDirection::Input)?;
        format.validate()?;
        let path = self
            .config
            .input_path()
            .ok_or_else(|| VoiceProviderError::provider_failure("device_unavailable", false))?;
        let audio = read_audio(path, format)?;
        let stream_id = StreamId::new("local-capture").map_err(VoiceProviderError::Contract)?;
        push_chunks(audio, stream_id, sink, context)?;
        Ok(ProviderOutcome::Completed)
    }

    /// Playback is explicitly Host-authorized and writes a real WAV or raw
    /// PCM file. The file extension selects the container (`.pcm` is raw).
    pub fn playback_with_grant(
        &mut self,
        format: AudioFormat,
        grant: &DeviceGrant,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.check_grant(grant, LOCAL_OUTPUT_DEVICE_ID, DeviceDirection::Output)?;
        format.validate()?;
        let path = self
            .config
            .output_path()
            .ok_or_else(|| VoiceProviderError::provider_failure("device_unavailable", false))?;
        let data = collect_output(format, source, context)?;
        if is_pcm_path(path) {
            fs::write(path, &data).map_err(|_| {
                VoiceProviderError::provider_failure("audio_file_write_failed", false)
            })?;
        } else {
            let audio = PcmAudio::new(format, data).map_err(audio_error)?;
            write_wav_file(path, &audio).map_err(audio_error)?;
        }
        Ok(ProviderOutcome::Completed)
    }

    fn check_grant(
        &self,
        grant: &DeviceGrant,
        device_id: &str,
        direction: DeviceDirection,
    ) -> Result<(), VoiceProviderError> {
        grant.validate()?;
        let expected = DeviceId::new(device_id).map_err(VoiceProviderError::Contract)?;
        if grant.allows(&expected, direction) {
            Ok(())
        } else {
            Err(VoiceProviderError::provider_failure(
                "device_grant_mismatch",
                false,
            ))
        }
    }
}

impl Device for LocalFileDevice {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &mut self,
        _format: AudioFormat,
        _sink: &mut dyn AudioChunkSink,
        _context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        Err(VoiceProviderError::provider_failure(
            "device_grant_required",
            false,
        ))
    }

    fn playback(
        &mut self,
        _format: AudioFormat,
        _source: &mut dyn SynthesizedAudioSource,
        _context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        Err(VoiceProviderError::provider_failure(
            "device_grant_required",
            false,
        ))
    }
}

fn first_path(primary: &str, fallback: &str) -> Result<Option<PathBuf>, VoiceProviderError> {
    let value = std::env::var_os(primary).or_else(|| std::env::var_os(fallback));
    let Some(value) = value else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    LocalAudioConfig::new(Some(path.clone()), None).map(|_| Some(path))
}

fn input_availability(path: &Path) -> DeviceAvailability {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && fs::File::open(path).is_ok() => {
            DeviceAvailability::Available
        }
        Ok(_) => DeviceAvailability::Unavailable,
        Err(_) => DeviceAvailability::Unavailable,
    }
}

fn output_availability(path: &Path) -> DeviceAvailability {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if (path.exists() && path.is_file()) || (!path.exists() && parent.is_dir()) {
        DeviceAvailability::Available
    } else {
        DeviceAvailability::Unavailable
    }
}

fn read_audio(path: &Path, expected: AudioFormat) -> Result<PcmAudio, VoiceProviderError> {
    let audio = if is_pcm_path(path) {
        read_pcm_file(path, expected)
    } else {
        read_wav_file(path)
    }
    .map_err(audio_error)?;
    if audio.format != expected {
        return Err(VoiceProviderError::provider_failure(
            "audio_format_mismatch",
            false,
        ));
    }
    Ok(audio)
}

fn push_chunks(
    audio: PcmAudio,
    stream_id: StreamId,
    sink: &mut dyn AudioChunkSink,
    context: &OperationContext,
) -> Result<(), VoiceProviderError> {
    if audio.data.is_empty() {
        let chunk = AudioChunk::new(stream_id, 0, audio.format, Vec::new(), true)?;
        sink.push(chunk, context)?;
        return Ok(());
    }
    let frame_bytes = audio.format.bytes_per_sample().unwrap() * usize::from(audio.format.channels);
    let chunk_bytes = MAX_AUDIO_CHUNK_BYTES - (MAX_AUDIO_CHUNK_BYTES % frame_bytes);
    for (sequence, bytes) in audio.data.chunks(chunk_bytes).enumerate() {
        context.check()?;
        let end = (sequence + 1) * chunk_bytes >= audio.data.len();
        let chunk = AudioChunk::new(
            stream_id.clone(),
            sequence as u64,
            audio.format,
            bytes.to_vec(),
            end,
        )?;
        sink.push(chunk, context)?;
    }
    Ok(())
}

fn collect_output(
    format: AudioFormat,
    source: &mut dyn SynthesizedAudioSource,
    context: &OperationContext,
) -> Result<Vec<u8>, VoiceProviderError> {
    let mut data = Vec::new();
    while let Some(chunk) = source.next(context)? {
        context.check()?;
        chunk.validate()?;
        if chunk.format != format {
            return Err(VoiceProviderError::provider_failure(
                "audio_format_mismatch",
                false,
            ));
        }
        if data.len().saturating_add(chunk.data.len()) > MAX_AUDIO_FILE_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "audio_file_too_large",
                false,
            ));
        }
        data.extend_from_slice(&chunk.data);
    }
    Ok(data)
}

fn is_pcm_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pcm"))
}

fn audio_error(error: crate::audio::AudioFileError) -> VoiceProviderError {
    match error {
        crate::audio::AudioFileError::Io => {
            VoiceProviderError::provider_failure("audio_file_read_failed", false)
        }
        crate::audio::AudioFileError::TooLarge { .. } => {
            VoiceProviderError::provider_failure("audio_file_too_large", false)
        }
        crate::audio::AudioFileError::UnsupportedCodec => {
            VoiceProviderError::provider_failure("audio_codec_unsupported", false)
        }
        crate::audio::AudioFileError::InvalidWav | crate::audio::AudioFileError::Contract(_) => {
            VoiceProviderError::provider_failure("audio_file_invalid", false)
        }
    }
}
