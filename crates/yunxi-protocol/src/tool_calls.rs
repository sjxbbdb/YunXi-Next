//! Versioned model-tool orchestration contracts.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::GrantKind;

pub const TOOL_PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_MAX_TOOL_ROUNDS: u16 = 8;
pub const DEFAULT_MAX_TOOL_CALLS_PER_ROUND: u16 = 8;
pub const MAX_TOOL_ROUNDS: u16 = 32;
pub const MAX_TOOL_CALLS_PER_ROUND: u16 = 16;
pub const MAX_TOOL_CALL_ID_BYTES: usize = 128;
pub const MAX_TOOL_NAME_BYTES: usize = 128;
pub const MAX_TOOL_TEXT_BYTES: usize = 4096;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
pub const MAX_TOOL_RESULT_BYTES: usize = 1024 * 1024;
pub const MAX_APPROVAL_GRANTS: usize = 8;
pub const MAX_TOOL_DEFINITIONS: usize = 64;
pub const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(value: impl Into<String>) -> Result<Self, ToolProtocolError> {
        let value = value.into();
        validate_token(
            "tool call id",
            &value,
            MAX_TOOL_CALL_ID_BYTES,
            true,
            |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'),
        )?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ToolCallId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(value: impl Into<String>) -> Result<Self, ToolProtocolError> {
        let value = value.into();
        validate_token(
            "tool name",
            &value,
            MAX_TOOL_NAME_BYTES,
            true,
            |character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '-' | '_')
            },
        )?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ToolName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolDefinition {
    name: ToolName,
    description: String,
    input_schema: Value,
}

impl ToolDefinition {
    pub fn new(
        name: ToolName,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, ToolProtocolError> {
        let definition = Self {
            name,
            description: description.into(),
            input_schema,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn name(&self) -> &ToolName {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_text("tool description", &self.description)?;
        if !self.input_schema.is_object() {
            return Err(ToolProtocolError::ToolSchemaMustBeObject);
        }
        let size = value_size(&self.input_schema)?;
        if size > MAX_TOOL_SCHEMA_BYTES {
            return Err(ToolProtocolError::SchemaTooLarge {
                size,
                maximum: MAX_TOOL_SCHEMA_BYTES,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ToolDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireDefinition {
            name: ToolName,
            description: String,
            input_schema: Value,
        }

        let wire = WireDefinition::deserialize(deserializer)?;
        let definition = Self {
            name: wire.name,
            description: wire.description,
            input_schema: wire.input_schema,
        };
        definition.validate().map_err(D::Error::custom)?;
        Ok(definition)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCatalog {
    protocol_version: u32,
    tools: Vec<ToolDefinition>,
}

impl ToolCatalog {
    pub fn new(tools: Vec<ToolDefinition>) -> Result<Self, ToolProtocolError> {
        let catalog = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            tools,
        };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        if self.tools.len() > MAX_TOOL_DEFINITIONS {
            return Err(ToolProtocolError::TooManyToolDefinitions {
                count: self.tools.len(),
                maximum: MAX_TOOL_DEFINITIONS,
            });
        }
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            if !names.insert(tool.name().clone()) {
                return Err(ToolProtocolError::DuplicateToolDefinition {
                    name: tool.name().to_string(),
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ToolCatalog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCatalog {
            protocol_version: u32,
            tools: Vec<ToolDefinition>,
        }

        let wire = WireCatalog::deserialize(deserializer)?;
        let catalog = Self {
            protocol_version: wire.protocol_version,
            tools: wire.tools,
        };
        catalog.validate().map_err(D::Error::custom)?;
        Ok(catalog)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCall {
    id: ToolCallId,
    name: ToolName,
    arguments: Value,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: Value,
    ) -> Result<Self, ToolProtocolError> {
        let call = Self {
            id: ToolCallId::new(id)?,
            name: ToolName::new(name)?,
            arguments,
        };
        call.validate()?;
        Ok(call)
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn name(&self) -> &ToolName {
        &self.name
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        let size = value_size(&self.arguments)?;
        if size > MAX_TOOL_ARGUMENT_BYTES {
            return Err(ToolProtocolError::ArgumentsTooLarge {
                size,
                maximum: MAX_TOOL_ARGUMENT_BYTES,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireToolCall {
            id: ToolCallId,
            name: ToolName,
            arguments: Value,
        }

        let wire = WireToolCall::deserialize(deserializer)?;
        let call = Self {
            id: wire.id,
            name: wire.name,
            arguments: wire.arguments,
        };
        call.validate().map_err(D::Error::custom)?;
        Ok(call)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCallBatch {
    protocol_version: u32,
    round: u16,
    calls: Vec<ToolCall>,
}

impl ToolCallBatch {
    pub fn new(round: u16, calls: Vec<ToolCall>) -> Result<Self, ToolProtocolError> {
        let batch = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            round,
            calls,
        };
        batch.validate()?;
        Ok(batch)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn round(&self) -> u16 {
        self.round
    }

    pub fn calls(&self) -> &[ToolCall] {
        &self.calls
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        validate_round(self.round)?;
        validate_calls(&self.calls, MAX_TOOL_CALLS_PER_ROUND)
    }

    pub fn validate_with_policy(&self, policy: &ToolLoopPolicy) -> Result<(), ToolProtocolError> {
        self.validate()?;
        policy.validate()?;
        if self.round > policy.max_rounds {
            return Err(ToolProtocolError::RoundLimitExceeded {
                round: self.round,
                maximum: policy.max_rounds,
            });
        }
        if self.calls.len() > usize::from(policy.max_calls_per_round) {
            return Err(ToolProtocolError::TooManyCalls {
                count: self.calls.len(),
                maximum: usize::from(policy.max_calls_per_round),
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ToolCallBatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireBatch {
            protocol_version: u32,
            round: u16,
            calls: Vec<ToolCall>,
        }

        let wire = WireBatch::deserialize(deserializer)?;
        let batch = Self {
            protocol_version: wire.protocol_version,
            round: wire.round,
            calls: wire.calls,
        };
        batch.validate().map_err(D::Error::custom)?;
        Ok(batch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ToolResultOutcome {
    Completed {
        output: Value,
    },
    Failed {
        code: String,
        message: String,
        retryable: bool,
    },
    Rejected {
        code: String,
        message: String,
    },
    Cancelled {
        reason: String,
    },
}

impl ToolResultOutcome {
    pub fn completed(output: Value) -> Result<Self, ToolProtocolError> {
        let outcome = Self::Completed { output };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn failed(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Result<Self, ToolProtocolError> {
        let outcome = Self::Failed {
            code: code.into(),
            message: message.into(),
            retryable,
        };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn rejected(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, ToolProtocolError> {
        let outcome = Self::Rejected {
            code: code.into(),
            message: message.into(),
        };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn cancelled(reason: impl Into<String>) -> Result<Self, ToolProtocolError> {
        let outcome = Self::Cancelled {
            reason: reason.into(),
        };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        match self {
            Self::Completed { output } => {
                let size = value_size(output)?;
                if size > MAX_TOOL_RESULT_BYTES {
                    return Err(ToolProtocolError::ResultTooLarge {
                        size,
                        maximum: MAX_TOOL_RESULT_BYTES,
                    });
                }
            }
            Self::Failed { code, message, .. } | Self::Rejected { code, message, .. } => {
                validate_token(
                    "tool error code",
                    code,
                    MAX_TOOL_NAME_BYTES,
                    true,
                    |character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || matches!(character, '.' | '-' | '_')
                    },
                )?;
                validate_text("tool error message", message)?;
            }
            Self::Cancelled { reason } => validate_text("cancellation reason", reason)?,
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolResult {
    protocol_version: u32,
    round: u16,
    call_id: ToolCallId,
    tool_name: ToolName,
    outcome: ToolResultOutcome,
}

impl ToolResult {
    pub fn new(
        round: u16,
        call_id: ToolCallId,
        tool_name: ToolName,
        outcome: ToolResultOutcome,
    ) -> Result<Self, ToolProtocolError> {
        let result = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            round,
            call_id,
            tool_name,
            outcome,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn round(&self) -> u16 {
        self.round
    }

    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    pub fn outcome(&self) -> &ToolResultOutcome {
        &self.outcome
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        validate_round(self.round)?;
        self.outcome.validate()
    }
}

impl<'de> Deserialize<'de> for ToolResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            protocol_version: u32,
            round: u16,
            call_id: ToolCallId,
            tool_name: ToolName,
            outcome: ToolResultOutcome,
        }

        let wire = WireResult::deserialize(deserializer)?;
        let result = Self {
            protocol_version: wire.protocol_version,
            round: wire.round,
            call_id: wire.call_id,
            tool_name: wire.tool_name,
            outcome: wire.outcome,
        };
        result.validate().map_err(D::Error::custom)?;
        Ok(result)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolApprovalRequest {
    protocol_version: u32,
    round: u16,
    call_id: ToolCallId,
    tool_name: ToolName,
    summary: String,
    requested_grants: Vec<GrantKind>,
}

impl ToolApprovalRequest {
    pub fn new(
        round: u16,
        call_id: ToolCallId,
        tool_name: ToolName,
        summary: impl Into<String>,
        requested_grants: Vec<GrantKind>,
    ) -> Result<Self, ToolProtocolError> {
        let request = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            round,
            call_id,
            tool_name,
            summary: summary.into(),
            requested_grants,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn round(&self) -> u16 {
        self.round
    }

    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn requested_grants(&self) -> &[GrantKind] {
        &self.requested_grants
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        validate_round(self.round)?;
        validate_text("approval summary", &self.summary)?;
        validate_grants(&self.requested_grants)
    }
}

impl<'de> Deserialize<'de> for ToolApprovalRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireApprovalRequest {
            protocol_version: u32,
            round: u16,
            call_id: ToolCallId,
            tool_name: ToolName,
            summary: String,
            requested_grants: Vec<GrantKind>,
        }

        let wire = WireApprovalRequest::deserialize(deserializer)?;
        let request = Self {
            protocol_version: wire.protocol_version,
            round: wire.round,
            call_id: wire.call_id,
            tool_name: wire.tool_name,
            summary: wire.summary,
            requested_grants: wire.requested_grants,
        };
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum ToolApprovalState {
    Approved { ticket: String },
    Denied { reason: String },
}

impl ToolApprovalState {
    pub fn approved(ticket: impl Into<String>) -> Result<Self, ToolProtocolError> {
        let state = Self::Approved {
            ticket: ticket.into(),
        };
        state.validate()?;
        Ok(state)
    }

    pub fn denied(reason: impl Into<String>) -> Result<Self, ToolProtocolError> {
        let state = Self::Denied {
            reason: reason.into(),
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        match self {
            Self::Approved { ticket } => validate_token(
                "approval ticket",
                ticket,
                MAX_TOOL_TEXT_BYTES,
                true,
                |character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                },
            ),
            Self::Denied { reason } => validate_text("approval denial reason", reason),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolApprovalDecision {
    protocol_version: u32,
    round: u16,
    call_id: ToolCallId,
    tool_name: ToolName,
    state: ToolApprovalState,
}

impl ToolApprovalDecision {
    pub fn new(
        round: u16,
        call_id: ToolCallId,
        tool_name: ToolName,
        state: ToolApprovalState,
    ) -> Result<Self, ToolProtocolError> {
        let decision = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            round,
            call_id,
            tool_name,
            state,
        };
        decision.validate()?;
        Ok(decision)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn round(&self) -> u16 {
        self.round
    }

    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    pub fn state(&self) -> &ToolApprovalState {
        &self.state
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        validate_round(self.round)?;
        self.state.validate()
    }
}

impl<'de> Deserialize<'de> for ToolApprovalDecision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireApprovalDecision {
            protocol_version: u32,
            round: u16,
            call_id: ToolCallId,
            tool_name: ToolName,
            state: ToolApprovalState,
        }

        let wire = WireApprovalDecision::deserialize(deserializer)?;
        let decision = Self {
            protocol_version: wire.protocol_version,
            round: wire.round,
            call_id: wire.call_id,
            tool_name: wire.tool_name,
            state: wire.state,
        };
        decision.validate().map_err(D::Error::custom)?;
        Ok(decision)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCancellation {
    protocol_version: u32,
    round: u16,
    call_id: ToolCallId,
    reason: String,
}

impl ToolCancellation {
    pub fn new(
        round: u16,
        call_id: ToolCallId,
        reason: impl Into<String>,
    ) -> Result<Self, ToolProtocolError> {
        let cancellation = Self {
            protocol_version: TOOL_PROTOCOL_VERSION,
            round,
            call_id,
            reason: reason.into(),
        };
        cancellation.validate()?;
        Ok(cancellation)
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn round(&self) -> u16 {
        self.round
    }

    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        validate_version(self.protocol_version)?;
        validate_round(self.round)?;
        validate_text("cancellation reason", &self.reason)
    }
}

impl<'de> Deserialize<'de> for ToolCancellation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCancellation {
            protocol_version: u32,
            round: u16,
            call_id: ToolCallId,
            reason: String,
        }

        let wire = WireCancellation::deserialize(deserializer)?;
        let cancellation = Self {
            protocol_version: wire.protocol_version,
            round: wire.round,
            call_id: wire.call_id,
            reason: wire.reason,
        };
        cancellation.validate().map_err(D::Error::custom)?;
        Ok(cancellation)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ToolProtocolMessage {
    CallBatch(ToolCallBatch),
    ToolResult(ToolResult),
    ApprovalRequest(ToolApprovalRequest),
    ApprovalDecision(ToolApprovalDecision),
    Cancel(ToolCancellation),
}

impl ToolProtocolMessage {
    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        match self {
            Self::CallBatch(value) => value.validate(),
            Self::ToolResult(value) => value.validate(),
            Self::ApprovalRequest(value) => value.validate(),
            Self::ApprovalDecision(value) => value.validate(),
            Self::Cancel(value) => value.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolLoopPolicy {
    max_rounds: u16,
    max_calls_per_round: u16,
}

impl ToolLoopPolicy {
    pub const fn default_policy() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_TOOL_ROUNDS,
            max_calls_per_round: DEFAULT_MAX_TOOL_CALLS_PER_ROUND,
        }
    }

    pub const fn new(max_rounds: u16, max_calls_per_round: u16) -> Self {
        Self {
            max_rounds,
            max_calls_per_round,
        }
    }

    pub const fn max_rounds(&self) -> u16 {
        self.max_rounds
    }

    pub const fn max_calls_per_round(&self) -> u16 {
        self.max_calls_per_round
    }

    pub fn validate(&self) -> Result<(), ToolProtocolError> {
        if self.max_rounds == 0 || self.max_rounds > MAX_TOOL_ROUNDS {
            return Err(ToolProtocolError::InvalidPolicy {
                field: "max rounds",
                value: self.max_rounds,
                maximum: MAX_TOOL_ROUNDS,
            });
        }
        if self.max_calls_per_round == 0 || self.max_calls_per_round > MAX_TOOL_CALLS_PER_ROUND {
            return Err(ToolProtocolError::InvalidPolicy {
                field: "max calls per round",
                value: self.max_calls_per_round,
                maximum: MAX_TOOL_CALLS_PER_ROUND,
            });
        }
        Ok(())
    }
}

impl Default for ToolLoopPolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

fn validate_version(version: u32) -> Result<(), ToolProtocolError> {
    if version != TOOL_PROTOCOL_VERSION {
        return Err(ToolProtocolError::UnsupportedVersion {
            version,
            expected: TOOL_PROTOCOL_VERSION,
        });
    }
    Ok(())
}

fn validate_round(round: u16) -> Result<(), ToolProtocolError> {
    if round == 0 {
        return Err(ToolProtocolError::ZeroRound);
    }
    if round > MAX_TOOL_ROUNDS {
        return Err(ToolProtocolError::RoundLimitExceeded {
            round,
            maximum: MAX_TOOL_ROUNDS,
        });
    }
    Ok(())
}

fn validate_calls(calls: &[ToolCall], maximum: u16) -> Result<(), ToolProtocolError> {
    if calls.is_empty() {
        return Err(ToolProtocolError::EmptyCallBatch);
    }
    if calls.len() > usize::from(maximum) {
        return Err(ToolProtocolError::TooManyCalls {
            count: calls.len(),
            maximum: usize::from(maximum),
        });
    }
    let mut ids = BTreeSet::new();
    for call in calls {
        call.validate()?;
        if !ids.insert(call.id().clone()) {
            return Err(ToolProtocolError::DuplicateCallId {
                id: call.id().to_string(),
            });
        }
    }
    Ok(())
}

fn validate_grants(grants: &[GrantKind]) -> Result<(), ToolProtocolError> {
    if grants.is_empty() {
        return Err(ToolProtocolError::NoApprovalGrants);
    }
    if grants.len() > MAX_APPROVAL_GRANTS {
        return Err(ToolProtocolError::TooManyApprovalGrants {
            count: grants.len(),
            maximum: MAX_APPROVAL_GRANTS,
        });
    }
    let mut seen = BTreeSet::new();
    for grant in grants {
        if !seen.insert(*grant) {
            return Err(ToolProtocolError::DuplicateApprovalGrant { grant: *grant });
        }
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ToolProtocolError> {
    if value.trim().is_empty() {
        return Err(ToolProtocolError::EmptyField { field });
    }
    if value.len() > MAX_TOOL_TEXT_BYTES {
        return Err(ToolProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum: MAX_TOOL_TEXT_BYTES,
        });
    }
    Ok(())
}

fn validate_token<F>(
    field: &'static str,
    value: &str,
    maximum: usize,
    require_alphanumeric_start: bool,
    allowed: F,
) -> Result<(), ToolProtocolError>
where
    F: Fn(char) -> bool,
{
    if value.is_empty() {
        return Err(ToolProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(ToolProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    for (index, character) in value.char_indices() {
        if !allowed(character) {
            return Err(ToolProtocolError::InvalidTokenCharacter {
                field,
                index,
                character,
            });
        }
    }
    if require_alphanumeric_start
        && !value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        let character = value.chars().next().unwrap_or_default();
        return Err(ToolProtocolError::InvalidTokenCharacter {
            field,
            index: 0,
            character,
        });
    }
    Ok(())
}

fn value_size(value: &Value) -> Result<usize, ToolProtocolError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| ToolProtocolError::InvalidValue {
            message: error.to_string(),
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolProtocolError {
    UnsupportedVersion {
        version: u32,
        expected: u32,
    },
    ZeroRound,
    RoundLimitExceeded {
        round: u16,
        maximum: u16,
    },
    EmptyCallBatch,
    TooManyCalls {
        count: usize,
        maximum: usize,
    },
    DuplicateCallId {
        id: String,
    },
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    InvalidTokenCharacter {
        field: &'static str,
        index: usize,
        character: char,
    },
    ArgumentsTooLarge {
        size: usize,
        maximum: usize,
    },
    ResultTooLarge {
        size: usize,
        maximum: usize,
    },
    SchemaTooLarge {
        size: usize,
        maximum: usize,
    },
    ToolSchemaMustBeObject,
    TooManyToolDefinitions {
        count: usize,
        maximum: usize,
    },
    DuplicateToolDefinition {
        name: String,
    },
    NoApprovalGrants,
    TooManyApprovalGrants {
        count: usize,
        maximum: usize,
    },
    DuplicateApprovalGrant {
        grant: GrantKind,
    },
    InvalidPolicy {
        field: &'static str,
        value: u16,
        maximum: u16,
    },
    InvalidValue {
        message: String,
    },
}

impl fmt::Display for ToolProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { version, expected } => write!(
                formatter,
                "tool protocol version {version} is unsupported; expected {expected}"
            ),
            Self::ZeroRound => formatter.write_str("tool round must be greater than zero"),
            Self::RoundLimitExceeded { round, maximum } => {
                write!(formatter, "tool round {round} exceeds maximum {maximum}")
            }
            Self::EmptyCallBatch => formatter.write_str("tool call batch cannot be empty"),
            Self::TooManyCalls { count, maximum } => write!(
                formatter,
                "tool call batch contains {count} calls; maximum is {maximum}"
            ),
            Self::DuplicateCallId { id } => {
                write!(formatter, "tool call id `{id}` appears more than once")
            }
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::InvalidTokenCharacter {
                field,
                index,
                character,
            } => write!(
                formatter,
                "{field} contains unsupported character `{character}` at byte {index}"
            ),
            Self::ArgumentsTooLarge { size, maximum } => write!(
                formatter,
                "tool arguments are {size} bytes; maximum is {maximum}"
            ),
            Self::ResultTooLarge { size, maximum } => write!(
                formatter,
                "tool result is {size} bytes; maximum is {maximum}"
            ),
            Self::SchemaTooLarge { size, maximum } => write!(
                formatter,
                "tool input schema is {size} bytes; maximum is {maximum}"
            ),
            Self::ToolSchemaMustBeObject => {
                formatter.write_str("tool input schema must be a JSON object")
            }
            Self::TooManyToolDefinitions { count, maximum } => write!(
                formatter,
                "tool catalog contains {count} definitions; maximum is {maximum}"
            ),
            Self::DuplicateToolDefinition { name } => {
                write!(formatter, "tool catalog repeats definition `{name}`")
            }
            Self::NoApprovalGrants => {
                formatter.write_str("tool approval request must name at least one grant")
            }
            Self::TooManyApprovalGrants { count, maximum } => write!(
                formatter,
                "tool approval request names {count} grants; maximum is {maximum}"
            ),
            Self::DuplicateApprovalGrant { grant } => {
                write!(formatter, "tool approval request repeats grant `{grant}`")
            }
            Self::InvalidPolicy {
                field,
                value,
                maximum,
            } => write!(formatter, "{field} value {value} is outside 1..={maximum}"),
            Self::InvalidValue { message } => write!(formatter, "tool value is invalid: {message}"),
        }
    }
}

impl Error for ToolProtocolError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(id: &str) -> ToolCall {
        ToolCall::new(id, "file.read", json!({"path": "README.md"})).expect("tool call")
    }

    fn ids() -> (ToolCallId, ToolName) {
        (
            ToolCallId::new("call-1").expect("call id"),
            ToolName::new("shell.execute").expect("tool name"),
        )
    }

    #[test]
    fn tool_messages_round_trip_with_explicit_discriminants() {
        let (call_id, tool_name) = ids();
        let batch = ToolCallBatch::new(1, vec![call("call-1")]).expect("batch");
        let outcome = ToolResultOutcome::completed(json!({"stdout": "ok"})).expect("outcome");
        let result =
            ToolResult::new(1, call_id.clone(), tool_name.clone(), outcome).expect("result");
        let request = ToolApprovalRequest::new(
            1,
            call_id.clone(),
            tool_name.clone(),
            "Run the requested command",
            vec![GrantKind::Approval, GrantKind::WorkspaceRead],
        )
        .expect("approval request");
        let decision = ToolApprovalDecision::new(
            1,
            call_id.clone(),
            tool_name,
            ToolApprovalState::approved("ticket-1").expect("approval state"),
        )
        .expect("approval decision");
        let cancel = ToolCancellation::new(1, call_id, "user cancelled").expect("cancellation");

        for (message, expected_type) in [
            ToolProtocolMessage::CallBatch(batch),
            ToolProtocolMessage::ToolResult(result),
            ToolProtocolMessage::ApprovalRequest(request),
            ToolProtocolMessage::ApprovalDecision(decision),
            ToolProtocolMessage::Cancel(cancel),
        ]
        .into_iter()
        .zip([
            "call_batch",
            "tool_result",
            "approval_request",
            "approval_decision",
            "cancel",
        ]) {
            let json = serde_json::to_string(&message).expect("serialize tool message");
            assert!(json.contains(&format!("\"type\":\"{expected_type}\"")));
            let decoded =
                serde_json::from_str::<ToolProtocolMessage>(&json).expect("decode tool message");
            assert_eq!(decoded, message);
            decoded.validate().expect("validated decoded message");
        }
    }

    #[test]
    fn policy_limits_rounds_and_calls_before_execution() {
        let policy = ToolLoopPolicy::new(2, 1);
        let batch = ToolCallBatch::new(3, vec![call("call-1")]).expect("valid global batch");
        assert!(matches!(
            batch.validate_with_policy(&policy),
            Err(ToolProtocolError::RoundLimitExceeded { .. })
        ));

        let batch = ToolCallBatch::new(1, vec![call("call-1"), call("call-2")])
            .expect("valid global batch");
        assert!(matches!(
            batch.validate_with_policy(&policy),
            Err(ToolProtocolError::TooManyCalls { .. })
        ));
    }

    #[test]
    fn malformed_wire_values_are_rejected() {
        let bad_version = r#"{"protocol_version":2,"round":1,"calls":[{"id":"call-1","name":"file.read","arguments":{}}]}"#;
        let error = serde_json::from_str::<ToolCallBatch>(bad_version)
            .expect_err("unsupported version must fail");
        assert!(error.to_string().contains("unsupported"));

        let bad_name = r#"{"id":"call-1","name":"Shell.Read","arguments":{}}"#;
        let error = serde_json::from_str::<ToolCall>(bad_name).expect_err("bad tool name");
        assert!(error.to_string().contains("unsupported character"));

        let duplicate = ToolCallBatch::new(1, vec![call("call-1"), call("call-1")])
            .expect_err("duplicate call ids must fail");
        assert!(matches!(
            duplicate,
            ToolProtocolError::DuplicateCallId { .. }
        ));
    }

    #[test]
    fn approval_and_result_bounds_are_fail_closed() {
        let (call_id, tool_name) = ids();
        let approval = ToolApprovalRequest::new(
            1,
            call_id.clone(),
            tool_name.clone(),
            "needs approval",
            vec![GrantKind::Network, GrantKind::Network],
        )
        .expect_err("duplicate grants must fail");
        assert!(matches!(
            approval,
            ToolProtocolError::DuplicateApprovalGrant { .. }
        ));

        let output = Value::String("x".repeat(MAX_TOOL_RESULT_BYTES));
        let result = ToolResultOutcome::completed(output).expect_err("oversized result");
        assert!(matches!(result, ToolProtocolError::ResultTooLarge { .. }));

        let _ = ToolResult::new(
            1,
            call_id,
            tool_name,
            ToolResultOutcome::cancelled("stopped").expect("cancelled outcome"),
        )
        .expect("bounded cancellation result");
    }

    #[test]
    fn tool_catalog_validates_names_schemas_and_duplicates() {
        let definition = ToolDefinition::new(
            ToolName::new("file.read").expect("tool name"),
            "Read a bounded workspace file",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )
        .expect("tool definition");
        let catalog = ToolCatalog::new(vec![definition.clone()]).expect("catalog");
        let json = serde_json::to_string(&catalog).expect("serialize catalog");
        assert_eq!(
            serde_json::from_str::<ToolCatalog>(&json).expect("deserialize catalog"),
            catalog
        );

        let duplicate = ToolCatalog::new(vec![definition.clone(), definition])
            .expect_err("duplicate definitions must fail");
        assert!(matches!(
            duplicate,
            ToolProtocolError::DuplicateToolDefinition { .. }
        ));
        let schema = ToolDefinition::new(
            ToolName::new("file.read").expect("tool name"),
            "Read",
            json!("not an object"),
        )
        .expect_err("schema must be an object");
        assert!(matches!(schema, ToolProtocolError::ToolSchemaMustBeObject));
    }
}
