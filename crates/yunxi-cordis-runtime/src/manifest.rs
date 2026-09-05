//! Static plugin policy metadata.

use std::fmt;

pub const MAX_PLUGIN_ID_BYTES: usize = 128;
pub const MAX_DISPLAY_NAME_BYTES: usize = 128;

/// Ownership boundary for a runtime plugin.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PluginRole {
    /// Trusted runtime infrastructure. It cannot be disabled by the user.
    Core,
    /// The replaceable default Agent implementation. It cannot be disabled
    /// by the user while the runtime is running.
    AgentSpine,
    /// A user-selectable capability plugin.
    #[default]
    Optional,
}

impl PluginRole {
    pub const fn is_required(self) -> bool {
        matches!(self, Self::Core | Self::AgentSpine)
    }

    pub const fn is_user_toggleable(self) -> bool {
        matches!(self, Self::Optional)
    }
}

impl fmt::Display for PluginRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Core => "core",
            Self::AgentSpine => "agent-spine",
            Self::Optional => "optional",
        })
    }
}

/// Coarse operational risk used by the default startup policy.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PluginRisk {
    /// The plugin is expected to stay in memory and have no external effects.
    Safe,
    /// The plugin can access an external resource or has a larger blast radius.
    #[default]
    External,
}

impl PluginRisk {
    pub const fn has_external_effects(self) -> bool {
        matches!(self, Self::External)
    }
}

impl fmt::Display for PluginRisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Safe => "safe",
            Self::External => "external",
        })
    }
}

/// Default enablement declaration in a static manifest.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DefaultEnablement {
    /// Reserved for Core and AgentSpine entries.
    Always,
    /// A safe optional entry starts by default; an external entry does not.
    Safe,
    /// The entry starts disabled until the user enables it.
    #[default]
    Never,
}

impl fmt::Display for DefaultEnablement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Always => "always",
            Self::Safe => "safe",
            Self::Never => "never",
        })
    }
}

impl DefaultEnablement {
    /// Select the canonical default for a role/risk pair.
    pub const fn default_for(role: PluginRole, risk: PluginRisk) -> Self {
        match role {
            PluginRole::Core | PluginRole::AgentSpine => Self::Always,
            PluginRole::Optional => match risk {
                PluginRisk::Safe => Self::Safe,
                PluginRisk::External => Self::Never,
            },
        }
    }

    /// Resolve this declaration before considering a user override.
    pub const fn resolves_to_enabled(self, role: PluginRole, risk: PluginRisk) -> bool {
        match role {
            PluginRole::Core | PluginRole::AgentSpine => true,
            PluginRole::Optional => match self {
                Self::Always => true,
                Self::Safe => matches!(risk, PluginRisk::Safe),
                Self::Never => false,
            },
        }
    }
}

/// Immutable metadata attached to one statically registered plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginManifest {
    id: &'static str,
    display_name: &'static str,
    role: PluginRole,
    risk: PluginRisk,
    default_enablement: DefaultEnablement,
}

impl PluginManifest {
    pub const fn new(
        id: &'static str,
        display_name: &'static str,
        role: PluginRole,
        risk: PluginRisk,
        default_enablement: DefaultEnablement,
    ) -> Self {
        Self {
            id,
            display_name,
            role,
            risk,
            default_enablement,
        }
    }

    pub const fn for_plugin(
        id: &'static str,
        display_name: &'static str,
        role: PluginRole,
        risk: PluginRisk,
    ) -> Self {
        Self::new(
            id,
            display_name,
            role,
            risk,
            DefaultEnablement::default_for(role, risk),
        )
    }

    pub const fn core(id: &'static str, display_name: &'static str) -> Self {
        Self::new(
            id,
            display_name,
            PluginRole::Core,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        )
    }

    pub const fn agent_spine(id: &'static str, display_name: &'static str) -> Self {
        Self::new(
            id,
            display_name,
            PluginRole::AgentSpine,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        )
    }

    pub const fn safe_optional(id: &'static str, display_name: &'static str) -> Self {
        Self::new(
            id,
            display_name,
            PluginRole::Optional,
            PluginRisk::Safe,
            DefaultEnablement::Safe,
        )
    }

    pub const fn external_optional(id: &'static str, display_name: &'static str) -> Self {
        Self::new(
            id,
            display_name,
            PluginRole::Optional,
            PluginRisk::External,
            DefaultEnablement::Never,
        )
    }

    pub const fn id(self) -> &'static str {
        self.id
    }

    pub const fn display_name(self) -> &'static str {
        self.display_name
    }

    pub const fn role(self) -> PluginRole {
        self.role
    }

    pub const fn risk(self) -> PluginRisk {
        self.risk
    }

    pub const fn default_enablement(self) -> DefaultEnablement {
        self.default_enablement
    }

    /// Resolve the default without considering a user override.
    pub const fn default_enabled(self) -> bool {
        self.default_enablement
            .resolves_to_enabled(self.role, self.risk)
    }

    pub fn validate(self) -> Result<(), ManifestError> {
        validate_id(self.id)?;
        if self.display_name.trim().is_empty() {
            return Err(ManifestError::EmptyDisplayName);
        }
        if self.display_name.len() > MAX_DISPLAY_NAME_BYTES {
            return Err(ManifestError::DisplayNameTooLong {
                length: self.display_name.len(),
                maximum: MAX_DISPLAY_NAME_BYTES,
            });
        }
        if self.display_name.chars().any(char::is_control) {
            return Err(ManifestError::InvalidDisplayNameCharacter);
        }

        let valid_policy = match self.role {
            PluginRole::Core | PluginRole::AgentSpine => {
                matches!(self.default_enablement, DefaultEnablement::Always)
            }
            PluginRole::Optional => {
                !matches!(self.default_enablement, DefaultEnablement::Always)
                    && !(matches!(self.risk, PluginRisk::External)
                        && matches!(self.default_enablement, DefaultEnablement::Safe))
            }
        };
        if !valid_policy {
            return Err(ManifestError::InvalidDefaultPolicy {
                role: self.role,
                risk: self.risk,
                default_enablement: self.default_enablement,
            });
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() {
        return Err(ManifestError::EmptyId);
    }
    if id.len() > MAX_PLUGIN_ID_BYTES {
        return Err(ManifestError::IdTooLong {
            length: id.len(),
            maximum: MAX_PLUGIN_ID_BYTES,
        });
    }
    for (index, character) in id.char_indices() {
        if !character.is_ascii() || character.is_control() || character.is_whitespace() {
            return Err(ManifestError::InvalidIdCharacter { index, character });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    EmptyId,
    IdTooLong {
        length: usize,
        maximum: usize,
    },
    InvalidIdCharacter {
        index: usize,
        character: char,
    },
    EmptyDisplayName,
    DisplayNameTooLong {
        length: usize,
        maximum: usize,
    },
    InvalidDisplayNameCharacter,
    InvalidDefaultPolicy {
        role: PluginRole,
        risk: PluginRisk,
        default_enablement: DefaultEnablement,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => formatter.write_str("plugin id cannot be empty"),
            Self::IdTooLong { length, maximum } => {
                write!(
                    formatter,
                    "plugin id is {length} bytes; maximum is {maximum}"
                )
            }
            Self::InvalidIdCharacter { index, character } => write!(
                formatter,
                "plugin id contains unsupported character `{character}` at byte {index}"
            ),
            Self::EmptyDisplayName => formatter.write_str("plugin display name cannot be empty"),
            Self::DisplayNameTooLong { length, maximum } => write!(
                formatter,
                "plugin display name is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidDisplayNameCharacter => {
                formatter.write_str("plugin display name contains a control character")
            }
            Self::InvalidDefaultPolicy {
                role,
                risk,
                default_enablement,
            } => write!(
                formatter,
                "invalid default policy {default_enablement} for {role}/{risk} plugin"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_matches_role_and_risk() {
        assert_eq!(
            DefaultEnablement::default_for(PluginRole::Core, PluginRisk::Safe),
            DefaultEnablement::Always
        );
        assert_eq!(
            DefaultEnablement::default_for(PluginRole::Optional, PluginRisk::Safe),
            DefaultEnablement::Safe
        );
        assert_eq!(
            DefaultEnablement::default_for(PluginRole::Optional, PluginRisk::External),
            DefaultEnablement::Never
        );
        assert!(PluginManifest::core("core", "Core").default_enabled());
        assert!(PluginManifest::agent_spine("agent", "Agent").default_enabled());
        assert!(PluginManifest::safe_optional("safe", "Safe").default_enabled());
        assert!(!PluginManifest::external_optional("external", "External").default_enabled());
    }

    #[test]
    fn invalid_policy_is_rejected_before_mounting() {
        let manifest = PluginManifest::new(
            "optional.always",
            "Invalid",
            PluginRole::Optional,
            PluginRisk::Safe,
            DefaultEnablement::Always,
        );
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::InvalidDefaultPolicy { .. })
        ));
    }
}
