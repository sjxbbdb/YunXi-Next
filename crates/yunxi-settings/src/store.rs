//! Versioned, bounded, and atomically replaced capability and plugin settings.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CapabilityOverrides, CapabilitySetting, CapabilitySwitches};

pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const MAX_SETTINGS_FILE_BYTES: usize = 64 * 1024;
pub const MAX_PLUGIN_ID_BYTES: usize = 128;
pub const MAX_PLUGIN_OVERRIDES: usize = 256;
const SETTINGS_DOCUMENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityEdit {
    Set(CapabilitySetting, bool),
    Unset(CapabilitySetting),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginEdit {
    Set(String, bool),
    Unset(String),
}

#[derive(Clone, Debug)]
pub struct CapabilitySettingsStore {
    path: PathBuf,
    overrides: CapabilityOverrides,
    plugin_overrides: BTreeMap<String, bool>,
    revision: u64,
    warnings: Vec<String>,
}

impl CapabilitySettingsStore {
    pub fn from_environment() -> Self {
        Self::load(next_state_root().join(SETTINGS_FILE_NAME))
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let backup = backup_path(&path);
        let mut warnings = Vec::new();

        match load_document(&path) {
            Ok(Some(document)) => return Self::from_document(path, document, warnings),
            Ok(None) => {}
            Err(error) => warnings.push(error.to_string()),
        }

        match load_document(&backup) {
            Ok(Some(document)) => {
                warnings.push(
                    "capability settings recovered from an interrupted-write backup".to_string(),
                );
                Self::from_document(path, document, warnings)
            }
            Ok(None) => Self::empty(path, warnings),
            Err(error) => {
                warnings.push(error.to_string());
                Self::empty(path, warnings)
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn overrides(&self) -> &CapabilityOverrides {
        &self.overrides
    }

    /// Returns persisted user choices keyed by plugin id.
    pub fn plugin_overrides(&self) -> &BTreeMap<String, bool> {
        &self.plugin_overrides
    }

    /// Resolves a plugin choice without assuming that the plugin is installed.
    pub fn plugin_enabled(&self, id: &str, default: bool) -> bool {
        self.plugin_overrides.get(id).copied().unwrap_or(default)
    }

    pub fn resolved(&self) -> CapabilitySwitches {
        CapabilitySwitches::from_overrides(&self.overrides)
    }

    pub fn effective_from_environment(&self) -> CapabilitySwitches {
        self.resolved().with_environment(|name| env::var(name).ok())
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    pub fn update(
        &mut self,
        patch: CapabilityOverrides,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        let mut next = self.overrides.clone();
        next.merge(&patch);
        self.commit(next, self.plugin_overrides.clone())
    }

    pub fn replace(
        &mut self,
        section: CapabilityOverrides,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        self.commit(section, self.plugin_overrides.clone())
    }

    pub fn mutate(
        &mut self,
        edits: impl IntoIterator<Item = CapabilityEdit>,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        let mut next = self.overrides.clone();
        for edit in edits {
            match edit {
                CapabilityEdit::Set(setting, value) => next.set(setting, value),
                CapabilityEdit::Unset(setting) => next.unset(setting),
            }
        }
        self.commit(next, self.plugin_overrides.clone())
    }

    pub fn set_plugin(
        &mut self,
        id: impl Into<String>,
        enabled: bool,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.mutate_plugins([PluginEdit::Set(id.into(), enabled)], expected_revision)
    }

    pub fn unset_plugin(
        &mut self,
        id: impl Into<String>,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.mutate_plugins([PluginEdit::Unset(id.into())], expected_revision)
    }

    pub fn update_plugins(
        &mut self,
        patch: BTreeMap<String, bool>,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        validate_plugin_overrides(&patch)?;
        let mut next = self.plugin_overrides.clone();
        next.extend(patch);
        validate_plugin_overrides(&next)?;
        self.commit(self.overrides.clone(), next)
    }

    pub fn replace_plugins(
        &mut self,
        plugins: BTreeMap<String, bool>,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        validate_plugin_overrides(&plugins)?;
        self.commit(self.overrides.clone(), plugins)
    }

    pub fn mutate_plugins(
        &mut self,
        edits: impl IntoIterator<Item = PluginEdit>,
        expected_revision: Option<u64>,
    ) -> Result<bool, CapabilitySettingsError> {
        self.check_revision(expected_revision)?;
        let mut next = self.plugin_overrides.clone();
        for edit in edits {
            match edit {
                PluginEdit::Set(id, enabled) => {
                    validate_plugin_id(&id)?;
                    if !next.contains_key(&id) && next.len() >= MAX_PLUGIN_OVERRIDES {
                        return Err(CapabilitySettingsError::TooManyPluginOverrides {
                            count: next.len().saturating_add(1),
                            maximum: MAX_PLUGIN_OVERRIDES,
                        });
                    }
                    next.insert(id, enabled);
                }
                PluginEdit::Unset(id) => {
                    validate_plugin_id(&id)?;
                    next.remove(&id);
                }
            }
        }
        self.commit(self.overrides.clone(), next)
    }

    pub fn parse_section(value: &Value) -> Result<CapabilityOverrides, CapabilitySettingsError> {
        if !value.is_object() {
            return Err(CapabilitySettingsError::InvalidSection(
                "capability settings section must be an object".to_string(),
            ));
        }
        serde_json::from_value(value.clone()).map_err(|error| {
            CapabilitySettingsError::InvalidSection(format!(
                "capability settings section is invalid: {error}"
            ))
        })
    }

    pub fn parse_plugins(value: &Value) -> Result<BTreeMap<String, bool>, CapabilitySettingsError> {
        if !value.is_object() {
            return Err(CapabilitySettingsError::InvalidPluginSection(
                "plugin settings section must be an object".to_string(),
            ));
        }
        let plugins = serde_json::from_value(value.clone()).map_err(|error| {
            CapabilitySettingsError::InvalidPluginSection(format!(
                "plugin settings section is invalid: {error}"
            ))
        })?;
        validate_plugin_overrides(&plugins)?;
        Ok(plugins)
    }

    fn from_document(path: PathBuf, document: SettingsDocument, warnings: Vec<String>) -> Self {
        Self {
            path,
            overrides: document.capabilities,
            plugin_overrides: document.plugins,
            revision: document.revision,
            warnings,
        }
    }

    fn empty(path: PathBuf, warnings: Vec<String>) -> Self {
        Self {
            path,
            overrides: CapabilityOverrides::default(),
            plugin_overrides: BTreeMap::new(),
            revision: 0,
            warnings,
        }
    }

    fn check_revision(&self, expected: Option<u64>) -> Result<(), CapabilitySettingsError> {
        if let Some(expected) = expected
            && expected != self.revision
        {
            return Err(CapabilitySettingsError::Conflict {
                expected,
                current: self.revision,
            });
        }
        Ok(())
    }

    fn commit(
        &mut self,
        next: CapabilityOverrides,
        next_plugins: BTreeMap<String, bool>,
    ) -> Result<bool, CapabilitySettingsError> {
        if next == self.overrides && next_plugins == self.plugin_overrides {
            return Ok(false);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(CapabilitySettingsError::RevisionExhausted)?;
        let document = SettingsDocument {
            version: SETTINGS_DOCUMENT_VERSION,
            revision,
            capabilities: next.clone(),
            plugins: next_plugins.clone(),
        };
        let mut content =
            serde_json::to_vec_pretty(&document).map_err(CapabilitySettingsError::Serialize)?;
        content.push(b'\n');
        if content.len() > MAX_SETTINGS_FILE_BYTES {
            return Err(CapabilitySettingsError::TooLarge {
                path: self.path.clone(),
                maximum: MAX_SETTINGS_FILE_BYTES,
            });
        }
        replace_file(&self.path, &content)?;
        self.overrides = next;
        self.plugin_overrides = next_plugins;
        self.revision = revision;
        Ok(true)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsDocument {
    version: u32,
    revision: u64,
    #[serde(default)]
    capabilities: CapabilityOverrides,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    plugins: BTreeMap<String, bool>,
}

fn load_document(path: &Path) -> Result<Option<SettingsDocument>, CapabilitySettingsError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CapabilitySettingsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(CapabilitySettingsError::InvalidDocument {
            path: path.to_path_buf(),
            message: "settings path is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_SETTINGS_FILE_BYTES as u64 {
        return Err(CapabilitySettingsError::TooLarge {
            path: path.to_path_buf(),
            maximum: MAX_SETTINGS_FILE_BYTES,
        });
    }
    let content = fs::read(path).map_err(|source| CapabilitySettingsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let document = serde_json::from_slice::<SettingsDocument>(&content).map_err(|error| {
        CapabilitySettingsError::InvalidDocument {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    validate_plugin_overrides(&document.plugins)?;
    if document.version != SETTINGS_DOCUMENT_VERSION {
        return Err(CapabilitySettingsError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: document.version,
        });
    }
    Ok(Some(document))
}

fn validate_plugin_overrides(
    plugins: &BTreeMap<String, bool>,
) -> Result<(), CapabilitySettingsError> {
    if plugins.len() > MAX_PLUGIN_OVERRIDES {
        return Err(CapabilitySettingsError::TooManyPluginOverrides {
            count: plugins.len(),
            maximum: MAX_PLUGIN_OVERRIDES,
        });
    }
    for id in plugins.keys() {
        validate_plugin_id(id)?;
    }
    Ok(())
}

fn validate_plugin_id(id: &str) -> Result<(), CapabilitySettingsError> {
    if id.is_empty()
        || id.len() > MAX_PLUGIN_ID_BYTES
        || id.chars().any(|character| {
            !character.is_ascii() || character.is_control() || character.is_whitespace()
        })
    {
        return Err(CapabilitySettingsError::InvalidPluginId { id: id.to_string() });
    }
    Ok(())
}

fn replace_file(target: &Path, content: &[u8]) -> Result<(), CapabilitySettingsError> {
    let parent = target
        .parent()
        .ok_or_else(|| CapabilitySettingsError::InvalidDocument {
            path: target.to_path_buf(),
            message: "settings path has no parent directory".to_string(),
        })?;
    fs::create_dir_all(parent).map_err(|source| CapabilitySettingsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".yunxi-settings-{}-{unique}.tmp", process::id()));
    let backup = backup_path(target);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| CapabilitySettingsError::Io {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(&temporary);
        return Err(CapabilitySettingsError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);

    if target.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|source| CapabilitySettingsError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        fs::rename(target, &backup).map_err(|source| CapabilitySettingsError::Io {
            path: target.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temporary, target) {
        if backup.exists() {
            let _ignored = fs::rename(&backup, target);
        }
        let _ignored = fs::remove_file(&temporary);
        return Err(CapabilitySettingsError::Io {
            path: target.to_path_buf(),
            source,
        });
    }
    if backup.exists() {
        let _ignored = fs::remove_file(backup);
    }
    Ok(())
}

fn backup_path(target: &Path) -> PathBuf {
    target.with_extension("json.bak")
}

pub fn next_state_root() -> PathBuf {
    if let Some(path) = env::var_os("YUNXI_NEXT_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = env::var_os("USERPROFILE") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    if let Some(path) = env::var_os("HOME") {
        return PathBuf::from(path).join(".yunxi-next");
    }
    PathBuf::from(".yunxi-next")
}

#[derive(Debug)]
pub enum CapabilitySettingsError {
    Conflict {
        expected: u64,
        current: u64,
    },
    InvalidSection(String),
    InvalidPluginSection(String),
    InvalidPluginId {
        id: String,
    },
    TooManyPluginOverrides {
        count: usize,
        maximum: usize,
    },
    InvalidDocument {
        path: PathBuf,
        message: String,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
    },
    TooLarge {
        path: PathBuf,
        maximum: usize,
    },
    RevisionExhausted,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for CapabilitySettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { expected, current } => write!(
                formatter,
                "settings revision conflict: expected {expected}, current {current}"
            ),
            Self::InvalidSection(message) => formatter.write_str(message),
            Self::InvalidPluginSection(message) => formatter.write_str(message),
            Self::InvalidPluginId { id } => {
                write!(formatter, "invalid plugin id `{id}` in settings")
            }
            Self::TooManyPluginOverrides { count, maximum } => write!(
                formatter,
                "settings contain {count} plugin overrides; maximum is {maximum}"
            ),
            Self::InvalidDocument { path, message } => {
                write!(
                    formatter,
                    "invalid settings document {}: {message}",
                    path.display()
                )
            }
            Self::UnsupportedVersion { path, version } => write!(
                formatter,
                "settings document {} uses unsupported version {version}",
                path.display()
            ),
            Self::TooLarge { path, maximum } => write!(
                formatter,
                "settings document {} exceeds {maximum} bytes",
                path.display()
            ),
            Self::RevisionExhausted => formatter.write_str("settings revision is exhausted"),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "settings I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Serialize(error) => write!(formatter, "failed to serialize settings: {error}"),
        }
    }
}

impl Error for CapabilitySettingsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    #[test]
    fn revisioned_updates_round_trip_through_the_bounded_document() {
        let root = test_root("round-trip");
        let path = root.join(SETTINGS_FILE_NAME);
        let mut store = CapabilitySettingsStore::load(&path);
        let patch = CapabilitySettingsStore::parse_section(&json!({
            "memory": true,
            "shell": true
        }))
        .expect("patch");

        assert!(store.update(patch, Some(0)).expect("persist"));
        assert_eq!(store.revision(), 1);
        let loaded = CapabilitySettingsStore::load(&path);
        assert_eq!(loaded.revision(), 1);
        assert!(loaded.resolved().memory);
        assert!(loaded.resolved().shell);
        assert!(!path.with_extension("json.bak").exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn legacy_document_without_voice_or_weixin_loads_with_external_capabilities_off() {
        let root = test_root("legacy-capabilities");
        let path = root.join(SETTINGS_FILE_NAME);
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": SETTINGS_DOCUMENT_VERSION,
                "revision": 3,
                "capabilities": {
                    "context": true,
                    "persona": true,
                    "storage": true,
                    "memory": true
                }
            }))
            .expect("legacy settings JSON"),
        )
        .expect("write legacy settings");

        let loaded = CapabilitySettingsStore::load(&path);
        assert_eq!(loaded.revision(), 3);
        assert!(loaded.resolved().memory);
        assert!(!loaded.resolved().voice);
        assert!(!loaded.resolved().weixin);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn plugin_switches_persist_even_when_the_plugin_is_not_installed() {
        let root = test_root("plugin-switch");
        let path = root.join(SETTINGS_FILE_NAME);
        let mut store = CapabilitySettingsStore::load(&path);

        assert!(
            store
                .set_plugin("not-installed", true, Some(0))
                .expect("persist plugin choice")
        );
        assert!(store.plugin_enabled("not-installed", false));
        assert!(!store.plugin_enabled("missing", false));

        let loaded = CapabilitySettingsStore::load(&path);
        assert_eq!(loaded.plugin_overrides().get("not-installed"), Some(&true));
        assert!(loaded.plugin_enabled("not-installed", false));
        assert!(
            serde_json::from_slice::<Value>(&fs::read(&path).expect("settings"))
                .expect("JSON")
                .get("plugins")
                .is_some()
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn plugin_switch_can_be_disabled_and_removed_with_revision_fencing() {
        let root = test_root("plugin-disable");
        let path = root.join(SETTINGS_FILE_NAME);
        let mut store = CapabilitySettingsStore::load(&path);

        assert!(
            store
                .set_plugin("optional.plugin", true, Some(0))
                .expect("enable plugin")
        );
        assert!(store.plugin_enabled("optional.plugin", false));
        assert!(
            store
                .set_plugin("optional.plugin", false, Some(1))
                .expect("disable plugin")
        );
        assert!(!store.plugin_enabled("optional.plugin", true));
        assert!(
            store
                .unset_plugin("optional.plugin", Some(2))
                .expect("remove plugin override")
        );
        assert!(store.plugin_enabled("optional.plugin", true));
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn stale_revision_is_rejected_before_the_document_changes() {
        let root = test_root("conflict");
        let path = root.join(SETTINGS_FILE_NAME);
        fs::create_dir_all(&root).expect("create root");
        let mut store = CapabilitySettingsStore::load(&path);
        let patch =
            CapabilitySettingsStore::parse_section(&json!({ "memory": true })).expect("patch");
        let error = store.update(patch, Some(9)).expect_err("conflict");
        assert!(matches!(
            error,
            CapabilitySettingsError::Conflict {
                expected: 9,
                current: 0
            }
        ));
        assert!(!path.exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn malformed_or_unknown_sections_are_rejected() {
        assert!(CapabilitySettingsStore::parse_section(&json!([])).is_err());
        assert!(CapabilitySettingsStore::parse_section(&json!({ "unknown": true })).is_err());
        assert!(CapabilitySettingsStore::parse_section(&json!({ "memory": "yes" })).is_err());
        assert!(CapabilitySettingsStore::parse_plugins(&json!({ "bad id": true })).is_err());
        assert!(
            CapabilitySettingsStore::parse_plugins(&json!({ "unknown.plugin": false })).is_ok()
        );
    }

    #[test]
    fn malformed_target_recovers_from_a_valid_backup() {
        let root = test_root("backup");
        let path = root.join(SETTINGS_FILE_NAME);
        fs::create_dir_all(&root).expect("create root");
        fs::write(&path, b"not-json").expect("write malformed target");
        fs::write(
            path.with_extension("json.bak"),
            serde_json::to_vec(&SettingsDocument {
                version: SETTINGS_DOCUMENT_VERSION,
                revision: 7,
                capabilities: CapabilitySettingsStore::parse_section(&json!({
                    "files": true
                }))
                .expect("section"),
                plugins: BTreeMap::from([(String::from("recovered.plugin"), true)]),
            })
            .expect("backup JSON"),
        )
        .expect("write backup");

        let mut store = CapabilitySettingsStore::load(&path);
        assert_eq!(store.revision(), 7);
        assert!(store.resolved().files);
        assert!(store.plugin_enabled("recovered.plugin", false));
        assert_eq!(store.take_warnings().len(), 2);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn oversized_files_fall_back_to_defaults_without_unbounded_reads() {
        let root = test_root("oversized");
        let path = root.join(SETTINGS_FILE_NAME);
        fs::create_dir_all(&root).expect("create root");
        fs::write(&path, vec![b'x'; MAX_SETTINGS_FILE_BYTES + 1]).expect("write fixture");

        let mut store = CapabilitySettingsStore::load(&path);
        assert_eq!(store.resolved(), CapabilitySwitches::default());
        assert_eq!(store.take_warnings().len(), 1);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        env::temp_dir().join(format!("yunxi-settings-{label}-{}-{unique}", process::id()))
    }
}
