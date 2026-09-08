//! Legacy persona settings path and environment compatibility.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use yunxi_protocol::WorkspaceGrant;

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersonaSettings {
    pub(crate) persona_enabled: bool,
    pub(crate) active_profile: String,
}

impl Default for PersonaSettings {
    fn default() -> Self {
        Self {
            persona_enabled: true,
            active_profile: "yunxi_companion_strong".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LoadedSettings {
    pub(crate) settings: PersonaSettings,
    pub(crate) warnings: Vec<String>,
}

impl PersonaSettings {
    pub(crate) fn load() -> LoadedSettings {
        Self::load_from_roots(&PersonaRoots::from_environment(), true)
    }

    pub(crate) fn load_from_grant(grant: &WorkspaceGrant) -> LoadedSettings {
        Self::load_from_roots(&PersonaRoots::from_grant(grant), false)
    }

    pub(crate) fn load_from_roots_for_management(roots: &PersonaRoots) -> LoadedSettings {
        Self::load_from_roots(roots, false)
    }

    fn load_from_roots(roots: &PersonaRoots, apply_environment_override: bool) -> LoadedSettings {
        let mut warnings = Vec::new();
        let next_path = roots.next_persona_root().join("config.toml");
        let legacy_path = roots
            .legacy_persona_root()
            .map(|root| root.join("config.toml"));
        let using_legacy = !next_path.exists()
            && legacy_path
                .as_ref()
                .is_some_and(|legacy_path| legacy_path.exists());
        let path = if next_path.exists() {
            next_path
        } else {
            legacy_path.unwrap_or(next_path)
        };
        let mut settings = match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_SETTINGS_BYTES => {
                warnings.push(format!(
                    "persona settings {} exceed {MAX_SETTINGS_BYTES} bytes; defaults were used",
                    path.display()
                ));
                Self::default()
            }
            Ok(_) => match fs::read_to_string(&path) {
                Ok(content) => Self::parse_lossy(&content),
                Err(error) => {
                    warnings.push(format!(
                        "failed to read persona settings {}: {error}; defaults were used",
                        path.display()
                    ));
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                warnings.push(format!(
                    "failed to inspect persona settings {}: {error}; defaults were used",
                    path.display()
                ));
                Self::default()
            }
        };
        if apply_environment_override && let Ok(value) = std::env::var("YUNXI_PERSONA_ENABLED") {
            settings.persona_enabled = env_bool(&value, settings.persona_enabled);
        }
        if using_legacy {
            warnings.push(
                "persona settings were read from the legacy location; writes use the Next location"
                    .to_string(),
            );
        }
        LoadedSettings { settings, warnings }
    }

    pub(crate) fn parse_lossy(content: &str) -> Self {
        let mut settings = Self::default();
        for line in content.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "persona_enabled" => {
                    settings.persona_enabled = env_bool(value, settings.persona_enabled);
                }
                "active_profile" if !value.is_empty() => {
                    settings.active_profile = value.to_string();
                }
                _ => {}
            }
        }
        settings
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersonaRoots {
    next_home: PathBuf,
    legacy_home: Option<PathBuf>,
}

impl PersonaRoots {
    pub(crate) fn from_environment() -> Self {
        Self {
            next_home: yunxi_next_home_dir(),
            legacy_home: Some(yunxi_home_dir()),
        }
    }

    pub(crate) fn from_grant(grant: &WorkspaceGrant) -> Self {
        let next_home = grant
            .state_root()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| grant.root().join(".yunxi-next"));
        Self {
            next_home,
            legacy_home: grant.allows_legacy_read().then(yunxi_home_dir),
        }
    }

    pub(crate) fn next_persona_root(&self) -> PathBuf {
        self.next_home.join("persona")
    }

    pub(crate) fn legacy_persona_root(&self) -> Option<PathBuf> {
        self.legacy_home.as_ref().map(|home| home.join("persona"))
    }
}

pub(crate) fn save(settings: &PersonaSettings) -> Result<PathBuf, String> {
    save_to_path(
        settings,
        &yunxi_next_home_dir().join("persona").join("config.toml"),
    )
}

pub(crate) fn save_with_grant(
    settings: &PersonaSettings,
    grant: &WorkspaceGrant,
) -> Result<PathBuf, String> {
    if !grant.allows_next_write() {
        return Err("persona mutation requires a Next write grant".to_string());
    }
    let roots = PersonaRoots::from_grant(grant);
    save_to_path(settings, &roots.next_persona_root().join("config.toml"))
}

fn save_to_path(settings: &PersonaSettings, path: &Path) -> Result<PathBuf, String> {
    if settings.active_profile.is_empty()
        || settings.active_profile.len() > 64
        || !settings
            .active_profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("active profile id is invalid".to_string());
    }
    let content = format!(
        "persona_enabled = {}\nactive_profile = \"{}\"\n",
        settings.persona_enabled, settings.active_profile
    );
    atomic_replace(path, content.as_bytes())?;
    Ok(path.to_path_buf())
}

pub(crate) fn yunxi_home_dir() -> PathBuf {
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

pub(crate) fn yunxi_next_home_dir() -> PathBuf {
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

pub(crate) fn atomic_replace(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "settings path has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".yunxi-persona-{}-{stamp}.tmp", process::id()));
    let backup = path.with_extension("toml.bak");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    if let Err(error) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    drop(file);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| error.to_string())?;
        }
        fs::rename(path, &backup).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn env_bool(value: &str, default: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_keep_profile_and_persona_toggle() {
        let settings = PersonaSettings::parse_lossy(
            "persona_enabled = false\nmemory_enabled = true\nactive_profile = \"custom\"\n",
        );
        assert!(!settings.persona_enabled);
        assert_eq!(settings.active_profile, "custom");
    }
}
