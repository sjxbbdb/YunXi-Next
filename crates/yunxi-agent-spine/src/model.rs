use crate::cancellation::CancellationToken;
use crate::error::ModelError;
use yunxi_protocol::{ChatRequest, ChatResult, ToolCall};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    turn_id: String,
    round: u16,
    chat: ChatRequest,
}

impl ModelRequest {
    pub fn new(turn_id: impl Into<String>, round: u16, chat: ChatRequest) -> Self {
        Self {
            turn_id: turn_id.into(),
            round,
            chat,
        }
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub const fn round(&self) -> u16 {
        self.round
    }

    pub fn chat(&self) -> &ChatRequest {
        &self.chat
    }
}

/// Receives bounded callbacks from a streaming model adapter.
///
/// The callback is deliberately synchronous.  A provider must return from
/// each callback before producing the next chunk, which gives the host's
/// [`crate::EventSink`] a chance to apply backpressure.  Existing providers
/// only need to implement [`ModelProvider::complete`].
pub trait ModelEventSink {
    fn text_delta(&mut self, delta: &str) -> Result<(), ModelError>;

    /// Announces a tool call while the model is streaming.  The Agent also
    /// validates the final `ChatResult`, so this is only an early UI signal.
    fn tool_call_start(&mut self, _call: &ToolCall) -> Result<(), ModelError> {
        Ok(())
    }
}

pub trait ModelProvider {
    fn complete(
        &mut self,
        request: &ModelRequest,
        cancellation: &CancellationToken,
    ) -> Result<ChatResult, ModelError>;

    /// Streaming extension point with a compatibility default.
    ///
    /// A legacy synchronous provider is represented as one bounded text delta
    /// (and optional tool-call announcements).  A real SSE/IPC adapter should
    /// override this method and call the sink once per bounded chunk while
    /// checking `cancellation` between chunks.
    fn complete_streaming(
        &mut self,
        request: &ModelRequest,
        cancellation: &CancellationToken,
        events: &mut dyn ModelEventSink,
    ) -> Result<ChatResult, ModelError> {
        let response = self.complete(request, cancellation)?;
        if !response.content().is_empty() {
            events.text_delta(response.content())?;
        }
        for call in response.tool_calls() {
            events.tool_call_start(call)?;
        }
        Ok(response)
    }
}
