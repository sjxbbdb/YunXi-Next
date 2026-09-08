//! Bounded dsh-compatible unary RPC dispatch.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use yunxi_web_contract::{
    ClientRequest, ClientResponse, EVENTS_HOST_METHOD, EVENTS_MUX_METHOD, EventChannel, RpcError,
    RpcId, RpcMessage, RpcResult, ServerResponse,
};

use crate::events::EventBuffer;
use crate::{GatewayError, GatewayProjection, GatewaySessionSummary};

pub const AGENT_PRESET_LIST_METHOD: &str = "agentPreset.list";
pub const COMMANDS_LIST_METHOD: &str = "commands/list";
pub const CREDENTIALS_DESCRIBE_METHOD: &str = "credentials.describe";
pub const DYNAMIC_CORDIS_INVENTORY_METHOD: &str = "dynamicCordisRunner/inventory";
pub const DYNAMIC_CORDIS_SYNC_INSPECT_METHOD: &str = "dynamicCordisRunner/syncInspectManifest";
pub const HEALTH_STATUS_METHOD: &str = "health.status";
pub const HOST_DESCRIBE_METHOD: &str = "host.describe";
pub const LLM_PROVIDERS_METHOD: &str = "llm.providers";
pub const MEMORY_STATUS_METHOD: &str = "memory.status";
pub const MEMORY_LIST_METHOD: &str = "memory.list";
pub const MEMORY_SHOW_METHOD: &str = "memory.show";
pub const PERSONA_STATUS_METHOD: &str = "persona.status";
pub const PERSONA_LIST_METHOD: &str = "persona.list";
pub const PERSONA_PROFILE_METHOD: &str = "persona.profile";
pub const RELATIONSHIP_STATUS_METHOD: &str = "relationship.status";
pub const RELATIONSHIP_LIST_METHOD: &str = "relationship.list";
pub const MAILBOX_LIST_METHOD: &str = "mailbox.list";
pub const MAILBOX_GET_METHOD: &str = "mailbox.get";
pub const MAILBOX_MARK_READ_METHOD: &str = "mailbox.markRead";
pub const VOICE_DOCTOR_METHOD: &str = "voice.doctor";
pub const VOICE_DEVICES_METHOD: &str = "voice.devices";
pub const VOICE_TRANSCRIBE_METHOD: &str = "voice.transcribe";
pub const VOICE_SPEAK_METHOD: &str = "voice.speak";
pub const VOICE_CHAT_METHOD: &str = "voice.chat";
pub const VOICE_TALK_METHOD: &str = "voice.talk";
pub const VOICE_PLAYBACK_METHOD: &str = "voice.playback";
pub const VOICE_SAVE_METHOD: &str = "voice.save";
pub const VOICE_CANCEL_METHOD: &str = "voice.cancel";
pub const WEIXIN_STATUS_METHOD: &str = "weixin.status";
pub const WEIXIN_DOCTOR_METHOD: &str = "weixin.doctor";
pub const WEIXIN_LOGIN_METHOD: &str = "weixin.login";
pub const WEIXIN_POLL_LOGIN_METHOD: &str = "weixin.pollLogin";
pub const WEIXIN_SERVE_METHOD: &str = "weixin.serve";
pub const WEIXIN_SERVE_START_METHOD: &str = "weixin.serveStart";
pub const WEIXIN_SERVE_STATUS_METHOD: &str = "weixin.serveStatus";
pub const WEIXIN_SERVE_STOP_METHOD: &str = "weixin.serveStop";
pub const WEIXIN_QUEUED_METHOD: &str = "weixin.queued";
pub const WEIXIN_SEND_METHOD: &str = "weixin.send";
pub const WEIXIN_REPLY_METHOD: &str = "weixin.reply";
pub const WEIXIN_CONTROL_METHOD: &str = "weixin.control";
pub const WEIXIN_PAIR_METHOD: &str = "weixin.pair";
pub const WEIXIN_SESSION_METHOD: &str = "weixin.session";
pub const WEIXIN_LOGOUT_METHOD: &str = "weixin.logout";
pub const PLUGIN_INVENTORY_LIST_METHOD: &str = "pluginInventory/list";
pub const SETTINGS_DESCRIBE_METHOD: &str = "settings.describe";
pub const SETTINGS_MUTATE_METHOD: &str = "settings.mutate";
pub const SETTINGS_REPLACE_METHOD: &str = "settings.replace";
pub const SETTINGS_UPDATE_METHOD: &str = "settings.update";
pub const SESSION_LIST_METHOD: &str = "session.list";
pub const SESSION_CREATE_METHOD: &str = "session.create";
pub const SESSION_CANCEL_METHOD: &str = "session.cancel";
pub const SESSION_HISTORY_METHOD: &str = "session.history";
pub const SESSION_MODELS_METHOD: &str = "session.models";
pub const SESSION_PROMPT_METHOD: &str = "session.prompt";
pub const SESSION_SEARCH_METHOD: &str = "session.search";
pub const SESSION_RENAME_METHOD: &str = "session.rename";
pub const SESSION_FORK_METHOD: &str = "session.fork";
pub const SESSION_SELECT_MODEL_METHOD: &str = "session.selectModel";
pub const SESSION_UPDATE_QUEUE_METHOD: &str = "session.updateQueue";
pub const SESSION_ATTACHMENT_METHOD: &str = "session.attachment";
pub const SKILL_LIST_METHOD: &str = "skill.list";
pub const SUBAGENT_LIST_METHOD: &str = "subagent.list";
pub const SUBAGENT_HISTORY_METHOD: &str = "subagent.history";
pub const SUBAGENT_PROMPT_METHOD: &str = "subagent.prompt";
pub const SUBAGENT_INTERRUPT_METHOD: &str = "subagent.interrupt";
pub const WORKSPACE_LIST_METHOD: &str = "workspace.list";

const DEFAULT_WORKSPACE_ID: &str = "yunxi-default";
const DSH_WELCOME_NOTICE_VERSION: &str = "2026-08-13.1";

/// Backend boundary consumed by the transport carrier.
///
/// The trait intentionally exposes projections and already-validated RPC
/// envelopes only. A carrier cannot invoke a plugin, obtain a credential, or
/// bypass the Host approval path through this interface.
pub trait GatewayBackend {
    type Error;

    fn refresh(&mut self);

    fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, Self::Error>;

    fn respond(&mut self, response: &ClientResponse) -> Result<Value, Self::Error>;

    fn take_events(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, Self::Error>;
}

pub struct Gateway {
    projection: GatewayProjection,
    events: EventBuffer,
}

impl Gateway {
    pub fn new(projection: GatewayProjection) -> Self {
        Self {
            projection,
            events: EventBuffer::new(),
        }
    }

    pub fn projection(&self) -> &GatewayProjection {
        &self.projection
    }

    pub fn replace_projection(&mut self, projection: GatewayProjection) {
        self.projection = projection;
    }

    pub fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, GatewayError> {
        let result = match request.method() {
            AGENT_PRESET_LIST_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!({
                    "presets": [],
                    "authorable": false,
                    "hasDocument": false,
                })),
                Err(error) => RpcResult::failure(error),
            },
            COMMANDS_LIST_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!([])),
                Err(error) => RpcResult::failure(error),
            },
            CREDENTIALS_DESCRIBE_METHOD => match self.credentials_description(request.payload()) {
                Ok(value) => RpcResult::success(value),
                Err(error) => RpcResult::failure(error),
            },
            DYNAMIC_CORDIS_INVENTORY_METHOD => {
                match object_payload(request.method(), request.payload()) {
                    Ok(()) => RpcResult::success(json!([])),
                    Err(error) => RpcResult::failure(error),
                }
            }
            DYNAMIC_CORDIS_SYNC_INSPECT_METHOD => {
                match object_payload(request.method(), request.payload()) {
                    Ok(()) => RpcResult::success(Value::Null),
                    Err(error) => RpcResult::failure(error),
                }
            }
            HEALTH_STATUS_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(serde_json::to_value(self.projection.status())?),
                Err(error) => RpcResult::failure(error),
            },
            HOST_DESCRIBE_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(self.host_description()),
                Err(error) => RpcResult::failure(error),
            },
            LLM_PROVIDERS_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(self.llm_providers()),
                Err(error) => RpcResult::failure(error),
            },
            PLUGIN_INVENTORY_LIST_METHOD => {
                match object_payload(request.method(), request.payload()) {
                    Ok(()) => {
                        RpcResult::success(serde_json::to_value(self.projection.dsh_inventory())?)
                    }
                    Err(error) => RpcResult::failure(error),
                }
            }
            SESSION_LIST_METHOD => match session_list_payload(request.payload()) {
                Ok(()) => RpcResult::success(json!({
                    "items": self.projection.sessions(),
                })),
                Err(error) => RpcResult::failure(error),
            },
            SESSION_MODELS_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(self.session_models()),
                Err(error) => RpcResult::failure(error),
            },
            SETTINGS_DESCRIBE_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!({
                    "writable": false,
                    "hasDocument": false,
                    "namespaces": [{
                        "ns": "ui-onboarding",
                        "schema": {
                            "type": "object",
                            "properties": {
                                "welcomeNoticeVersion": { "type": "string" },
                            },
                        },
                        "value": {
                            "welcomeNoticeVersion": DSH_WELCOME_NOTICE_VERSION,
                        },
                        "applies": "live",
                        "secrets": [],
                        "revision": 0,
                    }],
                })),
                Err(error) => RpcResult::failure(error),
            },
            SKILL_LIST_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!({ "skills": [] })),
                Err(error) => RpcResult::failure(error),
            },
            SUBAGENT_LIST_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!({
                    "entries": [],
                    "parentAvailable": true,
                })),
                Err(error) => RpcResult::failure(error),
            },
            SUBAGENT_HISTORY_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(json!({
                    "events": [],
                    "hasMore": false,
                })),
                Err(error) => RpcResult::failure(error),
            },
            WORKSPACE_LIST_METHOD => match object_payload(request.method(), request.payload()) {
                Ok(()) => RpcResult::success(self.workspace_list()),
                Err(error) => RpcResult::failure(error),
            },
            EVENTS_MUX_METHOD | EVENTS_HOST_METHOD => RpcResult::failure(rpc_error(
                "stream-only-method",
                "event channels are exposed by the event carrier, not unary RPC",
                json!({ "method": request.method() }),
            )),
            _ => RpcResult::failure(rpc_error(
                "method-not-supported",
                "the requested Web RPC method is not enabled by this Gateway",
                json!({ "method": request.method() }),
            )),
        };
        Ok(ServerResponse::new(request.rpc_id().clone(), result)?)
    }

    fn host_description(&self) -> Value {
        let mut value = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": self.projection.cwd(),
            "attachedSessions": self
                .projection
                .sessions()
                .iter()
                .filter(|session| session.running())
                .count(),
            "home": self.projection.home(),
            "canOpenPath": false,
        });
        let object = value
            .as_object_mut()
            .expect("host description is a JSON object");
        if let Some(provider) = self.projection.status().provider() {
            object.insert("provider".to_string(), Value::String(provider.to_string()));
        }
        if let Some(model) = self.projection.status().model() {
            object.insert("model".to_string(), Value::String(model.to_string()));
        }
        value
    }

    fn credentials_description(&self, payload: &Value) -> Result<Value, RpcError> {
        object_payload(CREDENTIALS_DESCRIBE_METHOD, payload)?;
        let Some(refs) = payload.get("refs").and_then(Value::as_array) else {
            return Err(rpc_error(
                "invalid-payload",
                "credentials.describe requires a refs array",
                json!({ "method": CREDENTIALS_DESCRIBE_METHOD, "field": "refs" }),
            ));
        };
        if refs.len() > 64
            || refs
                .iter()
                .any(|reference| !valid_credential_ref(reference))
        {
            return Err(rpc_error(
                "invalid-payload",
                "credentials.describe refs must contain at most 64 environment-style names",
                json!({ "method": CREDENTIALS_DESCRIBE_METHOD, "field": "refs" }),
            ));
        }
        let expected_ref = self
            .projection
            .status()
            .provider()
            .filter(|provider| provider.to_ascii_lowercase().contains("deepseek"))
            .map(|_| "DEEPSEEK_API_KEY");
        let mut credentials = Map::new();
        for reference in refs.iter().filter_map(Value::as_str) {
            let configured = expected_ref == Some(reference);
            let mut descriptor = json!({
                "configured": configured,
                "writable": false,
            });
            if configured {
                descriptor
                    .as_object_mut()
                    .expect("credential descriptor is an object")
                    .insert("source".to_string(), Value::String("host".to_string()));
            }
            credentials.insert(reference.to_string(), descriptor);
        }
        Ok(json!({ "credentials": credentials }))
    }

    fn llm_providers(&self) -> Value {
        let provider = self.projection.status().provider().unwrap_or("yunxi");
        json!({
            "providers": [{
                "provider": provider,
                "displayName": provider,
                "settingsNs": "",
                "settingsPath": [],
                "active": true,
                "declared": true,
            }],
        })
    }

    fn session_models(&self) -> Value {
        let status = self.projection.status();
        let provider = status.provider();
        let model = status.model();
        let routable = status.protocol_ready()
            && provider.is_some_and(|value| !value.trim().is_empty())
            && model.is_some_and(|value| !value.trim().is_empty());
        let provider = provider.unwrap_or("yunxi");
        let model = model.unwrap_or("default");
        json!({
            "current": {
                "provider": provider,
                "model": model,
            },
            "routable": routable,
            "groups": [{
                "id": provider,
                "name": provider,
                "models": [{
                    "id": model,
                    "name": model,
                }],
            }],
            "failures": [],
        })
    }

    fn workspace_list(&self) -> Value {
        let updated_at = self
            .projection
            .sessions()
            .iter()
            .map(GatewaySessionSummary::updated_at)
            .max()
            .unwrap_or_else(now_millis);
        let timestamp = unix_millis_rfc3339(updated_at);
        let title = Path::new(self.projection.cwd())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("YunXi Next");
        let session_ids = self
            .projection
            .sessions()
            .iter()
            .map(GatewaySessionSummary::session_id)
            .collect::<Vec<_>>();
        json!({
            "items": [{
                "workspaceId": DEFAULT_WORKSPACE_ID,
                "path": self.projection.cwd(),
                "title": title,
                "sessionIds": session_ids,
                "createdAt": timestamp,
                "updatedAt": timestamp,
            }],
            "archivedSessionIds": [],
        })
    }

    pub fn dispatch_message(&mut self, message: RpcMessage) -> Result<RpcMessage, GatewayError> {
        match message {
            RpcMessage::ClientRequest(request) => {
                self.dispatch(&request).map(RpcMessage::ServerResponse)
            }
            RpcMessage::ServerRequest(_) => Err(GatewayError::UnexpectedMessage {
                message: "server-request cannot be submitted to unary Gateway dispatch".to_string(),
            }),
            RpcMessage::ServerResponse(_) => Err(GatewayError::UnexpectedMessage {
                message: "server-response cannot be submitted to unary Gateway dispatch"
                    .to_string(),
            }),
            RpcMessage::ClientResponse(_) => Err(GatewayError::UnexpectedMessage {
                message: "client-response cannot be submitted to unary Gateway dispatch"
                    .to_string(),
            }),
        }
    }

    pub fn publish_event(
        &mut self,
        channel: EventChannel,
        payload: Value,
    ) -> Result<(), GatewayError> {
        self.events.publish(channel, payload)
    }

    pub fn publish_event_with_id(
        &mut self,
        channel: EventChannel,
        rpc_id: RpcId,
        payload: Value,
    ) -> Result<(), GatewayError> {
        self.events.publish_with_id(channel, rpc_id, payload)
    }

    pub fn drain_events(&mut self, channel: EventChannel) -> Vec<RpcMessage> {
        self.events.drain(channel)
    }

    pub fn has_events(&self, channel: EventChannel) -> bool {
        self.events.has_events(channel)
    }

    pub fn take_events(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, GatewayError> {
        self.events
            .take_bounded(channel, maximum_events, maximum_encoded_bytes)
    }
}

impl GatewayBackend for Gateway {
    type Error = GatewayError;

    fn refresh(&mut self) {}

    fn dispatch(&mut self, request: &ClientRequest) -> Result<ServerResponse, Self::Error> {
        Gateway::dispatch(self, request)
    }

    fn respond(&mut self, _response: &ClientResponse) -> Result<Value, Self::Error> {
        Ok(json!({
            "accepted": false,
            "reason": "not-pending",
        }))
    }

    fn take_events(
        &mut self,
        channel: EventChannel,
        maximum_events: usize,
        maximum_encoded_bytes: usize,
    ) -> Result<Vec<RpcMessage>, Self::Error> {
        Gateway::take_events(self, channel, maximum_events, maximum_encoded_bytes)
    }
}

fn object_payload(method: &str, payload: &Value) -> Result<(), RpcError> {
    if payload.is_object() {
        Ok(())
    } else {
        Err(rpc_error(
            "invalid-payload",
            "the RPC payload must be a JSON object",
            json!({ "method": method }),
        ))
    }
}

fn session_list_payload(payload: &Value) -> Result<(), RpcError> {
    object_payload(SESSION_LIST_METHOD, payload)?;
    if payload
        .get("cursor")
        .is_some_and(|cursor| !cursor.is_string())
    {
        return Err(rpc_error(
            "invalid-payload",
            "session.list cursor must be a string",
            json!({ "method": SESSION_LIST_METHOD, "field": "cursor" }),
        ));
    }
    Ok(())
}

fn rpc_error(code: &str, message: &str, details: Value) -> RpcError {
    RpcError::new(code, message, details).expect("Gateway error shape is bounded")
}

fn valid_credential_ref(value: &Value) -> bool {
    let Some(value) = value.as_str() else {
        return false;
    };
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn unix_millis_rfc3339(millis: u64) -> String {
    let seconds = millis / 1_000;
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        millis % 1_000
    )
}

fn civil_date_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch.saturating_add(719_468);
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::unix_millis_rfc3339;

    #[test]
    fn unix_millis_are_formatted_for_workspace_views() {
        assert_eq!(unix_millis_rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            unix_millis_rfc3339(1_767_225_600_123),
            "2026-01-01T00:00:00.123Z"
        );
    }
}
