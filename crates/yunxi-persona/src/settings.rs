//! Legacy persona settings path and environment compatibility.

use std::fs;
use std::path::PathBuf;

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
        let path = yunxi_home_dir().join("persona").join("config.toml");
        let mut warnings = Vec::new();
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
        if let Ok(value) = std::env::var("YUNXI_PERSONA_ENABLED") {
            settings.persona_enabled = env_bool(&value, settings.persona_enabled);
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
