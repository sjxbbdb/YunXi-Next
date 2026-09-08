//! Reusable Web-facing facade over the existing CLI Host.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yunxi_agent_spine::{
    BackpressureStrategy, CancellationToken as SpineCancellationToken,
    EventChannel as AgentEventChannel, EventReceiveError, EventReceiver,
};
use yunxi_multi_agent::{
    AsyncMultiAgentRuntime, CancellationToken as MultiAgentCancellationToken, ChildExecutor,
    ChildTurn, ChildWorkerError, ChildWorkerSpec, CoordinatorStore, WorkerHandle, WorkerOutcome,
    WorkerRecovery, WorkerRecoveryPlan,
};
use yunxi_protocol::{
    AgentDelegationGrant, AgentInspectRequest, AgentListResult, AgentStatus, AgentStreamEvent,
    AgentTranscriptRole, ChatMessage, ChatRole, GrantKind, MailboxEntry, MailboxItemKind,
    MailboxSummary, ModelStreamEvent, ROOT_AGENT_ID, SessionListResult, SessionMutation,
    SessionMutationRequest, StreamTurnState, ToolResultOutcome, WorkspaceGrant,
};
use yunxi_settings::{
    CAPABILITY_SETTINGS_NAMESPACE, CapabilityEdit, CapabilitySetting, CapabilitySettingsError,
    CapabilitySettingsStore, CapabilitySwitches, PluginEdit,
};
use yunxi_web_contract::{
    ClientRequest, ClientResponse, EventChannel, RpcError, RpcId, RpcMessage, RpcResult,
    ServerResponse,
};
use yunxi_web_gateway::{
    Gateway, GatewayBackend, GatewayError, GatewayProjection, MAILBOX_GET_METHOD,
    MAILBOX_LIST_METHOD, MAILBOX_MARK_READ_METHOD, MEMORY_LIST_METHOD, MEMORY_SHOW_METHOD,
    MEMORY_STATUS_METHOD, PERSONA_LIST_METHOD, PERSONA_PROFILE_METHOD, PERSONA_STATUS_METHOD,
    RELATIONSHIP_LIST_METHOD, RELATIONSHIP_STATUS_METHOD, SESSION_ATTACHMENT_METHOD,
    SESSION_CANCEL_METHOD, SESSION_CREATE_METHOD, SESSION_FORK_METHOD, SESSION_HISTORY_METHOD,
    SESSION_PROMPT_METHOD, SESSION_RENAME_METHOD, SESSION_SEARCH_METHOD,
    SESSION_SELECT_MODEL_METHOD, SESSION_UPDATE_QUEUE_METHOD, SETTINGS_DESCRIBE_METHOD,
    SETTINGS_MUTATE_METHOD, SETTINGS_REPLACE_METHOD, SETTINGS_UPDATE_METHOD,
    SUBAGENT_HISTORY_METHOD, SUBAGENT_INTERRUPT_METHOD, SUBAGENT_LIST_METHOD,
    SUBAGENT_PROMPT_METHOD, VOICE_CANCEL_METHOD, VOICE_CHAT_METHOD, VOICE_DEVICES_METHOD,
    VOICE_DOCTOR_METHOD, VOICE_PLAYBACK_METHOD, VOICE_SAVE_METHOD, VOICE_SPEAK_METHOD,
    VOICE_TALK_METHOD, VOICE_TRANSCRIBE_METHOD, WEIXIN_CONTROL_METHOD, WEIXIN_DOCTOR_METHOD,
    WEIXIN_LOGIN_METHOD, WEIXIN_LOGOUT_METHOD, WEIXIN_PAIR_METHOD, WEIXIN_POLL_LOGIN_METHOD,
    WEIXIN_QUEUED_METHOD, WEIXIN_REPLY_METHOD, WEIXIN_SEND_METHOD, WEIXIN_SERVE_METHOD,
    WEIXIN_SERVE_START_METHOD, WEIXIN_SERVE_STATUS_METHOD, WEIXIN_SERVE_STOP_METHOD,
    WEIXIN_SESSION_METHOD, WEIXIN_STATUS_METHOD,
};

use crate::args::SessionOptions;
use crate::session::{ChatBackend, ChatSession, WebAgentTask};

const MAX_SETTINGS_MUTATIONS: usize = 64;
const MAX_WEB_SESSIONS: usize = 32;
const WEB_STREAM_CAPACITY: usize = 128;
const MAX_WEB_RETAINED_EVENTS: usize = 4096;
const MAX_WEB_RETAINED_BYTES: usize = 4 * 1024 * 1024;
const MAX_WEB_QUEUED_PROMPTS: usize = 8;
const MAX_WEB_PROMPT_CHARS: usize = 256 * 1024;
const MAX_WEB_SEARCH_CHARS: usize = 500;
const WEB_FAST_PATH_WAIT: Duration = Duration::from_millis(30);
const WEB_EVENT_LONG_POLL: Duration = Duration::from_millis(40);
const MAX_WEB_SUBAGENT_JOBS: usize = 16;
const WEB_SUBAGENT_EVENT_CAPACITY: usize = 128;
const MAX_WEB_SUBAGENT_TEXT_BYTES: usize = 4 * 1024 * 1024;

pub struct WebHost {
    session: SessionSlot,
    gateway: Gateway,
    session_options: SessionOptions,
    sessions: BTreeMap<String, WebSessionState>,
    subagent_jobs: BTreeMap<String, WebSubagentJob>,
    multi_agent_runtimes: BTreeMap<String, AsyncMultiAgentRuntime>,
    next_web_id: u64,
    cordis_event_cursor: u64,
    rebuild_in_progress: bool,
}

struct WebSessionState {
    session: SessionSlot,
    history: Vec<ChatMessage>,
    pending_approval: Option<WebPendingApproval>,
    running_turn: Option<WebRunningTurn>,
    queued_prompts: VecDeque<QueuedPrompt>,
    next_queue_id: u64,
    events: Vec<Value>,
    event_bytes: usize,
    next_event_seq: u64,
}

struct QueuedPrompt {
    item_id: String,
    content: String,
}

impl WebSessionState {
    fn new(session: ChatSession, history: Vec<ChatMessage>) -> Self {
        let mut events = session_event_values(&history);
        let mut event_bytes = events.iter().map(approximate_event_bytes).sum();
        trim_bounded_events(&mut events, &mut event_bytes);
        let next_event_seq = events
            .last()
            .and_then(|event| event.get("seq"))
            .and_then(Value::as_u64)
            .map_or(0, |sequence| sequence.saturating_add(1));
        Self {
            session: SessionSlot::new(session),
            history,
            pending_approval: None,
            running_turn: None,
            queued_prompts: VecDeque::new(),
            next_queue_id: 1,
            events,
            event_bytes,
            next_event_seq,
        }
    }

    fn is_running(&self) -> bool {
        self.running_turn.is_some()
    }

    fn is_pending_approval(&self) -> bool {
        self.pending_approval.is_some()
            || self
                .session
                .0
                .as_ref()
                .is_some_and(|session| session.pending_approval().is_some())
    }
}

struct SessionSlot(Option<ChatSession>);

impl SessionSlot {
    fn new(session: ChatSession) -> Self {
        Self(Some(session))
    }

    fn is_available(&self) -> bool {
        self.0.is_some()
    }

    fn take(&mut self) -> ChatSession {
        self.0
            .take()
            .expect("Web session is checked before starting a worker")
    }

    fn put(&mut self, session: ChatSession) {
        debug_assert!(self.0.is_none());
        self.0 = Some(session);
    }

    fn replace(&mut self, session: ChatSession) {
        self.0 = Some(session);
    }
}

impl Deref for SessionSlot {
    type Target = ChatSession;

    fn deref(&self) -> &Self::Target {
        self.0
            .as_ref()
            .expect("Web session is owned by a running turn")
    }
}

impl DerefMut for SessionSlot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
            .as_mut()
            .expect("Web session is owned by a running turn")
    }
}

enum WebWorkerResult {
    Prompt(Result<String, String>),
    Approval(Result<Option<String>, String>),
}

struct WebWorkerCompletion {
    session: Option<ChatSession>,
    result: WebWorkerResult,
}

#[derive(Clone, Copy)]
enum WebTurnKind {
    Prompt { previous_length: usize },
    Approval,
}

impl WebTurnKind {
    fn previous_length(self) -> Option<usize> {
        match self {
            Self::Prompt { previous_length } => Some(previous_length),
            Self::Approval => None,
        }
    }
}

struct WebRunningTurn {
    kind: WebTurnKind,
    session_id: String,
    turn: u64,
    current_step: u16,
    started_steps: BTreeSet<u16>,
    text_blocks: BTreeSet<u16>,
    cancellation: SpineCancellationToken,
    events: EventReceiver,
    completion: Receiver<WebWorkerCompletion>,
    worker: Option<JoinHandle<()>>,
    reported_drops: u64,
}

struct WebSubagentJob {
    parent_session_id: String,
    root_session_id: String,
    child_session_id: String,
    message_id: String,
    authority: AgentDelegationGrant,
    runtime: AsyncMultiAgentRuntime,
    handle: WorkerHandle,
    events: Receiver<ModelStreamEvent>,
    dropped_events: Arc<AtomicU64>,
    text: String,
    text_overflowed: bool,
    reported_drops: u64,
    reported_runtime_events: u64,
}

struct WebSubagentStart {
    task: WebAgentTask,
    authority: AgentDelegationGrant,
    runtime: AsyncMultiAgentRuntime,
    parent_session_id: String,
    root_session_id: String,
    child_session_id: String,
    message_id: String,
    message: String,
    model: String,
    tool_grants: Vec<GrantKind>,
    recovery: Option<WorkerRecovery>,
}

struct PendingSessionEvent {
    event_type: &'static str,
    data: Value,
    surface_append: bool,
    retain: bool,
    ignorable: bool,
}

impl WebRunningTurn {
    fn project(&mut self, event: AgentStreamEvent) -> Vec<PendingSessionEvent> {
        let mut projected = Vec::new();
        match event {
            AgentStreamEvent::TextDelta { round, delta, .. } => {
                let step = round.saturating_sub(1);
                self.ensure_step(step, &mut projected);
                if self.text_blocks.insert(step) {
                    projected.push(PendingSessionEvent::transient(
                        "assistant/chunk",
                        json!({
                            "turn": self.turn,
                            "step": step,
                            "chunk": { "type": "block-start", "index": 0, "blockType": "text" },
                        }),
                    ));
                }
                projected.push(PendingSessionEvent::transient(
                    "assistant/chunk",
                    json!({
                        "turn": self.turn,
                        "step": step,
                        "chunk": { "type": "text-delta", "index": 0, "text": delta },
                    }),
                ));
            }
            AgentStreamEvent::ToolStart {
                round,
                call_id,
                tool_name,
                ..
            } => {
                let step = round.saturating_sub(1);
                self.ensure_step(step, &mut projected);
                projected.push(PendingSessionEvent::retained(
                    "tool/call",
                    json!({
                        "turn": self.turn,
                        "step": step,
                        "callId": call_id.as_str(),
                        "name": tool_name.as_str(),
                        "arguments": "{}",
                    }),
                    false,
                ));
            }
            AgentStreamEvent::ToolProgress {
                round,
                call_id,
                tool_name,
                progress,
                ..
            } => {
                let step = round.saturating_sub(1);
                self.ensure_step(step, &mut projected);
                projected.push(PendingSessionEvent::diagnostic(
                    "tool/progress",
                    json!({
                        "turn": self.turn,
                        "step": step,
                        "callId": call_id.as_str(),
                        "name": tool_name.as_str(),
                        "progress": progress,
                    }),
                ));
            }
            AgentStreamEvent::ToolResult { round, result, .. } => {
                let step = round.saturating_sub(1);
                self.ensure_step(step, &mut projected);
                let is_error = !matches!(result.outcome(), ToolResultOutcome::Completed { .. });
                let text = serde_json::to_string(result.outcome())
                    .unwrap_or_else(|_| "tool result unavailable".to_string());
                projected.push(PendingSessionEvent::retained(
                    "tool/result",
                    json!({
                        "turn": self.turn,
                        "step": step,
                        "message": {
                            "id": format!("tool-result-{}", result.call_id().as_str()),
                            "role": "user",
                            "content": [{
                                "type": "tool-result",
                                "toolCallId": result.call_id().as_str(),
                                "content": [{ "type": "text", "text": text }],
                                "isError": is_error,
                            }],
                            "source": { "kind": "tool", "callId": result.call_id().as_str() },
                        },
                    }),
                    true,
                ));
            }
            AgentStreamEvent::TurnState {
                round,
                state: StreamTurnState::ModelCalling,
                ..
            } => self.ensure_step(round.saturating_sub(1), &mut projected),
            AgentStreamEvent::TurnState {
                round,
                state,
                sequence,
                turn_id,
                ..
            } => projected.push(PendingSessionEvent::diagnostic(
                "agent/state",
                json!({
                    "turn": self.turn,
                    "step": round.saturating_sub(1),
                    "state": state,
                    "agentSequence": sequence,
                    "agentTurnId": turn_id,
                }),
            )),
            AgentStreamEvent::TurnError {
                round,
                error,
                sequence,
                turn_id,
                ..
            } => projected.push(PendingSessionEvent::diagnostic(
                "agent/error",
                json!({
                    "turn": self.turn,
                    "step": round.saturating_sub(1),
                    "code": error.code(),
                    "message": error.message(),
                    "retryable": error.retryable(),
                    "agentSequence": sequence,
                    "agentTurnId": turn_id,
                }),
            )),
            AgentStreamEvent::TurnDone { .. } => {}
        }
        projected
    }

    fn ensure_step(&mut self, step: u16, projected: &mut Vec<PendingSessionEvent>) {
        if self.started_steps.contains(&step) {
            self.current_step = step;
            return;
        }
        if self.started_steps.contains(&self.current_step) {
            projected.push(PendingSessionEvent::retained(
                "step/end",
                json!({ "turn": self.turn, "step": self.current_step }),
                false,
            ));
        }
        self.current_step = step;
        self.started_steps.insert(step);
        projected.push(PendingSessionEvent::retained(
            "step/start",
            json!({ "turn": self.turn, "step": step }),
            false,
        ));
    }
}

impl PendingSessionEvent {
    fn transient(event_type: &'static str, data: Value) -> Self {
        Self {
            event_type,
            data,
            surface_append: false,
            retain: false,
            ignorable: false,
        }
    }

    fn retained(event_type: &'static str, data: Value, surface_append: bool) -> Self {
        Self {
            event_type,
            data,
            surface_append,
            retain: true,
            ignorable: false,
        }
    }

    fn diagnostic(event_type: &'static str, data: Value) -> Self {
        Self {
            event_type,
            data,
            surface_append: false,
            retain: false,
            ignorable: true,
        }
    }
}

struct WebPendingApproval {
    rpc_id: RpcId,
    approval_id: String,
    session_id: String,
    tool_name: String,
    call_id: String,
    reason: String,
}

struct SettingsSnapshot {
    store: CapabilitySettingsStore,
    file: SettingsFileState,
}

enum SettingsFileState {
    Missing,
    Present(Vec<u8>),
}

struct SettingsApplyFailure {
    cause: String,
    rollback_error: Option<String>,
}

impl WebHost {
    pub fn launch(plugin_path: Option<&Path>) -> Result<Self, WebHostError> {
        let options = SessionOptions {
            plugin_path: plugin_path.map(Path::to_path_buf),
            ..SessionOptions::default()
        };
        Self::launch_with_options(options)
    }

    pub(crate) fn launch_with_options(
        session_options: SessionOptions,
    ) -> Result<Self, WebHostError> {
        let mut session = ChatSession::launch_with_options(&session_options)
            .map_err(|error| WebHostError::Session(error.to_string()))?;
        let gateway = Gateway::new(session.web_projection());
        Ok(Self {
            session: SessionSlot::new(session),
            gateway,
            session_options,
            sessions: BTreeMap::new(),
            subagent_jobs: BTreeMap::new(),
            multi_agent_runtimes: BTreeMap::new(),
            next_web_id: 1,
            cordis_event_cursor: 0,
            rebuild_in_progress: false,
        })
    }

    pub fn refresh(&mut self) {
        let _ = self.poll_all_running_turns();
        let _ = self.poll_subagent_jobs();
        let projection = if let Some(session_id) = self.sessions.keys().next().cloned()
            && let Some(state) = self.sessions.get_mut(&session_id)
            && state.session.is_available()
        {
            Some(state.session.web_projection())
        } else if self.session.is_available() {
            Some(self.session.web_projection())
        } else {
            None
        };
        if let Some(projection) = projection {
            let summaries = projection
                .sessions()
                .iter()
                .cloned()
                .map(|summary| {
                    let running = self
                        .sessions
                        .get(summary.session_id())
                        .is_some_and(WebSessionState::is_running);
                    summary.with_running(running)
                })
                .collect::<Vec<_>>();
            self.gateway
                .replace_projection(projection.with_sessions(summaries));
        }
        self.publish_cordis_events();
    }

    pub fn projection(&mut self) -> &GatewayProjection {
        self.refresh();
        self.gateway.projection()
    }

    pub fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, WebHostError> {
        self.refresh();
        if self.any_running()
            && matches!(
                request.method(),
                SETTINGS_UPDATE_METHOD | SETTINGS_REPLACE_METHOD | SETTINGS_MUTATE_METHOD
            )
        {
            return self.failure(
                request,
                "agent-busy",
                "a Web session is still running",
                json!({ "runningSessions": self.running_session_ids() }),
            );
        }
        match request.method() {
            SESSION_CREATE_METHOD => self.dispatch_session_create(request),
            SESSION_CANCEL_METHOD => self.dispatch_session_cancel(request),
            SESSION_HISTORY_METHOD => self.dispatch_session_history(request),
            SESSION_PROMPT_METHOD => self.dispatch_session_prompt(request),
            SESSION_SEARCH_METHOD => self.dispatch_session_search(request),
            SESSION_RENAME_METHOD => self.dispatch_session_rename(request),
            SESSION_FORK_METHOD => self.dispatch_session_fork(request),
            SESSION_SELECT_MODEL_METHOD => self.dispatch_session_select_model(request),
            SESSION_UPDATE_QUEUE_METHOD => self.dispatch_session_update_queue(request),
            SESSION_ATTACHMENT_METHOD => self.dispatch_session_attachment(request),
            SUBAGENT_LIST_METHOD => self.dispatch_subagent_list(request),
            SUBAGENT_HISTORY_METHOD => self.dispatch_subagent_history(request),
            SUBAGENT_PROMPT_METHOD => self.dispatch_subagent_prompt(request),
            SUBAGENT_INTERRUPT_METHOD => self.dispatch_subagent_interrupt(request),
            SETTINGS_DESCRIBE_METHOD => self.dispatch_settings_describe(request),
            SETTINGS_UPDATE_METHOD => self.dispatch_settings_update(request),
            SETTINGS_REPLACE_METHOD => self.dispatch_settings_replace(request),
            SETTINGS_MUTATE_METHOD => self.dispatch_settings_mutate(request),
            MEMORY_STATUS_METHOD => self.dispatch_memory_status(request),
            MEMORY_LIST_METHOD => self.dispatch_memory_list(request),
            MEMORY_SHOW_METHOD => self.dispatch_memory_show(request),
            PERSONA_STATUS_METHOD => self.dispatch_persona_status(request),
            PERSONA_LIST_METHOD => self.dispatch_persona_list(request),
            PERSONA_PROFILE_METHOD => self.dispatch_persona_profile(request),
            RELATIONSHIP_STATUS_METHOD => self.dispatch_relationship_status(request),
            RELATIONSHIP_LIST_METHOD => self.dispatch_relationship_list(request),
            MAILBOX_LIST_METHOD => self.dispatch_mailbox_list(request),
            MAILBOX_GET_METHOD => self.dispatch_mailbox_get(request),
            MAILBOX_MARK_READ_METHOD => self.dispatch_mailbox_mark_read(request),
            VOICE_DOCTOR_METHOD => {
                self.dispatch_voice_operation(request, "doctor", &["request_id"])
            }
            VOICE_DEVICES_METHOD => {
                self.dispatch_voice_operation(request, "enumerate_devices", &["request_id"])
            }
            VOICE_TRANSCRIBE_METHOD => self.dispatch_voice_operation(
                request,
                "transcribe",
                &[
                    "request_id",
                    "stream_id",
                    "format",
                    "chunks",
                    "input_complete",
                    "status",
                ],
            ),
            VOICE_SPEAK_METHOD => self.dispatch_voice_operation(
                request,
                "speak",
                &["request_id", "stream_id", "text", "format", "status"],
            ),
            VOICE_CHAT_METHOD => self.dispatch_voice_operation(
                request,
                "chat",
                &["request_id", "conversation_id", "text"],
            ),
            VOICE_TALK_METHOD => self.dispatch_voice_operation(
                request,
                "talk",
                &[
                    "request_id",
                    "input",
                    "output_format",
                    "status",
                    "input_device_grant",
                ],
            ),
            VOICE_PLAYBACK_METHOD => {
                self.dispatch_voice_operation(request, "playback", &["request", "chunks"])
            }
            VOICE_SAVE_METHOD => {
                self.dispatch_voice_operation(request, "save", &["request", "chunks"])
            }
            VOICE_CANCEL_METHOD => {
                self.dispatch_voice_operation(request, "cancel", &["stream_id", "status"])
            }
            WEIXIN_STATUS_METHOD => self.dispatch_weixin_status(request),
            WEIXIN_DOCTOR_METHOD => self.dispatch_weixin_doctor(request),
            WEIXIN_LOGIN_METHOD => self.dispatch_weixin_login(request),
            WEIXIN_POLL_LOGIN_METHOD => self.dispatch_weixin_poll_login(request),
            WEIXIN_SERVE_METHOD => self.dispatch_weixin_serve(request),
            WEIXIN_SERVE_START_METHOD => self.dispatch_weixin_serve_start(request),
            WEIXIN_SERVE_STATUS_METHOD => self.dispatch_weixin_serve_status(request),
            WEIXIN_SERVE_STOP_METHOD => self.dispatch_weixin_serve_stop(request),
            WEIXIN_QUEUED_METHOD => self.dispatch_weixin_queued(request),
            WEIXIN_SEND_METHOD => self.dispatch_weixin_send(request),
            WEIXIN_REPLY_METHOD => self.dispatch_weixin_reply(request),
            WEIXIN_CONTROL_METHOD => self.dispatch_weixin_control(request),
            WEIXIN_PAIR_METHOD => self.dispatch_weixin_pair(request),
            WEIXIN_SESSION_METHOD => self.dispatch_weixin_session(request),
            WEIXIN_LOGOUT_METHOD => self.dispatch_weixin_logout(request),
            _ => self
                .gateway
                .dispatch(request)
                .map_err(WebHostError::Gateway),
        }
    }

    pub fn dispatch_message(&mut self, message: RpcMessage) -> Result<RpcMessage, WebHostError> {
        match message {
            RpcMessage::ClientRequest(request) => {
                self.dispatch(&request).map(RpcMessage::ServerResponse)
            }
            message => {
                self.refresh();
                self.gateway
                    .dispatch_message(message)
                    .map_err(WebHostError::Gateway)
            }
        }
    }

    pub fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, WebHostError> {
        if self.any_running() {
            return Err(WebHostError::Chat(
                "a Web turn is already running".to_string(),
            ));
        }
        ChatBackend::complete(&mut *self.session, messages)
            .map_err(|error| WebHostError::Chat(error.to_string()))
    }

    pub fn drain_notices(&mut self) -> Vec<String> {
        let _ = self.poll_all_running_turns();
        let _ = self.poll_subagent_jobs();
        let mut notices = if self.session.is_available() {
            self.session.drain_notices()
        } else {
            Vec::new()
        };
        for state in self.sessions.values_mut() {
            if state.session.is_available() {
                notices.extend(state.session.drain_notices());
            }
        }
        notices
    }

    pub fn publish_event(
        &mut self,
        channel: EventChannel,
        payload: Value,
    ) -> Result<(), WebHostError> {
        match self.gateway.publish_event(channel, payload) {
            Err(GatewayError::EventQueueFull { .. }) => Ok(()),
            result => result.map_err(WebHostError::Gateway),
        }
    }

    pub fn publish_event_with_id(
        &mut self,
        channel: EventChannel,
        rpc_id: RpcId,
        payload: Value,
    ) -> Result<(), WebHostError> {
        match self.gateway.publish_event_with_id(channel, rpc_id, payload) {
            Err(GatewayError::EventQueueFull { .. }) => Ok(()),
            result => result.map_err(WebHostError::Gateway),
        }
    }

    pub fn drain_events(&mut self, channel: EventChannel) -> Vec<RpcMessage> {
        self.gateway.drain_events(channel)
    }

    fn dispatch_settings_describe(
        &self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "settings.describe payload must be an object",
                json!({}),
            );
        }
        self.success(request, self.settings_document_view())
    }

    fn dispatch_settings_update(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if let Err(response) = self.validate_settings_namespace(request) {
            return response;
        }
        if let Err(response) = self.validate_settings_change_allowed(request) {
            return response;
        }
        let Some(patch) = request.payload().get("patch") else {
            return self.failure(
                request,
                "invalid-payload",
                "settings.update requires an object patch",
                json!({ "field": "patch" }),
            );
        };
        if let Some(plugins) = patch.get("plugins") {
            if patch.as_object().is_none_or(|object| object.len() != 1) {
                return self.failure(
                    request,
                    "invalid-payload",
                    "settings.update cannot mix capability and plugin sections",
                    json!({ "field": "patch" }),
                );
            }
            let plugins = match CapabilitySettingsStore::parse_plugins(plugins) {
                Ok(plugins) => plugins,
                Err(error) => return self.settings_rejected(request, error),
            };
            let expected_revision = match expected_revision(request.payload()) {
                Ok(revision) => revision,
                Err(()) => return self.invalid_settings_revision(request),
            };
            let previous = match self.capture_settings_snapshot(request) {
                Ok(snapshot) => snapshot,
                Err(response) => return response,
            };
            let result = self
                .session
                .capability_settings_mut()
                .update_plugins(plugins, expected_revision);
            return self.finish_settings_write(request, previous, result);
        }
        let patch = match CapabilitySettingsStore::parse_section(patch) {
            Ok(patch) => patch,
            Err(error) => return self.settings_rejected(request, error),
        };
        let expected_revision = match expected_revision(request.payload()) {
            Ok(revision) => revision,
            Err(()) => return self.invalid_settings_revision(request),
        };
        let previous = match self.capture_settings_snapshot(request) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        };
        let result = self
            .session
            .capability_settings_mut()
            .update(patch, expected_revision);
        self.finish_settings_write(request, previous, result)
    }

    fn dispatch_settings_replace(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if let Err(response) = self.validate_settings_namespace(request) {
            return response;
        }
        if let Err(response) = self.validate_settings_change_allowed(request) {
            return response;
        }
        let Some(section) = request.payload().get("section") else {
            return self.failure(
                request,
                "invalid-payload",
                "settings.replace requires an object section",
                json!({ "field": "section" }),
            );
        };
        if let Some(plugins) = section.get("plugins") {
            if section.as_object().is_none_or(|object| object.len() != 1) {
                return self.failure(
                    request,
                    "invalid-payload",
                    "settings.replace cannot mix capability and plugin sections",
                    json!({ "field": "section" }),
                );
            }
            let plugins = match CapabilitySettingsStore::parse_plugins(plugins) {
                Ok(plugins) => plugins,
                Err(error) => return self.settings_rejected(request, error),
            };
            let expected_revision = match expected_revision(request.payload()) {
                Ok(revision) => revision,
                Err(()) => return self.invalid_settings_revision(request),
            };
            let previous = match self.capture_settings_snapshot(request) {
                Ok(snapshot) => snapshot,
                Err(response) => return response,
            };
            let result = self
                .session
                .capability_settings_mut()
                .replace_plugins(plugins, expected_revision);
            return self.finish_settings_write(request, previous, result);
        }
        let section = match CapabilitySettingsStore::parse_section(section) {
            Ok(section) => section,
            Err(error) => return self.settings_rejected(request, error),
        };
        let expected_revision = match expected_revision(request.payload()) {
            Ok(revision) => revision,
            Err(()) => return self.invalid_settings_revision(request),
        };
        let previous = match self.capture_settings_snapshot(request) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        };
        let result = self
            .session
            .capability_settings_mut()
            .replace(section, expected_revision);
        self.finish_settings_write(request, previous, result)
    }

    fn dispatch_settings_mutate(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if let Err(response) = self.validate_settings_namespace(request) {
            return response;
        }
        if let Err(response) = self.validate_settings_change_allowed(request) {
            return response;
        }
        let edits = match settings_edits(request.payload()) {
            Ok(edits) => edits,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "ops" }),
                );
            }
        };
        let expected_revision = match expected_revision(request.payload()) {
            Ok(revision) => revision,
            Err(()) => return self.invalid_settings_revision(request),
        };
        let previous = match self.capture_settings_snapshot(request) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        };
        let result = match edits {
            SettingsEdits::Capabilities(edits) => self
                .session
                .capability_settings_mut()
                .mutate(edits, expected_revision),
            SettingsEdits::Plugins(edits) => self
                .session
                .capability_settings_mut()
                .mutate_plugins(edits, expected_revision),
        };
        self.finish_settings_write(request, previous, result)
    }

    fn validate_settings_namespace(
        &self,
        request: &ClientRequest,
    ) -> Result<(), Result<ServerResponse, WebHostError>> {
        if !request.payload().is_object() {
            return Err(self.failure(
                request,
                "invalid-payload",
                "settings write payload must be an object",
                json!({}),
            ));
        }
        if request.payload().get("ns").and_then(Value::as_str)
            != Some(CAPABILITY_SETTINGS_NAMESPACE)
        {
            return Err(self.failure(
                request,
                "settings-rejected",
                "the settings namespace is not writable",
                json!({ "ns": request.payload().get("ns") }),
            ));
        }
        Ok(())
    }

    fn validate_settings_change_allowed(
        &self,
        request: &ClientRequest,
    ) -> Result<(), Result<ServerResponse, WebHostError>> {
        if self.rebuild_in_progress || self.any_pending_approval() || self.any_running() {
            return Err(self.failure(
                request,
                "agent-busy",
                "resolve active Web sessions before changing plugin settings",
                json!({
                    "ns": CAPABILITY_SETTINGS_NAMESPACE,
                    "runningSessions": self.running_session_ids(),
                }),
            ));
        }
        Ok(())
    }

    fn invalid_settings_revision(
        &self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        self.failure(
            request,
            "invalid-payload",
            "expectedRevision must be a non-negative integer",
            json!({ "field": "expectedRevision" }),
        )
    }

    fn finish_settings_write(
        &mut self,
        request: &ClientRequest,
        previous: SettingsSnapshot,
        result: Result<bool, CapabilitySettingsError>,
    ) -> Result<ServerResponse, WebHostError> {
        match result {
            Ok(changed) => {
                if changed {
                    if let Err(error) = self.rebuild_after_settings_change(previous) {
                        let mut details = json!({
                            "ns": CAPABILITY_SETTINGS_NAMESPACE,
                            "rolledBack": error.rollback_error.is_none(),
                            "message": bounded_message(error.cause),
                        });
                        if let Some(rollback_error) = error.rollback_error {
                            details
                                .as_object_mut()
                                .expect("settings failure details are an object")
                                .insert(
                                    "rollbackError".to_string(),
                                    Value::String(bounded_message(rollback_error)),
                                );
                        }
                        return self.failure(
                            request,
                            "settings-apply-failed",
                            "the setting was not applied to the live WebHost",
                            details,
                        );
                    }
                    let revision = self.session.capability_settings().revision();
                    self.publish_event(
                        EventChannel::Host,
                        json!({
                            "type": "host/remote-event",
                            "event": "settings/document-updated",
                            "args": [CAPABILITY_SETTINGS_NAMESPACE, revision],
                        }),
                    )?;
                }
                self.success(request, self.capability_namespace_view())
            }
            Err(error) => self.settings_rejected(request, error),
        }
    }

    fn capture_settings_snapshot(
        &self,
        request: &ClientRequest,
    ) -> Result<SettingsSnapshot, Result<ServerResponse, WebHostError>> {
        let path = self.session.capability_settings().path();
        let file = match fs::read(path) {
            Ok(bytes) => SettingsFileState::Present(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SettingsFileState::Missing
            }
            Err(error) => {
                return Err(self.failure(
                    request,
                    "settings-unavailable",
                    "the current settings document could not be snapshotted",
                    json!({
                        "ns": CAPABILITY_SETTINGS_NAMESPACE,
                        "message": bounded_message(error.to_string()),
                    }),
                ));
            }
        };
        Ok(SettingsSnapshot {
            store: self.session.capability_settings().clone(),
            file,
        })
    }

    fn rebuild_after_settings_change(
        &mut self,
        previous: SettingsSnapshot,
    ) -> Result<(), SettingsApplyFailure> {
        self.rebuild_in_progress = true;
        let result = self.rebuild_after_settings_change_inner(previous);
        self.rebuild_in_progress = false;
        result
    }

    fn rebuild_after_settings_change_inner(
        &mut self,
        previous: SettingsSnapshot,
    ) -> Result<(), SettingsApplyFailure> {
        let session_ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        let mut replacements = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let mut replacement = match ChatSession::launch_with_options(&self.session_options) {
                Ok(session) => session,
                Err(error) => return Err(self.settings_apply_failure(previous, error.to_string())),
            };
            if let Err(error) = replacement.activate_web_session(&session_id) {
                return Err(self.settings_apply_failure(previous, error));
            }
            replacements.push((session_id, replacement));
        }
        let replacement = match ChatSession::launch_with_options(&self.session_options) {
            Ok(session) => session,
            Err(error) => return Err(self.settings_apply_failure(previous, error.to_string())),
        };
        self.session.replace(replacement);
        for (session_id, replacement) in replacements {
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.session.replace(replacement);
            }
        }
        self.gateway
            .replace_projection(self.session.web_projection());
        // A replacement creates a new trusted runtime and therefore a new
        // lifecycle journal. Start replay from its initial cursor.
        self.cordis_event_cursor = 0;
        Ok(())
    }

    /// Project trusted Cordis lifecycle metadata onto the dsh Host channel.
    ///
    /// The journal is intentionally read from the stable base session only:
    /// Web child sessions have independent application state, while plugin
    /// lifecycle belongs to the Host composition. If the base session is
    /// temporarily owned by a running worker, the next refresh resumes from
    /// the same cursor.
    fn publish_cordis_events(&mut self) {
        if !self.session.is_available() {
            return;
        }
        let page = self
            .session
            .cordis_events_since(self.cordis_event_cursor, 64);
        if page.gap() {
            let _ = self.publish_event(
                EventChannel::Host,
                json!({
                    "type": "host/remote-event",
                    "event": "cordis/replay-gap",
                    "args": [{
                        "afterSeq": self.cordis_event_cursor,
                        "oldestSeq": page.oldest_sequence(),
                        "latestSeq": page.latest_sequence(),
                    }],
                }),
            );
            self.cordis_event_cursor = page
                .oldest_sequence()
                .and_then(|sequence| sequence.checked_sub(1))
                .unwrap_or(0);
        }
        for event in page.events() {
            let mut args = serde_json::Map::new();
            args.insert(
                "seq".to_string(),
                Value::Number(serde_json::Number::from(event.sequence())),
            );
            args.insert("kind".to_string(), Value::String(event.kind().to_string()));
            if let Some(plugin_id) = event.plugin_id() {
                args.insert("pluginId".to_string(), Value::String(plugin_id.to_string()));
            }
            if let Some(state) = event.state() {
                args.insert("state".to_string(), Value::String(state.to_string()));
            }
            if let Some(message) = event.message() {
                args.insert("message".to_string(), Value::String(message.to_string()));
            }
            let sequence = event.sequence();
            let _ = self.publish_event(
                EventChannel::Host,
                json!({
                    "type": "host/remote-event",
                    "event": "cordis/lifecycle",
                    "args": [Value::Object(args)],
                }),
            );
            self.cordis_event_cursor = sequence;
        }
    }

    fn settings_apply_failure(
        &mut self,
        previous: SettingsSnapshot,
        cause: String,
    ) -> SettingsApplyFailure {
        let rollback_error =
            restore_settings_file(self.session.capability_settings().path(), previous.file).err();
        *self.session.capability_settings_mut() = previous.store;
        SettingsApplyFailure {
            cause,
            rollback_error: rollback_error.map(|error| error.to_string()),
        }
    }

    fn settings_rejected(
        &self,
        request: &ClientRequest,
        error: CapabilitySettingsError,
    ) -> Result<ServerResponse, WebHostError> {
        match error {
            CapabilitySettingsError::Conflict { expected, current } => self.failure(
                request,
                "settings-conflict",
                "the capability settings changed after this view was read",
                json!({
                    "expectedRevision": expected,
                    "currentRevision": current,
                }),
            ),
            _ => self.failure(
                request,
                "settings-rejected",
                "the capability settings write was rejected",
                json!({ "ns": CAPABILITY_SETTINGS_NAMESPACE }),
            ),
        }
    }

    fn settings_document_view(&self) -> Value {
        json!({
            "writable": true,
            "hasDocument": false,
            "namespaces": [
                {
                    "ns": "ui-onboarding",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "welcomeNoticeVersion": { "type": "string" },
                        },
                    },
                    "value": {
                        "welcomeNoticeVersion": "2026-08-13.1",
                    },
                    "applies": "live",
                    "secrets": [],
                    "revision": 0,
                },
                self.capability_namespace_view(),
            ],
        })
    }

    fn capability_namespace_view(&self) -> Value {
        let settings = self.session.capability_settings();
        let user = serde_json::to_value(settings.overrides())
            .expect("capability overrides are serializable");
        let mut view = json!({
            "ns": CAPABILITY_SETTINGS_NAMESPACE,
            "schema": capability_settings_schema(),
            "value": settings.resolved(),
            "base": CapabilitySwitches::default(),
            "applies": "live",
            "secrets": [],
            "revision": settings.revision(),
            "plugins": settings.plugin_overrides(),
        });
        if user.as_object().is_some_and(|object| !object.is_empty()) {
            view.as_object_mut()
                .expect("capability namespace view is an object")
                .insert("user".to_string(), user);
        }
        view
    }

    fn dispatch_memory_status(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "memory.status payload must be an object",
                json!({}),
            );
        }
        if !self.memory_route_available() {
            return self.management_unavailable(
                request,
                "memory",
                "memory plugin is disabled or unavailable",
            );
        }
        let (cwd, report) = match self.memory_session_mut() {
            Ok(session) => {
                let cwd = session.workspace_root().to_path_buf();
                let report = session.web_memory_status();
                (cwd, report)
            }
            Err(error) => return self.management_unavailable(request, "memory", error),
        };
        let report = match report {
            Ok(report) => report,
            Err(error) => return self.management_unavailable(request, "memory", error.to_string()),
        };
        let mut value = serde_json::to_value(report).map_err(|error| {
            WebHostError::Session(format!("memory status serialization failed: {error}"))
        })?;
        let object = value
            .as_object_mut()
            .expect("memory status is serialized as an object");
        object.insert(
            "schemaVersion".to_string(),
            Value::Number(serde_json::Number::from(1)),
        );
        object.insert(
            "workspace".to_string(),
            Value::String(cwd.to_string_lossy().into_owned()),
        );
        object.insert(
            "source".to_string(),
            Value::String("yunxi-memory".to_string()),
        );
        self.success(request, value)
    }

    fn dispatch_memory_list(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "memory.list payload must be an object",
                json!({}),
            );
        }
        if !self.memory_route_available() {
            return self.management_unavailable(
                request,
                "memory",
                "memory plugin is disabled or unavailable",
            );
        }
        let limit = match bounded_management_limit(request.payload(), "limit", 100, 200) {
            Ok(limit) => limit,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "limit", "maximum": 200 }),
                );
            }
        };
        let (cwd, result) = match self.memory_session_mut() {
            Ok(session) => {
                let cwd = session.workspace_root().to_path_buf();
                let result = session.web_memory_list(limit);
                (cwd, result)
            }
            Err(error) => return self.management_unavailable(request, "memory", error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.management_unavailable(request, "memory", error.to_string()),
        };
        let value = serde_json::to_value(result).map_err(|error| {
            WebHostError::Session(format!("memory list serialization failed: {error}"))
        })?;
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "workspace": cwd.to_string_lossy(),
                "source": "yunxi-memory",
                "result": value,
            }),
        )
    }

    fn dispatch_memory_show(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "memory.show payload must be an object",
                json!({}),
            );
        }
        if !self.memory_route_available() {
            return self.management_unavailable(
                request,
                "memory",
                "memory plugin is disabled or unavailable",
            );
        }
        let Some(id) = string_field(request.payload(), "id") else {
            return self.failure(
                request,
                "invalid-payload",
                "memory.show requires a string id",
                json!({ "field": "id" }),
            );
        };
        let (cwd, record) = match self.memory_session_mut() {
            Ok(session) => {
                let cwd = session.workspace_root().to_path_buf();
                let record = session.web_memory_show(&id);
                (cwd, record)
            }
            Err(error) => return self.management_unavailable(request, "memory", error),
        };
        let record = match record {
            Ok(record) => record,
            Err(error) => return self.management_unavailable(request, "memory", error.to_string()),
        };
        let Some(record) = record else {
            return self.failure(
                request,
                "memory-not-found",
                "the requested memory record does not exist",
                json!({ "id": id }),
            );
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "workspace": cwd.to_string_lossy(),
                "source": "yunxi-memory",
                "record": record,
            }),
        )
    }

    fn dispatch_persona_status(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "persona.status payload must be an object",
                json!({}),
            );
        }
        if !self.persona_route_available() {
            return self.management_unavailable(
                request,
                "persona",
                "persona plugin is disabled or unavailable",
            );
        }
        let status = match self.persona_session_mut() {
            Ok(session) => session.web_persona_status(),
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "source": "yunxi-persona",
                "status": status,
            }),
        )
    }

    fn dispatch_persona_list(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "persona.list payload must be an object",
                json!({}),
            );
        }
        if !self.persona_route_available() {
            return self.management_unavailable(
                request,
                "persona",
                "persona plugin is disabled or unavailable",
            );
        }
        let profiles = match self.persona_session_mut() {
            Ok(session) => session.web_persona_list(),
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        let profiles = match profiles {
            Ok(profiles) => profiles,
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "source": "yunxi-persona",
                "profiles": profiles,
            }),
        )
    }

    fn dispatch_persona_profile(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "persona.profile payload must be an object",
                json!({}),
            );
        }
        if !self.persona_route_available() {
            return self.management_unavailable(
                request,
                "persona",
                "persona plugin is disabled or unavailable",
            );
        }
        let requested = request.payload().get("id");
        let requested_id = match requested {
            None => None,
            Some(value) => match value.as_str().filter(|value| !value.trim().is_empty()) {
                Some(value) => Some(value.to_string()),
                None => {
                    return self.failure(
                        request,
                        "invalid-payload",
                        "persona.profile id must be a non-empty string",
                        json!({ "field": "id" }),
                    );
                }
            },
        };
        let (status, profile) = match self.persona_session_mut() {
            Ok(session) => {
                let status = session.web_persona_status();
                let profile = session.web_persona_profile(requested_id.clone());
                (status, profile)
            }
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        let profile = match profile {
            Ok(profile) => profile,
            Err(error) => return self.management_unavailable(request, "persona", error),
        };
        let id = requested_id.unwrap_or_else(|| status.active_profile.clone());
        let Some(profile) = profile else {
            return self.failure(
                request,
                "persona-profile-not-found",
                "the requested persona profile does not exist",
                json!({ "id": id }),
            );
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "source": "yunxi-persona",
                "enabled": status.enabled,
                "activeProfile": status.active_profile,
                "profile": profile,
            }),
        )
    }

    fn dispatch_relationship_status(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "relationship.status payload must be an object",
                json!({}),
            );
        }
        if !self.memory_route_available() {
            return self.management_unavailable(
                request,
                "relationship",
                "memory plugin is disabled or unavailable",
            );
        }
        let (cwd, memory_status, memory_records) = match self.memory_session_mut() {
            Ok(session) => {
                let cwd = session.workspace_root().to_path_buf();
                let status = session.web_memory_status();
                let records = session.web_memory_list(200);
                (cwd, status, records)
            }
            Err(error) => return self.management_unavailable(request, "relationship", error),
        };
        let memory_status = match memory_status {
            Ok(status) => status,
            Err(error) => {
                return self.management_unavailable(request, "relationship", error.to_string());
            }
        };
        let records = match memory_records {
            Ok(result) => relationship_records(result.records),
            Err(error) => {
                return self.management_unavailable(request, "relationship", error.to_string());
            }
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "workspace": cwd.to_string_lossy(),
                "source": "yunxi-memory",
                "enabled": memory_status.enabled,
                "count": records.len(),
                "records": records,
                "warnings": memory_status.warnings,
            }),
        )
    }

    fn dispatch_relationship_list(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "relationship.list payload must be an object",
                json!({}),
            );
        }
        if !self.memory_route_available() {
            return self.management_unavailable(
                request,
                "relationship",
                "memory plugin is disabled or unavailable",
            );
        }
        let limit = match bounded_management_limit(request.payload(), "limit", 100, 200) {
            Ok(limit) => limit,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "limit", "maximum": 200 }),
                );
            }
        };
        let (cwd, result) = match self.memory_session_mut() {
            Ok(session) => {
                let cwd = session.workspace_root().to_path_buf();
                let result = session.web_memory_list(limit);
                (cwd, result)
            }
            Err(error) => return self.management_unavailable(request, "relationship", error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                return self.management_unavailable(request, "relationship", error.to_string());
            }
        };
        let records = relationship_records(result.records);
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "workspace": cwd.to_string_lossy(),
                "source": "yunxi-memory",
                "count": records.len(),
                "records": records,
                "truncated": result.truncated,
                "warnings": result.warnings,
            }),
        )
    }

    fn dispatch_mailbox_list(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "mailbox.list payload must be an object",
                json!({}),
            );
        }
        let limit = match bounded_management_limit(request.payload(), "limit", 50, 200) {
            Ok(limit) => limit,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "limit", "maximum": 200 }),
                );
            }
        };
        let unread_only = match optional_bool(request.payload(), "unreadOnly") {
            Ok(value) => value.unwrap_or(false),
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "unreadOnly" }),
                );
            }
        };
        let session_id = string_field(request.payload(), "sessionId");
        let result = match self.mailbox_session_mut(session_id.as_deref()) {
            Ok(session) if session.mailbox_web_available() => {
                session.web_mailbox_list(limit, unread_only)
            }
            Ok(_) => Err("mailbox plugin is disabled or unavailable".to_string()),
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.management_unavailable(request, "mailbox", error),
        };
        let items = result
            .items()
            .iter()
            .map(mailbox_summary_value)
            .collect::<Vec<_>>();
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "sessionId": session_id,
                "items": items,
                "unreadCount": result.unread_count(),
                "truncated": result.truncated(),
                "warnings": result.warnings(),
            }),
        )
    }

    fn dispatch_mailbox_get(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "mailbox.get payload must be an object",
                json!({}),
            );
        }
        let Some(item_id) = string_field(request.payload(), "itemId") else {
            return self.failure(
                request,
                "invalid-payload",
                "mailbox.get requires a string itemId",
                json!({ "field": "itemId" }),
            );
        };
        let session_id = string_field(request.payload(), "sessionId");
        let result = match self.mailbox_session_mut(session_id.as_deref()) {
            Ok(session) if session.mailbox_web_available() => session.web_mailbox_get(&item_id),
            Ok(_) => Err("mailbox plugin is disabled or unavailable".to_string()),
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.management_unavailable(request, "mailbox", error),
        };
        let Some(entry) = result.entry() else {
            return self.failure(
                request,
                "mailbox-not-found",
                "the requested mailbox item does not exist",
                json!({ "itemId": item_id }),
            );
        };
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "sessionId": session_id,
                "item": mailbox_entry_value(entry),
                "warnings": result.warnings(),
            }),
        )
    }

    fn dispatch_mailbox_mark_read(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "mailbox.markRead payload must be an object",
                json!({}),
            );
        }
        if self.any_running() {
            return self.failure(
                request,
                "agent-busy",
                "mailbox state cannot change while a Web turn is running",
                json!({ "runningSessions": self.running_session_ids() }),
            );
        }
        let Some(item_id) = string_field(request.payload(), "itemId") else {
            return self.failure(
                request,
                "invalid-payload",
                "mailbox.markRead requires a string itemId",
                json!({ "field": "itemId" }),
            );
        };
        let read = match request.payload().get("read").and_then(Value::as_bool) {
            Some(read) => read,
            None => {
                return self.failure(
                    request,
                    "invalid-payload",
                    "mailbox.markRead requires a boolean read",
                    json!({ "field": "read" }),
                );
            }
        };
        let session_id = string_field(request.payload(), "sessionId");
        let result = match self.mailbox_session_mut(session_id.as_deref()) {
            Ok(session) if session.mailbox_web_available() => {
                session.web_mailbox_mark_read(&item_id, read)
            }
            Ok(_) => Err("mailbox plugin is disabled or unavailable".to_string()),
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return self.management_unavailable(request, "mailbox", error),
        };
        let item = result.item().map(mailbox_summary_value);
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/mailbox-updated",
                "sessionId": session_id,
                "itemId": item_id,
                "read": read,
            }),
        )?;
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "sessionId": session_id,
                "item": item,
                "changed": result.item().is_some(),
                "warnings": result.warnings(),
            }),
        )
    }

    fn dispatch_voice_operation(
        &mut self,
        request: &ClientRequest,
        operation: &str,
        allowed_fields: &[&str],
    ) -> Result<ServerResponse, WebHostError> {
        let (target, mut payload) = match self.parse_voice_request(request, allowed_fields) {
            Ok(parsed) => parsed,
            Err(response) => return response,
        };
        if matches!(operation, "doctor" | "enumerate_devices")
            && payload.get("request_id").is_none()
        {
            let request_id = format!("web-voice-{}", self.next_web_id);
            self.next_web_id = self.next_web_id.saturating_add(1);
            payload
                .as_object_mut()
                .expect("validated Voice payload is an object")
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if !self.voice_route_available() {
            return self.management_unavailable(
                request,
                "voice",
                "voice plugin is disabled or unavailable",
            );
        }
        let runtime = match self.voice_session_mut(target.as_deref()) {
            Ok(session) => session.invoke_voice(operation, &payload),
            Err(error) => Err(error),
        };
        let runtime = match runtime {
            Ok(value) => value,
            Err(error) => return self.management_unavailable(request, "voice", error),
        };
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/voice-updated",
                "operation": operation,
                "sessionId": target,
            }),
        )?;
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "source": "yunxi-voice",
                "sessionId": target,
                "operation": operation,
                "report": runtime,
            }),
        )
    }

    fn parse_voice_request(
        &self,
        request: &ClientRequest,
        allowed_fields: &[&str],
    ) -> Result<(Option<String>, Value), Result<ServerResponse, WebHostError>> {
        let Some(object) = request.payload().as_object() else {
            return Err(self.failure(
                request,
                "invalid-payload",
                "Voice RPC payload must be an object",
                json!({}),
            ));
        };
        if let Some(unknown) = object.keys().find(|key| {
            key.as_str() != "sessionId"
                && !allowed_fields
                    .iter()
                    .any(|allowed| allowed == &key.as_str())
        }) {
            return Err(self.failure(
                request,
                "invalid-payload",
                "Voice RPC payload contains an unknown field",
                json!({ "field": unknown }),
            ));
        }
        let target = match optional_weixin_string(request.payload(), "sessionId") {
            Ok(target) => target,
            Err(message) => {
                return Err(self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "sessionId" }),
                ));
            }
        };
        let payload = Value::Object(
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "sessionId")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        Ok((target, payload))
    }

    fn dispatch_weixin_status(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::STATUS_OPERATION,
            json!({}),
        )
    }

    fn dispatch_weixin_doctor(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::RUNTIME_DOCTOR_OPERATION,
            json!({}),
        )
    }

    fn dispatch_weixin_login(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::LOGIN_OPERATION,
            json!({}),
        )
    }

    fn dispatch_weixin_poll_login(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId", "verifyCode"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let verify_code = match optional_weixin_string(request.payload(), "verifyCode") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "verifyCode" }),
                );
            }
        };
        let mut payload = serde_json::Map::new();
        if let Some(verify_code) = verify_code {
            payload.insert("verify_code".to_string(), Value::String(verify_code));
        }
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::POLL_LOGIN_OPERATION,
            Value::Object(payload),
        )
    }

    fn dispatch_weixin_serve(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        self.dispatch_weixin_serve_operation(request, yunxi_weixin::SERVE_OPERATION)
    }

    fn dispatch_weixin_serve_start(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        self.dispatch_weixin_serve_operation(request, yunxi_weixin::SERVE_START_OPERATION)
    }

    fn dispatch_weixin_serve_operation(
        &mut self,
        request: &ClientRequest,
        operation: &str,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(
            request,
            &[
                "sessionId",
                "maxPolls",
                "maxMessagesPerPoll",
                "requireApproval",
            ],
        ) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let max_polls = match optional_weixin_limit(request.payload(), "maxPolls", 10_000) {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "maxPolls", "maximum": 10000 }),
                );
            }
        };
        let max_messages = match optional_weixin_limit(
            request.payload(),
            "maxMessagesPerPoll",
            yunxi_weixin::MAX_ILINK_MESSAGES as u64,
        ) {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({
                        "field": "maxMessagesPerPoll",
                        "maximum": yunxi_weixin::MAX_ILINK_MESSAGES,
                    }),
                );
            }
        };
        let require_approval = match optional_weixin_bool(request.payload(), "requireApproval") {
            Ok(value) => value.unwrap_or(false),
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "requireApproval" }),
                );
            }
        };
        let mut payload = serde_json::Map::new();
        if let Some(value) = max_polls {
            payload.insert("max_polls".to_string(), Value::Number(value.into()));
        }
        if let Some(value) = max_messages {
            payload.insert(
                "max_messages_per_poll".to_string(),
                Value::Number(value.into()),
            );
        }
        payload.insert(
            "require_approval".to_string(),
            Value::Bool(require_approval),
        );
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            operation,
            Value::Object(payload),
        )
    }

    fn dispatch_weixin_serve_status(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::SERVE_STATUS_OPERATION,
            json!({}),
        )
    }

    fn dispatch_weixin_serve_stop(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::SERVE_STOP_OPERATION,
            json!({}),
        )
    }

    fn dispatch_weixin_queued(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId", "maximum"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let maximum = match optional_weixin_limit(
            request.payload(),
            "maximum",
            yunxi_weixin::MAX_ILINK_MESSAGES as u64,
        ) {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({
                        "field": "maximum",
                        "maximum": yunxi_weixin::MAX_ILINK_MESSAGES,
                    }),
                );
            }
        };
        let payload = maximum.map_or_else(|| json!({}), |value| json!({ "maximum": value }));
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::QUEUED_MESSAGES_OPERATION,
            payload,
        )
    }

    fn dispatch_weixin_send(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId", "message"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let Some(message) = request.payload().get("message") else {
            return self.failure(
                request,
                "invalid-payload",
                "weixin.send requires a message object",
                json!({ "field": "message" }),
            );
        };
        if !message.is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "weixin.send message must be an object",
                json!({ "field": "message" }),
            );
        }
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::SEND_MESSAGE_OPERATION,
            json!({ "message": message }),
        )
    }

    fn dispatch_weixin_reply(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(
            request,
            &["sessionId", "idempotencyKey", "replyMessageId", "text"],
        ) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let idempotency_key = match required_weixin_string(request.payload(), "idempotencyKey") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "idempotencyKey" }),
                );
            }
        };
        let reply_message_id = match required_weixin_string(request.payload(), "replyMessageId") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "replyMessageId" }),
                );
            }
        };
        let text = match required_weixin_string(request.payload(), "text") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "text" }),
                );
            }
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::REPLY_TEXT_OPERATION,
            json!({
                "idempotency_key": idempotency_key,
                "reply_message_id": reply_message_id,
                "text": text,
            }),
        )
    }

    fn dispatch_weixin_control(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(
            request,
            &["sessionId", "action", "idempotencyKey", "reason"],
        ) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let action = match required_weixin_string(request.payload(), "action") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "action" }),
                );
            }
        };
        let idempotency_key = match required_weixin_string(request.payload(), "idempotencyKey") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "idempotencyKey" }),
                );
            }
        };
        let reason = match optional_weixin_string(request.payload(), "reason") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "reason" }),
                );
            }
        };
        let mut payload = serde_json::Map::new();
        payload.insert(
            "idempotency_key".to_string(),
            Value::String(idempotency_key),
        );
        match action.as_str() {
            "acknowledge" | "approve" | "requestApproval" | "request_approval" => {
                let action = if action == "requestApproval" {
                    "request_approval"
                } else {
                    action.as_str()
                };
                payload.insert("action".to_string(), Value::String(action.to_string()));
            }
            "cancel" | "deny" => {
                let Some(reason) = reason else {
                    return self.failure(
                        request,
                        "invalid-payload",
                        "weixin.control cancel and deny require a non-empty reason",
                        json!({ "field": "reason" }),
                    );
                };
                payload.insert("action".to_string(), Value::String(action));
                payload.insert("reason".to_string(), Value::String(reason));
            }
            _ => {
                return self.failure(
                    request,
                    "invalid-payload",
                    "weixin.control action must be acknowledge, cancel, requestApproval, approve, or deny",
                    json!({ "field": "action" }),
                );
            }
        }
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::REMOTE_CONTROL_OPERATION,
            Value::Object(payload),
        )
    }

    fn dispatch_weixin_pair(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(
            request,
            &["sessionId", "action", "peerId", "requestId", "reason"],
        ) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let action = match required_weixin_string(request.payload(), "action") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "action" }),
                );
            }
        };
        let payload = match action.as_str() {
            "request" => {
                let peer_id = match required_weixin_string(request.payload(), "peerId") {
                    Ok(value) => value,
                    Err(message) => {
                        return self.failure(
                            request,
                            "invalid-payload",
                            &message,
                            json!({ "field": "peerId" }),
                        );
                    }
                };
                json!({ "action": "request", "peer_id": peer_id })
            }
            "approve" => {
                let request_id = match required_weixin_string(request.payload(), "requestId") {
                    Ok(value) => value,
                    Err(message) => {
                        return self.failure(
                            request,
                            "invalid-payload",
                            &message,
                            json!({ "field": "requestId" }),
                        );
                    }
                };
                json!({ "action": "approve", "request_id": request_id })
            }
            "deny" => {
                let request_id = match required_weixin_string(request.payload(), "requestId") {
                    Ok(value) => value,
                    Err(message) => {
                        return self.failure(
                            request,
                            "invalid-payload",
                            &message,
                            json!({ "field": "requestId" }),
                        );
                    }
                };
                let reason = match required_weixin_string(request.payload(), "reason") {
                    Ok(value) => value,
                    Err(message) => {
                        return self.failure(
                            request,
                            "invalid-payload",
                            &message,
                            json!({ "field": "reason" }),
                        );
                    }
                };
                json!({ "action": "deny", "request_id": request_id, "reason": reason })
            }
            _ => {
                return self.failure(
                    request,
                    "invalid-payload",
                    "weixin.pair action must be request, approve, or deny",
                    json!({ "field": "action" }),
                );
            }
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::PAIR_OPERATION,
            payload,
        )
    }

    fn dispatch_weixin_session(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(
            request,
            &["sessionId", "action", "bindingSessionId", "peerId"],
        ) {
            Ok(target) => target,
            Err(response) => return response,
        };
        let action = match required_weixin_string(request.payload(), "action") {
            Ok(value) => value,
            Err(message) => {
                return self.failure(
                    request,
                    "invalid-payload",
                    &message,
                    json!({ "field": "action" }),
                );
            }
        };
        let payload = match action.as_str() {
            "list" => json!({ "action": "list" }),
            "bind" => {
                let binding_session_id =
                    match required_weixin_string(request.payload(), "bindingSessionId") {
                        Ok(value) => value,
                        Err(message) => {
                            return self.failure(
                                request,
                                "invalid-payload",
                                &message,
                                json!({ "field": "bindingSessionId" }),
                            );
                        }
                    };
                let peer_id = match required_weixin_string(request.payload(), "peerId") {
                    Ok(value) => value,
                    Err(message) => {
                        return self.failure(
                            request,
                            "invalid-payload",
                            &message,
                            json!({ "field": "peerId" }),
                        );
                    }
                };
                json!({
                    "action": "bind",
                    "session_id": binding_session_id,
                    "peer_id": peer_id,
                })
            }
            "unbind" => {
                let binding_session_id =
                    match required_weixin_string(request.payload(), "bindingSessionId") {
                        Ok(value) => value,
                        Err(message) => {
                            return self.failure(
                                request,
                                "invalid-payload",
                                &message,
                                json!({ "field": "bindingSessionId" }),
                            );
                        }
                    };
                json!({ "action": "unbind", "session_id": binding_session_id })
            }
            _ => {
                return self.failure(
                    request,
                    "invalid-payload",
                    "weixin.session action must be list, bind, or unbind",
                    json!({ "field": "action" }),
                );
            }
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::SESSION_OPERATION,
            payload,
        )
    }

    fn dispatch_weixin_logout(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let target = match self.parse_weixin_request(request, &["sessionId"]) {
            Ok(target) => target,
            Err(response) => return response,
        };
        self.invoke_weixin_route(
            request,
            target.as_deref(),
            yunxi_weixin::LOGOUT_OPERATION,
            json!({}),
        )
    }

    fn parse_weixin_request(
        &self,
        request: &ClientRequest,
        allowed: &[&str],
    ) -> Result<Option<String>, Result<ServerResponse, WebHostError>> {
        let payload = request.payload();
        let Some(object) = payload.as_object() else {
            return Err(self.failure(
                request,
                "invalid-payload",
                "Weixin RPC payload must be an object",
                json!({}),
            ));
        };
        if let Some(unknown) = object
            .keys()
            .find(|key| !allowed.iter().any(|allowed| allowed == key))
        {
            return Err(self.failure(
                request,
                "invalid-payload",
                "Weixin RPC payload contains an unknown field",
                json!({ "field": unknown }),
            ));
        }
        match optional_weixin_string(payload, "sessionId") {
            Ok(target) => Ok(target),
            Err(message) => Err(self.failure(
                request,
                "invalid-payload",
                &message,
                json!({ "field": "sessionId" }),
            )),
        }
    }

    fn invoke_weixin_route(
        &mut self,
        request: &ClientRequest,
        target: Option<&str>,
        operation: &str,
        payload: Value,
    ) -> Result<ServerResponse, WebHostError> {
        if !self.weixin_route_available() {
            return self.management_unavailable(
                request,
                "weixin",
                "weixin plugin is disabled or unavailable",
            );
        }
        let runtime = match self.weixin_session_mut(target) {
            Ok(session) => session.invoke_weixin(operation, &payload),
            Err(error) => Err(error),
        };
        let runtime = match runtime {
            Ok(value) => value,
            Err(error) => return self.management_unavailable(request, "weixin", error),
        };
        let mode = runtime.get("mode").cloned().unwrap_or(Value::Null);
        let report = runtime.get("report").cloned().unwrap_or(Value::Null);
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/weixin-updated",
                "operation": operation,
                "sessionId": target,
                "mode": mode,
            }),
        )?;
        self.success(
            request,
            json!({
                "schemaVersion": 1,
                "source": "yunxi-weixin",
                "sessionId": target,
                "operation": operation,
                "mode": runtime.get("mode"),
                "report": report,
                "runtime": runtime,
            }),
        )
    }

    fn memory_route_available(&self) -> bool {
        (self.session.is_available() && self.session.memory_web_available())
            || self
                .sessions
                .values()
                .any(|state| state.session.is_available() && state.session.memory_web_available())
    }

    fn memory_session_mut(&mut self) -> Result<&mut ChatSession, String> {
        if self.session.is_available() && self.session.memory_web_available() {
            return Ok(&mut self.session);
        }
        if let Some(state) = self
            .sessions
            .values_mut()
            .find(|state| state.session.is_available() && state.session.memory_web_available())
        {
            return Ok(&mut state.session);
        }
        Err("no available Web session can access Memory".to_string())
    }

    fn persona_route_available(&self) -> bool {
        (self.session.is_available() && self.session.persona_web_available())
            || self
                .sessions
                .values()
                .any(|state| state.session.is_available() && state.session.persona_web_available())
    }

    fn persona_session_mut(&mut self) -> Result<&mut ChatSession, String> {
        if self.session.is_available() && self.session.persona_web_available() {
            return Ok(&mut self.session);
        }
        if let Some(state) = self
            .sessions
            .values_mut()
            .find(|state| state.session.is_available() && state.session.persona_web_available())
        {
            return Ok(&mut state.session);
        }
        Err("no available Web session can access Persona".to_string())
    }

    fn voice_route_available(&self) -> bool {
        (self.session.is_available() && self.session.voice_web_available())
            || self
                .sessions
                .values()
                .any(|state| state.session.is_available() && state.session.voice_web_available())
    }

    fn voice_session_mut(&mut self, session_id: Option<&str>) -> Result<&mut ChatSession, String> {
        if let Some(session_id) = session_id {
            let state = self
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| format!("session `{session_id}` was not found"))?;
            if !state.session.is_available() {
                return Err(format!("session `{session_id}` is busy"));
            }
            if !state.session.voice_web_available() {
                return Err("voice plugin is disabled or unavailable".to_string());
            }
            return Ok(&mut state.session);
        }
        if self.session.is_available() && self.session.voice_web_available() {
            return Ok(&mut self.session);
        }
        if let Some(state) = self
            .sessions
            .values_mut()
            .find(|state| state.session.is_available() && state.session.voice_web_available())
        {
            return Ok(&mut state.session);
        }
        Err("no available Web session can access Voice".to_string())
    }

    fn weixin_route_available(&self) -> bool {
        (self.session.is_available() && self.session.weixin_web_available())
            || self
                .sessions
                .values()
                .any(|state| state.session.is_available() && state.session.weixin_web_available())
    }

    fn weixin_session_mut(&mut self, session_id: Option<&str>) -> Result<&mut ChatSession, String> {
        if let Some(session_id) = session_id {
            let state = self
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| format!("session `{session_id}` was not found"))?;
            if !state.session.is_available() {
                return Err(format!("session `{session_id}` is busy"));
            }
            if !state.session.weixin_web_available() {
                return Err("weixin plugin is disabled or unavailable".to_string());
            }
            return Ok(&mut state.session);
        }
        if self.session.is_available() && self.session.weixin_web_available() {
            return Ok(&mut self.session);
        }
        if let Some(state) = self
            .sessions
            .values_mut()
            .find(|state| state.session.is_available() && state.session.weixin_web_available())
        {
            return Ok(&mut state.session);
        }
        Err("no available Web session can access Weixin".to_string())
    }

    fn mailbox_session_mut(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<&mut ChatSession, String> {
        if let Some(session_id) = session_id {
            let state = self
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| format!("session `{session_id}` was not found"))?;
            if !state.session.is_available() {
                return Err(format!("session `{session_id}` is busy"));
            }
            return Ok(&mut state.session);
        }
        if self.session.is_available() {
            return Ok(&mut self.session);
        }
        self.sessions
            .values_mut()
            .find(|state| state.session.is_available())
            .map(|state| &mut *state.session)
            .ok_or_else(|| "no available Web session can access the mailbox".to_string())
    }

    fn management_unavailable(
        &self,
        request: &ClientRequest,
        capability: &str,
        error: impl Into<String>,
    ) -> Result<ServerResponse, WebHostError> {
        let error = error.into();
        let disabled = error.contains("disabled") || error.contains("unavailable");
        self.failure(
            request,
            if disabled {
                "plugin-disabled"
            } else {
                "plugin-failed"
            },
            if disabled {
                "the requested capability is disabled or unavailable"
            } else {
                "the requested capability could not complete the operation"
            },
            json!({
                "capability": capability,
                "message": bounded_message(error),
            }),
        )
    }

    fn dispatch_session_create(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "session.create payload must be an object",
                json!({}),
            );
        }
        if self.sessions.len() >= MAX_WEB_SESSIONS {
            return self.failure(
                request,
                "session-limit",
                "the WebHost session limit has been reached",
                json!({ "limit": MAX_WEB_SESSIONS }),
            );
        }
        let mut session = match ChatSession::launch_with_options(&self.session_options) {
            Ok(session) => session,
            Err(error) => return self.session_failure(request, None, error.to_string()),
        };
        let snapshot = match session.create_web_session() {
            Ok(session) => session,
            Err(error) => return self.session_failure(request, None, error),
        };
        let session_id = snapshot.id().to_string();
        self.sessions.insert(
            session_id.clone(),
            WebSessionState::new(session, snapshot.messages().to_vec()),
        );
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/session-added",
                "sessionId": session_id,
                "blank": true,
                "cwd": snapshot.cwd(),
            }),
        )?;
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "session/subscribed",
                "sessionId": session_id,
                "lastSeq": -1,
            }),
        )?;
        self.success(request, json!({ "sessionId": session_id }))
    }

    fn dispatch_session_history(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.history requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let before_seq = optional_u64(request.payload(), "beforeSeq");
        let after_seq = optional_u64(request.payload(), "afterSeq");
        let raw_maximum = optional_u64(request.payload(), "maxMessages");
        let maximum = raw_maximum.unwrap_or(50).clamp(1, 128) as usize;
        if request.payload().get("beforeSeq").is_some() && before_seq.is_none() {
            return self.failure(
                request,
                "invalid-payload",
                "session.history beforeSeq must be a non-negative integer",
                json!({ "field": "beforeSeq" }),
            );
        }
        if request.payload().get("afterSeq").is_some() && after_seq.is_none() {
            return self.failure(
                request,
                "invalid-payload",
                "session.history afterSeq must be a non-negative integer",
                json!({ "field": "afterSeq" }),
            );
        }
        if before_seq.is_some() && after_seq.is_some() {
            return self.failure(
                request,
                "invalid-payload",
                "session.history cannot combine beforeSeq and afterSeq",
                json!({ "fields": ["beforeSeq", "afterSeq"] }),
            );
        }
        if request.payload().get("maxMessages").is_some()
            && !raw_maximum.is_some_and(|value| value > 0)
        {
            return self.failure(
                request,
                "invalid-payload",
                "session.history maxMessages must be a positive integer",
                json!({ "field": "maxMessages" }),
            );
        }
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let state = self
            .sessions
            .get(&session_id)
            .expect("ensure_web_session inserts the requested session");
        let (events, has_more, replay_gap) = if let Some(after_seq) = after_seq {
            replay_page_from_events_with_latest(
                &state.events,
                after_seq,
                maximum,
                state.next_event_seq.checked_sub(1),
            )
        } else {
            let (events, has_more) = history_page_from_events(&state.events, before_seq, maximum);
            (events, has_more, false)
        };
        let oldest_seq = state
            .events
            .first()
            .and_then(|event| event.get("seq"))
            .and_then(Value::as_u64);
        let latest_seq = state.next_event_seq.checked_sub(1);
        self.success(
            request,
            json!({
                "events": events,
                "hasMore": has_more,
                "replayGap": replay_gap,
                "oldestSeq": oldest_seq,
                "latestSeq": latest_seq,
            }),
        )
    }

    fn dispatch_session_search(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "session.search payload must be an object",
                json!({}),
            );
        }
        let query = request
            .payload()
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() || query.chars().count() > MAX_WEB_SEARCH_CHARS {
            return self.failure(
                request,
                "invalid-payload",
                "session.search query must contain between 1 and 500 characters",
                json!({
                    "minimumCharacters": 1,
                    "maximumCharacters": MAX_WEB_SEARCH_CHARS,
                }),
            );
        }
        let query_lower = query.to_ascii_lowercase();
        let listed = match self.list_web_sessions(true, 200) {
            Ok(result) => result,
            Err(error) => return self.session_failure(request, None, error),
        };
        let mut items = Vec::new();
        for summary in listed.sessions() {
            let mut snippet = summary.title().to_string();
            let mut matched = query_lower.is_empty()
                || summary.id().to_ascii_lowercase().contains(&query_lower)
                || summary.title().to_ascii_lowercase().contains(&query_lower);
            if let Some(state) = self.sessions.get(summary.id()) {
                if let Some(message) = state.history.iter().find(|message| {
                    query_lower.is_empty()
                        || message
                            .content()
                            .to_ascii_lowercase()
                            .contains(&query_lower)
                }) {
                    if !query_lower.is_empty() {
                        matched = true;
                    }
                    snippet = message.content().chars().take(240).collect();
                }
            }
            if matched {
                items.push(json!({
                    "sessionId": summary.id(),
                    "snippet": snippet.chars().take(240).collect::<String>(),
                }));
            }
        }
        let has_more = items.len() > 20;
        items.truncate(20);
        self.success(request, json!({ "items": items, "hasMore": has_more }))
    }

    fn dispatch_session_rename(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.rename requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let Some(title) = string_field(request.payload(), "title") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.rename requires a non-empty title",
                json!({ "field": "title" }),
            );
        };
        if title.chars().count() > 240 {
            return self.failure(
                request,
                "invalid-payload",
                "session.rename title is too long",
                json!({ "maximumCharacters": 240 }),
            );
        }
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("ensure_web_session inserts the requested session");
        if state.is_running() || state.is_pending_approval() {
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "agent-busy",
                "cannot rename a session while it is running",
                json!({ "sessionId": session_id }),
            );
        }
        let mutation = SessionMutationRequest::new(
            WorkspaceGrant::read_write(state.session.workspace_root()),
            &session_id,
            SessionMutation::Rename,
        )
        .with_title(&title);
        let result = match state.session.mutate_web_session(&mutation) {
            Ok(result) => result,
            Err(error) => {
                self.sessions.insert(session_id.clone(), state);
                return self.session_failure(request, Some(&session_id), error);
            }
        };
        let Some(snapshot) = result.session() else {
            self.sessions.insert(session_id.clone(), state);
            return self.session_failure(
                request,
                Some(&session_id),
                format!("session `{session_id}` was not found"),
            );
        };
        let title = snapshot.title().to_string();
        let seq = state.next_event_seq;
        self.sessions.insert(session_id.clone(), state);
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/session-updated",
                "sessionId": session_id,
                "title": title,
            }),
        )?;
        self.success(request, json!({ "title": title, "seq": seq }))
    }

    fn dispatch_session_fork(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.fork requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let at_seq = if request.payload().get("atSeq").is_some() {
            match request.payload().get("atSeq").and_then(Value::as_u64) {
                Some(value) => Some(value),
                None => {
                    return self.failure(
                        request,
                        "invalid-payload",
                        "session.fork atSeq must be a non-negative integer",
                        json!({ "field": "atSeq" }),
                    );
                }
            }
        } else {
            None
        };
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("ensure_web_session inserts the requested session");
        if state.is_running() || state.is_pending_approval() {
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "agent-busy",
                "cannot fork a session while it is running",
                json!({ "sessionId": session_id }),
            );
        }
        let at_message = match at_seq {
            Some(sequence) => match message_count_at_seq(&state.events, sequence) {
                Ok(count) => Some(count),
                Err(error) => {
                    self.sessions.insert(session_id.clone(), state);
                    return self.failure(
                        request,
                        "invalid-payload",
                        &error,
                        json!({ "field": "atSeq", "sessionId": session_id }),
                    );
                }
            },
            None => None,
        };
        let mut mutation = SessionMutationRequest::new(
            WorkspaceGrant::read_write(state.session.workspace_root()),
            &session_id,
            SessionMutation::Fork,
        );
        if let Some(at_message) = at_message {
            mutation = mutation.with_at_message(at_message);
        }
        let result = match state.session.mutate_web_session(&mutation) {
            Ok(result) => result,
            Err(error) => {
                self.sessions.insert(session_id.clone(), state);
                return self.session_failure(request, Some(&session_id), error);
            }
        };
        let Some(snapshot) = result.session() else {
            self.sessions.insert(session_id.clone(), state);
            return self.session_failure(
                request,
                Some(&session_id),
                format!("session `{session_id}` was not found"),
            );
        };
        let fork_id = snapshot.id().to_string();
        let mut fork_session = match ChatSession::launch_with_options(&self.session_options) {
            Ok(session) => session,
            Err(error) => {
                self.sessions.insert(session_id.clone(), state);
                return self.session_failure(request, Some(&fork_id), error.to_string());
            }
        };
        let messages = match fork_session.activate_web_session(&fork_id) {
            Ok(messages) => messages,
            Err(error) => {
                self.sessions.insert(session_id.clone(), state);
                return self.session_failure(request, Some(&fork_id), error);
            }
        };
        self.sessions.insert(session_id.clone(), state);
        self.sessions.insert(
            fork_id.clone(),
            WebSessionState::new(fork_session, messages),
        );
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/session-added",
                "sessionId": fork_id,
                "parentSessionId": session_id,
                "blank": snapshot.messages().is_empty(),
                "cwd": snapshot.cwd(),
            }),
        )?;
        self.success(request, json!({ "sessionId": fork_id }))
    }

    fn dispatch_session_select_model(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.selectModel requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let Some(provider) = string_field(request.payload(), "provider") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.selectModel requires a provider",
                json!({ "field": "provider" }),
            );
        };
        let Some(model) = string_field(request.payload(), "model") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.selectModel requires a model",
                json!({ "field": "model" }),
            );
        };
        if let Some(effort) = request.payload().get("reasoningEffort") {
            let valid = effort.as_str().is_some_and(|value| {
                matches!(
                    value,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                )
            });
            if !valid {
                return self.failure(
                    request,
                    "invalid-payload",
                    "session.selectModel reasoningEffort is unsupported",
                    json!({ "field": "reasoningEffort" }),
                );
            }
        }
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("ensure_web_session inserts the requested session");
        if state.is_running() || state.is_pending_approval() {
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "agent-busy",
                "cannot select a model while the session is running",
                json!({ "sessionId": session_id }),
            );
        }
        let current_provider = state
            .session
            .0
            .as_ref()
            .expect("session is available")
            .provider()
            .to_string();
        let current_model = state
            .session
            .0
            .as_ref()
            .expect("session is available")
            .model()
            .to_string();
        if provider != current_provider || model != current_model {
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "model-not-supported",
                "this Host exposes only its configured provider and model",
                json!({
                    "available": [{ "provider": current_provider, "model": current_model }],
                }),
            );
        }
        let mutation = SessionMutationRequest::new(
            WorkspaceGrant::read_write(state.session.workspace_root()),
            &session_id,
            SessionMutation::SelectModel,
        )
        .with_provider(provider.clone())
        .with_model(model.clone());
        if let Err(error) = state.session.mutate_web_session(&mutation) {
            self.sessions.insert(session_id.clone(), state);
            return self.session_failure(request, Some(&session_id), error);
        }
        self.sessions.insert(session_id, state);
        let mut selected = json!({
            "provider": provider,
            "model": model,
        });
        if let Some(effort) = request
            .payload()
            .get("reasoningEffort")
            .and_then(Value::as_str)
        {
            selected
                .as_object_mut()
                .expect("selected model is a JSON object")
                .insert(
                    "reasoningEffort".to_string(),
                    Value::String(effort.to_string()),
                );
        }
        self.success(request, json!({ "selected": selected }))
    }

    fn dispatch_session_update_queue(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.updateQueue requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let Some(item_id) = string_field(request.payload(), "itemId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.updateQueue requires a string itemId",
                json!({ "field": "itemId" }),
            );
        };
        let Some(action) = request.payload().get("action").and_then(Value::as_object) else {
            return self.failure(
                request,
                "invalid-payload",
                "session.updateQueue requires an action object",
                json!({ "field": "action" }),
            );
        };
        let Some(kind) = action.get("kind").and_then(Value::as_str) else {
            return self.failure(
                request,
                "invalid-payload",
                "session.updateQueue action requires kind",
                json!({ "field": "action.kind" }),
            );
        };
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("ensure_web_session inserts the requested session");
        let Some(index) = state
            .queued_prompts
            .iter()
            .position(|item| item.item_id == item_id)
        else {
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "queue-item-not-found",
                "the requested queued prompt does not exist",
                json!({ "sessionId": session_id, "itemId": item_id }),
            );
        };
        match kind {
            "edit" => {
                let Some(content) = action.get("content").and_then(Value::as_str) else {
                    self.sessions.insert(session_id.clone(), state);
                    return self.failure(
                        request,
                        "invalid-payload",
                        "queue edit requires string content",
                        json!({ "field": "action.content" }),
                    );
                };
                if content.trim().is_empty() || content.chars().count() > MAX_WEB_PROMPT_CHARS {
                    self.sessions.insert(session_id.clone(), state);
                    return self.failure(
                        request,
                        "invalid-payload",
                        "queue edit content is empty or too long",
                        json!({ "maximumCharacters": MAX_WEB_PROMPT_CHARS }),
                    );
                }
                state.queued_prompts[index].content = content.to_string();
            }
            "remove" => {
                state.queued_prompts.remove(index);
            }
            "steer" => {
                self.sessions.insert(session_id.clone(), state);
                return self.failure(
                    request,
                    "mode-not-supported",
                    "queue steering is not enabled while preserving turn ordering",
                    json!({ "kind": "steer" }),
                );
            }
            _ => {
                self.sessions.insert(session_id.clone(), state);
                return self.failure(
                    request,
                    "invalid-payload",
                    "unknown queue action kind",
                    json!({ "kind": kind }),
                );
            }
        }
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "session/queue-updated",
                "sessionId": session_id,
                "itemId": item_id,
                "action": kind,
            }),
        )?;
        self.sessions.insert(session_id, state);
        self.success(request, json!({ "accepted": true }))
    }

    fn dispatch_session_attachment(
        &self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if !request.payload().is_object() {
            return self.failure(
                request,
                "invalid-payload",
                "session.attachment payload must be an object",
                json!({}),
            );
        }
        self.failure(
            request,
            "attachment-not-supported",
            "this Rust Host does not expose a local attachment store yet",
            json!({ "supportedMedia": [], "canRetry": false }),
        )
    }

    fn list_web_sessions(
        &mut self,
        include_archived: bool,
        limit: usize,
    ) -> Result<SessionListResult, String> {
        if self.session.is_available() {
            return self.session.list_web_sessions(include_archived, limit);
        }
        for state in self.sessions.values_mut() {
            if state.session.is_available() {
                return state.session.list_web_sessions(include_archived, limit);
            }
        }
        Err("session storage is disabled or unavailable".to_string())
    }

    fn dispatch_subagent_list(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(parent_session_id) = string_field(request.payload(), "parentSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.list requires a string parentSessionId",
                json!({ "field": "parentSessionId" }),
            );
        };
        let Some(parent_state) = self.sessions.get(parent_session_id.as_str()) else {
            return self.success(request, json!({ "entries": [], "parentAvailable": false }));
        };
        if !parent_state.session.multi_agent_web_available() {
            return self.success(
                request,
                json!({
                    "entries": [],
                    "parentAvailable": true,
                }),
            );
        }

        let context = match self.subagent_context(&parent_session_id) {
            Ok(context) => context,
            Err(_error) => {
                return self.failure(
                    request,
                    "subagent-parent-unavailable",
                    "the multi-agent catalog is unavailable",
                    json!({ "parentSessionId": parent_session_id }),
                );
            }
        };
        let Some((_root_session_id, parent_agent_id, graph)) = context else {
            return self.success(request, json!({ "entries": [], "parentAvailable": false }));
        };
        let entries = graph
            .agents()
            .iter()
            .filter(|agent| agent.parent_id() == parent_agent_id)
            .map(|agent| {
                let has_children = graph
                    .agents()
                    .iter()
                    .any(|child| child.parent_id() == agent.id());
                json!({
                    "kind": "child",
                    "id": agent.id(),
                    "mode": "continuable",
                    "label": agent.name(),
                    "activity": if self.subagent_jobs.contains_key(agent.id())
                        || agent.status() == AgentStatus::Running
                    {
                        "running"
                    } else {
                        "inactive"
                    },
                    "hasChildren": has_children,
                })
            })
            .collect::<Vec<_>>();
        self.success(
            request,
            json!({
                "entries": entries,
                "parentAvailable": true,
            }),
        )
    }

    fn dispatch_subagent_history(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(parent_session_id) = string_field(request.payload(), "parentSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.history requires a string parentSessionId",
                json!({ "field": "parentSessionId" }),
            );
        };
        let Some(child_session_id) = string_field(request.payload(), "childSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.history requires a string childSessionId",
                json!({ "field": "childSessionId" }),
            );
        };
        if let Some(mode) = request.payload().get("mode").and_then(Value::as_str)
            && !matches!(mode, "one-shot" | "continuable")
        {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.history mode must be one-shot or continuable",
                json!({ "childSessionId": child_session_id }),
            );
        }
        let before_seq = optional_u64(request.payload(), "beforeSeq");
        let raw_maximum = optional_u64(request.payload(), "maxMessages");
        let maximum = raw_maximum.unwrap_or(50).clamp(1, 128) as usize;
        if request.payload().get("beforeSeq").is_some() && before_seq.is_none() {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.history beforeSeq must be a non-negative integer",
                json!({ "field": "beforeSeq" }),
            );
        }
        if request.payload().get("maxMessages").is_some()
            && !raw_maximum.is_some_and(|value| value > 0)
        {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.history maxMessages must be a positive integer",
                json!({ "field": "maxMessages" }),
            );
        }
        let Some(parent_state) = self.sessions.get(parent_session_id.as_str()) else {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested parent session does not exist",
                json!({ "parentSessionId": parent_session_id }),
            );
        };
        if !parent_state.session.multi_agent_web_available() {
            return self.failure(
                request,
                "subagent-catalog-diagnostic",
                "the multi-agent coordinator is disabled or unavailable",
                json!({
                    "parentSessionId": parent_session_id,
                    "childSessionId": child_session_id,
                    "reason": "unavailable",
                }),
            );
        }

        let context = match self.subagent_context(&parent_session_id) {
            Ok(context) => context,
            Err(_error) => {
                return self.failure(
                    request,
                    "subagent-catalog-diagnostic",
                    "the multi-agent history is unavailable",
                    json!({
                        "parentSessionId": parent_session_id,
                        "childSessionId": child_session_id,
                        "reason": "unavailable",
                    }),
                );
            }
        };
        let Some((root_session_id, parent_agent_id, graph)) = context else {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested parent session does not exist",
                json!({
                    "parentSessionId": parent_session_id,
                    "childSessionId": child_session_id,
                }),
            );
        };
        let is_direct_child = graph
            .agents()
            .iter()
            .any(|agent| agent.id() == child_session_id && agent.parent_id() == parent_agent_id);
        if !is_direct_child {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested child session does not exist under this parent",
                json!({
                    "parentSessionId": parent_session_id,
                    "childSessionId": child_session_id,
                }),
            );
        }

        let inspected = match if let Some(runtime) = self.multi_agent_runtimes.get(&root_session_id)
        {
            let grant = self
                .sessions
                .get(&root_session_id)
                .expect("subagent context root is registered")
                .session
                .web_agent_read_grant(&root_session_id)
                .map_err(|error| error.to_string());
            grant.and_then(|grant| {
                AgentInspectRequest::new(grant, &child_session_id)
                    .map_err(|error| error.to_string())
                    .and_then(|inspect| {
                        runtime
                            .store()
                            .inspect(&inspect)
                            .map_err(|error| error.to_string())
                    })
            })
        } else {
            self.sessions
                .get_mut(&root_session_id)
                .expect("subagent context root is registered")
                .session
                .web_agent_inspect(&root_session_id, &child_session_id)
        } {
            Ok(inspected) => inspected,
            Err(_error) => {
                return self.failure(
                    request,
                    "subagent-catalog-diagnostic",
                    "the child session history is unavailable",
                    json!({
                        "parentSessionId": parent_session_id,
                        "childSessionId": child_session_id,
                        "reason": "unavailable",
                    }),
                );
            }
        };
        let messages = inspected
            .transcript()
            .iter()
            .map(|entry| match entry.role() {
                AgentTranscriptRole::User => ChatMessage::user(entry.content()),
                AgentTranscriptRole::Assistant => ChatMessage::assistant(entry.content()),
            })
            .collect::<Vec<_>>();
        let (events, has_more) = history_page(&messages, before_seq, maximum);
        self.success(
            request,
            json!({
                "events": events,
                "hasMore": has_more,
            }),
        )
    }

    fn dispatch_subagent_prompt(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(parent_session_id) = string_field(request.payload(), "parentSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.prompt requires a string parentSessionId",
                json!({ "field": "parentSessionId" }),
            );
        };
        let Some(child_session_id) = string_field(request.payload(), "childSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.prompt requires a string childSessionId",
                json!({ "field": "childSessionId" }),
            );
        };
        if request.payload().get("mode").and_then(Value::as_str) != Some("continuable") {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.prompt mode must be continuable",
                json!({ "field": "mode" }),
            );
        }
        let Some(content) = prompt_text(request.payload()) else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.prompt requires non-empty text content",
                json!({ "field": "content" }),
            );
        };
        let Some((root_session_id, parent_agent_id, graph)) =
            self.subagent_context(&parent_session_id).map_err(|error| {
                WebHostError::Session(format!("multi-agent catalog unavailable: {error}"))
            })?
        else {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested parent session does not exist",
                json!({ "parentSessionId": parent_session_id }),
            );
        };
        let direct_child = graph
            .agents()
            .iter()
            .any(|agent| agent.id() == child_session_id && agent.parent_id() == parent_agent_id);
        if !direct_child {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested child session does not exist under this parent",
                json!({
                    "parentSessionId": parent_session_id,
                    "childSessionId": child_session_id,
                }),
            );
        }
        if self.subagent_jobs.contains_key(&child_session_id) {
            return self.failure(
                request,
                "agent-busy",
                "the child agent already has a running turn",
                json!({ "childSessionId": child_session_id }),
            );
        }
        if self.subagent_jobs.len() >= MAX_WEB_SUBAGENT_JOBS {
            return self.failure(
                request,
                "subagent-capacity",
                "the WebHost child-agent worker limit has been reached",
                json!({ "limit": MAX_WEB_SUBAGENT_JOBS }),
            );
        }
        let requested_model = request.payload().get("model").and_then(Value::as_str);
        let (authority, task, model, tool_grants) = {
            let root = self
                .sessions
                .get(&root_session_id)
                .expect("subagent context root is registered");
            if root.is_running() || root.is_pending_approval() {
                return self.failure(
                    request,
                    "agent-busy",
                    "the parent session is busy",
                    json!({ "parentSessionId": parent_session_id }),
                );
            }
            let model = requested_model
                .map(ToString::to_string)
                .unwrap_or_else(|| root.session.web_agent_model().to_string());
            match root.session.prepare_web_agent_runtime_with_model(
                &root_session_id,
                &child_session_id,
                &content,
                requested_model,
            ) {
                Ok((authority, task)) => {
                    let tool_grants = graph
                        .agents()
                        .iter()
                        .find(|agent| agent.id() == child_session_id)
                        .map(|agent| agent.child_grants().to_vec())
                        .unwrap_or_default();
                    (authority, task, model, tool_grants)
                }
                Err(error) => {
                    return self.failure(
                        request,
                        "subagent-failed",
                        "the child agent turn could not be started",
                        json!({
                            "childSessionId": child_session_id,
                            "message": bounded_message(error),
                        }),
                    );
                }
            }
        };
        let message_id = format!("subagent-message-{}", self.next_web_id);
        self.next_web_id = self.next_web_id.saturating_add(1);
        let runtime = self.runtime_for_root(&root_session_id, &authority)?;
        self.start_subagent_job(WebSubagentStart {
            task,
            authority,
            runtime,
            parent_session_id: parent_session_id.clone(),
            root_session_id,
            child_session_id: child_session_id.clone(),
            message_id: message_id.clone(),
            message: content,
            model,
            tool_grants,
            recovery: None,
        })?;
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "subagent/state",
                "parentSessionId": parent_session_id,
                "childSessionId": child_session_id,
                "messageId": message_id,
                "state": "running",
            }),
        )?;
        self.success(
            request,
            json!({ "messageId": message_id, "accepted": true, "running": true }),
        )
    }

    fn dispatch_subagent_interrupt(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(parent_session_id) = string_field(request.payload(), "parentSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.interrupt requires a string parentSessionId",
                json!({ "field": "parentSessionId" }),
            );
        };
        let Some(child_session_id) = string_field(request.payload(), "childSessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.interrupt requires a string childSessionId",
                json!({ "field": "childSessionId" }),
            );
        };
        if request.payload().get("mode").and_then(Value::as_str) != Some("continuable") {
            return self.failure(
                request,
                "invalid-payload",
                "subagent.interrupt mode must be continuable",
                json!({ "field": "mode" }),
            );
        }
        let Some((root_session_id, parent_agent_id, graph)) =
            self.subagent_context(&parent_session_id).map_err(|error| {
                WebHostError::Session(format!("multi-agent catalog unavailable: {error}"))
            })?
        else {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested parent session does not exist",
                json!({ "parentSessionId": parent_session_id }),
            );
        };
        if !graph
            .agents()
            .iter()
            .any(|agent| agent.id() == child_session_id && agent.parent_id() == parent_agent_id)
        {
            return self.failure(
                request,
                "subagent-not-found",
                "the requested child session does not exist under this parent",
                json!({ "childSessionId": child_session_id }),
            );
        }
        if let Some(job) = self.subagent_jobs.get(&child_session_id)
            && job.parent_session_id == parent_session_id
        {
            if let Err(error) = job.handle.interrupt(&job.authority) {
                return self.failure(
                    request,
                    "subagent-interrupt-failed",
                    "the child agent could not be interrupted",
                    json!({
                        "childSessionId": child_session_id,
                        "message": bounded_message(error.to_string()),
                    }),
                );
            }
            let _ = self.publish_event(
                EventChannel::Mux,
                json!({
                    "type": "subagent/state",
                    "parentSessionId": parent_session_id,
                    "childSessionId": child_session_id,
                    "messageId": job.message_id,
                    "state": "cancelling",
                }),
            );
            return self.success(
                request,
                json!({
                    "accepted": true,
                    "cancellationRequested": true,
                    "coordinatorAccepted": true,
                }),
            );
        }
        let mut root = self
            .sessions
            .remove(&root_session_id)
            .expect("subagent context root is registered");
        let result = root
            .session
            .web_agent_interrupt(&root_session_id, &child_session_id);
        self.sessions.insert(root_session_id, root);
        match result {
            Ok(()) => self.success(request, json!({ "accepted": true })),
            Err(error) => self.failure(
                request,
                "subagent-interrupt-failed",
                "the child agent could not be interrupted",
                json!({ "childSessionId": child_session_id, "message": bounded_message(error) }),
            ),
        }
    }

    fn subagent_context(
        &mut self,
        parent_session_id: &str,
    ) -> Result<Option<(String, String, AgentListResult)>, String> {
        let Some(root_session_id) = self
            .sessions
            .keys()
            .find(|id| *id == parent_session_id)
            .cloned()
        else {
            return Ok(None);
        };
        let graph = if let Some(runtime) = self.multi_agent_runtimes.get(&root_session_id) {
            runtime.store().list().map_err(|error| error.to_string())?
        } else {
            self.sessions
                .get_mut(&root_session_id)
                .expect("root session is registered")
                .session
                .web_agent_graph(&root_session_id)?
        };
        if parent_session_id == root_session_id {
            return Ok(Some((root_session_id, ROOT_AGENT_ID.to_string(), graph)));
        }
        if graph
            .agents()
            .iter()
            .any(|agent| agent.id() == parent_session_id)
        {
            return Ok(Some((
                root_session_id,
                parent_session_id.to_string(),
                graph,
            )));
        }
        Ok(None)
    }

    fn runtime_for_root(
        &mut self,
        root_session_id: &str,
        authority: &AgentDelegationGrant,
    ) -> Result<AsyncMultiAgentRuntime, WebHostError> {
        if let Some(runtime) = self.multi_agent_runtimes.get(root_session_id) {
            return Ok(runtime.clone());
        }
        let store =
            CoordinatorStore::from_grant(authority, format!("web-host-{}", std::process::id()))
                .map_err(|error| WebHostError::Session(error.to_string()))?;
        let runtime = AsyncMultiAgentRuntime::new(store);
        self.multi_agent_runtimes
            .insert(root_session_id.to_string(), runtime.clone());
        self.recover_persisted_workers(root_session_id, authority, &runtime)?;
        Ok(runtime)
    }

    /// Reattach coordinator records that survived a Web process restart.
    ///
    /// The coordinator owns the durable state transition; this layer only
    /// recreates the stateless model adapter and bounded event queue needed to
    /// surface the resumed branch through the existing Web multiplex channel.
    fn recover_persisted_workers(
        &mut self,
        root_session_id: &str,
        authority: &AgentDelegationGrant,
        runtime: &AsyncMultiAgentRuntime,
    ) -> Result<(), WebHostError> {
        let recoveries = runtime.recover_workers().map_err(|error| {
            WebHostError::Session(format!("multi-agent recovery failed: {error}"))
        })?;
        if recoveries.is_empty() {
            return Ok(());
        }
        let (task, model) = {
            let root = self.sessions.get(root_session_id).ok_or_else(|| {
                WebHostError::Session("multi-agent recovery root is missing".to_string())
            })?;
            (
                root.session.web_agent_task(),
                root.session.web_agent_model().to_string(),
            )
        };
        for recovered in recoveries {
            let recovery = match recovered {
                Ok(recovery) => recovery,
                Err(error) => {
                    self.publish_subagent_event(json!({
                        "type": "subagent/error",
                        "rootSessionId": root_session_id,
                        "message": bounded_message(error.to_string()),
                        "recovery": true,
                    }))?;
                    continue;
                }
            };
            let child_session_id = recovery.agent_id().to_string();
            if self.subagent_jobs.contains_key(&child_session_id) {
                continue;
            }
            let parent_session_id = if recovery.agent().parent_id() == ROOT_AGENT_ID {
                root_session_id.to_string()
            } else {
                recovery.agent().parent_id().to_string()
            };
            let message = recovery
                .transcript()
                .last()
                .map(|entry| entry.content().to_string())
                .unwrap_or_default();
            if message.trim().is_empty() {
                self.publish_subagent_event(json!({
                    "type": "subagent/error",
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "message": "recovered worker has no resumable user message",
                    "recovery": true,
                }))?;
                continue;
            }
            let message_id = format!(
                "recovered-subagent-{}-{}",
                child_session_id, self.next_web_id
            );
            self.next_web_id = self.next_web_id.saturating_add(1);
            let tool_grants = recovery.agent().child_grants().to_vec();
            let selected_model = recovery.model().unwrap_or(model.as_str()).to_string();
            let start = WebSubagentStart {
                task: task.clone(),
                authority: authority.clone(),
                runtime: runtime.clone(),
                parent_session_id: parent_session_id.clone(),
                root_session_id: root_session_id.to_string(),
                child_session_id: child_session_id.clone(),
                message_id: message_id.clone(),
                message,
                model: selected_model.clone(),
                tool_grants,
                recovery: Some(recovery),
            };
            match self.start_subagent_job(start) {
                Ok(()) => self.publish_subagent_event(json!({
                    "type": "subagent/state",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "state": "running",
                    "recovered": true,
                    "model": selected_model,
                }))?,
                Err(error) => self.publish_subagent_event(json!({
                    "type": "subagent/error",
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "message": bounded_message(error.to_string()),
                    "recovery": true,
                }))?,
            }
        }
        Ok(())
    }

    fn dispatch_session_prompt(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.prompt requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let Some(content) = prompt_text(request.payload()) else {
            return self.failure(
                request,
                "invalid-payload",
                "session.prompt requires non-empty text content",
                json!({ "field": "content" }),
            );
        };
        let mode = request.payload().get("mode").and_then(Value::as_str);
        if !matches!(mode, Some("queue") | Some("steer")) {
            return self.failure(
                request,
                "invalid-payload",
                "session.prompt mode must be queue or steer",
                json!({ "field": "mode" }),
            );
        }
        if mode == Some("steer") {
            return self.failure(
                request,
                "mode-not-supported",
                "session.prompt steer mode is not enabled by this WebHost",
                json!({ "mode": "steer" }),
            );
        }
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("ensure_web_session inserts the requested session");
        if state.is_running() || state.is_pending_approval() {
            if mode == Some("queue") && !state.is_pending_approval() {
                if state.queued_prompts.len() >= MAX_WEB_QUEUED_PROMPTS {
                    self.sessions.insert(session_id.clone(), state);
                    return self.failure(
                        request,
                        "queue-full",
                        "the session prompt queue is full",
                        json!({
                            "sessionId": session_id,
                            "limit": MAX_WEB_QUEUED_PROMPTS,
                        }),
                    );
                }
                let item_id = format!("queue-{}", state.next_queue_id);
                state.next_queue_id = state.next_queue_id.saturating_add(1);
                state.queued_prompts.push_back(QueuedPrompt {
                    item_id: item_id.clone(),
                    content,
                });
                self.publish_event(
                    EventChannel::Mux,
                    json!({
                        "type": "session/queue-updated",
                        "sessionId": session_id,
                        "itemId": item_id,
                        "action": "queued",
                    }),
                )?;
                self.sessions.insert(session_id, state);
                return self.success(
                    request,
                    json!({ "accepted": true, "queued": true, "itemId": item_id }),
                );
            }
            self.sessions.insert(session_id.clone(), state);
            return self.failure(
                request,
                "agent-busy",
                "resolve the pending approval before sending another prompt",
                json!({ "sessionId": session_id }),
            );
        }

        let previous_length = state.history.len();
        state.history.push(ChatMessage::user(content));
        if let Err(error) = self.publish_turn_opening(&mut state, previous_length, &session_id) {
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        if let Err(error) = self.publish_session_status(&session_id, true) {
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        if let Err(error) =
            self.start_prompt_worker(&mut state, session_id.clone(), previous_length)
        {
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        self.sessions.insert(session_id.clone(), state);

        // Preserve the historical fast fixture path without coupling request
        // latency to a real provider. Slow turns remain detached and stream
        // through the Mux endpoint.
        let deadline = Instant::now() + WEB_FAST_PATH_WAIT;
        while self.session_is_running(&session_id) && Instant::now() < deadline {
            self.poll_running_turn(&session_id)?;
            if self.session_is_running(&session_id) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.success(request, json!({ "accepted": true }))
    }

    fn dispatch_session_cancel(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        let Some(session_id) = string_field(request.payload(), "sessionId") else {
            return self.failure(
                request,
                "invalid-payload",
                "session.cancel requires a string sessionId",
                json!({ "field": "sessionId" }),
            );
        };
        let mut state = match self.sessions.remove(&session_id) {
            Some(state) => state,
            None => {
                return self.failure(
                    request,
                    "session-not-running",
                    "the requested session is not running",
                    json!({ "sessionId": session_id }),
                );
            }
        };
        if let Some(running) = state.running_turn.as_ref() {
            running.cancellation.cancel("cancelled from the Web client");
            state.queued_prompts.clear();
            self.sessions.insert(session_id, state);
            return self.success(request, json!({ "accepted": true }));
        }
        if let Some(pending) = state.pending_approval.take() {
            if let Err(error) = state
                .session
                .abort_pending_model_tool("cancelled from the Web client")
            {
                state.pending_approval = Some(pending);
                self.sessions.insert(session_id, state);
                return Err(WebHostError::Session(error));
            }
            self.publish_event(
                EventChannel::Mux,
                json!({
                    "type": "approval/resolved",
                    "sessionId": pending.session_id,
                    "approvalId": pending.approval_id,
                    "outcome": "cancelled",
                }),
            )?;
            let turn = u64::try_from(state.history.len().saturating_sub(1) / 2).unwrap_or(u64::MAX);
            let step = self.current_web_step(&state, turn);
            self.publish_pending_session_event(
                &mut state,
                &session_id,
                PendingSessionEvent::retained(
                    "step/end",
                    json!({ "turn": turn, "step": step }),
                    false,
                ),
            )?;
            self.publish_pending_session_event(
                &mut state,
                &session_id,
                PendingSessionEvent::retained(
                    "turn/end",
                    json!({ "turn": turn, "reason": { "kind": "cancelled" } }),
                    false,
                ),
            )?;
        } else if !state.queued_prompts.is_empty() {
            state.queued_prompts.clear();
            self.publish_event(
                EventChannel::Mux,
                json!({
                    "type": "session/queue-updated",
                    "sessionId": session_id,
                    "action": "cleared",
                }),
            )?;
        }
        self.sessions.insert(session_id, state);
        self.success(request, json!({ "accepted": true }))
    }

    fn start_prompt_worker(
        &mut self,
        state: &mut WebSessionState,
        session_id: String,
        previous_length: usize,
    ) -> Result<(), WebHostError> {
        let (mut event_sender, events) =
            AgentEventChannel::new(WEB_STREAM_CAPACITY, BackpressureStrategy::DropNonTerminal)
                .map_err(|error| WebHostError::Session(error.to_string()))?;
        let (completion_sender, completion) = sync_channel(1);
        let cancellation = SpineCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let history = state.history.clone();
        let mut session = state.session.take();
        let worker = std::thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                ChatBackend::complete_streaming(
                    &mut session,
                    &history,
                    &worker_cancellation,
                    &mut event_sender,
                )
                .map_err(|error| error.to_string())
            }));
            drop(event_sender);
            let completion = match result {
                Ok(result) => WebWorkerCompletion {
                    session: Some(session),
                    result: WebWorkerResult::Prompt(result),
                },
                Err(_) => WebWorkerCompletion {
                    session: None,
                    result: WebWorkerResult::Prompt(Err(
                        "the isolated Web turn worker panicked".to_string()
                    )),
                },
            };
            let _ = completion_sender.send(completion);
        });
        let turn = u64::try_from(previous_length / 2).unwrap_or(u64::MAX);
        state.running_turn = Some(WebRunningTurn {
            kind: WebTurnKind::Prompt { previous_length },
            session_id,
            turn,
            current_step: 0,
            started_steps: BTreeSet::from([0]),
            text_blocks: BTreeSet::new(),
            cancellation,
            events,
            completion,
            worker: Some(worker),
            reported_drops: 0,
        });
        Ok(())
    }

    fn start_approval_worker(
        &mut self,
        state: &mut WebSessionState,
        session_id: String,
        approved: bool,
    ) -> Result<(), WebHostError> {
        let (mut event_sender, events) =
            AgentEventChannel::new(WEB_STREAM_CAPACITY, BackpressureStrategy::DropNonTerminal)
                .map_err(|error| WebHostError::Session(error.to_string()))?;
        let (completion_sender, completion) = sync_channel(1);
        let cancellation = SpineCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let mut session = state.session.take();
        let worker = std::thread::spawn(move || {
            eprintln!("yunxi web approval worker starting");
            let result = catch_unwind(AssertUnwindSafe(|| {
                session
                    .resolve_pending_model_tool_streaming(
                        approved,
                        &worker_cancellation,
                        &mut event_sender,
                    )
                    .map(|management| management.assistant_reply)
            }));
            eprintln!("yunxi web approval worker completed: {}", result.is_ok());
            drop(event_sender);
            let completion = match result {
                Ok(result) => WebWorkerCompletion {
                    session: Some(session),
                    result: WebWorkerResult::Approval(result),
                },
                Err(_) => WebWorkerCompletion {
                    session: None,
                    result: WebWorkerResult::Approval(Err(
                        "the isolated Web approval worker panicked".to_string(),
                    )),
                },
            };
            let _ = completion_sender.send(completion);
        });
        let turn = u64::try_from(state.history.len().saturating_sub(1) / 2).unwrap_or(u64::MAX);
        let current_step = self.current_web_step(state, turn);
        state.running_turn = Some(WebRunningTurn {
            kind: WebTurnKind::Approval,
            session_id,
            turn,
            current_step,
            started_steps: BTreeSet::from([current_step]),
            text_blocks: BTreeSet::new(),
            cancellation,
            events,
            completion,
            worker: Some(worker),
            reported_drops: 0,
        });
        Ok(())
    }

    fn current_web_step(&self, state: &WebSessionState, turn: u64) -> u16 {
        state
            .events
            .iter()
            .rev()
            .find(|event| {
                event_type(event) == Some("step/start")
                    && event["data"]["turn"].as_u64() == Some(turn)
            })
            .and_then(|event| event["data"]["step"].as_u64())
            .and_then(|step| u16::try_from(step).ok())
            .unwrap_or(0)
    }

    fn any_running(&self) -> bool {
        self.session.pending_approval().is_some()
            || self.sessions.values().any(WebSessionState::is_running)
            || !self.subagent_jobs.is_empty()
    }

    fn any_pending_approval(&self) -> bool {
        self.sessions
            .values()
            .any(WebSessionState::is_pending_approval)
    }

    fn running_session_ids(&self) -> Vec<String> {
        let mut ids = self
            .sessions
            .iter()
            .filter(|(_, state)| state.is_running())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            self.subagent_jobs
                .values()
                .map(|job| job.child_session_id.clone()),
        );
        ids
    }

    fn session_is_running(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(WebSessionState::is_running)
    }

    fn poll_all_running_turns(&mut self) -> Result<(), WebHostError> {
        let session_ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            self.poll_running_turn(&session_id)?;
        }
        Ok(())
    }

    fn start_subagent_job(&mut self, start: WebSubagentStart) -> Result<(), WebHostError> {
        let WebSubagentStart {
            task,
            authority,
            runtime,
            parent_session_id,
            root_session_id,
            child_session_id,
            message_id,
            message,
            model,
            tool_grants,
            recovery,
        } = start;
        if self.subagent_jobs.len() >= MAX_WEB_SUBAGENT_JOBS {
            return Err(WebHostError::Session(format!(
                "the WebHost child-agent worker limit of {MAX_WEB_SUBAGENT_JOBS} has been reached"
            )));
        }
        if self.subagent_jobs.contains_key(&child_session_id) {
            return Err(WebHostError::Session(
                "the child agent already has a running turn".to_string(),
            ));
        }

        let (event_sender, events) = sync_channel(WEB_SUBAGENT_EVENT_CAPACITY);
        let dropped_events = Arc::new(AtomicU64::new(0));
        let worker_dropped_events = Arc::clone(&dropped_events);
        let executor: Arc<dyn ChildExecutor> = Arc::new(
            move |turn: ChildTurn, cancellation: MultiAgentCancellationToken| {
                let task = task.clone();
                let event_sender = event_sender.clone();
                let worker_dropped_events = Arc::clone(&worker_dropped_events);
                async move {
                    task.run(turn, &cancellation, |event| {
                        match event_sender.try_send(event) {
                            Ok(()) => Ok(()),
                            Err(TrySendError::Full(ModelStreamEvent::TextDelta { .. })) => {
                                worker_dropped_events.fetch_add(1, Ordering::Relaxed);
                                Ok(())
                            }
                            Err(TrySendError::Full(_)) => Err(ChildWorkerError::new(
                                "event_queue_full",
                                "the bounded child-agent event queue is full",
                            )
                            .expect("bounded queue error"))
                            .map_err(|error| error.to_string()),
                            Err(TrySendError::Disconnected(_)) => Err(ChildWorkerError::new(
                                "event_consumer_closed",
                                "the child-agent event consumer is closed",
                            )
                            .expect("bounded consumer error"))
                            .map_err(|error| error.to_string()),
                        }
                    })
                }
            },
        );
        let spec = ChildWorkerSpec::new(model.clone(), tool_grants.clone())
            .map_err(|error| WebHostError::Session(error.to_string()))?;
        let handle = match recovery {
            Some(recovery) => runtime
                .resume_recovered_worker(WorkerRecoveryPlan::new(recovery, spec), executor)
                .map_err(|error| WebHostError::Session(error.to_string()))?,
            None => runtime
                .resume_worker(
                    &authority,
                    child_session_id.clone(),
                    message,
                    spec,
                    executor,
                )
                .map_err(|error| WebHostError::Session(error.to_string()))?,
        };

        let job_child_session_id = child_session_id.clone();
        self.subagent_jobs.insert(
            child_session_id,
            WebSubagentJob {
                parent_session_id,
                root_session_id,
                child_session_id: job_child_session_id,
                message_id,
                authority,
                runtime,
                handle,
                events,
                dropped_events,
                text: String::new(),
                text_overflowed: false,
                reported_drops: 0,
                reported_runtime_events: 0,
            },
        );
        Ok(())
    }

    fn poll_subagent_jobs(&mut self) -> Result<(), WebHostError> {
        let child_ids = self.subagent_jobs.keys().cloned().collect::<Vec<_>>();
        for child_id in child_ids {
            self.poll_subagent_job(&child_id)?;
        }
        Ok(())
    }

    fn poll_subagent_job(&mut self, child_session_id: &str) -> Result<(), WebHostError> {
        let Some(mut job) = self.subagent_jobs.remove(child_session_id) else {
            return Ok(());
        };
        let mut projected = Vec::new();
        drain_subagent_events(&mut job, &mut projected);
        let dropped = job.dropped_events.load(Ordering::Relaxed);
        if dropped > job.reported_drops {
            projected.push(json!({
                "type": "subagent/diagnostic",
                "parentSessionId": job.parent_session_id,
                "rootSessionId": job.root_session_id,
                "childSessionId": job.child_session_id,
                "messageId": job.message_id,
                "droppedEvents": dropped.saturating_sub(job.reported_drops),
            }));
            job.reported_drops = dropped;
        }

        if let Ok(runtime_projection) = job.runtime.projection() {
            let runtime_events = runtime_projection
                .events()
                .iter()
                .filter(|event| {
                    event.agent_id() == job.child_session_id
                        && event.sequence() > job.reported_runtime_events
                })
                .cloned()
                .collect::<Vec<_>>();
            for event in runtime_events {
                projected.push(json!({
                    "type": "subagent/runtime",
                    "parentSessionId": job.parent_session_id,
                    "rootSessionId": job.root_session_id,
                    "childSessionId": job.child_session_id,
                    "messageId": job.message_id,
                    "event": {
                        "sequence": event.sequence(),
                        "kind": serde_json::to_value(event.kind()).unwrap_or(Value::Null),
                        "detail": event.detail(),
                    },
                }));
                job.reported_runtime_events = job.reported_runtime_events.max(event.sequence());
            }
            if runtime_projection.events_truncated() {
                projected.push(json!({
                    "type": "subagent/diagnostic",
                    "parentSessionId": job.parent_session_id,
                    "rootSessionId": job.root_session_id,
                    "childSessionId": job.child_session_id,
                    "messageId": job.message_id,
                    "runtimeEventsTruncated": true,
                }));
            }
        }

        let completion = job.handle.is_finished().then(|| job.handle.wait_blocking());
        if completion.is_some() {
            drain_subagent_events(&mut job, &mut projected);
        }

        for event in projected {
            self.publish_subagent_event(event)?;
        }

        let Some(completion) = completion else {
            self.subagent_jobs.insert(child_session_id.to_string(), job);
            return Ok(());
        };
        let cancelled = job.handle.is_cancelled();
        let parent_session_id = job.parent_session_id.clone();
        let root_session_id = job.root_session_id.clone();
        let message_id = job.message_id.clone();
        match completion {
            Ok(WorkerOutcome::Completed { reply, .. })
                if !cancelled && !reply.trim().is_empty() =>
            {
                let bounded_reply = bounded_subagent_text(reply);
                self.publish_subagent_event(json!({
                    "type": "subagent/message",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "content": bounded_reply,
                    "final": true,
                }))?;
                self.publish_subagent_event(json!({
                    "type": "subagent/state",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "state": "completed",
                }))?;
            }
            Ok(WorkerOutcome::Cancelled { .. }) | Ok(_) => {
                self.publish_subagent_event(json!({
                    "type": "subagent/state",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "state": "cancelled",
                }))?;
            }
            Err(error) => {
                let state = if cancelled { "cancelled" } else { "failed" };
                self.publish_subagent_event(json!({
                    "type": "subagent/error",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "message": bounded_message(error.to_string()),
                }))?;
                self.publish_subagent_event(json!({
                    "type": "subagent/state",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "state": state,
                }))?;
            }
        }
        Ok(())
    }

    fn publish_subagent_event(&mut self, payload: Value) -> Result<(), WebHostError> {
        match self.publish_event(EventChannel::Mux, payload) {
            Err(WebHostError::Gateway(GatewayError::EventQueueFull { .. })) => Ok(()),
            result => result,
        }
    }

    fn poll_running_turn(&mut self, session_id: &str) -> Result<(), WebHostError> {
        let Some(mut state) = self.sessions.remove(session_id) else {
            return Ok(());
        };
        let result = self.poll_running_turn_state(session_id, &mut state);
        self.sessions.insert(session_id.to_string(), state);
        result
    }

    fn poll_running_turn_state(
        &mut self,
        _session_id: &str,
        state: &mut WebSessionState,
    ) -> Result<(), WebHostError> {
        let mut projected = Vec::new();
        let mut completed = None;
        let mut dropped = None;
        let Some(running) = state.running_turn.as_mut() else {
            return Ok(());
        };
        let event_session_id = running.session_id.clone();
        for _ in 0..WEB_STREAM_CAPACITY.saturating_mul(2) {
            match running.events.try_recv() {
                Ok(event) => projected.extend(running.project(event)),
                Err(EventReceiveError::Empty | EventReceiveError::Closed) => break,
                Err(EventReceiveError::Timeout) => break,
            }
        }
        let dropped_events = running.events.dropped_events();
        if dropped_events > running.reported_drops {
            dropped = Some(dropped_events.saturating_sub(running.reported_drops));
            running.reported_drops = dropped_events;
        }
        match running.completion.try_recv() {
            Ok(completion) => completed = Some(completion),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                completed = Some(WebWorkerCompletion {
                    session: None,
                    result: match running.kind {
                        WebTurnKind::Prompt { .. } => WebWorkerResult::Prompt(Err(
                            "the isolated Web turn worker stopped unexpectedly".to_string(),
                        )),
                        WebTurnKind::Approval => WebWorkerResult::Approval(Err(
                            "the isolated Web approval worker stopped unexpectedly".to_string(),
                        )),
                    },
                });
            }
        }

        // The worker drops its sender before publishing completion. Drain the
        // remaining bounded queue before finalizing so terminal lifecycle
        // events cannot overtake already queued stream events or get lost.
        if completed.is_some() {
            loop {
                match running.events.try_recv() {
                    Ok(event) => projected.extend(running.project(event)),
                    Err(EventReceiveError::Empty | EventReceiveError::Closed) => break,
                    Err(EventReceiveError::Timeout) => break,
                }
            }
        }

        for event in projected {
            self.publish_pending_session_event(state, &event_session_id, event)?;
        }
        if let Some(dropped) = dropped {
            self.publish_pending_session_event(
                state,
                &event_session_id,
                PendingSessionEvent::diagnostic(
                    "agent/backpressure",
                    json!({ "droppedEvents": dropped }),
                ),
            )?;
        }
        if let Some(completion) = completed {
            self.finish_worker(state, completion)?;
        }
        Ok(())
    }

    fn finish_worker(
        &mut self,
        state: &mut WebSessionState,
        completion: WebWorkerCompletion,
    ) -> Result<(), WebHostError> {
        let mut running = state
            .running_turn
            .take()
            .expect("a completion belongs to the active Web turn");
        if let Some(worker) = running.worker.take() {
            let _ = worker.join();
        }
        let session_id = running.session_id.clone();
        let accepted_user = running
            .kind
            .previous_length()
            .and_then(|index| state.history.get(index))
            .filter(|message| message.role() == ChatRole::User)
            .cloned();
        let was_cancelled = running.cancellation.is_cancelled();
        if let Some(session) = completion.session {
            state.session.put(session);
            // A cancelled provider stream may have torn down its model route.
            // Recreate only this session's host while retaining its local
            // event/history view, so another session is unaffected.
            if was_cancelled
                && let Ok(mut replacement) = ChatSession::launch_with_options(&self.session_options)
                && replacement.activate_web_session(&session_id).is_ok()
            {
                state.session.replace(replacement);
            }
        } else {
            self.recover_session_after_worker_failure(state, &session_id)?;
        }

        match completion.result {
            WebWorkerResult::Prompt(Ok(reply)) => {
                self.finish_successful_turn(state, &running, reply)?;
            }
            WebWorkerResult::Prompt(Err(_error)) if state.session.pending_approval().is_some() => {
                self.publish_pending_approval(state, &session_id)?;
            }
            WebWorkerResult::Prompt(Err(error)) => {
                if let Some(previous_length) = running.kind.previous_length() {
                    self.reconcile_failed_prompt_history(state, previous_length, accepted_user);
                }
                let reason = if running.cancellation.is_cancelled() {
                    json!({ "kind": "cancelled" })
                } else {
                    json!({ "kind": "failed", "message": bounded_message(error.clone()) })
                };
                self.finish_failed_turn(state, &running, reason)?;
                self.publish_pending_session_event(
                    state,
                    &session_id,
                    PendingSessionEvent::diagnostic(
                        "agent/error",
                        json!({ "message": bounded_message(error) }),
                    ),
                )?;
            }
            WebWorkerResult::Approval(Ok(reply)) => {
                if let Some(reply) = reply {
                    self.finish_successful_turn(state, &running, reply)?;
                }
                if state.session.pending_approval().is_some() {
                    self.publish_pending_approval(state, &session_id)?;
                }
            }
            WebWorkerResult::Approval(Err(error)) => {
                let reason = if running.cancellation.is_cancelled() {
                    json!({ "kind": "cancelled" })
                } else {
                    json!({ "kind": "failed", "message": bounded_message(error.clone()) })
                };
                self.finish_failed_turn(state, &running, reason)?;
                self.publish_pending_session_event(
                    state,
                    &session_id,
                    PendingSessionEvent::diagnostic(
                        "agent/error",
                        json!({ "message": bounded_message(error) }),
                    ),
                )?;
            }
        }
        if state.session.pending_approval().is_none()
            && state.pending_approval.is_none()
            && !state.queued_prompts.is_empty()
        {
            self.start_next_queued_prompt(state, &session_id)?;
            self.publish_session_status(&session_id, true)?;
        } else {
            self.publish_session_status(&session_id, false)?;
        }
        Ok(())
    }

    fn start_next_queued_prompt(
        &mut self,
        state: &mut WebSessionState,
        session_id: &str,
    ) -> Result<(), WebHostError> {
        let Some(queued) = state.queued_prompts.pop_front() else {
            return Ok(());
        };
        let item_id = queued.item_id.clone();
        let content = queued.content.clone();
        let previous_length = state.history.len();
        state.history.push(ChatMessage::user(content));
        if let Err(error) = self.publish_turn_opening(state, previous_length, session_id) {
            state.history.truncate(previous_length);
            state.queued_prompts.push_front(queued);
            return Err(error);
        }
        if let Err(error) = self.start_prompt_worker(state, session_id.to_string(), previous_length)
        {
            state.history.truncate(previous_length);
            state.queued_prompts.push_front(queued);
            return Err(error);
        }
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "session/queue-updated",
                "sessionId": session_id,
                "itemId": item_id,
                "action": "started",
            }),
        )?;
        Ok(())
    }

    fn reconcile_failed_prompt_history(
        &mut self,
        state: &mut WebSessionState,
        previous_length: usize,
        accepted_user: Option<ChatMessage>,
    ) {
        state.history.truncate(previous_length);
        if let Some(message) = accepted_user {
            state.history.push(message);
        }
    }

    fn finish_successful_turn(
        &mut self,
        state: &mut WebSessionState,
        running: &WebRunningTurn,
        reply: String,
    ) -> Result<(), WebHostError> {
        let message_index = state.history.len();
        let message = ChatMessage::assistant(reply);
        let message_value = web_message_value(message_index, &message);
        state.history.push(message);
        self.publish_pending_session_event(
            state,
            &running.session_id,
            PendingSessionEvent::retained(
                "assistant/message",
                json!({
                    "turn": running.turn,
                    "step": running.current_step,
                    "message": message_value,
                }),
                true,
            ),
        )?;
        self.publish_pending_session_event(
            state,
            &running.session_id,
            PendingSessionEvent::retained(
                "step/end",
                json!({ "turn": running.turn, "step": running.current_step }),
                false,
            ),
        )?;
        self.publish_pending_session_event(
            state,
            &running.session_id,
            PendingSessionEvent::retained(
                "turn/end",
                json!({ "turn": running.turn, "reason": { "kind": "completed" } }),
                false,
            ),
        )
    }

    fn finish_failed_turn(
        &mut self,
        state: &mut WebSessionState,
        running: &WebRunningTurn,
        reason: Value,
    ) -> Result<(), WebHostError> {
        self.publish_pending_session_event(
            state,
            &running.session_id,
            PendingSessionEvent::retained(
                "step/end",
                json!({ "turn": running.turn, "step": running.current_step }),
                false,
            ),
        )?;
        self.publish_pending_session_event(
            state,
            &running.session_id,
            PendingSessionEvent::retained(
                "turn/end",
                json!({ "turn": running.turn, "reason": reason }),
                false,
            ),
        )
    }

    fn publish_turn_opening(
        &mut self,
        state: &mut WebSessionState,
        message_index: usize,
        session_id: &str,
    ) -> Result<(), WebHostError> {
        let turn = u64::try_from(message_index / 2).unwrap_or(u64::MAX);
        let message = state
            .history
            .get(message_index)
            .expect("the prompt was appended before opening the turn");
        let message_value = web_message_value(message_index, message);
        for event in [
            PendingSessionEvent::retained("turn/start", json!({ "turn": turn }), false),
            PendingSessionEvent::retained("user/message", message_value, true),
            PendingSessionEvent::retained("step/start", json!({ "turn": turn, "step": 0 }), false),
        ] {
            self.publish_pending_session_event(state, session_id, event)?;
        }
        Ok(())
    }

    fn publish_pending_session_event(
        &mut self,
        state: &mut WebSessionState,
        session_id: &str,
        pending: PendingSessionEvent,
    ) -> Result<(), WebHostError> {
        let sequence = state.next_event_seq;
        state.next_event_seq = sequence.checked_add(1).ok_or_else(|| {
            WebHostError::Session("Web session event sequence is exhausted".to_string())
        })?;
        let mut event = json!({
            "type": pending.event_type,
            "seq": sequence,
            "time": now_millis(),
            "data": pending.data,
        });
        if pending.surface_append {
            event
                .as_object_mut()
                .expect("session event is an object")
                .insert("surfaceOp".to_string(), Value::String("append".to_string()));
        }
        if pending.ignorable {
            event
                .as_object_mut()
                .expect("session event is an object")
                .insert("ignorable".to_string(), Value::Bool(true));
        }
        if pending.retain {
            self.retain_web_event(state, event.clone());
        }
        match self.publish_event(EventChannel::Mux, session_event_payload(session_id, event)) {
            // The canonical event remains available through session.history;
            // a full live queue is a recoverable disconnect/backlog condition.
            Err(WebHostError::Gateway(GatewayError::EventQueueFull { .. })) => Ok(()),
            result => result,
        }
    }

    fn retain_web_event(&mut self, state: &mut WebSessionState, event: Value) {
        append_bounded_event(&mut state.events, &mut state.event_bytes, event);
    }

    fn replace_web_events(&mut self, state: &mut WebSessionState, events: Vec<Value>) {
        state.event_bytes = events.iter().fold(0usize, |bytes, event| {
            bytes.saturating_add(approximate_event_bytes(event))
        });
        state.events = events;
        trim_bounded_events(&mut state.events, &mut state.event_bytes);
    }

    fn recover_session_after_worker_failure(
        &mut self,
        state: &mut WebSessionState,
        session_id: &str,
    ) -> Result<(), WebHostError> {
        let mut replacement = ChatSession::launch_with_options(&self.session_options)
            .map_err(|error| WebHostError::Session(error.to_string()))?;
        let messages = replacement
            .activate_web_session(session_id)
            .map_err(WebHostError::Session)?;
        state.history = messages;
        self.replace_web_events(state, session_event_values(&state.history));
        let recovered_next_event_seq = state
            .events
            .last()
            .and_then(|event| event.get("seq"))
            .and_then(Value::as_u64)
            .map_or(0, |sequence| sequence.saturating_add(1));
        state.next_event_seq = state.next_event_seq.max(recovered_next_event_seq);
        state.session.put(replacement);
        Ok(())
    }

    fn ensure_web_session(&mut self, session_id: &str) -> Result<(), String> {
        if self.sessions.contains_key(session_id) {
            return Ok(());
        }
        if self.sessions.len() >= MAX_WEB_SESSIONS {
            return Err(format!(
                "the WebHost session limit of {MAX_WEB_SESSIONS} has been reached"
            ));
        }
        let mut session = ChatSession::launch_with_options(&self.session_options)
            .map_err(|error| error.to_string())?;
        let messages = session.activate_web_session(session_id)?;
        let recovery_authority = session
            .multi_agent_web_available()
            .then(|| session.web_agent_recovery_grant(session_id))
            .transpose()
            .map_err(|error| error.to_string())?;
        self.sessions.insert(
            session_id.to_string(),
            WebSessionState::new(session, messages),
        );
        if let Some(authority) = recovery_authority
            && let Err(error) = self.runtime_for_root(session_id, &authority)
        {
            self.sessions.remove(session_id);
            self.multi_agent_runtimes.remove(session_id);
            return Err(format!(
                "persisted multi-agent workers could not be recovered: {error}"
            ));
        }
        Ok(())
    }

    fn session_failure(
        &self,
        request: &ClientRequest,
        session_id: Option<&str>,
        error: String,
    ) -> Result<ServerResponse, WebHostError> {
        let not_found =
            session_id.is_some_and(|id| error == format!("session `{id}` was not found"));
        let unavailable = error.contains("disabled or unavailable");
        let (code, message) = if not_found {
            ("session-not-found", "the requested session does not exist")
        } else if unavailable {
            (
                "storage-unavailable",
                "session storage is disabled or unavailable",
            )
        } else {
            ("internal", "session storage could not complete the request")
        };
        self.failure(
            request,
            code,
            message,
            session_id.map_or_else(|| json!({}), |id| json!({ "sessionId": id })),
        )
    }

    fn publish_session_status(
        &mut self,
        session_id: &str,
        running: bool,
    ) -> Result<(), WebHostError> {
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/session-status",
                "sessionId": session_id,
                "running": running,
            }),
        )
    }

    fn publish_pending_approval(
        &mut self,
        state: &mut WebSessionState,
        session_id: &str,
    ) -> Result<(), WebHostError> {
        let Some(approval) = state.session.pending_approval() else {
            return Ok(());
        };
        if let Some(pending) = state.pending_approval.as_ref() {
            return self.publish_event_with_id(
                EventChannel::Mux,
                pending.rpc_id.clone(),
                json!({
                    "type": "approval/requested",
                    "sessionId": pending.session_id,
                    "approvalId": pending.approval_id,
                    "toolName": pending.tool_name,
                    "callId": pending.call_id,
                    "reason": pending.reason,
                }),
            );
        }
        let number = self.next_web_id;
        self.next_web_id = self.next_web_id.saturating_add(1);
        let rpc_id = RpcId::new(format!("approval-rpc-{number}"))
            .map_err(|error| WebHostError::Gateway(GatewayError::Contract(error)))?;
        let approval_id = format!("approval-{number}");
        let tool_name = approval.tool_name.to_string();
        let call_id = approval.call_id.clone();
        let reason = approval.summary.to_string();
        state.pending_approval = Some(WebPendingApproval {
            rpc_id: rpc_id.clone(),
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            tool_name: tool_name.clone(),
            call_id: call_id.clone(),
            reason: reason.clone(),
        });
        self.publish_event_with_id(
            EventChannel::Mux,
            rpc_id,
            json!({
                "type": "approval/requested",
                "sessionId": session_id,
                "approvalId": approval_id,
                "toolName": tool_name,
                "callId": call_id,
                "reason": reason,
            }),
        )
    }

    fn respond(&mut self, response: &ClientResponse) -> Result<Value, WebHostError> {
        let Some(session_id) = self.sessions.iter().find_map(|(id, state)| {
            state.pending_approval.as_ref().and_then(|pending| {
                (pending.rpc_id.as_str() == response.rpc_id().as_str()).then(|| id.clone())
            })
        }) else {
            return Ok(json!({ "accepted": false, "reason": "not-pending" }));
        };
        let mut state = self
            .sessions
            .remove(&session_id)
            .expect("pending approval session is registered");
        let pending = state
            .pending_approval
            .take()
            .expect("pending approval was found");
        let outcome = response
            .result()
            .value()
            .and_then(|value| value.get("outcome"))
            .and_then(Value::as_str)
            .unwrap_or("rejected");
        let value = response.result().value();
        let valid = value.and_then(Value::as_object).is_some_and(|object| {
            object.get("sessionId").and_then(Value::as_str) == Some(pending.session_id.as_str())
                && object.get("approvalId").and_then(Value::as_str)
                    == Some(pending.approval_id.as_str())
                && matches!(outcome, "allowed-once" | "rejected")
        });
        if !valid {
            state.pending_approval = Some(pending);
            self.sessions.insert(session_id, state);
            return Ok(json!({ "accepted": false, "reason": "bad-response" }));
        }

        let approved = outcome == "allowed-once";
        if let Err(error) = self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "approval/resolved",
                "sessionId": pending.session_id,
                "approvalId": pending.approval_id,
                "outcome": outcome,
            }),
        ) {
            state.pending_approval = Some(pending);
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        if let Err(error) = self.publish_session_status(&pending.session_id, true) {
            state.pending_approval = Some(pending);
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        if let Err(error) =
            self.start_approval_worker(&mut state, pending.session_id.clone(), approved)
        {
            state.pending_approval = Some(pending);
            self.sessions.insert(session_id, state);
            return Err(error);
        }
        self.sessions.insert(session_id.clone(), state);
        let deadline = Instant::now() + WEB_FAST_PATH_WAIT;
        while self.session_is_running(&session_id) && Instant::now() < deadline {
            self.poll_running_turn(&session_id)?;
            if self.session_is_running(&session_id) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(json!({ "accepted": true }))
    }

    fn success(
        &self,
        request: &ClientRequest,
        value: Value,
    ) -> Result<ServerResponse, WebHostError> {
        ServerResponse::new(request.rpc_id().clone(), RpcResult::success(value))
            .map_err(|error| WebHostError::Gateway(GatewayError::Contract(error)))
    }

    fn failure(
        &self,
        request: &ClientRequest,
        code: &str,
        message: &str,
        details: Value,
    ) -> Result<ServerResponse, WebHostError> {
        let error = RpcError::new(code, message, details)
            .map_err(|error| WebHostError::Gateway(GatewayError::Contract(error)))?;
        ServerResponse::new(request.rpc_id().clone(), RpcResult::failure(error))
            .map_err(|error| WebHostError::Gateway(GatewayError::Contract(error)))
    }
}

impl Drop for WebHost {
    fn drop(&mut self) {
        for state in self.sessions.values_mut() {
            if let Some(mut running) = state.running_turn.take() {
                running
                    .cancellation
                    .cancel("YunXi Web Host is shutting down");
                if let Some(worker) = running.worker.take() {
                    let _ = worker.join();
                }
            }
        }
        let jobs = std::mem::take(&mut self.subagent_jobs);
        for (_, job) in jobs {
            job.handle.cancel();
            let _ = job.handle.wait_blocking();
        }
    }
}

impl GatewayBackend for WebHost {
    type Error = WebHostError;

    fn refresh(&mut self) {
        WebHost::refresh(self);
    }

    fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, Self::Error> {
        WebHost::dispatch(self, request)
    }

    fn respond(&mut self, response: &ClientResponse) -> Result<Value, Self::Error> {
        WebHost::respond(self, response)
    }

    fn take_events(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, Self::Error> {
        let deadline = Instant::now() + WEB_EVENT_LONG_POLL;
        let mut events = loop {
            self.poll_all_running_turns()?;
            self.poll_subagent_jobs()?;
            let events = self
                .gateway
                .take_events(channel, maximum_events, maximum_encoded_bytes)
                .map_err(WebHostError::Gateway)?;
            if !events.is_empty()
                || channel != EventChannel::Mux
                || !self.any_running()
                || Instant::now() >= deadline
            {
                break events;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        if channel == EventChannel::Mux
            && events.is_empty()
            && self.any_pending_approval()
            && !self.gateway.has_events(EventChannel::Mux)
        {
            let session_id = self
                .sessions
                .iter()
                .find_map(|(id, state)| {
                    (state.pending_approval.is_some()
                        || state
                            .session
                            .0
                            .as_ref()
                            .is_some_and(|session| session.pending_approval().is_some()))
                    .then(|| id.clone())
                })
                .expect("pending approval checked above");
            let mut state = self
                .sessions
                .remove(&session_id)
                .expect("pending approval session is registered");
            let publish_result = WebHost::publish_pending_approval(self, &mut state, &session_id);
            self.sessions.insert(session_id, state);
            publish_result?;
            events = self
                .gateway
                .take_events(channel, maximum_events, maximum_encoded_bytes)
                .map_err(WebHostError::Gateway)?;
        }
        Ok(events)
    }
}

fn restore_settings_file(path: &Path, state: SettingsFileState) -> std::io::Result<()> {
    match state {
        SettingsFileState::Missing => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        SettingsFileState::Present(bytes) => fs::write(path, bytes),
    }
}

fn expected_revision(payload: &Value) -> Result<Option<u64>, ()> {
    match payload.get("expectedRevision") {
        None => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(()),
    }
}

enum SettingsEdits {
    Capabilities(Vec<CapabilityEdit>),
    Plugins(Vec<PluginEdit>),
}

fn settings_edits(payload: &Value) -> Result<SettingsEdits, String> {
    let ops = payload
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| "settings.mutate requires an ops array".to_string())?;
    if ops.len() > MAX_SETTINGS_MUTATIONS {
        return Err(format!(
            "settings.mutate accepts at most {MAX_SETTINGS_MUTATIONS} operations"
        ));
    }
    let mut capability_edits = Vec::new();
    let mut plugin_edits = Vec::new();
    for operation in ops {
        let object = operation
            .as_object()
            .ok_or_else(|| "each settings mutation must be an object".to_string())?;
        let op = object
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "each settings mutation requires an op".to_string())?;
        let path = object
            .get("path")
            .and_then(Value::as_array)
            .ok_or_else(|| "settings mutations require a path array".to_string())?;
        let capability = path
            .first()
            .and_then(Value::as_str)
            .and_then(CapabilitySetting::parse)
            .filter(|_| path.len() == 1);
        let plugin = (path.len() == 2 && path[0].as_str() == Some("plugins"))
            .then(|| path[1].as_str())
            .flatten();
        if capability.is_none() && plugin.is_none() {
            return Err(
                "settings mutations require one supported capability or plugin path".to_string(),
            );
        }
        match op {
            "set" => {
                if object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "op" | "path" | "value"))
                {
                    return Err("settings set mutation contains an unknown field".to_string());
                }
                let value = object
                    .get("value")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "settings capability values must be booleans".to_string())?;
                if let Some(capability) = capability {
                    capability_edits.push(CapabilityEdit::Set(capability, value));
                } else if let Some(plugin) = plugin {
                    plugin_edits.push(PluginEdit::Set(plugin.to_string(), value));
                }
            }
            "unset" => {
                if object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "op" | "path"))
                {
                    return Err("settings unset mutation contains an unknown field".to_string());
                }
                if let Some(capability) = capability {
                    capability_edits.push(CapabilityEdit::Unset(capability));
                } else if let Some(plugin) = plugin {
                    plugin_edits.push(PluginEdit::Unset(plugin.to_string()));
                }
            }
            _ => return Err("settings mutation op must be set or unset".to_string()),
        }
    }
    match (capability_edits.is_empty(), plugin_edits.is_empty()) {
        (false, true) => Ok(SettingsEdits::Capabilities(capability_edits)),
        (true, false) => Ok(SettingsEdits::Plugins(plugin_edits)),
        _ => Err("settings mutations cannot mix capability and plugin paths".to_string()),
    }
}

fn capability_settings_schema() -> Value {
    json!({
        "uid": 16,
        "refs": {
            "1": { "type": "boolean" },
            "2": { "type": "boolean" },
            "3": { "type": "boolean" },
            "4": { "type": "boolean" },
            "5": { "type": "boolean" },
            "6": { "type": "boolean" },
            "7": { "type": "boolean" },
            "8": { "type": "boolean" },
            "9": { "type": "boolean" },
            "10": { "type": "boolean" },
            "11": { "type": "boolean" },
            "12": { "type": "boolean" },
            "13": { "type": "boolean" },
            "14": { "type": "boolean" },
            "15": { "type": "boolean" },
            "16": {
                "type": "object",
                "dict": {
                    "context": 1,
                    "persona": 2,
                    "memory": 3,
                    "companion": 4,
                    "storage": 5,
                    "mailbox": 6,
                    "scheduler": 7,
                    "shell": 8,
                    "patch": 9,
                    "files": 10,
                    "mcp": 11,
                    "skills": 12,
                    "multi_agent": 13,
                    "voice": 14,
                    "weixin": 15
                }
            }
        }
    })
}

fn string_field(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn optional_weixin_string(payload: &Value, field: &str) -> Result<Option<String>, String> {
    match payload.get(field) {
        None => Ok(None),
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| format!("{field} must be a non-empty string"))?;
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("{field} must be a non-empty string"));
            }
            Ok(Some(value.to_string()))
        }
    }
}

fn required_weixin_string(payload: &Value, field: &str) -> Result<String, String> {
    optional_weixin_string(payload, field)?.ok_or_else(|| format!("{field} is required"))
}

fn optional_weixin_bool(payload: &Value, field: &str) -> Result<Option<bool>, String> {
    optional_bool(payload, field)
}

fn optional_weixin_limit(
    payload: &Value,
    field: &str,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| format!("{field} must be a positive integer"))?;
    if value == 0 || value > maximum {
        return Err(format!("{field} must be between 1 and {maximum}"));
    }
    Ok(Some(value))
}

fn bounded_management_limit(
    payload: &Value,
    field: &str,
    default: u64,
    maximum: u64,
) -> Result<usize, String> {
    let Some(value) = payload.get(field) else {
        return Ok(default as usize);
    };
    let Some(value) = value.as_u64() else {
        return Err(format!("{field} must be a positive integer"));
    };
    if value == 0 || value > maximum {
        return Err(format!("{field} must be between 1 and {maximum}"));
    }
    usize::try_from(value).map_err(|_| format!("{field} is too large"))
}

fn optional_bool(payload: &Value, field: &str) -> Result<Option<bool>, String> {
    match payload.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{field} must be a boolean")),
    }
}

fn relationship_records(records: Vec<yunxi_memory::MemoryRecordSummary>) -> Vec<Value> {
    records
        .into_iter()
        .filter(|record| {
            record.kind == "relationship_note"
                || record.scope == "relationship"
                || record.layer == "relationship"
        })
        .map(|record| serde_json::to_value(record).expect("memory summary is serializable"))
        .collect()
}

fn mailbox_kind(kind: MailboxItemKind) -> &'static str {
    match kind {
        MailboxItemKind::ProactiveMessage => "proactive_message",
        MailboxItemKind::LoveLetter => "love_letter",
        MailboxItemKind::Reminder => "reminder",
    }
}

fn mailbox_summary_value(item: &MailboxSummary) -> Value {
    json!({
        "id": item.id(),
        "kind": mailbox_kind(item.kind()),
        "subject": item.subject(),
        "reason": item.reason(),
        "read": item.read(),
        "createdAt": item.created_at_millis(),
    })
}

fn mailbox_entry_value(entry: &MailboxEntry) -> Value {
    json!({
        "summary": mailbox_summary_value(entry.summary()),
        "content": entry.content(),
    })
}

fn optional_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}

fn message_count_at_seq(events: &[Value], sequence: u64) -> Result<usize, String> {
    let Some(oldest) = events
        .first()
        .and_then(|event| event.get("seq"))
        .and_then(Value::as_u64)
    else {
        return Ok(0);
    };
    if sequence < oldest {
        return Err("the requested event sequence is no longer retained".to_string());
    }
    Ok(events
        .iter()
        .take_while(|event| {
            event
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|value| value <= sequence)
        })
        .filter(|event| {
            matches!(
                event_type(event),
                Some("user/message" | "assistant/message")
            )
        })
        .count())
}

fn prompt_text(payload: &Value) -> Option<String> {
    let content = payload.get("content")?.as_array()?;
    if content.is_empty() {
        return None;
    }
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return None;
        }
        let part = block.get("text").and_then(Value::as_str)?;
        text.push_str(part);
    }
    if text.trim().is_empty() || text.chars().count() > MAX_WEB_PROMPT_CHARS {
        return None;
    }
    Some(text)
}

fn history_page(
    messages: &[ChatMessage],
    before_seq: Option<u64>,
    maximum_messages: usize,
) -> (Vec<Value>, bool) {
    let events = session_event_values(messages);
    history_page_from_events(&events, before_seq, maximum_messages)
}

fn history_page_from_events(
    events: &[Value],
    before_seq: Option<u64>,
    maximum_messages: usize,
) -> (Vec<Value>, bool) {
    let end = before_seq.map_or(events.len(), |before| {
        events
            .iter()
            .position(|event| {
                event
                    .get("seq")
                    .and_then(Value::as_u64)
                    .is_some_and(|sequence| sequence >= before)
            })
            .unwrap_or(events.len())
    });
    let mut start = end;
    let mut seen_messages = 0usize;
    while start > 0 && seen_messages < maximum_messages {
        start -= 1;
        if matches!(
            event_type(&events[start]),
            Some("user/message" | "assistant/message")
        ) {
            seen_messages += 1;
        }
    }
    while start > 0 && event_type(&events[start]) != Some("turn/start") {
        start -= 1;
    }
    let page = events[start..end]
        .iter()
        .map(|event| json!({ "event": event }))
        .collect();
    (page, start > 0)
}

#[cfg(test)]
fn replay_page_from_events(
    events: &[Value],
    after_seq: u64,
    maximum_messages: usize,
) -> (Vec<Value>, bool, bool) {
    replay_page_from_events_with_latest(
        events,
        after_seq,
        maximum_messages,
        events
            .last()
            .and_then(|event| event.get("seq"))
            .and_then(Value::as_u64),
    )
}

fn replay_page_from_events_with_latest(
    events: &[Value],
    after_seq: u64,
    maximum_messages: usize,
    latest_seq: Option<u64>,
) -> (Vec<Value>, bool, bool) {
    let oldest_seq = events
        .first()
        .and_then(|event| event.get("seq"))
        .and_then(Value::as_u64);
    let replay_gap = oldest_seq.is_some_and(|oldest| after_seq.saturating_add(1) < oldest)
        || latest_seq.is_some_and(|latest| {
            latest > after_seq
                && events
                    .last()
                    .and_then(|event| event.get("seq"))
                    .and_then(Value::as_u64)
                    .is_none_or(|last| last <= after_seq)
        });
    let start = events
        .iter()
        .position(|event| {
            event
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|sequence| sequence > after_seq)
        })
        .unwrap_or(events.len());
    let mut end = start;
    let mut seen_messages = 0usize;
    while end < events.len() {
        if matches!(
            event_type(&events[end]),
            Some("user/message" | "assistant/message")
        ) {
            seen_messages += 1;
        }
        end += 1;
        if seen_messages >= maximum_messages && event_type(&events[end - 1]) == Some("turn/end") {
            break;
        }
    }
    let page = events[start..end]
        .iter()
        .map(|event| json!({ "event": event }))
        .collect();
    (page, end < events.len(), replay_gap)
}

fn session_event_payload(session_id: &str, event: Value) -> Value {
    json!({
        "type": "session/event",
        "sessionId": session_id,
        "event": event,
    })
}

fn session_event_values(messages: &[ChatMessage]) -> Vec<Value> {
    let mut events = Vec::with_capacity(messages.len().saturating_mul(3));
    let time = now_millis();
    for (turn, pair) in messages.chunks(2).enumerate() {
        let Some(user) = pair.first() else {
            continue;
        };
        push_session_event(
            &mut events,
            time,
            "turn/start",
            json!({ "turn": turn }),
            false,
        );
        push_session_event(
            &mut events,
            time,
            "user/message",
            web_message_value(turn * 2, user),
            true,
        );
        push_session_event(
            &mut events,
            time,
            "step/start",
            json!({ "turn": turn, "step": 0 }),
            false,
        );
        let Some(assistant) = pair.get(1) else {
            continue;
        };
        push_session_event(
            &mut events,
            time,
            "assistant/message",
            json!({
                "turn": turn,
                "step": 0,
                "message": web_message_value(turn * 2 + 1, assistant),
            }),
            true,
        );
        push_session_event(
            &mut events,
            time,
            "step/end",
            json!({ "turn": turn, "step": 0 }),
            false,
        );
        push_session_event(
            &mut events,
            time,
            "turn/end",
            json!({ "turn": turn, "reason": { "kind": "completed" } }),
            false,
        );
    }
    events
}

fn push_session_event(
    events: &mut Vec<Value>,
    time: u128,
    event_type: &str,
    data: Value,
    surface_append: bool,
) {
    let mut event = json!({
        "type": event_type,
        "seq": events.len(),
        "time": time,
        "data": data,
    });
    if surface_append {
        event
            .as_object_mut()
            .expect("session event is an object")
            .insert("surfaceOp".to_string(), Value::String("append".to_string()));
    }
    events.push(event);
}

fn approximate_event_bytes(event: &Value) -> usize {
    event.to_string().len()
}

fn append_bounded_event(events: &mut Vec<Value>, bytes: &mut usize, event: Value) {
    *bytes = bytes.saturating_add(approximate_event_bytes(&event));
    events.push(event);
    trim_bounded_events(events, bytes);
}

fn trim_bounded_events(events: &mut Vec<Value>, bytes: &mut usize) {
    while events.len() > MAX_WEB_RETAINED_EVENTS || *bytes > MAX_WEB_RETAINED_BYTES {
        let Some(removed) = events.first().cloned() else {
            break;
        };
        *bytes = bytes.saturating_sub(approximate_event_bytes(&removed));
        events.remove(0);
    }
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

fn web_message_value(index: usize, message: &ChatMessage) -> Value {
    let source = match message.role() {
        ChatRole::Assistant => json!({
            "kind": "model",
            "provider": "yunxi",
            "model": "yunxi-next",
        }),
        _ => json!({ "kind": "user" }),
    };
    json!({
        "id": format!("web-message-{index}"),
        "role": match message.role() {
            ChatRole::Assistant => "assistant",
            ChatRole::User | ChatRole::System | ChatRole::Tool => "user",
        },
        "content": [{ "type": "text", "text": message.content() }],
        "source": source,
    })
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn bounded_message(message: String) -> String {
    const MAX_MESSAGE_CHARS: usize = 512;
    let mut chars = message.chars();
    let bounded = chars.by_ref().take(MAX_MESSAGE_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn drain_subagent_events(job: &mut WebSubagentJob, projected: &mut Vec<Value>) {
    for _ in 0..WEB_SUBAGENT_EVENT_CAPACITY.saturating_mul(2) {
        match job.events.try_recv() {
            Ok(event) => {
                if let Some(value) = project_subagent_event(job, event) {
                    projected.push(value);
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn project_subagent_event(job: &mut WebSubagentJob, event: ModelStreamEvent) -> Option<Value> {
    let parent_session_id = job.parent_session_id.clone();
    let root_session_id = job.root_session_id.clone();
    let child_session_id = job.child_session_id.clone();
    let message_id = job.message_id.clone();
    match event {
        ModelStreamEvent::TextDelta { text } => {
            let exceeds_limit = job
                .text
                .len()
                .checked_add(text.len())
                .is_none_or(|length| length > MAX_WEB_SUBAGENT_TEXT_BYTES);
            if exceeds_limit {
                job.handle.cancel();
                if job.text_overflowed {
                    return None;
                }
                job.text_overflowed = true;
                return Some(json!({
                    "type": "subagent/error",
                    "parentSessionId": parent_session_id,
                    "rootSessionId": root_session_id,
                    "childSessionId": child_session_id,
                    "messageId": message_id,
                    "message": "the child-agent text output exceeded its limit",
                }));
            }
            job.text.push_str(&text);
            Some(json!({
                "type": "subagent/message",
                "parentSessionId": parent_session_id,
                "rootSessionId": root_session_id,
                "childSessionId": child_session_id,
                "messageId": message_id,
                "delta": text,
                "final": false,
            }))
        }
        ModelStreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
        } => Some(json!({
            "type": "subagent/tool",
            "parentSessionId": parent_session_id,
            "rootSessionId": root_session_id,
            "childSessionId": child_session_id,
            "messageId": message_id,
            "index": index,
            "callId": id,
            "name": name,
            "arguments": arguments,
        })),
        ModelStreamEvent::Finished { reason } => Some(json!({
            "type": "subagent/state",
            "parentSessionId": parent_session_id,
            "rootSessionId": root_session_id,
            "childSessionId": child_session_id,
            "messageId": message_id,
            "state": "model-finished",
            "reason": reason,
        })),
    }
}

fn bounded_subagent_text(mut text: String) -> String {
    if text.len() <= MAX_WEB_SUBAGENT_TEXT_BYTES {
        return text;
    }
    let suffix = "\n[child-agent output truncated]";
    let maximum = MAX_WEB_SUBAGENT_TEXT_BYTES.saturating_sub(suffix.len());
    while !text.is_char_boundary(maximum) {
        text.pop();
    }
    text.truncate(maximum);
    text.push_str(suffix);
    text
}

#[derive(Debug)]
pub enum WebHostError {
    Session(String),
    Chat(String),
    Gateway(GatewayError),
}

impl fmt::Display for WebHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(message) => write!(formatter, "Web Host session failed: {message}"),
            Self::Chat(message) => write!(formatter, "Web Host chat failed: {message}"),
            Self::Gateway(error) => write!(formatter, "Web Gateway failed: {error}"),
        }
    }
}

impl Error for WebHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Gateway(error) => Some(error),
            Self::Session(_) | Self::Chat(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_entries_keep_the_dsh_history_shape() {
        let (events, has_more) = history_page(
            &[ChatMessage::user("hello"), ChatMessage::assistant("hi")],
            None,
            50,
        );
        assert!(!has_more);
        assert_eq!(events.len(), 6);
        assert_eq!(events[0]["event"]["type"], "turn/start");
        assert_eq!(events[1]["event"]["type"], "user/message");
        assert_eq!(events[1]["event"]["surfaceOp"], "append");
        assert_eq!(events[1]["event"]["data"]["content"][0]["text"], "hello");
        assert_eq!(events[2]["event"]["type"], "step/start");
        assert_eq!(events[3]["event"]["type"], "assistant/message");
        assert_eq!(
            events[3]["event"]["data"]["message"]["content"][0]["text"],
            "hi"
        );
        assert_eq!(events[4]["event"]["type"], "step/end");
        assert_eq!(events[5]["event"]["type"], "turn/end");
    }

    #[test]
    fn mux_events_wrap_history_events_with_the_session_identity() {
        let event = session_event_payload(
            "session-1",
            session_event_values(&[ChatMessage::user("hello")]).remove(1),
        );
        assert_eq!(event["type"], "session/event");
        assert_eq!(event["sessionId"], "session-1");
        assert_eq!(event["event"]["type"], "user/message");
    }

    #[test]
    fn history_pages_start_on_turn_boundaries() {
        let messages = [
            ChatMessage::user("one"),
            ChatMessage::assistant("first"),
            ChatMessage::user("two"),
            ChatMessage::assistant("second"),
        ];
        let (tail, has_more) = history_page(&messages, None, 2);
        assert!(has_more);
        assert_eq!(tail.first().expect("tail event")["event"]["seq"], 6);
        assert_eq!(
            tail.first().expect("tail event")["event"]["type"],
            "turn/start"
        );

        let (head, has_more) = history_page(&messages, Some(6), 2);
        assert!(!has_more);
        assert_eq!(head.len(), 6);
        assert_eq!(
            head.last().expect("head event")["event"]["type"],
            "turn/end"
        );
    }

    #[test]
    fn retained_events_are_bounded_without_renumbering() {
        let mut events = Vec::new();
        let mut bytes = 0;
        for sequence in 0..=MAX_WEB_RETAINED_EVENTS {
            append_bounded_event(
                &mut events,
                &mut bytes,
                json!({ "seq": sequence, "type": "step/end" }),
            );
        }
        assert_eq!(events.len(), MAX_WEB_RETAINED_EVENTS);
        assert_eq!(
            events.first().and_then(|event| event["seq"].as_u64()),
            Some(1)
        );
        assert_eq!(
            events.last().and_then(|event| event["seq"].as_u64()),
            Some(MAX_WEB_RETAINED_EVENTS as u64)
        );
        assert!(bytes <= MAX_WEB_RETAINED_BYTES);

        let large = "x".repeat(MAX_WEB_RETAINED_BYTES / 2);
        append_bounded_event(&mut events, &mut bytes, json!({ "data": large }));
        append_bounded_event(
            &mut events,
            &mut bytes,
            json!({ "data": "y".repeat(MAX_WEB_RETAINED_BYTES / 2) }),
        );
        assert!(bytes <= MAX_WEB_RETAINED_BYTES);
        assert!(events.len() < MAX_WEB_RETAINED_EVENTS);
    }

    #[test]
    fn replay_after_seq_reports_evicted_window_gaps() {
        let events = vec![
            json!({ "seq": 10, "type": "turn/start", "data": {} }),
            json!({ "seq": 11, "type": "user/message", "data": {} }),
            json!({ "seq": 12, "type": "turn/end", "data": {} }),
        ];
        let (page, has_more, gap) = replay_page_from_events(&events, 0, 1);
        assert!(gap);
        assert!(!has_more);
        assert_eq!(page.first().expect("replay event")["event"]["seq"], 10);

        let (page, has_more, gap) = replay_page_from_events(&events, 11, 1);
        assert!(!gap);
        assert!(!has_more);
        assert_eq!(page.first().expect("replay event")["event"]["seq"], 12);
    }

    #[test]
    fn replay_after_seq_reports_a_gap_when_recovery_removed_the_tail() {
        let events = vec![json!({
            "seq": 0,
            "type": "turn/start",
            "data": {},
        })];
        let (page, has_more, gap) = replay_page_from_events_with_latest(&events, 0, 1, Some(3));
        assert!(page.is_empty());
        assert!(!has_more);
        assert!(gap);
    }

    #[test]
    fn prompt_text_rejects_non_text_and_empty_content() {
        assert_eq!(
            prompt_text(&json!({
                "content": [{ "type": "text", "text": "hello" }]
            })),
            Some("hello".to_string())
        );
        assert!(prompt_text(&json!({ "content": [] })).is_none());
        assert!(
            prompt_text(&json!({
                "content": [{ "type": "image", "data": "..." }]
            }))
            .is_none()
        );
    }
}
