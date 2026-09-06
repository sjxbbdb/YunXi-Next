//! Multi-plugin host orchestration and chat request assembly.

mod cordis;
mod multi_agent;
mod plugin_policy;
mod spine_adapter;
mod spine_runtime;
mod stateful;
mod tool_loop;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::time::Duration;

use yunxi_companion::COMPANION_PLUGIN_ID;
use yunxi_companion_mailbox::MAILBOX_PLUGIN_ID;
use yunxi_composition::{
    CompositionEntry, CompositionError, CompositionSnapshot, ConfigLayer, DefaultEnablement,
    PluginFiberPhase, PluginInventoryEntry, PluginInventorySnapshot, PluginManifest, PluginRisk,
    PluginRole, Profile,
};
use yunxi_context::CONTEXT_PLUGIN_ID;
use yunxi_cordis_runtime::RuntimeError;
use yunxi_kernel::{PluginCommand, PluginId, PluginIdError};
use yunxi_memory::MEMORY_PLUGIN_ID;
use yunxi_model_openai::{MODEL_PLUGIN_ID, ProviderConfig, ProviderConfigError};
use yunxi_multi_agent::MULTI_AGENT_PLUGIN_ID;
use yunxi_persona::PERSONA_PLUGIN_ID;
use yunxi_plugin_host::{
    CatalogError, PluginCallError, PluginDirectory, PluginDiscoveryManager,
    PluginDiscoveryManagerError, PluginHostError, PluginLaunch, PluginReloadReport,
    ProcessPluginHost,
};
use yunxi_protocol::{
    ActionGrant, COMPANION_DECIDE_OPERATION, CONTEXT_COMPOSE_OPERATION, CapabilityDescriptor,
    CapabilityError, ChatMessage, ChatRequest, ChatResult, CompanionDecisionRequest,
    CompanionDecisionResult, ContextComposeRequest, ContextComposeResult, GrantKind,
    MEMORY_RECALL_OPERATION, MODEL_CHAT_COMPLETE_OPERATION, MemoryContextRecord,
    MemoryRecallRequest, MemoryRecallResult, NetworkGrant, NetworkScope,
    PERSONA_CONTEXT_COMPILE_OPERATION, PersonaContextRequest, PersonaContextResult, SecretGrant,
    SkillContextRequest, SkillContextResult, SkillListRequest, SkillListResult,
    ToolApprovalRequest, ToolCall, ToolCallBatch, ToolLoopPolicy, ToolResultOutcome, capabilities,
};
use yunxi_scheduler::SCHEDULER_PLUGIN_ID;
use yunxi_settings::{CapabilitySettingsStore, next_state_root};
use yunxi_storage::STORAGE_PLUGIN_ID;
use yunxi_tool_files::FILES_PLUGIN_ID;
use yunxi_tool_mcp::{
    ARGS_ENV, CHILD_ENV_ENV, COMMAND_ENV, HTTP_ENDPOINT_ENV, HTTP_HEADERS_ENV, HTTP_SECRETS_ENV,
    MCP_PLUGIN_ID, NAME_ENV, NETWORK_GRANT_ENV, SECRET_GRANT_ENV, TRANSPORT_ENV,
};
use yunxi_tool_patch::PATCH_PLUGIN_ID;
use yunxi_tool_shell::SHELL_PLUGIN_ID;
use yunxi_tool_skills::{SKILLS_DISABLED_ENV, SKILLS_MODE_ENV, SKILLS_PLUGIN_ID, SKILLS_ROOT_ENV};
use yunxi_voice::VOICE_FIXTURE_PLUGIN_ID;
use yunxi_web_gateway::{GatewayProjection, GatewayStatus};
use yunxi_weixin::WEIXIN_PLUGIN_ID;

use crate::{
    INTERNAL_COMPANION_PLUGIN_ARGUMENT, INTERNAL_CONTEXT_PLUGIN_ARGUMENT,
    INTERNAL_FILES_PLUGIN_ARGUMENT, INTERNAL_MAILBOX_PLUGIN_ARGUMENT, INTERNAL_MCP_PLUGIN_ARGUMENT,
    INTERNAL_MEMORY_PLUGIN_ARGUMENT, INTERNAL_MODEL_PLUGIN_ARGUMENT,
    INTERNAL_MULTI_AGENT_PLUGIN_ARGUMENT, INTERNAL_PATCH_PLUGIN_ARGUMENT,
    INTERNAL_PERSONA_PLUGIN_ARGUMENT, INTERNAL_SCHEDULER_PLUGIN_ARGUMENT,
    INTERNAL_SHELL_PLUGIN_ARGUMENT, INTERNAL_SKILLS_PLUGIN_ARGUMENT,
    INTERNAL_STORAGE_PLUGIN_ARGUMENT, INTERNAL_VOICE_PLUGIN_ARGUMENT,
    INTERNAL_WEIXIN_PLUGIN_ARGUMENT,
    management::{ManagementCommand, ManagementResult},
};

use cordis::CordisBridge;
use plugin_policy::PluginSwitches;
use spine_adapter::ProcessPluginHostHandle;
use spine_runtime::{SpineController, SpineRuntimeConfig};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const READ_ONLY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(125);
const RESPONSE_TIMEOUT_MARGIN: Duration = Duration::from_secs(5);

/// Optional package root for user-installed Rust capability plugins.
pub(crate) const DYNAMIC_PLUGIN_DIRECTORY_ENV: &str = "YUNXI_NEXT_PLUGIN_DIR";
const DYNAMIC_PLUGIN_DIRECTORY_ALIAS_ENV: &str = "YUNXI_NEXT_PLUGIN_DIRECTORY";
const DYNAMIC_PLUGIN_DIRECTORY_NAME: &str = "plugins";

// A plugin process starts with a small launch environment.  Provider and MCP
// credentials/configuration are copied only by their dedicated helpers below.
const SAFE_PLUGIN_ENVIRONMENT: &[&str] = &[
    "PATH",
    "Path",
    "PATHEXT",
    "SystemRoot",
    "WINDIR",
    "ComSpec",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERPROFILE",
    "HOME",
    "YUNXI_HOME",
    "YUNXI_NEXT_HOME",
    "LANG",
    "LC_ALL",
];

const MODEL_ENVIRONMENT: &[&str] = &[
    "YUNXI_PROVIDER_PROFILE",
    "YUNXI_PROVIDER_BASE_URL",
    "OPENAI_BASE_URL",
    "YUNXI_PROVIDER_API_KEY",
    "YUNXI_PROVIDER_API_KEY_ENV",
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "YUNXI_AGENT_MODEL",
    "YUNXI_PROVIDER_TIMEOUT_MILLIS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

const MCP_ENVIRONMENT: &[&str] = &[
    TRANSPORT_ENV,
    COMMAND_ENV,
    ARGS_ENV,
    NAME_ENV,
    CHILD_ENV_ENV,
    HTTP_ENDPOINT_ENV,
    HTTP_HEADERS_ENV,
    HTTP_SECRETS_ENV,
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

const CONTEXT_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_CONTEXT_PLUGIN";
const COMPANION_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_COMPANION_PLUGIN";
const MAILBOX_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_MAILBOX_PLUGIN";
const MEMORY_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_MEMORY_PLUGIN";
const PERSONA_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_PERSONA_PLUGIN";
const SCHEDULER_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_SCHEDULER_PLUGIN";
const STORAGE_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_STORAGE_PLUGIN";
const SHELL_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_SHELL_PLUGIN";
const PATCH_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_PATCH_PLUGIN";
const FILES_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_FILES_PLUGIN";
const MCP_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_MCP_PLUGIN";
const SKILLS_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_SKILLS_PLUGIN";
const MULTI_AGENT_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_MULTI_AGENT_PLUGIN";
const VOICE_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_VOICE_PLUGIN";
const WEIXIN_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_WEIXIN_PLUGIN";
const AGENT_ENGINE_ENV: &str = "YUNXI_NEXT_AGENT_ENGINE";

pub(crate) trait ChatBackend {
    fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure>;
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    fn status(&mut self) -> BackendStatus;
    fn drain_notices(&mut self) -> Vec<String>;
    fn manage(&mut self, command: ManagementCommand) -> Result<ManagementResult, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendStatus {
    pub kernel: String,
    pub plugin: String,
    pub protocol_ready: bool,
    pub plugins: usize,
    pub capabilities: usize,
    pub failed_plugins: usize,
}

pub(crate) struct ChatSession {
    cordis: CordisBridge,
    host: ProcessPluginHostHandle,
    dynamic_plugins: Option<PluginDiscoveryManager>,
    spine: Option<SpineController>,
    composition: CompositionSnapshot,
    capability_settings: CapabilitySettingsStore,
    model_plugin_id: PluginId,
    model_capability: CapabilityDescriptor,
    child_model_command: PluginCommand,
    child_model_required_grants: Vec<GrantKind>,
    child_model_response_timeout: Duration,
    context_capability: Option<CapabilityDescriptor>,
    memory_capability: Option<CapabilityDescriptor>,
    memory_write_capability: Option<CapabilityDescriptor>,
    persona_capability: Option<CapabilityDescriptor>,
    companion_capability: Option<CapabilityDescriptor>,
    storage_capability: Option<CapabilityDescriptor>,
    mailbox_capability: Option<CapabilityDescriptor>,
    scheduler_capability: Option<CapabilityDescriptor>,
    shell_capability: Option<CapabilityDescriptor>,
    patch_capability: Option<CapabilityDescriptor>,
    files_capability: Option<CapabilityDescriptor>,
    mcp_capability: Option<CapabilityDescriptor>,
    mcp_tools: Vec<tool_loop::McpToolBinding>,
    mcp_network_grant: Option<NetworkGrant>,
    mcp_secret_grant: SecretGrant,
    skills_capability: Option<CapabilityDescriptor>,
    skill_ids: Vec<String>,
    skill_tools: Vec<tool_loop::SkillToolBinding>,
    multi_agent_capability: Option<CapabilityDescriptor>,
    agent_session_id: String,
    agent_budget: yunxi_protocol::AgentBudget,
    cwd: PathBuf,
    provider: String,
    model: String,
    first_turn: bool,
    active_session_id: Option<String>,
    proactive_in_session: u32,
    notices: Vec<String>,
    reported_notices: BTreeSet<String>,
    pending_action: Option<PendingAction>,
    next_action_ticket: u64,
    tool_continuation: Option<ToolContinuation>,
    tool_loop_policy: ToolLoopPolicy,
}

#[derive(Clone, Debug)]
enum PendingAction {
    Shell { command: String },
    Patch { path: PathBuf, content: String },
    ModelTool { call: ToolCall },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingApprovalView {
    pub call_id: String,
    pub tool_name: String,
    pub summary: String,
}

struct ToolContinuation {
    messages: Vec<ChatMessage>,
    pending_calls: Vec<ToolCall>,
    next_call: usize,
    round: u16,
    prompt: String,
    history_prefix: Vec<ChatMessage>,
}

impl ChatSession {
    pub(crate) fn launch(plugin_path: Option<&Path>) -> Result<Self, SessionError> {
        let config = ProviderConfig::from_env()?;
        let response_timeout = config
            .timeout()
            .checked_add(RESPONSE_TIMEOUT_MARGIN)
            .unwrap_or(config.timeout());
        let provider = config.provider().to_string();
        let model = config.model().to_string();
        drop(config);

        let executable = env::current_exe()?;
        let cwd = env::current_dir()?;
        let mut capability_settings = CapabilitySettingsStore::from_environment();
        let legacy_switches = capability_settings.effective_from_environment();
        let switches = PluginSwitches::resolve(&capability_settings, &legacy_switches);
        let composition = build_composition(&switches)?;
        let cordis = CordisBridge::start()?;
        let mut host = ProcessPluginHost::new();
        let mut notices = capability_settings.take_warnings();

        let model_plugin_id = PluginId::new(MODEL_PLUGIN_ID)?;
        let model_command = with_model_environment(match plugin_path {
            Some(path) => PluginCommand::new(path),
            None => PluginCommand::new(&executable).arg(INTERNAL_MODEL_PLUGIN_ARGUMENT),
        });
        let model_required_grants = if plugin_path.is_none() {
            vec![GrantKind::Network, GrantKind::ProviderCredential]
        } else {
            Vec::new()
        };
        let model_capability =
            descriptor(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
        let model_launch = PluginLaunch::new(model_plugin_id.clone(), model_command.clone())
            .with_display_name("OpenAI-compatible chat model")
            .with_handshake_timeout(HANDSHAKE_TIMEOUT)
            .with_io_timeouts(Some(response_timeout), Some(WRITE_TIMEOUT))
            .with_required_grants(model_required_grants.iter().copied())
            .with_expected_capabilities([model_capability.clone()]);
        host.launch(model_launch)?;
        require_provider(&mut host, &model_plugin_id, &model_capability)?;

        let context_capability = if switches.context {
            let id = PluginId::new(CONTEXT_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::CONTEXT_COMPOSE,
                capabilities::CONTEXT_COMPOSE_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Project instruction context",
                optional_command(
                    &executable,
                    INTERNAL_CONTEXT_PLUGIN_ARGUMENT,
                    CONTEXT_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let persona_process_needed = switches.persona;
        let persona_capability = if persona_process_needed {
            let id = PluginId::new(PERSONA_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::PERSONA_CONTEXT,
                capabilities::PERSONA_CONTEXT_VERSION,
            )?;
            let command = optional_command(
                &executable,
                INTERNAL_PERSONA_PLUGIN_ARGUMENT,
                PERSONA_PLUGIN_PATH_ENV,
            )
            .env(
                "YUNXI_PERSONA_ENABLED",
                if switches.persona { "true" } else { "false" },
            );
            launch_optional(
                &mut host,
                id,
                "Persona context compiler",
                command,
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let memory_capabilities = if switches.memory {
            let id = PluginId::new(MEMORY_PLUGIN_ID)?;
            let recall_capability = descriptor(
                capabilities::MEMORY_RECALL,
                capabilities::MEMORY_RECALL_VERSION,
            )?;
            let write_capability = descriptor(
                capabilities::MEMORY_WRITE,
                capabilities::MEMORY_WRITE_VERSION,
            )?;
            let expected_capabilities = [recall_capability.clone(), write_capability.clone()];
            let launched = launch_optional_with_expected_capabilities(
                &mut host,
                id,
                "Long-term memory",
                optional_command(
                    &executable,
                    INTERNAL_MEMORY_PLUGIN_ARGUMENT,
                    MEMORY_PLUGIN_PATH_ENV,
                ),
                &recall_capability,
                &expected_capabilities,
                OptionalLaunchPolicy {
                    required_grants: &[],
                    read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
                },
                &mut notices,
            );
            launched.then_some((recall_capability, write_capability))
        } else {
            None
        };

        let (memory_capability, memory_write_capability) =
            if let Some((recall_capability, write_capability)) = memory_capabilities {
                let write_available = optional_secondary_capability(
                    &mut host,
                    &PluginId::new(MEMORY_PLUGIN_ID)?,
                    &write_capability,
                    &mut notices,
                );
                (
                    Some(recall_capability),
                    write_available.then_some(write_capability),
                )
            } else {
                (None, None)
            };

        let storage_capability = if switches.storage {
            let id = PluginId::new(STORAGE_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::STORAGE_SESSIONS,
                capabilities::STORAGE_SESSIONS_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Persistent conversation sessions",
                optional_command(
                    &executable,
                    INTERNAL_STORAGE_PLUGIN_ARGUMENT,
                    STORAGE_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let companion_capability = if switches.companion {
            let id = PluginId::new(COMPANION_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::COMPANION_DECIDE,
                capabilities::COMPANION_DECIDE_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Deterministic companion policy",
                optional_command(
                    &executable,
                    INTERNAL_COMPANION_PLUGIN_ARGUMENT,
                    COMPANION_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let mailbox_capability = if switches.mailbox {
            let id = PluginId::new(MAILBOX_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::COMPANION_MAILBOX,
                capabilities::COMPANION_MAILBOX_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Encrypted companion mailbox",
                optional_command(
                    &executable,
                    INTERNAL_MAILBOX_PLUGIN_ARGUMENT,
                    MAILBOX_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let scheduler_capability = if switches.scheduler {
            let id = PluginId::new(SCHEDULER_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::SCHEDULER_PROACTIVE,
                capabilities::SCHEDULER_PROACTIVE_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Bounded proactive scheduler",
                optional_command(
                    &executable,
                    INTERNAL_SCHEDULER_PLUGIN_ARGUMENT,
                    SCHEDULER_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let shell_capability = if switches.shell {
            let id = PluginId::new(SHELL_PLUGIN_ID)?;
            let capability =
                descriptor(capabilities::TOOL_SHELL, capabilities::TOOL_SHELL_VERSION)?;
            launch_action_optional(
                &mut host,
                id,
                "Host-approved shell execution",
                optional_command(
                    &executable,
                    INTERNAL_SHELL_PLUGIN_ARGUMENT,
                    SHELL_PLUGIN_PATH_ENV,
                ),
                &capability,
                &[GrantKind::Approval, GrantKind::WorkspaceRead],
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let patch_capability = if switches.patch {
            let id = PluginId::new(PATCH_PLUGIN_ID)?;
            let capability =
                descriptor(capabilities::TOOL_PATCH, capabilities::TOOL_PATCH_VERSION)?;
            launch_action_optional(
                &mut host,
                id,
                "Host-approved patch application",
                optional_command(
                    &executable,
                    INTERNAL_PATCH_PLUGIN_ARGUMENT,
                    PATCH_PLUGIN_PATH_ENV,
                ),
                &capability,
                &[
                    GrantKind::Approval,
                    GrantKind::WorkspaceRead,
                    GrantKind::WorkspaceWrite,
                ],
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let files_capability = if switches.files {
            let id = PluginId::new(FILES_PLUGIN_ID)?;
            let capability =
                descriptor(capabilities::TOOL_FILES, capabilities::TOOL_FILES_VERSION)?;
            launch_files_optional(
                &mut host,
                id,
                "Read-only workspace file tools",
                optional_command(
                    &executable,
                    INTERNAL_FILES_PLUGIN_ARGUMENT,
                    FILES_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let mut mcp_tools = Vec::new();
        let (configured_mcp_network_grant, configured_mcp_secret_grant) =
            mcp_authority_from_env(&mut notices);
        let mut mcp_network_grant = None;
        let mut mcp_secret_grant = SecretGrant::empty();
        let mcp_capability = if switches.mcp {
            let id = PluginId::new(MCP_PLUGIN_ID)?;
            let capability = descriptor(capabilities::TOOL_MCP, capabilities::TOOL_MCP_VERSION)?;
            let launched = launch_action_optional(
                &mut host,
                id.clone(),
                "MCP tool bridge",
                optional_mcp_command(
                    &executable,
                    INTERNAL_MCP_PLUGIN_ARGUMENT,
                    MCP_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mcp_required_grants(),
                &mut notices,
            );
            if launched {
                let mut list_request = yunxi_protocol::McpToolListRequest::new();
                if let Some(grant) = configured_mcp_network_grant.clone() {
                    list_request = list_request.with_network_grant(grant);
                }
                if !configured_mcp_secret_grant.is_empty() {
                    list_request =
                        list_request.with_secret_grant(configured_mcp_secret_grant.clone());
                }
                match host.invoke::<_, yunxi_protocol::McpToolListResult>(
                    &capability,
                    yunxi_protocol::TOOL_MCP_LIST_OPERATION,
                    &list_request,
                ) {
                    Ok(result) => {
                        mcp_network_grant = configured_mcp_network_grant.clone();
                        mcp_secret_grant = configured_mcp_secret_grant.clone();
                        for descriptor in result.tools() {
                            match tool_loop::McpToolBinding::from_descriptor(
                                result.server_name(),
                                descriptor,
                            ) {
                                Ok(binding) => mcp_tools.push(binding),
                                Err(error) => notices.push(format!(
                                    "MCP tool `{}` was not exposed: {error}",
                                    descriptor.name()
                                )),
                            }
                        }
                        if result.truncated() {
                            notices.push("MCP tool discovery was truncated".to_string());
                        }
                        Some(capability)
                    }
                    Err(error) => {
                        notices.push(format!("MCP tool discovery failed: {error}"));
                        host.stop(&id);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut skill_ids = Vec::new();
        let mut skill_tools = Vec::new();
        let skills_capability = if switches.skills {
            let id = PluginId::new(SKILLS_PLUGIN_ID)?;
            let capability =
                descriptor(capabilities::TOOL_SKILLS, capabilities::TOOL_SKILLS_VERSION)?;
            let root = resolve_skills_root(&cwd, &mut notices);
            if let Some(root) = root {
                let launched = launch_skills_optional(
                    &mut host,
                    id.clone(),
                    optional_skills_command(&executable, &root),
                    &capability,
                    &mut notices,
                );
                if launched {
                    match host.invoke::<_, SkillListResult>(
                        &capability,
                        yunxi_protocol::TOOL_SKILLS_LIST_OPERATION,
                        &SkillListRequest::new(),
                    ) {
                        Ok(result) => {
                            skill_ids
                                .extend(result.skills().iter().map(|skill| skill.id().to_string()));
                            for skill in result.skills() {
                                for tool in skill.tools() {
                                    match tool_loop::SkillToolBinding::from_descriptor(skill, tool)
                                    {
                                        Ok(binding) => skill_tools.push(binding),
                                        Err(error) => notices.push(format!(
                                            "Skill tool `{}` was not exposed: {error}",
                                            tool.name()
                                        )),
                                    }
                                }
                            }
                            for warning in result.warnings() {
                                notices.push(format!("Skills discovery warning: {warning}"));
                            }
                            if result.truncated() {
                                notices.push("Skills discovery was truncated".to_string());
                            }
                            Some(capability)
                        }
                        Err(error) => {
                            notices.push(format!("Skills discovery failed: {error}"));
                            host.stop(&id);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let multi_agent_capability = if switches.multi_agent {
            let id = PluginId::new(MULTI_AGENT_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::TOOL_MULTI_AGENT,
                capabilities::TOOL_MULTI_AGENT_VERSION,
            )?;
            launch_action_optional(
                &mut host,
                id,
                "Isolated multi-agent coordinator",
                optional_command(
                    &executable,
                    INTERNAL_MULTI_AGENT_PLUGIN_ARGUMENT,
                    MULTI_AGENT_PLUGIN_PATH_ENV,
                ),
                &capability,
                &[
                    GrantKind::Approval,
                    GrantKind::WorkspaceRead,
                    GrantKind::WorkspaceWrite,
                    GrantKind::AgentDelegation,
                ],
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        // The fixture announces both voice routes in one process.  Keep the
        // two descriptors separate so a future device-backed adapter can
        // replace either route without changing the host contract.
        let voice_capabilities = if switches.voice {
            let id = PluginId::new(VOICE_FIXTURE_PLUGIN_ID)?;
            let transcribe_capability = descriptor(
                capabilities::VOICE_TRANSCRIBE,
                capabilities::VOICE_TRANSCRIBE_VERSION,
            )?;
            let synthesize_capability = descriptor(
                capabilities::VOICE_SYNTHESIZE,
                capabilities::VOICE_SYNTHESIZE_VERSION,
            )?;
            let expected_capabilities =
                [transcribe_capability.clone(), synthesize_capability.clone()];
            let launched = launch_optional_with_expected_capabilities(
                &mut host,
                id,
                "Voice transcription and synthesis",
                optional_command(
                    &executable,
                    INTERNAL_VOICE_PLUGIN_ARGUMENT,
                    VOICE_PLUGIN_PATH_ENV,
                ),
                &transcribe_capability,
                &expected_capabilities,
                OptionalLaunchPolicy {
                    required_grants: &[],
                    read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
                },
                &mut notices,
            );
            launched.then_some((transcribe_capability, synthesize_capability))
        } else {
            None
        };
        let (_voice_transcribe_capability, _voice_synthesize_capability) =
            if let Some((transcribe_capability, synthesize_capability)) = voice_capabilities {
                let synthesize_available = optional_secondary_capability(
                    &mut host,
                    &PluginId::new(VOICE_FIXTURE_PLUGIN_ID)?,
                    &synthesize_capability,
                    &mut notices,
                );
                (
                    Some(transcribe_capability),
                    synthesize_available.then_some(synthesize_capability),
                )
            } else {
                (None, None)
            };

        let _weixin_capability = if switches.weixin {
            let id = PluginId::new(WEIXIN_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::CHANNEL_WEIXIN,
                capabilities::CHANNEL_WEIXIN_VERSION,
            )?;
            launch_action_optional(
                &mut host,
                id,
                "Weixin channel bridge",
                optional_command(
                    &executable,
                    INTERNAL_WEIXIN_PLUGIN_ARGUMENT,
                    WEIXIN_PLUGIN_PATH_ENV,
                ),
                &capability,
                &[GrantKind::Network, GrantKind::Secret],
                &mut notices,
            )
            .then_some(capability)
        } else {
            None
        };

        let mut dynamic_plugins = configured_dynamic_plugin_root()
            .map(|root| PluginDiscoveryManager::new(PluginDirectory::new(root)));
        if let Some(manager) = dynamic_plugins.as_mut() {
            let overrides = capability_settings.plugin_overrides().clone();
            match manager.reload(&mut host, &overrides) {
                Ok(report) => append_dynamic_report_notices(&report, &mut notices),
                Err(error) => notices.push(error.to_string()),
            }
        }

        let host = ProcessPluginHostHandle::new(host);

        let agent_session_id = multi_agent::new_agent_session_id();
        let agent_budget = yunxi_protocol::AgentBudget::conservative();

        let reported_notices = notices.iter().cloned().collect();
        let mut session = Self {
            cordis,
            host,
            dynamic_plugins,
            spine: None,
            composition,
            capability_settings,
            model_plugin_id,
            model_capability,
            child_model_command: model_command,
            child_model_required_grants: model_required_grants,
            child_model_response_timeout: response_timeout,
            context_capability,
            memory_capability,
            memory_write_capability,
            persona_capability,
            companion_capability,
            storage_capability,
            mailbox_capability,
            scheduler_capability,
            shell_capability,
            patch_capability,
            files_capability,
            mcp_capability,
            mcp_tools,
            mcp_network_grant,
            mcp_secret_grant,
            skills_capability,
            skill_ids,
            skill_tools,
            multi_agent_capability,
            agent_session_id,
            agent_budget,
            cwd,
            provider,
            model,
            first_turn: true,
            active_session_id: None,
            proactive_in_session: 0,
            notices,
            reported_notices,
            pending_action: None,
            next_action_ticket: 1,
            tool_continuation: None,
            tool_loop_policy: ToolLoopPolicy::default(),
        };
        session.initialize_spine();
        Ok(session)
    }

    fn initialize_spine(&mut self) {
        if legacy_engine_requested() {
            self.push_notice(format!(
                "Agent spine disabled by {AGENT_ENGINE_ENV}; using compatibility loop"
            ));
            return;
        }
        let config = SpineRuntimeConfig {
            host: self.host.clone(),
            model_capability: self.model_capability.clone(),
            cwd: self.cwd.clone(),
            shell_capability: self.shell_capability.clone(),
            patch_capability: self.patch_capability.clone(),
            files_capability: self.files_capability.clone(),
            mcp_capability: self.mcp_capability.clone(),
            mcp_tools: self.mcp_tools.clone(),
            mcp_network_grant: self.mcp_network_grant.clone(),
            mcp_secret_grant: self.mcp_secret_grant.clone(),
            skill_tools: self.skill_tools.clone(),
            multi_agent_capability: self.multi_agent_capability.clone(),
            agent_session_id: self.agent_session_id.clone(),
            agent_budget: self.agent_budget,
            child_model_command: self.child_model_command.clone(),
            child_model_required_grants: self.child_model_required_grants.clone(),
            child_model_response_timeout: self.child_model_response_timeout,
        };
        match SpineController::new(config) {
            Ok(spine) => self.spine = Some(spine),
            Err(error) => self.push_notice(format!(
                "Agent spine unavailable; using compatibility loop: {error}"
            )),
        }
    }

    fn sync_spine_notices(&mut self) {
        let notices = self
            .spine
            .as_mut()
            .map(SpineController::drain_notices)
            .unwrap_or_default();
        for notice in notices {
            self.push_notice(notice);
        }
    }

    /// Reconcile user-installed packages before exposing the inventory.  A
    /// scan is bounded and each package is isolated by the manager, so this
    /// also provides the Web UI's hot-add/hot-remove boundary.
    pub(super) fn refresh_dynamic_plugins(&mut self) {
        let Some(root) = configured_dynamic_plugin_root() else {
            if let Some(mut manager) = self.dynamic_plugins.take() {
                let report = manager.unload_all(&mut self.host.borrow_mut());
                append_dynamic_report_notices(&report, &mut self.notices);
                if report.has_failures() {
                    // Keep ownership of any package that could not be
                    // unregistered so a later refresh can retry cleanup.
                    self.dynamic_plugins = Some(manager);
                }
            }
            return;
        };
        if self
            .dynamic_plugins
            .as_ref()
            .is_some_and(|manager| manager.directory().root() != root)
        {
            if let Some(mut manager) = self.dynamic_plugins.take() {
                let report = manager.unload_all(&mut self.host.borrow_mut());
                append_dynamic_report_notices(&report, &mut self.notices);
                if report.has_failures() {
                    // Do not replace a manager while it still owns a live
                    // registration. Retrying on the next refresh preserves
                    // the old route instead of leaking it into the new root.
                    self.dynamic_plugins = Some(manager);
                    return;
                }
            }
        }
        if self.dynamic_plugins.is_none() {
            self.dynamic_plugins = Some(PluginDiscoveryManager::new(PluginDirectory::new(root)));
        }
        let overrides = self.capability_settings.plugin_overrides().clone();
        let result = {
            let Some(manager) = self.dynamic_plugins.as_mut() else {
                return;
            };
            let mut host = self.host.borrow_mut();
            manager.reload(&mut host, &overrides)
        };
        match result {
            Ok(report) => append_dynamic_report_notices(&report, &mut self.notices),
            Err(error) => self.push_notice(error.to_string()),
        }
    }

    pub(super) fn has_pending_action(&self) -> bool {
        self.pending_action.is_some()
            || self.tool_continuation.is_some()
            || self
                .spine
                .as_ref()
                .is_some_and(|spine| spine.pending().is_some())
    }

    pub(super) fn has_spine_pending_approval(&self) -> bool {
        self.spine
            .as_ref()
            .is_some_and(|spine| spine.pending().is_some())
    }

    fn reset_spine(&mut self) {
        if let Some(spine) = self.spine.as_mut()
            && let Err(error) = spine.reset()
        {
            self.push_notice(format!("Agent spine session reset failed: {error}"));
        }
        self.sync_spine_notices();
    }

    pub(crate) fn web_projection(&mut self) -> GatewayProjection {
        let status = <Self as ChatBackend>::status(self);
        let inventory = self.plugin_inventory();
        let sessions = self.web_session_summaries();
        let gateway_status = GatewayStatus::new(
            status.kernel,
            status.plugin,
            status.protocol_ready,
            status.plugins,
            status.capabilities,
            status.failed_plugins,
        )
        .with_model(self.provider.clone(), self.model.clone());
        let home = env::var_os("USERPROFILE")
            .or_else(|| env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| self.cwd.clone());
        GatewayProjection::new(gateway_status, inventory)
            .with_sessions(sessions)
            .with_host_paths(self.cwd.to_string_lossy(), home.to_string_lossy())
    }

    /// Build the same bounded plugin catalog used by a live session without
    /// starting any child process. Management commands use this adapter so
    /// read-only inspection and settings validation stay side-effect free.
    pub(crate) fn plugin_inventory_for_settings(
        settings: &CapabilitySettingsStore,
    ) -> Result<PluginInventorySnapshot, SessionError> {
        let legacy_switches = settings.effective_from_environment();
        let switches = PluginSwitches::resolve(settings, &legacy_switches);
        let inventory = build_composition(&switches)?.inventory();
        let Some(root) = configured_dynamic_plugin_root() else {
            return Ok(inventory);
        };
        let report = PluginDirectory::new(root)
            .discover_or_empty()
            .map_err(PluginDiscoveryManagerError::from)
            .map_err(SessionError::DynamicDiscovery)?;
        let additions = report
            .plugins()
            .iter()
            .map(|plugin| dynamic_inventory_entry(plugin, settings.plugin_overrides(), None))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(inventory.with_additional(additions))
    }

    pub(crate) fn capability_settings(&self) -> &CapabilitySettingsStore {
        &self.capability_settings
    }

    pub(crate) fn capability_settings_mut(&mut self) -> &mut CapabilitySettingsStore {
        &mut self.capability_settings
    }

    pub(crate) fn pending_approval(&self) -> Option<PendingApprovalView> {
        if let Some(request) = self.spine.as_ref().and_then(SpineController::pending) {
            return Some(PendingApprovalView {
                call_id: request.call_id().to_string(),
                tool_name: request.tool_name().to_string(),
                summary: request.summary().to_string(),
            });
        }
        let Some(PendingAction::ModelTool { call }) = self.pending_action.as_ref() else {
            return None;
        };
        Some(PendingApprovalView {
            call_id: call.id().to_string(),
            tool_name: call.name().to_string(),
            summary: tool_loop::decode_call_with_skills(
                call,
                self.multi_agent_capability.is_some(),
                &self.mcp_tools,
                &self.skill_tools,
            )
            .map(|action| action.summary().to_string())
            .unwrap_or_else(|_| "model requested a tool action".to_string()),
        })
    }

    pub(crate) fn drain_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn assemble_messages(&mut self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut assembled = Vec::new();

        if let Some(capability) = self.context_capability.clone() {
            let result = self.host.invoke::<_, ContextComposeResult>(
                &capability,
                CONTEXT_COMPOSE_OPERATION,
                &ContextComposeRequest::new(&self.cwd),
            );
            match result {
                Ok(context) if !context.instructions().trim().is_empty() => {
                    assembled.push(ChatMessage::system(context.instructions()));
                }
                Ok(_) => {}
                Err(error) => {
                    if call_lost_route(&error) {
                        self.context_capability = None;
                    }
                    self.push_notice(format!("context capability degraded: {error}"));
                }
            }
        }

        if let Some(capability) = self.skills_capability.clone() {
            match SkillContextRequest::new(self.skill_ids.clone()) {
                Ok(request) => match self.host.invoke::<_, SkillContextResult>(
                    &capability,
                    yunxi_protocol::TOOL_SKILLS_CONTEXT_OPERATION,
                    &request,
                ) {
                    Ok(context) => {
                        for block in context.blocks() {
                            assembled.push(ChatMessage::system(block.instructions()));
                        }
                        for warning in context.warnings() {
                            self.push_notice(format!("Skills context warning: {warning}"));
                        }
                        if context.truncated() {
                            self.push_notice("Skills context was truncated");
                        }
                    }
                    Err(error) => {
                        if call_lost_route(&error) {
                            self.skills_capability = None;
                            self.skill_ids.clear();
                            self.skill_tools.clear();
                        }
                        self.push_notice(format!("Skills capability degraded: {error}"));
                    }
                },
                Err(error) => {
                    self.push_notice(format!("Skills context request was rejected: {error}"));
                }
            }
        }

        let include_boot_context = self.first_turn;
        let mut boot_memories = Vec::<MemoryContextRecord>::new();
        let mut dynamic_memories = Vec::<MemoryContextRecord>::new();
        if self.persona_capability.is_some()
            && let Some(capability) = self.memory_capability.clone()
        {
            let request = MemoryRecallRequest::new(&self.cwd, memory_recall_query(messages))
                .with_boot_context(include_boot_context);
            match self.host.invoke::<_, MemoryRecallResult>(
                &capability,
                MEMORY_RECALL_OPERATION,
                &request,
            ) {
                Ok(memory) => {
                    boot_memories.extend_from_slice(memory.boot());
                    dynamic_memories.extend_from_slice(memory.dynamic());
                    for warning in memory.warnings() {
                        self.push_notice(format!("memory data warning: {warning}"));
                    }
                    if memory.truncated() {
                        self.push_notice("memory recall was truncated to its context budget");
                    }
                }
                Err(error) => {
                    if call_lost_route(&error) {
                        self.memory_capability = None;
                    }
                    self.push_notice(format!("memory capability degraded: {error}"));
                }
            }
        }

        let companion_memories = boot_memories
            .iter()
            .chain(dynamic_memories.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut persona_identity = None::<(String, String)>;
        if let Some(capability) = self.persona_capability.clone() {
            let request = PersonaContextRequest::new(
                boot_memories.clone(),
                dynamic_memories.clone(),
                include_boot_context,
            );
            match self.host.invoke::<_, PersonaContextResult>(
                &capability,
                PERSONA_CONTEXT_COMPILE_OPERATION,
                &request,
            ) {
                Ok(persona) => {
                    persona_identity = Some((
                        persona.profile_id().to_string(),
                        persona.display_name().to_string(),
                    ));
                    if let Some(content) =
                        persona.content().filter(|value| !value.trim().is_empty())
                    {
                        assembled.push(ChatMessage::system(content));
                    }
                    for warning in persona.warnings() {
                        self.push_notice(format!("persona data warning: {warning}"));
                    }
                }
                Err(error) => {
                    if call_lost_route(&error) {
                        self.persona_capability = None;
                        self.memory_capability = None;
                    }
                    self.push_notice(format!("persona capability degraded: {error}"));
                }
            }
        }

        if let Some(capability) = self.companion_capability.clone()
            && let Some(prompt) = latest_user_message(messages)
        {
            let mut request =
                CompanionDecisionRequest::new(prompt).with_memories(companion_memories);
            if let Some((profile_id, display_name)) = persona_identity {
                request = request.with_persona(profile_id, display_name);
            }
            match self.host.invoke::<_, CompanionDecisionResult>(
                &capability,
                COMPANION_DECIDE_OPERATION,
                &request,
            ) {
                Ok(decision) => {
                    let mut instruction = decision.instruction().unwrap_or_default().to_string();
                    if let Some(follow_up) = decision.follow_up() {
                        instruction.push_str("\nOptional follow-up, only when it fits naturally: ");
                        instruction.push_str(follow_up);
                    }
                    if !instruction.trim().is_empty() {
                        assembled.push(ChatMessage::system(instruction));
                    }
                }
                Err(error) => {
                    if call_lost_route(&error) {
                        self.companion_capability = None;
                    }
                    self.push_notice(format!("companion capability degraded: {error}"));
                }
            }
        }

        self.first_turn = false;
        assembled.extend_from_slice(messages);
        assembled
    }

    fn push_notice(&mut self, notice: impl Into<String>) {
        let notice = notice.into();
        if self.reported_notices.insert(notice.clone()) {
            self.notices.push(notice);
        }
    }
}

impl ChatBackend for ChatSession {
    fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure> {
        if self.has_pending_action() {
            let pending = self.pending_approval();
            return Err(ChatFailure::ToolApprovalRequired {
                tool: pending
                    .as_ref()
                    .map(|value| value.tool_name.clone())
                    .unwrap_or_else(|| "pending action".to_string()),
                summary: pending
                    .as_ref()
                    .map(|value| value.summary.clone())
                    .unwrap_or_else(|| {
                        "approve or deny the pending action before starting another turn"
                            .to_string()
                    }),
            });
        }
        self.prepare_agent_session();
        let prompt = latest_user_message(messages)
            .unwrap_or_default()
            .to_string();
        let assembled = self.assemble_messages(messages);

        if let Some(spine) = self.spine.as_mut() {
            spine.set_agent_session_id(self.agent_session_id.clone());
            let mut seed = assembled.clone();
            let current_index = seed
                .iter()
                .rposition(|message| {
                    message.role() == yunxi_protocol::ChatRole::User && message.content() == prompt
                })
                .or_else(|| {
                    seed.iter()
                        .rposition(|message| message.role() == yunxi_protocol::ChatRole::User)
                });
            let Some(current_index) = current_index else {
                self.sync_spine_notices();
                return Err(ChatFailure::ProtocolViolation(
                    "a chat turn requires a user message".to_string(),
                ));
            };
            seed.remove(current_index);
            let outcome = spine.start(ChatMessage::user(prompt.clone()), seed);
            self.sync_spine_notices();
            return match outcome {
                Ok(yunxi_agent_spine::AgentTurnOutcome::Completed(result)) => {
                    let reply = result.content().to_string();
                    self.persist_turn(&prompt, &reply);
                    self.extract_turn_memories(&prompt, &reply);
                    self.evaluate_proactive(&prompt);
                    Ok(reply)
                }
                Ok(yunxi_agent_spine::AgentTurnOutcome::AwaitingApproval(request)) => {
                    Err(ChatFailure::ToolApprovalRequired {
                        tool: request.tool_name().to_string(),
                        summary: request.summary().to_string(),
                    })
                }
                Err(error) => Err(spine_runtime::chat_failure_from_agent(error)),
            };
        }

        let history_prefix = vec![ChatMessage::user(prompt.clone())];
        self.run_model_loop(assembled, prompt, history_prefix, 1)
            .map(|reply| reply.content)
    }

    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn status(&mut self) -> BackendStatus {
        let cordis_ready = self.cordis.ready();
        let snapshot = self.host.snapshot();
        let plugin = snapshot
            .plugins()
            .iter()
            .find(|plugin| plugin.id() == &self.model_plugin_id)
            .map(|plugin| plugin.state().to_string())
            .unwrap_or_else(|| "not registered".to_string());
        let protocol_ready = cordis_ready
            && self
                .host
                .catalog()
                .providers(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)
                .iter()
                .any(|provider| provider.id() == &self.model_plugin_id);
        BackendStatus {
            kernel: snapshot.state().to_string(),
            plugin,
            protocol_ready,
            plugins: self.host.connection_count(),
            capabilities: self.host.catalog().capability_count(),
            failed_plugins: snapshot.failed_plugin_count(),
        }
    }

    fn drain_notices(&mut self) -> Vec<String> {
        ChatSession::drain_notices(self)
    }

    fn manage(&mut self, command: ManagementCommand) -> Result<ManagementResult, String> {
        self.manage_command(command)
    }
}

struct ModelReply {
    content: String,
    history: Vec<ChatMessage>,
}

impl ChatSession {
    fn run_model_loop(
        &mut self,
        mut messages: Vec<ChatMessage>,
        prompt: String,
        history_prefix: Vec<ChatMessage>,
        round: u16,
    ) -> Result<ModelReply, ChatFailure> {
        let mut request = ChatRequest::new(messages.clone());
        if let Some(catalog) = tool_loop::catalog_with_skills(
            self.shell_capability.is_some(),
            self.patch_capability.is_some(),
            self.files_capability.is_some(),
            self.multi_agent_capability.is_some(),
            &self.mcp_tools,
            &self.skill_tools,
        ) {
            request = request.with_tools(catalog);
        }
        let result = match self.host.invoke::<_, ChatResult>(
            &self.model_capability,
            MODEL_CHAT_COMPLETE_OPERATION,
            &request,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.tool_continuation = None;
                self.pending_action = None;
                return Err(chat_failure_from_plugin(error));
            }
        };

        if result.tool_calls().is_empty() {
            let reply = result.content().to_string();
            if reply.trim().is_empty() {
                self.tool_continuation = None;
                return Err(ChatFailure::ProtocolViolation(
                    "model returned an empty response without tool calls".to_string(),
                ));
            }
            self.persist_turn(&prompt, &reply);
            self.extract_turn_memories(&prompt, &reply);
            self.evaluate_proactive(&prompt);
            let mut history = history_prefix;
            history.push(ChatMessage::assistant(reply.clone()));
            return Ok(ModelReply {
                content: reply,
                history,
            });
        }

        let calls = result.tool_calls().to_vec();
        let batch = match ToolCallBatch::new(round, calls.clone()) {
            Ok(batch) => batch,
            Err(error) => {
                self.tool_continuation = None;
                self.pending_action = None;
                return Err(ChatFailure::ToolLoop(error.to_string()));
            }
        };
        if let Err(error) = batch.validate_with_policy(&self.tool_loop_policy) {
            self.tool_continuation = None;
            self.pending_action = None;
            return Err(ChatFailure::ToolLoop(error.to_string()));
        }
        messages.push(ChatMessage::assistant_tool_calls(batch.calls().to_vec()));
        self.tool_continuation = Some(ToolContinuation {
            messages,
            pending_calls: batch.calls().to_vec(),
            next_call: 0,
            round,
            prompt,
            history_prefix,
        });
        self.queue_next_tool_approval()
    }

    fn queue_next_tool_approval(&mut self) -> Result<ModelReply, ChatFailure> {
        let Some(continuation) = self.tool_continuation.as_ref() else {
            return Err(ChatFailure::ToolLoop(
                "tool continuation is missing".to_string(),
            ));
        };
        let Some(call) = continuation
            .pending_calls
            .get(continuation.next_call)
            .cloned()
        else {
            return Err(ChatFailure::ToolLoop(
                "tool continuation has no pending call".to_string(),
            ));
        };
        match tool_loop::decode_call_with_skills(
            &call,
            self.multi_agent_capability.is_some(),
            &self.mcp_tools,
            &self.skill_tools,
        ) {
            Ok(
                action @ (tool_loop::ToolAction::FileSearch { .. }
                | tool_loop::ToolAction::FileRead { .. }
                | tool_loop::ToolAction::Skill { .. }
                | tool_loop::ToolAction::AgentList
                | tool_loop::ToolAction::AgentInterrupt { .. }),
            ) => {
                let outcome = self.execute_model_tool(&action);
                self.append_tool_outcome(&call, outcome);
                self.advance_tool_continuation()
            }
            Ok(action) => {
                let requested_grants = match &action {
                    tool_loop::ToolAction::Shell { .. } => {
                        vec![GrantKind::Approval, GrantKind::WorkspaceRead]
                    }
                    tool_loop::ToolAction::Patch { .. } => vec![
                        GrantKind::Approval,
                        GrantKind::WorkspaceRead,
                        GrantKind::WorkspaceWrite,
                    ],
                    tool_loop::ToolAction::FileSearch { .. }
                    | tool_loop::ToolAction::FileRead { .. } => {
                        unreachable!("read-only file tools are handled before approval")
                    }
                    tool_loop::ToolAction::Mcp { .. } => {
                        let mut grants = vec![GrantKind::Approval];
                        if self.mcp_network_grant.is_some() {
                            grants.push(GrantKind::Network);
                        }
                        if !self.mcp_secret_grant.is_empty() {
                            grants.push(GrantKind::Secret);
                        }
                        grants
                    }
                    tool_loop::ToolAction::Skill { .. } => {
                        unreachable!("metadata-only Skill tools are handled before approval")
                    }
                    tool_loop::ToolAction::AgentSpawn { .. }
                    | tool_loop::ToolAction::AgentMessage { .. } => vec![
                        GrantKind::Approval,
                        GrantKind::WorkspaceRead,
                        GrantKind::WorkspaceWrite,
                        GrantKind::AgentDelegation,
                    ],
                    tool_loop::ToolAction::AgentList
                    | tool_loop::ToolAction::AgentInterrupt { .. } => {
                        unreachable!("read-only and cancellation agent tools run without approval")
                    }
                };
                let approval = ToolApprovalRequest::new(
                    continuation.round,
                    call.id().clone(),
                    call.name().clone(),
                    action.summary(),
                    requested_grants,
                )
                .map_err(|error| ChatFailure::ToolLoop(error.to_string()))?;
                self.pending_action = Some(PendingAction::ModelTool { call });
                Err(ChatFailure::ToolApprovalRequired {
                    tool: approval.tool_name().to_string(),
                    summary: approval.summary().to_string(),
                })
            }
            Err(error) => {
                self.append_tool_outcome(
                    &call,
                    ToolResultOutcome::rejected("invalid_arguments", error.to_string())
                        .expect("bounded tool rejection"),
                );
                self.advance_tool_continuation()
            }
        }
    }

    fn append_tool_outcome(&mut self, call: &ToolCall, outcome: ToolResultOutcome) {
        if let Some(continuation) = self.tool_continuation.as_mut() {
            continuation
                .messages
                .push(tool_loop::tool_result_message(call, &outcome));
            continuation.next_call = continuation.next_call.saturating_add(1);
        }
    }

    fn advance_tool_continuation(&mut self) -> Result<ModelReply, ChatFailure> {
        let Some(continuation) = self.tool_continuation.take() else {
            return Err(ChatFailure::ToolLoop(
                "tool continuation is missing".to_string(),
            ));
        };
        if continuation.next_call < continuation.pending_calls.len() {
            self.tool_continuation = Some(continuation);
            return self.queue_next_tool_approval();
        }
        let next_round = continuation.round.saturating_add(1);
        if next_round > self.tool_loop_policy.max_rounds() {
            return Err(ChatFailure::ToolLoop(format!(
                "tool loop exceeded {} rounds",
                self.tool_loop_policy.max_rounds()
            )));
        }
        self.run_model_loop(
            continuation.messages,
            continuation.prompt,
            continuation.history_prefix,
            next_round,
        )
    }

    pub(super) fn approve_pending_model_tool(
        &mut self,
    ) -> Result<crate::management::ManagementResult, String> {
        if self.has_spine_pending_approval() {
            return self.resolve_spine_approval(true);
        }
        let Some(PendingAction::ModelTool { call }) = self.pending_action.take() else {
            return Err("there is no pending model tool action".to_string());
        };
        let action = tool_loop::decode_call_with_skills(
            &call,
            self.multi_agent_capability.is_some(),
            &self.mcp_tools,
            &self.skill_tools,
        )
        .map_err(|error| error.to_string())?;
        let outcome = self.execute_model_tool(&action);
        self.append_tool_outcome(&call, outcome);
        self.finish_model_tool_progress()
    }

    pub(super) fn deny_pending_model_tool(
        &mut self,
    ) -> Result<crate::management::ManagementResult, String> {
        if self.has_spine_pending_approval() {
            return self.resolve_spine_approval(false);
        }
        let Some(PendingAction::ModelTool { call }) = self.pending_action.take() else {
            return Err("there is no pending model tool action".to_string());
        };
        self.append_tool_outcome(
            &call,
            ToolResultOutcome::rejected("user_denied", "the user denied this tool call")
                .expect("bounded denial result"),
        );
        self.finish_model_tool_progress()
    }

    pub(super) fn cancel_pending_model_tool(
        &mut self,
    ) -> Result<crate::management::ManagementResult, String> {
        if self.has_spine_pending_approval() {
            return self.resolve_spine_approval_with_reason(
                false,
                "the user cancelled the pending tool call",
            );
        }
        let Some(PendingAction::ModelTool { call }) = self.pending_action.take() else {
            return Err("there is no pending model tool action".to_string());
        };
        self.append_tool_outcome(
            &call,
            ToolResultOutcome::cancelled("user cancelled the pending tool call")
                .expect("bounded cancellation result"),
        );
        self.finish_model_tool_progress()
    }

    fn finish_model_tool_progress(
        &mut self,
    ) -> Result<crate::management::ManagementResult, String> {
        match self.advance_tool_continuation() {
            Ok(reply) => Ok(crate::management::ManagementResult::assistant_reply(
                reply.content,
                reply.history,
            )),
            Err(ChatFailure::ToolApprovalRequired { tool, summary }) => Ok(
                crate::management::ManagementResult::lines(tool_approval_lines(&tool, &summary)),
            ),
            Err(error) => {
                self.tool_continuation = None;
                Err(error.to_string())
            }
        }
    }

    fn resolve_spine_approval(
        &mut self,
        approved: bool,
    ) -> Result<crate::management::ManagementResult, String> {
        let outcome = self
            .spine
            .as_mut()
            .ok_or_else(|| "Agent spine is unavailable".to_string())?
            .resolve(approved);
        self.finish_spine_resolution(outcome)
    }

    fn resolve_spine_approval_with_reason(
        &mut self,
        approved: bool,
        denial_reason: &str,
    ) -> Result<crate::management::ManagementResult, String> {
        let outcome = self
            .spine
            .as_mut()
            .ok_or_else(|| "Agent spine is unavailable".to_string())?
            .resolve_with_denial_reason(approved, denial_reason);
        self.finish_spine_resolution(outcome)
    }

    fn finish_spine_resolution(
        &mut self,
        outcome: Result<yunxi_agent_spine::AgentTurnOutcome, yunxi_agent_spine::AgentError>,
    ) -> Result<crate::management::ManagementResult, String> {
        self.sync_spine_notices();
        match outcome {
            Ok(yunxi_agent_spine::AgentTurnOutcome::Completed(result)) => {
                let reply = result.content().to_string();
                let prompt = self
                    .spine
                    .as_mut()
                    .and_then(SpineController::take_prompt)
                    .unwrap_or_default();
                self.persist_turn(&prompt, &reply);
                self.extract_turn_memories(&prompt, &reply);
                self.evaluate_proactive(&prompt);
                Ok(crate::management::ManagementResult::assistant_reply(
                    reply.clone(),
                    vec![ChatMessage::user(prompt), ChatMessage::assistant(reply)],
                ))
            }
            Ok(yunxi_agent_spine::AgentTurnOutcome::AwaitingApproval(request)) => {
                Ok(crate::management::ManagementResult::lines(
                    tool_approval_lines(request.tool_name().as_str(), request.summary()),
                ))
            }
            Err(error) => {
                if let Some(spine) = self.spine.as_mut() {
                    let _ = spine.take_prompt();
                }
                Err(spine_runtime::chat_failure_from_agent(error).to_string())
            }
        }
    }

    fn execute_model_tool(&mut self, action: &tool_loop::ToolAction) -> ToolResultOutcome {
        let ticket = format!(
            "model-tool-{}-{}",
            std::process::id(),
            self.next_action_ticket
        );
        self.next_action_ticket = self.next_action_ticket.saturating_add(1);
        match action {
            tool_loop::ToolAction::Skill { binding, .. } => ToolResultOutcome::rejected(
                "skill_tool_unavailable",
                format!(
                    "Skill `{}` declared tool `{}` as metadata only; execution is not enabled",
                    binding.skill_id(),
                    binding.remote_name()
                ),
            )
            .expect("bounded Skill tool rejection"),
            tool_loop::ToolAction::AgentSpawn {
                task,
                name,
                parent_id,
            } => self.execute_agent_spawn(task, name.as_deref(), parent_id.as_deref(), &ticket),
            tool_loop::ToolAction::AgentList => self.execute_agent_list(&ticket),
            tool_loop::ToolAction::AgentMessage { agent_id, message } => {
                self.execute_agent_message(agent_id, message, &ticket)
            }
            tool_loop::ToolAction::AgentInterrupt {
                agent_id,
                recursive,
            } => self.execute_agent_interrupt(agent_id, *recursive, &ticket),
            tool_loop::ToolAction::Shell {
                command,
                timeout_millis,
            } => {
                let grant = ActionGrant::approved(
                    yunxi_protocol::WorkspaceGrant::read_only(&self.cwd),
                    &self.cwd,
                    ticket,
                )
                .with_limits(*timeout_millis, 64 * 1024);
                let Some(capability) = self.shell_capability.clone() else {
                    return ToolResultOutcome::failed(
                        "tool_unavailable",
                        "shell capability is no longer available",
                        false,
                    )
                    .expect("bounded tool failure");
                };
                match self.host.invoke::<_, yunxi_protocol::ShellExecuteResult>(
                    &capability,
                    yunxi_protocol::TOOL_SHELL_EXECUTE_OPERATION,
                    &yunxi_protocol::ShellExecuteRequest::new(grant, command),
                ) {
                    Ok(result) => serde_json::to_value(result)
                        .ok()
                        .and_then(|value| ToolResultOutcome::completed(value).ok())
                        .unwrap_or_else(|| {
                            ToolResultOutcome::rejected(
                                "invalid_tool_result",
                                "shell returned an invalid result",
                            )
                            .expect("bounded invalid result")
                        }),
                    Err(error) => self.tool_error_outcome(tool_loop::SHELL_TOOL_NAME, error, true),
                }
            }
            tool_loop::ToolAction::Patch {
                patch,
                timeout_millis,
            } => {
                let grant = ActionGrant::approved(
                    yunxi_protocol::WorkspaceGrant::read_write(&self.cwd).with_workspace_write(),
                    &self.cwd,
                    ticket,
                )
                .with_write(true)
                .with_limits(*timeout_millis, 64 * 1024);
                let Some(capability) = self.patch_capability.clone() else {
                    return ToolResultOutcome::failed(
                        "tool_unavailable",
                        "patch capability is no longer available",
                        false,
                    )
                    .expect("bounded tool failure");
                };
                match self.host.invoke::<_, yunxi_protocol::PatchApplyResult>(
                    &capability,
                    yunxi_protocol::TOOL_PATCH_APPLY_OPERATION,
                    &yunxi_protocol::PatchApplyRequest::new(grant, patch),
                ) {
                    Ok(result) => serde_json::to_value(result)
                        .ok()
                        .and_then(|value| ToolResultOutcome::completed(value).ok())
                        .unwrap_or_else(|| {
                            ToolResultOutcome::rejected(
                                "invalid_tool_result",
                                "patch returned an invalid result",
                            )
                            .expect("bounded invalid result")
                        }),
                    Err(error) => self.tool_error_outcome(tool_loop::PATCH_TOOL_NAME, error, false),
                }
            }
            tool_loop::ToolAction::FileSearch { query, path } => {
                let Some(capability) = self.files_capability.clone() else {
                    return ToolResultOutcome::failed(
                        "tool_unavailable",
                        "file capability is no longer available",
                        false,
                    )
                    .expect("bounded tool failure");
                };
                let request = yunxi_protocol::FileSearchRequest::new(
                    yunxi_protocol::WorkspaceGrant::read_only(&self.cwd),
                    self.cwd.join(path),
                    query,
                );
                match self.host.invoke::<_, yunxi_protocol::FileSearchResult>(
                    &capability,
                    yunxi_protocol::TOOL_FILES_SEARCH_OPERATION,
                    &request,
                ) {
                    Ok(result) => serde_json::to_value(result)
                        .ok()
                        .and_then(|value| ToolResultOutcome::completed(value).ok())
                        .unwrap_or_else(|| {
                            ToolResultOutcome::rejected(
                                "invalid_tool_result",
                                "file search returned an invalid result",
                            )
                            .expect("bounded invalid result")
                        }),
                    Err(error) => {
                        self.file_tool_error_outcome(tool_loop::FILE_SEARCH_TOOL_NAME, error)
                    }
                }
            }
            tool_loop::ToolAction::FileRead { path } => {
                let Some(capability) = self.files_capability.clone() else {
                    return ToolResultOutcome::failed(
                        "tool_unavailable",
                        "file capability is no longer available",
                        false,
                    )
                    .expect("bounded tool failure");
                };
                let request = yunxi_protocol::FileReadRequest::new(
                    yunxi_protocol::WorkspaceGrant::read_only(&self.cwd),
                    path,
                );
                match self.host.invoke::<_, yunxi_protocol::FileReadResult>(
                    &capability,
                    yunxi_protocol::TOOL_FILES_READ_OPERATION,
                    &request,
                ) {
                    Ok(result) => serde_json::to_value(result)
                        .ok()
                        .and_then(|value| ToolResultOutcome::completed(value).ok())
                        .unwrap_or_else(|| {
                            ToolResultOutcome::rejected(
                                "invalid_tool_result",
                                "file read returned an invalid result",
                            )
                            .expect("bounded invalid result")
                        }),
                    Err(error) => {
                        self.file_tool_error_outcome(tool_loop::FILE_READ_TOOL_NAME, error)
                    }
                }
            }
            tool_loop::ToolAction::Mcp { binding, arguments } => {
                let Some(capability) = self.mcp_capability.clone() else {
                    return ToolResultOutcome::failed(
                        "mcp_unavailable",
                        "MCP capability is no longer available",
                        false,
                    )
                    .expect("bounded MCP failure");
                };
                let grant = ActionGrant::approved(
                    yunxi_protocol::WorkspaceGrant::read_only(&self.cwd),
                    &self.cwd,
                    ticket,
                )
                .with_limits(tool_loop::DEFAULT_MODEL_TOOL_TIMEOUT_MILLIS, 1024 * 1024);
                let grant = match self.mcp_network_grant.clone() {
                    Some(network) => grant.with_network_grant(network),
                    None => grant,
                }
                .with_secret_grant(self.mcp_secret_grant.clone());
                let request = match yunxi_protocol::McpToolCallRequest::new(
                    grant,
                    binding.server_name(),
                    binding.remote_name(),
                    arguments.clone(),
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        return ToolResultOutcome::rejected(
                            "invalid_mcp_request",
                            error.to_string(),
                        )
                        .expect("bounded MCP request rejection");
                    }
                };
                match self.host.invoke::<_, yunxi_protocol::McpToolCallResult>(
                    &capability,
                    yunxi_protocol::TOOL_MCP_CALL_OPERATION,
                    &request,
                ) {
                    Ok(result) => serde_json::to_value(result)
                        .ok()
                        .and_then(|value| ToolResultOutcome::completed(value).ok())
                        .unwrap_or_else(|| {
                            ToolResultOutcome::rejected(
                                "invalid_tool_result",
                                "MCP returned an invalid result",
                            )
                            .expect("bounded invalid MCP result")
                        }),
                    Err(error) => self.mcp_tool_error_outcome(binding.model_name().as_str(), error),
                }
            }
        }
    }

    fn tool_error_outcome(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
        shell: bool,
    ) -> ToolResultOutcome {
        let (code, message, retryable) = match error {
            PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            } => (code, message, retryable),
            other => {
                if call_lost_route(&other) {
                    if shell {
                        self.shell_capability = None;
                    } else {
                        self.patch_capability = None;
                    }
                }
                ("tool_unavailable".to_string(), other.to_string(), false)
            }
        };
        self.push_notice(format!(
            "model tool `{tool_name}` failed: {code}: {message}"
        ));
        ToolResultOutcome::failed(code, message, retryable)
            .unwrap_or_else(|_| ToolResultOutcome::cancelled("tool result was invalid").unwrap())
    }

    fn file_tool_error_outcome(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
    ) -> ToolResultOutcome {
        let (code, message, retryable) = match error {
            PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            } => (code, message, retryable),
            other => {
                if call_lost_route(&other) {
                    self.files_capability = None;
                }
                (
                    "file_tool_unavailable".to_string(),
                    other.to_string(),
                    false,
                )
            }
        };
        self.push_notice(format!(
            "model tool `{tool_name}` failed: {code}: {message}"
        ));
        ToolResultOutcome::failed(code, message, retryable)
            .unwrap_or_else(|_| ToolResultOutcome::cancelled("file result was invalid").unwrap())
    }

    fn mcp_tool_error_outcome(
        &mut self,
        tool_name: &str,
        error: PluginCallError,
    ) -> ToolResultOutcome {
        let (code, message, retryable) = match error {
            PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            } => (code, message, retryable),
            other => {
                if call_lost_route(&other) {
                    self.mcp_capability = None;
                    self.mcp_tools.clear();
                }
                ("mcp_unavailable".to_string(), other.to_string(), false)
            }
        };
        self.push_notice(format!(
            "model MCP tool `{tool_name}` failed: {code}: {message}"
        ));
        ToolResultOutcome::failed(code, message, retryable)
            .unwrap_or_else(|_| ToolResultOutcome::cancelled("MCP result was invalid").unwrap())
    }
}

fn chat_failure_from_plugin(error: PluginCallError) -> ChatFailure {
    match error {
        PluginCallError::Rejected {
            code,
            message,
            retryable,
            ..
        } => ChatFailure::Request {
            code,
            message,
            retryable,
        },
        PluginCallError::ProtocolViolation { message, .. } => {
            ChatFailure::ProtocolViolation(message)
        }
        other => ChatFailure::Unavailable(other.to_string()),
    }
}

fn tool_approval_lines(tool: &str, summary: &str) -> Vec<String> {
    vec![
        "Approval required for model tool action.".to_string(),
        format!("tool: {tool}"),
        summary.to_string(),
        "Use /approve to run it, /deny to deny it, or /cancel to cancel it.".to_string(),
    ]
}

fn descriptor(id: &str, version: u32) -> Result<CapabilityDescriptor, CapabilityError> {
    CapabilityDescriptor::new(id, version)
}

fn optional_command(executable: &Path, internal_argument: &str, path_env: &str) -> PluginCommand {
    isolate_plugin_command(
        match env::var_os(path_env).filter(|value| !value.is_empty()) {
            Some(path) => PluginCommand::new(PathBuf::from(path)),
            None => PluginCommand::new(executable).arg(internal_argument),
        },
    )
}

fn optional_mcp_command(
    executable: &Path,
    internal_argument: &str,
    path_env: &str,
) -> PluginCommand {
    with_process_environment(
        optional_command(executable, internal_argument, path_env),
        MCP_ENVIRONMENT,
    )
}

fn isolate_plugin_command(command: PluginCommand) -> PluginCommand {
    with_process_environment(command.clear_environment(), SAFE_PLUGIN_ENVIRONMENT)
}

fn with_process_environment(mut command: PluginCommand, names: &[&str]) -> PluginCommand {
    for name in names {
        if let Some(value) = env::var_os(name) {
            command = command.env(*name, value);
        }
    }
    command
}

fn with_model_environment(command: PluginCommand) -> PluginCommand {
    let mut command = with_process_environment(isolate_plugin_command(command), MODEL_ENVIRONMENT);
    if let Some(name) = env::var_os("YUNXI_PROVIDER_API_KEY_ENV").filter(|value| !value.is_empty())
        && let Some(value) = env::var_os(&name)
    {
        command = command.env(name, value);
    }
    command
}

fn optional_skills_command(executable: &Path, root: &Path) -> PluginCommand {
    let mut command = optional_command(
        executable,
        INTERNAL_SKILLS_PLUGIN_ARGUMENT,
        SKILLS_PLUGIN_PATH_ENV,
    )
    .env(SKILLS_ROOT_ENV, root.as_os_str());
    if let Some(value) = env::var_os(SKILLS_DISABLED_ENV) {
        command = command.env(SKILLS_DISABLED_ENV, value);
    }
    if let Some(value) = env::var_os(SKILLS_MODE_ENV) {
        command = command.env(SKILLS_MODE_ENV, value);
    }
    command
}

fn resolve_skills_root(cwd: &Path, notices: &mut Vec<String>) -> Option<PathBuf> {
    let configured = env::var_os(SKILLS_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.join("skills"));
    let candidate = if configured.is_absolute() {
        configured
    } else {
        cwd.join(configured)
    };
    match canonicalize_with_missing(&candidate) {
        Ok(canonical_root) => match std::fs::canonicalize(cwd) {
            Ok(canonical_cwd) if canonical_root.starts_with(&canonical_cwd) => Some(candidate),
            Ok(_) => {
                notices.push(format!(
                    "Skills root is outside the current workspace and was disabled: {}",
                    candidate.display()
                ));
                None
            }
            Err(error) => {
                notices.push(format!("Skills root could not be checked: {error}"));
                None
            }
        },
        Err(error) => {
            notices.push(format!("Skills root could not be checked: {error}"));
            None
        }
    }
}

fn canonicalize_with_missing(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    let mut suffix = Vec::new();
    while !current.exists() {
        let Some(name) = current.file_name() else {
            return std::fs::canonicalize(&current);
        };
        suffix.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            return std::fs::canonicalize(&current);
        };
        current = parent.to_path_buf();
    }
    let mut resolved = std::fs::canonicalize(current)?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    if resolved
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        resolved = std::fs::canonicalize(resolved)?;
    }
    Ok(resolved)
}

fn launch_optional(
    host: &mut ProcessPluginHost,
    id: PluginId,
    display_name: &str,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    notices: &mut Vec<String>,
) -> bool {
    launch_optional_with_expected_capabilities(
        host,
        id,
        display_name,
        command,
        capability,
        std::slice::from_ref(capability),
        OptionalLaunchPolicy {
            required_grants: &[],
            read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
        },
        notices,
    )
}

fn launch_action_optional(
    host: &mut ProcessPluginHost,
    id: PluginId,
    display_name: &str,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    required_grants: &[GrantKind],
    notices: &mut Vec<String>,
) -> bool {
    launch_optional_with_expected_capabilities(
        host,
        id,
        display_name,
        command,
        capability,
        std::slice::from_ref(capability),
        OptionalLaunchPolicy {
            required_grants,
            read_timeout: ACTION_RESPONSE_TIMEOUT,
        },
        notices,
    )
}

fn launch_files_optional(
    host: &mut ProcessPluginHost,
    id: PluginId,
    display_name: &str,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    notices: &mut Vec<String>,
) -> bool {
    launch_optional_with_expected_capabilities(
        host,
        id,
        display_name,
        command,
        capability,
        std::slice::from_ref(capability),
        OptionalLaunchPolicy {
            required_grants: &[GrantKind::WorkspaceRead],
            read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
        },
        notices,
    )
}

fn launch_skills_optional(
    host: &mut ProcessPluginHost,
    id: PluginId,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    notices: &mut Vec<String>,
) -> bool {
    launch_optional_with_expected_capabilities(
        host,
        id,
        "Read-only Skills metadata and context",
        command,
        capability,
        std::slice::from_ref(capability),
        OptionalLaunchPolicy {
            required_grants: &[GrantKind::WorkspaceRead],
            read_timeout: READ_ONLY_RESPONSE_TIMEOUT,
        },
        notices,
    )
}

fn mcp_required_grants() -> Vec<GrantKind> {
    let transport = env::var(TRANSPORT_ENV)
        .unwrap_or_else(|_| "stdio".to_string())
        .trim()
        .to_ascii_lowercase();
    if matches!(transport.as_str(), "http" | "https" | "streamable-http") {
        vec![GrantKind::Approval, GrantKind::Network]
    } else {
        vec![GrantKind::Approval]
    }
}

fn mcp_authority_from_env(notices: &mut Vec<String>) -> (Option<NetworkGrant>, SecretGrant) {
    let transport = env::var(TRANSPORT_ENV)
        .unwrap_or_else(|_| "stdio".to_string())
        .trim()
        .to_ascii_lowercase();
    if !matches!(transport.as_str(), "http" | "https" | "streamable-http") {
        return (None, SecretGrant::empty());
    }

    let endpoint = env::var(HTTP_ENDPOINT_ENV).ok();
    if endpoint
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        notices.push(format!(
            "MCP HTTP transport is enabled but {HTTP_ENDPOINT_ENV} is missing"
        ));
    }

    let network = match env::var(NETWORK_GRANT_ENV) {
        Ok(raw) => match parse_network_grant(&raw) {
            Ok(grant) => Some(grant),
            Err(error) => {
                notices.push(format!("MCP network grant was ignored: {error}"));
                None
            }
        },
        Err(env::VarError::NotPresent) => {
            notices.push(format!(
                "MCP HTTP transport is disabled until {NETWORK_GRANT_ENV} names an endpoint"
            ));
            None
        }
        Err(error) => {
            notices.push(format!("MCP network grant could not be read: {error}"));
            None
        }
    };

    let secrets = match env::var(SECRET_GRANT_ENV) {
        Ok(raw) => match parse_secret_grant(&raw) {
            Ok(grant) => grant,
            Err(error) => {
                notices.push(format!("MCP secret grant was ignored: {error}"));
                SecretGrant::empty()
            }
        },
        Err(env::VarError::NotPresent) => SecretGrant::empty(),
        Err(error) => {
            notices.push(format!("MCP secret grant could not be read: {error}"));
            SecretGrant::empty()
        }
    };
    (network, secrets)
}

fn parse_network_grant(raw: &str) -> Result<NetworkGrant, String> {
    let values = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|error| format!("invalid JSON: {error}"))?;
    let values = values
        .as_array()
        .ok_or_else(|| "expected a JSON array of endpoint URLs".to_string())?;
    let scopes = values
        .iter()
        .map(|value| {
            let endpoint = value
                .as_str()
                .ok_or_else(|| "every network scope must be a URL string".to_string())?;
            NetworkScope::from_url(endpoint).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    NetworkGrant::new(scopes).map_err(|error| error.to_string())
}

fn parse_secret_grant(raw: &str) -> Result<SecretGrant, String> {
    let values = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|error| format!("invalid JSON: {error}"))?;
    let values = values
        .as_array()
        .ok_or_else(|| "expected a JSON array of secret references".to_string())?;
    let references = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| "every secret grant entry must be a string".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    SecretGrant::new(references).map_err(|error| error.to_string())
}

struct OptionalLaunchPolicy<'a> {
    required_grants: &'a [GrantKind],
    read_timeout: Duration,
}

#[allow(clippy::too_many_arguments)]
fn launch_optional_with_expected_capabilities(
    host: &mut ProcessPluginHost,
    id: PluginId,
    display_name: &str,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    expected_capabilities: &[CapabilityDescriptor],
    policy: OptionalLaunchPolicy<'_>,
    notices: &mut Vec<String>,
) -> bool {
    let launch = PluginLaunch::new(id.clone(), command)
        .with_display_name(display_name)
        .with_handshake_timeout(HANDSHAKE_TIMEOUT)
        .with_io_timeouts(Some(policy.read_timeout), Some(WRITE_TIMEOUT))
        .with_required_grants(policy.required_grants.iter().copied())
        .with_expected_capabilities(expected_capabilities.iter().cloned());
    if let Err(error) = host.launch(launch) {
        notices.push(format!("optional plugin `{id}` failed to start: {error}"));
        return false;
    }
    match host
        .catalog()
        .resolve_unique(capability.id().as_str(), capability.version())
    {
        Ok(provider) if provider.id() == &id => true,
        Ok(provider) => {
            notices.push(format!(
                "optional plugin `{id}` capability routed to unexpected provider `{}`",
                provider.id()
            ));
            host.stop(&id);
            false
        }
        Err(error) => {
            notices.push(format!(
                "optional plugin `{id}` did not provide {}@{}: {error}",
                capability.id(),
                capability.version()
            ));
            host.stop(&id);
            false
        }
    }
}

fn optional_secondary_capability(
    host: &mut ProcessPluginHost,
    expected: &PluginId,
    capability: &CapabilityDescriptor,
    notices: &mut Vec<String>,
) -> bool {
    match host
        .catalog()
        .resolve_unique(capability.id().as_str(), capability.version())
    {
        Ok(provider) if provider.id() == expected => true,
        Ok(provider) => {
            notices.push(format!(
                "{}@{} routed to unexpected provider `{}` instead of `{expected}`",
                capability.id(),
                capability.version(),
                provider.id()
            ));
            false
        }
        Err(error) => {
            notices.push(format!(
                "plugin `{expected}` did not provide {}@{}: {error}",
                capability.id(),
                capability.version()
            ));
            false
        }
    }
}

fn require_provider(
    host: &mut ProcessPluginHost,
    expected: &PluginId,
    capability: &CapabilityDescriptor,
) -> Result<(), SessionError> {
    let provider = host
        .catalog()
        .resolve_unique(capability.id().as_str(), capability.version())?;
    if provider.id() == expected {
        Ok(())
    } else {
        let actual = provider.id().clone();
        host.stop(expected);
        Err(SessionError::ProviderMismatch {
            capability: capability.clone(),
            expected: expected.clone(),
            actual,
        })
    }
}

fn call_lost_route(error: &PluginCallError) -> bool {
    matches!(
        error,
        PluginCallError::Route(_)
            | PluginCallError::Unavailable { .. }
            | PluginCallError::ProtocolViolation { .. }
    )
}

fn legacy_engine_requested() -> bool {
    env::var(AGENT_ENGINE_ENV).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "legacy" | "compat" | "compatibility"
        )
    })
}

fn memory_recall_query(messages: &[ChatMessage]) -> String {
    const RECENT_CONTEXT_BUDGET_CHARS: usize = 600;
    const RECENT_MESSAGE_MAX_CHARS: usize = 200;

    let Some(current_index) = messages
        .iter()
        .rposition(|message| message.role() == yunxi_protocol::ChatRole::User)
    else {
        return String::new();
    };
    let current = messages[current_index].content();
    let mut recent = Vec::new();
    let mut used = 0;
    for message in messages[..current_index].iter().rev().filter(|message| {
        matches!(
            message.role(),
            yunxi_protocol::ChatRole::User | yunxi_protocol::ChatRole::Assistant
        )
    }) {
        if recent.len() >= 4 || used >= RECENT_CONTEXT_BUDGET_CHARS {
            break;
        }
        let remaining = RECENT_CONTEXT_BUDGET_CHARS
            .saturating_sub(used)
            .min(RECENT_MESSAGE_MAX_CHARS);
        let bounded = message
            .content()
            .chars()
            .take(remaining)
            .collect::<String>();
        used += bounded.chars().count();
        if !bounded.trim().is_empty() {
            recent.push(bounded);
        }
    }
    recent.reverse();
    recent.push(current.to_string());
    recent.join("\n")
}

fn latest_user_message(messages: &[ChatMessage]) -> Option<&str> {
    messages
        .iter()
        .rfind(|message| message.role() == yunxi_protocol::ChatRole::User)
        .map(ChatMessage::content)
}

fn build_composition(switches: &PluginSwitches) -> Result<CompositionSnapshot, CompositionError> {
    let mut profile = Profile::new("yunxi-next")?;
    let mut builtin = ConfigLayer::new("yunxi-next.builtin")?;
    let persona_process_needed = switches.persona;
    let entries = [
        (MODEL_PLUGIN_ID, true),
        (CONTEXT_PLUGIN_ID, switches.context),
        (PERSONA_PLUGIN_ID, persona_process_needed),
        (MEMORY_PLUGIN_ID, switches.memory),
        (STORAGE_PLUGIN_ID, switches.storage),
        (COMPANION_PLUGIN_ID, switches.companion),
        (MAILBOX_PLUGIN_ID, switches.mailbox),
        (SCHEDULER_PLUGIN_ID, switches.scheduler),
        (SHELL_PLUGIN_ID, switches.shell),
        (PATCH_PLUGIN_ID, switches.patch),
        (FILES_PLUGIN_ID, switches.files),
        (MCP_PLUGIN_ID, switches.mcp),
        (SKILLS_PLUGIN_ID, switches.skills),
        (MULTI_AGENT_PLUGIN_ID, switches.multi_agent),
        (VOICE_FIXTURE_PLUGIN_ID, switches.voice),
        (WEIXIN_PLUGIN_ID, switches.weixin),
    ]
    .into_iter()
    .map(|(id, requested_enabled)| {
        let manifest = plugin_policy::manifest_for(id);
        let enabled = if manifest.role().is_user_toggleable() {
            requested_enabled
        } else {
            true
        };
        CompositionEntry::new_with_manifest(id, format!("yunxi.plugin.{id}"), manifest)
            .map(|entry| entry.with_enabled(enabled))
    })
    .collect::<Result<Vec<_>, _>>()?;
    builtin.insert(entries)?;
    profile.add_bundle(builtin)?;
    profile.compose()
}

pub(crate) fn configured_dynamic_plugin_root() -> Option<PathBuf> {
    if let Some(path) = env::var_os(DYNAMIC_PLUGIN_DIRECTORY_ENV)
        .or_else(|| env::var_os(DYNAMIC_PLUGIN_DIRECTORY_ALIAS_ENV))
        .filter(|path| !path.is_empty())
    {
        return Some(PathBuf::from(path));
    }
    let default_root = next_state_root().join(DYNAMIC_PLUGIN_DIRECTORY_NAME);
    default_root.is_dir().then_some(default_root)
}

pub(super) fn dynamic_inventory_entry(
    plugin: &yunxi_plugin_host::DiscoveredPlugin,
    overrides: &BTreeMap<String, bool>,
    phase: Option<PluginFiberPhase>,
) -> Result<PluginInventoryEntry, CompositionError> {
    let package = plugin.manifest();
    let risk = match package.runtime_metadata().map(|metadata| metadata.risk()) {
        None | Some(yunxi_protocol::PluginRiskLevel::Safe) => PluginRisk::None,
        Some(yunxi_protocol::PluginRiskLevel::External | yunxi_protocol::PluginRiskLevel::High) => {
            PluginRisk::External
        }
    };
    let default_enablement = if package.default_enabled() {
        DefaultEnablement::Safe
    } else {
        DefaultEnablement::Never
    };
    let manifest = PluginManifest::new(PluginRole::Optional, risk, default_enablement);
    let enabled = overrides
        .get(package.id().as_str())
        .copied()
        .unwrap_or_else(|| package.default_enabled());
    PluginInventoryEntry::external(
        package.id().as_str(),
        format!("yunxi.dynamic.{}", package.id()),
        enabled,
        phase,
        manifest,
    )
    .map_err(CompositionError::from)
}

pub(super) fn dynamic_plugin_phase(
    id: &PluginId,
    enabled: bool,
    snapshot: &yunxi_kernel::KernelSnapshot,
    report: Option<&PluginReloadReport>,
) -> Option<PluginFiberPhase> {
    if !enabled {
        return Some(PluginFiberPhase::Disabled);
    }
    if report.is_some_and(|report| {
        report
            .skipped()
            .iter()
            .chain(report.failures().iter())
            .any(|failure| failure.id() == Some(id))
    }) {
        return Some(PluginFiberPhase::Failed);
    }
    let state = snapshot
        .plugins()
        .iter()
        .find(|plugin| plugin.id() == id)
        .map(|plugin| plugin.state());
    match state {
        Some(yunxi_kernel::PluginState::Registered) => Some(PluginFiberPhase::Pending),
        Some(yunxi_kernel::PluginState::Starting) => Some(PluginFiberPhase::Loading),
        Some(yunxi_kernel::PluginState::Running { .. }) => Some(PluginFiberPhase::Active),
        Some(yunxi_kernel::PluginState::Stopping) => Some(PluginFiberPhase::Unloading),
        Some(yunxi_kernel::PluginState::Stopped) => Some(PluginFiberPhase::Disabled),
        Some(yunxi_kernel::PluginState::Failed(_)) => Some(PluginFiberPhase::Failed),
        None => Some(PluginFiberPhase::Pending),
    }
}

fn append_dynamic_report_notices(report: &PluginReloadReport, notices: &mut Vec<String>) {
    if !report.launched().is_empty() {
        notices.push(format!(
            "dynamic plugins launched: {}",
            report
                .launched()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !report.removed().is_empty() {
        notices.push(format!(
            "dynamic plugins removed: {}",
            report
                .removed()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for failure in report.discovery_failures() {
        notices.push(format!(
            "dynamic plugin package `{}` was rejected: {}",
            failure.path().display(),
            failure.error()
        ));
    }
    for failure in report.skipped().iter().chain(report.failures().iter()) {
        let id = failure
            .id()
            .map_or_else(|| "<unknown>".to_string(), ToString::to_string);
        notices.push(format!(
            "dynamic plugin `{id}` unavailable: {}",
            failure.reason()
        ));
    }
}

#[derive(Debug)]
pub(crate) enum SessionError {
    Config(ProviderConfigError),
    Executable(io::Error),
    PluginId(PluginIdError),
    Capability(CapabilityError),
    Catalog(CatalogError),
    Host(PluginHostError),
    DynamicDiscovery(PluginDiscoveryManagerError),
    Composition(CompositionError),
    Cordis(RuntimeError),
    ProviderMismatch {
        capability: CapabilityDescriptor,
        expected: PluginId,
        actual: PluginId,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "provider configuration failed: {error}"),
            Self::Executable(error) => write!(formatter, "failed to resolve host paths: {error}"),
            Self::PluginId(error) => write!(formatter, "plugin id is invalid: {error}"),
            Self::Capability(error) => write!(formatter, "capability is invalid: {error}"),
            Self::Catalog(error) => write!(formatter, "capability routing failed: {error}"),
            Self::Host(error) => write!(formatter, "plugin host failed: {error}"),
            Self::DynamicDiscovery(error) => {
                write!(formatter, "dynamic plugin discovery failed: {error}")
            }
            Self::Composition(error) => write!(formatter, "plugin composition failed: {error}"),
            Self::Cordis(error) => write!(formatter, "Cordis runtime failed: {error}"),
            Self::ProviderMismatch {
                capability,
                expected,
                actual,
            } => write!(
                formatter,
                "{}@{} resolved to `{actual}` instead of `{expected}`",
                capability.id(),
                capability.version()
            ),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Executable(error) => Some(error),
            Self::PluginId(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Catalog(error) => Some(error),
            Self::Host(error) => Some(error),
            Self::DynamicDiscovery(error) => Some(error),
            Self::Composition(error) => Some(error),
            Self::Cordis(error) => Some(error),
            Self::ProviderMismatch { .. } => None,
        }
    }
}

impl From<ProviderConfigError> for SessionError {
    fn from(error: ProviderConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Executable(error)
    }
}

impl From<PluginIdError> for SessionError {
    fn from(error: PluginIdError) -> Self {
        Self::PluginId(error)
    }
}

impl From<CapabilityError> for SessionError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<CatalogError> for SessionError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<PluginHostError> for SessionError {
    fn from(error: PluginHostError) -> Self {
        Self::Host(error)
    }
}

impl From<PluginDiscoveryManagerError> for SessionError {
    fn from(error: PluginDiscoveryManagerError) -> Self {
        Self::DynamicDiscovery(error)
    }
}

impl From<CompositionError> for SessionError {
    fn from(error: CompositionError) -> Self {
        Self::Composition(error)
    }
}

impl From<RuntimeError> for SessionError {
    fn from(error: RuntimeError) -> Self {
        Self::Cordis(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChatFailure {
    Request {
        code: String,
        message: String,
        retryable: bool,
    },
    Unavailable(String),
    ProtocolViolation(String),
    ToolApprovalRequired {
        tool: String,
        summary: String,
    },
    ToolLoop(String),
}

impl fmt::Display for ChatFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request {
                code,
                message,
                retryable,
            } => {
                write!(formatter, "model request failed ({code}): {message}")?;
                if *retryable {
                    formatter.write_str(" [retryable]")?;
                }
                Ok(())
            }
            Self::Unavailable(message) => write!(formatter, "model plugin unavailable: {message}"),
            Self::ProtocolViolation(message) => {
                write!(formatter, "model plugin protocol violation: {message}")
            }
            Self::ToolApprovalRequired { tool, summary } => write!(
                formatter,
                "Approval required for model tool action; tool: {tool}; {summary}; use /approve, /deny, or /cancel"
            ),
            Self::ToolLoop(message) => write!(formatter, "model tool loop stopped: {message}"),
        }
    }
}

impl Error for ChatFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_query_includes_bounded_recent_conversation() {
        let messages = vec![
            ChatMessage::user("first"),
            ChatMessage::assistant("second"),
            ChatMessage::user("current"),
        ];
        assert_eq!(memory_recall_query(&messages), "first\nsecond\ncurrent");
    }

    #[test]
    fn optional_plugin_commands_are_cleared_and_mcp_keeps_only_explicit_configuration() {
        let optional = optional_command(Path::new("yunxi-next"), "__fixture", "__missing_path");
        assert!(optional.environment_is_cleared());
        assert!(
            !optional
                .environment()
                .contains_key(std::ffi::OsStr::new("DEEPSEEK_API_KEY"))
        );
        assert!(
            !optional
                .environment()
                .contains_key(std::ffi::OsStr::new("OPENAI_API_KEY"))
        );

        let mcp = optional_mcp_command(Path::new("yunxi-next"), "__fixture", "__missing_path");
        assert!(mcp.environment_is_cleared());
        assert!(
            !mcp.environment()
                .contains_key(std::ffi::OsStr::new("DEEPSEEK_API_KEY"))
        );
        assert!(
            !mcp.environment()
                .contains_key(std::ffi::OsStr::new("OPENAI_API_KEY"))
        );
    }
}
