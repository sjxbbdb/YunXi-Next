//! Small, serializable plugin metadata used by composition entries.

use serde::{Deserialize, Serialize};

/// The ownership boundary shown by the plugin inventory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginRole {
    /// Required runtime infrastructure.
    Core,
    /// The replaceable default Agent implementation.
    AgentSpine,
    /// A user-selectable capability.
    #[default]
    Optional,
}

impl PluginRole {
    pub const fn is_user_toggleable(self) -> bool {
        matches!(self, Self::Optional)
    }
}

/// Coarse risk metadata for display and default activation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginRisk {
    /// No external side effects; memory-only behavior is expected.
    None,
    /// The plugin has an external effect or its scope is not yet classified.
    #[default]
    External,
}

impl PluginRisk {
    pub const fn has_external_side_effects(self) -> bool {
        matches!(self, Self::External)
    }
}

/// How an entry chooses its initial enabled state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultEnablement {
    /// Always enabled, used by core and Agent-spine entries.
    Always,
    /// Enabled only for a no-side-effect plugin.
    Safe,
    /// Disabled until the user enables it.
    #[default]
    Never,
}

impl DefaultEnablement {
    pub const fn default_for(role: PluginRole, risk: PluginRisk) -> Self {
        match role {
            PluginRole::Core | PluginRole::AgentSpine => Self::Always,
            PluginRole::Optional => {
                if matches!(risk, PluginRisk::None) {
                    Self::Safe
                } else {
                    Self::Never
                }
            }
        }
    }

    pub const fn resolves_to_enabled(self, risk: PluginRisk, role: PluginRole) -> bool {
        match role {
            PluginRole::Core | PluginRole::AgentSpine => true,
            PluginRole::Optional => match self {
                Self::Always => true,
                Self::Safe => matches!(risk, PluginRisk::None),
                Self::Never => false,
            },
        }
    }
}

/// Manifest policy carried by a composition entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(default)]
    role: PluginRole,
    #[serde(default)]
    risk: PluginRisk,
    #[serde(default)]
    default_enablement: DefaultEnablement,
}

impl PluginManifest {
    pub const fn new(
        role: PluginRole,
        risk: PluginRisk,
        default_enablement: DefaultEnablement,
    ) -> Self {
        Self {
            role,
            risk,
            default_enablement,
        }
    }

    pub const fn for_plugin(role: PluginRole, risk: PluginRisk) -> Self {
        Self::new(role, risk, DefaultEnablement::default_for(role, risk))
    }

    pub const fn core() -> Self {
        Self::for_plugin(PluginRole::Core, PluginRisk::None)
    }

    pub const fn agent_spine() -> Self {
        Self::for_plugin(PluginRole::AgentSpine, PluginRisk::None)
    }

    pub const fn optional(risk: PluginRisk) -> Self {
        Self::for_plugin(PluginRole::Optional, risk)
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

    pub const fn default_enabled(self) -> bool {
        self.default_enablement
            .resolves_to_enabled(self.risk, self.role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_starts_safe_plugins_and_keeps_external_plugins_off() {
        assert!(PluginManifest::optional(PluginRisk::None).default_enabled());
        assert!(!PluginManifest::optional(PluginRisk::External).default_enabled());
        assert!(PluginManifest::core().default_enabled());
        assert!(PluginManifest::agent_spine().default_enabled());
    }
}
