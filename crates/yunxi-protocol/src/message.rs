//! Serializable messages shared by the host and isolated plugins.

use serde::{Deserialize, Serialize};

use crate::{CapabilityDescriptor, InvocationRequest, InvocationResponse};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MODEL_CHAT_COMPLETE_OPERATION: &str = "complete";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    role: ChatRole,
    content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self { messages }
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatResult {
    content: String,
    finish_reason: Option<String>,
}

impl ChatResult {
    pub fn new(content: impl Into<String>, finish_reason: Option<String>) -> Self {
        Self {
            content: content.into(),
            finish_reason,
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ChatRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }

    pub fn role(&self) -> ChatRole {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Welcome { protocol_version: u32 },
    Invoke { request: InvocationRequest },
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    Hello {
        protocol_version: u32,
        plugin_id: String,
        connection_token: String,
        display_name: String,
        plugin_version: String,
        capabilities: Vec<CapabilityDescriptor>,
    },
    Ready,
    InvocationCompleted {
        response: InvocationResponse,
    },
    InvocationFailed {
        request_id: u64,
        code: String,
        message: String,
        retryable: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_invocation_round_trips_as_stable_json() {
        let request = InvocationRequest::encode(
            7,
            crate::CapabilityDescriptor::new(
                crate::capabilities::MODEL_CHAT,
                crate::capabilities::MODEL_CHAT_VERSION,
            )
            .expect("valid capability"),
            MODEL_CHAT_COMPLETE_OPERATION,
            &ChatRequest::new(vec![ChatMessage::user("hello")]),
        )
        .expect("encode invocation");
        let message = HostMessage::Invoke { request };
        let json = serde_json::to_string(&message).expect("serialize host message");
        assert!(json.contains("\"type\":\"invoke\""));
        assert!(json.contains("\"id\":\"model.chat\""));
        assert_eq!(
            serde_json::from_str::<HostMessage>(&json).expect("deserialize host message"),
            message
        );
    }
}
