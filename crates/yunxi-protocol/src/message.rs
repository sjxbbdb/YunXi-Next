//! Serializable messages shared by the host and isolated plugins.

use serde::{Deserialize, Serialize};

use crate::{
    CapabilityDescriptor, InvocationRequest, InvocationResponse, ModelStreamEvent, PluginManifest,
    ToolCall, ToolCallId, ToolCatalog, ToolName,
};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MODEL_CHAT_COMPLETE_OPERATION: &str = "complete";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    role: ChatRole,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<ToolName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCall>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<ToolCatalog>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            tools: None,
        }
    }

    pub fn with_tools(mut self, tools: ToolCatalog) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn tools(&self) -> Option<&ToolCatalog> {
        self.tools.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatResult {
    content: String,
    finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCall>,
}

impl ChatResult {
    pub fn new(content: impl Into<String>, finish_reason: Option<String>) -> Self {
        Self {
            content: content.into(),
            finish_reason,
            tool_calls: Vec::new(),
        }
    }

    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

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

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
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

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_name: None,
            tool_calls,
        }
    }

    pub fn tool_result(
        call_id: &ToolCallId,
        tool_name: &ToolName,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.clone()),
            tool_name: Some(tool_name.clone()),
            tool_calls: Vec::new(),
        }
    }

    pub fn role(&self) -> ChatRole {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn tool_call_id(&self) -> Option<&ToolCallId> {
        self.tool_call_id.as_ref()
    }

    pub fn tool_name(&self) -> Option<&ToolName> {
        self.tool_name.as_ref()
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Welcome {
        protocol_version: u32,
    },
    Invoke {
        request: InvocationRequest,
    },
    /// Requests that the plugin stop the in-flight invocation with this id.
    ///
    /// This is separate from Invoke so a plugin can observe control frames
    /// while a worker is blocked in provider I/O.
    Cancel {
        request_id: u64,
    },
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// Keep the manifest inline to preserve the existing public Hello shape and
// wire representation; this enum is short-lived at the handshake boundary.
#[allow(clippy::large_enum_variant)]
pub enum PluginMessage {
    Hello {
        protocol_version: u32,
        plugin_id: String,
        connection_token: String,
        display_name: String,
        plugin_version: String,
        capabilities: Vec<CapabilityDescriptor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest: Option<PluginManifest>,
    },
    Ready,
    /// A bounded, non-terminal model delta for an invocation currently in
    /// flight. Older callers can ignore streaming by using `invoke`.
    InvocationProgress {
        request_id: u64,
        event: ModelStreamEvent,
    },
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

    #[test]
    fn hello_without_a_manifest_remains_wire_compatible() {
        let json = r#"{"type":"hello","protocol_version":2,"plugin_id":"yunxi.legacy","connection_token":"token","display_name":"Legacy plugin","plugin_version":"1.0.0","capabilities":[{"id":"model.chat","version":1}]}"#;
        let message = serde_json::from_str::<PluginMessage>(json).expect("decode legacy hello");
        assert!(matches!(
            message,
            PluginMessage::Hello { manifest: None, .. }
        ));
    }

    #[test]
    fn ordinary_chat_messages_keep_the_legacy_wire_shape() {
        let message = ChatMessage::user("hello");
        assert_eq!(
            serde_json::to_string(&message).expect("serialize chat message"),
            r#"{"role":"user","content":"hello"}"#
        );
    }

    #[test]
    fn invocation_cancel_has_a_small_stable_wire_shape() {
        let message = HostMessage::Cancel { request_id: 42 };
        let json = serde_json::to_string(&message).expect("serialize cancel message");
        assert_eq!(json, r#"{"type":"cancel","request_id":42}"#);
        assert_eq!(
            serde_json::from_str::<HostMessage>(&json).expect("deserialize cancel message"),
            message
        );
    }
}
