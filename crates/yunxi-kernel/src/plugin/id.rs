//! Stable plugin identity and validation.

use std::error::Error;
use std::fmt;

const MAX_PLUGIN_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PluginIdError::Empty);
        }
        if value.len() > MAX_PLUGIN_ID_BYTES {
            return Err(PluginIdError::TooLong {
                length: value.len(),
                maximum: MAX_PLUGIN_ID_BYTES,
            });
        }
        for (index, character) in value.char_indices() {
            if !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.') {
                return Err(PluginIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PluginId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginIdError {
    Empty,
    TooLong { length: usize, maximum: usize },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for PluginIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("plugin id cannot be empty"),
            Self::TooLong { length, maximum } => {
                write!(
                    formatter,
                    "plugin id is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "plugin id contains unsupported character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for PluginIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids_accept_stable_ascii_identifiers() {
        let id = PluginId::new("yunxi.voice_v1").expect("valid plugin id");
        assert_eq!(id.as_str(), "yunxi.voice_v1");
    }

    #[test]
    fn plugin_ids_reject_path_like_values() {
        let error = PluginId::new("../voice").expect_err("path-like id must fail");
        assert!(matches!(
            error,
            PluginIdError::InvalidCharacter { character: '/', .. }
        ));
    }
}
