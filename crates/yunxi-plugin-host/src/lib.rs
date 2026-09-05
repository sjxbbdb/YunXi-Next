#![doc = "Capability catalog for process-isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod catalog;
mod retry;
mod runtime;

pub use catalog::{CapabilityCatalog, CatalogError, PluginRecord};
pub use retry::{
    DEFAULT_MAX_AUTOMATIC_RESTARTS, MAX_AUTOMATIC_RESTARTS, RetryAction, RetryController,
    RetryPolicy, RetrySnapshot,
};
pub use runtime::{PluginCallError, PluginHostError, PluginLaunch, ProcessPluginHost};
