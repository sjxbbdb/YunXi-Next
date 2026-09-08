//! Bounded lifecycle events emitted by the Cordis runtime.
//!
//! The runtime is synchronous, so a small in-memory journal is enough to give
//! CLI and Web callers a cursor-based view of plugin lifecycle changes.  The
//! journal deliberately contains metadata only: service values, callback
//! addresses, credentials, and arbitrary payloads never cross this boundary.

use std::collections::VecDeque;
use std::fmt;

use crate::snapshot::PluginRuntimeState;

/// Maximum number of lifecycle events retained for cursor-based replay.
pub const MAX_RUNTIME_EVENTS: usize = 512;
/// Maximum size of a diagnostic message retained in one event.
pub const MAX_RUNTIME_EVENT_MESSAGE_BYTES: usize = 1024;
/// Maximum number of events returned by one replay request.
pub const MAX_RUNTIME_EVENT_PAGE: usize = 128;

/// The kind of lifecycle transition represented by a [`RuntimeEvent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEventKind {
    StartupStarted,
    PluginMounting,
    PluginMounted,
    PluginSkipped,
    PluginFailed,
    PluginUnmounting,
    PluginDisabled,
    ShutdownStarted,
    ShutdownCompleted,
}

impl fmt::Display for RuntimeEventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StartupStarted => "startup_started",
            Self::PluginMounting => "plugin_mounting",
            Self::PluginMounted => "plugin_mounted",
            Self::PluginSkipped => "plugin_skipped",
            Self::PluginFailed => "plugin_failed",
            Self::PluginUnmounting => "plugin_unmounting",
            Self::PluginDisabled => "plugin_disabled",
            Self::ShutdownStarted => "shutdown_started",
            Self::ShutdownCompleted => "shutdown_completed",
        })
    }
}

/// Metadata describing one runtime lifecycle transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEvent {
    sequence: u64,
    kind: RuntimeEventKind,
    plugin_id: Option<String>,
    state: Option<PluginRuntimeState>,
    message: Option<String>,
}

impl RuntimeEvent {
    pub(crate) fn new(
        sequence: u64,
        kind: RuntimeEventKind,
        plugin_id: Option<String>,
        state: Option<PluginRuntimeState>,
        message: Option<String>,
    ) -> Self {
        Self {
            sequence,
            kind,
            plugin_id,
            state,
            message,
        }
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn kind(&self) -> RuntimeEventKind {
        self.kind
    }

    pub fn plugin_id(&self) -> Option<&str> {
        self.plugin_id.as_deref()
    }

    pub const fn state(&self) -> Option<PluginRuntimeState> {
        self.state
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// A bounded page of lifecycle events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventPage {
    events: Vec<RuntimeEvent>,
    next_sequence: u64,
    oldest_sequence: Option<u64>,
    latest_sequence: u64,
    gap: bool,
    has_more: bool,
}

impl RuntimeEventPage {
    fn empty(latest_sequence: u64) -> Self {
        Self {
            events: Vec::new(),
            next_sequence: latest_sequence,
            oldest_sequence: None,
            latest_sequence,
            gap: false,
            has_more: false,
        }
    }

    pub fn events(&self) -> &[RuntimeEvent] {
        &self.events
    }

    /// The cursor to send as `after` on the next request.
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub const fn oldest_sequence(&self) -> Option<u64> {
        self.oldest_sequence
    }

    pub const fn latest_sequence(&self) -> u64 {
        self.latest_sequence
    }

    /// True when the requested cursor predates the retained event window.
    pub const fn gap(&self) -> bool {
        self.gap
    }

    pub const fn has_more(&self) -> bool {
        self.has_more
    }
}

/// Internal bounded event journal owned by one [`crate::CordisRuntime`].
#[derive(Clone, Debug)]
pub(crate) struct RuntimeEventJournal {
    events: VecDeque<RuntimeEvent>,
    next_sequence: u64,
}

impl RuntimeEventJournal {
    pub(crate) fn new() -> Self {
        Self {
            events: VecDeque::new(),
            next_sequence: 1,
        }
    }

    pub(crate) fn record(
        &mut self,
        kind: RuntimeEventKind,
        plugin_id: Option<&str>,
        state: Option<PluginRuntimeState>,
        message: Option<&str>,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push_back(RuntimeEvent::new(
            sequence,
            kind,
            plugin_id.map(ToOwned::to_owned),
            state,
            message.map(bound_message),
        ));
        while self.events.len() > MAX_RUNTIME_EVENTS {
            self.events.pop_front();
        }
    }

    pub(crate) fn page(&self, after_sequence: u64, requested_limit: usize) -> RuntimeEventPage {
        let latest_sequence = self.next_sequence.saturating_sub(1);
        let Some(oldest_sequence) = self.events.front().map(RuntimeEvent::sequence) else {
            return RuntimeEventPage::empty(latest_sequence);
        };
        let limit = requested_limit.clamp(1, MAX_RUNTIME_EVENT_PAGE);
        let gap = after_sequence != 0
            && after_sequence
                .checked_add(1)
                .is_none_or(|expected| expected < oldest_sequence);
        let mut events = self
            .events
            .iter()
            .filter(|event| event.sequence() > after_sequence)
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = events.len() > limit;
        if has_more {
            events.truncate(limit);
        }
        let next_sequence = events
            .last()
            .map(RuntimeEvent::sequence)
            .unwrap_or(after_sequence.min(latest_sequence));
        RuntimeEventPage {
            events,
            next_sequence,
            oldest_sequence: Some(oldest_sequence),
            latest_sequence,
            gap,
            has_more,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}

impl Default for RuntimeEventJournal {
    fn default() -> Self {
        Self::new()
    }
}

fn bound_message(message: &str) -> String {
    if message.len() <= MAX_RUNTIME_EVENT_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_RUNTIME_EVENT_MESSAGE_BYTES.saturating_sub(3);
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &message[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_keeps_a_bounded_window_and_reports_gaps() {
        let mut journal = RuntimeEventJournal::new();
        for index in 0..(MAX_RUNTIME_EVENTS + 3) {
            journal.record(
                RuntimeEventKind::PluginMounted,
                Some("test.plugin"),
                Some(PluginRuntimeState::Mounted),
                Some(&index.to_string()),
            );
        }
        assert_eq!(journal.len(), MAX_RUNTIME_EVENTS);
        let page = journal.page(1, MAX_RUNTIME_EVENT_PAGE);
        assert!(page.gap());
        assert_eq!(page.events().len(), MAX_RUNTIME_EVENT_PAGE);
        assert!(page.has_more());
    }

    #[test]
    fn journal_truncates_messages_on_character_boundaries() {
        let mut journal = RuntimeEventJournal::new();
        let message = "界".repeat(MAX_RUNTIME_EVENT_MESSAGE_BYTES);
        journal.record(RuntimeEventKind::PluginFailed, None, None, Some(&message));
        let page = journal.page(0, 1);
        let stored = page
            .events()
            .first()
            .and_then(RuntimeEvent::message)
            .expect("message");
        assert!(stored.ends_with("..."));
        assert!(stored.len() <= MAX_RUNTIME_EVENT_MESSAGE_BYTES);
    }

    #[test]
    fn empty_page_uses_the_latest_cursor() {
        let journal = RuntimeEventJournal::new();
        let page = journal.page(99, 0);
        assert!(page.events().is_empty());
        assert_eq!(page.next_sequence(), 0);
        assert_eq!(page.latest_sequence(), 0);
    }
}
