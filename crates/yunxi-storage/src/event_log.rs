//! Read-only legacy JSONL event replay and reversible import.
//!
//! Legacy session logs are treated as an append-only source. This module never
//! writes to the source and only emits a deliberately small, normalized event
//! envelope. Unknown fields and credential-bearing fields are not copied.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use yunxi_protocol::WorkspaceGrant;

use crate::store::{MAX_SESSION_FILE_BYTES, StorageError, read_bounded};

const EVENT_IMPORT_SCHEMA_VERSION: u32 = 1;
const MAX_EVENT_LINE_BYTES: usize = 1024 * 1024;
const MAX_EVENT_PAGE: usize = 128;
const MAX_EVENT_TEXT_CHARS: usize = 256 * 1024;
const MAX_EVENT_TYPE_CHARS: usize = 96;
const MAX_EVENT_FIELD_CHARS: usize = 256;
const MAX_EVENT_WARNINGS: usize = 128;
const MAX_IMPORT_MANIFEST_BYTES: u64 = 1024 * 1024;
static IMPORT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A physical source line is the replay cursor. It advances over malformed
/// lines too, so a client cannot get stuck retrying one corrupt record.
pub type EventCursor = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyEventKind {
    Session,
    Assistant,
    Tool,
    Lifecycle,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedLegacyEvent {
    source_line: EventCursor,
    event_type: String,
    kind: LegacyEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    fields: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dropped_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    redacted_fields: Vec<String>,
}

impl NormalizedLegacyEvent {
    pub fn source_line(&self) -> EventCursor {
        self.source_line
    }

    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    pub fn kind(&self) -> LegacyEventKind {
        self.kind
    }

    pub fn timestamp(&self) -> Option<&str> {
        self.timestamp.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }

    pub fn dropped_fields(&self) -> &[String] {
        &self.dropped_fields
    }

    pub fn redacted_fields(&self) -> &[String] {
        &self.redacted_fields
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventLogWarning {
    line: EventCursor,
    message: String,
}

impl EventLogWarning {
    pub fn line(&self) -> EventCursor {
        self.line
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventLogPage {
    events: Vec<NormalizedLegacyEvent>,
    next_cursor: EventCursor,
    has_more: bool,
    warnings: Vec<EventLogWarning>,
}

impl EventLogPage {
    pub fn events(&self) -> &[NormalizedLegacyEvent] {
        &self.events
    }

    pub fn next_cursor(&self) -> EventCursor {
        self.next_cursor
    }

    pub fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn warnings(&self) -> &[EventLogWarning] {
        &self.warnings
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventLogSummary {
    line_count: EventCursor,
    event_count: usize,
    malformed_line_count: usize,
    unknown_event_count: usize,
    warnings: Vec<EventLogWarning>,
}

impl EventLogSummary {
    pub fn line_count(&self) -> EventCursor {
        self.line_count
    }

    pub fn event_count(&self) -> usize {
        self.event_count
    }

    pub fn malformed_line_count(&self) -> usize {
        self.malformed_line_count
    }

    pub fn unknown_event_count(&self) -> usize {
        self.unknown_event_count
    }

    pub fn warnings(&self) -> &[EventLogWarning] {
        &self.warnings
    }
}

/// Bounded, read-only view of one legacy JSONL event log.
#[derive(Clone, Debug)]
pub struct LegacyEventLog {
    path: PathBuf,
    bytes: Vec<u8>,
    fingerprint: u64,
}

impl LegacyEventLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventLogError> {
        let path = path.as_ref().to_path_buf();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            return Err(EventLogError::UnsupportedFormat(path));
        }
        let bytes =
            read_bounded(&path)?.ok_or_else(|| EventLogError::SourceMissing(path.clone()))?;
        Ok(Self {
            path,
            fingerprint: stable_fingerprint(&bytes),
            bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stable content identity used to detect source changes before import.
    /// This is intentionally the same non-cryptographic FNV identity used by
    /// the existing migration manifest; it is not a security signature.
    pub fn source_fingerprint(&self) -> String {
        format!("fnv1a64:{:016x}", self.fingerprint)
    }

    /// Replay records after the physical-line cursor. `limit` is bounded and
    /// malformed lines are skipped with a warning rather than aborting replay.
    pub fn replay(
        &self,
        after_cursor: EventCursor,
        limit: usize,
    ) -> Result<EventLogPage, EventLogError> {
        let limit = limit.clamp(1, MAX_EVENT_PAGE);
        let mut events = Vec::with_capacity(limit);
        let mut warnings = Vec::new();
        let mut line_count = 0;
        let mut last_returned = after_cursor;
        let mut has_more = false;

        for (index, line) in self
            .bytes
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
        {
            let line_number = index as u64 + 1;
            line_count = line_number;
            if line_number <= after_cursor {
                continue;
            }
            match parse_line(line_number, line, &mut warnings) {
                Some(event) if events.len() < limit => {
                    last_returned = line_number;
                    events.push(event);
                }
                Some(_) => has_more = true,
                None => {}
            }
        }

        if events.is_empty() {
            last_returned = line_count.max(after_cursor);
        } else if !has_more {
            // A valid event after the page boundary sets `has_more`; invalid
            // trailing records do not create an empty extra page.
            has_more = false;
        }
        Ok(EventLogPage {
            events,
            next_cursor: last_returned,
            has_more,
            warnings,
        })
    }

    pub fn summary(&self) -> Result<EventLogSummary, EventLogError> {
        let mut warnings = Vec::new();
        let mut event_count = 0;
        let mut malformed_line_count = 0;
        let mut unknown_event_count = 0;
        let mut line_count = 0;
        for (index, line) in self
            .bytes
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
        {
            let line_number = index as u64 + 1;
            line_count = line_number;
            let before = warnings.len();
            if let Some(event) = parse_line(line_number, line, &mut warnings) {
                event_count += 1;
                if event.kind == LegacyEventKind::Unknown {
                    unknown_event_count += 1;
                }
            } else if warnings.len() > before {
                malformed_line_count += 1;
            }
        }
        Ok(EventLogSummary {
            line_count,
            event_count,
            malformed_line_count,
            unknown_event_count,
            warnings,
        })
    }

    pub fn normalized_jsonl(&self) -> Result<Vec<u8>, EventLogError> {
        let mut output = Vec::new();
        let mut cursor = 0;
        loop {
            let page = self.replay(cursor, MAX_EVENT_PAGE)?;
            for event in page.events() {
                serde_json::to_writer(&mut output, event).map_err(EventLogError::Serialize)?;
                output.push(b'\n');
            }
            if !page.has_more() || page.next_cursor() == cursor {
                break;
            }
            cursor = page.next_cursor();
        }
        Ok(output)
    }
}

/// A grant-bound, one-file event-log importer.
#[derive(Clone, Debug)]
pub struct LegacyEventLogMigration {
    workspace_root: PathBuf,
    event_target_root: PathBuf,
    manifest_root: PathBuf,
    source: PathBuf,
    legacy_read: bool,
    next_write: bool,
}

impl LegacyEventLogMigration {
    pub fn from_grant(
        grant: &WorkspaceGrant,
        source: impl AsRef<Path>,
    ) -> Result<Self, EventImportError> {
        let workspace_root =
            fs::canonicalize(grant.root()).map_err(|source_error| EventImportError::Io {
                path: grant.root().to_path_buf(),
                source: source_error,
            })?;
        if !workspace_root.is_dir() {
            return Err(EventImportError::InvalidPlan(
                "workspace is not a directory".to_string(),
            ));
        }
        let legacy_root = workspace_root.join(".yunxi").join("sessions");
        let source = source.as_ref().to_path_buf();
        ensure_under(&legacy_root, &source)?;
        if source.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            return Err(EventImportError::UnsupportedFormat(source));
        }
        let source = fs::canonicalize(&source).unwrap_or(source);
        let event_target_root = workspace_root
            .join(".yunxi-next")
            .join("migrations")
            .join("events");
        let manifest_root = event_target_root.join("manifests");
        Ok(Self {
            workspace_root,
            event_target_root,
            manifest_root,
            source,
            legacy_read: grant.allows_legacy_read(),
            next_write: grant.allows_next_write(),
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn plan(&self) -> Result<EventImportPlan, EventImportError> {
        if !self.legacy_read {
            return Err(EventImportError::LegacyReadNotGranted);
        }
        let log = LegacyEventLog::open(&self.source)?;
        let summary = log.summary()?;
        let migration_id = new_import_id();
        let source = relative_path(&self.workspace_root, &self.source)?;
        let target = relative_path(
            &self.workspace_root,
            &self.event_target_root.join(format!("{migration_id}.jsonl")),
        )?;
        let state = if summary.event_count == 0 {
            EventImportState::Invalid {
                reason: "legacy event log contains no valid JSON event records".to_string(),
            }
        } else if self.workspace_root.join(&target).exists() {
            EventImportState::TargetExists
        } else {
            EventImportState::Ready
        };
        Ok(EventImportPlan {
            schema_version: EVENT_IMPORT_SCHEMA_VERSION,
            migration_id,
            source,
            target,
            source_bytes: log.bytes.len() as u64,
            source_fingerprint: log.source_fingerprint(),
            event_count: summary.event_count,
            malformed_line_count: summary.malformed_line_count,
            unknown_event_count: summary.unknown_event_count,
            warnings: summary.warnings,
            state,
        })
    }

    pub fn apply(&self, plan: &EventImportPlan) -> Result<EventImportReport, EventImportError> {
        self.require_write()?;
        self.validate_plan(plan)?;
        if !matches!(plan.state, EventImportState::Ready) {
            return Err(EventImportError::InvalidPlan(
                "event import plan is not ready".to_string(),
            ));
        }
        let log = LegacyEventLog::open(&self.source)?;
        if log.source_fingerprint() != plan.source_fingerprint
            || log.bytes.len() as u64 != plan.source_bytes
        {
            return Err(EventImportError::SourceChanged(plan.source.clone()));
        }
        let normalized = log.normalized_jsonl()?;
        let target = self.workspace_root.join(&plan.target);
        write_new_file(&target, &normalized)?;
        let target_fingerprint = stable_fingerprint(&normalized);
        let manifest = EventImportManifest {
            schema_version: EVENT_IMPORT_SCHEMA_VERSION,
            plan: plan.clone(),
            target_bytes: normalized.len() as u64,
            target_fingerprint,
        };
        let manifest_path = self.manifest_path(plan.migration_id())?;
        if let Err(error) = write_new_manifest(&manifest_path, &manifest) {
            if target.exists()
                && fs::read(&target)
                    .ok()
                    .is_some_and(|bytes| stable_fingerprint(&bytes) == target_fingerprint)
            {
                let _ignored = fs::remove_file(&target);
            }
            return Err(error);
        }
        Ok(EventImportReport {
            migration_id: plan.migration_id.clone(),
            target,
            imported_events: plan.event_count,
            malformed_lines: plan.malformed_line_count,
            unknown_events: plan.unknown_event_count,
            warnings: plan.warnings.clone(),
        })
    }

    pub fn rollback(&self, migration_id: &str) -> Result<EventRollbackReport, EventImportError> {
        self.require_write()?;
        validate_import_id(migration_id)?;
        let manifest_path = self.manifest_path(migration_id)?;
        let manifest = read_manifest(&manifest_path)?;
        self.validate_plan(&manifest.plan)?;
        let target = self.workspace_root.join(&manifest.plan.target);
        let removed = if let Ok(bytes) = fs::read(&target) {
            if bytes.len() as u64 == manifest.target_bytes
                && stable_fingerprint(&bytes) == manifest.target_fingerprint
            {
                fs::remove_file(&target).map_err(|source| EventImportError::Io {
                    path: target.clone(),
                    source,
                })?;
                true
            } else {
                false
            }
        } else {
            false
        };
        Ok(EventRollbackReport {
            migration_id: migration_id.to_string(),
            target,
            removed,
            preserved: !removed,
        })
    }

    fn validate_plan(&self, plan: &EventImportPlan) -> Result<(), EventImportError> {
        if plan.schema_version != EVENT_IMPORT_SCHEMA_VERSION {
            return Err(EventImportError::InvalidPlan(
                "unsupported event import plan schema version".to_string(),
            ));
        }
        validate_import_id(&plan.migration_id)?;
        let expected_source = relative_path(&self.workspace_root, &self.source)?;
        if plan.source != expected_source {
            return Err(EventImportError::InvalidPlan(
                "event import plan belongs to a different source".to_string(),
            ));
        }
        let expected_target = relative_path(
            &self.workspace_root,
            &self
                .event_target_root
                .join(format!("{}.jsonl", plan.migration_id)),
        )?;
        if plan.target != expected_target {
            return Err(EventImportError::InvalidPlan(
                "event import plan target is outside the event migration namespace".to_string(),
            ));
        }
        Ok(())
    }

    fn manifest_path(&self, migration_id: &str) -> Result<PathBuf, EventImportError> {
        validate_import_id(migration_id)?;
        Ok(self.manifest_root.join(format!("{migration_id}.json")))
    }

    fn require_write(&self) -> Result<(), EventImportError> {
        if self.next_write {
            Ok(())
        } else {
            Err(EventImportError::NextWriteNotGranted)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventImportPlan {
    schema_version: u32,
    migration_id: String,
    source: String,
    target: String,
    source_bytes: u64,
    source_fingerprint: String,
    event_count: usize,
    malformed_line_count: usize,
    unknown_event_count: usize,
    warnings: Vec<EventLogWarning>,
    state: EventImportState,
}

impl EventImportPlan {
    pub fn migration_id(&self) -> &str {
        &self.migration_id
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn source_bytes(&self) -> u64 {
        self.source_bytes
    }

    pub fn source_fingerprint(&self) -> &str {
        &self.source_fingerprint
    }

    pub fn event_count(&self) -> usize {
        self.event_count
    }

    pub fn malformed_line_count(&self) -> usize {
        self.malformed_line_count
    }

    pub fn unknown_event_count(&self) -> usize {
        self.unknown_event_count
    }

    pub fn warnings(&self) -> &[EventLogWarning] {
        &self.warnings
    }

    pub fn state(&self) -> &EventImportState {
        &self.state
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventImportState {
    Ready,
    TargetExists,
    Invalid { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventImportReport {
    migration_id: String,
    target: PathBuf,
    imported_events: usize,
    malformed_lines: usize,
    unknown_events: usize,
    warnings: Vec<EventLogWarning>,
}

impl EventImportReport {
    pub fn migration_id(&self) -> &str {
        &self.migration_id
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn imported_events(&self) -> usize {
        self.imported_events
    }

    pub fn malformed_lines(&self) -> usize {
        self.malformed_lines
    }

    pub fn unknown_events(&self) -> usize {
        self.unknown_events
    }

    pub fn warnings(&self) -> &[EventLogWarning] {
        &self.warnings
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventRollbackReport {
    migration_id: String,
    target: PathBuf,
    removed: bool,
    preserved: bool,
}

impl EventRollbackReport {
    pub fn migration_id(&self) -> &str {
        &self.migration_id
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn removed(&self) -> bool {
        self.removed
    }

    pub fn preserved(&self) -> bool {
        self.preserved
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct EventImportManifest {
    schema_version: u32,
    plan: EventImportPlan,
    target_bytes: u64,
    target_fingerprint: u64,
}

#[derive(Debug)]
pub enum EventLogError {
    Storage(StorageError),
    SourceMissing(PathBuf),
    UnsupportedFormat(PathBuf),
    InvalidUtf8 { line: EventCursor },
    Serialize(serde_json::Error),
}

impl fmt::Display for EventLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::SourceMissing(path) => {
                write!(formatter, "legacy event log not found: {}", path.display())
            }
            Self::UnsupportedFormat(path) => write!(
                formatter,
                "unsupported legacy event log format: {}",
                path.display()
            ),
            Self::InvalidUtf8 { line } => {
                write!(formatter, "legacy event log line {line} is not UTF-8")
            }
            Self::Serialize(error) => write!(formatter, "event log serialization failed: {error}"),
        }
    }
}

impl Error for EventLogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Serialize(error) => Some(error),
            Self::SourceMissing(_) | Self::UnsupportedFormat(_) | Self::InvalidUtf8 { .. } => None,
        }
    }
}

impl From<StorageError> for EventLogError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub enum EventImportError {
    Log(EventLogError),
    LegacyReadNotGranted,
    NextWriteNotGranted,
    InvalidPlan(String),
    SourceChanged(String),
    ManifestMissing(PathBuf),
    InvalidManifest(String),
    UnsupportedFormat(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for EventImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Log(error) => error.fmt(formatter),
            Self::LegacyReadNotGranted => {
                formatter.write_str("legacy session read access was not granted")
            }
            Self::NextWriteNotGranted => {
                formatter.write_str("Next session write access was not granted")
            }
            Self::InvalidPlan(message) => write!(formatter, "invalid event import plan: {message}"),
            Self::SourceChanged(source) => {
                write!(formatter, "legacy event source changed: {source}")
            }
            Self::ManifestMissing(path) => write!(
                formatter,
                "event import manifest not found: {}",
                path.display()
            ),
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid event import manifest: {message}")
            }
            Self::UnsupportedFormat(path) => write!(
                formatter,
                "unsupported legacy event log format: {}",
                path.display()
            ),
            Self::Io { path, source } => write!(
                formatter,
                "event import I/O failed at {}: {source}",
                path.display()
            ),
            Self::Serialize(error) => {
                write!(formatter, "event import serialization failed: {error}")
            }
        }
    }
}

impl Error for EventImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Log(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EventLogError> for EventImportError {
    fn from(error: EventLogError) -> Self {
        Self::Log(error)
    }
}

fn parse_line(
    line_number: EventCursor,
    bytes: &[u8],
    warnings: &mut Vec<EventLogWarning>,
) -> Option<NormalizedLegacyEvent> {
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_EVENT_LINE_BYTES {
        push_warning(warnings, line_number, "line exceeds the event size limit");
        return None;
    }
    let Ok(text) = std::str::from_utf8(trimmed) else {
        push_warning(warnings, line_number, "line is not valid UTF-8");
        return None;
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        push_warning(warnings, line_number, "line is not valid JSON");
        return None;
    };
    let Some(object) = value.as_object() else {
        push_warning(warnings, line_number, "event record must be a JSON object");
        return None;
    };
    Some(normalize_event(line_number, object))
}

fn normalize_event(line: EventCursor, object: &Map<String, Value>) -> NormalizedLegacyEvent {
    let item = object.get("item").and_then(Value::as_object);
    let raw_type = object
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("event")
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    let nested_type = item
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let nested_role = item
        .and_then(|value| value.get("role"))
        .and_then(Value::as_str);
    let event_type = match (nested_type, nested_role) {
        (Some("message"), Some("assistant")) => "assistant_message",
        (Some("message"), Some("user")) => "user_message",
        (Some(value), _) => value,
        (None, _) => raw_type,
    };
    let event_type = normalize_type(event_type);
    let kind = classify(&event_type);
    let timestamp = first_string(object, &["timestamp", "created_at", "createdAt"])
        .map(|value| truncate(value, MAX_EVENT_FIELD_CHARS));
    let session_id = first_string(
        object,
        &["session_id", "sessionId", "thread_id", "threadId"],
    )
    .map(|value| truncate(value, MAX_EVENT_FIELD_CHARS));
    let mut fields = BTreeMap::new();
    let mut dropped_fields = Vec::new();
    let mut redacted_fields = Vec::new();
    for (key, value) in object {
        collect_field(
            key,
            value,
            &mut fields,
            &mut dropped_fields,
            &mut redacted_fields,
        );
    }
    if let Some(item) = item {
        for (key, value) in item {
            collect_field(
                key,
                value,
                &mut fields,
                &mut dropped_fields,
                &mut redacted_fields,
            );
        }
    }
    dropped_fields.sort();
    redacted_fields.sort();
    NormalizedLegacyEvent {
        source_line: line,
        event_type,
        kind,
        timestamp,
        session_id,
        fields,
        dropped_fields,
        redacted_fields,
    }
}

fn collect_field(
    key: &str,
    value: &Value,
    fields: &mut BTreeMap<String, Value>,
    dropped_fields: &mut Vec<String>,
    redacted_fields: &mut Vec<String>,
) {
    if is_structural(key) {
        return;
    }
    if is_sensitive(key) {
        redacted_fields.push(key.to_string());
    } else if let Some(normalized) = normalize_allowed_field(key, value) {
        fields.insert(key.to_string(), normalized);
    } else {
        dropped_fields.push(key.to_string());
    }
}

fn normalize_allowed_field(key: &str, value: &Value) -> Option<Value> {
    const ALLOWED: &[&str] = &[
        "id",
        "turn_id",
        "turnId",
        "call_id",
        "callId",
        "tool_name",
        "toolName",
        "name",
        "status",
        "reason",
        "phase",
        "message",
        "content",
        "text",
        "delta",
        "role",
    ];
    if !ALLOWED.contains(&key) {
        return None;
    }
    if matches!(value, Value::String(_)) || key == "message" || key == "content" {
        return extract_text(value).map(|text| Value::String(truncate(text, MAX_EVENT_TEXT_CHARS)));
    }
    match value {
        Value::Bool(_) | Value::Number(_) | Value::Null => Some(value.clone()),
        _ => None,
    }
}

fn extract_text(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.get("content").and_then(|content| {
            content
                .as_str()
                .or_else(|| content.get("text").and_then(Value::as_str))
        })
    })
}

fn classify(event_type: &str) -> LegacyEventKind {
    const SESSION: &[&str] = &[
        "session_meta",
        "session_start",
        "session_started",
        "session_end",
        "session_ended",
        "thread_started",
        "thread_ended",
        "user",
        "user_message",
    ];
    const ASSISTANT: &[&str] = &[
        "assistant",
        "assistant_message",
        "agent_message",
        "agent_message_content_delta",
        "reasoning",
        "reasoning_content_delta",
    ];
    const TOOL: &[&str] = &[
        "function_call",
        "function_call_output",
        "tool_call",
        "tool_result",
        "mcp_tool_call",
        "mcp_tool_result",
        "command_execution",
        "shell_command",
        "web_search",
        "file_change",
    ];
    const LIFECYCLE: &[&str] = &[
        "turn_started",
        "turn_start",
        "turn_completed",
        "turn_ended",
        "turn_failed",
        "context_compacted",
        "compaction",
        "error",
        "warning",
        "cancelled",
        "canceled",
    ];
    if SESSION.contains(&event_type) {
        LegacyEventKind::Session
    } else if ASSISTANT.contains(&event_type) {
        LegacyEventKind::Assistant
    } else if TOOL.contains(&event_type) {
        LegacyEventKind::Tool
    } else if LIFECYCLE.contains(&event_type) {
        LegacyEventKind::Lifecycle
    } else {
        LegacyEventKind::Unknown
    }
}

fn normalize_type(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars().take(MAX_EVENT_TYPE_CHARS) {
        if character.is_ascii_alphanumeric() || character == '_' {
            output.push(character.to_ascii_lowercase());
        } else if character == '-' || character == ' ' {
            output.push('_');
        }
    }
    if output.is_empty() {
        "unknown".to_string()
    } else {
        output
    }
}

fn first_string<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn is_structural(key: &str) -> bool {
    matches!(
        key,
        "type"
            | "timestamp"
            | "created_at"
            | "createdAt"
            | "session_id"
            | "sessionId"
            | "thread_id"
            | "threadId"
            | "event"
            | "item"
    )
}

fn is_sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "token",
        "secret",
        "password",
        "cookie",
        "api_key",
        "apikey",
        "credential",
        "headers",
        "env",
        "arguments",
        "args",
        "input",
        "output",
        "result",
    ]
    .iter()
    .any(|part| key == *part || key.contains(part))
}

fn truncate(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn push_warning(warnings: &mut Vec<EventLogWarning>, line: EventCursor, message: &str) {
    if warnings.len() < MAX_EVENT_WARNINGS {
        warnings.push(EventLogWarning {
            line,
            message: message.to_string(),
        });
    }
}

fn ensure_under(root: &Path, path: &Path) -> Result<(), EventImportError> {
    if let Ok(canonical_root) = fs::canonicalize(root) {
        let mut ancestor = path;
        while !ancestor.exists() {
            let Some(parent) = ancestor.parent() else {
                break;
            };
            ancestor = parent;
        }
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            if canonical.strip_prefix(&canonical_root).is_err() {
                return Err(EventImportError::InvalidPlan(format!(
                    "canonical source escapes legacy sessions root: {}",
                    path.display()
                )));
            }
        }
    } else if path.strip_prefix(root).is_err() {
        return Err(EventImportError::InvalidPlan(format!(
            "source escapes legacy sessions root: {}",
            path.display()
        )));
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<String, EventImportError> {
    path.strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .map_err(|_| EventImportError::InvalidPlan("path is outside workspace".to_string()))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), EventImportError> {
    if bytes.len() as u64 > MAX_SESSION_FILE_BYTES {
        return Err(EventImportError::InvalidPlan(
            "normalized event log exceeds the storage limit".to_string(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| EventImportError::InvalidPlan("event target has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|source| EventImportError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| EventImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if let Err(source) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(path);
        return Err(EventImportError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn write_new_manifest(path: &Path, manifest: &EventImportManifest) -> Result<(), EventImportError> {
    let parent = path
        .parent()
        .ok_or_else(|| EventImportError::InvalidPlan("manifest has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|source| EventImportError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(EventImportError::Serialize)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| EventImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if let Err(source) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(path);
        return Err(EventImportError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<EventImportManifest, EventImportError> {
    let metadata = fs::metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            EventImportError::ManifestMissing(path.to_path_buf())
        } else {
            EventImportError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES {
        return Err(EventImportError::InvalidManifest(
            "manifest exceeds size limit".to_string(),
        ));
    }
    let bytes = fs::read(path).map_err(|source| EventImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let manifest: EventImportManifest =
        serde_json::from_slice(&bytes).map_err(EventImportError::Serialize)?;
    if manifest.schema_version != EVENT_IMPORT_SCHEMA_VERSION {
        return Err(EventImportError::InvalidManifest(
            "unsupported manifest schema version".to_string(),
        ));
    }
    Ok(manifest)
}

fn validate_import_id(id: &str) -> Result<(), EventImportError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(EventImportError::InvalidPlan(
            "migration id contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

fn new_import_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!(
        "events-{now}-{}-{}",
        process::id(),
        IMPORT_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn stable_fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use yunxi_protocol::WorkspaceGrant;

    use super::*;

    #[test]
    fn replay_preserves_event_kinds_and_uses_physical_cursor() {
        let root = test_root("replay");
        let path = root.join(".yunxi/sessions/session.jsonl");
        fs::create_dir_all(path.parent().expect("parent")).expect("legacy root");
        let content = [
            json!({"type":"session_meta","session_id":"s-1","cwd":"ignored"}),
            json!({"type":"user","message":{"content":"hello"}}),
            json!({"type":"assistant","message":{"content":"hi"}}),
            json!({"type":"function_call","call_id":"c-1","name":"shell","arguments":{"token":"must drop"}}),
            json!({"type":"turn_completed","status":"completed"}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        fs::write(&path, content).expect("log");
        let log = LegacyEventLog::open(&path).expect("open");
        let first = log.replay(0, 2).expect("first page");
        assert_eq!(first.events().len(), 2);
        assert_eq!(first.events()[0].kind(), LegacyEventKind::Session);
        assert_eq!(first.events()[1].kind(), LegacyEventKind::Session);
        assert!(first.has_more());
        let second = log.replay(first.next_cursor(), 8).expect("second page");
        assert_eq!(second.events().len(), 3);
        assert_eq!(second.events()[0].kind(), LegacyEventKind::Assistant);
        assert_eq!(second.events()[1].kind(), LegacyEventKind::Tool);
        assert!(
            second.events()[1]
                .redacted_fields()
                .contains(&"arguments".to_string())
        );
        assert_eq!(second.events()[2].kind(), LegacyEventKind::Lifecycle);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn malformed_lines_are_isolated_and_do_not_repeat() {
        let root = test_root("bad-lines");
        let path = root.join("events.jsonl");
        fs::write(
            &path,
            b"not-json\n{\"type\":\"assistant\",\"text\":\"ok\"}\n",
        )
        .expect("log");
        let log = LegacyEventLog::open(&path).expect("open");
        let page = log.replay(0, 10).expect("replay");
        assert_eq!(page.events().len(), 1);
        assert_eq!(page.events()[0].source_line(), 2);
        assert_eq!(page.warnings().len(), 1);
        assert!(!page.has_more());
        let empty = log.replay(page.next_cursor(), 10).expect("replay end");
        assert!(empty.events().is_empty());
        assert_eq!(empty.next_cursor(), 2);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn import_is_read_only_non_overwriting_and_reversible() {
        let root = test_root("import");
        let source = root.join(".yunxi/sessions/session.jsonl");
        fs::create_dir_all(source.parent().expect("parent")).expect("legacy root");
        fs::write(&source, b"{\"type\":\"user\",\"message\":\"hello\"}\n").expect("source");
        let migration =
            LegacyEventLogMigration::from_grant(&WorkspaceGrant::read_write(&root), &source)
                .expect("migration");
        let plan = migration.plan().expect("plan");
        assert_eq!(plan.event_count(), 1);
        let report = migration.apply(&plan).expect("apply");
        assert!(report.target().is_file());
        assert_eq!(
            fs::read(&source).expect("source bytes"),
            b"{\"type\":\"user\",\"message\":\"hello\"}\n"
        );
        let original = fs::read(report.target()).expect("target");
        let second = migration.plan().expect("new plan");
        let second_report = migration.apply(&second).expect("second independent import");
        assert_ne!(report.target(), second_report.target());
        fs::write(report.target(), b"user changed imported event").expect("edit target");
        let rollback = migration.rollback(plan.migration_id()).expect("rollback");
        assert!(!rollback.removed());
        assert!(rollback.preserved());
        assert_eq!(
            fs::read(report.target()).expect("preserved target"),
            b"user changed imported event"
        );
        assert!(!original.is_empty());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unsupported_formats_and_read_only_grants_are_explicit() {
        let root = test_root("unsupported");
        let source = root.join(".yunxi/sessions/session.json");
        fs::create_dir_all(source.parent().expect("parent")).expect("legacy root");
        fs::write(&source, b"{}").expect("source");
        assert!(matches!(
            LegacyEventLog::open(&source),
            Err(EventLogError::UnsupportedFormat(_))
        ));
        let error = LegacyEventLogMigration::from_grant(
            &WorkspaceGrant::read_only(&root),
            source.with_extension("jsonl"),
        );
        assert!(error.is_ok());
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn test_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("yunxi-storage-event-{label}-{}", process::id()));
        let _ignored = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("root");
        root
    }
}
