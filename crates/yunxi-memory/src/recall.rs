//! Bounded boot and prompt-relevant recall policy inherited from legacy YunXi.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use yunxi_protocol::{MemoryContextRecord, MemoryRecallRequest, MemoryRecallResult};

use crate::record::{
    MemoryKind, MemoryLayer, MemorySensitivity, MemoryStatus, StoredMemoryRecord, contains_any,
    now_millis,
};
use crate::store::{MemoryLoad, MemoryStore, MemoryStoreError};

const MAX_QUERY_CHARS: usize = 16 * 1024;
const BOOT_MAX_RECORDS: usize = 6;
const BOOT_BUDGET_CHARS: usize = 1000;
const DYNAMIC_MAX_RECORDS: usize = 8;
const DYNAMIC_BUDGET_CHARS: usize = 1200;
const RECALL_RELEVANCE_THRESHOLD: f32 = 2.0;
const BOOT_MIN_CONFIDENCE: f32 = 0.6;
const BOOT_MIN_IMPORTANCE: f32 = 0.4;

pub fn recall(request: &MemoryRecallRequest) -> Result<MemoryRecallResult, MemoryRecallError> {
    if request.query().chars().count() > MAX_QUERY_CHARS {
        return Err(MemoryRecallError::QueryTooLong {
            maximum: MAX_QUERY_CHARS,
        });
    }
    let store = MemoryStore::for_workspace(request.cwd())?;
    Ok(recall_loaded(
        store.load(),
        store.workspace_fingerprint(),
        request.query(),
        request.include_boot_context(),
    ))
}

fn recall_loaded(
    load: MemoryLoad,
    workspace_fingerprint: &str,
    query: &str,
    include_boot_context: bool,
) -> MemoryRecallResult {
    let now = now_millis();
    let timeline_query = is_relationship_timeline_query(query);
    let mut recallable = Vec::new();
    let mut historical = Vec::new();

    for record in load.records {
        if record.sensitivity == MemorySensitivity::High
            || !record.scope.matches_workspace(workspace_fingerprint)
        {
            continue;
        }
        if record.is_recallable(now) {
            recallable.push(record);
        } else if timeline_query
            && record.status == MemoryStatus::Active
            && is_graph_memory(record.kind)
        {
            historical.push(record);
        }
    }

    let mut boot = Vec::new();
    let mut truncated = false;
    if include_boot_context {
        let mut candidates = recallable
            .iter()
            .filter(|record| {
                is_boot_candidate(record) && !(timeline_query && is_graph_memory(record.kind))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            boot_score(right)
                .partial_cmp(&boot_score(left))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.updated_at_millis.cmp(&left.updated_at_millis))
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut used = 0;
        for record in candidates {
            let chars = record.content.chars().count();
            if boot.len() >= BOOT_MAX_RECORDS || used + chars > BOOT_BUDGET_CHARS {
                truncated = true;
                continue;
            }
            used += chars;
            boot.push(record.to_context_record());
        }
    }

    let boot_ids = boot
        .iter()
        .map(MemoryContextRecord::id)
        .collect::<BTreeSet<_>>();
    let mut dynamic_candidates = recallable
        .iter()
        .filter(|record| !boot_ids.contains(record.id.as_str()))
        .collect::<Vec<_>>();
    dynamic_candidates.extend(historical.iter());

    if timeline_query {
        dynamic_candidates.sort_by(|left, right| {
            right
                .ordering_time()
                .cmp(&left.ordering_time())
                .then_with(|| left.id.cmp(&right.id))
        });
    } else {
        dynamic_candidates.retain(|record| {
            !query.trim().is_empty() && score_record(record, query) >= RECALL_RELEVANCE_THRESHOLD
        });
        dynamic_candidates.sort_by(|left, right| {
            score_record(right, query)
                .partial_cmp(&score_record(left, query))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.updated_at_millis.cmp(&left.updated_at_millis))
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    let mut dynamic = Vec::new();
    let mut used = 0;
    for record in dynamic_candidates {
        let chars = record.content.chars().count();
        if dynamic.len() >= DYNAMIC_MAX_RECORDS || used + chars > DYNAMIC_BUDGET_CHARS {
            truncated = true;
            continue;
        }
        used += chars;
        dynamic.push(record.to_context_record());
    }

    MemoryRecallResult::new(
        workspace_fingerprint,
        boot,
        dynamic,
        load.warnings,
        truncated,
    )
}

fn is_boot_candidate(record: &StoredMemoryRecord) -> bool {
    record.confidence >= BOOT_MIN_CONFIDENCE
        && record.importance >= BOOT_MIN_IMPORTANCE
        && matches!(
            record.layer,
            MemoryLayer::Profile
                | MemoryLayer::Preference
                | MemoryLayer::Relationship
                | MemoryLayer::Workspace
        )
        && matches!(
            record.kind,
            MemoryKind::Preference
                | MemoryKind::PersonalFact
                | MemoryKind::RelationshipNote
                | MemoryKind::ProjectContext
                | MemoryKind::Correction
        )
}

fn boot_score(record: &StoredMemoryRecord) -> f32 {
    let layer_bonus = match record.layer {
        MemoryLayer::Preference | MemoryLayer::Workspace => 1.0,
        MemoryLayer::Profile | MemoryLayer::Relationship => 0.75,
        MemoryLayer::Episode | MemoryLayer::ToolTrace | MemoryLayer::Unknown => 0.0,
    };
    record.importance * 2.0 + record.confidence + layer_bonus
}

fn score_record(record: &StoredMemoryRecord, query: &str) -> f32 {
    let query_lower = query.to_ascii_lowercase();
    let content_lower = record.content.to_ascii_lowercase();
    let mut score = 0.0;
    for term in query_lower
        .split_whitespace()
        .filter(|term| term.len() >= 2)
    {
        if content_lower.contains(term) {
            score += 2.0;
        }
    }
    if matches!(record.scope, crate::record::MemoryScope::Workspace { .. }) {
        score += 0.5;
    }
    if kind_trigger_matches(record.kind, &query_lower, &content_lower) {
        score += 2.0;
    }
    if score > 0.0 {
        score += record.importance + record.confidence * 0.5;
    }
    score
}

fn kind_trigger_matches(kind: MemoryKind, query: &str, content: &str) -> bool {
    match kind {
        MemoryKind::ProjectContext | MemoryKind::ToolTraceSummary => {
            contains_any(
                query,
                &[
                    "项目",
                    "仓库",
                    "代码",
                    "源码",
                    "workspace",
                    "repo",
                    "project",
                ],
            ) && contains_any(
                content,
                &[
                    "项目",
                    "仓库",
                    "代码",
                    "源码",
                    "workspace",
                    "repo",
                    "project",
                ],
            )
        }
        MemoryKind::PersonalFact => contains_any(
            query,
            &["我", "我的", "个人", "偏好", "profile", "me", "my"],
        ),
        MemoryKind::Goal => contains_any(query, &["目标", "计划", "下一步", "goal", "plan"]),
        MemoryKind::Correction => contains_any(
            query,
            &["要求", "约束", "不要", "纠正", "rule", "constraint"],
        ),
        MemoryKind::RelationshipNote | MemoryKind::EmotionalState => {
            contains_any(query, &["关系", "情绪", "感受", "relationship", "emotion"])
        }
        MemoryKind::Preference | MemoryKind::Event => false,
    }
}

fn is_graph_memory(kind: MemoryKind) -> bool {
    matches!(
        kind,
        MemoryKind::Preference
            | MemoryKind::Correction
            | MemoryKind::RelationshipNote
            | MemoryKind::EmotionalState
            | MemoryKind::Goal
            | MemoryKind::ProjectContext
            | MemoryKind::Event
    )
}

fn is_relationship_timeline_query(query: &str) -> bool {
    contains_any(
        query,
        &[
            "关系变化",
            "感情变化",
            "时间线",
            "什么时候",
            "之前发生",
            "relationship timeline",
            "how has our relationship",
            "when did",
        ],
    )
}

#[derive(Debug)]
pub enum MemoryRecallError {
    Store(MemoryStoreError),
    QueryTooLong { maximum: usize },
}

impl fmt::Display for MemoryRecallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::QueryTooLong { maximum } => {
                write!(formatter, "memory query exceeds {maximum} characters")
            }
        }
    }
}

impl Error for MemoryRecallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::QueryTooLong { .. } => None,
        }
    }
}

impl From<MemoryStoreError> for MemoryRecallError {
    fn from(error: MemoryStoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(value: serde_json::Value) -> StoredMemoryRecord {
        let mut record = serde_json::from_value::<StoredMemoryRecord>(value)
            .expect("deserialize memory fixture");
        record.validate().expect("validate memory fixture");
        record
    }

    #[test]
    fn boot_recall_keeps_safe_matching_scope_and_excludes_sensitive_records() {
        let records = vec![
            record(json!({
                "id": "language",
                "schema_version": 3,
                "scope": "global_user",
                "kind": "preference",
                "content": "默认使用中文回答",
                "confidence": 0.9,
                "importance": 0.9,
                "sensitivity": "low",
                "status": "active",
                "created_at_millis": 1,
                "updated_at_millis": 1
            })),
            record(json!({
                "id": "other-workspace",
                "schema_version": 3,
                "scope": {"workspace": {"root_fingerprint": "other"}},
                "kind": "project_context",
                "content": "另一个项目使用 Python",
                "confidence": 0.9,
                "importance": 0.9,
                "sensitivity": "low",
                "status": "active",
                "created_at_millis": 1,
                "updated_at_millis": 1
            })),
            record(json!({
                "id": "private",
                "schema_version": 3,
                "scope": "global_user",
                "kind": "personal_fact",
                "content": "private value",
                "confidence": 0.9,
                "importance": 0.9,
                "sensitivity": "high",
                "status": "active",
                "created_at_millis": 1,
                "updated_at_millis": 1
            })),
        ];
        let result = recall_loaded(
            MemoryLoad {
                records,
                warnings: Vec::new(),
            },
            "current",
            "请用中文回答",
            true,
        );

        assert_eq!(result.boot().len(), 1);
        assert_eq!(result.boot()[0].id(), "language");
        assert!(result.dynamic().is_empty());
    }
}
