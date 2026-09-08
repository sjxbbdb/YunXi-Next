//! Asynchronous, isolated child-worker execution over the persisted graph.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker, Waker as TaskWaker};
use std::thread;

use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    AgentDelegationGrant, AgentInspectRequest, AgentInterruptRequest, AgentMutationResult,
    AgentSnapshot, AgentStatus, AgentTranscriptEntry, AgentTurnCompleteRequest,
    AgentTurnFailRequest, AgentTurnStartRequest, AgentTurnStartResult, GrantKind,
};

use crate::{CoordinatorStore, MultiAgentStoreError};

pub const MAX_RUNTIME_EVENTS: usize = 128;
const MAX_MODEL_ID_BYTES: usize = 128;
const MAX_RUNTIME_ERROR_BYTES: usize = 4096;
const MAX_RUNTIME_EVENT_DETAIL_BYTES: usize = 4096;
const MAX_CANCELLATION_WAITERS: usize = 64;
const MAX_COMPLETION_WAITERS: usize = 64;
const MAX_RUNTIME_FAILURE_MESSAGE_BYTES: usize = MAX_RUNTIME_ERROR_BYTES - 66;

/// The executable tools that the baseline child runtime may expose.
///
/// This is intentionally a typed mapping from grants to tools. A persisted
/// child grant cannot turn into an unrelated model tool by name alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildToolKind {
    WorkspaceSearch,
    WorkspaceRead,
    WorkspacePatch,
}

/// The controlled executable tool directory derived from one child's grants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildToolCatalog {
    tools: Vec<ChildToolKind>,
}

impl ChildToolCatalog {
    pub fn from_grants(grants: &[GrantKind]) -> Result<Self, RuntimeError> {
        let read = grants.contains(&GrantKind::WorkspaceRead);
        let write = grants.contains(&GrantKind::WorkspaceWrite);
        if grants
            .iter()
            .any(|grant| !matches!(grant, GrantKind::WorkspaceRead | GrantKind::WorkspaceWrite))
        {
            return Err(RuntimeError::InvalidSpec(
                "child grants contain no executable tool mapping".to_string(),
            ));
        }
        if write && !read {
            return Err(RuntimeError::InvalidSpec(
                "workspace_write requires workspace_read".to_string(),
            ));
        }
        let mut tools = Vec::new();
        if read {
            tools.extend([ChildToolKind::WorkspaceSearch, ChildToolKind::WorkspaceRead]);
        }
        if write {
            tools.push(ChildToolKind::WorkspacePatch);
        }
        Ok(Self { tools })
    }

    pub fn tools(&self) -> &[ChildToolKind] {
        &self.tools
    }

    pub fn allows(&self, tool: ChildToolKind) -> bool {
        self.tools.contains(&tool)
    }
}

/// The immutable execution policy supplied to one child worker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildWorkerSpec {
    model: String,
    tool_grants: Vec<GrantKind>,
}

impl ChildWorkerSpec {
    pub fn new(
        model: impl Into<String>,
        tool_grants: impl IntoIterator<Item = GrantKind>,
    ) -> Result<Self, RuntimeError> {
        let spec = Self {
            model: model.into(),
            tool_grants: tool_grants.into_iter().collect(),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn tool_grants(&self) -> &[GrantKind] {
        &self.tool_grants
    }

    /// Builds a worker policy only when the parent authority explicitly
    /// permits every requested executable grant.
    pub fn from_parent_grant(
        model: impl Into<String>,
        parent: &AgentDelegationGrant,
        tool_grants: impl IntoIterator<Item = GrantKind>,
    ) -> Result<Self, RuntimeError> {
        let spec = Self::new(model, tool_grants)?;
        if !parent.permits(&spec.tool_grants) {
            return Err(RuntimeError::InvalidSpec(
                "worker tool grants exceed the parent delegation grant".to_string(),
            ));
        }
        Ok(spec)
    }

    fn validate(&self) -> Result<(), RuntimeError> {
        if self.model.is_empty()
            || self.model.len() > MAX_MODEL_ID_BYTES
            || !self.model.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
            })
        {
            return Err(RuntimeError::InvalidSpec(
                "model id must be a bounded ASCII token".to_string(),
            ));
        }
        if self.tool_grants.len() > 8
            || self
                .tool_grants
                .iter()
                .enumerate()
                .any(|(index, grant)| self.tool_grants[..index].contains(grant))
        {
            return Err(RuntimeError::InvalidSpec(
                "tool grants must be unique and bounded".to_string(),
            ));
        }
        ChildToolCatalog::from_grants(&self.tool_grants)?;
        Ok(())
    }
}

/// Input delivered to a model/tool adapter for one turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildTurn {
    pub agent_id: String,
    pub session_id: String,
    pub message: String,
    pub transcript: Vec<AgentTranscriptEntry>,
    pub model: String,
    pub tool_grants: Vec<GrantKind>,
    pub tool_catalog: ChildToolCatalog,
}

/// A bounded, non-secret worker error that can be persisted as a branch failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildWorkerError {
    code: String,
    message: String,
}

impl ChildWorkerError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Result<Self, RuntimeError> {
        let error = Self {
            code: code.into(),
            message: message.into(),
        };
        if error.code.is_empty()
            || error.code.len() > 64
            || !error.code.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
            || error.message.len() > MAX_RUNTIME_ERROR_BYTES
        {
            return Err(RuntimeError::InvalidSpec(
                "worker error code or message exceeds its bound".to_string(),
            ));
        }
        Ok(error)
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ChildWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for ChildWorkerError {}

pub type ChildWorkerFuture =
    Pin<Box<dyn Future<Output = Result<String, ChildWorkerError>> + Send + 'static>>;

/// Adapter boundary for a model plugin or another child-turn implementation.
pub trait ChildExecutor: Send + Sync + 'static {
    fn execute(&self, turn: ChildTurn, cancellation: CancellationToken) -> ChildWorkerFuture;
}

impl<F, Fut> ChildExecutor for F
where
    F: Fn(ChildTurn, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, ChildWorkerError>> + Send + 'static,
{
    fn execute(&self, turn: ChildTurn, cancellation: CancellationToken) -> ChildWorkerFuture {
        Box::pin(self(turn, cancellation))
    }
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    waiters: Mutex<Vec<Waker>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                waiters: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn cancel(&self) -> bool {
        if self.state.cancelled.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Ok(mut waiters) = self.state.waiters.lock() {
            for waiter in waiters.drain(..) {
                waiter.wake();
            }
        }
        true
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub fn cancelled(&self) -> CancellationFuture {
        CancellationFuture {
            token: self.clone(),
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct CancellationFuture {
    token: CancellationToken,
}

impl Future for CancellationFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        if let Ok(mut waiters) = self.token.state.waiters.lock() {
            if !waiters
                .iter()
                .any(|waiter| waiter.will_wake(context.waker()))
                && waiters.len() < MAX_CANCELLATION_WAITERS
            {
                waiters.push(context.waker().clone());
            }
        }
        if self.token.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEventKind {
    Spawned,
    Started,
    Resumed,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEvent {
    sequence: u64,
    agent_id: String,
    kind: WorkerEventKind,
    detail: String,
}

impl WorkerEvent {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub const fn kind(&self) -> WorkerEventKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerProjection {
    agent: AgentSnapshot,
    model: String,
    tool_grants: Vec<GrantKind>,
    cancel_requested: bool,
}

impl WorkerProjection {
    pub fn agent(&self) -> &AgentSnapshot {
        &self.agent
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn tool_grants(&self) -> &[GrantKind] {
        &self.tool_grants
    }

    pub const fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjection {
    workers: Vec<WorkerProjection>,
    events: Vec<WorkerEvent>,
    events_truncated: bool,
}

impl RuntimeProjection {
    pub fn workers(&self) -> &[WorkerProjection] {
        &self.workers
    }

    pub fn events(&self) -> &[WorkerEvent] {
        &self.events
    }

    pub const fn events_truncated(&self) -> bool {
        self.events_truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerOutcome {
    Completed { agent_id: String, reply: String },
    Cancelled { agent_id: String },
}

#[derive(Clone)]
pub struct WorkerHandle {
    agent_id: String,
    cancellation: CancellationToken,
    completion: Arc<CompletionCell>,
    runtime: Arc<RuntimeInner>,
}

impl fmt::Debug for WorkerHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerHandle")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

impl WorkerHandle {
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn cancel(&self) -> bool {
        self.cancellation.cancel()
    }

    pub fn interrupt(
        &self,
        authority: &AgentDelegationGrant,
    ) -> Result<AgentMutationResult, RuntimeError> {
        AsyncMultiAgentRuntime {
            inner: self.runtime.clone(),
        }
        .interrupt(
            &AgentInterruptRequest::new(authority.clone(), self.agent_id.clone(), false)
                .map_err(|error| RuntimeError::InvalidSpec(error.to_string()))?,
        )
    }

    pub fn is_finished(&self) -> bool {
        self.completion.is_finished()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub async fn wait(&self) -> Result<WorkerOutcome, ChildWorkerError> {
        CompletionFuture {
            completion: self.completion.clone(),
        }
        .await
    }

    pub fn wait_blocking(&self) -> Result<WorkerOutcome, ChildWorkerError> {
        block_on(self.wait())
    }
}

#[derive(Clone)]
pub struct WorkerPlan {
    pub request: yunxi_protocol::AgentSpawnRequest,
    pub spec: ChildWorkerSpec,
}

impl WorkerPlan {
    pub fn new(request: yunxi_protocol::AgentSpawnRequest, spec: ChildWorkerSpec) -> Self {
        Self { request, spec }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerRecoveryOrigin {
    Running,
    Queued,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerRecovery {
    agent: AgentSnapshot,
    transcript: Vec<AgentTranscriptEntry>,
    model: Option<String>,
    origin: WorkerRecoveryOrigin,
}

impl WorkerRecovery {
    pub fn agent(&self) -> &AgentSnapshot {
        &self.agent
    }

    pub fn agent_id(&self) -> &str {
        self.agent.id()
    }

    pub fn transcript(&self) -> &[AgentTranscriptEntry] {
        &self.transcript
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub const fn origin(&self) -> WorkerRecoveryOrigin {
        self.origin
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerRecoveryPlan {
    pub recovery: WorkerRecovery,
    pub spec: ChildWorkerSpec,
}

impl WorkerRecoveryPlan {
    pub fn new(recovery: WorkerRecovery, spec: ChildWorkerSpec) -> Self {
        Self { recovery, spec }
    }
}

#[derive(Clone)]
pub struct AsyncMultiAgentRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    store: CoordinatorStore,
    workers: Mutex<BTreeMap<String, WorkerControl>>,
    events: Mutex<EventLog>,
    next_event: AtomicU64,
}

struct WorkerControl {
    spec: ChildWorkerSpec,
    cancellation: CancellationToken,
    completion: Arc<CompletionCell>,
}

impl WorkerControl {
    fn handle(&self, agent_id: &str, runtime: Arc<RuntimeInner>) -> WorkerHandle {
        WorkerHandle {
            agent_id: agent_id.to_string(),
            cancellation: self.cancellation.clone(),
            completion: self.completion.clone(),
            runtime,
        }
    }
}

#[derive(Debug)]
struct EventLog {
    items: std::collections::VecDeque<WorkerEvent>,
    truncated: bool,
}

impl AsyncMultiAgentRuntime {
    pub fn new(store: CoordinatorStore) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                store,
                workers: Mutex::new(BTreeMap::new()),
                events: Mutex::new(EventLog {
                    items: std::collections::VecDeque::with_capacity(MAX_RUNTIME_EVENTS),
                    truncated: false,
                }),
                next_event: AtomicU64::new(1),
            }),
        }
    }

    pub fn store(&self) -> &CoordinatorStore {
        &self.inner.store
    }

    pub fn spawn_worker(
        &self,
        request: yunxi_protocol::AgentSpawnRequest,
        spec: ChildWorkerSpec,
        executor: Arc<dyn ChildExecutor>,
    ) -> Result<WorkerHandle, RuntimeError> {
        let spec = ChildWorkerSpec::from_parent_grant(
            spec.model(),
            request.grant(),
            spec.tool_grants().iter().copied(),
        )?;
        if !spec
            .tool_grants()
            .iter()
            .all(|grant| request.requested_child_grants().contains(grant))
        {
            return Err(RuntimeError::InvalidSpec(
                "worker tool grants must be requested by the child".to_string(),
            ));
        }
        let spawned = self.inner.store.spawn(&request)?;
        if !spec
            .tool_grants()
            .iter()
            .all(|grant| spawned.agent().child_grants().contains(grant))
        {
            return Err(RuntimeError::InvalidSpec(
                "worker tool grants exceed the child grant".to_string(),
            ));
        }
        self.inner
            .store
            .set_worker_model(spawned.agent().id(), spec.model())?;
        self.emit(
            spawned.agent().id(),
            WorkerEventKind::Spawned,
            "child worker spawned",
        );
        self.launch_turn(
            spawned.agent().id().to_string(),
            request.task().to_string(),
            spec,
            executor,
            false,
        )
    }

    pub fn spawn_parallel(
        &self,
        plans: impl IntoIterator<Item = WorkerPlan>,
        executor: Arc<dyn ChildExecutor>,
    ) -> Vec<Result<WorkerHandle, RuntimeError>> {
        plans
            .into_iter()
            .map(|plan| self.spawn_worker(plan.request, plan.spec, executor.clone()))
            .collect()
    }

    pub fn resume_worker(
        &self,
        authority: &AgentDelegationGrant,
        agent_id: impl Into<String>,
        message: impl Into<String>,
        spec: ChildWorkerSpec,
        executor: Arc<dyn ChildExecutor>,
    ) -> Result<WorkerHandle, RuntimeError> {
        let spec = ChildWorkerSpec::from_parent_grant(
            spec.model(),
            authority,
            spec.tool_grants().iter().copied(),
        )?;
        let agent_id = agent_id.into();
        let inspected = self.inner.store.inspect(
            &AgentInspectRequest::new(authority.clone(), agent_id.clone())
                .map_err(|error| RuntimeError::InvalidSpec(error.to_string()))?,
        )?;
        if !spec
            .tool_grants()
            .iter()
            .all(|grant| inspected.agent().child_grants().contains(grant))
        {
            return Err(RuntimeError::InvalidSpec(
                "resumed worker tool grants exceed the child grant".to_string(),
            ));
        }
        self.inner.store.set_worker_model(&agent_id, spec.model())?;
        self.launch_turn(agent_id, message.into(), spec, executor, true)
    }

    pub fn recover_workers(
        &self,
    ) -> Result<Vec<Result<WorkerRecovery, RuntimeError>>, RuntimeError> {
        let list = self.inner.store.list()?;
        let active_workers = {
            let workers = self
                .inner
                .workers
                .lock()
                .map_err(|_| RuntimeError::RuntimePoisoned)?;
            workers
                .iter()
                .filter(|(_, worker)| !worker.completion.is_finished())
                .map(|(agent_id, _)| agent_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        };

        let mut recovered = Vec::new();
        for agent in list.agents() {
            if active_workers.contains(agent.id()) {
                continue;
            }
            match agent.status() {
                AgentStatus::Running => {
                    let result = self
                        .inner
                        .store
                        .requeue_running_turn(agent.id())
                        .map_err(RuntimeError::from)
                        .and_then(|inspection| {
                            let model = self
                                .inner
                                .store
                                .worker_model(agent.id())
                                .map_err(RuntimeError::from)?;
                            Ok(WorkerRecovery {
                                agent: inspection.agent().clone(),
                                transcript: inspection.transcript().to_vec(),
                                model,
                                origin: WorkerRecoveryOrigin::Running,
                            })
                        });
                    recovered.push(result);
                }
                AgentStatus::Pending => {
                    let request = AgentInspectRequest::new(
                        self.inner.store_authority(),
                        agent.id().to_string(),
                    )
                    .map_err(|error| RuntimeError::InvalidSpec(error.to_string()))?;
                    let inspection = match self
                        .inner
                        .store
                        .inspect(&request)
                        .map_err(RuntimeError::from)
                    {
                        Ok(inspection) => inspection,
                        Err(error) => {
                            recovered.push(Err(error));
                            continue;
                        }
                    };
                    if !inspection.transcript().last().is_some_and(|entry| {
                        entry.role() == yunxi_protocol::AgentTranscriptRole::User
                    }) {
                        continue;
                    }
                    let model = match self
                        .inner
                        .store
                        .worker_model(agent.id())
                        .map_err(RuntimeError::from)
                    {
                        Ok(model) => model,
                        Err(error) => {
                            recovered.push(Err(error));
                            continue;
                        }
                    };
                    recovered.push(Ok(WorkerRecovery {
                        agent: inspection.agent().clone(),
                        transcript: inspection.transcript().to_vec(),
                        model,
                        origin: WorkerRecoveryOrigin::Queued,
                    }));
                }
                _ => {}
            }
        }
        Ok(recovered)
    }

    pub fn resume_recovered_worker(
        &self,
        plan: WorkerRecoveryPlan,
        executor: Arc<dyn ChildExecutor>,
    ) -> Result<WorkerHandle, RuntimeError> {
        let WorkerRecoveryPlan { recovery, spec } = plan;
        let agent_id = recovery.agent_id().to_string();
        let spec = ChildWorkerSpec::from_parent_grant(
            spec.model(),
            &self.inner.store_authority(),
            spec.tool_grants().iter().copied(),
        )?;
        if !spec
            .tool_grants()
            .iter()
            .all(|grant| recovery.agent().child_grants().contains(grant))
        {
            return Err(RuntimeError::InvalidSpec(
                "recovered worker tool grants exceed the child grant".to_string(),
            ));
        }

        {
            let workers = self
                .inner
                .workers
                .lock()
                .map_err(|_| RuntimeError::RuntimePoisoned)?;
            if let Some(worker) = workers.get(&agent_id) {
                if !worker.completion.is_finished() {
                    if worker.spec != spec {
                        return Err(RuntimeError::InvalidSpec(
                            "recovered worker spec does not match the active worker".to_string(),
                        ));
                    }
                    return Ok(worker.handle(&agent_id, self.inner.clone()));
                }
            }
        }

        self.inner.store.set_worker_model(&agent_id, spec.model())?;
        self.launch_recovered_turn(agent_id, spec, executor, recovery.origin())
    }

    pub fn resume_recovered_workers(
        &self,
        plans: impl IntoIterator<Item = WorkerRecoveryPlan>,
        executor: Arc<dyn ChildExecutor>,
    ) -> Vec<Result<WorkerHandle, RuntimeError>> {
        plans
            .into_iter()
            .map(|plan| self.resume_recovered_worker(plan, executor.clone()))
            .collect()
    }

    pub fn interrupt(
        &self,
        request: &AgentInterruptRequest,
    ) -> Result<AgentMutationResult, RuntimeError> {
        request
            .validate()
            .map_err(|error| RuntimeError::InvalidSpec(error.to_string()))?;
        self.inner.store.validate_authority(request.grant())?;
        let list = self.inner.store.list()?;
        let mut targets = vec![request.agent_id().to_string()];
        if request.recursive() {
            let mut cursor = 0;
            while cursor < targets.len() {
                let parent = targets[cursor].clone();
                targets.extend(
                    list.agents()
                        .iter()
                        .filter(|agent| agent.parent_id() == parent)
                        .map(|agent| agent.id().to_string()),
                );
                cursor += 1;
            }
        }
        let workers = self
            .inner
            .workers
            .lock()
            .map_err(|_| RuntimeError::RuntimePoisoned)?;
        for target in &targets {
            if let Some(worker) = workers.get(target) {
                worker.cancellation.cancel();
                self.emit(
                    target,
                    WorkerEventKind::CancelRequested,
                    "cancellation requested",
                );
            }
        }
        drop(workers);
        let mutation = self.inner.store.interrupt(request)?;
        for agent in mutation.agents() {
            self.emit(
                agent.id(),
                WorkerEventKind::Cancelled,
                "child branch interrupted",
            );
        }
        Ok(mutation)
    }

    pub fn projection(&self) -> Result<RuntimeProjection, RuntimeError> {
        let list = self.inner.store.list()?;
        let workers = self
            .inner
            .workers
            .lock()
            .map_err(|_| RuntimeError::RuntimePoisoned)?;
        let projected = list
            .agents()
            .iter()
            .filter_map(|agent| {
                workers.get(agent.id()).map(|worker| WorkerProjection {
                    agent: agent.clone(),
                    model: worker.spec.model().to_string(),
                    tool_grants: worker.spec.tool_grants().to_vec(),
                    cancel_requested: worker.cancellation.is_cancelled()
                        && agent.status() == AgentStatus::Running,
                })
            })
            .collect();
        let events = self
            .inner
            .events
            .lock()
            .map_err(|_| RuntimeError::RuntimePoisoned)?;
        Ok(RuntimeProjection {
            workers: projected,
            events: events.items.iter().cloned().collect(),
            events_truncated: events.truncated,
        })
    }

    fn launch_turn(
        &self,
        agent_id: String,
        message: String,
        spec: ChildWorkerSpec,
        executor: Arc<dyn ChildExecutor>,
        resumed: bool,
    ) -> Result<WorkerHandle, RuntimeError> {
        {
            let workers = self
                .inner
                .workers
                .lock()
                .map_err(|_| RuntimeError::RuntimePoisoned)?;
            if workers
                .get(&agent_id)
                .is_some_and(|worker| !worker.completion.is_finished())
            {
                return Err(RuntimeError::WorkerAlreadyRunning(agent_id));
            }
        }
        let request =
            AgentTurnStartRequest::new(self.inner.store_authority(), agent_id.clone(), message)
                .map_err(|error| RuntimeError::InvalidSpec(error.to_string()))?;
        let started = self.inner.store.start_turn(&request)?;
        self.launch_started_worker(
            agent_id,
            started,
            spec,
            executor,
            if resumed {
                WorkerEventKind::Resumed
            } else {
                WorkerEventKind::Started
            },
            if resumed {
                "resumed child turn started"
            } else {
                "child turn started"
            },
        )
    }

    fn launch_recovered_turn(
        &self,
        agent_id: String,
        spec: ChildWorkerSpec,
        executor: Arc<dyn ChildExecutor>,
        origin: WorkerRecoveryOrigin,
    ) -> Result<WorkerHandle, RuntimeError> {
        let started = self.inner.store.start_recovered_turn(&agent_id)?;
        let detail = match origin {
            WorkerRecoveryOrigin::Running => {
                "recovered child turn resumed after coordinator restart"
            }
            WorkerRecoveryOrigin::Queued => "queued child turn resumed after coordinator restart",
        };
        self.launch_started_worker(
            agent_id,
            started,
            spec,
            executor,
            WorkerEventKind::Resumed,
            detail,
        )
    }

    fn launch_started_worker(
        &self,
        agent_id: String,
        started: AgentTurnStartResult,
        spec: ChildWorkerSpec,
        executor: Arc<dyn ChildExecutor>,
        event_kind: WorkerEventKind,
        event_detail: &'static str,
    ) -> Result<WorkerHandle, RuntimeError> {
        let transcript = started.transcript().to_vec();
        let message = transcript
            .last()
            .map(|entry| entry.content().to_string())
            .unwrap_or_default();
        let cancellation = CancellationToken::new();
        let tool_catalog = ChildToolCatalog::from_grants(spec.tool_grants())?;
        let completion = Arc::new(CompletionCell::new());
        {
            let mut workers = self
                .inner
                .workers
                .lock()
                .map_err(|_| RuntimeError::RuntimePoisoned)?;
            workers.insert(
                agent_id.clone(),
                WorkerControl {
                    spec: spec.clone(),
                    cancellation: cancellation.clone(),
                    completion: completion.clone(),
                },
            );
        }
        self.emit(&agent_id, event_kind, event_detail);
        let inner = self.inner.clone();
        let worker_id = agent_id.clone();
        let worker_cancellation = cancellation.clone();
        let worker_completion = completion.clone();
        let worker_spec = spec.clone();
        let spawned_thread = thread::Builder::new()
            .name(format!("yunxi-child-{worker_id}"))
            .spawn(move || {
                let turn = ChildTurn {
                    agent_id: worker_id.clone(),
                    session_id: inner.store_authority().session_id().to_string(),
                    message,
                    transcript,
                    model: worker_spec.model().to_string(),
                    tool_grants: worker_spec.tool_grants().to_vec(),
                    tool_catalog,
                };
                let execution = panic::catch_unwind(AssertUnwindSafe(|| {
                    block_on(executor.execute(turn, worker_cancellation.clone()))
                }));
                complete_worker(
                    &inner,
                    &worker_id,
                    worker_cancellation,
                    worker_completion,
                    execution,
                );
            });
        if let Err(error) = spawned_thread {
            if let Ok(mut workers) = self.inner.workers.lock() {
                workers.remove(&agent_id);
            }
            let failure = ChildWorkerError::unchecked(
                "worker_thread_spawn",
                format!("cannot start child worker: {error}"),
            );
            let _ = persist_worker_failure(&self.inner, &agent_id, &failure);
            self.emit(&agent_id, WorkerEventKind::Failed, failure.message());
            completion.complete(Err(failure));
            return Err(RuntimeError::ThreadSpawn(error.to_string()));
        }
        Ok(WorkerHandle {
            agent_id,
            cancellation,
            completion,
            runtime: self.inner.clone(),
        })
    }

    fn emit(&self, agent_id: &str, kind: WorkerEventKind, detail: &str) {
        self.inner.emit(agent_id, kind, detail);
    }
}

impl RuntimeInner {
    fn store_authority(&self) -> AgentDelegationGrant {
        self.store.authority_for_runtime()
    }

    fn emit(&self, agent_id: &str, kind: WorkerEventKind, detail: &str) {
        let sequence = self.next_event.fetch_add(1, Ordering::Relaxed);
        let event = WorkerEvent {
            sequence,
            agent_id: agent_id.to_string(),
            kind,
            detail: bounded_text(detail, MAX_RUNTIME_EVENT_DETAIL_BYTES),
        };
        if let Ok(mut events) = self.events.lock() {
            if events.items.len() == MAX_RUNTIME_EVENTS {
                events.items.pop_front();
                events.truncated = true;
            }
            events.items.push_back(event);
        }
    }
}

fn complete_worker(
    inner: &Arc<RuntimeInner>,
    agent_id: &str,
    cancellation: CancellationToken,
    completion: Arc<CompletionCell>,
    execution: Result<Result<String, ChildWorkerError>, Box<dyn std::any::Any + Send>>,
) {
    if cancellation.is_cancelled() {
        finish_cancelled(inner, agent_id, completion);
        return;
    }
    match execution {
        Ok(Ok(reply)) => {
            if cancellation.is_cancelled() {
                finish_cancelled(inner, agent_id, completion);
                return;
            }
            let result =
                AgentTurnCompleteRequest::new(inner.store_authority(), agent_id, reply.clone())
                    .map_err(|error| {
                        ChildWorkerError::unchecked("invalid_completion", error.to_string())
                    })
                    .and_then(|request| {
                        inner
                            .store
                            .complete_turn(&request)
                            .map(|_| ())
                            .map_err(|error| {
                                ChildWorkerError::unchecked("store_error", error.to_string())
                            })
                    });
            match result {
                Ok(()) => {
                    inner.emit(
                        agent_id,
                        WorkerEventKind::Completed,
                        "child worker completed",
                    );
                    completion.complete(Ok(WorkerOutcome::Completed {
                        agent_id: agent_id.to_string(),
                        reply,
                    }));
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        finish_cancelled(inner, agent_id, completion);
                        return;
                    }
                    let persisted = persist_worker_failure(inner, agent_id, &error);
                    inner.emit(agent_id, WorkerEventKind::Failed, error.message());
                    completion.complete(Err(persisted.err().unwrap_or(error)));
                }
            }
        }
        Ok(Err(error)) => {
            if cancellation.is_cancelled() {
                finish_cancelled(inner, agent_id, completion);
                return;
            }
            let persisted = persist_worker_failure(inner, agent_id, &error);
            inner.emit(agent_id, WorkerEventKind::Failed, error.message());
            completion.complete(Err(persisted.err().unwrap_or(error)));
        }
        Err(_) => {
            let error = ChildWorkerError::unchecked(
                "worker_panic",
                "child executor panicked; sibling branches remain isolated",
            );
            let _ignored = persist_worker_failure(inner, agent_id, &error);
            inner.emit(agent_id, WorkerEventKind::Failed, error.message());
            completion.complete(Err(error));
        }
    }
}

fn finish_cancelled(inner: &Arc<RuntimeInner>, agent_id: &str, completion: Arc<CompletionCell>) {
    let result = persist_worker_interruption(inner, agent_id);
    inner.emit(
        agent_id,
        WorkerEventKind::Cancelled,
        "child worker cancelled",
    );
    completion.complete(match result {
        Ok(()) => Ok(WorkerOutcome::Cancelled {
            agent_id: agent_id.to_string(),
        }),
        Err(error) => Err(error),
    });
}

fn persist_worker_failure(
    inner: &Arc<RuntimeInner>,
    agent_id: &str,
    error: &ChildWorkerError,
) -> Result<(), ChildWorkerError> {
    let request = AgentTurnFailRequest::new(
        inner.store_authority(),
        agent_id,
        error.code(),
        bounded_text(error.message(), MAX_RUNTIME_FAILURE_MESSAGE_BYTES),
    )
    .map_err(|failure| ChildWorkerError::unchecked("invalid_failure", failure.to_string()))?;
    inner
        .store
        .fail_turn(&request)
        .map(|_| ())
        .map_err(|failure| ChildWorkerError::unchecked("store_error", failure.to_string()))
}

fn persist_worker_interruption(
    inner: &Arc<RuntimeInner>,
    agent_id: &str,
) -> Result<(), ChildWorkerError> {
    let request = AgentInterruptRequest::new(inner.store_authority(), agent_id, false)
        .map_err(|error| ChildWorkerError::unchecked("invalid_interrupt", error.to_string()))?;
    inner
        .store
        .interrupt(&request)
        .map(|_| ())
        .map_err(|error| ChildWorkerError::unchecked("store_error", error.to_string()))
}

impl ChildWorkerError {
    fn unchecked(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: bounded_text(&code.into(), 64),
            message: bounded_text(&message.into(), MAX_RUNTIME_ERROR_BYTES),
        }
    }
}

fn bounded_text(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[derive(Debug)]
struct CompletionCell {
    state: Mutex<CompletionState>,
    wake: Condvar,
}

#[derive(Debug)]
struct CompletionState {
    result: Option<Result<WorkerOutcome, ChildWorkerError>>,
    waiters: Vec<Waker>,
}

impl CompletionCell {
    fn new() -> Self {
        Self {
            state: Mutex::new(CompletionState {
                result: None,
                waiters: Vec::new(),
            }),
            wake: Condvar::new(),
        }
    }

    fn is_finished(&self) -> bool {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.result.as_ref().map(|_| ()))
            .is_some()
    }

    fn complete(&self, result: Result<WorkerOutcome, ChildWorkerError>) {
        if let Ok(mut state) = self.state.lock() {
            state.result = Some(result);
            for waiter in state.waiters.drain(..) {
                waiter.wake();
            }
            self.wake.notify_all();
        }
    }
}

struct CompletionFuture {
    completion: Arc<CompletionCell>,
}

impl Future for CompletionFuture {
    type Output = Result<WorkerOutcome, ChildWorkerError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut state) = self.completion.state.lock() else {
            return Poll::Ready(Err(ChildWorkerError::unchecked(
                "runtime_poisoned",
                "worker completion state is poisoned",
            )));
        };
        if let Some(result) = &state.result {
            return Poll::Ready(result.clone());
        }
        if !state
            .waiters
            .iter()
            .any(|waiter| waiter.will_wake(context.waker()))
            && state.waiters.len() < MAX_COMPLETION_WAITERS
        {
            state.waiters.push(context.waker().clone());
        }
        Poll::Pending
    }
}

#[derive(Debug)]
struct Parker {
    state: Mutex<bool>,
    wake: Condvar,
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        if let Ok(mut ready) = self.state.lock() {
            *ready = true;
            self.wake.notify_one();
        }
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let parker = Arc::new(Parker {
        state: Mutex::new(false),
        wake: Condvar::new(),
    });
    let waker = TaskWaker::from(parker.clone());
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        let Ok(mut ready) = parker.state.lock() else {
            thread::yield_now();
            continue;
        };
        while !*ready {
            ready = parker
                .wake
                .wait(ready)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *ready = false;
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    Store(MultiAgentStoreError),
    InvalidSpec(String),
    WorkerAlreadyRunning(String),
    RuntimePoisoned,
    ThreadSpawn(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::InvalidSpec(message) => formatter.write_str(message),
            Self::WorkerAlreadyRunning(id) => write!(formatter, "worker `{id}` is already running"),
            Self::RuntimePoisoned => formatter.write_str("multi-agent runtime state is poisoned"),
            Self::ThreadSpawn(message) => write!(formatter, "cannot start child worker: {message}"),
        }
    }
}

impl Error for RuntimeError {}

impl From<MultiAgentStoreError> for RuntimeError {
    fn from(error: MultiAgentStoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn fixture_root(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{stamp}", std::process::id()))
    }

    fn grant(root: &Path) -> AgentDelegationGrant {
        AgentDelegationGrant::new(
            yunxi_protocol::WorkspaceGrant::read_write(root),
            "runtime-session",
            "runtime-ticket",
            yunxi_protocol::AgentBudget::new(4, 2, 4, 8).expect("budget"),
        )
        .expect("grant")
        .with_allowed_child_grants([GrantKind::WorkspaceRead])
        .expect("child grants")
    }

    fn runtime(name: &str) -> (PathBuf, AgentDelegationGrant, AsyncMultiAgentRuntime) {
        let root = fixture_root(name);
        fs::create_dir_all(&root).expect("workspace");
        let authority = grant(&root);
        let store = CoordinatorStore::from_grant(&authority, "runtime-instance").expect("store");
        (root, authority, AsyncMultiAgentRuntime::new(store))
    }

    fn restart_runtime(
        authority: &AgentDelegationGrant,
        instance_id: &str,
    ) -> AsyncMultiAgentRuntime {
        AsyncMultiAgentRuntime::new(
            CoordinatorStore::from_grant(authority, instance_id).expect("store"),
        )
    }

    fn seed_running_worker(
        authority: &AgentDelegationGrant,
        instance_id: &str,
        task: &str,
    ) -> (CoordinatorStore, String) {
        let store = CoordinatorStore::from_grant(authority, instance_id).expect("store");
        let spawned = store
            .spawn(
                &yunxi_protocol::AgentSpawnRequest::new(authority.clone(), task)
                    .expect("spawn")
                    .with_requested_child_grants([GrantKind::WorkspaceRead])
                    .expect("requested grants"),
            )
            .expect("spawn worker");
        let agent_id = spawned.agent().id().to_string();
        store
            .start_turn(
                &yunxi_protocol::AgentTurnStartRequest::new(authority.clone(), &agent_id, task)
                    .expect("start"),
            )
            .expect("start turn");
        (store, agent_id)
    }

    fn wait_for_started(flag: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, wake) = &**flag;
        let mut started = lock.lock().expect("started lock");
        while !*started {
            started = wake.wait(started).expect("started wait");
        }
    }

    fn request(authority: AgentDelegationGrant, task: &str) -> yunxi_protocol::AgentSpawnRequest {
        yunxi_protocol::AgentSpawnRequest::new(authority, task)
            .expect("request")
            .with_requested_child_grants([GrantKind::WorkspaceRead])
            .expect("requested grants")
    }

    struct TwoPartyRendezvous {
        arrived: Mutex<usize>,
        wake: Condvar,
    }

    impl TwoPartyRendezvous {
        fn new() -> Self {
            Self {
                arrived: Mutex::new(0),
                wake: Condvar::new(),
            }
        }

        fn wait(&self) -> bool {
            let mut arrived = self.arrived.lock().expect("rendezvous lock");
            *arrived += 1;
            if *arrived == 2 {
                self.wake.notify_all();
                return true;
            }
            let (arrived, timeout) = self
                .wake
                .wait_timeout_while(arrived, Duration::from_millis(500), |count| *count < 2)
                .expect("rendezvous wait");
            !timeout.timed_out() && *arrived == 2
        }
    }

    #[test]
    fn workers_run_in_parallel_with_bounded_elapsed_time() {
        let (root, authority, runtime) = runtime("parallel");
        let rendezvous = Arc::new(TwoPartyRendezvous::new());
        let executor: Arc<dyn ChildExecutor> = Arc::new(move |turn: ChildTurn, _token| {
            let rendezvous = rendezvous.clone();
            async move {
                if !rendezvous.wait() {
                    return Err(ChildWorkerError::new(
                        "parallel_timeout",
                        "worker did not rendezvous with its sibling",
                    )
                    .expect("error"));
                }
                thread::sleep(Duration::from_millis(40));
                Ok(turn.message)
            }
        });
        let plans = [
            WorkerPlan::new(
                request(authority.clone(), "first"),
                ChildWorkerSpec::new("model-a", [GrantKind::WorkspaceRead]).expect("spec"),
            ),
            WorkerPlan::new(
                request(authority.clone(), "second"),
                ChildWorkerSpec::new("model-b", [GrantKind::WorkspaceRead]).expect("spec"),
            ),
        ];
        let started_at = Instant::now();
        let handles = runtime.spawn_parallel(plans, executor);
        let first = handles[0].as_ref().expect("first worker").clone();
        let second = handles[1].as_ref().expect("second worker").clone();
        assert!(matches!(
            first.wait_blocking(),
            Ok(WorkerOutcome::Completed { .. })
        ));
        assert!(matches!(
            second.wait_blocking(),
            Ok(WorkerOutcome::Completed { .. })
        ));
        assert!(started_at.elapsed() < Duration::from_millis(300));
        let projection = runtime.projection().expect("projection");
        assert_eq!(projection.workers().len(), 2);
        assert!(
            projection
                .workers()
                .iter()
                .all(|worker| worker.agent().status() == AgentStatus::Completed)
        );
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn branch_failure_does_not_cancel_or_fail_sibling() {
        let (root, authority, runtime) = runtime("isolation");
        let rendezvous = Arc::new(TwoPartyRendezvous::new());
        let executor: Arc<dyn ChildExecutor> = Arc::new(move |turn: ChildTurn, _token| {
            let rendezvous = rendezvous.clone();
            async move {
                if !rendezvous.wait() {
                    return Err(
                        ChildWorkerError::new("parallel_timeout", "sibling did not start")
                            .expect("error"),
                    );
                }
                if turn.message == "fail" {
                    Err(
                        ChildWorkerError::new("expected_failure", "only this branch fails")
                            .expect("error"),
                    )
                } else {
                    Ok("sibling completed".to_string())
                }
            }
        });
        let handles = runtime.spawn_parallel(
            [
                WorkerPlan::new(
                    request(authority.clone(), "fail"),
                    ChildWorkerSpec::new("model-fail", [GrantKind::WorkspaceRead]).expect("spec"),
                ),
                WorkerPlan::new(
                    request(authority.clone(), "succeed"),
                    ChildWorkerSpec::new("model-succeed", [GrantKind::WorkspaceRead])
                        .expect("spec"),
                ),
            ],
            executor,
        );
        assert!(
            handles[0]
                .as_ref()
                .expect("failed worker")
                .wait_blocking()
                .is_err()
        );
        assert!(matches!(
            handles[1].as_ref().expect("sibling worker").wait_blocking(),
            Ok(WorkerOutcome::Completed { .. })
        ));
        let agents = runtime.store().list().expect("list").agents().to_vec();
        assert_eq!(agents[0].status(), AgentStatus::Failed);
        assert_eq!(agents[1].status(), AgentStatus::Completed);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn targeted_interrupt_wakes_cancellable_worker_immediately() {
        let (root, authority, runtime) = runtime("cancel");
        let executor: Arc<dyn ChildExecutor> =
            Arc::new(|_turn: ChildTurn, token: CancellationToken| async move {
                token.cancelled().await;
                Err(ChildWorkerError::new("cancelled", "cancelled by test").expect("error"))
            });
        let handle = runtime
            .spawn_worker(
                request(authority.clone(), "cancel me"),
                ChildWorkerSpec::new("model-c", [GrantKind::WorkspaceRead]).expect("spec"),
                executor,
            )
            .expect("worker");
        let started_at = Instant::now();
        let mutation = handle.interrupt(&authority).expect("interrupt mutation");
        assert_eq!(mutation.agents().len(), 1);
        assert!(started_at.elapsed() < Duration::from_millis(250));
        assert!(matches!(
            handle.wait_blocking(),
            Ok(WorkerOutcome::Cancelled { .. })
        ));
        assert_eq!(
            runtime.store().list().expect("list").agents()[0].status(),
            AgentStatus::Interrupted
        );
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn worker_route_receives_selected_model_and_exact_grants() {
        let (root, authority, runtime) = runtime("route");
        let observed = Arc::new(Mutex::new(None));
        let observed_by_executor = observed.clone();
        let executor: Arc<dyn ChildExecutor> = Arc::new(move |turn: ChildTurn, _token| {
            let observed = observed_by_executor.clone();
            async move {
                *observed.lock().expect("observation lock") =
                    Some((turn.model.clone(), turn.tool_grants.clone()));
                Ok("routed reply".to_string())
            }
        });
        let handle = runtime
            .spawn_worker(
                request(authority, "route me"),
                ChildWorkerSpec::new("model-special", [GrantKind::WorkspaceRead]).expect("spec"),
                executor,
            )
            .expect("worker");
        assert!(matches!(
            handle.wait_blocking(),
            Ok(WorkerOutcome::Completed { reply, .. }) if reply == "routed reply"
        ));
        assert_eq!(
            observed.lock().expect("observation lock").as_ref(),
            Some(&("model-special".to_string(), vec![GrantKind::WorkspaceRead]))
        );
        let projection = runtime.projection().expect("projection");
        let worker = projection.workers().first().expect("worker projection");
        assert_eq!(worker.model(), "model-special");
        assert_eq!(worker.tool_grants(), &[GrantKind::WorkspaceRead]);
        assert_eq!(
            observed.lock().expect("observation lock").as_ref(),
            Some(&("model-special".to_string(), vec![GrantKind::WorkspaceRead]))
        );
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn child_tool_catalog_is_derived_from_grants_and_fails_closed() {
        let read =
            ChildToolCatalog::from_grants(&[GrantKind::WorkspaceRead]).expect("read catalog");
        assert_eq!(
            read.tools(),
            &[ChildToolKind::WorkspaceSearch, ChildToolKind::WorkspaceRead]
        );
        assert!(!read.allows(ChildToolKind::WorkspacePatch));

        let write =
            ChildToolCatalog::from_grants(&[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite])
                .expect("write catalog");
        assert!(write.allows(ChildToolKind::WorkspacePatch));
        assert!(ChildToolCatalog::from_grants(&[GrantKind::WorkspaceWrite]).is_err());
        assert!(ChildToolCatalog::from_grants(&[GrantKind::Approval]).is_err());
        assert!(
            ChildWorkerSpec::new("model", [GrantKind::WorkspaceWrite]).is_err(),
            "a write-only child policy must never be executable"
        );
    }

    #[test]
    fn worker_policy_cannot_widen_parent_authority() {
        let root = fixture_root("parent-policy");
        fs::create_dir_all(&root).expect("workspace");
        let parent = grant(&root);
        let child_policy = ChildWorkerSpec::new(
            "model",
            [GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite],
        )
        .expect("valid child tool policy");
        let result = ChildWorkerSpec::from_parent_grant(
            "model",
            &parent,
            child_policy.tool_grants().iter().copied(),
        );
        assert!(matches!(
            result,
            Err(RuntimeError::InvalidSpec(message))
                if message.contains("parent delegation grant")
        ));
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn worker_grant_denial_happens_before_spawn() {
        let (root, authority, runtime) = runtime("grant-denial");
        let request = yunxi_protocol::AgentSpawnRequest::new(authority, "denied")
            .expect("request without child grants");
        let result = runtime.spawn_worker(
            request,
            ChildWorkerSpec::new("model-denied", [GrantKind::WorkspaceRead]).expect("spec"),
            Arc::new(|_turn: ChildTurn, _token: CancellationToken| async {
                Ok("must not run".to_string())
            }),
        );
        assert!(
            matches!(result, Err(RuntimeError::InvalidSpec(message)) if message.contains("requested"))
        );
        assert!(runtime.store().list().expect("list").agents().is_empty());
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn resumed_session_keeps_transcript_and_updates_model_projection() {
        let (root, authority, runtime) = runtime("resume");
        let executor: Arc<dyn ChildExecutor> =
            Arc::new(|turn: ChildTurn, _token: CancellationToken| async move {
                Ok(format!("reply {} {}", turn.transcript.len(), turn.model))
            });
        let spec = ChildWorkerSpec::new("model-initial", [GrantKind::WorkspaceRead]).expect("spec");
        let handle = runtime
            .spawn_worker(request(authority.clone(), "one"), spec, executor.clone())
            .expect("worker");
        handle.wait_blocking().expect("first result");
        let resumed_runtime = AsyncMultiAgentRuntime::new(
            CoordinatorStore::from_grant(&authority, "runtime-restarted").expect("store"),
        );
        let resumed = resumed_runtime
            .resume_worker(
                &authority,
                handle.agent_id(),
                "two",
                ChildWorkerSpec::new("model-resumed", [GrantKind::WorkspaceRead]).expect("spec"),
                executor,
            )
            .expect("resume");
        resumed.wait_blocking().expect("resumed result");
        let inspected = resumed_runtime
            .store()
            .inspect(&AgentInspectRequest::new(authority, handle.agent_id()).expect("inspect"))
            .expect("inspect result");
        assert_eq!(inspected.transcript().len(), 4);
        assert_eq!(
            resumed_runtime
                .store()
                .worker_model(handle.agent_id())
                .expect("stored worker model")
                .as_deref(),
            Some("model-resumed")
        );
        let projection = resumed_runtime.projection().expect("projection");
        let worker = projection
            .workers()
            .iter()
            .find(|worker| worker.agent().id() == handle.agent_id())
            .expect("worker projection");
        assert_eq!(worker.model(), "model-resumed");
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_requeues_running_workers_and_preserves_budget_and_transcript() {
        let (root, authority, _) = runtime("recovery-restart");
        let (store, agent_id) = seed_running_worker(&authority, "runtime-original", "recover me");
        store
            .set_worker_model(&agent_id, "model-persisted")
            .expect("persist worker model");
        let restarted_runtime = restart_runtime(&authority, "runtime-restarted");

        let recoveries = restarted_runtime.recover_workers().expect("recoveries");
        assert_eq!(recoveries.len(), 1);
        let recovery = recoveries
            .into_iter()
            .next()
            .expect("recovery result")
            .expect("recovery candidate");
        assert_eq!(recovery.origin(), WorkerRecoveryOrigin::Running);
        assert_eq!(recovery.agent().id(), agent_id);
        assert_eq!(recovery.agent().status(), AgentStatus::Pending);
        assert_eq!(recovery.transcript().len(), 1);
        assert_eq!(recovery.model(), Some("model-persisted"));
        let recovered_model = recovery.model().expect("persisted model").to_string();

        let executor: Arc<dyn ChildExecutor> = Arc::new(|turn: ChildTurn, _token| async move {
            assert_eq!(turn.transcript.len(), 1);
            Ok(format!("resumed {}", turn.message))
        });
        let handle = restarted_runtime
            .resume_recovered_worker(
                WorkerRecoveryPlan::new(
                    recovery,
                    ChildWorkerSpec::new(recovered_model, [GrantKind::WorkspaceRead])
                        .expect("spec"),
                ),
                executor,
            )
            .expect("resume");
        assert!(matches!(
            handle.wait_blocking(),
            Ok(WorkerOutcome::Completed { reply, .. }) if reply == "resumed recover me"
        ));

        let list = restarted_runtime.store().list().expect("list");
        let agent = list
            .agents()
            .iter()
            .find(|agent| agent.id() == agent_id)
            .expect("agent");
        assert_eq!(agent.status(), AgentStatus::Completed);
        assert_eq!(list.total_turns(), 1);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_is_idempotent_for_already_requeued_workers() {
        let (root, authority, _) = runtime("recovery-idempotent");
        let (_store, agent_id) = seed_running_worker(&authority, "runtime-original", "recover me");
        let restarted_runtime = restart_runtime(&authority, "runtime-restarted");
        let first = restarted_runtime.recover_workers().expect("first recover");
        let first_recovery = first
            .into_iter()
            .next()
            .expect("first recovery result")
            .expect("first recovery");
        assert_eq!(first_recovery.origin(), WorkerRecoveryOrigin::Running);
        let second = restarted_runtime.recover_workers().expect("second recover");
        let second_recovery = second
            .into_iter()
            .next()
            .expect("second recovery result")
            .expect("second recovery");
        assert_eq!(second_recovery.origin(), WorkerRecoveryOrigin::Queued);
        assert_eq!(second_recovery.agent().id(), agent_id);

        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let runs = Arc::new(AtomicUsize::new(0));
        let executor: Arc<dyn ChildExecutor> = {
            let started = started.clone();
            let release = release.clone();
            let runs = runs.clone();
            Arc::new(move |turn: ChildTurn, _token| {
                let started = started.clone();
                let release = release.clone();
                let runs = runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(turn.transcript.len(), 1);
                    {
                        let (lock, wake) = &*started;
                        let mut flag = lock.lock().expect("started flag");
                        *flag = true;
                        wake.notify_one();
                    }
                    let (lock, wake) = &*release;
                    let mut proceed = lock.lock().expect("release flag");
                    while !*proceed {
                        proceed = wake.wait(proceed).expect("release wait");
                    }
                    Ok(format!("idempotent {}", turn.message))
                }
            })
        };
        let plan = WorkerRecoveryPlan::new(
            second_recovery.clone(),
            ChildWorkerSpec::new("model-idempotent", [GrantKind::WorkspaceRead]).expect("spec"),
        );
        let first_handle = restarted_runtime
            .resume_recovered_worker(plan.clone(), executor.clone())
            .expect("first resume");
        wait_for_started(&started);
        let second_handle = restarted_runtime
            .resume_recovered_worker(plan, executor)
            .expect("second resume");
        assert_eq!(first_handle.agent_id(), second_handle.agent_id());

        {
            let (lock, wake) = &*release;
            let mut proceed = lock.lock().expect("release flag");
            *proceed = true;
            wake.notify_all();
        }
        assert!(matches!(
            first_handle.wait_blocking(),
            Ok(WorkerOutcome::Completed { reply, .. }) if reply == "idempotent recover me"
        ));
        assert!(matches!(
            second_handle.wait_blocking(),
            Ok(WorkerOutcome::Completed { reply, .. }) if reply == "idempotent recover me"
        ));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let list = restarted_runtime.store().list().expect("list");
        let agent = list
            .agents()
            .iter()
            .find(|agent| agent.id() == agent_id)
            .expect("agent");
        assert_eq!(agent.status(), AgentStatus::Completed);
        assert_eq!(list.total_turns(), 1);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_workers_are_not_recovered_after_restart() {
        let (root, authority, _) = runtime("recovery-cancelled");
        let (store, agent_id) = seed_running_worker(&authority, "runtime-original", "cancel me");
        store
            .interrupt(
                &yunxi_protocol::AgentInterruptRequest::new(authority.clone(), &agent_id, false)
                    .expect("interrupt"),
            )
            .expect("interrupt turn");

        let restarted_runtime = restart_runtime(&authority, "runtime-restarted");
        let recoveries = restarted_runtime.recover_workers().expect("recoveries");
        assert!(recoveries.is_empty());
        let list = restarted_runtime.store().list().expect("list");
        let agent = list
            .agents()
            .iter()
            .find(|agent| agent.id() == agent_id)
            .expect("agent");
        assert_eq!(agent.status(), AgentStatus::Interrupted);
        assert_eq!(list.total_turns(), 1);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_batch_keeps_sibling_failure_isolated() {
        let (root, authority, _) = runtime("recovery-siblings");
        let store = CoordinatorStore::from_grant(&authority, "runtime-original").expect("store");
        let first = store
            .spawn(
                &yunxi_protocol::AgentSpawnRequest::new(authority.clone(), "first")
                    .expect("spawn first")
                    .with_requested_child_grants([GrantKind::WorkspaceRead])
                    .expect("requested grants"),
            )
            .expect("spawn first agent");
        let second = store
            .spawn(
                &yunxi_protocol::AgentSpawnRequest::new(authority.clone(), "second")
                    .expect("spawn second")
                    .with_requested_child_grants([GrantKind::WorkspaceRead])
                    .expect("requested grants"),
            )
            .expect("spawn second agent");
        let first_id = first.agent().id().to_string();
        let second_id = second.agent().id().to_string();
        store
            .start_turn(
                &yunxi_protocol::AgentTurnStartRequest::new(authority.clone(), &first_id, "first")
                    .expect("start first"),
            )
            .expect("start first turn");
        store
            .start_turn(
                &yunxi_protocol::AgentTurnStartRequest::new(
                    authority.clone(),
                    &second_id,
                    "second",
                )
                .expect("start second"),
            )
            .expect("start second turn");

        let restarted_runtime = restart_runtime(&authority, "runtime-restarted");
        let recoveries = restarted_runtime.recover_workers().expect("recoveries");
        assert_eq!(recoveries.len(), 2);
        let first_recovery = recoveries[0].as_ref().expect("first recovery").clone();
        let second_recovery = recoveries[1].as_ref().expect("second recovery").clone();
        assert_eq!(first_recovery.origin(), WorkerRecoveryOrigin::Running);
        assert_eq!(second_recovery.origin(), WorkerRecoveryOrigin::Running);

        let rendezvous = Arc::new(TwoPartyRendezvous::new());
        let failure_target = first_recovery.agent_id().to_string();
        let executor: Arc<dyn ChildExecutor> = Arc::new(move |turn: ChildTurn, _token| {
            let rendezvous = rendezvous.clone();
            let failure_target = failure_target.clone();
            async move {
                assert_eq!(turn.transcript.len(), 1);
                if !rendezvous.wait() {
                    return Err(ChildWorkerError::new(
                        "recovery_timeout",
                        "worker did not rendezvous",
                    )
                    .expect("error"));
                }
                if turn.agent_id == failure_target {
                    Err(ChildWorkerError::new(
                        "recovery_failure",
                        "only one recovered worker should fail",
                    )
                    .expect("error"))
                } else {
                    Ok(format!("recovered {}", turn.message))
                }
            }
        });
        let handles = restarted_runtime.resume_recovered_workers(
            [
                WorkerRecoveryPlan::new(
                    first_recovery,
                    ChildWorkerSpec::new("model-first", [GrantKind::WorkspaceRead]).expect("spec"),
                ),
                WorkerRecoveryPlan::new(
                    second_recovery,
                    ChildWorkerSpec::new("model-second", [GrantKind::WorkspaceRead]).expect("spec"),
                ),
            ],
            executor,
        );
        assert!(
            handles[0]
                .as_ref()
                .expect("first handle")
                .wait_blocking()
                .is_err()
        );
        assert!(matches!(
            handles[1].as_ref().expect("second handle").wait_blocking(),
            Ok(WorkerOutcome::Completed { reply, .. }) if reply == "recovered second"
        ));

        let list = restarted_runtime.store().list().expect("list");
        let first_agent = list
            .agents()
            .iter()
            .find(|agent| agent.id() == first_id)
            .expect("first agent");
        let second_agent = list
            .agents()
            .iter()
            .find(|agent| agent.id() == second_id)
            .expect("second agent");
        assert_eq!(first_agent.status(), AgentStatus::Failed);
        assert_eq!(second_agent.status(), AgentStatus::Completed);
        assert_eq!(list.total_turns(), 2);
        let _ignored = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_event_log_is_bounded_and_reports_truncation() {
        let (root, _authority, runtime) = runtime("events");
        for index in 0..(MAX_RUNTIME_EVENTS + 7) {
            runtime.emit(
                "agent-1",
                WorkerEventKind::Started,
                &format!("event-{index}"),
            );
        }
        let projection = runtime.projection().expect("projection");
        assert_eq!(projection.events().len(), MAX_RUNTIME_EVENTS);
        assert!(projection.events_truncated());
        assert_eq!(
            projection.events().first().expect("first event").detail(),
            "event-7"
        );
        let _ignored = fs::remove_dir_all(root);
    }
}
