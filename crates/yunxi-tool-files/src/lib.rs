#![doc = "Host-granted, read-only workspace file tools for YunXi Next."]
#![forbid(unsafe_code)]

mod executor;
mod plugin;

pub use executor::{FileToolError, read_file, search_files};
pub use plugin::{FILES_PLUGIN_ID, FileToolPluginError, run_file_tool_plugin};
