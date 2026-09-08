//! Bounded, cancellable proactive scheduling facade and worker.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use yunxi_protocol::{MailboxItemKind, ProactiveSchedulerRequest, ProactiveTrigger};

use crate::{TemplateContext, evaluate, render_love_letter};

pub const MIN_WORKER_INTERVAL: Duration = Duration::from_millis(10);
pub const DEFAULT_WORKER_INTERVAL: Duration = Duration::from_secs(60);
pub const MAX_ENQUEUES_PER_TICK: usize = 8;
pub const MAX_IDEMPOTENCY_KEYS: usize = 512;

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    enabled: bool,
    interval: Duration,
    max_enqueues_per_tick: usize,
    template_enabled: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: DEFAULT_WORKER_INTERVAL,
            max_enqueues_per_tick: 1,
            template_enabled: false,
        }
    }
}

impl SchedulerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval.max(MIN_WORKER_INTERVAL);
        self
    }

    pub fn with_max_enqueues_per_tick(mut self, maximum: usize) -> Self {
        self.max_enqueues_per_tick = maximum.clamp(1, MAX_ENQUEUES_PER_TICK);
        self
    }

    pub fn with_template_fallback(mut self, enabled: bool) -> Self {
        self.template_enabled = enabled;
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn max_enqueues_per_tick(&self) -> usize {
        self.max_enqueues_per_tick
    }

    pub fn template_fallback_enabled(&self) -> bool {
        self.template_enabled
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerTick {
    request: ProactiveSchedulerRequest,
    idempotency_scope: String,
    template: Option<TemplateContext>,
}

impl SchedulerTick {
    pub fn new(request: ProactiveSchedulerRequest, idempotency_scope: impl Into<String>) -> Self {
        Self {
            request,
            idempotency_scope: compact(&idempotency_scope.into(), 160),
            template: None,
        }
    }

    pub fn with_template(mut self, template: TemplateContext) -> Self {
        self.template = Some(template);
        self
    }

    pub fn request(&self) -> &ProactiveSchedulerRequest {
        &self.request
    }

    pub fn idempotency_scope(&self) -> &str {
        &self.idempotency_scope
    }

    pub fn template(&self) -> Option<&TemplateContext> {
        self.template.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledMessage {
    kind: MailboxItemKind,
    subject: String,
    content: String,
    reason: String,
    idempotency_key: String,
}

impl ScheduledMessage {
    fn new(
        kind: MailboxItemKind,
        subject: impl Into<String>,
        content: impl Into<String>,
        reason: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            subject: compact(&subject.into(), 200),
            content: compact(&content.into(), 64 * 1024),
            reason: compact(&reason.into(), 500),
            idempotency_key: compact(&idempotency_key.into(), 256),
        }
    }

    pub fn kind(&self) -> MailboxItemKind {
        self.kind
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueDisposition {
    Created,
    AlreadyPresent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerSinkError {
    message: String,
}

impl SchedulerSinkError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: compact(&message.into(), 1_024),
        }
    }
}

impl fmt::Display for SchedulerSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SchedulerSinkError {}

pub trait ProactiveSink: Send + Sync + 'static {
    fn enqueue(&self, message: &ScheduledMessage)
    -> Result<EnqueueDisposition, SchedulerSinkError>;
}

impl<F> ProactiveSink for F
where
    F: Fn(&ScheduledMessage) -> Result<EnqueueDisposition, SchedulerSinkError>
        + Send
        + Sync
        + 'static,
{
    fn enqueue(
        &self,
        message: &ScheduledMessage,
    ) -> Result<EnqueueDisposition, SchedulerSinkError> {
        self(message)
    }
}

pub trait TickSource: Send + Sync + 'static {
    fn next_tick(&self) -> Result<Option<SchedulerTick>, String>;
}

impl<F> TickSource for F
where
    F: Fn() -> Result<Option<SchedulerTick>, String> + Send + Sync + 'static,
{
    fn next_tick(&self) -> Result<Option<SchedulerTick>, String> {
        self()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerLifecycle {
    Disabled,
    Running,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerStatus {
    lifecycle: WorkerLifecycle,
    ticks: u64,
    enqueued: u64,
    duplicates: u64,
    failures: u64,
    skipped_disabled: u64,
    last_suppressed_reason: Option<String>,
    last_error: Option<String>,
}

impl SchedulerStatus {
    pub fn lifecycle(&self) -> WorkerLifecycle {
        self.lifecycle
    }

    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    pub fn enqueued(&self) -> u64 {
        self.enqueued
    }

    pub fn duplicates(&self) -> u64 {
        self.duplicates
    }

    pub fn failures(&self) -> u64 {
        self.failures
    }

    pub fn skipped_disabled(&self) -> u64 {
        self.skipped_disabled
    }

    pub fn last_suppressed_reason(&self) -> Option<&str> {
        self.last_suppressed_reason.as_deref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickOutcome {
    enqueued: usize,
    duplicates: usize,
    failures: usize,
    template_generated: bool,
    suppressed_reason: Option<String>,
}

impl TickOutcome {
    pub fn enqueued(&self) -> usize {
        self.enqueued
    }

    pub fn duplicates(&self) -> usize {
        self.duplicates
    }

    pub fn failures(&self) -> usize {
        self.failures
    }

    pub fn template_generated(&self) -> bool {
        self.template_generated
    }

    pub fn suppressed_reason(&self) -> Option<&str> {
        self.suppressed_reason.as_deref()
    }
}

struct SharedState {
    status: Mutex<SchedulerStatus>,
    keys: Mutex<BoundedKeys>,
}

struct BoundedKeys {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl BoundedKeys {
    fn contains(&self, key: &str) -> bool {
        self.set.contains(key)
    }

    fn insert(&mut self, key: String) {
        if !self.set.insert(key.clone()) {
            return;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_IDEMPOTENCY_KEYS {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }
}

pub struct SchedulerFacade<S> {
    config: SchedulerConfig,
    sink: S,
    shared: Arc<SharedState>,
}

impl<S> SchedulerFacade<S>
where
    S: ProactiveSink,
{
    pub fn new(config: SchedulerConfig, sink: S) -> Arc<Self> {
        let lifecycle = if config.is_enabled() {
            WorkerLifecycle::Stopped
        } else {
            WorkerLifecycle::Disabled
        };
        Arc::new(Self {
            config,
            sink,
            shared: Arc::new(SharedState {
                status: Mutex::new(SchedulerStatus {
                    lifecycle,
                    ticks: 0,
                    enqueued: 0,
                    duplicates: 0,
                    failures: 0,
                    skipped_disabled: 0,
                    last_suppressed_reason: None,
                    last_error: None,
                }),
                keys: Mutex::new(BoundedKeys {
                    set: HashSet::new(),
                    order: VecDeque::new(),
                }),
            }),
        })
    }

    pub fn status(&self) -> SchedulerStatus {
        lock(&self.shared.status).clone()
    }

    pub fn process_tick(&self, tick: SchedulerTick) -> TickOutcome {
        if !self.config.is_enabled() {
            let mut status = lock(&self.shared.status);
            status.skipped_disabled = status.skipped_disabled.saturating_add(1);
            return TickOutcome {
                enqueued: 0,
                duplicates: 0,
                failures: 0,
                template_generated: false,
                suppressed_reason: Some("disabled".to_string()),
            };
        }
        {
            let mut status = lock(&self.shared.status);
            status.ticks = status.ticks.saturating_add(1);
        }

        let result = evaluate(tick.request());
        let mut messages = result
            .plans()
            .iter()
            .take(self.config.max_enqueues_per_tick())
            .map(|plan| {
                ScheduledMessage::new(
                    MailboxItemKind::ProactiveMessage,
                    "YunXi follow-up",
                    plan.message(),
                    plan.reason(),
                    format!(
                        "proactive:{}:{}:{}",
                        tick.idempotency_scope(),
                        trigger_name(plan.trigger()),
                        plan.reason()
                    ),
                )
            })
            .collect::<Vec<_>>();
        let mut template_generated = false;
        if messages.is_empty()
            && matches!(result.suppressed_reason(), None | Some("no_signal"))
            && self.config.template_fallback_enabled()
            && let Some(context) = tick.template()
        {
            let template = render_love_letter(context);
            messages.push(ScheduledMessage::new(
                template.kind(),
                template.subject(),
                template.content(),
                "deterministic_local_template",
                format!("love-letter:{}", tick.idempotency_scope()),
            ));
            template_generated = true;
        }

        let mut outcome = TickOutcome {
            enqueued: 0,
            duplicates: 0,
            failures: 0,
            template_generated,
            suppressed_reason: result.suppressed_reason().map(str::to_string),
        };
        for message in messages {
            if lock(&self.shared.keys).contains(message.idempotency_key()) {
                outcome.duplicates += 1;
                lock(&self.shared.status).duplicates += 1;
                continue;
            }
            let enqueue = catch_unwind(AssertUnwindSafe(|| self.sink.enqueue(&message)));
            match enqueue {
                Ok(Ok(EnqueueDisposition::Created)) => {
                    lock(&self.shared.keys).insert(message.idempotency_key().to_string());
                    outcome.enqueued += 1;
                    lock(&self.shared.status).enqueued += 1;
                }
                Ok(Ok(EnqueueDisposition::AlreadyPresent)) => {
                    lock(&self.shared.keys).insert(message.idempotency_key().to_string());
                    outcome.duplicates += 1;
                    lock(&self.shared.status).duplicates += 1;
                }
                Ok(Err(error)) => {
                    outcome.failures += 1;
                    record_failure(&self.shared, error.to_string());
                }
                Err(_) => {
                    outcome.failures += 1;
                    record_failure(&self.shared, "proactive sink panicked".to_string());
                }
            }
        }
        lock(&self.shared.status).last_suppressed_reason = outcome.suppressed_reason.clone();
        outcome
    }

    pub fn start<T>(self: &Arc<Self>, source: T) -> SchedulerHandle
    where
        T: TickSource,
    {
        if !self.config.is_enabled() {
            let erased: Arc<dyn StatusFacade> = self.clone();
            return SchedulerHandle {
                facade: erased,
                control: Arc::new(WorkerControl::new()),
                join: Mutex::new(None),
            };
        }
        lock(&self.shared.status).lifecycle = WorkerLifecycle::Running;
        let control = Arc::new(WorkerControl::new());
        let worker_control = Arc::clone(&control);
        let facade = Arc::clone(self);
        let interval = self.config.interval();
        let join = thread::Builder::new()
            .name("yunxi-scheduler".to_string())
            .spawn(move || worker_loop(facade, source, worker_control, interval))
            .map(Some);
        let join = match join {
            Ok(join) => join,
            Err(error) => {
                record_failure(
                    &self.shared,
                    format!("scheduler worker could not start: {error}"),
                );
                lock(&self.shared.status).lifecycle = WorkerLifecycle::Stopped;
                None
            }
        };
        let erased: Arc<dyn StatusFacade> = self.clone();
        SchedulerHandle {
            facade: erased,
            control,
            join: Mutex::new(join),
        }
    }
}

pub struct SchedulerHandle {
    facade: Arc<dyn StatusFacade>,
    control: Arc<WorkerControl>,
    join: Mutex<Option<JoinHandle<()>>>,
}

trait StatusFacade: Send + Sync {
    fn status_erased(&self) -> SchedulerStatus;
    fn mark_stopping(&self);
    fn mark_stopped(&self);
}

impl<S> StatusFacade for SchedulerFacade<S>
where
    S: ProactiveSink,
{
    fn status_erased(&self) -> SchedulerStatus {
        self.status()
    }

    fn mark_stopping(&self) {
        let mut status = lock(&self.shared.status);
        if status.lifecycle == WorkerLifecycle::Running {
            status.lifecycle = WorkerLifecycle::Stopping;
        }
    }

    fn mark_stopped(&self) {
        let mut status = lock(&self.shared.status);
        if status.lifecycle != WorkerLifecycle::Disabled {
            status.lifecycle = WorkerLifecycle::Stopped;
        }
    }
}

impl SchedulerHandle {
    pub fn status(&self) -> SchedulerStatus {
        self.facade.status_erased()
    }

    pub fn cancel(&self) {
        self.facade.mark_stopping();
        {
            let mut stop = lock(&self.control.stop);
            *stop = true;
            self.control.wake.notify_one();
        }
        let join = lock(&self.join).take();
        if let Some(join) = join {
            let _ = join.join();
        }
        self.facade.mark_stopped();
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct WorkerControl {
    stop: Mutex<bool>,
    wake: Condvar,
}

impl WorkerControl {
    fn new() -> Self {
        Self {
            stop: Mutex::new(false),
            wake: Condvar::new(),
        }
    }
}

fn worker_loop<S, T>(
    facade: Arc<SchedulerFacade<S>>,
    source: T,
    control: Arc<WorkerControl>,
    interval: Duration,
) where
    S: ProactiveSink,
    T: TickSource,
{
    loop {
        if *lock(&control.stop) {
            break;
        }
        let next_tick = catch_unwind(AssertUnwindSafe(|| source.next_tick()));
        match next_tick {
            Ok(Ok(Some(tick))) => {
                facade.process_tick(tick);
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => record_failure(&facade.shared, compact(&error, 1_024)),
            Err(_) => record_failure(&facade.shared, "tick source panicked".to_string()),
        }
        let stop = lock(&control.stop);
        if *stop {
            break;
        }
        let (stop, _) = control
            .wake
            .wait_timeout(stop, interval)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *stop {
            break;
        }
    }
}

fn record_failure(shared: &SharedState, message: String) {
    let mut status = lock(&shared.status);
    status.failures = status.failures.saturating_add(1);
    status.last_error = Some(compact(&message, 1_024));
}

fn trigger_name(trigger: ProactiveTrigger) -> &'static str {
    match trigger {
        ProactiveTrigger::ReminderDue => "reminder_due",
        ProactiveTrigger::UnfinishedTask => "unfinished_task",
        ProactiveTrigger::LongIdleCheckIn => "long_idle_check_in",
        ProactiveTrigger::TopicContinuation => "topic_continuation",
        ProactiveTrigger::PeriodicSummary => "periodic_summary",
        ProactiveTrigger::RelationshipMilestone => "relationship_milestone",
    }
}

fn compact(value: &str, maximum: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn reminder(scope: &str) -> SchedulerTick {
        SchedulerTick::new(
            ProactiveSchedulerRequest::new(12 * 60).with_reminder_due(true),
            scope,
        )
    }

    #[test]
    fn disabled_facade_does_not_call_sink() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sink_calls = Arc::clone(&calls);
        let facade = SchedulerFacade::new(
            SchedulerConfig::new(),
            move |_message: &ScheduledMessage| -> Result<EnqueueDisposition, SchedulerSinkError> {
                sink_calls.fetch_add(1, Ordering::SeqCst);
                Ok(EnqueueDisposition::Created)
            },
        );
        let outcome = facade.process_tick(reminder("disabled"));
        assert_eq!(outcome.suppressed_reason(), Some("disabled"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(facade.status().lifecycle(), WorkerLifecycle::Disabled);
    }

    #[test]
    fn repeated_ticks_are_idempotent_and_quiet_hours_suppress() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sink_calls = Arc::clone(&calls);
        let facade = SchedulerFacade::new(
            SchedulerConfig::new().enabled(true),
            move |_message: &ScheduledMessage| -> Result<EnqueueDisposition, SchedulerSinkError> {
                sink_calls.fetch_add(1, Ordering::SeqCst);
                Ok(EnqueueDisposition::Created)
            },
        );
        let first = facade.process_tick(reminder("same"));
        let second = facade.process_tick(reminder("same"));
        assert_eq!(first.enqueued(), 1);
        assert_eq!(second.duplicates(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let quiet = SchedulerTick::new(
            ProactiveSchedulerRequest::new(23 * 60)
                .with_reminder_due(true)
                .with_quiet_hours(yunxi_protocol::QuietHours::new(22 * 60, 7 * 60).unwrap()),
            "quiet",
        );
        assert_eq!(
            facade.process_tick(quiet).suppressed_reason(),
            Some("quiet_hours")
        );
    }

    #[test]
    fn sink_failure_is_isolated_and_worker_cancels_promptly() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sink_calls = Arc::clone(&calls);
        let facade = SchedulerFacade::new(
            SchedulerConfig::new()
                .enabled(true)
                .with_interval(Duration::from_secs(60)),
            move |_message: &ScheduledMessage| -> Result<EnqueueDisposition, SchedulerSinkError> {
                sink_calls.fetch_add(1, Ordering::SeqCst);
                Err(SchedulerSinkError::new("sink unavailable"))
            },
        );
        let source = || Ok(Some(reminder("worker")));
        let handle = facade.start(source);
        std::thread::sleep(Duration::from_millis(20));
        handle.cancel();
        let status = handle.status();
        assert_eq!(status.lifecycle(), WorkerLifecycle::Stopped);
        assert!(status.failures() >= 1);
        assert!(calls.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn deterministic_template_is_used_only_when_explicitly_enabled() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink_messages = Arc::clone(&messages);
        let facade = SchedulerFacade::new(
            SchedulerConfig::new()
                .enabled(true)
                .with_template_fallback(true),
            move |message: &ScheduledMessage| {
                lock(&sink_messages).push(message.clone());
                Ok(EnqueueDisposition::Created)
            },
        );
        let tick = SchedulerTick::new(ProactiveSchedulerRequest::new(12 * 60), "template")
            .with_template(TemplateContext::new("小云", "下一步"));
        let outcome = facade.process_tick(tick);
        assert_eq!(outcome.enqueued(), 1);
        assert!(outcome.template_generated());
        assert_eq!(lock(&messages)[0].kind(), MailboxItemKind::LoveLetter);
    }
}
