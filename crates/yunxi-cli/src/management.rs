//! Internal management commands shared by the REPL and plugin-backed session.

use yunxi_protocol::ChatMessage;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ManagementCommand {
    ListSessions,
    ResumeSession(String),
    NewSession,
    ReviewMemory { id: String, approve: bool },
    ListMailbox,
    ReadMailbox(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ManagementResult {
    pub lines: Vec<String>,
    pub replacement_history: Option<Vec<ChatMessage>>,
}

impl ManagementResult {
    pub fn lines(lines: Vec<String>) -> Self {
        Self {
            lines,
            replacement_history: None,
        }
    }

    pub fn replace_history(lines: Vec<String>, history: Vec<ChatMessage>) -> Self {
        Self {
            lines,
            replacement_history: Some(history),
        }
    }
}
