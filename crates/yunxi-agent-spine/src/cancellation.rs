use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const MAX_CANCELLATION_REASON_BYTES: usize = 4096;
const MAX_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Debug)]
struct CancellationInner {
    cancelled: AtomicBool,
    info: OnceLock<CancellationInfo>,
    deadline: Option<Instant>,
    parents: Vec<Arc<CancellationInner>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationKind {
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug)]
struct CancellationInfo {
    kind: CancellationKind,
    reason: String,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                info: OnceLock::new(),
                deadline: None,
                parents: Vec::new(),
            }),
        }
    }

    /// Creates a token that automatically becomes timed out after `timeout`.
    ///
    /// The token is lazy: no timer thread is created.  The deadline is checked
    /// whenever a caller asks for its state or calls [`Self::check`].
    pub fn with_timeout(timeout: Duration) -> Self {
        let deadline = Instant::now()
            .checked_add(timeout.min(MAX_CANCELLATION_TIMEOUT))
            .unwrap_or_else(Instant::now);
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                info: OnceLock::new(),
                deadline: Some(deadline),
                parents: Vec::new(),
            }),
        }
    }

    /// Creates a token linked to a parent and bounded by a local timeout.
    ///
    /// A child observes parent cancellation at its next check and preserves
    /// the parent's cancellation kind.  This lets a turn combine a caller's
    /// cancellation with its own deadline without changing the old token API.
    pub fn child_with_timeout(parent: &Self, timeout: Duration) -> Self {
        let deadline = Instant::now()
            .checked_add(timeout.min(MAX_CANCELLATION_TIMEOUT))
            .unwrap_or_else(Instant::now);
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                info: OnceLock::new(),
                deadline: Some(deadline),
                parents: vec![Arc::clone(&parent.inner)],
            }),
        }
    }

    pub(crate) fn linked(first: &Self, second: &Self) -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                info: OnceLock::new(),
                deadline: None,
                parents: vec![Arc::clone(&first.inner), Arc::clone(&second.inner)],
            }),
        }
    }

    pub fn cancel(&self, reason: impl Into<String>) {
        self.trigger(CancellationKind::Cancelled, reason.into());
    }

    /// Cancels this token as a timeout.  This is useful for hosts that have a
    /// deadline source outside the lazy [`Self::with_timeout`] constructor.
    pub fn cancel_timeout(&self, reason: impl Into<String>) {
        self.trigger(CancellationKind::TimedOut, reason.into());
    }

    pub fn is_cancelled(&self) -> bool {
        self.expire_or_follow_parent();
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub fn reason(&self) -> Option<String> {
        self.expire_or_follow_parent();
        if self.is_cancelled() {
            Some(
                self.inner
                    .info
                    .get()
                    .map(|info| info.reason.clone())
                    .unwrap_or_else(|| "cancelled".to_string()),
            )
        } else {
            None
        }
    }

    pub fn kind(&self) -> Option<CancellationKind> {
        self.expire_or_follow_parent();
        self.inner.info.get().map(|info| info.kind)
    }

    pub fn check(&self) -> Result<(), CancellationError> {
        self.expire_or_follow_parent();
        if self.inner.cancelled.load(Ordering::Acquire) {
            let info = self.inner.info.get().cloned().unwrap_or(CancellationInfo {
                kind: CancellationKind::Cancelled,
                reason: "cancelled".to_string(),
            });
            Err(CancellationError::from_info(info.kind, info.reason))
        } else {
            Ok(())
        }
    }

    fn expire_or_follow_parent(&self) {
        for parent in &self.inner.parents {
            if let Some(info) = parent_info(parent) {
                self.trigger(info.kind, info.reason);
                return;
            }
        }
        if self
            .inner
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.trigger(CancellationKind::TimedOut, "turn timed out".to_string());
        }
    }

    fn trigger(&self, kind: CancellationKind, reason: String) {
        let info = CancellationInfo {
            kind,
            reason: bounded_reason(reason),
        };
        let _ = self.inner.info.set(info);
        self.inner.cancelled.store(true, Ordering::Release);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationError {
    kind: CancellationKind,
    reason: String,
}

impl CancellationError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self::from_info(CancellationKind::Cancelled, reason.into())
    }

    pub fn timeout(reason: impl Into<String>) -> Self {
        Self::from_info(CancellationKind::TimedOut, reason.into())
    }

    fn from_info(kind: CancellationKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: bounded_reason(reason.into()),
        }
    }

    pub const fn kind(&self) -> CancellationKind {
        self.kind
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            CancellationKind::Cancelled => "cancelled",
            CancellationKind::TimedOut => "timeout",
        }
    }

    pub const fn is_timeout(&self) -> bool {
        matches!(self.kind, CancellationKind::TimedOut)
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

fn parent_info(parent: &CancellationInner) -> Option<CancellationInfo> {
    if parent.cancelled.load(Ordering::Acquire) {
        return Some(parent.info.get().cloned().unwrap_or(CancellationInfo {
            kind: CancellationKind::Cancelled,
            reason: "cancelled".to_string(),
        }));
    }
    if parent
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Some(CancellationInfo {
            kind: CancellationKind::TimedOut,
            reason: "turn timed out".to_string(),
        });
    }
    parent.parents.iter().find_map(|parent| parent_info(parent))
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

    #[test]
    fn deadline_is_lazy_and_reports_timeout_without_a_timer_thread() {
        let token = CancellationToken::with_timeout(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        let error = token.check().expect_err("deadline");
        assert!(error.is_timeout());
        assert_eq!(error.code(), "timeout");
    }

    #[test]
    fn child_follows_parent_cancellation_and_keeps_kind() {
        let parent = CancellationToken::new();
        let child = CancellationToken::child_with_timeout(&parent, Duration::from_secs(1));
        parent.cancel("user stopped");
        let error = child.check().expect_err("child cancellation");
        assert_eq!(error.code(), "cancelled");
        assert_eq!(error.reason(), "user stopped");
    }

    #[test]
    fn linked_token_follows_either_parent() {
        let original = CancellationToken::new();
        let approval_call = CancellationToken::new();
        let linked = CancellationToken::linked(&original, &approval_call);

        approval_call.cancel("approval call stopped");
        let error = linked.check().expect_err("linked cancellation");
        assert_eq!(error.reason(), "approval call stopped");
    }
}
