//! Stable capability keys, defaults, and environment override precedence.

use serde::{Deserialize, Serialize};

pub const CAPABILITY_SETTINGS_NAMESPACE: &str = "yunxi-capabilities";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilitySetting {
    Context,
    Persona,
    Memory,
    Companion,
    Storage,
    Mailbox,
    Scheduler,
    Shell,
    Patch,
    Files,
    Mcp,
    Skills,
    MultiAgent,
    Voice,
    Weixin,
}

impl CapabilitySetting {
    pub const ALL: [Self; 15] = [
        Self::Context,
        Self::Persona,
        Self::Memory,
        Self::Companion,
        Self::Storage,
        Self::Mailbox,
        Self::Scheduler,
        Self::Shell,
        Self::Patch,
        Self::Files,
        Self::Mcp,
        Self::Skills,
        Self::MultiAgent,
        Self::Voice,
        Self::Weixin,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Persona => "persona",
            Self::Memory => "memory",
            Self::Companion => "companion",
            Self::Storage => "storage",
            Self::Mailbox => "mailbox",
            Self::Scheduler => "scheduler",
            Self::Shell => "shell",
            Self::Patch => "patch",
            Self::Files => "files",
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::MultiAgent => "multi_agent",
            Self::Voice => "voice",
            Self::Weixin => "weixin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.as_str() == value)
    }

    const fn environment_names(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Context => ("YUNXI_NEXT_CONTEXT_ENABLED", None),
            Self::Persona => ("YUNXI_NEXT_PERSONA_ENABLED", Some("YUNXI_PERSONA_ENABLED")),
            Self::Memory => ("YUNXI_NEXT_MEMORY_ENABLED", Some("YUNXI_MEMORY_ENABLED")),
            Self::Companion => (
                "YUNXI_NEXT_COMPANION_ENABLED",
                Some("YUNXI_COMPANION_ENABLED"),
            ),
            Self::Storage => ("YUNXI_NEXT_STORAGE_ENABLED", None),
            Self::Mailbox => ("YUNXI_NEXT_MAILBOX_ENABLED", None),
            Self::Scheduler => ("YUNXI_NEXT_SCHEDULER_ENABLED", None),
            Self::Shell => ("YUNXI_NEXT_SHELL_ENABLED", None),
            Self::Patch => ("YUNXI_NEXT_PATCH_ENABLED", None),
            Self::Files => ("YUNXI_NEXT_FILES_ENABLED", None),
            Self::Mcp => ("YUNXI_NEXT_MCP_ENABLED", None),
            Self::Skills => ("YUNXI_NEXT_SKILLS_ENABLED", None),
            Self::MultiAgent => ("YUNXI_NEXT_MULTI_AGENT_ENABLED", None),
            Self::Voice => ("YUNXI_NEXT_VOICE_ENABLED", None),
            Self::Weixin => ("YUNXI_NEXT_WEIXIN_ENABLED", None),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persona: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    companion: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mailbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shell: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    patch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skills: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    multi_agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    weixin: Option<bool>,
}

impl CapabilityOverrides {
    pub fn get(&self, setting: CapabilitySetting) -> Option<bool> {
        match setting {
            CapabilitySetting::Context => self.context,
            CapabilitySetting::Persona => self.persona,
            CapabilitySetting::Memory => self.memory,
            CapabilitySetting::Companion => self.companion,
            CapabilitySetting::Storage => self.storage,
            CapabilitySetting::Mailbox => self.mailbox,
            CapabilitySetting::Scheduler => self.scheduler,
            CapabilitySetting::Shell => self.shell,
            CapabilitySetting::Patch => self.patch,
            CapabilitySetting::Files => self.files,
            CapabilitySetting::Mcp => self.mcp,
            CapabilitySetting::Skills => self.skills,
            CapabilitySetting::MultiAgent => self.multi_agent,
            CapabilitySetting::Voice => self.voice,
            CapabilitySetting::Weixin => self.weixin,
        }
    }

    pub fn set(&mut self, setting: CapabilitySetting, value: bool) {
        *self.slot_mut(setting) = Some(value);
    }

    pub fn unset(&mut self, setting: CapabilitySetting) {
        *self.slot_mut(setting) = None;
    }

    pub fn merge(&mut self, patch: &Self) {
        for setting in CapabilitySetting::ALL {
            if let Some(value) = patch.get(setting) {
                self.set(setting, value);
            }
        }
    }

    fn slot_mut(&mut self, setting: CapabilitySetting) -> &mut Option<bool> {
        match setting {
            CapabilitySetting::Context => &mut self.context,
            CapabilitySetting::Persona => &mut self.persona,
            CapabilitySetting::Memory => &mut self.memory,
            CapabilitySetting::Companion => &mut self.companion,
            CapabilitySetting::Storage => &mut self.storage,
            CapabilitySetting::Mailbox => &mut self.mailbox,
            CapabilitySetting::Scheduler => &mut self.scheduler,
            CapabilitySetting::Shell => &mut self.shell,
            CapabilitySetting::Patch => &mut self.patch,
            CapabilitySetting::Files => &mut self.files,
            CapabilitySetting::Mcp => &mut self.mcp,
            CapabilitySetting::Skills => &mut self.skills,
            CapabilitySetting::MultiAgent => &mut self.multi_agent,
            CapabilitySetting::Voice => &mut self.voice,
            CapabilitySetting::Weixin => &mut self.weixin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilitySwitches {
    pub context: bool,
    pub persona: bool,
    pub memory: bool,
    pub companion: bool,
    pub storage: bool,
    pub mailbox: bool,
    pub scheduler: bool,
    pub shell: bool,
    pub patch: bool,
    pub files: bool,
    pub mcp: bool,
    pub skills: bool,
    pub multi_agent: bool,
    pub voice: bool,
    pub weixin: bool,
}

impl Default for CapabilitySwitches {
    fn default() -> Self {
        Self::from_overrides(&CapabilityOverrides::default())
    }
}

impl CapabilitySwitches {
    pub fn from_overrides(overrides: &CapabilityOverrides) -> Self {
        let companion = overrides.get(CapabilitySetting::Companion).unwrap_or(false);
        Self {
            context: overrides.get(CapabilitySetting::Context).unwrap_or(true),
            persona: overrides.get(CapabilitySetting::Persona).unwrap_or(true),
            memory: overrides.get(CapabilitySetting::Memory).unwrap_or(false),
            companion,
            storage: overrides.get(CapabilitySetting::Storage).unwrap_or(true),
            mailbox: overrides
                .get(CapabilitySetting::Mailbox)
                .unwrap_or(companion),
            scheduler: overrides
                .get(CapabilitySetting::Scheduler)
                .unwrap_or(companion),
            shell: overrides.get(CapabilitySetting::Shell).unwrap_or(false),
            patch: overrides.get(CapabilitySetting::Patch).unwrap_or(false),
            files: overrides.get(CapabilitySetting::Files).unwrap_or(false),
            mcp: overrides.get(CapabilitySetting::Mcp).unwrap_or(false),
            skills: overrides.get(CapabilitySetting::Skills).unwrap_or(false),
            multi_agent: overrides
                .get(CapabilitySetting::MultiAgent)
                .unwrap_or(false),
            voice: overrides.get(CapabilitySetting::Voice).unwrap_or(false),
            weixin: overrides.get(CapabilitySetting::Weixin).unwrap_or(false),
        }
    }

    pub fn with_environment<F>(mut self, read: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        for setting in CapabilitySetting::ALL {
            let (primary, legacy) = setting.environment_names();
            let value = read_bool(&read, primary)
                .or_else(|| legacy.and_then(|name| read_bool(&read, name)));
            if let Some(value) = value {
                self.set(setting, value);
            }
        }
        self
    }

    pub fn get(&self, setting: CapabilitySetting) -> bool {
        match setting {
            CapabilitySetting::Context => self.context,
            CapabilitySetting::Persona => self.persona,
            CapabilitySetting::Memory => self.memory,
            CapabilitySetting::Companion => self.companion,
            CapabilitySetting::Storage => self.storage,
            CapabilitySetting::Mailbox => self.mailbox,
            CapabilitySetting::Scheduler => self.scheduler,
            CapabilitySetting::Shell => self.shell,
            CapabilitySetting::Patch => self.patch,
            CapabilitySetting::Files => self.files,
            CapabilitySetting::Mcp => self.mcp,
            CapabilitySetting::Skills => self.skills,
            CapabilitySetting::MultiAgent => self.multi_agent,
            CapabilitySetting::Voice => self.voice,
            CapabilitySetting::Weixin => self.weixin,
        }
    }

    fn set(&mut self, setting: CapabilitySetting, value: bool) {
        match setting {
            CapabilitySetting::Context => self.context = value,
            CapabilitySetting::Persona => self.persona = value,
            CapabilitySetting::Memory => self.memory = value,
            CapabilitySetting::Companion => self.companion = value,
            CapabilitySetting::Storage => self.storage = value,
            CapabilitySetting::Mailbox => self.mailbox = value,
            CapabilitySetting::Scheduler => self.scheduler = value,
            CapabilitySetting::Shell => self.shell = value,
            CapabilitySetting::Patch => self.patch = value,
            CapabilitySetting::Files => self.files = value,
            CapabilitySetting::Mcp => self.mcp = value,
            CapabilitySetting::Skills => self.skills = value,
            CapabilitySetting::MultiAgent => self.multi_agent = value,
            CapabilitySetting::Voice => self.voice = value,
            CapabilitySetting::Weixin => self.weixin = value,
        }
    }
}

fn read_bool<F>(read: &F, name: &str) -> Option<bool>
where
    F: Fn(&str) -> Option<String>,
{
    match read(name)?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn defaults_keep_optional_tools_off_and_core_context_on() {
        let switches = CapabilitySwitches::default();
        assert!(switches.context);
        assert!(switches.persona);
        assert!(switches.storage);
        assert!(!switches.memory);
        assert!(!switches.companion);
        assert!(!switches.shell);
        assert!(!switches.voice);
        assert!(!switches.weixin);
    }

    #[test]
    fn voice_and_weixin_are_stable_settings_with_expected_keys() {
        assert_eq!(CapabilitySetting::ALL.len(), 15);
        assert_eq!(CapabilitySetting::Voice.as_str(), "voice");
        assert_eq!(CapabilitySetting::Weixin.as_str(), "weixin");
        assert_eq!(
            CapabilitySetting::parse("voice"),
            Some(CapabilitySetting::Voice)
        );
        assert_eq!(
            CapabilitySetting::parse("weixin"),
            Some(CapabilitySetting::Weixin)
        );
    }

    #[test]
    fn overrides_get_set_unset_and_merge_cover_voice_and_weixin() {
        let mut base = CapabilityOverrides::default();
        base.set(CapabilitySetting::Voice, true);
        assert_eq!(base.get(CapabilitySetting::Voice), Some(true));

        let mut patch = CapabilityOverrides::default();
        patch.set(CapabilitySetting::Weixin, true);
        base.merge(&patch);
        assert_eq!(base.get(CapabilitySetting::Voice), Some(true));
        assert_eq!(base.get(CapabilitySetting::Weixin), Some(true));

        base.unset(CapabilitySetting::Voice);
        assert_eq!(base.get(CapabilitySetting::Voice), None);
    }

    #[test]
    fn old_serialized_overrides_without_new_fields_remain_compatible() {
        let old = serde_json::json!({ "memory": true, "shell": false });
        let overrides: CapabilityOverrides = serde_json::from_value(old).expect("old settings");
        assert_eq!(overrides.get(CapabilitySetting::Memory), Some(true));
        assert_eq!(overrides.get(CapabilitySetting::Voice), None);
        assert_eq!(overrides.get(CapabilitySetting::Weixin), None);
        let switches = CapabilitySwitches::from_overrides(&overrides);
        assert!(!switches.voice);
        assert!(!switches.weixin);
    }

    #[test]
    fn companion_supplies_mailbox_and_scheduler_defaults_only_when_unset() {
        let mut overrides = CapabilityOverrides::default();
        overrides.set(CapabilitySetting::Companion, true);
        let switches = CapabilitySwitches::from_overrides(&overrides);
        assert!(switches.mailbox);
        assert!(switches.scheduler);

        overrides.set(CapabilitySetting::Mailbox, false);
        let switches = CapabilitySwitches::from_overrides(&overrides);
        assert!(!switches.mailbox);
        assert!(switches.scheduler);
    }

    #[test]
    fn explicit_next_environment_values_override_file_and_legacy_values() {
        let mut overrides = CapabilityOverrides::default();
        overrides.set(CapabilitySetting::Persona, false);
        overrides.set(CapabilitySetting::Shell, true);
        let values = BTreeMap::from([
            ("YUNXI_PERSONA_ENABLED", "false"),
            ("YUNXI_NEXT_PERSONA_ENABLED", "true"),
            ("YUNXI_NEXT_SHELL_ENABLED", "false"),
            ("YUNXI_NEXT_VOICE_ENABLED", "on"),
            ("YUNXI_NEXT_WEIXIN_ENABLED", "1"),
        ]);
        let switches = CapabilitySwitches::from_overrides(&overrides)
            .with_environment(|name| values.get(name).map(|value| (*value).to_string()));
        assert!(switches.persona);
        assert!(!switches.shell);
        assert!(switches.voice);
        assert!(switches.weixin);
    }
}
