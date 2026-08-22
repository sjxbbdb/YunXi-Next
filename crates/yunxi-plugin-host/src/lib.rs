#![doc = "Capability catalog for process-isolated YunXi plugins."]
#![forbid(unsafe_code)]

mod catalog;
mod runtime;

pub use catalog::{CapabilityCatalog, CatalogError, PluginRecord};
pub use runtime::{PluginCallError, PluginHostError, PluginLaunch, ProcessPluginHost};
