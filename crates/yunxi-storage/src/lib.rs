#![doc = "Workspace-scoped persistent session capability for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod record;
mod store;

pub use plugin::{STORAGE_PLUGIN_ID, StoragePluginError, run_storage_plugin};
pub use store::{SessionStore, StorageError};
