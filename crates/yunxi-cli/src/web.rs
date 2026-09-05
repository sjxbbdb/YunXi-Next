//! Reusable Web-facing facade over the existing CLI Host.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use yunxi_protocol::{
    AgentListResult, AgentStatus, AgentTranscriptRole, ChatMessage, ChatRole, ROOT_AGENT_ID,
};
use yunxi_settings::{
    CAPABILITY_SETTINGS_NAMESPACE, CapabilityEdit, CapabilitySetting, CapabilitySettingsError,
    CapabilitySettingsStore, CapabilitySwitches,
};
use yunxi_web_contract::{
    ClientRequest, ClientResponse, EventChannel, RpcError, RpcId, RpcMessage, RpcResult,
    ServerResponse,
};
use yunxi_web_gateway::{
    Gateway, GatewayBackend, GatewayError, GatewayProjection, SESSION_CREATE_METHOD,
    SESSION_HISTORY_METHOD, SESSION_PROMPT_METHOD, SETTINGS_DESCRIBE_METHOD,
    SETTINGS_MUTATE_METHOD, SETTINGS_REPLACE_METHOD, SETTINGS_UPDATE_METHOD,
    SUBAGENT_HISTORY_METHOD, SUBAGENT_LIST_METHOD,
};

use crate::session::{ChatBackend, ChatSession};

const MAX_SETTINGS_MUTATIONS: usize = 64;

pub struct WebHost {
    session: ChatSession,
    gateway: Gateway,
    web_history: Vec<ChatMessage>,
    web_session_id: Option<String>,
    pending_approval: Option<WebPendingApproval>,
    next_web_id: u64,
}

struct WebPendingApproval {
    rpc_id: RpcId,
    approval_id: String,
    session_id: String,
    tool_name: String,
    call_id: String,
    reason: String,
}

impl WebHost {
    pub fn launch(plugin_path: Option<&Path>) -> Result<Self, WebHostError> {
        let mut session = ChatSession::launch(plugin_path)
            .map_err(|error| WebHostError::Session(error.to_string()))?;
        let gateway = Gateway::new(session.web_projection());
        Ok(Self {
            session,
            gateway,
            web_history: Vec::new(),
            web_session_id: None,
            pending_approval: None,
            next_web_id: 1,
        })
    }

    pub fn refresh(&mut self) {
        self.gateway
            .replace_projection(self.session.web_projection());
    }

    pub fn projection(&mut self) -> &GatewayProjection {
        self.refresh();
        self.gateway.projection()
    }

    pub fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, WebHostError> {
        self.refresh();
        match request.method() {
            SESSION_CREATE_METHOD => self.dispatch_session_create(request),
            SESSION_HISTORY_METHOD => self.dispatch_session_history(request),
            SESSION_PROMPT_METHOD => self.dispatch_session_prompt(request),
            SUBAGENT_LIST_METHOD => self.dispatch_subagent_list(request),
            SUBAGENT_HISTORY_METHOD => self.dispatch_subagent_history(request),
            SETTINGS_DESCRIBE_METHOD => self.dispatch_settings_describe(request),
            SETTINGS_UPDATE_METHOD => self.dispatch_settings_update(request),
            SETTINGS_REPLACE_METHOD => self.dispatch_settings_replace(request),
            SETTINGS_MUTATE_METHOD => self.dispatch_settings_mutate(request),
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
        ChatBackend::complete(&mut self.session, messages)
            .map_err(|error| WebHostError::Chat(error.to_string()))
    }

    pub fn drain_notices(&mut self) -> Vec<String> {
        self.session.drain_notices()
    }

    pub fn publish_event(
        &mut self,
        channel: EventChannel,
        payload: Value,
    ) -> Result<(), WebHostError> {
        self.gateway
            .publish_event(channel, payload)
            .map_err(WebHostError::Gateway)
    }

    pub fn publish_event_with_id(
        &mut self,
        channel: EventChannel,
        rpc_id: RpcId,
        payload: Value,
    ) -> Result<(), WebHostError> {
        self.gateway
            .publish_event_with_id(channel, rpc_id, payload)
            .map_err(WebHostError::Gateway)
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
        let Some(patch) = request.payload().get("patch") else {
            return self.failure(
                request,
                "invalid-payload",
                "settings.update requires an object patch",
                json!({ "field": "patch" }),
            );
        };
        let patch = match CapabilitySettingsStore::parse_section(patch) {
            Ok(patch) => patch,
            Err(error) => return self.settings_rejected(request, error),
        };
        let expected_revision = match expected_revision(request.payload()) {
            Ok(revision) => revision,
            Err(()) => return self.invalid_settings_revision(request),
        };
        let result = self
            .session
            .capability_settings_mut()
            .update(patch, expected_revision);
        self.finish_settings_write(request, result)
    }

    fn dispatch_settings_replace(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if let Err(response) = self.validate_settings_namespace(request) {
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
        let section = match CapabilitySettingsStore::parse_section(section) {
            Ok(section) => section,
            Err(error) => return self.settings_rejected(request, error),
        };
        let expected_revision = match expected_revision(request.payload()) {
            Ok(revision) => revision,
            Err(()) => return self.invalid_settings_revision(request),
        };
        let result = self
            .session
            .capability_settings_mut()
            .replace(section, expected_revision);
        self.finish_settings_write(request, result)
    }

    fn dispatch_settings_mutate(
        &mut self,
        request: &ClientRequest,
    ) -> Result<ServerResponse, WebHostError> {
        if let Err(response) = self.validate_settings_namespace(request) {
            return response;
        }
        let edits = match capability_edits(request.payload()) {
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
        let result = self
            .session
            .capability_settings_mut()
            .mutate(edits, expected_revision);
        self.finish_settings_write(request, result)
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
        result: Result<bool, CapabilitySettingsError>,
    ) -> Result<ServerResponse, WebHostError> {
        match result {
            Ok(changed) => {
                if changed {
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
            "applies": "restart",
            "secrets": [],
            "revision": settings.revision(),
        });
        if user.as_object().is_some_and(|object| !object.is_empty()) {
            view.as_object_mut()
                .expect("capability namespace view is an object")
                .insert("user".to_string(), user);
        }
        view
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
        if self.pending_approval.is_some() {
            return self.failure(
                request,
                "agent-busy",
                "resolve the pending approval before creating another session",
                json!({}),
            );
        }
        let session = match self.session.create_web_session() {
            Ok(session) => session,
            Err(error) => return self.session_failure(request, None, error),
        };
        self.web_history.clear();
        self.web_session_id = Some(session.id().to_string());
        self.pending_approval = None;
        self.publish_event(
            EventChannel::Host,
            json!({
                "type": "host/session-added",
                "sessionId": session.id(),
                "blank": true,
                "cwd": session.cwd(),
            }),
        )?;
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "session/subscribed",
                "sessionId": session.id(),
                "lastSeq": -1,
            }),
        )?;
        self.success(request, json!({ "sessionId": session.id() }))
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
        if self.pending_approval.is_some()
            && self.web_session_id.as_deref() != Some(session_id.as_str())
        {
            return self.failure(
                request,
                "agent-busy",
                "resolve the pending approval before switching sessions",
                json!({ "sessionId": self.web_session_id }),
            );
        }
        let before_seq = optional_u64(request.payload(), "beforeSeq");
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
        let (events, has_more) = history_page(&self.web_history, before_seq, maximum);
        self.success(
            request,
            json!({
                "events": events,
                "hasMore": has_more,
            }),
        )
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
        if !self.session.multi_agent_web_available() {
            return self.success(
                request,
                json!({
                    "entries": [],
                    "parentAvailable": self.web_session_id.as_deref()
                        == Some(parent_session_id.as_str()),
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
                    "mode": "one-shot",
                    "label": agent.name(),
                    "activity": if agent.status() == AgentStatus::Running {
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
        if request.payload().get("mode").and_then(Value::as_str) != Some("one-shot") {
            return self.failure(
                request,
                "subagent-not-resumable",
                "this WebHost exposes one-shot child history only",
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
        if !self.session.multi_agent_web_available() {
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

        let inspected = match self
            .session
            .web_agent_inspect(&root_session_id, &child_session_id)
        {
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

    fn subagent_context(
        &mut self,
        parent_session_id: &str,
    ) -> Result<Option<(String, String, AgentListResult)>, String> {
        let Some(root_session_id) = self.web_session_id.clone() else {
            return Ok(None);
        };
        let graph = self.session.web_agent_graph(&root_session_id)?;
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
        if self.pending_approval.is_some()
            && self.web_session_id.as_deref() != Some(session_id.as_str())
        {
            return self.failure(
                request,
                "agent-busy",
                "resolve the pending approval before switching sessions",
                json!({ "sessionId": self.web_session_id }),
            );
        }
        if let Err(error) = self.ensure_web_session(&session_id) {
            return self.session_failure(request, Some(&session_id), error);
        }
        if self.pending_approval.is_some() {
            return self.failure(
                request,
                "agent-busy",
                "resolve the pending approval before sending another prompt",
                json!({ "sessionId": session_id }),
            );
        }

        let previous_length = self.web_history.len();
        self.web_history.push(ChatMessage::user(content));
        self.publish_session_status(&session_id, true)?;
        let result = self.session.complete(&self.web_history);
        self.publish_session_status(&session_id, false)?;
        match result {
            Ok(reply) => {
                self.web_history.push(ChatMessage::assistant(reply));
                self.publish_turn_events(previous_length, &session_id)?;
                self.success(request, json!({ "accepted": true }))
            }
            Err(_error) if self.session.pending_approval().is_some() => {
                self.publish_turn_events(previous_length, &session_id)?;
                self.publish_pending_approval(&session_id)?;
                self.success(request, json!({ "accepted": true }))
            }
            Err(error) => {
                self.web_history.truncate(previous_length);
                self.failure(
                    request,
                    "model-error",
                    "the model could not complete the prompt",
                    json!({ "message": bounded_message(error.to_string()) }),
                )
            }
        }
    }

    fn ensure_web_session(&mut self, session_id: &str) -> Result<(), String> {
        if self.web_session_id.as_deref() == Some(session_id) {
            return Ok(());
        }
        let messages = self.session.activate_web_session(session_id)?;
        self.web_history = messages;
        self.web_session_id = Some(session_id.to_string());
        self.pending_approval = None;
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

    fn publish_turn_events(&mut self, start: usize, session_id: &str) -> Result<(), WebHostError> {
        let first_seq = first_event_seq_for_message(start);
        let events = session_event_values(&self.web_history);
        for event in events.into_iter().filter(|event| {
            event
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|seq| seq >= first_seq as u64)
        }) {
            self.publish_event(EventChannel::Mux, session_event_payload(session_id, event))?;
        }
        Ok(())
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

    fn publish_pending_approval(&mut self, session_id: &str) -> Result<(), WebHostError> {
        let Some(approval) = self.session.pending_approval() else {
            return Ok(());
        };
        if let Some(pending) = self.pending_approval.as_ref() {
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
        self.pending_approval = Some(WebPendingApproval {
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
        let Some(pending) = self.pending_approval.take() else {
            return Ok(json!({ "accepted": false, "reason": "not-pending" }));
        };
        if response.rpc_id().as_str() != pending.rpc_id.as_str() {
            self.pending_approval = Some(pending);
            return Ok(json!({ "accepted": false, "reason": "not-pending" }));
        }
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
            self.pending_approval = Some(pending);
            return Ok(json!({ "accepted": false, "reason": "bad-response" }));
        }

        let result = match outcome {
            "allowed-once" => self.session.approve_pending_model_tool(),
            _ => self.session.deny_pending_model_tool(),
        };
        self.publish_event(
            EventChannel::Mux,
            json!({
                "type": "approval/resolved",
                "sessionId": pending.session_id,
                "approvalId": pending.approval_id,
                "outcome": outcome,
            }),
        )?;
        match result {
            Ok(management) => {
                if let Some(reply) = management.assistant_reply {
                    let start = self.web_history.len();
                    self.web_history.push(ChatMessage::assistant(reply));
                    self.publish_turn_events(start, &pending.session_id)?;
                }
                if self.session.pending_approval().is_some() {
                    self.publish_pending_approval(&pending.session_id)?;
                }
            }
            Err(error) => {
                self.publish_event(
                    EventChannel::Mux,
                    json!({
                        "type": "session/error",
                        "sessionId": pending.session_id,
                        "message": bounded_message(error),
                    }),
                )?;
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
        let mut events = self
            .gateway
            .take_events(channel, maximum_events, maximum_encoded_bytes)
            .map_err(WebHostError::Gateway)?;
        if channel == EventChannel::Mux
            && events.is_empty()
            && self.pending_approval.is_some()
            && !self.gateway.has_events(EventChannel::Mux)
        {
            let session_id = self
                .pending_approval
                .as_ref()
                .map(|pending| pending.session_id.clone())
                .expect("pending approval checked above");
            WebHost::publish_pending_approval(self, &session_id)?;
            events = self
                .gateway
                .take_events(channel, maximum_events, maximum_encoded_bytes)
                .map_err(WebHostError::Gateway)?;
        }
        Ok(events)
    }
}

fn expected_revision(payload: &Value) -> Result<Option<u64>, ()> {
    match payload.get("expectedRevision") {
        None => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(()),
    }
}

fn capability_edits(payload: &Value) -> Result<Vec<CapabilityEdit>, String> {
    let ops = payload
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| "settings.mutate requires an ops array".to_string())?;
    if ops.len() > MAX_SETTINGS_MUTATIONS {
        return Err(format!(
            "settings.mutate accepts at most {MAX_SETTINGS_MUTATIONS} operations"
        ));
    }
    ops.iter()
        .map(|operation| {
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
                .filter(|path| path.len() == 1)
                .and_then(|path| path[0].as_str())
                .and_then(CapabilitySetting::parse)
                .ok_or_else(|| {
                    "settings mutations require one supported capability path".to_string()
                })?;
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
                    Ok(CapabilityEdit::Set(path, value))
                }
                "unset" => {
                    if object
                        .keys()
                        .any(|key| !matches!(key.as_str(), "op" | "path"))
                    {
                        return Err("settings unset mutation contains an unknown field".to_string());
                    }
                    Ok(CapabilityEdit::Unset(path))
                }
                _ => Err("settings mutation op must be set or unset".to_string()),
            }
        })
        .collect()
}

fn capability_settings_schema() -> Value {
    json!({
        "uid": 14,
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
            "14": {
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
                    "multi_agent": 13
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

fn optional_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
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
    (!text.trim().is_empty()).then_some(text)
}

fn history_page(
    messages: &[ChatMessage],
    before_seq: Option<u64>,
    maximum_messages: usize,
) -> (Vec<Value>, bool) {
    let events = session_event_values(messages);
    let end = before_seq
        .and_then(|seq| usize::try_from(seq).ok())
        .unwrap_or(events.len())
        .min(events.len());
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

fn first_event_seq_for_message(message_index: usize) -> usize {
    let turn = message_index / 2;
    if message_index % 2 == 0 {
        turn * 6
    } else {
        turn * 6 + 3
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
