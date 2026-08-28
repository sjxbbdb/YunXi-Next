//! Bounded MCP streamable-HTTP transport.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use serde_json::Value;
use yunxi_protocol::{NetworkGrant, SecretGrant};

use crate::client::{MAX_MCP_FRAME_BYTES, MAX_MCP_RESULT_BYTES, McpClientError};
use crate::config::{McpConfig, SECRET_REFERENCE_PREFIX};

const MAX_HTTP_RESPONSE_BYTES: usize = MAX_MCP_RESULT_BYTES;
const CANCEL_GRACE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_SESSION_ID_BYTES: usize = 256;

pub(crate) struct HttpTransport {
    endpoint: String,
    client: Client,
    headers: BTreeMap<String, String>,
    secret_values: BTreeMap<String, String>,
    network_grant: NetworkGrant,
    secret_grant: SecretGrant,
    session_id: Option<String>,
}

impl HttpTransport {
    pub(crate) fn new(
        config: &McpConfig,
        timeout: Duration,
        network_grant: NetworkGrant,
        secret_grant: SecretGrant,
    ) -> Result<Self, McpClientError> {
        let endpoint = config
            .endpoint()
            .ok_or_else(|| McpClientError::HttpConfiguration {
                message: "HTTP endpoint is missing".to_string(),
            })?
            .to_string();
        if !network_grant.allows_url(&endpoint) {
            return Err(McpClientError::NetworkDenied { endpoint });
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .map_err(McpClientError::HttpTransport)?;
        Ok(Self {
            endpoint,
            client,
            headers: config.http_headers().cloned().unwrap_or_default(),
            secret_values: config
                .secret_references()
                .into_iter()
                .filter_map(|reference| {
                    config
                        .secret_value(reference)
                        .map(|value| (reference.to_string(), value.to_string()))
                })
                .collect(),
            network_grant,
            secret_grant,
            session_id: None,
        })
    }

    pub(crate) fn request(
        &mut self,
        payload: &Value,
        method: &'static str,
        timeout: Duration,
        network_grant: Option<&NetworkGrant>,
        secret_grant: Option<&SecretGrant>,
    ) -> Result<Value, McpClientError> {
        let network_grant = network_grant.unwrap_or(&self.network_grant);
        if !network_grant.allows_url(&self.endpoint) {
            return Err(McpClientError::NetworkDenied {
                endpoint: self.endpoint.clone(),
            });
        }
        let secret_grant = secret_grant.unwrap_or(&self.secret_grant);
        let headers = self.resolve_headers(secret_grant)?;
        let mut headers = headers;
        if let Some(session_id) = &self.session_id {
            let value = HeaderValue::from_str(session_id).map_err(|_| {
                McpClientError::HttpConfiguration {
                    message: "MCP session id is invalid".to_string(),
                }
            })?;
            headers.insert(HeaderName::from_static("mcp-session-id"), value);
        }
        let body = serde_json::to_vec(payload).map_err(|error| McpClientError::Encode {
            message: error.to_string(),
        })?;
        if body.len() > MAX_MCP_FRAME_BYTES {
            return Err(McpClientError::FrameTooLarge {
                size: body.len(),
                maximum: MAX_MCP_FRAME_BYTES,
            });
        }

        let response = self
            .client
            .post(&self.endpoint)
            .headers(headers)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .timeout(timeout)
            .body(body)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    McpClientError::Timeout { method, timeout }
                } else {
                    McpClientError::HttpTransport(error)
                }
            })?;
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if let Some(session_id) = session_id {
            if session_id.is_empty()
                || session_id.len() > MAX_SESSION_ID_BYTES
                || session_id.chars().any(char::is_control)
            {
                return Err(McpClientError::HttpConfiguration {
                    message: "MCP response session id is invalid".to_string(),
                });
            }
            self.session_id = Some(session_id);
        }
        parse_response(response, method)
    }

    pub(crate) fn notification(
        &mut self,
        payload: &Value,
        method: &'static str,
    ) -> Result<(), McpClientError> {
        let _ = self.request(payload, method, CANCEL_GRACE_TIMEOUT, None, None)?;
        Ok(())
    }

    pub(crate) fn cancel(
        &mut self,
        request_id: u64,
        reason: &str,
        network_grant: Option<&NetworkGrant>,
        secret_grant: Option<&SecretGrant>,
    ) -> Result<(), McpClientError> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": request_id,
                "reason": reason,
            }
        });
        let _ = self.request(
            &payload,
            "notifications/cancelled",
            CANCEL_GRACE_TIMEOUT,
            network_grant,
            secret_grant,
        )?;
        Ok(())
    }

    fn resolve_headers(&self, secret_grant: &SecretGrant) -> Result<HeaderMap, McpClientError> {
        let mut headers = HeaderMap::new();
        for (name, configured_value) in &self.headers {
            let value =
                if let Some(reference) = configured_value.strip_prefix(SECRET_REFERENCE_PREFIX) {
                    if !secret_grant.allows(reference) {
                        return Err(McpClientError::SecretDenied {
                            reference: reference.to_string(),
                        });
                    }
                    self.secret_values.get(reference).ok_or_else(|| {
                        McpClientError::HttpConfiguration {
                            message: "configured secret reference has no local value".to_string(),
                        }
                    })?
                } else {
                    configured_value
                };
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                McpClientError::HttpConfiguration {
                    message: "configured HTTP header name is invalid".to_string(),
                }
            })?;
            let value =
                HeaderValue::from_str(value).map_err(|_| McpClientError::HttpConfiguration {
                    message: "configured HTTP header value is invalid".to_string(),
                })?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    pub(crate) fn redact_text(&self, value: &str) -> String {
        self.secret_values
            .values()
            .filter(|secret| !secret.is_empty())
            .fold(value.to_string(), |value, secret| {
                value.replace(secret, "<redacted>")
            })
    }
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .field("endpoint", &self.endpoint)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("secret_values", &"<redacted>")
            .field("network_grant", &self.network_grant)
            .field("secret_grant", &self.secret_grant)
            .field("session_id", &self.session_id.as_ref().map(|_| "<present>"))
            .finish()
    }
}

fn parse_response(mut response: Response, method: &'static str) -> Result<Value, McpClientError> {
    let status = response.status();
    let is_empty = status.as_u16() == 202 || status.as_u16() == 204;
    let body = read_bounded(&mut response)?;
    if !status.is_success() {
        return Err(McpClientError::HttpStatus {
            status: status.as_u16(),
        });
    }
    if body.is_empty() || is_empty {
        return Ok(Value::Null);
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("text/event-stream") {
        parse_sse(&body, method)
    } else {
        serde_json::from_slice(&body).map_err(|error| McpClientError::MalformedJson {
            message: error.to_string(),
        })
    }
}

fn parse_sse(body: &[u8], method: &'static str) -> Result<Value, McpClientError> {
    for line in body.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data == b"[DONE]" || data.is_empty() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(data).map_err(|error| {
            McpClientError::MalformedJson {
                message: format!("MCP SSE event for `{method}` is invalid: {error}"),
            }
        })?;
        if value.is_object() {
            return Ok(value);
        }
    }
    Err(McpClientError::InvalidResponse {
        method,
        message: "MCP SSE response contained no JSON-RPC event".to_string(),
    })
}

fn read_bounded(response: &mut Response) -> Result<Vec<u8>, McpClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_RESPONSE_BYTES as u64)
    {
        return Err(McpClientError::HttpResponseTooLarge {
            maximum: MAX_HTTP_RESPONSE_BYTES,
        });
    }
    let mut body = Vec::new();
    response
        .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(McpClientError::HttpBodyRead)?;
    if body.len() > MAX_HTTP_RESPONSE_BYTES {
        return Err(McpClientError::HttpResponseTooLarge {
            maximum: MAX_HTTP_RESPONSE_BYTES,
        });
    }
    Ok(body)
}
