//! Stable plugin identity, capability, and host-grant declarations.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::CapabilityDescriptor;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

const MAX_MANIFEST_METADATA_BYTES: usize = 128;
const MAX_MANIFEST_CAPABILITIES: usize = 64;
const MAX_MANIFEST_GRANTS: usize = 32;

/// Permissions a plugin may need from the trusted host.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    Approval,
    WorkspaceRead,
    WorkspaceWrite,
    Network,
    Secret,
    ProviderCredential,
}

impl fmt::Display for GrantKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Approval => "approval",
            Self::WorkspaceRead => "workspace_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::Network => "network",
            Self::Secret => "secret",
            Self::ProviderCredential => "provider_credential",
        };
        formatter.write_str(value)
    }
}

/// Whether a declared host grant is mandatory or merely supported.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "requirement", content = "grant", rename_all = "snake_case")]
pub enum GrantRequirement {
    Required(GrantKind),
    Optional(GrantKind),
}

impl GrantRequirement {
    pub const fn required(kind: GrantKind) -> Self {
        Self::Required(kind)
    }

    pub const fn optional(kind: GrantKind) -> Self {
        Self::Optional(kind)
    }

    pub const fn kind(self) -> GrantKind {
        match self {
            Self::Required(kind) | Self::Optional(kind) => kind,
        }
    }

    pub const fn is_required(self) -> bool {
        matches!(self, Self::Required(_))
    }
}

/// Self-description sent during the plugin readiness handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    schema_version: u32,
    plugin_id: String,
    display_name: String,
    plugin_version: String,
    capabilities: Vec<CapabilityDescriptor>,
    grants: Vec<GrantRequirement>,
}

impl PluginManifest {
    pub fn new(
        plugin_id: impl Into<String>,
        display_name: impl Into<String>,
        plugin_version: impl Into<String>,
        capabilities: Vec<CapabilityDescriptor>,
    ) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            plugin_id: plugin_id.into(),
            display_name: display_name.into(),
            plugin_version: plugin_version.into(),
            capabilities,
            grants: Vec::new(),
        }
    }

    pub fn with_grants(mut self, grants: Vec<GrantRequirement>) -> Self {
        self.grants = grants;
        self
    }

    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }

    pub fn grants(&self) -> &[GrantRequirement] {
        &self.grants
    }

    pub fn declares_required_grant(&self, kind: GrantKind) -> bool {
        self.grants
            .iter()
            .any(|requirement| requirement.kind() == kind && requirement.is_required())
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                version: self.schema_version,
                expected: MANIFEST_SCHEMA_VERSION,
            });
        }
        validate_metadata("plugin id", &self.plugin_id)?;
        validate_metadata("display name", &self.display_name)?;
        validate_metadata("plugin version", &self.plugin_version)?;

        if self.capabilities.is_empty() {
            return Err(ManifestError::NoCapabilities);
        }
        if self.capabilities.len() > MAX_MANIFEST_CAPABILITIES {
            return Err(ManifestError::TooManyCapabilities {
                count: self.capabilities.len(),
                maximum: MAX_MANIFEST_CAPABILITIES,
            });
        }
        let mut capabilities = BTreeSet::new();
        for capability in &self.capabilities {
            if !capabilities.insert(capability.id().as_str()) {
                return Err(ManifestError::DuplicateCapability {
                    capability: capability.id().to_string(),
                });
            }
        }

        if self.grants.len() > MAX_MANIFEST_GRANTS {
            return Err(ManifestError::TooManyGrants {
                count: self.grants.len(),
                maximum: MAX_MANIFEST_GRANTS,
            });
        }
        let mut grants = BTreeSet::new();
        for requirement in &self.grants {
            if !grants.insert(requirement.kind()) {
                return Err(ManifestError::DuplicateGrant {
                    grant: requirement.kind(),
                });
            }
        }
        Ok(())
    }
}

fn validate_metadata(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        return Err(ManifestError::EmptyField { field });
    }
    if value.len() > MAX_MANIFEST_METADATA_BYTES {
        return Err(ManifestError::FieldTooLong {
            field,
            length: value.len(),
            maximum: MAX_MANIFEST_METADATA_BYTES,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    UnsupportedSchema {
        version: u32,
        expected: u32,
    },
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    NoCapabilities,
    TooManyCapabilities {
        count: usize,
        maximum: usize,
    },
    DuplicateCapability {
        capability: String,
    },
    TooManyGrants {
        count: usize,
        maximum: usize,
    },
    DuplicateGrant {
        grant: GrantKind,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { version, expected } => write!(
                formatter,
                "manifest schema {version} is unsupported; expected {expected}"
            ),
            Self::EmptyField { field } => write!(formatter, "manifest {field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(
                formatter,
                "manifest {field} is {length} bytes; maximum is {maximum}"
            ),
            Self::NoCapabilities => {
                formatter.write_str("manifest must announce at least one capability")
            }
            Self::TooManyCapabilities { count, maximum } => write!(
                formatter,
                "manifest announces {count} capabilities; maximum is {maximum}"
            ),
            Self::DuplicateCapability { capability } => write!(
                formatter,
                "manifest announces capability `{capability}` more than once"
            ),
            Self::TooManyGrants { count, maximum } => write!(
                formatter,
                "manifest declares {count} grants; maximum is {maximum}"
            ),
            Self::DuplicateGrant { grant } => {
                write!(
                    formatter,
                    "manifest declares grant `{grant}` more than once"
                )
            }
        }
    }
}

impl Error for ManifestError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability() -> CapabilityDescriptor {
        CapabilityDescriptor::new(crate::capabilities::MODEL_CHAT, 1).expect("capability")
    }

    #[test]
    fn manifest_round_trips_identity_capabilities_and_grants() {
        let manifest = PluginManifest::new("yunxi.test", "Fixture", "1.0.0", vec![capability()])
            .with_grants(vec![
                GrantRequirement::required(GrantKind::Network),
                GrantRequirement::optional(GrantKind::WorkspaceRead),
            ]);
        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        let decoded = serde_json::from_str::<PluginManifest>(&json).expect("deserialize manifest");
        assert_eq!(decoded, manifest);
        assert!(decoded.declares_required_grant(GrantKind::Network));
        assert!(!decoded.declares_required_grant(GrantKind::WorkspaceRead));
    }

    #[test]
    fn duplicate_grants_and_capabilities_are_rejected() {
        let duplicate_capability = capability();
        let capability_error = PluginManifest::new(
            "yunxi.test",
            "Fixture",
            "1.0.0",
            vec![duplicate_capability.clone(), duplicate_capability],
        )
        .validate()
        .expect_err("duplicate capability must fail");
        assert!(matches!(
            capability_error,
            ManifestError::DuplicateCapability { .. }
        ));

        let grant_error = PluginManifest::new("yunxi.test", "Fixture", "1.0.0", vec![capability()])
            .with_grants(vec![
                GrantRequirement::required(GrantKind::Network),
                GrantRequirement::optional(GrantKind::Network),
            ])
            .validate()
            .expect_err("duplicate grant must fail");
        assert!(matches!(grant_error, ManifestError::DuplicateGrant { .. }));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let mut manifest =
            PluginManifest::new("yunxi.test", "Fixture", "1.0.0", vec![capability()]);
        manifest.schema_version = MANIFEST_SCHEMA_VERSION + 1;
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::UnsupportedSchema { .. })
        ));
    }
}
