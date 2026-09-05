//! Built-in plugin metadata and startup policy for the CLI Host.
//!
//! This module is deliberately local to the adapter.  The composition and
//! settings crates describe data and persistence; this is the boundary where
//! those values become a concrete Host boot decision.

use std::env;

use yunxi_composition::{PluginManifest, PluginRisk};
use yunxi_settings::{CapabilitySetting, CapabilitySettingsStore, CapabilitySwitches};

use super::{
    COMPANION_PLUGIN_ID, CONTEXT_PLUGIN_ID, FILES_PLUGIN_ID, MAILBOX_PLUGIN_ID, MCP_PLUGIN_ID,
    MEMORY_PLUGIN_ID, MODEL_PLUGIN_ID, MULTI_AGENT_PLUGIN_ID, PATCH_PLUGIN_ID, PERSONA_PLUGIN_ID,
    SCHEDULER_PLUGIN_ID, SHELL_PLUGIN_ID, SKILLS_PLUGIN_ID, STORAGE_PLUGIN_ID,
    VOICE_FIXTURE_PLUGIN_ID, WEIXIN_PLUGIN_ID,
};

/// The resolved enablement values used by one Host generation.
///
/// The fields intentionally mirror the legacy capability switches so the
/// existing launch code remains the single place that wires capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PluginSwitches {
    pub(super) context: bool,
    pub(super) persona: bool,
    pub(super) memory: bool,
    pub(super) companion: bool,
    pub(super) storage: bool,
    pub(super) mailbox: bool,
    pub(super) scheduler: bool,
    pub(super) shell: bool,
    pub(super) patch: bool,
    pub(super) files: bool,
    pub(super) mcp: bool,
    pub(super) skills: bool,
    pub(super) multi_agent: bool,
    pub(super) voice: bool,
    pub(super) weixin: bool,
}

impl PluginSwitches {
    pub(super) fn resolve(settings: &CapabilitySettingsStore, legacy: &CapabilitySwitches) -> Self {
        Self {
            context: resolve_optional(
                settings,
                legacy,
                CONTEXT_PLUGIN_ID,
                CapabilitySetting::Context,
            ),
            persona: resolve_optional(
                settings,
                legacy,
                PERSONA_PLUGIN_ID,
                CapabilitySetting::Persona,
            ),
            memory: resolve_optional(
                settings,
                legacy,
                MEMORY_PLUGIN_ID,
                CapabilitySetting::Memory,
            ),
            companion: resolve_optional(
                settings,
                legacy,
                COMPANION_PLUGIN_ID,
                CapabilitySetting::Companion,
            ),
            storage: resolve_optional(
                settings,
                legacy,
                STORAGE_PLUGIN_ID,
                CapabilitySetting::Storage,
            ),
            mailbox: resolve_optional(
                settings,
                legacy,
                MAILBOX_PLUGIN_ID,
                CapabilitySetting::Mailbox,
            ),
            scheduler: resolve_optional(
                settings,
                legacy,
                SCHEDULER_PLUGIN_ID,
                CapabilitySetting::Scheduler,
            ),
            shell: resolve_optional(settings, legacy, SHELL_PLUGIN_ID, CapabilitySetting::Shell),
            patch: resolve_optional(settings, legacy, PATCH_PLUGIN_ID, CapabilitySetting::Patch),
            files: resolve_optional(settings, legacy, FILES_PLUGIN_ID, CapabilitySetting::Files),
            mcp: resolve_optional(settings, legacy, MCP_PLUGIN_ID, CapabilitySetting::Mcp),
            skills: resolve_optional(
                settings,
                legacy,
                SKILLS_PLUGIN_ID,
                CapabilitySetting::Skills,
            ),
            multi_agent: resolve_optional(
                settings,
                legacy,
                MULTI_AGENT_PLUGIN_ID,
                CapabilitySetting::MultiAgent,
            ),
            voice: resolve_optional(
                settings,
                legacy,
                VOICE_FIXTURE_PLUGIN_ID,
                CapabilitySetting::Voice,
            ),
            weixin: resolve_optional(
                settings,
                legacy,
                WEIXIN_PLUGIN_ID,
                CapabilitySetting::Weixin,
            ),
        }
    }
}

/// Returns the machine-readable policy for a built-in entry.
pub(super) fn manifest_for(plugin_id: &str) -> PluginManifest {
    match plugin_id {
        // The model is the replaceable Agent-spine implementation in the
        // current CLI.  It is a required path and is never user-toggleable.
        MODEL_PLUGIN_ID => PluginManifest::agent_spine(),
        // These plugins only compile/read in-memory prompt context.  They are
        // the safe optional defaults; an explicit user choice still wins.
        CONTEXT_PLUGIN_ID | PERSONA_PLUGIN_ID => PluginManifest::optional(PluginRisk::None),
        // Session storage is local host infrastructure and was enabled by
        // default before plugin overrides existed.  Keep that compatibility
        // default while treating network/process/device capabilities as
        // external below.
        STORAGE_PLUGIN_ID => PluginManifest::optional(PluginRisk::None),
        // The current composition manifest has a conservative external class
        // (including high-risk capabilities).  Such entries default off.
        MEMORY_PLUGIN_ID
        | COMPANION_PLUGIN_ID
        | MAILBOX_PLUGIN_ID
        | SCHEDULER_PLUGIN_ID
        | SHELL_PLUGIN_ID
        | PATCH_PLUGIN_ID
        | FILES_PLUGIN_ID
        | MCP_PLUGIN_ID
        | SKILLS_PLUGIN_ID
        | MULTI_AGENT_PLUGIN_ID
        | WEIXIN_PLUGIN_ID => PluginManifest::optional(PluginRisk::External),
        // Voice is represented by a deterministic fixture today, but the
        // production contract is device-facing. Keep it opt-in so enabling a
        // future microphone/speaker adapter always remains an explicit choice.
        VOICE_FIXTURE_PLUGIN_ID => PluginManifest::optional(PluginRisk::External),
        // Unknown entries are never assumed safe at this boundary.
        _ => PluginManifest::optional(PluginRisk::External),
    }
}

/// Resolves one optional plugin without allowing a legacy switch to override
/// an explicit total-switch choice.
fn resolve_optional(
    settings: &CapabilitySettingsStore,
    legacy: &CapabilitySwitches,
    plugin_id: &str,
    capability: CapabilitySetting,
) -> bool {
    let manifest = manifest_for(plugin_id);
    resolve_value(
        manifest,
        settings.plugin_overrides().get(plugin_id).copied(),
        legacy_capability_override(settings, legacy, capability),
    )
}

/// Applies the precedence rules without reading process state.
///
/// Keeping this operation pure makes the policy auditable and keeps tests
/// independent of unrelated `YUNXI_*` variables inherited by the test runner.
fn resolve_value(
    manifest: PluginManifest,
    plugin_override: Option<bool>,
    legacy_override: Option<bool>,
) -> bool {
    if !manifest.role().is_user_toggleable() {
        true
    } else {
        plugin_override
            .or(legacy_override)
            .unwrap_or_else(|| manifest.default_enabled())
    }
}

/// Finds an explicitly supplied legacy setting while preserving the old
/// environment-over-file precedence.  A value synthesized by
/// `CapabilitySwitches` (for example mailbox/scheduler from companion) is
/// accepted only when the source capability was explicit.
fn legacy_capability_override(
    settings: &CapabilitySettingsStore,
    legacy: &CapabilitySwitches,
    setting: CapabilitySetting,
) -> Option<bool> {
    legacy_capability_override_with(settings, legacy, setting, read_bool)
}

fn legacy_capability_override_with<F>(
    settings: &CapabilitySettingsStore,
    legacy: &CapabilitySwitches,
    setting: CapabilitySetting,
    read: F,
) -> Option<bool>
where
    F: Fn(&str) -> Option<bool> + Copy,
{
    let (primary, legacy_name) = environment_names(setting);
    if let Some(value) = read(primary).or_else(|| legacy_name.and_then(read)) {
        return Some(value);
    }
    if settings.overrides().get(setting).is_some() {
        return Some(legacy.get(setting));
    }

    if matches!(
        setting,
        CapabilitySetting::Mailbox | CapabilitySetting::Scheduler
    ) && (settings
        .overrides()
        .get(CapabilitySetting::Companion)
        .is_some()
        || read("YUNXI_NEXT_COMPANION_ENABLED")
            .or_else(|| read("YUNXI_COMPANION_ENABLED"))
            .is_some())
    {
        return Some(legacy.get(setting));
    }
    None
}

fn environment_names(setting: CapabilitySetting) -> (&'static str, Option<&'static str>) {
    match setting {
        CapabilitySetting::Context => ("YUNXI_NEXT_CONTEXT_ENABLED", None),
        CapabilitySetting::Persona => ("YUNXI_NEXT_PERSONA_ENABLED", Some("YUNXI_PERSONA_ENABLED")),
        CapabilitySetting::Memory => ("YUNXI_NEXT_MEMORY_ENABLED", Some("YUNXI_MEMORY_ENABLED")),
        CapabilitySetting::Companion => (
            "YUNXI_NEXT_COMPANION_ENABLED",
            Some("YUNXI_COMPANION_ENABLED"),
        ),
        CapabilitySetting::Storage => ("YUNXI_NEXT_STORAGE_ENABLED", None),
        CapabilitySetting::Mailbox => ("YUNXI_NEXT_MAILBOX_ENABLED", None),
        CapabilitySetting::Scheduler => ("YUNXI_NEXT_SCHEDULER_ENABLED", None),
        CapabilitySetting::Shell => ("YUNXI_NEXT_SHELL_ENABLED", None),
        CapabilitySetting::Patch => ("YUNXI_NEXT_PATCH_ENABLED", None),
        CapabilitySetting::Files => ("YUNXI_NEXT_FILES_ENABLED", None),
        CapabilitySetting::Mcp => ("YUNXI_NEXT_MCP_ENABLED", None),
        CapabilitySetting::Skills => ("YUNXI_NEXT_SKILLS_ENABLED", None),
        CapabilitySetting::MultiAgent => ("YUNXI_NEXT_MULTI_AGENT_ENABLED", None),
        CapabilitySetting::Voice => ("YUNXI_NEXT_VOICE_ENABLED", None),
        CapabilitySetting::Weixin => ("YUNXI_NEXT_WEIXIN_ENABLED", None),
    }
}

fn read_bool(name: &str) -> Option<bool> {
    match env::var(name).ok()?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use yunxi_settings::CapabilityEdit;

    fn empty_settings() -> CapabilitySettingsStore {
        CapabilitySettingsStore::load(std::env::temp_dir().join(format!(
            "yunxi-policy-test-{}-{}",
            std::process::id(),
            unique_stamp()
        )))
    }

    fn unique_stamp() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    }

    #[test]
    fn required_model_manifest_is_always_enabled() {
        let manifest = manifest_for(MODEL_PLUGIN_ID);
        assert!(manifest.default_enabled());
        assert!(!manifest.role().is_user_toggleable());
    }

    #[test]
    fn safe_and_external_defaults_are_distinct() {
        let safe = manifest_for(CONTEXT_PLUGIN_ID);
        assert!(safe.default_enabled());
        assert!(manifest_for(PERSONA_PLUGIN_ID).default_enabled());
        assert!(manifest_for(STORAGE_PLUGIN_ID).default_enabled());
        assert!(resolve_value(safe, None, None));

        let external = manifest_for(SHELL_PLUGIN_ID);
        assert!(!external.default_enabled());
        assert!(!manifest_for(MULTI_AGENT_PLUGIN_ID).default_enabled());
        assert!(!resolve_value(external, None, None));
    }

    #[test]
    fn required_entries_ignore_disable_choices() {
        assert!(resolve_value(
            manifest_for(MODEL_PLUGIN_ID),
            Some(false),
            Some(false)
        ));
    }

    #[test]
    fn plugin_override_wins_over_legacy_value() {
        let manifest = manifest_for(SHELL_PLUGIN_ID);
        assert!(!resolve_value(manifest, Some(false), Some(true)));
        assert!(resolve_value(manifest, Some(true), Some(false)));

        let mut settings = empty_settings();
        settings
            .set_plugin(SHELL_PLUGIN_ID, false, Some(0))
            .expect("plugin override");
        let legacy = CapabilitySwitches {
            shell: true,
            ..CapabilitySwitches::default()
        };
        assert!(!resolve_optional(
            &settings,
            &legacy,
            SHELL_PLUGIN_ID,
            CapabilitySetting::Shell
        ));
        let _ignored = std::fs::remove_file(settings.path());
    }

    #[test]
    fn absent_plugin_override_uses_explicit_legacy_value() {
        let mut settings = empty_settings();
        settings
            .mutate(
                [CapabilityEdit::Set(CapabilitySetting::Shell, true)],
                Some(0),
            )
            .expect("legacy capability override");
        let legacy = settings.resolved();
        let legacy_override =
            legacy_capability_override_with(&settings, &legacy, CapabilitySetting::Shell, |_| None);
        assert!(resolve_value(
            manifest_for(SHELL_PLUGIN_ID),
            settings.plugin_overrides().get(SHELL_PLUGIN_ID).copied(),
            legacy_override,
        ));
        let _ignored = std::fs::remove_file(settings.path());
    }

    #[test]
    fn legacy_environment_precedence_is_preserved_without_global_environment() {
        let settings = empty_settings();
        let legacy = CapabilitySwitches::default();
        let value =
            legacy_capability_override_with(&settings, &legacy, CapabilitySetting::Shell, |name| {
                match name {
                    "YUNXI_NEXT_SHELL_ENABLED" => Some(false),
                    _ => None,
                }
            });
        assert_eq!(value, Some(false));
        let _ignored = std::fs::remove_file(settings.path());
    }
}
