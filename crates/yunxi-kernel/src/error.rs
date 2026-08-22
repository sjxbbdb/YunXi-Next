//! Public failures returned by kernel lifecycle operations.

use std::error::Error;
use std::fmt;

use crate::{PluginId, PluginState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelError {
    NotRunning,
    DuplicatePlugin { id: PluginId },
    UnknownPlugin { id: PluginId },
    PluginBusy { id: PluginId, state: PluginState },
    SupervisorThread { id: PluginId, message: String },
    SupervisorUnavailable { id: PluginId },
}

impl fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => formatter.write_str("the YunXi kernel is not running"),
            Self::DuplicatePlugin { id } => {
                write!(formatter, "plugin `{id}` is already registered")
            }
            Self::UnknownPlugin { id } => write!(formatter, "plugin `{id}` is not registered"),
            Self::PluginBusy { id, state } => {
                write!(formatter, "plugin `{id}` cannot start while it is {state}")
            }
            Self::SupervisorThread { id, message } => {
                write!(
                    formatter,
                    "failed to create supervisor for plugin `{id}`: {message}"
                )
            }
            Self::SupervisorUnavailable { id } => {
                write!(formatter, "plugin `{id}` supervisor is unavailable")
            }
        }
    }
}

impl Error for KernelError {}
