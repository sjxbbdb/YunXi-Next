//! Bounded, cancellable voice stream primitives.
//!
//! The queue in this module is deliberately synchronous.  It is suitable for
//! a process-isolated plugin boundary without forcing an async runtime on the
//! kernel.  A future async adapter can implement the provider traits on top of
//! the same validation rules.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::audio::AudioChunk;
use crate::error::VoiceContractError;
use crate::message::TranscribeRequest;

const WAIT_SLICE: Duration = Duration::from_millis(25);
const MAX_PROVIDER_CODE_BYTES: usize = 64;

/// Errors exposed by a provider boundary.  Diagnostics intentionally contain
/// no audio bytes, request bodies, credentials, or arbitrary provider text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoiceProviderError {
    Contract(VoiceContractError),
    Disabled,
    Cancelled,
    TimedOut,
    Backpressure { buffered: usize, capacity: usize },
    ChunkLargerThanQueue { size: usize, capacity: usize },
    QueueClosed,
    QueuePoisoned,
    ProviderFailure { code: String, retryable: bool },
    InvalidProvider { code: String },
}

impl VoiceProviderError {
    pub fn provider_failure(code: impl Into<String>, retryable: bool) -> Self {
        Self::ProviderFailure {
            code: bounded_code(code.into()),
            retryable,
        }
    }

    pub fn invalid_provider(code: impl Into<String>) -> Self {
        Self::InvalidProvider {
            code: bounded_code(code.into()),
        }
    }

    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::ProviderFailure { retryable, .. } => *retryable,
            Self::TimedOut => true,
            Self::Contract(_)
            | Self::Disabled
            | Self::Cancelled
            | Self::Backpressure { .. }
            | Self::ChunkLargerThanQueue { .. }
            | Self::QueueClosed
            | Self::QueuePoisoned
            | Self::InvalidProvider { .. } => false,
        }
    }
}

impl fmt::Display for VoiceProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(formatter),
            Self::Disabled => formatter.write_str("voice provider is disabled"),
            Self::Cancelled => formatter.write_str("voice operation was cancelled"),
            Self::TimedOut => formatter.write_str("voice operation timed out"),
            Self::Backpressure { buffered, capacity } => write!(
                formatter,
                "voice stream is backpressured ({buffered}/{capacity} bytes)"
            ),
            Self::ChunkLargerThanQueue { size, capacity } => write!(
                formatter,
                "voice chunk is {size} bytes but queue capacity is {capacity}"
            ),
            Self::QueueClosed => formatter.write_str("voice stream queue is closed"),
            Self::QueuePoisoned => formatter.write_str("voice stream queue lock was poisoned"),
            Self::ProviderFailure { code, retryable } => {
                write!(
                    formatter,
                    "voice provider failure ({code}, retryable={retryable})"
                )
            }
            Self::InvalidProvider { code } => write!(formatter, "invalid voice provider ({code})"),
        }
    }
}

impl std::error::Error for VoiceProviderError {}

impl From<VoiceContractError> for VoiceProviderError {
    fn from(error: VoiceContractError) -> Self {
        Self::Contract(error)
    }
}

fn bounded_code(mut code: String) -> String {
    if code.is_empty()
        || !code
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return "provider_error".to_owned();
    }
    code.truncate(MAX_PROVIDER_CODE_BYTES);
    code
}

/// A cloneable cancellation signal shared by a Host and a provider.
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

/// Cancellation and deadline state passed to every blocking provider call.
#[derive(Clone, Debug)]
pub struct OperationContext {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl Default for OperationContext {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationContext {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: None,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Instant::now() + timeout)
    }

    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: Some(deadline),
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

    pub fn check(&self) -> Result<(), VoiceProviderError> {
        if self.is_cancelled() {
            return Err(VoiceProviderError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(VoiceProviderError::TimedOut);
        }
        Ok(())
    }

    fn wait_duration(&self) -> Duration {
        self.remaining()
            .map_or(WAIT_SLICE, |remaining| remaining.min(WAIT_SLICE))
    }
}

struct QueueState {
    chunks: VecDeque<AudioChunk>,
    buffered_bytes: usize,
    capacity_bytes: usize,
    closed: bool,
}

struct QueueShared {
    state: Mutex<QueueState>,
    not_empty: Condvar,
    not_full: Condvar,
}

/// A bounded byte-capacity queue for captured audio chunks.
///
/// `push` waits while the queue is full, periodically checking cancellation
/// and the deadline.  `try_push` provides a non-blocking backpressure signal.
#[derive(Clone)]
pub struct AudioChunkQueue {
    shared: Arc<QueueShared>,
}

impl fmt::Debug for AudioChunkQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (buffered_bytes, capacity_bytes, closed) = self
            .shared
            .state
            .lock()
            .map(|state| (state.buffered_bytes, state.capacity_bytes, state.closed))
            .unwrap_or((0, 0, true));
        formatter
            .debug_struct("AudioChunkQueue")
            .field("buffered_bytes", &buffered_bytes)
            .field("capacity_bytes", &capacity_bytes)
            .field("closed", &closed)
            .finish()
    }
}

impl AudioChunkQueue {
    pub fn new(capacity_bytes: usize) -> Result<Self, VoiceProviderError> {
        crate::StreamStatus::with_capacity(capacity_bytes)?;
        Ok(Self {
            shared: Arc::new(QueueShared {
                state: Mutex::new(QueueState {
                    chunks: VecDeque::new(),
                    buffered_bytes: 0,
                    capacity_bytes,
                    closed: false,
                }),
                not_empty: Condvar::new(),
                not_full: Condvar::new(),
            }),
        })
    }

    pub fn capacity_bytes(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| state.capacity_bytes)
            .unwrap_or_default()
    }

    pub fn buffered_bytes(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| state.buffered_bytes)
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| state.chunks.len())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_closed(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|state| state.closed)
            .unwrap_or(true)
    }

    pub fn try_push(&self, chunk: AudioChunk) -> Result<(), VoiceProviderError> {
        chunk.validate()?;
        let chunk_bytes = chunk.data.len();
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| VoiceProviderError::QueuePoisoned)?;
        if state.closed {
            return Err(VoiceProviderError::QueueClosed);
        }
        if chunk_bytes > state.capacity_bytes {
            return Err(VoiceProviderError::ChunkLargerThanQueue {
                size: chunk_bytes,
                capacity: state.capacity_bytes,
            });
        }
        if state.buffered_bytes.saturating_add(chunk_bytes) > state.capacity_bytes {
            return Err(VoiceProviderError::Backpressure {
                buffered: state.buffered_bytes,
                capacity: state.capacity_bytes,
            });
        }
        state.buffered_bytes += chunk_bytes;
        state.chunks.push_back(chunk);
        self.shared.not_empty.notify_one();
        Ok(())
    }

    pub fn push(
        &self,
        chunk: AudioChunk,
        context: &OperationContext,
    ) -> Result<(), VoiceProviderError> {
        chunk.validate()?;
        let chunk_bytes = chunk.data.len();
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| VoiceProviderError::QueuePoisoned)?;
        loop {
            context.check()?;
            if state.closed {
                return Err(VoiceProviderError::QueueClosed);
            }
            if chunk_bytes > state.capacity_bytes {
                return Err(VoiceProviderError::ChunkLargerThanQueue {
                    size: chunk_bytes,
                    capacity: state.capacity_bytes,
                });
            }
            if state.buffered_bytes.saturating_add(chunk_bytes) <= state.capacity_bytes {
                state.buffered_bytes += chunk_bytes;
                state.chunks.push_back(chunk);
                self.shared.not_empty.notify_one();
                return Ok(());
            }
            let wait_duration = context.wait_duration();
            let (next_state, _) = self
                .shared
                .not_full
                .wait_timeout(state, wait_duration)
                .map_err(|_| VoiceProviderError::QueuePoisoned)?;
            state = next_state;
        }
    }

    pub fn pop(
        &self,
        context: &OperationContext,
    ) -> Result<Option<AudioChunk>, VoiceProviderError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| VoiceProviderError::QueuePoisoned)?;
        loop {
            context.check()?;
            if let Some(chunk) = state.chunks.pop_front() {
                state.buffered_bytes = state.buffered_bytes.saturating_sub(chunk.data.len());
                self.shared.not_full.notify_one();
                return Ok(Some(chunk));
            }
            if state.closed {
                return Ok(None);
            }
            let wait_duration = context.wait_duration();
            let (next_state, _) = self
                .shared
                .not_empty
                .wait_timeout(state, wait_duration)
                .map_err(|_| VoiceProviderError::QueuePoisoned)?;
            state = next_state;
        }
    }

    pub fn close(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            self.shared.not_empty.notify_all();
            self.shared.not_full.notify_all();
        }
    }
}

/// A validated, borrowed iterator over a transcription request's chunks.
pub struct AudioChunkIterator<'a> {
    request_stream: &'a crate::StreamId,
    request_format: crate::AudioFormat,
    chunks: std::slice::Iter<'a, AudioChunk>,
    expected_sequence: u64,
}

impl<'a> AudioChunkIterator<'a> {
    pub fn new(request: &'a TranscribeRequest) -> Result<Self, VoiceContractError> {
        request.validate()?;
        Ok(Self {
            request_stream: &request.stream_id,
            request_format: request.format,
            chunks: request.chunks.iter(),
            expected_sequence: 0,
        })
    }
}

impl<'a> Iterator for AudioChunkIterator<'a> {
    type Item = Result<&'a AudioChunk, VoiceContractError>;

    fn next(&mut self) -> Option<Self::Item> {
        let chunk = self.chunks.next()?;
        let result = if chunk.stream_id != *self.request_stream {
            Err(VoiceContractError::MixedStream)
        } else if chunk.format != self.request_format {
            Err(VoiceContractError::MixedFormat)
        } else if chunk.sequence != self.expected_sequence {
            Err(VoiceContractError::InvalidSequence {
                expected: self.expected_sequence,
                actual: chunk.sequence,
            })
        } else {
            chunk.validate().map(|()| chunk)
        };
        self.expected_sequence = self.expected_sequence.saturating_add(1);
        Some(result)
    }
}
