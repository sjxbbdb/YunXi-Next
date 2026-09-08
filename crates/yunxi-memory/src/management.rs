//! Bounded, explicit management operations for the memory plugin.

use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::record::{MemoryScope, MemoryStatus, StoredMemoryRecord, now_millis};
use crate::store::{MemoryStore, MemoryStoreError};
use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    MemoryClearRequest, MemoryClearScope, MemoryManagementScope, MemoryMutationAction,
    MemoryMutationRequest, MemoryQueryRequest, WorkspaceGrant,
};

const MAX_MANAGEMENT_RECORDS: usize = 512;
const MAX_WARNINGS: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecordSummary {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub status: String,
    pub sensitivity: String,
    pub layer: String,
    pub content: String,
    pub confidence: f32,
    pub importance: f32,
    pub created_at_millis: u128,
    pub updated_at_millis: u128,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryStatusReport {
    pub enabled: bool,
    pub counts: MemoryCounts,
    pub warnings: Vec<String>,
    pub settings_path: String,
    pub workspace_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryCounts {
    pub active: usize,
    pub pending: usize,
    pub rejected: usize,
    pub archived: usize,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryListResult {
    pub records: Vec<MemoryRecordSummary>,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryMutationResult {
    pub id: Option<String>,
    pub changed: bool,
    pub affected: usize,
    pub record: Option<MemoryRecordSummary>,
    pub warnings: Vec<String>,
}

pub fn status(cwd: &Path) -> Result<MemoryStatusReport, MemoryManagementError> {
    let store = MemoryStore::for_workspace(cwd)?;
    status_from_store(&store)
}

pub fn status_with_grant(
    grant: &WorkspaceGrant,
) -> Result<MemoryStatusReport, MemoryManagementError> {
    let store = MemoryStore::from_grant(grant)?;
    status_from_store(&store)
}

fn status_from_store(store: &MemoryStore) -> Result<MemoryStatusReport, MemoryManagementError> {
    let load = store.load();
    let mut warnings = load.warnings;
    let enabled = store.enabled(&mut warnings);
    let counts = counts(&load.records);
    Ok(MemoryStatusReport {
        enabled,
        counts,
        warnings: bounded_warnings(warnings),
        settings_path: store.settings_path().to_string_lossy().into_owned(),
        workspace_fingerprint: store.workspace_fingerprint().to_string(),
    })
}

pub fn list(cwd: &Path, limit: usize) -> Result<MemoryListResult, MemoryManagementError> {
    let store = MemoryStore::for_workspace(cwd)?;
    list_from_store(&store, limit)
}

pub fn list_with_grant(
    grant: &WorkspaceGrant,
    limit: usize,
) -> Result<MemoryListResult, MemoryManagementError> {
    let store = MemoryStore::from_grant(grant)?;
    list_from_store(&store, limit)
}

fn list_from_store(
    store: &MemoryStore,
    limit: usize,
) -> Result<MemoryListResult, MemoryManagementError> {
    let load = store.load();
    let limit = limit.clamp(1, MAX_MANAGEMENT_RECORDS);
    let truncated = load.records.len() > limit;
    let records = load
        .records
        .iter()
        .take(limit)
        .map(MemoryRecordSummary::from_record)
        .collect();
    Ok(MemoryListResult {
        records,
        truncated,
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn pending(cwd: &Path, limit: usize) -> Result<MemoryListResult, MemoryManagementError> {
    let store = MemoryStore::for_workspace(cwd)?;
    let load = store.load();
    let limit = limit.clamp(1, MAX_MANAGEMENT_RECORDS);
    let pending = load
        .records
        .iter()
        .filter(|record| record.status.is_pending())
        .collect::<Vec<_>>();
    let truncated = pending.len() > limit;
    let records = pending
        .into_iter()
        .take(limit)
        .map(MemoryRecordSummary::from_record)
        .collect();
    Ok(MemoryListResult {
        records,
        truncated,
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn query_with_grant(
    request: &MemoryQueryRequest,
) -> Result<MemoryListResult, MemoryManagementError> {
    let store = MemoryStore::from_grant(request.grant())?;
    let load = store.load();
    let query = request.query().map(|query| query.trim().to_lowercase());
    let filtered = load
        .records
        .iter()
        .filter(|record| record_matches_scope(record, request.scope(), &store))
        .filter(|record| !request.pending_only() || record.status.is_pending())
        .filter(|record| {
            query
                .as_deref()
                .is_none_or(|query| record_matches_query(record, query))
        })
        .collect::<Vec<_>>();
    let truncated = filtered.len() > request.limit();
    let records = filtered
        .into_iter()
        .take(request.limit())
        .map(MemoryRecordSummary::from_record)
        .collect();
    Ok(MemoryListResult {
        records,
        truncated,
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn show(cwd: &Path, id: &str) -> Result<Option<MemoryRecordSummary>, MemoryManagementError> {
    validate_id(id)?;
    let store = MemoryStore::for_workspace(cwd)?;
    show_from_store(&store, id)
}

pub fn show_with_grant(
    grant: &WorkspaceGrant,
    id: &str,
) -> Result<Option<MemoryRecordSummary>, MemoryManagementError> {
    validate_id(id)?;
    let store = MemoryStore::from_grant(grant)?;
    show_from_store(&store, id)
}

fn show_from_store(
    store: &MemoryStore,
    id: &str,
) -> Result<Option<MemoryRecordSummary>, MemoryManagementError> {
    Ok(store
        .load()
        .records
        .iter()
        .find(|record| record.id == id)
        .map(MemoryRecordSummary::from_record))
}

pub fn delete(cwd: &Path, id: &str) -> Result<MemoryMutationResult, MemoryManagementError> {
    validate_id(id)?;
    let store = MemoryStore::for_workspace(cwd)?;
    let load = store.load();
    let Some(mut record) = load.records.iter().find(|record| record.id == id).cloned() else {
        return Ok(MemoryMutationResult {
            id: Some(id.to_string()),
            changed: false,
            affected: 0,
            record: None,
            warnings: bounded_warnings(load.warnings),
        });
    };
    if record.status.is_archived() {
        return Ok(MemoryMutationResult {
            id: Some(id.to_string()),
            changed: false,
            affected: 0,
            record: Some(MemoryRecordSummary::from_record(&record)),
            warnings: bounded_warnings(load.warnings),
        });
    }
    record.set_status(crate::record::MemoryStatus::Archived, now_millis());
    store.append(&record)?;
    Ok(MemoryMutationResult {
        id: Some(id.to_string()),
        changed: true,
        affected: 1,
        record: Some(MemoryRecordSummary::from_record(&record)),
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn mutate_with_grant(
    request: &MemoryMutationRequest,
) -> Result<MemoryMutationResult, MemoryManagementError> {
    require_write(request.grant())?;
    validate_id(request.id())?;
    let store = MemoryStore::from_grant(request.grant())?;
    let load = store.load();
    let Some(mut record) = load
        .records
        .iter()
        .find(|record| record.id == request.id())
        .cloned()
    else {
        return Ok(MemoryMutationResult {
            id: Some(request.id().to_string()),
            changed: false,
            affected: 0,
            record: None,
            warnings: bounded_warnings(load.warnings),
        });
    };
    let target = match request.action() {
        MemoryMutationAction::Approve => MemoryStatus::Active,
        MemoryMutationAction::Reject => MemoryStatus::Rejected,
        MemoryMutationAction::Archive => MemoryStatus::Archived,
    };
    let changed = record.status != target;
    if changed {
        record.set_status(target, now_millis());
        store.append(&record)?;
    }
    Ok(MemoryMutationResult {
        id: Some(request.id().to_string()),
        changed,
        affected: usize::from(changed),
        record: Some(MemoryRecordSummary::from_record(&record)),
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn clear(cwd: &Path) -> Result<MemoryMutationResult, MemoryManagementError> {
    let store = MemoryStore::for_workspace(cwd)?;
    let load = store.load();
    let mut affected = 0;
    for mut record in load
        .records
        .iter()
        .filter(|record| !record.status.is_archived())
        .cloned()
    {
        record.set_status(crate::record::MemoryStatus::Archived, now_millis());
        store.append(&record)?;
        affected += 1;
        if affected >= MAX_MANAGEMENT_RECORDS {
            break;
        }
    }
    Ok(MemoryMutationResult {
        id: None,
        changed: affected != 0,
        affected,
        record: None,
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn clear_with_grant(
    request: &MemoryClearRequest,
) -> Result<MemoryMutationResult, MemoryManagementError> {
    require_write(request.grant())?;
    let store = MemoryStore::from_grant(request.grant())?;
    let load = store.load();
    let mut affected = 0;
    let mut last_record = None;
    for mut record in load
        .records
        .iter()
        .filter(|record| clear_scope_matches(record, request.scope(), &store))
        .filter(|record| !record.status.is_archived())
        .cloned()
    {
        record.set_status(MemoryStatus::Archived, now_millis());
        store.append(&record)?;
        affected += 1;
        last_record = Some(MemoryRecordSummary::from_record(&record));
        if affected >= MAX_MANAGEMENT_RECORDS {
            break;
        }
    }
    Ok(MemoryMutationResult {
        id: None,
        changed: affected != 0,
        affected,
        record: last_record,
        warnings: bounded_warnings(load.warnings),
    })
}

pub fn set_enabled(cwd: &Path, enabled: bool) -> Result<MemoryStatusReport, MemoryManagementError> {
    let store = MemoryStore::for_workspace(cwd)?;
    let content = format!("{{\n  \"enabled\": {}\n}}\n", enabled);
    atomic_replace(&store.settings_path(), content.as_bytes())?;
    status(cwd)
}

pub fn set_enabled_with_grant(
    grant: &WorkspaceGrant,
    enabled: bool,
) -> Result<MemoryStatusReport, MemoryManagementError> {
    require_write(grant)?;
    let store = MemoryStore::from_grant(grant)?;
    let content = format!("{{\n  \"enabled\": {}\n}}\n", enabled);
    atomic_replace(&store.settings_path(), content.as_bytes())?;
    status_from_store(&store)
}

impl MemoryRecordSummary {
    fn from_record(record: &StoredMemoryRecord) -> Self {
        Self {
            id: record.id.clone(),
            scope: record.scope.label(),
            kind: record.kind.storage_key().to_string(),
            status: record.status.storage_group().to_string(),
            sensitivity: match record.sensitivity {
                crate::record::MemorySensitivity::Low => "low",
                crate::record::MemorySensitivity::Medium => "medium",
                crate::record::MemorySensitivity::High => "high",
            }
            .to_string(),
            layer: match record.layer {
                crate::record::MemoryLayer::Profile => "profile",
                crate::record::MemoryLayer::Preference => "preference",
                crate::record::MemoryLayer::Relationship => "relationship",
                crate::record::MemoryLayer::Workspace => "workspace",
                crate::record::MemoryLayer::Episode => "episode",
                crate::record::MemoryLayer::ToolTrace => "tool_trace",
                crate::record::MemoryLayer::Unknown => "unknown",
            }
            .to_string(),
            content: record.content.clone(),
            confidence: record.confidence,
            importance: record.importance,
            created_at_millis: record.created_at_millis,
            updated_at_millis: record.updated_at_millis,
        }
    }
}

fn counts(records: &[StoredMemoryRecord]) -> MemoryCounts {
    let mut result = MemoryCounts {
        active: 0,
        pending: 0,
        rejected: 0,
        archived: 0,
        total: records.len(),
    };
    for record in records {
        match record.status {
            crate::record::MemoryStatus::Active => result.active += 1,
            crate::record::MemoryStatus::Pending => result.pending += 1,
            crate::record::MemoryStatus::Rejected => result.rejected += 1,
            crate::record::MemoryStatus::Archived => result.archived += 1,
        }
    }
    result
}

fn record_matches_scope(
    record: &StoredMemoryRecord,
    scope: MemoryManagementScope,
    store: &MemoryStore,
) -> bool {
    match scope {
        MemoryManagementScope::All => record
            .scope
            .matches_workspace(store.workspace_fingerprint()),
        MemoryManagementScope::Global => !matches!(record.scope, MemoryScope::Workspace { .. }),
        MemoryManagementScope::Workspace => matches!(
            &record.scope,
            MemoryScope::Workspace { root_fingerprint }
                if root_fingerprint == store.workspace_fingerprint()
        ),
        MemoryManagementScope::Relationship => matches!(record.scope, MemoryScope::Relationship),
    }
}

fn clear_scope_matches(
    record: &StoredMemoryRecord,
    scope: MemoryClearScope,
    store: &MemoryStore,
) -> bool {
    match scope {
        MemoryClearScope::Workspace => matches!(
            &record.scope,
            MemoryScope::Workspace { root_fingerprint }
                if root_fingerprint == store.workspace_fingerprint()
        ),
        MemoryClearScope::Relationship => matches!(record.scope, MemoryScope::Relationship),
    }
}

fn record_matches_query(record: &StoredMemoryRecord, query: &str) -> bool {
    record.content.to_lowercase().contains(query)
        || record.id.to_lowercase().contains(query)
        || record.scope.label().to_lowercase().contains(query)
        || record.kind.storage_key().contains(query)
        || record.status.storage_group().contains(query)
}

fn require_write(grant: &WorkspaceGrant) -> Result<(), MemoryManagementError> {
    if grant.allows_next_write() {
        Ok(())
    } else {
        Err(MemoryManagementError::WriteNotGranted)
    }
}

fn validate_id(id: &str) -> Result<(), MemoryManagementError> {
    if id.is_empty() || id.chars().count() > 256 || id.chars().any(char::is_control) {
        return Err(MemoryManagementError::InvalidId);
    }
    Ok(())
}

fn bounded_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.truncate(MAX_WARNINGS);
    warnings
}

fn atomic_replace(path: &Path, content: &[u8]) -> Result<(), MemoryManagementError> {
    let parent = path.parent().ok_or(MemoryManagementError::InvalidPath)?;
    fs::create_dir_all(parent).map_err(|error| MemoryManagementError::Io {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".yunxi-memory-{}-{stamp}.tmp", process::id()));
    let backup = path.with_extension("json.bak");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| MemoryManagementError::Io {
            path: temporary.clone(),
            message: error.to_string(),
        })?;
    if let Err(error) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(MemoryManagementError::Io {
            path: temporary,
            message: error.to_string(),
        });
    }
    drop(file);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| MemoryManagementError::Io {
                path: backup.clone(),
                message: error.to_string(),
            })?;
        }
        fs::rename(path, &backup).map_err(|error| MemoryManagementError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(MemoryManagementError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        });
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

#[derive(Debug)]
pub enum MemoryManagementError {
    Store(MemoryStoreError),
    Io { path: PathBuf, message: String },
    InvalidId,
    InvalidPath,
    WriteNotGranted,
}

impl fmt::Display for MemoryManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Io { path, message } => write!(
                formatter,
                "memory I/O failed at {}: {message}",
                path.display()
            ),
            Self::InvalidId => formatter.write_str("memory id is invalid"),
            Self::InvalidPath => formatter.write_str("memory settings path has no parent"),
            Self::WriteNotGranted => {
                formatter.write_str("memory mutation requires a Next write grant")
            }
        }
    }
}

impl Error for MemoryManagementError {}

impl From<MemoryStoreError> for MemoryManagementError {
    fn from(error: MemoryStoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn explicit_grant_state_root_controls_management_reads() {
        let sequence = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "yunxi-memory-management-{}-{sequence}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let state_root = root.join("state");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(state_root.join("memory")).expect("create state memory root");
        fs::write(
            state_root.join("memory").join("global-memory.jsonl"),
            concat!(
                "{\"id\":\"granted-memory\",\"schema_version\":3,",
                "\"scope\":\"global_user\",\"kind\":\"preference\",",
                "\"content\":\"loaded from explicit state root\",",
                "\"confidence\":0.9,\"importance\":0.9,\"sensitivity\":\"low\",",
                "\"status\":\"active\",\"created_at_millis\":1,",
                "\"updated_at_millis\":1}\n"
            ),
        )
        .expect("write granted memory");

        let grant = WorkspaceGrant::read_only(&workspace).with_state_root(&state_root);
        let result = list_with_grant(&grant, 20).expect("list granted memories");
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].id, "granted-memory");
        assert_eq!(result.records[0].content, "loaded from explicit state root");

        fs::remove_dir_all(root).expect("remove fixture");
    }
}
