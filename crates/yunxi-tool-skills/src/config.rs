//! Explicit, bounded configuration for the Skills child process.

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

pub const SKILLS_ROOT_ENV: &str = "YUNXI_NEXT_SKILLS_ROOT";
pub const SKILLS_DISABLED_ENV: &str = "YUNXI_NEXT_SKILLS_DISABLED";
pub const SKILLS_MODE_ENV: &str = "YUNXI_NEXT_SKILLS_MODE";
const MAX_ROOT_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillsConfig {
    root: PathBuf,
    disabled: BTreeSet<String>,
}

impl SkillsConfig {
    pub fn from_env() -> Result<Self, SkillsConfigError> {
        let current_dir = env::current_dir().map_err(SkillsConfigError::CurrentDirectory)?;
        let root = env::var_os(SKILLS_ROOT_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| current_dir.join("skills"));
        let root = if root.is_absolute() {
            root
        } else {
            current_dir.join(root)
        };
        let disabled = parse_disabled(env::var(SKILLS_DISABLED_ENV).ok().as_deref())?;
        Self::new(root, disabled)
    }

    pub fn new(
        root: impl Into<PathBuf>,
        disabled: impl IntoIterator<Item = String>,
    ) -> Result<Self, SkillsConfigError> {
        let root = root.into();
        let root = if root.is_absolute() {
            root
        } else {
            env::current_dir()
                .map_err(SkillsConfigError::CurrentDirectory)?
                .join(root)
        };
        validate_root(&root)?;
        let disabled = disabled.into_iter().collect::<BTreeSet<_>>();
        for id in &disabled {
            validate_skill_id(id)?;
        }
        Ok(Self { root, disabled })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    pub fn disabled(&self) -> &BTreeSet<String> {
        &self.disabled
    }
}

fn parse_disabled(value: Option<&str>) -> Result<Vec<String>, SkillsConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        validate_skill_id(id)?;
        ids.push(id.to_string());
    }
    Ok(ids)
}

fn validate_root(root: &Path) -> Result<(), SkillsConfigError> {
    let value = root.to_string_lossy();
    if value.trim().is_empty() {
        return Err(SkillsConfigError::EmptyRoot);
    }
    if value.len() > MAX_ROOT_BYTES {
        return Err(SkillsConfigError::RootTooLong {
            length: value.len(),
            maximum: MAX_ROOT_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(SkillsConfigError::RootControlCharacter);
    }
    Ok(())
}

fn validate_skill_id(id: &str) -> Result<(), SkillsConfigError> {
    if id.is_empty()
        || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        || id.ends_with('-')
        || id.ends_with('_')
        || id.chars().any(|character| {
            !character.is_ascii_lowercase()
                && !character.is_ascii_digit()
                && character != '-'
                && character != '_'
        })
    {
        return Err(SkillsConfigError::InvalidSkillId { id: id.to_string() });
    }
    Ok(())
}

#[derive(Debug)]
pub enum SkillsConfigError {
    CurrentDirectory(std::io::Error),
    EmptyRoot,
    RootTooLong { length: usize, maximum: usize },
    RootControlCharacter,
    InvalidSkillId { id: String },
}

impl fmt::Display for SkillsConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(error) => {
                write!(
                    formatter,
                    "failed to resolve Skills working directory: {error}"
                )
            }
            Self::EmptyRoot => formatter.write_str("Skills root cannot be empty"),
            Self::RootTooLong { length, maximum } => {
                write!(
                    formatter,
                    "Skills root is {length} bytes; maximum is {maximum}"
                )
            }
            Self::RootControlCharacter => {
                formatter.write_str("Skills root contains a control character")
            }
            Self::InvalidSkillId { id } => {
                write!(
                    formatter,
                    "Skills id `{id}` is not a valid lowercase identifier"
                )
            }
        }
    }
}

impl Error for SkillsConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CurrentDirectory(error) => Some(error),
            Self::EmptyRoot
            | Self::RootTooLong { .. }
            | Self::RootControlCharacter
            | Self::InvalidSkillId { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_root_is_resolved_and_disabled_ids_are_deduplicated() {
        let config = SkillsConfig::new("skills", ["review".to_string(), "review".to_string()])
            .expect("config");
        assert!(config.root().is_absolute());
        assert!(config.is_disabled("review"));
        assert_eq!(config.disabled().len(), 1);
    }

    #[test]
    fn invalid_disabled_id_is_fail_closed() {
        assert!(parse_disabled(Some("../outside")).is_err());
    }
}
