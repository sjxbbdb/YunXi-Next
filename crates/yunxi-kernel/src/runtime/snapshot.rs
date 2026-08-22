//! Immutable aggregate views of kernel health and plugin state.

use std::fmt;

use crate::{PluginSnapshot, PluginState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelState {
    Running,
    ShuttingDown,
    Stopped,
}

impl fmt::Display for KernelState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => formatter.write_str("running"),
            Self::ShuttingDown => formatter.write_str("shutting down"),
            Self::Stopped => formatter.write_str("stopped"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelSnapshot {
    pub(crate) state: KernelState,
    pub(crate) plugins: Vec<PluginSnapshot>,
}

impl KernelSnapshot {
    pub fn state(&self) -> KernelState {
        self.state
    }

    pub fn plugins(&self) -> &[PluginSnapshot] {
        &self.plugins
    }

    pub fn running_plugin_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|plugin| matches!(plugin.state(), PluginState::Running { .. }))
            .count()
    }

    pub fn failed_plugin_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|plugin| plugin.state().is_failed())
            .count()
    }
}
