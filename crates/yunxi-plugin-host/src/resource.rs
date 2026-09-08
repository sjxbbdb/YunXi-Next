//! Host-owned cooperative limits for one plugin process.
//!
//! These limits are deliberately portable. They bound protocol memory,
//! invocation duration, and concurrency in the trusted host; they are not a
//! claim that a child process has an operating-system CPU or memory sandbox.

use std::fmt;
use std::time::Duration;

pub const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_INVOCATION_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_INVOCATION_DURATION: Duration = Duration::from_secs(10 * 60);
pub const MAX_RESOURCE_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RESOURCE_INVOCATION_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RESOURCE_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RESOURCE_INVOCATION_DURATION: Duration = Duration::from_secs(60 * 60);
/// One process connection is leased to at most one invocation at a time.
pub const MAX_CONCURRENT_INVOCATIONS_PER_PLUGIN: usize = 1;

/// Limits applied by the Host to every invocation on a plugin connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginResourcePolicy {
    max_frame_bytes: usize,
    max_invocation_bytes: usize,
    max_output_bytes: usize,
    max_invocation_duration: Duration,
}

impl PluginResourcePolicy {
    pub fn new(
        max_frame_bytes: usize,
        max_invocation_bytes: usize,
        max_invocation_duration: Duration,
    ) -> Result<Self, ResourcePolicyError> {
        if max_frame_bytes == 0 || max_frame_bytes > MAX_RESOURCE_FRAME_BYTES {
            return Err(ResourcePolicyError::OutOfRange {
                field: "max_frame_bytes",
                maximum: MAX_RESOURCE_FRAME_BYTES,
            });
        }
        if max_invocation_bytes == 0 || max_invocation_bytes > MAX_RESOURCE_INVOCATION_BYTES {
            return Err(ResourcePolicyError::OutOfRange {
                field: "max_invocation_bytes",
                maximum: MAX_RESOURCE_INVOCATION_BYTES,
            });
        }
        if max_invocation_duration.is_zero()
            || max_invocation_duration > MAX_RESOURCE_INVOCATION_DURATION
        {
            return Err(ResourcePolicyError::DurationOutOfRange {
                field: "max_invocation_duration",
            });
        }
        Ok(Self {
            max_frame_bytes,
            max_invocation_bytes,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_invocation_duration,
        })
    }

    pub const fn max_frame_bytes(self) -> usize {
        self.max_frame_bytes
    }

    pub const fn max_invocation_bytes(self) -> usize {
        self.max_invocation_bytes
    }

    pub const fn max_output_bytes(self) -> usize {
        self.max_output_bytes
    }

    pub fn with_max_output_bytes(
        mut self,
        max_output_bytes: usize,
    ) -> Result<Self, ResourcePolicyError> {
        if max_output_bytes == 0 || max_output_bytes > MAX_RESOURCE_OUTPUT_BYTES {
            return Err(ResourcePolicyError::OutOfRange {
                field: "max_output_bytes",
                maximum: MAX_RESOURCE_OUTPUT_BYTES,
            });
        }
        self.max_output_bytes = max_output_bytes;
        Ok(self)
    }

    pub const fn max_invocation_duration(self) -> Duration {
        self.max_invocation_duration
    }
}

impl Default for PluginResourcePolicy {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_invocation_bytes: DEFAULT_MAX_INVOCATION_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_invocation_duration: DEFAULT_MAX_INVOCATION_DURATION,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourcePolicyError {
    OutOfRange { field: &'static str, maximum: usize },
    DurationOutOfRange { field: &'static str },
}

impl fmt::Display for ResourcePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { field, maximum } => {
                write!(formatter, "{field} must be in 1..={maximum}")
            }
            Self::DurationOutOfRange { field } => {
                write!(
                    formatter,
                    "{field} must be positive and within the host maximum"
                )
            }
        }
    }
}

impl std::error::Error for ResourcePolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_bounded() {
        let policy = PluginResourcePolicy::default();
        assert!(policy.max_frame_bytes() <= MAX_RESOURCE_FRAME_BYTES);
        assert!(policy.max_invocation_bytes() <= MAX_RESOURCE_INVOCATION_BYTES);
        assert!(policy.max_output_bytes() <= MAX_RESOURCE_OUTPUT_BYTES);
        assert!(policy.max_invocation_duration() <= MAX_RESOURCE_INVOCATION_DURATION);
    }

    #[test]
    fn invalid_policy_is_rejected() {
        assert!(PluginResourcePolicy::new(0, 1, Duration::from_secs(1)).is_err());
        assert!(PluginResourcePolicy::new(1, 0, Duration::from_secs(1)).is_err());
        assert!(PluginResourcePolicy::new(1, 1, Duration::ZERO).is_err());
        assert!(
            PluginResourcePolicy::new(MAX_RESOURCE_FRAME_BYTES + 1, 1, Duration::from_secs(1),)
                .is_err()
        );
        assert!(policy_output_bytes(0).is_err());
        assert!(policy_output_bytes(MAX_RESOURCE_OUTPUT_BYTES + 1).is_err());
    }

    fn policy_output_bytes(value: usize) -> Result<PluginResourcePolicy, ResourcePolicyError> {
        PluginResourcePolicy::default().with_max_output_bytes(value)
    }
}
