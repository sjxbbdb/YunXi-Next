//! Bounded MCP server discovery, invocation, and lifecycle contracts.

use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{ActionGrant, NetworkGrant, SecretGrant};

pub const TOOL_MCP_LIST_OPERATION: &str = "list";
pub const TOOL_MCP_CALL_OPERATION: &str = "call";
pub const TOOL_MCP_CANCEL_OPERATION: &str = "cancel";
pub const TOOL_MCP_STATUS_OPERATION: &str = "status";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

const MAX_SERVER_NAME_BYTES: usize = 64;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 4096;
const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;
const MAX_TOOLS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_STATUS_ERROR_BYTES: usize = 4096;
const MAX_CANCEL_REASON_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolDescriptor {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

impl McpToolDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, McpProtocolError> {
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

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_text("MCP tool name", &self.name, MAX_TOOL_NAME_BYTES, false)?;
        validate_text(
            "MCP tool description",
            &self.description,
            MAX_DESCRIPTION_BYTES,
            false,
        )?;
        if !self.input_schema.is_object() {
            return Err(McpProtocolError::SchemaMustBeObject);
        }
        validate_value_size(&self.input_schema, MAX_TOOL_SCHEMA_BYTES, "MCP tool schema")
    }
}

impl<'de> Deserialize<'de> for McpToolDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireDescriptor {
            name: String,
            #[serde(default)]
            description: String,
            #[serde(rename = "inputSchema")]
            input_schema: Value,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        Self::new(wire.name, wire.description, wire.input_schema).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolListRequest {
    #[serde(default)]
    refresh: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network_grant: Option<NetworkGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_grant: Option<SecretGrant>,
}

impl<'de> Deserialize<'de> for McpToolListRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRequest {
            #[serde(default)]
            refresh: bool,
            #[serde(default)]
            network_grant: Option<NetworkGrant>,
            #[serde(default)]
            secret_grant: Option<SecretGrant>,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            refresh: wire.refresh,
            network_grant: wire.network_grant,
            secret_grant: wire.secret_grant,
        };
        request.validate().map_err(D::Error::custom)?;
        Ok(request)
    }
}

impl McpToolListRequest {
    pub const fn new() -> Self {
        Self {
            refresh: false,
            network_grant: None,
            secret_grant: None,
        }
    }

    pub const fn with_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    pub const fn refresh(&self) -> bool {
        self.refresh
    }

    pub fn with_network_grant(mut self, grant: NetworkGrant) -> Self {
        self.network_grant = Some(grant);
        self
    }

    pub fn with_secret_grant(mut self, grant: SecretGrant) -> Self {
        self.secret_grant = Some(grant);
        self
    }

    pub fn network_grant(&self) -> Option<&NetworkGrant> {
        self.network_grant.as_ref()
    }

    pub fn secret_grant(&self) -> Option<&SecretGrant> {
        self.secret_grant.as_ref()
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        if let Some(grant) = &self.network_grant {
            grant
                .validate()
                .map_err(|error| McpProtocolError::InvalidNetworkGrant(error.to_string()))?;
        }
        if let Some(grant) = &self.secret_grant {
            grant
                .validate()
                .map_err(|error| McpProtocolError::InvalidSecretGrant(error.to_string()))?;
        }
        Ok(())
    }
}

impl Default for McpToolListRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolCancelRequest {
    server_name: String,
    request_id: u64,
    reason: String,
}

impl McpToolCancelRequest {
    pub fn new(
        server_name: impl Into<String>,
        request_id: u64,
        reason: impl Into<String>,
    ) -> Result<Self, McpProtocolError> {
        let request = Self {
            server_name: server_name.into(),
            request_id,
            reason: reason.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_server_name(&self.server_name)?;
        if self.request_id == 0 {
            return Err(McpProtocolError::ZeroRequestId);
        }
        validate_text(
            "MCP cancellation reason",
            &self.reason,
            MAX_CANCEL_REASON_BYTES,
            false,
        )
    }
}

impl<'de> Deserialize<'de> for McpToolCancelRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRequest {
            server_name: String,
            request_id: u64,
            #[serde(default)]
            reason: String,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(wire.server_name, wire.request_id, wire.reason).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolCancelResult {
    server_name: String,
    request_id: u64,
    cancelled: bool,
    supported: bool,
}

impl<'de> Deserialize<'de> for McpToolCancelResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            server_name: String,
            request_id: u64,
            #[serde(default)]
            cancelled: bool,
            #[serde(default)]
            supported: bool,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(
            wire.server_name,
            wire.request_id,
            wire.cancelled,
            wire.supported,
        )
        .map_err(D::Error::custom)
    }
}

impl McpToolCancelResult {
    pub fn new(
        server_name: impl Into<String>,
        request_id: u64,
        cancelled: bool,
        supported: bool,
    ) -> Result<Self, McpProtocolError> {
        let result = Self {
            server_name: server_name.into(),
            request_id,
            cancelled,
            supported,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    pub const fn cancelled(&self) -> bool {
        self.cancelled
    }

    pub const fn supported(&self) -> bool {
        self.supported
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_server_name(&self.server_name)?;
        if self.request_id == 0 {
            return Err(McpProtocolError::ZeroRequestId);
        }
        if self.cancelled && !self.supported {
            return Err(McpProtocolError::CancelledWithoutSupport);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolListResult {
    server_name: String,
    tools: Vec<McpToolDescriptor>,
    truncated: bool,
}

impl McpToolListResult {
    pub fn new(
        server_name: impl Into<String>,
        tools: Vec<McpToolDescriptor>,
        truncated: bool,
    ) -> Result<Self, McpProtocolError> {
        let result = Self {
            server_name: server_name.into(),
            tools,
            truncated,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tools(&self) -> &[McpToolDescriptor] {
        &self.tools
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_server_name(&self.server_name)?;
        if self.tools.len() > MAX_TOOLS {
            return Err(McpProtocolError::TooManyTools {
                count: self.tools.len(),
                maximum: MAX_TOOLS,
            });
        }
        let mut names = std::collections::BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            if !names.insert(tool.name()) {
                return Err(McpProtocolError::DuplicateTool {
                    name: tool.name().to_string(),
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpToolListResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            server_name: String,
            tools: Vec<McpToolDescriptor>,
            #[serde(default)]
            truncated: bool,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(wire.server_name, wire.tools, wire.truncated).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolCallRequest {
    grant: ActionGrant,
    server_name: String,
    tool_name: String,
    arguments: Value,
}

impl McpToolCallRequest {
    pub fn new(
        grant: ActionGrant,
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        arguments: Value,
    ) -> Result<Self, McpProtocolError> {
        let request = Self {
            grant,
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            arguments,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn grant(&self) -> &ActionGrant {
        &self.grant
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        self.grant
            .validate()
            .map_err(|error| McpProtocolError::InvalidGrant(error.to_string()))?;
        validate_server_name(&self.server_name)?;
        validate_text("MCP tool name", &self.tool_name, MAX_TOOL_NAME_BYTES, false)?;
        validate_value_size(&self.arguments, MAX_ARGUMENT_BYTES, "MCP tool arguments")
    }
}

impl<'de> Deserialize<'de> for McpToolCallRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRequest {
            grant: ActionGrant,
            server_name: String,
            tool_name: String,
            arguments: Value,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(wire.grant, wire.server_name, wire.tool_name, wire.arguments)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpToolCallResult {
    server_name: String,
    tool_name: String,
    output: Value,
    is_error: bool,
}

impl McpToolCallResult {
    pub fn new(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        output: Value,
        is_error: bool,
    ) -> Result<Self, McpProtocolError> {
        let result = Self {
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            output,
            is_error,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn output(&self) -> &Value {
        &self.output
    }

    pub const fn is_error(&self) -> bool {
        self.is_error
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_server_name(&self.server_name)?;
        validate_text("MCP tool name", &self.tool_name, MAX_TOOL_NAME_BYTES, false)?;
        validate_value_size(&self.output, MAX_RESULT_BYTES, "MCP tool result")
    }
}

impl<'de> Deserialize<'de> for McpToolCallResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResult {
            server_name: String,
            tool_name: String,
            output: Value,
            #[serde(default)]
            is_error: bool,
        }

        let wire = WireResult::deserialize(deserializer)?;
        Self::new(wire.server_name, wire.tool_name, wire.output, wire.is_error)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    Starting,
    Ready,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpStatusRequest {
    server_name: String,
}

impl McpStatusRequest {
    pub fn new(server_name: impl Into<String>) -> Result<Self, McpProtocolError> {
        let request = Self {
            server_name: server_name.into(),
        };
        validate_server_name(&request.server_name)?;
        Ok(request)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpStatusResult {
    server_name: String,
    state: McpServerState,
    tool_count: usize,
    last_error: Option<String>,
}

impl McpStatusResult {
    pub fn new(
        server_name: impl Into<String>,
        state: McpServerState,
        tool_count: usize,
        last_error: Option<String>,
    ) -> Result<Self, McpProtocolError> {
        let result = Self {
            server_name: server_name.into(),
            state,
            tool_count,
            last_error,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub const fn state(&self) -> McpServerState {
        self.state
    }

    pub const fn tool_count(&self) -> usize {
        self.tool_count
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn validate(&self) -> Result<(), McpProtocolError> {
        validate_server_name(&self.server_name)?;
        if self.tool_count > MAX_TOOLS {
            return Err(McpProtocolError::TooManyTools {
                count: self.tool_count,
                maximum: MAX_TOOLS,
            });
        }
        if let Some(error) = &self.last_error {
            validate_text("MCP status error", error, MAX_STATUS_ERROR_BYTES, false)?;
        }
        Ok(())
    }
}

fn validate_server_name(value: &str) -> Result<(), McpProtocolError> {
    validate_text("MCP server name", value, MAX_SERVER_NAME_BYTES, true)?;
    for (index, character) in value.char_indices() {
        if !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '-' | '_')
        {
            return Err(McpProtocolError::InvalidNameCharacter { index, character });
        }
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    require_lowercase_start: bool,
) -> Result<(), McpProtocolError> {
    if value.trim().is_empty() {
        return Err(McpProtocolError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(McpProtocolError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(McpProtocolError::ControlCharacter { field });
    }
    if require_lowercase_start
        && !value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
    {
        return Err(McpProtocolError::InvalidNameStart {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_value_size(
    value: &Value,
    maximum: usize,
    field: &'static str,
) -> Result<(), McpProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|error| McpProtocolError::InvalidValue {
        message: error.to_string(),
    })?;
    if bytes.len() > maximum {
        return Err(McpProtocolError::ValueTooLarge {
            field,
            size: bytes.len(),
            maximum,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpProtocolError {
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    ControlCharacter {
        field: &'static str,
    },
    InvalidNameStart {
        field: &'static str,
        value: String,
    },
    InvalidNameCharacter {
        index: usize,
        character: char,
    },
    SchemaMustBeObject,
    TooManyTools {
        count: usize,
        maximum: usize,
    },
    DuplicateTool {
        name: String,
    },
    InvalidGrant(String),
    InvalidNetworkGrant(String),
    InvalidSecretGrant(String),
    ZeroRequestId,
    CancelledWithoutSupport,
    ValueTooLarge {
        field: &'static str,
        size: usize,
        maximum: usize,
    },
    InvalidValue {
        message: String,
    },
}

impl fmt::Display for McpProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::ControlCharacter { field } => {
                write!(formatter, "{field} contains a control character")
            }
            Self::InvalidNameStart { field, value } => {
                write!(
                    formatter,
                    "{field} `{value}` must start with a lowercase ASCII letter"
                )
            }
            Self::InvalidNameCharacter { index, character } => write!(
                formatter,
                "MCP server name contains unsupported character `{character}` at byte {index}"
            ),
            Self::SchemaMustBeObject => formatter.write_str("MCP tool schema must be an object"),
            Self::TooManyTools { count, maximum } => write!(
                formatter,
                "MCP tool list contains {count} tools; maximum is {maximum}"
            ),
            Self::DuplicateTool { name } => {
                write!(formatter, "MCP tool list repeats `{name}`")
            }
            Self::InvalidGrant(message) => {
                write!(formatter, "MCP action grant is invalid: {message}")
            }
            Self::InvalidNetworkGrant(message) => {
                write!(formatter, "MCP network grant is invalid: {message}")
            }
            Self::InvalidSecretGrant(message) => {
                write!(formatter, "MCP secret grant is invalid: {message}")
            }
            Self::ZeroRequestId => formatter.write_str("MCP request id must be greater than zero"),
            Self::CancelledWithoutSupport => {
                formatter.write_str("MCP cancellation cannot be marked cancelled when unsupported")
            }
            Self::ValueTooLarge {
                field,
                size,
                maximum,
            } => write!(formatter, "{field} is {size} bytes; maximum is {maximum}"),
            Self::InvalidValue { message } => write!(formatter, "MCP value is invalid: {message}"),
        }
    }
}

impl Error for McpProtocolError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{ActionGrant, WorkspaceGrant};

    fn grant() -> ActionGrant {
        ActionGrant::approved(
            WorkspaceGrant::read_only("C:\\workspace"),
            "C:\\workspace",
            "ticket-1",
        )
    }

    #[test]
    fn mcp_descriptors_round_trip_with_bounded_schema() {
        let descriptor = McpToolDescriptor::new(
            "echo",
            "Echo text",
            json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        )
        .expect("descriptor");
        let result = McpToolListResult::new("fixture", vec![descriptor], false).expect("list");
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("inputSchema"));
        assert!(!json.contains("input_schema"));
        let decoded = serde_json::from_str::<McpToolListResult>(&json).expect("deserialize");
        assert_eq!(decoded, result);
    }

    #[test]
    fn mcp_call_requires_approved_grant_and_bounds_arguments() {
        let pending =
            ActionGrant::pending(WorkspaceGrant::read_only("C:\\workspace"), "C:\\workspace");
        assert!(matches!(
            McpToolCallRequest::new(pending, "fixture", "echo", json!({})),
            Err(McpProtocolError::InvalidGrant(_))
        ));

        let request = McpToolCallRequest::new(grant(), "fixture", "echo", json!({"text": "hi"}))
            .expect("call request");
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<McpToolCallRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn invalid_server_names_and_duplicate_tools_fail_closed() {
        assert!(McpToolListResult::new("Fixture", Vec::new(), false).is_err());
        let tool = McpToolDescriptor::new("echo", "Echo", json!({"type": "object"})).expect("tool");
        assert!(matches!(
            McpToolListResult::new("fixture", vec![tool.clone(), tool], false),
            Err(McpProtocolError::DuplicateTool { .. })
        ));
    }

    #[test]
    fn discovery_authority_is_optional_for_legacy_stdio_but_round_trips_when_present() {
        let network = NetworkGrant::for_url("https://api.example.test/mcp").expect("network");
        let secrets = SecretGrant::one("provider/openai/api_key").expect("secret");
        let request = McpToolListRequest::new()
            .with_network_grant(network)
            .with_secret_grant(secrets);
        request.validate().expect("valid discovery authority");
        let json = serde_json::to_string(&request).expect("serialize request");
        assert!(!json.contains("secret-value"));
        assert_eq!(
            serde_json::from_str::<McpToolListRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn cancellation_contract_round_trips_and_rejects_zero_ids() {
        let request = McpToolCancelRequest::new("fixture", 7, "user requested cancellation")
            .expect("cancel request");
        let json = serde_json::to_string(&request).expect("serialize cancellation");
        assert_eq!(
            serde_json::from_str::<McpToolCancelRequest>(&json).expect("deserialize cancellation"),
            request
        );
        assert!(matches!(
            McpToolCancelRequest::new("fixture", 0, "stop"),
            Err(McpProtocolError::ZeroRequestId)
        ));
        let result = McpToolCancelResult::new("fixture", 7, true, true).expect("cancel result");
        assert!(result.cancelled());
        assert!(result.supported());
    }
}
