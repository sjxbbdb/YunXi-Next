#![doc = "Versioned local protocol for isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod capability;
mod companion;
mod grant;
mod handshake;
mod identity;
mod invocation;
mod mailbox;
mod memory_write;
mod message;
mod scheduler;
mod sessions;
mod transport;

pub use capability::{
    CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityIdError, capabilities,
};
pub use companion::{
    COMPANION_DECIDE_OPERATION, CompanionDecisionRequest, CompanionDecisionResult,
    CompanionEmotionKind, CompanionTone,
};
pub use grant::WorkspaceGrant;
pub use handshake::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, HostPluginSession, PluginAcceptor,
    PluginConnectionInfo, PluginSession, connect_plugin,
};
pub use identity::{
    CONTEXT_COMPOSE_OPERATION, ContextComposeRequest, ContextComposeResult,
    MEMORY_RECALL_OPERATION, MemoryContextKind, MemoryContextRecord, MemoryRecallRequest,
    MemoryRecallResult, PERSONA_CONTEXT_COMPILE_OPERATION, PersonaContextRequest,
    PersonaContextResult,
};
pub use invocation::{InvocationCodecError, InvocationRequest, InvocationResponse};
pub use mailbox::{
    COMPANION_MAILBOX_ENQUEUE_OPERATION, COMPANION_MAILBOX_GET_OPERATION,
    COMPANION_MAILBOX_LIST_OPERATION, COMPANION_MAILBOX_MARK_READ_OPERATION, MailboxEnqueueRequest,
    MailboxEntry, MailboxGetRequest, MailboxGetResult, MailboxItemKind, MailboxListRequest,
    MailboxListResult, MailboxMarkReadRequest, MailboxMutationResult, MailboxSummary,
};
pub use memory_write::{
    MEMORY_WRITE_EXTRACT_OPERATION, MEMORY_WRITE_REVIEW_OPERATION, MemoryReviewAction,
    MemoryReviewRequest, MemoryReviewResult, MemoryWriteRequest, MemoryWriteResult,
    MemoryWriteStatus, MemoryWriteSummary,
};
pub use message::{
    ChatMessage, ChatRequest, ChatResult, ChatRole, HostMessage, MODEL_CHAT_COMPLETE_OPERATION,
    PROTOCOL_VERSION, PluginMessage,
};
pub use scheduler::{
    ProactiveAction, ProactivePlan, ProactiveSchedulerRequest, ProactiveSchedulerResult,
    ProactiveTrigger, QuietHours, SCHEDULER_PROACTIVE_EVALUATE_OPERATION,
};
pub use sessions::{
    STORAGE_SESSIONS_APPEND_OPERATION, STORAGE_SESSIONS_LIST_OPERATION,
    STORAGE_SESSIONS_LOAD_OPERATION, STORAGE_SESSIONS_MUTATE_OPERATION, SessionAppendRequest,
    SessionListRequest, SessionListResult, SessionLoadRequest, SessionLoadResult, SessionMutation,
    SessionMutationRequest, SessionMutationResult, SessionSnapshot, SessionSummary,
};
pub use transport::{DEFAULT_MAX_FRAME_BYTES, JsonLineTransport, ProtocolError};
