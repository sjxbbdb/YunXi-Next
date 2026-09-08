//! Cancellation and deadline propagation for channel adapters.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestControlError {
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for RequestControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("channel operation was cancelled"),
            Self::TimedOut => formatter.write_str("channel operation timed out"),
        }
    }
}

impl std::error::Error for RequestControlError {}

/// A cloneable cancellation signal that can be shared with a transport worker.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Per-request cancellation and monotonic timeout context.
#[derive(Clone, Debug)]
pub struct RequestContext {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl Default for RequestContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestContext {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: None,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        let now = Instant::now();
        Self::with_deadline(now.checked_add(timeout).unwrap_or(now))
    }

    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: Some(deadline),
        }
    }

    /// Creates a request context backed by a caller-owned cancellation token.
    /// This is used by background channel workers so a Host can request stop
    /// without borrowing or blocking the worker thread.
    pub fn with_cancellation(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            deadline: None,
        }
    }

    pub fn with_cancellation_and_timeout(
        cancellation: CancellationToken,
        timeout: Duration,
    ) -> Self {
        let now = Instant::now();
        Self {
            cancellation,
            deadline: Some(now.checked_add(timeout).unwrap_or(now)),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub fn check(&self) -> Result<(), RequestControlError> {
        if self.is_cancelled() {
            return Err(RequestControlError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(RequestControlError::TimedOut);
        }
        Ok(())
    }
}
