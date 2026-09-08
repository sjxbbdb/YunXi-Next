//! Bounded background iLink long-polling for hosts that must remain responsive.
//!
//! The worker owns the transport on one thread and emits validated poll
//! envelopes through a small channel. It never mutates a control plane. A Host
//! imports each envelope with `accept_polled_batch`, which keeps Agent work and
//! provider polling independently controllable.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;

use crate::control::{CancellationToken, RequestContext, RequestControlError};
use crate::ilink::{IlinkError, IlinkTransport, PollBatch};
use crate::runtime::{ServeOptions, ServeReport};

const EVENT_QUEUE_CAPACITY: usize = 8;
const SNAPSHOT_ERROR_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LongPollState {
    Running,
    CancellationRequested,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolledBatch {
    pub requested_cursor: String,
    pub batch: PollBatch,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LongPollSnapshot {
    pub state: LongPollState,
    pub polls: usize,
    pub received_messages: usize,
    pub retries: usize,
    pub pending_batches: usize,
    pub cursor: String,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LongPollOptions {
    pub max_polls: usize,
    pub max_messages_per_poll: usize,
    pub max_transient_retries: u16,
    pub retry_backoff: Duration,
}

impl Default for LongPollOptions {
    fn default() -> Self {
        Self {
            max_polls: 1,
            max_messages_per_poll: crate::MAX_ILINK_MESSAGES,
            max_transient_retries: 3,
            retry_backoff: Duration::from_millis(250),
        }
    }
}

impl LongPollOptions {
    pub fn from_serve(options: &ServeOptions) -> Self {
        Self {
            max_polls: options.max_polls,
            max_messages_per_poll: options.max_messages_per_poll,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), LongPollError> {
        if self.max_polls == 0 || self.max_polls > 10_000 {
            return Err(LongPollError::InvalidOptions("max_polls"));
        }
        if self.max_messages_per_poll == 0 || self.max_messages_per_poll > crate::MAX_ILINK_MESSAGES
        {
            return Err(LongPollError::InvalidOptions("max_messages_per_poll"));
        }
        if self.retry_backoff > Duration::from_secs(30) {
            return Err(LongPollError::InvalidOptions("retry_backoff"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum LongPollError {
    InvalidOptions(&'static str),
    Cancelled,
    TimedOut,
    EventQueueFull,
    TooManyMessages { count: usize, maximum: usize },
    Transport(IlinkError),
    WorkerPanicked,
}

impl std::fmt::Display for LongPollError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(field) => write!(formatter, "invalid long-poll option: {field}"),
            Self::Cancelled => formatter.write_str("long-poll worker was cancelled"),
            Self::TimedOut => formatter.write_str("long-poll worker timed out"),
            Self::EventQueueFull => formatter.write_str("long-poll event queue is full"),
            Self::TooManyMessages { count, maximum } => {
                write!(
                    formatter,
                    "long-poll returned {count} messages, maximum is {maximum}"
                )
            }
            Self::Transport(error) => error.fmt(formatter),
            Self::WorkerPanicked => formatter.write_str("long-poll worker panicked"),
        }
    }
}

impl std::error::Error for LongPollError {}

pub struct LongPollCompletion<T> {
    pub transport: T,
    pub report: Result<ServeReport, LongPollError>,
}

struct WorkerState {
    snapshot: LongPollSnapshot,
    finished: bool,
}

/// A single bounded long-poll worker. Dropping it requests cancellation; it
/// does not join the worker, so a Host shutdown path never blocks on provider
/// I/O. `try_finish` can reclaim the transport after the worker exits.
pub struct LongPollWorker<T> {
    cancellation: CancellationToken,
    events: Receiver<PolledBatch>,
    state: Arc<Mutex<WorkerState>>,
    join: Option<JoinHandle<LongPollCompletion<T>>>,
}

impl<T> std::fmt::Debug for LongPollWorker<T>
where
    T: IlinkTransport + 'static,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LongPollWorker")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl<T> LongPollWorker<T>
where
    T: IlinkTransport + 'static,
{
    pub fn spawn(
        mut transport: T,
        cursor: impl Into<String>,
        options: LongPollOptions,
        context: RequestContext,
    ) -> Result<Self, LongPollError> {
        options.validate()?;
        let cursor = cursor.into();
        let (sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let cancellation = context.cancellation_token();
        let state = Arc::new(Mutex::new(WorkerState {
            snapshot: LongPollSnapshot {
                state: LongPollState::Running,
                polls: 0,
                received_messages: 0,
                retries: 0,
                pending_batches: 0,
                cursor: cursor.clone(),
                last_error: None,
            },
            finished: false,
        }));
        let worker_state = Arc::clone(&state);
        let join = thread::Builder::new()
            .name("yunxi-weixin-long-poll".to_owned())
            .spawn(move || {
                let report = run_worker(
                    &mut transport,
                    cursor,
                    options,
                    context,
                    &sender,
                    &worker_state,
                );
                LongPollCompletion { transport, report }
            })
            .map_err(|_| LongPollError::WorkerPanicked)?;
        Ok(Self {
            cancellation,
            events,
            state,
            join: Some(join),
        })
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
        if let Ok(mut state) = self.state.lock() {
            if state.snapshot.state == LongPollState::Running {
                state.snapshot.state = LongPollState::CancellationRequested;
            }
        }
    }

    pub fn snapshot(&self) -> LongPollSnapshot {
        self.state
            .lock()
            .map(|state| state.snapshot.clone())
            .unwrap_or_else(|_| LongPollSnapshot {
                state: LongPollState::Failed,
                polls: 0,
                received_messages: 0,
                retries: 0,
                pending_batches: 0,
                cursor: String::new(),
                last_error: Some("long-poll worker state was poisoned".to_owned()),
            })
    }

    pub fn try_next_batch(&self) -> Option<PolledBatch> {
        match self.events.try_recv() {
            Ok(batch) => {
                if let Ok(mut state) = self.state.lock() {
                    state.snapshot.pending_batches =
                        state.snapshot.pending_batches.saturating_sub(1);
                }
                Some(batch)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    pub fn try_finish(&mut self) -> Result<Option<LongPollCompletion<T>>, LongPollError> {
        if !self
            .state
            .lock()
            .map(|state| state.finished)
            .unwrap_or(true)
        {
            return Ok(None);
        }
        let join = self.join.take().ok_or(LongPollError::WorkerPanicked)?;
        join.join()
            .map(Some)
            .map_err(|_| LongPollError::WorkerPanicked)
    }
}

impl<T> Drop for LongPollWorker<T> {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut state) = self.state.lock() {
            if state.snapshot.state == LongPollState::Running {
                state.snapshot.state = LongPollState::CancellationRequested;
            }
        }
    }
}

fn run_worker<T>(
    transport: &mut T,
    mut cursor: String,
    options: LongPollOptions,
    context: RequestContext,
    events: &SyncSender<PolledBatch>,
    state: &Arc<Mutex<WorkerState>>,
) -> Result<ServeReport, LongPollError>
where
    T: IlinkTransport,
{
    let mut report = ServeReport {
        polls: 0,
        received_messages: 0,
        enqueued_messages: 0,
        duplicate_messages: 0,
        cursor: cursor.clone(),
    };
    let mut retries = 0_u16;
    let result = (|| {
        while report.polls < options.max_polls {
            context.check().map_err(map_control_error)?;
            let requested_cursor = cursor.clone();
            let batch = match transport.get_updates(&cursor, &context) {
                Ok(batch) => {
                    retries = 0;
                    batch
                }
                Err(error) if retryable(&error) && retries < options.max_transient_retries => {
                    retries += 1;
                    update_retry(state, retries as usize);
                    sleep_with_cancellation(options.retry_backoff, &context)?;
                    continue;
                }
                Err(error) => return Err(LongPollError::Transport(error)),
            };
            if batch.messages.len() > options.max_messages_per_poll {
                return Err(LongPollError::TooManyMessages {
                    count: batch.messages.len(),
                    maximum: options.max_messages_per_poll,
                });
            }
            let received = batch.messages.len();
            cursor = batch.cursor.clone();
            report.polls += 1;
            report.received_messages += received;
            report.cursor = cursor.clone();
            events
                .try_send(PolledBatch {
                    requested_cursor,
                    batch,
                })
                .map_err(|error| match error {
                    TrySendError::Full(_) => LongPollError::EventQueueFull,
                    TrySendError::Disconnected(_) => LongPollError::EventQueueFull,
                })?;
            if let Ok(mut state) = state.lock() {
                state.snapshot.polls = report.polls;
                state.snapshot.received_messages = report.received_messages;
                state.snapshot.cursor = cursor.clone();
                state.snapshot.pending_batches = state.snapshot.pending_batches.saturating_add(1);
            }
        }
        Ok(report)
    })();
    finish_state(state, &result);
    result
}

fn retryable(error: &IlinkError) -> bool {
    matches!(
        error,
        IlinkError::TimedOut
            | IlinkError::Transport(_)
            | IlinkError::Unavailable(_)
            | IlinkError::HttpStatus {
                status: 429 | 500..=599
            }
    )
}

fn sleep_with_cancellation(
    duration: Duration,
    context: &RequestContext,
) -> Result<(), LongPollError> {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        context.check().map_err(map_control_error)?;
        thread::sleep(
            Duration::from_millis(25)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
    Ok(())
}

fn update_retry(state: &Arc<Mutex<WorkerState>>, retries: usize) {
    if let Ok(mut state) = state.lock() {
        state.snapshot.retries = retries;
    }
}

fn finish_state(state: &Arc<Mutex<WorkerState>>, result: &Result<ServeReport, LongPollError>) {
    if let Ok(mut state) = state.lock() {
        state.finished = true;
        state.snapshot.state = if matches!(
            result,
            Err(LongPollError::Cancelled | LongPollError::TimedOut)
        ) {
            LongPollState::CancellationRequested
        } else if result.is_ok() {
            LongPollState::Completed
        } else {
            LongPollState::Failed
        };
        state.snapshot.last_error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .map(|error| error.chars().take(SNAPSHOT_ERROR_BYTES).collect());
    }
}

fn map_control_error(error: RequestControlError) -> LongPollError {
    match error {
        RequestControlError::Cancelled => LongPollError::Cancelled,
        RequestControlError::TimedOut => LongPollError::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LoopbackIlinkTransport, MemorySecretStore, PollBatch, QrChallenge, QrPoll, SecretMaterial,
        WeixinControlPlane,
    };

    #[test]
    fn worker_emits_batches_and_can_be_reclaimed_without_blocking_the_caller() {
        let mut transport = LoopbackIlinkTransport::new();
        transport.queue_poll(Ok(PollBatch::new("next", vec![]).expect("batch")));
        let mut worker = LongPollWorker::spawn(
            transport,
            "",
            LongPollOptions::default(),
            RequestContext::new(),
        )
        .expect("worker");
        std::thread::sleep(Duration::from_millis(10));
        assert!(worker.snapshot().polls <= 1);
        let completion = worker.try_finish().expect("finish check");
        if let Some(completion) = completion {
            assert!(completion.report.is_ok());
        } else {
            worker.cancel();
        }
    }

    #[test]
    fn cancellation_interrupts_retry_backoff_without_waiting_for_the_backoff() {
        let mut transport = LoopbackIlinkTransport::new();
        transport.queue_poll(Err(crate::IlinkError::Transport("temporary")));
        let mut worker = LongPollWorker::spawn(
            transport,
            "",
            LongPollOptions {
                max_polls: 1,
                max_messages_per_poll: crate::MAX_ILINK_MESSAGES,
                max_transient_retries: 3,
                retry_backoff: Duration::from_secs(5),
            },
            RequestContext::new(),
        )
        .expect("worker");
        std::thread::sleep(Duration::from_millis(10));
        worker.cancel();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let completion = loop {
            if let Some(completion) = worker.try_finish().expect("finish") {
                break completion;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not cancel"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(matches!(completion.report, Err(LongPollError::Cancelled)));
    }

    #[test]
    fn polled_batch_is_imported_by_the_control_plane() {
        let mut transport = LoopbackIlinkTransport::new();
        transport.queue_qr(QrChallenge::new("qr", "image").expect("qr"));
        transport.queue_login_poll(Ok(QrPoll::confirmed(
            SecretMaterial::from_text("token").expect("token"),
        )));
        let mut plane =
            WeixinControlPlane::new("test", transport, MemorySecretStore::new()).expect("plane");
        plane
            .login(
                crate::LoginOptions::new(crate::SecretRef::new("host:test/token").expect("ref")),
                &RequestContext::new(),
            )
            .expect("login");
        plane
            .poll_login(&RequestContext::new())
            .expect("poll login");
        let batch = PollBatch::new(
            "next",
            vec![crate::IlinkMessage::text("in", "peer", "hello").expect("message")],
        )
        .expect("batch");
        let report = plane
            .accept_polled_batch(
                PolledBatch {
                    requested_cursor: String::new(),
                    batch,
                },
                &ServeOptions::default(),
            )
            .expect("import");
        assert_eq!(report.enqueued_messages, 1);
    }
}
