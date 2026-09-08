//! Legacy-compatible memory record schema used only inside the memory process.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use yunxi_protocol::{MemoryContextKind, MemoryContextRecord};

const CURRENT_SCHEMA_VERSION: u32 = 3;
const MAX_MEMORY_ID_CHARS: usize = 256;
const MAX_MEMORY_CONTENT_CHARS: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryScope {
    GlobalUser,
    Workspace { root_fingerprint: String },
    AgentIdentity,
    Relationship,
}

impl MemoryScope {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::GlobalUser => "global_user".to_string(),
            Self::Workspace { root_fingerprint } => format!("workspace:{root_fingerprint}"),
            Self::AgentIdentity => "agent_identity".to_string(),
            Self::Relationship => "relationship".to_string(),
        }
    }

    pub(crate) fn matches_workspace(&self, fingerprint: &str) -> bool {
        match self {
            Self::Workspace { root_fingerprint } => root_fingerprint == fingerprint,
            Self::GlobalUser | Self::AgentIdentity | Self::Relationship => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryKind {
    Preference,
    PersonalFact,
    RelationshipNote,
    EmotionalState,
    Goal,
    ProjectContext,
    Correction,
    Event,
    ToolTraceSummary,
}

impl MemoryKind {
    pub(crate) fn protocol_kind(self) -> MemoryContextKind {
        match self {
            Self::Preference => MemoryContextKind::Preference,
            Self::PersonalFact => MemoryContextKind::PersonalFact,
            Self::RelationshipNote => MemoryContextKind::RelationshipNote,
            Self::EmotionalState => MemoryContextKind::EmotionalState,
            Self::Goal => MemoryContextKind::Goal,
            Self::ProjectContext => MemoryContextKind::ProjectContext,
            Self::Correction => MemoryContextKind::Correction,
            Self::Event => MemoryContextKind::Event,
            Self::ToolTraceSummary => MemoryContextKind::ToolTraceSummary,
        }
    }

    pub(crate) fn storage_key(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::PersonalFact => "personal_fact",
            Self::RelationshipNote => "relationship_note",
            Self::EmotionalState => "emotional_state",
            Self::Goal => "goal",
            Self::ProjectContext => "project_context",
            Self::Correction => "correction",
            Self::Event => "event",
            Self::ToolTraceSummary => "tool_trace_summary",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemorySensitivity {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryStatus {
    Active,
    #[default]
    Pending,
    Rejected,
    Archived,
}

impl MemoryStatus {
    pub(crate) fn storage_group(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Rejected => "rejected",
            Self::Archived => "archived",
        }
    }

    pub(crate) fn protocol_status(self) -> yunxi_protocol::MemoryWriteStatus {
        match self {
            Self::Active => yunxi_protocol::MemoryWriteStatus::Active,
            Self::Pending => yunxi_protocol::MemoryWriteStatus::Pending,
            Self::Rejected | Self::Archived => yunxi_protocol::MemoryWriteStatus::Rejected,
        }
    }

    pub(crate) fn is_pending(self) -> bool {
        self == Self::Pending
    }

    pub(crate) fn is_archived(self) -> bool {
        self == Self::Archived
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryLayer {
    Profile,
    Preference,
    Relationship,
    Workspace,
    Episode,
    ToolTrace,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MemoryTemporal {
    #[serde(default)]
    pub(crate) observed_at_millis: u128,
    #[serde(default)]
    pub(crate) event_at_millis: Option<u128>,
    #[serde(default)]
    pub(crate) valid_from_millis: Option<u128>,
    #[serde(default)]
    pub(crate) expires_at_millis: Option<u128>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MemoryInvalidation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) supersedes: Vec<String>,
    #[serde(default)]
    pub(crate) superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) conflicts_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires_reason: Option<String>,
    #[serde(default)]
    pub(crate) invalidated_at_millis: Option<u128>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredMemoryRecord {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) schema_version: u32,
    pub(crate) scope: MemoryScope,
    pub(crate) kind: MemoryKind,
    pub(crate) content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_session_id: Option<String>,
    #[serde(default = "default_confidence")]
    pub(crate) confidence: f32,
    #[serde(default = "default_importance")]
    pub(crate) importance: f32,
    #[serde(default)]
    pub(crate) sensitivity: MemorySensitivity,
    #[serde(default)]
    pub(crate) status: MemoryStatus,
    #[serde(default)]
    pub(crate) created_at_millis: u128,
    #[serde(default)]
    pub(crate) updated_at_millis: u128,
    #[serde(default)]
    pub(crate) dedup_key: String,
    #[serde(default = "default_revision")]
    pub(crate) revision: u32,
    #[serde(default = "default_merged_count")]
    pub(crate) merged_count: u32,
    #[serde(default)]
    pub(crate) layer: MemoryLayer,
    #[serde(default)]
    pub(crate) temporal: MemoryTemporal,
    #[serde(default)]
    pub(crate) invalidation: MemoryInvalidation,
}

impl StoredMemoryRecord {
    pub(crate) fn new(
        id: impl Into<String>,
        scope: MemoryScope,
        kind: MemoryKind,
        content: impl Into<String>,
        now: u128,
    ) -> Self {
        let mut record = Self {
            id: id.into(),
            schema_version: CURRENT_SCHEMA_VERSION,
            scope,
            kind,
            content: content.into(),
            source_session_id: None,
            confidence: 0.82,
            importance: 0.65,
            sensitivity: MemorySensitivity::Low,
            status: MemoryStatus::Pending,
            created_at_millis: now,
            updated_at_millis: now,
            dedup_key: String::new(),
            revision: 1,
            merged_count: 1,
            layer: layer_for_kind(kind),
            temporal: MemoryTemporal {
                observed_at_millis: now,
                valid_from_millis: Some(now),
                ..MemoryTemporal::default()
            },
            invalidation: MemoryInvalidation::default(),
        };
        record.dedup_key = record.dedup_key();
        record
    }

    pub(crate) fn validate(&mut self) -> Result<(), String> {
        if self.schema_version > CURRENT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported future memory schema_version={}; current={CURRENT_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.id.trim().is_empty() || self.id.chars().count() > MAX_MEMORY_ID_CHARS {
            return Err(format!(
                "memory id must contain 1 to {MAX_MEMORY_ID_CHARS} characters"
            ));
        }
        if self.content.trim().is_empty() || self.content.chars().count() > MAX_MEMORY_CONTENT_CHARS
        {
            return Err(format!(
                "memory content must contain 1 to {MAX_MEMORY_CONTENT_CHARS} characters"
            ));
        }
        if !self.confidence.is_finite() || !self.importance.is_finite() {
            return Err("memory confidence and importance must be finite".to_string());
        }
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self.importance = self.importance.clamp(0.0, 1.0);
        if self.updated_at_millis == 0 {
            self.updated_at_millis = self.created_at_millis;
        }
        if self.temporal.observed_at_millis == 0 {
            self.temporal.observed_at_millis = self.created_at_millis;
        }
        if self.temporal.valid_from_millis.is_none() && self.created_at_millis != 0 {
            self.temporal.valid_from_millis = Some(self.created_at_millis);
        }
        if self.revision == 0 {
            self.revision = 1;
        }
        if self.merged_count == 0 {
            self.merged_count = 1;
        }
        if self.layer == MemoryLayer::Unknown {
            self.layer = layer_for_kind(self.kind);
        }
        Ok(())
    }

    pub(crate) fn is_recallable(&self, now: u128) -> bool {
        self.status == MemoryStatus::Active
            && self
                .temporal
                .valid_from_millis
                .is_none_or(|valid_from| valid_from <= now)
            && self
                .temporal
                .expires_at_millis
                .is_none_or(|expires_at| expires_at > now)
            && self.invalidation.invalidated_at_millis.is_none()
            && self.invalidation.superseded_by.is_none()
    }

    pub(crate) fn dedup_key(&self) -> String {
        if !self.dedup_key.trim().is_empty() {
            return self.dedup_key.clone();
        }
        format!(
            "{}|{}|{}",
            self.scope.label(),
            self.kind.storage_key(),
            normalized_content(self.kind, &self.content)
        )
    }

    pub(crate) fn ordering_time(&self) -> u128 {
        self.temporal
            .event_at_millis
            .unwrap_or(self.temporal.observed_at_millis)
            .max(self.updated_at_millis)
            .max(self.created_at_millis)
    }

    pub(crate) fn to_context_record(&self) -> MemoryContextRecord {
        MemoryContextRecord::new(
            self.id.clone(),
            self.scope.label(),
            self.kind.protocol_kind(),
            self.content.clone(),
        )
    }

    pub(crate) fn protocol_kind(&self) -> MemoryContextKind {
        self.kind.protocol_kind()
    }

    pub(crate) fn set_status(&mut self, status: MemoryStatus, now: u128) {
        self.status = status;
        self.updated_at_millis = now;
        self.revision = self.revision.saturating_add(1);
    }
}

pub(crate) fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn layer_for_kind(kind: MemoryKind) -> MemoryLayer {
    match kind {
        MemoryKind::Preference => MemoryLayer::Preference,
        MemoryKind::PersonalFact => MemoryLayer::Profile,
        MemoryKind::RelationshipNote | MemoryKind::EmotionalState => MemoryLayer::Relationship,
        MemoryKind::ProjectContext | MemoryKind::Correction => MemoryLayer::Workspace,
        MemoryKind::Goal | MemoryKind::Event => MemoryLayer::Episode,
        MemoryKind::ToolTraceSummary => MemoryLayer::ToolTrace,
    }
}

fn normalized_content(kind: MemoryKind, content: &str) -> String {
    if kind == MemoryKind::Preference {
        if contains_any(content, &["中文", "汉语", "Chinese", "Mandarin"]) {
            return "language:zh".to_string();
        }
        if contains_any(content, &["英文", "英语", "English"]) {
            return "language:en".to_string();
        }
    }
    let mut output = String::new();
    let mut last_space = false;
    for character in content.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            last_space = false;
        } else if character.is_whitespace() {
            if !last_space && !output.is_empty() {
                output.push(' ');
                last_space = true;
            }
        } else if is_cjk(character) {
            output.push(character);
            last_space = false;
        }
    }
    output.trim().to_string()
}

pub(crate) fn contains_any(value: &str, needles: &[&str]) -> bool {
    let lower = value.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| value.contains(needle) || lower.contains(&needle.to_ascii_lowercase()))
}

fn is_cjk(character: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&character)
        || ('\u{3400}'..='\u{4dbf}').contains(&character)
        || ('\u{f900}'..='\u{faff}').contains(&character)
}

fn default_confidence() -> f32 {
    0.75
}

fn default_importance() -> f32 {
    0.5
}

fn default_revision() -> u32 {
    1
}

fn default_merged_count() -> u32 {
    1
}
