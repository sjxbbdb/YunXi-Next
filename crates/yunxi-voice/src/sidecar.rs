//! Versioned sidecar/device provider contracts and routing.
//!
//! This module defines the process boundary a real device or speech SDK
//! adapter can implement.  It intentionally contains no process spawning,
//! socket code, device handles, filesystem paths, credentials, or SDK types.
//! A host supplies a bounded [`SidecarTransport`] implementation and retains
//! control of lifecycle, grants, and restart policy.

use std::collections::VecDeque;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::audio::{AudioChunk, AudioFormat, MAX_STREAM_CHUNKS, SynthesizedAudioChunk};
use crate::error::{VoiceContractError, validate_text, validate_token};
use crate::identifiers::{RequestId, StreamId};
use crate::message::{SynthesisRequest, TranscribeEvent, TranscribeRequest};
use crate::plugin::{CancelRequest, CancelResult};
use crate::provider::{
    AudioChunkSource, Device, LoopbackProvider, ProviderDescriptor, ProviderGate, ProviderOutcome,
    SynthesizedAudioSink, SynthesizedAudioSource, Synthesizer, TextFallback, Transcriber,
    TranscriptSink, VecSynthesizedAudioSink, VecTranscriptSink,
};
use crate::state::StreamStatus;
use crate::stream::{OperationContext, VoiceProviderError};
use crate::transcript::TranscriptEvent;

pub const SIDECAR_PROTOCOL_VERSION: u16 = 1;
pub const DOCTOR_OPERATION: &str = "doctor";
pub const DEVICE_ENUMERATION_OPERATION: &str = "enumerate_devices";
pub const SPEAK_OPERATION: &str = "speak";
pub const CHAT_OPERATION: &str = "chat";
pub const TALK_OPERATION: &str = "talk";
pub const PLAYBACK_OPERATION: &str = "playback";
pub const SAVE_OPERATION: &str = "save";
pub const AUDIO_OUTPUT_OPERATION: &str = "audio_output";

pub const MAX_DEVICES: usize = 64;
pub const MAX_DEVICE_NAME_BYTES: usize = 256;
pub const MAX_CHAT_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_SIDECAR_ERROR_CODE_BYTES: usize = 64;
pub const MAX_SIDECAR_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_DEVICE_FORMATS: usize = 8;
const MAX_DEVICE_GRANT_ID_BYTES: usize = 128;
const MAX_CHAT_EVENTS: usize = 256;
const MAX_TALK_EVENTS: usize = MAX_STREAM_CHUNKS * 3;

/// Feature bits negotiated by the external provider rather than inferred
/// from a fixture identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFeatures {
    pub doctor: bool,
    pub device_enumeration: bool,
    pub transcribe: bool,
    pub speak: bool,
    pub chat: bool,
    pub talk: bool,
    pub playback: bool,
    pub save: bool,
}

impl ProviderFeatures {
    pub const fn audio_and_text() -> Self {
        Self {
            doctor: true,
            device_enumeration: true,
            transcribe: true,
            speak: true,
            chat: true,
            talk: true,
            playback: true,
            save: true,
        }
    }

    /// Returns the first capability dependency that is inconsistent with a
    /// provider descriptor.  `talk` is a compound operation: it must be able
    /// to accept input and produce speech in addition to owning a device.
    fn invalid_dependency(&self, descriptor: &ProviderDescriptor) -> Option<&'static str> {
        if (self.device_enumeration || self.talk || self.playback) && !descriptor.supports_device {
            return Some("device_capability");
        }
        if (self.transcribe || self.talk) && !descriptor.supports_transcribe {
            return Some("transcribe_capability");
        }
        if (self.speak || self.talk) && !descriptor.supports_synthesize {
            return Some("speak_capability");
        }
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Result<Self, VoiceContractError> {
        let value = value.into();
        validate_token("device id", &value, crate::identifiers::MAX_STREAM_ID_BYTES)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Host-issued authority for one physical device and direction.
///
/// The grant is deliberately opaque to this crate. A sidecar may use the
/// identifier to correlate host authority, but it must not mint or broaden a
/// grant itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeviceGrant {
    pub grant_id: String,
    pub device_id: DeviceId,
    pub direction: DeviceDirection,
}

impl DeviceGrant {
    pub fn new(
        grant_id: impl Into<String>,
        device_id: DeviceId,
        direction: DeviceDirection,
    ) -> Result<Self, VoiceProviderError> {
        let grant = Self {
            grant_id: grant_id.into(),
            device_id,
            direction,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        validate_token("device grant id", &self.grant_id, MAX_DEVICE_GRANT_ID_BYTES)?;
        Ok(())
    }

    pub fn allows(&self, device_id: &DeviceId, direction: DeviceDirection) -> bool {
        self.device_id == *device_id
            && (self.direction == direction || self.direction == DeviceDirection::Duplex)
    }
}

impl<'de> Deserialize<'de> for DeviceGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDeviceGrant {
            grant_id: String,
            device_id: DeviceId,
            direction: DeviceDirection,
        }
        let wire = WireDeviceGrant::deserialize(deserializer)?;
        Self::new(wire.grant_id, wire.device_id, wire.direction).map_err(D::Error::custom)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct SaveDestinationId(String);

impl SaveDestinationId {
    pub fn new(value: impl Into<String>) -> Result<Self, VoiceContractError> {
        let value = value.into();
        validate_token(
            "save destination id",
            &value,
            crate::identifiers::MAX_STREAM_ID_BYTES,
        )?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SaveDestinationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceDirection {
    Input,
    Output,
    Duplex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAvailability {
    Available,
    Unavailable,
    PermissionDenied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub direction: DeviceDirection,
    pub availability: DeviceAvailability,
    pub formats: Vec<AudioFormat>,
}

impl DeviceInfo {
    pub fn new(
        id: DeviceId,
        name: impl Into<String>,
        direction: DeviceDirection,
        availability: DeviceAvailability,
        formats: Vec<AudioFormat>,
    ) -> Result<Self, VoiceProviderError> {
        let info = Self {
            id,
            name: name.into(),
            direction,
            availability,
            formats,
        };
        info.validate()?;
        Ok(info)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        validate_text("device name", &self.name, MAX_DEVICE_NAME_BYTES, false)?;
        if self.formats.len() > MAX_DEVICE_FORMATS {
            return Err(VoiceProviderError::provider_failure(
                "too_many_formats",
                false,
            ));
        }
        for format in &self.formats {
            format.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for DeviceInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDeviceInfo {
            id: DeviceId,
            name: String,
            direction: DeviceDirection,
            availability: DeviceAvailability,
            formats: Vec<AudioFormat>,
        }
        let wire = WireDeviceInfo::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.name,
            wire.direction,
            wire.availability,
            wire.formats,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnumeratedDevices {
    pub devices: Vec<DeviceInfo>,
}

impl EnumeratedDevices {
    pub fn new(devices: Vec<DeviceInfo>) -> Result<Self, VoiceProviderError> {
        let result = Self { devices };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.devices.len() > MAX_DEVICES {
            return Err(VoiceProviderError::provider_failure(
                "too_many_devices",
                false,
            ));
        }
        for device in &self.devices {
            device.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for EnumeratedDevices {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDevices {
            devices: Vec<DeviceInfo>,
        }
        Self::new(WireDevices::deserialize(deserializer)?.devices).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub status: DoctorStatus,
    pub code: String,
    pub device_count: usize,
    pub features: ProviderFeatures,
}

impl DoctorReport {
    pub fn new(
        status: DoctorStatus,
        code: impl Into<String>,
        device_count: usize,
        features: ProviderFeatures,
    ) -> Result<Self, VoiceProviderError> {
        let code = code.into();
        validate_token("doctor code", &code, MAX_SIDECAR_ERROR_CODE_BYTES)?;
        if device_count > MAX_DEVICES {
            return Err(VoiceProviderError::provider_failure(
                "too_many_devices",
                false,
            ));
        }
        Ok(Self {
            status,
            code,
            device_count,
            features,
        })
    }

    pub fn ready(
        features: ProviderFeatures,
        device_count: usize,
    ) -> Result<Self, VoiceProviderError> {
        Self::new(DoctorStatus::Ready, "ok", device_count, features)
    }

    pub fn unavailable(code: &str) -> Result<Self, VoiceProviderError> {
        Self::new(
            DoctorStatus::Unavailable,
            code,
            0,
            ProviderFeatures {
                doctor: true,
                device_enumeration: false,
                transcribe: false,
                speak: false,
                chat: true,
                talk: false,
                playback: false,
                save: false,
            },
        )
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        validate_token("doctor code", &self.code, MAX_SIDECAR_ERROR_CODE_BYTES)?;
        if self.device_count > MAX_DEVICES {
            return Err(VoiceProviderError::provider_failure(
                "too_many_devices",
                false,
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for DoctorReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDoctor {
            status: DoctorStatus,
            code: String,
            device_count: usize,
            features: ProviderFeatures,
        }
        let wire = WireDoctor::deserialize(deserializer)?;
        Self::new(wire.status, wire.code, wire.device_count, wire.features)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorRequest {
    pub request_id: RequestId,
}

impl DoctorRequest {
    pub fn new(request_id: RequestId) -> Self {
        Self { request_id }
    }
}

impl<'de> Deserialize<'de> for DoctorRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDoctorRequest {
            request_id: RequestId,
        }
        Ok(Self::new(
            WireDoctorRequest::deserialize(deserializer)?.request_id,
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceEnumerationRequest {
    pub request_id: RequestId,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct ChatRequest {
    pub request_id: RequestId,
    pub conversation_id: String,
    pub text: String,
}

impl fmt::Debug for ChatRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatRequest")
            .field("request_id", &self.request_id)
            .field("conversation_id", &self.conversation_id)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl ChatRequest {
    pub fn new(
        request_id: RequestId,
        conversation_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, VoiceProviderError> {
        let request = Self {
            request_id,
            conversation_id: conversation_id.into(),
            text: text.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        validate_token("conversation id", &self.conversation_id, 128)?;
        validate_text("chat text", &self.text, MAX_CHAT_TEXT_BYTES, false)?;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ChatRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireChatRequest {
            request_id: RequestId,
            conversation_id: String,
            text: String,
        }
        let wire = WireChatRequest::deserialize(deserializer)?;
        Self::new(wire.request_id, wire.conversation_id, wire.text).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatEventKind {
    Delta,
    Final,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct ChatEvent {
    pub kind: ChatEventKind,
    pub text: String,
}

impl fmt::Debug for ChatEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatEvent")
            .field("kind", &self.kind)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl ChatEvent {
    pub fn new(kind: ChatEventKind, text: impl Into<String>) -> Result<Self, VoiceProviderError> {
        let event = Self {
            kind,
            text: text.into(),
        };
        validate_text("chat event text", &event.text, MAX_CHAT_TEXT_BYTES, false)?;
        Ok(event)
    }
}

impl<'de> Deserialize<'de> for ChatEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireChatEvent {
            kind: ChatEventKind,
            text: String,
        }
        let wire = WireChatEvent::deserialize(deserializer)?;
        Self::new(wire.kind, wire.text).map_err(D::Error::custom)
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct TalkRequest {
    pub request_id: RequestId,
    pub input: TranscribeRequest,
    pub output_format: AudioFormat,
    pub status: StreamStatus,
    pub input_device_grant: Option<DeviceGrant>,
}

impl fmt::Debug for TalkRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TalkRequest")
            .field("request_id", &self.request_id)
            .field("stream_id", &self.input.stream_id)
            .field("chunk_count", &self.input.chunks.len())
            .field("output_format", &self.output_format)
            .finish()
    }
}

impl TalkRequest {
    pub fn new(
        request_id: RequestId,
        input: TranscribeRequest,
        output_format: AudioFormat,
        status: StreamStatus,
    ) -> Result<Self, VoiceProviderError> {
        let request = Self {
            request_id,
            input,
            output_format,
            status,
            input_device_grant: None,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        self.input.validate()?;
        self.output_format.validate()?;
        self.status.validate()?;
        if let Some(grant) = &self.input_device_grant {
            grant.validate()?;
            if !matches!(
                grant.direction,
                DeviceDirection::Input | DeviceDirection::Duplex
            ) {
                return Err(VoiceProviderError::provider_failure(
                    "device_grant_direction",
                    false,
                ));
            }
        }
        Ok(())
    }

    pub fn with_input_device_grant(mut self, grant: DeviceGrant) -> Self {
        self.input_device_grant = Some(grant);
        self
    }
}

impl<'de> Deserialize<'de> for TalkRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireTalkRequest {
            request_id: RequestId,
            input: TranscribeRequest,
            output_format: AudioFormat,
            status: StreamStatus,
            #[serde(default)]
            input_device_grant: Option<DeviceGrant>,
        }
        let wire = WireTalkRequest::deserialize(deserializer)?;
        let mut request = Self::new(wire.request_id, wire.input, wire.output_format, wire.status)
            .map_err(D::Error::custom)?;
        request.input_device_grant = wire.input_device_grant;
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TalkEvent {
    Transcript(TranscriptEvent),
    Chat(ChatEvent),
    Audio(SynthesizedAudioChunk),
    Status(StreamStatus),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSelection {
    Playback(DeviceId),
    Save(SaveDestinationId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AudioOutputRequest {
    pub request_id: RequestId,
    pub stream_id: StreamId,
    pub format: AudioFormat,
    pub selection: OutputSelection,
    pub status: StreamStatus,
    pub device_grant: Option<DeviceGrant>,
}

impl AudioOutputRequest {
    pub fn new(
        request_id: RequestId,
        stream_id: StreamId,
        format: AudioFormat,
        selection: OutputSelection,
        status: StreamStatus,
    ) -> Result<Self, VoiceProviderError> {
        let request = Self {
            request_id,
            stream_id,
            format,
            selection,
            status,
            device_grant: None,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        self.format.validate()?;
        self.status.validate()?;
        if let Some(grant) = &self.device_grant {
            grant.validate()?;
            match &self.selection {
                OutputSelection::Playback(device_id)
                    if grant.allows(device_id, DeviceDirection::Output) => {}
                OutputSelection::Playback(_) => {
                    return Err(VoiceProviderError::provider_failure(
                        "device_grant_mismatch",
                        false,
                    ));
                }
                OutputSelection::Save(_) => {
                    return Err(VoiceProviderError::provider_failure(
                        "device_grant_on_save",
                        false,
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn with_device_grant(mut self, grant: DeviceGrant) -> Self {
        self.device_grant = Some(grant);
        self
    }
}

impl<'de> Deserialize<'de> for AudioOutputRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireOutputRequest {
            request_id: RequestId,
            stream_id: StreamId,
            format: AudioFormat,
            selection: OutputSelection,
            status: StreamStatus,
            #[serde(default)]
            device_grant: Option<DeviceGrant>,
        }
        let wire = WireOutputRequest::deserialize(deserializer)?;
        let mut request = Self::new(
            wire.request_id,
            wire.stream_id,
            wire.format,
            wire.selection,
            wire.status,
        )
        .map_err(D::Error::custom)?;
        request.device_grant = wire.device_grant;
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputResult {
    pub chunks: usize,
    pub bytes: usize,
}

impl OutputResult {
    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.chunks > MAX_STREAM_CHUNKS {
            return Err(VoiceProviderError::provider_failure(
                "too_many_chunks",
                false,
            ));
        }
        if self.bytes > MAX_SIDECAR_PAYLOAD_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "payload_too_large",
                false,
            ));
        }
        Ok(())
    }
}

pub type SpeakRequest = SynthesisRequest;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarError {
    pub code: String,
    pub retryable: bool,
}

impl SidecarError {
    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        validate_token(
            "sidecar error code",
            &self.code,
            MAX_SIDECAR_ERROR_CODE_BYTES,
        )?;
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "payload", rename_all = "snake_case")]
pub enum SidecarRequest {
    Doctor(DoctorRequest),
    EnumerateDevices(DeviceEnumerationRequest),
    Transcribe(TranscribeRequest),
    Speak(SynthesisRequest),
    Chat(ChatRequest),
    Talk(TalkRequest),
    Playback {
        request: AudioOutputRequest,
        chunks: Vec<SynthesizedAudioChunk>,
    },
    Save {
        request: AudioOutputRequest,
        chunks: Vec<SynthesizedAudioChunk>,
    },
}

/// Versioned wire envelope. A transport should serialize this value as one
/// bounded frame and reject an unsupported version before dispatch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarRequestFrame {
    pub version: u16,
    pub request: SidecarRequest,
}

impl SidecarRequestFrame {
    pub fn new(request: SidecarRequest) -> Self {
        Self {
            version: SIDECAR_PROTOCOL_VERSION,
            request,
        }
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.version != SIDECAR_PROTOCOL_VERSION {
            return Err(VoiceProviderError::invalid_provider("protocol_version"));
        }
        self.request.validate()
    }
}

impl fmt::Debug for SidecarRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::Doctor(_) => DOCTOR_OPERATION,
            Self::EnumerateDevices(_) => DEVICE_ENUMERATION_OPERATION,
            Self::Transcribe(_) => crate::plugin::TRANSCRIBE_OPERATION,
            Self::Speak(_) => SPEAK_OPERATION,
            Self::Chat(_) => CHAT_OPERATION,
            Self::Talk(_) => TALK_OPERATION,
            Self::Playback { .. } => PLAYBACK_OPERATION,
            Self::Save { .. } => SAVE_OPERATION,
        };
        formatter
            .debug_struct("SidecarRequest")
            .field("protocol_version", &SIDECAR_PROTOCOL_VERSION)
            .field("operation", &operation)
            .finish()
    }
}

impl SidecarRequest {
    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        match self {
            Self::Doctor(request) => {
                let _ = request;
            }
            Self::EnumerateDevices(request) => {
                let _ = request;
            }
            Self::Transcribe(request) => request.validate()?,
            Self::Speak(request) => request.validate()?,
            Self::Chat(request) => request.validate()?,
            Self::Talk(request) => {
                request.validate()?;
                if request.input_device_grant.is_none() {
                    return Err(VoiceProviderError::provider_failure(
                        "device_grant_required",
                        false,
                    ));
                }
            }
            Self::Playback { request, chunks } | Self::Save { request, chunks } => {
                request.validate()?;
                let selection_is_valid = match self {
                    Self::Playback { .. } => {
                        matches!(request.selection, OutputSelection::Playback(_))
                    }
                    Self::Save { .. } => matches!(request.selection, OutputSelection::Save(_)),
                    _ => unreachable!("selection is checked only for output operations"),
                };
                if !selection_is_valid {
                    return Err(VoiceProviderError::provider_failure(
                        "wrong_output_selection",
                        false,
                    ));
                }
                if matches!(self, Self::Playback { .. }) && request.device_grant.is_none() {
                    return Err(VoiceProviderError::provider_failure(
                        "device_grant_required",
                        false,
                    ));
                }
                validate_output_chunks(request, chunks)?;
            }
        }
        if self.encoded_size_hint() > MAX_SIDECAR_PAYLOAD_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "payload_too_large",
                false,
            ));
        }
        Ok(())
    }

    fn encoded_size_hint(&self) -> usize {
        match self {
            Self::Doctor(_) | Self::EnumerateDevices(_) => 256,
            Self::Transcribe(request) => request
                .chunks
                .iter()
                .map(|chunk| chunk.data.len().saturating_mul(2))
                .sum::<usize>()
                .saturating_add(1024),
            Self::Speak(request) => request.text.len().saturating_add(1024),
            Self::Chat(request) => request.text.len().saturating_add(1024),
            Self::Talk(request) => request
                .input
                .chunks
                .iter()
                .map(|chunk| chunk.data.len().saturating_mul(2))
                .sum::<usize>()
                .saturating_add(2048),
            Self::Playback { chunks, .. } | Self::Save { chunks, .. } => chunks
                .iter()
                .map(|chunk| chunk.data.len().saturating_mul(2))
                .sum::<usize>()
                .saturating_add(1024),
        }
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "payload", rename_all = "snake_case")]
pub enum SidecarResponse {
    Doctor(DoctorReport),
    Devices(EnumeratedDevices),
    Transcripts(Vec<TranscribeEvent>),
    Speech(Vec<crate::SynthesisEvent>),
    Chat(Vec<ChatEvent>),
    Talk(Vec<TalkEvent>),
    Playback(OutputResult),
    Saved(OutputResult),
    Error(SidecarError),
}

impl fmt::Debug for SidecarResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Doctor(_) => "doctor",
            Self::Devices(_) => "devices",
            Self::Transcripts(events) => {
                return formatter
                    .debug_tuple("Transcripts")
                    .field(&events.len())
                    .finish();
            }
            Self::Speech(events) => {
                return formatter
                    .debug_tuple("Speech")
                    .field(&events.len())
                    .finish();
            }
            Self::Chat(events) => {
                return formatter.debug_tuple("Chat").field(&events.len()).finish();
            }
            Self::Talk(events) => {
                return formatter.debug_tuple("Talk").field(&events.len()).finish();
            }
            Self::Playback(_) => "playback",
            Self::Saved(_) => "saved",
            Self::Error(_) => "error",
        };
        formatter.write_str(name)
    }
}

impl SidecarResponse {
    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        let size_hint = match self {
            Self::Doctor(_) | Self::Devices(_) | Self::Playback(_) | Self::Saved(_) => 4096,
            Self::Transcripts(events) => events
                .iter()
                .map(|event| match event {
                    TranscribeEvent::Transcript(event) => event.text.len().saturating_mul(2),
                    TranscribeEvent::Status(_) => 1024,
                })
                .sum::<usize>(),
            Self::Speech(events) => events
                .iter()
                .map(|event| match event {
                    crate::SynthesisEvent::Audio(chunk) => chunk.data.len().saturating_mul(2),
                    crate::SynthesisEvent::Status(_) => 1024,
                })
                .sum::<usize>(),
            Self::Chat(events) => events
                .iter()
                .map(|event| event.text.len().saturating_mul(2))
                .sum::<usize>(),
            Self::Talk(events) => events
                .iter()
                .map(|event| match event {
                    TalkEvent::Transcript(event) => event.text.len().saturating_mul(2),
                    TalkEvent::Chat(event) => event.text.len().saturating_mul(2),
                    TalkEvent::Audio(chunk) => chunk.data.len().saturating_mul(2),
                    TalkEvent::Status(_) => 1024,
                })
                .sum::<usize>(),
            Self::Error(_) => 256,
        };
        if size_hint > MAX_SIDECAR_PAYLOAD_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "payload_too_large",
                false,
            ));
        }
        match self {
            Self::Doctor(report) => {
                DoctorReport::new(
                    report.status,
                    report.code.clone(),
                    report.device_count,
                    report.features,
                )?;
            }
            Self::Devices(devices) => devices.validate()?,
            Self::Transcripts(events) => {
                if events.len() > MAX_TALK_EVENTS {
                    return Err(VoiceProviderError::provider_failure(
                        "too_many_events",
                        false,
                    ));
                }
                for event in events {
                    match event {
                        TranscribeEvent::Transcript(event) => event.validate()?,
                        TranscribeEvent::Status(status) => status.validate()?,
                    }
                }
            }
            Self::Speech(events) => {
                if events.len() > MAX_TALK_EVENTS {
                    return Err(VoiceProviderError::provider_failure(
                        "too_many_events",
                        false,
                    ));
                }
                for event in events {
                    match event {
                        crate::SynthesisEvent::Audio(chunk) => chunk.validate()?,
                        crate::SynthesisEvent::Status(status) => status.validate()?,
                    }
                }
            }
            Self::Chat(events) => {
                if events.len() > MAX_CHAT_EVENTS {
                    return Err(VoiceProviderError::provider_failure(
                        "too_many_events",
                        false,
                    ));
                }
                for event in events {
                    ChatEvent::new(event.kind, event.text.clone())?;
                }
            }
            Self::Talk(events) => {
                if events.len() > MAX_TALK_EVENTS {
                    return Err(VoiceProviderError::provider_failure(
                        "too_many_events",
                        false,
                    ));
                }
                for event in events {
                    match event {
                        TalkEvent::Transcript(event) => event.validate()?,
                        TalkEvent::Chat(event) => {
                            ChatEvent::new(event.kind, event.text.clone())?;
                        }
                        TalkEvent::Audio(chunk) => chunk.validate()?,
                        TalkEvent::Status(status) => status.validate()?,
                    }
                }
            }
            Self::Playback(result) | Self::Saved(result) => {
                result.validate()?;
            }
            Self::Error(error) => error.validate()?,
        }
        Ok(())
    }
}

/// The only implementation-specific part of an external provider.
/// Implementations must enforce an IO timeout using `context`, cap one wire
/// message at [`MAX_SIDECAR_PAYLOAD_BYTES`], and never log the request body.
pub trait SidecarTransport: Send {
    fn exchange(
        &mut self,
        request: SidecarRequestFrame,
        context: &OperationContext,
    ) -> Result<SidecarResponseFrame, VoiceProviderError>;

    /// Drops any state associated with the current exchange. A process-backed
    /// transport uses this after a protocol violation so a bad response cannot
    /// be consumed by a later request. In-memory transports may keep this as a
    /// no-op.
    fn reset(&mut self) {}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarResponseFrame {
    pub version: u16,
    pub response: SidecarResponse,
}

impl SidecarResponseFrame {
    pub fn new(response: SidecarResponse) -> Self {
        Self {
            version: SIDECAR_PROTOCOL_VERSION,
            response,
        }
    }

    pub fn validate(&self) -> Result<(), VoiceProviderError> {
        if self.version != SIDECAR_PROTOCOL_VERSION {
            return Err(VoiceProviderError::invalid_provider("protocol_version"));
        }
        self.response.validate()
    }
}

/// External provider adapter. It is usable with a process, device runtime, or
/// SDK transport supplied by the application without linking any of those
/// implementations into this crate.
pub struct ExternalSidecarProvider<T> {
    descriptor: ProviderDescriptor,
    features: ProviderFeatures,
    gate: ProviderGate,
    transport: T,
}

impl<T> fmt::Debug for ExternalSidecarProvider<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalSidecarProvider")
            .field("provider_id", &self.descriptor.id)
            .field("api_version", &SIDECAR_PROTOCOL_VERSION)
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl<T> ExternalSidecarProvider<T> {
    pub fn new(
        descriptor: ProviderDescriptor,
        features: ProviderFeatures,
        transport: T,
    ) -> Result<Self, VoiceProviderError> {
        if descriptor.api_version != crate::VOICE_PROVIDER_API_VERSION {
            return Err(VoiceProviderError::invalid_provider("api_version"));
        }
        if let Some(code) = features.invalid_dependency(&descriptor) {
            return Err(VoiceProviderError::invalid_provider(code));
        }
        Ok(Self {
            descriptor,
            features,
            gate: ProviderGate::new(true),
            transport,
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Disables the route while the transport is discarded.  A process-backed
    /// transport therefore cannot keep a microphone or speaker operation
    /// alive across a provider restart.
    pub fn restart(&mut self)
    where
        T: SidecarTransport,
    {
        self.gate.disable();
        self.transport.reset();
        self.gate.enable();
    }

    fn require_feature(&self, enabled: bool) -> Result<(), VoiceProviderError> {
        if enabled {
            Ok(())
        } else {
            Err(VoiceProviderError::provider_failure(
                "unsupported_operation",
                false,
            ))
        }
    }

    fn require_playback_grant(
        &self,
        request: &AudioOutputRequest,
    ) -> Result<(), VoiceProviderError> {
        if request.device_grant.is_some() {
            Ok(())
        } else {
            Err(VoiceProviderError::provider_failure(
                "device_grant_required",
                false,
            ))
        }
    }

    fn require_input_grant(&self, request: &TalkRequest) -> Result<(), VoiceProviderError> {
        if request.input_device_grant.is_some() {
            Ok(())
        } else {
            Err(VoiceProviderError::provider_failure(
                "device_grant_required",
                false,
            ))
        }
    }

    fn exchange(
        &mut self,
        request: SidecarRequest,
        context: &OperationContext,
    ) -> Result<SidecarResponse, VoiceProviderError>
    where
        T: SidecarTransport,
    {
        self.gate.check()?;
        context.check()?;
        request.validate()?;
        let response = self
            .transport
            .exchange(SidecarRequestFrame::new(request), context)?;
        if let Err(error) = response.validate() {
            self.transport.reset();
            return Err(error);
        }
        context.check()?;
        if let SidecarResponse::Error(error) = response.response {
            return Err(VoiceProviderError::provider_failure(
                error.code,
                error.retryable,
            ));
        }
        Ok(response.response)
    }

    fn validate_doctor_report(&self, report: &DoctorReport) -> Result<(), VoiceProviderError> {
        report.validate()?;
        let advertised = report.features;
        let configured = self.features;
        let compatible = (!advertised.device_enumeration || configured.device_enumeration)
            && (!advertised.transcribe || configured.transcribe)
            && (!advertised.speak || configured.speak)
            && (!advertised.chat || configured.chat)
            && (!advertised.talk || configured.talk)
            && (!advertised.playback || configured.playback)
            && (!advertised.save || configured.save);
        if compatible {
            Ok(())
        } else {
            Err(VoiceProviderError::invalid_provider(
                "doctor_capability_mismatch",
            ))
        }
    }

    fn invalid_response<U>(&mut self) -> Result<U, VoiceProviderError>
    where
        T: SidecarTransport,
    {
        self.transport.reset();
        Err(VoiceProviderError::provider_failure(
            "invalid_response",
            false,
        ))
    }
}

pub trait ChatSink: Send {
    fn push(
        &mut self,
        event: ChatEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError>;
}

pub trait TalkSink: Send {
    fn push(
        &mut self,
        event: TalkEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError>;
}

pub trait VoiceProvider: Send {
    fn descriptor(&self) -> &ProviderDescriptor;
    fn features(&self) -> ProviderFeatures;

    /// Resets provider-owned runtime state. Providers that do not own a
    /// process may keep the default no-op implementation.
    fn restart(&mut self) {}

    fn doctor(&mut self, context: &OperationContext) -> Result<DoctorReport, VoiceProviderError>;
    fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError>;
    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
    fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
    fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
    fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
    fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError>;

    /// A provider-level cancellation acknowledgement and lifecycle reset.
    ///
    /// Existing providers remain source-compatible because the default only
    /// validates the request, resets provider state, and returns a terminal
    /// result. A process-backed provider can use this hook to tear down its
    /// child; a live operation should still be cancelled through its shared
    /// [`OperationContext`] token.
    fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        let result = cancelled_result(request)?;
        self.restart();
        Ok(result)
    }
}

fn cancelled_result(request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
    request.validate()?;
    let mut status = request.status.clone();
    if status.cancellation == crate::CancellationState::Requested {
        status.mark_cancelled()?;
    }
    Ok(CancelResult {
        stream_id: request.stream_id.clone(),
        status,
    })
}

// Keep the provider boundary object-safe for hosts that select a provider at
// runtime (for example, a configured sidecar versus the local fallback).
impl<T: VoiceProvider + ?Sized> VoiceProvider for Box<T> {
    fn descriptor(&self) -> &ProviderDescriptor {
        (**self).descriptor()
    }

    fn features(&self) -> ProviderFeatures {
        (**self).features()
    }

    fn doctor(&mut self, context: &OperationContext) -> Result<DoctorReport, VoiceProviderError> {
        (**self).doctor(context)
    }

    fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        (**self).enumerate_devices(context)
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        (**self).transcribe(request, source, sink, context)
    }

    fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        (**self).speak(request, sink, context)
    }

    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        (**self).chat(request, sink, context)
    }

    fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        (**self).talk(request, sink, context)
    }

    fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        (**self).playback(request, source, context)
    }

    fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        (**self).save(request, source, context)
    }

    fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        (**self).cancel(request)
    }
}

impl<T: SidecarTransport> VoiceProvider for ExternalSidecarProvider<T> {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn features(&self) -> ProviderFeatures {
        self.features
    }

    fn restart(&mut self) {
        ExternalSidecarProvider::restart(self);
    }

    fn doctor(&mut self, context: &OperationContext) -> Result<DoctorReport, VoiceProviderError> {
        self.require_feature(self.features.doctor)?;
        match self.exchange(
            SidecarRequest::Doctor(DoctorRequest::new(
                RequestId::new("doctor").map_err(VoiceProviderError::Contract)?,
            )),
            context,
        )? {
            SidecarResponse::Doctor(report) => {
                if let Err(error) = self.validate_doctor_report(&report) {
                    self.transport.reset();
                    return Err(error);
                }
                Ok(report)
            }
            _ => self.invalid_response(),
        }
    }

    fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        self.require_feature(self.features.device_enumeration)?;
        let request = DeviceEnumerationRequest {
            request_id: RequestId::new("enumerate-devices")
                .map_err(VoiceProviderError::Contract)?,
        };
        match self.exchange(SidecarRequest::EnumerateDevices(request), context)? {
            SidecarResponse::Devices(devices) => Ok(devices),
            _ => self.invalid_response(),
        }
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.require_feature(self.features.transcribe)?;
        request.validate()?;
        let input = collect_input_request(request, source, context)?;
        match self.exchange(SidecarRequest::Transcribe(input), context)? {
            SidecarResponse::Transcripts(events) => {
                if let Err(error) = validate_transcript_events(request, &events) {
                    self.transport.reset();
                    return Err(error);
                }
                for event in events {
                    match event {
                        TranscribeEvent::Transcript(event) => sink.push(event, context)?,
                        TranscribeEvent::Status(_) => {}
                    }
                }
                Ok(ProviderOutcome::Completed)
            }
            _ => self.invalid_response(),
        }
    }

    fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.require_feature(self.features.speak)?;
        match self.exchange(SidecarRequest::Speak(request.clone()), context)? {
            SidecarResponse::Speech(events) => {
                if let Err(error) = validate_synthesis_events(request, &events) {
                    self.transport.reset();
                    return Err(error);
                }
                for event in events {
                    if let crate::SynthesisEvent::Audio(chunk) = event {
                        sink.push(chunk, context)?;
                    }
                }
                Ok(ProviderOutcome::Completed)
            }
            _ => self.invalid_response(),
        }
    }

    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.require_feature(self.features.chat)?;
        match self.exchange(SidecarRequest::Chat(request.clone()), context)? {
            SidecarResponse::Chat(events) => {
                for event in events {
                    sink.push(event, context)?;
                }
                Ok(ProviderOutcome::Completed)
            }
            _ => self.invalid_response(),
        }
    }

    fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.require_feature(self.features.talk)?;
        self.require_input_grant(request)?;
        match self.exchange(SidecarRequest::Talk(request.clone()), context)? {
            SidecarResponse::Talk(events) => {
                if let Err(error) = validate_talk_events(request, &events) {
                    self.transport.reset();
                    return Err(error);
                }
                for event in events {
                    sink.push(event, context)?;
                }
                Ok(ProviderOutcome::Completed)
            }
            _ => self.invalid_response(),
        }
    }

    fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.require_feature(self.features.playback)?;
        self.require_playback_grant(request)?;
        let chunks = collect_output_chunks(request, source, context)?;
        let expected = output_result_for(&chunks);
        match self.exchange(
            SidecarRequest::Playback {
                request: request.clone(),
                chunks,
            },
            context,
        )? {
            SidecarResponse::Playback(result) => {
                if let Err(error) = validate_output_result(&result, &expected) {
                    self.transport.reset();
                    return Err(error);
                }
                Ok(ProviderOutcome::Completed)
            }
            _ => self.invalid_response(),
        }
    }

    fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        self.require_feature(self.features.save)?;
        let chunks = collect_output_chunks(request, source, context)?;
        let expected = output_result_for(&chunks);
        match self.exchange(
            SidecarRequest::Save {
                request: request.clone(),
                chunks,
            },
            context,
        )? {
            SidecarResponse::Saved(result) => {
                if let Err(error) = validate_output_result(&result, &expected) {
                    self.transport.reset();
                    return Err(error);
                }
                Ok(result)
            }
            _ => self.invalid_response(),
        }
    }

    fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        let result = cancelled_result(request)?;
        self.restart();
        Ok(result)
    }
}

fn collect_output_chunks(
    request: &AudioOutputRequest,
    source: &mut dyn SynthesizedAudioSource,
    context: &OperationContext,
) -> Result<Vec<SynthesizedAudioChunk>, VoiceProviderError> {
    request.validate()?;
    let mut chunks = Vec::new();
    let mut bytes = 0usize;
    loop {
        context.check()?;
        let Some(chunk) = source.next(context)? else {
            break;
        };
        if chunks.len() >= MAX_STREAM_CHUNKS {
            return Err(VoiceProviderError::provider_failure(
                "too_many_chunks",
                false,
            ));
        }
        chunk.validate()?;
        bytes = bytes.saturating_add(chunk.data.len());
        if bytes > MAX_SIDECAR_PAYLOAD_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "payload_too_large",
                false,
            ));
        }
        chunks.push(chunk);
    }
    validate_output_chunks(request, &chunks)?;
    Ok(chunks)
}

fn collect_input_request(
    request: &TranscribeRequest,
    source: &mut dyn AudioChunkSource,
    context: &OperationContext,
) -> Result<TranscribeRequest, VoiceProviderError> {
    let mut chunks = Vec::new();
    let mut bytes = 0usize;
    loop {
        context.check()?;
        let Some(chunk) = source.next(context)? else {
            break;
        };
        if chunks.len() >= MAX_STREAM_CHUNKS {
            return Err(VoiceProviderError::provider_failure(
                "too_many_chunks",
                false,
            ));
        }
        chunk.validate()?;
        bytes = bytes.saturating_add(chunk.data.len());
        if bytes > MAX_SIDECAR_PAYLOAD_BYTES {
            return Err(VoiceProviderError::provider_failure(
                "payload_too_large",
                false,
            ));
        }
        chunks.push(chunk);
    }
    TranscribeRequest::new(
        request.request_id.clone(),
        request.stream_id.clone(),
        request.format,
        chunks,
        request.input_complete,
        request.status.clone(),
    )
    .map_err(Into::into)
}

fn validate_transcript_events(
    request: &TranscribeRequest,
    events: &[TranscribeEvent],
) -> Result<(), VoiceProviderError> {
    let mut sequence = 0_u64;
    for event in events {
        if let TranscribeEvent::Transcript(event) = event {
            if event.stream_id != request.stream_id {
                return Err(VoiceProviderError::Contract(
                    VoiceContractError::MixedStream,
                ));
            }
            if event.sequence != sequence {
                return Err(VoiceProviderError::Contract(
                    VoiceContractError::InvalidSequence {
                        expected: sequence,
                        actual: event.sequence,
                    },
                ));
            }
            sequence = sequence.saturating_add(1);
        }
    }
    Ok(())
}

fn validate_synthesis_events(
    request: &SynthesisRequest,
    events: &[crate::SynthesisEvent],
) -> Result<(), VoiceProviderError> {
    let mut sequence = 0_u64;
    for event in events {
        if let crate::SynthesisEvent::Audio(chunk) = event {
            validate_synthesized_chunk(
                chunk,
                &request.request_id,
                &request.stream_id,
                request.format,
                sequence,
            )?;
            sequence = sequence.saturating_add(1);
        }
    }
    Ok(())
}

fn validate_talk_events(
    request: &TalkRequest,
    events: &[TalkEvent],
) -> Result<(), VoiceProviderError> {
    let mut transcript_sequence = 0_u64;
    let mut audio_sequence = 0_u64;
    for event in events {
        match event {
            TalkEvent::Transcript(event) => {
                if event.stream_id != request.input.stream_id {
                    return Err(VoiceProviderError::Contract(
                        VoiceContractError::MixedStream,
                    ));
                }
                if event.sequence != transcript_sequence {
                    return Err(VoiceProviderError::Contract(
                        VoiceContractError::InvalidSequence {
                            expected: transcript_sequence,
                            actual: event.sequence,
                        },
                    ));
                }
                transcript_sequence = transcript_sequence.saturating_add(1);
            }
            TalkEvent::Audio(chunk) => {
                validate_synthesized_chunk(
                    chunk,
                    &request.request_id,
                    &request.input.stream_id,
                    request.output_format,
                    audio_sequence,
                )?;
                audio_sequence = audio_sequence.saturating_add(1);
            }
            TalkEvent::Chat(_) | TalkEvent::Status(_) => {}
        }
    }
    Ok(())
}

fn validate_synthesized_chunk(
    chunk: &SynthesizedAudioChunk,
    request_id: &RequestId,
    stream_id: &StreamId,
    format: AudioFormat,
    expected_sequence: u64,
) -> Result<(), VoiceProviderError> {
    if chunk.request_id != *request_id || chunk.stream_id != *stream_id {
        return Err(VoiceProviderError::Contract(
            VoiceContractError::MixedStream,
        ));
    }
    if chunk.format != format {
        return Err(VoiceProviderError::Contract(
            VoiceContractError::MixedFormat,
        ));
    }
    if chunk.sequence != expected_sequence {
        return Err(VoiceProviderError::Contract(
            VoiceContractError::InvalidSequence {
                expected: expected_sequence,
                actual: chunk.sequence,
            },
        ));
    }
    Ok(())
}

fn validate_output_chunks(
    request: &AudioOutputRequest,
    chunks: &[SynthesizedAudioChunk],
) -> Result<(), VoiceProviderError> {
    if chunks.len() > MAX_STREAM_CHUNKS {
        return Err(VoiceProviderError::provider_failure(
            "too_many_chunks",
            false,
        ));
    }
    let mut bytes = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        chunk.validate()?;
        if chunk.request_id != request.request_id || chunk.stream_id != request.stream_id {
            return Err(VoiceProviderError::Contract(
                VoiceContractError::MixedStream,
            ));
        }
        if chunk.format != request.format {
            return Err(VoiceProviderError::Contract(
                VoiceContractError::MixedFormat,
            ));
        }
        if chunk.sequence != index as u64 {
            return Err(VoiceProviderError::Contract(
                VoiceContractError::InvalidSequence {
                    expected: index as u64,
                    actual: chunk.sequence,
                },
            ));
        }
        bytes = bytes.saturating_add(chunk.data.len());
    }
    if bytes > MAX_SIDECAR_PAYLOAD_BYTES {
        return Err(VoiceProviderError::provider_failure(
            "payload_too_large",
            false,
        ));
    }
    Ok(())
}

fn output_result_for(chunks: &[SynthesizedAudioChunk]) -> OutputResult {
    OutputResult {
        chunks: chunks.len(),
        bytes: chunks.iter().fold(0usize, |total, chunk| {
            total.saturating_add(chunk.data.len())
        }),
    }
}

fn validate_output_result(
    actual: &OutputResult,
    expected: &OutputResult,
) -> Result<(), VoiceProviderError> {
    actual.validate()?;
    if actual == expected {
        Ok(())
    } else {
        Err(VoiceProviderError::provider_failure(
            "output_result_mismatch",
            false,
        ))
    }
}

#[derive(Clone, Default)]
pub struct VecChatSink {
    events: Vec<ChatEvent>,
}

impl VecChatSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[ChatEvent] {
        &self.events
    }
}

impl fmt::Debug for VecChatSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VecChatSink")
            .field("event_count", &self.events.len())
            .finish()
    }
}

impl ChatSink for VecChatSink {
    fn push(
        &mut self,
        event: ChatEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        context.check()?;
        ChatEvent::new(event.kind, &event.text)?;
        self.events.push(event);
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct VecTalkSink {
    events: Vec<TalkEvent>,
}

impl VecTalkSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[TalkEvent] {
        &self.events
    }
}

impl TalkSink for VecTalkSink {
    fn push(
        &mut self,
        event: TalkEvent,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        context.check()?;
        match &event {
            TalkEvent::Transcript(event) => event.validate()?,
            TalkEvent::Chat(event) => {
                ChatEvent::new(event.kind, &event.text)?;
            }
            TalkEvent::Audio(chunk) => chunk.validate()?,
            TalkEvent::Status(status) => status.validate()?,
        }
        self.events.push(event);
        Ok(())
    }
}

/// Deterministic chat provider used by Loopback and isolation tests.
pub struct MockChatProvider {
    descriptor: ProviderDescriptor,
    reply: String,
}

impl fmt::Debug for MockChatProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockChatProvider")
            .field("provider_id", &self.descriptor.id)
            .field("reply_bytes", &self.reply.len())
            .finish()
    }
}

impl MockChatProvider {
    pub fn new(reply: impl Into<String>) -> Result<Self, VoiceProviderError> {
        let reply = reply.into();
        ChatEvent::new(ChatEventKind::Final, &reply)?;
        Ok(Self {
            descriptor: ProviderDescriptor::new(
                "mock.chat",
                "Mock text chat",
                false,
                false,
                false,
            )?,
            reply,
        })
    }

    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        context.check()?;
        request.validate()?;
        sink.push(ChatEvent::new(ChatEventKind::Final, &self.reply)?, context)?;
        Ok(ProviderOutcome::Completed)
    }
}

/// Full deterministic local provider. It uses the existing Mock primitives
/// and never opens a device or contacts a service.
pub struct LoopbackVoiceProvider {
    descriptor: ProviderDescriptor,
    features: ProviderFeatures,
    gate: ProviderGate,
    inner: LoopbackProvider,
    chat: MockChatProvider,
}

impl fmt::Debug for LoopbackVoiceProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoopbackVoiceProvider")
            .field("provider_id", &self.descriptor.id)
            .field("features", &self.features)
            .field("enabled", &self.gate.is_enabled())
            .finish()
    }
}

impl LoopbackVoiceProvider {
    pub fn new(
        chunks: Vec<AudioChunk>,
        partial_text: impl Into<String>,
        final_text: impl Into<String>,
        chat_reply: impl Into<String>,
    ) -> Result<Self, VoiceProviderError> {
        Ok(Self {
            descriptor: ProviderDescriptor::new(
                "loopback.voice",
                "Loopback voice",
                true,
                true,
                true,
            )?,
            features: ProviderFeatures::audio_and_text(),
            gate: ProviderGate::new(true),
            inner: LoopbackProvider::new(chunks, partial_text, final_text)?,
            chat: MockChatProvider::new(chat_reply)?,
        })
    }

    pub fn gate(&self) -> ProviderGate {
        self.gate.clone()
    }
}

impl VoiceProvider for LoopbackVoiceProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn features(&self) -> ProviderFeatures {
        self.features
    }

    fn doctor(&mut self, context: &OperationContext) -> Result<DoctorReport, VoiceProviderError> {
        self.gate.check()?;
        context.check()?;
        DoctorReport::ready(self.features, 1)
    }

    fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        self.gate.check()?;
        context.check()?;
        let format = AudioFormat::new(crate::AudioCodec::PcmS16Le, 16_000, 1)?;
        EnumeratedDevices::new(vec![DeviceInfo::new(
            DeviceId::new("loopback-default")?,
            "Loopback device",
            DeviceDirection::Duplex,
            DeviceAvailability::Available,
            vec![format],
        )?])
    }

    fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        self.inner
            .transcriber_mut()
            .transcribe(request, source, sink, context)
    }

    fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        self.inner
            .synthesizer_mut()
            .synthesize(request, sink, context)
    }

    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        self.chat.chat(request, sink, context)
    }

    fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        let mut source = crate::request_source(&request.input)?;
        let mut transcripts = VecTranscriptSink::new();
        self.transcribe(&request.input, &mut source, &mut transcripts, context)?;
        for event in transcripts.into_events() {
            sink.push(TalkEvent::Transcript(event), context)?;
        }
        let text = "loopback talk";
        let chat_request = ChatRequest::new(request.request_id.clone(), "talk", text)?;
        let mut chat = VecChatSink::new();
        self.chat(&chat_request, &mut chat, context)?;
        for event in chat.events().iter().cloned() {
            sink.push(TalkEvent::Chat(event), context)?;
        }
        let synthesis = SynthesisRequest::new(
            request.request_id.clone(),
            request.input.stream_id.clone(),
            text,
            request.output_format,
            request.status.clone(),
        )?;
        let mut audio = VecSynthesizedAudioSink::new();
        self.speak(&synthesis, &mut audio, context)?;
        for chunk in audio.into_chunks() {
            sink.push(TalkEvent::Audio(chunk), context)?;
        }
        Ok(ProviderOutcome::Completed)
    }

    fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        if !matches!(request.selection, OutputSelection::Playback(_)) {
            return Err(VoiceProviderError::provider_failure(
                "wrong_output_selection",
                false,
            ));
        }
        self.inner
            .device_mut()
            .playback(request.format, source, context)
    }

    fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        self.gate.check()?;
        request.validate()?;
        if !matches!(request.selection, OutputSelection::Save(_)) {
            return Err(VoiceProviderError::provider_failure(
                "wrong_output_selection",
                false,
            ));
        }
        let mut chunks = 0;
        let mut bytes: usize = 0;
        while let Some(chunk) = source.next(context)? {
            context.check()?;
            if chunk.format != request.format {
                return Err(VoiceProviderError::Contract(
                    VoiceContractError::MixedFormat,
                ));
            }
            chunks += 1;
            bytes = bytes.saturating_add(chunk.data.len());
            if chunks > MAX_STREAM_CHUNKS || bytes > MAX_SIDECAR_PAYLOAD_BYTES {
                return Err(VoiceProviderError::provider_failure(
                    "payload_too_large",
                    false,
                ));
            }
        }
        Ok(OutputResult { chunks, bytes })
    }

    fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        let result = cancelled_result(request)?;
        self.gate.check()?;
        Ok(result)
    }
}

/// Routes audio work through the provider and falls back only for disabled,
/// timed-out, or unavailable runtime errors. Text chat is deliberately held
/// separately and never checks the audio gate.
pub struct VoiceRouter<P, F, C> {
    provider: P,
    fallback: F,
    chat: C,
    gate: ProviderGate,
    generation: u64,
}

impl<P, F, C> VoiceRouter<P, F, C> {
    pub fn new(provider: P, fallback: F, chat: C) -> Self {
        Self {
            provider,
            fallback,
            chat,
            gate: ProviderGate::new(true),
            generation: 0,
        }
    }

    pub fn disable(&self) {
        self.gate.disable();
    }

    pub fn enable(&self) {
        self.gate.enable();
    }

    pub fn restart(&mut self)
    where
        P: VoiceProvider,
    {
        self.gate.disable();
        self.provider.restart();
        self.generation = self.generation.saturating_add(1);
        self.gate.enable();
    }

    pub fn is_enabled(&self) -> bool {
        self.gate.is_enabled()
    }

    /// Replaces the provider while its route is disabled. This is the hot
    /// swap point used by a host that selects a different sidecar at runtime.
    pub fn replace_provider(&mut self, provider: P) {
        self.gate.disable();
        self.provider = provider;
        self.generation = self.generation.saturating_add(1);
        self.gate.enable();
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }
}

impl<P, F, C> VoiceRouter<P, F, C>
where
    P: VoiceProvider,
    F: TextFallback,
    C: ChatProvider,
{
    pub fn doctor(&mut self, context: &OperationContext) -> DoctorReport {
        if let Err(error) = self.gate.check().and_then(|()| context.check()) {
            return DoctorReport::unavailable(if matches!(error, VoiceProviderError::Disabled) {
                "disabled"
            } else {
                "runtime_unavailable"
            })
            .unwrap_or_else(|_| unreachable!("static doctor code is valid"));
        }
        self.provider.doctor(context).unwrap_or_else(|error| {
            DoctorReport::unavailable(if error.is_unavailable() {
                "runtime_unavailable"
            } else {
                "provider_error"
            })
            .unwrap_or_else(|_| unreachable!("static doctor code is valid"))
        })
    }

    pub fn enumerate_devices(
        &mut self,
        context: &OperationContext,
    ) -> Result<EnumeratedDevices, VoiceProviderError> {
        self.gate.check()?;
        self.provider.enumerate_devices(context)
    }

    pub fn transcribe(
        &mut self,
        request: &TranscribeRequest,
        source: &mut dyn AudioChunkSource,
        sink: &mut dyn TranscriptSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        match self
            .gate
            .check()
            .and_then(|()| self.provider.transcribe(request, source, sink, context))
        {
            Ok(outcome) => Ok(outcome),
            Err(error) if error.is_unavailable() => {
                self.fallback.transcribe_text(request, sink, context)
            }
            Err(error) => Err(error),
        }
    }

    pub fn speak(
        &mut self,
        request: &SpeakRequest,
        sink: &mut dyn SynthesizedAudioSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        match self
            .gate
            .check()
            .and_then(|()| self.provider.speak(request, sink, context))
        {
            Ok(outcome) => Ok(outcome),
            Err(error) if error.is_unavailable() => {
                let _ = self.fallback.synthesize_text(request, context)?;
                Ok(ProviderOutcome::TextFallback)
            }
            Err(error) => Err(error),
        }
    }

    /// A healthy provider can supply voice-aware chat, while the independent
    /// text provider remains the deterministic fallback when audio runtime
    /// state is disabled, timed out, or otherwise unavailable. This keeps
    /// text chat available across voice-provider failures without making the
    /// configured sidecar's chat capability unreachable.
    pub fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        if self.gate.is_enabled() {
            match self.provider.chat(request, sink, context) {
                Ok(outcome) => return Ok(outcome),
                Err(error) if error.is_unavailable() => {}
                Err(error) => return Err(error),
            }
        }
        self.chat.chat(request, sink, context)
    }

    pub fn talk(
        &mut self,
        request: &TalkRequest,
        sink: &mut dyn TalkSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        match self
            .gate
            .check()
            .and_then(|()| self.provider.talk(request, sink, context))
        {
            Ok(outcome) => Ok(outcome),
            Err(error) if error.is_unavailable() => Ok(ProviderOutcome::TextFallback),
            Err(error) => Err(error),
        }
    }

    pub fn playback(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        match self
            .gate
            .check()
            .and_then(|()| self.provider.playback(request, source, context))
        {
            Ok(outcome) => Ok(outcome),
            Err(error) if error.is_unavailable() => Ok(ProviderOutcome::TextFallback),
            Err(error) => Err(error),
        }
    }

    pub fn save(
        &mut self,
        request: &AudioOutputRequest,
        source: &mut dyn SynthesizedAudioSource,
        context: &OperationContext,
    ) -> Result<OutputResult, VoiceProviderError> {
        match self
            .gate
            .check()
            .and_then(|()| self.provider.save(request, source, context))
        {
            Ok(result) => Ok(result),
            Err(error) if error.is_unavailable() => Ok(OutputResult {
                chunks: 0,
                bytes: 0,
            }),
            Err(error) => Err(error),
        }
    }

    /// A cancellation request is accepted even while the audio route is
    /// disabled, so the Host can always acknowledge user intent. The shared
    /// operation context remains the immediate cancellation mechanism for an
    /// operation that is currently running.
    pub fn cancel(&mut self, request: &CancelRequest) -> Result<CancelResult, VoiceProviderError> {
        if !self.gate.is_enabled() {
            return cancelled_result(request);
        }
        self.provider.cancel(request)
    }
}

pub trait ChatProvider: Send {
    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError>;
}

impl ChatProvider for MockChatProvider {
    fn chat(
        &mut self,
        request: &ChatRequest,
        sink: &mut dyn ChatSink,
        context: &OperationContext,
    ) -> Result<ProviderOutcome, VoiceProviderError> {
        Self::chat(self, request, sink, context)
    }
}

/// Deterministic transport for contract tests. It records only operation
/// kinds and returns preloaded responses; request payloads are never exposed.
pub struct ScriptedSidecarTransport {
    responses: VecDeque<Result<SidecarResponseFrame, VoiceProviderError>>,
    request_count: usize,
    reset_count: usize,
}

impl fmt::Debug for ScriptedSidecarTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedSidecarTransport")
            .field("queued_responses", &self.responses.len())
            .field("request_count", &self.request_count)
            .field("reset_count", &self.reset_count)
            .finish()
    }
}

impl ScriptedSidecarTransport {
    pub fn new<R>(responses: impl IntoIterator<Item = Result<R, VoiceProviderError>>) -> Self
    where
        R: Into<SidecarResponseFrame>,
    {
        Self {
            responses: responses
                .into_iter()
                .map(|response| response.map(Into::into))
                .collect(),
            request_count: 0,
            reset_count: 0,
        }
    }

    pub fn request_count(&self) -> usize {
        self.request_count
    }

    pub fn reset_count(&self) -> usize {
        self.reset_count
    }
}

impl SidecarTransport for ScriptedSidecarTransport {
    fn exchange(
        &mut self,
        request: SidecarRequestFrame,
        context: &OperationContext,
    ) -> Result<SidecarResponseFrame, VoiceProviderError> {
        context.check()?;
        request.validate()?;
        self.request_count = self.request_count.saturating_add(1);
        self.responses.pop_front().unwrap_or_else(|| {
            Err(VoiceProviderError::provider_failure(
                "sidecar_unavailable",
                true,
            ))
        })
    }

    fn reset(&mut self) {
        self.reset_count = self.reset_count.saturating_add(1);
    }
}

impl From<SidecarResponse> for SidecarResponseFrame {
    fn from(response: SidecarResponse) -> Self {
        Self::new(response)
    }
}

/// A compact error alias for callers that want to classify router failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoiceRuntimeError {
    Provider(VoiceProviderError),
    TextFallback(VoiceProviderError),
}

impl From<VoiceProviderError> for VoiceRuntimeError {
    fn from(error: VoiceProviderError) -> Self {
        Self::Provider(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioCodec, RequestId, VecSynthesizedAudioSource};

    fn descriptor() -> ProviderDescriptor {
        ProviderDescriptor::new("external.voice", "External voice", true, true, true)
            .expect("descriptor")
    }

    #[test]
    fn sidecar_frames_are_versioned_bounded_and_redacted() {
        let request = ChatRequest::new(
            RequestId::new("request").expect("request id"),
            "conversation",
            "secret text",
        )
        .expect("chat request");
        let debug = format!("{request:?}");
        assert!(!debug.contains("secret text"));
        let frame = SidecarRequestFrame::new(SidecarRequest::Chat(request));
        assert!(frame.validate().is_ok());
        let mut wrong_version = frame;
        wrong_version.version = SIDECAR_PROTOCOL_VERSION + 1;
        assert!(matches!(
            wrong_version.validate(),
            Err(VoiceProviderError::InvalidProvider { code }) if code == "protocol_version"
        ));
        let too_many = EnumeratedDevices { devices: vec![] };
        assert!(too_many.validate().is_ok());
    }

    #[test]
    fn external_provider_forwards_contract_and_rejects_unavailable_response() {
        let report = DoctorReport::ready(ProviderFeatures::audio_and_text(), 0).expect("report");
        let transport = ScriptedSidecarTransport::new([Ok(SidecarResponse::Doctor(report))]);
        let mut provider = ExternalSidecarProvider::new(
            descriptor(),
            ProviderFeatures::audio_and_text(),
            transport,
        )
        .expect("provider");
        let result = provider
            .doctor(&OperationContext::with_timeout(
                std::time::Duration::from_secs(1),
            ))
            .expect("doctor");
        assert_eq!(result.status, DoctorStatus::Ready);
        assert_eq!(provider.transport_mut().request_count(), 1);
    }

    #[test]
    fn loopback_provider_exposes_devices_and_chat() {
        let format = AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format");
        let chunk = AudioChunk::new(
            StreamId::new("stream").expect("stream"),
            0,
            format,
            vec![1, 2],
            true,
        )
        .expect("chunk");
        let mut provider =
            LoopbackVoiceProvider::new(vec![chunk], "partial", "final", "reply").expect("loopback");
        assert_eq!(
            provider
                .enumerate_devices(&OperationContext::new())
                .expect("devices")
                .devices
                .len(),
            1
        );
        let request = ChatRequest::new(
            RequestId::new("request").expect("request"),
            "conversation",
            "hello",
        )
        .expect("chat");
        let mut sink = VecChatSink::new();
        provider
            .chat(&request, &mut sink, &OperationContext::new())
            .expect("chat");
        assert_eq!(sink.events()[0].text, "reply");
    }

    #[test]
    fn compound_talk_feature_requires_input_and_speech_capabilities() {
        let descriptor =
            ProviderDescriptor::new("external.voice", "External voice", true, false, true)
                .expect("descriptor");
        let error = ExternalSidecarProvider::new(
            descriptor,
            ProviderFeatures {
                talk: true,
                ..ProviderFeatures::audio_and_text()
            },
            ScriptedSidecarTransport::new(std::iter::empty::<
                Result<SidecarResponse, VoiceProviderError>,
            >()),
        )
        .expect_err("talk without transcription must be rejected");
        assert_eq!(
            error,
            VoiceProviderError::invalid_provider("transcribe_capability")
        );
    }

    #[test]
    fn doctor_cannot_escalate_configured_capabilities() {
        let configured = ProviderFeatures {
            chat: false,
            ..ProviderFeatures::audio_and_text()
        };
        let report = DoctorReport::ready(ProviderFeatures::audio_and_text(), 0).expect("report");
        let mut provider = ExternalSidecarProvider::new(
            descriptor(),
            configured,
            ScriptedSidecarTransport::new([Ok(SidecarResponse::Doctor(report))]),
        )
        .expect("provider");
        let error = provider
            .doctor(&OperationContext::new())
            .expect_err("doctor cannot add an undeclared feature");
        assert_eq!(
            error,
            VoiceProviderError::invalid_provider("doctor_capability_mismatch")
        );
        assert_eq!(provider.transport_mut().reset_count(), 1);
    }

    #[test]
    fn output_result_must_match_the_bounded_payload() {
        let format = AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format");
        let device = DeviceId::new("speaker").expect("device");
        let request = AudioOutputRequest::new(
            RequestId::new("output-request").expect("request id"),
            StreamId::new("output-stream").expect("stream id"),
            format,
            OutputSelection::Playback(device.clone()),
            StreamStatus::new(),
        )
        .expect("request")
        .with_device_grant(
            DeviceGrant::new("grant", device, DeviceDirection::Output).expect("grant"),
        );
        let chunk = SynthesizedAudioChunk::new(
            request.request_id.clone(),
            request.stream_id.clone(),
            0,
            format,
            vec![1, 2, 3],
            true,
        )
        .expect("chunk");
        let mut provider = ExternalSidecarProvider::new(
            descriptor(),
            ProviderFeatures::audio_and_text(),
            ScriptedSidecarTransport::new([Ok(SidecarResponse::Playback(OutputResult {
                chunks: 0,
                bytes: 0,
            }))]),
        )
        .expect("provider");
        let mut source = VecSynthesizedAudioSource::new([chunk]);
        let error = provider
            .playback(&request, &mut source, &OperationContext::new())
            .expect_err("provider result must describe consumed audio");
        assert_eq!(
            error,
            VoiceProviderError::provider_failure("output_result_mismatch", false)
        );
        assert_eq!(provider.transport_mut().reset_count(), 1);
    }
}
