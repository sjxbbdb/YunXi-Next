#![doc = "Deterministic companion response policy for YunXi Next."]
#![forbid(unsafe_code)]

mod decision;
mod plugin;

pub use decision::decide;
pub use plugin::{COMPANION_PLUGIN_ID, CompanionPluginError, run_companion_plugin};
