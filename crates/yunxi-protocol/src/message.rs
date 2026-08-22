//! Serializable messages shared by the host and isolated plugins.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CHAT_CAPABILITY: &str = "chat";

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
    Welcome {
        protocol_version: u32,
    },
    Chat {
        request_id: u64,
        messages: Vec<ChatMessage>,
    },
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    Hello {
        protocol_version: u32,
        plugin_id: String,
        connection_token: String,
        provider: String,
        model: String,
        capabilities: Vec<String>,
    },
    Ready,
    ChatCompleted {
        request_id: u64,
        content: String,
        finish_reason: Option<String>,
    },
    RequestFailed {
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
    fn chat_message_round_trips_as_stable_json() {
        let message = HostMessage::Chat {
            request_id: 7,
            messages: vec![ChatMessage::user("hello")],
        };
        let json = serde_json::to_string(&message).expect("serialize host message");
        assert!(json.contains("\"type\":\"chat\""));
        assert_eq!(
            serde_json::from_str::<HostMessage>(&json).expect("deserialize host message"),
            message
        );
    }
}
