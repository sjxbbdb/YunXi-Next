#![doc = "Read-only legacy-compatible memory recall capability for YunXi Next."]
#![forbid(unsafe_code)]

mod management;
mod plugin;
mod recall;
mod record;
mod store;
mod write;

pub use management::{
    MemoryCounts, MemoryListResult, MemoryManagementError, MemoryMutationResult,
    MemoryRecordSummary, MemoryStatusReport, clear, clear_with_grant, delete, list,
    list_with_grant, mutate_with_grant, pending, query_with_grant, set_enabled,
    set_enabled_with_grant, show, show_with_grant, status, status_with_grant,
};
pub use plugin::{MEMORY_PLUGIN_ID, MemoryPluginError, run_memory_plugin};
pub use recall::{MemoryRecallError, recall};
pub use store::MemoryStoreError;
pub use write::{MemoryWriteError, extract_and_store, review_memory};
