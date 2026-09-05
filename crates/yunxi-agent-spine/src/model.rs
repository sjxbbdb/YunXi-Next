use crate::cancellation::CancellationToken;
use crate::error::ModelError;
use yunxi_protocol::{ChatRequest, ChatResult};

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

pub trait ModelProvider {
    fn complete(
        &mut self,
        request: &ModelRequest,
        cancellation: &CancellationToken,
    ) -> Result<ChatResult, ModelError>;
}
