//! Bounded read-only Skill discovery, context, and tool metadata contracts.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const TOOL_SKILLS_LIST_OPERATION: &str = "list";
pub const TOOL_SKILLS_CONTEXT_OPERATION: &str = "context";
pub const TOOL_SKILLS_STATUS_OPERATION: &str = "status";
pub const TOOL_SKILLS_ACTION_OPERATION: &str = "action";
pub const SKILL_ACTION_PROTOCOL_VERSION: u32 = 1;

pub const MAX_SKILL_ID_BYTES: usize = 96;
pub const MAX_SKILL_NAME_BYTES: usize = 128;
pub const MAX_SKILL_DESCRIPTION_BYTES: usize = 2 * 1024;
pub const MAX_SKILL_PATH_BYTES: usize = 512;
pub const MAX_SKILL_INSTRUCTION_BYTES: usize = 32 * 1024;
pub const MAX_SKILL_CONTEXT_BYTES: usize = 128 * 1024;
pub const MAX_SKILL_METADATA: usize = 64;
pub const MAX_SKILL_TOOL_DECLARATIONS: usize = 64;
pub const MAX_SKILL_TOOL_NAME_BYTES: usize = 64;
pub const MAX_SKILL_TOOL_DESCRIPTION_BYTES: usize = 1024;
pub const MAX_SKILL_TOOL_SCHEMA_BYTES: usize = 32 * 1024;
pub const MAX_SKILL_ACTIONS: usize = 64;
pub const MAX_SKILL_ACTION_TOOL_NAME_BYTES: usize = 64;
pub const MAX_SKILL_ACTION_PROGRAM_BYTES: usize = 512;
pub const MAX_SKILL_ACTION_ARGUMENTS: usize = 32;
pub const MAX_SKILL_ACTION_ARGUMENT_BYTES: usize = 1024;
pub const MAX_SKILL_ACTION_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_SKILL_ACTION_TIMEOUT_MILLIS: u64 = 120_000;
pub const MAX_SKILL_ACTION_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_SKILL_ACTION_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_SKILL_STATUS_ERROR_BYTES: usize = 2 * 1024;
const MAX_SKILL_WARNINGS: usize = 32;
const MAX_SKILL_WARNING_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillToolDescriptor {
    name: String,
    description: String,
    input_schema: Value,
}

/// An explicitly declared executable Skill action.
///
/// Action declarations live in `actions.json`, separate from the
/// metadata-only `tools.json` contract. The program and arguments are fixed
/// by the Skill package; callers can supply only a bounded JSON input object.
/// This type grants no authority by itself. The host must still supply an
/// approved action grant before executing it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillActionSpec {
    tool_name: String,
    program: String,
    #[serde(default)]
    arguments: Vec<String>,
    timeout_millis: u64,
    max_output_bytes: usize,
    #[serde(default)]
    requires_workspace_write: bool,
}

impl<'de> Deserialize<'de> for SkillActionSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireActionSpec {
            tool_name: String,
            program: String,
            #[serde(default)]
            arguments: Vec<String>,
            timeout_millis: u64,
            max_output_bytes: usize,
            #[serde(default)]
            requires_workspace_write: bool,
        }

        let wire = WireActionSpec::deserialize(deserializer)?;
        Self::new(
            wire.tool_name,
            wire.program,
            wire.arguments,
            wire.timeout_millis,
            wire.max_output_bytes,
            wire.requires_workspace_write,
        )
        .map_err(D::Error::custom)
    }
}

impl SkillActionSpec {
    pub fn new(
        tool_name: impl Into<String>,
        program: impl Into<String>,
        arguments: Vec<String>,
        timeout_millis: u64,
        max_output_bytes: usize,
        requires_workspace_write: bool,
    ) -> Result<Self, SkillProtocolError> {
        let spec = Self {
            tool_name: tool_name.into(),
            program: program.into(),
            arguments,
            timeout_millis,
            max_output_bytes,
            requires_workspace_write,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub const fn timeout_millis(&self) -> u64 {
        self.timeout_millis
    }

    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub const fn requires_workspace_write(&self) -> bool {
        self.requires_workspace_write
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_identifier(
            "Skill action tool name",
            &self.tool_name,
            MAX_SKILL_ACTION_TOOL_NAME_BYTES,
        )?;
        validate_relative_path(&self.program)?;
        if self.program.len() > MAX_SKILL_ACTION_PROGRAM_BYTES {
            return Err(SkillProtocolError::ActionProgramTooLarge {
                size: self.program.len(),
                maximum: MAX_SKILL_ACTION_PROGRAM_BYTES,
            });
        }
        if self.arguments.len() > MAX_SKILL_ACTION_ARGUMENTS {
            return Err(SkillProtocolError::TooManyActionArguments {
                count: self.arguments.len(),
                maximum: MAX_SKILL_ACTION_ARGUMENTS,
            });
        }
        for argument in &self.arguments {
            validate_text(
                "Skill action argument",
                argument,
                MAX_SKILL_ACTION_ARGUMENT_BYTES,
                true,
            )?;
        }
        if self.timeout_millis == 0 || self.timeout_millis > MAX_SKILL_ACTION_TIMEOUT_MILLIS {
            return Err(SkillProtocolError::InvalidActionTimeout {
                value: self.timeout_millis,
                maximum: MAX_SKILL_ACTION_TIMEOUT_MILLIS,
            });
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > MAX_SKILL_ACTION_OUTPUT_BYTES {
            return Err(SkillProtocolError::InvalidActionOutputLimit {
                value: self.max_output_bytes,
                maximum: MAX_SKILL_ACTION_OUTPUT_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillActionRequest {
    protocol_version: u32,
    skill_id: String,
    tool_name: String,
    input: Value,
}

impl<'de> Deserialize<'de> for SkillActionRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireActionRequest {
            protocol_version: u32,
            skill_id: String,
            tool_name: String,
            input: Value,
        }

        let wire = WireActionRequest::deserialize(deserializer)?;
        let request = Self {
            protocol_version: wire.protocol_version,
            skill_id: wire.skill_id,
            tool_name: wire.tool_name,
            input: wire.input,
        };
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

impl SkillActionRequest {
    pub fn new(
        skill_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: Value,
    ) -> Result<Self, SkillProtocolError> {
        let request = Self {
            protocol_version: SKILL_ACTION_PROTOCOL_VERSION,
            skill_id: skill_id.into(),
            tool_name: tool_name.into(),
            input,
        };
        request.validate()?;
        Ok(request)
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn input(&self) -> &Value {
        &self.input
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        if self.protocol_version != SKILL_ACTION_PROTOCOL_VERSION {
            return Err(SkillProtocolError::UnsupportedActionProtocol {
                version: self.protocol_version,
                expected: SKILL_ACTION_PROTOCOL_VERSION,
            });
        }
        validate_identifier("Skill id", &self.skill_id, MAX_SKILL_ID_BYTES)?;
        validate_identifier(
            "Skill action tool name",
            &self.tool_name,
            MAX_SKILL_ACTION_TOOL_NAME_BYTES,
        )?;
        if !self.input.is_object() {
            return Err(SkillProtocolError::ActionInputMustBeObject);
        }
        let size = value_size(&self.input);
        if size > MAX_SKILL_ACTION_INPUT_BYTES {
            return Err(SkillProtocolError::ActionInputTooLarge {
                size,
                maximum: MAX_SKILL_ACTION_INPUT_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActionOutcome {
    Success,
    Failed,
    TimedOut,
    Cancelled,
    ProtocolError,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillActionResponse {
    protocol_version: u32,
    outcome: SkillActionOutcome,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
}

impl<'de> Deserialize<'de> for SkillActionResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireActionResponse {
            protocol_version: u32,
            outcome: SkillActionOutcome,
            exit_code: Option<i32>,
            stdout: String,
            stderr: String,
            output_truncated: bool,
        }

        let wire = WireActionResponse::deserialize(deserializer)?;
        let response = Self {
            protocol_version: wire.protocol_version,
            outcome: wire.outcome,
            exit_code: wire.exit_code,
            stdout: wire.stdout,
            stderr: wire.stderr,
            output_truncated: wire.output_truncated,
        };
        response.validate().map_err(D::Error::custom)?;
        Ok(response)
    }
}

impl SkillActionResponse {
    pub fn new(
        outcome: SkillActionOutcome,
        exit_code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        output_truncated: bool,
    ) -> Result<Self, SkillProtocolError> {
        let response = Self {
            protocol_version: SKILL_ACTION_PROTOCOL_VERSION,
            outcome,
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            output_truncated,
        };
        response.validate()?;
        Ok(response)
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub const fn outcome(&self) -> SkillActionOutcome {
        self.outcome
    }

    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub const fn output_truncated(&self) -> bool {
        self.output_truncated
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        if self.protocol_version != SKILL_ACTION_PROTOCOL_VERSION {
            return Err(SkillProtocolError::UnsupportedActionProtocol {
                version: self.protocol_version,
                expected: SKILL_ACTION_PROTOCOL_VERSION,
            });
        }
        validate_text(
            "Skill action stdout",
            &self.stdout,
            MAX_SKILL_ACTION_OUTPUT_BYTES,
            true,
        )?;
        validate_text(
            "Skill action stderr",
            &self.stderr,
            MAX_SKILL_ACTION_OUTPUT_BYTES,
            true,
        )?;
        Ok(())
    }
}

impl SkillToolDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, SkillProtocolError> {
        let descriptor = Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_identifier("Skill tool name", &self.name, MAX_SKILL_TOOL_NAME_BYTES)?;
        validate_text(
            "Skill tool description",
            &self.description,
            MAX_SKILL_TOOL_DESCRIPTION_BYTES,
            true,
        )?;
        if !self.input_schema.is_object() {
            return Err(SkillProtocolError::SchemaMustBeObject);
        }
        let size = value_size(&self.input_schema);
        if size > MAX_SKILL_TOOL_SCHEMA_BYTES {
            return Err(SkillProtocolError::SchemaTooLarge {
                size,
                maximum: MAX_SKILL_TOOL_SCHEMA_BYTES,
            });
        }
        reject_executable_metadata(&self.input_schema)?;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SkillToolDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            name: String,
            #[serde(default)]
            description: String,
            input_schema: Value,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        Self::new(wire.name, wire.description, wire.input_schema).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillMetadata {
    id: String,
    name: String,
    description: String,
    relative_path: String,
    instruction_bytes: usize,
    tools: Vec<SkillToolDescriptor>,
}

impl SkillMetadata {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        relative_path: impl Into<String>,
        instruction_bytes: usize,
        tools: Vec<SkillToolDescriptor>,
    ) -> Result<Self, SkillProtocolError> {
        let metadata = Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            relative_path: relative_path.into(),
            instruction_bytes,
            tools,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub const fn instruction_bytes(&self) -> usize {
        self.instruction_bytes
    }

    pub fn tools(&self) -> &[SkillToolDescriptor] {
        &self.tools
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_identifier("Skill id", &self.id, MAX_SKILL_ID_BYTES)?;
        validate_text("Skill name", &self.name, MAX_SKILL_NAME_BYTES, false)?;
        validate_text(
            "Skill description",
            &self.description,
            MAX_SKILL_DESCRIPTION_BYTES,
            true,
        )?;
        validate_relative_path(&self.relative_path)?;
        if self.instruction_bytes > MAX_SKILL_INSTRUCTION_BYTES {
            return Err(SkillProtocolError::InstructionTooLarge {
                size: self.instruction_bytes,
                maximum: MAX_SKILL_INSTRUCTION_BYTES,
            });
        }
        if self.tools.len() > MAX_SKILL_TOOL_DECLARATIONS {
            return Err(SkillProtocolError::TooManyTools {
                count: self.tools.len(),
                maximum: MAX_SKILL_TOOL_DECLARATIONS,
            });
        }
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            if !names.insert(tool.name()) {
                return Err(SkillProtocolError::DuplicateTool {
                    name: tool.name().to_string(),
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SkillMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMetadata {
            id: String,
            name: String,
            #[serde(default)]
            description: String,
            relative_path: String,
            instruction_bytes: usize,
            #[serde(default)]
            tools: Vec<SkillToolDescriptor>,
        }

        let wire = WireMetadata::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.name,
            wire.description,
            wire.relative_path,
            wire.instruction_bytes,
            wire.tools,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillListRequest {
    #[serde(default)]
    refresh: bool,
}

impl SkillListRequest {
    pub const fn new() -> Self {
        Self { refresh: false }
    }

    pub const fn with_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    pub const fn refresh(&self) -> bool {
        self.refresh
    }
}

impl Default for SkillListRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillListResult {
    root: String,
    skills: Vec<SkillMetadata>,
    warnings: Vec<String>,
    truncated: bool,
}

impl SkillListResult {
    pub fn new(
        root: impl Into<String>,
        skills: Vec<SkillMetadata>,
        warnings: Vec<String>,
        truncated: bool,
    ) -> Result<Self, SkillProtocolError> {
        let result = Self {
            root: root.into(),
            skills,
            warnings,
            truncated,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_text("Skill root", &self.root, MAX_SKILL_PATH_BYTES, false)?;
        validate_metadata(&self.skills)?;
        validate_warnings(&self.warnings)
    }
}

impl<'de> Deserialize<'de> for SkillListResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            root: String,
            skills: Vec<SkillMetadata>,
            #[serde(default)]
            warnings: Vec<String>,
            #[serde(default)]
            truncated: bool,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(wire.root, wire.skills, wire.warnings, wire.truncated).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillContextRequest {
    skill_ids: Vec<String>,
}

impl SkillContextRequest {
    pub fn new(skill_ids: Vec<String>) -> Result<Self, SkillProtocolError> {
        let request = Self { skill_ids };
        request.validate()?;
        Ok(request)
    }

    pub fn skill_ids(&self) -> &[String] {
        &self.skill_ids
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        if self.skill_ids.len() > MAX_SKILL_METADATA {
            return Err(SkillProtocolError::TooManySkills {
                count: self.skill_ids.len(),
                maximum: MAX_SKILL_METADATA,
            });
        }
        let mut ids = BTreeSet::new();
        for id in &self.skill_ids {
            validate_identifier("Skill id", id, MAX_SKILL_ID_BYTES)?;
            if !ids.insert(id) {
                return Err(SkillProtocolError::DuplicateSkill { id: id.clone() });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SkillContextRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRequest {
            skill_ids: Vec<String>,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(wire.skill_ids).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillContextBlock {
    skill_id: String,
    instructions: String,
}

impl SkillContextBlock {
    pub fn new(
        skill_id: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, SkillProtocolError> {
        let block = Self {
            skill_id: skill_id.into(),
            instructions: instructions.into(),
        };
        block.validate()?;
        Ok(block)
    }

    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_identifier("Skill id", &self.skill_id, MAX_SKILL_ID_BYTES)?;
        validate_multiline_text(
            "Skill instructions",
            &self.instructions,
            MAX_SKILL_INSTRUCTION_BYTES,
            true,
        )
    }
}

impl<'de> Deserialize<'de> for SkillContextBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireBlock {
            skill_id: String,
            instructions: String,
        }

        let wire = WireBlock::deserialize(deserializer)?;
        Self::new(wire.skill_id, wire.instructions).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillContextResult {
    blocks: Vec<SkillContextBlock>,
    warnings: Vec<String>,
    truncated: bool,
}

impl SkillContextResult {
    pub fn new(
        blocks: Vec<SkillContextBlock>,
        warnings: Vec<String>,
        truncated: bool,
    ) -> Result<Self, SkillProtocolError> {
        let result = Self {
            blocks,
            warnings,
            truncated,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn blocks(&self) -> &[SkillContextBlock] {
        &self.blocks
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        let mut ids = BTreeSet::new();
        let mut total: usize = 0;
        for block in &self.blocks {
            block.validate()?;
            if !ids.insert(block.skill_id()) {
                return Err(SkillProtocolError::DuplicateSkill {
                    id: block.skill_id().to_string(),
                });
            }
            total = total.saturating_add(block.instructions.len());
        }
        if total > MAX_SKILL_CONTEXT_BYTES {
            return Err(SkillProtocolError::ContextTooLarge {
                size: total,
                maximum: MAX_SKILL_CONTEXT_BYTES,
            });
        }
        validate_warnings(&self.warnings)
    }
}

impl<'de> Deserialize<'de> for SkillContextResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            blocks: Vec<SkillContextBlock>,
            #[serde(default)]
            warnings: Vec<String>,
            #[serde(default)]
            truncated: bool,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(wire.blocks, wire.warnings, wire.truncated).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct SkillStatusRequest {}

impl SkillStatusRequest {
    pub const fn new() -> Self {
        Self {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRuntimeState {
    Ready,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillStatusResult {
    root: String,
    state: SkillRuntimeState,
    skill_count: usize,
    tool_count: usize,
    last_error: Option<String>,
}

impl SkillStatusResult {
    pub fn new(
        root: impl Into<String>,
        state: SkillRuntimeState,
        skill_count: usize,
        tool_count: usize,
        last_error: Option<String>,
    ) -> Result<Self, SkillProtocolError> {
        let result = Self {
            root: root.into(),
            state,
            skill_count,
            tool_count,
            last_error,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub const fn state(&self) -> SkillRuntimeState {
        self.state
    }

    pub const fn skill_count(&self) -> usize {
        self.skill_count
    }

    pub const fn tool_count(&self) -> usize {
        self.tool_count
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn validate(&self) -> Result<(), SkillProtocolError> {
        validate_text("Skill root", &self.root, MAX_SKILL_PATH_BYTES, false)?;
        if self.skill_count > MAX_SKILL_METADATA {
            return Err(SkillProtocolError::TooManySkills {
                count: self.skill_count,
                maximum: MAX_SKILL_METADATA,
            });
        }
        if self.tool_count > MAX_SKILL_TOOL_DECLARATIONS {
            return Err(SkillProtocolError::TooManyTools {
                count: self.tool_count,
                maximum: MAX_SKILL_TOOL_DECLARATIONS,
            });
        }
        if let Some(error) = &self.last_error {
            validate_text(
                "Skill status error",
                error,
                MAX_SKILL_STATUS_ERROR_BYTES,
                true,
            )?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SkillStatusResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            root: String,
            state: SkillRuntimeState,
            skill_count: usize,
            tool_count: usize,
            #[serde(default)]
            last_error: Option<String>,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(
            wire.root,
            wire.state,
            wire.skill_count,
            wire.tool_count,
            wire.last_error,
        )
        .map_err(D::Error::custom)
    }
}

fn validate_metadata(skills: &[SkillMetadata]) -> Result<(), SkillProtocolError> {
    if skills.len() > MAX_SKILL_METADATA {
        return Err(SkillProtocolError::TooManySkills {
            count: skills.len(),
            maximum: MAX_SKILL_METADATA,
        });
    }
    let mut ids = BTreeSet::new();
    let mut tools = BTreeSet::new();
    for skill in skills {
        skill.validate()?;
        if !ids.insert(skill.id()) {
            return Err(SkillProtocolError::DuplicateSkill {
                id: skill.id().to_string(),
            });
        }
        for tool in skill.tools() {
            let key = format!("{}.{}", skill.id(), tool.name());
            if !tools.insert(key.clone()) {
                return Err(SkillProtocolError::DuplicateTool { name: key });
            }
        }
    }
    if tools.len() > MAX_SKILL_TOOL_DECLARATIONS {
        return Err(SkillProtocolError::TooManyTools {
            count: tools.len(),
            maximum: MAX_SKILL_TOOL_DECLARATIONS,
        });
    }
    Ok(())
}

fn validate_warnings(warnings: &[String]) -> Result<(), SkillProtocolError> {
    if warnings.len() > MAX_SKILL_WARNINGS {
        return Err(SkillProtocolError::TooManyWarnings {
            count: warnings.len(),
            maximum: MAX_SKILL_WARNINGS,
        });
    }
    for warning in warnings {
        validate_text("Skill warning", warning, MAX_SKILL_WARNING_BYTES, false)?;
    }
    Ok(())
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), SkillProtocolError> {
    validate_text(field, value, maximum, false)?;
    if !value.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
        return Err(SkillProtocolError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    if value.ends_with('-') || value.ends_with('_') {
        return Err(SkillProtocolError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    if value.chars().any(|character| {
        !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && character != '-'
            && character != '_'
    }) {
        return Err(SkillProtocolError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), SkillProtocolError> {
    validate_text("Skill relative path", value, MAX_SKILL_PATH_BYTES, false)?;
    if value.starts_with('/') || value.contains('\\') || value.contains(':') {
        return Err(SkillProtocolError::InvalidRelativePath {
            value: value.to_string(),
        });
    }
    if value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(SkillProtocolError::InvalidRelativePath {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), SkillProtocolError> {
    if !allow_empty && value.trim().is_empty() {
        return Err(SkillProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(SkillProtocolError::TextTooLarge {
            field,
            size: value.len(),
            maximum,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(SkillProtocolError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_multiline_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), SkillProtocolError> {
    if !allow_empty && value.trim().is_empty() {
        return Err(SkillProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(SkillProtocolError::TextTooLarge {
            field,
            size: value.len(),
            maximum,
        });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(SkillProtocolError::ControlCharacter { field });
    }
    Ok(())
}

fn reject_executable_metadata(value: &Value) -> Result<(), SkillProtocolError> {
    let Value::Object(object) = value else {
        return Ok(());
    };
    for key in object.keys() {
        if matches!(
            key.as_str(),
            "command"
                | "commands"
                | "executable"
                | "program"
                | "shell"
                | "env"
                | "environment"
                | "network"
                | "provider_credential"
        ) || key.starts_with("x-yunxi")
            || key.starts_with("x_yunxi")
        {
            return Err(SkillProtocolError::ExecutableMetadata { field: key.clone() });
        }
    }
    Ok(())
}

fn value_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkillProtocolError {
    EmptyField {
        field: &'static str,
    },
    TextTooLarge {
        field: &'static str,
        size: usize,
        maximum: usize,
    },
    ControlCharacter {
        field: &'static str,
    },
    InvalidIdentifier {
        field: &'static str,
        value: String,
    },
    InvalidRelativePath {
        value: String,
    },
    InstructionTooLarge {
        size: usize,
        maximum: usize,
    },
    ContextTooLarge {
        size: usize,
        maximum: usize,
    },
    SchemaMustBeObject,
    SchemaTooLarge {
        size: usize,
        maximum: usize,
    },
    ExecutableMetadata {
        field: String,
    },
    ActionProgramTooLarge {
        size: usize,
        maximum: usize,
    },
    TooManyActionArguments {
        count: usize,
        maximum: usize,
    },
    InvalidActionTimeout {
        value: u64,
        maximum: u64,
    },
    InvalidActionOutputLimit {
        value: usize,
        maximum: usize,
    },
    UnsupportedActionProtocol {
        version: u32,
        expected: u32,
    },
    ActionInputMustBeObject,
    ActionInputTooLarge {
        size: usize,
        maximum: usize,
    },
    TooManySkills {
        count: usize,
        maximum: usize,
    },
    TooManyTools {
        count: usize,
        maximum: usize,
    },
    TooManyWarnings {
        count: usize,
        maximum: usize,
    },
    DuplicateSkill {
        id: String,
    },
    DuplicateTool {
        name: String,
    },
}

impl fmt::Display for SkillProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::TextTooLarge {
                field,
                size,
                maximum,
            } => write!(formatter, "{field} is {size} bytes; maximum is {maximum}"),
            Self::ControlCharacter { field } => {
                write!(formatter, "{field} contains a control character")
            }
            Self::InvalidIdentifier { field, value } => {
                write!(
                    formatter,
                    "{field} `{value}` is not a stable lowercase identifier"
                )
            }
            Self::InvalidRelativePath { value } => {
                write!(
                    formatter,
                    "Skill relative path `{value}` is outside the allowed form"
                )
            }
            Self::InstructionTooLarge { size, maximum } => write!(
                formatter,
                "Skill instruction body is {size} bytes; maximum is {maximum}"
            ),
            Self::ContextTooLarge { size, maximum } => write!(
                formatter,
                "Skill context is {size} bytes; maximum is {maximum}"
            ),
            Self::SchemaMustBeObject => {
                formatter.write_str("Skill input schema must be a JSON object")
            }
            Self::SchemaTooLarge { size, maximum } => write!(
                formatter,
                "Skill input schema is {size} bytes; maximum is {maximum}"
            ),
            Self::ExecutableMetadata { field } => write!(
                formatter,
                "Skill tool metadata cannot contain executable field `{field}`"
            ),
            Self::ActionProgramTooLarge { size, maximum } => write!(
                formatter,
                "Skill action program is {size} bytes; maximum is {maximum}"
            ),
            Self::TooManyActionArguments { count, maximum } => write!(
                formatter,
                "Skill action declares {count} arguments; maximum is {maximum}"
            ),
            Self::InvalidActionTimeout { value, maximum } => write!(
                formatter,
                "Skill action timeout {value} ms is outside 1..={maximum}"
            ),
            Self::InvalidActionOutputLimit { value, maximum } => write!(
                formatter,
                "Skill action output limit {value} bytes is outside 1..={maximum}"
            ),
            Self::UnsupportedActionProtocol { version, expected } => write!(
                formatter,
                "Skill action protocol {version} is unsupported; expected {expected}"
            ),
            Self::ActionInputMustBeObject => {
                formatter.write_str("Skill action input must be a JSON object")
            }
            Self::ActionInputTooLarge { size, maximum } => write!(
                formatter,
                "Skill action input is {size} bytes; maximum is {maximum}"
            ),
            Self::TooManySkills { count, maximum } => {
                write!(
                    formatter,
                    "Skill list has {count} entries; maximum is {maximum}"
                )
            }
            Self::TooManyTools { count, maximum } => write!(
                formatter,
                "Skill tool declarations have {count} entries; maximum is {maximum}"
            ),
            Self::TooManyWarnings { count, maximum } => write!(
                formatter,
                "Skill response has {count} warnings; maximum is {maximum}"
            ),
            Self::DuplicateSkill { id } => write!(formatter, "Skill id `{id}` is duplicated"),
            Self::DuplicateTool { name } => write!(formatter, "Skill tool `{name}` is duplicated"),
        }
    }
}

impl Error for SkillProtocolError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> SkillToolDescriptor {
        SkillToolDescriptor::new(name, "metadata only", json!({"type": "object"}))
            .expect("skill tool")
    }

    #[test]
    fn skill_metadata_round_trips_with_relative_path_and_tools() {
        let metadata = SkillMetadata::new(
            "review",
            "Review",
            "Review code",
            "review/SKILL.md",
            42,
            vec![tool("check")],
        )
        .expect("metadata");
        let json = serde_json::to_string(&metadata).expect("serialize metadata");
        assert_eq!(
            serde_json::from_str::<SkillMetadata>(&json).expect("deserialize metadata"),
            metadata
        );
    }

    #[test]
    fn paths_and_executable_tool_metadata_are_rejected() {
        assert!(matches!(
            SkillMetadata::new("review", "Review", "desc", "../SKILL.md", 1, Vec::new()),
            Err(SkillProtocolError::InvalidRelativePath { .. })
        ));
        assert!(matches!(
            SkillToolDescriptor::new(
                "run",
                "metadata",
                json!({"type":"object", "x-yunxi": {"command": "echo"}})
            ),
            Err(SkillProtocolError::ExecutableMetadata { .. })
        ));
    }

    #[test]
    fn context_result_is_bounded_and_deduplicated() {
        let block = SkillContextBlock::new("review", "instructions").expect("block");
        let result = SkillContextResult::new(vec![block.clone(), block], Vec::new(), false);
        assert!(matches!(
            result,
            Err(SkillProtocolError::DuplicateSkill { .. })
        ));
    }

    #[test]
    fn multiline_skill_instructions_allow_layout_whitespace_but_reject_nul() {
        assert!(SkillContextBlock::new("review", "first line\n\tsecond line\r\n").is_ok());
        assert!(matches!(
            SkillContextBlock::new("review", "invalid\0instruction"),
            Err(SkillProtocolError::ControlCharacter {
                field: "Skill instructions"
            })
        ));
    }

    #[test]
    fn executable_action_contract_is_separate_and_bounded() {
        let spec = SkillActionSpec::new(
            "check",
            "bin/check.exe",
            vec!["--strict".to_string()],
            5_000,
            4_096,
            false,
        )
        .expect("action spec");
        let wire = serde_json::to_string(&spec).expect("serialize action spec");
        assert_eq!(
            serde_json::from_str::<SkillActionSpec>(&wire).expect("parse action spec"),
            spec
        );
        assert!(serde_json::from_str::<SkillActionSpec>(
            r#"{"tool_name":"check","program":"../run","arguments":[],"timeout_millis":5000,"max_output_bytes":4096}"#
        )
        .is_err());
    }

    #[test]
    fn action_request_and_response_reject_wrong_protocol_and_unbounded_input() {
        let request =
            SkillActionRequest::new("check", "run", json!({"value":"ok"})).expect("request");
        let mut wire = serde_json::to_value(request).expect("request JSON");
        wire["protocol_version"] = json!(99);
        assert!(serde_json::from_value::<SkillActionRequest>(wire).is_err());
        assert!(SkillActionRequest::new("check", "run", json!("not an object")).is_err());

        let response =
            SkillActionResponse::new(SkillActionOutcome::Success, Some(0), "ok", "", false)
                .expect("response");
        assert_eq!(response.protocol_version(), SKILL_ACTION_PROTOCOL_VERSION);
    }
}
