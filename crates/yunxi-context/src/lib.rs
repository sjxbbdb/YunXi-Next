#![doc = "Isolated AGENTS.md context capability for YunXi Next."]
#![forbid(unsafe_code)]

mod compose;
mod plugin;

pub use compose::{ContextComposeError, compose_context};
pub use plugin::{CONTEXT_PLUGIN_ID, ContextPluginError, run_context_plugin};
