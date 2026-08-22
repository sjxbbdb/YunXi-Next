//! Blocking, bounded OpenAI-compatible Chat Completions client.

use std::error::Error;
use std::fmt;
use std::io::Read;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use yunxi_protocol::ChatMessage;

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
        if messages.is_empty() {
            return Err(ApiError::InvalidResponse(
                "chat request must contain at least one message".to_string(),
            ));
        }
        let request = ChatCompletionRequest {
            model: self.config.model(),
            messages,
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
}

impl ChatCompletion {
    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
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
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
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
    let content = choice
        .message
        .content
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            ApiError::InvalidResponse("first choice contained no content".to_string())
        })?;
    Ok(ChatCompletion {
        content,
        finish_reason: choice.finish_reason,
    })
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
}
