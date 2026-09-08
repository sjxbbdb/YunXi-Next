//! Typed persistent conversation session operations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ChatMessage, WorkspaceGrant};

pub const STORAGE_SESSIONS_APPEND_OPERATION: &str = "append";
pub const STORAGE_SESSIONS_CREATE_OPERATION: &str = "create";
pub const STORAGE_SESSIONS_LOAD_OPERATION: &str = "load";
pub const STORAGE_SESSIONS_LIST_OPERATION: &str = "list";
pub const STORAGE_SESSIONS_MUTATE_OPERATION: &str = "mutate";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    grant: WorkspaceGrant,
}

impl SessionCreateRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self { grant }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionCreateResult {
    session: SessionSnapshot,
}

impl SessionCreateResult {
    pub fn new(session: SessionSnapshot) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &SessionSnapshot {
        &self.session
    }

    pub fn into_session(self) -> SessionSnapshot {
        self.session
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionAppendRequest {
    grant: WorkspaceGrant,
    session_id: Option<String>,
    user_message: String,
    assistant_message: String,
    provider: Option<String>,
    model: Option<String>,
}

impl SessionAppendRequest {
    pub fn new(
        grant: WorkspaceGrant,
        user_message: impl Into<String>,
        assistant_message: impl Into<String>,
    ) -> Self {
        Self {
            grant,
            session_id: None,
            user_message: user_message.into(),
            assistant_message: assistant_message.into(),
            provider: None,
            model: None,
        }
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn user_message(&self) -> &str {
        &self.user_message
    }

    pub fn assistant_message(&self) -> &str {
        &self.assistant_message
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    id: String,
    cwd: PathBuf,
    messages: Vec<ChatMessage>,
    provider: Option<String>,
    model: Option<String>,
    parent_id: Option<String>,
    title: String,
    archived: bool,
    pinned: bool,
    legacy: bool,
    created_at_millis: u128,
    updated_at_millis: u128,
}

impl SessionSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        cwd: impl Into<PathBuf>,
        messages: Vec<ChatMessage>,
        provider: Option<String>,
        model: Option<String>,
        parent_id: Option<String>,
        title: impl Into<String>,
        archived: bool,
        pinned: bool,
        legacy: bool,
        created_at_millis: u128,
        updated_at_millis: u128,
    ) -> Self {
        Self {
            id: id.into(),
            cwd: cwd.into(),
            messages,
            provider,
            model,
            parent_id,
            title: title.into(),
            archived,
            pinned,
            legacy,
            created_at_millis,
            updated_at_millis,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn archived(&self) -> bool {
        self.archived
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    pub fn legacy(&self) -> bool {
        self.legacy
    }

    pub fn created_at_millis(&self) -> u128 {
        self.created_at_millis
    }

    pub fn updated_at_millis(&self) -> u128 {
        self.updated_at_millis
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    id: String,
    title: String,
    message_count: usize,
    archived: bool,
    pinned: bool,
    legacy: bool,
    updated_at_millis: u128,
}

impl From<&SessionSnapshot> for SessionSummary {
    fn from(session: &SessionSnapshot) -> Self {
        Self {
            id: session.id.clone(),
            title: session.title.clone(),
            message_count: session.messages.len(),
            archived: session.archived,
            pinned: session.pinned,
            legacy: session.legacy,
            updated_at_millis: session.updated_at_millis,
        }
    }
}

impl SessionSummary {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn message_count(&self) -> usize {
        self.message_count
    }

    pub fn archived(&self) -> bool {
        self.archived
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    pub fn legacy(&self) -> bool {
        self.legacy
    }

    pub fn updated_at_millis(&self) -> u128 {
        self.updated_at_millis
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLoadRequest {
    grant: WorkspaceGrant,
    session_id: String,
}

impl SessionLoadRequest {
    pub fn new(grant: WorkspaceGrant, session_id: impl Into<String>) -> Self {
        Self {
            grant,
            session_id: session_id.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLoadResult {
    session: Option<SessionSnapshot>,
    warnings: Vec<String>,
}

impl SessionLoadResult {
    pub fn new(session: Option<SessionSnapshot>, warnings: Vec<String>) -> Self {
        Self { session, warnings }
    }

    pub fn session(&self) -> Option<&SessionSnapshot> {
        self.session.as_ref()
    }

    pub fn into_session(self) -> Option<SessionSnapshot> {
        self.session
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionListRequest {
    grant: WorkspaceGrant,
    include_archived: bool,
    limit: usize,
}

impl SessionListRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self {
            grant,
            include_archived: false,
            limit: 50,
        }
    }

    pub fn with_archived(mut self, include: bool) -> Self {
        self.include_archived = include;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn include_archived(&self) -> bool {
        self.include_archived
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionListResult {
    sessions: Vec<SessionSummary>,
    warnings: Vec<String>,
    truncated: bool,
}

impl SessionListResult {
    pub fn new(sessions: Vec<SessionSummary>, warnings: Vec<String>, truncated: bool) -> Self {
        Self {
            sessions,
            warnings,
            truncated,
        }
    }

    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMutation {
    Archive,
    Unarchive,
    Pin,
    Unpin,
    Fork,
    Rename,
    SelectModel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionMutationRequest {
    grant: WorkspaceGrant,
    session_id: String,
    mutation: SessionMutation,
    title: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    at_message: Option<usize>,
}

impl SessionMutationRequest {
    pub fn new(
        grant: WorkspaceGrant,
        session_id: impl Into<String>,
        mutation: SessionMutation,
    ) -> Self {
        Self {
            grant,
            session_id: session_id.into(),
            mutation,
            title: None,
            provider: None,
            model: None,
            at_message: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Limit a fork to this many persisted chat messages. The Web adapter
    /// maps its event sequence to this message boundary before invoking the
    /// storage capability.
    pub fn with_at_message(mut self, at_message: usize) -> Self {
        self.at_message = Some(at_message);
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn mutation(&self) -> SessionMutation {
        self.mutation
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn at_message(&self) -> Option<usize> {
        self.at_message
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionMutationResult {
    session: Option<SessionSnapshot>,
    warnings: Vec<String>,
}

impl SessionMutationResult {
    pub fn new(session: Option<SessionSnapshot>, warnings: Vec<String>) -> Self {
        Self { session, warnings }
    }

    pub fn session(&self) -> Option<&SessionSnapshot> {
        self.session.as_ref()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_snapshot_round_trip_keeps_conversation_order() {
        let session = SessionSnapshot::new(
            "session-1",
            ".",
            vec![ChatMessage::user("hello"), ChatMessage::assistant("hi")],
            Some("fixture".to_string()),
            Some("model".to_string()),
            None,
            "hello",
            false,
            false,
            false,
            1,
            2,
        );
        let json = serde_json::to_string(&session).expect("serialize session");
        assert_eq!(
            serde_json::from_str::<SessionSnapshot>(&json).expect("deserialize session"),
            session
        );
    }
}
