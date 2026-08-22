//! Multi-plugin host orchestration and chat request assembly.

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use yunxi_context::CONTEXT_PLUGIN_ID;
use yunxi_kernel::{PluginCommand, PluginId, PluginIdError};
use yunxi_memory::MEMORY_PLUGIN_ID;
use yunxi_model_openai::{MODEL_PLUGIN_ID, ProviderConfig, ProviderConfigError};
use yunxi_persona::PERSONA_PLUGIN_ID;
use yunxi_plugin_host::{
    CatalogError, PluginCallError, PluginHostError, PluginLaunch, ProcessPluginHost,
};
use yunxi_protocol::{
    CONTEXT_COMPOSE_OPERATION, CapabilityDescriptor, CapabilityError, ChatMessage, ChatRequest,
    ChatResult, ContextComposeRequest, ContextComposeResult, MEMORY_RECALL_OPERATION,
    MODEL_CHAT_COMPLETE_OPERATION, MemoryContextRecord, MemoryRecallRequest, MemoryRecallResult,
    PERSONA_CONTEXT_COMPILE_OPERATION, PersonaContextRequest, PersonaContextResult, capabilities,
};

use crate::{
    INTERNAL_CONTEXT_PLUGIN_ARGUMENT, INTERNAL_MEMORY_PLUGIN_ARGUMENT,
    INTERNAL_MODEL_PLUGIN_ARGUMENT, INTERNAL_PERSONA_PLUGIN_ARGUMENT,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const READ_ONLY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT_MARGIN: Duration = Duration::from_secs(5);

const CONTEXT_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_CONTEXT_PLUGIN";
const MEMORY_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_MEMORY_PLUGIN";
const PERSONA_PLUGIN_PATH_ENV: &str = "YUNXI_NEXT_PERSONA_PLUGIN";

pub(crate) trait ChatBackend {
    fn complete(&mut self, messages: &[ChatMessage]) -> Result<String, ChatFailure>;
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    fn status(&mut self) -> BackendStatus;
    fn drain_notices(&mut self) -> Vec<String>;
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
    host: ProcessPluginHost,
    model_plugin_id: PluginId,
    model_capability: CapabilityDescriptor,
    context_capability: Option<CapabilityDescriptor>,
    memory_capability: Option<CapabilityDescriptor>,
    persona_capability: Option<CapabilityDescriptor>,
    cwd: PathBuf,
    provider: String,
    model: String,
    first_turn: bool,
    notices: Vec<String>,
    reported_notices: BTreeSet<String>,
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
        let switches = CapabilitySwitches::from_env();
        let mut host = ProcessPluginHost::new();
        let mut notices = Vec::new();

        let model_plugin_id = PluginId::new(MODEL_PLUGIN_ID)?;
        let model_command = match plugin_path {
            Some(path) => PluginCommand::new(path),
            None => PluginCommand::new(&executable).arg(INTERNAL_MODEL_PLUGIN_ARGUMENT),
        };
        host.launch(
            PluginLaunch::new(model_plugin_id.clone(), model_command)
                .with_display_name("OpenAI-compatible chat model")
                .with_handshake_timeout(HANDSHAKE_TIMEOUT)
                .with_io_timeouts(Some(response_timeout), Some(WRITE_TIMEOUT)),
        )?;
        let model_capability =
            descriptor(capabilities::MODEL_CHAT, capabilities::MODEL_CHAT_VERSION)?;
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

        let persona_process_needed = switches.persona || switches.memory;
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

        let memory_capability = if switches.memory && persona_capability.is_some() {
            let id = PluginId::new(MEMORY_PLUGIN_ID)?;
            let capability = descriptor(
                capabilities::MEMORY_RECALL,
                capabilities::MEMORY_RECALL_VERSION,
            )?;
            launch_optional(
                &mut host,
                id,
                "Read-only long-term memory",
                optional_command(
                    &executable,
                    INTERNAL_MEMORY_PLUGIN_ARGUMENT,
                    MEMORY_PLUGIN_PATH_ENV,
                ),
                &capability,
                &mut notices,
            )
            .then_some(capability)
        } else {
            if switches.memory && persona_capability.is_none() {
                notices.push(
                    "memory recall stayed disabled because its context compiler was unavailable"
                        .to_string(),
                );
            }
            None
        };

        let reported_notices = notices.iter().cloned().collect();
        Ok(Self {
            host,
            model_plugin_id,
            model_capability,
            context_capability,
            memory_capability,
            persona_capability,
            cwd,
            provider,
            model,
            first_turn: true,
            notices,
            reported_notices,
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

        if let Some(capability) = self.persona_capability.clone() {
            let request =
                PersonaContextRequest::new(boot_memories, dynamic_memories, include_boot_context);
            match self.host.invoke::<_, PersonaContextResult>(
                &capability,
                PERSONA_CONTEXT_COMPILE_OPERATION,
                &request,
            ) {
                Ok(persona) => {
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
        let messages = self.assemble_messages(messages);
        let result = self.host.invoke::<_, ChatResult>(
            &self.model_capability,
            MODEL_CHAT_COMPLETE_OPERATION,
            &ChatRequest::new(messages),
        );
        match result {
            Ok(result) => Ok(result.content().to_string()),
            Err(PluginCallError::Rejected {
                code,
                message,
                retryable,
                ..
            }) => Err(ChatFailure::Request {
                code,
                message,
                retryable,
            }),
            Err(PluginCallError::ProtocolViolation { message, .. }) => {
                Err(ChatFailure::ProtocolViolation(message))
            }
            Err(error) => Err(ChatFailure::Unavailable(error.to_string())),
        }
    }

    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn status(&mut self) -> BackendStatus {
        let snapshot = self.host.snapshot();
        let plugin = snapshot
            .plugins()
            .iter()
            .find(|plugin| plugin.id() == &self.model_plugin_id)
            .map(|plugin| plugin.state().to_string())
            .unwrap_or_else(|| "not registered".to_string());
        let protocol_ready = self
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
}

fn descriptor(id: &str, version: u32) -> Result<CapabilityDescriptor, CapabilityError> {
    CapabilityDescriptor::new(id, version)
}

fn optional_command(executable: &Path, internal_argument: &str, path_env: &str) -> PluginCommand {
    match env::var_os(path_env).filter(|value| !value.is_empty()) {
        Some(path) => PluginCommand::new(PathBuf::from(path)),
        None => PluginCommand::new(executable).arg(internal_argument),
    }
}

fn launch_optional(
    host: &mut ProcessPluginHost,
    id: PluginId,
    display_name: &str,
    command: PluginCommand,
    capability: &CapabilityDescriptor,
    notices: &mut Vec<String>,
) -> bool {
    let launch = PluginLaunch::new(id.clone(), command)
        .with_display_name(display_name)
        .with_handshake_timeout(HANDSHAKE_TIMEOUT)
        .with_io_timeouts(Some(READ_ONLY_RESPONSE_TIMEOUT), Some(WRITE_TIMEOUT));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CapabilitySwitches {
    context: bool,
    persona: bool,
    memory: bool,
}

impl CapabilitySwitches {
    fn from_env() -> Self {
        Self::from_reader(|name| env::var(name).ok())
    }

    fn from_reader<F>(read: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            context: read_bool(&read, "YUNXI_NEXT_CONTEXT_ENABLED").unwrap_or(true),
            persona: read_bool(&read, "YUNXI_NEXT_PERSONA_ENABLED")
                .or_else(|| read_bool(&read, "YUNXI_PERSONA_ENABLED"))
                .unwrap_or(true),
            memory: read_bool(&read, "YUNXI_NEXT_MEMORY_ENABLED")
                .or_else(|| read_bool(&read, "YUNXI_MEMORY_ENABLED"))
                .unwrap_or(false),
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

#[derive(Debug)]
pub(crate) enum SessionError {
    Config(ProviderConfigError),
    Executable(io::Error),
    PluginId(PluginIdError),
    Capability(CapabilityError),
    Catalog(CatalogError),
    Host(PluginHostError),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChatFailure {
    Request {
        code: String,
        message: String,
        retryable: bool,
    },
    Unavailable(String),
    ProtocolViolation(String),
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
        }
    }
}

impl Error for ChatFailure {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn new_switches_override_legacy_defaults_without_enabling_memory_implicitly() {
        let values = BTreeMap::from([
            ("YUNXI_PERSONA_ENABLED", "false"),
            ("YUNXI_NEXT_PERSONA_ENABLED", "true"),
            ("YUNXI_NEXT_CONTEXT_ENABLED", "false"),
        ]);
        let switches = CapabilitySwitches::from_reader(|name| {
            values.get(name).map(|value| (*value).to_string())
        });

        assert_eq!(
            switches,
            CapabilitySwitches {
                context: false,
                persona: true,
                memory: false,
            }
        );
    }

    #[test]
    fn recall_query_includes_bounded_recent_conversation() {
        let messages = vec![
            ChatMessage::user("first"),
            ChatMessage::assistant("second"),
            ChatMessage::user("current"),
        ];
        assert_eq!(memory_recall_query(&messages), "first\nsecond\ncurrent");
    }
}
