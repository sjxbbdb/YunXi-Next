//! Internal management commands shared by the REPL and plugin-backed session.

use yunxi_protocol::ChatMessage;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ManagementCommand {
    ListPlugins,
    ListSessions,
    ResumeSession(String),
    NewSession,
    ReviewMemory { id: String, approve: bool },
    ListMailbox,
    ReadMailbox(String),
    RequestShell(String),
    RequestPatch(String),
    ApproveAction,
    DenyAction,
    CancelAction,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ManagementResult {
    pub lines: Vec<String>,
    pub replacement_history: Option<Vec<ChatMessage>>,
    pub append_history: Vec<ChatMessage>,
    pub assistant_reply: Option<String>,
}

impl ManagementResult {
    pub fn lines(lines: Vec<String>) -> Self {
        Self {
            lines,
            replacement_history: None,
            append_history: Vec::new(),
            assistant_reply: None,
        }
    }

    pub fn replace_history(lines: Vec<String>, history: Vec<ChatMessage>) -> Self {
        Self {
            lines,
            replacement_history: Some(history),
            append_history: Vec::new(),
            assistant_reply: None,
        }
    }

    pub fn assistant_reply(reply: impl Into<String>, history: Vec<ChatMessage>) -> Self {
        Self {
            lines: Vec::new(),
            replacement_history: None,
            append_history: history,
            assistant_reply: Some(reply.into()),
        }
    }
}
