//! Explicit configuration for one external MCP Server process.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use serde_json::Value;
use yunxi_protocol::NetworkScope;

pub const TRANSPORT_ENV: &str = "YUNXI_NEXT_MCP_TRANSPORT";
const COMMAND_ENV: &str = "YUNXI_NEXT_MCP_COMMAND";
const ARGS_ENV: &str = "YUNXI_NEXT_MCP_ARGS_JSON";
const NAME_ENV: &str = "YUNXI_NEXT_MCP_NAME";
const CHILD_ENV_ENV: &str = "YUNXI_NEXT_MCP_ENV_JSON";
pub const HTTP_ENDPOINT_ENV: &str = "YUNXI_NEXT_MCP_URL";
pub const HTTP_HEADERS_ENV: &str = "YUNXI_NEXT_MCP_HEADERS_JSON";
pub const HTTP_SECRETS_ENV: &str = "YUNXI_NEXT_MCP_SECRETS_JSON";
pub const NETWORK_GRANT_ENV: &str = "YUNXI_NEXT_MCP_NETWORK_GRANT_JSON";
pub const SECRET_GRANT_ENV: &str = "YUNXI_NEXT_MCP_SECRET_GRANT_JSON";
pub const SECRET_REFERENCE_PREFIX: &str = "secret://";
const DEFAULT_SERVER_NAME: &str = "default";
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_ENV_ENTRIES: usize = 64;
const MAX_ENV_KEY_BYTES: usize = 256;
const MAX_ENV_VALUE_BYTES: usize = 64 * 1024;
const MAX_ENDPOINT_BYTES: usize = 4096;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 64 * 1024;
const MAX_SECRET_VALUES: usize = 64;
const MAX_SECRET_REFERENCE_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpTransportKind {
    Stdio,
    Http,
}

#[derive(Clone, Eq, PartialEq)]
enum McpTransportConfig {
    Stdio,
    Http {
        endpoint: String,
        headers: BTreeMap<String, String>,
        secret_values: BTreeMap<String, String>,
    },
}

#[derive(Clone, Eq, PartialEq)]
pub struct McpConfig {
    command: PathBuf,
    arguments: Vec<String>,
    server_name: String,
    environment: BTreeMap<String, String>,
    transport: McpTransportConfig,
}

impl McpConfig {
    pub fn new(
        command: impl Into<PathBuf>,
        arguments: Vec<String>,
        server_name: impl Into<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self, McpConfigError> {
        let config = Self {
            command: command.into(),
            arguments,
            server_name: server_name.into(),
            environment,
            transport: McpTransportConfig::Stdio,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn http(
        endpoint: impl Into<String>,
        server_name: impl Into<String>,
        headers: BTreeMap<String, String>,
    ) -> Result<Self, McpConfigError> {
        Self::http_with_secrets(endpoint, server_name, headers, BTreeMap::new())
    }

    pub fn http_with_secrets(
        endpoint: impl Into<String>,
        server_name: impl Into<String>,
        headers: BTreeMap<String, String>,
        secret_values: BTreeMap<String, String>,
    ) -> Result<Self, McpConfigError> {
        let config = Self {
            command: PathBuf::new(),
            arguments: Vec::new(),
            server_name: server_name.into(),
            environment: BTreeMap::new(),
            transport: McpTransportConfig::Http {
                endpoint: endpoint.into(),
                headers,
                secret_values,
            },
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_env() -> Result<Self, McpConfigError> {
        let transport = env::var(TRANSPORT_ENV).unwrap_or_else(|_| "stdio".to_string());
        match transport.trim().to_ascii_lowercase().as_str() {
            "stdio" => Self::from_stdio_env(),
            "http" | "https" | "streamable-http" => Self::from_http_env(),
            _ => Err(McpConfigError::InvalidTransport { value: transport }),
        }
    }

    fn from_stdio_env() -> Result<Self, McpConfigError> {
        let command = env::var(COMMAND_ENV)
            .map_err(|_| McpConfigError::MissingEnvironment { name: COMMAND_ENV })?;
        let arguments = match env::var(ARGS_ENV) {
            Ok(value) if !value.trim().is_empty() => parse_arguments(&value)?,
            Ok(_) | Err(env::VarError::NotPresent) => Vec::new(),
            Err(error) => {
                return Err(McpConfigError::InvalidEnvironment {
                    name: ARGS_ENV,
                    message: error.to_string(),
                });
            }
        };
        let server_name = env::var(NAME_ENV).unwrap_or_else(|_| DEFAULT_SERVER_NAME.to_string());
        let mut environment = match env::var(CHILD_ENV_ENV) {
            Ok(value) if !value.trim().is_empty() => parse_environment(&value)?,
            Ok(_) | Err(env::VarError::NotPresent) => BTreeMap::new(),
            Err(error) => {
                return Err(McpConfigError::InvalidEnvironment {
                    name: CHILD_ENV_ENV,
                    message: error.to_string(),
                });
            }
        };

        // PATH is launch plumbing, not a credential. No other parent variable
        // is inherited unless it appears in the explicit JSON allowlist.
        if !environment.contains_key("PATH")
            && let Some(path) = env::var_os("PATH")
            && let Some(path) = path.to_str()
        {
            environment.insert("PATH".to_string(), path.to_string());
        }

        Self::new(command, arguments, server_name, environment)
    }

    fn from_http_env() -> Result<Self, McpConfigError> {
        let endpoint =
            env::var(HTTP_ENDPOINT_ENV).map_err(|_| McpConfigError::MissingEnvironment {
                name: HTTP_ENDPOINT_ENV,
            })?;
        let server_name = env::var(NAME_ENV).unwrap_or_else(|_| DEFAULT_SERVER_NAME.to_string());
        let headers = match env::var(HTTP_HEADERS_ENV) {
            Ok(value) if !value.trim().is_empty() => parse_string_map(&value, HTTP_HEADERS_ENV)?,
            Ok(_) | Err(env::VarError::NotPresent) => BTreeMap::new(),
            Err(error) => {
                return Err(McpConfigError::InvalidEnvironment {
                    name: HTTP_HEADERS_ENV,
                    message: error.to_string(),
                });
            }
        };
        let secrets = match env::var(HTTP_SECRETS_ENV) {
            Ok(value) if !value.trim().is_empty() => parse_string_map(&value, HTTP_SECRETS_ENV)?,
            Ok(_) | Err(env::VarError::NotPresent) => BTreeMap::new(),
            Err(error) => {
                return Err(McpConfigError::InvalidEnvironment {
                    name: HTTP_SECRETS_ENV,
                    message: error.to_string(),
                });
            }
        };
        Self::http_with_secrets(endpoint, server_name, headers, secrets)
    }

    pub fn command(&self) -> &std::path::Path {
        &self.command
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub const fn transport_kind(&self) -> McpTransportKind {
        match self.transport {
            McpTransportConfig::Stdio => McpTransportKind::Stdio,
            McpTransportConfig::Http { .. } => McpTransportKind::Http,
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        match &self.transport {
            McpTransportConfig::Stdio => None,
            McpTransportConfig::Http { endpoint, .. } => Some(endpoint),
        }
    }

    pub fn http_headers(&self) -> Option<&BTreeMap<String, String>> {
        match &self.transport {
            McpTransportConfig::Stdio => None,
            McpTransportConfig::Http { headers, .. } => Some(headers),
        }
    }

    pub fn secret_value(&self, reference: &str) -> Option<&str> {
        match &self.transport {
            McpTransportConfig::Stdio => None,
            McpTransportConfig::Http { secret_values, .. } => {
                secret_values.get(reference).map(String::as_str)
            }
        }
    }

    pub fn secret_references(&self) -> Vec<&str> {
        self.http_headers()
            .into_iter()
            .flat_map(|headers| headers.values())
            .filter_map(|value| secret_reference(value))
            .collect()
    }

    fn validate(&self) -> Result<(), McpConfigError> {
        validate_server_name(&self.server_name)?;
        match &self.transport {
            McpTransportConfig::Stdio => {
                let command = self.command.as_os_str().to_string_lossy();
                validate_text("MCP command", &command, MAX_COMMAND_BYTES, true)?;
                if self.arguments.len() > MAX_ARGUMENTS {
                    return Err(McpConfigError::TooManyArguments {
                        count: self.arguments.len(),
                        maximum: MAX_ARGUMENTS,
                    });
                }
                for argument in &self.arguments {
                    validate_text("MCP argument", argument, MAX_ARGUMENT_BYTES, true)?;
                }
                validate_environment(&self.environment)?;
            }
            McpTransportConfig::Http {
                endpoint,
                headers,
                secret_values,
            } => {
                validate_endpoint(endpoint)?;
                validate_headers(headers)?;
                validate_secret_values(secret_values)?;
                let mut references = BTreeSet::new();
                for value in headers.values() {
                    if let Some(reference) = secret_reference(value) {
                        if !secret_values.contains_key(reference) {
                            return Err(McpConfigError::MissingSecretValue {
                                reference: reference.to_string(),
                            });
                        }
                        references.insert(reference.to_string());
                    }
                }
                if references.len() > MAX_SECRET_VALUES {
                    return Err(McpConfigError::TooManySecretValues {
                        count: references.len(),
                        maximum: MAX_SECRET_VALUES,
                    });
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for McpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("McpConfig");
        debug
            .field("transport", &self.transport_kind())
            .field("server_name", &self.server_name);
        match &self.transport {
            McpTransportConfig::Stdio => {
                debug
                    .field("command", &self.command)
                    .field("arguments", &self.arguments)
                    .field(
                        "environment_keys",
                        &self.environment.keys().collect::<Vec<_>>(),
                    );
            }
            McpTransportConfig::Http {
                endpoint, headers, ..
            } => {
                debug
                    .field("endpoint", endpoint)
                    .field("header_names", &headers.keys().collect::<Vec<_>>())
                    .field("secret_values", &"<redacted>");
            }
        }
        debug.finish()
    }
}

fn parse_arguments(raw: &str) -> Result<Vec<String>, McpConfigError> {
    let value =
        serde_json::from_str::<Value>(raw).map_err(|error| McpConfigError::InvalidJson {
            field: ARGS_ENV,
            message: error.to_string(),
        })?;
    let Some(values) = value.as_array() else {
        return Err(McpConfigError::InvalidJson {
            field: ARGS_ENV,
            message: "expected a JSON array of strings".to_string(),
        });
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| McpConfigError::InvalidJson {
                    field: ARGS_ENV,
                    message: "every argument must be a string".to_string(),
                })
        })
        .collect()
}

fn parse_environment(raw: &str) -> Result<BTreeMap<String, String>, McpConfigError> {
    parse_string_map(raw, CHILD_ENV_ENV)
}

fn parse_string_map(
    raw: &str,
    field: &'static str,
) -> Result<BTreeMap<String, String>, McpConfigError> {
    let value =
        serde_json::from_str::<Value>(raw).map_err(|error| McpConfigError::InvalidJson {
            field,
            message: error.to_string(),
        })?;
    let Some(values) = value.as_object() else {
        return Err(McpConfigError::InvalidJson {
            field,
            message: "expected a JSON object mapping names to strings".to_string(),
        });
    };
    values
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().ok_or_else(|| McpConfigError::InvalidJson {
                field,
                message: format!("environment value `{key}` must be a string"),
            })?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<(), McpConfigError> {
    if environment.len() > MAX_ENV_ENTRIES {
        return Err(McpConfigError::TooManyEnvironmentEntries {
            count: environment.len(),
            maximum: MAX_ENV_ENTRIES,
        });
    }
    for (key, value) in environment {
        validate_environment_key(key)?;
        validate_text("MCP environment value", value, MAX_ENV_VALUE_BYTES, false)?;
    }
    Ok(())
}

fn validate_endpoint(value: &str) -> Result<(), McpConfigError> {
    if value.len() > MAX_ENDPOINT_BYTES {
        return Err(McpConfigError::FieldTooLong {
            field: "MCP HTTP endpoint",
            length: value.len(),
            maximum: MAX_ENDPOINT_BYTES,
        });
    }
    if value.contains('?') || value.contains('#') || value.contains('@') {
        return Err(McpConfigError::InvalidEndpoint {
            value: value.to_string(),
        });
    }
    NetworkScope::from_url(value).map_err(|error| McpConfigError::InvalidEndpoint {
        value: error.to_string(),
    })?;
    Ok(())
}

fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), McpConfigError> {
    if headers.len() > MAX_HEADERS {
        return Err(McpConfigError::TooManyHeaders {
            count: headers.len(),
            maximum: MAX_HEADERS,
        });
    }
    for (name, value) in headers {
        validate_header_name(name)?;
        validate_text(
            "MCP HTTP header value",
            value,
            MAX_HEADER_VALUE_BYTES,
            false,
        )?;
        if let Some(reference) = secret_reference(value) {
            validate_secret_reference(reference)?;
        } else if is_secret_header_name(name) {
            return Err(McpConfigError::SecretHeaderMustUseReference {
                value: name.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_header_name(value: &str) -> Result<(), McpConfigError> {
    validate_text("MCP HTTP header name", value, MAX_HEADER_NAME_BYTES, true)?;
    if !value.chars().all(is_header_name_character) {
        return Err(McpConfigError::InvalidHeaderName {
            value: value.to_string(),
        });
    }
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "transfer-encoding"
    ) {
        return Err(McpConfigError::ForbiddenHeader {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn is_header_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn is_secret_header_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "proxy-authorization" | "set-cookie" | "x-api-key"
    )
}

fn validate_secret_values(values: &BTreeMap<String, String>) -> Result<(), McpConfigError> {
    if values.len() > MAX_SECRET_VALUES {
        return Err(McpConfigError::TooManySecretValues {
            count: values.len(),
            maximum: MAX_SECRET_VALUES,
        });
    }
    for (reference, value) in values {
        validate_secret_reference(reference)?;
        validate_text("MCP secret value", value, MAX_ENV_VALUE_BYTES, true)?;
    }
    Ok(())
}

fn validate_secret_reference(value: &str) -> Result<(), McpConfigError> {
    if value.trim().is_empty() || value.len() > MAX_SECRET_REFERENCE_BYTES {
        return Err(McpConfigError::InvalidSecretReference {
            value: value.to_string(),
        });
    }
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '/')
    }) || !value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        return Err(McpConfigError::InvalidSecretReference {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn secret_reference(value: &str) -> Option<&str> {
    value
        .strip_prefix(SECRET_REFERENCE_PREFIX)
        .filter(|reference| !reference.is_empty())
}

fn validate_server_name(value: &str) -> Result<(), McpConfigError> {
    validate_text("MCP server name", value, 64, true)?;
    for character in value.chars() {
        if !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '-' | '_')
        {
            return Err(McpConfigError::InvalidServerName {
                value: value.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_environment_key(value: &str) -> Result<(), McpConfigError> {
    validate_text("MCP environment key", value, MAX_ENV_KEY_BYTES, true)?;
    if value.contains('=') {
        return Err(McpConfigError::InvalidEnvironmentKey {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    reject_empty: bool,
) -> Result<(), McpConfigError> {
    if reject_empty && value.trim().is_empty() {
        return Err(McpConfigError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(McpConfigError::FieldTooLong {
            field,
            length: value.len(),
            maximum,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(McpConfigError::ControlCharacter { field });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpConfigError {
    InvalidTransport {
        value: String,
    },
    MissingEnvironment {
        name: &'static str,
    },
    InvalidEnvironment {
        name: &'static str,
        message: String,
    },
    InvalidJson {
        field: &'static str,
        message: String,
    },
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
    TooManyArguments {
        count: usize,
        maximum: usize,
    },
    TooManyEnvironmentEntries {
        count: usize,
        maximum: usize,
    },
    InvalidServerName {
        value: String,
    },
    InvalidEnvironmentKey {
        value: String,
    },
    InvalidEndpoint {
        value: String,
    },
    TooManyHeaders {
        count: usize,
        maximum: usize,
    },
    InvalidHeaderName {
        value: String,
    },
    ForbiddenHeader {
        value: String,
    },
    SecretHeaderMustUseReference {
        value: String,
    },
    TooManySecretValues {
        count: usize,
        maximum: usize,
    },
    InvalidSecretReference {
        value: String,
    },
    MissingSecretValue {
        reference: String,
    },
}

impl fmt::Display for McpConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransport { value } => write!(
                formatter,
                "MCP transport `{value}` is unsupported; expected `stdio` or `http`"
            ),
            Self::MissingEnvironment { name } => {
                write!(formatter, "required environment variable {name} is missing")
            }
            Self::InvalidEnvironment { name, message } => {
                write!(
                    formatter,
                    "environment variable {name} is invalid: {message}"
                )
            }
            Self::InvalidJson { field, message } => {
                write!(formatter, "{field} contains invalid JSON: {message}")
            }
            Self::EmptyField { field } => write!(formatter, "{field} cannot be empty"),
            Self::FieldTooLong {
                field,
                length,
                maximum,
            } => write!(formatter, "{field} is {length} bytes; maximum is {maximum}"),
            Self::ControlCharacter { field } => {
                write!(formatter, "{field} contains a control character")
            }
            Self::TooManyArguments { count, maximum } => write!(
                formatter,
                "MCP command has {count} arguments; maximum is {maximum}"
            ),
            Self::TooManyEnvironmentEntries { count, maximum } => write!(
                formatter,
                "MCP child environment has {count} entries; maximum is {maximum}"
            ),
            Self::InvalidServerName { value } => write!(
                formatter,
                "MCP server name `{value}` must contain only lowercase letters, digits, `-`, or `_`"
            ),
            Self::InvalidEnvironmentKey { value } => {
                write!(formatter, "MCP environment key `{value}` is invalid")
            }
            Self::InvalidEndpoint { value } => {
                write!(formatter, "MCP HTTP endpoint is invalid: {value}")
            }
            Self::TooManyHeaders { count, maximum } => write!(
                formatter,
                "MCP HTTP configuration has {count} headers; maximum is {maximum}"
            ),
            Self::InvalidHeaderName { value } => {
                write!(formatter, "MCP HTTP header name `{value}` is invalid")
            }
            Self::ForbiddenHeader { value } => {
                write!(formatter, "MCP HTTP header `{value}` cannot be overridden")
            }
            Self::SecretHeaderMustUseReference { value } => write!(
                formatter,
                "MCP HTTP header `{value}` must use a secret reference"
            ),
            Self::TooManySecretValues { count, maximum } => write!(
                formatter,
                "MCP configuration has {count} secret values; maximum is {maximum}"
            ),
            Self::InvalidSecretReference { value } => {
                write!(formatter, "MCP secret reference `{value}` is invalid")
            }
            Self::MissingSecretValue { reference } => write!(
                formatter,
                "MCP header references secret `{reference}`, but no value was configured"
            ),
        }
    }
}

impl Error for McpConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_accepts_direct_command_and_explicit_environment() {
        let config = McpConfig::new(
            "fixture",
            vec!["--stdio".to_string()],
            "fixture",
            BTreeMap::from([("TOKEN".to_string(), "explicit".to_string())]),
        )
        .expect("valid config");
        assert_eq!(config.arguments(), ["--stdio"]);
        assert_eq!(
            config.environment().get("TOKEN"),
            Some(&"explicit".to_string())
        );
    }

    #[test]
    fn argument_and_environment_shapes_are_fail_closed() {
        assert!(matches!(
            parse_arguments(r#"{"not":"an array"}"#),
            Err(McpConfigError::InvalidJson {
                field: ARGS_ENV,
                ..
            })
        ));
        assert!(matches!(
            parse_environment(r#"["not", "an object"]"#),
            Err(McpConfigError::InvalidJson {
                field: CHILD_ENV_ENV,
                ..
            })
        ));
        assert!(McpConfig::new("fixture", Vec::new(), "Fixture", BTreeMap::new()).is_err());
    }

    #[test]
    fn http_configuration_requires_a_bounded_endpoint_and_redacts_values() {
        let config = McpConfig::http_with_secrets(
            "http://127.0.0.1:43123/mcp",
            "fixture",
            BTreeMap::from([(
                "Authorization".to_string(),
                "secret://fixture/token".to_string(),
            )]),
            BTreeMap::from([("fixture/token".to_string(), "top-secret".to_string())]),
        )
        .expect("valid HTTP config");
        assert_eq!(config.transport_kind(), McpTransportKind::Http);
        assert_eq!(config.secret_references(), ["fixture/token"]);
        let debug = format!("{config:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("secret://fixture/token"));
        assert!(debug.contains("Authorization"));
    }

    #[test]
    fn http_configuration_fails_closed_for_missing_secret_and_forbidden_headers() {
        let missing = McpConfig::http(
            "https://api.example.test/mcp",
            "fixture",
            BTreeMap::from([(
                "Authorization".to_string(),
                "secret://fixture/token".to_string(),
            )]),
        )
        .expect_err("missing secret value must fail");
        assert!(matches!(missing, McpConfigError::MissingSecretValue { .. }));

        let forbidden = McpConfig::http(
            "https://api.example.test/mcp",
            "fixture",
            BTreeMap::from([("Host".to_string(), "evil.example.test".to_string())]),
        )
        .expect_err("Host override must fail");
        assert!(matches!(forbidden, McpConfigError::ForbiddenHeader { .. }));

        let literal_secret = McpConfig::http(
            "https://api.example.test/mcp",
            "fixture",
            BTreeMap::from([("Authorization".to_string(), "Bearer raw-value".to_string())]),
        )
        .expect_err("literal credentials must fail");
        assert!(matches!(
            literal_secret,
            McpConfigError::SecretHeaderMustUseReference { .. }
        ));
    }
}
