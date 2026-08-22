//! Plugin-domain types used by the kernel facade.
//!
//! This module describes a plugin without supervising it. Process ownership
//! belongs to `supervision`, and registry coordination belongs to `runtime`.

mod command;
mod id;
mod spec;
mod state;

pub use command::PluginCommand;
pub use id::{PluginId, PluginIdError};
pub use spec::PluginSpec;
pub use state::{PluginFailure, PluginSnapshot, PluginState};
