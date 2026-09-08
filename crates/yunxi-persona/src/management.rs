//! Bounded user-facing persona management.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use yunxi_protocol::WorkspaceGrant;

use crate::profile::{
    DEFAULT_PROFILE_ID, PersonaProfile, load_active, load_active_with_roots, profile_paths,
    profile_paths_with_roots,
};
use crate::settings::{
    PersonaRoots, PersonaSettings, atomic_replace, save, save_with_grant, yunxi_next_home_dir,
};

const MAX_PROFILE_IMPORT_BYTES: u64 = 512 * 1024;
const MAX_PROFILE_COUNT: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaProfileSummary {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub authoritative_soul: bool,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaStatus {
    pub enabled: bool,
    pub active_profile: String,
    pub active_profile_summary: PersonaProfileSummary,
    pub profiles: Vec<PersonaProfileSummary>,
    pub warnings: Vec<String>,
    pub settings_path: String,
}

pub fn status() -> PersonaStatus {
    status_with_roots(&PersonaRoots::from_environment())
}

pub fn status_with_grant(grant: &WorkspaceGrant) -> PersonaStatus {
    status_with_roots(&PersonaRoots::from_grant(grant))
}

fn status_with_roots(roots: &PersonaRoots) -> PersonaStatus {
    let loaded = PersonaSettings::load_from_roots_for_management(roots);
    let active = load_active_with_roots(&loaded.settings.active_profile, roots);
    let active_summary = summary(
        &active.profile,
        source_for_with_roots(&active.profile.id, roots),
    );
    let (profiles, mut warnings) = collect_profiles_with_roots(roots);
    warnings.extend(loaded.warnings);
    warnings.extend(active.warnings);
    PersonaStatus {
        enabled: loaded.settings.persona_enabled,
        active_profile: loaded.settings.active_profile,
        active_profile_summary: active_summary,
        profiles,
        warnings: bounded_warnings(warnings),
        settings_path: roots
            .next_persona_root()
            .join("config.toml")
            .to_string_lossy()
            .into_owned(),
    }
}

pub fn list_profiles() -> Vec<PersonaProfileSummary> {
    collect_profiles().0
}

pub fn list_profiles_with_grant(grant: &WorkspaceGrant) -> Vec<PersonaProfileSummary> {
    collect_profiles_with_roots(&PersonaRoots::from_grant(grant)).0
}

pub fn profile_with_grant(
    grant: &WorkspaceGrant,
    requested_id: Option<&str>,
) -> Option<PersonaProfileSummary> {
    let status = status_with_grant(grant);
    let id = requested_id.unwrap_or(&status.active_profile);
    status.profiles.into_iter().find(|profile| profile.id == id)
}

pub fn set_enabled(enabled: bool) -> Result<PersonaStatus, PersonaManagementError> {
    let loaded = PersonaSettings::load();
    let settings = PersonaSettings {
        persona_enabled: enabled,
        active_profile: loaded.settings.active_profile,
    };
    save(&settings).map_err(PersonaManagementError::Write)?;
    Ok(status())
}

pub fn set_enabled_with_grant(
    grant: &WorkspaceGrant,
    enabled: bool,
) -> Result<PersonaStatus, PersonaManagementError> {
    require_write(grant)?;
    let loaded = PersonaSettings::load_from_grant(grant);
    let settings = PersonaSettings {
        persona_enabled: enabled,
        active_profile: loaded.settings.active_profile,
    };
    save_with_grant(&settings, grant).map_err(PersonaManagementError::Write)?;
    Ok(status_with_grant(grant))
}

pub fn set_active_profile(id: &str) -> Result<PersonaStatus, PersonaManagementError> {
    let loaded = PersonaSettings::load();
    let profile = load_active(id);
    if profile.profile.id != id {
        return Err(PersonaManagementError::ProfileNotFound(id.to_string()));
    }
    let settings = PersonaSettings {
        persona_enabled: loaded.settings.persona_enabled,
        active_profile: id.to_string(),
    };
    save(&settings).map_err(PersonaManagementError::Write)?;
    Ok(status())
}

pub fn set_active_profile_with_grant(
    grant: &WorkspaceGrant,
    id: &str,
) -> Result<PersonaStatus, PersonaManagementError> {
    require_write(grant)?;
    let roots = PersonaRoots::from_grant(grant);
    let loaded = PersonaSettings::load_from_grant(grant);
    let profile = load_active_with_roots(id, &roots);
    if profile.profile.id != id {
        return Err(PersonaManagementError::ProfileNotFound(id.to_string()));
    }
    let settings = PersonaSettings {
        persona_enabled: loaded.settings.persona_enabled,
        active_profile: id.to_string(),
    };
    save_with_grant(&settings, grant).map_err(PersonaManagementError::Write)?;
    Ok(status_with_grant(grant))
}

pub fn import_profile(path: &Path) -> Result<PersonaProfileSummary, PersonaManagementError> {
    let metadata = fs::metadata(path).map_err(|error| PersonaManagementError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if !metadata.is_file() {
        return Err(PersonaManagementError::Read {
            path: path.to_path_buf(),
            message: "profile path is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_PROFILE_IMPORT_BYTES {
        return Err(PersonaManagementError::TooLarge {
            maximum: MAX_PROFILE_IMPORT_BYTES,
        });
    }
    let content = fs::read(path).map_err(|error| PersonaManagementError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let profile: PersonaProfile = serde_json::from_slice(&content)
        .map_err(|error| PersonaManagementError::Invalid(error.to_string()))?;
    profile
        .validate()
        .map_err(PersonaManagementError::Invalid)?;
    let target = yunxi_next_home_dir()
        .join("persona")
        .join("profiles")
        .join(format!("{}.json", profile.id));
    let mut encoded = serde_json::to_vec_pretty(&profile)
        .map_err(|error| PersonaManagementError::Invalid(error.to_string()))?;
    encoded.push(b'\n');
    atomic_replace(&target, &encoded).map_err(PersonaManagementError::Write)?;
    Ok(summary(&profile, "next"))
}

pub fn import_profile_json_with_grant(
    grant: &WorkspaceGrant,
    content: &str,
) -> Result<PersonaProfileSummary, PersonaManagementError> {
    require_write(grant)?;
    if content.is_empty() || content.len() as u64 > MAX_PROFILE_IMPORT_BYTES {
        return Err(PersonaManagementError::TooLarge {
            maximum: MAX_PROFILE_IMPORT_BYTES,
        });
    }
    let profile: PersonaProfile = serde_json::from_str(content)
        .map_err(|error| PersonaManagementError::Invalid(error.to_string()))?;
    profile
        .validate()
        .map_err(PersonaManagementError::Invalid)?;
    let roots = PersonaRoots::from_grant(grant);
    let target = roots
        .next_persona_root()
        .join("profiles")
        .join(format!("{}.json", profile.id));
    let mut encoded = serde_json::to_vec_pretty(&profile)
        .map_err(|error| PersonaManagementError::Invalid(error.to_string()))?;
    encoded.push(b'\n');
    atomic_replace(&target, &encoded).map_err(PersonaManagementError::Write)?;
    Ok(summary(&profile, "next"))
}

pub fn reset_with_grant(grant: &WorkspaceGrant) -> Result<PersonaStatus, PersonaManagementError> {
    require_write(grant)?;
    save_with_grant(&PersonaSettings::default(), grant).map_err(PersonaManagementError::Write)?;
    Ok(status_with_grant(grant))
}

fn collect_profiles() -> (Vec<PersonaProfileSummary>, Vec<String>) {
    collect_profiles_with_roots(&PersonaRoots::from_environment())
}

fn collect_profiles_with_roots(roots: &PersonaRoots) -> (Vec<PersonaProfileSummary>, Vec<String>) {
    let mut profiles = vec![summary(&crate::profile::default_profile(), "built-in")];
    let mut warnings = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    ids.insert(DEFAULT_PROFILE_ID.to_string());
    let profile_roots = std::iter::once(roots.next_persona_root().join("profiles")).chain(
        roots
            .legacy_persona_root()
            .map(|root| root.join("profiles")),
    );
    for root in profile_roots {
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!(
                    "failed to read persona profile directory {}: {error}",
                    root.display()
                ));
                continue;
            }
        };
        for entry in entries.flatten().take(MAX_PROFILE_COUNT) {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !ids.insert(id.to_string()) {
                continue;
            }
            match load_profile_file_for_management(id, &path) {
                Ok(profile) => profiles.push(summary(
                    &profile,
                    if path.starts_with(roots.next_persona_root()) {
                        "next"
                    } else {
                        "legacy"
                    },
                )),
                Err(error) => warnings.push(format!("profile `{id}` was skipped: {error}")),
            }
        }
    }
    profiles.sort_by(|left, right| left.id.cmp(&right.id));
    (profiles, bounded_warnings(warnings))
}

fn load_profile_file_for_management(id: &str, path: &Path) -> Result<PersonaProfile, String> {
    let _ = profile_paths(id);
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_PROFILE_IMPORT_BYTES {
        return Err("profile exceeds the 512 KiB limit".to_string());
    }
    let profile: PersonaProfile =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    profile.validate()?;
    if profile.id != id {
        return Err("profile id does not match its filename".to_string());
    }
    Ok(profile)
}

fn summary(profile: &PersonaProfile, source: &str) -> PersonaProfileSummary {
    PersonaProfileSummary {
        id: profile.id.clone(),
        display_name: profile.display_name.clone(),
        version: profile.version.clone(),
        authoritative_soul: profile.authoritative_soul,
        source: source.to_string(),
    }
}

fn source_for_with_roots(id: &str, roots: &PersonaRoots) -> &'static str {
    if id == DEFAULT_PROFILE_ID {
        "built-in"
    } else if profile_paths_with_roots(id, roots)
        .first()
        .is_some_and(|path| path.is_file())
    {
        "next"
    } else {
        "legacy"
    }
}

fn bounded_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.truncate(32);
    warnings
}

fn require_write(grant: &WorkspaceGrant) -> Result<(), PersonaManagementError> {
    if grant.allows_next_write() {
        Ok(())
    } else {
        Err(PersonaManagementError::WriteNotGranted)
    }
}

#[derive(Debug)]
pub enum PersonaManagementError {
    Read { path: PathBuf, message: String },
    Write(String),
    Invalid(String),
    TooLarge { maximum: u64 },
    ProfileNotFound(String),
    WriteNotGranted,
}

impl fmt::Display for PersonaManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, message } => {
                write!(formatter, "failed to read {}: {message}", path.display())
            }
            Self::Write(message) => {
                write!(formatter, "failed to write persona settings: {message}")
            }
            Self::Invalid(message) => write!(formatter, "invalid persona profile: {message}"),
            Self::TooLarge { maximum } => {
                write!(formatter, "persona profile exceeds {maximum} bytes")
            }
            Self::ProfileNotFound(id) => write!(formatter, "persona profile `{id}` was not found"),
            Self::WriteNotGranted => {
                formatter.write_str("persona mutation requires a Next write grant")
            }
        }
    }
}

impl Error for PersonaManagementError {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn explicit_grant_state_root_controls_persona_management_reads() {
        let sequence = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "yunxi-persona-management-{}-{sequence}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let state_root = root.join("state");
        let persona_root = state_root.join("persona");
        fs::create_dir_all(persona_root.join("profiles")).expect("create persona roots");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(
            persona_root.join("config.toml"),
            "persona_enabled = true\nactive_profile = \"granted-profile\"\n",
        )
        .expect("write persona settings");
        fs::write(
            persona_root.join("profiles").join("granted-profile.json"),
            concat!(
                "{\"id\":\"granted-profile\",\"display_name\":\"Granted Profile\",",
                "\"version\":\"1\",\"layers\":{}}"
            ),
        )
        .expect("write persona profile");

        let grant = WorkspaceGrant::read_only(&workspace).with_state_root(&state_root);
        let status = status_with_grant(&grant);
        assert_eq!(status.active_profile, "granted-profile");
        assert_eq!(status.active_profile_summary.id, "granted-profile");
        assert_eq!(
            PathBuf::from(status.settings_path),
            persona_root.join("config.toml")
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }
}
