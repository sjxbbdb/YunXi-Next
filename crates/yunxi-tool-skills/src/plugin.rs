//! Isolated protocol loop for read-only Skill metadata and context.

use std::error::Error;
use std::fmt;
use std::process;
use std::time::Duration;

use yunxi_protocol::{
    CapabilityDescriptor, CapabilityError, GrantKind, GrantRequirement, HostMessage,
    InvocationCodecError, InvocationResponse, MAX_SKILL_CONTEXT_BYTES, PluginMessage,
    ProtocolError, SkillContextBlock, SkillContextRequest, SkillContextResult, SkillListRequest,
    SkillListResult, SkillRuntimeState, SkillStatusRequest, SkillStatusResult,
    TOOL_SKILLS_CONTEXT_OPERATION, TOOL_SKILLS_LIST_OPERATION, TOOL_SKILLS_STATUS_OPERATION,
    capabilities, connect_plugin_with_grants,
};

use crate::discovery::{DiscoverySnapshot, discover};
use crate::{SKILLS_MODE_ENV, SkillsConfig, SkillsConfigError};

pub const SKILLS_PLUGIN_ID: &str = "yunxi.tool.skills";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run_skills_plugin() -> Result<(), SkillsPluginError> {
    let config = SkillsConfig::from_env()?;
    let mode = std::env::var(SKILLS_MODE_ENV).unwrap_or_default();
    if mode == "crash-before-handshake" {
        process::exit(42);
    }
    let mut snapshot = discover(&config)?;
    let capability =
        CapabilityDescriptor::new(capabilities::TOOL_SKILLS, capabilities::TOOL_SKILLS_VERSION)?;
    let mut session = connect_plugin_with_grants(
        SKILLS_PLUGIN_ID,
        "Read-only Skills metadata and context",
        env!("CARGO_PKG_VERSION"),
        vec![capability],
        vec![GrantRequirement::required(GrantKind::WorkspaceRead)],
        CONNECT_TIMEOUT,
    )?;

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let request_id = request.request_id();
                if request.capability().id().as_str() != capabilities::TOOL_SKILLS
                    || request.capability().version() != capabilities::TOOL_SKILLS_VERSION
                    || !matches!(
                        request.operation(),
                        TOOL_SKILLS_LIST_OPERATION
                            | TOOL_SKILLS_CONTEXT_OPERATION
                            | TOOL_SKILLS_STATUS_OPERATION
                    )
                {
                    send_failure(
                        &mut session,
                        request_id,
                        "unsupported_operation",
                        "Skills plugin does not support the requested operation".to_string(),
                    )?;
                    continue;
                }
                if mode == "crash-list" && request.operation() == TOOL_SKILLS_LIST_OPERATION {
                    process::exit(43);
                }
                if mode == "crash-context" && request.operation() == TOOL_SKILLS_CONTEXT_OPERATION {
                    process::exit(44);
                }
                match request.operation() {
                    TOOL_SKILLS_LIST_OPERATION => {
                        let payload = match request.decode_payload::<SkillListRequest>() {
                            Ok(payload) => payload,
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    "invalid_request",
                                    error.to_string(),
                                )?;
                                continue;
                            }
                        };
                        if payload.refresh() {
                            match discover(&config) {
                                Ok(next) => snapshot = next,
                                Err(error) => {
                                    send_failure(
                                        &mut session,
                                        request_id,
                                        "skills_discovery",
                                        error.to_string(),
                                    )?;
                                    continue;
                                }
                            }
                        }
                        let result = list_result(&snapshot)?;
                        send_response(&mut session, request_id, &result)?;
                    }
                    TOOL_SKILLS_CONTEXT_OPERATION => {
                        let payload = match request.decode_payload::<SkillContextRequest>() {
                            Ok(payload) => payload,
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    "invalid_request",
                                    error.to_string(),
                                )?;
                                continue;
                            }
                        };
                        let result = context_result(&snapshot, &payload)?;
                        send_response(&mut session, request_id, &result)?;
                    }
                    TOOL_SKILLS_STATUS_OPERATION => {
                        let _payload = match request.decode_payload::<SkillStatusRequest>() {
                            Ok(payload) => payload,
                            Err(error) => {
                                send_failure(
                                    &mut session,
                                    request_id,
                                    "invalid_request",
                                    error.to_string(),
                                )?;
                                continue;
                            }
                        };
                        let tool_count = snapshot
                            .skills
                            .iter()
                            .map(|skill| skill.metadata.tools().len())
                            .sum();
                        let result = SkillStatusResult::new(
                            snapshot.root.clone(),
                            SkillRuntimeState::Ready,
                            snapshot.skills.len(),
                            tool_count,
                            None,
                        )?;
                        send_response(&mut session, request_id, &result)?;
                    }
                    _ => unreachable!("operation checked above"),
                }
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => {
                return Err(SkillsPluginError::UnexpectedHostMessage(
                    "received a second welcome after readiness".to_string(),
                ));
            }
        }
    }
}

fn list_result(snapshot: &DiscoverySnapshot) -> Result<SkillListResult, SkillsPluginError> {
    let skills = snapshot
        .skills
        .iter()
        .map(|skill| skill.metadata.clone())
        .collect();
    SkillListResult::new(
        snapshot.root.clone(),
        skills,
        snapshot.warnings.clone(),
        snapshot.truncated,
    )
    .map_err(SkillsPluginError::ProtocolData)
}

fn context_result(
    snapshot: &DiscoverySnapshot,
    request: &SkillContextRequest,
) -> Result<SkillContextResult, SkillsPluginError> {
    let mut blocks = Vec::new();
    let mut warnings = snapshot.warnings.clone();
    let mut context_bytes = 0usize;
    let mut truncated = false;
    for id in request.skill_ids() {
        let Some(skill) = snapshot
            .skills
            .iter()
            .find(|skill| skill.metadata.id() == id)
        else {
            warnings.push(format!("Skill `{id}` is not available"));
            continue;
        };
        if skill.instructions.trim().is_empty() {
            warnings.push(format!("Skill `{id}` has no instruction body"));
            continue;
        }
        if context_bytes.saturating_add(skill.instructions.len()) > MAX_SKILL_CONTEXT_BYTES {
            warnings.push(format!(
                "Skill `{id}` was omitted because the context budget was exhausted"
            ));
            truncated = true;
            continue;
        }
        context_bytes = context_bytes.saturating_add(skill.instructions.len());
        blocks.push(SkillContextBlock::new(id, skill.instructions.clone())?);
    }
    SkillContextResult::new(blocks, warnings.into_iter().take(32).collect(), truncated)
        .map_err(SkillsPluginError::ProtocolData)
}

fn send_response<T: serde::Serialize>(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    result: &T,
) -> Result<(), SkillsPluginError> {
    let response = InvocationResponse::encode(request_id, result)?;
    session.send(&PluginMessage::InvocationCompleted { response })?;
    Ok(())
}

fn send_failure(
    session: &mut yunxi_protocol::PluginSession,
    request_id: u64,
    code: &str,
    message: String,
) -> Result<(), ProtocolError> {
    session.send(&PluginMessage::InvocationFailed {
        request_id,
        code: code.to_string(),
        message,
        retryable: false,
    })
}

#[derive(Debug)]
pub enum SkillsPluginError {
    Config(SkillsConfigError),
    Discovery(crate::discovery::DiscoveryError),
    Capability(CapabilityError),
    Invocation(InvocationCodecError),
    Protocol(ProtocolError),
    ProtocolData(yunxi_protocol::SkillProtocolError),
    UnexpectedHostMessage(String),
}

impl fmt::Display for SkillsPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Discovery(error) => error.fmt(formatter),
            Self::Capability(error) => write!(formatter, "invalid capability: {error}"),
            Self::Invocation(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::ProtocolData(error) => error.fmt(formatter),
            Self::UnexpectedHostMessage(message) => formatter.write_str(message),
        }
    }
}

impl Error for SkillsPluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Discovery(error) => Some(error),
            Self::Capability(error) => Some(error),
            Self::Invocation(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::ProtocolData(error) => Some(error),
            Self::UnexpectedHostMessage(_) => None,
        }
    }
}

impl From<SkillsConfigError> for SkillsPluginError {
    fn from(error: SkillsConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<crate::discovery::DiscoveryError> for SkillsPluginError {
    fn from(error: crate::discovery::DiscoveryError) -> Self {
        Self::Discovery(error)
    }
}

impl From<CapabilityError> for SkillsPluginError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<InvocationCodecError> for SkillsPluginError {
    fn from(error: InvocationCodecError) -> Self {
        Self::Invocation(error)
    }
}

impl From<ProtocolError> for SkillsPluginError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<yunxi_protocol::SkillProtocolError> for SkillsPluginError {
    fn from(error: yunxi_protocol::SkillProtocolError) -> Self {
        Self::ProtocolData(error)
    }
}

#[cfg(test)]
mod tests {
    use yunxi_protocol::{MAX_SKILL_INSTRUCTION_BYTES, SkillMetadata};

    use super::*;
    use crate::discovery::DiscoveredSkill;

    #[test]
    fn context_result_truncates_at_the_total_budget() {
        let mut skills = Vec::new();
        let mut ids = Vec::new();
        for index in 0..5 {
            let id = format!("skill-{index}");
            let instructions = "x".repeat(MAX_SKILL_INSTRUCTION_BYTES);
            let metadata = SkillMetadata::new(
                id.clone(),
                format!("Skill {index}"),
                "fixture",
                format!("{id}/SKILL.md"),
                instructions.len(),
                Vec::new(),
            )
            .expect("Skill metadata");
            ids.push(id);
            skills.push(DiscoveredSkill {
                metadata,
                instructions,
                directory: std::path::PathBuf::from("skills"),
                actions: Vec::new(),
            });
        }
        let snapshot = DiscoverySnapshot {
            root: "skills".to_string(),
            skills,
            warnings: Vec::new(),
            truncated: false,
        };
        let request = SkillContextRequest::new(ids).expect("context request");

        let result = context_result(&snapshot, &request).expect("context result");

        assert_eq!(result.blocks().len(), 4);
        assert!(result.truncated());
        assert!(
            result
                .warnings()
                .iter()
                .any(|warning| warning.contains("budget was exhausted"))
        );
    }
}
