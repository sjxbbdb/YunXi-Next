//! Standalone CLI control commands.

use std::env;
use std::error::Error;
use std::fmt;

use serde_json::{Value, json};
use yunxi_composition::{PluginFiberPhase, PluginInventoryEntry, PluginInventorySnapshot};
use yunxi_settings::{CapabilitySettingsError, CapabilitySettingsStore};

use crate::args::{ControlCommand, ControlOptions};
use crate::session::{ChatSession, SessionError, configured_dynamic_plugin_root};

const SCHEMA_VERSION: u64 = 1;
const MAX_WARNINGS: usize = 32;
const MAX_KNOWN_IDS: usize = 64;

pub(crate) fn run(options: ControlOptions) -> Result<(), ControlError> {
    let value = execute(&options.command)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| ControlError::internal(error.to_string()))?
        );
    } else {
        render_human(&value);
    }
    Ok(())
}

fn execute(command: &ControlCommand) -> Result<Value, ControlError> {
    let mut settings = CapabilitySettingsStore::from_environment();
    execute_with_settings(command, &mut settings)
}

fn execute_with_settings(
    command: &ControlCommand,
    settings: &mut CapabilitySettingsStore,
) -> Result<Value, ControlError> {
    match command {
        ControlCommand::Status => status(settings, false),
        ControlCommand::Diagnostics => status(settings, true),
        ControlCommand::Enable(id) => set_enabled(settings, id, true),
        ControlCommand::Disable(id) => set_enabled(settings, id, false),
        ControlCommand::Reload(id) => reload(settings, id.as_deref()),
    }
}

#[derive(Debug)]
pub(crate) struct ControlError {
    code: String,
    message: String,
    details: Value,
}

impl ControlError {
    fn new(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new("control-internal", message, json!({}))
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "details": self.details,
            }
        });
        match serde_json::to_string(&value) {
            Ok(value) => formatter.write_str(&value),
            Err(_) => write!(formatter, "{}: {}", self.code, self.message),
        }
    }
}

impl Error for ControlError {}

fn status(
    settings: &mut CapabilitySettingsStore,
    diagnostics: bool,
) -> Result<Value, ControlError> {
    let inventory = ChatSession::plugin_inventory_for_settings(settings).map_err(session_error)?;
    let plugins = inventory
        .entries()
        .iter()
        .map(|entry| plugin_value(entry, settings))
        .collect::<Vec<_>>();
    let warnings = settings
        .take_warnings()
        .into_iter()
        .take(MAX_WARNINGS)
        .collect::<Vec<_>>();
    let api_key_configured = [
        "YUNXI_PROVIDER_API_KEY",
        "DEEPSEEK_API_KEY",
        "OPENAI_API_KEY",
    ]
    .into_iter()
    .any(env_configured);
    let mut value = json!({
        "schemaVersion": SCHEMA_VERSION,
        "ok": true,
        "command": if diagnostics { "diagnostics" } else { "status" },
        "runtime": detached_runtime(),
        "settings": {
            "path": settings.path().to_string_lossy(),
            "revision": settings.revision(),
            "pluginOverrides": settings.plugin_overrides(),
        },
        "plugins": plugins,
    });
    if diagnostics {
        let dynamic_root = configured_dynamic_plugin_root();
        let dynamic_count = plugins
            .iter()
            .filter(|plugin| {
                plugin["moduleName"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("yunxi.dynamic."))
            })
            .count();
        value["diagnostics"] = json!({
            "warnings": warnings,
            "dynamicPluginDiscovery": {
                "configured": dynamic_root.is_some(),
                "root": dynamic_root.map(|path| path.to_string_lossy().into_owned()),
                "acceptedPackages": dynamic_count,
            },
            "liveHostControl": "adapter-required",
            "streaming": "available-as-library-adapter",
            "environment": {
                "providerProfileConfigured": env_configured("YUNXI_PROVIDER_PROFILE"),
                "providerBaseUrlConfigured": env_configured("YUNXI_PROVIDER_BASE_URL"),
                "modelConfigured": env_configured("YUNXI_AGENT_MODEL"),
                "apiKeyConfigured": api_key_configured,
            },
        });
    }
    Ok(value)
}

fn set_enabled(
    settings: &mut CapabilitySettingsStore,
    requested_id: &str,
    enabled: bool,
) -> Result<Value, ControlError> {
    validate_id(requested_id)?;
    let inventory = ChatSession::plugin_inventory_for_settings(settings).map_err(session_error)?;
    let entry = resolve_entry(&inventory, requested_id)?;
    let Some(manifest) = entry.manifest() else {
        return Err(ControlError::new(
            "plugin-policy-unavailable",
            "plugin has no user-facing manifest",
            json!({ "pluginId": entry.entry_id() }),
        ));
    };
    if !manifest.role().is_user_toggleable() {
        return Err(ControlError::new(
            "required-plugin",
            "required runtime plugins cannot be toggled",
            json!({
                "pluginId": entry.entry_id(),
                "role": role_name(manifest.role()),
            }),
        ));
    }
    let plugin_id = entry.entry_id().to_string();
    let revision = settings.revision();
    let changed = settings
        .set_plugin(plugin_id.clone(), enabled, Some(revision))
        .map_err(settings_error)?;
    let effective = ChatSession::plugin_inventory_for_settings(settings)
        .map_err(session_error)?
        .entries()
        .iter()
        .find(|candidate| candidate.entry_id().as_str() == plugin_id)
        .is_some_and(PluginInventoryEntry::enabled);
    Ok(json!({
        "schemaVersion": SCHEMA_VERSION,
        "ok": true,
        "command": if enabled { "enable" } else { "disable" },
        "pluginId": plugin_id,
        "requestedPluginId": requested_id,
        "enabled": effective,
        "changed": changed,
        "persisted": true,
        "applied": false,
        "liveApplied": false,
        "requiresRestart": changed,
        "settingsRevision": settings.revision(),
        "runtime": detached_runtime(),
    }))
}

fn reload(
    settings: &mut CapabilitySettingsStore,
    requested_id: Option<&str>,
) -> Result<Value, ControlError> {
    let inventory = ChatSession::plugin_inventory_for_settings(settings).map_err(session_error)?;
    let target = requested_id
        .map(|id| {
            validate_id(id)?;
            let entry = resolve_entry(&inventory, id)?;
            Ok(json!({
                "pluginId": entry.entry_id(),
                "requestedPluginId": id,
                "configured": entry.enabled(),
                "routeAvailable": entry_is_active(entry),
                "active": entry_is_active(entry),
            }))
        })
        .transpose()?;
    Ok(json!({
        "schemaVersion": SCHEMA_VERSION,
        "ok": true,
        "command": "reload",
        "validated": true,
        "applied": false,
        "liveApplied": false,
        "requiresRestart": true,
        "target": target,
        "runtime": detached_runtime(),
            "reason": "standalone CLI has no attached Host; restart yunxi-next to apply settings and rescan the dynamic plugin directory",
            "dynamicPluginDirectory": configured_dynamic_plugin_root()
                .map(|path| path.to_string_lossy().into_owned()),
    }))
}

fn resolve_entry<'a>(
    inventory: &'a PluginInventorySnapshot,
    requested_id: &str,
) -> Result<&'a PluginInventoryEntry, ControlError> {
    let matches = inventory
        .entries()
        .iter()
        .filter(|entry| {
            entry.entry_id().as_str() == requested_id
                || entry.module_name() == requested_id
                || entry
                    .entry_id()
                    .as_str()
                    .strip_prefix("yunxi.")
                    .is_some_and(|short| short == requested_id)
                || entry
                    .module_name()
                    .rsplit('.')
                    .next()
                    .is_some_and(|tail| tail == requested_id)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [entry] => Ok(entry),
        [] => Err(ControlError::new(
            "plugin-not-found",
            "plugin is not in the configured YunXi Next composition or dynamic plugin directory",
            json!({
                "requestedPluginId": requested_id,
                "knownPluginIds": inventory
                    .entries()
                    .iter()
                    .take(MAX_KNOWN_IDS)
                    .map(|entry| entry.entry_id())
                    .collect::<Vec<_>>(),
                "dynamicDiscovery": {
                    "configured": configured_dynamic_plugin_root().is_some(),
                },
            }),
        )),
        _ => Err(ControlError::new(
            "ambiguous-plugin",
            "short plugin name matches more than one entry",
            json!({
                "requestedPluginId": requested_id,
                "matches": matches.iter().map(|entry| entry.entry_id()).collect::<Vec<_>>(),
            }),
        )),
    }
}

fn validate_id(id: &str) -> Result<(), ControlError> {
    if id.is_empty()
        || id.len() > 128
        || id.chars().any(|character| {
            !character.is_ascii() || character.is_control() || character.is_whitespace()
        })
    {
        return Err(ControlError::new(
            "invalid-plugin-id",
            "plugin id must be non-empty ASCII text without whitespace",
            json!({ "pluginId": id }),
        ));
    }
    Ok(())
}

fn plugin_value(entry: &PluginInventoryEntry, settings: &CapabilitySettingsStore) -> Value {
    let enabled = entry.enabled();
    let manifest = entry.manifest();
    let active = entry_is_active(entry);
    json!({
        "entryId": entry.entry_id(),
        "moduleName": entry.module_name(),
        "enabled": enabled,
        "effectiveEnabled": enabled,
        "fiberPhase": entry.fiber_phase().map(fiber_phase_name),
        "routeAvailable": active,
        "routeState": route_state(entry),
        "active": active,
        "settingsOverride": settings.plugin_overrides().get(entry.entry_id().as_str()),
        "role": manifest.map(|value| role_name(value.role())),
        "risk": manifest.map(|value| risk_name(value.risk())),
        "defaultEnabled": entry.default_enabled(),
    })
}

fn entry_is_active(entry: &PluginInventoryEntry) -> bool {
    matches!(entry.fiber_phase(), Some(PluginFiberPhase::Active))
}

fn route_state(entry: &PluginInventoryEntry) -> &'static str {
    match entry.fiber_phase() {
        Some(PluginFiberPhase::Disabled) => "disabled",
        Some(PluginFiberPhase::Pending | PluginFiberPhase::Loading) => "starting",
        Some(PluginFiberPhase::Active) => "active",
        Some(PluginFiberPhase::Failed) => "failed",
        Some(PluginFiberPhase::Unloading) => "stopping",
        None => "unobserved",
    }
}

fn detached_runtime() -> Value {
    json!({
        "state": "detached",
        "attached": false,
        "live": false,
        "control": "standalone-settings-adapter",
    })
}

fn fiber_phase_name(phase: PluginFiberPhase) -> &'static str {
    match phase {
        PluginFiberPhase::Disabled => "disabled",
        PluginFiberPhase::Pending => "pending",
        PluginFiberPhase::Loading => "loading",
        PluginFiberPhase::Active => "active",
        PluginFiberPhase::Failed => "failed",
        PluginFiberPhase::Unloading => "unloading",
    }
}

fn role_name(role: yunxi_composition::PluginRole) -> &'static str {
    match role {
        yunxi_composition::PluginRole::Core => "core",
        yunxi_composition::PluginRole::AgentSpine => "agent-spine",
        yunxi_composition::PluginRole::Optional => "optional",
    }
}

fn risk_name(risk: yunxi_composition::PluginRisk) -> &'static str {
    match risk {
        yunxi_composition::PluginRisk::None => "none",
        yunxi_composition::PluginRisk::External => "external",
    }
}

fn env_configured(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn session_error(error: SessionError) -> ControlError {
    ControlError::new(
        "composition-unavailable",
        "could not build configured plugin composition",
        json!({ "message": error.to_string() }),
    )
}

fn settings_error(error: CapabilitySettingsError) -> ControlError {
    let message = error.to_string();
    let code = match &error {
        CapabilitySettingsError::Conflict { .. } => "settings-conflict",
        CapabilitySettingsError::InvalidPluginId { .. } => "invalid-plugin-id",
        _ => "settings-write-failed",
    };
    ControlError::new(
        code,
        "could not persist plugin setting",
        json!({ "message": message }),
    )
}

fn render_human(value: &Value) {
    println!(
        "{} (runtime: {})",
        value["command"].as_str().unwrap_or("control"),
        value["runtime"]["state"].as_str().unwrap_or("unknown")
    );
    if let Some(plugin_id) = value["pluginId"].as_str() {
        println!(
            "plugin: {plugin_id} enabled={} persisted={} restart-required={}",
            value["enabled"].as_bool().unwrap_or(false),
            value["persisted"].as_bool().unwrap_or(false),
            value["requiresRestart"].as_bool().unwrap_or(false)
        );
        return;
    }
    if value["command"] == "reload" {
        println!(
            "validated={} live-applied={} restart-required={}",
            value["validated"].as_bool().unwrap_or(false),
            value["liveApplied"].as_bool().unwrap_or(false),
            value["requiresRestart"].as_bool().unwrap_or(false)
        );
        return;
    }
    println!(
        "settings revision: {}",
        value["settings"]["revision"].as_u64().unwrap_or_default()
    );
    if let Some(plugins) = value["plugins"].as_array() {
        for plugin in plugins {
            println!(
                "- {}: {}",
                plugin["entryId"].as_str().unwrap_or("unknown"),
                if plugin["enabled"].as_bool().unwrap_or(false) {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use yunxi_composition::{DefaultEnablement, PluginManifest, PluginRisk, PluginRole};

    fn entry(phase: PluginFiberPhase) -> PluginInventoryEntry {
        PluginInventoryEntry::external(
            "fixture.plugin",
            "yunxi.dynamic.fixture.plugin",
            true,
            Some(phase),
            PluginManifest::new(
                PluginRole::Optional,
                PluginRisk::None,
                DefaultEnablement::Safe,
            ),
        )
        .expect("inventory entry")
    }

    #[test]
    fn detached_status_distinguishes_configured_from_active_routes() {
        let settings = CapabilitySettingsStore::load(PathBuf::from(
            "C:/yunxi-next-control-test-settings.json",
        ));
        let active = plugin_value(&entry(PluginFiberPhase::Active), &settings);
        assert_eq!(active["effectiveEnabled"], true);
        assert_eq!(active["routeAvailable"], true);
        assert_eq!(active["active"], true);
        assert_eq!(active["routeState"], "active");

        let pending = plugin_value(&entry(PluginFiberPhase::Pending), &settings);
        assert_eq!(pending["effectiveEnabled"], true);
        assert_eq!(pending["routeAvailable"], false);
        assert_eq!(pending["active"], false);
        assert_eq!(pending["routeState"], "starting");
    }
}
