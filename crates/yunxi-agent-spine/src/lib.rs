#![forbid(unsafe_code)]
#![doc = "A small, replaceable Rust Agent spine over the YunXi protocol types."]

mod agent;
mod cancellation;
mod context;
mod error;
mod model;
mod session;
mod state;
mod tool;

pub use agent::{Agent, AgentConfig, AgentTurnOutcome, TurnBudget, TurnResult};
pub use cancellation::{CancellationError, CancellationToken};
pub use context::{ContextAssembler, ContextAssemblyRequest, ConversationContextAssembler};
pub use error::{AgentError, BudgetKind, ComponentError, ContextError, ModelError, ToolError};
pub use model::{ModelProvider, ModelRequest};
pub use session::{
    SessionError, SessionEvent, SessionEventKind, SessionLimits, SessionLog, SessionRecord,
};
pub use state::{AgentSnapshot, AgentState, TurnSnapshot, TurnState};
pub use tool::{
    ApprovalAwareToolBroker, EmptyToolBroker, RequireApproval, ToolApprovalPolicy, ToolBroker,
    ToolDecision, ToolExecutionOutcome, ToolRequest,
};
