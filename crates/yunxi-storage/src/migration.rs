//! Explicit, workspace-scoped migration of legacy session files.
//!
//! The source namespace is read-only. Migration writes only new session files
//! and a durable manifest below `.yunxi-next/migrations/sessions`. Existing
//! Next files are never replaced. Rollback removes only files whose content
//! still matches the manifest, so a later user edit is preserved.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use yunxi_protocol::WorkspaceGrant;

use crate::record::validate_session_id;
use crate::store::{
    MAX_SCANNED_FILES, MAX_SESSION_FILE_BYTES, SessionStore, StorageError, read_bounded,
    read_legacy_record,
};

const MIGRATION_SCHEMA_VERSION: u32 = 1;
const MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MEMORY_MIGRATION_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEMORY_MIGRATION_LINE_BYTES: usize = 1024 * 1024;
const MAX_MEMORY_MIGRATION_RECORDS: usize = 50_000;
static MIGRATION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A validated migration facade bound to one canonical workspace.
#[derive(Clone, Debug)]
pub struct SessionMigration {
    workspace_root: PathBuf,
    legacy_root: PathBuf,
    next_root: PathBuf,
    manifest_root: PathBuf,
    legacy_home: Option<PathBuf>,
    next_home: Option<PathBuf>,
    legacy_read: bool,
    next_write: bool,
}

impl SessionMigration {
    /// Create a migration facade from the same workspace grant used by the
    /// session store. The legacy namespace is never opened for writing.
    pub fn from_grant(grant: &WorkspaceGrant) -> Result<Self, MigrationError> {
        let legacy_home =
            optional_home("YUNXI_MIGRATION_LEGACY_HOME").or_else(|| optional_home("YUNXI_HOME"));
        let next_home = grant
            .state_root()
            .map(Path::to_path_buf)
            .or_else(|| optional_home("YUNXI_NEXT_HOME"));
        Self::from_grant_with_homes(grant, legacy_home.as_deref(), next_home.as_deref())
    }

    /// Create a migration facade with explicit optional user-home roots.
    ///
    /// This is useful to callers that already have a user-approved discovery
    /// result. Passing `None` keeps migration workspace-local only.
    pub fn from_grant_with_homes(
        grant: &WorkspaceGrant,
        legacy_home: Option<&Path>,
        next_home: Option<&Path>,
    ) -> Result<Self, MigrationError> {
        let store = SessionStore::from_grant(grant).map_err(MigrationError::Storage)?;
        let workspace_root =
            fs::canonicalize(grant.root()).map_err(|source| MigrationError::Io {
                path: grant.root().to_path_buf(),
                source,
            })?;
        let legacy_root = workspace_root.join(".yunxi").join("sessions");
        let next_root = workspace_root.join(".yunxi-next").join("sessions");
        let manifest_root = workspace_root
            .join(".yunxi-next")
            .join("migrations")
            .join("sessions");
        ensure_workspace_path(&workspace_root, &legacy_root)?;
        ensure_workspace_path(&workspace_root, &next_root)?;
        ensure_workspace_path(&workspace_root, &manifest_root)?;
        if let Some(path) = legacy_home {
            ensure_home_path(path)?;
        }
        if let Some(path) = next_home {
            ensure_home_path(path)?;
        }
        // Keep this constructor coupled to SessionStore's grant validation.
        let _ = store;
        Ok(Self {
            workspace_root,
            legacy_root,
            next_root,
            manifest_root,
            legacy_home: legacy_home.map(Path::to_path_buf),
            next_home: next_home.map(Path::to_path_buf),
            legacy_read: grant.allows_legacy_read(),
            next_write: grant.allows_next_write(),
        })
    }

    /// Inspect legacy session files without creating or modifying any file.
    pub fn plan(&self) -> Result<MigrationPlan, MigrationError> {
        if !self.legacy_read {
            return Err(MigrationError::LegacyReadNotGranted);
        }
        let migration_id = new_migration_id();
        let source_root = relative_path(&self.workspace_root, &self.legacy_root)?;
        let target_root = relative_path(&self.workspace_root, &self.next_root)?;
        let manifest_path = relative_path(
            &self.workspace_root,
            &self.manifest_root.join(format!("{migration_id}.json")),
        )?;
        let mut items = Vec::new();
        let mut warnings = Vec::new();
        let mut seen_targets = BTreeSet::new();
        let entries = match fs::read_dir(&self.legacy_root) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(MigrationError::Io {
                    path: self.legacy_root.clone(),
                    source,
                });
            }
        };
        if let Some(entries) = entries {
            for (index, entry) in entries.enumerate() {
                if index >= MAX_SCANNED_FILES {
                    warnings.push(format!(
                        "migration scan stopped after {MAX_SCANNED_FILES} files"
                    ));
                    break;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        warnings.push(format!("failed to read legacy session entry: {error}"));
                        continue;
                    }
                };
                let source = entry.path();
                if source.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let source_relative = match relative_path(&self.workspace_root, &source) {
                    Ok(path) => path,
                    Err(error) => {
                        warnings.push(error.to_string());
                        continue;
                    }
                };
                let source_bytes = match read_bounded(&source) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => continue,
                    Err(error) => {
                        items.push(MigrationItem::invalid(source_relative, error.to_string()));
                        continue;
                    }
                };
                let record = match read_legacy_record(&source) {
                    Ok(Some(record)) => record,
                    Ok(None) => continue,
                    Err(error) => {
                        items.push(MigrationItem::invalid(source_relative, error.to_string()));
                        continue;
                    }
                };
                let id = record.id().to_string();
                let target = self.next_root.join(format!("{id}.json"));
                let target_relative = relative_path(&self.workspace_root, &target)?;
                let target_exists = target.exists();
                let duplicate = !seen_targets.insert(target_relative.clone());
                let state = if duplicate {
                    MigrationItemState::Invalid {
                        reason: "multiple legacy files resolve to the same session id".to_string(),
                    }
                } else if target_exists {
                    MigrationItemState::TargetExists
                } else {
                    MigrationItemState::Ready
                };
                items.push(MigrationItem {
                    id,
                    capability: "sessions".to_string(),
                    scope: MigrationScope::Workspace,
                    source: source_relative,
                    target: target_relative,
                    source_bytes: source_bytes.len() as u64,
                    source_fingerprint: fingerprint(&source_bytes),
                    state,
                });
            }
        }
        let workspace_memory_before = items.len();
        let workspace_relationship_count = self.plan_workspace_memory(&mut items, &mut warnings)?;
        let mut capabilities = vec![capability_from_items(
            "sessions",
            "workspace",
            &items,
            "sessions",
        )];
        if !items[workspace_memory_before..].is_empty() {
            capabilities.push(capability_from_item_range(
                "memory",
                "workspace",
                &items[workspace_memory_before..],
            ));
            capabilities.push(derived_relationship_capability(
                "workspace",
                workspace_relationship_count,
                true,
                has_unsupported_memory(&items[workspace_memory_before..]),
            ));
        }
        capabilities.extend(self.plan_user_home(&mut items, &mut warnings)?);
        Ok(MigrationPlan {
            schema_version: MIGRATION_SCHEMA_VERSION,
            migration_id,
            source_root,
            target_root,
            manifest_path,
            workspace_root: path_string(&self.workspace_root),
            items,
            warnings,
            legacy_user_home: self.legacy_home.as_ref().map(|path| path_string(path)),
            next_user_home: self.next_home.as_ref().map(|path| path_string(path)),
            capabilities,
        })
    }

    fn plan_workspace_memory(
        &self,
        items: &mut Vec<MigrationItem>,
        warnings: &mut Vec<String>,
    ) -> Result<usize, MigrationError> {
        let mut relationship_count = 0;
        for relative in [
            ".yunxi/memory/workspace-memory.jsonl",
            ".yunxi/memory/pending.jsonl",
            ".yunxi/memory/global-memory.jsonl",
        ] {
            let target = relative.replacen(".yunxi/", ".yunxi-next/", 1);
            relationship_count += self.plan_memory_file(
                items,
                warnings,
                MigrationScope::Workspace,
                relative,
                &target,
            )?;
        }
        Ok(relationship_count)
    }

    fn plan_memory_file(
        &self,
        items: &mut Vec<MigrationItem>,
        warnings: &mut Vec<String>,
        scope: MigrationScope,
        source_relative: &str,
        target_relative: &str,
    ) -> Result<usize, MigrationError> {
        validate_relative_path(source_relative)?;
        validate_relative_path(target_relative)?;
        let source_root = match scope {
            MigrationScope::Workspace => &self.workspace_root,
            MigrationScope::LegacyUserHome => self.legacy_home.as_ref().ok_or_else(|| {
                MigrationError::InvalidPlan(
                    "legacy user home was not discovered for this migration plan".to_string(),
                )
            })?,
        };
        let target_root = match scope {
            MigrationScope::Workspace => &self.workspace_root,
            MigrationScope::LegacyUserHome => self.next_home.as_ref().ok_or_else(|| {
                MigrationError::InvalidPlan(
                    "Next user home was not configured for this migration plan".to_string(),
                )
            })?,
        };
        let source = source_root.join(source_relative);
        let target = target_root.join(target_relative);
        ensure_contained_path(source_root, &source)?;
        ensure_contained_path(target_root, &target)?;
        let Some(bytes) = read_bounded_with_limit(&source, MAX_MEMORY_MIGRATION_FILE_BYTES)? else {
            return Ok(0);
        };

        let (state, relationship_count) = match normalize_memory_jsonl(&bytes) {
            Ok(normalized) => {
                let state = if target.exists() {
                    MigrationItemState::TargetExists
                } else {
                    MigrationItemState::Ready
                };
                (state, normalized.relationship_count)
            }
            Err(reason) => {
                let unsupported = !looks_like_json_memory_file(&bytes);
                let reason = if unsupported {
                    format!("unsupported: legacy memory format requires privacy review: {reason}")
                } else {
                    format!("memory file is invalid: {reason}")
                };
                warnings.push(format!(
                    "memory file {source_relative} is not migratable: {reason}"
                ));
                (MigrationItemState::Invalid { reason }, 0)
            }
        };
        items.push(MigrationItem {
            id: format!("memory:{source_relative}"),
            capability: "memory".to_string(),
            scope,
            source: source_relative.to_string(),
            target: target_relative.to_string(),
            source_bytes: bytes.len() as u64,
            source_fingerprint: fingerprint(&bytes),
            state,
        });
        Ok(relationship_count)
    }

    /// Apply a previously generated plan without replacing existing Next data.
    pub fn apply(&self, plan: &MigrationPlan) -> Result<MigrationReport, MigrationError> {
        self.require_write()?;
        self.validate_plan(plan)?;
        let manifest_path = self.manifest_path(plan)?;
        if manifest_path.exists() {
            return Err(MigrationError::ManifestExists(manifest_path));
        }
        let mut manifest = MigrationManifest::from_plan(plan);
        write_new_manifest(&manifest_path, &manifest)?;
        let mut report = MigrationReport::default();
        for (index, item) in plan.items.iter().enumerate() {
            match &item.state {
                MigrationItemState::Invalid { reason } => {
                    report.invalid += 1;
                    report.warnings.push(format!("{}: {reason}", item.source));
                    manifest.items[index].state = MigrationItemState::Invalid {
                        reason: reason.clone(),
                    };
                }
                MigrationItemState::TargetExists => {
                    report.skipped_existing += 1;
                    manifest.items[index].state = MigrationItemState::TargetExists;
                }
                MigrationItemState::Ready => {
                    let outcome = self.copy_one(item)?;
                    match outcome {
                        CopyOutcome::Copied(target_fingerprint) => {
                            report.copied += 1;
                            manifest.items[index].state = MigrationItemState::Applied {
                                target_bytes: target_fingerprint.0,
                                target_fingerprint: target_fingerprint.1,
                            };
                        }
                        CopyOutcome::TargetAppeared => {
                            report.skipped_existing += 1;
                            manifest.items[index].state = MigrationItemState::TargetExists;
                        }
                    }
                }
                MigrationItemState::Applied { .. } | MigrationItemState::RolledBack => {
                    return Err(MigrationError::InvalidPlan(
                        "plan contains an item with an execution state".to_string(),
                    ));
                }
            }
            manifest.state = MigrationState::Applying;
            write_manifest(&manifest_path, &manifest)?;
        }
        manifest.state = MigrationState::Applied;
        manifest.updated_at_millis = now_millis();
        write_manifest(&manifest_path, &manifest)?;
        Ok(report)
    }

    /// Roll back files recorded as created by a migration.
    ///
    /// A target is removed only when its current bytes match the manifest. If
    /// a user or another process changed it, rollback leaves it in place and
    /// reports the reason.
    pub fn rollback(&self, migration_id: &str) -> Result<RollbackReport, MigrationError> {
        self.require_write()?;
        validate_migration_id(migration_id)?;
        let manifest_path = self.manifest_root.join(format!("{migration_id}.json"));
        ensure_workspace_path(&self.workspace_root, &manifest_path)?;
        let mut manifest = read_manifest(&manifest_path)?;
        if manifest.migration_id != migration_id {
            return Err(MigrationError::InvalidManifest(
                "manifest id does not match its filename".to_string(),
            ));
        }
        if (!manifest.workspace_root.is_empty()
            && manifest.workspace_root != path_string(&self.workspace_root))
            || manifest.legacy_user_home != self.legacy_home.as_deref().map(path_string)
            || manifest.next_user_home != self.next_home.as_deref().map(path_string)
        {
            return Err(MigrationError::InvalidManifest(
                "manifest belongs to a different workspace or user-home configuration".to_string(),
            ));
        }
        if manifest.state == MigrationState::RolledBack {
            return Ok(RollbackReport {
                removed: 0,
                preserved: 0,
                already_rolled_back: true,
                warnings: Vec::new(),
            });
        }
        let mut report = RollbackReport::default();
        for item in &mut manifest.items {
            let MigrationItemState::Applied {
                target_bytes,
                target_fingerprint,
            } = &item.state
            else {
                continue;
            };
            let target = self.target_path(item)?;
            let Some(bytes) = read_bounded(&target)? else {
                report
                    .warnings
                    .push(format!("target is already absent: {}", item.target));
                item.state = MigrationItemState::RolledBack;
                continue;
            };
            if bytes.len() as u64 == *target_bytes && fingerprint(&bytes) == *target_fingerprint {
                fs::remove_file(&target).map_err(|source| MigrationError::Io {
                    path: target,
                    source,
                })?;
                report.removed += 1;
                item.state = MigrationItemState::RolledBack;
            } else {
                report.preserved += 1;
                report.warnings.push(format!(
                    "target changed after migration; preserved: {}",
                    item.target
                ));
            }
        }
        manifest.state = MigrationState::RolledBack;
        manifest.updated_at_millis = now_millis();
        write_manifest(&manifest_path, &manifest)?;
        Ok(report)
    }

    fn copy_one(&self, item: &MigrationItem) -> Result<CopyOutcome, MigrationError> {
        let source = self.source_path(item)?;
        let target = self.target_path(item)?;
        let source_bytes = read_bounded(&source)?.ok_or_else(|| {
            MigrationError::SourceChanged(format!("source disappeared: {}", item.source))
        })?;
        if source_bytes.len() as u64 != item.source_bytes
            || fingerprint(&source_bytes) != item.source_fingerprint
        {
            return Err(MigrationError::SourceChanged(item.source.clone()));
        }
        let bytes = if item.capability == "sessions" {
            let record = read_legacy_record(&source)?.ok_or_else(|| {
                MigrationError::SourceChanged(format!("source disappeared: {}", item.source))
            })?;
            if record.id() != item.id {
                return Err(MigrationError::SourceChanged(format!(
                    "session id changed in {}",
                    item.source
                )));
            }
            serde_json::to_vec_pretty(&record).map_err(MigrationError::Serialize)?
        } else if item.capability == "memory" {
            normalize_memory_jsonl(&source_bytes)
                .map_err(|reason| {
                    MigrationError::InvalidSource(format!("{}: {reason}", item.source))
                })?
                .bytes
        } else {
            source_bytes.clone()
        };
        if bytes.len() as u64 > MAX_SESSION_FILE_BYTES {
            return Err(MigrationError::Storage(StorageError::FileTooLarge {
                path: target,
                maximum: MAX_SESSION_FILE_BYTES,
            }));
        }
        let parent = target
            .parent()
            .ok_or_else(|| MigrationError::InvalidPlan("target path has no parent".to_string()))?;
        fs::create_dir_all(parent).map_err(|source| MigrationError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let temporary = parent.join(format!(
            ".migration-{}-{}-{:.0}.tmp",
            process::id(),
            MIGRATION_COUNTER.fetch_add(1, Ordering::Relaxed),
            now_millis()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| MigrationError::Io {
                path: temporary.clone(),
                source,
            })?;
        if let Err(source) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            let _ignored = fs::remove_file(&temporary);
            return Err(MigrationError::Io {
                path: temporary,
                source,
            });
        }
        drop(file);
        if target.exists() {
            let _ignored = fs::remove_file(&temporary);
            return Ok(CopyOutcome::TargetAppeared);
        }
        if let Err(source) = fs::rename(&temporary, &target) {
            let _ignored = fs::remove_file(&temporary);
            if target.exists() {
                return Ok(CopyOutcome::TargetAppeared);
            }
            return Err(MigrationError::Io {
                path: target,
                source,
            });
        }
        Ok(CopyOutcome::Copied((
            bytes.len() as u64,
            fingerprint(&bytes),
        )))
    }

    fn validate_plan(&self, plan: &MigrationPlan) -> Result<(), MigrationError> {
        if plan.schema_version != MIGRATION_SCHEMA_VERSION {
            return Err(MigrationError::InvalidPlan(format!(
                "unsupported migration plan schema_version={}",
                plan.schema_version
            )));
        }
        validate_migration_id(&plan.migration_id)?;
        let expected_source = relative_path(&self.workspace_root, &self.legacy_root)?;
        let expected_target = relative_path(&self.workspace_root, &self.next_root)?;
        if plan.source_root != expected_source || plan.target_root != expected_target {
            return Err(MigrationError::InvalidPlan(
                "migration plan belongs to a different workspace".to_string(),
            ));
        }
        if !plan.workspace_root.is_empty()
            && plan.workspace_root != path_string(&self.workspace_root)
        {
            return Err(MigrationError::InvalidPlan(
                "migration plan belongs to a different workspace".to_string(),
            ));
        }
        if plan.legacy_user_home.as_deref()
            != self.legacy_home.as_deref().map(path_string).as_deref()
            || plan.next_user_home.as_deref()
                != self.next_home.as_deref().map(path_string).as_deref()
        {
            return Err(MigrationError::InvalidPlan(
                "migration plan belongs to a different user-home configuration".to_string(),
            ));
        }
        for item in &plan.items {
            if matches!(item.state, MigrationItemState::Invalid { .. }) {
                let _ = self.path_from_relative(&item.source)?;
                continue;
            }
            if item.capability == "sessions" {
                validate_session_id(&item.id).map_err(MigrationError::InvalidPlan)?;
            }
            let source = self.source_path(item)?;
            let target = self.target_path(item)?;
            if matches!(item.capability.as_str(), "sessions" | "memory")
                && (source.extension().and_then(|value| value.to_str()) != Some("json")
                    && source.extension().and_then(|value| value.to_str()) != Some("jsonl")
                    || target.extension().and_then(|value| value.to_str()) != Some("json")
                        && target.extension().and_then(|value| value.to_str()) != Some("jsonl"))
            {
                return Err(MigrationError::InvalidPlan(
                    "session and memory migration item paths must be JSON or JSONL files"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }

    fn source_path(&self, item: &MigrationItem) -> Result<PathBuf, MigrationError> {
        validate_relative_path(&item.source)?;
        let root = match item.scope {
            MigrationScope::Workspace => &self.workspace_root,
            MigrationScope::LegacyUserHome => self.legacy_home.as_ref().ok_or_else(|| {
                MigrationError::InvalidPlan(
                    "legacy user home was not discovered for this migration plan".to_string(),
                )
            })?,
        };
        let path = root.join(&item.source);
        ensure_contained_path(root, &path)?;
        Ok(path)
    }

    fn target_path(&self, item: &MigrationItem) -> Result<PathBuf, MigrationError> {
        validate_relative_path(&item.target)?;
        let root = match item.scope {
            MigrationScope::Workspace => &self.workspace_root,
            MigrationScope::LegacyUserHome => self.next_home.as_ref().ok_or_else(|| {
                MigrationError::InvalidPlan(
                    "Next user home was not configured for this migration plan".to_string(),
                )
            })?,
        };
        let path = root.join(&item.target);
        ensure_contained_path(root, &path)?;
        Ok(path)
    }

    fn plan_user_home(
        &self,
        items: &mut Vec<MigrationItem>,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<MigrationCapability>, MigrationError> {
        let mut capabilities = Vec::new();

        let Some(legacy_home) = &self.legacy_home else {
            for (name, reason) in [
                (
                    "persona",
                    "legacy user home discovery is disabled; set YUNXI_HOME or YUNXI_MIGRATION_LEGACY_HOME",
                ),
                (
                    "memory",
                    "legacy user home discovery is disabled; set YUNXI_HOME or YUNXI_MIGRATION_LEGACY_HOME",
                ),
                (
                    "controls",
                    "legacy user home discovery is disabled; set YUNXI_HOME or YUNXI_MIGRATION_LEGACY_HOME",
                ),
            ] {
                capabilities.push(MigrationCapability::unsupported(
                    name,
                    "legacy_user_home",
                    reason,
                ));
            }
            capabilities.push(MigrationCapability::unsupported(
                "mailbox",
                "legacy_user_home",
                "legacy mailbox format and encryption key ownership are not known; no files were copied",
            ));
            capabilities.push(MigrationCapability::unsupported(
                "relationship",
                "legacy_user_home",
                "no standalone legacy relationship file format is defined; relationship data is not copied",
            ));
            return Ok(capabilities);
        };
        let Some(next_home) = &self.next_home else {
            capabilities.extend([
                MigrationCapability::unsupported(
                    "persona",
                    "legacy_user_home",
                    "Next user home is not configured; set YUNXI_NEXT_HOME before applying user-home files",
                ),
                MigrationCapability::unsupported(
                    "memory",
                    "legacy_user_home",
                    "Next user home is not configured; legacy memory is not copied",
                ),
                MigrationCapability::unsupported(
                    "controls",
                    "legacy_user_home",
                    "Next user home is not configured; set YUNXI_NEXT_HOME before applying user-home files",
                ),
                MigrationCapability::unsupported(
                    "mailbox",
                    "legacy_user_home",
                    "legacy mailbox format and encryption key ownership are not known; no files were copied",
                ),
                MigrationCapability::unsupported(
                    "relationship",
                    "legacy_user_home",
                    "no standalone legacy relationship file format is defined; relationship data is not copied",
                ),
            ]);
            return Ok(capabilities);
        };
        if paths_overlap(legacy_home, next_home) {
            capabilities.extend([
                MigrationCapability::unsupported(
                    "persona",
                    "legacy_user_home",
                    "legacy and Next user homes overlap; refusing to write into the legacy directory",
                ),
                MigrationCapability::unsupported(
                    "memory",
                    "legacy_user_home",
                    "legacy and Next user homes overlap; legacy memory is not copied",
                ),
                MigrationCapability::unsupported(
                    "controls",
                    "legacy_user_home",
                    "legacy and Next user homes overlap; refusing to write into the legacy directory",
                ),
                MigrationCapability::unsupported(
                    "mailbox",
                    "legacy_user_home",
                    "legacy and Next user homes overlap; no mailbox files were copied",
                ),
                MigrationCapability::unsupported(
                    "relationship",
                    "legacy_user_home",
                    "no standalone legacy relationship file format is defined; relationship data is not copied",
                ),
            ]);
            return Ok(capabilities);
        }

        let persona_before = items.len();
        self.plan_known_file(
            items,
            legacy_home,
            "persona",
            "persona/config.toml",
            64 * 1024,
        )?;
        self.plan_known_file(
            items,
            legacy_home,
            "persona",
            "persona/soul.txt",
            128 * 1024,
        )?;
        self.plan_persona_profiles(items, legacy_home, warnings)?;
        capabilities.push(capability_from_item_range(
            "persona",
            "legacy_user_home",
            &items[persona_before..],
        ));

        let controls_before = items.len();
        self.plan_known_file(items, legacy_home, "controls", "settings.json", 1024 * 1024)?;
        capabilities.push(capability_from_item_range(
            "controls",
            "legacy_user_home",
            &items[controls_before..],
        ));

        let memory_before = items.len();
        let relationship_count = self.plan_user_memory(items, legacy_home, warnings)?;
        capabilities.push(capability_from_item_range(
            "memory",
            "legacy_user_home",
            &items[memory_before..],
        ));
        capabilities.push(derived_relationship_capability(
            "legacy_user_home",
            relationship_count,
            !items[memory_before..].is_empty(),
            has_unsupported_memory(&items[memory_before..]),
        ));

        let mailbox_present =
            legacy_home.join("companion-mailbox").exists() || legacy_home.join("mailbox").exists();
        capabilities.push(MigrationCapability::unsupported(
            "mailbox",
            "legacy_user_home",
            if mailbox_present {
                "legacy mailbox was found, but its format and encryption key ownership are not known; no files were copied"
            } else {
                "legacy mailbox format and encryption key ownership are not known; no files were copied"
            },
        ));
        capabilities.push(MigrationCapability::unsupported(
            "relationship",
            "legacy_user_home",
            "no standalone legacy relationship file format is defined; relationship data is not copied",
        ));
        Ok(capabilities)
    }

    fn plan_user_memory(
        &self,
        items: &mut Vec<MigrationItem>,
        legacy_home: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<usize, MigrationError> {
        let mut relationship_count = 0;
        for relative in [
            "memory/global-memory.jsonl",
            "memory/pending.jsonl",
            "memory/workspace-memory.jsonl",
        ] {
            // `plan_memory_file` resolves user-home paths relative to the
            // configured home; keep this existence check explicit so a
            // missing optional file does not create a migration item.
            if !legacy_home.join(relative).exists() {
                continue;
            }
            relationship_count += self.plan_memory_file(
                items,
                warnings,
                MigrationScope::LegacyUserHome,
                relative,
                relative,
            )?;
        }
        Ok(relationship_count)
    }

    fn plan_known_file(
        &self,
        items: &mut Vec<MigrationItem>,
        legacy_home: &Path,
        capability: &str,
        relative: &str,
        maximum: u64,
    ) -> Result<(), MigrationError> {
        let source = legacy_home.join(relative);
        ensure_contained_path(legacy_home, &source)?;
        let Some(bytes) = read_bounded_with_limit(&source, maximum)? else {
            return Ok(());
        };
        let target = self
            .next_home
            .as_ref()
            .ok_or_else(|| {
                MigrationError::InvalidPlan("Next user home is not configured".to_string())
            })?
            .join(relative);
        ensure_contained_path(
            self.next_home.as_ref().expect("checked Next user home"),
            &target,
        )?;
        let target_exists = target.exists();
        items.push(MigrationItem {
            id: relative.to_string(),
            capability: capability.to_string(),
            scope: MigrationScope::LegacyUserHome,
            source: relative.to_string(),
            target: relative.to_string(),
            source_bytes: bytes.len() as u64,
            source_fingerprint: fingerprint(&bytes),
            state: if target_exists {
                MigrationItemState::TargetExists
            } else {
                MigrationItemState::Ready
            },
        });
        Ok(())
    }

    fn plan_persona_profiles(
        &self,
        items: &mut Vec<MigrationItem>,
        legacy_home: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<(), MigrationError> {
        let root = legacy_home.join("persona").join("profiles");
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                warnings.push(format!(
                    "failed to read persona profile directory {}: {error}",
                    root.display()
                ));
                return Ok(());
            }
        };
        for entry in entries.take(MAX_SCANNED_FILES) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("failed to read persona profile entry: {error}"));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(relative) = path
                .strip_prefix(legacy_home)
                .ok()
                .map(|value| value.to_string_lossy().replace('\\', "/"))
            else {
                continue;
            };
            self.plan_known_file(items, legacy_home, "persona", &relative, 512 * 1024)?;
        }
        Ok(())
    }

    fn path_from_relative(&self, relative: &str) -> Result<PathBuf, MigrationError> {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(MigrationError::InvalidPlan(format!(
                "path must be workspace-relative: {relative}"
            )));
        }
        let path = self.workspace_root.join(relative_path);
        ensure_workspace_path(&self.workspace_root, &path)?;
        Ok(path)
    }

    fn manifest_path(&self, plan: &MigrationPlan) -> Result<PathBuf, MigrationError> {
        self.path_from_relative(&plan.manifest_path)
    }

    fn require_write(&self) -> Result<(), MigrationError> {
        if self.next_write {
            Ok(())
        } else {
            Err(MigrationError::NextWriteNotGranted)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    schema_version: u32,
    migration_id: String,
    source_root: String,
    target_root: String,
    manifest_path: String,
    #[serde(default)]
    workspace_root: String,
    #[serde(default)]
    legacy_user_home: Option<String>,
    #[serde(default)]
    next_user_home: Option<String>,
    #[serde(default)]
    capabilities: Vec<MigrationCapability>,
    items: Vec<MigrationItem>,
    warnings: Vec<String>,
}

impl MigrationPlan {
    pub fn migration_id(&self) -> &str {
        &self.migration_id
    }
    pub fn source_root(&self) -> &str {
        &self.source_root
    }
    pub fn target_root(&self) -> &str {
        &self.target_root
    }
    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }
    pub fn items(&self) -> &[MigrationItem] {
        &self.items
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    pub fn legacy_user_home(&self) -> Option<&str> {
        self.legacy_user_home.as_deref()
    }
    pub fn next_user_home(&self) -> Option<&str> {
        self.next_user_home.as_deref()
    }
    pub fn capabilities(&self) -> &[MigrationCapability] {
        &self.capabilities
    }
    pub fn ready_count(&self) -> usize {
        self.items.iter().filter(|item| item.is_ready()).count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationItem {
    id: String,
    #[serde(default = "default_session_capability")]
    capability: String,
    #[serde(default)]
    scope: MigrationScope,
    source: String,
    target: String,
    source_bytes: u64,
    source_fingerprint: u64,
    state: MigrationItemState,
}

impl MigrationItem {
    fn invalid(source: String, reason: String) -> Self {
        Self {
            id: String::new(),
            capability: "sessions".to_string(),
            scope: MigrationScope::Workspace,
            source,
            target: String::new(),
            source_bytes: 0,
            source_fingerprint: 0,
            state: MigrationItemState::Invalid { reason },
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn capability(&self) -> &str {
        &self.capability
    }
    pub fn scope(&self) -> MigrationScope {
        self.scope
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
    pub fn state(&self) -> &MigrationItemState {
        &self.state
    }
    fn is_ready(&self) -> bool {
        matches!(self.state, MigrationItemState::Ready)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationScope {
    #[default]
    Workspace,
    LegacyUserHome,
}

impl MigrationScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::LegacyUserHome => "legacy_user_home",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationCapability {
    capability: String,
    scope: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    item_count: usize,
}

impl MigrationCapability {
    fn not_found(capability: &str, scope: &str) -> Self {
        Self {
            capability: capability.to_string(),
            scope: scope.to_string(),
            status: "not_found".to_string(),
            reason: None,
            item_count: 0,
        }
    }

    fn unsupported(capability: &str, scope: &str, reason: &str) -> Self {
        Self {
            capability: capability.to_string(),
            scope: scope.to_string(),
            status: "unsupported".to_string(),
            reason: Some(reason.to_string()),
            item_count: 0,
        }
    }

    pub fn capability(&self) -> &str {
        &self.capability
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn item_count(&self) -> usize {
        self.item_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MigrationItemState {
    Ready,
    TargetExists,
    Invalid {
        reason: String,
    },
    Applied {
        target_bytes: u64,
        target_fingerprint: u64,
    },
    RolledBack,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationManifest {
    schema_version: u32,
    migration_id: String,
    state: MigrationState,
    created_at_millis: u128,
    updated_at_millis: u128,
    #[serde(default)]
    workspace_root: String,
    #[serde(default)]
    legacy_user_home: Option<String>,
    #[serde(default)]
    next_user_home: Option<String>,
    items: Vec<MigrationItem>,
}

impl MigrationManifest {
    pub fn migration_id(&self) -> &str {
        &self.migration_id
    }
    pub fn state(&self) -> &MigrationState {
        &self.state
    }
    pub fn items(&self) -> &[MigrationItem] {
        &self.items
    }

    fn from_plan(plan: &MigrationPlan) -> Self {
        let now = now_millis();
        Self {
            schema_version: MIGRATION_SCHEMA_VERSION,
            migration_id: plan.migration_id.clone(),
            state: MigrationState::Applying,
            created_at_millis: now,
            updated_at_millis: now,
            workspace_root: plan.workspace_root.clone(),
            legacy_user_home: plan.legacy_user_home.clone(),
            next_user_home: plan.next_user_home.clone(),
            items: plan.items.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MigrationState {
    Applying,
    Applied,
    RolledBack,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub copied: usize,
    pub skipped_existing: usize,
    pub invalid: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RollbackReport {
    pub removed: usize,
    pub preserved: usize,
    pub already_rolled_back: bool,
    pub warnings: Vec<String>,
}

enum CopyOutcome {
    Copied((u64, u64)),
    TargetAppeared,
}

#[derive(Debug)]
pub enum MigrationError {
    Storage(StorageError),
    LegacyReadNotGranted,
    NextWriteNotGranted,
    InvalidPlan(String),
    InvalidManifest(String),
    ManifestExists(PathBuf),
    ManifestMissing(PathBuf),
    SourceChanged(String),
    InvalidSource(String),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::LegacyReadNotGranted => {
                formatter.write_str("legacy session read access was not granted")
            }
            Self::NextWriteNotGranted => {
                formatter.write_str("Next session write access was not granted")
            }
            Self::InvalidPlan(message) => write!(formatter, "invalid migration plan: {message}"),
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid migration manifest: {message}")
            }
            Self::ManifestExists(path) => write!(
                formatter,
                "migration manifest already exists: {}",
                path.display()
            ),
            Self::ManifestMissing(path) => write!(
                formatter,
                "migration manifest was not found: {}",
                path.display()
            ),
            Self::SourceChanged(path) => {
                write!(formatter, "legacy source changed since plan: {path}")
            }
            Self::InvalidSource(message) => {
                write!(
                    formatter,
                    "legacy source could not be normalized: {message}"
                )
            }
            Self::Io { path, source } => write!(
                formatter,
                "migration I/O failed at {}: {source}",
                path.display()
            ),
            Self::Serialize(error) => write!(formatter, "migration serialization failed: {error}"),
        }
    }
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StorageError> for MigrationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn read_manifest(path: &Path) -> Result<MigrationManifest, MigrationError> {
    // A process can stop after the current manifest is moved to its backup
    // but before the replacement is renamed into place. Recover that durable
    // backup so an interrupted apply still has a rollback record.
    let manifest_path = if path.exists() {
        path.to_path_buf()
    } else {
        let backup = path.with_extension("json.bak");
        if backup.exists() {
            backup
        } else {
            return Err(MigrationError::ManifestMissing(path.to_path_buf()));
        }
    };
    let Some(bytes) = read_bounded_manifest(&manifest_path)? else {
        return Err(MigrationError::ManifestMissing(path.to_path_buf()));
    };
    let manifest: MigrationManifest =
        serde_json::from_slice(&bytes).map_err(MigrationError::Serialize)?;
    if manifest.schema_version != MIGRATION_SCHEMA_VERSION {
        return Err(MigrationError::InvalidManifest(
            "unsupported manifest schema version".to_string(),
        ));
    }
    Ok(manifest)
}

fn read_bounded_manifest(path: &Path) -> Result<Option<Vec<u8>>, MigrationError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MigrationError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MANIFEST_MAX_BYTES {
        return Err(MigrationError::InvalidManifest(
            "manifest exceeds size limit".to_string(),
        ));
    }
    fs::read(path)
        .map(Some)
        .map_err(|source| MigrationError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn write_new_manifest(path: &Path, manifest: &MigrationManifest) -> Result<(), MigrationError> {
    let parent = path.parent().ok_or_else(|| {
        MigrationError::InvalidManifest("manifest path has no parent".to_string())
    })?;
    fs::create_dir_all(parent).map_err(|source| MigrationError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(MigrationError::Serialize)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| MigrationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|source| MigrationError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn write_manifest(path: &Path, manifest: &MigrationManifest) -> Result<(), MigrationError> {
    let parent = path.parent().ok_or_else(|| {
        MigrationError::InvalidManifest("manifest path has no parent".to_string())
    })?;
    let temporary = parent.join(format!(
        ".manifest-{}-{}.tmp",
        process::id(),
        MIGRATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec_pretty(manifest).map_err(MigrationError::Serialize)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| MigrationError::Io {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ignored = fs::remove_file(&temporary);
        return Err(MigrationError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    let backup = path.with_extension("json.bak");
    if !path.exists() && backup.exists() {
        if let Err(source) = fs::rename(&backup, path) {
            let _ignored = fs::remove_file(&temporary);
            return Err(MigrationError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    if backup.exists() {
        fs::remove_file(&backup).map_err(|source| MigrationError::Io {
            path: backup.clone(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(path, &backup) {
        let _ignored = fs::remove_file(&temporary);
        return Err(MigrationError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    if let Err(source) = fs::rename(&temporary, path) {
        let _ignored = fs::rename(&backup, path);
        let _ignored = fs::remove_file(&temporary);
        return Err(MigrationError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    let _ignored = fs::remove_file(backup);
    Ok(())
}

fn ensure_workspace_path(workspace: &Path, path: &Path) -> Result<(), MigrationError> {
    ensure_contained_path(workspace, path)
}

fn ensure_home_path(path: &Path) -> Result<(), MigrationError> {
    if path.as_os_str().is_empty() {
        return Err(MigrationError::InvalidPlan(
            "user home path must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn ensure_contained_path(root: &Path, path: &Path) -> Result<(), MigrationError> {
    if path.strip_prefix(root).is_err() {
        return Err(MigrationError::InvalidPlan(format!(
            "path escapes root: {}",
            path.display()
        )));
    }
    if let Ok(canonical_root) = fs::canonicalize(root) {
        if let Ok(canonical) = fs::canonicalize(existing_ancestor(path)) {
            if canonical.strip_prefix(&canonical_root).is_err() {
                return Err(MigrationError::InvalidPlan(format!(
                    "canonical path escapes root: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn optional_home(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = canonical_or_absolute(left);
    let right = canonical_or_absolute(right);
    // A Next root may be a parent of the legacy root (for example when both
    // are test fixtures below one temporary directory). Only reject a target
    // root inside the legacy root, where apply could write into old state.
    right.starts_with(&left)
}

fn canonical_or_absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn validate_relative_path(relative: &str) -> Result<(), MigrationError> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(MigrationError::InvalidPlan(format!(
            "migration path must be a non-empty relative path: {relative}"
        )));
    }
    Ok(())
}

fn default_session_capability() -> String {
    "sessions".to_string()
}

fn capability_from_items(
    capability: &str,
    scope: &str,
    items: &[MigrationItem],
    item_capability: &str,
) -> MigrationCapability {
    capability_from_iter(
        capability,
        scope,
        items
            .iter()
            .filter(|item| item.capability == item_capability),
    )
}

fn capability_from_item_range(
    capability: &str,
    scope: &str,
    items: &[MigrationItem],
) -> MigrationCapability {
    capability_from_iter(capability, scope, items.iter())
}

fn capability_from_iter<'a, I>(capability: &str, scope: &str, items: I) -> MigrationCapability
where
    I: IntoIterator<Item = &'a MigrationItem>,
{
    let items = items.into_iter().collect::<Vec<_>>();
    let unsupported_reason = items.iter().find_map(|item| match &item.state {
        MigrationItemState::Invalid { reason } if reason.starts_with("unsupported: ") => {
            Some(reason.trim_start_matches("unsupported: ").to_string())
        }
        _ => None,
    });
    let status = if unsupported_reason.is_some() {
        "unsupported"
    } else if items.is_empty() {
        "not_found"
    } else if items
        .iter()
        .any(|item| matches!(item.state, MigrationItemState::Ready))
    {
        "ready"
    } else if items
        .iter()
        .all(|item| matches!(item.state, MigrationItemState::TargetExists))
    {
        "target_exists"
    } else {
        "invalid"
    };
    MigrationCapability {
        capability: capability.to_string(),
        scope: scope.to_string(),
        status: status.to_string(),
        reason: unsupported_reason,
        item_count: items.len(),
    }
}

fn looks_like_json_memory_file(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{')
}

fn derived_relationship_capability(
    scope: &str,
    relationship_count: usize,
    memory_present: bool,
    memory_unsupported: bool,
) -> MigrationCapability {
    if memory_unsupported {
        MigrationCapability::unsupported(
            "relationship",
            scope,
            "relationship graph cannot be derived until the legacy memory format passes privacy review",
        )
    } else if relationship_count > 0 {
        MigrationCapability {
            capability: "relationship".to_string(),
            scope: scope.to_string(),
            status: "derived".to_string(),
            reason: Some(
                "relationship graph records are derived from migrated memory JSONL".to_string(),
            ),
            item_count: relationship_count,
        }
    } else if memory_present {
        MigrationCapability {
            capability: "relationship".to_string(),
            scope: scope.to_string(),
            status: "not_found".to_string(),
            reason: Some(
                "no relationship records were present in the known memory files".to_string(),
            ),
            item_count: 0,
        }
    } else {
        MigrationCapability::not_found("relationship", scope)
    }
}

fn has_unsupported_memory(items: &[MigrationItem]) -> bool {
    items.iter().any(|item| {
        item.capability == "memory"
            && matches!(&item.state, MigrationItemState::Invalid { reason } if reason.starts_with("unsupported: "))
    })
}

struct NormalizedMemory {
    bytes: Vec<u8>,
    relationship_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MemoryScopeWire {
    GlobalUser,
    Workspace { root_fingerprint: String },
    AgentIdentity,
    Relationship,
}

impl MemoryScopeWire {
    fn label(&self) -> String {
        match self {
            Self::GlobalUser => "global_user".to_string(),
            Self::Workspace { root_fingerprint } => format!("workspace:{root_fingerprint}"),
            Self::AgentIdentity => "agent_identity".to_string(),
            Self::Relationship => "relationship".to_string(),
        }
    }

    fn is_relationship(&self) -> bool {
        matches!(self, Self::Relationship)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MemoryKindWire {
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

impl MemoryKindWire {
    fn storage_key(self) -> &'static str {
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

    fn is_relationship(self) -> bool {
        matches!(self, Self::RelationshipNote | Self::EmotionalState)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MemorySensitivityWire {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MemoryStatusWire {
    Active,
    #[default]
    Pending,
    Rejected,
    Archived,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MemoryLayerWire {
    Profile,
    Preference,
    Relationship,
    Workspace,
    Episode,
    ToolTrace,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct MemoryTemporalWire {
    #[serde(default)]
    observed_at_millis: u128,
    #[serde(default)]
    event_at_millis: Option<u128>,
    #[serde(default)]
    valid_from_millis: Option<u128>,
    #[serde(default)]
    expires_at_millis: Option<u128>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct MemoryInvalidationWire {
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    superseded_by: Option<String>,
    #[serde(default)]
    conflicts_with: Vec<String>,
    #[serde(default)]
    expires_reason: Option<String>,
    #[serde(default)]
    invalidated_at_millis: Option<u128>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyMemoryRecord {
    id: String,
    #[serde(default = "default_legacy_memory_schema")]
    schema_version: u32,
    scope: MemoryScopeWire,
    kind: MemoryKindWire,
    content: String,
    #[serde(default)]
    source_session_id: Option<String>,
    #[serde(default = "default_memory_confidence")]
    confidence: f32,
    #[serde(default = "default_memory_importance")]
    importance: f32,
    #[serde(default)]
    sensitivity: MemorySensitivityWire,
    #[serde(default)]
    status: MemoryStatusWire,
    #[serde(default)]
    created_at_millis: u128,
    #[serde(default)]
    updated_at_millis: u128,
    #[serde(default)]
    dedup_key: String,
    #[serde(default = "default_memory_revision")]
    revision: u32,
    #[serde(default = "default_memory_merged_count")]
    merged_count: u32,
    #[serde(default)]
    layer: MemoryLayerWire,
    #[serde(default)]
    temporal: MemoryTemporalWire,
    #[serde(default)]
    invalidation: MemoryInvalidationWire,
}

#[derive(Serialize)]
struct NormalizedMemoryRecord {
    id: String,
    schema_version: u32,
    scope: MemoryScopeWire,
    kind: MemoryKindWire,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_session_id: Option<String>,
    confidence: f32,
    importance: f32,
    sensitivity: MemorySensitivityWire,
    status: MemoryStatusWire,
    created_at_millis: u128,
    updated_at_millis: u128,
    dedup_key: String,
    revision: u32,
    merged_count: u32,
    layer: MemoryLayerWire,
    temporal: MemoryTemporalWire,
    invalidation: MemoryInvalidationWire,
}

fn normalize_memory_jsonl(bytes: &[u8]) -> Result<NormalizedMemory, String> {
    if bytes.len() as u64 > MAX_MEMORY_MIGRATION_FILE_BYTES {
        return Err(format!(
            "file exceeds {MAX_MEMORY_MIGRATION_FILE_BYTES} bytes"
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| format!("file is not UTF-8: {error}"))?;
    let mut output = Vec::with_capacity(bytes.len());
    let mut relationship_count = 0;
    let mut records = 0;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_MEMORY_MIGRATION_LINE_BYTES {
            return Err(format!(
                "line {} exceeds {MAX_MEMORY_MIGRATION_LINE_BYTES} bytes",
                index + 1
            ));
        }
        records += 1;
        if records > MAX_MEMORY_MIGRATION_RECORDS {
            return Err(format!(
                "file contains more than {MAX_MEMORY_MIGRATION_RECORDS} records"
            ));
        }
        let record = serde_json::from_str::<LegacyMemoryRecord>(line)
            .map_err(|error| format!("line {} is invalid JSON memory: {error}", index + 1))?;
        let (record, relationship) = normalize_memory_record(record)
            .map_err(|error| format!("line {}: {error}", index + 1))?;
        if relationship {
            relationship_count += 1;
        }
        let encoded = serde_json::to_vec(&record)
            .map_err(|error| format!("line {} could not be encoded: {error}", index + 1))?;
        if encoded.len() > MAX_MEMORY_MIGRATION_LINE_BYTES {
            return Err(format!(
                "normalized line {} exceeds {MAX_MEMORY_MIGRATION_LINE_BYTES} bytes",
                index + 1
            ));
        }
        output.extend_from_slice(&encoded);
        output.push(b'\n');
    }
    Ok(NormalizedMemory {
        bytes: output,
        relationship_count,
    })
}

fn normalize_memory_record(
    mut record: LegacyMemoryRecord,
) -> Result<(NormalizedMemoryRecord, bool), String> {
    if record.schema_version == 0 {
        record.schema_version = 1;
    }
    if record.schema_version > 3 {
        return Err(format!(
            "unsupported future schema_version={}",
            record.schema_version
        ));
    }
    validate_memory_text("id", &record.id, 256, false)?;
    validate_memory_text("content", &record.content, 64 * 1024, true)?;
    if !record.confidence.is_finite() || !record.importance.is_finite() {
        return Err("confidence and importance must be finite".to_string());
    }
    record.confidence = record.confidence.clamp(0.0, 1.0);
    record.importance = record.importance.clamp(0.0, 1.0);
    if let Some(source_session_id) = &record.source_session_id {
        validate_memory_text("source_session_id", source_session_id, 256, false)?;
    }
    if let MemoryScopeWire::Workspace { root_fingerprint } = &record.scope {
        validate_memory_text("scope.root_fingerprint", root_fingerprint, 256, false)?;
    }
    if record.updated_at_millis == 0 {
        record.updated_at_millis = record.created_at_millis;
    }
    if record.temporal.observed_at_millis == 0 {
        record.temporal.observed_at_millis = record.created_at_millis;
    }
    if record.temporal.valid_from_millis.is_none() && record.created_at_millis != 0 {
        record.temporal.valid_from_millis = Some(record.created_at_millis);
    }
    if record.revision == 0 {
        record.revision = 1;
    }
    if record.merged_count == 0 {
        record.merged_count = 1;
    }
    if record.layer == MemoryLayerWire::Unknown {
        record.layer = memory_layer_for_kind(record.kind);
    }
    for reference in record
        .invalidation
        .supersedes
        .iter()
        .chain(record.invalidation.conflicts_with.iter())
    {
        validate_memory_text("invalidation reference", reference, 256, false)?;
    }
    if let Some(reference) = &record.invalidation.superseded_by {
        validate_memory_text("invalidation superseded_by", reference, 256, false)?;
    }
    if let Some(reason) = &record.invalidation.expires_reason {
        validate_memory_text("invalidation expires_reason", reason, 1024, true)?;
    }
    if record.invalidation.supersedes.len() > 256 || record.invalidation.conflicts_with.len() > 256
    {
        return Err("invalidation reference count exceeds 256".to_string());
    }
    let dedup_key = if record.dedup_key.trim().is_empty() {
        format!(
            "{}|{}|{}",
            record.scope.label(),
            record.kind.storage_key(),
            normalize_memory_content(&record.content)
        )
    } else {
        validate_memory_text("dedup_key", &record.dedup_key, 1024, true)?;
        record.dedup_key
    };
    let relationship = record.scope.is_relationship() || record.kind.is_relationship();
    Ok((
        NormalizedMemoryRecord {
            id: record.id,
            schema_version: 3,
            scope: record.scope,
            kind: record.kind,
            content: record.content,
            source_session_id: record.source_session_id,
            confidence: record.confidence,
            importance: record.importance,
            sensitivity: record.sensitivity,
            status: record.status,
            created_at_millis: record.created_at_millis,
            updated_at_millis: record.updated_at_millis,
            dedup_key,
            revision: record.revision,
            merged_count: record.merged_count,
            layer: record.layer,
            temporal: record.temporal,
            invalidation: record.invalidation,
        },
        relationship,
    ))
}

fn validate_memory_text(
    field: &str,
    value: &str,
    maximum: usize,
    allow_whitespace: bool,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().count() > maximum {
        return Err(format!("{field} exceeds {maximum} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} contains a control character"));
    }
    if !allow_whitespace && value.chars().any(char::is_whitespace) {
        return Err(format!("{field} contains whitespace"));
    }
    Ok(())
}

fn normalize_memory_content(content: &str) -> String {
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

fn memory_layer_for_kind(kind: MemoryKindWire) -> MemoryLayerWire {
    match kind {
        MemoryKindWire::Preference => MemoryLayerWire::Preference,
        MemoryKindWire::PersonalFact => MemoryLayerWire::Profile,
        MemoryKindWire::RelationshipNote | MemoryKindWire::EmotionalState => {
            MemoryLayerWire::Relationship
        }
        MemoryKindWire::ProjectContext | MemoryKindWire::Correction => MemoryLayerWire::Workspace,
        MemoryKindWire::Goal | MemoryKindWire::Event => MemoryLayerWire::Episode,
        MemoryKindWire::ToolTraceSummary => MemoryLayerWire::ToolTrace,
    }
}

fn is_cjk(character: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&character)
        || ('\u{3400}'..='\u{4dbf}').contains(&character)
        || ('\u{f900}'..='\u{faff}').contains(&character)
}

fn default_legacy_memory_schema() -> u32 {
    1
}

fn default_memory_confidence() -> f32 {
    0.75
}

fn default_memory_importance() -> f32 {
    0.5
}

fn default_memory_revision() -> u32 {
    1
}

fn default_memory_merged_count() -> u32 {
    1
}

fn read_bounded_with_limit(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, MigrationError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MigrationError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > maximum {
        return Err(MigrationError::Storage(StorageError::FileTooLarge {
            path: path.to_path_buf(),
            maximum,
        }));
    }
    fs::read(path)
        .map(Some)
        .map_err(|source| MigrationError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn existing_ancestor(path: &Path) -> &Path {
    let mut candidate = path;
    while !candidate.exists() {
        let Some(parent) = candidate.parent() else {
            break;
        };
        candidate = parent;
    }
    candidate
}

fn relative_path(workspace: &Path, path: &Path) -> Result<String, MigrationError> {
    ensure_workspace_path(workspace, path)?;
    path.strip_prefix(workspace)
        .map_err(|_| MigrationError::InvalidPlan("path is outside workspace".to_string()))
        .map(|value| value.to_string_lossy().replace('\\', "/"))
}

fn validate_migration_id(id: &str) -> Result<(), MigrationError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(MigrationError::InvalidPlan(
            "migration id contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

fn new_migration_id() -> String {
    format!(
        "sessions-{}-{}-{}",
        now_millis(),
        process::id(),
        MIGRATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

// Stable, non-cryptographic content identity for rollback/change detection.
fn fingerprint(bytes: &[u8]) -> u64 {
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

    use super::*;

    #[test]
    fn plan_is_read_only_and_apply_preserves_legacy() {
        let root = test_root("plan-apply");
        let legacy = root.join(".yunxi/sessions/legacy.json");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy root");
        let source = br#"{"id":"legacy","cwd":".","prompt":"hello","final_response":"reply","created_at_millis":1}"#;
        fs::write(&legacy, source).expect("legacy session");
        let migration =
            SessionMigration::from_grant(&WorkspaceGrant::read_write(&root)).expect("migration");
        let plan = migration.plan().expect("plan");
        assert_eq!(plan.ready_count(), 1);
        assert!(!root.join(".yunxi-next").exists());
        let report = migration.apply(&plan).expect("apply");
        assert_eq!(report.copied, 1);
        assert_eq!(fs::read(&legacy).expect("source bytes"), source);
        assert!(root.join(".yunxi-next/sessions/legacy.json").is_file());
        let manifest = root.join(format!(
            ".yunxi-next/migrations/sessions/{}.json",
            plan.migration_id()
        ));
        assert!(manifest.is_file());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn apply_never_overwrites_existing_next_data() {
        let root = test_root("conflict");
        let legacy = root.join(".yunxi/sessions/legacy.json");
        let next = root.join(".yunxi-next/sessions/legacy.json");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy root");
        fs::create_dir_all(next.parent().expect("next parent")).expect("next root");
        fs::write(&legacy, br#"{"id":"legacy","cwd":".","prompt":"old"}"#).expect("legacy");
        fs::write(&next, b"existing-next-data").expect("next");
        let migration =
            SessionMigration::from_grant(&WorkspaceGrant::read_write(&root)).expect("migration");
        let plan = migration.plan().expect("plan");
        assert_eq!(plan.ready_count(), 0);
        let report = migration.apply(&plan).expect("apply");
        assert_eq!(report.skipped_existing, 1);
        assert_eq!(fs::read(next).expect("next bytes"), b"existing-next-data");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rollback_removes_only_unchanged_outputs() {
        let root = test_root("rollback");
        let legacy = root.join(".yunxi/sessions/legacy.json");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy root");
        fs::write(&legacy, br#"{"id":"legacy","cwd":".","prompt":"old"}"#).expect("legacy");
        let migration =
            SessionMigration::from_grant(&WorkspaceGrant::read_write(&root)).expect("migration");
        let plan = migration.plan().expect("plan");
        migration.apply(&plan).expect("apply");
        let target = root.join(".yunxi-next/sessions/legacy.json");
        fs::write(&target, b"user changed this").expect("edit target");
        let report = migration.rollback(plan.migration_id()).expect("rollback");
        assert_eq!(report.removed, 0);
        assert_eq!(report.preserved, 1);
        assert!(target.is_file());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn read_only_grant_can_plan_but_cannot_apply_or_rollback() {
        let root = test_root("grants");
        let legacy = root.join(".yunxi/sessions/legacy.json");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy root");
        fs::write(&legacy, br#"{"id":"legacy","cwd":".","prompt":"old"}"#).expect("legacy");
        let migration =
            SessionMigration::from_grant(&WorkspaceGrant::read_only(&root)).expect("migration");
        let plan = migration.plan().expect("plan");
        assert!(matches!(
            migration.apply(&plan),
            Err(MigrationError::NextWriteNotGranted)
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rollback_recovers_manifest_left_in_backup_after_interrupted_replace() {
        let root = test_root("manifest-backup-recovery");
        let legacy = root.join(".yunxi/sessions/legacy.json");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy root");
        fs::write(&legacy, br#"{"id":"legacy","cwd":".","prompt":"old"}"#).expect("legacy");
        let migration =
            SessionMigration::from_grant(&WorkspaceGrant::read_write(&root)).expect("migration");
        let plan = migration.plan().expect("plan");
        migration.apply(&plan).expect("apply");
        let manifest = root.join(format!(
            ".yunxi-next/migrations/sessions/{}.json",
            plan.migration_id()
        ));
        let backup = manifest.with_extension("json.bak");
        fs::rename(&manifest, &backup).expect("simulate interrupted manifest replace");

        let report = migration
            .rollback(plan.migration_id())
            .expect("rollback from backup");
        assert_eq!(report.removed, 1);
        assert!(!root.join(".yunxi-next/sessions/legacy.json").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn user_home_files_are_scoped_and_source_fingerprint_is_required() {
        let root = test_root("user-home");
        let legacy = root.join("legacy-home");
        let next = root.join("next-home");
        fs::create_dir_all(legacy.join("persona")).expect("legacy persona");
        fs::write(legacy.join("persona/soul.txt"), b"original").expect("legacy soul");
        fs::write(legacy.join("settings.json"), b"{\"version\":1}").expect("legacy controls");
        let migration = SessionMigration::from_grant_with_homes(
            &WorkspaceGrant::read_write(&root),
            Some(&legacy),
            Some(&next),
        )
        .expect("migration");
        let plan = migration.plan().expect("plan");
        assert!(plan.items().iter().any(|item| {
            item.capability() == "persona"
                && item.scope() == MigrationScope::LegacyUserHome
                && item.source() == "persona/soul.txt"
        }));
        assert_eq!(
            plan.capabilities()
                .iter()
                .find(|capability| capability.capability() == "memory")
                .expect("memory capability")
                .status(),
            "not_found"
        );
        fs::write(legacy.join("persona/soul.txt"), b"changed after plan")
            .expect("change legacy soul");
        let result = migration.apply(&plan);
        assert!(
            matches!(result, Err(MigrationError::SourceChanged(_))),
            "expected source fingerprint failure, got {result:?}"
        );
        assert!(!next.join("persona/soul.txt").exists());
        assert_eq!(
            fs::read(legacy.join("persona/soul.txt")).expect("legacy bytes"),
            b"changed after plan"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn missing_user_home_is_explicitly_unsupported_without_writing() {
        let root = test_root("user-home-disabled");
        let migration =
            SessionMigration::from_grant_with_homes(&WorkspaceGrant::read_write(&root), None, None)
                .expect("migration");
        let plan = migration.plan().expect("plan");
        assert_eq!(plan.legacy_user_home(), None);
        for name in ["persona", "memory", "controls", "mailbox", "relationship"] {
            let capability = plan
                .capabilities()
                .iter()
                .find(|capability| capability.capability() == name)
                .expect("capability");
            assert_eq!(capability.status(), "unsupported");
            assert_eq!(capability.scope(), "legacy_user_home");
        }
        assert!(!root.join(".yunxi-next").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn memory_jsonl_is_normalized_and_relationships_are_reported() {
        let root = test_root("memory");
        let source_path = root.join(".yunxi/memory/workspace-memory.jsonl");
        fs::create_dir_all(source_path.parent().expect("memory parent")).expect("memory root");
        let first = json!({
            "id": "legacy-preference",
            "schema_version": 1,
            "scope": "global_user",
            "kind": "preference",
            "content": "用户偏好使用中文回答。",
            "confidence": 0.9,
            "importance": 0.7,
            "sensitivity": "low",
            "status": "active",
            "created_at_millis": 1,
            "updated_at_millis": 2,
            "entities": [{"id": "ignored-by-next"}]
        });
        let relationship = json!({
            "id": "legacy-relationship",
            "schema_version": 2,
            "scope": "relationship",
            "kind": "relationship_note",
            "content": "用户希望保持直接沟通。",
            "confidence": 0.8,
            "importance": 0.6,
            "sensitivity": "low",
            "status": "active",
            "created_at_millis": 3,
            "updated_at_millis": 3
        });
        let source = format!("{}\n{}\n", first, relationship);
        fs::write(&source_path, &source).expect("write memory source");

        let migration =
            SessionMigration::from_grant_with_homes(&WorkspaceGrant::read_write(&root), None, None)
                .expect("migration");
        let plan = migration.plan().expect("plan");
        let item = plan
            .items()
            .iter()
            .find(|item| item.capability() == "memory")
            .expect("memory item");
        assert_eq!(item.state(), &MigrationItemState::Ready);
        assert_eq!(
            plan.capabilities()
                .iter()
                .find(|capability| {
                    capability.capability() == "relationship" && capability.scope() == "workspace"
                })
                .expect("relationship capability")
                .status(),
            "derived"
        );
        let report = migration.apply(&plan).expect("apply");
        assert_eq!(report.copied, 1);
        assert_eq!(
            fs::read(&source_path).expect("source bytes"),
            source.as_bytes()
        );

        let target_path = root.join(".yunxi-next/memory/workspace-memory.jsonl");
        let target = fs::read_to_string(&target_path).expect("normalized target");
        let values = target
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("target JSON"))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values.iter().all(|value| value["schema_version"] == 3));
        assert!(values.iter().all(|value| value.get("entities").is_none()));
        assert!(values.iter().any(|value| value["scope"] == "relationship"));

        let rollback = migration.rollback(plan.migration_id()).expect("rollback");
        assert_eq!(rollback.removed, 1);
        assert!(!target_path.exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn invalid_memory_jsonl_is_reported_without_partial_output() {
        let root = test_root("memory-invalid");
        let source_path = root.join(".yunxi/memory/global-memory.jsonl");
        fs::create_dir_all(source_path.parent().expect("memory parent")).expect("memory root");
        let source = r#"{"id":"valid","scope":"global_user","kind":"preference","content":"中文","status":"active"}
not-json
"#
            .as_bytes();
        fs::write(&source_path, source).expect("write invalid memory source");
        let migration =
            SessionMigration::from_grant_with_homes(&WorkspaceGrant::read_write(&root), None, None)
                .expect("migration");
        let plan = migration.plan().expect("plan");
        assert!(plan.items().iter().any(|item| {
            item.capability() == "memory"
                && matches!(item.state(), MigrationItemState::Invalid { .. })
        }));
        let report = migration.apply(&plan).expect("apply reports invalid item");
        assert_eq!(report.invalid, 1);
        assert!(!root.join(".yunxi-next/memory/global-memory.jsonl").exists());
        assert_eq!(fs::read(&source_path).expect("source bytes"), source);
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "yunxi-storage-migration-{label}-{}-{}",
            process::id(),
            now_millis()
        ));
        fs::create_dir_all(&root).expect("create test root");
        root
    }
}
