use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{VoiceContractError, validate_text};

pub const MAX_CANCELLATION_REASON_BYTES: usize = 4096;
pub const MAX_BUFFER_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    Active,
    Requested,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackpressureState {
    Ready,
    Paused,
    Draining,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StreamStatus {
    pub cancellation: CancellationState,
    pub cancellation_reason: Option<String>,
    pub backpressure: BackpressureState,
    pub buffered_bytes: usize,
    pub capacity_bytes: usize,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamStatus {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationState::Active,
            cancellation_reason: None,
            backpressure: BackpressureState::Ready,
            buffered_bytes: 0,
            capacity_bytes: MAX_BUFFER_CAPACITY_BYTES,
        }
    }

    pub fn with_capacity(capacity_bytes: usize) -> Result<Self, VoiceContractError> {
        let status = Self {
            capacity_bytes,
            ..Self::new()
        };
        status.validate()?;
        Ok(status)
    }

    pub fn request_cancel(&mut self, reason: impl Into<String>) -> Result<(), VoiceContractError> {
        if self.cancellation == CancellationState::Cancelled {
            return Err(VoiceContractError::InvalidStateTransition {
                from: "cancelled",
                to: "requested",
            });
        }
        let reason = reason.into();
        validate_text(
            "cancellation reason",
            &reason,
            MAX_CANCELLATION_REASON_BYTES,
            false,
        )?;
        self.cancellation = CancellationState::Requested;
        self.cancellation_reason = Some(reason);
        Ok(())
    }

    pub fn mark_cancelled(&mut self) -> Result<(), VoiceContractError> {
        match self.cancellation {
            CancellationState::Requested => {
                self.cancellation = CancellationState::Cancelled;
                Ok(())
            }
            CancellationState::Cancelled => Ok(()),
            CancellationState::Active => Err(VoiceContractError::InvalidStateTransition {
                from: "active",
                to: "cancelled",
            }),
        }
    }

    pub fn pause(&mut self) -> Result<(), VoiceContractError> {
        match self.backpressure {
            BackpressureState::Ready => {
                self.backpressure = BackpressureState::Paused;
                Ok(())
            }
            BackpressureState::Paused => Ok(()),
            BackpressureState::Draining | BackpressureState::Closed => {
                Err(VoiceContractError::InvalidStateTransition {
                    from: self.backpressure.as_str(),
                    to: "paused",
                })
            }
        }
    }

    pub fn resume(&mut self) -> Result<(), VoiceContractError> {
        match self.backpressure {
            BackpressureState::Paused => {
                self.backpressure = BackpressureState::Ready;
                Ok(())
            }
            BackpressureState::Ready => Ok(()),
            BackpressureState::Draining | BackpressureState::Closed => {
                Err(VoiceContractError::InvalidStateTransition {
                    from: self.backpressure.as_str(),
                    to: "ready",
                })
            }
        }
    }

    pub fn set_buffered_bytes(&mut self, buffered_bytes: usize) -> Result<(), VoiceContractError> {
        if buffered_bytes > self.capacity_bytes {
            return Err(VoiceContractError::BufferExceedsCapacity {
                buffered: buffered_bytes,
                capacity: self.capacity_bytes,
            });
        }
        self.buffered_bytes = buffered_bytes;
        if buffered_bytes == 0 && self.backpressure == BackpressureState::Draining {
            self.backpressure = BackpressureState::Ready;
        }
        Ok(())
    }

    pub fn start_draining(&mut self) -> Result<(), VoiceContractError> {
        match self.backpressure {
            BackpressureState::Ready | BackpressureState::Paused => {
                self.backpressure = BackpressureState::Draining;
                Ok(())
            }
            BackpressureState::Draining => Ok(()),
            BackpressureState::Closed => Err(VoiceContractError::InvalidStateTransition {
                from: "closed",
                to: "draining",
            }),
        }
    }

    pub fn close(&mut self) {
        self.backpressure = BackpressureState::Closed;
    }

    pub fn validate(&self) -> Result<(), VoiceContractError> {
        if self.capacity_bytes == 0 || self.capacity_bytes > MAX_BUFFER_CAPACITY_BYTES {
            return Err(VoiceContractError::InvalidValue {
                field: "capacity_bytes",
                message: "must be between 1 and 4194304",
            });
        }
        if self.buffered_bytes > self.capacity_bytes {
            return Err(VoiceContractError::BufferExceedsCapacity {
                buffered: self.buffered_bytes,
                capacity: self.capacity_bytes,
            });
        }
        match (&self.cancellation, &self.cancellation_reason) {
            (CancellationState::Active, Some(_)) => Err(VoiceContractError::InvalidValue {
                field: "cancellation_reason",
                message: "active streams cannot have a cancellation reason",
            }),
            (CancellationState::Requested | CancellationState::Cancelled, None) => {
                Err(VoiceContractError::InvalidValue {
                    field: "cancellation_reason",
                    message: "cancelled streams must include a reason",
                })
            }
            (_, Some(reason)) => validate_text(
                "cancellation reason",
                reason,
                MAX_CANCELLATION_REASON_BYTES,
                false,
            ),
            _ => Ok(()),
        }
    }
}

impl BackpressureState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Closed => "closed",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStatus {
    cancellation: CancellationState,
    cancellation_reason: Option<String>,
    backpressure: BackpressureState,
    buffered_bytes: usize,
    capacity_bytes: usize,
}

impl<'de> Deserialize<'de> for StreamStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireStatus::deserialize(deserializer)?;
        let status = Self {
            cancellation: wire.cancellation,
            cancellation_reason: wire.cancellation_reason,
            backpressure: wire.backpressure,
            buffered_bytes: wire.buffered_bytes,
            capacity_bytes: wire.capacity_bytes,
        };
        status.validate().map_err(D::Error::custom)?;
        Ok(status)
    }
}
