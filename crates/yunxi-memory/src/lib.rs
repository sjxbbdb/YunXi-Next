#![doc = "Read-only legacy-compatible memory recall capability for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod recall;
mod record;
mod store;
mod write;

pub use plugin::{MEMORY_PLUGIN_ID, MemoryPluginError, run_memory_plugin};
pub use recall::{MemoryRecallError, recall};
pub use store::MemoryStoreError;
pub use write::{MemoryWriteError, extract_and_store, review_memory};
