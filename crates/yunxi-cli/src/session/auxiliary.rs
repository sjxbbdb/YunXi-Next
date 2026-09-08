//! Narrow process-Host adapters for standalone capability commands.
//!
//! Management commands must not instantiate capability implementations in the
//! CLI process. These adapters reuse the session launch policy without
//! starting the mandatory model plugin.

use std::env;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use yunxi_companion::COMPANION_PLUGIN_ID;
use yunxi_kernel::{PluginCommand, PluginId};
use yunxi_memory::MEMORY_PLUGIN_ID;
use yunxi_persona::PERSONA_PLUGIN_ID;
use yunxi_plugin_host::ProcessPluginHost;
use yunxi_protocol::{CapabilityDescriptor, GrantKind, capabilities};
use yunxi_settings::CapabilitySettingsStore;
use yunxi_storage::STORAGE_PLUGIN_ID;

use super::{
    ACTION_RESPONSE_TIMEOUT, COMPANION_PLUGIN_PATH_ENV, INTERNAL_COMPANION_PLUGIN_ARGUMENT,
    INTERNAL_MEMORY_PLUGIN_ARGUMENT, INTERNAL_PERSONA_PLUGIN_ARGUMENT,
    INTERNAL_STORAGE_PLUGIN_ARGUMENT, INTERNAL_VOICE_PLUGIN_ARGUMENT,
    INTERNAL_WEIXIN_PLUGIN_ARGUMENT, MEMORY_PLUGIN_PATH_ENV, OptionalLaunchPolicy,
    PERSONA_PLUGIN_PATH_ENV, PluginSwitches, READ_ONLY_RESPONSE_TIMEOUT, STORAGE_PLUGIN_PATH_ENV,
    VOICE_FIXTURE_PLUGIN_ID, VOICE_PLUGIN_PATH_ENV, WEIXIN_PLUGIN_ID, WEIXIN_PLUGIN_PATH_ENV,
    descriptor, launch_optional_with_expected_capabilities, optional_command,
    optional_secondary_capability, optional_voice_command, optional_weixin_command,
};

struct StandaloneCapabilityHost {
    host: ProcessPluginHost,
    capability: CapabilityDescriptor,
}

impl StandaloneCapabilityHost {
    #[allow(clippy::too_many_arguments)]
    fn launch(
        cwd: &Path,
        id: PluginId,
        display_name: &str,
        command: PluginCommand,
        capability: CapabilityDescriptor,
        expected: &[CapabilityDescriptor],
        required_grants: &[GrantKind],
        label: &str,
    ) -> Result<Self, String> {
        let mut host = ProcessPluginHost::new();
        let mut notices = Vec::new();
        let launched = launch_optional_with_expected_capabilities(
            &mut host,
            id,
            display_name,
            command.current_dir(cwd),
            &capability,
            expected,
            OptionalLaunchPolicy {
                required_grants,
                read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
            },
            &mut notices,
        );
        if !launched {
            return Err(launch_failure(label, notices));
        }
        Ok(Self { host, capability })
    }

    fn invoke<Request, Response>(
        &mut self,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.host
            .invoke(&self.capability, operation, payload)
            .map_err(|error| error.to_string())
    }
}

/// A standalone Memory process exposing only its management route to the caller.
pub(crate) struct MemoryPluginHost(StandaloneCapabilityHost);

impl MemoryPluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let switches = effective_switches();
        require_enabled("memory", switches.memory)?;
        let executable = current_executable()?;
        let recall = descriptor(
            capabilities::MEMORY_RECALL,
            capabilities::MEMORY_RECALL_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let write = descriptor(
            capabilities::MEMORY_WRITE,
            capabilities::MEMORY_WRITE_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let management = descriptor(
            capabilities::MEMORY_MANAGEMENT,
            capabilities::MEMORY_MANAGEMENT_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let expected = [recall, write, management.clone()];
        StandaloneCapabilityHost::launch(
            cwd,
            PluginId::new(MEMORY_PLUGIN_ID).map_err(|error| error.to_string())?,
            "Long-term memory",
            optional_command(
                &executable,
                INTERNAL_MEMORY_PLUGIN_ARGUMENT,
                MEMORY_PLUGIN_PATH_ENV,
            ),
            management,
            &expected,
            &[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite],
            "memory",
        )
        .map(Self)
    }

    pub(crate) fn invoke<Request, Response>(
        &mut self,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.0.invoke(operation, payload)
    }
}

/// A standalone Persona process exposing its bounded management contract.
pub(crate) struct PersonaPluginHost(StandaloneCapabilityHost);

impl PersonaPluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let switches = effective_switches();
        require_enabled("persona", switches.persona)?;
        let executable = current_executable()?;
        let context = descriptor(
            capabilities::PERSONA_CONTEXT,
            capabilities::PERSONA_CONTEXT_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let management = descriptor(
            capabilities::PERSONA_MANAGEMENT,
            capabilities::PERSONA_MANAGEMENT_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let expected = [context, management.clone()];
        StandaloneCapabilityHost::launch(
            cwd,
            PluginId::new(PERSONA_PLUGIN_ID).map_err(|error| error.to_string())?,
            "Persona context compiler",
            optional_command(
                &executable,
                INTERNAL_PERSONA_PLUGIN_ARGUMENT,
                PERSONA_PLUGIN_PATH_ENV,
            ),
            management,
            &expected,
            &[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite],
            "persona",
        )
        .map(Self)
    }

    pub(crate) fn invoke<Request, Response>(
        &mut self,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.0.invoke(operation, payload)
    }
}

/// A standalone Companion process for decision history and switch management.
pub(crate) struct CompanionPluginHost(StandaloneCapabilityHost);

impl CompanionPluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let switches = effective_switches();
        require_enabled("companion", switches.companion)?;
        let executable = current_executable()?;
        let decision = descriptor(
            capabilities::COMPANION_DECIDE,
            capabilities::COMPANION_DECIDE_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let management = descriptor(
            capabilities::COMPANION_MANAGEMENT,
            capabilities::COMPANION_MANAGEMENT_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let expected = [decision, management.clone()];
        StandaloneCapabilityHost::launch(
            cwd,
            PluginId::new(COMPANION_PLUGIN_ID).map_err(|error| error.to_string())?,
            "Deterministic companion policy",
            optional_command(
                &executable,
                INTERNAL_COMPANION_PLUGIN_ARGUMENT,
                COMPANION_PLUGIN_PATH_ENV,
            ),
            management,
            &expected,
            &[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite],
            "companion",
        )
        .map(Self)
    }

    pub(crate) fn invoke<Request, Response>(
        &mut self,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.0.invoke(operation, payload)
    }
}

/// A standalone Storage process for session list/load/mutation commands.
pub(crate) struct StoragePluginHost(StandaloneCapabilityHost);

impl StoragePluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let switches = effective_switches();
        require_enabled("storage", switches.storage)?;
        let executable = current_executable()?;
        let capability = descriptor(
            capabilities::STORAGE_SESSIONS,
            capabilities::STORAGE_SESSIONS_VERSION,
        )
        .map_err(|error| error.to_string())?;
        StandaloneCapabilityHost::launch(
            cwd,
            PluginId::new(STORAGE_PLUGIN_ID).map_err(|error| error.to_string())?,
            "Persistent conversation sessions",
            optional_command(
                &executable,
                INTERNAL_STORAGE_PLUGIN_ARGUMENT,
                STORAGE_PLUGIN_PATH_ENV,
            ),
            capability.clone(),
            std::slice::from_ref(&capability),
            &[GrantKind::WorkspaceRead, GrantKind::WorkspaceWrite],
            "storage",
        )
        .map(Self)
    }

    pub(crate) fn invoke<Request, Response>(
        &mut self,
        operation: &str,
        payload: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        self.0.invoke(operation, payload)
    }
}

/// A standalone, supervised Voice process with the same route policy as a chat session.
pub(crate) struct VoicePluginHost {
    host: ProcessPluginHost,
    transcribe: CapabilityDescriptor,
    synthesize: Option<CapabilityDescriptor>,
}

impl VoicePluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let settings = CapabilitySettingsStore::from_environment();
        let switches = PluginSwitches::resolve(&settings, &settings.effective_from_environment());
        if !switches.voice {
            return Err("voice plugin is disabled; enable it before granting device access".into());
        }

        let executable = env::current_exe()
            .map_err(|error| format!("failed to resolve the YunXi executable: {error}"))?;
        let id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID).map_err(|error| error.to_string())?;
        let transcribe = descriptor(
            capabilities::VOICE_TRANSCRIBE,
            capabilities::VOICE_TRANSCRIBE_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let synthesize = descriptor(
            capabilities::VOICE_SYNTHESIZE,
            capabilities::VOICE_SYNTHESIZE_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let expected = [transcribe.clone(), synthesize.clone()];
        let external_voice_plugin =
            env::var_os(VOICE_PLUGIN_PATH_ENV).is_some_and(|path| !path.is_empty());
        let required_grants = crate::voice_runtime::session_required_grants(external_voice_plugin);
        let command = optional_voice_command(
            &executable,
            INTERNAL_VOICE_PLUGIN_ARGUMENT,
            VOICE_PLUGIN_PATH_ENV,
        )
        .current_dir(cwd);
        let mut host = ProcessPluginHost::new();
        let mut notices = Vec::new();
        let launched = launch_optional_with_expected_capabilities(
            &mut host,
            id.clone(),
            "Voice transcription and synthesis",
            command,
            &transcribe,
            &expected,
            OptionalLaunchPolicy {
                required_grants: &required_grants,
                read_timeout: crate::voice_runtime::session_response_timeout(),
            },
            &mut notices,
        );
        if !launched {
            return Err(launch_failure("voice", notices));
        }
        let synthesize_available =
            optional_secondary_capability(&mut host, &id, &synthesize, &mut notices);
        if !synthesize_available {
            return Err(launch_failure("voice synthesis", notices));
        }
        Ok(Self {
            host,
            transcribe,
            synthesize: Some(synthesize),
        })
    }

    pub(crate) fn invoke(&mut self, operation: &str, payload: &Value) -> Result<Value, String> {
        let capability = match operation {
            "doctor" | "enumerate_devices" | "transcribe" | "chat" | "talk" => &self.transcribe,
            "synthesize" | "speak" | "playback" | "save" => self
                .synthesize
                .as_ref()
                .ok_or_else(|| "voice synthesis route is unavailable".to_string())?,
            "cancel" => &self.transcribe,
            _ => return Err(format!("unsupported voice operation `{operation}`")),
        };
        self.host
            .invoke(capability, operation, payload)
            .map_err(|error| error.to_string())
    }
}

/// A standalone, supervised Weixin process with enablement-bound grants.
pub(crate) struct WeixinPluginHost {
    host: ProcessPluginHost,
    capability: CapabilityDescriptor,
}

impl WeixinPluginHost {
    pub(crate) fn launch(cwd: &Path) -> Result<Self, String> {
        let settings = CapabilitySettingsStore::from_environment();
        let switches = PluginSwitches::resolve(&settings, &settings.effective_from_environment());
        if !switches.weixin {
            return Err(
                "weixin plugin is disabled; enable it before granting network access".into(),
            );
        }

        let executable = env::current_exe()
            .map_err(|error| format!("failed to resolve the YunXi executable: {error}"))?;
        let id = PluginId::new(WEIXIN_PLUGIN_ID).map_err(|error| error.to_string())?;
        let capability = descriptor(
            capabilities::CHANNEL_WEIXIN,
            capabilities::CHANNEL_WEIXIN_VERSION,
        )
        .map_err(|error| error.to_string())?;
        let command = optional_weixin_command(
            &executable,
            INTERNAL_WEIXIN_PLUGIN_ARGUMENT,
            WEIXIN_PLUGIN_PATH_ENV,
        )
        .current_dir(cwd);
        let mut host = ProcessPluginHost::new();
        let mut notices = Vec::new();
        let launched = launch_optional_with_expected_capabilities(
            &mut host,
            id,
            "Weixin channel bridge",
            command,
            &capability,
            std::slice::from_ref(&capability),
            OptionalLaunchPolicy {
                required_grants: &[GrantKind::Network, GrantKind::Secret],
                read_timeout: ACTION_RESPONSE_TIMEOUT,
            },
            &mut notices,
        );
        if !launched {
            return Err(launch_failure("weixin", notices));
        }
        Ok(Self { host, capability })
    }

    pub(crate) fn invoke(&mut self, operation: &str, payload: &Value) -> Result<Value, String> {
        self.host
            .invoke(&self.capability, operation, payload)
            .map_err(|error| error.to_string())
    }
}

fn effective_switches() -> PluginSwitches {
    let settings = CapabilitySettingsStore::from_environment();
    PluginSwitches::resolve(&settings, &settings.effective_from_environment())
}

fn current_executable() -> Result<std::path::PathBuf, String> {
    env::current_exe().map_err(|error| format!("failed to resolve the YunXi executable: {error}"))
}

fn require_enabled(capability: &str, enabled: bool) -> Result<(), String> {
    if enabled {
        Ok(())
    } else {
        Err(format!(
            "{capability} plugin is disabled; enable it before using its management commands"
        ))
    }
}

fn launch_failure(capability: &str, notices: Vec<String>) -> String {
    let detail = notices
        .last()
        .cloned()
        .unwrap_or_else(|| "the plugin did not become ready".to_string());
    format!("{capability} plugin is unavailable: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_launch_failure_stays_actionable() {
        assert_eq!(
            launch_failure("voice", Vec::new()),
            "voice plugin is unavailable: the plugin did not become ready"
        );
    }

    #[test]
    fn launch_failure_uses_only_the_final_bounded_notice() {
        assert_eq!(
            launch_failure("weixin", vec!["first".into(), "last".into()]),
            "weixin plugin is unavailable: last"
        );
    }
}
