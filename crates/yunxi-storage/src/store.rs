//! Bounded session persistence under a granted workspace root.

use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use yunxi_protocol::{
    SessionAppendRequest, SessionCreateRequest, SessionCreateResult, SessionListRequest,
    SessionListResult, SessionLoadRequest, SessionLoadResult, SessionMutation,
    SessionMutationRequest, SessionMutationResult, SessionSnapshot, SessionSummary, WorkspaceGrant,
};

use crate::record::{StoredSession, validate_session_id};

const MAX_SESSION_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCANNED_FILES: usize = 2_000;
const MAX_LIST_LIMIT: usize = 200;

#[derive(Clone, Debug)]
pub struct SessionStore {
    workspace_root: PathBuf,
    next_root: PathBuf,
    legacy_root: Option<PathBuf>,
    writable: bool,
}

impl SessionStore {
    pub fn from_grant(grant: &WorkspaceGrant) -> Result<Self, StorageError> {
        let workspace_root =
            fs::canonicalize(grant.root()).map_err(|source| StorageError::Workspace {
                path: grant.root().to_path_buf(),
                source,
            })?;
        if !workspace_root.is_dir() {
            return Err(StorageError::NotDirectory(workspace_root));
        }
        Ok(Self {
            next_root: workspace_root.join(".yunxi-next").join("sessions"),
            legacy_root: grant
                .allows_legacy_read()
                .then(|| workspace_root.join(".yunxi").join("sessions")),
            workspace_root,
            writable: grant.allows_next_write(),
        })
    }

    pub fn append(&self, request: &SessionAppendRequest) -> Result<SessionSnapshot, StorageError> {
        self.require_write()?;
        let mut session = match request.session_id() {
            Some(id) => match self.load_record(id)? {
                Some((record, false)) => record,
                Some((record, true)) => record.import_from_legacy(),
                None => return Err(StorageError::NotFound(id.to_string())),
            },
            None => StoredSession::new(
                self.workspace_root.clone(),
                request.user_message(),
                request.assistant_message(),
            )
            .map_err(StorageError::InvalidRecord)?,
        };

        if request.session_id().is_some() {
            session
                .append(
                    request.user_message(),
                    request.assistant_message(),
                    request.provider(),
                    request.model(),
                )
                .map_err(StorageError::InvalidRecord)?;
        } else {
            session.set_provider_model(request.provider(), request.model());
        }
        self.save(&session)?;
        Ok(session.snapshot(false))
    }

    pub fn create(
        &self,
        _request: &SessionCreateRequest,
    ) -> Result<SessionCreateResult, StorageError> {
        self.require_write()?;
        let session = StoredSession::empty(self.workspace_root.clone())
            .map_err(StorageError::InvalidRecord)?;
        self.save(&session)?;
        Ok(SessionCreateResult::new(session.snapshot(false)))
    }

    pub fn load(&self, request: &SessionLoadRequest) -> Result<SessionLoadResult, StorageError> {
        let (session, warnings) = match self.load_record(request.session_id()) {
            Ok(Some((session, legacy))) => (Some(session.snapshot(legacy)), Vec::new()),
            Ok(None) => (None, Vec::new()),
            Err(StorageError::InvalidRecord(message)) => (None, vec![message]),
            Err(error) => return Err(error),
        };
        Ok(SessionLoadResult::new(session, warnings))
    }

    pub fn list(&self, request: &SessionListRequest) -> Result<SessionListResult, StorageError> {
        let mut warnings = Vec::new();
        let mut sessions = Vec::<SessionSnapshot>::new();
        self.scan_root(&self.next_root, false, &mut sessions, &mut warnings)?;
        if let Some(root) = &self.legacy_root {
            self.scan_root(root, true, &mut sessions, &mut warnings)?;
        }
        sessions.retain(|session| request.include_archived() || !session.archived());
        sessions.sort_by(|left, right| {
            right
                .pinned()
                .cmp(&left.pinned())
                .then_with(|| right.updated_at_millis().cmp(&left.updated_at_millis()))
                .then_with(|| left.id().cmp(right.id()))
        });
        let limit = request.limit().clamp(1, MAX_LIST_LIMIT);
        let truncated = sessions.len() > limit;
        sessions.truncate(limit);
        Ok(SessionListResult::new(
            sessions.iter().map(SessionSummary::from).collect(),
            warnings,
            truncated,
        ))
    }

    pub fn mutate(
        &self,
        request: &SessionMutationRequest,
    ) -> Result<SessionMutationResult, StorageError> {
        self.require_write()?;
        let Some((mut session, legacy)) = self.load_record(request.session_id())? else {
            return Ok(SessionMutationResult::new(None, Vec::new()));
        };
        if legacy && request.mutation() != SessionMutation::Fork {
            return Err(StorageError::LegacyReadOnly(
                request.session_id().to_string(),
            ));
        }
        match request.mutation() {
            SessionMutation::Archive => session.set_archived(true),
            SessionMutation::Unarchive => session.set_archived(false),
            SessionMutation::Pin => session.set_pinned(true),
            SessionMutation::Unpin => session.set_pinned(false),
            SessionMutation::Fork => {
                session = if legacy {
                    session.import_from_legacy()
                } else {
                    session.fork()
                };
            }
        }
        self.save(&session)?;
        Ok(SessionMutationResult::new(
            Some(session.snapshot(false)),
            Vec::new(),
        ))
    }

    fn require_write(&self) -> Result<(), StorageError> {
        if self.writable {
            Ok(())
        } else {
            Err(StorageError::WriteNotGranted)
        }
    }

    fn load_record(&self, id: &str) -> Result<Option<(StoredSession, bool)>, StorageError> {
        validate_session_id(id).map_err(StorageError::InvalidRecord)?;
        let next_path = self.next_root.join(format!("{id}.json"));
        if let Some(record) = read_next_record(&next_path)? {
            return Ok(Some((record, false)));
        }
        if let Some(root) = &self.legacy_root {
            let legacy_path = root.join(format!("{id}.json"));
            if let Some(record) = read_legacy_record(&legacy_path)? {
                return Ok(Some((record, true)));
            }
        }
        Ok(None)
    }

    fn scan_root(
        &self,
        root: &Path,
        legacy: bool,
        sessions: &mut Vec<SessionSnapshot>,
        warnings: &mut Vec<String>,
    ) -> Result<(), StorageError> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(StorageError::Io {
                    path: root.to_path_buf(),
                    source,
                });
            }
        };
        for (index, entry) in entries.enumerate() {
            if index >= MAX_SCANNED_FILES {
                warnings.push(format!(
                    "session scan stopped after {MAX_SCANNED_FILES} files in {}",
                    root.display()
                ));
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("failed to read session entry: {error}"));
                    continue;
                }
            };
            let path = entry.path();
            let result = if legacy {
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                read_legacy_record(&path)
            } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
                read_next_record(&path)
            } else if path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(".json.bak"))
            {
                let target = path.with_extension("");
                if target.exists() {
                    continue;
                }
                read_next_record(&target)
            } else {
                continue;
            };
            match result {
                Ok(Some(record)) => sessions.push(record.snapshot(legacy)),
                Ok(None) => {}
                Err(error) => warnings.push(error.to_string()),
            }
        }
        Ok(())
    }

    fn save(&self, session: &StoredSession) -> Result<(), StorageError> {
        session.validate().map_err(StorageError::InvalidRecord)?;
        fs::create_dir_all(&self.next_root).map_err(|source| StorageError::Io {
            path: self.next_root.clone(),
            source,
        })?;
        let content = serde_json::to_vec_pretty(session).map_err(StorageError::Serialize)?;
        if content.len() as u64 > MAX_SESSION_FILE_BYTES {
            return Err(StorageError::FileTooLarge {
                path: self.next_root.join(format!("{}.json", session.id())),
                maximum: MAX_SESSION_FILE_BYTES,
            });
        }
        let target = self.next_root.join(format!("{}.json", session.id()));
        replace_file(&target, &content)
    }
}

fn read_next_record(path: &Path) -> Result<Option<StoredSession>, StorageError> {
    let (content, source_path) = match read_bounded(path)? {
        Some(content) => (content, path.to_path_buf()),
        None => {
            let backup = session_backup_path(path);
            let Some(content) = read_bounded(&backup)? else {
                return Ok(None);
            };
            (content, backup)
        }
    };
    let record = serde_json::from_slice::<StoredSession>(&content).map_err(|error| {
        StorageError::InvalidRecord(format!(
            "failed to parse session {}: {error}",
            source_path.display()
        ))
    })?;
    record.validate().map_err(|error| {
        StorageError::InvalidRecord(format!(
            "invalid session {}: {error}",
            source_path.display()
        ))
    })?;
    Ok(Some(record))
}

fn read_legacy_record(path: &Path) -> Result<Option<StoredSession>, StorageError> {
    let Some(content) = read_bounded(path)? else {
        return Ok(None);
    };
    let value = serde_json::from_slice(&content).map_err(|error| {
        StorageError::InvalidRecord(format!(
            "failed to parse legacy session {}: {error}",
            path.display()
        ))
    })?;
    StoredSession::from_legacy(value)
        .map(Some)
        .map_err(|error| {
            StorageError::InvalidRecord(format!(
                "invalid legacy session {}: {error}",
                path.display()
            ))
        })
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, StorageError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StorageError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_SESSION_FILE_BYTES {
        return Err(StorageError::FileTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_SESSION_FILE_BYTES,
        });
    }
    fs::read(path).map(Some).map_err(|source| StorageError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn replace_file(target: &Path, content: &[u8]) -> Result<(), StorageError> {
    let parent = target.parent().ok_or_else(|| {
        StorageError::InvalidRecord("session path has no parent directory".to_string())
    })?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".session-{}-{unique}.tmp", process::id()));
    let backup = session_backup_path(target);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| StorageError::Io {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(&temporary);
        return Err(StorageError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);

    if target.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|source| StorageError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        fs::rename(target, &backup).map_err(|source| StorageError::Io {
            path: target.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temporary, target) {
        if backup.exists() {
            let _ignored = fs::rename(&backup, target);
        }
        let _ignored = fs::remove_file(&temporary);
        return Err(StorageError::Io {
            path: target.to_path_buf(),
            source,
        });
    }
    if backup.exists() {
        let _ignored = fs::remove_file(backup);
    }
    Ok(())
}

fn session_backup_path(target: &Path) -> PathBuf {
    target.with_extension("json.bak")
}

#[derive(Debug)]
pub enum StorageError {
    Workspace {
        path: PathBuf,
        source: std::io::Error,
    },
    NotDirectory(PathBuf),
    WriteNotGranted,
    NotFound(String),
    LegacyReadOnly(String),
    InvalidRecord(String),
    FileTooLarge {
        path: PathBuf,
        maximum: u64,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace { path, source } => {
                write!(
                    formatter,
                    "failed to resolve workspace {}: {source}",
                    path.display()
                )
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "workspace is not a directory: {}",
                    path.display()
                )
            }
            Self::WriteNotGranted => formatter.write_str("session write access was not granted"),
            Self::NotFound(id) => write!(formatter, "session `{id}` was not found"),
            Self::LegacyReadOnly(id) => {
                write!(
                    formatter,
                    "legacy session `{id}` is read-only; fork it first"
                )
            }
            Self::InvalidRecord(message) => formatter.write_str(message),
            Self::FileTooLarge { path, maximum } => write!(
                formatter,
                "session file {} exceeds {maximum} bytes",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "session I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Serialize(error) => write!(formatter, "session serialization failed: {error}"),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Workspace { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            Self::NotDirectory(_)
            | Self::WriteNotGranted
            | Self::NotFound(_)
            | Self::LegacyReadOnly(_)
            | Self::InvalidRecord(_)
            | Self::FileTooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn writes_next_namespace_and_only_reads_legacy_namespace() {
        let root = test_root("namespace");
        fs::create_dir_all(root.join(".yunxi/sessions")).expect("create legacy root");
        fs::write(
            root.join(".yunxi/sessions/yunxi-old.json"),
            r#"{"id":"yunxi-old","cwd":".","prompt":"old","final_response":"reply","created_at_millis":1}"#,
        )
        .expect("write legacy record");
        let grant = WorkspaceGrant::read_write(&root);
        let store = SessionStore::from_grant(&grant).expect("create store");

        let legacy = store
            .load(&SessionLoadRequest::new(grant.clone(), "yunxi-old"))
            .expect("load legacy")
            .into_session()
            .expect("legacy session");
        assert!(legacy.legacy());

        let saved = store
            .append(&SessionAppendRequest::new(grant, "new", "reply"))
            .expect("append session");
        assert!(
            root.join(format!(".yunxi-next/sessions/{}.json", saved.id()))
                .is_file()
        );
        assert!(root.join(".yunxi/sessions/yunxi-old.json").is_file());
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn missing_target_is_read_from_interrupted_replacement_backup() {
        let root = test_root("backup-recovery");
        let grant = WorkspaceGrant::read_write(&root);
        let store = SessionStore::from_grant(&grant).expect("create store");
        let saved = store
            .append(&SessionAppendRequest::new(grant.clone(), "new", "reply"))
            .expect("append session");
        let target = root.join(format!(".yunxi-next/sessions/{}.json", saved.id()));
        let backup = session_backup_path(&target);
        fs::rename(&target, &backup).expect("simulate interrupted replacement");

        let loaded = store
            .load(&SessionLoadRequest::new(grant.clone(), saved.id()))
            .expect("load backup")
            .into_session()
            .expect("recovered session");
        assert_eq!(loaded.id(), saved.id());

        let listed = store
            .list(&SessionListRequest::new(grant).with_limit(10))
            .expect("list backup");
        assert_eq!(listed.sessions().len(), 1);
        assert_eq!(listed.sessions()[0].id(), saved.id());
        fs::remove_dir_all(root).expect("remove test root");
    }

    fn test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("yunxi-storage-{label}-{}-{unique}", process::id()));
        fs::create_dir_all(&root).expect("create test root");
        root
    }
}
