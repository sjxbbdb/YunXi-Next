#![doc = "Isolated multi-agent coordination capability for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod store;

pub use plugin::{MULTI_AGENT_PLUGIN_ID, MultiAgentPluginError, run_multi_agent_plugin};
pub use store::{CoordinatorStore, MultiAgentStoreError};
