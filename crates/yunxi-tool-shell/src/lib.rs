#![doc = "Host-approved, bounded shell execution for YunXi Next."]
#![forbid(unsafe_code)]

mod executor;
mod plugin;

pub use executor::{ShellError, execute};
pub use plugin::{SHELL_PLUGIN_ID, ShellPluginError, run_shell_plugin};
