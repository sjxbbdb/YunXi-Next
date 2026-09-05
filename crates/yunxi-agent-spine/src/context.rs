use crate::error::ContextError;
use crate::session::SessionLog;
use yunxi_protocol::{ChatMessage, ChatRequest, ToolCatalog};

pub struct ContextAssemblyRequest<'a> {
    turn_id: &'a str,
    user_message: &'a ChatMessage,
    session: &'a SessionLog,
    tools: &'a ToolCatalog,
}

impl<'a> ContextAssemblyRequest<'a> {
    pub(crate) fn new(
        turn_id: &'a str,
        user_message: &'a ChatMessage,
        session: &'a SessionLog,
        tools: &'a ToolCatalog,
    ) -> Self {
        Self {
            turn_id,
            user_message,
            session,
            tools,
        }
    }

    pub fn turn_id(&self) -> &str {
        self.turn_id
    }

    pub fn user_message(&self) -> &ChatMessage {
        self.user_message
    }

    pub fn session(&self) -> &SessionLog {
        self.session
    }

    pub fn tools(&self) -> &ToolCatalog {
        self.tools
    }
}

pub trait ContextAssembler {
    fn assemble(
        &mut self,
        request: ContextAssemblyRequest<'_>,
    ) -> Result<ChatRequest, ContextError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConversationContextAssembler;

impl ContextAssembler for ConversationContextAssembler {
    fn assemble(
        &mut self,
        request: ContextAssemblyRequest<'_>,
    ) -> Result<ChatRequest, ContextError> {
        let mut messages = request.session().conversation();
        let turn_started = request
            .session()
            .records()
            .iter()
            .any(|record| record.event().turn_id() == request.turn_id());
        if !turn_started {
            messages.push(request.user_message().clone());
        }
        if messages.is_empty() {
            return Err(ContextError::new(
                "empty_context",
                "context assembler produced no messages",
                false,
            ));
        }
        let mut chat_request = ChatRequest::new(messages);
        if !request.tools().tools().is_empty() {
            chat_request = chat_request.with_tools(request.tools().clone());
        }
        Ok(chat_request)
    }
}
