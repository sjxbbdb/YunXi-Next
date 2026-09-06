use crate::cancellation::CancellationToken;
use crate::context::{ContextAssembler, ContextAssemblyRequest};
use crate::error::{AgentError, BudgetKind, ComponentError};
use crate::model::{ModelProvider, ModelRequest};
use crate::session::{SessionEvent, SessionLimits, SessionLog};
use crate::state::{AgentSnapshot, AgentState, TurnSnapshot, TurnState};
use crate::stream::{
    EventSink, EventSinkError, ModelStreamBridge, StreamEmitter, ToolProgressBridge,
};
use crate::tool::{ToolBroker, ToolExecutionOutcome, ToolRequest};
use std::time::Duration;
use yunxi_protocol::{
    ChatMessage, ChatResult, ChatRole, StreamError, StreamTurnState, ToolApprovalDecision,
    ToolApprovalRequest, ToolApprovalState, ToolCallBatch, ToolLoopPolicy, ToolResult,
    ToolResultOutcome,
};

pub const MAX_TURN_TIMEOUT_MILLIS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnBudget {
    max_rounds: u16,
    max_calls_per_round: u16,
    max_tool_calls: u32,
    max_model_calls: u32,
}

impl TurnBudget {
    pub const fn new(
        max_rounds: u16,
        max_calls_per_round: u16,
        max_tool_calls: u32,
        max_model_calls: u32,
    ) -> Self {
        Self {
            max_rounds,
            max_calls_per_round,
            max_tool_calls,
            max_model_calls,
        }
    }

    pub const fn conservative() -> Self {
        Self::new(8, 8, 64, 9)
    }

    pub const fn max_rounds(&self) -> u16 {
        self.max_rounds
    }

    pub const fn max_calls_per_round(&self) -> u16 {
        self.max_calls_per_round
    }

    pub const fn max_tool_calls(&self) -> u32 {
        self.max_tool_calls
    }

    pub const fn max_model_calls(&self) -> u32 {
        self.max_model_calls
    }

    pub const fn policy(&self) -> ToolLoopPolicy {
        ToolLoopPolicy::new(self.max_rounds, self.max_calls_per_round)
    }

    pub fn validate(&self) -> Result<(), AgentError> {
        self.policy()
            .validate()
            .map_err(|error| AgentError::invalid_input("invalid_budget", error.to_string()))?;
        if self.max_tool_calls == 0 {
            return Err(AgentError::invalid_input(
                "invalid_budget",
                "max_tool_calls must be greater than zero",
            ));
        }
        if self.max_model_calls == 0 {
            return Err(AgentError::invalid_input(
                "invalid_budget",
                "max_model_calls must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::conservative()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentConfig {
    budget: TurnBudget,
    session_limits: SessionLimits,
    turn_timeout: Option<Duration>,
}

impl AgentConfig {
    pub fn new(budget: TurnBudget, session_limits: SessionLimits) -> Result<Self, AgentError> {
        budget.validate()?;
        session_limits.validate()?;
        Ok(Self {
            budget,
            session_limits,
            turn_timeout: None,
        })
    }

    pub fn budget(&self) -> TurnBudget {
        self.budget
    }

    pub const fn session_limits(&self) -> SessionLimits {
        self.session_limits
    }

    /// Sets a lazy, per-turn deadline.  No timer thread is created.
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Result<Self, AgentError> {
        validate_turn_timeout(timeout)?;
        self.turn_timeout = Some(timeout);
        Ok(self)
    }

    pub fn without_turn_timeout(mut self) -> Self {
        self.turn_timeout = None;
        self
    }

    pub const fn turn_timeout(&self) -> Option<Duration> {
        self.turn_timeout
    }
}

pub struct Agent<M, C, T> {
    id: String,
    model: M,
    context: C,
    tools: T,
    config: AgentConfig,
    session: SessionLog,
    state: AgentState,
    turns_started: u64,
    next_turn_id: u64,
    last_turn: Option<TurnSnapshot>,
    pending_turn: Option<PendingTurn>,
}

impl<M, C, T> Agent<M, C, T>
where
    M: ModelProvider,
    C: ContextAssembler,
    T: ToolBroker,
{
    pub fn new(
        id: impl Into<String>,
        model: M,
        context: C,
        tools: T,
        config: AgentConfig,
    ) -> Result<Self, AgentError> {
        let id = id.into();
        validate_identifier("agent id", &id)?;
        config.budget.validate()?;
        config.session_limits.validate()?;
        let session = SessionLog::with_limits(config.session_limits)?;
        Ok(Self {
            id,
            model,
            context,
            tools,
            config,
            session,
            state: AgentState::Ready,
            turns_started: 0,
            next_turn_id: 1,
            last_turn: None,
            pending_turn: None,
        })
    }

    pub fn state(&self) -> AgentState {
        self.state
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            id: self.id.clone(),
            state: self.state,
            turns_started: self.turns_started,
            last_turn_id: self.last_turn.as_ref().map(|turn| turn.id.clone()),
        }
    }

    pub fn last_turn(&self) -> Option<&TurnSnapshot> {
        self.last_turn.as_ref()
    }

    pub fn session(&self) -> &SessionLog {
        &self.session
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Rehydrate the conversation owned by an outer session adapter.
    ///
    /// The operation is only valid while the agent is idle. It intentionally
    /// leaves the turn sequence intact so replacing a Web/CLI transcript
    /// cannot make an older turn id appear to be a new one.
    pub fn reset_conversation(&mut self, messages: Vec<ChatMessage>) -> Result<(), AgentError> {
        if self.state != AgentState::Ready {
            return Err(AgentError::InvalidState {
                operation: "reset the conversation".to_string(),
                state: self.state,
            });
        }
        self.session.reset_with_messages(messages)?;
        self.last_turn = None;
        Ok(())
    }

    pub fn conversation(&self) -> Vec<ChatMessage> {
        self.session.conversation()
    }

    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    pub fn context_mut(&mut self) -> &mut C {
        &mut self.context
    }

    pub fn tools_mut(&mut self) -> &mut T {
        &mut self.tools
    }

    pub fn run_text_turn(
        &mut self,
        content: impl Into<String>,
        cancellation: &CancellationToken,
    ) -> Result<TurnResult, AgentError> {
        self.run_turn(ChatMessage::user(content), cancellation)
    }

    pub fn run_turn(
        &mut self,
        user_message: ChatMessage,
        cancellation: &CancellationToken,
    ) -> Result<TurnResult, AgentError> {
        if self.state != AgentState::Ready {
            return Err(AgentError::InvalidState {
                operation: "start a turn".to_string(),
                state: self.state,
            });
        }
        if user_message.role() != ChatRole::User {
            return Err(AgentError::invalid_input(
                "invalid_user_message",
                "agent turns must start with a user message",
            ));
        }
        cancellation.check()?;
        let turn_cancellation = self.turn_cancellation(cancellation);
        let turn_number = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.checked_add(1).ok_or_else(|| {
            AgentError::protocol("turn_id_exhausted", "turn id sequence exhausted")
        })?;
        let turn_id = format!("turn-{turn_number}");
        let mut tracker = TurnTracker::new(turn_id.clone());
        self.state = AgentState::Running;
        self.turns_started = self.turns_started.saturating_add(1);

        let outcome = self.run_turn_inner(
            &mut tracker,
            &turn_id,
            &user_message,
            &turn_cancellation,
            1,
            true,
            ExecutionMode::Legacy,
            None,
            None,
        );
        self.state = AgentState::Ready;
        match outcome {
            Ok(InnerTurnOutcome::Completed(response)) => {
                let snapshot = tracker.into_snapshot();
                self.last_turn = Some(snapshot.clone());
                Ok(TurnResult { response, snapshot })
            }
            Ok(InnerTurnOutcome::AwaitingApproval(_)) => Err(AgentError::protocol(
                "unexpected_approval_request",
                "legacy turn execution returned an approval request",
            )),
            Err(error) => {
                if error.is_cancelled() {
                    tracker.cancel(error.cancellation_reason().unwrap_or("cancelled"));
                    let _ = self.session.append(SessionEvent::TurnCancelled {
                        turn_id,
                        reason: error
                            .cancellation_reason()
                            .unwrap_or("cancelled")
                            .to_string(),
                    });
                } else {
                    tracker.fail(&error);
                    let _ = self.session.append(SessionEvent::TurnFailed {
                        turn_id,
                        code: error.code().to_string(),
                        message: error.message(),
                        retryable: error.retryable(),
                    });
                }
                self.last_turn = Some(tracker.snapshot.clone());
                Err(error)
            }
        }
    }

    /// Runs a turn while publishing bounded model, tool, and lifecycle events.
    ///
    /// The existing [`Self::run_turn`] method remains the compatibility path;
    /// this method opts into the streaming extension without changing any
    /// synchronous provider trait implementations.
    pub fn run_text_turn_streaming<S: EventSink>(
        &mut self,
        content: impl Into<String>,
        cancellation: &CancellationToken,
        sink: &mut S,
    ) -> Result<TurnResult, AgentError> {
        self.run_turn_streaming(ChatMessage::user(content), cancellation, sink)
    }

    pub fn run_turn_streaming<S: EventSink>(
        &mut self,
        user_message: ChatMessage,
        cancellation: &CancellationToken,
        sink: &mut S,
    ) -> Result<TurnResult, AgentError> {
        if self.state != AgentState::Ready {
            return Err(AgentError::InvalidState {
                operation: "start a streaming turn".to_string(),
                state: self.state,
            });
        }
        if user_message.role() != ChatRole::User {
            return Err(AgentError::invalid_input(
                "invalid_user_message",
                "agent turns must start with a user message",
            ));
        }
        cancellation.check()?;
        let turn_cancellation = self.turn_cancellation(cancellation);
        let turn_number = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.checked_add(1).ok_or_else(|| {
            AgentError::protocol("turn_id_exhausted", "turn id sequence exhausted")
        })?;
        let turn_id = format!("turn-{turn_number}");
        let mut tracker = TurnTracker::new(turn_id.clone());
        self.state = AgentState::Running;
        self.turns_started = self.turns_started.saturating_add(1);

        let mut emitter = StreamEmitter::new(sink, &turn_cancellation, turn_id.clone());
        let outcome = match emitter.state(0, StreamTurnState::Created) {
            Ok(()) => self.run_turn_inner(
                &mut tracker,
                &turn_id,
                &user_message,
                &turn_cancellation,
                1,
                true,
                ExecutionMode::Legacy,
                None,
                Some(&mut emitter),
            ),
            Err(error) => Err(stream_sink_error(error)),
        };
        self.state = AgentState::Ready;

        match outcome {
            Ok(InnerTurnOutcome::Completed(response)) => {
                tracker.state = TurnState::Completed;
                let final_events = emitter
                    .state(tracker.round, StreamTurnState::Completed)
                    .and_then(|_| emitter.done(tracker.round, Some(response.clone())));
                if let Err(error) = final_events {
                    let error = stream_sink_error(error);
                    tracker.fail(&error);
                    let _ = self.session.append(SessionEvent::TurnFailed {
                        turn_id: turn_id.clone(),
                        code: error.code().to_string(),
                        message: error.message(),
                        retryable: error.retryable(),
                    });
                    self.last_turn = Some(tracker.snapshot.clone());
                    return Err(error);
                }
                let snapshot = tracker.into_snapshot();
                self.last_turn = Some(snapshot.clone());
                Ok(TurnResult { response, snapshot })
            }
            Ok(InnerTurnOutcome::AwaitingApproval(_)) => {
                let error = AgentError::protocol(
                    "streaming_approval_unsupported",
                    "streaming turns cannot pause for approval; use the approval API",
                );
                tracker.fail(&error);
                let _ = emitter.error(
                    tracker.round,
                    StreamError::redacted(error.code(), error.retryable()),
                );
                let _ = emitter.state(tracker.round, StreamTurnState::Failed);
                let _ = emitter.done(tracker.round, None);
                self.last_turn = Some(tracker.snapshot.clone());
                Err(error)
            }
            Err(error) => {
                if error.is_cancelled() {
                    tracker.cancel(error.cancellation_reason().unwrap_or("cancelled"));
                    let _ = self.session.append(SessionEvent::TurnCancelled {
                        turn_id: turn_id.clone(),
                        reason: error
                            .cancellation_reason()
                            .unwrap_or("cancelled")
                            .to_string(),
                    });
                } else {
                    tracker.fail(&error);
                    let _ = self.session.append(SessionEvent::TurnFailed {
                        turn_id: turn_id.clone(),
                        code: error.code().to_string(),
                        message: error.message(),
                        retryable: error.retryable(),
                    });
                }
                let state = if error.is_timeout() {
                    StreamTurnState::TimedOut
                } else if error.is_cancelled() {
                    StreamTurnState::Cancelled
                } else {
                    StreamTurnState::Failed
                };
                let _ = emitter.error(
                    tracker.round,
                    StreamError::redacted(error.code(), error.retryable()),
                );
                let _ = emitter.state(tracker.round, state);
                let _ = emitter.done(tracker.round, None);
                self.last_turn = Some(tracker.snapshot.clone());
                Err(error)
            }
        }
    }

    /// Starts a turn through the approval-aware tool path.
    ///
    /// The default broker decision is to return `AwaitingApproval`; no tool
    /// is executed until `approve_pending_tool` receives a matching decision.
    pub fn run_turn_with_approval(
        &mut self,
        user_message: ChatMessage,
        cancellation: &CancellationToken,
    ) -> Result<AgentTurnOutcome, AgentError> {
        if self.state != AgentState::Ready {
            return Err(AgentError::InvalidState {
                operation: "start an approval-aware turn".to_string(),
                state: self.state,
            });
        }
        if user_message.role() != ChatRole::User {
            return Err(AgentError::invalid_input(
                "invalid_user_message",
                "agent turns must start with a user message",
            ));
        }
        cancellation.check()?;
        let turn_cancellation = self.turn_cancellation(cancellation);
        let turn_number = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.checked_add(1).ok_or_else(|| {
            AgentError::protocol("turn_id_exhausted", "turn id sequence exhausted")
        })?;
        let turn_id = format!("turn-{turn_number}");
        let mut tracker = TurnTracker::new(turn_id.clone());
        self.state = AgentState::Running;
        self.turns_started = self.turns_started.saturating_add(1);

        let outcome = self.run_turn_inner(
            &mut tracker,
            &turn_id,
            &user_message,
            &turn_cancellation,
            1,
            true,
            ExecutionMode::ApprovalAware,
            None,
            None,
        );
        self.finish_turn_outcome(outcome, tracker, user_message)
    }

    pub fn run_text_turn_with_approval(
        &mut self,
        content: impl Into<String>,
        cancellation: &CancellationToken,
    ) -> Result<AgentTurnOutcome, AgentError> {
        self.run_turn_with_approval(ChatMessage::user(content), cancellation)
    }

    /// Returns the request currently blocking an approval-aware turn.
    pub fn pending_approval(&self) -> Option<&ToolApprovalRequest> {
        self.pending_turn.as_ref().map(|pending| &pending.request)
    }

    /// Resolves the pending call and continues the same turn.
    pub fn approve_pending_tool(
        &mut self,
        decision: ToolApprovalDecision,
        cancellation: &CancellationToken,
    ) -> Result<AgentTurnOutcome, AgentError> {
        let Some(pending) = self.pending_turn.as_ref() else {
            return Err(AgentError::InvalidState {
                operation: "approve a pending tool".to_string(),
                state: self.state,
            });
        };
        if self.state != AgentState::AwaitingApproval {
            return Err(AgentError::InvalidState {
                operation: "approve a pending tool".to_string(),
                state: self.state,
            });
        }
        validate_pending_decision(&pending.request, &decision)?;
        cancellation.check()?;

        let pending = self
            .pending_turn
            .take()
            .expect("pending turn checked immediately above");
        let turn_id = pending.tracker.snapshot.id.clone();
        let mut tracker = pending.tracker;
        self.state = AgentState::Running;
        let outcome = self.continue_pending_turn(
            &mut tracker,
            &turn_id,
            &pending.user_message,
            &pending.batch,
            pending.next_call,
            &decision,
            cancellation,
        );
        self.finish_turn_outcome(outcome, tracker, pending.user_message)
    }

    /// Discards a turn that is waiting for user approval.
    ///
    /// This is used by an outer session when the user changes sessions or
    /// explicitly cancels an approval. It never executes the pending tool and
    /// leaves the agent ready for a fresh conversation.
    pub fn cancel_pending_turn(&mut self, reason: impl Into<String>) -> Result<(), AgentError> {
        if self.state != AgentState::AwaitingApproval {
            return Err(AgentError::InvalidState {
                operation: "cancel a pending tool".to_string(),
                state: self.state,
            });
        }
        let Some(pending) = self.pending_turn.take() else {
            return Err(AgentError::InvalidState {
                operation: "cancel a pending tool".to_string(),
                state: self.state,
            });
        };
        let reason = reason.into();
        let turn_id = pending.tracker.snapshot.id.clone();
        let mut tracker = pending.tracker;
        tracker.cancel(&reason);
        self.session
            .append(SessionEvent::TurnCancelled { turn_id, reason })?;
        self.last_turn = Some(tracker.snapshot);
        self.state = AgentState::Ready;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn run_turn_inner(
        &mut self,
        tracker: &mut TurnTracker,
        turn_id: &str,
        user_message: &ChatMessage,
        cancellation: &CancellationToken,
        starting_round: u16,
        record_start: bool,
        mode: ExecutionMode,
        approval: Option<&ToolApprovalDecision>,
        mut stream: Option<&mut StreamEmitter<'_>>,
    ) -> Result<InnerTurnOutcome, AgentError> {
        if record_start {
            self.record(SessionEvent::TurnStarted {
                turn_id: turn_id.to_string(),
                message: user_message.clone(),
            })?;
        }
        let mut round = starting_round;
        loop {
            cancellation.check()?;
            if round > self.config.budget.max_rounds() {
                return Err(AgentError::BudgetExceeded {
                    kind: BudgetKind::Rounds,
                    limit: u64::from(self.config.budget.max_rounds()),
                    used: u64::from(round),
                });
            }
            tracker.round = round;

            let catalog = self.tools.catalog().map_err(AgentError::Tool)?;
            tracker.state = TurnState::ContextBuilding;
            if let Some(emitter) = stream.as_deref_mut() {
                emitter
                    .state(round, StreamTurnState::ContextBuilding)
                    .map_err(stream_sink_error)?;
            }
            let chat = self
                .context
                .assemble(ContextAssemblyRequest::new(
                    turn_id,
                    user_message,
                    &self.session,
                    &catalog,
                ))
                .map_err(AgentError::Context)?;
            self.record(SessionEvent::ContextBuilt {
                turn_id: turn_id.to_string(),
                message_count: chat.messages().len(),
                tool_count: catalog.tools().len(),
            })?;

            if tracker.model_calls >= self.config.budget.max_model_calls() {
                return Err(AgentError::BudgetExceeded {
                    kind: BudgetKind::ModelCalls,
                    limit: u64::from(self.config.budget.max_model_calls()),
                    used: u64::from(tracker.model_calls),
                });
            }
            tracker.model_calls = tracker.model_calls.saturating_add(1);
            tracker.state = TurnState::ModelCalling;
            if let Some(emitter) = stream.as_deref_mut() {
                emitter
                    .state(round, StreamTurnState::ModelCalling)
                    .map_err(stream_sink_error)?;
            }
            self.record(SessionEvent::ModelRequested {
                turn_id: turn_id.to_string(),
                round,
                message_count: chat.messages().len(),
                tool_count: catalog.tools().len(),
            })?;
            let request = ModelRequest::new(turn_id, round, chat);
            let response = if let Some(emitter) = stream.as_deref_mut() {
                let mut bridge = ModelStreamBridge::new(emitter, round);
                self.model
                    .complete_streaming(&request, cancellation, &mut bridge)
                    .map_err(|error| {
                        cancellation
                            .check()
                            .err()
                            .map_or_else(|| AgentError::Model(error.clone()), AgentError::from)
                    })?
            } else {
                self.model
                    .complete(&request, cancellation)
                    .map_err(|error| {
                        cancellation
                            .check()
                            .err()
                            .map_or_else(|| AgentError::Model(error.clone()), AgentError::from)
                    })?
            };
            cancellation.check()?;

            let batch = if response.tool_calls().is_empty() {
                None
            } else {
                let batch =
                    ToolCallBatch::new(round, response.tool_calls().to_vec()).map_err(|error| {
                        AgentError::protocol("invalid_model_tool_calls", error.to_string())
                    })?;
                batch
                    .validate_with_policy(&self.config.budget.policy())
                    .map_err(|error| {
                        AgentError::protocol("invalid_model_tool_calls", error.to_string())
                    })?;
                Some(batch)
            };
            self.record(SessionEvent::ModelResponded {
                turn_id: turn_id.to_string(),
                round,
                response: response.clone(),
            })?;
            let Some(batch) = batch else {
                if response.content().trim().is_empty() {
                    return Err(AgentError::protocol(
                        "empty_model_response",
                        "model returned neither text nor tool calls",
                    ));
                }
                self.record(SessionEvent::TurnCompleted {
                    turn_id: turn_id.to_string(),
                    content: response.content().to_string(),
                })?;
                tracker.state = TurnState::Completed;
                return Ok(InnerTurnOutcome::Completed(response));
            };

            self.record(SessionEvent::ToolCallsRequested {
                turn_id: turn_id.to_string(),
                batch: batch.clone(),
            })?;
            if let Some(emitter) = stream.as_deref_mut() {
                emitter
                    .state(round, StreamTurnState::ToolCalling)
                    .map_err(stream_sink_error)?;
            }
            if let Some(pending) = self.process_tool_batch(
                tracker,
                turn_id,
                &batch,
                0,
                approval,
                cancellation,
                mode,
                stream.as_deref_mut(),
            )? {
                return Ok(InnerTurnOutcome::AwaitingApproval(pending));
            }
            round = round
                .checked_add(1)
                .ok_or_else(|| AgentError::BudgetExceeded {
                    kind: BudgetKind::Rounds,
                    limit: u64::from(self.config.budget.max_rounds()),
                    used: u64::from(u16::MAX),
                })?;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn continue_pending_turn(
        &mut self,
        tracker: &mut TurnTracker,
        turn_id: &str,
        user_message: &ChatMessage,
        batch: &ToolCallBatch,
        next_call: usize,
        approval: &ToolApprovalDecision,
        cancellation: &CancellationToken,
    ) -> Result<InnerTurnOutcome, AgentError> {
        if let Some(pending) = self.process_tool_batch(
            tracker,
            turn_id,
            batch,
            next_call,
            Some(approval),
            cancellation,
            ExecutionMode::ApprovalAware,
            None,
        )? {
            return Ok(InnerTurnOutcome::AwaitingApproval(pending));
        }
        let next_round =
            batch
                .round()
                .checked_add(1)
                .ok_or_else(|| AgentError::BudgetExceeded {
                    kind: BudgetKind::Rounds,
                    limit: u64::from(self.config.budget.max_rounds()),
                    used: u64::from(u16::MAX),
                })?;
        self.run_turn_inner(
            tracker,
            turn_id,
            user_message,
            cancellation,
            next_round,
            false,
            ExecutionMode::ApprovalAware,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn process_tool_batch(
        &mut self,
        tracker: &mut TurnTracker,
        turn_id: &str,
        batch: &ToolCallBatch,
        start_call: usize,
        approval: Option<&ToolApprovalDecision>,
        cancellation: &CancellationToken,
        mode: ExecutionMode,
        mut stream: Option<&mut StreamEmitter<'_>>,
    ) -> Result<Option<PendingTool>, AgentError> {
        for (index, call) in batch.calls().iter().enumerate().skip(start_call) {
            cancellation.check()?;
            if tracker.tool_calls >= self.config.budget.max_tool_calls() {
                return Err(AgentError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    limit: u64::from(self.config.budget.max_tool_calls()),
                    used: u64::from(tracker.tool_calls),
                });
            }
            tracker.tool_calls = tracker.tool_calls.saturating_add(1);
            tracker.state = TurnState::ToolCalling;
            if let Some(emitter) = stream.as_deref_mut() {
                emitter
                    .tool_start(batch.round(), call)
                    .map_err(stream_sink_error)?;
            }
            let request = ToolRequest::new(turn_id, batch.round(), call);
            let execution = if let Some(emitter) = stream.as_deref_mut() {
                let mut progress = ToolProgressBridge::new(
                    emitter,
                    batch.round(),
                    call.id().clone(),
                    call.name().clone(),
                );
                match mode {
                    ExecutionMode::Legacy => self
                        .tools
                        .execute_with_progress(request, cancellation, &mut progress)
                        .map(ToolExecutionOutcome::Executed),
                    ExecutionMode::ApprovalAware => self.tools.execute_with_approval_and_progress(
                        request,
                        approval.filter(|_| index == start_call),
                        cancellation,
                        &mut progress,
                    ),
                }
            } else {
                match mode {
                    ExecutionMode::Legacy => self
                        .tools
                        .execute(request, cancellation)
                        .map(ToolExecutionOutcome::Executed),
                    ExecutionMode::ApprovalAware => self.tools.execute_with_approval(
                        request,
                        approval.filter(|_| index == start_call),
                        cancellation,
                    ),
                }
            };
            let outcome = match execution {
                Ok(ToolExecutionOutcome::AwaitingApproval(request)) => {
                    self.record(SessionEvent::ToolApprovalRequested {
                        turn_id: turn_id.to_string(),
                        request: request.clone(),
                    })?;
                    return Ok(Some(PendingTool {
                        request,
                        batch: batch.clone(),
                        next_call: index,
                    }));
                }
                Ok(ToolExecutionOutcome::Executed(outcome)) => outcome,
                Err(error) => {
                    ToolResultOutcome::failed(error.code(), error.message(), error.retryable())
                        .map_err(|protocol_error| {
                            AgentError::protocol("invalid_tool_failure", protocol_error.to_string())
                        })?
                }
            };
            if let Some(decision) = approval.filter(|_| index == start_call) {
                self.record(SessionEvent::ToolApprovalResolved {
                    turn_id: turn_id.to_string(),
                    decision: decision.clone(),
                })?;
            }
            let result = ToolResult::new(
                batch.round(),
                call.id().clone(),
                call.name().clone(),
                outcome,
            )
            .map_err(|error| AgentError::protocol("invalid_tool_result", error.to_string()))?;
            self.record(SessionEvent::ToolResultRecorded {
                turn_id: turn_id.to_string(),
                result: result.clone(),
            })?;
            if let Some(emitter) = stream.as_deref_mut() {
                emitter
                    .tool_result(batch.round(), result)
                    .map_err(stream_sink_error)?;
            }
        }
        Ok(None)
    }

    fn finish_turn_outcome(
        &mut self,
        outcome: Result<InnerTurnOutcome, AgentError>,
        mut tracker: TurnTracker,
        user_message: ChatMessage,
    ) -> Result<AgentTurnOutcome, AgentError> {
        match outcome {
            Ok(InnerTurnOutcome::Completed(response)) => {
                self.state = AgentState::Ready;
                let snapshot = tracker.into_snapshot();
                self.last_turn = Some(snapshot.clone());
                Ok(AgentTurnOutcome::Completed(TurnResult {
                    response,
                    snapshot,
                }))
            }
            Ok(InnerTurnOutcome::AwaitingApproval(pending)) => {
                tracker.state = TurnState::AwaitingApproval;
                tracker.sync();
                self.state = AgentState::AwaitingApproval;
                self.last_turn = Some(tracker.snapshot.clone());
                let request = pending.request.clone();
                self.pending_turn = Some(PendingTurn {
                    tracker,
                    user_message,
                    request: pending.request,
                    batch: pending.batch,
                    next_call: pending.next_call,
                });
                Ok(AgentTurnOutcome::AwaitingApproval(request))
            }
            Err(error) => {
                self.state = AgentState::Ready;
                self.pending_turn = None;
                if error.is_cancelled() {
                    tracker.cancel(error.cancellation_reason().unwrap_or("cancelled"));
                    let _ = self.session.append(SessionEvent::TurnCancelled {
                        turn_id: tracker.snapshot.id.clone(),
                        reason: error
                            .cancellation_reason()
                            .unwrap_or("cancelled")
                            .to_string(),
                    });
                } else {
                    tracker.fail(&error);
                    let _ = self.session.append(SessionEvent::TurnFailed {
                        turn_id: tracker.snapshot.id.clone(),
                        code: error.code().to_string(),
                        message: error.message(),
                        retryable: error.retryable(),
                    });
                }
                self.last_turn = Some(tracker.snapshot.clone());
                Err(error)
            }
        }
    }

    fn record(&mut self, event: SessionEvent) -> Result<(), AgentError> {
        self.session
            .append(event)
            .map(|_| ())
            .map_err(AgentError::from)
    }

    fn turn_cancellation(&self, parent: &CancellationToken) -> CancellationToken {
        self.config.turn_timeout().map_or_else(
            || parent.clone(),
            |timeout| CancellationToken::child_with_timeout(parent, timeout),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnResult {
    response: ChatResult,
    snapshot: TurnSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentTurnOutcome {
    Completed(TurnResult),
    AwaitingApproval(ToolApprovalRequest),
}

impl AgentTurnOutcome {
    pub fn completed(&self) -> Option<&TurnResult> {
        match self {
            Self::Completed(result) => Some(result),
            Self::AwaitingApproval(_) => None,
        }
    }

    pub fn approval_request(&self) -> Option<&ToolApprovalRequest> {
        match self {
            Self::Completed(_) => None,
            Self::AwaitingApproval(request) => Some(request),
        }
    }
}

impl TurnResult {
    pub fn response(&self) -> &ChatResult {
        &self.response
    }

    pub fn content(&self) -> &str {
        self.response.content()
    }

    pub fn snapshot(&self) -> &TurnSnapshot {
        &self.snapshot
    }
}

struct TurnTracker {
    snapshot: TurnSnapshot,
    round: u16,
    model_calls: u32,
    tool_calls: u32,
    state: TurnState,
}

#[derive(Clone, Copy)]
enum ExecutionMode {
    Legacy,
    ApprovalAware,
}

struct InnerPendingTool {
    request: ToolApprovalRequest,
    batch: ToolCallBatch,
    next_call: usize,
}

enum InnerTurnOutcome {
    Completed(ChatResult),
    AwaitingApproval(InnerPendingTool),
}

struct PendingTurn {
    tracker: TurnTracker,
    user_message: ChatMessage,
    request: ToolApprovalRequest,
    batch: ToolCallBatch,
    next_call: usize,
}

type PendingTool = InnerPendingTool;

impl TurnTracker {
    fn new(id: String) -> Self {
        Self {
            snapshot: TurnSnapshot {
                id,
                state: TurnState::Created,
                round: 0,
                model_calls: 0,
                tool_calls: 0,
                error: None,
            },
            round: 0,
            model_calls: 0,
            tool_calls: 0,
            state: TurnState::Created,
        }
    }

    fn fail(&mut self, error: &AgentError) {
        self.state = TurnState::Failed;
        self.snapshot.error = Some(ComponentError::new(
            error.code(),
            error.message(),
            error.retryable(),
        ));
        self.sync();
    }

    fn cancel(&mut self, reason: &str) {
        self.state = TurnState::Cancelled;
        self.snapshot.error = Some(ComponentError::new("cancelled", reason, false));
        self.sync();
    }

    fn sync(&mut self) {
        self.snapshot.state = self.state;
        self.snapshot.round = self.round;
        self.snapshot.model_calls = self.model_calls;
        self.snapshot.tool_calls = self.tool_calls;
    }
}

impl TurnTracker {
    fn into_snapshot(mut self) -> TurnSnapshot {
        self.sync();
        self.snapshot
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), AgentError> {
    if value.trim().is_empty() {
        return Err(AgentError::invalid_input(
            "empty_identifier",
            format!("{field} cannot be empty"),
        ));
    }
    if value.len() > 128 || value.contains('\0') {
        return Err(AgentError::invalid_input(
            "invalid_identifier",
            format!("{field} is empty, contains NUL, or exceeds 128 bytes"),
        ));
    }
    Ok(())
}

fn validate_pending_decision(
    request: &ToolApprovalRequest,
    decision: &ToolApprovalDecision,
) -> Result<(), AgentError> {
    decision
        .validate()
        .map_err(|error| AgentError::invalid_input("invalid_approval", error.to_string()))?;
    if decision.round() != request.round()
        || decision.call_id() != request.call_id()
        || decision.tool_name() != request.tool_name()
    {
        return Err(AgentError::invalid_input(
            "approval_mismatch",
            "approval does not match the pending tool call",
        ));
    }
    if matches!(decision.state(), ToolApprovalState::Approved { ticket } if ticket.trim().is_empty())
    {
        return Err(AgentError::invalid_input(
            "invalid_approval",
            "approved tool calls require a non-empty ticket",
        ));
    }
    Ok(())
}

fn validate_turn_timeout(timeout: Duration) -> Result<(), AgentError> {
    if timeout.is_zero() || timeout.as_millis() > u128::from(MAX_TURN_TIMEOUT_MILLIS) {
        return Err(AgentError::invalid_input(
            "invalid_turn_timeout",
            format!("turn timeout must be between 1 and {MAX_TURN_TIMEOUT_MILLIS} milliseconds"),
        ));
    }
    Ok(())
}

fn stream_sink_error(_error: EventSinkError) -> AgentError {
    // Never place event payloads or component diagnostics in the error sent
    // back to a host.  The sink error itself is intentionally coarse.
    AgentError::protocol("event_sink_failed", "stream event sink is unavailable")
}
