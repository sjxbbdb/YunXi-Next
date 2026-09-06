//! A single, bounded standard-library worker for background Agent turns.

use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use yunxi_protocol::{ChatMessage, ChatRole, MAX_STREAM_TEXT_BYTES};

use crate::agent::{Agent, TurnResult};
use crate::cancellation::CancellationToken;
use crate::error::AgentError;
use crate::stream::{
    EventChannel, EventChannelConfig, EventChannelError, EventReceiver, EventSender,
};
use crate::{ContextAssembler, ModelProvider, ToolBroker};

const MAX_BACKGROUND_MESSAGE_BYTES: usize = MAX_STREAM_TEXT_BYTES;

/// State visible to callers without exposing prompts, responses, or panic
/// payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundTurnState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    WorkerStopped,
}

impl BackgroundTurnState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut | Self::WorkerStopped
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundTurnSnapshot {
    id: String,
    turn_id: Option<String>,
    state: BackgroundTurnState,
    error_code: Option<String>,
    dropped_events: u64,
}

impl BackgroundTurnSnapshot {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    pub const fn state(&self) -> BackgroundTurnState {
        self.state
    }

    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    pub const fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

/// Errors returned before a background turn is accepted by the worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackgroundError {
    Busy,
    QueueFull,
    WorkerStopped,
    InvalidInput(String),
    InvalidEventChannel(EventChannelError),
    SpawnFailed,
}

impl std::fmt::Display for BackgroundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("background Agent already has an active turn"),
            Self::QueueFull => formatter.write_str("background Agent command queue is full"),
            Self::WorkerStopped => formatter.write_str("background Agent worker is stopped"),
            Self::InvalidInput(message) => write!(formatter, "invalid background input: {message}"),
            Self::InvalidEventChannel(error) => write!(formatter, "invalid event channel: {error}"),
            Self::SpawnFailed => formatter.write_str("could not start background Agent worker"),
        }
    }
}

impl std::error::Error for BackgroundError {}

/// A handle for querying, cancelling, waiting for, and consuming one turn's
/// bounded event stream.
pub struct BackgroundTurnHandle {
    id: String,
    job: Arc<JobState>,
    events: EventReceiver,
}

impl BackgroundTurnHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn turn_id(&self) -> Option<String> {
        self.job.snapshot().turn_id
    }

    pub fn snapshot(&self) -> BackgroundTurnSnapshot {
        self.job.snapshot()
    }

    pub fn cancel(&self, reason: impl Into<String>) {
        self.job.cancellation.cancel(reason);
    }

    pub fn is_finished(&self) -> bool {
        self.snapshot().state.is_terminal()
    }

    pub fn try_recv_event(
        &self,
    ) -> Result<yunxi_protocol::AgentStreamEvent, crate::EventReceiveError> {
        self.events.try_recv()
    }

    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<yunxi_protocol::AgentStreamEvent, crate::EventReceiveError> {
        self.events.recv_timeout(timeout)
    }

    pub fn dropped_events(&self) -> u64 {
        self.events.dropped_events()
    }

    /// Waits for completion while retaining the result for subsequent reads.
    pub fn wait(&self) -> Result<TurnResult, AgentError> {
        let mut data = lock_or_recover(&self.job.data);
        while !data.snapshot.state.is_terminal() {
            data = self
                .job
                .done
                .wait(data)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        data.result.clone().unwrap_or_else(|| {
            Err(AgentError::protocol(
                "background_worker_stopped",
                "background worker stopped before producing a result",
            ))
        })
    }
}

impl Drop for BackgroundTurnHandle {
    fn drop(&mut self) {
        if !self.is_finished() {
            self.job.cancellation.cancel("background handle dropped");
        }
    }
}

/// Owns one worker thread and one Agent.  The worker is reused for sequential
/// turns, so a caller cannot accidentally create an unbounded thread pool.
pub struct BackgroundAgent<M, C, T> {
    inner: Arc<BackgroundInner>,
    worker: Option<JoinHandle<()>>,
    marker: PhantomData<(M, C, T)>,
}

impl<M, C, T> BackgroundAgent<M, C, T>
where
    M: ModelProvider + Send + 'static,
    C: ContextAssembler + Send + 'static,
    T: ToolBroker + Send + 'static,
{
    pub fn new(agent: Agent<M, C, T>) -> Result<Self, BackgroundError> {
        let (commands, receiver) = sync_channel(1);
        let inner = Arc::new(BackgroundInner {
            commands,
            active: Mutex::new(None),
            next_id: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
        });
        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name("yunxi-agent-background".to_string())
            .spawn(move || worker_loop(agent, receiver, worker_inner))
            .map_err(|_| BackgroundError::SpawnFailed)?;
        Ok(Self {
            inner,
            worker: Some(worker),
            marker: PhantomData,
        })
    }

    pub fn start_text_turn(
        &self,
        content: impl Into<String>,
    ) -> Result<BackgroundTurnHandle, BackgroundError> {
        self.start_text_turn_with_config(content, EventChannelConfig::default())
    }

    pub fn start_text_turn_with_config(
        &self,
        content: impl Into<String>,
        event_config: EventChannelConfig,
    ) -> Result<BackgroundTurnHandle, BackgroundError> {
        self.start_turn_with_config(ChatMessage::user(content), event_config)
    }

    pub fn start_turn(
        &self,
        message: ChatMessage,
    ) -> Result<BackgroundTurnHandle, BackgroundError> {
        self.start_turn_with_config(message, EventChannelConfig::default())
    }

    pub fn start_turn_with_config(
        &self,
        message: ChatMessage,
        event_config: EventChannelConfig,
    ) -> Result<BackgroundTurnHandle, BackgroundError> {
        validate_message(&message)?;
        let (sender, receiver) = EventChannel::with_config(event_config)
            .map_err(BackgroundError::InvalidEventChannel)?;
        let job_number = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let job = Arc::new(JobState::new(format!("job-{job_number}")));

        {
            let mut active = lock_or_recover(&self.inner.active);
            if self.inner.stopping.load(Ordering::Acquire) {
                return Err(BackgroundError::WorkerStopped);
            }
            if active
                .as_ref()
                .is_some_and(|current| !current.snapshot().state.is_terminal())
            {
                return Err(BackgroundError::Busy);
            }
            *active = Some(Arc::clone(&job));
        }

        let command = BackgroundCommand::Start {
            job: Arc::clone(&job),
            message,
            events: sender,
        };
        match self.inner.commands.try_send(command) {
            Ok(()) => Ok(BackgroundTurnHandle {
                id: job.snapshot().id.clone(),
                job,
                events: receiver,
            }),
            Err(TrySendError::Full(_)) => {
                *lock_or_recover(&self.inner.active) = None;
                Err(BackgroundError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.inner.stopping.store(true, Ordering::Release);
                *lock_or_recover(&self.inner.active) = None;
                Err(BackgroundError::WorkerStopped)
            }
        }
    }

    pub fn active_snapshot(&self) -> Option<BackgroundTurnSnapshot> {
        lock_or_recover(&self.inner.active)
            .as_ref()
            .map(|job| job.snapshot())
    }

    /// Stops the worker and joins it.  Providers are required to honor the
    /// cancellation token; safe Rust cannot forcibly kill a provider that
    /// blocks forever inside a synchronous foreign call.
    pub fn shutdown(&mut self) {
        self.stop_worker();
    }
}

impl<M, C, T> BackgroundAgent<M, C, T> {
    fn stop_worker(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
        if let Some(job) = lock_or_recover(&self.inner.active).as_ref() {
            job.cancellation.cancel("background worker shutdown");
        }
        let _ = self.inner.commands.try_send(BackgroundCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl<M, C, T> Drop for BackgroundAgent<M, C, T> {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

struct BackgroundInner {
    commands: SyncSender<BackgroundCommand>,
    active: Mutex<Option<Arc<JobState>>>,
    next_id: AtomicU64,
    stopping: AtomicBool,
}

enum BackgroundCommand {
    Start {
        job: Arc<JobState>,
        message: ChatMessage,
        events: EventSender,
    },
    Shutdown,
}

struct JobState {
    cancellation: CancellationToken,
    data: Mutex<JobData>,
    done: Condvar,
}

struct JobData {
    snapshot: BackgroundTurnSnapshot,
    result: Option<Result<TurnResult, AgentError>>,
}

impl JobState {
    fn new(id: String) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            data: Mutex::new(JobData {
                snapshot: BackgroundTurnSnapshot {
                    id,
                    turn_id: None,
                    state: BackgroundTurnState::Queued,
                    error_code: None,
                    dropped_events: 0,
                },
                result: None,
            }),
            done: Condvar::new(),
        }
    }

    fn set_running(&self) {
        lock_or_recover(&self.data).snapshot.state = BackgroundTurnState::Running;
    }

    fn finish(
        &self,
        result: Result<TurnResult, AgentError>,
        dropped_events: u64,
        turn_id: Option<String>,
    ) {
        let state = match &result {
            Ok(_) => BackgroundTurnState::Completed,
            Err(error) if error.is_timeout() => BackgroundTurnState::TimedOut,
            Err(error) if error.is_cancelled() => BackgroundTurnState::Cancelled,
            Err(_) => BackgroundTurnState::Failed,
        };
        let error_code = result.as_ref().err().map(|error| error.code().to_string());
        let mut data = lock_or_recover(&self.data);
        data.snapshot.state = state;
        data.snapshot.error_code = error_code;
        data.snapshot.turn_id = turn_id;
        data.snapshot.dropped_events = dropped_events;
        data.result = Some(result);
        self.done.notify_all();
    }

    fn stop_without_result(&self, code: &'static str) {
        let mut data = lock_or_recover(&self.data);
        data.snapshot.state = BackgroundTurnState::WorkerStopped;
        data.snapshot.error_code = Some(code.to_string());
        self.done.notify_all();
    }

    fn snapshot(&self) -> BackgroundTurnSnapshot {
        lock_or_recover(&self.data).snapshot.clone()
    }
}

fn worker_loop<M, C, T>(
    mut agent: Agent<M, C, T>,
    receiver: std::sync::mpsc::Receiver<BackgroundCommand>,
    inner: Arc<BackgroundInner>,
) where
    M: ModelProvider + Send + 'static,
    C: ContextAssembler + Send + 'static,
    T: ToolBroker + Send + 'static,
{
    while let Ok(command) = receiver.recv() {
        match command {
            BackgroundCommand::Shutdown => break,
            BackgroundCommand::Start {
                job,
                message,
                mut events,
            } => {
                if inner.stopping.load(Ordering::Acquire) {
                    job.stop_without_result("background_worker_stopped");
                    break;
                }
                job.set_running();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    agent.run_turn_streaming(message, &job.cancellation, &mut events)
                }));
                let dropped_events = events.dropped_events();
                let turn_id = match &result {
                    Ok(Ok(turn)) => Some(turn.snapshot().id().to_string()),
                    Ok(Err(_)) => agent.last_turn().map(|turn| turn.id().to_string()),
                    Err(_) => None,
                };
                match result {
                    Ok(result) => job.finish(result, dropped_events, turn_id),
                    Err(_) => {
                        job.finish(
                            Err(AgentError::protocol(
                                "worker_panic",
                                "background turn failed in an isolated worker",
                            )),
                            dropped_events,
                            turn_id,
                        );
                        inner.stopping.store(true, Ordering::Release);
                        break;
                    }
                }
                if inner.stopping.load(Ordering::Acquire) {
                    break;
                }
            }
        }
    }

    inner.stopping.store(true, Ordering::Release);
    if let Some(job) = lock_or_recover(&inner.active).as_ref() {
        if !job.snapshot().state.is_terminal() {
            job.stop_without_result("background_worker_stopped");
        }
    }
}

fn validate_message(message: &ChatMessage) -> Result<(), BackgroundError> {
    if message.role() != ChatRole::User {
        return Err(BackgroundError::InvalidInput(
            "background turns must start with a user message".to_string(),
        ));
    }
    if message.content().is_empty() || message.content().len() > MAX_BACKGROUND_MESSAGE_BYTES {
        return Err(BackgroundError::InvalidInput(
            "user message is empty or exceeds the bounded input size".to_string(),
        ));
    }
    if message.content().contains('\0') {
        return Err(BackgroundError::InvalidInput(
            "user message contains a NUL character".to_string(),
        ));
    }
    Ok(())
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentConfig, ConversationContextAssembler, ModelError, ModelProvider, ModelRequest,
        SessionLimits, ToolBroker, ToolError, ToolRequest, TurnBudget,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    struct Model {
        responses: VecDeque<Result<yunxi_protocol::ChatResult, ModelError>>,
        delay: Duration,
        panics: bool,
        calls: Arc<AtomicUsize>,
    }

    impl Model {
        fn text(response: &str) -> Self {
            Self {
                responses: VecDeque::from([Ok(yunxi_protocol::ChatResult::new(response, None))]),
                delay: Duration::ZERO,
                panics: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ModelProvider for Model {
        fn complete(
            &mut self,
            _request: &ModelRequest,
            cancellation: &CancellationToken,
        ) -> Result<yunxi_protocol::ChatResult, ModelError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.panics {
                panic!("fixture panic");
            }
            let end = std::time::Instant::now() + self.delay;
            while std::time::Instant::now() < end {
                cancellation
                    .check()
                    .map_err(|error| ModelError::new(error.code(), "model cancelled", false))?;
                thread::sleep(Duration::from_millis(1));
            }
            self.responses
                .pop_front()
                .unwrap_or_else(|| Ok(yunxi_protocol::ChatResult::new("done", None)))
        }
    }

    struct Tools;

    impl ToolBroker for Tools {
        fn catalog(&self) -> Result<yunxi_protocol::ToolCatalog, ToolError> {
            yunxi_protocol::ToolCatalog::new(Vec::new())
                .map_err(|error| ToolError::new("catalog", error.to_string(), false))
        }

        fn execute(
            &mut self,
            _request: ToolRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<yunxi_protocol::ToolResultOutcome, ToolError> {
            unreachable!("empty catalog has no tool calls")
        }
    }

    fn agent(
        model: Model,
        config: AgentConfig,
    ) -> Agent<Model, ConversationContextAssembler, Tools> {
        Agent::new(
            "background",
            model,
            ConversationContextAssembler,
            Tools,
            config,
        )
        .expect("agent")
    }

    #[test]
    fn background_turn_can_be_queried_and_reused_after_reaping() {
        let mut runner = BackgroundAgent::new(agent(Model::text("done"), AgentConfig::default()))
            .expect("runner");
        let handle = runner.start_text_turn("hello").expect("start");
        let result = handle.wait().expect("result");
        assert_eq!(result.content(), "done");
        assert_eq!(handle.snapshot().state(), BackgroundTurnState::Completed);
        drop(handle);
        let second = runner.start_text_turn("again").expect("reuse");
        assert_eq!(second.wait().expect("second").content(), "done");
        runner.shutdown();
    }

    #[test]
    fn timeout_and_cancel_propagate_without_leaking_a_worker_thread() {
        let config = AgentConfig::new(TurnBudget::conservative(), SessionLimits::default())
            .expect("config")
            .with_turn_timeout(Duration::from_millis(10))
            .expect("timeout");
        let mut runner = BackgroundAgent::new(agent(
            Model {
                delay: Duration::from_millis(100),
                ..Model::text("late")
            },
            config,
        ))
        .expect("runner");
        let timeout = runner.start_text_turn("timeout").expect("start");
        let timeout_result = timeout.wait().expect_err("timeout");
        assert!(timeout_result.is_timeout());
        assert_eq!(timeout.snapshot().state(), BackgroundTurnState::TimedOut);

        let cancel = runner.start_text_turn("cancel").expect("start");
        cancel.cancel("user stopped");
        let cancel_result = cancel.wait().expect_err("cancel");
        assert!(cancel_result.is_cancelled());
        assert_eq!(cancel.snapshot().state(), BackgroundTurnState::Cancelled);
        runner.shutdown();
    }

    #[test]
    fn a_panicking_provider_is_isolated_and_reaped() {
        let mut runner = BackgroundAgent::new(agent(
            Model {
                panics: true,
                ..Model::text("never")
            },
            AgentConfig::default(),
        ))
        .expect("runner");
        let handle = runner.start_text_turn("panic").expect("start");
        let error = handle.wait().expect_err("panic is failure");
        assert_eq!(error.code(), "worker_panic");
        assert_eq!(handle.snapshot().state(), BackgroundTurnState::Failed);
        assert!(matches!(
            runner.start_text_turn("second"),
            Err(BackgroundError::WorkerStopped)
        ));
        runner.shutdown();
    }
}
