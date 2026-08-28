//! Blocking, bounded OpenAI-compatible Chat Completions client.

use std::error::Error;
use std::fmt;
use std::io::Read;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use yunxi_protocol::{ChatMessage, ChatRole, ToolCall, ToolCatalog};

use crate::ProviderConfig;

const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

pub struct OpenAiChatClient {
    config: ProviderConfig,
    client: Client,
}

impl OpenAiChatClient {
    pub fn new(config: ProviderConfig) -> Result<Self, ApiError> {
        let client = Client::builder()
            .timeout(config.timeout())
            .build()
            .map_err(ApiError::Transport)?;
        Ok(Self { config, client })
    }

    pub fn complete(&self, messages: &[ChatMessage]) -> Result<ChatCompletion, ApiError> {
        self.complete_with_tools(messages, None)
    }

    pub fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: Option<&ToolCatalog>,
    ) -> Result<ChatCompletion, ApiError> {
        if messages.is_empty() {
            return Err(ApiError::InvalidResponse(
                "chat request must contain at least one message".to_string(),
            ));
        }
        let request = ChatCompletionRequest {
            model: self.config.model().to_string(),
            messages: messages
                .iter()
                .map(ApiChatMessage::from_protocol)
                .collect::<Result<Vec<_>, _>>()?,
            tools: tools.map(ApiToolDefinition::from_protocol),
            stream: false,
        };
        let response = self
            .client
            .post(self.config.chat_completions_url())
            .bearer_auth(self.config.api_key())
            .json(&request)
            .send()
            .map_err(ApiError::Transport)?;
        parse_response(response)
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatCompletion {
    content: String,
    finish_reason: Option<String>,
    tool_calls: Vec<ToolCall>,
}

impl ChatCompletion {
    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }
}

#[derive(Debug)]
pub enum ApiError {
    Transport(reqwest::Error),
    Http {
        status: u16,
        message: String,
        retryable: bool,
    },
    ResponseTooLarge {
        limit: u64,
    },
    InvalidResponse(String),
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport_error",
            Self::Http { .. } => "http_error",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::InvalidResponse(_) => "invalid_response",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport(error) => error.is_connect() || error.is_timeout(),
            Self::Http { retryable, .. } => *retryable,
            Self::ResponseTooLarge { .. } | Self::InvalidResponse(_) => false,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "model API request failed: {error}"),
            Self::Http {
                status, message, ..
            } => write!(formatter, "model API returned HTTP {status}: {message}"),
            Self::ResponseTooLarge { limit } => {
                write!(formatter, "model API response exceeded {limit} bytes")
            }
            Self::InvalidResponse(message) => {
                write!(formatter, "model API response was invalid: {message}")
            }
        }
    }
}

impl Error for ApiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Http { .. } | Self::ResponseTooLarge { .. } | Self::InvalidResponse(_) => None,
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ApiChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDefinition>>,
    stream: bool,
}

#[derive(Serialize)]
struct ApiChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ApiToolCall>>,
}

impl ApiChatMessage {
    fn from_protocol(message: &ChatMessage) -> Result<Self, ApiError> {
        let role = match message.role() {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };
        let tool_calls = if message.tool_calls().is_empty() {
            None
        } else {
            Some(
                message
                    .tool_calls()
                    .iter()
                    .map(ApiToolCall::from_protocol)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };
        Ok(Self {
            role,
            content: if message.role() == ChatRole::Assistant
                && message.content().is_empty()
                && tool_calls.is_some()
            {
                None
            } else {
                Some(message.content().to_string())
            },
            tool_call_id: message.tool_call_id().map(ToString::to_string),
            tool_calls,
        })
    }
}

#[derive(Serialize)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ApiFunctionCall,
}

impl ApiToolCall {
    fn from_protocol(call: &ToolCall) -> Result<Self, ApiError> {
        let arguments = serde_json::to_string(call.arguments())
            .map_err(|error| ApiError::InvalidResponse(error.to_string()))?;
        Ok(Self {
            id: call.id().to_string(),
            kind: "function",
            function: ApiFunctionCall {
                name: call.name().to_string(),
                arguments,
            },
        })
    }
}

#[derive(Serialize)]
struct ApiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ApiToolDefinition {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ApiFunctionDefinition,
}

impl ApiToolDefinition {
    fn from_protocol(catalog: &ToolCatalog) -> Vec<Self> {
        catalog
            .tools()
            .iter()
            .map(|definition| Self {
                kind: "function",
                function: ApiFunctionDefinition {
                    name: definition.name().to_string(),
                    description: definition.description().to_string(),
                    parameters: definition.input_schema().clone(),
                },
            })
            .collect()
    }
}

#[derive(Serialize)]
struct ApiFunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: AssistantMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ApiResponseToolCall>>,
}

#[derive(Deserialize)]
struct ApiResponseToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ApiResponseFunctionCall,
}

#[derive(Deserialize)]
struct ApiResponseFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
}

fn parse_response(mut response: Response) -> Result<ChatCompletion, ApiError> {
    let status = response.status();
    let body = read_bounded(&mut response)?;
    if !status.is_success() {
        let message = serde_json::from_slice::<ErrorEnvelope>(&body)
            .ok()
            .and_then(|envelope| envelope.error)
            .and_then(|error| error.message)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
        return Err(ApiError::Http {
            status: status.as_u16(),
            message: if message.is_empty() {
                "empty error response".to_string()
            } else {
                message
            },
            retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
        });
    }

    let mut parsed = serde_json::from_slice::<ChatCompletionResponse>(&body)
        .map_err(|error| ApiError::InvalidResponse(error.to_string()))?;
    let choice =
        parsed.choices.drain(..).next().ok_or_else(|| {
            ApiError::InvalidResponse("response contained no choices".to_string())
        })?;
    let Choice {
        message,
        finish_reason,
    } = choice;
    let AssistantMessage {
        content,
        tool_calls,
    } = message;
    let content = content.unwrap_or_default();
    let tool_calls = tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(parse_tool_call)
        .collect::<Result<Vec<_>, _>>()?;
    if content.is_empty() && tool_calls.is_empty() {
        return Err(ApiError::InvalidResponse(
            "first choice contained no content or tool calls".to_string(),
        ));
    }
    Ok(ChatCompletion {
        content,
        finish_reason,
        tool_calls,
    })
}

fn parse_tool_call(call: ApiResponseToolCall) -> Result<ToolCall, ApiError> {
    if call.kind != "function" {
        return Err(ApiError::InvalidResponse(format!(
            "tool call `{}` has unsupported type `{}`",
            call.id, call.kind
        )));
    }
    let arguments = serde_json::from_str(&call.function.arguments).map_err(|error| {
        ApiError::InvalidResponse(format!(
            "tool call `{}` arguments are invalid JSON: {error}",
            call.id
        ))
    })?;
    ToolCall::new(call.id, call.function.name, arguments)
        .map_err(|error| ApiError::InvalidResponse(error.to_string()))
}

fn read_bounded(response: &mut Response) -> Result<Vec<u8>, ApiError> {
    let mut body = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|error| ApiError::InvalidResponse(error.to_string()))?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(ApiError::ResponseTooLarge {
            limit: MAX_RESPONSE_BYTES,
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use serde_json::json;
    use yunxi_protocol::{ToolCatalog, ToolDefinition, ToolName};

    use super::*;

    #[test]
    fn client_sends_bearer_auth_and_parses_first_choice() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
        let address = listener.local_addr().expect("read mock API address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept API request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone API stream"));
            let mut headers = String::new();
            let mut content_length = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length =
                            Some(value.trim().parse::<usize>().expect("valid content length"));
                    }
                }
                headers.push_str(&line);
            }
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-key")
            );
            let mut request_body = vec![0; content_length.expect("request content length")];
            reader
                .read_exact(&mut request_body)
                .expect("read request body");
            let body =
                r#"{"choices":[{"message":{"content":"fixture reply"},"finish_reason":"stop"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write API response");
        });

        let config = ProviderConfig::new(
            "fixture",
            "fixture-model",
            format!("http://{address}"),
            "test-key",
        )
        .expect("create provider config")
        .with_timeout(Duration::from_secs(2));
        let client = OpenAiChatClient::new(config).expect("create API client");
        let completion = client
            .complete(&[ChatMessage::user("hello")])
            .expect("complete chat");

        assert_eq!(completion.content(), "fixture reply");
        assert_eq!(completion.finish_reason(), Some("stop"));
        server.join().expect("join mock API");
    }

    #[test]
    fn client_sends_tool_catalog_and_parses_function_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
        let address = listener.local_addr().expect("read mock API address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept API request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone API stream"));
            let mut content_length = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = Some(value.trim().parse::<usize>().expect("content length"));
                }
            }
            let mut request_body = vec![0; content_length.expect("request content length")];
            reader
                .read_exact(&mut request_body)
                .expect("read request body");
            let request_body = String::from_utf8(request_body).expect("UTF-8 request body");
            assert!(request_body.contains("\"tools\":["));
            assert!(request_body.contains("shell.execute"));
            let body = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"shell.execute","arguments":"{\"command\":\"echo hello\"}"}}]},"finish_reason":"tool_calls"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write API response");
        });

        let config = ProviderConfig::new(
            "fixture",
            "fixture-model",
            format!("http://{address}"),
            "test-key",
        )
        .expect("create provider config")
        .with_timeout(Duration::from_secs(2));
        let client = OpenAiChatClient::new(config).expect("create API client");
        let catalog = ToolCatalog::new(vec![
            ToolDefinition::new(
                ToolName::new("shell.execute").expect("tool name"),
                "Execute a shell command",
                json!({"type": "object"}),
            )
            .expect("tool definition"),
        ])
        .expect("tool catalog");
        let completion = client
            .complete_with_tools(&[ChatMessage::user("run it")], Some(&catalog))
            .expect("complete with tool call");

        assert_eq!(completion.content(), "");
        assert_eq!(completion.tool_calls().len(), 1);
        assert_eq!(completion.tool_calls()[0].name().as_str(), "shell.execute");
        assert_eq!(
            completion.tool_calls()[0].arguments()["command"],
            "echo hello"
        );
        server.join().expect("join mock API");
    }
}
