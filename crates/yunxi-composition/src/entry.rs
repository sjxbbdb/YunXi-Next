//! Validated composition entries and their bounded JSON configuration.

use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

const MAX_ENTRY_ID_BYTES: usize = 128;
const MAX_MODULE_NAME_BYTES: usize = 512;
const MAX_ENTRY_CONFIG_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryId(String);

impl EntryId {
    pub fn new(value: impl Into<String>) -> Result<Self, EntryIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(EntryIdError::Empty);
        }
        if value.len() > MAX_ENTRY_ID_BYTES {
            return Err(EntryIdError::TooLong {
                length: value.len(),
                maximum: MAX_ENTRY_ID_BYTES,
            });
        }
        for (index, character) in value.char_indices() {
            if !character.is_ascii() || character.is_control() || character.is_whitespace() {
                return Err(EntryIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for EntryId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for EntryId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EntryId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryIdError {
    Empty,
    TooLong { length: usize, maximum: usize },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for EntryIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("entry id cannot be empty"),
            Self::TooLong { length, maximum } => {
                write!(
                    formatter,
                    "entry id is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "entry id contains unsupported character `{character}` at byte {index}"
            ),
        }
    }
}

impl Error for EntryIdError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryError {
    InvalidId(EntryIdError),
    EmptyModuleName,
    ModuleNameTooLong { length: usize, maximum: usize },
    InvalidModuleCharacter { index: usize, character: char },
    ConfigTooLarge { bytes: usize, maximum: usize },
    ConfigNotSerializable,
}

impl fmt::Display for EntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(error) => error.fmt(formatter),
            Self::EmptyModuleName => formatter.write_str("entry module name cannot be empty"),
            Self::ModuleNameTooLong { length, maximum } => write!(
                formatter,
                "entry module name is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidModuleCharacter { index, character } => write!(
                formatter,
                "entry module name contains unsupported character `{character}` at byte {index}"
            ),
            Self::ConfigTooLarge { bytes, maximum } => write!(
                formatter,
                "entry config is {bytes} bytes; maximum is {maximum}"
            ),
            Self::ConfigNotSerializable => formatter.write_str("entry config is not valid JSON"),
        }
    }
}

impl Error for EntryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidId(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EntryIdError> for EntryError {
    fn from(error: EntryIdError) -> Self {
        Self::InvalidId(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompositionEntry {
    id: EntryId,
    module_name: String,
    enabled: bool,
    config: Value,
    group: Option<EntryId>,
}

impl CompositionEntry {
    pub fn new(id: impl Into<String>, module_name: impl Into<String>) -> Result<Self, EntryError> {
        let id = EntryId::new(id).map_err(EntryError::InvalidId)?;
        Self::from_id(id, module_name)
    }

    pub fn from_id(id: EntryId, module_name: impl Into<String>) -> Result<Self, EntryError> {
        let module_name = module_name.into();
        validate_module_name(&module_name)?;
        Ok(Self {
            id,
            module_name,
            enabled: true,
            config: empty_config(),
            group: None,
        })
    }

    pub fn with_config(mut self, config: Value) -> Result<Self, EntryError> {
        validate_config(&config)?;
        self.config = config;
        Ok(self)
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_group(mut self, group: EntryId) -> Self {
        self.group = Some(group);
        self
    }

    pub fn id(&self) -> &EntryId {
        &self.id
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn config(&self) -> &Value {
        &self.config
    }

    pub fn group(&self) -> Option<&EntryId> {
        self.group.as_ref()
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

impl<'de> Deserialize<'de> for CompositionEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireEntry {
            id: EntryId,
            module_name: String,
            #[serde(default = "default_enabled")]
            enabled: bool,
            #[serde(default = "empty_config")]
            config: Value,
            #[serde(default)]
            group: Option<EntryId>,
        }

        let wire = WireEntry::deserialize(deserializer)?;
        let mut entry = Self::from_id(wire.id, wire.module_name).map_err(D::Error::custom)?;
        entry.enabled = wire.enabled;
        entry.group = wire.group;
        entry.config = wire.config;
        validate_config(&entry.config).map_err(D::Error::custom)?;
        Ok(entry)
    }
}

fn default_enabled() -> bool {
    true
}

fn empty_config() -> Value {
    Value::Object(Map::new())
}

fn validate_module_name(value: &str) -> Result<(), EntryError> {
    if value.is_empty() {
        return Err(EntryError::EmptyModuleName);
    }
    if value.len() > MAX_MODULE_NAME_BYTES {
        return Err(EntryError::ModuleNameTooLong {
            length: value.len(),
            maximum: MAX_MODULE_NAME_BYTES,
        });
    }
    for (index, character) in value.char_indices() {
        if !character.is_ascii() || character.is_control() || character.is_whitespace() {
            return Err(EntryError::InvalidModuleCharacter { index, character });
        }
    }
    Ok(())
}

fn validate_config(value: &Value) -> Result<(), EntryError> {
    let bytes = serde_json::to_vec(value).map_err(|_| EntryError::ConfigNotSerializable)?;
    if bytes.len() > MAX_ENTRY_CONFIG_BYTES {
        return Err(EntryError::ConfigTooLarge {
            bytes: bytes.len(),
            maximum: MAX_ENTRY_CONFIG_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entry_round_trips_with_dsh_style_camel_case_fields() {
        let entry = CompositionEntry::new("tool-bash", "@deepseek-ai/dsh-tool-bash")
            .expect("valid entry")
            .with_enabled(false)
            .with_config(json!({ "timeoutMs": 1000 }))
            .expect("valid config");
        let encoded = serde_json::to_string(&entry).expect("serialize entry");
        assert!(encoded.contains("moduleName"));
        assert_eq!(
            serde_json::from_str::<CompositionEntry>(&encoded).expect("deserialize entry"),
            entry
        );
    }

    #[test]
    fn ids_allow_dsh_operation_style_punctuation_but_reject_whitespace() {
        assert!(EntryId::new("@fixture/plugin#goals/create").is_ok());
        assert!(matches!(
            EntryId::new("has space"),
            Err(EntryIdError::InvalidCharacter { .. })
        ));
    }

    #[test]
    fn oversized_config_is_rejected_before_composition() {
        let value = json!({ "payload": "x".repeat(MAX_ENTRY_CONFIG_BYTES) });
        let error = CompositionEntry::new("large", "fixture")
            .expect("valid entry")
            .with_config(value)
            .expect_err("large config must fail");
        assert!(matches!(error, EntryError::ConfigTooLarge { .. }));
    }
}
