use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{WeixinContractError, validate_text};

pub const MAX_RETRY_ATTEMPTS: u16 = 8;
pub const MAX_RETRY_ERROR_BYTES: usize = 4096;
pub const MAX_CANCELLATION_REASON_BYTES: usize = 4096;
pub const MAX_BUFFER_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    InFlight,
    Accepted,
    Delivered,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckState {
    Pending,
    Acknowledged,
    Rejected,
}

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
pub struct RetryState {
    pub attempt: u16,
    pub max_attempts: u16,
    pub last_error: Option<String>,
}

impl RetryState {
    pub fn new(max_attempts: u16) -> Result<Self, WeixinContractError> {
        let retry = Self {
            attempt: 0,
            max_attempts,
            last_error: None,
        };
        retry.validate()?;
        Ok(retry)
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        if self.max_attempts == 0 || self.max_attempts > MAX_RETRY_ATTEMPTS {
            return Err(WeixinContractError::InvalidValue {
                field: "max_attempts",
                message: "must be between 1 and 8",
            });
        }
        if self.attempt > self.max_attempts {
            return Err(WeixinContractError::InvalidValue {
                field: "attempt",
                message: "must not exceed max_attempts",
            });
        }
        if let Some(error) = &self.last_error {
            validate_text("retry error", error, MAX_RETRY_ERROR_BYTES, false)?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RetryState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRetryState {
            attempt: u16,
            max_attempts: u16,
            last_error: Option<String>,
        }

        let wire = WireRetryState::deserialize(deserializer)?;
        let retry = Self {
            attempt: wire.attempt,
            max_attempts: wire.max_attempts,
            last_error: wire.last_error,
        };
        retry.validate().map_err(D::Error::custom)?;
        Ok(retry)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeliveryStatus {
    pub state: DeliveryState,
    pub acknowledgement: AckState,
    pub acknowledgement_reason: Option<String>,
    pub retry: RetryState,
    pub cancellation: CancellationState,
    pub cancellation_reason: Option<String>,
    pub backpressure: BackpressureState,
    pub buffered_bytes: usize,
    pub capacity_bytes: usize,
}

impl DeliveryStatus {
    pub fn new_inbound() -> Self {
        Self::base(DeliveryState::Accepted)
    }

    pub fn new_outbound() -> Self {
        Self::base(DeliveryState::Queued)
    }

    pub fn new_inbound_with_capacity(capacity_bytes: usize) -> Result<Self, WeixinContractError> {
        Self::with_capacity(DeliveryState::Accepted, capacity_bytes)
    }

    pub fn new_outbound_with_capacity(capacity_bytes: usize) -> Result<Self, WeixinContractError> {
        Self::with_capacity(DeliveryState::Queued, capacity_bytes)
    }

    fn base(state: DeliveryState) -> Self {
        Self {
            state,
            acknowledgement: AckState::Pending,
            acknowledgement_reason: None,
            retry: RetryState {
                attempt: 0,
                max_attempts: MAX_RETRY_ATTEMPTS,
                last_error: None,
            },
            cancellation: CancellationState::Active,
            cancellation_reason: None,
            backpressure: BackpressureState::Ready,
            buffered_bytes: 0,
            capacity_bytes: MAX_BUFFER_CAPACITY_BYTES,
        }
    }

    fn with_capacity(
        state: DeliveryState,
        capacity_bytes: usize,
    ) -> Result<Self, WeixinContractError> {
        let status = Self {
            capacity_bytes,
            ..Self::base(state)
        };
        status.validate()?;
        Ok(status)
    }

    pub fn acknowledge(&mut self) -> Result<(), WeixinContractError> {
        if self.acknowledgement != AckState::Pending {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.acknowledgement.as_str(),
                to: "acknowledged",
            });
        }
        self.acknowledgement = AckState::Acknowledged;
        self.acknowledgement_reason = None;
        Ok(())
    }

    pub fn reject(&mut self, reason: impl Into<String>) -> Result<(), WeixinContractError> {
        if self.acknowledgement != AckState::Pending {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.acknowledgement.as_str(),
                to: "rejected",
            });
        }
        let reason = reason.into();
        validate_text(
            "acknowledgement reason",
            &reason,
            MAX_RETRY_ERROR_BYTES,
            false,
        )?;
        self.acknowledgement = AckState::Rejected;
        self.acknowledgement_reason = Some(reason.clone());
        self.state = DeliveryState::Failed;
        self.retry.last_error = Some(reason);
        Ok(())
    }

    pub fn mark_in_flight(&mut self) -> Result<(), WeixinContractError> {
        if self.state != DeliveryState::Queued {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.state.as_str(),
                to: "in_flight",
            });
        }
        if self.retry.attempt >= self.retry.max_attempts {
            return Err(WeixinContractError::RetryExhausted);
        }
        self.retry.attempt += 1;
        self.state = DeliveryState::InFlight;
        Ok(())
    }

    pub fn mark_accepted(&mut self) -> Result<(), WeixinContractError> {
        if self.state != DeliveryState::InFlight {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.state.as_str(),
                to: "accepted",
            });
        }
        self.state = DeliveryState::Accepted;
        Ok(())
    }

    pub fn mark_delivered(&mut self) -> Result<(), WeixinContractError> {
        match self.state {
            DeliveryState::Accepted | DeliveryState::InFlight => {
                self.state = DeliveryState::Delivered;
                Ok(())
            }
            _ => Err(WeixinContractError::InvalidStateTransition {
                from: self.state.as_str(),
                to: "delivered",
            }),
        }
    }

    pub fn mark_failed(&mut self, reason: impl Into<String>) -> Result<(), WeixinContractError> {
        if matches!(
            self.state,
            DeliveryState::Delivered | DeliveryState::Cancelled
        ) {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.state.as_str(),
                to: "failed",
            });
        }
        let reason = reason.into();
        validate_text("retry error", &reason, MAX_RETRY_ERROR_BYTES, false)?;
        self.state = DeliveryState::Failed;
        self.retry.last_error = Some(reason);
        Ok(())
    }

    pub fn schedule_retry(&mut self) -> Result<(), WeixinContractError> {
        if self.state != DeliveryState::Failed {
            return Err(WeixinContractError::InvalidStateTransition {
                from: self.state.as_str(),
                to: "queued",
            });
        }
        if self.retry.attempt >= self.retry.max_attempts {
            return Err(WeixinContractError::RetryExhausted);
        }
        self.state = DeliveryState::Queued;
        Ok(())
    }

    pub fn request_cancel(&mut self, reason: impl Into<String>) -> Result<(), WeixinContractError> {
        if self.cancellation == CancellationState::Cancelled {
            return Err(WeixinContractError::InvalidStateTransition {
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

    pub fn mark_cancelled(&mut self) -> Result<(), WeixinContractError> {
        if self.cancellation == CancellationState::Cancelled {
            return Ok(());
        }
        if self.cancellation != CancellationState::Requested {
            return Err(WeixinContractError::InvalidStateTransition {
                from: "active",
                to: "cancelled",
            });
        }
        self.cancellation = CancellationState::Cancelled;
        self.state = DeliveryState::Cancelled;
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), WeixinContractError> {
        match self.backpressure {
            BackpressureState::Ready => {
                self.backpressure = BackpressureState::Paused;
                Ok(())
            }
            BackpressureState::Paused => Ok(()),
            BackpressureState::Draining | BackpressureState::Closed => {
                Err(WeixinContractError::InvalidStateTransition {
                    from: self.backpressure.as_str(),
                    to: "paused",
                })
            }
        }
    }

    pub fn resume(&mut self) -> Result<(), WeixinContractError> {
        match self.backpressure {
            BackpressureState::Paused => {
                self.backpressure = BackpressureState::Ready;
                Ok(())
            }
            BackpressureState::Ready => Ok(()),
            BackpressureState::Draining | BackpressureState::Closed => {
                Err(WeixinContractError::InvalidStateTransition {
                    from: self.backpressure.as_str(),
                    to: "ready",
                })
            }
        }
    }

    pub fn start_draining(&mut self) -> Result<(), WeixinContractError> {
        match self.backpressure {
            BackpressureState::Ready | BackpressureState::Paused => {
                self.backpressure = BackpressureState::Draining;
                Ok(())
            }
            BackpressureState::Draining => Ok(()),
            BackpressureState::Closed => Err(WeixinContractError::InvalidStateTransition {
                from: "closed",
                to: "draining",
            }),
        }
    }

    pub fn set_buffered_bytes(&mut self, buffered_bytes: usize) -> Result<(), WeixinContractError> {
        if buffered_bytes > self.capacity_bytes {
            return Err(WeixinContractError::BufferExceedsCapacity {
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

    pub fn close(&mut self) {
        self.backpressure = BackpressureState::Closed;
    }

    pub fn validate(&self) -> Result<(), WeixinContractError> {
        self.retry.validate()?;
        if self.capacity_bytes == 0 || self.capacity_bytes > MAX_BUFFER_CAPACITY_BYTES {
            return Err(WeixinContractError::InvalidValue {
                field: "capacity_bytes",
                message: "must be between 1 and 4194304",
            });
        }
        if self.buffered_bytes > self.capacity_bytes {
            return Err(WeixinContractError::BufferExceedsCapacity {
                buffered: self.buffered_bytes,
                capacity: self.capacity_bytes,
            });
        }
        match (&self.acknowledgement, &self.acknowledgement_reason) {
            (AckState::Rejected, None) => {
                return Err(WeixinContractError::InvalidValue {
                    field: "acknowledgement_reason",
                    message: "rejected delivery must include a reason",
                });
            }
            (AckState::Pending | AckState::Acknowledged, Some(_)) => {
                return Err(WeixinContractError::InvalidValue {
                    field: "acknowledgement_reason",
                    message: "only rejected delivery may include a reason",
                });
            }
            (_, Some(reason)) => validate_text(
                "acknowledgement reason",
                reason,
                MAX_RETRY_ERROR_BYTES,
                false,
            )?,
            _ => {}
        }
        match (&self.cancellation, &self.cancellation_reason) {
            (CancellationState::Active, Some(_)) => {
                return Err(WeixinContractError::InvalidValue {
                    field: "cancellation_reason",
                    message: "active delivery cannot have a cancellation reason",
                });
            }
            (CancellationState::Requested | CancellationState::Cancelled, None) => {
                return Err(WeixinContractError::InvalidValue {
                    field: "cancellation_reason",
                    message: "requested or cancelled delivery must include a reason",
                });
            }
            (_, Some(reason)) => validate_text(
                "cancellation reason",
                reason,
                MAX_CANCELLATION_REASON_BYTES,
                false,
            )?,
            _ => {}
        }
        if self.state == DeliveryState::Cancelled
            && self.cancellation != CancellationState::Cancelled
        {
            return Err(WeixinContractError::InvalidValue {
                field: "state",
                message: "cancelled delivery must have cancelled cancellation state",
            });
        }
        if self.cancellation == CancellationState::Cancelled
            && self.state != DeliveryState::Cancelled
        {
            return Err(WeixinContractError::InvalidValue {
                field: "cancellation",
                message: "cancelled cancellation must have cancelled delivery state",
            });
        }
        if self.acknowledgement == AckState::Rejected && self.state != DeliveryState::Failed {
            return Err(WeixinContractError::InvalidValue {
                field: "acknowledgement",
                message: "rejected delivery must be failed",
            });
        }
        Ok(())
    }
}

impl Default for DeliveryStatus {
    fn default() -> Self {
        Self::new_outbound()
    }
}

impl DeliveryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InFlight => "in_flight",
            Self::Accepted => "accepted",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl AckState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acknowledged => "acknowledged",
            Self::Rejected => "rejected",
        }
    }
}

impl BackpressureState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Closed => "closed",
        }
    }
}

impl<'de> Deserialize<'de> for DeliveryStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDeliveryStatus {
            state: DeliveryState,
            acknowledgement: AckState,
            acknowledgement_reason: Option<String>,
            retry: RetryState,
            cancellation: CancellationState,
            cancellation_reason: Option<String>,
            backpressure: BackpressureState,
            buffered_bytes: usize,
            capacity_bytes: usize,
        }

        let wire = WireDeliveryStatus::deserialize(deserializer)?;
        let status = Self {
            state: wire.state,
            acknowledgement: wire.acknowledgement,
            acknowledgement_reason: wire.acknowledgement_reason,
            retry: wire.retry,
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
