//! CLI-side tool catalog and fail-closed argument decoding.

use std::error::Error;
use std::fmt;

use serde_json::{Value, json};
use yunxi_protocol::{
    ChatMessage, MAX_TOOL_DEFINITIONS, McpToolDescriptor, SkillMetadata, SkillToolDescriptor,
    ToolCall, ToolCatalog, ToolDefinition, ToolName, ToolProtocolError, ToolResultOutcome,
};

pub(crate) const SHELL_TOOL_NAME: &str = "shell.execute";
pub(crate) const PATCH_TOOL_NAME: &str = "patch.apply";
pub(crate) const FILE_SEARCH_TOOL_NAME: &str = "file.search";
pub(crate) const FILE_READ_TOOL_NAME: &str = "file.read";
const MCP_TOOL_PREFIX: &str = "mcp";

const MAX_SHELL_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_PATCH_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const DEFAULT_MODEL_TOOL_TIMEOUT_MILLIS: u64 = 120_000;
const MAX_MODEL_TOOL_TIMEOUT_MILLIS: u64 = 120_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ToolAction {
    Shell {
        command: String,
        timeout_millis: u64,
    },
    Patch {
        patch: String,
        timeout_millis: u64,
    },
    FileSearch {
        query: String,
        path: String,
    },
    FileRead {
        path: String,
    },
    Mcp {
        binding: McpToolBinding,
        arguments: Value,
    },
    Skill {
        binding: SkillToolBinding,
        arguments: Value,
    },
}

impl ToolAction {
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Shell {
                command,
                timeout_millis,
            } => format!("command: {command} (timeout: {timeout_millis} ms)"),
            Self::Patch {
                patch,
                timeout_millis,
            } => format!(
                "patch bytes: {} (timeout: {timeout_millis} ms)",
                patch.len()
            ),
            Self::FileSearch { query, path } => format!("query: {query} (root: {path})"),
            Self::FileRead { path } => format!("path: {path}"),
            Self::Mcp { binding, arguments } => format!(
                "server: {} | tool: {} | arguments: {} bytes",
                binding.server_name,
                binding.remote_name,
                serde_json::to_vec(arguments).map_or(0, |value| value.len())
            ),
            Self::Skill { binding, arguments } => format!(
                "skill: {} | tool: {} | arguments: {} bytes",
                binding.skill_id,
                binding.remote_name,
                serde_json::to_vec(arguments).map_or(0, |value| value.len())
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpToolBinding {
    model_name: ToolName,
    server_name: String,
    remote_name: String,
    description: String,
    input_schema: Value,
}

impl McpToolBinding {
    pub(crate) fn from_descriptor(
        server_name: &str,
        descriptor: &McpToolDescriptor,
    ) -> Result<Self, ToolCallError> {
        let model_name = ToolName::new(format!(
            "{MCP_TOOL_PREFIX}.{server_name}.{}",
            descriptor.name()
        ))
        .map_err(|error| ToolCallError::InvalidMcpToolName {
            remote_name: descriptor.name().to_string(),
            message: error.to_string(),
        })?;
        let description = if descriptor.description().trim().is_empty() {
            format!(
                "Call MCP tool `{}` on server `{server_name}`.",
                descriptor.name()
            )
        } else {
            format!("MCP `{server_name}` tool: {}", descriptor.description())
        };
        let binding = Self {
            model_name,
            server_name: server_name.to_string(),
            remote_name: descriptor.name().to_string(),
            description,
            input_schema: descriptor.input_schema().clone(),
        };
        ToolDefinition::new(
            binding.model_name.clone(),
            binding.description.clone(),
            binding.input_schema.clone(),
        )
        .map_err(|error| ToolCallError::InvalidMcpToolName {
            remote_name: descriptor.name().to_string(),
            message: error.to_string(),
        })?;
        Ok(binding)
    }

    pub(crate) fn model_name(&self) -> &ToolName {
        &self.model_name
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.server_name
    }

    pub(crate) fn remote_name(&self) -> &str {
        &self.remote_name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SkillToolBinding {
    model_name: ToolName,
    skill_id: String,
    remote_name: String,
    description: String,
    input_schema: Value,
}

impl SkillToolBinding {
    pub(crate) fn from_descriptor(
        skill: &SkillMetadata,
        descriptor: &SkillToolDescriptor,
    ) -> Result<Self, ToolCallError> {
        let model_name = ToolName::new(format!("skill.{}.{}", skill.id(), descriptor.name()))
            .map_err(|error| ToolCallError::InvalidSkillToolName {
                skill_id: skill.id().to_string(),
                remote_name: descriptor.name().to_string(),
                message: error.to_string(),
            })?;
        let binding = Self {
            model_name,
            skill_id: skill.id().to_string(),
            remote_name: descriptor.name().to_string(),
            description: format!(
                "Skill `{}` metadata declaration: {}",
                skill.name(),
                descriptor.description()
            ),
            input_schema: descriptor.input_schema().clone(),
        };
        ToolDefinition::new(
            binding.model_name().clone(),
            binding.description.clone(),
            binding.input_schema.clone(),
        )
        .map_err(|error| ToolCallError::InvalidSkillToolName {
            skill_id: skill.id().to_string(),
            remote_name: descriptor.name().to_string(),
            message: error.to_string(),
        })?;
        Ok(binding)
    }

    pub(crate) fn model_name(&self) -> &ToolName {
        &self.model_name
    }

    pub(crate) fn skill_id(&self) -> &str {
        &self.skill_id
    }

    pub(crate) fn remote_name(&self) -> &str {
        &self.remote_name
    }
}

pub(crate) fn catalog_with_skills(
    shell_enabled: bool,
    patch_enabled: bool,
    files_enabled: bool,
    mcp_tools: &[McpToolBinding],
    skill_tools: &[SkillToolBinding],
) -> Option<ToolCatalog> {
    let mut definitions = Vec::new();
    if shell_enabled {
        definitions.push(
            ToolDefinition::new(
                ToolName::new(SHELL_TOOL_NAME).expect("built-in shell tool name"),
                "Execute one bounded shell command after explicit user approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "maxLength": MAX_SHELL_ARGUMENT_BYTES},
                        "timeout_millis": {"type": "integer", "minimum": 1, "maximum": MAX_MODEL_TOOL_TIMEOUT_MILLIS}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
            )
            .expect("built-in shell tool definition"),
        );
    }
    if patch_enabled {
        definitions.push(
            ToolDefinition::new(
                ToolName::new(PATCH_TOOL_NAME).expect("built-in patch tool name"),
                "Apply a bounded workspace patch after explicit user approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "patch": {"type": "string", "maxLength": MAX_PATCH_ARGUMENT_BYTES},
                        "timeout_millis": {"type": "integer", "minimum": 1, "maximum": MAX_MODEL_TOOL_TIMEOUT_MILLIS}
                    },
                    "required": ["patch"],
                    "additionalProperties": false
                }),
            )
            .expect("built-in patch tool definition"),
        );
    }
    if files_enabled {
        definitions.push(
            ToolDefinition::new(
                ToolName::new(FILE_SEARCH_TOOL_NAME).expect("built-in file search tool name"),
                "Search file and directory names inside the granted workspace.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "maxLength": 256},
                        "path": {"type": "string", "maxLength": 1024}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            )
            .expect("built-in file search tool definition"),
        );
        definitions.push(
            ToolDefinition::new(
                ToolName::new(FILE_READ_TOOL_NAME).expect("built-in file read tool name"),
                "Read a bounded UTF-8 text file inside the granted workspace.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "maxLength": 1024}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            )
            .expect("built-in file read tool definition"),
        );
    }
    definitions.extend(mcp_tools.iter().map(|binding| {
        ToolDefinition::new(
            binding.model_name().clone(),
            binding.description.clone(),
            binding.input_schema.clone(),
        )
        .expect("validated MCP tool definition")
    }));
    definitions.extend(skill_tools.iter().map(|binding| {
        ToolDefinition::new(
            binding.model_name.clone(),
            binding.description.clone(),
            binding.input_schema.clone(),
        )
        .expect("validated Skill tool definition")
    }));
    definitions.truncate(MAX_TOOL_DEFINITIONS);
    (!definitions.is_empty()).then(|| ToolCatalog::new(definitions).expect("built-in tool catalog"))
}

pub(crate) fn decode_call_with_skills(
    call: &ToolCall,
    mcp_tools: &[McpToolBinding],
    skill_tools: &[SkillToolBinding],
) -> Result<ToolAction, ToolCallError> {
    let object = call
        .arguments()
        .as_object()
        .ok_or(ToolCallError::ArgumentsMustBeObject)?;
    match call.name().as_str() {
        SHELL_TOOL_NAME => Ok(ToolAction::Shell {
            command: required_text(object, "command", MAX_SHELL_ARGUMENT_BYTES)?,
            timeout_millis: optional_timeout(object)?,
        }),
        PATCH_TOOL_NAME => Ok(ToolAction::Patch {
            patch: required_text(object, "patch", MAX_PATCH_ARGUMENT_BYTES)?,
            timeout_millis: optional_timeout(object)?,
        }),
        FILE_SEARCH_TOOL_NAME => Ok(ToolAction::FileSearch {
            query: required_text(object, "query", 256)?,
            path: optional_text(object, "path", 1024)?.unwrap_or_else(|| ".".to_string()),
        }),
        FILE_READ_TOOL_NAME => Ok(ToolAction::FileRead {
            path: required_text(object, "path", 1024)?,
        }),
        name => mcp_tools
            .iter()
            .find(|binding| binding.model_name.as_str() == name)
            .map(|binding| ToolAction::Mcp {
                binding: binding.clone(),
                arguments: call.arguments().clone(),
            })
            .or_else(|| {
                skill_tools
                    .iter()
                    .find(|binding| binding.model_name.as_str() == name)
                    .map(|binding| ToolAction::Skill {
                        binding: binding.clone(),
                        arguments: call.arguments().clone(),
                    })
            })
            .ok_or_else(|| ToolCallError::UnsupportedTool(name.to_string())),
    }
}

fn optional_timeout(object: &serde_json::Map<String, Value>) -> Result<u64, ToolCallError> {
    let Some(value) = object.get("timeout_millis") else {
        return Ok(DEFAULT_MODEL_TOOL_TIMEOUT_MILLIS);
    };
    let timeout = value.as_u64().ok_or(ToolCallError::TimeoutMustBeInteger)?;
    if timeout == 0 || timeout > MAX_MODEL_TOOL_TIMEOUT_MILLIS {
        return Err(ToolCallError::TimeoutOutOfRange {
            value: timeout,
            maximum: MAX_MODEL_TOOL_TIMEOUT_MILLIS,
        });
    }
    Ok(timeout)
}

fn optional_text(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    maximum: usize,
) -> Result<Option<String>, ToolCallError> {
    object
        .get(field)
        .map(|_| required_text(object, field, maximum))
        .transpose()
}

pub(crate) fn tool_result_message(call: &ToolCall, outcome: &ToolResultOutcome) -> ChatMessage {
    let content = serde_json::to_string(outcome)
        .unwrap_or_else(|error| format!("{{\"status\":\"failed\",\"message\":\"{error}\"}}"));
    ChatMessage::tool_result(call.id(), call.name(), content)
}

fn required_text(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    maximum: usize,
) -> Result<String, ToolCallError> {
    let value = object
        .get(field)
        .ok_or(ToolCallError::MissingArgument { field })?
        .as_str()
        .ok_or(ToolCallError::ArgumentMustBeText { field })?;
    if value.trim().is_empty() {
        return Err(ToolCallError::ArgumentEmpty { field });
    }
    if value.len() > maximum {
        return Err(ToolCallError::ArgumentTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    Ok(value.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ToolCallError {
    UnsupportedTool(String),
    ArgumentsMustBeObject,
    MissingArgument {
        field: &'static str,
    },
    ArgumentMustBeText {
        field: &'static str,
    },
    ArgumentEmpty {
        field: &'static str,
    },
    ArgumentTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    InvalidMcpToolName {
        remote_name: String,
        message: String,
    },
    InvalidSkillToolName {
        skill_id: String,
        remote_name: String,
        message: String,
    },
    TimeoutMustBeInteger,
    TimeoutOutOfRange {
        value: u64,
        maximum: u64,
    },
}

impl fmt::Display for ToolCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTool(name) => write!(formatter, "unsupported model tool `{name}`"),
            Self::ArgumentsMustBeObject => {
                formatter.write_str("model tool arguments must be a JSON object")
            }
            Self::MissingArgument { field } => {
                write!(formatter, "tool argument `{field}` is missing")
            }
            Self::ArgumentMustBeText { field } => {
                write!(formatter, "tool argument `{field}` must be text")
            }
            Self::ArgumentEmpty { field } => {
                write!(formatter, "tool argument `{field}` cannot be empty")
            }
            Self::ArgumentTooLong {
                field,
                length,
                maximum,
            } => write!(
                formatter,
                "tool argument `{field}` is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidMcpToolName {
                remote_name,
                message,
            } => write!(
                formatter,
                "MCP tool `{remote_name}` cannot be exposed to the model: {message}"
            ),
            Self::InvalidSkillToolName {
                skill_id,
                remote_name,
                message,
            } => write!(
                formatter,
                "Skill `{skill_id}` tool `{remote_name}` cannot be exposed to the model: {message}"
            ),
            Self::TimeoutMustBeInteger => {
                formatter.write_str("tool argument `timeout_millis` must be an integer")
            }
            Self::TimeoutOutOfRange { value, maximum } => write!(
                formatter,
                "tool argument `timeout_millis` value {value} is outside 1..={maximum}"
            ),
        }
    }
}

impl Error for ToolCallError {}

impl From<ToolProtocolError> for ToolCallError {
    fn from(error: ToolProtocolError) -> Self {
        Self::UnsupportedTool(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yunxi_protocol::{McpToolDescriptor, SkillMetadata, SkillToolDescriptor};

    use super::*;

    #[test]
    fn catalog_only_exposes_enabled_tools() {
        let tool_catalog =
            catalog_with_skills(true, false, false, &[], &[]).expect("shell catalog");
        assert_eq!(tool_catalog.tools().len(), 1);
        assert_eq!(tool_catalog.tools()[0].name().as_str(), SHELL_TOOL_NAME);
        assert!(super::catalog_with_skills(false, false, false, &[], &[]).is_none());
        let file_catalog = catalog_with_skills(false, false, true, &[], &[]).expect("file catalog");
        assert_eq!(file_catalog.tools().len(), 2);
    }

    #[test]
    fn tool_arguments_are_decoded_and_bounded() {
        let call = ToolCall::new("call-1", SHELL_TOOL_NAME, json!({"command": "echo hello"}))
            .expect("shell call");
        assert_eq!(
            decode_call_with_skills(&call, &[], &[]).expect("decode shell call"),
            ToolAction::Shell {
                command: "echo hello".to_string(),
                timeout_millis: DEFAULT_MODEL_TOOL_TIMEOUT_MILLIS,
            }
        );

        let invalid =
            ToolCall::new("call-2", PATCH_TOOL_NAME, json!({"patch": 1})).expect("wire-valid call");
        assert!(matches!(
            decode_call_with_skills(&invalid, &[], &[]),
            Err(ToolCallError::ArgumentMustBeText { field: "patch" })
        ));

        let timed = ToolCall::new(
            "call-3",
            SHELL_TOOL_NAME,
            json!({"command": "echo hello", "timeout_millis": 25}),
        )
        .expect("timed shell call");
        assert!(matches!(
            decode_call_with_skills(&timed, &[], &[]),
            Ok(ToolAction::Shell {
                timeout_millis: 25,
                ..
            })
        ));

        let invalid_timeout = ToolCall::new(
            "call-4",
            SHELL_TOOL_NAME,
            json!({"command": "echo hello", "timeout_millis": 0}),
        )
        .expect("wire-valid timeout call");
        assert!(matches!(
            decode_call_with_skills(&invalid_timeout, &[], &[]),
            Err(ToolCallError::TimeoutOutOfRange { .. })
        ));

        let file_call = ToolCall::new("call-5", FILE_READ_TOOL_NAME, json!({"path": "src/lib.rs"}))
            .expect("file call");
        assert!(matches!(
            decode_call_with_skills(&file_call, &[], &[]),
            Ok(ToolAction::FileRead { .. })
        ));
    }

    #[test]
    fn mcp_descriptors_project_into_namespaced_model_tools() {
        let descriptor = McpToolDescriptor::new(
            "echo",
            "Return the input",
            json!({"type": "object", "additionalProperties": true}),
        )
        .expect("MCP descriptor");
        let binding = McpToolBinding::from_descriptor("fixture", &descriptor).expect("MCP binding");
        assert_eq!(binding.model_name().as_str(), "mcp.fixture.echo");
        let catalog = catalog_with_skills(false, false, false, std::slice::from_ref(&binding), &[])
            .expect("MCP catalog");
        assert_eq!(catalog.tools()[0].name().as_str(), "mcp.fixture.echo");
        let call =
            ToolCall::new("mcp-1", "mcp.fixture.echo", json!({"text": "hello"})).expect("MCP call");
        assert_eq!(
            decode_call_with_skills(&call, &[binding], &[]).expect("decode MCP call"),
            ToolAction::Mcp {
                binding: McpToolBinding::from_descriptor("fixture", &descriptor)
                    .expect("MCP binding"),
                arguments: json!({"text": "hello"}),
            }
        );
    }

    #[test]
    fn skill_descriptors_project_and_decode_as_metadata_only_actions() {
        let descriptor =
            SkillToolDescriptor::new("check", "Inspect metadata", json!({"type": "object"}))
                .expect("Skill tool descriptor");
        let skill = SkillMetadata::new(
            "review",
            "Code Review",
            "Review source",
            "review/SKILL.md",
            32,
            vec![descriptor.clone()],
        )
        .expect("Skill metadata");
        let binding =
            SkillToolBinding::from_descriptor(&skill, &descriptor).expect("Skill binding");

        let catalog = catalog_with_skills(false, false, false, &[], std::slice::from_ref(&binding))
            .expect("Skill catalog");
        assert_eq!(catalog.tools()[0].name().as_str(), "skill.review.check");

        let call =
            ToolCall::new("skill-1", "skill.review.check", json!({})).expect("Skill tool call");
        assert_eq!(
            decode_call_with_skills(&call, &[], std::slice::from_ref(&binding))
                .expect("decode Skill call"),
            ToolAction::Skill {
                binding,
                arguments: json!({}),
            }
        );
    }
}
