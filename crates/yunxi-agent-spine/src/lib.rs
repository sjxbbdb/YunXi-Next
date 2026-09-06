#![forbid(unsafe_code)]
#![doc = "A small, replaceable Rust Agent spine over the YunXi protocol types."]

mod agent;
mod background;
mod cancellation;
mod context;
mod error;
mod model;
mod session;
mod state;
mod stream;
mod tool;

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, MAX_TURN_TIMEOUT_MILLIS, TurnBudget, TurnResult,
};
pub use background::{
    BackgroundAgent, BackgroundError, BackgroundTurnHandle, BackgroundTurnSnapshot,
    BackgroundTurnState,
};
pub use cancellation::{CancellationError, CancellationKind, CancellationToken};
pub use context::{ContextAssembler, ContextAssemblyRequest, ConversationContextAssembler};
pub use error::{AgentError, BudgetKind, ComponentError, ContextError, ModelError, ToolError};
pub use model::{ModelEventSink, ModelProvider, ModelRequest};
pub use session::{
    SessionError, SessionEvent, SessionEventKind, SessionLimits, SessionLog, SessionRecord,
};
pub use state::{AgentSnapshot, AgentState, TurnSnapshot, TurnState};
pub use stream::{
    BackpressurePolicy, BackpressureStrategy, DEFAULT_EVENT_BLOCK_TIMEOUT,
    DEFAULT_EVENT_CHANNEL_CAPACITY, EventChannel, EventChannelConfig, EventChannelError,
    EventReceiveError, EventReceiver, EventSender, EventSink, EventSinkError,
    MAX_EVENT_BLOCK_TIMEOUT, MAX_EVENT_CHANNEL_CAPACITY, bounded_event_channel,
};
pub use tool::{
    ApprovalAwareToolBroker, EmptyToolBroker, RequireApproval, ToolApprovalPolicy, ToolBroker,
    ToolDecision, ToolExecutionOutcome, ToolProgressSink, ToolRequest,
};
