//! Bounded read-only loading of legacy global and workspace memory files.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::record::StoredMemoryRecord;

const MAX_MEMORY_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEMORY_LINE_BYTES: usize = 1024 * 1024;
const MAX_LOADED_RECORDS: usize = 50_000;

#[derive(Clone, Debug)]
pub(crate) struct MemoryStore {
    global_root: PathBuf,
    workspace_root: PathBuf,
    workspace_fingerprint: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryLoad {
    pub(crate) records: Vec<StoredMemoryRecord>,
    pub(crate) warnings: Vec<String>,
}

impl MemoryStore {
    pub(crate) fn for_workspace(cwd: &Path) -> Result<Self, MemoryStoreError> {
        Self::with_home(cwd, &yunxi_home_dir())
    }

    pub(crate) fn with_home(cwd: &Path, home: &Path) -> Result<Self, MemoryStoreError> {
        let canonical = fs::canonicalize(cwd).map_err(|source| MemoryStoreError::Workspace {
            path: cwd.to_path_buf(),
            source,
        })?;
        if !canonical.is_dir() {
            return Err(MemoryStoreError::NotDirectory(canonical));
        }
        Ok(Self {
            global_root: home.join("memory"),
            workspace_root: cwd.join(".yunxi").join("memory"),
            workspace_fingerprint: workspace_fingerprint(&canonical),
        })
    }

    pub(crate) fn workspace_fingerprint(&self) -> &str {
        &self.workspace_fingerprint
    }

    pub(crate) fn load(&self) -> MemoryLoad {
        let paths = [
            self.global_root.join("global-memory.jsonl"),
            self.global_root.join("pending.jsonl"),
            self.workspace_root.join("workspace-memory.jsonl"),
            self.workspace_root.join("pending.jsonl"),
        ];
        let mut load = MemoryLoad::default();
        for path in paths {
            read_memory_file(&path, &mut load);
            if load.records.len() >= MAX_LOADED_RECORDS {
                load.warnings.push(format!(
                    "memory record limit reached at {MAX_LOADED_RECORDS}; remaining files were skipped"
                ));
                break;
            }
        }
        load.records = collapse_latest(load.records);
        load.records.sort_by(|left, right| {
            right
                .updated_at_millis
                .cmp(&left.updated_at_millis)
                .then_with(|| left.id.cmp(&right.id))
        });
        load
    }
}

pub(crate) fn workspace_fingerprint(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical
        .to_string_lossy()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn yunxi_home_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("YUNXI_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(path).join(".yunxi");
    }
    if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".yunxi");
    }
    PathBuf::from(".yunxi")
}

fn read_memory_file(path: &Path, load: &mut MemoryLoad) {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            load.warnings
                .push(format!("failed to inspect {}: {error}", path.display()));
            return;
        }
    };
    if !metadata.is_file() {
        load.warnings
            .push(format!("memory path is not a file: {}", path.display()));
        return;
    }
    if metadata.len() > MAX_MEMORY_FILE_BYTES {
        load.warnings.push(format!(
            "memory file {} is {} bytes; maximum is {MAX_MEMORY_FILE_BYTES}",
            path.display(),
            metadata.len()
        ));
        return;
    }
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            load.warnings
                .push(format!("failed to read {}: {error}", path.display()));
            return;
        }
    };

    for (index, line) in content.lines().enumerate() {
        if load.records.len() >= MAX_LOADED_RECORDS {
            return;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_MEMORY_LINE_BYTES {
            load.warnings.push(format!(
                "memory line {} in {} exceeds {MAX_MEMORY_LINE_BYTES} bytes",
                index + 1,
                path.display()
            ));
            continue;
        }
        let mut record = match serde_json::from_str::<StoredMemoryRecord>(line) {
            Ok(record) => record,
            Err(error) => {
                load.warnings.push(format!(
                    "failed to parse memory line {} in {}: {error}",
                    index + 1,
                    path.display()
                ));
                continue;
            }
        };
        if let Err(error) = record.validate() {
            load.warnings.push(format!(
                "invalid memory line {} in {}: {error}",
                index + 1,
                path.display()
            ));
            continue;
        }
        load.records.push(record);
    }
}

fn collapse_latest(records: Vec<StoredMemoryRecord>) -> Vec<StoredMemoryRecord> {
    let mut by_id = BTreeMap::<String, StoredMemoryRecord>::new();
    for record in records {
        match by_id.get_mut(&record.id) {
            Some(existing) if should_replace(existing, &record) => *existing = record,
            Some(_) => {}
            None => {
                by_id.insert(record.id.clone(), record);
            }
        }
    }

    let mut by_key = BTreeMap::<String, StoredMemoryRecord>::new();
    for record in by_id.into_values() {
        let key = format!("{}|{}", record.dedup_key(), record.status.storage_group());
        match by_key.get_mut(&key) {
            Some(existing) if should_replace(existing, &record) => *existing = record,
            Some(_) => {}
            None => {
                by_key.insert(key, record);
            }
        }
    }
    by_key.into_values().collect()
}

fn should_replace(existing: &StoredMemoryRecord, candidate: &StoredMemoryRecord) -> bool {
    candidate.updated_at_millis > existing.updated_at_millis
        || (candidate.updated_at_millis == existing.updated_at_millis
            && candidate.revision > existing.revision)
        || (candidate.updated_at_millis == existing.updated_at_millis
            && candidate.revision == existing.revision
            && candidate.importance > existing.importance)
}

#[derive(Debug)]
pub enum MemoryStoreError {
    Workspace {
        path: PathBuf,
        source: std::io::Error,
    },
    NotDirectory(PathBuf),
}

impl fmt::Display for MemoryStoreError {
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
                    "memory workspace is not a directory: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for MemoryStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Workspace { source, .. } => Some(source),
            Self::NotDirectory(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn malformed_lines_are_warnings_and_valid_records_still_load() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "yunxi-memory-store-{}-{unique}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(home.join("memory")).expect("create memory directory");
        let fixture = concat!(
            "not-json\n",
            "{\"id\":\"valid\",\"scope\":\"global_user\",\"kind\":\"preference\",",
            "\"content\":\"Use concise replies\",\"confidence\":0.9,\"importance\":0.8,",
            "\"sensitivity\":\"low\",\"status\":\"active\",",
            "\"created_at_millis\":1,\"updated_at_millis\":1}\n",
            "{\"id\":\"pending-copy\",\"scope\":\"global_user\",\"kind\":\"preference\",",
            "\"content\":\"Use concise replies\",\"confidence\":0.9,\"importance\":0.8,",
            "\"sensitivity\":\"low\",\"status\":\"pending\",",
            "\"created_at_millis\":2,\"updated_at_millis\":2}\n"
        );
        fs::write(home.join("memory/global-memory.jsonl"), fixture).expect("write memory fixture");
        let store = MemoryStore::with_home(&workspace, &home).expect("create store");

        let load = store.load();

        assert_eq!(load.records.len(), 2);
        assert!(load.records.iter().any(|record| record.id == "valid"));
        assert!(
            load.records
                .iter()
                .any(|record| record.id == "pending-copy")
        );
        assert_eq!(load.warnings.len(), 1);
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
