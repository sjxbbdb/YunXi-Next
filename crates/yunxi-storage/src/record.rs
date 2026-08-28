//! Validated YunXi Next session records and read-only legacy projection.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yunxi_protocol::{ChatMessage, SessionSnapshot};

const SESSION_SCHEMA_VERSION: u32 = 1;
const MAX_SESSION_ID_CHARS: usize = 128;
const MAX_MESSAGE_CHARS: usize = 256 * 1024;
const MAX_MESSAGES: usize = 512;
static NEXT_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredSession {
    schema_version: u32,
    id: String,
    cwd: PathBuf,
    messages: Vec<ChatMessage>,
    provider: Option<String>,
    model: Option<String>,
    parent_id: Option<String>,
    title: String,
    archived: bool,
    pinned: bool,
    created_at_millis: u128,
    updated_at_millis: u128,
}

impl StoredSession {
    pub(crate) fn empty(cwd: PathBuf) -> Result<Self, String> {
        let now = now_millis();
        let session = Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id: generate_id(now),
            cwd,
            messages: Vec::new(),
            provider: None,
            model: None,
            parent_id: None,
            title: "untitled session".to_string(),
            archived: false,
            pinned: false,
            created_at_millis: now,
            updated_at_millis: now,
        };
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn new(cwd: PathBuf, user: &str, assistant: &str) -> Result<Self, String> {
        let now = now_millis();
        let session = Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id: generate_id(now),
            cwd,
            messages: vec![
                ChatMessage::user(user.to_string()),
                ChatMessage::assistant(assistant.to_string()),
            ],
            provider: None,
            model: None,
            parent_id: None,
            title: preview_title(user),
            archived: false,
            pinned: false,
            created_at_millis: now,
            updated_at_millis: now,
        };
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn from_legacy(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "legacy session root must be an object".to_string())?;
        let id = string_field(object, "id")?;
        let cwd = PathBuf::from(string_field(object, "cwd")?);
        let prompt = string_field(object, "prompt")?;
        let final_response = optional_string_field(object, "final_response");
        let mut messages = vec![ChatMessage::user(prompt.clone())];
        if let Some(response) = final_response {
            messages.push(ChatMessage::assistant(response));
        }
        let created_at_millis = integer_field(object, "created_at_millis").unwrap_or_default();
        let updated_at_millis =
            integer_field(object, "updated_at_millis").unwrap_or(created_at_millis);
        let session = Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id,
            cwd,
            messages,
            provider: optional_string_field(object, "provider"),
            model: optional_string_field(object, "model"),
            parent_id: optional_string_field(object, "parent_id"),
            title: optional_string_field(object, "title").unwrap_or_else(|| preview_title(&prompt)),
            archived: bool_field(object, "archived").unwrap_or(false),
            pinned: bool_field(object, "pinned").unwrap_or(false),
            created_at_millis,
            updated_at_millis,
        };
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != SESSION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported session schema_version={}; expected {SESSION_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        validate_session_id(&self.id)?;
        if self.messages.len() > MAX_MESSAGES {
            return Err(format!("session has more than {MAX_MESSAGES} messages"));
        }
        if self
            .messages
            .iter()
            .any(|message| message.content().chars().count() > MAX_MESSAGE_CHARS)
        {
            return Err(format!(
                "session message exceeds {MAX_MESSAGE_CHARS} characters"
            ));
        }
        Ok(())
    }

    pub(crate) fn append(
        &mut self,
        user: &str,
        assistant: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), String> {
        if self.messages.len().saturating_add(2) > MAX_MESSAGES {
            return Err(format!("session has reached {MAX_MESSAGES} messages"));
        }
        for message in [user, assistant] {
            if message.chars().count() > MAX_MESSAGE_CHARS {
                return Err(format!(
                    "session message exceeds {MAX_MESSAGE_CHARS} characters"
                ));
            }
        }
        self.messages.push(ChatMessage::user(user.to_string()));
        self.messages
            .push(ChatMessage::assistant(assistant.to_string()));
        if let Some(provider) = provider {
            self.provider = Some(provider.to_string());
        }
        if let Some(model) = model {
            self.model = Some(model.to_string());
        }
        self.updated_at_millis = now_millis();
        Ok(())
    }

    pub(crate) fn set_provider_model(&mut self, provider: Option<&str>, model: Option<&str>) {
        if let Some(provider) = provider {
            self.provider = Some(provider.to_string());
        }
        if let Some(model) = model {
            self.model = Some(model.to_string());
        }
    }

    pub(crate) fn import_from_legacy(mut self) -> Self {
        let parent_id = self.id;
        let now = now_millis();
        self.id = generate_id(now);
        self.parent_id = Some(parent_id);
        self.title = format!("Imported: {}", self.title);
        self.archived = false;
        self.pinned = false;
        self.created_at_millis = now;
        self.updated_at_millis = now;
        self
    }

    pub(crate) fn fork(mut self) -> Self {
        let parent_id = self.id;
        let now = now_millis();
        self.id = generate_id(now);
        self.parent_id = Some(parent_id);
        self.title = format!("Fork of {}", self.title);
        self.archived = false;
        self.pinned = false;
        self.created_at_millis = now;
        self.updated_at_millis = now;
        self
    }

    pub(crate) fn set_archived(&mut self, archived: bool) {
        self.archived = archived;
        self.updated_at_millis = now_millis();
    }

    pub(crate) fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        self.updated_at_millis = now_millis();
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn snapshot(&self, legacy: bool) -> SessionSnapshot {
        SessionSnapshot::new(
            self.id.clone(),
            self.cwd.clone(),
            self.messages.clone(),
            self.provider.clone(),
            self.model.clone(),
            self.parent_id.clone(),
            self.title.clone(),
            self.archived,
            self.pinned,
            legacy,
            self.created_at_millis,
            self.updated_at_millis,
        )
    }
}

pub(crate) fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.chars().count() > MAX_SESSION_ID_CHARS {
        return Err(format!(
            "session id must contain 1 to {MAX_SESSION_ID_CHARS} characters"
        ));
    }
    if id.contains("..")
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("session id contains unsupported characters".to_string());
    }
    Ok(())
}

fn generate_id(now: u128) -> String {
    let counter = NEXT_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("yn-{now}-{counter}")
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn preview_title(value: &str) -> String {
    const MAX_CHARS: usize = 48;
    let value = value.trim();
    if value.is_empty() {
        return "untitled session".to_string();
    }
    if value.chars().count() <= MAX_CHARS {
        return value.to_string();
    }
    let mut title = value
        .chars()
        .take(MAX_CHARS.saturating_sub(3))
        .collect::<String>();
    title.push_str("...");
    title
}

fn string_field(object: &serde_json::Map<String, Value>, name: &str) -> Result<String, String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("legacy session field `{name}` must be a string"))
}

fn optional_string_field(object: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn integer_field(object: &serde_json::Map<String, Value>, name: &str) -> Option<u128> {
    object.get(name).and_then(Value::as_u64).map(u128::from)
}

fn bool_field(object: &serde_json::Map<String, Value>, name: &str) -> Option<bool> {
    object.get(name).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_record_projects_to_chat_messages() {
        let record = StoredSession::from_legacy(json!({
            "id": "yunxi-1-1",
            "cwd": ".",
            "prompt": "hello",
            "final_response": "hi",
            "created_at_millis": 1,
            "updated_at_millis": 2
        }))
        .expect("project legacy record");
        let snapshot = record.snapshot(true);
        assert_eq!(snapshot.messages().len(), 2);
        assert!(snapshot.legacy());
    }

    #[test]
    fn unsafe_session_ids_are_rejected() {
        assert!(validate_session_id("../outside").is_err());
        assert!(validate_session_id("valid-session_1").is_ok());
    }
}
