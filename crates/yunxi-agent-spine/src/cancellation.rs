use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

const MAX_CANCELLATION_REASON_BYTES: usize = 4096;

#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Debug)]
struct CancellationInner {
    cancelled: AtomicBool,
    reason: OnceLock<String>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                reason: OnceLock::new(),
            }),
        }
    }

    pub fn cancel(&self, reason: impl Into<String>) {
        let reason = bounded_reason(reason.into());
        let _ = self.inner.reason.set(reason);
        self.inner.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub fn reason(&self) -> Option<String> {
        if self.is_cancelled() {
            Some(
                self.inner
                    .reason
                    .get()
                    .cloned()
                    .unwrap_or_else(|| "cancelled".to_string()),
            )
        } else {
            None
        }
    }

    pub fn check(&self) -> Result<(), CancellationError> {
        self.reason()
            .map(CancellationError::new)
            .map_or(Ok(()), Err)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationError {
    reason: String,
}

impl CancellationError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: bounded_reason(reason.into()),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn code(&self) -> &'static str {
        "cancelled"
    }
}

impl fmt::Display for CancellationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.reason)
    }
}

impl Error for CancellationError {}

fn bounded_reason(value: String) -> String {
    if value.trim().is_empty() {
        return "cancelled".to_string();
    }
    truncate(value, MAX_CANCELLATION_REASON_BYTES)
}

fn truncate(value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_tokens_share_cancellation_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        clone.cancel("stop");
        assert_eq!(token.reason().as_deref(), Some("stop"));
        assert_eq!(
            token.check().expect_err("cancelled"),
            CancellationError::new("stop")
        );
    }
}
