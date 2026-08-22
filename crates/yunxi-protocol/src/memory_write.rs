//! Typed extraction, persistence, and review calls for long-term memory.

use serde::{Deserialize, Serialize};

use crate::{MemoryContextKind, WorkspaceGrant};

pub const MEMORY_WRITE_EXTRACT_OPERATION: &str = "extract";
pub const MEMORY_WRITE_REVIEW_OPERATION: &str = "review";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryWriteRequest {
    grant: WorkspaceGrant,
    prompt: String,
    assistant_response: Option<String>,
    source_session_id: Option<String>,
}

impl MemoryWriteRequest {
    pub fn new(grant: WorkspaceGrant, prompt: impl Into<String>) -> Self {
        Self {
            grant,
            prompt: prompt.into(),
            assistant_response: None,
            source_session_id: None,
        }
    }

    pub fn with_assistant_response(mut self, response: impl Into<String>) -> Self {
        self.assistant_response = Some(response.into());
        self
    }

    pub fn with_source_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.source_session_id = Some(session_id.into());
        self
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn assistant_response(&self) -> Option<&str> {
        self.assistant_response.as_deref()
    }

    pub fn source_session_id(&self) -> Option<&str> {
        self.source_session_id.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteStatus {
    Active,
    Pending,
    Rejected,
    Discarded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryWriteSummary {
    id: Option<String>,
    kind: MemoryContextKind,
    content: String,
    status: MemoryWriteStatus,
    reason: String,
}

impl MemoryWriteSummary {
    pub fn new(
        id: Option<String>,
        kind: MemoryContextKind,
        content: impl Into<String>,
        status: MemoryWriteStatus,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id,
            kind,
            content: content.into(),
            status,
            reason: reason.into(),
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn kind(&self) -> MemoryContextKind {
        self.kind
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn status(&self) -> MemoryWriteStatus {
        self.status
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryWriteResult {
    records: Vec<MemoryWriteSummary>,
    warnings: Vec<String>,
}

impl MemoryWriteResult {
    pub fn new(records: Vec<MemoryWriteSummary>, warnings: Vec<String>) -> Self {
        Self { records, warnings }
    }

    pub fn records(&self) -> &[MemoryWriteSummary] {
        &self.records
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryReviewAction {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryReviewRequest {
    grant: WorkspaceGrant,
    memory_id: String,
    action: MemoryReviewAction,
}

impl MemoryReviewRequest {
    pub fn new(
        grant: WorkspaceGrant,
        memory_id: impl Into<String>,
        action: MemoryReviewAction,
    ) -> Self {
        Self {
            grant,
            memory_id: memory_id.into(),
            action,
        }
    }

    pub fn grant(&self) -> &WorkspaceGrant {
        &self.grant
    }

    pub fn memory_id(&self) -> &str {
        &self.memory_id
    }

    pub fn action(&self) -> MemoryReviewAction {
        self.action
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryReviewResult {
    record: Option<MemoryWriteSummary>,
    warnings: Vec<String>,
}

impl MemoryReviewResult {
    pub fn new(record: Option<MemoryWriteSummary>, warnings: Vec<String>) -> Self {
        Self { record, warnings }
    }

    pub fn record(&self) -> Option<&MemoryWriteSummary> {
        self.record.as_ref()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_review_round_trip_preserves_explicit_action() {
        let request = MemoryReviewRequest::new(
            WorkspaceGrant::read_write("."),
            "memory-1",
            MemoryReviewAction::Approve,
        );
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<MemoryReviewRequest>(&json).expect("deserialize request"),
            request
        );
    }
}
