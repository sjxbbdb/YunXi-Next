//! Validated composition entries and their bounded JSON configuration.

use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{DefaultEnablement, PluginManifest, PluginRisk, PluginRole};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest: Option<PluginManifest>,
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
            manifest: None,
        })
    }

    /// Creates an entry whose initial state follows its manifest policy.
    pub fn new_with_manifest(
        id: impl Into<String>,
        module_name: impl Into<String>,
        manifest: PluginManifest,
    ) -> Result<Self, EntryError> {
        Ok(Self::new(id, module_name)?.with_manifest(manifest))
    }

    /// Attaches manifest metadata and applies its default enablement.
    pub fn with_manifest(mut self, manifest: PluginManifest) -> Self {
        self.enabled = manifest.default_enabled();
        self.manifest = Some(manifest);
        self
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

    pub fn manifest(&self) -> Option<PluginManifest> {
        self.manifest
    }

    pub fn role(&self) -> Option<PluginRole> {
        self.manifest.map(PluginManifest::role)
    }

    pub fn risk(&self) -> Option<PluginRisk> {
        self.manifest.map(PluginManifest::risk)
    }

    pub fn default_enablement(&self) -> Option<DefaultEnablement> {
        self.manifest.map(PluginManifest::default_enablement)
    }

    pub fn default_enabled(&self) -> Option<bool> {
        self.manifest.map(PluginManifest::default_enabled)
    }

    pub fn user_toggleable(&self) -> bool {
        self.manifest
            .is_none_or(|manifest| manifest.role().is_user_toggleable())
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
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default = "empty_config")]
            config: Value,
            #[serde(default)]
            group: Option<EntryId>,
            #[serde(default)]
            manifest: Option<PluginManifest>,
        }

        let wire = WireEntry::deserialize(deserializer)?;
        let mut entry = Self::from_id(wire.id, wire.module_name).map_err(D::Error::custom)?;
        entry.group = wire.group;
        entry.config = wire.config;
        entry.manifest = wire.manifest;
        entry.enabled = wire
            .enabled
            .unwrap_or_else(|| entry.manifest.is_none_or(PluginManifest::default_enabled));
        validate_config(&entry.config).map_err(D::Error::custom)?;
        Ok(entry)
    }
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

    #[test]
    fn manifest_defaults_are_applied_and_old_entries_keep_their_wire_shape() {
        let safe = CompositionEntry::new_with_manifest(
            "safe",
            "fixture.safe",
            PluginManifest::optional(PluginRisk::None),
        )
        .expect("safe entry");
        assert!(safe.enabled());
        assert_eq!(safe.role(), Some(PluginRole::Optional));
        assert_eq!(safe.default_enablement(), Some(DefaultEnablement::Safe));
        assert!(safe.user_toggleable());

        let external = CompositionEntry::new_with_manifest(
            "external",
            "fixture.external",
            PluginManifest::optional(PluginRisk::External),
        )
        .expect("external entry");
        assert!(!external.enabled());

        let core =
            CompositionEntry::new_with_manifest("core", "fixture.core", PluginManifest::core())
                .expect("core entry");
        assert!(core.enabled());
        assert!(!core.user_toggleable());

        let legacy =
            serde_json::to_value(CompositionEntry::new("legacy", "fixture.legacy").expect("entry"))
                .expect("serialize legacy entry");
        assert!(legacy.get("manifest").is_none());
    }

    #[test]
    fn omitted_enabled_field_uses_the_manifest_default() {
        let external = serde_json::json!({
            "id": "external",
            "moduleName": "fixture.external",
            "manifest": {
                "role": "optional",
                "risk": "external",
                "defaultEnablement": "never"
            }
        });
        let entry = serde_json::from_value::<CompositionEntry>(external).expect("entry");
        assert!(!entry.enabled());

        let safe = serde_json::json!({
            "id": "safe",
            "moduleName": "fixture.safe",
            "manifest": {
                "role": "optional",
                "risk": "none",
                "defaultEnablement": "safe"
            }
        });
        let entry = serde_json::from_value::<CompositionEntry>(safe).expect("entry");
        assert!(entry.enabled());
    }
}
