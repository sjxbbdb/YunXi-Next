//! Observable plugin failures, lifecycle states, and immutable snapshots.

use std::fmt;

use super::PluginId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginFailure {
    Spawn { message: String },
    UnexpectedExit { code: Option<i32> },
    Protocol { message: String },
    Monitor { message: String },
    Supervisor { message: String },
}

impl fmt::Display for PluginFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { message } => write!(formatter, "process failed to start: {message}"),
            Self::UnexpectedExit { code: Some(code) } => {
                write!(formatter, "process exited unexpectedly with code {code}")
            }
            Self::UnexpectedExit { code: None } => {
                formatter.write_str("process exited unexpectedly without an exit code")
            }
            Self::Protocol { message } => write!(formatter, "protocol failed: {message}"),
            Self::Monitor { message } => write!(formatter, "process monitor failed: {message}"),
            Self::Supervisor { message } => write!(formatter, "supervisor failed: {message}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginState {
    Registered,
    Starting,
    Running { pid: u32 },
    Stopping,
    Stopped,
    Failed(PluginFailure),
}

impl PluginState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running { .. } | Self::Stopping)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed(_))
    }
}

impl fmt::Display for PluginState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registered => formatter.write_str("registered"),
            Self::Starting => formatter.write_str("starting"),
            Self::Running { pid } => write!(formatter, "running as process {pid}"),
            Self::Stopping => formatter.write_str("stopping"),
            Self::Stopped => formatter.write_str("stopped"),
            Self::Failed(failure) => write!(formatter, "failed: {failure}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSnapshot {
    id: PluginId,
    display_name: String,
    state: PluginState,
    generation: u64,
}

impl PluginSnapshot {
    pub(crate) fn new(
        id: PluginId,
        display_name: String,
        state: PluginState,
        generation: u64,
    ) -> Self {
        Self {
            id,
            display_name,
            state,
            generation,
        }
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn state(&self) -> &PluginState {
        &self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}
