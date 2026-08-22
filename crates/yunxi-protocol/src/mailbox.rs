//! Typed encrypted companion mailbox operations.

use serde::{Deserialize, Serialize};

use crate::WorkspaceGrant;

pub const COMPANION_MAILBOX_ENQUEUE_OPERATION: &str = "enqueue";
pub const COMPANION_MAILBOX_LIST_OPERATION: &str = "list";
pub const COMPANION_MAILBOX_GET_OPERATION: &str = "get";
pub const COMPANION_MAILBOX_MARK_READ_OPERATION: &str = "mark_read";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxItemKind {
    ProactiveMessage,
    LoveLetter,
    Reminder,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxEnqueueRequest {
    grant: WorkspaceGrant,
    kind: MailboxItemKind,
    subject: String,
    content: String,
    reason: String,
    idempotency_key: String,
}

impl MailboxEnqueueRequest {
    pub fn new(
        grant: WorkspaceGrant,
        kind: MailboxItemKind,
        subject: impl Into<String>,
        content: impl Into<String>,
        reason: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            grant,
            kind,
            subject: subject.into(),
            content: content.into(),
            reason: reason.into(),
            idempotency_key: idempotency_key.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxSummary {
    id: String,
    kind: MailboxItemKind,
    subject: String,
    reason: String,
    read: bool,
    created_at_millis: u128,
}

impl MailboxSummary {
    pub fn new(
        id: impl Into<String>,
        kind: MailboxItemKind,
        subject: impl Into<String>,
        reason: impl Into<String>,
        read: bool,
        created_at_millis: u128,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            subject: subject.into(),
            reason: reason.into(),
            read,
            created_at_millis,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> MailboxItemKind {
        self.kind
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn read(&self) -> bool {
        self.read
    }

    pub fn created_at_millis(&self) -> u128 {
        self.created_at_millis
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxEntry {
    summary: MailboxSummary,
    content: String,
}

impl MailboxEntry {
    pub fn new(summary: MailboxSummary, content: impl Into<String>) -> Self {
        Self {
            summary,
            content: content.into(),
        }
    }

    pub fn summary(&self) -> &MailboxSummary {
        &self.summary
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxListRequest {
    grant: WorkspaceGrant,
    limit: usize,
    unread_only: bool,
}

impl MailboxListRequest {
    pub fn new(grant: WorkspaceGrant) -> Self {
        Self {
            grant,
            limit: 50,
            unread_only: false,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn unread_only(mut self, unread_only: bool) -> Self {
        self.unread_only = unread_only;
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn only_unread(&self) -> bool {
        self.unread_only
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxListResult {
    items: Vec<MailboxSummary>,
    unread_count: usize,
    warnings: Vec<String>,
    truncated: bool,
}

impl MailboxListResult {
    pub fn new(
        items: Vec<MailboxSummary>,
        unread_count: usize,
        warnings: Vec<String>,
        truncated: bool,
    ) -> Self {
        Self {
            items,
            unread_count,
            warnings,
            truncated,
        }
    }

    pub fn items(&self) -> &[MailboxSummary] {
        &self.items
    }

    pub fn unread_count(&self) -> usize {
        self.unread_count
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxGetRequest {
    grant: WorkspaceGrant,
    item_id: String,
}

impl MailboxGetRequest {
    pub fn new(grant: WorkspaceGrant, item_id: impl Into<String>) -> Self {
        Self {
            grant,
            item_id: item_id.into(),
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxGetResult {
    entry: Option<MailboxEntry>,
    warnings: Vec<String>,
}

impl MailboxGetResult {
    pub fn new(entry: Option<MailboxEntry>, warnings: Vec<String>) -> Self {
        Self { entry, warnings }
    }

    pub fn entry(&self) -> Option<&MailboxEntry> {
        self.entry.as_ref()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxMarkReadRequest {
    grant: WorkspaceGrant,
    item_id: String,
    read: bool,
}

impl MailboxMarkReadRequest {
    pub fn new(grant: WorkspaceGrant, item_id: impl Into<String>, read: bool) -> Self {
        Self {
            grant,
            item_id: item_id.into(),
            read,
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn read(&self) -> bool {
        self.read
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MailboxMutationResult {
    item: Option<MailboxSummary>,
    created: bool,
    warnings: Vec<String>,
}

impl MailboxMutationResult {
    pub fn new(item: Option<MailboxSummary>, created: bool, warnings: Vec<String>) -> Self {
        Self {
            item,
            created,
            warnings,
        }
    }

    pub fn item(&self) -> Option<&MailboxSummary> {
        self.item.as_ref()
    }

    pub fn created(&self) -> bool {
        self.created
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_request_round_trip_keeps_idempotency_key() {
        let request = MailboxEnqueueRequest::new(
            WorkspaceGrant::read_write("."),
            MailboxItemKind::Reminder,
            "subject",
            "content",
            "reason",
            "key-1",
        );
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<MailboxEnqueueRequest>(&json).expect("deserialize request"),
            request
        );
    }
}
