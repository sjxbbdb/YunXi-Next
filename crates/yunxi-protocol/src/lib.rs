#![doc = "Versioned local protocol for isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod authority;
mod capability;
mod companion;
mod files;
mod grant;
mod handshake;
mod identity;
mod invocation;
mod mailbox;
mod management;
mod manifest;
mod mcp;
mod memory_write;
mod message;
mod model_stream;
mod multi_agent;
mod scheduler;
mod sessions;
mod skills;
mod stream;
mod tool_calls;
mod tools;
mod transport;

pub use authority::{
    AuthorityError, MAX_NETWORK_HOST_BYTES, MAX_NETWORK_SCOPES, MAX_SECRET_REFERENCE_BYTES,
    MAX_SECRET_REFERENCES, NetworkGrant, NetworkScheme, NetworkScope, SecretGrant,
};
pub use capability::{
    CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityIdError, capabilities,
};
pub use companion::{
    COMPANION_DECIDE_OPERATION, CompanionDecisionRequest, CompanionDecisionResult,
    CompanionEmotionKind, CompanionTone,
};
pub use files::{
    FileReadRequest, FileReadResult, FileSearchMatch, FileSearchRequest, FileSearchResult,
    TOOL_FILES_READ_OPERATION, TOOL_FILES_SEARCH_OPERATION,
};
pub use grant::WorkspaceGrant;
pub use handshake::{
    CONNECT_ADDRESS_ENV, CONNECT_TOKEN_ENV, HostPluginSession, PluginAcceptor,
    PluginConnectionInfo, PluginSession, connect_plugin, connect_plugin_with_grants,
    connect_plugin_with_manifest,
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
pub use management::{
    COMPANION_MANAGEMENT_CHECK_OPERATION, COMPANION_MANAGEMENT_CLEAR_OPERATION,
    COMPANION_MANAGEMENT_HISTORY_OPERATION, COMPANION_MANAGEMENT_SET_ENABLED_OPERATION,
    COMPANION_MANAGEMENT_STATUS_OPERATION, CompanionCheckRequest, CompanionClearRequest,
    CompanionHistoryRequest, CompanionSetEnabledRequest, CompanionStatusRequest,
    MAX_COMPANION_HISTORY_RECORDS, MAX_MANAGEMENT_ID_CHARS, MAX_MANAGEMENT_QUERY_CHARS,
    MAX_MANAGEMENT_RECORDS, MAX_PERSONA_PROFILE_BYTES, MAX_PERSONA_PROFILE_ID_BYTES,
    MEMORY_MANAGEMENT_CLEAR_OPERATION, MEMORY_MANAGEMENT_LIST_OPERATION,
    MEMORY_MANAGEMENT_MUTATE_OPERATION, MEMORY_MANAGEMENT_QUERY_OPERATION,
    MEMORY_MANAGEMENT_SET_ENABLED_OPERATION, MEMORY_MANAGEMENT_SHOW_OPERATION,
    MEMORY_MANAGEMENT_STATUS_OPERATION, ManagementRequestError, MemoryClearRequest,
    MemoryClearScope, MemoryListRequest, MemoryManagementScope, MemoryMutationAction,
    MemoryMutationRequest, MemoryQueryRequest, MemorySetEnabledRequest, MemoryShowRequest,
    MemoryStatusRequest, PERSONA_MANAGEMENT_IMPORT_OPERATION, PERSONA_MANAGEMENT_LIST_OPERATION,
    PERSONA_MANAGEMENT_PROFILE_OPERATION, PERSONA_MANAGEMENT_RESET_OPERATION,
    PERSONA_MANAGEMENT_SET_ACTIVE_OPERATION, PERSONA_MANAGEMENT_SET_ENABLED_OPERATION,
    PERSONA_MANAGEMENT_STATUS_OPERATION, PersonaImportRequest, PersonaListRequest,
    PersonaProfileRequest, PersonaResetRequest, PersonaSetActiveRequest, PersonaSetEnabledRequest,
    PersonaStatusRequest,
};
pub use manifest::{
    DEFAULT_HOST_GROUP, GrantKind, GrantRequirement, MANIFEST_SCHEMA_VERSION, MAX_HOST_GROUP_BYTES,
    ManifestError, PluginManifest, PluginRiskLevel, PluginRuntimeMetadata,
};
pub use mcp::{
    MCP_PROTOCOL_VERSION, McpProtocolError, McpServerState, McpStatusRequest, McpStatusResult,
    McpToolCallRequest, McpToolCallResult, McpToolCancelRequest, McpToolCancelResult,
    McpToolDescriptor, McpToolListRequest, McpToolListResult, TOOL_MCP_CALL_OPERATION,
    TOOL_MCP_CANCEL_OPERATION, TOOL_MCP_LIST_OPERATION, TOOL_MCP_STATUS_OPERATION,
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
pub use model_stream::{
    MAX_MODEL_STREAM_FINISH_REASON_BYTES, MAX_MODEL_STREAM_ID_BYTES, MAX_MODEL_STREAM_NAME_BYTES,
    MAX_MODEL_STREAM_TEXT_BYTES, MAX_MODEL_STREAM_TOOL_CALL_INDEX, ModelStreamEvent,
};
pub use multi_agent::{
    AgentBudget, AgentDelegationGrant, AgentEvent, AgentEventKind, AgentInspectRequest,
    AgentInspectResult, AgentInterruptRequest, AgentListRequest, AgentListResult,
    AgentMutationResult, AgentProtocolError, AgentSnapshot, AgentSpawnRequest, AgentSpawnResult,
    AgentStatus, AgentTranscriptEntry, AgentTranscriptRole, AgentTurnCompleteRequest,
    AgentTurnFailRequest, AgentTurnStartRequest, AgentTurnStartResult, MAX_AGENT_COUNT,
    MAX_AGENT_DEPTH, MAX_AGENT_EVENTS, MAX_AGENT_MESSAGE_BYTES, MAX_AGENT_REPLY_BYTES,
    MAX_AGENT_TOTAL_TURNS, MAX_AGENT_TRANSCRIPT_ENTRIES, MAX_AGENT_TURNS_PER_AGENT, ROOT_AGENT_ID,
    TOOL_MULTI_AGENT_INSPECT_OPERATION, TOOL_MULTI_AGENT_INTERRUPT_OPERATION,
    TOOL_MULTI_AGENT_LIST_OPERATION, TOOL_MULTI_AGENT_SPAWN_OPERATION,
    TOOL_MULTI_AGENT_TURN_COMPLETE_OPERATION, TOOL_MULTI_AGENT_TURN_FAIL_OPERATION,
    TOOL_MULTI_AGENT_TURN_START_OPERATION,
};
pub use scheduler::{
    ProactiveAction, ProactivePlan, ProactiveSchedulerRequest, ProactiveSchedulerResult,
    ProactiveTrigger, QuietHours, SCHEDULER_PROACTIVE_EVALUATE_OPERATION,
};
pub use sessions::{
    STORAGE_SESSIONS_APPEND_OPERATION, STORAGE_SESSIONS_CREATE_OPERATION,
    STORAGE_SESSIONS_LIST_OPERATION, STORAGE_SESSIONS_LOAD_OPERATION,
    STORAGE_SESSIONS_MUTATE_OPERATION, SessionAppendRequest, SessionCreateRequest,
    SessionCreateResult, SessionListRequest, SessionListResult, SessionLoadRequest,
    SessionLoadResult, SessionMutation, SessionMutationRequest, SessionMutationResult,
    SessionSnapshot, SessionSummary,
};
pub use skills::{
    MAX_SKILL_ACTION_ARGUMENT_BYTES, MAX_SKILL_ACTION_ARGUMENTS, MAX_SKILL_ACTION_FRAME_BYTES,
    MAX_SKILL_ACTION_INPUT_BYTES, MAX_SKILL_ACTION_OUTPUT_BYTES, MAX_SKILL_ACTION_PROGRAM_BYTES,
    MAX_SKILL_ACTION_TIMEOUT_MILLIS, MAX_SKILL_ACTION_TOOL_NAME_BYTES, MAX_SKILL_ACTIONS,
    MAX_SKILL_CONTEXT_BYTES, MAX_SKILL_DESCRIPTION_BYTES, MAX_SKILL_ID_BYTES,
    MAX_SKILL_INSTRUCTION_BYTES, MAX_SKILL_METADATA, MAX_SKILL_NAME_BYTES, MAX_SKILL_PATH_BYTES,
    MAX_SKILL_TOOL_DECLARATIONS, MAX_SKILL_TOOL_DESCRIPTION_BYTES, MAX_SKILL_TOOL_NAME_BYTES,
    MAX_SKILL_TOOL_SCHEMA_BYTES, SKILL_ACTION_PROTOCOL_VERSION, SkillActionOutcome,
    SkillActionRequest, SkillActionResponse, SkillActionSpec, SkillContextBlock,
    SkillContextRequest, SkillContextResult, SkillListRequest, SkillListResult, SkillMetadata,
    SkillProtocolError, SkillRuntimeState, SkillStatusRequest, SkillStatusResult,
    SkillToolDescriptor, TOOL_SKILLS_ACTION_OPERATION, TOOL_SKILLS_CONTEXT_OPERATION,
    TOOL_SKILLS_LIST_OPERATION, TOOL_SKILLS_STATUS_OPERATION,
};
pub use stream::{
    AgentStreamEnvelope, AgentStreamEvent, MAX_STREAM_ERROR_CODE_BYTES,
    MAX_STREAM_ERROR_MESSAGE_BYTES, MAX_STREAM_EVENT_BYTES, MAX_STREAM_EVENTS_PER_TURN,
    MAX_STREAM_FINISH_REASON_BYTES, MAX_STREAM_TEXT_BYTES, MAX_STREAM_TURN_ID_BYTES,
    STREAM_PROTOCOL_VERSION, StreamEnvelope, StreamError, StreamEvent, StreamProtocolError,
    StreamTurnState,
};
pub use tool_calls::{
    DEFAULT_MAX_TOOL_CALLS_PER_ROUND, DEFAULT_MAX_TOOL_ROUNDS, MAX_APPROVAL_GRANTS,
    MAX_TOOL_ARGUMENT_BYTES, MAX_TOOL_CALL_ID_BYTES, MAX_TOOL_CALLS_PER_ROUND,
    MAX_TOOL_DEFINITIONS, MAX_TOOL_NAME_BYTES, MAX_TOOL_RESULT_BYTES, MAX_TOOL_ROUNDS,
    MAX_TOOL_SCHEMA_BYTES, MAX_TOOL_TEXT_BYTES, TOOL_PROTOCOL_VERSION, ToolApprovalDecision,
    ToolApprovalRequest, ToolApprovalState, ToolCall, ToolCallBatch, ToolCallId, ToolCancellation,
    ToolCatalog, ToolDefinition, ToolLoopPolicy, ToolName, ToolProtocolError, ToolProtocolMessage,
    ToolResult, ToolResultOutcome,
};
pub use tools::{
    ActionApproval, ActionGrant, ActionGrantError, PatchApplyRequest, PatchApplyResult,
    PatchChangeKind, PatchFileChange, ShellExecuteRequest, ShellExecuteResult,
    TOOL_PATCH_APPLY_OPERATION, TOOL_SHELL_EXECUTE_OPERATION,
};
pub use transport::{DEFAULT_MAX_FRAME_BYTES, JsonLineTransport, ProtocolError};
