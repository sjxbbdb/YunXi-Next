//! Rule-based memory extraction, privacy policy, deduplication, and review.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use yunxi_protocol::{
    MemoryReviewAction, MemoryReviewRequest, MemoryReviewResult, MemoryWriteRequest,
    MemoryWriteResult, MemoryWriteStatus, MemoryWriteSummary,
};

use crate::record::{
    MemoryKind, MemoryScope, MemorySensitivity, MemoryStatus, StoredMemoryRecord, contains_any,
    now_millis,
};
use crate::store::{MemoryStore, MemoryStoreError};

const MAX_PROMPT_CHARS: usize = 64 * 1024;
const MAX_RESPONSE_CHARS: usize = 128 * 1024;
static NEXT_MEMORY_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn extract_and_store(
    request: &MemoryWriteRequest,
) -> Result<MemoryWriteResult, MemoryWriteError> {
    if !request.grant().allows_next_write() {
        return Err(MemoryWriteError::WriteNotGranted);
    }
    if request.prompt().chars().count() > MAX_PROMPT_CHARS {
        return Err(MemoryWriteError::PromptTooLong {
            maximum: MAX_PROMPT_CHARS,
        });
    }
    if request
        .assistant_response()
        .is_some_and(|response| response.chars().count() > MAX_RESPONSE_CHARS)
    {
        return Err(MemoryWriteError::ResponseTooLong {
            maximum: MAX_RESPONSE_CHARS,
        });
    }

    let store = MemoryStore::from_grant(request.grant())?;
    let mut load = store.load();
    if !store.enabled(&mut load.warnings) {
        load.warnings
            .push("memory is disabled by the user; no records were written".to_string());
        return Ok(MemoryWriteResult::new(Vec::new(), load.warnings));
    }
    let now = now_millis();
    let mut summaries = Vec::new();
    for candidate in deduplicate_candidates(detect_prompt_memories(request.prompt())) {
        let sensitivity = classify_sensitivity(&candidate.content);
        if contains_secret(&candidate.content) || contains_secret(request.prompt()) {
            summaries.push(MemoryWriteSummary::new(
                None,
                candidate.kind.protocol_kind(),
                "[redacted: secret-like memory was not stored]",
                MemoryWriteStatus::Discarded,
                "privacy:secret-like-content",
            ));
            continue;
        }

        let mut status = write_status(candidate.kind, sensitivity);
        let scope = scope_for_kind(candidate.kind, store.workspace_fingerprint());
        let mut record = StoredMemoryRecord::new(
            generate_memory_id(now),
            scope,
            candidate.kind,
            candidate.content,
            now,
        );
        record.sensitivity = sensitivity;
        record.status = status;
        record.source_session_id = request.source_session_id().map(ToString::to_string);

        let exact = load
            .records
            .iter()
            .find(|existing| existing.dedup_key() == record.dedup_key());
        if let Some(existing) = exact {
            summaries.push(summary(
                existing,
                format!("{}:deduplicated", candidate.reason),
            ));
            continue;
        }

        if let Some(conflicting) = language_conflict(&load.records, &record) {
            status = MemoryStatus::Pending;
            record.status = status;
            record
                .invalidation
                .conflicts_with
                .push(conflicting.id.clone());
        }
        store.append(&record)?;
        summaries.push(summary(&record, candidate.reason));
    }

    Ok(MemoryWriteResult::new(summaries, load.warnings))
}

pub fn review_memory(
    request: &MemoryReviewRequest,
) -> Result<MemoryReviewResult, MemoryWriteError> {
    if !request.grant().allows_next_write() {
        return Err(MemoryWriteError::WriteNotGranted);
    }
    if request.memory_id().trim().is_empty() || request.memory_id().chars().count() > 256 {
        return Err(MemoryWriteError::InvalidMemoryId);
    }
    let store = MemoryStore::from_grant(request.grant())?;
    let load = store.load();
    let Some(mut record) = load
        .records
        .iter()
        .find(|record| record.id == request.memory_id())
        .cloned()
    else {
        return Ok(MemoryReviewResult::new(None, load.warnings));
    };
    if record.status == MemoryStatus::Pending {
        let status = match request.action() {
            MemoryReviewAction::Approve => MemoryStatus::Active,
            MemoryReviewAction::Reject => MemoryStatus::Rejected,
        };
        record.set_status(status, now_millis());
        store.append(&record)?;
    }
    Ok(MemoryReviewResult::new(
        Some(summary(&record, "reviewed")),
        load.warnings,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    kind: MemoryKind,
    content: String,
    reason: String,
}

fn detect_prompt_memories(prompt: &str) -> Vec<Candidate> {
    let value = prompt.trim();
    if value.is_empty() {
        return Vec::new();
    }
    let lower = value.to_ascii_lowercase();
    let mut candidates = Vec::new();
    if let Some(payload) = explicit_memory_payload(value, &lower) {
        let kind = classify_explicit_kind(payload);
        candidates.push(Candidate {
            kind,
            content: normalize_explicit_content(kind, payload),
            reason: format!("rule:explicit-remember-{}", kind.storage_key()),
        });
    }
    if (value.contains("以后") && contains_any(value, &["中文", "说中文"]))
        || contains_any(value, &["说中文", "用中文"])
    {
        candidates.push(Candidate {
            kind: MemoryKind::Preference,
            content: "用户偏好后续默认使用中文交流。".to_string(),
            reason: "rule:language-preference".to_string(),
        });
    }
    if contains_any(value, &["英文", "英语", "english"])
        && (value.contains("以后")
            || contains_any(
                &lower,
                &[
                    "from now on",
                    "by default",
                    "answer in english",
                    "reply in english",
                    "use english",
                ],
            ))
    {
        candidates.push(Candidate {
            kind: MemoryKind::Preference,
            content: "用户偏好后续默认使用英文交流。".to_string(),
            reason: "rule:language-preference".to_string(),
        });
    }
    if value.contains("不要") || lower.contains("do not") {
        candidates.push(Candidate {
            kind: MemoryKind::Correction,
            content: format!("用户纠正/限制：{}", compact(value, 160)),
            reason: "rule:correction".to_string(),
        });
    }
    if value.contains("硬性要求") || value.contains("硬约束") {
        candidates.push(Candidate {
            kind: MemoryKind::ProjectContext,
            content: format!("项目硬性约束：{}", compact(value, 180)),
            reason: "rule:project-hard-constraint".to_string(),
        });
    }
    if value.contains("我的") || lower.contains("my ") {
        candidates.push(Candidate {
            kind: MemoryKind::PersonalFact,
            content: format!("用户自述事实候选：{}", compact(value, 160)),
            reason: "rule:self-disclosure".to_string(),
        });
    }
    if value.contains("关系") || lower.contains("relationship") {
        candidates.push(Candidate {
            kind: MemoryKind::RelationshipNote,
            content: format!("关系事件候选：{}", compact(value, 160)),
            reason: "rule:relationship-note".to_string(),
        });
    }
    if contains_any(value, &["情绪", "感到", "i feel", "emotion"]) {
        candidates.push(Candidate {
            kind: MemoryKind::EmotionalState,
            content: format!("情绪状态候选：{}", compact(value, 160)),
            reason: "rule:emotional-state".to_string(),
        });
    }
    if value.contains("目标") || lower.contains("my goal") {
        candidates.push(Candidate {
            kind: MemoryKind::Goal,
            content: format!("目标候选：{}", compact(value, 160)),
            reason: "rule:goal".to_string(),
        });
    }
    candidates
}

fn deduplicate_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut unique = BTreeMap::new();
    for candidate in candidates {
        unique
            .entry((candidate.kind, candidate.content.clone()))
            .or_insert(candidate);
    }
    unique.into_values().collect()
}

fn explicit_memory_payload<'a>(value: &'a str, lower: &str) -> Option<&'a str> {
    for marker in [
        "我希望你记住",
        "请你记住",
        "请记住",
        "帮我记住",
        "你要记住",
        "please remember that",
        "please remember",
        "remember that",
        "keep in mind that",
    ] {
        if let Some(start) = lower.find(marker) {
            let payload = trim_payload(&value[start + marker.len()..]);
            let payload = trim_followup(payload);
            if !payload.is_empty() {
                return Some(payload);
            }
        }
    }
    None
}

fn trim_payload(value: &str) -> &str {
    value
        .trim()
        .trim_start_matches(['：', ':', '，', ',', '。', '.', '；', ';', ' '])
        .trim()
        .trim_end_matches(['。', '.', '；', ';', '，', ',', ' '])
        .trim()
}

fn trim_followup(value: &str) -> &str {
    let lower = value.to_ascii_lowercase();
    let mut end = value.len();
    for marker in [
        "请只回复",
        "只回复",
        "不用解释",
        "不要解释",
        "only reply",
        "reply only",
        "respond only",
        "do not explain",
        "don't explain",
    ] {
        if let Some(index) = lower.find(marker) {
            end = end.min(index);
        }
    }
    trim_payload(&value[..end])
}

fn classify_explicit_kind(value: &str) -> MemoryKind {
    let lower = value.to_ascii_lowercase();
    if contains_any(
        value,
        &[
            "偏好",
            "喜欢",
            "希望",
            "以后",
            "prefer",
            "preference",
            "from now on",
            "by default",
        ],
    ) {
        MemoryKind::Preference
    } else if value.contains("目标") || lower.contains("goal") {
        MemoryKind::Goal
    } else if contains_any(value, &["项目", "硬性要求", "硬约束", "project"]) {
        MemoryKind::ProjectContext
    } else if value.contains("关系") || lower.contains("relationship") {
        MemoryKind::RelationshipNote
    } else if contains_any(value, &["情绪", "感到", "emotion"]) {
        MemoryKind::EmotionalState
    } else {
        MemoryKind::PersonalFact
    }
}

fn normalize_explicit_content(kind: MemoryKind, payload: &str) -> String {
    let payload = compact(payload, 180);
    match kind {
        MemoryKind::Preference => format!("用户偏好：{payload}"),
        MemoryKind::Goal => format!("用户目标：{payload}"),
        MemoryKind::ProjectContext => format!("项目上下文：{payload}"),
        MemoryKind::RelationshipNote => format!("关系事件候选：{payload}"),
        MemoryKind::EmotionalState => format!("情绪状态候选：{payload}"),
        MemoryKind::PersonalFact => format!("用户自述事实候选：{payload}"),
        MemoryKind::Correction => format!("用户纠正/限制：{payload}"),
        MemoryKind::Event => format!("事件候选：{payload}"),
        MemoryKind::ToolTraceSummary => format!("工具轨迹摘要：{payload}"),
    }
}

fn scope_for_kind(kind: MemoryKind, fingerprint: &str) -> MemoryScope {
    match kind {
        MemoryKind::ProjectContext | MemoryKind::Correction | MemoryKind::ToolTraceSummary => {
            MemoryScope::Workspace {
                root_fingerprint: fingerprint.to_string(),
            }
        }
        MemoryKind::RelationshipNote | MemoryKind::EmotionalState => MemoryScope::Relationship,
        MemoryKind::Preference
        | MemoryKind::PersonalFact
        | MemoryKind::Goal
        | MemoryKind::Event => MemoryScope::GlobalUser,
    }
}

fn write_status(kind: MemoryKind, sensitivity: MemorySensitivity) -> MemoryStatus {
    if sensitivity != MemorySensitivity::Low {
        return MemoryStatus::Pending;
    }
    match kind {
        MemoryKind::Preference | MemoryKind::Correction | MemoryKind::ProjectContext => {
            MemoryStatus::Active
        }
        MemoryKind::PersonalFact
        | MemoryKind::RelationshipNote
        | MemoryKind::EmotionalState
        | MemoryKind::Goal
        | MemoryKind::Event
        | MemoryKind::ToolTraceSummary => MemoryStatus::Pending,
    }
}

fn classify_sensitivity(value: &str) -> MemorySensitivity {
    let lower = value.to_ascii_lowercase();
    if contains_secret(value) {
        MemorySensitivity::High
    } else if contains_any(
        value,
        &[
            "health",
            "medical",
            "finance",
            "bank",
            "emotion",
            "relationship",
            "情绪",
            "关系",
            "健康",
            "财务",
            "身份证",
            "银行卡",
        ],
    ) || lower.contains("private")
    {
        MemorySensitivity::Medium
    } else {
        MemorySensitivity::Low
    }
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "api key",
        "apikey",
        "authorization:",
        "bearer ",
        "password",
        "token",
        "secret",
        "sk-",
        "github_pat_",
        "ghp_",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn language_conflict<'a>(
    records: &'a [StoredMemoryRecord],
    incoming: &StoredMemoryRecord,
) -> Option<&'a StoredMemoryRecord> {
    if incoming.kind != MemoryKind::Preference {
        return None;
    }
    let key = incoming.dedup_key();
    if !key.ends_with("language:zh") && !key.ends_with("language:en") {
        return None;
    }
    records.iter().find(|record| {
        record.kind == MemoryKind::Preference
            && record.status == MemoryStatus::Active
            && record.dedup_key() != key
            && (record.dedup_key().ends_with("language:zh")
                || record.dedup_key().ends_with("language:en"))
    })
}

fn summary(record: &StoredMemoryRecord, reason: impl Into<String>) -> MemoryWriteSummary {
    MemoryWriteSummary::new(
        Some(record.id.clone()),
        record.protocol_kind(),
        record.content.clone(),
        record.status.protocol_status(),
        reason,
    )
}

fn generate_memory_id(now: u128) -> String {
    let counter = NEXT_MEMORY_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("yn-mem-{now}-{counter}")
}

fn compact(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
}

#[derive(Debug)]
pub enum MemoryWriteError {
    Store(MemoryStoreError),
    WriteNotGranted,
    InvalidMemoryId,
    PromptTooLong { maximum: usize },
    ResponseTooLong { maximum: usize },
}

impl fmt::Display for MemoryWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::WriteNotGranted => formatter.write_str("memory write access was not granted"),
            Self::InvalidMemoryId => formatter.write_str("memory id is invalid"),
            Self::PromptTooLong { maximum } => {
                write!(formatter, "memory prompt exceeds {maximum} characters")
            }
            Self::ResponseTooLong { maximum } => {
                write!(formatter, "assistant response exceeds {maximum} characters")
            }
        }
    }
}

impl Error for MemoryWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::WriteNotGranted
            | Self::InvalidMemoryId
            | Self::PromptTooLong { .. }
            | Self::ResponseTooLong { .. } => None,
        }
    }
}

impl From<MemoryStoreError> for MemoryWriteError {
    fn from(error: MemoryStoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    use yunxi_protocol::{MemoryReviewAction, WorkspaceGrant};

    use super::*;

    #[test]
    fn project_constraint_is_written_only_to_next_namespace() {
        let root = test_root("write");
        let request = MemoryWriteRequest::new(
            WorkspaceGrant::read_write(&root),
            "这是项目硬性要求：所有模块必须可测试",
        );
        let result = extract_and_store(&request).expect("extract memory");
        assert!(
            result
                .records()
                .iter()
                .any(|record| record.status() == MemoryWriteStatus::Active)
        );
        assert!(
            root.join(".yunxi-next/memory/workspace-memory.jsonl")
                .is_file()
        );
        assert!(!root.join(".yunxi/memory/workspace-memory.jsonl").exists());
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn personal_fact_waits_for_review_and_can_be_approved() {
        let root = test_root("review");
        let grant = WorkspaceGrant::read_write(&root);
        let result = extract_and_store(&MemoryWriteRequest::new(
            grant.clone(),
            "请记住我的生日是五月一日",
        ))
        .expect("extract memory");
        let pending = result
            .records()
            .iter()
            .find(|record| record.status() == MemoryWriteStatus::Pending)
            .expect("pending memory");
        let reviewed = review_memory(&MemoryReviewRequest::new(
            grant,
            pending.id().expect("memory id"),
            MemoryReviewAction::Approve,
        ))
        .expect("approve memory");
        assert_eq!(
            reviewed.record().expect("reviewed memory").status(),
            MemoryWriteStatus::Active
        );
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn secret_like_content_is_never_persisted() {
        let root = test_root("secret");
        let result = extract_and_store(&MemoryWriteRequest::new(
            WorkspaceGrant::read_write(&root),
            "请记住我的 api key 是 sk-example",
        ))
        .expect("apply privacy policy");
        assert_eq!(result.records()[0].status(), MemoryWriteStatus::Discarded);
        assert!(!root.join(".yunxi-next/memory").exists());
        fs::remove_dir_all(root).expect("remove test root");
    }

    fn test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("yunxi-memory-{label}-{}-{unique}", process::id()));
        fs::create_dir_all(&root).expect("create test root");
        root
    }
}
