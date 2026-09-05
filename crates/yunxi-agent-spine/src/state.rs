use serde::{Deserialize, Serialize};

use crate::error::ComponentError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Ready,
    Running,
    AwaitingApproval,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Created,
    ContextBuilding,
    ModelCalling,
    ToolCalling,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for TurnState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Created => "created",
            Self::ContextBuilding => "context_building",
            Self::ModelCalling => "model_calling",
            Self::ToolCalling => "tool_calling",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub(crate) id: String,
    pub(crate) state: AgentState,
    pub(crate) turns_started: u64,
    pub(crate) last_turn_id: Option<String>,
}

impl AgentSnapshot {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn state(&self) -> AgentState {
        self.state
    }

    pub const fn turns_started(&self) -> u64 {
        self.turns_started
    }

    pub fn last_turn_id(&self) -> Option<&str> {
        self.last_turn_id.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TurnSnapshot {
    pub(crate) id: String,
    pub(crate) state: TurnState,
    pub(crate) round: u16,
    pub(crate) model_calls: u32,
    pub(crate) tool_calls: u32,
    pub(crate) error: Option<ComponentError>,
}

impl TurnSnapshot {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn state(&self) -> TurnState {
        self.state
    }

    pub const fn round(&self) -> u16 {
        self.round
    }

    pub const fn model_calls(&self) -> u32 {
        self.model_calls
    }

    pub const fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    pub fn error(&self) -> Option<&ComponentError> {
        self.error.as_ref()
    }
}
