//! Typed payloads for the read-only context, persona, and memory capabilities.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONTEXT_COMPOSE_OPERATION: &str = "compose";
pub const MEMORY_RECALL_OPERATION: &str = "recall";
pub const PERSONA_CONTEXT_COMPILE_OPERATION: &str = "compile";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextComposeRequest {
    cwd: PathBuf,
}

impl ContextComposeRequest {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextComposeResult {
    instructions: String,
    sources: Vec<PathBuf>,
}

impl ContextComposeResult {
    pub fn new(instructions: impl Into<String>, sources: Vec<PathBuf>) -> Self {
        Self {
            instructions: instructions.into(),
            sources,
        }
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryContextKind {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryContextRecord {
    id: String,
    scope: String,
    kind: MemoryContextKind,
    content: String,
}

impl MemoryContextRecord {
    pub fn new(
        id: impl Into<String>,
        scope: impl Into<String>,
        kind: MemoryContextKind,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            scope: scope.into(),
            kind,
            content: content.into(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn kind(&self) -> MemoryContextKind {
        self.kind
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecallRequest {
    cwd: PathBuf,
    query: String,
    include_boot_context: bool,
}

impl MemoryRecallRequest {
    pub fn new(cwd: impl Into<PathBuf>, query: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            query: query.into(),
            include_boot_context: true,
        }
    }

    pub fn with_boot_context(mut self, include: bool) -> Self {
        self.include_boot_context = include;
        self
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn include_boot_context(&self) -> bool {
        self.include_boot_context
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecallResult {
    workspace_fingerprint: String,
    boot: Vec<MemoryContextRecord>,
    dynamic: Vec<MemoryContextRecord>,
    warnings: Vec<String>,
    truncated: bool,
}

impl MemoryRecallResult {
    pub fn new(
        workspace_fingerprint: impl Into<String>,
        boot: Vec<MemoryContextRecord>,
        dynamic: Vec<MemoryContextRecord>,
        warnings: Vec<String>,
        truncated: bool,
    ) -> Self {
        Self {
            workspace_fingerprint: workspace_fingerprint.into(),
            boot,
            dynamic,
            warnings,
            truncated,
        }
    }

    pub fn workspace_fingerprint(&self) -> &str {
        &self.workspace_fingerprint
    }

    pub fn boot(&self) -> &[MemoryContextRecord] {
        &self.boot
    }

    pub fn dynamic(&self) -> &[MemoryContextRecord] {
        &self.dynamic
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaContextRequest {
    boot_memories: Vec<MemoryContextRecord>,
    dynamic_memories: Vec<MemoryContextRecord>,
    include_boot_context: bool,
}

impl PersonaContextRequest {
    pub fn new(
        boot_memories: Vec<MemoryContextRecord>,
        dynamic_memories: Vec<MemoryContextRecord>,
        include_boot_context: bool,
    ) -> Self {
        Self {
            boot_memories,
            dynamic_memories,
            include_boot_context,
        }
    }

    pub fn boot_memories(&self) -> &[MemoryContextRecord] {
        &self.boot_memories
    }

    pub fn dynamic_memories(&self) -> &[MemoryContextRecord] {
        &self.dynamic_memories
    }

    pub fn include_boot_context(&self) -> bool {
        self.include_boot_context
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaContextResult {
    content: Option<String>,
    profile_id: String,
    display_name: String,
    memory_count: usize,
    warnings: Vec<String>,
}

impl PersonaContextResult {
    pub fn new(
        content: Option<String>,
        profile_id: impl Into<String>,
        display_name: impl Into<String>,
        memory_count: usize,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            content,
            profile_id: profile_id.into(),
            display_name: display_name.into(),
            memory_count,
            warnings,
        }
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn memory_count(&self) -> usize {
        self.memory_count
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_payloads_round_trip_without_losing_path_or_memory_type() {
        let request = MemoryRecallRequest::new(PathBuf::from(r"C:\workspace"), "偏好")
            .with_boot_context(false);
        let json = serde_json::to_string(&request).expect("serialize memory request");
        let decoded =
            serde_json::from_str::<MemoryRecallRequest>(&json).expect("deserialize memory request");
        assert_eq!(decoded, request);

        let record = MemoryContextRecord::new(
            "memory-1",
            "global_user",
            MemoryContextKind::Preference,
            "默认使用中文",
        );
        let json = serde_json::to_string(&record).expect("serialize memory record");
        assert_eq!(
            serde_json::from_str::<MemoryContextRecord>(&json).expect("deserialize memory record"),
            record
        );
    }
}
