//! Complete plugin description accepted by the runtime registry.

use super::{PluginCommand, PluginId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSpec {
    id: PluginId,
    display_name: String,
    command: PluginCommand,
}

impl PluginSpec {
    pub fn new(id: PluginId, command: PluginCommand) -> Self {
        let display_name = id.to_string();
        Self {
            id,
            display_name,
            command,
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        if !display_name.trim().is_empty() {
            self.display_name = display_name;
        }
        self
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn command(&self) -> &PluginCommand {
        &self.command
    }
}
