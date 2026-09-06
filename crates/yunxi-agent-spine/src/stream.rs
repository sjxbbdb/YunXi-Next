//! Bounded Agent event delivery and the bridge used by model/tool adapters.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use yunxi_protocol::{
    AgentStreamEvent, MAX_STREAM_EVENTS_PER_TURN, StreamError, StreamEvent, StreamProtocolError,
    StreamTurnState, ToolCall, ToolResult,
};

use crate::cancellation::CancellationToken;

pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 64;
pub const MAX_EVENT_CHANNEL_CAPACITY: usize = 1024;
pub const DEFAULT_EVENT_BLOCK_TIMEOUT: Duration = Duration::from_millis(250);
pub const MAX_EVENT_BLOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How a bounded event channel reacts when its queue is full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackpressureStrategy {
    /// Wait for a consumer, but never longer than the configured timeout.
    Block,
    /// Drop only text deltas and tool progress.  Terminal and state events
    /// are retained, evicting an older droppable event when necessary.
    DropNonTerminal,
    /// Return immediately when the queue is full.
    Fail,
}

/// Alias used by host integrations that call this a policy.
pub type BackpressurePolicy = BackpressureStrategy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventChannelConfig {
    capacity: usize,
    strategy: BackpressureStrategy,
    block_timeout: Duration,
}

impl EventChannelConfig {
    pub fn new(capacity: usize, strategy: BackpressureStrategy) -> Result<Self, EventChannelError> {
        let config = Self {
            capacity,
            strategy,
            block_timeout: DEFAULT_EVENT_BLOCK_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_block_timeout(mut self, timeout: Duration) -> Result<Self, EventChannelError> {
        self.block_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub const fn strategy(&self) -> BackpressureStrategy {
        self.strategy
    }

    pub const fn block_timeout(&self) -> Duration {
        self.block_timeout
    }

    pub fn validate(&self) -> Result<(), EventChannelError> {
        if self.capacity == 0 || self.capacity > MAX_EVENT_CHANNEL_CAPACITY {
            return Err(EventChannelError::InvalidCapacity {
                capacity: self.capacity,
                maximum: MAX_EVENT_CHANNEL_CAPACITY,
            });
        }
        if self.block_timeout.is_zero() || self.block_timeout > MAX_EVENT_BLOCK_TIMEOUT {
            return Err(EventChannelError::InvalidBlockTimeout {
                timeout_millis: self.block_timeout.as_millis() as u64,
                maximum_millis: MAX_EVENT_BLOCK_TIMEOUT.as_millis() as u64,
            });
        }
        Ok(())
    }
}

impl Default for EventChannelConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
            strategy: BackpressureStrategy::Block,
            block_timeout: DEFAULT_EVENT_BLOCK_TIMEOUT,
        }
    }
}

/// Creates a bounded sender/receiver pair backed only by standard-library
/// synchronization primitives.
pub fn bounded_event_channel(
    capacity: usize,
    strategy: BackpressureStrategy,
) -> Result<(EventSender, EventReceiver), EventChannelError> {
    EventChannel::with_config(EventChannelConfig::new(capacity, strategy)?)
}

pub struct EventChannel;

impl EventChannel {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        capacity: usize,
        strategy: BackpressureStrategy,
    ) -> Result<(EventSender, EventReceiver), EventChannelError> {
        bounded_event_channel(capacity, strategy)
    }

    pub fn with_config(
        config: EventChannelConfig,
    ) -> Result<(EventSender, EventReceiver), EventChannelError> {
        config.validate()?;
        let inner = Arc::new(ChannelInner {
            state: Mutex::new(ChannelState {
                queue: VecDeque::with_capacity(config.capacity),
                capacity: config.capacity,
                strategy: config.strategy,
                block_timeout: config.block_timeout,
                closed: false,
                sender_count: 1,
                attempted_events: 0,
                dropped_events: 0,
            }),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
        });
        Ok((
            EventSender {
                inner: Arc::clone(&inner),
            },
            EventReceiver { inner },
        ))
    }
}

pub trait EventSink {
    fn emit(&mut self, event: AgentStreamEvent) -> Result<(), EventSinkError>;

    /// Default cancellation propagation for custom sinks.  Channel sinks
    /// override this to interrupt a blocked send promptly.
    fn emit_with_cancellation(
        &mut self,
        event: AgentStreamEvent,
        cancellation: &CancellationToken,
    ) -> Result<(), EventSinkError> {
        cancellation
            .check()
            .map_err(|_| EventSinkError::Cancelled)?;
        self.emit(event)
    }
}

impl<F> EventSink for F
where
    F: FnMut(AgentStreamEvent) -> Result<(), EventSinkError>,
{
    fn emit(&mut self, event: AgentStreamEvent) -> Result<(), EventSinkError> {
        self(event)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventSinkError {
    Closed,
    Cancelled,
    BackpressureTimeout,
    Full,
    EventLimitReached { maximum: usize },
    InvalidEvent(String),
}

impl fmt::Display for EventSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("event sink is closed"),
            Self::Cancelled => formatter.write_str("event sink send was cancelled"),
            Self::BackpressureTimeout => formatter.write_str("event sink backpressure timeout"),
            Self::Full => formatter.write_str("event sink queue is full"),
            Self::EventLimitReached { maximum } => {
                write!(formatter, "event limit of {maximum} has been reached")
            }
            Self::InvalidEvent(message) => write!(formatter, "invalid stream event: {message}"),
        }
    }
}

impl std::error::Error for EventSinkError {}

impl From<StreamProtocolError> for EventSinkError {
    fn from(error: StreamProtocolError) -> Self {
        Self::InvalidEvent(error.to_string())
    }
}

pub struct EventSender {
    inner: Arc<ChannelInner>,
}

impl Clone for EventSender {
    fn clone(&self) -> Self {
        if let Ok(mut state) = self.inner.state.lock() {
            state.sender_count = state.sender_count.saturating_add(1);
        }
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl EventSender {
    pub fn dropped_events(&self) -> u64 {
        self.inner
            .state
            .lock()
            .map(|state| state.dropped_events)
            .unwrap_or(0)
    }

    pub fn close(&self) {
        close_channel(&self.inner);
    }

    fn send(
        &self,
        event: AgentStreamEvent,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EventSinkError> {
        event.validate()?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| EventSinkError::Closed)?;
        let deadline = Instant::now() + state.block_timeout;
        loop {
            if state.closed {
                return Err(EventSinkError::Closed);
            }
            if state.queue.len() < state.capacity {
                if state.attempted_events >= MAX_STREAM_EVENTS_PER_TURN {
                    return Err(EventSinkError::EventLimitReached {
                        maximum: MAX_STREAM_EVENTS_PER_TURN,
                    });
                }
                state.attempted_events += 1;
                state.queue.push_back(event);
                self.inner.not_empty.notify_one();
                return Ok(());
            }

            if state.attempted_events >= MAX_STREAM_EVENTS_PER_TURN {
                return Err(EventSinkError::EventLimitReached {
                    maximum: MAX_STREAM_EVENTS_PER_TURN,
                });
            }

            match state.strategy {
                BackpressureStrategy::Fail => return Err(EventSinkError::Full),
                BackpressureStrategy::DropNonTerminal => {
                    if event.is_droppable() {
                        state.attempted_events += 1;
                        state.dropped_events = state.dropped_events.saturating_add(1);
                        return Ok(());
                    }
                    if let Some(index) = state.queue.iter().position(StreamEvent::is_droppable) {
                        state.queue.remove(index);
                        state.dropped_events = state.dropped_events.saturating_add(1);
                        state.attempted_events += 1;
                        state.queue.push_back(event);
                        self.inner.not_empty.notify_one();
                        return Ok(());
                    }
                }
                BackpressureStrategy::Block => {}
            }

            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Err(EventSinkError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EventSinkError::BackpressureTimeout);
            }
            let wait_for = (deadline - now).min(Duration::from_millis(10));
            let (next_state, _) = self
                .inner
                .not_full
                .wait_timeout(state, wait_for)
                .map_err(|_| EventSinkError::Closed)?;
            state = next_state;
        }
    }
}

impl EventSink for EventSender {
    fn emit(&mut self, event: AgentStreamEvent) -> Result<(), EventSinkError> {
        self.send(event, None)
    }

    fn emit_with_cancellation(
        &mut self,
        event: AgentStreamEvent,
        cancellation: &CancellationToken,
    ) -> Result<(), EventSinkError> {
        self.send(event, Some(cancellation))
    }
}

impl Drop for EventSender {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.sender_count = state.sender_count.saturating_sub(1);
            if state.sender_count == 0 {
                state.closed = true;
                self.inner.not_empty.notify_all();
                self.inner.not_full.notify_all();
            }
        }
    }
}

pub struct EventReceiver {
    inner: Arc<ChannelInner>,
}

impl EventReceiver {
    pub fn recv(&self) -> Result<AgentStreamEvent, EventReceiveError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| EventReceiveError::Closed)?;
        loop {
            if let Some(event) = state.queue.pop_front() {
                self.inner.not_full.notify_one();
                return Ok(event);
            }
            if state.closed {
                return Err(EventReceiveError::Closed);
            }
            state = self
                .inner
                .not_empty
                .wait(state)
                .map_err(|_| EventReceiveError::Closed)?;
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<AgentStreamEvent, EventReceiveError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| EventReceiveError::Closed)?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = state.queue.pop_front() {
                self.inner.not_full.notify_one();
                return Ok(event);
            }
            if state.closed {
                return Err(EventReceiveError::Closed);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EventReceiveError::Timeout);
            }
            let (next_state, wait_result) = self
                .inner
                .not_empty
                .wait_timeout(state, deadline - now)
                .map_err(|_| EventReceiveError::Closed)?;
            state = next_state;
            if wait_result.timed_out() && state.queue.is_empty() {
                return Err(EventReceiveError::Timeout);
            }
        }
    }

    pub fn try_recv(&self) -> Result<AgentStreamEvent, EventReceiveError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| EventReceiveError::Closed)?;
        if let Some(event) = state.queue.pop_front() {
            self.inner.not_full.notify_one();
            Ok(event)
        } else if state.closed {
            Err(EventReceiveError::Closed)
        } else {
            Err(EventReceiveError::Empty)
        }
    }

    pub fn close(&self) {
        close_channel(&self.inner);
    }

    pub fn dropped_events(&self) -> u64 {
        self.inner
            .state
            .lock()
            .map(|state| state.dropped_events)
            .unwrap_or(0)
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        close_channel(&self.inner);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventReceiveError {
    Empty,
    Timeout,
    Closed,
}

impl fmt::Display for EventReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "event queue is empty",
            Self::Timeout => "event receive timed out",
            Self::Closed => "event channel is closed",
        })
    }
}

impl std::error::Error for EventReceiveError {}

struct ChannelInner {
    state: Mutex<ChannelState>,
    not_empty: Condvar,
    not_full: Condvar,
}

struct ChannelState {
    queue: VecDeque<AgentStreamEvent>,
    capacity: usize,
    strategy: BackpressureStrategy,
    block_timeout: Duration,
    closed: bool,
    sender_count: usize,
    attempted_events: usize,
    dropped_events: u64,
}

fn close_channel(inner: &ChannelInner) {
    if let Ok(mut state) = inner.state.lock() {
        state.closed = true;
        inner.not_empty.notify_all();
        inner.not_full.notify_all();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventChannelError {
    InvalidCapacity {
        capacity: usize,
        maximum: usize,
    },
    InvalidBlockTimeout {
        timeout_millis: u64,
        maximum_millis: u64,
    },
}

impl fmt::Display for EventChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity { capacity, maximum } => {
                write!(
                    formatter,
                    "event capacity {capacity} is outside 1..={maximum}"
                )
            }
            Self::InvalidBlockTimeout {
                timeout_millis,
                maximum_millis,
            } => write!(
                formatter,
                "event block timeout {timeout_millis} ms is outside 1..={maximum_millis}"
            ),
        }
    }
}

impl std::error::Error for EventChannelError {}

/// Internal adapter that adds sequence numbers and enforces the per-turn
/// event budget before forwarding to a host sink.
pub(crate) struct StreamEmitter<'a> {
    sink: &'a mut dyn EventSink,
    cancellation: &'a CancellationToken,
    turn_id: String,
    next_sequence: u64,
    emitted: usize,
    announced_tools: BTreeSet<yunxi_protocol::ToolCallId>,
}

impl<'a> StreamEmitter<'a> {
    pub(crate) fn new(
        sink: &'a mut dyn EventSink,
        cancellation: &'a CancellationToken,
        turn_id: impl Into<String>,
    ) -> Self {
        Self {
            sink,
            cancellation,
            turn_id: turn_id.into(),
            next_sequence: 1,
            emitted: 0,
            announced_tools: BTreeSet::new(),
        }
    }

    pub(crate) fn state(
        &mut self,
        round: u16,
        state: StreamTurnState,
    ) -> Result<(), EventSinkError> {
        let event =
            StreamEvent::turn_state(self.next_sequence, self.turn_id.clone(), round, state)?;
        self.send(event)
    }

    pub(crate) fn text_delta(&mut self, round: u16, delta: &str) -> Result<(), EventSinkError> {
        let event = StreamEvent::text_delta(
            self.next_sequence,
            self.turn_id.clone(),
            round,
            delta.to_string(),
        )?;
        self.send(event)
    }

    pub(crate) fn tool_start(&mut self, round: u16, call: &ToolCall) -> Result<(), EventSinkError> {
        if self.announced_tools.contains(call.id()) {
            return Ok(());
        }
        let event = StreamEvent::tool_start(
            self.next_sequence,
            self.turn_id.clone(),
            round,
            call.id().clone(),
            call.name().clone(),
        )?;
        self.send(event)?;
        self.announced_tools.insert(call.id().clone());
        Ok(())
    }

    pub(crate) fn tool_progress(
        &mut self,
        round: u16,
        call_id: yunxi_protocol::ToolCallId,
        tool_name: yunxi_protocol::ToolName,
        progress: &str,
    ) -> Result<(), EventSinkError> {
        let event = StreamEvent::tool_progress(
            self.next_sequence,
            self.turn_id.clone(),
            round,
            call_id,
            tool_name,
            progress.to_string(),
        )?;
        self.send(event)
    }

    pub(crate) fn tool_result(
        &mut self,
        round: u16,
        result: ToolResult,
    ) -> Result<(), EventSinkError> {
        let event =
            StreamEvent::tool_result(self.next_sequence, self.turn_id.clone(), round, result)?;
        self.send(event)
    }

    pub(crate) fn error(&mut self, round: u16, error: StreamError) -> Result<(), EventSinkError> {
        let event =
            StreamEvent::turn_error(self.next_sequence, self.turn_id.clone(), round, error)?;
        self.send(event)
    }

    pub(crate) fn done(
        &mut self,
        round: u16,
        response: Option<yunxi_protocol::ChatResult>,
    ) -> Result<(), EventSinkError> {
        let event =
            StreamEvent::turn_done(self.next_sequence, self.turn_id.clone(), round, response)?;
        self.send(event)
    }

    pub(crate) fn send(&mut self, event: AgentStreamEvent) -> Result<(), EventSinkError> {
        if self.emitted >= MAX_STREAM_EVENTS_PER_TURN {
            return Err(EventSinkError::EventLimitReached {
                maximum: MAX_STREAM_EVENTS_PER_TURN,
            });
        }
        event.validate()?;
        self.emitted += 1;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.sink.emit_with_cancellation(event, self.cancellation)
    }
}

pub(crate) struct ModelStreamBridge<'a, 'b> {
    emitter: &'a mut StreamEmitter<'b>,
    round: u16,
}

impl<'a, 'b> ModelStreamBridge<'a, 'b> {
    pub(crate) fn new(emitter: &'a mut StreamEmitter<'b>, round: u16) -> Self {
        Self { emitter, round }
    }
}

impl crate::model::ModelEventSink for ModelStreamBridge<'_, '_> {
    fn text_delta(&mut self, delta: &str) -> Result<(), crate::error::ModelError> {
        self.emitter.text_delta(self.round, delta).map_err(|_| {
            crate::error::ComponentError::new("event_sink_failed", "event sink unavailable", false)
        })
    }

    fn tool_call_start(&mut self, call: &ToolCall) -> Result<(), crate::error::ModelError> {
        self.emitter.tool_start(self.round, call).map_err(|_| {
            crate::error::ComponentError::new("event_sink_failed", "event sink unavailable", false)
        })
    }
}

pub(crate) struct ToolProgressBridge<'a, 'b> {
    emitter: &'a mut StreamEmitter<'b>,
    round: u16,
    call_id: yunxi_protocol::ToolCallId,
    tool_name: yunxi_protocol::ToolName,
}

impl<'a, 'b> ToolProgressBridge<'a, 'b> {
    pub(crate) fn new(
        emitter: &'a mut StreamEmitter<'b>,
        round: u16,
        call_id: yunxi_protocol::ToolCallId,
        tool_name: yunxi_protocol::ToolName,
    ) -> Self {
        Self {
            emitter,
            round,
            call_id,
            tool_name,
        }
    }
}

impl crate::tool::ToolProgressSink for ToolProgressBridge<'_, '_> {
    fn progress(&mut self, progress: &str) -> Result<(), crate::error::ToolError> {
        self.emitter
            .tool_progress(
                self.round,
                self.call_id.clone(),
                self.tool_name.clone(),
                progress,
            )
            .map_err(|_| {
                crate::error::ComponentError::new(
                    "event_sink_failed",
                    "event sink unavailable",
                    false,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use yunxi_protocol::ChatResult;

    #[test]
    fn drop_policy_keeps_terminal_event_when_consumer_is_slow() {
        let (mut sender, receiver) = EventChannel::with_config(
            EventChannelConfig::new(1, BackpressureStrategy::DropNonTerminal).expect("config"),
        )
        .expect("channel");
        sender
            .emit(StreamEvent::text_delta(1, "turn-1", 1, "a").expect("delta"))
            .expect("first");
        sender
            .emit(StreamEvent::text_delta(2, "turn-1", 1, "b").expect("delta"))
            .expect("drop");
        sender
            .emit(
                StreamEvent::turn_done(3, "turn-1", 1, Some(ChatResult::new("done", None)))
                    .expect("done"),
            )
            .expect("terminal");
        let event = receiver.try_recv().expect("terminal is retained");
        assert!(matches!(event, StreamEvent::TurnDone { .. }));
        assert_eq!(receiver.dropped_events(), 2);
    }

    #[test]
    fn blocked_send_can_be_cancelled_without_waiting_for_the_consumer() {
        let (mut sender, _receiver) = EventChannel::with_config(
            EventChannelConfig::new(1, BackpressureStrategy::Block)
                .expect("config")
                .with_block_timeout(Duration::from_secs(1))
                .expect("timeout"),
        )
        .expect("channel");
        sender
            .emit(StreamEvent::text_delta(1, "turn-1", 1, "a").expect("delta"))
            .expect("first");
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let join = thread::spawn(move || {
            sender.emit_with_cancellation(
                StreamEvent::turn_state(2, "turn-1", 1, StreamTurnState::ModelCalling)
                    .expect("state"),
                &cancellation,
            )
        });
        thread::sleep(Duration::from_millis(20));
        token.cancel("stop");
        assert_eq!(join.join().expect("join"), Err(EventSinkError::Cancelled));
    }
}
