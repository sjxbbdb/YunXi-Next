//! Bounded user-facing companion management owned by the isolated plugin.

use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use yunxi_protocol::{
    CompanionCheckRequest, CompanionClearRequest, CompanionDecisionResult, CompanionHistoryRequest,
    CompanionSetEnabledRequest, WorkspaceGrant,
};

use crate::decide;

const MAX_SETTINGS_BYTES: u64 = 16 * 1024;
const MAX_HISTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HISTORY_LINE_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_RECORDS: usize = yunxi_protocol::MAX_COMPANION_HISTORY_RECORDS;
const MAX_WARNINGS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionStatus {
    pub enabled: bool,
    pub settings_path: String,
    pub history_path: String,
    pub history_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionHistoryRecord {
    pub timestamp_millis: u128,
    pub action: String,
    pub trigger: String,
    pub enabled: bool,
    pub outcome: String,
    pub detail: String,
    pub requires_user_confirmation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionCheckResult {
    pub enabled: bool,
    pub settings_path: String,
    pub history_path: String,
    pub history_count: usize,
    pub warnings: Vec<String>,
    pub decision: Option<CompanionDecisionResult>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionHistoryResult {
    pub history_path: String,
    pub records: Vec<CompanionHistoryRecord>,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionClearResult {
    pub cleared: usize,
    pub history_path: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompanionSettings {
    #[serde(default = "default_enabled")]
    enabled: bool,
}

pub fn enabled() -> bool {
    load_settings(&settings_path(&next_state_root()))
        .map(|settings| settings.enabled)
        .unwrap_or(true)
}

pub fn status() -> CompanionStatus {
    status_at(&next_state_root())
}

pub fn status_with_grant(grant: &WorkspaceGrant) -> CompanionStatus {
    status_at(&state_root_from_grant(grant))
}

pub fn set_enabled(value: bool) -> Result<CompanionStatus, CompanionManagementError> {
    let state_root = next_state_root();
    write_enabled(&state_root, value)?;
    Ok(status_at(&state_root))
}

pub fn set_enabled_with_grant(
    request: &CompanionSetEnabledRequest,
) -> Result<CompanionStatus, CompanionManagementError> {
    require_write(request.grant())?;
    let state_root = state_root_from_grant(request.grant());
    write_enabled(&state_root, request.enabled())?;
    let mut status = status_at(&state_root);
    let record = CompanionHistoryRecord {
        timestamp_millis: now_millis(),
        action: if request.enabled() { "on" } else { "off" }.to_string(),
        trigger: "toggle".to_string(),
        enabled: request.enabled(),
        outcome: "completed".to_string(),
        detail: format!("companion enabled={}", request.enabled()),
        requires_user_confirmation: false,
    };
    append_history(&history_path(&state_root), &record)?;
    status.history_count = status.history_count.saturating_add(1);
    Ok(status)
}

pub fn check_with_grant(
    request: &CompanionCheckRequest,
) -> Result<CompanionCheckResult, CompanionManagementError> {
    require_write(request.grant())?;
    let state_root = state_root_from_grant(request.grant());
    let status = status_at(&state_root);
    let (decision, note) = if status.enabled {
        (
            Some(decide(&yunxi_protocol::CompanionDecisionRequest::new(
                request.prompt().to_string(),
            ))),
            None,
        )
    } else {
        (None, Some("companion is disabled by the user".to_string()))
    };
    let record = CompanionHistoryRecord {
        timestamp_millis: now_millis(),
        action: "check".to_string(),
        trigger: request.prompt().to_string(),
        enabled: status.enabled,
        outcome: if status.enabled {
            "completed".to_string()
        } else {
            "skipped".to_string()
        },
        detail: note.as_deref().unwrap_or("decision recorded").to_string(),
        requires_user_confirmation: false,
    };
    append_history(&history_path(&state_root), &record)?;
    Ok(CompanionCheckResult {
        enabled: status.enabled,
        settings_path: status.settings_path,
        history_path: status.history_path,
        history_count: status.history_count.saturating_add(1),
        warnings: status.warnings,
        decision,
        note,
    })
}

pub fn history_with_grant(request: &CompanionHistoryRequest) -> CompanionHistoryResult {
    let path = history_path(&state_root_from_grant(request.grant()));
    let loaded = load_history(&path, request.limit());
    CompanionHistoryResult {
        history_path: path.to_string_lossy().into_owned(),
        records: loaded.records,
        truncated: loaded.truncated,
        warnings: loaded.warnings,
    }
}

pub fn clear_with_grant(
    request: &CompanionClearRequest,
) -> Result<CompanionClearResult, CompanionManagementError> {
    require_write(request.grant())?;
    let path = history_path(&state_root_from_grant(request.grant()));
    let loaded = load_history(&path, MAX_HISTORY_RECORDS);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&path, error)),
    }
    Ok(CompanionClearResult {
        cleared: loaded.records.len(),
        history_path: path.to_string_lossy().into_owned(),
        warnings: loaded.warnings,
    })
}

fn status_at(state_root: &Path) -> CompanionStatus {
    let settings_path = settings_path(state_root);
    let history_path = history_path(state_root);
    let mut warnings = Vec::new();
    let enabled = match read_settings(&settings_path) {
        Ok(Some(settings)) => settings.enabled,
        Ok(None) => true,
        Err(error) => {
            warnings.push(error);
            true
        }
    };
    let history = load_history(&history_path, MAX_HISTORY_RECORDS);
    warnings.extend(history.warnings);
    warnings.truncate(MAX_WARNINGS);
    CompanionStatus {
        enabled,
        settings_path: settings_path.to_string_lossy().into_owned(),
        history_path: history_path.to_string_lossy().into_owned(),
        history_count: history.records.len(),
        warnings,
    }
}

fn write_enabled(state_root: &Path, enabled: bool) -> Result<(), CompanionManagementError> {
    let content = format!("{{\n  \"enabled\": {enabled}\n}}\n");
    atomic_replace(&settings_path(state_root), content.as_bytes())
}

fn load_settings(path: &Path) -> Result<CompanionSettings, String> {
    match read_settings(path)? {
        Some(settings) => Ok(settings),
        None => Ok(CompanionSettings { enabled: true }),
    }
}

fn read_settings(path: &Path) -> Result<Option<CompanionSettings>, String> {
    let content = match fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to read companion settings {}: {error}",
                path.display()
            ));
        }
    };
    if content.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(format!(
            "companion settings {} exceed {MAX_SETTINGS_BYTES} bytes",
            path.display()
        ));
    }
    serde_json::from_slice(&content)
        .map(Some)
        .map_err(|error| format!("invalid companion settings {}: {error}", path.display()))
}

struct LoadedHistory {
    records: Vec<CompanionHistoryRecord>,
    warnings: Vec<String>,
    truncated: bool,
}

fn load_history(path: &Path, limit: usize) -> LoadedHistory {
    let mut warnings = Vec::new();
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedHistory {
                records: Vec::new(),
                warnings,
                truncated: false,
            };
        }
        Err(error) => {
            warnings.push(format!("failed to inspect {}: {error}", path.display()));
            return LoadedHistory {
                records: Vec::new(),
                warnings,
                truncated: false,
            };
        }
    };
    if !metadata.is_file() {
        warnings.push(format!("path is not a file: {}", path.display()));
        return LoadedHistory {
            records: Vec::new(),
            warnings,
            truncated: false,
        };
    }
    if metadata.len() > MAX_HISTORY_BYTES {
        warnings.push(format!(
            "file {} is {} bytes; maximum is {MAX_HISTORY_BYTES}",
            path.display(),
            metadata.len()
        ));
        return LoadedHistory {
            records: Vec::new(),
            warnings,
            truncated: false,
        };
    }
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            warnings.push(format!("failed to read {}: {error}", path.display()));
            return LoadedHistory {
                records: Vec::new(),
                warnings,
                truncated: false,
            };
        }
    };
    let mut records = Vec::new();
    let mut truncated = false;
    for (index, line) in content.lines().enumerate() {
        if records.len() >= limit {
            truncated = true;
            warnings.push(format!(
                "record limit reached at {limit}; remaining entries in {} were skipped",
                path.display()
            ));
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_HISTORY_LINE_BYTES {
            warnings.push(format!(
                "line {} in {} exceeds {MAX_HISTORY_LINE_BYTES} bytes",
                index + 1,
                path.display()
            ));
            continue;
        }
        match serde_json::from_str::<CompanionHistoryRecord>(line) {
            Ok(record) => records.push(record),
            Err(error) => warnings.push(format!(
                "failed to parse line {} in {}: {error}",
                index + 1,
                path.display()
            )),
        }
        if warnings.len() >= MAX_WARNINGS {
            break;
        }
    }
    LoadedHistory {
        truncated: truncated || records.len() == limit,
        records,
        warnings,
    }
}

fn append_history(
    path: &Path,
    record: &CompanionHistoryRecord,
) -> Result<(), CompanionManagementError> {
    let parent = path.parent().ok_or(CompanionManagementError::InvalidPath)?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let mut line = serde_json::to_vec(record)
        .map_err(|error| CompanionManagementError::Encoding(error.to_string()))?;
    if line.len() > MAX_HISTORY_LINE_BYTES {
        return Err(CompanionManagementError::HistoryRecordTooLarge);
    }
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    file.write_all(&line)
        .and_then(|_| file.sync_data())
        .map_err(|error| io_error(path, error))
}

fn settings_path(state_root: &Path) -> PathBuf {
    state_root.join("companion").join("config.json")
}

fn history_path(state_root: &Path) -> PathBuf {
    state_root.join("companion").join("history.jsonl")
}

fn state_root_from_grant(grant: &WorkspaceGrant) -> PathBuf {
    grant
        .state_root()
        .map(Path::to_path_buf)
        .unwrap_or_else(next_state_root)
}

fn next_state_root() -> PathBuf {
    if let Some(path) = std::env::var_os("YUNXI_NEXT_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    PathBuf::from(".yunxi-next")
}

fn require_write(grant: &WorkspaceGrant) -> Result<(), CompanionManagementError> {
    if grant.allows_next_write() {
        Ok(())
    } else {
        Err(CompanionManagementError::WriteNotGranted)
    }
}

fn default_enabled() -> bool {
    true
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn atomic_replace(path: &Path, content: &[u8]) -> Result<(), CompanionManagementError> {
    let parent = path.parent().ok_or(CompanionManagementError::InvalidPath)?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".yunxi-companion-{}-{stamp}.tmp", process::id()));
    let backup = path.with_extension("json.bak");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| io_error(&temporary, error))?;
    if let Err(error) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, error));
    }
    drop(file);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| io_error(&backup, error))?;
        }
        fs::rename(path, &backup).map_err(|error| io_error(path, error))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(io_error(path, error));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn io_error(path: &Path, error: std::io::Error) -> CompanionManagementError {
    CompanionManagementError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

#[derive(Debug)]
pub enum CompanionManagementError {
    Io { path: PathBuf, message: String },
    Encoding(String),
    InvalidPath,
    HistoryRecordTooLarge,
    WriteNotGranted,
}

impl fmt::Display for CompanionManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(
                formatter,
                "companion I/O failed at {}: {message}",
                path.display()
            ),
            Self::Encoding(message) => {
                write!(formatter, "failed to encode companion state: {message}")
            }
            Self::InvalidPath => formatter.write_str("companion state path has no parent"),
            Self::HistoryRecordTooLarge => {
                formatter.write_str("companion history record is too large")
            }
            Self::WriteNotGranted => {
                formatter.write_str("companion mutation requires a Next write grant")
            }
        }
    }
}

impl Error for CompanionManagementError {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn fixture() -> (PathBuf, WorkspaceGrant) {
        let sequence = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "yunxi-companion-management-{}-{sequence}",
            process::id()
        ));
        let workspace = root.join("workspace");
        let state_root = root.join("state");
        fs::create_dir_all(&workspace).expect("create workspace");
        (
            root,
            WorkspaceGrant::read_write(workspace).with_state_root(state_root),
        )
    }

    #[test]
    fn grant_root_contains_settings_and_history() {
        let (root, grant) = fixture();
        let toggled =
            set_enabled_with_grant(&CompanionSetEnabledRequest::new(grant.clone(), false))
                .expect("disable companion");
        assert!(!toggled.enabled);
        assert_eq!(toggled.history_count, 1);

        let checked = check_with_grant(&CompanionCheckRequest::new(grant.clone(), "需要一点帮助"))
            .expect("record disabled check");
        assert!(checked.decision.is_none());
        assert_eq!(checked.history_count, 2);

        let history = history_with_grant(&CompanionHistoryRequest::new(grant, 10));
        assert_eq!(history.records.len(), 2);
        assert!(
            history
                .history_path
                .starts_with(root.join("state").to_string_lossy().as_ref())
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn read_only_grant_cannot_mutate_companion_state() {
        let (root, writable) = fixture();
        let read_only = WorkspaceGrant::read_only(writable.root())
            .with_state_root(writable.state_root().expect("state root"));
        let error = set_enabled_with_grant(&CompanionSetEnabledRequest::new(read_only, false))
            .expect_err("write must be denied");
        assert!(matches!(error, CompanionManagementError::WriteNotGranted));
        assert!(!root.join("state").join("companion").exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
